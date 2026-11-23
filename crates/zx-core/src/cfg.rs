//! settings.yaml —— 全部设置在 exe 目录，一次原子写。
//!
//! 键名统一 camelCase，并保持历史版本用过的拼写不变——这两点不能为了"更好听"而改：
//! 用户升级后要能直接读回旧配置，改键名等于把设置清零。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// 发布与更新的**唯一**来源。只在 GitHub 分发，所以更新检查写死在这里，
/// 不给用户填更新源——填错一个字就永远收不到更新，还多一个需要解释的输入框。
pub const UPDATE_REPO: &str = "adream2/Zhuxia";
/// 查最新版本号（`{}/releases/latest` 返回 JSON）。
pub const UPDATE_API: &str = "https://api.github.com/repos/adream2/Zhuxia";
/// 「立即更新」打开的页面。
pub const UPDATE_PAGE: &str = "https://github.com/adream2/Zhuxia/releases/latest";

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    Light,
    Dark,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum MetricDisplayMode {
    #[default]
    Average,
    Max,
    Specific,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum GpuTaskbarContent {
    #[default]
    Usage,
    Temperature,
    Both,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum GpuTempUnit {
    #[default]
    Celsius,
    Fahrenheit,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DisplaySel {
    pub id: String,
    pub name: String,
}

/// 某个来源程序（按进程名/路径子串匹配）的采集策略。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct ClipAppPolicy {
    /// 进程名或路径的子串（不区分大小写），例如 `keepass`。
    pub app: String,
    /// `ignore` = 完全不记录；`cleanup` = 只对该程序套用 `rules`。
    pub action: String,
    /// 仅 `cleanup` 用：`pattern => 替换` 逐行，`# action: ignore` 表示命中即不记录。
    pub rules: String,
}

/// 剪贴板模块的配置。**只有非默认值才写盘**，含义与整个 `Settings` 一致。
/// 字段名沿用 `app.` 前缀去掉后的 camelCase 形式，保证旧配置能直接读回；
/// 对照表见 `docs/CLIPBOARD.md`。
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct ClipboardCfg {
    pub enabled: Option<bool>,
    pub capture_files: Option<bool>,
    pub capture_rich_text: Option<bool>,
    pub record_images: Option<bool>,
    pub dedupe: Option<bool>,
    pub persistent: Option<bool>,
    pub limit_enabled: Option<bool>,
    pub limit: Option<u32>,
    pub delete_after_paste: Option<bool>,
    pub move_to_top_after_paste: Option<bool>,
    pub sound: Option<bool>,
    pub paste_sound: Option<bool>,
    pub sound_volume: Option<f64>,
    pub hotkey: Option<String>,
    pub plain_paste_hotkey: Option<String>,
    pub sequential_hotkey: Option<String>,
    pub search_hotkey: Option<String>,
    pub sequential_mode: Option<bool>,
    pub paste_method: Option<String>,
    pub privacy: Option<bool>,
    pub privacy_kinds: Option<Vec<String>>,
    pub privacy_custom: Option<Vec<String>>,
    pub cleanup_rules: Option<String>,
    pub app_policies: Option<Vec<ClipAppPolicy>>,
    pub sort: Option<String>,
    pub quick_paste_modifier: Option<String>,
    pub pause: Option<bool>,
    /// 用低级键盘钩子接管 Win+V，替换系统剪贴板浮窗。
    pub takeover_win_v: Option<bool>,
    pub window_pinned: Option<bool>,
    pub window_width: Option<i32>,
    pub window_height: Option<i32>,
}

/// 解析后的剪贴板配置：代码只读这一份，不必到处 `unwrap_or`。
#[derive(Clone, Debug, PartialEq)]
pub struct ClipboardSettings {
    pub enabled: bool,
    pub capture_files: bool,
    pub capture_rich_text: bool,
    pub record_images: bool,
    pub dedupe: bool,
    pub persistent: bool,
    pub limit_enabled: bool,
    pub limit: u32,
    pub delete_after_paste: bool,
    pub move_to_top_after_paste: bool,
    pub sound: bool,
    pub paste_sound: bool,
    pub sound_volume: f64,
    pub hotkey: String,
    pub plain_paste_hotkey: String,
    pub sequential_hotkey: String,
    pub search_hotkey: String,
    pub sequential_mode: bool,
    pub paste_method: String,
    pub privacy: bool,
    pub privacy_kinds: Vec<String>,
    pub privacy_custom: Vec<String>,
    pub cleanup_rules: String,
    pub app_policies: Vec<ClipAppPolicy>,
    pub sort: String,
    pub quick_paste_modifier: String,
    pub pause: bool,
    pub takeover_win_v: bool,
    pub window_pinned: bool,
    pub window_width: i32,
    pub window_height: i32,
}

/// 美化模板的配置（截图模块内的一段）。全部可选，缺省即用内置默认。
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct CaptureStyleCfg {
    pub enabled: Option<bool>,
    /// `gradient:<id>` / `solid:#RRGGBB` / `blur` / `transparent`
    pub background: Option<String>,
    /// 0–160
    pub padding: Option<i32>,
    /// 0–36
    pub radius: Option<i32>,
    /// 0–100
    pub shadow: Option<i32>,
    /// `none` / `classic` / `glass` / `macos-dark` / `macos-light` / `browser`
    pub frame: Option<String>,
    /// `auto` / `1:1` / `4:3` / `16:9` / `3:4` / `9:16`
    pub aspect: Option<String>,
    /// 旧字段：水印文字（现在水印有一整套参数，这里保留用于迁移）
    pub watermark: Option<String>,
    pub wm_enabled: Option<bool>,
    /// `margin` / `image`
    pub wm_placement: Option<String>,
    pub wm_text: Option<String>,
    /// `auto` / `#RRGGBB`
    pub wm_color: Option<String>,
    /// 24–80
    pub wm_opacity: Option<i32>,
    /// `system` / `serif` / `geometric` / `mono` / `script`
    pub wm_font: Option<String>,
    /// `light` / `regular` / `bold`
    pub wm_weight: Option<String>,
    /// 60–200（百分比）
    pub wm_size: Option<i32>,
}

/// 录屏：摄像头（外部设备）配置。
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct RecordCameraCfg {
    pub enabled: Option<bool>,
    /// 设备友好名（`MFEnumDeviceSources` 给的 `MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME`）。
    /// 空 = 用第一个可用设备。
    pub device: Option<String>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub fps: Option<i32>,
}

/// 录屏：声音配置。
///
/// 两路独立：**系统声音默认开**（录屏分享时观众要听到的通常是这个），
/// **麦克风默认关**（和摄像头同一个理由：默认不该把用户的声音录进去）。
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct RecordAudioCfg {
    /// 录制系统正在播放的声音（WASAPI loopback）
    pub system: Option<bool>,
    /// 录制麦克风
    pub mic: Option<bool>,
    /// 麦克风设备 id（WASAPI endpoint id）；空 = 系统默认
    pub mic_device: Option<String>,
    /// 音量百分比，100 = 原始（0–200）
    pub system_gain: Option<i32>,
    pub mic_gain: Option<i32>,
}

/// 录屏：布局配置（录制画面与摄像头画面怎么摆）。
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct RecordLayoutCfg {
    /// `screen` / `camera` / `pip` / `side-by-side`
    pub mode: Option<String>,
    /// `top-left` / `top-right` / `bottom-left` / `bottom-right`
    pub corner: Option<String>,
    /// 画中画边长占输出短边的百分比（8–60）
    pub size: Option<i32>,
    /// `rounded` / `circle`
    pub shape: Option<String>,
    pub mirror: Option<bool>,
    pub border: Option<bool>,
    pub shadow: Option<bool>,
    /// 画中画到输出边缘的距离（像素）
    pub margin: Option<i32>,
}

/// 录屏模块的配置。只有非默认值才写盘；`capture.record` 整段可缺省。
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct RecordCfg {
    pub enabled: Option<bool>,
    /// `video`（MP4/H.264）或 `gif`。两种能力的参数语义不同，默认值也不同。
    pub format: Option<String>,
    /// 画质档：`lite` / `standard` / `high`，对应 0.06 / 0.13 / 0.25 bpp。
    /// 只影响视频（GIF 没有码率，靠帧率+尺寸+调色板）。
    pub quality: Option<String>,
    /// 抓帧率。视频 24/30/60；GIF 受 1/100 秒延迟精度限制，10/15/20/25。
    pub fps: Option<i32>,
    /// 时长上限（秒）。**0 = 不限制**（默认）。
    ///
    /// 录制开始前没人知道自己要录多久——Bug 复现可能 20 秒，也可能要重演五分钟。
    /// 给个"安全上限"看着贴心，实际是到点把用户的录制掐断，比没有上限糟得多。
    /// 真需要防的是磁盘写满，那个用剩余空间保护（录制线程每 5 秒查一次）。
    pub max_seconds: Option<i32>,
    /// 输出宽度上限（0 = 不缩）。
    pub max_width: Option<i32>,
    /// 开录前倒计时（秒）。
    pub countdown: Option<i32>,
    /// 是否把鼠标画进帧里（BitBlt 不含光标）。
    pub cursor: Option<bool>,
    pub camera: Option<RecordCameraCfg>,
    pub layout: Option<RecordLayoutCfg>,
    pub audio: Option<RecordAudioCfg>,
}

/// 录制产出格式。两种能力共用同一条抓屏/合成管线，只在编码段分叉。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordFormat {
    /// MP4 / H.264，高帧率
    Video,
    /// GIF，低帧率小体积
    Gif,
}

impl RecordFormat {
    pub fn key(&self) -> &'static str {
        match self {
            RecordFormat::Video => "video",
            RecordFormat::Gif => "gif",
        }
    }
}

/// 解析后的录屏配置：代码只读这一份。
#[derive(Clone, Debug, PartialEq)]
pub struct RecordSettings {
    pub enabled: bool,
    pub format: RecordFormat,
    /// bpp（每像素每帧比特数），只在视频格式下用于换算码率
    pub bpp: f64,
    pub fps: i32,
    pub max_seconds: i32,
    pub max_width: i32,
    pub countdown: i32,
    pub cursor: bool,
    pub camera: RecordCamera,
    pub layout: RecordLayout,
    pub audio: RecordAudio,
}

/// 解析后的音频配置。
#[derive(Clone, Debug, PartialEq)]
pub struct RecordAudio {
    pub system: bool,
    pub mic: bool,
    pub mic_device: String,
    pub system_gain: i32,
    pub mic_gain: i32,
}

impl RecordAudio {
    pub fn any(&self) -> bool {
        self.system || self.mic
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RecordCamera {
    pub enabled: bool,
    pub device: String,
    pub width: i32,
    pub height: i32,
    pub fps: i32,
}

/// 布局的生效值。`mode` / `corner` / `shape` 保持字符串，由 `record` 模块解析成枚举
/// （未知值落到默认，不让用户写错一个词就录不出来）。
#[derive(Clone, Debug, PartialEq)]
pub struct RecordLayout {
    pub mode: String,
    pub corner: String,
    pub size: i32,
    pub shape: String,
    pub mirror: bool,
    pub border: bool,
    pub shadow: bool,
    pub margin: i32,
}

/// 截图模块的配置。只有非默认值才写盘；`capture` 整段可缺省。
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct CaptureCfg {
    pub enabled: Option<bool>,
    pub hotkey: Option<String>,
    /// 截图保存目录；空/缺省 = `<图片>/Zhuxia`。
    pub save_dir: Option<String>,
    /// 复制/贴图时同时存一份文件（sniplite 的 "Also save each capture as a file"）。
    pub auto_save: Option<bool>,
    /// 选区时显示放大镜（像素与颜色）。
    pub magnifier: Option<bool>,
    /// 选区时高亮鼠标下的窗口/控件（滚轮切换层级）。
    pub detect_window: Option<bool>,
    /// 美化模板（背景/边距/圆角/阴影/比例/水印）。
    pub style: Option<CaptureStyleCfg>,
    /// 录屏（GIF + 摄像头合成）。
    pub record: Option<RecordCfg>,
}

/// 解析后的截图配置：代码只读这一份。
#[derive(Clone, Debug, PartialEq)]
pub struct CaptureSettings {
    pub enabled: bool,
    pub hotkey: String,
    pub save_dir: PathBuf,
    pub auto_save: bool,
    pub magnifier: bool,
    pub detect_window: bool,
    pub style: Option<CaptureStyleCfg>,
    pub record: RecordSettings,
}

/// 手势动作。`type` = `hotkey` / `window` / `program` / `volume`，
/// 其余字段按动作类型取用：同一套结构承载窗口/程序/按键/音量几类动作。
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct GestureActionCfg {
    pub r#type: Option<String>,
    /// `hotkey`：`["Alt", "Left"]`，主键走 `zx_core::hotkey::vk_of`。
    pub keys: Option<Vec<String>>,
    /// `window`：toggle-topmost / toggle-maximize / minimize / close；
    /// `volume`：increase / decrease / mute。
    pub operation: Option<String>,
    /// `volume` 的步长（百分比，1–25）。
    pub amount: Option<i32>,
    /// `program`：可执行文件路径。
    pub path: Option<String>,
    pub args: Option<Vec<String>>,
}

/// 一条手势规则。
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct GestureRuleCfg {
    pub name: Option<String>,
    /// `global` / `app:<进程名>` / `category:<分类名>`
    pub scope: Option<String>,
    /// `right`（默认）/ `middle`
    pub button: Option<String>,
    /// 方向序列，接受 `Left` / `left` / `l` / `←` 四种写法（见 gesture::model）。
    pub pattern: Option<Vec<String>>,
    /// 滚轮修饰：`up` / `down`；空 = 纯方向手势。
    pub wheel: Option<String>,
    /// 滚轮规则：true = 每格立即执行，false = 松手执行一次。
    pub execute_on_wheel: Option<bool>,
    pub action: Option<GestureActionCfg>,
}

/// 轨迹线外观。
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct GestureTrailCfg {
    /// `#RRGGBB`
    pub active_color: Option<String>,
    pub inactive_color: Option<String>,
    /// 1–12（逻辑像素，绘制时按 DPI 缩放）
    pub thickness: Option<i32>,
    /// 0–100
    pub opacity: Option<i32>,
}

/// 手势分组：一组程序共用一套规则（主要给浏览器这种"一个手势多个程序通用"的场景）。
///
/// 匹配时**先看分组、再看通用**：当前窗口的进程命中某个组，就只用这个组的规则；
/// 组里没有这个手势，才落到 `gesture.rules`。
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct GestureGroupCfg {
    /// 显示名，例如「浏览器」。
    pub name: Option<String>,
    /// 进程名（可带 `.exe`，不分大小写；以 `*` 结尾表示前缀匹配，例如 `360*`）。
    pub apps: Option<Vec<String>>,
    pub rules: Option<Vec<GestureRuleCfg>>,
}

/// 程序 → 分类的归属，供 `category:<名>` 作用域使用。
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct GestureAppCfg {
    /// 进程名（不带路径，不区分大小写），例如 `chrome`。
    pub name: Option<String>,
    pub categories: Option<Vec<String>>,
}

/// 屏幕一条边上的滚轮行为。
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct EdgeCfg {
    pub enabled: Option<bool>,
    /// `volume`（默认·左）/ `brightness`（右）/ `tab`（上）/ `app`（下）/ `none`
    pub action: Option<String>,
}

/// 四条边的滚轮行为。缺省左/右开（音量 / 亮度）、上/下关：
/// 左右是"顺手调一下"的高频动作，上下会抢走页面的滚轮，打扰大于收益。
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct EdgesCfg {
    pub left: Option<EdgeCfg>,
    pub right: Option<EdgeCfg>,
    pub top: Option<EdgeCfg>,
    pub bottom: Option<EdgeCfg>,
    /// 距边多少像素算"在边上"（默认 2）。鼠标被系统夹在边上时坐标通常是 0。
    pub margin: Option<i32>,
    /// 同一个动作的最小间隔（毫秒），滚太快时合并，默认 45。
    pub repeat_ms: Option<u32>,
}

/// 按键回显（屏幕上显示刚按下的键）。组合键会合成一颗键帽，例如 `Ctrl+C`。
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct KeyEchoCfg {
    pub enabled: Option<bool>,
    /// `bottom-center`（默认）/ `top-center` / `top-left` / `top-right` /
    /// `bottom-left` / `bottom-right`
    pub position: Option<String>,
    /// 一个键显示多久（毫秒），默认 1200。
    pub duration_ms: Option<u32>,
    /// 键帽字号（逻辑像素，默认 15）。
    pub font_size: Option<i32>,
}

/// 图片格式转换模块的配置（第五模块）。整段可缺省。
///
/// 只存"用户改过"的项（与全文件一致）；`Option` 才能区分"没写过"和"写了 false"。
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct ImgConvCfg {
    /// 是否挂资源管理器右键菜单。
    pub enabled: Option<bool>,
    /// 有损格式质量 1–100（JPG / 有损 WebP 用）。
    pub quality: Option<u32>,
    /// WebP 用无损编码（照片没必要，截图/图标值得）。
    pub webp_lossless: Option<bool>,
    /// 同名文件直接覆盖；默认 false（自动改叫 `名字 (2).ext`）。
    pub overwrite: Option<bool>,
    /// 输出目录；空 = 与源文件同目录。
    pub out_dir: Option<String>,
    /// 输出 ICO 时塞哪几个规格（8–256，常用 16/24/32/48/64/128/256）。
    pub ico_sizes: Option<Vec<u32>>,
}

/// 图标提取模块的配置（第七模块）。整段可缺省。
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct AppIconCfg {
    /// 是否挂资源管理器右键菜单。
    pub enabled: Option<bool>,
    /// 默认输出形态：png / ico / all。
    pub mode: Option<String>,
    /// PNG 最长边上限（16–4096，只缩不放）。
    pub max_size: Option<u32>,
    /// ICO 里塞哪几档（8–256）。
    pub ico_sizes: Option<Vec<u32>>,
    /// 输出目录；空 = 与源文件同目录。
    pub out_dir: Option<String>,
    /// 同名文件直接覆盖；默认 false。
    pub overwrite: Option<bool>,
}

/// 图床模块的配置（第八模块）。
///
/// 三个页面（上传 / 图片 / 设置）共用这一份；平台私有字段放 `providers`，用通用 map 存，
/// 新增平台不用改 zx-core。**密钥不在这里**（见 imgbed 的 `secrets.dat`，DPAPI 加密）。
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct ImgBedCfg {
    pub enabled: Option<bool>,
    /// 当前平台：github / s3 / webdav / custom
    pub provider: Option<String>,

    pub upload: ImgBedUploadCfg,
    pub copy: ImgBedCopyCfg,
    pub browser: ImgBedBrowserCfg,

    /// 上传剪贴板图片的热键；空 = 不注册
    pub hotkey: Option<String>,
    /// HTTP 代理（如 `http://127.0.0.1:7890`）；空 = 跟随系统（WinHTTP）
    pub proxy: Option<String>,

    /// 各平台自己的字段（`providers.<id>`），后端自己 serde 成结构体。
    pub providers: BTreeMap<String, serde_yaml::Value>,

    // ---- v0 遗留字段：只用于迁移，读到就转成上面的结构 ----
    pub owner: Option<String>,
    pub repo: Option<String>,
    pub branch: Option<String>,
    pub dir: Option<String>,
    pub date_dir: Option<bool>,
    pub cdn: Option<String>,
    pub cdn_custom: Option<String>,
    pub format: Option<String>,
    pub naming: Option<String>,
    pub prefix: Option<String>,
    pub auto_copy: Option<bool>,
    pub commit_message: Option<String>,
    pub max_mb: Option<u32>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct ImgBedUploadCfg {
    /// timestamp / original
    pub naming: Option<String>,
    pub prefix: Option<String>,
    /// keep / webp
    pub convert: Option<String>,
    /// 1–100，仅 webp
    pub quality: Option<u8>,
    /// 0 = 不限制；>0 时长边超出就等比例缩小（只缩不放）
    pub max_side: Option<u32>,
    pub max_mb: Option<u32>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct ImgBedCopyCfg {
    /// markdown / html / bbcode / url
    pub format: Option<String>,
    pub auto: Option<bool>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct ImgBedBrowserCfg {
    /// local / remote
    pub source: Option<String>,
    /// newest / oldest / name / size
    pub sort: Option<String>,
    pub page_size: Option<u32>,
}

/// 解析后的图床配置（代码只读这一份）。
#[derive(Clone, Debug, PartialEq)]
pub struct ImgBedSettings {
    pub enabled: bool,
    pub provider: String,
    pub naming: String,
    pub prefix: String,
    pub convert: String,
    pub quality: u8,
    pub max_side: u32,
    pub max_mb: u32,
    pub copy_format: String,
    pub auto_copy: bool,
    pub hotkey: String,
    pub proxy: String,
    pub browser_source: String,
    pub browser_sort: String,
    pub browser_page_size: u32,
    pub providers: BTreeMap<String, serde_yaml::Value>,
}

impl ImgBedSettings {
    /// 当前平台自己的配置段（可能是 None：还没填过）。
    pub fn provider_value(&self) -> Option<&serde_yaml::Value> {
        self.providers.get(&self.provider)
    }

    /// 当前平台某个字符串字段（各后端自检/UI 回显用）。
    pub fn provider_str(&self, key: &str) -> String {
        self.provider_value()
            .and_then(|v| v.as_mapping())
            .and_then(|m| m.get(serde_yaml::Value::String(key.into())))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string()
    }
}

/// v0 → v2 迁移：把平铺的 GitHub 字段挪到 `providers.github`，全局项挪到 `upload/copy`。
fn migrate_imgbed(c: &mut ImgBedCfg) {
    let legacy = c.owner.is_some()
        || c.repo.is_some()
        || c.branch.is_some()
        || c.cdn.is_some()
        || c.commit_message.is_some();
    if legacy && !c.providers.contains_key("github") {
        let mut m = serde_yaml::Mapping::new();
        let mut put = |k: &str, v: String| {
            m.insert(
                serde_yaml::Value::String(k.into()),
                serde_yaml::Value::String(v),
            );
        };
        put("owner", c.owner.clone().unwrap_or_default());
        put("repo", c.repo.clone().unwrap_or_default());
        put("branch", c.branch.clone().unwrap_or_default());
        put("cdn", c.cdn.clone().unwrap_or_else(|| "jsdelivr".into()));
        put("cdnCustom", c.cdn_custom.clone().unwrap_or_default());
        put(
            "commitMessage",
            c.commit_message
                .clone()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "Upload image via Zhuxia".into()),
        );
        // 老路径：dir + dateDir（yyyyMMdd）→ 模板变量
        let dir = c.dir.clone().unwrap_or_else(|| "images".into());
        let dir = dir.trim().trim_matches('/').to_string();
        let path = if c.date_dir != Some(false) {
            if dir.is_empty() {
                "{yyyy}/{MM}/{dd}".to_string()
            } else {
                format!("{dir}/{{yyyy}}/{{MM}}/{{dd}}")
            }
        } else {
            dir
        };
        put("path", path);
        c.providers
            .insert("github".into(), serde_yaml::Value::Mapping(m));
    }

    if c.upload.naming.is_none() {
        c.upload.naming = c.naming.clone();
    }
    if c.upload.prefix.is_none() {
        c.upload.prefix = c.prefix.clone();
    }
    if c.upload.max_mb.is_none() {
        c.upload.max_mb = c.max_mb;
    }
    if c.copy.format.is_none() {
        c.copy.format = c.format.clone();
    }
    if c.copy.auto.is_none() {
        c.copy.auto = c.auto_copy;
    }
}

/// 解析后的图标提取配置（代码只读这一份）。
#[derive(Clone, Debug, PartialEq)]
pub struct AppIconSettings {
    pub enabled: bool,
    pub mode: String,
    pub max_size: u32,
    pub ico_sizes: Vec<u32>,
    pub out_dir: Option<PathBuf>,
    pub overwrite: bool,
}

/// 解析后的转换配置（代码只读这一份）。
#[derive(Clone, Debug, PartialEq)]
pub struct ImgConvSettings {
    pub enabled: bool,
    pub quality: u8,
    pub webp_lossless: bool,
    pub overwrite: bool,
    pub out_dir: Option<PathBuf>,
    /// ICO 输出规格（已清洗：8–256、去重、降序）。
    pub ico_sizes: Vec<u32>,
}

/// ICO 规格清洗：8–256、去重、降序、最多 8 个；列表为空时回落到最常用的四档。
fn clean_ico_sizes(v: Vec<u32>) -> Vec<u32> {
    let mut v: Vec<u32> = v.into_iter().filter(|s| (8..=256).contains(s)).collect();
    v.sort_unstable_by(|a, b| b.cmp(a));
    v.dedup();
    v.truncate(8);
    if v.is_empty() {
        v = vec![256, 48, 32, 16];
    }
    v
}

/// 鼠标手势模块的配置（第四模块）。整段可缺省。
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct GestureCfg {
    pub enabled: Option<bool>,
    /// 0–200：100 为默认档；数值越低，方向判定越迟钝（更难识别出手势）。
    pub sensitivity: Option<i32>,
    /// 允许中键手势（默认关，避免抢中键粘贴/自动滚动）。
    pub middle_button: Option<bool>,
    pub disable_in_fullscreen: Option<bool>,
    pub disable_on_desktop: Option<bool>,
    /// 暂停（托盘/设置页用；也可以手改配置文件）。
    pub paused: Option<bool>,
    /// 命中后显示动作名。
    pub hint: Option<bool>,
    pub hint_duration_ms: Option<u32>,
    /// `start-window`（默认）/ `current-window`
    pub target_window: Option<String>,
    /// 不响应手势的进程名（不带 .exe 也行，子串匹配）。
    pub excluded_apps: Option<Vec<String>>,
    pub trail: Option<GestureTrailCfg>,
    pub apps: Option<Vec<GestureAppCfg>>,
    /// 按软件分组的专用规则（浏览器等）。组顺序 = 匹配顺序。
    pub groups: Option<Vec<GestureGroupCfg>>,
    /// 规则表。**缺省 = 用内置默认规则**；写成空表 = 一条规则都不装。
    pub rules: Option<Vec<GestureRuleCfg>>,
    /// 屏幕四条边的滚轮行为。
    pub edges: Option<EdgesCfg>,
    /// 按键回显。
    pub key_echo: Option<KeyEchoCfg>,
}

/// 一条边的滚轮动作。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EdgeAction {
    /// 系统音量（走 VK_VOLUME_UP/DOWN，OSD 由系统画）
    Volume,
    /// 显示器亮度（DDC/CI；笔记本内屏需要 WMI，暂时只支持外接屏）
    Brightness,
    /// 切换标签页（Ctrl+Tab / Ctrl+Shift+Tab）
    Tab,
    /// 切换程序（Alt+Tab / Alt+Shift+Tab）
    App,
}

impl EdgeAction {
    pub fn key(self) -> &'static str {
        match self {
            EdgeAction::Volume => "volume",
            EdgeAction::Brightness => "brightness",
            EdgeAction::Tab => "tab",
            EdgeAction::App => "app",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            EdgeAction::Volume => "音量",
            EdgeAction::Brightness => "亮度",
            EdgeAction::Tab => "标签切换",
            EdgeAction::App => "程序切换",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "volume" | "音量" => Some(EdgeAction::Volume),
            "brightness" | "亮度" => Some(EdgeAction::Brightness),
            "tab" | "标签" | "标签切换" => Some(EdgeAction::Tab),
            "app" | "程序" | "程序切换" => Some(EdgeAction::App),
            "none" | "off" | "" => None,
            _ => None,
        }
    }
}

/// 四条边解析后的状态。`None` = 这条边不接管滚轮。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EdgeSettings {
    pub left: Option<EdgeAction>,
    pub right: Option<EdgeAction>,
    pub top: Option<EdgeAction>,
    pub bottom: Option<EdgeAction>,
    pub margin: i32,
    pub repeat_ms: u32,
}

impl Default for EdgeSettings {
    fn default() -> Self {
        // 左边音量、右边亮度：这两条边默认接管（不需要用户先去设置页开开关）。
        // 上/下会抢页面滚轮，保持关闭。
        EdgeSettings {
            left: Some(EdgeAction::Volume),
            right: Some(EdgeAction::Brightness),
            top: None,
            bottom: None,
            margin: 2,
            repeat_ms: 45,
        }
    }
}

impl EdgeSettings {
    pub fn any(&self) -> bool {
        self.left.is_some() || self.right.is_some() || self.top.is_some() || self.bottom.is_some()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct KeyEchoSettings {
    pub enabled: bool,
    /// `bottom-center` / `bottom-left` / `bottom-right` / `top-center`
    pub position: &'static str,
    pub duration_ms: u32,
    pub font_size: i32,
}

impl Default for KeyEchoSettings {
    fn default() -> Self {
        KeyEchoSettings { enabled: false, position: "bottom-center", duration_ms: 1200, font_size: 15 }
    }
}

/// 解析后的手势配置：代码只读这一份。
#[derive(Clone, Debug, PartialEq)]
pub struct GestureSettings {
    pub enabled: bool,
    pub sensitivity: i32,
    pub middle_button: bool,
    pub disable_in_fullscreen: bool,
    pub disable_on_desktop: bool,
    pub paused: bool,
    pub hint: bool,
    pub hint_duration_ms: u32,
    pub target_window: String,
    pub excluded_apps: Vec<String>,
    /// 0xAARRGGBB（已按 opacity 合过 alpha）
    pub trail_active_color: u32,
    pub trail_inactive_color: u32,
    pub trail_thickness: i32,
    pub apps: Vec<GestureAppCfg>,
    /// `None` = 没写过（装内置浏览器分组）；`Some([])` = 明确清空。
    pub groups: Option<Vec<GestureGroupCfg>>,
    pub rules: Option<Vec<GestureRuleCfg>>,
    pub edges: EdgeSettings,
    pub key_echo: KeyEchoSettings,
}

/// `#RRGGBB` / `#AARRGGBB` / `RRGGBB` → 0xAARRGGBB；解析失败用默认值。
/// 默认动作按"左边音量、右边亮度、上边标签、下边程序"落位。
///
/// 左右两条边**缺省就开**（这是最常见的手感），上下两条边缺省关（会抢页面滚轮）。
/// 显式写 `enabled: false` 仍然能关掉；缺省动作只在这条边没写 `action` 时兜底。
fn parse_edges(c: Option<&EdgesCfg>) -> EdgeSettings {
    let Some(c) = c else { return EdgeSettings::default() };
    let side = |cfg: Option<&EdgeCfg>, fallback: EdgeAction, default_on: bool| -> Option<EdgeAction> {
        let cfg = match cfg {
            Some(cfg) => cfg,
            None => return if default_on { Some(fallback) } else { None },
        };
        // 没写 enabled：按这条边的默认值（左/右开，上/下关）
        if !cfg.enabled.unwrap_or(default_on) {
            return None;
        }
        match cfg.action.as_deref() {
            None => Some(fallback),
            Some(txt) => {
                if txt.trim().is_empty() {
                    Some(fallback)
                } else if txt.trim().eq_ignore_ascii_case("none") {
                    None
                } else {
                    EdgeAction::parse(txt).or(Some(fallback))
                }
            }
        }
    };
    EdgeSettings {
        left: side(c.left.as_ref(), EdgeAction::Volume, true),
        right: side(c.right.as_ref(), EdgeAction::Brightness, true),
        top: side(c.top.as_ref(), EdgeAction::Tab, false),
        bottom: side(c.bottom.as_ref(), EdgeAction::App, false),
        margin: c.margin.unwrap_or(2).clamp(0, 32),
        repeat_ms: c.repeat_ms.unwrap_or(45).clamp(10, 500),
    }
}

fn parse_key_echo(c: Option<&KeyEchoCfg>) -> KeyEchoSettings {
    let d = KeyEchoSettings::default();
    let Some(c) = c else { return d };
    let position = match c.position.as_deref().map(|v| v.trim()) {
        Some("top-center") => "top-center",
        Some("top-left") => "top-left",
        Some("top-right") => "top-right",
        Some("bottom-left") => "bottom-left",
        Some("bottom-right") => "bottom-right",
        _ => "bottom-center",
    };
    KeyEchoSettings {
        enabled: c.enabled == Some(true),
        position,
        duration_ms: c.duration_ms.unwrap_or(d.duration_ms).clamp(400, 6000),
        font_size: c.font_size.unwrap_or(d.font_size).clamp(9, 32),
    }
}

pub fn parse_color(text: &str, default: u32) -> u32 {
    let t = text.trim().trim_start_matches('#');
    let v = u32::from_str_radix(t, 16);
    match (t.len(), v) {
        (6, Ok(v)) => 0xFF00_0000 | v,
        (8, Ok(v)) => v,
        _ => default,
    }
}

/// 全部设置。`None` 表示"跟随系统/默认值"，只有非默认值才写盘——
/// 与上游 `AppSettings` 的语义一致（DefaultValuesHandling.OmitNull）。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub elevation_consent: Option<bool>,
    pub startup_guide_completed: Option<bool>,

    pub overlay_on_left: Option<bool>,
    pub overlay_snap_to_start: Option<bool>,
    pub taskbar_displays: Option<Vec<DisplaySel>>,
    pub compact_layout: Option<bool>,
    pub show_separators: Option<bool>,
    pub item_order: Option<Vec<usize>>,

    pub floating_mode: Option<bool>,
    pub floating_topmost: Option<bool>,
    pub floating_allow_out_of_bounds: Option<bool>,
    pub floating_opacity: Option<f64>,
    pub floating_edge_hide: Option<bool>,
    pub floating_fullscreen_hide: Option<bool>,
    pub floating_x: Option<i32>,
    pub floating_y: Option<i32>,

    pub theme: Option<Theme>,
    pub sample_interval_ms: Option<u32>,

    pub cpu_sampling_enabled: Option<bool>,
    pub ram_sampling_enabled: Option<bool>,
    pub disk_sampling_enabled: Option<bool>,
    pub gpu_sampling_enabled: Option<bool>,
    pub net_sampling_enabled: Option<bool>,

    pub merge_same_path_processes: Option<bool>,
    pub disk_display: Option<MetricDisplayMode>,
    pub disk_display_index: Option<usize>,
    pub gpu_display: Option<MetricDisplayMode>,
    pub gpu_display_index: Option<usize>,
    pub gpu_taskbar_content: Option<GpuTaskbarContent>,
    pub gpu_temp_index: Option<usize>,
    pub gpu_temp_unit: Option<GpuTempUnit>,

    pub net_adapter_id: Option<String>,
    pub net_adapter_name: Option<String>,
    pub public_ip_enabled: Option<bool>,
    pub clash_enabled: Option<bool>,
    pub clash_api_address: Option<String>,
    pub clash_api_secret: Option<String>,

    pub update_check_enabled: Option<bool>,
    pub ignored_update_version: Option<String>,

    /// 剪贴板模块（第二模块）。整段可缺省，旧配置文件照样能读。
    pub clipboard: Option<ClipboardCfg>,

    /// 截图模块（第三模块）。同样整段可缺省。
    pub capture: Option<CaptureCfg>,

    /// 鼠标手势模块（第四模块）。同样整段可缺省。
    pub gesture: Option<GestureCfg>,

    /// 图片转换模块（第五模块）。同样整段可缺省。
    pub imgconv: Option<ImgConvCfg>,

    /// 图标提取模块（第七模块）。同样整段可缺省。
    pub appicon: Option<AppIconCfg>,

    /// 图床模块（第八模块）。同样整段可缺省。
    pub imgbed: Option<ImgBedCfg>,
}

impl Settings {
    // ---------- 带默认值的读取口：代码里只读这些，不直接读 Option ----------
    pub fn on_left(&self) -> bool { self.overlay_on_left != Some(false) }
    pub fn snap_to_start(&self) -> bool { self.overlay_snap_to_start == Some(true) }
    pub fn compact(&self) -> bool { self.compact_layout == Some(true) }
    pub fn separators(&self) -> bool { self.show_separators != Some(false) }
    pub fn floating(&self) -> bool { self.floating_mode == Some(true) }
    pub fn floating_topmost(&self) -> bool { self.floating_topmost != Some(false) }
    pub fn floating_edge_hide(&self) -> bool { self.floating_edge_hide == Some(true) }
    pub fn floating_fullscreen_hide(&self) -> bool { self.floating_fullscreen_hide != Some(false) }
    pub fn floating_opacity(&self) -> f32 {
        (self.floating_opacity.unwrap_or(1.0) as f32).clamp(0.2, 1.0)
    }
    pub fn sample_interval_ms(&self) -> u32 { self.sample_interval_ms.unwrap_or(1000).clamp(250, 10000) }
    pub fn merge_by_path(&self) -> bool { self.merge_same_path_processes != Some(false) }
    pub fn disk_display(&self) -> MetricDisplayMode { self.disk_display.unwrap_or(MetricDisplayMode::Average) }
    pub fn gpu_display(&self) -> MetricDisplayMode { self.gpu_display.unwrap_or(MetricDisplayMode::Max) }
    pub fn gpu_content(&self) -> GpuTaskbarContent { self.gpu_taskbar_content.unwrap_or(GpuTaskbarContent::Usage) }
    pub fn gpu_temp_unit(&self) -> GpuTempUnit { self.gpu_temp_unit.unwrap_or(GpuTempUnit::Celsius) }
    pub fn public_ip(&self) -> bool { self.public_ip_enabled != Some(false) }
    pub fn clash(&self) -> bool { self.clash_enabled != Some(false) }
    pub fn update_check(&self) -> bool { self.update_check_enabled != Some(false) }
    pub fn theme(&self) -> Option<Theme> { self.theme }

    /// 剪贴板模块的生效配置（缺省值即首次运行时的默认行为）。
    pub fn clipboard(&self) -> ClipboardSettings {
        let c = self.clipboard.clone().unwrap_or_default();
        let mut kinds = c
            .privacy_kinds
            .clone()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| vec![
                "phone".into(), "idcard".into(), "email".into(), "secret".into(),
            ]);
        kinds.retain(|k| !k.trim().is_empty());
        ClipboardSettings {
            enabled: c.enabled != Some(false),
            capture_files: c.capture_files != Some(false),
            // 富文本默认开：Word / 网页复制过来本来就带格式，丢掉才是"改行为"
            capture_rich_text: c.capture_rich_text != Some(false),
            record_images: c.record_images != Some(false),
            dedupe: c.dedupe != Some(false),
            persistent: c.persistent != Some(false),
            limit_enabled: c.limit_enabled != Some(false),
            limit: c.limit.unwrap_or(1000).clamp(50, 99999),
            delete_after_paste: c.delete_after_paste == Some(true),
            move_to_top_after_paste: c.move_to_top_after_paste != Some(false),
            sound: c.sound == Some(true),
            paste_sound: c.paste_sound != Some(false),
            sound_volume: c.sound_volume.unwrap_or(1.0).clamp(0.0, 1.0),
            hotkey: c.hotkey.unwrap_or_else(|| "Alt+C".into()),
            plain_paste_hotkey: c.plain_paste_hotkey.unwrap_or_default(),
            sequential_hotkey: c.sequential_hotkey.unwrap_or_else(|| "Alt+V".into()),
            search_hotkey: c.search_hotkey.unwrap_or_else(|| "Alt+F".into()),
            sequential_mode: c.sequential_mode == Some(true),
            paste_method: c.paste_method.unwrap_or_else(|| "auto".into()),
            privacy: c.privacy == Some(true),
            privacy_kinds: kinds,
            privacy_custom: c.privacy_custom.unwrap_or_default(),
            cleanup_rules: c.cleanup_rules.unwrap_or_default(),
            app_policies: c.app_policies.unwrap_or_default(),
            sort: c.sort.unwrap_or_else(|| "recent".into()),
            quick_paste_modifier: c.quick_paste_modifier.unwrap_or_else(|| "disabled".into()),
            pause: c.pause == Some(true),
            takeover_win_v: c.takeover_win_v == Some(true),
            window_pinned: c.window_pinned == Some(true),
            window_width: c.window_width.unwrap_or(460).clamp(360, 1600),
            window_height: c.window_height.unwrap_or(580).clamp(320, 1200),
        }
    }

    /// 图片转换模块的生效配置。缺省：启用、质量 90、有损 WebP、不覆盖、原目录、
    /// ICO 四档（256/48/32/16）。
    pub fn imgconv(&self) -> ImgConvSettings {
        let c = self.imgconv.clone().unwrap_or_default();
        let raw = c.out_dir.unwrap_or_default();
        let dir = raw.trim();
        ImgConvSettings {
            enabled: c.enabled != Some(false),
            quality: c.quality.unwrap_or(90).clamp(1, 100) as u8,
            webp_lossless: c.webp_lossless == Some(true),
            overwrite: c.overwrite == Some(true),
            out_dir: if dir.is_empty() { None } else { Some(PathBuf::from(dir)) },
            ico_sizes: clean_ico_sizes(c.ico_sizes.unwrap_or_default()),
        }
    }

    /// 图床模块的生效配置。缺省：启用、GitHub、WebP/82/长边 2560、Markdown、自动复制、
    /// `Ctrl+Shift+U`、单文件 25 MB。
    pub fn imgbed(&self) -> ImgBedSettings {
        let mut c = self.imgbed.clone().unwrap_or_default();
        migrate_imgbed(&mut c);

        let provider = c.provider.unwrap_or_else(|| "github".into()).to_ascii_lowercase();
        let provider = match provider.as_str() {
            "github" | "s3" | "webdav" | "custom" => provider,
            _ => "github".into(),
        };
        let naming = c.upload.naming.unwrap_or_else(|| "timestamp".into()).to_ascii_lowercase();
        let naming = match naming.as_str() {
            "original" => naming,
            _ => "timestamp".into(),
        };
        let convert = c.upload.convert.unwrap_or_else(|| "webp".into()).to_ascii_lowercase();
        let convert = if convert == "keep" { convert } else { "webp".into() };
        let copy_format = c.copy.format.unwrap_or_else(|| "markdown".into()).to_ascii_lowercase();
        let copy_format = match copy_format.as_str() {
            "html" | "bbcode" | "url" => copy_format,
            _ => "markdown".into(),
        };
        let browser_source = c.browser.source.unwrap_or_else(|| "local".into()).to_ascii_lowercase();
        let browser_source = if browser_source == "remote" { browser_source } else { "local".into() };
        let browser_sort = c.browser.sort.unwrap_or_else(|| "newest".into()).to_ascii_lowercase();
        let browser_sort = match browser_sort.as_str() {
            "oldest" | "name" | "size" => browser_sort,
            _ => "newest".into(),
        };
        ImgBedSettings {
            enabled: c.enabled != Some(false),
            provider,
            naming,
            prefix: c.upload.prefix.unwrap_or_default().trim().to_string(),
            convert,
            quality: c.upload.quality.unwrap_or(82).clamp(1, 100),
            // 0 是有效值（不限制），所以夹紧时不能再走 clamp(1, …)
            max_side: c.upload.max_side.unwrap_or(2560).min(8192),
            max_mb: c.upload.max_mb.unwrap_or(25).clamp(1, 100),
            copy_format,
            auto_copy: c.copy.auto != Some(false),
            hotkey: c.hotkey.unwrap_or_else(|| "Ctrl+Shift+U".into()).trim().to_string(),
            proxy: c.proxy.unwrap_or_default().trim().to_string(),
            browser_source,
            browser_sort,
            browser_page_size: c.browser.page_size.unwrap_or(40).clamp(8, 200),
            providers: c.providers,
        }
    }

    /// 图标提取模块的生效配置。缺省：启用、PNG、长边 512、ICO 六档。
    pub fn appicon(&self) -> AppIconSettings {
        let c = self.appicon.clone().unwrap_or_default();
        let raw = c.out_dir.unwrap_or_default();
        let dir = raw.trim();
        let mode = c.mode.unwrap_or_else(|| "png".into()).to_ascii_lowercase();
        let mode = match mode.as_str() {
            "png" | "ico" | "all" => mode,
            _ => "png".into(),
        };
        AppIconSettings {
            enabled: c.enabled != Some(false),
            mode,
            max_size: c.max_size.unwrap_or(512).clamp(16, 4096),
            ico_sizes: clean_ico_sizes(c.ico_sizes.unwrap_or_else(|| vec![256, 128, 64, 48, 32, 16])),
            out_dir: if dir.is_empty() { None } else { Some(PathBuf::from(dir)) },
            overwrite: c.overwrite == Some(true),
        }
    }

    /// 截图模块的生效配置（缺省值为常见截图工具的默认习惯）。
    ///
    /// 热键默认 `Alt+A`：剪贴板已经占了 `Alt+C` / `Alt+V` / `Alt+F`，别撞。
    pub fn capture(&self) -> CaptureSettings {
        let c = self.capture.clone().unwrap_or_default();
        let raw = c.save_dir.unwrap_or_default();
        let trimmed = raw.trim();
        let save_dir = if trimmed.is_empty() {
            default_capture_dir()
        } else {
            PathBuf::from(trimmed)
        };
        CaptureSettings {
            enabled: c.enabled != Some(false),
            hotkey: c.hotkey.unwrap_or_else(|| "Alt+A".into()),
            save_dir,
            auto_save: c.auto_save == Some(true),
            magnifier: c.magnifier != Some(false),
            detect_window: c.detect_window != Some(false),
            style: c.style.clone(),
            record: self.record(),
        }
    }

    /// 录屏模块的生效配置。缺省值按 `docs/RECORD.md`：10fps、960 宽、3 秒倒计时、
    /// 右下角画中画（24%、圆角、镜像）。
    pub fn record(&self) -> RecordSettings {
        let c = self.capture.clone().unwrap_or_default().record.unwrap_or_default();
        let format = match c.format.as_deref() {
            Some("gif") => RecordFormat::Gif,
            _ => RecordFormat::Video,
        };
        // 帧率的默认值与上限随格式走：视频 30/60，GIF 15/25
        let (def_fps, max_fps) = match format {
            RecordFormat::Video => (30, 60),
            RecordFormat::Gif => (15, 25),
        };
        let fps = c.fps.unwrap_or(def_fps).clamp(5, max_fps);
        let bpp = match c.quality.as_deref() {
            Some("lite") => 0.06,
            Some("high") => 0.25,
            _ => 0.13,
        };
        // 视频默认原始尺寸（0 = 不限），GIF 默认 960 宽（体积第一杠杆）
        let def_width = match format {
            RecordFormat::Video => 0,
            RecordFormat::Gif => 960,
        };
        let cam = c.camera.clone().unwrap_or_default();
        let lay = c.layout.clone().unwrap_or_default();
        let aud = c.audio.clone().unwrap_or_default();
        RecordSettings {
            enabled: c.enabled != Some(false),
            format,
            bpp,
            fps,
            // 默认不限；填正数才是硬上限
            max_seconds: c.max_seconds.unwrap_or(0).clamp(0, 86_400),
            max_width: c.max_width.unwrap_or(def_width).clamp(0, 7680),
            countdown: c.countdown.unwrap_or(3).clamp(0, 10),
            cursor: c.cursor != Some(false),
            camera: RecordCamera {
                // 默认关闭：Bug 复现/文档演示不需要人脸，开了还多一路采集与 30% 体积
                enabled: cam.enabled == Some(true),
                device: cam.device.clone().unwrap_or_default(),
                width: cam.width.unwrap_or(640).clamp(160, 3840),
                height: cam.height.unwrap_or(480).clamp(120, 2160),
                fps: cam.fps.unwrap_or(30).clamp(5, 60),
            },
            layout: RecordLayout {
                mode: lay.mode.clone().unwrap_or_else(|| "screen".into()),
                corner: lay.corner.clone().unwrap_or_else(|| "bottom-right".into()),
                size: lay.size.unwrap_or(24).clamp(8, 60),
                shape: lay.shape.clone().unwrap_or_else(|| "rounded".into()),
                mirror: lay.mirror != Some(false),
                border: lay.border != Some(false),
                shadow: lay.shadow != Some(false),
                margin: lay.margin.unwrap_or(16).clamp(0, 200),
            },
            audio: RecordAudio {
                // 系统声音默认**开**：录屏给别看的场景里，观众要听的就是这个
                system: aud.system != Some(false),
                // 麦克风默认**关**：默认不该把用户说话录进去
                mic: aud.mic == Some(true),
                mic_device: aud.mic_device.clone().unwrap_or_default(),
                system_gain: aud.system_gain.unwrap_or(100).clamp(0, 200),
                mic_gain: aud.mic_gain.unwrap_or(100).clamp(0, 200),
            },
        }
    }

    /// 手势模块的生效配置。缺省为默认手感（灵敏度 100）。
    ///
    /// `rules` 保持 `Option`：`None` 表示"没写过"，由 gesture 模块装内置默认规则；
    /// `Some(vec![])` 表示用户明确清空了规则表，此时一条都不装。
    pub fn gesture(&self) -> GestureSettings {
        let g = self.gesture.clone().unwrap_or_default();
        let trail = g.trail.clone().unwrap_or_default();
        let opacity = trail.opacity.unwrap_or(90).clamp(0, 100) as u32;
        let alpha = opacity * 255 / 100;
        let active = parse_color(
            trail.active_color.as_deref().unwrap_or("#3B82F6"),
            0xFF3B_82F6,
        );
        let inactive = parse_color(
            trail.inactive_color.as_deref().unwrap_or("#BDBDBD"),
            0xFFBDBDBD,
        );
        let with_alpha = |c: u32| (c & 0x00FF_FFFF) | (alpha << 24);
        GestureSettings {
            enabled: g.enabled != Some(false),
            sensitivity: g.sensitivity.unwrap_or(100).clamp(0, 200),
            middle_button: g.middle_button == Some(true),
            // 默认开：进游戏/全屏播放器里手势本来就用不上，误触发只会更烦。
            // 判定侧只认真·全屏（`IsZoomed` 的最大化窗口不算，见 zx-ui::window）。
            disable_in_fullscreen: g.disable_in_fullscreen != Some(false),
            disable_on_desktop: g.disable_on_desktop == Some(true),
            paused: g.paused == Some(true),
            hint: g.hint != Some(false),
            hint_duration_ms: g.hint_duration_ms.unwrap_or(700).clamp(200, 5000),
            target_window: g
                .target_window
                .unwrap_or_else(|| "start-window".into()),
            excluded_apps: g
                .excluded_apps
                .unwrap_or_default()
                .into_iter()
                .map(|s| s.trim().to_ascii_lowercase())
                .filter(|s| !s.is_empty())
                .collect(),
            trail_active_color: with_alpha(active),
            trail_inactive_color: with_alpha(inactive),
            trail_thickness: trail.thickness.unwrap_or(4).clamp(1, 12),
            apps: g.apps.unwrap_or_default(),
            groups: g.groups,
            rules: g.rules,
            edges: parse_edges(g.edges.as_ref()),
            key_echo: parse_key_echo(g.key_echo.as_ref()),
        }
    }

    /// 采样项目掩码（位序 = 点击槽位 0=CPU 1=内存 2=磁盘 3=GPU 4=网络）。
    pub fn mask(&self) -> u32 {
        let mut m = 0;
        if self.cpu_sampling_enabled != Some(false) { m |= 1; }
        if self.ram_sampling_enabled != Some(false) { m |= 2; }
        if self.disk_sampling_enabled != Some(false) { m |= 4; }
        if self.gpu_sampling_enabled != Some(false) { m |= 8; }
        if self.net_sampling_enabled != Some(false) { m |= 16; }
        if m == 0 { m = 1 } // 手改成全关也至少留 CPU
        m
    }

    /// 项目顺序：去重、剔除非法、补齐缺项（与 TaskbarWindow.NormalizeItemOrder 一致）。
    pub fn normalized_order(&self) -> [usize; 5] {
        let mut out = Vec::with_capacity(5);
        if let Some(o) = &self.item_order {
            for &s in o {
                if s < 5 && !out.contains(&s) { out.push(s); }
            }
        }
        for s in 0..5 {
            if !out.contains(&s) { out.push(s); }
        }
        let mut a = [0usize; 5];
        a.copy_from_slice(&out);
        a
    }

    pub fn save(&self, path: &Path) {
        // 只写非默认值：与上游的 OmitNull 一致，配置文件永远是一份"变更清单"。
        // 递归剪掉 null —— 只剪顶层是不够的：`capture_patch` / `clipboard_patch` 会把
        // 整段 `Option` 结构反序列化成 `enabled: null, hotkey: null, …` 写进文件。
        let text = match serde_yaml::to_value(self) {
            Ok(v) => match prune_nulls(v) {
                Some(v) => serde_yaml::to_string(&v).unwrap_or_default(),
                None => return,
            },
            Err(_) => return,
        };
        let tmp = path.with_extension("yaml.tmp");
        if std::fs::write(&tmp, text).is_err() { return; }
        // 原子替换：老实现"先删后改名"会在中间态丢掉整份配置。
        if std::fs::rename(&tmp, path).is_err() {
            let _ = std::fs::copy(&tmp, path);
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

/// 递归剪掉 null 与空表：`{a: null, b: {c: null}}` → `{}`。
fn prune_nulls(v: serde_yaml::Value) -> Option<serde_yaml::Value> {
    match v {
        serde_yaml::Value::Null => None,
        serde_yaml::Value::Mapping(map) => {
            let mut kept = serde_yaml::Mapping::new();
            for (k, val) in map {
                if let Some(val) = prune_nulls(val) {
                    kept.insert(k, val);
                }
            }
            if kept.is_empty() { None } else { Some(serde_yaml::Value::Mapping(kept)) }
        }
        serde_yaml::Value::Sequence(seq) => {
            if seq.is_empty() { None } else { Some(serde_yaml::Value::Sequence(seq)) }
        }
        other => Some(other),
    }
}

/// 全局配置：UI 线程写、采样线程读，一次一整份克隆（开销可忽略，1s 一次）。
static CFG: OnceLock<Mutex<Settings>> = OnceLock::new();

/// 当前配置快照。
///
/// 锁中毒不 panic：release 编译开了 `panic = "abort"`，这里 unwrap 一次失手就是整个进程没了，
/// 而配置读失败本来是可以退化成默认值的小事。
pub fn cfg() -> Settings {
    let m = CFG.get_or_init(|| Mutex::new(Settings::default()));
    match m.lock() {
        Ok(g) => g.clone(),
        Err(e) => e.into_inner().clone(),
    }
}

/// 修改配置并落盘。
///
/// **写前先从磁盘重读**：设置面板是独立进程，常驻进程的托盘也会写同一份
/// `settings.yaml`；只改进程内快照再把整份写回，会把对方刚改的开关覆盖掉。
/// 读盘放在锁内，避免同进程两个写线程互相覆盖（代价是一次约 1ms 的 YAML 解析）。
pub fn update_cfg(f: impl FnOnce(&mut Settings)) {
    let m = CFG.get_or_init(|| Mutex::new(Settings::default()));
    let mut g = match m.lock() { Ok(g) => g, Err(e) => e.into_inner() };
    if let Some(disk) = read_settings_from_disk() {
        *g = disk;
    }
    f(&mut g);
    if let Some(p) = settings_path() { g.save(&p); }
}

/// 只读磁盘上的配置；文件不存在 / 解析失败时返回 None（调用方保留现有内存值）。
fn read_settings_from_disk() -> Option<Settings> {
    let p = settings_path()?;
    let text = std::fs::read_to_string(p).ok()?;
    serde_yaml::from_str::<Settings>(&text).ok()
}

/// 整份替换全局配置并落盘（设置页用；写的是同一份 settings.yaml）。
pub fn set(s: Settings) {
    let m = CFG.get_or_init(|| Mutex::new(Settings::default()));
    *match m.lock() { Ok(g) => g, Err(e) => e.into_inner() } = s.clone();
    if let Some(p) = settings_path() { s.save(&p); }
}

/// 从磁盘重新读取并替换全局配置（面板进程改完文件后，常驻进程靠它同步）。
pub fn reload() {
    if let Some(p) = settings_path() {
        if let Ok(text) = std::fs::read_to_string(&p) {
            if let Ok(v) = serde_yaml::from_str::<Settings>(&text) {
                let m = CFG.get_or_init(|| Mutex::new(Settings::default()));
                *match m.lock() { Ok(g) => g, Err(e) => e.into_inner() } = v;
            }
        }
    }
}

/// 读取配置并装载进全局；返回 (settings, 是否全新安装)。
pub fn load() -> (Settings, bool) {
    let path = settings_path();
    let mut fresh = true;
    let mut s = Settings::default();
    if let Some(p) = &path {
        if p.exists() {
            match std::fs::read_to_string(p) {
                Ok(text) if !text.trim().is_empty() => {
                    match serde_yaml::from_str::<Settings>(&text) {
                        Ok(v) => { s = v; fresh = false; }
                        Err(_) => {
                            // 损坏的文件不静默丢弃：留一份 .bad，再用默认值。
                            let _ = std::fs::copy(p, p.with_extension("yaml.bad"));
                        }
                    }
                }
                _ => {}
            }
        }
    }
    *CFG.get_or_init(|| Mutex::new(Settings::default()))
        .lock()
        .unwrap_or_else(|p| p.into_inner()) = s.clone();
    (s, fresh)
}

/// exe 所在目录下的 settings.yaml（工作目录在提权重启后不可靠，必须用 exe 目录）。
pub fn settings_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.join("settings.yaml"))
}

/// 运行目录（日志、配置都放这儿）。
pub fn run_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// 剪贴板模块的数据目录（`<exe>/clipboard`：history.json + attachments/）。
pub fn clipboard_dir() -> PathBuf {
    run_dir().join("clipboard")
}

/// 截图的默认保存目录：`%USERPROFILE%\Pictures\Zhuxia`。
///
/// 不查注册表里的 `My Pictures`：那是一个可被 OneDrive 重定向的路径，
/// 误读会让截图散到别处；`Pictures` 是所有人的默认约定，且用户随时能在设置里改。
fn default_capture_dir() -> PathBuf {
    let home = std::env::var("USERPROFILE").unwrap_or_default();
    if home.trim().is_empty() {
        return run_dir().join("captures");
    }
    PathBuf::from(home).join("Pictures").join("Zhuxia")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imgbed_defaults_and_legacy_migration() {
        let d = Settings::default().imgbed();
        assert!(d.enabled);
        assert_eq!(d.provider, "github");
        assert_eq!(d.convert, "webp");
        assert_eq!(d.quality, 82);
        assert_eq!(d.max_side, 2560);
        assert_eq!(d.copy_format, "markdown");
        assert_eq!(d.browser_source, "local");
        assert_eq!(d.hotkey, "Ctrl+Shift+U");

        // 长边：0 = 不限制，必须原样保留（不能被夹成 1 或回填默认）
        let s: Settings = serde_yaml::from_str("imgbed:
  upload:
    maxSide: 0
").unwrap();
        assert_eq!(s.imgbed().max_side, 0);
        // 超出上限按 8192 收（GitHub/S3 都不会给更大的图）
        let s: Settings = serde_yaml::from_str("imgbed:
  upload:
    maxSide: 99999
").unwrap();
        assert_eq!(s.imgbed().max_side, 8192);

        // v0 平铺字段 → providers.github + upload
        let s: Settings = serde_yaml::from_str(
            "imgbed:
  owner: me
  repo: imgs
  dir: blog
  dateDir: false
  cdn: ghproxy
  maxMb: 40
",
        )
        .unwrap();
        let d = s.imgbed();
        assert_eq!(d.provider_str("owner"), "me");
        assert_eq!(d.provider_str("repo"), "imgs");
        assert_eq!(d.provider_str("path"), "blog");
        assert_eq!(d.provider_str("cdn"), "ghproxy");
        assert_eq!(d.max_mb, 40);

        // 老路径带日期：images + yyyyMMdd → 模板变量
        let s: Settings = serde_yaml::from_str("imgbed:
  owner: me
  repo: x
  dir: images
").unwrap();
        assert_eq!(s.imgbed().provider_str("path"), "images/{yyyy}/{MM}/{dd}");

        // 未知取值收敛到默认
        let s: Settings = serde_yaml::from_str("imgbed:
  provider: ftp
  upload:
    naming: uuid
    convert: avif
").unwrap();
        let d = s.imgbed();
        assert_eq!(d.provider, "github");
        assert_eq!(d.naming, "timestamp");
        assert_eq!(d.convert, "webp");
    }

    #[test]
    fn record_has_no_duration_cap_by_default() {
        // 录制前没人知道自己要录多久，默认给一个上限只会把用户的录制掐断
        let plain = Settings {
            capture: Some(CaptureCfg { record: Some(RecordCfg::default()), ..Default::default() }),
            ..Default::default()
        };
        assert_eq!(plain.record().max_seconds, 0, "默认应当不限制");

        // 明确填了才生效
        let capped = Settings {
            capture: Some(CaptureCfg {
                record: Some(RecordCfg { max_seconds: Some(600), ..Default::default() }),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(capped.record().max_seconds, 600);

        // 负值/越界都收敛到合法区间
        let neg = Settings {
            capture: Some(CaptureCfg {
                record: Some(RecordCfg { max_seconds: Some(-30), ..Default::default() }),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(neg.record().max_seconds, 0);
    }

    #[test]
    fn prune_drops_nested_nulls_and_empty_maps() {
        let raw = "a: null\nb:\n  c: null\n  d: 1\ne: []\nf: {}\ng: 2\n";
        let v: serde_yaml::Value = serde_yaml::from_str(raw).unwrap();
        let out = prune_nulls(v).unwrap();
        let text = serde_yaml::to_string(&out).unwrap();
        assert_eq!(text.trim(), "b:\n  d: 1\ng: 2");
    }

    #[test]
    fn gesture_section_defaults_when_absent() {
        let g = Settings::default().gesture();
        assert!(g.enabled);
        assert_eq!(g.sensitivity, 100);
        assert!(!g.middle_button);
        assert!(g.disable_in_fullscreen); // 默认开：全屏应用里不收手势
        assert!(!g.disable_on_desktop);
        assert!(!g.paused);
        assert!(g.hint);
        assert_eq!(g.trail_thickness, 4);
        // rules = None 表示"没写过"，由 gesture 模块装内置默认规则
        assert!(g.rules.is_none());
        assert_eq!(g.target_window, "start-window");
        // 左右两条边默认接管（音量 / 亮度），上下不接管
        assert_eq!(g.edges.left, Some(EdgeAction::Volume));
        assert_eq!(g.edges.right, Some(EdgeAction::Brightness));
        assert_eq!(g.edges.top, None);
        assert_eq!(g.edges.bottom, None);
    }

    #[test]
    fn edges_default_on_but_can_be_turned_off() {
        // 只写了一边的开关：另一边仍按默认开
        let s: Settings = serde_yaml::from_str("gesture:\n  edges:\n    left:\n      enabled: false\n").unwrap();
        let e = s.gesture().edges;
        assert_eq!(e.left, None);
        assert_eq!(e.right, Some(EdgeAction::Brightness));

        // 显式开上边才接管
        let s: Settings = serde_yaml::from_str("gesture:\n  edges:\n    top:\n      enabled: true\n").unwrap();
        assert_eq!(s.gesture().edges.top, Some(EdgeAction::Tab));

        // action: none 等价于不接管
        let s: Settings = serde_yaml::from_str("gesture:\n  edges:\n    right:\n      action: none\n").unwrap();
        assert_eq!(s.gesture().edges.right, None);
    }

    #[test]
    fn clipboard_rich_text_is_on_by_default() {
        assert!(Settings::default().clipboard().capture_rich_text);
        let s: Settings = serde_yaml::from_str("clipboard:\n  captureRichText: false\n").unwrap();
        assert!(!s.clipboard().capture_rich_text);
    }

    #[test]
    fn gesture_section_parses_and_keeps_explicit_empty_rules() {
        let raw = "gesture:\n  sensitivity: 150\n  trail:\n    thickness: 6\n    activeColor: \"#FF0000\"\n  rules:\n    - pattern: [\"←\"]\n      action: { type: hotkey, keys: [Alt, Left] }\n";
        let s: Settings = serde_yaml::from_str(raw).unwrap();
        let g = s.gesture();
        assert_eq!(g.sensitivity, 150);
        assert_eq!(g.trail_thickness, 6);
        assert_eq!(g.trail_active_color & 0x00FF_FFFF, 0x00FF_0000);
        assert_eq!(g.rules.as_ref().map(|r| r.len()), Some(1));

        // 明确写空表 ≠ 没写：前者一条规则都不装
        let s: Settings = serde_yaml::from_str("gesture:\n  rules: []\n").unwrap();
        assert_eq!(s.gesture().rules.map(|r| r.len()), Some(0));
    }

    #[test]
    fn capture_patch_shape_survives_pruning() {        // 设置页把整段结构（含大量 None）回写后，落盘应该只剩改过的键
        let cfg = CaptureCfg { hotkey: Some("Alt+A".into()), ..Default::default() };
        let v = serde_yaml::to_value(&cfg).unwrap();
        let text = serde_yaml::to_string(&prune_nulls(v).unwrap()).unwrap();
        assert_eq!(text.trim(), "hotkey: Alt+A");
    }
}
