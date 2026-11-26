(() => {
/* ---------- 截图：OCR 与保存（自 app.js 拆出） ---------- */

/* ---------- 截图 ---------- */
const cappatch = (obj) => save("capture", obj);

/* 探活：直接拿真实接口发一次请求，省得用户猜"到底通没通" */
function testBtn(kind) {
  const b = h("button", "btn", "测试");
  b.onclick = async () => {
    b.disabled = true;
    const old = b.textContent;
    b.textContent = "测试中…";
    try {
      const r = await inv("ocr_ai_probe", { kind });
      if (r && r.ok) toast("通了：" + String(r.text).slice(0, 40));
      else toast("失败：" + ((r && r.error) || "未知错误"));
    } catch (e) {
      toast("失败：" + String(e));
    }
    b.textContent = old;
    b.disabled = false;
  };
  return b;
}


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
    try { p = await inv("ocr_model_progress", undefined, { quiet: true }); } catch (e) { p = null; }
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
  if (v.uielemDisabled) {
    // 卡死的程序会把采集线程拖住，连续三次就自动停用（10 分钟后自恢复）。
    // 用户关掉那个程序后想立刻恢复，不该被迫重启整个托盘程序。
    const restore = h("button", "btn", "立即恢复");
    restore.onclick = async () => {
      const r = await inv("uielem_reset");
      if (r && r.state) { S.capture = r.state; render(); }
      toast("元素识别已恢复");
    };
    c1.append(row("元素识别已停用", "有程序不响应 UI 自动化，连续卡死 3 次后自动关闭（10 分钟后自动重试）。关掉那个程序后可以点右边立刻恢复", restore));
  }
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

  // ---- OCR 的翻译与 AI ----
  frag.append(h("h2", "", "OCR 翻译与 AI"));
  const ai = h("div", "card");
  ai.append(h("div", "hint", "识别结果窗口底部的「翻译」「AI 整理」用的就是这里的配置。翻译留空即可用内置免费接口（有道，国内直连、不需要 key）。"));

  ai.append(row("翻译接口", "留空 = 内置免费接口（需要先在下面允许一次）。填了就 POST {\"text\":\"...\"}，期望返回 {\"text\":\"...\"}",
    textInput(v.translateUrl, "留空使用内置免费接口", (x) => cappatch({ translateUrl: x })),
    false, testBtn("translate")));

  ai.append(row("翻译默认语言", "只决定结果窗口的初始选择——识别完可以在窗口里临时换，自动 = 有中文译成英文，否则译成中文",
    sel([["auto", "自动"], ["zh", "中文"], ["en", "英文"], ["ja", "日语"], ["ko", "韩语"],
         ["fr", "法语"], ["de", "德语"], ["es", "西班牙语"], ["ru", "俄语"]],
      v.translateTo || "auto", (x) => cappatch({ translateTo: x }))));

  // 内置接口 = 把正文发给第三方。这是个隐私开关，默认关，且不藏在折叠区里。
  const consent = h("input");
  consent.type = "checkbox";
  consent.checked = !!v.translateConsent;
  consent.onchange = () => cappatch({ translateConsent: consent.checked });
  ai.append(row("允许内置翻译", "内置接口会把识别到的文字发送到有道 / Google 的免费接口。内容可能含密码或私聊，所以默认关闭；填了自建「翻译接口」就不需要它", consent));

  ai.append(row("AI 接口", "OpenAI 兼容的 base，例如 https://api.openai.com/v1、https://api.deepseek.com/v1",
    textInput(v.aiBaseUrl, "https://api.openai.com/v1", (x) => cappatch({ aiBaseUrl: x })),
    false, testBtn("ai")));

  const keyInput = h("input");
  keyInput.type = "password";
  keyInput.value = "";
  keyInput.placeholder = v.aiKeySet ? "已保存（留空不改，输入新值即覆盖）" : "sk-...";
  keyInput.style.minWidth = "220px";
  keyInput.onchange = async () => {
    const val = keyInput.value.trim();
    if (!val) return; // 空 = 不改；要清空请点右边的「清除」
    const r = await inv("capture_ai_key", { value: val });
    if (r && r.ok === false) { toast(r.error || "保存失败"); return; }
    keyInput.value = "";
    keyInput.placeholder = "已保存（留空不改，输入新值即覆盖）";
    toast("AI Key 已加密保存");
  };
  const clearKey = h("button", "btn", v.aiKeySet ? "清除" : "未设置");
  clearKey.disabled = !v.aiKeySet;
  clearKey.onclick = async () => {
    const r = await inv("capture_ai_key", { value: "" });
    if (r && r.ok === false) { toast(r.error || "清除失败"); return; }
    keyInput.placeholder = "sk-...";
    clearKey.textContent = "未设置";
    clearKey.disabled = true;
    toast("AI Key 已清除");
  };
  ai.append(row("AI Key", "存在 <exe>/imgbed/secrets.dat（DPAPI 加密，绑定当前 Windows 用户），不写 settings.yaml、不上传", keyInput,
    false, clearKey));

  ai.append(row("AI 模型", "例如 gpt-4o-mini / deepseek-chat / qwen-plus",
    textInput(v.aiModel, "gpt-4o-mini", (x) => cappatch({ aiModel: x }))));

  const prompt = h("textarea");
  prompt.value = v.aiPrompt || "";
  prompt.placeholder = "留空使用内置提示词：修错字、恢复换行段落、表格转 Markdown、代码保缩进，只输出正文";
  prompt.rows = 3;
  prompt.style.cssText = "min-width:320px;width:min(520px,48vw);resize:vertical;font:inherit;padding:6px 8px";
  prompt.onchange = () => cappatch({ aiPrompt: prompt.value });
  ai.append(row("AI 提示词", "可选。改坏了就清空回到默认", prompt));
  frag.append(ai);

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

window.pageCapture = pageCapture;
})();
