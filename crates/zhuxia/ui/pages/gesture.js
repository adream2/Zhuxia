(() => {
/* ---------- 手势（自 app.js 拆出） ---------- */

/* ---------- 手势 ---------- */

/// 视图里的规则（箭头串 + 动作名）→ 配置格式。
function toCfgRules(list) {
  return (list || []).map((r) => {
    const action = { type: r.action.type };
    if (r.action.type === "hotkey") {
      action.keys = (r.action.keys || []).filter(Boolean);
    } else if (["window", "volume", "internal"].includes(r.action.type)) {
      action.operation = r.action.operation;
    } else {
      action.path = r.action.path || "";
    }
    return {
      name: r.name,
      scope: r.scope || "global",
      button: r.button,
      pattern: [r.pattern],
      wheel: r.wheel || null,
      executeOnWheel: !!r.onWheel,
      action,
    };
  });
}

const ACTION_KINDS = [
  ["hotkey", "快捷键"], ["window", "窗口"], ["volume", "音量"],
  ["internal", "内置"], ["program", "运行程序"],
];

const WINDOW_OPS = [["toggle-maximize", "最大化/还原"], ["minimize", "最小化"], ["close", "关闭"], ["toggle-topmost", "切换置顶"]];
const VOLUME_OPS = [["increase", "音量 +"], ["decrease", "音量 −"], ["mute", "静音"]];
const INTERNAL_OPS = [
  ["capture", "截图"], ["clipboard", "剪贴板面板"], ["imgbed", "图床"], ["devtools", "开发工具"],
  ["settings", "打开设置"], ["toggle-exclude", "排除当前程序"], ["close-similar", "关闭相似窗口"],
  ["edit-hosts", "编辑 hosts"],
];

/// 动作参数控件：返回 { el, read, key }，由调用方在 onchange 时把值写回规则。
function actionArgsFor(type, r) {
  if (type === "hotkey") {
    const input = document.createElement("input");
    input.type = "text";
    input.className = "garg";
    input.placeholder = "Alt+Left";
    input.value = (r.action.keys || []).join("+");
    return { el: input, key: "keys", read: () => input.value.split("+").map((s) => s.trim()).filter(Boolean) };
  }
  if (type === "program") {
    const input = document.createElement("input");
    input.type = "text";
    input.className = "garg";
    input.placeholder = "C:\\path\\app.exe";
    input.value = r.action.path || "";
    return { el: input, key: "path", read: () => input.value.trim() };
  }
  const opts = type === "window" ? WINDOW_OPS : type === "volume" ? VOLUME_OPS : INTERNAL_OPS;
  const s = sel(opts, r.action.operation, () => {});
  s.className = "garg";
  return { el: s, key: "operation", read: () => s.value };
}

/// 规则表编辑器：list 是视图格式，onSave 收到编辑后的整张表。
function rulesEditor(list, onSave, opts) {
  opts = opts || {};
  const card = h("div", "card grules");
  const rules = (list || []).map((r) => JSON.parse(JSON.stringify(r)));
  const save = () => onSave(rules);

  rules.forEach((r, i) => {
    const line = h("div", "grule");

    const pat = document.createElement("input");
    pat.type = "text";
    pat.className = "gpat";
    pat.value = r.pattern;
    pat.title = "方向串：←↓ / left,down / ld 都可以";
    pat.onchange = () => { r.pattern = pat.value.trim(); save(); };

    const name = document.createElement("input");
    name.type = "text";
    name.className = "gname";
    name.value = r.name || "";
    name.placeholder = "名称";
    name.onchange = () => { r.name = name.value.trim(); save(); };

    const scopeKind = r.scope.startsWith("app:") ? "app" : r.scope.startsWith("category:") ? "category" : "global";
    const scopeSel = sel([["global", "通用"], ["app", "程序"], ["category", "分类"]], scopeKind, (v) => {
      const cur = r.scope.includes(":") ? r.scope.slice(r.scope.indexOf(":") + 1) : "";
      r.scope = v === "global" ? "global" : v + ":" + cur;
      save();
    });
    scopeSel.className = "gscope";

    const scopeVal = document.createElement("input");
    scopeVal.type = "text";
    scopeVal.className = "gval";
    scopeVal.value = scopeKind === "global" ? "" : r.scope.slice(scopeKind.length + 1);
    scopeVal.placeholder = scopeKind === "category" ? "分类名" : "进程名";
    scopeVal.disabled = scopeKind === "global";
    scopeVal.onchange = () => {
      r.scope = scopeKind === "global" ? "global" : scopeKind + ":" + scopeVal.value.trim();
      save();
    };

    const btnSel = sel([["right", "右键"], ["middle", "中键"]], r.button, (v) => { r.button = v; save(); });
    btnSel.className = "gbtn";

    const wheelSel = sel([["", "无滚轮"], ["up", "滚轮↑"], ["down", "滚轮↓"]], r.wheel || "", (v) => {
      r.wheel = v;
      save();
    });
    wheelSel.className = "gwheel";

    const actSel = sel(ACTION_KINDS, r.action.type, (v) => {
      r.action = v === "hotkey" ? { type: "hotkey", keys: ["Alt", "Left"] }
        : v === "window" ? { type: "window", operation: "toggle-maximize" }
        : v === "volume" ? { type: "volume", operation: "increase" }
        : v === "internal" ? { type: "internal", operation: "capture" }
        : { type: "program", path: "" };
      save();
    });
    actSel.className = "gact";

    const arg = actionArgsFor(r.action.type, r);
    arg.el.onchange = () => {
      r.action[arg.key] = arg.read();
      save();
    };

    const del = h("button", "btn-mini danger", "删除");
    del.onclick = () => { rules.splice(i, 1); save(); };

    // 分组内的规则不需要"作用域"（组就是作用域），紧凑模式里不显示这两个控件
    if (opts.compact) line.append(pat, name, btnSel, wheelSel, actSel, arg.el, del);
    else line.append(pat, name, scopeSel, scopeVal, btnSel, wheelSel, actSel, arg.el, del);
    card.append(line);
  });

  if (!rules.length) card.append(h("div", "empty", "还没有规则"));

  const foot = h("div", "gfoot");
  const add = h("button", "btn", "新增规则");
  add.onclick = () => {
    rules.push({
      name: "新规则", scope: "global", button: "right", pattern: "→", wheel: "",
      onWheel: false, action: { type: "hotkey", keys: ["Alt", "Left"] },
    });
    save();
  };
  foot.append(add);
  if (opts.compact) {
    foot.append(h("span", "hint", "组内规则优先于通用规则"));
  } else {
    const reset = h("button", "btn", "恢复默认");
    reset.onclick = () => gpatch({ rules: null });
    foot.append(reset, h("span", "hint", opts.explicit ? "自定义规则表" : "当前是内置默认规则，改任意一条即转成自定义"));
  }
  card.append(foot);
  return card;
}

function saveGestureRules(list) {
  gpatch({ rules: toCfgRules(list) });
}

function saveGestureGroups(groups) {
  gpatch({
    groups: groups.map((x) => ({
      name: x.name,
      apps: x.apps,
      rules: toCfgRules(x.rules),
    })),
  });
}

function gestureGroups() {
  const g = S.gesture;
  const groups = (g.groups || []).map((x) => JSON.parse(JSON.stringify(x)));
  const frag = document.createDocumentFragment();

  groups.forEach((gr, gi) => {
    const card = h("div", "card ggroup");

    const head = h("div", "ghead");
    const name = document.createElement("input");
    name.type = "text";
    name.className = "gname2";
    name.value = gr.name || "";
    name.placeholder = "分组名，例如 浏览器";
    name.onchange = () => { gr.name = name.value.trim() || "未命名分组"; saveGestureGroups(groups); };
    const del = h("button", "btn-mini danger", "删除分组");
    del.onclick = () => { groups.splice(gi, 1); saveGestureGroups(groups); };
    head.append(h("span", "gdot", "◆"), name, del);
    card.append(head);

    const apps = document.createElement("textarea");
    apps.rows = 3;
    apps.placeholder = "一行一个进程名，例如\nchrome\nmsedge\n360*";
    apps.value = (gr.apps || []).join("\n");
    apps.onchange = () => {
      gr.apps = apps.value.split("\n").map((s) => s.trim()).filter(Boolean);
      saveGestureGroups(groups);
    };
    card.append(row("适用程序", "进程名，不分大小写；`360*` 这种星号结尾表示前缀匹配", apps));

    card.append(rulesEditor(gr.rules || [], (next) => {
      gr.rules = next;
      saveGestureGroups(groups);
    }, { compact: true }));

    frag.append(card);
  });

  if (!groups.length) {
    frag.append(h("p", "sub", "还没有分组：所有程序都用通用规则。"));
  }

  const bar = h("div", "toolbar");
  const add = h("button", "btn primary", "新增分组");
  add.onclick = () => {
    groups.push({
      name: "新分组",
      apps: [],
      rules: [{
        name: "新规则", scope: "global", button: "right", pattern: "↓", wheel: "", onWheel: false,
        action: { type: "hotkey", keys: ["Ctrl", "W"] },
      }],
    });
    saveGestureGroups(groups);
  };
  const reset = h("button", "btn", "恢复内置浏览器分组");
  reset.onclick = () => gpatch({ groups: null });
  bar.append(add, reset, h("span", "hint", groups.length + " 个分组"));
  frag.append(bar);
  return frag;
}

function gestureExcluded() {
  const list = (S.gesture.excludedApps || []).slice();
  const card = h("div", "card");

  const chips = h("div", "chips");
  if (!list.length) {
    chips.append(h("span", "hint", "（空）"));
  }
  list.forEach((app) => {
    const chip = h("button", "chip on", app + " ×");
    chip.title = "点击恢复该程序的手势";
    chip.onclick = () => gpatch({ excludedApps: list.filter((x) => x !== app) });
    chips.append(chip);
  });
  card.append(row("已排除", "这些程序里不响应手势；点标签恢复", chips));

  const input = document.createElement("input");
  input.type = "text";
  input.placeholder = "进程名，回车添加";
  input.onchange = () => {
    const v = input.value.trim();
    if (v) gpatch({ excludedApps: list.concat([v]) });
  };
  card.append(row("添加排除", "例如 chrome / keepass（`→↙→` 手势也能加）", input));
  return card;
}

function pageGesture() {
  const g = S.gesture;
  const rt = g.runtime || {};
  const frag = document.createDocumentFragment();
  frag.append(h("h1", "", "鼠标手势"), h("p", "sub", HINTS.gesture[0]));

  const status = !g.enabled ? "手势模块已禁用"
    : !rt.alive ? "常驻进程没有运行：改动会在下次启动生效"
    : "钩子" + (rt.hook ? "已安装" : "未安装") + " · 覆盖层" + (rt.overlay ? "就绪" : "未就绪")
      + " · 通用 " + (rt.rules ?? g.rules.length) + " 条 · 分组 " + (rt.groups ?? (g.groups || []).length) + " 个";
  frag.append(h("p", "sub", status));

  frag.append(h("h2", "", "开关"));
  const c1 = h("div", "card");
  c1.append(row("启用手势", "按住右键（可另开中键）划动触发动作；启用/禁用需重启驻匣", sw(g.enabled, (x) => gpatch({ enabled: x }))));
  c1.append(row("暂时停用", "保留配置，只是不再响应手势", sw(g.paused, (x) => gpatch({ paused: x })), !g.enabled));
  c1.append(row("中键手势", "中键还要用于粘贴与自动滚动，默认关闭", sw(g.middleButton, (x) => gpatch({ middleButton: x })), !g.enabled));
  c1.append(row("全屏时禁用", "只对真·全屏应用生效（最大化窗口不算）", sw(g.disableInFullscreen, (x) => gpatch({ disableInFullscreen: x })), !g.enabled));
  c1.append(row("桌面上禁用", "桌面空白处与图标上不响应", sw(g.disableOnDesktop, (x) => gpatch({ disableOnDesktop: x })), !g.enabled));
  c1.append(row("动作提示", "命中规则时在屏幕底部显示动作名", sw(g.hint, (x) => gpatch({ hint: x })), !g.enabled));
  frag.append(c1);

  frag.append(h("h2", "", "手感"));
  const c2 = h("div", "card");
  const range = document.createElement("input");
  range.type = "range";
  range.min = "0"; range.max = "200"; range.step = "5";
  range.value = String(g.sensitivity);
  const lab = h("span", "", g.sensitivity + "%");
  lab.style.color = "var(--text-2)";
  lab.style.width = "46px";
  range.oninput = () => { lab.textContent = range.value + "%"; };
  range.onchange = () => gpatch({ sensitivity: parseInt(range.value, 10) });
  const rangeCtl = h("div", "ctl");
  rangeCtl.append(range, lab);
  c2.append(row("灵敏度", "100 = 原版手感；越高越容易识别出方向与转角", rangeCtl, !g.enabled));
  c2.append(row("动作目标窗口", "起始窗口 = 手势开始时鼠标下的窗口；当前窗口 = 执行时的前台窗口",
    seg([["start-window", "起始窗口"], ["current-window", "当前窗口"]], g.targetWindow,
      (x) => gpatch({ targetWindow: x })), !g.enabled));
  frag.append(c2);

  frag.append(h("h2", "", "轨迹线"));
  const c3 = h("div", "card");
  const colorRow = (label, hint, value, key) => {
    const i = document.createElement("input");
    i.type = "color";
    i.value = value;
    i.onchange = () => gpatch({ trail: Object.assign({}, g.trail, { [key]: i.value }) });
    const v = h("span", "val", value);
    v.style.color = "var(--text-2)";
    const w = h("div", "ctl");
    w.append(i, v);
    return row(label, hint, w, !g.enabled);
  };
  c3.append(colorRow("已匹配", "方向串命中规则时的颜色", g.trail.activeColor, "activeColor"));
  c3.append(colorRow("未匹配", "还没命中时的颜色", g.trail.inactiveColor, "inactiveColor"));
  const thick = document.createElement("input");
  thick.type = "number";
  thick.min = "1"; thick.max = "12";
  thick.value = String(g.trail.thickness);
  thick.style.width = "72px";
  thick.onchange = () => gpatch({ trail: Object.assign({}, g.trail, { thickness: parseInt(thick.value, 10) }) });
  c3.append(row("线宽", "1–12（逻辑像素，按 DPI 缩放）", thick, !g.enabled));
  frag.append(c3);

  frag.append(h("h2", "", "边缘滚动"));
  frag.append(h("p", "sub", "鼠标贴在屏幕边上滚滚轮：左边调音量、右边调亮度（这两条边默认就开）；上边切标签、下边切程序。四条边各自独立开关，关着的边滚轮照常滚页面。"));
  const ce = h("div", "card");
  const ed = g.edges || {};
  const EDGE_OPTS = [["volume", "音量"], ["brightness", "亮度"], ["tab", "标签切换"], ["app", "程序切换"], ["none", "不接管"]];
  const edgeRow = (key, label, hint, fallback, defaultOn) => {
    const cur = Object.assign({ enabled: !!defaultOn, action: fallback }, ed[key] || {});
    const ctl = h("div", "ctl");
    ctl.append(seg(EDGE_OPTS, cur.action || fallback,
      (v) => gpatch({ edges: Object.assign({}, ed, { [key]: Object.assign({}, cur, { action: v }) }) })));
    ctl.append(sw(!!cur.enabled && cur.action !== "none",
      (x) => gpatch({ edges: Object.assign({}, ed, { [key]: Object.assign({}, cur, { enabled: x }) }) })));
    return row(label, hint, ctl, !g.enabled);
  };
  ce.append(edgeRow("left", "左边", "音量 ±（走系统音量键，OSD 由系统画）", "volume", true));
  ce.append(edgeRow("right", "右边", "亮度 ±5%（DDC/CI；笔记本内屏一般不支持，见日志）", "brightness", true));
  ce.append(edgeRow("top", "上边", "切标签：Ctrl+Tab / Ctrl+Shift+Tab", "tab", false));
  ce.append(edgeRow("bottom", "下边", "切程序：Alt+Tab / Alt+Shift+Tab", "app", false));
  frag.append(ce);

  frag.append(h("h2", "", "按键回显"));
  frag.append(h("p", "sub", "在屏幕上把按下的键显示成一排键帽，演示和录屏用。手势自己注入的快捷键（如 Ctrl+Tab）不会回显。"));
  const ck = h("div", "card");
  const ke = Object.assign({ enabled: false, position: "bottom-center", durationMs: 1200, fontSize: 15 }, g.keyEcho || {});
  ck.append(row("启用回显", "全局生效，不需要按住手势键；与手势模块的启停无关", sw(!!ke.enabled,
    (x) => gpatch({ keyEcho: Object.assign({}, ke, { enabled: x }) }))));
  ck.append(row("位置", "离屏幕边 48px，跟随虚拟桌面（多屏按外接矩形算）",
    seg([["top-left", "上左"], ["top-center", "上中"], ["top-right", "上右"],
         ["bottom-left", "下左"], ["bottom-center", "下中"], ["bottom-right", "下右"]],
      ke.position || "bottom-center",
      (x) => gpatch({ keyEcho: Object.assign({}, ke, { position: x }) }))));
  const keDur = document.createElement("input");
  keDur.type = "number";
  keDur.min = "400"; keDur.max = "6000"; keDur.step = "100";
  keDur.value = String(ke.durationMs || 1200);
  keDur.style.width = "84px";
  keDur.onchange = () => gpatch({ keyEcho: Object.assign({}, ke, { durationMs: parseInt(keDur.value, 10) }) });
  ck.append(row("显示时长", "一个键在屏幕上留多久（400–6000 ms）", keDur));
  const keFont = document.createElement("input");
  keFont.type = "number";
  keFont.min = "9"; keFont.max = "32";
  keFont.value = String(ke.fontSize || 15);
  keFont.style.width = "72px";
  keFont.onchange = () => gpatch({ keyEcho: Object.assign({}, ke, { fontSize: parseInt(keFont.value, 10) }) });
  ck.append(row("字号", "键帽字号（9–32，按 DPI 缩放）", keFont));
  frag.append(ck);

  frag.append(h("h2", "", "通用手势"));
  frag.append(h("p", "sub", "所有程序默认用这套。`→↙→` 是内置的「排除本程序」，删掉它就没法一键排除。"));
  frag.append(rulesEditor(g.rules, saveGestureRules, { explicit: g.rulesExplicit }));

  frag.append(h("h2", "", "分组手势"));
  frag.append(h("p", "sub", "命中分组的程序优先用组内规则；组里没有这个手势才落回通用。"));
  frag.append(gestureGroups());

  frag.append(h("h2", "", "排除的程序"));
  frag.append(gestureExcluded());

  return frag;
}

window.pageGesture = pageGesture;
})();
