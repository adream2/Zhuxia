# 技术架构

## 技术栈

- **Rust**：全部业务逻辑与原生 UI，无 .NET / Electron 运行时。
- **windows crate**：Win32、GDI/GDI+、D3D11/DXGI、Media Foundation、WinRT 调用。
- **Tauri 2 + WebView2**：仅用于设置面板，独立进程、按需创建。
- **存储**：YAML 配置、JSON 运行时快照、DPAPI 加密的模块数据。

## 进程模型

```text
zhuxia.exe（常驻）
  ├─ 任务栏 / 悬浮卡片覆盖层（每屏一个 HWND）
  ├─ 全局钩子（鼠标手势、按键回显）
  ├─ 采样线程（CPU / 内存 / 磁盘 / GPU / 网络）
  └─ 托盘图标

zhuxia.exe --settings（面板，按需启动）
  ├─ Tauri 2 + WebView2
  ├─ 通过 Tauri command 读写配置
  └─ 关闭窗口即退出进程
```

- 常驻进程用命名互斥体做单实例；面板通过运行时快照与配置文件读取常驻状态。
- 面板不常驻：✕ 关闭或最小化 10 分钟后退出，WebView2 子进程随之回收。
- 常驻退出时广播退出请求，面板由 watchdog 跟随关闭。

## 模块与 crate

| crate | 职责 |
|---|---|
| `zhuxia` | 宿主：启动模块、托盘、设置面板、单实例与生命周期 |
| `zx-core` | 配置、日志、热键、显示器/窗口等公共能力 |
| `zx-ui` | GDI+ 画布、窗口、原生菜单、主题 token |
| `zx-media` | 图像编解码与 WebP |
| `taskmon` | 任务栏系统监视、采样、详情弹窗、流量控制 |
| `clipboard` | 剪贴板采集、历史、搜索、粘贴回填 |
| `capture` | 截图、标注、美化、长截图、OCR、贴图、录屏 |
| `gesture` | 鼠标手势、边缘滚动、按键回显 |
| `imgconv` | 图片格式转换与资源管理器右键菜单 |
| `appicon` | 图标提取与右键菜单 |
| `imgbed` | 多平台图床上传与浏览 |

模块之间不直接依赖：宿主注入动作钩子（截图、剪贴板、图床、开发工具、退出），
模块只通过 `zx-core` 的配置与日志交互。

## 线程与渲染

- 每个覆盖层窗口在创建线程上绘制；GDI+ 对象不跨线程。
- 手势钩子线程只做轻量判定，解析与会话状态机在独立线程；重绘通过 `PostMessage` 合并。
- 采样按配置间隔运行；网络按进程统计依赖 ETW / SRUM（管理员权限更完整）。
- 录屏把抓屏、合成、编码拆成独立线程，摄像头只取最新帧，避免队列延迟。
- 面板渲染在 WebView2 内，配置变更走 section patch，只回传本模块视图。

## 数据与存储

```text
<exe>/
  settings.yaml          用户配置（稀疏写入）
  runtime.json           常驻进程写出的运行时快照
  logs/                  按日期滚动日志
  clipboard/history.dat  DPAPI 加密的剪贴板历史 + 附件
  imgbed/                图床历史与缩略图缓存
  webview2/              面板的 WebView2 数据目录
```

- 配置写入前先读盘，避免面板与常驻进程互相覆盖。
- 剪贴板历史、图床密钥用 DPAPI 加密；日志不记录密钥与剪贴板正文。
- 截图、录屏、转换产物默认写到用户配置的目录，不覆盖源文件。

## 资源控制

- 常驻空闲时只保留采样线程与覆盖层 DIB；不加载 WebView2。
- 覆盖层画布按需创建，手势轨迹画布闲置 10 分钟释放。
- 面板关闭即退出；WebView2 启动参数关闭后台网络、组件更新与多余辅助进程。
- 图床缩略图按视口懒加载，切页释放 DOM 与位图缓存。

## 权限与网络

- 普通权限可用：任务栏监控、剪贴板、截图、手势、转换、图标、图床。
- 管理员权限增强：按进程网速（ETW/SRUM）、部分磁盘/GPU 计数、防火墙封禁。
- 出网仅限：更新检查、公网 IP 查询、图床请求、Clash 控制器；其余功能本地完成。
- 密钥只存 DPAPI 密文；面板不显示已保存密钥的明文。

## 扩展方式

新增模块只需：

1. 新建 crate，暴露 `start()` 与可选的设置页数据/命令；
2. 在 `zhuxia/src/modules.rs` 注册启动与动作；
3. 在 `zx-core` 增加配置段（可选）；
4. 在设置面板增加页面与导航项。

模块不得直接依赖其他业务 crate；跨模块动作通过宿主注入的钩子完成。

## 版本号

`major.minor.patch`，三位各有含义，写版本时按这个来（不对外解释，界面只显示数字）：

- **最右位（patch）**：`0` = 正式版；**非 0 = 预览版**。预览版可以叠加
  （`0.0.2`、`0.0.3`…），不承诺行为稳定。
- **中间位（minor）**：功能有增减就 +1；同时把最右位归 0。
- **最左位（major）**：发生不兼容的重大改变才 +1。

例：`1.0.0` 首个正式版 → `1.0.1` 第一版预览 → `1.0.2` 第二版预览 → `1.1.0` 稳定小改。

### 发版

版本号只有一处真源：`Cargo.toml` 的 `[workspace.package] version`。exe 的 VERSIONINFO
由 `crates/zhuxia/build.rs` 从 `CARGO_PKG_VERSION` 生成，界面显示的版本号也来自同一个值，
所以改版本只需要动 `Cargo.toml`（连带 `Cargo.lock` 与 `crates/zhuxia/tauri.conf.json`）。

```text
改版本 → 本地 cargo build 更新 Cargo.lock → 推送
       → Actions「发布构建」手动触发，输入版本号
       → 校验 → 编译打包 → 建 v<版本> 标签 → 发 Release
```

`release.yml` 只有 `workflow_dispatch` 一个入口，三个输入：

| 输入 | 说明 |
| --- | --- |
| `version` | 目标版本，必须与 `Cargo.toml` 完全一致；最右位 0/1 决定正式还是预览 |
| `create_release` | 是否建标签并发 Release；关掉就只产出可下载的构建产物 |
| `draft` | Release 先存草稿，人工确认后再公开 |

触发后先跑校验，任一条不满足直接失败：格式是 `x.y.z`、最右位只能是 0/1、
`Cargo.toml`/`Cargo.lock`/`tauri.conf.json` 三处版本一致、标签 `v<版本>` 不存在、
不低于已发布的最新版本。校验通过后才编译，并用 `ProductVersion` 反查 exe 内嵌版本是否等于目标版本。

产物命名全部由同一个版本号派生，和标签严格对应：

| 产物 | 文件 |
| --- | --- |
| 标签 / Release | `v<版本>`（最右位 1 自动标 prerelease） |
| 运行产物 | `zhuxia-<版本>-win-x64.zip`（exe + 许可/声明/README） |
| 源码包 | `zhuxia-<版本>-src.zip`（`git archive` 该提交，带顶层目录） |
| 校验和 | `SHA256SUMS.txt` |

归档：Artifact 保留 30 天；发布时三个文件作为 Release asset 长期保存。
