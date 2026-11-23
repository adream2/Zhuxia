const inv = (cmd, args) => window.__TAURI__.core.invoke(cmd, args);

let S = null;        // 完整状态
// 支持 index.html?page=appearance 直达某页（--settings appearance 会带上来）
let page = new URLSearchParams(location.search).get("page") || "appearance";
// 流量页：数据来自 runtime.json 的 net 段（常驻进程采样，面板只负责展示与操作）
const HINTS = {
  general: ["主题、启动与更新", "改动立即生效"],
  appearance: ["任务栏与悬浮卡片的显示方式", "改动立即生效"],
  sampling: ["刷新频率、指标开关与数据来源", "关闭的指标不会被采样"],
  traffic: ["按进程的累计流量与联网控制", "每 3 秒刷新"],
  clipboard: ["剪贴板历史：采集 / 去重 / 搜索 / 粘贴回填", "改动立即生效"],
  gesture: ["按住右键划一笔：方向 + 滚轮 → 动作", "规则改动 1 秒内生效"],
  imgconv: ["右键图片就地换格式", "改动下次转换生效"],
  appicon: ["从安装包和程序里取图标", "PNG / ICO / 全尺寸"],
  imgbed: ["上传 / 图片 / 设置 —— 顶部切换", "GitHub / S3 / WebDAV / 自定义"],
  devtools: ["JSON / 编码 / 时间戳 / 正则 / 哈希 / UUID / 颜色 / JWT", "全部本地计算，不联网、不落盘"],
  record: ["视频与 GIF 录制 · 摄像头合成", "改动立即生效"],
  about: ["", ""],
};

/* ---------- 保存：统一的“命令 + 返回值落点”表 ----------
 * 规则：根 patch 返回完整 state；每个 *_patch 只返回该模块的视图。
 * 这样每改一个开关不需要把 runtime（磁盘/GPU/网络流）整包传回来。
 */
const SCOPES = {
  root:      { cmd: "patch",           apply: (r) => { S = r; } },
  gesture:   { cmd: "gesture_patch",   apply: (r) => { S.gesture = r; } },
  imgconv:   { cmd: "imgconv_patch",   apply: (r) => { S.imgconv = r; } },
  appicon:   { cmd: "appicon_patch",   apply: (r) => { S.appicon = r; } },
  clipboard: { cmd: "clipboard_patch", apply: (r) => { S.clipboard = r; } },
  capture:   { cmd: "capture_patch",   apply: (r) => { S.capture = r; } },
  record:    { cmd: "record_patch",    apply: (r) => { S.capture.record = r; } },
  imgbed:    { cmd: "imgbed_patch",    apply: (r) => { S.imgbed = r; } },
};

async function save(scope, obj) {
  const sc = SCOPES[scope];
  try {
    const r = await inv(sc.cmd, { patch: obj });
    sc.apply(r);
    applyTheme();
    render();
    toast("已保存");
    return true;
  } catch (e) {
    toast("保存失败：" + e);
    return false;
  }
}

// 根配置：仪表盘外观这类跨模块字段（返回全量 state）
async function patch(obj) {
  try {
    S = await inv("patch", { patch: obj });
    applyTheme();
    render();
  } catch (e) {
    toast("保存失败：" + e);
  }
}

// 手势模块的配置单独一段（gesture_patch 只合并 gesture 段，保持 settings.yaml 稀疏）
const gpatch = (obj) => save("gesture", obj);

// 图片转换的配置单独一段（imgconv 段），保持 settings.yaml 稀疏
const ipatch = (obj) => save("imgconv", obj);

// 图标提取的配置单独一段（appicon 段），同样只写改过的键
const apatch = (obj) => save("appicon", obj);

/* ---------- 渲染 ---------- */
const PAGES = { general: pageGeneral, appearance: pageAppearance, sampling: pageSampling, traffic: pageTraffic, clipboard: pageClipboard, capture: pageCapture, record: pageRecord, gesture: pageGesture, imgconv: pageImgconv, appicon: pageAppicon, imgbed: pageImgbed, devtools: pageDevtools, about: pageAbout };

function render() {
  if (!S) return;
  document.querySelectorAll("#nav button").forEach((b) => {
    b.classList.toggle("active", b.dataset.page === page);
  });
  const main = document.getElementById("page");
  const scroll = main.scrollTop;
  try {
    main.replaceChildren(PAGES[page]());
  } catch (e) {
    // 渲染出错时给出可见信息，总比白屏好排查
    main.replaceChildren(h("div", "card", "页面渲染出错：" + (e && e.message ? e.message : e)));
  }
  main.scrollTop = scroll;
  const foot = document.getElementById("nav-foot");
  foot.innerHTML = "";
  foot.append(h("div", "", `v${S.meta.version}`));
  foot.append(h("div", "", S.runtime.admin ? "管理员" : "普通权限"));
}

function applyTheme() {
  const t = S.view.theme;
  const dark = t === "dark" || (t === "system" && window.matchMedia("(prefers-color-scheme: dark)").matches);
  document.body.dataset.theme = dark ? "dark" : "light";
}

/* ---------- 启动 ---------- */
window.addEventListener("DOMContentLoaded", async () => {
  document.querySelectorAll("#nav button").forEach((b) => {
    b.onclick = async () => {
      page = b.dataset.page;
      // 设备列表可能还没采到（面板刚打开时快照是空的），进采样页前补拉一次
      if (page === "sampling" && (!S.runtime.disks.length || !S.runtime.gpus.length)) {
        S = await inv("get_state");
      }
      if (page === "traffic") { await loadTraffic(); }
      if (page === "clipboard") { await loadClip(); }
      render();
    };
  });
  // 关闭 = 退出进程。以前这里调 hide()，窗口藏起来但 6 个 WebView2 子进程
  // 会一直活着（约 160MB 私有内存），和「关掉面板即归还内存」的设计相反。
  const win = window.__TAURI__.window.getCurrentWindow();
  // 留 200ms 让最后一次 onchange 触发的 save() 落到 settings.yaml 再退；
  // 直接 close 有可能在 invoke 返回前把进程杀掉，表现为“刚改的开关没保存”。
  document.getElementById("btn-close").onclick = () => setTimeout(() => win.close(), 200);
  document.getElementById("btn-min").onclick = () => win.minimize();

  S = await inv("get_state");
  if (!PAGES[page]) page = "appearance";
  if (page === "clipboard") await loadClip();
  applyTheme();
  render();
  // 采样线程第一帧只建基线，设备列表稍后才出现——2 秒后再补一次
  if (!S.runtime.disks.length || !S.runtime.gpus.length) {
    setTimeout(async () => {
      S = await inv("get_state");
      applyTheme();
      render();
    }, 2200);
  }
  // 常驻进程（托盘「图床…」）可能要求跳到某一页：请求写在 run_dir/panel-page.txt，
  // 这里取走并切换。只在可见时轮询，隐藏时不做无用功。
  setInterval(async () => {
    if (document.visibilityState !== "visible") return;
    try {
      const want = await inv("take_page_request");
      if (want && want !== page && PAGES[want]) {
        page = want;
        if (page === "traffic") await loadTraffic();
        if (page === "clipboard") await loadClip();
        render();
      }
    } catch (_) {}
  }, 1200);

  // 最小化/隐藏超过 10 分钟就自己退出：面板只是偶尔改配置，
  // 没必要让 WebView2 在后台常驻一整天。重新打开是冷启动（约 0.6s），值。
  let idleExit = 0;
  const armIdleExit = () => {
    clearTimeout(idleExit);
    idleExit = setTimeout(() => {
      if (document.visibilityState !== "visible") {
        console.info("面板空闲 10 分钟，退出以释放 WebView2");
        win.close();
      }
    }, 10 * 60 * 1000);
  };
  document.addEventListener("visibilitychange", async () => {
    if (document.visibilityState === "visible") {
      clearTimeout(idleExit);
      S = await inv("get_state");
      applyTheme();
      render();
    } else {
      armIdleExit();
    }
  });
});
