/* ---------- 截图：OCR 与保存（自 app.js 拆出） ---------- */

/* ---------- 截图 ---------- */
const cappatch = (obj) => save("capture", obj);

/* OCR 增强模型（PaddleOCR，可选下载） */
let OCRP = null;
let OCR_LOADING = false;
let OCR_TIMER = 0;
let OCR_SRC = "auto";
let OCR_CUSTOM = "";
/// 下载状态。进度条只在"真的在下载（或刚失败）"时出现——以前这一行是常驻的，
/// 一进页面就看到一根空进度条，像是已经下了一半，其实什么都没开始。
let OCR_DL = { running: false, error: "" };

async function loadOcrModel() {
  try { OCRP = await inv("ocr_model_state"); } catch (e) { OCRP = null; }
}

function fmtMB(n) { return (n / 1048576).toFixed(1) + " MB"; }

function stopOcrPoll() {
  if (OCR_TIMER) clearInterval(OCR_TIMER);
  OCR_TIMER = 0;
}

function startOcrPoll() {
  stopOcrPoll();
  // 下载线程刚启动时 RUNNING 还是 false（它是在工作线程里置位的）。
  // 所以"第一次看到 running=false"不能当作完成，否则轮询会在 400ms 就自己停掉。
  let seenRunning = false;
  let idleTicks = 0;
  OCR_TIMER = setInterval(async () => {
    let p = null;
    try { p = await inv("ocr_model_progress"); } catch (e) { p = null; }
    const bar = document.getElementById("ocr-progress");
    const label = document.getElementById("ocr-progress-label");
    if (!p) return;
    if (p.running) { seenRunning = true; idleTicks = 0; }
    const pct = p.total > 0
      ? Math.min(100, Math.round((p.done / p.total) * 100))
      : (p.running ? 8 : 100);
    if (bar) bar.style.width = pct + "%";
    if (label) {
      label.textContent = p.error ? "失败：" + p.error
        : p.running ? (p.stage || "下载中") + " " + pct + "%"
        : seenRunning ? "已完成" : "启动下载…";
    }
    const finished = !p.running && (seenRunning || !!p.error);
    if (!finished) {
      idleTicks += 1;
      // 10 秒还没见过 running：线程没起来，别再空转
      if (idleTicks > 25) {
        stopOcrPoll();
        OCR_DL.running = false;
        OCR_DL.error = "下载线程没有启动，请重试或看 logs/";
        if (page === "capture") render();
      }
      return;
    }
    stopOcrPoll();
    OCR_DL.running = false;
    OCR_DL.error = p.error || "";
    await loadOcrModel();
    if (page === "capture") render();
  }, 400);
}


/* ---------- 截图：页面（自 app.js 拆出） ---------- */

function pageCapture() {
  const v = S.capture || {};
  if (!OCRP && !OCR_LOADING) {
    OCR_LOADING = true;
    loadOcrModel().then(() => { OCR_LOADING = false; if (page === "capture") render(); });
  }
  const frag = document.createDocumentFragment();
  frag.append(h("h1", "", "截图工具"));
  frag.append(h("p", "sub",
    "冻结屏幕 → 框选/单击窗口 → 标注 → 复制/保存/贴图。美化、拼接、长截图、文字识别都在编辑器工具条里，全部本地完成。"));

  const c0 = h("div", "card");
  c0.append(row("启用截图模块", "关闭后不注册热键（重启生效）", sw(v.enabled, (x) => cappatch({ enabled: x }))));
  const hkHint = v.hotkeyError
    ? "⚠ " + v.hotkeyError + (v.hotkeyActive ? "，已临时改用 " + v.hotkeyActive : "，截图目前无法用热键唤起")
    : (v.hotkeyActive
        ? "已注册：" + v.hotkeyActive + "；解析规则与剪贴板模块共用，必须带修饰键"
        : "默认 Alt+A；解析规则与剪贴板模块共用，必须带修饰键");
  c0.append(row("截图热键", hkHint,
    textInput(v.hotkey, "Alt+A", (x) => cappatch({ hotkey: x }))));
  const openDir = h("button", "btn", "打开目录");
  openDir.onclick = () => inv("capture_open_dir");
  c0.append(row("保存目录", "留空 = 图片\\Zhuxia（默认）",
    textInput(v.saveDirRaw, "默认：" + (v.saveDir || "图片\\Zhuxia"), (x) => cappatch({ saveDir: x })), false, openDir));
  c0.append(row("复制/贴图时同时存盘", "每张都留一份文件，不用再手动保存", sw(v.autoSave, (x) => cappatch({ autoSave: x }))));
  frag.append(c0);

  frag.append(h("h2", "", "选区"));
  const c1 = h("div", "card");
  c1.append(row("放大镜", "跟随鼠标，显示像素与颜色值（#RRGGBB）", sw(v.magnifier, (x) => cappatch({ magnifier: x }))));
  c1.append(row("窗口识别", "高亮鼠标下的窗口/控件，滚轮在控件→窗口→屏幕之间切换", sw(v.detectWindow, (x) => cappatch({ detectWindow: x }))));
  frag.append(c1);

  frag.append(h("h2", "", "OCR 增强（PaddleOCR，可选）"));
  const oc = h("div", "card");
  const st = OCRP;
  const engine = st ? st.engine : "…";
  oc.append(row("当前引擎", st && st.installed
      ? "已装 PaddleOCR：中文、小字、表格明显更准；出错会自动回退系统 OCR"
      : "未装模型：用 Windows 自带 OCR（免下载，但中文识别弱一些）",
    h("b", "", engine)));
  oc.append(row("模型文件", st ? (st.dir || "") : "ocr/", null,
    false, st ? h("span", "hint", st.installed ? fmtMB(st.bytes) : "未安装（约 15 MB）") : null));

  // 下载源 + 下载按钮。
  // 这里以前只创建了按钮对象却没 append 到任何一行，页面上根本没有"下载"可点；
  // 而进度条是常驻的，所以看到的是一根不会动的空进度条。
  const dl = h("button", "btn primary", st && st.installed ? "重新下载" : "下载模型");
  dl.disabled = OCR_DL.running;
  dl.onclick = async () => {
    OCR_DL.running = true;
    OCR_DL.error = "";
    render(); // 先把「下载进度」这一行画出来，别让人以为点了没反应
    try {
      await inv("ocr_model_download", { source: OCR_SRC, custom: OCR_CUSTOM });
    } catch (e) {
      OCR_DL.running = false;
      OCR_DL.error = String(e);
      render();
      return;
    }
    startOcrPoll();
  };
  const srcSeg = seg([["auto", "自动"], ["hf", "官方"], ["custom", "自定义"]], OCR_SRC, (x) => {
    OCR_SRC = x;
    render();
  });
  const srcCtl = h("div", "ctl");
  srcCtl.append(srcSeg, dl);
  oc.append(row("下载源", "自动 = hf-mirror.com 优先、失败退官方；字典和 GitHub 地址自动走 ghproxy 加速",
    srcCtl));
  if (OCR_SRC === "custom") {
    oc.append(row("自定义地址", "填模型目录前缀（含 PP-OCRv4/ 的那一层），或 GitHub 地址（自动套镜像）",
      textInput(OCR_CUSTOM, "https://…/SWHL/RapidOCR/resolve/main", (x) => { OCR_CUSTOM = x; })));
  }

  // 进度条：只在下载中或失败后出现（平时不占一行）
  if (OCR_DL.running || OCR_DL.error) {
    const track = h("div");
    track.style.cssText = "height:6px;width:220px;background:rgba(128,128,128,.22);border-radius:3px;overflow:hidden";
    const pbar = h("div");
    pbar.id = "ocr-progress";
    pbar.style.cssText = "height:100%;width:0;background:#2e90fa;transition:width .2s";
    track.append(pbar);
    const plabel = h("div", "hint", OCR_DL.error ? "失败：" + OCR_DL.error : "准备中…");
    plabel.id = "ocr-progress-label";
    plabel.style.marginTop = "6px";
    const progWrap = h("div");
    progWrap.append(track, plabel);
    oc.append(row("下载进度",
      OCR_DL.error ? "可以点「重新下载」再试一次；详细原因见 logs/" : "下载完成后立即可用，常驻进程下次识别自动加载",
      progWrap));
  }

  // 导入
  const importPath = textInput("", "本地模型目录（含 det.onnx / rec.onnx / keys.txt）", () => {});
  const imp = h("button", "btn", "导入");
  imp.onclick = async () => {
    const dir = importPath.value.trim();
    if (!dir) { toast("先填目录"); return; }
    const r = await inv("ocr_model_import", { dir });
    if (r && r.ok) { toast("已导入"); await loadOcrModel(); render(); }
    else { toast("导入失败：" + ((r && r.error) || "文件不全")); }
  };
  const row2 = h("div", "row");
  const t2 = h("div", "text");
  t2.append(h("div", "label", "已有模型？"), h("div", "hint", "自行下载后放到任意目录，填路径导入"));
  const c2 = h("div", "ctl");
  c2.append(importPath, imp);
  row2.append(t2, c2);
  oc.append(row2);

  const del = h("button", "btn", "删除模型");
  del.disabled = !(st && st.installed);
  del.onclick = async () => {
    if (!confirm("删除已下载的 PaddleOCR 模型？（随时可以重新下载）")) return;
    await inv("ocr_model_remove");
    await loadOcrModel();
    render();
  };
  oc.append(row("清理",
    st && st.installed ? "只删 <exe>/ocr/ 下的三个文件，不碰配置" : "当前没有已下载的模型",
    del));

  frag.append(oc);

  frag.append(h("h2", "", "操作"));
  const help = h("div", "card");
  // 键名一列定宽、说明一列自适应：以前两个 div 直接平铺在 flex 行里，
  // 短标签（比如「美化」）会被长说明挤成一列一个字，看着像竖排。
  const item = (k, text) => {
    const r = h("div", "row helpitem");
    r.append(h("div", "label", k));
    r.append(h("div", "hint", text));
    return r;
  };
  help.append(item("拖动 / 单击", "拖动框选任意区域；单击直接截鼠标下的窗口"));
  help.append(item("松手之后", "拖内部移动、拖边角缩放、方向键微调（Shift 一次 10 像素）"));
  help.append(item("放大镜", "选区时跟随鼠标显示像素与颜色；右键直接把当前颜色值（#RRGGBB）复制到剪贴板"));
  help.append(item("Enter / 双击", "进入标注；在标注里再按一次 = 复制并退出"));
  help.append(item("Esc", "标注态：回到可调整选区（长截图/拼接成品没有对应屏幕区域，直接取消）；选区态：先清选区，再按一次退出"));
  help.append(item("滚轮", "选区时切换控件→窗口→屏幕；标注态把鼠标放到图形/箭头上直接改它的粗细，空白处改下一条的默认粗细"));
  help.append(item("V R O A L P H M T N", "选择、矩形、椭圆、箭头、直线、画笔、荧光笔、马赛克、文字、序号"));
  help.append(item("序号（N）", "选序号工具后在图上连续点，编号自动 1、2、3…；删除或撤销后自动补空，白底换深色数字"));
  help.append(item("Ctrl+Z / Shift+Ctrl+Z", "撤销 / 重做（标注可再次选中、移动、删除）"));
  help.append(item("Ctrl+C / Ctrl+S", "复制成品图 / 存 PNG 并退出"));
  help.append(item("工具条（图标）", "鼠标停在图标上看名字与快捷键；点当前颜色展开 8 色板 + 「自定义」，点外面收起"));
  help.append(item("美化", "背景（12 渐变/8 纯色/截图模糊/透明）、6 种窗口边框、边距/圆角/阴影、6 种比例；分「背景/边框/水印」三页，设置随复制/保存自动套用"));
  help.append(item("拼接", "点「拼接」后连续框选，滚轮切换纵向/横向/网格，Enter 出成品"));
  help.append(item("长截图", "选好区域按 Ctrl+L，关闭覆盖层后自行滚动页面，控制条上点「完成」"));
  help.append(item("识别", "本机 OCR：装了 PaddleOCR 就优先用它，否则退 Windows 自带引擎；结果窗口可 Ctrl+C 复制全部"));
  help.append(item("贴图", "置顶悬浮：滚轮缩放、拖动移动、双击关闭、右键菜单"));
  frag.append(help);

  return frag;
}

