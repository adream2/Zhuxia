/* ---------- 图标提取（自 app.js 拆出） ---------- */

/* 操作页的临时状态（同图片转换：只活在面板进程里，不落盘） */
const AICON = { paths: [], mode: "png", items: null, busy: false, error: "" };

async function appiconPick() {
  try {
    const r = await inv("appicon_pick");
    if (r && r.paths && r.paths.length) {
      const seen = new Set(AICON.paths);
      r.paths.forEach((p) => { if (!seen.has(p)) { seen.add(p); AICON.paths.push(p); } });
      render();
    }
  } catch (e) { /* 全局 inv 已提示 */ }
}

async function appiconStart() {
  if (!AICON.paths.length || AICON.busy) return;
  AICON.busy = true;
  AICON.items = null;
  AICON.error = "";
  render();
  try {
    const r = await inv("appicon_run", { paths: AICON.paths, mode: AICON.mode });
    AICON.items = r.items || [];
    if (r.error) AICON.error = r.error;
  } catch (e) {
    AICON.error = String(e);
  }
  AICON.busy = false;
  render();
}

function pageAppicon() {
  const v = S.appicon || {};
  const frag = document.createDocumentFragment();
  frag.append(h("h1", "", "图标提取"));
  frag.append(h("p", "sub",
    "选程序、快捷方式或安装包 → 提取图标，生成 PNG / ICO / 全尺寸图集；右键菜单用的是同一套设置。全部本地解析，不上传。"));

  /* ---- 操作页 ---- */
  const c0 = h("div", "card");
  const pick = h("button", "btn primary", "选择文件");
  pick.onclick = appiconPick;
  const clear = h("button", "btn", "清空");
  clear.onclick = () => { AICON.paths = []; AICON.items = null; render(); };
  const ctl = h("div", "ctl");
  ctl.append(pick, clear);
  c0.append(row("文件", "exe / dll / lnk / msi / apk / ipa / dmg / deb / rpm / ico…", ctl));

  const chipsRow = h("div", "row");
  const chipsText = h("div", "text");
  chipsText.append(h("div", "label", "已选 " + AICON.paths.length + " 个"));
  if (!AICON.paths.length) {
    chipsText.append(h("span", "hint", "（还没选文件，点「选择文件」）"));
  } else {
    const box = h("div", "chips files");
    AICON.paths.forEach((p) => {
      const c = h("button", "chip on", baseName(p) + " ×");
      c.title = p;
      c.onclick = () => { AICON.paths = AICON.paths.filter((x) => x !== p); render(); };
      box.append(c);
    });
    chipsText.append(box);
  }
  chipsRow.append(chipsText);
  c0.append(chipsRow);

  const run = h("button", "btn primary", AICON.busy ? "提取中…" : "开始提取");
  run.disabled = AICON.busy || !AICON.paths.length;
  run.onclick = appiconStart;
  const runCtl = h("div", "ctl");
  runCtl.append(seg([["png", "PNG"], ["ico", "ICO"], ["all", "全部尺寸"]], AICON.mode,
    (x) => { AICON.mode = x; render(); }), run);
  c0.append(row("输出形态", "右键菜单里每次还能另选；这里决定这次提取什么", runCtl));

  if (AICON.error) c0.append(row("出错", AICON.error, null, false, h("span", "hint", "看 logs/ 里的记录")));
  if (AICON.items && AICON.items.length) {
    const box = h("div", "row");
    const t = h("div", "text");
    t.append(h("div", "label", "结果"));
    t.append(convSummary(AICON.items), resultList(AICON.items));
    const firstOk = AICON.items.find((x) => x.ok && x.outputs && x.outputs.length);
    if (firstOk) {
      const ob = h("button", "btn", "打开输出目录");
      ob.onclick = () => {
        const s = String(firstOk.outputs[0]);
        const i = Math.max(s.lastIndexOf("\\"), s.lastIndexOf("/"));
        inv("open_folder", { path: i > 0 ? s.slice(0, i) : s }).then((r) => {
          if (!r || !r.ok) toast("打开失败：" + ((r && r.error) || "目录不存在"));
        });
      };
      const c = h("div", "ctl");
      c.append(ob);
      box.append(t, c);
    } else {
      box.append(t);
    }
    c0.append(box);
  }
  frag.append(c0);

  /* ---- 右键菜单 ---- */
  frag.append(h("h2", "", "资源管理器右键菜单"));
  const c1 = h("div", "card");
  c1.append(row("状态",
    v.installed
      ? "已注册：右键 exe / apk / dmg 等 →「提取应用图标」（Win11 在「显示更多选项」里）"
      : "未注册：打开下面的开关会自动写入注册表",
    sw(v.enabled, (x) => apatch({ enabled: x }))));
  const btn = h("button", "btn", "重新注册");
  btn.onclick = async () => {
    try {
      S.appicon = await inv("appicon_menu", { on: true });
      render();
      toast("菜单已刷新");
    } catch (e) { /* 全局 inv 已提示 */ }
  };
  c1.append(row("刷新菜单", "换了 exe 路径、或菜单没出现时点这里重写注册表", btn));
  c1.append(row("挂载扩展名", "每个格式一个父菜单，共用同一个三选一子菜单", null, false,
    h("span", "hint", (v.exts || 0) + " 个")));
  frag.append(c1);

  /* ---- 默认输出 ---- */
  frag.append(h("h2", "", "默认输出"));
  const c2 = h("div", "card");
  const modes = (v.modes || []).map((m) => [m.id, m.label]);
  c2.append(row("形态", "右键菜单里每次都还能另选；这里只决定默认值",
    seg(modes, v.mode || "png", (x) => apatch({ mode: x }))));
  c2.append(row("PNG 最长边", "16–4096，只缩不放；图标一般 512 足够，要原图就填 4096",
    textInput(String(v.maxSize == null ? 512 : v.maxSize), "512", (x) => {
      const n = Math.max(16, Math.min(4096, parseInt(x, 10) || 512));
      apatch({ maxSize: n });
    })));
  c2.append(row("ICO 规格", "输出 .ico 时塞这几档；超过原生尺寸的档位不会放大生成",
    chips(v.icoOptions || [16, 24, 32, 48, 64, 128, 256], v.icoSizes || [256, 128, 64, 48, 32, 16],
      (list) => apatch({ icoSizes: list }))));
  c2.append(row("覆盖同名文件", "关闭时自动改叫「名字 (2).png」，绝不覆盖原文件",
    sw(v.overwrite, (x) => apatch({ overwrite: x }))));
  c2.append(row("输出目录", "留空 = 与源文件同目录；填绝对路径则统一输出到那里",
    textInput(v.outDir || "", "默认：源文件所在目录", (x) => apatch({ outDir: x }))));
  frag.append(c2);

  frag.append(h("h2", "", "支持范围"));
  const c3 = h("div", "card");
  (v.formats || []).forEach((line) => c3.append(h("div", "hint", "· " + line)));
  frag.append(c3);

  frag.append(h("h2", "", "说明"));
  const c4 = h("div", "card");
  c4.append(h("div", "hint", "· 先看文件内容再看扩展名：改名成 .bin 的 exe、扩展名写错的安装包照样能认出来。"));
  c4.append(h("div", "hint", "· 多分辨率：PE / ICO / ICNS / APK / deb 有多少档收多少档，「全部尺寸」会把每一档单独存成 icon-宽x高.png。"));
  c4.append(h("div", "hint", "· 只缩不放：原生 32px 的图标不会生成 256px 的糊图；ICO 里缺的档位从最大帧缩，原生同尺寸帧优先。"));
  c4.append(h("div", "hint", "· 不支持或解不开：明确提示「不支持 / 没有图标 / 已加密 / 需要 APFS」，不会静默失败，也不会拿一张无关的图凑数。"));
  frag.append(c4);
  return frag;
}
