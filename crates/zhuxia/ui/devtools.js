/* 开发者工具箱：设置面板里的一页（顶部一排工具，下面是对应的工作区）。
 *
 * 定位：纯前端计算页。JSON / 编码 / 时间戳 / 正则 / 哈希 / UUID / 颜色 / JWT 都在
 * WebView 里算完——不落盘、不联网、不改 settings.yaml，所以这里没有对应的 Rust
 * command，也没有新 crate。这与 imgbed（要发请求、要密钥）本质不同。
 *
 * 整段包在 IIFE 里：它和 app.js 共享同一个全局作用域，`const inv` / `function h`
 * 这类重名会直接让后加载的脚本报 SyntaxError。对外只暴露 window.DevTools。
 */
(() => {
const h = (tag, cls, text) => {
  const el = document.createElement(tag);
  if (cls) el.className = cls;
  if (text != null) el.textContent = text;
  return el;
};

// 面板共用同一个 toast（app.js），这里不重复实现
const toast = (msg) => (window.toast ? window.toast(msg) : console.log(msg));

const enc = new TextEncoder();
const dec = new TextDecoder("utf-8");                                  // 宽松：坏字节给替换符
const decFatal = new TextDecoder("utf-8", { fatal: true });            // 严格：用来判断"是不是文本"

const byteLen = (s) => enc.encode(s).length;
const fmtBytes = (n) => (n < 1024 ? n + " B"
  : n < 1048576 ? (n / 1024).toFixed(1) + " KB"
    : (n / 1048576).toFixed(2) + " MB");

async function copyText(text) {
  if (!text) { toast("没有可复制的内容"); return; }
  try {
    await navigator.clipboard.writeText(text);
    toast("已复制");
  } catch (_) {
    // 非安全上下文或剪贴板被占用：退回常驻进程的命令（Rust 侧 OpenClipboard）
    try {
      const r = await window.__TAURI__.core.invoke("ib_copy", { text });
      toast(r && r.ok ? "已复制" : "复制失败：剪贴板被占用");
    } catch (e) {
      toast("复制失败：" + e);
    }
  }
}

/* ---------------- 通用控件 ---------------- */

function btn(label, onClick, primary) {
  const b = h("button", "dt-btn" + (primary ? " primary" : ""), label);
  b.type = "button";
  b.onclick = onClick;
  return b;
}

function area(value, placeholder, readOnly) {
  const t = h("textarea", "dt-ta" + (readOnly ? " dt-out" : ""));
  t.value = value || "";
  t.placeholder = placeholder || "";
  t.spellcheck = false;
  t.readOnly = !!readOnly;
  return t;
}

function field(label, node, extra) {
  const f = h("div", "dt-field");
  const lb = h("div", "lb");
  lb.append(h("span", "", label));
  if (extra) lb.append(extra);
  f.append(lb, node);
  return f;
}

// 状态条：成功/警告/错误都走这一条，避免每个工具各写一套提示
function note(text, cls) {
  const n = h("div", "dt-note" + (cls ? " " + cls : ""), text);
  return n;
}
function setNote(n, text, cls) {
  n.textContent = text;
  n.className = "dt-note" + (cls ? " " + cls : "");
}

// 分段控件（等价 app.js 的 seg，但这里自己实现，避免依赖宿主内部函数）
function segBox(options, cur, onChange) {
  const box = h("div", "seg");
  options.forEach(([val, label]) => {
    const b = h("button", val === cur ? "on" : "", label);
    b.type = "button";
    b.onclick = () => {
      box.querySelectorAll("button").forEach((x) => x.classList.remove("on"));
      b.classList.add("on");
      onChange(val);
    };
    box.append(b);
  });
  return box;
}

// key/value 结果表，每行带独立复制按钮
function kvTable() {
  const box = h("div", "dt-kv");
  box.add = (k, v) => {
    const r = h("div", "r");
    r.append(h("div", "k", k));
    const val = h("div", "v", v);
    const c = btn("复制", () => copyText(val.textContent));
    c.classList.add("mini");
    r.append(val, c);
    box.append(r);
    return r;
  };
  return box;
}

/* ---------------- 工具 1：JSON ---------------- */

// 递归按键名排序。数字键会被 JS 对象重排到前面，这是语言行为，不额外处理。
function sortKeys(v) {
  if (Array.isArray(v)) return v.map(sortKeys);
  if (v && typeof v === "object") {
    const o = {};
    for (const k of Object.keys(v).sort()) o[k] = sortKeys(v[k]);
    return o;
  }
  return v;
}

// 新版 V8（WebView2 也是）的 JSON 报错已经不带 position 了，只丢一句
// "Unexpected token '}' … is not valid JSON"。所以自己扫一遍定位：
// 只负责找到"第一个不合法的地方"并给中文说明，合法性仍由 JSON.parse 判定。
function jsonScanError(text) {
  let i = 0;
  const n = text.length;
  const isWs = (c) => c === " " || c === "\n" || c === "\r" || c === "\t" || c === "\uFEFF";
  const ws = () => { while (i < n && isWs(text[i])) i++; };
  const fail = (msg, at) => { throw { pos: at === undefined ? i : at, msg }; };

  function value() {
    ws();
    if (i >= n) fail("内容意外结束（缺少值）");
    const c = text[i];
    if (c === "{") return object();
    if (c === "[") return array();
    if (c === '"') return string();
    if (c === "-" || (c >= "0" && c <= "9")) return number();
    for (const [lit, len] of [["true", 4], ["false", 5], ["null", 4]]) {
      if (text.startsWith(lit, i)) { i += len; return; }
    }
    fail(`意外的字符 '${c}'`);
  }

  function object() {
    i++;
    ws();
    if (text[i] === "}") { i++; return; }
    for (;;) {
      ws();
      if (text[i] !== '"') fail("对象的键必须是双引号字符串");
      string();
      ws();
      if (text[i] !== ":") fail("键后面应该是冒号");
      i++;
      value();
      ws();
      if (text[i] === ",") {
        i++;
        ws();
        if (text[i] === "}") fail("多余的逗号：} 前面不能再有 ,");
        continue;
      }
      if (text[i] === "}") { i++; return; }
      if (i >= n) fail("对象没有闭合（缺少 }）");
      fail("键值对之间缺少逗号");
    }
  }

  function array() {
    i++;
    ws();
    if (text[i] === "]") { i++; return; }
    for (;;) {
      value();
      ws();
      if (text[i] === ",") {
        i++;
        ws();
        if (text[i] === "]") fail("多余的逗号：] 前面不能再有 ,");
        continue;
      }
      if (text[i] === "]") { i++; return; }
      if (i >= n) fail("数组没有闭合（缺少 ]）");
      fail("数组元素之间缺少逗号");
    }
  }

  function string() {
    const start = i;
    i++;
    while (i < n) {
      const c = text[i];
      if (c === "\\") { i += 2; continue; }
      if (c === '"') { i++; return; }
      if (c === "\n") fail("字符串里不能直接换行（要换行请写 \\n）", i);
      i++;
    }
    fail("字符串没有闭合（缺少收尾的双引号）", start);
  }

  function number() {
    const m = /^-?(0|[1-9]\d*)(\.\d+)?([eE][+-]?\d+)?/.exec(text.slice(i));
    if (!m) fail("数字格式不对");
    i += m[0].length;
  }

  try {
    value();
    ws();
    if (i < n) fail("JSON 结束后还有多余内容");
    return null;
  } catch (e) {
    if (e && typeof e.pos === "number") return e;
    return null;
  }
}

function lineCol(text, pos) {
  const before = text.slice(0, Math.max(0, pos));
  return { line: before.split("\n").length, col: pos - before.lastIndexOf("\n") };
}

// JSON.parse 的报错文案各版本不一致（有/没有 "line N column M"），统一解析出位置。
function jsonErrorInfo(err, text) {
  const scanned = jsonScanError(text);
  if (scanned) {
    const { line, col } = lineCol(text, scanned.pos);
    return { line, col, pos: scanned.pos, text: scanned.msg };
  }
  const raw = String((err && err.message) || err);
  let pos = -1;
  const pm = raw.match(/position (\d+)/i);
  if (pm) pos = Number(pm[1]);
  let line = 0;
  let col = 0;
  if (pos < 0) {
    const lm = raw.match(/line (\d+) column (\d+)/i);
    if (lm) { line = Number(lm[1]); col = Number(lm[2]); }
  }
  if (pos >= 0) {
    const lc = lineCol(text, pos);
    line = lc.line;
    col = lc.col;
  }
  // 去掉尾巴上的位置描述，位置单独展示（不然一行里出现两遍）
  const clean = raw
    .replace(/\s*in JSON at position \d+.*$/i, "")
    .replace(/\s*\(line \d+ column \d+\)\s*$/i, "")
    .trim();
  return { line, col, pos, text: clean || raw };
}

function toolJson() {
  const st = MEM.json;
  const frag = document.createDocumentFragment();
  const bar = note("", "");
  const out = area("", "格式化结果", true);
  const wrap = h("div", "dt-grid");

  const input = area(st.input, "粘贴 JSON，边输边校验…");
  input.oninput = () => { st.input = input.value; schedule(); };

  const indentBox = segBox([[2, "2 空格"], [4, "4 空格"], [1, "Tab"]], st.indent,
    (v) => { st.indent = v; run(); });

  const sortSw = h("div", "switch" + (st.sort ? " on" : ""));
  sortSw.title = "按 key 升序重排（数组顺序不变）";
  sortSw.onclick = () => {
    st.sort = !st.sort;
    sortSw.classList.toggle("on", st.sort);
    run();
  };
  const sortWrap = h("div", "dt-inline");
  sortWrap.append(sortSw, h("span", "dt-inline-t", "键排序"));

  const actions = h("div", "dt-tools");
  actions.append(
    segBox([["pretty", "格式化"], ["min", "压缩"]], st.mode, (v) => { st.mode = v; run(); }),
    btn("复制结果", () => copyText(out.value), true),
    btn("清空", () => {
      input.value = "";
      st.input = "";
      out.value = "";
      setNote(bar, "输入为空：粘贴一段 JSON 再试", "");
    }),
    indentBox, sortWrap,
  );

  const jump = btn("定位错误", () => {
    const p = Math.max(0, jump._pos || 0);
    input.focus();
    input.setSelectionRange(p, Math.min(p + 1, input.value.length));
  });
  jump.classList.add("mini");
  jump.style.display = "none";

  const stats = h("div", "dt-tools");
  stats.append(bar, jump);

  wrap.append(field("输入", input), field("输出", out));
  frag.append(actions, stats, wrap, note("尾逗号、注释、单引号都不是标准 JSON，这里不做容错——报错位置就是真正不合法的地方。"));

  let timer = 0;
  const schedule = () => { clearTimeout(timer); timer = setTimeout(run, 180); };

  function run() {
    const raw = input.value;
    const text = raw.trim();
    if (!text) {
      out.value = "";
      setNote(bar, "输入为空：粘贴一段 JSON 再试", "");
      jump.style.display = "none";
      return;
    }
    let data;
    try {
      data = JSON.parse(text);
    } catch (e) {
      const info = jsonErrorInfo(e, text);
      const where = info.line ? `第 ${info.line} 行第 ${info.col} 列` : "位置未知";
      setNote(bar, `JSON 无效 · ${where} —— ${info.text}`, "err");
      // 输出清空：输入已经不合法，留着上一次的格式化结果只会让人以为那是当前内容
      out.value = "";
      // 位置是相对 trim 后的文本算的，加回前导空白才不会差几个字符
      const lead = raw.length - raw.trimStart().length;
      jump._pos = info.pos >= 0 ? info.pos + lead : -1;
      jump.style.display = jump._pos >= 0 ? "" : "none";
      return;
    }
    jump.style.display = "none";
    const data2 = st.sort ? sortKeys(data) : data;
    const indent = st.mode === "min" ? 0 : (st.indent === 1 ? "\t" : st.indent);
    out.value = JSON.stringify(data2, null, indent);
    const TYPE = { object: "对象", string: "字符串", number: "数字", boolean: "布尔", undefined: "空" };
    const top = Array.isArray(data2) ? "数组" : data2 === null ? "null" : (TYPE[typeof data2] || typeof data2);
    const keys = data2 && typeof data2 === "object" && !Array.isArray(data2) ? Object.keys(data2).length : 0;
    setNote(bar, `有效 JSON · 顶层${top}${keys ? ` · ${keys} 个顶层键` : ""} · ${fmtBytes(byteLen(out.value))}`,
      byteLen(out.value) > 512 * 1024 ? "warn" : "ok");
  }

  if (st.input.trim()) run();
  else setNote(bar, "输入为空：粘贴一段 JSON 再试", "");
  return frag;
}

/* ---------------- 工具 2：编码 / 解码 ---------------- */

const ENT = { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" };

const CODEC = {
  b64: {
    label: "Base64",
    enc(s, o) {
      let bin = "";
      const bytes = enc.encode(s);
      // 一次性铺开几十万参数会爆栈，按 32KB 分批
      for (let i = 0; i < bytes.length; i += 0x8000) {
        bin += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
      }
      let b = btoa(bin);
      if (o.urlSafe) b = b.replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
      return { text: b };
    },
    dec(s, o) {
      if (!s.trim()) return { err: "输入为空" };
      let t = s.replace(/\s+/g, "");
      if (!/^[A-Za-z0-9+/_=-]+$/.test(t)) {
        return { err: "含非 Base64 字符（只允许 A–Z a–z 0–9 + / = 与 - _）" };
      }
      t = t.replace(/-/g, "+").replace(/_/g, "/");
      const pad = t.length % 4;
      if (pad === 1) return { err: "长度不合法：去掉空白后应为 4 的倍数，当前除以 4 余 1" };
      if (pad) t += "=".repeat(4 - pad);
      let bin;
      try { bin = atob(t); } catch (_) { return { err: "解码失败：填充字符或长度不对" }; }
      const bytes = new Uint8Array(bin.length);
      for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
      try {
        return { text: decFatal.decode(bytes) };
      } catch (_) {
        return { text: dec.decode(bytes), warn: "结果不是合法 UTF-8（可能是图片/压缩包），坏字节显示为 �" };
      }
    },
  },
  url: {
    label: "URL",
    enc: (s) => ({ text: encodeURIComponent(s) }),
    dec(s) {
      try { return { text: decodeURIComponent(s) }; }
      catch (_) { return { err: "含非法的百分号转义（例如 %ZZ，或被截断的 %E4）" }; }
    },
  },
  html: {
    label: "HTML 实体",
    enc: (s) => ({ text: String(s).replace(/[&<>"']/g, (c) => ENT[c]) }),
    dec(s) {
      // 用 textarea 的 innerHTML 解实体：不执行脚本，未知实体原样保留
      const t = document.createElement("textarea");
      t.innerHTML = s;
      return { text: t.value };
    },
  },
  uni: {
    label: "Unicode",
    enc(s) {
      const out = Array.from(s).map((ch) => {
        const cp = ch.codePointAt(0);
        return cp > 0xffff
          ? `\\u{${cp.toString(16).toUpperCase()}}`
          : `\\u${cp.toString(16).toUpperCase().padStart(4, "0")}`;
      }).join("");
      return { text: out };
    },
    dec(s) {
      let hit = 0;
      const text = s.replace(/\\u\{([0-9a-fA-F]{1,6})\}|\\u([0-9a-fA-F]{4})|\\x([0-9a-fA-F]{2})/g, (m, a, b, c) => {
        const cp = parseInt(a || b || c, 16);
        if (cp > 0x10ffff) return m;     // 超出 Unicode 范围：原样留着，别悄悄吞掉
        hit++;
        return String.fromCodePoint(cp);
      });
      if (!hit) return { err: "没有找到 \\uXXXX / \\u{XXXX} / \\xFF 形式的转义" };
      return { text };
    },
  },
};

function toolCodec() {
  const st = MEM.codec;
  const frag = document.createDocumentFragment();
  const bar = note("选一个编码，再点编码或解码", "");
  const out = area(st.output || "", "结果", true);
  const input = area(st.input || "", "要编码或解码的内容…");
  input.oninput = () => { st.input = input.value; };

  const urlSafeSw = h("div", "switch" + (st.urlSafe ? " on" : ""));
  urlSafeSw.onclick = () => {
    st.urlSafe = !st.urlSafe;
    urlSafeSw.classList.toggle("on", st.urlSafe);
    if (st.mode === "b64" && out.value) doEnc();   // 换 URL-safe 后立刻按新规则重算
  };
  const optWrap = h("div", "dt-inline");
  optWrap.append(urlSafeSw, h("span", "dt-inline-t", "URL-safe（-_ / 去掉 =）"));

  const seg = segBox(Object.entries(CODEC).map(([id, c]) => [id, c.label]), st.mode, (v) => {
    st.mode = v;
    optWrap.style.display = v === "b64" ? "" : "none";
    setNote(bar, `${CODEC[v].label}：输入内容后点编码或解码`, "");
  });

  const run = (dir) => {
    if (dir === "enc") doEnc(); else doDec();
  };
  const doEnc = () => {
    const r = CODEC[st.mode].enc(input.value, st);
    if (r.err) { setNote(bar, r.err, "err"); return; }
    out.value = r.text || "";
    st.output = out.value;
    setNote(bar, `已编码 · ${fmtBytes(byteLen(input.value))} → ${fmtBytes(byteLen(out.value))}`, "ok");
  };
  const doDec = () => {
    const r = CODEC[st.mode].dec(input.value, st);
    if (r.err) { setNote(bar, r.err, "err"); return; }
    out.value = r.text || "";
    st.output = out.value;
    setNote(bar, r.warn ? r.warn : `已解码 · ${fmtBytes(byteLen(out.value))}`, r.warn ? "warn" : "ok");
  };

  const actions = h("div", "dt-tools");
  actions.append(
    seg,
    optWrap,
    btn("编码 →", () => run("enc"), true),
    btn("← 解码", () => run("dec")),
    btn("互换", () => {
      const t = input.value;
      input.value = out.value;
      st.input = input.value;
      out.value = t;
      st.output = out.value;
      setNote(bar, "已互换输入与输出", "");
    }),
  );
  optWrap.style.display = st.mode === "b64" ? "" : "none";

  const copyOut = btn("复制", () => copyText(out.value));
  copyOut.classList.add("mini");

  const wrap = h("div", "dt-grid");
  wrap.append(field("输入", input), field("输出", out, copyOut));
  frag.append(actions, bar, wrap,
    note("Base64 不是加密，只是编码；HTML 实体里的未知实体（如 &foo;）会原样保留。"));
  return frag;
}

/* ---------------- 工具 3：时间戳 ---------------- */

function relative(ms) {
  const diff = ms - Date.now();
  const abs = Math.abs(diff);
  if (abs < 5000) return "刚刚";
  const units = [[31536000000, "年"], [2592000000, "个月"], [604800000, "周"],
    [86400000, "天"], [3600000, "小时"], [60000, "分钟"], [1000, "秒"]];
  const [unit, name] = units.find(([v]) => abs >= v);
  const n = Math.floor(abs / unit);
  return diff > 0 ? `${n} ${name}后` : `${n} ${name}前`;
}

function tzOffset(d) {
  const m = -d.getTimezoneOffset();
  const sign = m >= 0 ? "+" : "-";
  const a = Math.abs(m);
  return `UTC${sign}${String(Math.floor(a / 60)).padStart(2, "0")}:${String(a % 60).padStart(2, "0")}`;
}

// 纯数字按时间戳解（10 位=秒、13 位=毫秒）；其余交给 Date，但纯日期要按"本地 0 点"算
function parseTime(text, unit) {
  const s = String(text).trim();
  if (!s) return { err: "输入为空" };
  if (/^-?\d+(\.\d+)?$/.test(s)) {
    const n = Number(s);
    let ms;
    if (unit === "ms") ms = n;
    else if (unit === "s") ms = n * 1000;
    else ms = Math.abs(n) >= 1e11 ? n : n * 1000;
    const d = new Date(ms);
    if (Number.isNaN(d.getTime())) return { err: "时间戳超出可表示范围（±8.64e15 毫秒）" };
    return { date: d, ms };
  }
  let iso = s;
  if (/^\d{4}-\d{1,2}-\d{1,2}$/.test(s)) iso = s + " 00:00:00";   // 否则 "2026-11-22" 会被当 UTC
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) {
    return { err: "认不出这个时间：试试 2026-11-22 18:15、2026/11/22 或 13 位时间戳" };
  }
  return { date: d, ms: d.getTime() };
}

function toolTime() {
  const st = MEM.time;
  const frag = document.createDocumentFragment();
  const bar = note("", "");
  const table = kvTable();
  const input = h("input", "dt-input");
  input.type = "text";
  input.placeholder = "13 位时间戳，或 2026-11-22 18:15:00";
  input.value = st.input || "";
  input.oninput = () => { st.input = input.value; };
  input.onkeydown = (e) => { if (e.key === "Enter") run(); };

  const unitSeg = segBox([["auto", "自动"], ["s", "秒"], ["ms", "毫秒"]], st.unit, (v) => {
    st.unit = v;
    run();
  });

  const actions = h("div", "dt-tools");
  actions.append(
    input,
    btn("转换", run, true),
    btn("现在", () => {
      input.value = String(Date.now());
      st.input = input.value;
      run();
    }),
    unitSeg,
    btn("复制毫秒", () => copyText(table.dataset.ms || "")),
  );

  frag.append(actions, bar, table);

  function run() {
    table.replaceChildren();
    const r = parseTime(input.value, st.unit || "auto");
    if (r.err) {
      setNote(bar, r.err, "err");
      return;
    }
    const d = r.date;
    const ms = r.ms;
    table.dataset.ms = String(ms);
    const week = "日一二三四五六"[d.getDay()];
    table.add("本地时间", `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")} ${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}:${String(d.getSeconds()).padStart(2, "0")} 周${week}`);
    table.add("ISO 8601", d.toISOString());
    table.add("UTC", d.toUTCString());
    table.add("Unix 秒", String(Math.floor(ms / 1000)));
    table.add("Unix 毫秒", String(ms));
    table.add("相对现在", relative(ms));
    table.add("时区", tzOffset(d));
    setNote(bar, ms > Date.now() ? "未来时间 · 上方是本地时区换算" : "过去时间 · 上方是本地时区换算", "ok");
  }

  // 顶部实时当前时间：切走工具后自动停，不在后台白跑。
  // 首帧不进 tick（此时还在 fragment 里、isConnected 为 false），1 秒后必已上屏。
  const live = h("div", "dt-live", "现在：—");
  frag.prepend(live);
  const timer = setInterval(() => {
    if (!live.isConnected) { clearInterval(timer); return; }
    const now = new Date();
    live.textContent = `现在：${String(now.getHours()).padStart(2, "0")}:${String(now.getMinutes()).padStart(2, "0")}:${String(now.getSeconds()).padStart(2, "0")} · ${Math.floor(now.getTime() / 1000)}（秒） / ${now.getTime()}（毫秒）`;
  }, 1000);

  if (st.input) run();
  else setNote(bar, "输入时间或时间戳后点「转换」；纯数字默认按 10 位=秒、13 位=毫秒判断", "");
  return frag;
}

/* ---------------- 工具 4：正则 ---------------- */

function collectMatches(re, text, limit) {
  const out = [];
  if (!re.global) {
    const m = re.exec(text);
    if (m) out.push(m);
    return { list: out, truncated: false };
  }
  re.lastIndex = 0;
  let m;
  while ((m = re.exec(text)) !== null) {
    out.push(m);
    // 零宽匹配（如 /a*/g）不推进 lastIndex 会死循环
    if (m[0] === "") re.lastIndex++;
    if (out.length >= limit) {
      // 再看一次确认后面还有没有：有才算截断，避免正好 500 处时误报
      return { list: out, truncated: re.exec(text) !== null };
    }
  }
  return { list: out, truncated: false };
}

// 用 DOM 文本节点拼高亮：不经过 innerHTML，用户内容里的尖括号不会被当标签
function highlight(text, matches) {
  const box = h("pre", "dt-hl");
  let last = 0;
  for (const m of matches) {
    if (m.index > last) box.append(document.createTextNode(text.slice(last, m.index)));
    box.append(h("mark", "", m[0]));
    last = m.index + m[0].length;
    if (m[0] === "" && last < text.length) {
      box.append(document.createTextNode(text[last]));
      last++;
    }
  }
  if (last < text.length) box.append(document.createTextNode(text.slice(last)));
  if (!text) box.append(document.createTextNode("（没有测试文本）"));
  else if (!matches.length) box.append(document.createTextNode("没有匹配"));
  return box;
}

function toolRegex() {
  const st = MEM.regex;
  const frag = document.createDocumentFragment();
  const bar = note("", "");
  const listBox = h("div", "dt-list");
  const hlBox = h("div", "dt-hl-wrap");
  const replaceOut = area("", "替换结果", true);

  const pattern = h("input", "dt-input mono");
  pattern.type = "text";
  pattern.placeholder = "例如  \\b\\w+@\\w+\\.\\w+";
  pattern.value = st.pattern || "";
  pattern.oninput = () => { st.pattern = pattern.value; schedule(); };

  const flagBox = h("div", "dt-flags");
  const FLAGS = ["g", "i", "m", "s", "u", "y"];
  FLAGS.forEach((f) => {
    const b = h("button", "dt-flag" + (st.flags[f] ? " on" : ""), f);
    b.type = "button";
    b.title = { g: "全局匹配", i: "忽略大小写", m: "多行 ^$", s: ". 匹配换行", u: "Unicode 模式", y: "粘性匹配" }[f];
    b.onclick = () => {
      st.flags[f] = !st.flags[f];
      b.classList.toggle("on", st.flags[f]);
      run();
    };
    flagBox.append(b);
  });

  const patRow = h("div", "dt-tools");
  patRow.append(h("span", "dt-tag", "/"), pattern, h("span", "dt-tag", "/"), flagBox);

  const text = area(st.text, "测试文本…");
  text.oninput = () => { st.text = text.value; schedule(); };

  const repl = h("input", "dt-input");
  repl.type = "text";
  repl.placeholder = "替换为（可用 $1 $2 引用分组）";
  repl.value = st.replace || "";
  repl.oninput = () => { st.replace = repl.value; schedule(); };

  const actions = h("div", "dt-tools");
  actions.append(btn("重新执行", () => run(), true), btn("复制替换结果", () => copyText(replaceOut.value)));

  frag.append(patRow, actions, bar, field("测试文本", text), hlBox,
    field("替换为", repl), field("替换结果", replaceOut),
    field("匹配明细", listBox));

  let timer = 0;
  const schedule = () => { clearTimeout(timer); timer = setTimeout(run, 160); };

  function run() {
    const flags = FLAGS.filter((f) => st.flags[f]).join("");
    if (!pattern.value) {
      setNote(bar, "先写一个正则表达式", "");
      hlBox.replaceChildren();
      listBox.replaceChildren();
      replaceOut.value = "";
      return;
    }
    let re;
    try {
      re = new RegExp(pattern.value, flags);
    } catch (e) {
      setNote(bar, "正则语法错误：" + String(e.message || e).replace(/^Invalid regular expression:\s*/i, ""), "err");
      hlBox.replaceChildren();
      listBox.replaceChildren();
      return;
    }
    const limit = 500;
    const { list, truncated } = collectMatches(re, text.value, limit);
    hlBox.replaceChildren(highlight(text.value, list));

    listBox.replaceChildren();
    if (!list.length) {
      const e = h("div", "m empty", "没有匹配");
      listBox.append(e);
    } else {
      list.forEach((m, i) => {
        const row = h("div", "m");
        row.append(h("span", "i", `#${i + 1} @${m.index}`));
        const body = h("span", "b");
        const shown = m[0].length > 120 ? m[0].slice(0, 120) + "…" : m[0];
        body.append(h("code", "", m[0] === "" ? "（空匹配）" : shown));
        for (let g = 1; g < m.length; g++) {
          body.append(h("span", "g", ` $${g}=${m[g] === undefined ? "undefined" : m[g]}`));
        }
        row.append(body);
        listBox.append(row);
      });
    }

    let replaced = "";
    try { replaced = text.value.replace(re, repl.value); }
    catch (e) { replaced = ""; }
    replaceOut.value = replaced;

    if (truncated) setNote(bar, `匹配超过 ${limit} 处，只显示前 ${limit} 处；替换结果仍是完整执行`, "warn");
    else if (!re.global) setNote(bar, `匹配 ${list.length} 处（未开 g，只取第一处）；替换也只替换第一处`, list.length ? "ok" : "");
    else setNote(bar, `匹配 ${list.length} 处 · 测试文本 ${byteLen(text.value)} 字节`, list.length ? "ok" : "");
  }

  run();
  return frag;
}

/* ---------------- 工具 5：哈希 ---------------- */

// MD5：Web Crypto 不提供（它只给 SHA 系），这里自带一份纯 JS 实现。
// 输入/输出都以字节为准，不经过字符串，避免中文与二进制被二次转换。
function md5Hex(bytes) {
  const rotl = (x, n) => (x << n) | (x >>> (32 - n));
  const K = new Uint32Array(64);
  for (let i = 0; i < 64; i++) K[i] = Math.floor(Math.abs(Math.sin(i + 1)) * 4294967296);
  const S = [7, 12, 17, 22, 5, 9, 14, 20, 4, 11, 16, 23, 6, 10, 15, 21];

  let a0 = 0x67452301;
  let b0 = 0xefcdab89;
  let c0 = 0x98badcfe;
  let d0 = 0x10325476;

  const len = bytes.length;
  const padded = new Uint8Array((((len + 8) >> 6) + 1) << 6);   // 补到 64 倍数，末尾留 8 字节长度
  padded.set(bytes);
  padded[len] = 0x80;
  const dv = new DataView(padded.buffer);
  const bits = len * 8;
  dv.setUint32(padded.length - 8, bits >>> 0, true);
  dv.setUint32(padded.length - 4, Math.floor(bits / 4294967296), true);

  const M = new Uint32Array(16);
  for (let off = 0; off < padded.length; off += 64) {
    for (let i = 0; i < 16; i++) M[i] = dv.getUint32(off + i * 4, true);
    let A = a0;
    let B = b0;
    let C = c0;
    let D = d0;
    for (let i = 0; i < 64; i++) {
      let F;
      let g;
      if (i < 16) { F = (B & C) | (~B & D); g = i; }
      else if (i < 32) { F = (D & B) | (~D & C); g = (5 * i + 1) % 16; }
      else if (i < 48) { F = B ^ C ^ D; g = (3 * i + 5) % 16; }
      else { F = C ^ (B | ~D); g = (7 * i) % 16; }
      F = (F + A + K[i] + M[g]) | 0;
      A = D;
      D = C;
      C = B;
      B = (B + rotl(F, S[(i >> 4) * 4 + (i % 4)])) | 0;
    }
    a0 = (a0 + A) | 0;
    b0 = (b0 + B) | 0;
    c0 = (c0 + C) | 0;
    d0 = (d0 + D) | 0;
  }

  const hex32 = (n) => {
    let s = "";
    for (let i = 0; i < 4; i++) s += ((n >>> (i * 8)) & 0xff).toString(16).padStart(2, "0");
    return s;
  };
  return hex32(a0) + hex32(b0) + hex32(c0) + hex32(d0);
}

const toHex = (bytes) => Array.from(bytes).map((x) => x.toString(16).padStart(2, "0")).join("");

async function hashAll(bytes) {
  const out = { md5: md5Hex(bytes), "sha-1": "", "sha-256": "", "sha-512": "" };
  if (!window.crypto || !crypto.subtle) return { out, err: "当前环境没有 Web Crypto（SHA 系算不了）" };
  for (const name of ["SHA-1", "SHA-256", "SHA-512"]) {
    const buf = await crypto.subtle.digest(name, bytes);
    out[name.toLowerCase()] = toHex(new Uint8Array(buf));
  }
  return { out };
}

function toolHash() {
  const st = MEM.hash;
  const frag = document.createDocumentFragment();
  const bar = note("", "");
  const table = kvTable();

  const text = area(st.input, "要计算哈希的文本…");
  text.oninput = () => { st.input = text.value; st.bytes = null; schedule(); };

  const fileInput = h("input", "dt-file");
  fileInput.type = "file";
  fileInput.onchange = async () => {
    const f = fileInput.files && fileInput.files[0];
    if (!f) return;
    if (f.size > 64 * 1024 * 1024) {
      setNote(bar, `文件 ${fmtBytes(f.size)} 超过 64 MB：主线程算会把界面卡住，换小一点的文件`, "err");
      return;
    }
    const buf = await f.arrayBuffer();
    st.bytes = new Uint8Array(buf);
    st.fileName = f.name;
    st.fileSize = f.size;
    run();
  };

  let mode = st.mode || "text";
  const seg = segBox([["text", "文本"], ["file", "文件"]], mode, (v) => {
    mode = v;
    st.mode = v;
    textWrap.style.display = v === "text" ? "" : "none";
    fileWrap.style.display = v === "file" ? "" : "none";
    run();
  });

  const upperSw = h("div", "switch" + (st.upper ? " on" : ""));
  upperSw.onclick = () => {
    st.upper = !st.upper;
    upperSw.classList.toggle("on", st.upper);
    run();
  };
  const upperWrap = h("div", "dt-inline");
  upperWrap.append(upperSw, h("span", "dt-inline-t", "大写"));

  const actions = h("div", "dt-tools");
  actions.append(seg, upperWrap, btn("重新计算", () => run(), true), btn("全复制", () => {
    const lines = ["md5", "sha-1", "sha-256", "sha-512"]
      .map((k) => table.querySelector(`[data-k="${k}"] .v`))
      .filter(Boolean)
      .map((v) => `${v.parentElement.dataset.k}: ${v.textContent}`);
    copyText(lines.join("\n"));
  }));

  const textWrap = field("文本", text);
  const fileWrap = field("文件（≤ 64 MB）", fileInput);
  fileWrap.style.display = mode === "file" ? "" : "none";
  textWrap.style.display = mode === "text" ? "" : "none";

  frag.append(actions, bar, textWrap, fileWrap, table,
    note("MD5 / SHA-1 已被证明不适合做安全校验，这里保留只为核对旧值；新场景用 SHA-256。"));

  let timer = 0;
  const schedule = () => { clearTimeout(timer); timer = setTimeout(run, 200); };

  async function run() {
    let bytes;
    let label;
    if (mode === "file") {
      if (!st.bytes) {
        table.replaceChildren();
        setNote(bar, "选择一个文件后自动计算", "");
        return;
      }
      bytes = st.bytes;
      label = `${st.fileName || "文件"} · ${fmtBytes(st.fileSize || bytes.length)}`;
    } else {
      if (!text.value) {
        table.replaceChildren();
        setNote(bar, "输入文本后自动计算", "");
        return;
      }
      bytes = enc.encode(text.value);
      label = `文本 · ${fmtBytes(bytes.length)} · UTF-8`;
    }
    const { out, err } = await hashAll(bytes);
    table.replaceChildren();
    for (const k of ["md5", "sha-1", "sha-256", "sha-512"]) {
      const v = st.upper ? out[k].toUpperCase() : out[k];
      const row = table.add(k.toUpperCase(), v || "—");
      row.dataset.k = k;
    }
    setNote(bar, err ? err : `${label}`, err ? "warn" : "ok");
  }

  if (st.input) run();
  else setNote(bar, "输入文本后自动计算；文件走本地读取，不上传", "");
  return frag;
}

/* ---------------- 工具 6：UUID ---------------- */

function uuid4() {
  if (window.crypto && crypto.randomUUID) return crypto.randomUUID();
  const b = new Uint8Array(16);
  crypto.getRandomValues(b);
  b[6] = (b[6] & 0x0f) | 0x40;
  b[8] = (b[8] & 0x3f) | 0x80;
  const s = toHex(b);
  return `${s.slice(0, 8)}-${s.slice(8, 12)}-${s.slice(12, 16)}-${s.slice(16, 20)}-${s.slice(20)}`;
}

function toolUuid() {
  const st = MEM.uuid;
  const frag = document.createDocumentFragment();
  const bar = note("", "");
  const out = area(st.output || "", "", true);

  const count = h("input", "dt-num");
  count.type = "number";
  count.min = "1";
  count.max = "200";
  count.value = String(st.count);
  count.onchange = () => {
    const n = Math.max(1, Math.min(200, parseInt(count.value, 10) || 1));
    count.value = String(n);
    st.count = n;
    run();
  };

  const upperSw = h("div", "switch" + (st.upper ? " on" : ""));
  upperSw.onclick = () => { st.upper = !st.upper; upperSw.classList.toggle("on", st.upper); run(); };
  const hySw = h("div", "switch" + (st.hyphen !== false ? " on" : ""));
  hySw.onclick = () => { st.hyphen = st.hyphen === false; hySw.classList.toggle("on", st.hyphen); run(); };

  const upperWrap = h("div", "dt-inline");
  upperWrap.append(upperSw, h("span", "dt-inline-t", "大写"));
  const hyWrap = h("div", "dt-inline");
  hyWrap.append(hySw, h("span", "dt-inline-t", "连字符"));

  const inline = h("div", "dt-tools");
  inline.append(
    h("span", "dt-tag", "数量"), count,
    upperWrap,
    hyWrap,
    btn("重新生成", run, true),
    btn("复制全部", () => copyText(out.value)),
  );

  frag.append(inline, bar, out,
    note("RFC 4122 v4：随机数来自系统 CSPRNG（crypto.getRandomValues），可以直接当主键用。"));

  function run() {
    const n = st.count || 1;
    const list = [];
    for (let i = 0; i < n; i++) {
      let u = uuid4();
      if (st.hyphen === false) u = u.replace(/-/g, "");
      list.push(st.upper ? u.toUpperCase() : u);
    }
    out.value = list.join("\n");
    st.output = out.value;
    setNote(bar, `已生成 ${n} 个 v4 UUID`, "ok");
  }

  run();
  return frag;
}

/* ---------------- 工具 7：颜色 ---------------- */

function hexToRgb(s) {
  let t = String(s).trim().replace(/^#/, "");
  if (/^[0-9a-f]{3,4}$/i.test(t)) t = t.split("").map((c) => c + c).join("");
  if (!/^[0-9a-f]{6}([0-9a-f]{2})?$/i.test(t)) return null;
  return {
    r: parseInt(t.slice(0, 2), 16),
    g: parseInt(t.slice(2, 4), 16),
    b: parseInt(t.slice(4, 6), 16),
    a: t.length === 8 ? parseInt(t.slice(6, 8), 16) : 255,
  };
}

function rgbToHsl({ r, g, b }) {
  const R = r / 255;
  const G = g / 255;
  const B = b / 255;
  const max = Math.max(R, G, B);
  const min = Math.min(R, G, B);
  const l = (max + min) / 2;
  let hh = 0;
  let s = 0;
  if (max !== min) {
    const d = max - min;
    s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
    if (max === R) hh = ((G - B) / d + (G < B ? 6 : 0));
    else if (max === G) hh = (B - R) / d + 2;
    else hh = (R - G) / d + 4;
    hh *= 60;
  }
  return { h: Math.round(hh), s: Math.round(s * 100), l: Math.round(l * 100) };
}

function luminance({ r, g, b }) {
  const f = (v) => {
    const x = v / 255;
    return x <= 0.03928 ? x / 12.92 : Math.pow((x + 0.055) / 1.055, 2.4);
  };
  return 0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b);
}

const contrast = (a, b) => {
  const l1 = luminance(a);
  const l2 = luminance(b);
  return (Math.max(l1, l2) + 0.05) / (Math.min(l1, l2) + 0.05);
};

function toolColor() {
  const st = MEM.color;
  const frag = document.createDocumentFragment();
  const bar = note("", "");
  const swatch = h("div", "dt-swatch");
  const table = kvTable();

  const hex = h("input", "dt-input mono");
  hex.type = "text";
  hex.value = st.hex || "#0F6CBD";
  hex.placeholder = "#0F6CBD / #fff / #0F6CBDCC";

  const picker = h("input", "dt-picker");
  picker.type = "color";
  picker.value = (st.hex || "#0F6CBD").slice(0, 7);

  const actions = h("div", "dt-tools");
  actions.append(h("span", "dt-tag", "HEX"), hex, picker, btn("复制 HEX", () => copyText(table.dataset.hex || "")));

  frag.append(actions, bar, swatch, table,
    note("HEX8 的最后两位是 alpha；对比度按 WCAG 2.1 的相对亮度算，正文建议 ≥ 4.5:1。"));

  hex.oninput = () => { st.hex = hex.value; run(); };
  picker.oninput = () => { hex.value = picker.value.toUpperCase(); st.hex = hex.value; run(); };

  function run() {
    const rgb = hexToRgb(hex.value);
    if (!rgb) {
      setNote(bar, "不是合法的颜色值：支持 #rgb / #rgba / #rrggbb / #rrggbbaa", "err");
      swatch.style.background = "repeating-linear-gradient(45deg, var(--card-2) 0 6px, var(--line) 6px 12px)";
      table.replaceChildren();
      return;
    }
    const { r, g, b, a } = rgb;
    const hsl = rgbToHsl(rgb);
    const hx = `#${[r, g, b].map((x) => x.toString(16).padStart(2, "0")).join("")}`.toUpperCase();
    const hxa = `${hx}${a.toString(16).padStart(2, "0")}`.toUpperCase();
    const k = 1 - Math.max(r, g, b) / 255;
    const c = k === 1 ? 0 : (1 - r / 255 - k) / (1 - k);
    const m = k === 1 ? 0 : (1 - g / 255 - k) / (1 - k);
    const y = k === 1 ? 0 : (1 - b / 255 - k) / (1 - k);
    const pct = (v) => Math.round(v * 100) + "%";

    swatch.style.background = hxa;
    table.dataset.hex = hx;
    table.replaceChildren();
    table.add("HEX", hx);
    table.add("HEX8", hxa);
    table.add("RGB", `rgb(${r}, ${g}, ${b})`);
    if (a !== 255) table.add("RGBA", `rgba(${r}, ${g}, ${b}, ${(a / 255).toFixed(2)})`);
    table.add("HSL", `hsl(${hsl.h}, ${hsl.s}%, ${hsl.l}%)`);
    table.add("CMYK", `cmyk(${pct(c)}, ${pct(m)}, ${pct(y)}, ${pct(k)})`);
    table.add("对比白字", contrast(rgb, { r: 255, g: 255, b: 255 }).toFixed(2) + " : 1");
    table.add("对比黑字", contrast(rgb, { r: 0, g: 0, b: 0 }).toFixed(2) + " : 1");
    setNote(bar, `已解析 · 相对亮度 ${luminance(rgb).toFixed(3)}`, "ok");
  }

  run();
  return frag;
}

/* ---------------- 工具 8：JWT ---------------- */

function b64urlDecode(seg) {
  let t = String(seg).replace(/-/g, "+").replace(/_/g, "/");
  const pad = t.length % 4;
  if (pad === 1) throw new Error("长度不是 4 的倍数");
  if (pad) t += "=".repeat(4 - pad);
  const bin = atob(t);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return decFatal.decode(bytes);
}

function toolJwt() {
  const st = MEM.jwt;
  const frag = document.createDocumentFragment();
  const bar = note("", "");
  const body = h("div", "dt-jwt");
  const input = area(st.input || "", "粘贴 JWT（header.payload.signature）…");
  input.oninput = () => { st.input = input.value; schedule(); };

  const actions = h("div", "dt-tools");
  actions.append(btn("解析", () => run(), true), btn("清空", () => {
    input.value = "";
    st.input = "";
    body.replaceChildren();
    setNote(bar, "粘贴一个 JWT 再点解析", "");
  }));

  frag.append(actions, bar, field("Token", input), body,
    note("只解码不验签：签名是否正确需要服务端密钥，这里不做判断。"));

  let timer = 0;
  const schedule = () => { clearTimeout(timer); timer = setTimeout(run, 200); };

  function run() {
    const raw = input.value.trim().replace(/^Bearer\s+/i, "");
    if (!raw) {
      body.replaceChildren();
      setNote(bar, "粘贴一个 JWT 再点解析", "");
      return;
    }
    const parts = raw.split(".");
    if (parts.length !== 3) {
      body.replaceChildren();
      setNote(bar, `不是 JWT：应为 header.payload.signature 三段，当前 ${parts.length} 段`, "err");
      return;
    }
    // 头和载荷分开报错：只给"解析失败"看不出是哪一段坏
    const decoded = [];
    for (const [name, seg] of [["头部", parts[0]], ["载荷", parts[1]]]) {
      try {
        decoded.push(JSON.parse(b64urlDecode(seg)));
      } catch (e) {
        body.replaceChildren();
        const isJson = e instanceof SyntaxError;
        setNote(bar, isJson
          ? `${name}不是合法 JSON`
          : `${name}不是合法的 Base64URL（${String(e.message || e)}）`, "err");
        return;
      }
    }
    const head = decoded[0];
    const payload = decoded[1];
    body.replaceChildren();

    const now = Math.floor(Date.now() / 1000);
    const claims = [];
    if (typeof payload.exp === "number") {
      claims.push(payload.exp > now
        ? `未过期，剩余 ${relative(payload.exp * 1000)}`
        : `已过期 ${relative(payload.exp * 1000)}`);
    }
    setNote(bar, claims.length ? claims[0] : "已解析 · 载荷里没有 exp", payload.exp && payload.exp <= now ? "err" : "ok");

    const meta = kvTable();
    meta.add("alg", head.alg || "—");
    meta.add("typ", head.typ || "—");
    if (typeof payload.iat === "number") meta.add("iat", new Date(payload.iat * 1000).toLocaleString("zh-CN", { hour12: false }));
    if (typeof payload.exp === "number") meta.add("exp", new Date(payload.exp * 1000).toLocaleString("zh-CN", { hour12: false }));
    if (typeof payload.nbf === "number") meta.add("nbf", new Date(payload.nbf * 1000).toLocaleString("zh-CN", { hour12: false }));
    meta.add("签名", parts[2].slice(0, 24) + (parts[2].length > 24 ? "…" : ""));
    body.append(meta);

    const pre = (title, obj) => {
      const wrap = h("div", "dt-jwt-sec");
      wrap.append(h("div", "dt-jwt-h", title));
      const code = h("pre", "dt-hl mono");
      code.textContent = JSON.stringify(obj, null, 2);
      wrap.append(code);
      body.append(wrap);
    };
    pre("HEADER", head);
    pre("PAYLOAD", payload);
  }

  if (st.input) run();
  else setNote(bar, "粘贴一个 JWT 再点解析", "");
  return frag;
}

/* ---------------- 工具表与挂载 ---------------- */

const TOOLS = [
  { id: "json", name: "JSON", hint: "格式化 / 压缩 / 校验", render: toolJson },
  { id: "codec", name: "编码解码", hint: "Base64 / URL / HTML 实体 / Unicode", render: toolCodec },
  { id: "time", name: "时间戳", hint: "秒 / 毫秒 / 本地时间互转", render: toolTime },
  { id: "regex", name: "正则", hint: "实时匹配高亮与替换", render: toolRegex },
  { id: "hash", name: "哈希", hint: "MD5 / SHA-1 / SHA-256 / SHA-512", render: toolHash },
  { id: "uuid", name: "UUID", hint: "批量生成 v4", render: toolUuid },
  { id: "color", name: "颜色", hint: "HEX / RGB / HSL / CMYK", render: toolColor },
  { id: "jwt", name: "JWT", hint: "解码头部与载荷", render: toolJwt },
];

// 每个工具的输入存在这里：切走再切回来内容还在（页面重绘会销毁 DOM）
const MEM = {
  json: {
    input: '{\n  "name": "zhuxia",\n  "tools": ["json", "base64", "regex"],\n  "local": true\n}',
    mode: "pretty",
    indent: 2,
    sort: false,
  },
  codec: { mode: "b64", input: "驻匣 · Zhuxia", output: "", urlSafe: true },
  time: { input: "", unit: "auto" },
  regex: { pattern: "\\b\\w+@\\w+\\.\\w+", flags: { g: true, i: true, m: false, s: false, u: false, y: false }, text: "联系 a@b.com 或 dev@zhuxia.fun，无效：not-an-email", replace: "[邮箱]" },
  hash: { mode: "text", input: "", upper: false, bytes: null },
  uuid: { count: 5, upper: false, hyphen: true, output: "" },
  color: { hex: "#0F6CBD" },
  jwt: { input: "" },
};

let CUR = "json";
let HOST = null;
let RAIL = null;
let BODY = null;

function markRail() {
  RAIL.querySelectorAll("button").forEach((b) => {
    b.classList.toggle("on", b.dataset.tool === CUR);
  });
}

function mount(host) {
  HOST = host;
  HOST.classList.add("dthost");
  HOST.innerHTML = "";

  RAIL = h("div", "dt-rail");
  for (const t of TOOLS) {
    const b = h("button", t.id === CUR ? "on" : "", t.name);
    b.type = "button";
    b.dataset.tool = t.id;
    b.title = t.hint;
    b.onclick = () => {
      if (CUR === t.id) return;
      CUR = t.id;
      markRail();
      render();
    };
    RAIL.append(b);
  }
  BODY = h("div", "dt-body");
  HOST.append(RAIL, BODY);
  render();
}

function render() {
  const t = TOOLS.find((x) => x.id === CUR) || TOOLS[0];
  BODY.replaceChildren(t.render());
}

window.DevTools = { mount };
})();
