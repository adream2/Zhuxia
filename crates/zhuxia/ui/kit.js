// 任务栏条目顺序（外观页拖拽排序、采样页都用这一份）
const SLOTS = ["CPU", "内存", "磁盘", "GPU", "网络"];

/* ---------- 公共控件：DOM 构造与轻提示（自 app.js 拆出） ---------- */

function h(tag, cls, text) {
  const el = document.createElement(tag);
  if (cls) el.className = cls;
  if (text != null) el.textContent = text;
  return el;
}

function toast(msg) {
  const t = document.getElementById("toast");
  t.textContent = msg;
  t.classList.add("show");
  clearTimeout(toast._t);
  toast._t = setTimeout(() => t.classList.remove("show"), 1400);
}


/* ---------- 公共控件：行、开关、分段、下拉、输入、滑杆（自 app.js 拆出） ---------- */

/* ---------- 控件 ---------- */
function row(label, hint, ctl, disabled = false, extra = null) {
  const r = h("div", "row" + (disabled ? " disabled" : ""));
  const t = h("div", "text");
  t.append(h("div", "label", label));
  if (hint) t.append(h("div", "hint", hint));
  const c = h("div", "ctl");
  if (ctl) c.append(ctl);
  if (extra) c.append(extra);
  r.append(t, c);
  return r;
}

function sw(on, onChange) {
  const el = h("div", "switch" + (on ? " on" : ""));
  el.onclick = () => onChange(!on);
  return el;
}

function seg(options, cur, onChange) {
  const box = h("div", "seg");
  options.forEach(([val, label]) => {
    const b = h("button", val === cur ? "on" : "", label);
    b.onclick = () => { if (val !== cur) onChange(val); };
    box.append(b);
  });
  return box;
}

function sel(options, cur, onChange) {
  const s = h("select");
  options.forEach(([val, label]) => {
    const o = h("option", "", label);
    o.value = String(val);
    if (String(val) === String(cur)) o.selected = true;
    s.append(o);
  });
  s.onchange = () => onChange(s.value);
  return s;
}

function textInput(value, placeholder, onCommit) {
  const i = h("input");
  i.type = "text";
  i.value = value || "";
  i.placeholder = placeholder || "";
  i.style.minWidth = "220px";
  i.onchange = () => onCommit(i.value.trim());
  return i;
}

// 多选小标签：ICO 规格这类“勾几个”的控件（至少保留一个）
function chips(options, selected, onChange) {
  const box = h("div", "chips");
  const cur = new Set((selected || []).map(Number));
  options.forEach((o) => {
    const b = h("button", "chip" + (cur.has(o) ? " on" : ""), o + "px");
    b.onclick = () => {
      if (cur.has(o)) {
        if (cur.size <= 1) return;
        cur.delete(o);
      } else {
        cur.add(o);
      }
      onChange([...cur].sort((a, b) => b - a));
    };
    box.append(b);
  });
  return box;
}

// 带范围和单位的滑块（音量 0–200% 这类）
function sliderR(min, max, v, unit, onCommit) {
  const i = h("input");
  i.type = "range";
  i.min = String(min); i.max = String(max); i.step = "1";
  i.value = String(Math.round(v));
  const lab = h("span", "", Math.round(v) + unit);
  lab.style.color = "var(--text-2)";
  lab.style.width = "46px";
  i.oninput = () => { lab.textContent = i.value + unit; };
  i.onchange = () => onCommit(parseInt(i.value, 10));
  const w = h("div", "ctl");
  w.append(i, lab);
  return w;
}

function slider(v, onCommit) {
  const i = h("input");
  i.type = "range";
  i.min = "20"; i.max = "100"; i.step = "1";
  i.value = Math.round(v * 100);
  const lab = h("span", "", Math.round(v * 100) + "%");
  lab.style.color = "var(--text-2)";
  lab.style.width = "38px";
  i.oninput = () => { lab.textContent = i.value + "%"; };
  i.onchange = () => onCommit(parseInt(i.value, 10) / 100);
  const w = h("div", "ctl");
  w.append(i, lab);
  return w;
}


/* ---------- 公共控件：数字/多行输入、标签、音量滑杆（自 app.js 拆出） ---------- */

function numInput(value, min, max, onCommit) {
  const i = h("input");
  i.type = "number";
  i.min = String(min);
  i.max = String(max);
  i.value = String(value);
  i.style.width = "110px";
  i.onchange = () => {
    let v = parseInt(i.value, 10);
    if (!Number.isFinite(v)) v = value;
    v = Math.max(min, Math.min(max, v));
    i.value = String(v);
    onCommit(v);
  };
  return i;
}

function textArea(value, rows, placeholder, onCommit) {
  const t = h("textarea");
  t.value = value || "";
  t.rows = rows || 4;
  t.placeholder = placeholder || "";
  t.spellcheck = false;
  t.onchange = () => onCommit(t.value);
  return t;
}

function chip(label, on, onToggle) {
  const c = h("button", "chip" + (on ? " on" : ""), label);
  c.onclick = () => onToggle(!on);
  return c;
}

function volSlider(v, onCommit) {
  const i = h("input");
  i.type = "range";
  i.min = "0";
  i.max = "100";
  i.step = "5";
  i.value = String(Math.round(v * 100));
  const lab = h("span", "", i.value + "%");
  lab.style.color = "var(--text-2)";
  lab.style.width = "38px";
  i.oninput = () => { lab.textContent = i.value + "%"; };
  i.onchange = () => onCommit(parseInt(i.value, 10) / 100);
  const w = h("div", "ctl");
  w.append(i, lab);
  return w;
}


/* ---------- 数值格式化（全局唯一一份） ----------
 * 以前 traffic.js 和 imgconv.js 各写了一个 `fmtBytes`：两个文件都没包 IIFE，
 * classic script 共享全局，后加载的 imgconv 版（封顶 MB）把 traffic 版（有 GB/TB 档）
 * 覆盖掉了——流量页 3 GB 显示成「3072.00 MB」。这类"同名覆盖"不会报错，只会静默出错，
 * 所以公共格式化统一放这里，页面里不要再定义同名函数。
 */
function fmtBytes(n) {
  n = Number(n);
  if (!Number.isFinite(n)) return "-";
  if (n <= 0) return "0 B";
  if (n < 1024) return Math.round(n) + " B";
  if (n < 1048576) return (n / 1024).toFixed(1) + " KB";
  if (n < 1073741824) return (n / 1048576).toFixed(1) + " MB";
  if (n < 1099511627776) return (n / 1073741824).toFixed(2) + " GB";
  return (n / 1099511627776).toFixed(2) + " TB";
}
