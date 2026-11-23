/* ---------- 图片转换（自 app.js 拆出） ---------- */

function pageImgconv() {
  const v = S.imgconv || {};
  const frag = document.createDocumentFragment();
  frag.append(h("h1", "", "图片转换"));
  frag.append(h("p", "sub",
    "在资源管理器里右键任意图片 →「图片格式转换」→ 选目标格式，就在原目录生成新文件。全部本地完成，不上传。"));

  const c0 = h("div", "card");
  c0.append(row("资源管理器右键菜单",
    v.installed ? "已注册：右键图片即可看到「图片格式转换」" : "未注册（Win11 在「显示更多选项」里）",
    sw(v.enabled, (x) => ipatch({ enabled: x }))));
  const btn = h("button", "btn", "重新注册");
  btn.onclick = async () => {
    try {
      S.imgconv = await inv("imgconv_menu", { on: true });
      render();
      toast("菜单已刷新");
    } catch (e) { toast("刷新失败：" + e); }
  };
  c0.append(row("刷新菜单", "改了配置后菜单没变，点这里重写注册表并通知外壳", btn));
  c0.append(row("目标格式", "每个格式一个子菜单项", null, false,
    h("span", "hint", (v.targets || []).join(" · "))));
  frag.append(c0);

  frag.append(h("h2", "", "输出"));
  const c1 = h("div", "card");
  c1.append(row("质量", "1–100，只影响 JPG 与有损 WebP；PNG/GIF/BMP/TIFF 无损不受影响",
    textInput(String(v.quality == null ? 90 : v.quality), "90", (x) => {
      const n = Math.max(1, Math.min(100, parseInt(x, 10) || 90));
      ipatch({ quality: n });
    })));
  c1.append(row("WebP 无损", "截图、图标、带文字的图值得开；照片体积会大很多",
    sw(v.webpLossless, (x) => ipatch({ webpLossless: x }))));
  c1.append(row("覆盖同名文件", "关闭时自动改叫「名字 (2).扩展名」，绝不覆盖原文件",
    sw(v.overwrite, (x) => ipatch({ overwrite: x }))));
  c1.append(row("输出目录", "留空 = 与源文件同目录；填绝对路径则统一输出到那里",
    textInput(v.outDir || "", "默认：源文件所在目录", (x) => ipatch({ outDir: x }))));
  c1.append(row("ICO 规格", "输出 .ico 时同时塞这几个尺寸（至少一个）；16/32/48 是桌面和资源管理器最常用的",
    chips(v.icoOptions || [16, 24, 32, 48, 64, 128, 256], v.icoSizes || [256, 48, 32, 16],
      (list) => ipatch({ icoSizes: list }))));
  frag.append(c1);

  frag.append(h("h2", "", "说明"));
  const c2 = h("div", "card");
  c2.append(h("div", "hint", "· 可读：PNG / JPG / GIF / BMP / TIFF / ICO / WebP / SVG（矢量）；HEIC、AVIF、PSD 等会提示「不支持读取」。"));
  c2.append(h("div", "hint", "· SVG 按自身尺寸渲染，长边不足 256px 时放大到 256px；输出 ICO 时按最大规格渲染。"));
  c2.append(h("div", "hint", "· 手机照片自动按 EXIF 方向转正；转 JPG/GIF/BMP 时透明区域压到白色背景。"));
  c2.append(h("div", "hint", "· 常驻进程在跑时批量转换只弹一张汇总卡片；点上就转，不再确认。"));
  frag.append(c2);
  return frag;
}

