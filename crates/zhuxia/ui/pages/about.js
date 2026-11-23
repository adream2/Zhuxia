/* ---------- 关于（自 app.js 拆出） ---------- */

function pageAbout() {
  const rt = S.runtime;
  const frag = document.createDocumentFragment();
  const hero = h("div", "card hero");
  const img = h("img");
  img.src = "icon.png";
  const box = h("div");
  box.append(h("div", "name", "驻匣 Zhuxia"));
  box.append(h("div", "ver", `v${S.meta.version} · 一个常驻运行小工具的集合体`));
  hero.append(img, box);

  frag.append(h("h1", "", "关于驻匣"), h("p", "sub", "Rust + Win32 原生渲染；设置面板用 Tauri 2 + 系统 WebView2（按需创建）"));
  frag.append(hero);

  const pills = h("div", "pills");
  const pill = (k, val) => {
    const p = h("span", "pill");
    p.append(document.createTextNode(k + " "), h("b", "", val));
    return p;
  };
  pills.append(pill("管理员", rt.admin ? "是" : "否"));
  pills.append(pill("任务栏形态", rt.floating ? "悬浮卡片" : "任务栏条"));
  pills.append(pill("采样间隔", S.view.interval + " ms"));
  pills.append(pill("按进程网速", rt.etw ? "ETW" : rt.srum ? "SRUM" : "不可用"));
  frag.append(pills);

  const c = h("div", "card");
  const rowBtn = (label, hint, btn) => row(label, hint, btn);
  const logs = h("button", "btn", "打开日志目录");
  logs.onclick = () => inv("open_logs");
  c.append(rowBtn("日志", "logs/zhuxia-日期.log，出问题先看这里", logs));
  const repo = h("button", "btn", "打开 GitHub");
  repo.onclick = () => inv("open_url", { url: "https://github.com/adream2/Zhuxia" });
  c.append(rowBtn("项目仓库", "源码、问题与更新都以这个仓库为准", repo));
  const quit = h("button", "btn", "退出驻匣");
  quit.onclick = () => inv("quit_app");
  c.append(rowBtn("退出", "结束常驻进程（右键任务栏条也有）", quit));
  frag.append(c);

  const link = (url) => {
    const b = h("button", "btn", "打开");
    b.onclick = () => inv("open_url", { url });
    return b;
  };

  /* 本项目许可：Apache-2.0（OSI 认可，允许商用，需保留声明） */
  frag.append(h("h2", "", "许可"));
  const lic = h("div", "card");
  lic.append(row("本项目", "Apache License 2.0：可商用、可修改、可分发，需保留版权与 NOTICE",
    link("https://www.apache.org/licenses/LICENSE-2.0")));
  lic.append(row("第三方", "各自原许可（MIT / Apache-2.0 / MS-PL），声明见 THIRD_PARTY_NOTICES.md",
    null, true));
  frag.append(lic);

  /* 第三方说明：只列仍保留声明的组件；已重写的上游不再列名 */
  frag.append(h("h2", "", "开源与致谢"));
  const oss = h("div", "card");
  const projects = [
    ["TaskMonitor", "Apache-2.0", "https://github.com/linesoft2/TaskMonitor"],
    ["sniplite", "MIT", "https://github.com/MrTangLuyao/sniplite"],
    ["Captura", "MIT", "https://github.com/MathewSachin/Captura"],
    ["Filee", "MIT", "https://github.com/KnifeLemon/Filee"],
    ["ScreenToGif", "MS-PL", "https://github.com/NickeManarin/ScreenToGif"],
  ];
  for (const [name, note, url] of projects) {
    oss.append(row(name, note, link(url)));
  }
  frag.append(oss);
  frag.append(h("p", "sub",
    "早期移植的 GPL / 无许可证代码已在本版本重写移除；完整声明见仓库根目录 THIRD_PARTY_NOTICES.md。"));

  frag.append(h("h2", "", "说明"));
  const notes = h("div", "card");
  notes.append(row("采样口径", "与任务管理器同源：GetSystemTimes / NtQuerySystemInformation / IOCTL_DISK_PERFORMANCE / PDH GPU Engine / GetIfTable2 / ETW"));
  notes.append(row("温度", "GPU 温度依赖 DXCore 的适配器温度查询，当前版本显示 --"));
  notes.append(row("权限", "按进程网速与部分磁盘/GPU 计数需要管理员；未提权时对应列表为空并给出提示"));
  frag.append(notes);
  return frag;
}
