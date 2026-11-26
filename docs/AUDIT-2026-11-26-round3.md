# 代码审计报告（第三轮）

日期：2026-11-26 · 范围：11 个 crate / 约 6.5 万行 Rust + 17 个前端文件 + 7 个 Python 脚本 + 2 个工作流
方法：clippy 全量、全量测试、**对本轮新改代码做对抗性复核**、前两轮未覆盖模块（`taskmon` 采样与网络、
`gesture` 钩子、`imgconv`/`appicon` CLI、`site/`、`tools/`、CI）人工阅读、Windows API 契约逐条核对。

## 0. 基线

| 检查项 | 结果 |
| --- | --- |
| `cargo clippy --workspace --all-targets` | **0 告警** |
| `cargo test --workspace` | **318 项全过**（本轮新增 3 项） |
| release 构建 | 通过（1m44s） |
| 前端 `innerHTML` | 仅用于清空（`= ""`）与**分离的 `<textarea>`** 解实体 → 无注入面 |
| `site/*.py`、`tools/*.py` | 无 `shell=True` / `os.system` / `eval` / `pickle` / `yaml.load` |
| 自动更新 | 只查版本号 + 弹链接，**不下载不执行** → 无 RCE 面 |

**P0：无。** 本轮最重的两条都在 P1，且**都已修复**（见 §1）。

结论先行：地基仍然扎实，但这一轮暴露出一个模式性问题——**"同一件事有多份实现"**。
两套 WinHTTP、三套持久化、四份 `get(hwnd) -> &'static mut T`。单看每一份都不算错，
放在一起就会**质量分叉**：一份修好的边界，另一份没有。本轮两个 P1 都是这个模式的产物。

---

## 1. P1 — 已修复

### P1-1 `taskmon` 的 HTTP 丢弃端口，Clash 集成从未工作过

- **位置**：`crates/taskmon/src/http.rs:68`（`split_url`）、`:21`（`WinHttpConnect`）
- **成因**：`split_url` 只拆出 `(scheme, host, path)`，**不拆端口**；`get_with_headers` 里
  `let port = if secure { 443 } else { 80 };` 是写死的。于是
  `http://127.0.0.1:9090/connections` 被传成 `WinHttpConnect(session, L"127.0.0.1:9090", 80, 0)`。
- **影响**：`WinHttpConnect` 的主机名参数只认主机名（端口是它自己的第 3 个参数，不会被从名字里解析），
  所以**默认配置下 Clash 的 `/connections` 永远连不上**，流量页的 `Clash` 行始终为空。
  用户按文档把 `external-controller` 从 `9090` 改掉也一样——任何非 80 端口的地址都不可达。
- **为什么一直没被发现**：失败路径是 `get_text` 返回 `None`，`clash.rs` 里静默
  `if let Some(body)`，日志又对 `127.0.0.1` 主动降噪（`http.rs:55`）——三层静默叠加，
  现象是"这个功能没数据"，看起来像"Clash 没开"。
- **修复**：`split_url` 返回端口，按 scheme 取默认值；3 项单测钉住（9090 / 443 / 80 / 非数字端口）。

### P1-2 密钥缓存跨进程不一致（**上一轮我引入的回归**）

- **位置**：`crates/imgbed/src/secrets.rs:86`（`CACHE`）
- **成因**：上一轮为消除"每次 `get()` 都 DPAPI 解密"加了进程内缓存，**但没做失效检测**。
  常驻进程与设置面板是两个进程，共用同一个 `secrets.dat`。
- **影响**：面板里改了 token / AI Key → 落盘成功 → **常驻进程仍用旧值**，上传 401、
  AI 整理认证失败，用户只能重启程序。以前每次读盘所以天然一致，这是我加的缓存引入的。
  同一份代码里 `cfg` 有 `reload()` 处理这件事，`secrets` 没有——不对称才是根因。
- **修复**：缓存项带上磁盘戳 `(mtime, len)`，每次 `all()` 先 `stat` 再决定是否重解。
  `stat` 比 DPAPI 解密便宜几个数量级，性能收益保留，一致性恢复。
  已知边界：同一 100ns 内、内容等长的两次写会漏检——实际操作路径（DPAPI + rename）不可能做到。

---

## 2. P2 — 能力缺口与长期风险（未修）

### P2-1 图床原图上限自相矛盾：宣称 30 MB，实际 8 MB

- **位置**：`crates/imgbed/src/http.rs:103`（`MAX_BODY = 8MB`）vs `crates/zhuxia/src/panel/imgbed.rs:327`
  （`bytes.len() <= 30 * 1024 * 1024`）
- **成因**：`fetch_url` → `http::request` 共用同一个 8 MB 硬上限，而面板层明确按 30 MB 设计。
- **影响**：>8 MB 的原图（手机直出、截图拼接成品）取不回；错误信息还是
  "取原图失败（链接不可访问或需要鉴权）"——**归因完全错**，用户会去查链接权限。
  上一轮我把静默截断改成报错，只是让错误变清楚，**没有解决能力缺口**。
- **建议**：`http::request` 加一个 `max_body` 参数（`request` 保持 8 MB 默认，
  新增 `request_large` 给 32 MB），`fetch_url` 用大的；`ib_image` 的超限文案改成
  "原图 X MB，超过 30 MB 上限"。

### P2-2 元素识别被停用后无法恢复，只能重启进程

- **位置**：`crates/capture/src/uielem.rs:500`（封顶后 `DISABLED`）、`:531`
- **成因**：上一轮加了"3 次卡死即停用"来封顶线程泄漏，但 `DISABLED` 是**单向**的。
- **影响**：用户遇到一个 UIA 无响应的程序（Electron 卡死、远程桌面），连续按 3 次 Alt+A
  就把元素识别永久关掉；**关掉那个程序之后依旧不可用**，只能退出托盘再启动。
  对一个常驻一整天的托盘工具，这是"越用越坏"的体验。
- **建议**：`DISABLED` 带过期（记录停用时刻，10 分钟后自动重置 `LEAKED` 与黑名单，
  因为卡死的那个程序大概率已经关了）；设置页加一个「重新启用元素识别」按钮。

### P2-3 流量统计非原子写，且写失败后不再重试

- **位置**：`crates/taskmon/src/netstat.rs:221-235`
- **成因**：`g.dirty = false;` 在 `std::fs::write` **之前**执行，写入结果被 `let _ =` 吞掉；
  写入不是 `tmp + rename`。
- **影响**：① 断电/强杀恰好落在写盘瞬间 → JSON 截断 → `load()` 解析失败直接返回 →
  **全部历史累计流量与封禁列表静默丢失**（而防火墙规则还在，界面与系统状态不一致）；
  ② 磁盘满导致一次写失败 → `dirty` 已被清 → 后续 tick 不再尝试，直到下一次流量变化。
- **建议**：与 `cfg.save` / `secrets.write` 对齐——先写 `tmp` 再 `rename`，成功后才清 `dirty`。
  这三处应当抽一个 `zx_core::store::write_atomic(path, bytes)` 复用，别再各写一遍。

### P2-4 两套 WinHTTP 客户端，质量已经分叉

- **位置**：`crates/imgbed/src/http.rs`（253 行）、`crates/taskmon/src/http.rs`（102 行）
- **差异**：imgbed 版有状态码、`Content-Length` 预分配、限流头解析、SOCKS 明确报错、8 MB 上限；
  taskmon 版**不检查状态码**（404 的 body 也会当成功）、4 MB **静默截断**、无 header 注入防护。
- **影响**：本轮 P1-1 的端口 bug 只存在于其中一份，另一份早就拆了端口——
  这正是"两份实现"的典型代价：修一份不修另一份，bug 就藏在没修的那份里。
- **建议**：抽 `zx_core::http`（或干脆让 taskmon 依赖 imgbed 已有的 `http`），
  只保留一份。taskmon 少一个 crate 依赖，也少一处安全边界要盯。

### P2-5 面板与常驻进程同体，`--settings` 链入全部依赖

- **位置**：`crates/zhuxia/Cargo.toml`（依赖 10 个 crate + tauri + resvg）、`main.rs:18`（argv 分派）
- **现状**：`zhuxia.exe --settings` 启动的是同一个二进制：Tauri + WebView2 + capture + clipboard +
  imgconv + appicon（含 `resvg` / `zip` / `libwebm`）+ imgbed + taskmon 全部在内存里。
  注释里说"关掉面板即退出（WebView2 内存全还给系统）"——这个设计是对的，但**冷启动成本**和
  **攻击面**都按最大集合算。
- **影响**：面板冷启动要映射整个 exe 并初始化不相关的模型代码；一次性 CLI 角色
  （`--img-convert`、`--appicon`、`--imgbed`）也承担同样的加载。
- **建议**（分阶段，不必一次做完）：把 `panel` 拆成独立 `bin`，只依赖 `zx-core` + `zx-ui` +
  被调用的少量 crate；CLI 角色同理。工作区已经是多 crate 结构，拆 bin 不需要动业务代码。

### P2-6 部分翻译失败会静默混入原文

- **位置**：`crates/zhuxia/src/ocr_ai.rs:170`（`out.push(chunk.clone())`）
- **成因**：上一轮为"别因为一块失败就丢掉全部译文"改成逐块容错，失败块保留原文。
- **影响**：界面状态仍写"已翻译"，结果是**中英混杂且没有任何提示**。对"修 OCR 错字"这种
  逐字敏感的场景，用户很难判断哪一段没翻。
- **建议**：`translate` 的返回从 `Result<String, String>` 扩成
  `Result<(String, Option<String>), String>`（第二个是警告），结果窗口把警告挂在副标题；
  或者失败块加可见标记（`[未翻译]`）——前者更干净。

---

## 3. P3 — 清理与配置

| # | 位置 | 问题 | 建议 |
| --- | --- | --- | --- |
| 1 | `taskmon/src/sampling/clash.rs:32-34` | `headers` 向量构造完立刻 `let _ = &headers;`，是删代码留下的空壳 | 删 |
| 2 | `taskmon/src/runtime.rs:135` | 单实例重试 5×200ms：**正常**重复启动也要白等最多 1 秒才弹"已在运行" | 只在检测到"疑似提权重启"时才重试（例如父进程传一个 `--after-elevate`），或降到 3×100ms |
| 3 | `Cargo.toml` | 无 `rust-version`（MSRV） | 写死一个并在 CI 加 `cargo msrv`，避免依赖升级悄悄抬高门槛 |
| 4 | `Cargo.lock` | `base64` 0.21.7 + 0.23.1 并存；`windows` 0.58 + 0.62 并存 | 升直接依赖到 0.22/0.23；`windows` 两个版本由 webview2-com 拖入，短期无解，记入 `cargo deny` 的 `bans` 白名单 |
| 5 | `zx-core/src/cfg.rs:1452` | 面板 `get_state`/`patch` 每次都 `read_settings_from_disk()`；配合前端 1.2s 轮询 = 每秒 1 次 YAML 解析 | 状态缓存 + mtime 校验（与 secrets 同款做法） |
| 6 | `Cargo.toml` `[profile.release]` | `strip = true` + `lto` 且**没开 `debug`** → 线上 panic 只有消息没有可用栈 | 加 `debug = 1`，`.pdb` 与发布包分开存；体积增几个 MB，换来可诊断性 |
| 7 | 版本号 | `Cargo.toml` / `tauri.conf.json` / `Cargo.lock` 三处，靠 CI 校验兜底 | 可接受（CI 已经拦），但 `tauri.conf.json` 的 version 其实没被用到（`bundle.active=false`），删掉少一处对账 |
| 8 | `docs/` | 第二轮报告 §7 与第三轮结论有交叉引用关系，未标注 superseded | 报告归档时加"以最新一轮为准"的指针 |

---

## 4. 依赖与配置

- **541 个包**，对单 exe 工具来说偏大，主要来自 `resvg`（SVG 渲染）+ `image` 家族 + Tauri。
  都在 `appicon` / `imgconv` / 面板路径上，不是无用依赖，但值得知道"体积的大头在哪"。
- 已加 `clippy -D warnings`（硬闸门）+ `rustsec/audit-check`（观察态）。**没有 `cargo fmt --check`**：
  本机未装 rustfmt，无法确认全仓已格式化，盲加会让 CI 立即变红。装好后跑一次 `cargo fmt --all`
  再开启，是笔一次性投入。
- `permissions: contents: write` 挂在 release 工作流全局；`pages.yml` 是 `contents: read` +
  `pages: write`。范围都合理，没有越权。
- `site/build.py` 用 `subprocess.run`（非 `shell=True`）读 git 历史生成更新日志；
  `GH_TOKEN` 通过 env 传入而非命令行参数——**没有把 token 暴露在进程列表里**，这点做对了。

## 5. 架构判断（结论，不是清单）

**分层是健康的**：`zx-core`/`zx-ui` 是纯底层，业务 crate 平行互不依赖，`zhuxia` 只做聚合。
没有出现"业务 crate 互相 import"的环形依赖。

**两个真实的架构债**：

1. **同一件事多份实现**（本轮 P1-1、P2-4 的共同根因）：两套 HTTP、三套持久化
   （cfg 原子 / secrets 原子 / netstat 不原子）、四份泄漏单例。这类重复不会立刻出事，
   但它让"修好的边界"不能自动传播。建议的收敛顺序：HTTP → 持久化 → 泄漏单例。
2. **UI 与逻辑未分离**：`capture` 里 `overlay.rs` 3136 行、`clipboard/panel.rs` 2827 行，
   状态机、绘制、命中测试、输入处理混在一起。这不是"代码丑"，而是**改一个交互要在 3000 行里
   找四处状态**，是 bug 的温床。上一轮已经把 `panel.rs` 拆过 8 个模块，方向对，继续按
   "元素悬停 / 动作条 / 绘制"拆 `overlay.rs` 即可。

**进程模型**（单二进制多角色 + argv 分派）对这个产品是正确的选择：一个 exe、无安装器、
托盘常驻。代价是所有角色共享最大依赖集（P2-5），属于可接受的权衡，不是缺陷。

## 6. 前两轮结论复核

| 上轮结论 | 本轮复核 |
| --- | --- |
| P1-1 异步结果覆盖/丢失 | ✅ 复读 `ACT_DONE`/`take_act`/`apply_remote` 路径：序号 + 代号 + 快照三重校验，4 项单测覆盖 |
| P1-2 翻译外发确认 | ✅ 闸门在 `start_remote` 单一入口；`translate()` 二次校验；拒绝时不发请求 |
| P1-3 UIA 泄漏封顶 | ⚠️ 封顶有效，但**引入 P2-2**（不可恢复），见上 |
| P2-6 密钥缓存 | ❌ **引入 P1-2 回归**，已修（加磁盘戳） |
| P2-5 HTTP 截断 | ⚠️ 报错正确了，但**暴露 P2-1**（8 MB 上限与 30 MB 承诺矛盾）——上一轮只治了症状 |
| P2-3 翻译容错 | ⚠️ 行为达成，但**引入 P2-6**（静默混原文） |
| `clippy -D warnings` 已进 CI | ✅ 已确认写在 `release.yml` 的 build job |

**一句话总结**：上一轮把"会丢数据的路径"堵住了，但引入了两处新的边界问题（缓存一致性、
不可恢复的停用），其中一个（端口/P1-1）是老代码里躺着的、跟我的改动无关。三轮下来
`P0` 始终为空，说明**风险主要不来自"会不会崩"，而来自"边界是否在多份实现里都一致"**。

## 7. 建议执行顺序

1. **P2-1**（8 MB vs 30 MB）——一行参数化，用户可感知的能力缺口。
2. **P2-2**（停用可恢复）——给它一个 10 分钟自动过期，成本极低。
3. **P2-3 + P2-4**（持久化与 HTTP 收敛）——这两个是"根因治理"，做完能消掉一整类问题。
4. **P2-6**（部分翻译提示）——改返回类型，波及 2 个调用点。
5. **P2-5**（拆 bin）——收益明确但工作量最大，放在没有其他改动时做。
6. P3 逐条清；其中 #6（`debug = 1`）建议尽早，因为它是"下次出问题能不能查"的前提。

---

## 8. 整改记录（2026-11-26 当天完成）

按 §7 的顺序执行。基线：`clippy --workspace --all-targets` **0 告警**、
`cargo test --workspace` **318 项全过**、release 构建通过（exe 13.7 MB + 独立 34 MB `.pdb`）。

| 条目 | 状态 | 具体改动 |
| --- | --- | --- |
| P2-1 8 MB / 30 MB 矛盾 | ✅ | `http::request_limited(..., max_body)`；`request()` 保持 8 MB 默认；`imgbed::fetch_url_result` 走 32 MB 并把**真实原因**带出来（超限 / HTTP 状态码 / 空响应）。`ib_image` 不再自己判 30 MB，文案改成"原图 X.X MB，超过 30 MB 上限" |
| P2-2 停用不可恢复 | ✅ | `uielem`：`DISABLED_AT` + `DISABLED_TTL = 10 min`，到点自动 `reset()`；`pub fn reset()` / `is_disabled()`；设置页在停用时多出一行「元素识别已停用」+「立即恢复」按钮（`uielem_reset` 命令） |
| P2-3 持久化不统一 | ✅ | 新增 `zx_core::store::write_atomic`（tmp + rename，跨卷回退 copy，3 项单测）；`netstat::save` 改用它，且 **`dirty` 只在写成功后清**，失败保留待下次；`cfg.save` / `secrets.write` 一并改用同一实现；`netstat::load` 解析失败不再静默 |
| P2-4 两套 HTTP | ✅ | 实现搬到 `zx_core::http`（全仓库唯一一份）；`imgbed::http` 退化成 `pub use`，调用点零改动；`taskmon::http` 变成 60 行薄封装并**补上状态码检查**；`imgbed`/`taskmon` 的 `WinHttp` 特性随之删除 |
| P2-6 部分翻译静默 | ✅ | `TranslateHook` → `Result<(String, Option<String>), String>`；`translate_builtin` 返回 `Some("2 段未翻译，保留原文")`；结果窗口把这句挂在副标题（`已翻译 · 2 段未翻译，保留原文`）；CLI 探活单独打一行 `WARN:` |
| P3-1 clash 死码 | ✅ | 删掉 `let mut headers …; let _ = &headers;` |
| P3-2 单实例白等 | ✅ | 拆分：普通启动 3×120 ms（肉眼无感），`--after-elevate`（由 `relaunch_elevated` 自动追加）才 8×200 ms |
| P3-6 无调试信息 | ✅ | `[profile.release] debug = 1`；CI 新增「上传符号包」并把 `.pdb` 作为独立 Release 资产（不进用户包） |
| P3-3 MSRV | ⏸ 未做 | 需要用 `cargo msrv`（本地未安装）实测最小编译版本，**不猜**——声明偏高会误伤能编译的旧工具链 |
| P3-4/5/7/8 | ⏸ 未做 | 依赖降重（`base64` 0.21→0.23）、面板状态缓存、`tauri.conf.json` 版本字段、报告归档标记 |
| P2-5 拆 bin | ⏸ 未做 | 见下 |

### 为什么 P2-5（面板独立成 bin）没做

收益清楚（面板冷启动少加载 8 个 crate + 缩攻击面），但它会动：`tauri.conf.json` 的
`frontendDist`、`build.rs` 的资源与图标、capabilities 的 `windows` 匹配、release 的产物与
校验、以及 `--settings` 的启动路径。**这些都属于"改完必须真机跑一遍面板"的改动**，
而且一旦漏项，表现是"设置打不开"而不是编译错误。放在一个有完整一轮手测时间的窗口做，
比现在顺手改掉安全。建议作为独立任务，先补一条"面板能冷启动并读完所有页面"的冒烟测试。

### 仍需人工确认

1. **Clash 集成**（P1-1 修复）：把 `external-controller` 指到 `127.0.0.1:9090`，
   流量页应出现带 `Clash` 标签的行。这条以前从没工作过，属"新功能上线"级别的手测。
2. **面板改 token 后常驻进程即时生效**（P1-2 修复）：面板改 github token → 不回退 →
   直接在托盘菜单里上传一张图，应成功。
3. **元素识别恢复**：对着一个 UIA 卡死的程序连按 3 次 Alt+A → 见「已停用」提示 →
   关掉该程序 → 设置页点「立即恢复」→ 再按 Alt+A 应恢复高亮。
