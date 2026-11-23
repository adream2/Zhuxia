/* 图床：设置面板里的一页（顶部 tab：上传 / 图片 / 设置）。
 *
 * 整段包在 IIFE 里：它和 app.js 共享同一个全局作用域，`const inv` / `function h`
 * 这类重名会直接让后加载的脚本报 SyntaxError。对外只暴露 window.ImgBed。
 */
(() => {
const inv = (cmd, args) => window.__TAURI__.core.invoke(cmd, args);

const TABS = [["upload", "上传"], ["images", "图片"], ["settings", "设置"]];

let S = null;                 // imgbed_state 的快照
// 当前 tab（切走再回来还停在原地）。支持 `?tab=images` 直达，便于排查与截图。
let TAB = new URLSearchParams(location.search).get("tab") || "upload";
if (!TABS.some(([id]) => id === TAB)) TAB = "upload";
let HOST = null;              // app.js 给的容器
let BODY = null;              // tab 内容区
let TABS_BOX = null;
let STATUS = null;
let MODAL = null;
let ITEMS = null;
let GRID = null;
let GO = null;
let PICKLIST = null;

const F = { items: [], dir: "", busy: false, pick: null };
const IMG = { source: "local", q: "", sort: "newest", provider: "", local: [], remote: [], total: 0 };
let seq = 0;
let dropHooked = false;

function h(tag, cls, text) {
  const el = document.createElement(tag);
  if (cls) el.className = cls;
  if (text != null) el.textContent = text;
  return el;
}

// 提示条是面板共用的那个（app.js 里的 toast），这里不重复实现
const toast = (msg) => (window.toast ? window.toast(msg) : console.log(msg));

const fmtBytes = (n) => {
  n = Number(n) || 0;
  if (n >= 1048576) return (n / 1048576).toFixed(1) + " MB";
  if (n >= 1024) return Math.round(n / 1024) + " KB";
  return n + " B";
};
const baseName = (p) => String(p).split(/[\\/]/).pop();
const stem = (n) => String(n).replace(/\.[^.]+$/, "");

function tokens(s) {
  const d = new Date();
  const p = (n) => String(n).padStart(2, "0");
  return String(s || "")
    .replace(/\{yyyy\}/g, String(d.getFullYear()))
    .replace(/\{MM\}/g, p(d.getMonth() + 1))
    .replace(/\{dd\}/g, p(d.getDate()))
    .replace(/\{HH\}/g, p(d.getHours()))
    .replace(/\{mm\}/g, p(d.getMinutes()));
}

function copyText(text) {
  inv("ib_copy", { text }).then((r) => {
    toast(r && r.ok ? "已复制" : "复制失败（剪贴板被占用）");
  });
}

function linkText(kind, r) {
  if (!r) return "";
  const name = stem(r.name || "image");
  if (kind === "md") return `![${name}](${r.url})`;
  if (kind === "html") return `<img src="${r.url}" alt="${name}" />`;
  if (kind === "bb") return `[img]${r.url}[/img]`;
  return r.url;
}

function providerMeta() {
  const id = S && S.provider;
  return ((S && S.metas) || []).find((m) => m.id === id) || { id, name: id || "", caps: {}, fields: [] };
}
function providerConfig(id) {
  const v = (S && S.providers || {})[id];
  return v && typeof v === "object" ? v : {};
}

/* ---------- 骨架 ---------- */

function mount(host) {
  HOST = host;
  HOST.classList.add("ibhost");
  HOST.innerHTML = "";

  const head = h("div", "ibhead");
  TABS_BOX = h("div", "tabs");
  for (const [id, label] of TABS) {
    const b = h("button", id === TAB ? "on" : "", label);
    b.dataset.tab = id;
    b.onclick = () => {
      if (TAB === id) return;
      TAB = id;
      renderTab();
    };
    TABS_BOX.append(b);
  }
  STATUS = h("div", "ibstatus");
  head.append(TABS_BOX, STATUS);
  HOST.append(head);

  BODY = h("div", "ibbody");
  HOST.append(BODY);

  ensureChrome();
  if (S) {
    renderTab();
  } else {
    BODY.append(h("div", "empty", "加载中…"));
    load().then(renderTab).catch((e) => {
      BODY.replaceChildren(h("div", "empty", "初始化失败：" + e));
    });
  }
}

async function load() {
  S = await inv("imgbed_state");
  F.dir = S.path || "";
  IMG.source = (S.browser && S.browser.source) || "local";
  IMG.sort = (S.browser && S.browser.sort) || "newest";
}

function renderTab() {
  if (!BODY) return;
  for (const b of TABS_BOX.querySelectorAll("button")) {
    b.classList.toggle("on", b.dataset.tab === TAB);
  }
  paintStatus();
  if (THUMB_OBS) { THUMB_OBS.disconnect(); THUMB_OBS = null; }
  BODY.innerHTML = "";
  ITEMS = null;
  GRID = null;
  GO = null;
  try {
    if (TAB === "upload") renderUpload();
    else if (TAB === "images") renderImages();
    else renderSettings();
  } catch (e) {
    BODY.append(h("div", "card", "页面渲染出错：" + (e && e.message ? e.message : e)));
  }
}

function chip(text, cls) {
  return h("span", "chip" + (cls ? " " + cls : ""), text);
}

function paintStatus() {
  if (!STATUS || !S) return;
  STATUS.innerHTML = "";
  const m = providerMeta();
  STATUS.append(chip(m.name || S.provider));
  STATUS.append(chip(tokens(F.dir || S.path || "/")));
  STATUS.append(chip(S.configured ? "已配置" : "未配置", S.configured ? "ok" : "bad"));
  if (S.alive) STATUS.append(chip("常驻运行中"));
}

/* ---------- 上传页 ---------- */

function addPaths(paths) {
  for (const p of paths) {
    if (!p) continue;
    const it = { id: ++seq, name: baseName(p), path: p, status: "等待", error: "", result: null, preview: "", width: 0, height: 0, bytes: 0 };
    F.items.push(it);
    inv("ib_preview", { path: p })
      .then((r) => {
        if (r && r.ok) {
          it.preview = r.dataUrl;
          it.width = r.width;
          it.height = r.height;
          it.bytes = r.bytes;
        } else if (r) {
          it.width = r.width || 0;
          it.height = r.height || 0;
          it.bytes = r.bytes || 0;
          if (r.error) it.error = r.error;
        }
        renderItems();
      })
      .catch(() => {});
  }
  renderItems();
}

function pasteClipboard() {
  inv("ib_clipboard").then((r) => {
    if (!r || !r.ok) { toast(r && r.error ? r.error : "剪贴板里没有图片"); return; }
    F.items.push({
      id: ++seq, name: r.name || "剪贴板图片.png", data: r.data, status: "等待",
      error: "", result: null, preview: r.dataUrl, width: r.width, height: r.height, bytes: r.bytes,
    });
    renderItems();
  });
}

function renderItems() {
  if (!ITEMS) return;
  ITEMS.innerHTML = "";
  if (!F.items.length) return;
  for (const it of F.items) {
    const row = h("div", "item");
    const ico = h("div", "ico");
    if (it.preview) ico.style.backgroundImage = `url(${it.preview})`;
    const nm = h("div", "nm");
    nm.append(h("div", "", it.name));
    const sub = h("div", "sub2", [it.bytes ? fmtBytes(it.bytes) : "", it.width ? `${it.width}×${it.height}` : ""].filter(Boolean).join(" · "));
    nm.append(sub);
    const orig = h("div", "sub2", "—");
    const after = h("div", "sub2", "—");
    if (it.result) {
      after.textContent = fmtBytes(it.result.bytes);
      if (it.result.note) after.title = it.result.note;
    }
    const st = h("div", "st");
    if (it.status === "等待") { st.textContent = it.error || "等待上传"; st.className += " wait"; }
    else if (it.status === "上传中") { st.textContent = "上传中…"; st.className += " busy"; }
    else if (it.status === "完成") { st.textContent = "完成"; st.className += " ok"; }
    else { st.textContent = "失败"; st.className += " err"; st.title = it.error || ""; }
    row.append(ico, nm, orig, after, st);
    if (it.status === "完成" && it.result) {
      const line = h("div", "link");
      line.append(h("code", "", it.result.url.length > 76 ? it.result.url.slice(0, 76) + "…" : it.result.url));
      for (const [k, label] of [["md", "MD"], ["url", "URL"], ["html", "HTML"], ["bb", "BBCode"]]) {
        const b = h("button", "btn-mini", label);
        b.onclick = () => copyText(linkText(k, it.result));
        line.append(b);
      }
      row.append(line);
    } else if (it.status === "失败" && it.error) {
      const line = h("div", "link");
      line.append(h("code", "", it.error));
      row.append(line);
    }
    ITEMS.append(row);
  }
}

function setParam(patch) {
  inv("imgbed_patch", { patch }).then((s) => {
    S = s;
    F.dir = s.path || "";
    renderTab();
  });
}

function renderUpload() {

  const drop = h("div", "drop");
  drop.append(h("div", "", "把图片拖到这里"));
  const acts = h("div", "acts");
  const pick = h("button", "btn primary", "选择文件");
  pick.onclick = async () => {
    const r = await inv("ib_pick_files");
    if (r && r.paths && r.paths.length) addPaths(r.paths);
  };
  const paste = h("button", "btn", "粘贴剪贴板");
  paste.onclick = pasteClipboard;
  acts.append(pick, paste);
  drop.append(acts);
  // 拖拽走 Tauri 的原生事件（`hookDrop()` → 真实路径 → `addPaths`）。
  // `dragDropEnabled` 默认开着时 HTML5 的 drop 收不到事件，这条分支以前是死代码；
  // 两套同时存在还会有“同一张图入队两次”的风险，所以只保留原生这一条。
  BODY.append(drop);

  const up = S.upload || {};
  const bar = h("div", "params");

  const conv = h("select");
  [["webp", "转 WebP"], ["keep", "原样上传"]].forEach(([v, t]) => {
    const o = h("option", "", t); o.value = v; if (v === up.convert) o.selected = true; conv.append(o);
  });
  conv.onchange = () => setParam({ upload: { convert: conv.value } });

  const q = h("input"); q.type = "number"; q.min = "1"; q.max = "100"; q.value = up.quality; q.style.width = "64px";
  q.disabled = up.convert !== "webp";
  q.title = "WebP 质量，1–100";
  q.onchange = () => {
    const n = Math.max(1, Math.min(100, parseInt(q.value, 10) || 82));
    q.value = String(n);
    setParam({ upload: { quality: n } });
  };

  const side = h("input"); side.type = "number"; side.min = "0"; side.max = "8192"; side.value = up.maxSide; side.style.width = "84px";
  side.disabled = up.convert !== "webp";
  side.title = "长边上限：0 = 不限制；超出则等比缩小";
  side.onchange = () => {
    let n = parseInt(side.value, 10);
    if (!Number.isFinite(n) || n < 0) n = 0;
    n = Math.min(8192, n);
    side.value = String(n);
    setParam({ upload: { maxSide: n } });
  };

  const path = h("input"); path.type = "text"; path.value = F.dir; path.placeholder = "（根目录）images/{yyyy}/{MM}";
  path.style.minWidth = "200px";
  path.onchange = () => {
    F.dir = path.value.trim();
    inv("imgbed_provider_patch", { provider: S.provider, patch: { path: F.dir } }).then((s) => { S = s; F.dir = s.path; renderTab(); });
  };
  const browse = h("button", "btn-mini", "浏览");
  browse.onclick = () => openPicker(path.value || F.dir);
  if (!providerMeta().caps || !providerMeta().caps.listDirs) browse.style.display = "none";

  const f1 = h("div", "field"); f1.append(h("span", "", "格式"), conv);
  const f2 = h("div", "field"); f2.append(h("span", "", "质量"), q);
  const f3 = h("div", "field"); f3.append(h("span", "", "长边"), side);
  const f5 = h("div", "field grow"); f5.append(h("span", "", "路径"), path, browse);
  bar.append(f1, f2, f3, f5);
  BODY.append(bar);

  const pv = h("div", "path-preview");
  pv.append(h("span", "", "最终路径："));
  pv.append(h("b", "", tokens(F.dir || "") || "（根目录）"));
  if (up.convert === "webp") {
    pv.append(h("span", "", up.maxSide ? `　长边 ≤ ${up.maxSide}px（等比缩）` : "　长边不限"));
  }
  BODY.append(pv);

  ITEMS = h("div", "items");
  BODY.append(ITEMS);

  const tail = h("div", "tools");
  tail.style.marginTop = "12px";
  tail.style.justifyContent = "flex-end";
  const clear = h("button", "btn", "清空列表");
  clear.onclick = () => { F.items = F.items.filter((i) => i.status === "上传中"); renderItems(); };
  const pending = F.items.filter((i) => i.status === "等待" || i.status === "失败").length;
  GO = h("button", "btn primary", F.busy ? "上传中…" : `上传 ${pending} 张`);
  GO.disabled = F.busy || pending === 0;
  GO.onclick = startUpload;
  tail.append(clear, GO);
  BODY.append(tail);

  renderItems();
}


async function startUpload() {
  if (F.busy) return;
  const pending = F.items.filter((i) => i.status === "等待" || i.status === "失败");
  if (!pending.length) { toast("没有待上传的图片"); return; }
  F.busy = true;
  if (GO) { GO.disabled = true; GO.textContent = "上传中…"; }
  const dir = F.dir || "";
  const done = [];
  for (const it of pending) {
    it.status = "上传中";
    renderItems();
    let r = null;
    try {
      if (it.path) {
        r = await inv("ib_upload_paths", { paths: [it.path], dir });
      } else if (it.data) {
        r = await inv("ib_upload_b64", { name: it.name, data: it.data, dir });
      }
    } catch (e) {
      it.error = String(e);
    }
    const res = r && r.items && r.items[0];
    if (res && res.ok) {
      it.status = "完成";
      it.result = res;
      it.error = "";
      done.push(res);
    } else {
      it.status = "失败";
      it.error = res ? res.error : (it.error || "没有返回结果");
    }
    renderItems();
  }
  F.busy = false;
  if (done.length && S.copy && S.copy.auto) {
    copyText(done.map((d) => d.text).join("\n"));
  }
  try { S = await inv("imgbed_state"); } catch (_) {}
  renderTab();
}

/* 路径选择器 */
function openPicker(start) {
  F.pick = { path: tokens(start || "").replace(/^\/+|\/+$/g, ""), dirs: [] };
  showPicker();
}
function showPicker() {
  ensureChrome();
  MODAL.hidden = false;
  MODAL.innerHTML = "";
  const box = h("div", "lightbox");
  box.style.width = "520px";
  const top = h("div", "top");
  top.append(h("div", "t", "选择已有目录"));
  const close = h("button", "btn-mini", "关闭");
  close.onclick = () => { MODAL.hidden = true; };
  top.append(close);
  const body = h("div");
  body.style.padding = "14px";
  const head = h("div", "pick-head");
  const cur = h("code", "", "/" + (F.pick.path || ""));
  const upBtn = h("button", "btn-mini", "上一级");
  upBtn.onclick = () => {
    F.pick.path = F.pick.path.split("/").slice(0, -1).join("/");
    loadPicker();
  };
  const choose = h("button", "btn primary", "用到这个目录");
  choose.onclick = () => {
    F.dir = F.pick.path;
    inv("imgbed_provider_patch", { provider: S.provider, patch: { path: F.dir } }).then((s) => {
      S = s; F.dir = s.path; MODAL.hidden = true; renderTab();
    });
  };
  head.append(upBtn, cur, choose);
  body.append(head);
  PICKLIST = h("div", "pick-list");
  body.append(PICKLIST);
  box.append(top, body);
  MODAL.append(box);
  loadPicker();
}
function loadPicker() {
  if (!PICKLIST || !F.pick) return;
  PICKLIST.textContent = "加载中…";
  const box = PICKLIST;
  inv("ib_paths", { dir: F.pick.path }).then((r) => {
    if (!box.isConnected && box !== PICKLIST) return;
    box.innerHTML = "";
    if (r && r.error) box.append(h("div", "note", r.error));
    if (!r || !r.dirs || !r.dirs.length) {
      box.append(h("div", "note", "这个目录下没有子目录"));
      return;
    }
    for (const d of r.dirs) {
      const b = h("button", "", "📁 " + d);
      b.onclick = () => { F.pick.path = (F.pick.path ? F.pick.path + "/" : "") + d; loadPicker(); };
      box.append(b);
    }
  });
}

/* ---------- 图片页 ---------- */

function renderImages() {

  const tools = h("div", "tools");
  const seg = h("div", "seg");
  for (const [v, t] of [["local", "本地上传记录"], ["remote", "远端目录"]]) {
    const b = h("button", v === IMG.source ? "on" : "", t);
    b.onclick = () => {
      IMG.source = v;
      paintStatus();
      inv("imgbed_patch", { patch: { browser: { source: v } } }).then((s) => { S = s; });
      renderTab();
    };
    seg.append(b);
  }
  const q = h("input"); q.type = "search"; q.placeholder = "搜索文件名 / 路径"; q.value = IMG.q;
  q.onchange = () => { IMG.q = q.value.trim(); renderTab(); };
  const sort = h("select");
  for (const [v, t] of [["newest", "最新"], ["oldest", "最早"], ["name", "名称"], ["size", "大小"]]) {
    const o = h("option", "", t); o.value = v; if (v === IMG.sort) o.selected = true; sort.append(o);
  }
  sort.onchange = () => {
    IMG.sort = sort.value;
    inv("imgbed_patch", { patch: { browser: { sort: sort.value } } }).then((s) => { S = s; });
    renderTab();
  };
  const refresh = h("button", "btn", "刷新");
  refresh.onclick = () => renderTab();
  const openDir = h("button", "btn-mini", "数据目录");
  openDir.onclick = () => inv("ib_open_dir");
  tools.append(seg, q, sort, refresh, openDir);
  BODY.append(tools);
  BODY.append(h("div", "note", "本地上传记录跨平台；远端目录取决于当前平台能力（自定义 HTTP 没有这个能力）。"));

  GRID = h("div", "thumbs");
  BODY.append(GRID);
  loadGrid();
}

// 缩略图懒加载：120 条记录全量拉 base64 会把渲染进程的位图缓存顶起来。
// 只给进入视口（含 200px 预取）的卡片取图，切走 tab 时 DOM 一起清掉。
let THUMB_OBS = null;
function thumbObserver() {
  if (THUMB_OBS || typeof IntersectionObserver === "undefined") return THUMB_OBS;
  THUMB_OBS = new IntersectionObserver((entries) => {
    for (const e of entries) {
      if (!e.isIntersecting) continue;
      const fn = e.target._loadThumb;
      e.target._loadThumb = null;
      try { if (fn) fn(); } catch (_) {}
      THUMB_OBS.unobserve(e.target);
    }
  }, { root: null, rootMargin: "200px" });
  return THUMB_OBS;
}

function loadGrid() {
  const grid = GRID;
  if (!grid) return;
  grid.innerHTML = "";
  if (IMG.source === "local") {
    inv("ib_history", { q: IMG.q, provider: IMG.provider, sort: IMG.sort, offset: 0, limit: 120 }).then((r) => {
      IMG.local = r.items || [];
      IMG.total = r.total || IMG.local.length;
      grid.innerHTML = "";
      if (!IMG.local.length) {
        grid.append(h("div", "empty", IMG.total ? "没有匹配的图片" : "还没有上传记录"));
        return;
      }
      for (const it of IMG.local) grid.append(thumbCard(it, "local"));
    });
  } else {
    const dir = tokens(F.dir || "");
    inv("ib_remote", { dir }).then((r) => {
      grid.innerHTML = "";
      if (r && !r.supported) { grid.append(h("div", "empty", "当前平台不支持远端浏览")); return; }
      if (r && r.error) { grid.append(h("div", "empty", r.error)); return; }
      IMG.remote = r.items || [];
      if (!IMG.remote.length) { grid.append(h("div", "empty", "这个目录里没有图片")); return; }
      for (const it of IMG.remote) grid.append(thumbCard(it, "remote"));
    });
  }
}

function thumbCard(it, kind) {
  const el = h("div", "thumb");
  const img = h("div", "img");
  const meta = h("div", "meta");
  meta.append(h("div", "", it.name));
  meta.append(h("div", "", [fmtBytes(it.bytes), it.format || ""].filter(Boolean).join(" · ")));
  el.append(img, meta);
  el.onclick = () => openViewer(it, kind, el);
  el._loadThumb = () => {
    if (kind === "local" && it.id) {
      inv("ib_thumb", { id: it.id }).then((r) => { if (r && r.ok && el.isConnected) img.style.backgroundImage = `url(${r.dataUrl})`; });
    } else if (it.url) {
      inv("ib_thumb_url", { url: it.url }).then((r) => { if (r && r.ok && el.isConnected) img.style.backgroundImage = `url(${r.dataUrl})`; });
    }
  };
  const obs = thumbObserver();
  if (obs) obs.observe(el); else el._loadThumb();
  return el;
}

function openViewer(it, kind, cardEl) {
  ensureChrome();
  MODAL.hidden = false;
  MODAL.innerHTML = "";
  const box = h("div", "lightbox");
  const top = h("div", "top");
  top.append(h("div", "t", it.name || ""));
  const open = h("button", "btn-mini", "打开原图");
  open.onclick = () => inv("ib_open", { url: it.url });
  const close = h("button", "btn-mini", "关闭");
  close.onclick = () => { MODAL.hidden = true; };
  top.append(open, close);
  const body = h("div", "body");
  const pic = h("div", "pic");
  const im = h("img");
  const bg = cardEl ? cardEl.querySelector(".img").style.backgroundImage : "";
  if (bg) im.src = bg.slice(5, -2);
  pic.append(im);
  const side = h("div", "side");
  const kv = (k, v) => { side.append(h("div", "k", k)); side.append(h("div", "v", v)); };
  kv("平台", it.provider || S.provider);
  kv("路径", it.path || "");
  kv("体积", fmtBytes(it.bytes));
  if (it.width) kv("尺寸", `${it.width} × ${it.height}`);
  if (it.createdAt) kv("时间", new Date(it.createdAt * 1000).toLocaleString());
  kv("链接", it.url || "");
  const links = h("div", "links");
  const rec = { name: it.name || "image", url: it.url || "" };
  for (const [k, label] of [["md", "Markdown"], ["url", "URL"], ["html", "HTML"], ["bb", "BBCode"]]) {
    const b = h("button", "btn-mini", label);
    b.onclick = () => copyText(linkText(k, rec));
    links.append(b);
  }
  side.append(links);
  if (it.url) {
    // 公开链接直接交给 WebView 取图：省掉“30MB 原图 → base64 过 IPC → 再解码”
    // 的双份内存。私有仓库/需要鉴权的会 401，onerror 再回退到后端取字节。
    if (/^https?:/i.test(it.url)) {
      im.onerror = () => {
        im.onerror = null;
        inv("ib_image", { url: it.url }).then((r) => { if (r && r.ok) im.src = r.dataUrl; });
      };
      im.src = it.url;
    } else {
      inv("ib_image", { url: it.url }).then((r) => { if (r && r.ok) im.src = r.dataUrl; });
    }
  }
  if (kind === "local") {
    const use = h("button", "btn", "设为上传路径");
    use.style.marginTop = "12px";
    use.onclick = () => {
      F.dir = String(it.path || "").split("/").slice(0, -1).join("/");
      inv("imgbed_provider_patch", { provider: S.provider, patch: { path: F.dir } }).then((s) => {
        S = s; F.dir = s.path; toast("已设为上传路径"); MODAL.hidden = true; renderTab();
      });
    };
    const del = h("button", "btn", "删除记录");
    del.style.marginTop = "8px";
    del.onclick = () => {
      inv("ib_delete", { id: it.id }).then((r) => {
        if (r && r.ok) { toast("已删除本地记录"); MODAL.hidden = true; loadGrid(); }
      });
    };
    side.append(use, del);
  } else if (providerMeta().caps && providerMeta().caps.delete) {
    const del = h("button", "btn", "删除远端文件");
    del.style.marginTop = "12px";
    del.style.color = "var(--danger)";
    del.onclick = () => {
      if (!confirm("删除远端文件不可恢复，确定？")) return;
      inv("ib_delete_remote", { path: it.path }).then((r) => {
        if (r && r.ok) { toast("已删除"); MODAL.hidden = true; loadGrid(); }
        else toast(r && r.error ? r.error : "删除失败");
      });
    };
    side.append(del);
  }
  body.append(pic, side);
  box.append(top, body);
  MODAL.append(box);
}

/* ---------- 设置页 ---------- */

function row(label, hint, ctl, extra) {
  const r = h("div", "row");
  const t = h("div", "text");
  t.append(h("div", "label", label));
  if (hint) t.append(h("div", "hint", hint));
  const c = h("div", "ctl");
  if (ctl) c.append(ctl);
  if (extra) c.append(extra);
  r.append(t, c);
  return r;
}
function textInput(value, placeholder, onCommit) {
  const i = h("input"); i.type = "text"; i.value = value || ""; i.placeholder = placeholder || "";
  i.onchange = () => onCommit(i.value.trim());
  return i;
}
function passInput(value, placeholder, onCommit) {
  const i = h("input"); i.type = "password"; i.value = value || ""; i.placeholder = placeholder || "";
  i.onchange = () => onCommit(i.value.trim());
  return i;
}
function switchCtl(on, cb) {
  const el = h("div", "switch" + (on ? " on" : "")); el.onclick = () => cb(!on); return el;
}
function segCtl(options, cur, cb) {
  const box = h("div", "seg");
  for (const [v, t] of options) {
    const b = h("button", v === cur ? "on" : "", t);
    b.onclick = () => { if (v !== cur) cb(v); };
    box.append(b);
  }
  return box;
}
function selectCtl(options, cur, cb) {
  const s = h("select");
  for (const [v, t] of options) {
    const o = h("option", "", t); o.value = v; if (String(v) === String(cur)) o.selected = true; s.append(o);
  }
  s.onchange = () => cb(s.value);
  return s;
}

/// 数字输入：越界就地夹紧并把输入框改回真实值，避免"看着 99999 实际 8192"。
function numCtl(value, min, max, onCommit, width) {
  const i = h("input");
  i.type = "number";
  i.min = String(min);
  i.max = String(max);
  i.value = String(value);
  i.style.width = width || "110px";
  i.onchange = () => {
    let v = parseInt(i.value, 10);
    if (!Number.isFinite(v)) v = value;
    v = Math.max(min, Math.min(max, v));
    i.value = String(v);
    onCommit(v);
  };
  return i;
}

function fieldControl(meta, f, id) {
  let val = providerConfig(id)[f.key];
  if (val == null && f.key === "path") val = meta.defaultPath || "";
  if (f.secret) {
    const has = (S.secrets || []).includes(`${id}.${f.key}`);
    const wrap = h("div", "grow-ctl");
    const input = passInput("", has ? "已保存（重新填则覆盖）" : "未设置");
    const save = h("button", "btn-mini", "保存");
    save.onclick = () => {
      if (!input.value.trim()) { toast("先粘贴内容"); return; }
      inv("imgbed_secret_set", { key: `${id}.${f.key}`, value: input.value.trim() }).then((r) => {
        if (r.ok) { S = r.state; renderTab(); toast("已加密保存"); }
        else toast(r.error || "保存失败");
      });
    };
    const clr = h("button", "btn-mini", "清除");
    clr.onclick = () => {
      inv("imgbed_secret_clear", { key: `${id}.${f.key}` }).then((r) => {
        if (r.ok) { S = r.state; renderTab(); toast("已清除"); }
        else toast(r.error || "清除失败");
      });
    };
    wrap.append(input, save, clr);
    return row(f.label, f.hint, wrap);
  }
  if (f.kind === "bool") {
    return row(f.label, f.hint, switchCtl(val === true, (v) => patchProvider(id, { [f.key]: v })));
  }
  if (f.kind === "select") {
    const opts = (f.options || []).map((o) => [o.value, o.label]);
    return row(f.label, f.hint, selectCtl(opts, val || (opts[0] && opts[0][0]) || "", (v) => patchProvider(id, { [f.key]: v })));
  }
  if (f.kind === "kv") {
    const wrap = h("div", "kvbox");
    const ta = h("textarea");
    const obj = val && typeof val === "object" ? val : {};
    ta.value = Object.entries(obj).map(([k, v]) => `${k}=${v}`).join("\n");
    ta.placeholder = "key=value，每行一条";
    ta.onchange = () => {
      const out = {};
      for (const line of ta.value.split("\n")) {
        const i = line.indexOf("=");
        if (i > 0) out[line.slice(0, i).trim()] = line.slice(i + 1).trim();
      }
      patchProvider(id, { [f.key]: out });
    };
    wrap.append(ta);
    return row(f.label, f.hint, wrap);
  }
  return row(f.label, f.hint, textInput(val || "", f.hint, (v) => patchProvider(id, { [f.key]: v })));
}

function patchProvider(id, patch) {
  inv("imgbed_provider_patch", { provider: id, patch }).then((s) => {
    S = s;
    F.dir = s.path;
    renderTab();
  });
}

function renderSettings() {
  BODY.append(h("div", "note", "平台之间互不影响：凭据、路径、链接规则各存一份。切平台只换当前使用的那一套。"));

  const cards = h("div", "providers");
  for (const m of S.metas || []) {
    const b = h("button", "pcard" + (m.id === S.provider ? " on" : ""));
    b.append(h("div", "nm", m.name));
    const caps = [];
    if (m.caps.listDirs) caps.push("目录");
    if (m.caps.listImages) caps.push("列图");
    if (m.caps.delete) caps.push("删除");
    if (!caps.length) caps.push("仅上传");
    b.append(h("div", "cap", caps.join(" · ") + ` · ≤${m.caps.maxMb}MB`));
    b.onclick = () => {
      inv("imgbed_patch", { patch: { provider: m.id } }).then((s) => {
        S = s;
        F.dir = s.path;
        renderTab();
      });
    };
    cards.append(b);
  }
  BODY.append(cards);

  const meta = providerMeta();
  BODY.append(h("h2", "", `${meta.name} · 连接`));
  const c1 = h("div", "card");
  for (const f of meta.fields) c1.append(fieldControl(meta, f, meta.id));
  const testWrap = h("div", "grow-ctl");
  const out = h("div", "note", "");
  const test = h("button", "btn primary", "测试连接");
  test.onclick = () => {
    out.textContent = "测试中…";
    inv("imgbed_test").then((r) => {
      out.textContent = r.ok ? r.lines.join("\n") : r.error;
    });
  };
  const upTest = h("button", "btn", "上传测试图");
  upTest.onclick = () => {
    out.textContent = "上传中…";
    inv("imgbed_upload_test").then((r) => {
      out.textContent = r.ok ? "已上传：" + r.url : r.error;
    });
  };
  testWrap.append(test, upTest);
  c1.append(row("验证", "先测连接，再传一张 64×64 测试图确认真的能写", testWrap));
  c1.append(out);
  BODY.append(c1);

  const up = S.upload || {};
  const cfg = providerConfig(meta.id);
  BODY.append(h("h2", "", "上传默认值"));
  const c2 = h("div", "card");
  c2.append(row("默认路径", "支持 {yyyy} {MM} {dd} {HH} {mm}；上传页可临时改",
    textInput(cfg.path != null ? cfg.path : S.path, "images/{yyyy}/{MM}", (v) => patchProvider(meta.id, { path: v }))));
  c2.append(row("命名", "时间戳 + 4 位内容 hash，或原名 + hash；永不覆盖",
    segCtl([["timestamp", "时间戳"], ["original", "原文件名"]], up.naming, (v) => setParam({ upload: { naming: v } }))));
  c2.append(row("文件名前缀", "例如 blog-；留空不加", textInput(up.prefix, "（留空）", (v) => setParam({ upload: { prefix: v } }))));
  c2.append(row("格式", "转 WebP 会先解码再按长边缩放；动图与 SVG 保留原格式",
    segCtl([["webp", "WebP"], ["keep", "原样"]], up.convert, (v) => setParam({ upload: { convert: v } }))));
  c2.append(row("WebP 质量", "1–100；文字截图建议 90+",
    (() => {
      const i = h("input"); i.type = "range"; i.min = "1"; i.max = "100"; i.value = up.quality; i.style.width = "180px";
      const lab = h("span", "", String(up.quality)); lab.style.color = "var(--text-2)";
      i.oninput = () => lab.textContent = i.value;
      i.onchange = () => setParam({ upload: { quality: parseInt(i.value, 10) } });
      const w = h("div", "ctl"); w.append(i, lab); return w;
    })()));
  c2.append(row("最大长边", "0 = 不限制；填具体值时只在超出后等比缩小，不会放大",
    numCtl(up.maxSide, 0, 8192, (v) => setParam({ upload: { maxSide: v } })), h("div", "hint", up.maxSide ? `长边限制：${up.maxSide}px` : "长边限制：不限制")));
  c2.append(row("单文件上限", "MB，1–100；本地先拦",
    numCtl(up.maxMb, 1, 100, (v) => setParam({ upload: { maxMb: v } }))));
  BODY.append(c2);

  BODY.append(h("h2", "", "链接与复制"));
  const c3 = h("div", "card");
  c3.append(row("复制格式", "上传成功后写进剪贴板的内容",
    segCtl([["markdown", "Markdown"], ["html", "HTML"], ["bbcode", "BBCode"], ["url", "裸链接"]], S.copy.format,
      (v) => setParam({ copy: { format: v } }))));
  c3.append(row("自动复制", "关掉就只在结果里显示", switchCtl(S.copy.auto, (v) => setParam({ copy: { auto: v } }))));
  BODY.append(c3);

  BODY.append(h("h2", "", "入口与网络"));
  const c4 = h("div", "card");
  c4.append(row("启用图床", "关掉后截图不显示上传按钮、热键不注册", switchCtl(S.enabled, (v) => setParam({ enabled: v }))));
  c4.append(row("上传剪贴板热键", "留空 = 不注册；被占用时自动降级",
    textInput(S.hotkey, "Ctrl+Shift+U", (v) => setParam({ hotkey: v })),
    h("div", "hint", S.hotkeyError ? `${S.hotkeyWanted}：${S.hotkeyError}（实际 ${S.hotkeyActive || "无"}）` : (S.hotkeyActive ? "已生效：" + S.hotkeyActive : "未注册"))));
  c4.append(row("HTTP 代理", "例如 http://127.0.0.1:7890；留空跟随系统；不支持 SOCKS5",
    textInput(S.proxy, "（留空）", (v) => setParam({ proxy: v }))));
  BODY.append(c4);

  BODY.append(h("h2", "", "数据"));
  const c5 = h("div", "card");
  const dirBtn = h("button", "btn", "打开目录");
  dirBtn.onclick = () => inv("ib_open_dir");
  c5.append(row("数据目录", "history.json / secrets.dat / thumbs 都在这里", dirBtn));
  const clr = h("button", "btn", "清空记录");
  clr.onclick = () => {
    if (!confirm("只清本地记录，不删远端文件。确定？")) return;
    inv("ib_clear_history").then(() => { toast("已清空"); S = null; load().then(renderTab); });
  };
  c5.append(row("本地上传记录", `${S.history} 条`, clr));
  BODY.append(c5);

  BODY.append(h("h2", "", "说明"));
  const c6 = h("div", "card");
  c6.append(h("div", "hint", "· 密钥只存在 secrets.dat（DPAPI 加密），settings.yaml 里没有；界面永不回显原文。"));
  c6.append(h("div", "hint", "· 上传永远走平台自己的接口；CDN 只影响读取地址（GitHub 的私有仓库 jsDelivr 拿不到）。"));
  c6.append(h("div", "hint", "· 「转 WebP」就是转：不再比较体积、也不回退原格式，结果可预测。"));
  c6.append(h("div", "hint", "· 路径按 / 分层，拒绝 ..、反斜杠与控制字符；最多 8 层。"));
  c6.append(h("div", "hint", "· 自定义 HTTP 没有目录/列图/删除能力，界面上对应的按钮不会出现。"));
  BODY.append(c6);
}

/* ---------- 共用 DOM ---------- */

function ensureChrome() {
  if (!MODAL) {
    MODAL = h("div", "modal");
    MODAL.hidden = true;
    MODAL.onclick = (e) => { if (e.target === MODAL) MODAL.hidden = true; };
    document.body.append(MODAL);
  }
  if (!dropHooked) {
    dropHooked = true;
    hookDrop();
  }
}

/// 拖到窗口里：只有站在「上传」tab 且图床页还在时才收，免得在别的设置页误吞文件。
function hookDrop() {
  try {
    const wv = window.__TAURI__.webview.getCurrentWebview();
    wv.onDragDropEvent((e) => {
      if (TAB !== "upload" || !HOST || !HOST.isConnected) return;
      const p = e.payload;
      if (p && p.type === "drop" && p.paths && p.paths.length) addPaths(p.paths);
    });
  } catch (_) {}
}

// app.js 调这个：给它一个容器，剩下的交给我
window.ImgBed = {
  mount,
  setTab(tab) {
    if (!TABS.some(([id]) => id === tab)) return;
    TAB = tab;
    if (BODY) renderTab();
  },
  get tab() { return TAB; },
};
})();
