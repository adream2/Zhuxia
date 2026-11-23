/* ---------- 流量状态（自 app.js 拆出） ---------- */

let TRAFFIC = { net: { flows: [] }, alive: true, admin: false, sort: "traffic", q: "", timer: 0 };

/* ---------- 流量（自 app.js 拆出） ---------- */

/* ---------- 流量 ---------- */

async function loadTraffic() {
  try {
    const r = await inv("net_state", undefined, { quiet: true });
    TRAFFIC.net = r.net || { flows: [] };
    TRAFFIC.alive = !!r.alive;
    TRAFFIC.admin = !!r.admin;
  } catch (e) {
    /* 常驻没在跑时读不到，保持上一次的数据 */
  }
}

/// 进入流量页：拉一次数据 + 起 3 秒轮询（离开页面自动停）
function enterTraffic() {
  if (TRAFFIC.timer) return;
  TRAFFIC.timer = setInterval(async () => {
    if (page !== "traffic") { clearInterval(TRAFFIC.timer); TRAFFIC.timer = 0; return; }
    // 正在输入/点按钮时不要重绘，否则搜索框会丢焦点
    const ae = document.activeElement;
    if (ae && (ae.tagName === "INPUT" || ae.tagName === "BUTTON")) return;
    await loadTraffic();
    const main = document.getElementById("page");
    if (main && page === "traffic") {
      const top = main.scrollTop;
      main.replaceChildren(pageTraffic());
      main.scrollTop = top;
    }
  }, 3000);
}

function statCard(label, value, cls) {
  const d = h("div", "stat");
  d.append(h("div", "k", label));
  d.append(h("div", "v" + (cls ? " " + cls : ""), value));
  return d;
}

function trafficRows() {
  const flows = (TRAFFIC.net.flows || []).slice();
  const q = TRAFFIC.q.trim().toLowerCase();
  const filtered = q
    ? flows.filter((f) => (f.name || "").toLowerCase().includes(q) || (f.path || "").toLowerCase().includes(q))
    : flows;
  if (TRAFFIC.sort === "name") {
    filtered.sort((a, b) => (a.name || "").localeCompare(b.name || ""));
  } else {
    filtered.sort((a, b) => (b.up + b.down) - (a.up + a.down));
  }
  return filtered;
}

function pageTraffic() {
  const frag = document.createDocumentFragment();
  frag.append(h("h1", "", "流量管理"), h("p", "sub", "按进程的历史累计与联网控制 · 数据来自 ETW / SRUM 采样"));

  enterTraffic();

  const net = TRAFFIC.net || {};
  const rows = trafficRows();
  const blockedCount = (net.flows || []).filter((f) => f.blocked).length;

  // 概览
  const stats = h("div", "stats");
  stats.append(statCard("累计上行", fmtBytes(net.up), "up"));
  stats.append(statCard("累计下行", fmtBytes(net.down), "down"));
  stats.append(statCard("有记录的进程", `${net.count ?? rows.length}`));
  stats.append(statCard("已阻止联网", `${blockedCount}`));
  frag.append(stats);

  if (!TRAFFIC.alive) {
    frag.append(h("p", "sub", "常驻进程没有运行，下面是上次退出前记录的数据。"));
  } else if (!TRAFFIC.admin) {
    frag.append(h("p", "sub", "当前是普通权限：可以查看统计，但「阻止 / 解除」需要以管理员身份运行驻匣。"));
  }

  // 工具条
  const bar = h("div", "toolbar");
  const search = document.createElement("input");
  search.type = "search";
  search.placeholder = "搜索进程名或路径";
  search.value = TRAFFIC.q;
  search.oninput = () => {
    TRAFFIC.q = search.value;
    const main = document.getElementById("page");
    const top = main.scrollTop;
    main.replaceChildren(pageTraffic());
    main.scrollTop = top;
    const again = main.querySelector('input[type="search"]');
    if (again) { again.focus(); again.setSelectionRange(again.value.length, again.value.length); }
  };
  bar.append(search);

  const sortSeg = seg(
    [["traffic", "按流量"], ["name", "按名称"]],
    TRAFFIC.sort,
    (x) => { TRAFFIC.sort = x; render(); },
  );
  bar.append(sortSeg);

  const refresh = h("button", "btn-mini", "刷新");
  refresh.onclick = async () => { await loadTraffic(); render(); };
  bar.append(refresh);

  const clear = h("button", "btn-mini danger", "清空统计");
  let armed = false;
  clear.onclick = async () => {
    // WebView2 里的 confirm() 行为不稳定，改成"再点一次确认"
    if (!armed) {
      armed = true;
      clear.textContent = "确认清空？";
      setTimeout(() => { armed = false; clear.textContent = "清空统计"; }, 3200);
      return;
    }
    armed = false;
    clear.textContent = "清空统计";
    await inv("reset_net_stats");
    toast("已请求清空，约 1 秒后生效");
    setTimeout(async () => { await loadTraffic(); render(); }, 1500);
  };
  clear.disabled = !TRAFFIC.alive;
  bar.append(clear);
  frag.append(bar);

  // 列表
  const list = h("div", "nlist");
  const head = h("div", "head");
  head.append(h("div", "c-name", "进程"));
  head.append(h("div", "c-num", "累计上行"));
  head.append(h("div", "c-num", "累计下行"));
  head.append(h("div", "c-act", "联网"));
  list.append(head);

  if (!rows.length) {
    const hint = TRAFFIC.q
      ? "没有匹配的进程"
      : (TRAFFIC.admin
        ? "还没有记录到流量（ETW / SRUM 需要一点时间才有数据）"
        : "按进程流量需要管理员权限。请以管理员身份运行驻匣。");
    list.append(h("div", "empty", hint));
  }

  for (const f of rows) {
    const item = h("div", "item" + (f.blocked ? " blocked" : ""));
    const nameBox = h("div", "c-name");
    const n = h("div", "n", f.name || "(未知进程)");
    if (f.blocked) {
      const tag = h("span", "tag-block", " 已阻止");
      n.append(tag);
    }
    nameBox.append(n);
    if (f.path) nameBox.append(h("div", "p", f.path));
    item.append(nameBox);
    item.append(h("div", "c-num", fmtBytes(f.up)));
    item.append(h("div", "c-num", fmtBytes(f.down)));

    const act = h("div", "c-act");
    const btn = h("button", "btn-mini" + (f.blocked ? "" : ""), f.blocked ? "解除" : "阻止");
    if (f.blocked) btn.classList.add("danger");
    btn.disabled = !f.path || !TRAFFIC.admin;
    btn.title = !f.path ? "读不到可执行文件路径" : (!TRAFFIC.admin ? "需要管理员权限" : "");
    btn.onclick = async () => {
      btn.disabled = true;
      btn.textContent = "…";
      try {
        const r = await inv("set_firewall", { path: f.path, block: !f.blocked });
        if (!r.ok) {
          toast("操作失败：需要管理员权限，或被系统策略拦截");
        } else {
          toast(f.blocked ? "已解除阻止" : "已加入出站阻止");
          // 乐观更新：runtime.json 是常驻进程每 5 秒才写的，先让界面立刻反馈
          const row = (TRAFFIC.net.flows || []).find((x) => x.path === f.path);
          if (row) row.blocked = !f.blocked;
          if (TRAFFIC.net.blocked != null) {
            TRAFFIC.net.blocked = (TRAFFIC.net.flows || []).filter((x) => x.blocked).length;
          }
        }
      } catch (e) {
        /* 全局 inv 已提示 */
      }
      await loadTraffic();
      render();
    };
    act.append(btn);
    item.append(act);
    list.append(item);
  }
  frag.append(list);

  frag.append(h("p", "sub",
    "阻止规则会写进 Windows 防火墙（出站、禁止该程序的全部网络访问），规则名形如 "
    + "“Zhuxia Block 程序名”，可在 wf.msc 里查看或手动删除。解除阻止只是删掉这条规则。"));

  return frag;
}

