/* ---------- 图标提取（自 app.js 拆出） ---------- */

function pageAppicon() {
  const v = S.appicon || {};
  const frag = document.createDocumentFragment();
  frag.append(h("h1", "", "图标提取"));
  frag.append(h("p", "sub",
    "右键程序、快捷方式或安装包 →「提取应用图标」→ 就地生成 PNG / ICO / 全尺寸图集。全部本地解析，不上传。"));

  const c0 = h("div", "card");
  c0.append(row("资源管理器右键菜单",
    v.installed ? "已注册：右键 exe / apk / dmg 等即可看到「提取应用图标」" : "未注册（Win11 在「显示更多选项」里）",
    sw(v.enabled, (x) => apatch({ enabled: x }))));
  const btn = h("button", "btn", "重新注册");
  btn.onclick = async () => {
    try {
      S.appicon = await inv("appicon_menu", { on: true });
      render();
      toast("菜单已刷新");
    } catch (e) { toast("刷新失败：" + e); }
  };
  c0.append(row("刷新菜单", "改了配置或换了 exe 路径，点这里重写注册表并通知外壳", btn));
  c0.append(row("挂载扩展名", "每个格式一个父菜单，共用同一个三选一子菜单", null, false,
    h("span", "hint", (v.exts || 0) + " 个")));
  frag.append(c0);

  frag.append(h("h2", "", "默认输出"));
  const c1 = h("div", "card");
  const modes = (v.modes || []).map((m) => [m.id, m.label]);
  c1.append(row("形态", "右键菜单里每次都还能另选；这里只决定默认值",
    seg(modes, v.mode || "png", (x) => apatch({ mode: x }))));
  c1.append(row("PNG 最长边", "16–4096，只缩不放；图标一般 512 足够，要原图就填 4096",
    textInput(String(v.maxSize == null ? 512 : v.maxSize), "512", (x) => {
      const n = Math.max(16, Math.min(4096, parseInt(x, 10) || 512));
      apatch({ maxSize: n });
    })));
  c1.append(row("ICO 规格", "输出 .ico 时塞这几档；超过原生尺寸的档位不会放大生成",
    chips(v.icoOptions || [16, 24, 32, 48, 64, 128, 256], v.icoSizes || [256, 128, 64, 48, 32, 16],
      (list) => apatch({ icoSizes: list }))));
  c1.append(row("覆盖同名文件", "关闭时自动改叫「名字 (2).png」，绝不覆盖原文件",
    sw(v.overwrite, (x) => apatch({ overwrite: x }))));
  c1.append(row("输出目录", "留空 = 与源文件同目录；填绝对路径则统一输出到那里",
    textInput(v.outDir || "", "默认：源文件所在目录", (x) => apatch({ outDir: x }))));
  frag.append(c1);

  frag.append(h("h2", "", "支持范围"));
  const c2 = h("div", "card");
  (v.formats || []).forEach((line) => c2.append(h("div", "hint", "· " + line)));
  frag.append(c2);

  frag.append(h("h2", "", "说明"));
  const c3 = h("div", "card");
  c3.append(h("div", "hint", "· 先看文件内容再看扩展名：改名成 .bin 的 exe、扩展名写错的安装包照样能认出来。"));
  c3.append(h("div", "hint", "· 多分辨率：PE / ICO / ICNS / APK / deb 有多少档收多少档，「全部尺寸」会把每一档单独存成 icon-宽x高.png。"));
  c3.append(h("div", "hint", "· 只缩不放：原生 32px 的图标不会生成 256px 的糊图；ICO 里缺的档位从最大帧缩，原生同尺寸帧优先。"));
  c3.append(h("div", "hint", "· 不支持或解不开：明确提示「不支持 / 没有图标 / 已加密 / 需要 APFS」，不会静默失败，也不会拿一张无关的图凑数。"));
  frag.append(c3);
  return frag;
}


// 图床：内容由 imgbed.js 渲染（它有上传/图片/设置三个 tab）。
// 这里只给一个容器：切走再回来，tab 停在上次那一页。
