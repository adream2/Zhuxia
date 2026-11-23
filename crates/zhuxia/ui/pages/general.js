(() => {
/* ---------- 通用（自 app.js 拆出） ---------- */

function pageGeneral() {
  const v = S.view;
  const frag = document.createDocumentFragment();
  frag.append(h("h1", "", "通用设置"), h("p", "sub", HINTS.general[0]));

  frag.append(h("h2", "", "主题"));
  const c1 = h("div", "card");
  c1.append(row("深浅色", "跟随系统或强制指定；同时作用于任务栏、剪贴板面板与本设置窗口",
    seg([["system", "跟随系统"], ["light", "浅色"], ["dark", "深色"]], v.theme,
      (x) => patch({ theme: x === "system" ? null : x }))));
  frag.append(c1);

  frag.append(h("h2", "", "启动"));
  const c2 = h("div", "card");
  c2.append(row("开机自启", "登录后用计划任务以最高权限启动", sw(v.autoStart, (x) => inv("set_autostart", { on: x }).then((s) => { S = s; render(); }))));
  c2.append(row("启动时检查更新", "从 GitHub Releases 读最新版本号；没有新版本时安静退出，不打扰",
    sw(v.updateCheck, (x) => patch({ updateCheckEnabled: x }))));
  frag.append(c2);

  return frag;
}

/* 任务栏覆盖层要挂到哪些屏幕。配置为空 = 只跟主屏（上游旧配置的语义）。 */

window.pageGeneral = pageGeneral;
})();
