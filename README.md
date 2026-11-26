# 驻匣（Zhuxia）

[![release](https://img.shields.io/github/v/release/adream2/Zhuxia?include_prereleases&label=release&color=2e90fa)](https://github.com/adream2/Zhuxia/releases)
[![license](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)
![platform](https://img.shields.io/badge/platform-Windows%2010%20%2F%2011%20x64-6b7480)

Windows 上的常驻小工具集合：**一个后台进程**提供系统监视、剪贴板历史、截图标注、鼠标手势、录屏、图片格式转换、图标提取与多平台图床。纯 Rust + Win32 原生绘制，免安装，Apache-2.0。

官网（含下载、截图与更新日志）：**https://zhuxia.ztsite.com/**

## 项目定位

把高频桌面小工具收进一个低占用的常驻进程，而不是给每个功能装一套后台程序。常驻部分只有 Win32 采样线程与 GDI+ 原生绘制；设置面板是独立进程，按需启动、关闭即退出，不长期占着 WebView2。

八个功能模块各自独立开关：关掉的模块不占线程、不占钩子，也不写数据。

<img src="https://zhuxia.ztsite.com/shots/capture.png" alt="设置面板的截图工具页：截图热键、保存目录、放大镜与窗口识别开关" width="720">

<img src="https://zhuxia.ztsite.com/shots/taskbar.png" alt="任务栏监视条：CPU、内存、磁盘、GPU 与上下行网速" width="440">

更多界面见[官网](https://zhuxia.ztsite.com/#shots)。

## 核心功能

- **任务栏系统监视**：CPU、内存、磁盘、GPU、网络实时数值与详情，任务栏条 / 悬浮卡片两种形态，多屏与项目排序可调；配套「流量管理」页按进程统计上下行，并可把某个程序一键加进 Windows 防火墙阻断联网。
- **剪贴板历史**：文本、富文本、图片、文件采集，去重合并、搜索、收藏、标签，粘贴时按目标程序回填原格式。
- **截图与贴图**：选区 / 窗口 / 全屏，标注、美化（背景 / 边框 / 圆角 / 水印）、拼接、长截图、OCR、贴图置顶；选区时放大镜显示像素与颜色值，右键直接复制色值。
- **鼠标手势**：8 方向 + 多笔组合，按程序分组的规则，滚轮修饰与屏幕边缘滚动，按键回显。
- **录屏 / GIF**：框选即录，MP4 与 GIF 双格式，摄像头画中画或并排，麦克风与系统声音可选。
- **图片格式转换**：资源管理器右键就地转 JPG / PNG / WebP / GIF / BMP / TIFF / ICO，也可以在设置里选文件批量转。
- **图标提取**：从 exe / dll / lnk / msi / msix / apk / aab / ipa / app / dmg / icns / deb / rpm / AppImage 提取 PNG / ICO / 全尺寸图集，同样有批量页。
- **多平台图床**：GitHub、S3 兼容、WebDAV、自定义 HTTP；上传、浏览、历史与链接复制，截图工具条可直接上传。

设置面板里还有一页**开发工具**（JSON 格式化、编码转换、时间戳、正则测试、哈希、UUID、颜色、JWT），纯前端计算，不联网、不落盘。

## 快速开始

1. 到 [Releases](https://github.com/adream2/Zhuxia/releases) 下载 `zhuxia-<版本>-win-x64.zip`。
2. 解压到任意目录，双击 `zhuxia.exe`。免安装，配置与日志都写在 exe 同目录。
3. 首次启动会请求管理员权限（部分指标与防火墙规则需要），随后问两件事：任务栏窗口还是悬浮卡片、要不要用驻匣接管 `Win+V`（不接管也能用 `Alt+C` 打开剪贴板）。

系统要求：Windows 10 / 11 x64。设置面板依赖 WebView2 运行时（Win11 自带，Win10 一般随 Edge 已有）；缺失时会回退到原生设置页，功能不受影响。

有新版本时启动会提示，可以在「通用设置 → 启动时检查更新」关掉。

### 默认热键

| 操作 | 热键 | 说明 |
| --- | --- | --- |
| 截图 | `Alt+A` | 进覆盖层后拖动框选，Enter 进标注 |
| 剪贴板面板 | `Alt+C` | 搜索、收藏、标签、粘贴回填 |
| 顺序粘贴 | `Alt+V` | 连续复制多条后依次粘贴 |
| 面板内搜索 | `Alt+F` | 打开面板并聚焦搜索框 |
| 图床上传 | `Ctrl+Shift+U` | 上传剪贴板里的图片，链接进剪贴板 |
| 鼠标手势 | 按住右键划动 | 例：`↗` 最大化、`↙` 最小化、`→←↓` 打开剪贴板 |

热键、手势规则、边缘滚动都能在设置面板里改。覆盖层内还有一组快捷键（`Ctrl+L` 长截图、`Ctrl+O` 识别文字、`Ctrl+R` 录屏等），鼠标停在工具条图标上就能看到。

## 适用场景

- **常看系统状态**：任务栏直接显示关键指标，不用常开任务管理器；有程序偷偷跑流量时，在流量管理里按进程查、按程序封。
- **高频内容处理**：截图 → 标注 → 美化 → 复制/贴图/上传，或者录一段 GIF，都在一个入口完成。
- **减少后台数量**：一套常驻进程替代截屏、剪贴板、手势、格式转换、取图标等一堆独立小工具。
- **本地优先**：截图、剪贴板、转换、图标、录屏全部在本机完成，不需要账号。

适合 Windows 10 / 11 上频繁截图与复制粘贴的开发者、运维、设计/办公用户，以及想常驻监控但不想装大型桌面套件的人。
不适合 macOS / Linux，也不适合要求「完全零后台进程」的场景。

## 联网与隐私

- 无账号、无遥测、无广告。绝大多数功能离线可用。
- 会出网的只有四处，且都能在设置里关掉、或必须由你主动配置：启动时检查更新（GitHub Releases）、公网 IP 与公网延迟探测（设置 → 采样项目）、图床上传（配了平台才用）、Clash 控制器（填了地址才读）。
- 剪贴板历史用 DPAPI（当前用户）加密落盘，敏感内容可配置为不记录；日志不记剪贴板正文与密钥。

## 数据与卸载

- `settings.yaml`、`logs/`、剪贴板附件、图床历史都在 **exe 同目录**，不写系统目录。
- 卸载：退出程序 → 删除整个目录。想清干净的话，先在设置里关掉「开机自启」（计划任务 `Zhuxia`）和「图片转换 / 图标提取」的右键菜单开关——关开关会顺手删掉 `HKCU\Software\Classes` 下对应的键。
- 开机自启默认是关的；右键菜单默认就注册了，不关也不会影响删除目录，只是右键里会留一条点了报错的菜单项。

## 文档

- 文档目录与收录范围：`docs/README.md`（审计报告、设计稿、踩坑记录属内部资料，不入库）。
- 技术架构：`docs/ARCHITECTURE.md`；各模块说明：`docs/` 下对应文件。
- 宣传页与部署流程：`site/README.md`（页面由发布流程自动重建并部署）。
- 第三方组件与许可证声明：`THIRD_PARTY_NOTICES.md`。

## 开发与构建

- 工具链：Rust stable + MSVC 工具链（`x86_64-pc-windows-msvc`）。
- 跑起来：`cargo run -p zhuxia`（会请求管理员权限；调试时设 `ZHUXIA_NO_ELEVATE=1` 可跳过）。
- 测试：`cargo test --workspace`。出单文件：`cargo build --release --bin zhuxia` → `target/release/zhuxia.exe`（图标、版本信息、设置面板前端资源都已内嵌，运行时只要这一个文件）。
- 目录：`crates/` 一个模块一个 crate（`taskmon` / `clipboard` / `capture` / `gesture` / `imgconv` / `appicon` / `imgbed`，录屏与截图同在 `capture`），`zx-core` 管配置与日志，`zx-ui` 管 GDI+ 画布与窗口，`zhuxia` 是宿主。
- 发版：改 `Cargo.toml` 的 `[workspace.package] version` → 手动触发 Actions「发布构建」，填同一个版本号（最右位 `0` 正式、`1` 预览）→ 自动编译、打包、打标签、发 Release 并重建宣传页。

## 赞助

软件免费、无广告、无会员，也不打算改。它替我干掉了桌面上七八个小工具，如果你也觉得顺手，可以扫下面任意一个码请我喝杯咖啡——不打赏不影响使用，功能一个都不会少。

<table>
  <tr>
    <td align="center"><img src="https://zhuxia.ztsite.com/pay/wechat.png" width="180" alt="微信支付收款码"><br><sub>微信支付</sub></td>
    <td align="center"><img src="https://zhuxia.ztsite.com/pay/alipay.png" width="180" alt="支付宝收款码"><br><sub>支付宝</sub></td>
  </tr>
</table>

也可以在[官网的「打赏」区块](https://zhuxia.ztsite.com/#support)扫码。作者的其他折腾在 [ztsite.com](https://ztsite.com)——网址导航与折腾随笔，生活如此美丽，折腾从不停止。

## 反馈

统一走 GitHub Issues，只有这一条渠道（不收邮件、不建群——不在 Issue 里记的问题会随对话一起消失）：

- **问题反馈**：[新建 issue](https://github.com/adream2/Zhuxia/issues/new?template=bug_report.yml)，
  表单会要版本、复现步骤、系统与运行形态、可选日志。
- **功能建议**：[新建 issue](https://github.com/adream2/Zhuxia/issues/new?template=feature_request.yml)，
  重点是「你现在怎么凑合的」，比「加个 X」有用。
- **应用内入口**：设置面板 → 关于 → 反馈 Bug / 功能建议，版本与系统信息会自动填进链接；
  旁边的「复制诊断信息」把同样的内容放进剪贴板，贴到哪儿都行。

诊断信息只含版本、系统、权限、任务栏形态、采样间隔这类公开事实，**不含路径与用户名**。
贴日志前请自行删掉 `%USERPROFILE%` 相关的路径、密钥和不想公开的窗口标题。

提之前建议先确认是**最新版本**（设置 → 关于 看版本号），并搜一下已有 issue。

## 许可

本项目以 **Apache License 2.0** 授权，全文见 `LICENSE`，署名信息见 `NOTICE`。

- **允许**：商业与非商业使用、修改、分发、专利实施；可用于闭源项目。
- **要求**：保留版权、许可与 `NOTICE` 声明；修改过的文件需标注改动；不得使用项目名称或商标暗示 endorsement。
- 第三方组件仍按其原许可证分发（MIT / Apache-2.0 / MS-PL），义务不因本项目许可而改变，详见 `THIRD_PARTY_NOTICES.md`。
