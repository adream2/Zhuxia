(() => {
/* ---------- 图床/开发工具挂载页（自 app.js 拆出） ---------- */

function pageImgbed() {
  const frag = document.createDocumentFragment();
  frag.append(h("h1", "", "图床管理"));
  frag.append(h("p", "sub",
    "截图工具条的「上传」与 Ctrl+Shift+U 用的就是这里的平台和路径。"));
  const host = h("div", "ibmount");
  frag.append(host);
  // 等挂到 DOM 上再交给模块：它要往 body 里塞灯箱，并读容器尺寸
  requestAnimationFrame(() => {
    if (host.isConnected && window.ImgBed) window.ImgBed.mount(host);
  });
  return frag;
}

/* 开发工具：内容由 devtools.js 渲染（纯前端计算页，不读写 settings.yaml）。
 * 和 imgbed 一样只给容器：切走再回来，选中的工具与输入内容都还在。 */
function pageDevtools() {
  const frag = document.createDocumentFragment();
  frag.append(h("h1", "", "开发工具"));
  frag.append(h("p", "sub", HINTS.devtools[0]));
  const host = h("div", "dtmount");
  frag.append(host);
  // 等挂到 DOM 上再交给模块：输入框的初始尺寸与工具栏换行要读容器宽度
  requestAnimationFrame(() => {
    if (host.isConnected && window.DevTools) window.DevTools.mount(host);
  });
  return frag;
}

window.pageImgbed = pageImgbed;
window.pageDevtools = pageDevtools;
})();
