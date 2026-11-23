(() => {
/* ---------- 录屏（自 app.js 拆出） ---------- */

/* ---------------- 录屏（GIF + 摄像头合成） ---------------- */

let RDEVS = null; // 摄像头设备名列表（null = 还没拉过）

const rpatch = (obj) => save("record", obj);

// 音频设备（null = 还没拉过）
let ADEVS = null;
let ASPEAKER = "";

function loadAudioDevices(done) {
  inv("record_audio_devices")
    .then((r) => { ADEVS = r.mics || []; ASPEAKER = r.speaker || ""; done(); })
    .catch(() => { ADEVS = []; done(); });
}

function loadRecordDevices(done) {
  inv("record_devices")
    .then((r) => { RDEVS = r.devices || []; done(); })
    .catch(() => { RDEVS = []; done(); });
}

function pageRecord() {
  const c = S.capture || {};
  const v = c.record || {};
  const cam = v.camera || {};
  const lay = v.layout || {};
  const aud = v.audio || {};
  const isVideo = (v.format || "video") === "video";
  if (RDEVS === null) {
    RDEVS = [];
    loadRecordDevices(() => { if (page === "record") render(); });
  }
  if (ADEVS === null) {
    ADEVS = [];
    loadAudioDevices(() => { if (page === "record") render(); });
  }

  const frag = document.createDocumentFragment();
  frag.append(h("h1", "", "录屏工具"));
  frag.append(h("p", "sub",
    "框选一块区域录下来。两种产出：视频（MP4，帧率高、能录长、可带声音）和 GIF（体积小、能直接贴，无声音）。选区复用截图的框选，Ctrl+Shift+R 框完就开录。"));

  const c0 = h("div", "card");
  c0.append(row("启用录屏", "关闭后 Ctrl+Shift+R 与工具条按钮都不工作",
    sw(v.enabled, (x) => rpatch({ enabled: x }))));
  c0.append(row("产出格式",
    isVideo ? "MP4 / H.264，无限时长、能后期剪辑；文件较大" : "GIF，256 色、体积小；适合 3~30 秒的片段",
    seg([["video", "录屏（视频）"], ["gif", "录 GIF"]], v.format || "video",
      (x) => rpatch({ format: x, fps: x === "gif" ? 15 : 30 }))));

  const fpsOpts = isVideo
    ? [[24, "24"], [30, "30"], [60, "60"]]
    : [[10, "10"], [15, "15"], [20, "20"], [25, "25"]];
  c0.append(row("帧率",
    isVideo
      ? "60fps 更流畅但文件翻倍；文档演示 30 就够"
      : "GIF 的帧延迟只有 1/100 秒精度，10/15/20/25 是可整除的值",
    seg(fpsOpts, v.fps, (x) => rpatch({ fps: x }))));

  if (isVideo) {
    c0.append(row("画质",
      "约 " + (v.mbps1080 || 8) + " Mbps（按 1080p 估算）。码率越高文字越锐、文件越大",
      seg([["lite", "省空间"], ["standard", "标准"], ["high", "高画质"]], v.quality || "standard",
        (x) => rpatch({ quality: x }))));
  }

  const widthOpts = isVideo
    ? [[0, "原始"], [2560, "1440"], [1920, "1080"], [1280, "720"]]
    : [[0, "原始"], [960, "960"], [720, "720"], [480, "480"]];
  c0.append(row("输出宽度",
    isVideo ? "原始尺寸最清晰；降到 1080 能显著减小体积" : "GIF 体积的第一杠杆，960 约为原尺寸一半",
    seg(widthOpts, v.maxWidth, (x) => rpatch({ maxWidth: x }))));

  c0.append(row("最长录制",
    (v.maxSeconds || 0) > 0
      ? "到点会自动保存并结束（秒）。填 0 表示不限制"
      : "不限制，录到你点停止为止。磁盘剩不到 2GB 时控制条计时会变橙，剩 512MB 自动保存收尾",
    numInput(v.maxSeconds || 0, 0, 86400, (x) => rpatch({ maxSeconds: x }))));
  c0.append(row("开录倒计时", "点完到真正开始之间的准备时间（秒）",
    numInput(v.countdown, 0, 10, (x) => rpatch({ countdown: x }))));
  c0.append(row("把鼠标画进帧里", "抓屏本身不含光标，补上才看得懂操作",
    sw(v.cursor, (x) => rpatch({ cursor: x }))));
  frag.append(c0);

  frag.append(h("h2", "", "声音"));
  const ca = h("div", "card");
  if (!isVideo) {
    ca.append(row("GIF 不能带声音", "GIF 格式本身没有音轨。要声音请把上面的产出格式改成「录屏（视频）」", null, true));
  }
  ca.append(row("录制系统声音",
    ASPEAKER ? "来自默认播放设备「" + ASPEAKER + "」——别人听得到的声音都会录进去" : "来自系统默认播放设备",
    sw(aud.system, (x) => rpatch({ audio: { system: x } })), !isVideo));
  ca.append(row("系统声音音量",
    "100% = 原始音量。系统里把音量调小会更小声，这里只是补偿",
    sliderR(0, 200, aud.systemGain == null ? 100 : aud.systemGain, "%",
      (x) => rpatch({ audio: { systemGain: x } })), !isVideo || !aud.system));

  ca.append(row("录制麦克风",
    "默认关闭：录屏多半是演示操作，不该把你的说话录进去",
    sw(aud.mic, (x) => rpatch({ audio: { mic: x } })), !isVideo));
  const micOpts = [["", "系统默认"]].concat((ADEVS || []).map((d) => [d.id, d.name]));
  ca.append(row("麦克风设备",
    ADEVS && ADEVS.length
      ? "选「系统默认」跟随 Windows 设置；插拔后重开本页刷新"
      : "没有枚举到麦克风（插上后重开本页）",
    sel(micOpts, aud.micDevice || "", (x) => rpatch({ audio: { micDevice: x } })),
    !isVideo || !aud.mic || !(ADEVS && ADEVS.length)));
  ca.append(row("麦克风音量",
    "录制中控制条上有静音按钮，按下立即生效",
    sliderR(0, 200, aud.micGain == null ? 100 : aud.micGain, "%",
      (x) => rpatch({ audio: { micGain: x } })), !isVideo || !aud.mic));
  frag.append(ca);

  frag.append(h("h2", "", "摄像头（可选）"));
  const c1 = h("div", "card");
  const devs = [["", "自动（第一个设备）"]].concat((RDEVS || []).map((n) => [n, n]));
  c1.append(row("叠加摄像头", "默认关闭。开了会多一路采集，画面里出现你的头像",
    sw(cam.enabled, (x) => rpatch({ camera: { enabled: x } }))));
  c1.append(row("设备",
    RDEVS && RDEVS.length
      ? "选「自动」时用枚举到的第一个；插拔后重开本页刷新"
      : "没有枚举到设备（插上摄像头后重开本页）",
    sel(devs, cam.device || "", (x) => rpatch({ camera: { device: x } })), !(RDEVS && RDEVS.length)));
  c1.append(row("分辨率", "相机会就近取一个支持的模式，实际值写进日志",
    seg([["320x240", "320×240"], ["640x480", "640×480"], ["1280x720", "1280×720"]],
      `${cam.width}x${cam.height}`, (x) => {
        const p = x.split("x").map(Number);
        rpatch({ camera: { width: p[0], height: p[1] } });
      }), !cam.enabled));
  frag.append(c1);

  frag.append(h("h2", "", "布局"));
  const c2 = h("div", "card");
  const isPip = lay.mode === "pip";
  c2.append(row("怎么摆", "画中画最适合录教程；并排适合跨屏录制",
    seg([["screen", "只录屏幕"], ["pip", "画中画"], ["side-by-side", "并排"], ["camera", "只录摄像头"]],
      lay.mode, (x) => rpatch({ layout: { mode: x } }))));
  c2.append(row("画中画位置", "贴哪个角",
    seg([["top-left", "左上"], ["top-right", "右上"], ["bottom-left", "左下"], ["bottom-right", "右下"]],
      lay.corner, (x) => rpatch({ layout: { corner: x } })), !isPip));
  c2.append(row("画中画大小", "占输出短边的 " + (lay.size || 24) + "%",
    slider(lay.size, (x) => rpatch({ layout: { size: x } })), !isPip));
  c2.append(row("画中画形状", "圆形更像人像，圆角更省像素",
    seg([["rounded", "圆角"], ["circle", "圆形"]], lay.shape, (x) => rpatch({ layout: { shape: x } })), !isPip));
  c2.append(row("边缘留白", "画中画到画面边缘的距离（像素）",
    numInput(lay.margin, 0, 200, (x) => rpatch({ layout: { margin: x } })), !isPip));
  c2.append(row("水平镜像", "像照镜子一样左右翻转",
    sw(lay.mirror, (x) => rpatch({ layout: { mirror: x } }))));
  c2.append(row("描边 / 阴影", "半透明白边 + 柔和投影，画中画不糊在内容上",
    sw(lay.border, (x) => rpatch({ layout: { border: x } })), !isPip));
  frag.append(c2);

  frag.append(h("h2", "", "输出"));
  const c3 = h("div", "card");
  const openBtn = h("button", "btn", "打开目录");
  openBtn.onclick = () => inv("record_open_dir");
  c3.append(row("保存位置", "和截图同一个目录：" + (c.saveDir || "图片\\Zhuxia"), null, false, openBtn));
  c3.append(row("开始录制",
    "Ctrl+Shift+R 框选即录；录制中再按一次停止；控制条上可暂停/丢弃",
    h("span", "val", isVideo ? "MP4" : "GIF")));
  frag.append(c3);

  return frag;
}

window.pageRecord = pageRecord;
})();
