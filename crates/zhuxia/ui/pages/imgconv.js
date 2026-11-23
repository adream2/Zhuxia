/* ---------- 图片转换（自 app.js 拆出） ---------- */

/* 操作页的临时状态：不进 settings.yaml（选中的文件和一屏结果没有持久化的意义）。
   放在模块外面，切页/重渲染都不会丢。 */
const ICONV = { paths: [], target: "png", items: null, busy: false, error: "" };

const IMGCONV_TARGETS = [
  ["png", "PNG"], ["jpg", "JPG"], ["webp", "WebP"], ["gif", "GIF"],
  ["bmp", "BMP"], ["tiff", "TIFF"], ["ico", "ICO"],
];

function fmtBytes(n) {
  if (!Number.isFinite(n)) return "-";
  if (n < 1024) return n + " B";
  if (n < 1048576) return (n / 1024).toFixed(1) + " KB";
  return (n / 1048576).toFixed(2) + " MB";
}

/// 文件名（取路径最后一段，找不到就原样）
function baseName(p) {
  const s = String(p || "");
  const i = Math.max(s.lastIndexOf("\\"), s.lastIndexOf("/"));
  return i >= 0 ? s.slice(i + 1) : s;
}

/// 选中文件的标签墙；点标签从列表里去掉
function fileChips(state, onChange) {
  const box = h("div", "chips files");
  if (!state.paths.length) {
    box.append(h("span", "hint", "（还没选文件，点「选择文件」）"));
    return box;
  }
  state.paths.forEach((p) => {
    const c = h("button", "chip on", baseName(p) + " ×");
    c.title = p;
    c.onclick = () => onChange(state.paths.filter((x) => x !== p));
    box.append(c);
  });
  return box;
}

/// 结果列表：成功一行「名字 → 输出名 · 体积变化 · 耗时」，失败一行原因
function resultList(items) {
  const box = h("div", "conv-results");
  items.forEach((it) => {
    const line = h("div", "conv-item" + (it.ok ? "" : " bad"));
    const name = h("div", "conv-name", baseName(it.source));
    name.title = it.source;
    line.append(name);
    if (it.ok) {
      const detail = it.outputs
        ? it.outputs.length + " 个文件 · " + it.detail
        : baseName(it.output) + " · " + it.width + "×" + it.height +
          (it.srcBytes && it.outBytes
            ? " · " + fmtBytes(it.srcBytes) + " → " + fmtBytes(it.outBytes)
            : "") + " · " + it.ms + " ms";
      line.append(h("div", "conv-detail", detail));
      if (it.notice) line.append(h("div", "conv-note", it.notice));
    } else {
      line.append(h("div", "conv-detail", it.error + (it.detail ? "：" + it.detail : "")));
    }
    box.append(line);
  });
  return box;
}

function convSummary(items) {
  const ok = items.filter((x) => x.ok).length;
  const bad = items.length - ok;
  const s = h("div", "hint");
  s.textContent = "成功 " + ok + " 个" + (bad ? " · 失败 " + bad + " 个" : "");
  s.style.margin = "8px 0";
  return s;
}

async function imgconvPick() {
  try {
    const r = await inv("imgconv_pick");
    if (r && r.paths && r.paths.length) {
      // 去重但不打乱顺序
      const seen = new Set(ICONV.paths);
      r.paths.forEach((p) => { if (!seen.has(p)) { seen.add(p); ICONV.paths.push(p); } });
      render();
    }
  } catch (e) { toast("选文件失败：" + e); }
}

async function imgconvStart() {
  if (!ICONV.paths.length || ICONV.busy) return;
  ICONV.busy = true;
  ICONV.items = null;
  ICONV.error = "";
  render();
  try {
    const r = await inv("imgconv_run", { paths: ICONV.paths, target: ICONV.target });
    ICONV.items = r.items || [];
    if (r.error) ICONV.error = r.error;
  } catch (e) {
    ICONV.error = String(e);
  }
  ICONV.busy = false;
  render();
}

function pageImgconv() {
  const v = S.imgconv || {};
  const frag = document.createDocumentFragment();
  frag.append(h("h1", "", "图片转换"));
  frag.append(h("p", "sub",
    "选文件 → 选目标格式 → 转换，就地生成新文件；右键菜单用的是同一套设置。全部本地完成，不上传。"));

  /* ---- 操作页 ---- */
  const c0 = h("div", "card");
  const pick = h("button", "btn primary", "选择文件");
  pick.onclick = imgconvPick;
  const clearOne = h("button", "btn", "清空");
  clearOne.onclick = () => { ICONV.paths = []; ICONV.items = null; render(); };
  const ctl = h("div", "ctl");
  ctl.append(pick, clearOne);
  c0.append(row("文件", "可多选；也可以再点一次「选择文件」继续加", ctl));

  const chipsBox = h("div", "row");
  const chipsText = h("div", "text");
  chipsText.append(h("div", "label", "已选 " + ICONV.paths.length + " 个"));
  chipsText.append(fileChips(ICONV, (next) => { ICONV.paths = next; render(); }));
  chipsBox.append(chipsText);
  c0.append(chipsBox);

  const run = h("button", "btn primary", ICONV.busy ? "转换中…" : "开始转换");
  run.disabled = ICONV.busy || !ICONV.paths.length;
  run.onclick = imgconvStart;
  const runCtl = h("div", "ctl");
  runCtl.append(seg(IMGCONV_TARGETS, ICONV.target, (x) => { ICONV.target = x; render(); }), run);
  c0.append(row("目标格式", "每个文件转成这个格式；已经是该格式的会跳过并说明", runCtl));

  if (ICONV.error) c0.append(row("出错", ICONV.error, null, false, h("span", "hint", "看 logs/ 里的记录")));
  if (ICONV.items && ICONV.items.length) {
    const box = h("div", "row");
    const t = h("div", "text");
    t.append(h("div", "label", "结果"));
    t.append(convSummary(ICONV.items), resultList(ICONV.items));
    const firstOk = ICONV.items.find((x) => x.ok);
    const openDir = firstOk && firstOk.outputs && firstOk.outputs.length ? firstOk.outputs[0] : firstOk && firstOk.output;
    if (openDir) {
      const ob = h("button", "btn", "打开输出目录");
      ob.onclick = () => {
        const s = String(openDir);
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
      ? "已注册：右键图片 →「图片格式转换」（Win11 在「显示更多选项」里）"
      : "未注册：打开下面的开关会自动写入注册表",
    sw(v.enabled, (x) => ipatch({ enabled: x }))));
  const btn = h("button", "btn", "重新注册");
  btn.onclick = async () => {
    try {
      S.imgconv = await inv("imgconv_menu", { on: true });
      render();
      toast("菜单已刷新");
    } catch (e) { toast("刷新失败：" + e); }
  };
  c1.append(row("刷新菜单", "换过 exe 位置、或菜单没出现时点这里重写注册表", btn));
  c1.append(row("可转换格式", "每个格式一个子菜单项", null, false,
    h("span", "hint", (v.targets || []).join(" · "))));
  frag.append(c1);

  /* ---- 输出 ---- */
  frag.append(h("h2", "", "输出"));
  const c2 = h("div", "card");
  c2.append(row("质量", "1–100，只影响 JPG 与有损 WebP；PNG/GIF/BMP/TIFF 无损不受影响",
    textInput(String(v.quality == null ? 90 : v.quality), "90", (x) => {
      const n = Math.max(1, Math.min(100, parseInt(x, 10) || 90));
      ipatch({ quality: n });
    })));
  c2.append(row("WebP 无损", "截图、图标、带文字的图值得开；照片体积会大很多",
    sw(v.webpLossless, (x) => ipatch({ webpLossless: x }))));
  c2.append(row("覆盖同名文件", "关闭时自动改叫「名字 (2).扩展名」，绝不覆盖原文件",
    sw(v.overwrite, (x) => ipatch({ overwrite: x }))));
  c2.append(row("输出目录", "留空 = 与源文件同目录；填绝对路径则统一输出到那里",
    textInput(v.outDir || "", "默认：源文件所在目录", (x) => ipatch({ outDir: x }))));
  c2.append(row("ICO 规格", "输出 .ico 时同时塞这几个尺寸（至少一个）；16/32/48 是桌面和资源管理器最常用的",
    chips(v.icoOptions || [16, 24, 32, 48, 64, 128, 256], v.icoSizes || [256, 48, 32, 16],
      (list) => ipatch({ icoSizes: list }))));
  frag.append(c2);

  frag.append(h("h2", "", "说明"));
  const c3 = h("div", "card");
  c3.append(h("div", "hint", "· 可读：PNG / JPG / GIF / BMP / TIFF / ICO / WebP / SVG（矢量）；HEIC、AVIF、PSD 等会提示「不支持读取」。"));
  c3.append(h("div", "hint", "· SVG 按自身尺寸渲染，长边不足 256px 时放大到 256px；输出 ICO 时按最大规格渲染。"));
  c3.append(h("div", "hint", "· 手机照片自动按 EXIF 方向转正；转 JPG/GIF/BMP 时透明区域压到白色背景。"));
  c3.append(h("div", "hint", "· 一次选很多张也没关系：串行转换，失败只影响那一张。"));
  frag.append(c3);
  return frag;
}
