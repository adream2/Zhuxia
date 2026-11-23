/* ---------- 剪贴板：统计与保存（自 app.js 拆出） ---------- */

let CLIPSTATS = null;

async function loadClip() {
  try {
    CLIPSTATS = await inv("clipboard_state");
  } catch (e) {
    CLIPSTATS = null;
  }
}

const cpatch = (obj) => save("clipboard", obj);


/* ---------- 剪贴板：隐私规则与页面（自 app.js 拆出） ---------- */

function policiesToText(list) {
  return (list || [])
    .map((p) => (p.action === "ignore" ? `${p.app} => ignore` : `${p.app} => ${p.rules || ""}`))
    .join("\n");
}

function textToPolicies(text) {
  const out = [];
  for (const raw of String(text || "").split("\n")) {
    const line = raw.trim();
    if (!line || line.startsWith("#")) continue;
    const i = line.indexOf("=>");
    if (i < 0) continue;
    const app = line.slice(0, i).trim();
    const rest = line.slice(i + 2).trim();
    if (!app) continue;
    if (rest.toLowerCase() === "ignore") out.push({ app, action: "ignore", rules: "" });
    else out.push({ app, action: "cleanup", rules: rest });
  }
  return out;
}

function pageClipboard() {
  const v = S.clipboard || {};
  const frag = document.createDocumentFragment();
  frag.append(h("h1", "", "剪贴管理"));

  /* 概览 */
  const st = CLIPSTATS;
  const pills = h("div", "pills");
  const pill = (k, val) => {
    const p = h("span", "pill");
    p.append(document.createTextNode(k + " "), h("b", "", val));
    return p;
  };
  if (st) {
    pills.append(pill("记录", String(st.total)));
    pills.append(pill("收藏", String(st.pinned)));
    pills.append(pill("密钥", String(st.secrets || 0)));
    pills.append(pill("图片", String(st.images)));
    pills.append(pill("文件", String(st.files)));
    pills.append(pill("敏感", String(st.sensitive)));
    pills.append(pill("占用", (st.bytes / 1048576).toFixed(1) + " MB"));
    pills.append(pill("常驻", st.alive ? "在运行" : "未运行"));
  }
  frag.append(pills);

  const c0 = h("div", "card");
  const openDir = h("button", "btn", "打开数据目录");
  openDir.onclick = () => inv("clipboard_open_dir");
  c0.append(row("数据", st ? st.dir : "clipboard/", openDir));
  const clearKeep = h("button", "btn", "清空（保留收藏）");
  clearKeep.onclick = async () => {
    await inv("clipboard_clear", { keepPinned: true });
    toast("已请求清空，常驻进程 1 秒内执行");
    setTimeout(async () => { await loadClip(); render(); }, 1200);
  };
  c0.append(row("清空历史", "只删未收藏的记录；附件一并清理", clearKeep));
  const clearAll = h("button", "btn", "全部清空");
  clearAll.onclick = async () => {
    if (!confirm("确定删除全部剪贴板记录（含收藏）？此操作不可撤销。")) return;
    await inv("clipboard_clear", { keepPinned: false });
    toast("已请求清空");
    setTimeout(async () => { await loadClip(); render(); }, 1200);
  };
  c0.append(row("危险操作", "包含收藏与非收藏", clearAll));
  frag.append(c0);

  /* 采集 */
  frag.append(h("h2", "", "采集"));
  const c1 = h("div", "card");
  c1.append(row("启用剪贴板模块", "关闭后不再监听（重启生效）",
    sw(v.enabled, (x) => cpatch({ enabled: x }))));
  c1.append(row("记录复制的文件", "Explorer 里的文件 / 文件夹路径", sw(v.captureFiles, (x) => cpatch({ captureFiles: x }))));
  c1.append(row("记录富文本", "保留 HTML 格式（Word / 网页 / WPS）", sw(v.captureRichText, (x) => cpatch({ captureRichText: x }))));
  c1.append(row("记录图片", "含 GIF（保留动画原始字节）", sw(v.recordImages, (x) => cpatch({ recordImages: x }))));
  c1.append(row("内容去重", "同一内容再次复制只更新时间并置顶", sw(v.dedupe, (x) => cpatch({ dedupe: x }))));
  c1.append(row("持久化", "关闭 = 只保留本次运行（最多 500 条，重启即清）", sw(v.persistent, (x) => cpatch({ persistent: x }))));
  c1.append(row("限制条数", "超过后删除最旧的未收藏、无标签记录",
    sw(v.limitEnabled, (x) => cpatch({ limitEnabled: x }))));
  c1.append(row("保留条数", "50–99999", numInput(v.limit, 50, 99999, (x) => cpatch({ limit: x })), !v.limitEnabled));
  frag.append(c1);

  /* 交互 */
  frag.append(h("h2", "", "交互"));
  const c2 = h("div", "card");
  c2.append(row("主界面热键", "默认 Alt+C；点面板外或 Esc 关闭", textInput(v.hotkey, "Alt+C", (x) => cpatch({ hotkey: x }))));
  c2.append(row("纯文本粘贴热键", "不开面板，直接把最新一条粘到当前窗口；留空关闭",
    textInput(v.plainPasteHotkey, "留空 = 关闭", (x) => cpatch({ plainPasteHotkey: x }))));
  c2.append(row("顺序粘贴热键", "复制多条后依次粘贴", textInput(v.sequentialHotkey, "Alt+V", (x) => cpatch({ sequentialHotkey: x }))));
  c2.append(row("搜索热键", "打开面板并聚焦搜索框", textInput(v.searchHotkey, "Alt+F", (x) => cpatch({ searchHotkey: x }))));
  c2.append(row("粘贴方式", "auto 按目标程序自动选键",
    seg([["auto", "自动"], ["ctrl_v", "Ctrl+V"], ["shift_insert", "Shift+Insert"], ["ctrl_shift_v", "Ctrl+Shift+V"]],
      v.pasteMethod, (x) => cpatch({ pasteMethod: x }))));
  c2.append(row("顺序复制模式", "每次复制入队，热键依次粘贴", sw(v.sequentialMode, (x) => cpatch({ sequentialMode: x }))));
  c2.append(row("快速粘贴修饰键", "面板打开时按住它 + 数字 1–9 直接粘贴第 N 条",
    sel([["disabled", "关闭"], ["alt", "Alt"], ["ctrl", "Ctrl"], ["shift", "Shift"]],
      v.quickPasteModifier, (x) => cpatch({ quickPasteModifier: x }))));
  c2.append(row("粘贴后删除", "把剪贴板当一次性中转站", sw(v.deleteAfterPaste, (x) => cpatch({ deleteAfterPaste: x }))));
  c2.append(row("粘贴后置顶", "刚用过的记录回到列表最前", sw(v.moveToTopAfterPaste, (x) => cpatch({ moveToTopAfterPaste: x }))));
  c2.append(row("面板钉住", "面板失焦不自动关闭（下次打开生效）", sw(v.windowPinned, (x) => cpatch({ windowPinned: x }))));
  c2.append(row("暂停采集", "临时停止记录（不重启）", sw(v.pause, (x) => cpatch({ pause: x }))));
  c2.append(row("接管 Win+V", "用低级键盘钩子吞掉系统快捷键，改为弹出本面板；与其它剪贴板工具可能冲突",
    sw(v.takeoverWinV, (x) => cpatch({ takeoverWinV: x }))));
  frag.append(c2);

  /* 隐私与规则 */
  frag.append(h("h2", "", "隐私与清理"));
  const c3 = h("div", "card");
  c3.append(row("隐私识别", "命中的记录打 sensitive 标签，列表预览打码", sw(v.privacy, (x) => cpatch({ privacy: x }))));
  const kinds = v.privacyKinds || [];
  const chips = h("div", "chips");
  const KINDS = [["phone", "手机号"], ["idcard", "身份证"], ["email", "邮箱"], ["secret", "密钥/令牌"], ["url", "链接"], ["password", "弱口令特征"]];
  for (const [key, label] of KINDS) {
    const on = kinds.includes(key);
    chips.append(chip(label, on, (next) => {
      const set = new Set(kinds);
      if (next) set.add(key); else set.delete(key);
      cpatch({ privacyKinds: Array.from(set) });
    }));
  }
  c3.append(row("识别类型", "只在这几类命中时打标签", chips, !v.privacy));
  c3.append(row("清理规则", "一行一条：pattern => 替换；> pattern 表示命中即不记录；# action: ignore 作用于下一条",
    textArea(v.cleanupRules, 5, "secret => ***\n> 机密", (x) => cpatch({ cleanupRules: x }))));
  c3.append(row("按程序处理", "一行一个：进程名 => ignore（不记录）或 进程名 => 规则（只对该程序清理）",
    textArea(policiesToText(v.appPolicies), 4, "keepass => ignore\nwps => secret => ***", (x) => cpatch({ appPolicies: textToPolicies(x) }))));
  frag.append(c3);

  /* 声音 */
  frag.append(h("h2", "", "声音"));
  const c4 = h("div", "card");
  c4.append(row("复制音", "采集成功时响一下", sw(v.sound, (x) => cpatch({ sound: x }))));
  c4.append(row("粘贴音", "粘贴回填时响一下", sw(v.pasteSound, (x) => cpatch({ pasteSound: x }))));
  c4.append(row("音量", "0–100%，音效由程序实时合成，不依赖外部音频文件",
    volSlider(v.soundVolume, (x) => cpatch({ soundVolume: x }))));
  frag.append(c4);

  return frag;
}

