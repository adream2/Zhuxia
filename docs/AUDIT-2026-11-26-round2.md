# 代码审计报告（第二轮）

日期：2026-11-26 · 范围：11 个 crate / 205 个 `.rs` / 约 6.4 万行 + Tauri 前端 17 个文件
方法：`cargo clippy --workspace --all-targets`、`cargo test --workspace`、依赖-使用交叉比对、
风险原语逐条查前置条件、关键路径人工阅读、Windows API 契约核对。

## 0. 基线（先说清"没什么"）

| 检查项 | 结果 |
| --- | --- |
| `cargo clippy --workspace --all-targets` | **0 告警** |
| `cargo test --workspace` | 全部通过（68 项：20+5+1+32+8+2） |
| 非测试代码 `unwrap()`/`expect()` | 15 处，**逐条核对均有前置保护**，当前不会 panic |
| `panic!` | 3 处，全在测试/调试路径 |
| Tauri capabilities | 极小面（窗口 + 自定义命令），无 fs/shell/process 权限 |
| Tauri CSP | `script-src` 回落 `default-src 'self'`，无 `unsafe-eval` |
| 压缩包解析（zip/msi/tar） | 有 `MAX_ENTRY` / `MAX_TOTAL` 闸门，不落盘解包 → 无路径穿越 |
| 凭据 | DPAPI + 熵 + 明文 `write_volatile` 清零 |
| 右键菜单注册 | exe 与 `%1` 都带引号，无命令拼接注入 |

**P0（数据损坏 / 崩溃 / 权限提升）：无。**

结论：地基确实扎实，上一轮整改基本落实。问题集中在**本次未提交的 OCR 翻译 / AI 功能**
（`ocr.rs` +394、`ocr_ai.rs` 新增 384、`ppocr.rs` +321、`overlay.rs` +394）——这批代码
没有并发与时序测试，是当前唯一的高风险区。

## 1. 上一轮结论复核（不采信、重新验证）

| 上轮声称 | 复核结果 | 依据 |
| --- | --- | --- |
| `Settings::save()` 返回 `Result` 并记录失败 | ✅ 已修 | `cfg.rs:1366`，失败写 `SAVE_ERR`，面板消费 |
| GitHub 目录翻页 | ✅ 已修 | `github.rs:154-168`，`per_page=100` + 最多 `PAGES_MAX` 页 |
| 上传重试消费 `Fail::retryable` | ✅ 已修 | `upload.rs:159-192`，3 次 + `[200,600]ms` 退避，有测试 |
| `"lib/bin 里 unwrap 一处都没有"` | ❌ **不准确** | 实测 15 处（`rules.rs:206,509-575`、`clip.rs:141-143`、`gifenc.rs:145`、`ppocr.rs:159`、`window.rs:141`、`build.rs:23`）。结论仍是"安全"，但依据错了，别拿这句话当验收标准 |
| 未用依赖清理 | ✅ 已修 | `lzma-rs` / `serde_json`(capture) / `serde`(taskmon) 已从 lock 移除 |
| 采集截断在界面可见 | ✅ 已修 | `uielem.rs:436-446` + `TRUNCATED` |

## 2. P1 — 会丢用户数据 / 会伤用户信任

### P1-1 异步翻译/AI 结果覆盖用户正在编辑的正文，且并发时结果丢失

- **位置**：`crates/capture/src/ocr.rs:118`（`ACT_SLOT`）、`:637`（`start_remote`）、
  `:836`（`WM_ACT_DONE`）、`:522`（`apply`）
- **成因**：结果回投走的是**进程级单槽** `static ACT_SLOT: Mutex<Option<(u8, Result<String,String>)>>`，
  而 `WM_ACT_DONE` 的 `WPARAM/LPARAM` 都是 0，消息本身不带任何身份信息。
- **可复现的三条路径**：
  1. **覆盖用户编辑**：点「翻译」→ 趁 3 秒网络往返在 EDIT 里改两处错字 → 结果回来时
     `apply()` 直接 `self.text = t` + `SetWindowTextW`，用户改的字**没了**，没有任何提示。
  2. **结果丢失**：点「翻译」再点「AI 整理」，两个线程在 UI 处理消息前都完成 →
     后写者覆盖先写者 → 第一条消息取到后者的 payload，第二条取到 `None`，**一条结果静默消失**。
  3. **旧结果盖新结果**：A 请求慢、B 请求快，B 先回来先显示，A 后回来再把 B 覆盖掉。
- **影响**：用户视角是"翻译/整理结果乱跳、偶尔没反应、手改的内容被吃掉"。数据丢失类。
- **修复**：
  - 消息带身份：`PostMessageW(hwnd, WM_ACT_DONE, WPARAM(seq), LPARAM(Box::into_raw(payload)))`，
    或用 `VecDeque` 取代单槽，UI 侧 `while let Some(x) = pop_front()`；
  - 每次 `show()` 递增 `generation`，worker 捕获当时的 `generation`，回投时不匹配就丢弃
    （同时解决"窗口关了又开，旧线程把结果投进新窗口"）；
  - `apply()` 前比对 `current_text()` 与发起请求时的文本，不一致就只改副标题提示
    "已翻译（原文已改动，点「替换」应用）"，不要直接覆盖。
  - `show()` / `close()` 里清空槽位。
- **验证**：给 `ocr.rs` 加一个纯逻辑单测——把上述三条路径抽成不依赖窗口的
  `fn resolve(pending: &mut Vec<(u64, ...)>, gen: u64) -> Option<...>`，或至少给
  `start_remote` 的 payload 生命周期写集成测试（现有 0 条覆盖）。

### P1-2 OCR 文本被默认发往第三方接口，无开关、无提示

- **位置**：`crates/zhuxia/src/ocr_ai.rs:149-221`、`:179`（`aidemo.youdao.com`）、`:199`（Google gtx）
- **成因**：`translate_url` 为空时代码**直接**走内置免费接口，把整段 OCR 文本（按 1000 字分块）
  以 GET 查询串 / POST body 明文交给有道和 Google。这不需要用户任何确认，也不需要开启任何开关。
- **影响**：截图 OCR 的内容经常是密码、验证码、私聊记录、内部文档、报错里的连接串/Token。
  一个"本地优先、单 exe"的截图工具把正文发给第三方，是信任问题，不只是隐私问题。
  另外 `User-Agent: Mozilla/5.0` 是伪装浏览器去打一个**演示接口**（文件自己写着"实测可用"），
  随时可能被关停或封 UA —— 用户点翻译会莫名其妙全失败。
- **修复**：
  - 首用弹一次确认：「翻译需要把这段文字发送到 ___，是否继续？（可在设置里改自建接口）」，
    结果存进 `settings.yaml`（`translate_consent`）；
  - 设置页给「翻译」一个三态：关闭 / 只用自建接口 / 允许内置免费接口；
  - 内置接口按可用性探测降级，并在失败文案里点明"这是第三方免费接口，可能随时不可用，
    建议在设置里配置自建 translateUrl"；
  - 不要伪装 UA，带真实 `Zhuxia/<version>`。
- **验证**：`translate_url` 空 + `consent=false` → `translate()` 必须返回可读的拒绝原因，
  且**不发出任何网络请求**（可用 `--ocr-ai-probe` 加一个 `--dry-run` 断言）。

### P1-3 UIA 超时线程无上限泄漏

- **位置**：`crates/capture/src/uielem.rs:449`（`STUCK`）、`:453-467`（`ensure_tx`）、`:681-700`
- **成因**：`snapshot_targets` 超时后置 `STUCK=true` 并**放弃等待**，注释写"宁可泄漏一个线程"。
  下一次调用 `ensure_tx` 里 `STUCK.swap(false)` → `recycle=true` → **再 spawn 一条新 worker**，
  老线程仍阻塞在 `walk()` 的跨进程 COM 调用里出不来。
- **影响**：目标应用一旦 UIA 无响应（Electron 卡死、远程桌面、某些游戏），
  **每按一次 Alt+A 就永久泄漏 1 条线程 + 1 套 IUIAutomation/MTA 公寓**（栈 2MB 起）。
  连续用 50 次 = 50 条僵线程、上百 MB 常驻不还，且进程里 COM 引用计数只增不减。
  单机自用不易察觉，但这是"用久了变卡、只能重启"的典型成因。
- **修复**：
  - 加 `static LEAKED: AtomicUsize`，`recycle` 前自增，超过阈值（建议 3）就
    `DISABLED.store(true)` 并在状态里告诉用户"元素识别已停用（有程序不响应 UIA），
    重启可恢复"；
  - 记录 `win_label(hwnd)` 到被放弃的线程里，方便用户定位是哪个程序；
  - 更好的做法：给 worker 加"同一 HWND 连续超时 2 次就拉黑"，后续跳过该窗口而不是整条通道。
- **验证**：写一个 mock 卡死窗口（`SendMessageW` 里 sleep 10s 的自绘窗口），
  连按 5 次 Alt+A，断言进程线程数增量 ≤ 阈值。

## 3. P2 — 功能不正确 / 明显缺陷

### P2-1 提权重启丢命令行参数，且与单实例互斥存在竞争

- **位置**：`crates/zhuxia/src/main.rs:264`、`crates/zx-core/src/win.rs:141-150`
- **成因**：`relaunch_elevated()` 用 `ShellExecuteW(..., lpParameters = null, ...)`，
  **原命令行被整条丢掉**；父进程发起后立刻 `return`，不等子进程。
- **影响**：
  1. `zhuxia.exe --open-settings`（未提权 + 已同意过提权）→ UAC 重启后参数没了 →
     面板根本没打开，用户以为点了没反应；
  2. 父进程退出与子进程 `acquire_single_instance()` 之间有窗口期，慢盘/杀软扫描时
     子进程拿到"已有实例"→ 提示"已在运行"然后退出，**结果什么都没起来**。
- **修复**：`SHELLEXECUTEINFOW` + `SEE_MASK_NOCLOSEPROCESS` 传 `lpParameters`（原参数按
  `CommandLineToArgvW` 反序列化后重新引用化，注意给含空格的参数补引号）；
  子进程 `acquire_single_instance` 失败时**重试 3 次 × 300ms** 再判定"已有实例"。
- **验证**：`zhuxia.exe --open-settings` 在未提权状态下应弹出 UAC 并在提权后打开面板。

### P2-2 `chunks()` 切块边界错误：超长 1 字符、可能产出空块

- **位置**：`crates/zhuxia/src/ocr_ai.rs:224-259`
- **成因**：切分分支里先 `cur.push('\n')` 再 `cur.push_str(&take)`，`room` 只按 `cur` 旧长度算，
  结果块长度为 `max + 1`；`room == 0` 时两个分支都不进，直接 `out.push(take(&mut cur))`，
  若此时 `cur` 为空则**产出一个空块**。
- **影响**：空块会被送进 `translate_chunk("")`，有道返回空或报错 → 整个翻译失败
  （见 P2-3 的 `break`）。现有测试 `chunks_splits_long_text` 断言 `<= 120`（max=100），
  恰好把超长当成了合法，**测试把 bug 固化了**。
- **修复**：切分前统一 `room = max - cur.chars().count() - (cur.is_empty() as usize ? 0 : 1)`，
  或干脆改成"先按行累积，超出再整体出块"的两段式；`out.push` 前过滤空串；
  测试改成 `assert!(s.chars().count() <= 100)`。

### P2-3 任一分块失败即整段失败，且串行双接口试错会被拉长到几十秒

- **位置**：`crates/zhuxia/src/ocr_ai.rs:149-172`（`break`）、`:174-221`
- **成因**：`translate_builtin` 里第一个分块失败就 `break` 并返回 `Err`，已翻好的分块被丢弃；
  每个分块先试有道（8s 超时）再试 Google（8s 超时），失败时单块成本 ~16s。
- **影响**：5000 字 OCR 结果 = 5 块，若接口不可用，用户要盯着"翻译中…"等 80 秒才拿到一个
  错误；中途没有任何进度、没有取消按钮。已有部分译文也白翻了。
- **修复**：分块结果容忍部分失败（返回"已翻译 N/M 块"+ 拼接结果）；
  先用 1 条 10 字探针确认通哪个接口，再批量走同一条；总预算（如 20s）到点就返回已有结果；
  界面上给「取消」。

### P2-4 表格识别在 UI 线程跑，大图会触发"未响应"

- **位置**：`crates/capture/src/ocr.rs:562`（`run_table`）
- **成因**：`Canvas` 握 GDI+ 句柄不是 `Send`，所以 det 只能留在 UI 线程，代码注释也承认
  "这一秒窗口不响应"。但 `ppocr` 的 det 在 4K 全屏/密集表格上会远超 1 秒。
- **影响**：Windows 5 秒判"未响应"并置灰标题栏；期间 `WM_PAINT` 也进不来，
  "表格识别中…"这行提示可能是**黑的/半截的**，用户只会以为程序挂了然后强杀。
- **修复**：`run_table` 里把像素搬到 `Vec<u8>`（已是这样）后在**工作线程**跑 det/rec
  （这两步只吃字节，不碰 GDI），只把 `Canvas` 相关的绘制留在 UI 线程；
  若短期不改，至少在阻塞前 `UpdateWindow(hwnd)` + 每 100ms `PeekMessage` 泵一次消息。

### P2-5 `http.rs` 8MB 静默截断，错误归因错误

- **位置**：`crates/imgbed/src/http.rs:113-127`
- **成因**：读循环里 `if body.len() > 8MB { break }` —— 不报错、不看 `Content-Length`，
  直接把半截 body 当完整响应返回，`status` 仍是 200。
- **影响**：上层 `serde_json::from_str` 失败 → 用户看到"响应不是 JSON（EOF while parsing）"，
  真实原因（响应太大）被掩盖；`ib_image` / 历史同步这类大响应会稳定踩到。
- **修复**：超限返回 `Err("响应超过 8 MB 上限")`；顺手读 `Content-Length` 预分配
  （现在每 16KB 一次 `extend_from_slice`，8MB 会重分配 ~9 次）。

### P2-6 每次取密钥都整份解密：`all()` 无缓存

- **位置**：`crates/imgbed/src/secrets.rs:87`（`all`）、`:105`（`get`）
- **成因**：`get()` → `all()` → `fs::read` + DPAPI 解密 + JSON 反序列化；`has()`、
  `set_keys()`、`set()`/`clear()` 全都走这条路。面板每次返回状态（`imgbed_view()`）都会调
  `set_keys()`；上传路径里每个 provider 构造都调 `get()`。
- **影响**：单次是毫秒级，但面板在前台时 `set_keys()` 随状态刷新反复触发；
  更实际的问题是 `write()` 也先 `all()`，于是"改一个字段"= 解密全表 + 再加密全表。
- **修复**：加 `OnceLock<Mutex<Option<BTreeMap>>>` 缓存，`write()` 成功后更新缓存，
  `set/clear` 复用缓存；只有首次/显式 `reload()` 才读盘。另：`all()` 里
  `String::from_utf8_lossy(&plain)` 产出的 `String` **没有清零**（只清了 `plain`），
  明文会在堆上留一份到释放——用 `Vec<u8>` 走完全程、最后 `zero` 掉。

## 4. P3 — 可维护性 / 清理

| # | 位置 | 问题 | 建议 |
| --- | --- | --- | --- |
| 1 | `ocr.rs:601-620` | `PCWSTR(zx_core::win::wide(name).as_ptr())` —— 临时量 `.as_ptr()` 传进 Win32。当前靠"`AppendMenuW` 调用内复制"侥幸成立，任何人改成延后使用就是 UAF | `let name_w = wide(name);` 先绑定，再传 |
| 2 | `ocr.rs:159` | `with_win()` 用裸指针取 `&mut`，**无重入保护**：嵌套调用即同时持有两个 `&mut`（UB），编译器完全帮不上 | debug 下加 `Cell<bool>` 断言"已在 with_win 内"，命中就 `panic`（比 UB 好） |
| 3 | `ocr.rs:501`、`:594` | `let _ = gap;` 死变量；`let _ = hwnd;` 形参未用 | 删 |
| 4 | `ocr_ai.rs:174` 等 | 内置接口无超时总预算、无取消；`get_json` 固定 8s | 见 P2-3 |
| 5 | `.github/workflows/release.yml:154` | CI 只跑 `cargo test` + `build`，**没有 clippy / fmt / audit / deny** | 加 `cargo clippy -- -D warnings`、`cargo fmt --check`、`cargo audit`（541 个包值得盯 CVE） |
| 6 | 本地环境 | `cargo test --workspace` 的 doctest 阶段因 `RUSTDOC` 指向裸工具链而失败（`can't find crate for 'std'`）；CI 上用真工具链会通过 → **本地红、CI 绿**，等于掩盖 | 固化 `RUSTC`/`RUSTDOC` 到 `.cargo/config.toml` 或 Taskfile |
| 7 | `Cargo.lock` | 541 个包，`base64`/`bitflags`/`digest`/`indexmap`/`getrandom` 等大量同名多版本；`windows` 同时 0.58 + 0.62 | 可接受（Tauri+resvg 拖进来的），但加 `cargo deny` 的 `bans` 规则防止再翻倍 |
| 8 | `painter.rs:52` | `unsafe impl Send for Canvas` 只靠注释维持"同线程创建/绘制/销毁" | 结构体里存 `creator_thread: ThreadId`，`present()`/`drop` 时断言一致（debug 下） |
| 9 | `capture/overlay.rs`（3136 行）、`clipboard/panel.rs`（2827 行） | 状态机 / 绘制 / 命中 / 输入混在一个文件，本轮又在 overlay 里加了 394 行 | 新增功能优先拆文件；先拆"元素悬停"这一块（`hover_*` / `chips` 自成一体） |
| 10 | `app.js:162` | 面板常开时 `setInterval(1200)` 轮询 `take_page_request`（每次一条 IPC），配合流量页 3s 轮询 | 常驻进程用事件推送（已有 `broadcast_reload` 通道）替代轮询 |
| 11 | `settings.yaml` 语义 | `save()` 只写非默认值、`prune_nulls` 剪空；好处是"变更清单"，代价是**无法从文件看出全貌**，排查"为什么不生效"要靠读默认值 | 文档里给一份"默认值 + 生效顺序"表；或提供 `--dump-config` 输出合并后的完整配置 |

## 5. 建议执行顺序

1. **P1-1**（结果覆盖/丢失）—— 用户已经在用这个功能，改起来是纯逻辑改动，收益最大。
2. **P1-2**（外发确认）—— 一行开关 + 一次弹窗，风险最小、性质最敏感，越早越好。
3. **P2-1**（提权丢参数 + 互斥竞争）—— 影响"点了没反应"这类最伤信任的体验。
4. **P1-3 + P2-2 + P2-3**（UIA 泄漏、分块与翻译链路）—— 一起改，因为它们都在"翻译/元素识别"这条线上。
5. **P2-4 / P2-5 / P2-6**（阻塞、截断、密钥缓存）。
6. **P3 全清**，其中 #5（CI 加 clippy/audit）应尽早，它是防止这批问题复发的唯一自动闸门。

## 6. 值得保留的做法（别在重构里弄丢）

- 窗口过程 / 钩子统一走 `guard_proc!`，panic 兜底返回**放行**（`CallNextHookEx`）而不是吞事件——
  这是"辅助工具不能把鼠标键盘锁死"的正确取舍。
- 压缩包解析一律"读进内存 + 硬上限"，不落盘解包。
- 凭据与剪贴板落盘都是"DPAPI 失败就放弃写盘"，绝不退回明文。
- `open_url` 只放行 `https://`、`open_folder` 只放行已存在目录、密钥 key 走 `secret_allowed` 白名单——
  命令面的输入校验是够的。

---

## 7. 整改记录（2026-11-26 当天完成）

按第 5 节的顺序执行完毕。`cargo clippy --workspace --all-targets` 仍是 **0 告警**，
`cargo test --workspace` **315 项全过**（新增 12 项覆盖本次改动）。

| 条目 | 状态 | 具体改动 |
| --- | --- | --- |
| P1-1 异步结果覆盖/丢失 | ✅ | `ocr.rs`：`ACT_SLOT` 单槽 → `ACT_DONE` 队列 + `ACT_SEQ` 序号 + `ACT_GEN` 窗口代号；`WM_ACT_DONE` 的 WPARAM/LPARAM 带身份；新增 `apply_remote`——等待期间正文被改过就不覆盖，挂起到 `pending`，底栏变「结果已就绪 · 点这行替换」；`take_act()` 抽出后可单测 |
| P1-2 外发明文无确认 | ✅ | `cfg` 新增 `translateConsent`（默认关）；结果窗口首次翻译弹一次确认（默认按钮=否）；`translate()` 没同意就**不发任何请求**；设置页加「允许内置翻译」开关；UA 改为 `…Zhuxia/<version>` 表明身份 |
| P1-2 附带发现 | ✅ | **AI Key 原本明文写在 settings.yaml**（同一份会被同步/备份/贴聊天）。已迁到 `imgbed/secrets.dat`（DPAPI）的 `ai.key`，面板不再回显原文只回 `aiKeySet`，旧值首次使用自动迁移并清空原字段 |
| P1-3 UIA 线程泄漏 | ✅ | `uielem.rs`：`LEAKED` 计数 + `MAX_LEAKS=3` 封顶后 `DISABLED`；`CURRENT` 记录正在遍历的窗口，超时即拉黑该 HWND；覆盖层提示里显示「元素识别已停用（有程序不响应 UI 自动化）」 |
| P2-1 提权丢参数 | ✅ | `relaunch_elevated(&args)` 改用 `SHELLEXECUTEINFOW` + 参数引用化（`quote_args`，含反斜杠/引号规则，4 项单测）；`acquire_single_instance` 改为 5×200ms 重试 |
| P2-2 切块边界 | ✅ | `chunks()` 重写为一遍 char 扫描：块长硬约束 ≤ max、不产空块、尾随空行剪掉；测试从"允许 <=120"改为断言"<=100 且不吞字符" |
| P2-3 整段失败/几十秒 | ✅ | `translate_builtin` 不再首错即 break，单块失败保留原文继续；加 20s 总预算；`PREFERRED` 记住上次通的接口，避免每块都在死接口上白等 8s |
| P2-4 表格阻塞 | ⚠️ 改方案 | 原建议"把 det 移到工作线程"**不可行**：WinML 会话是 `thread_local` 且非 `Send`，换线程要重新加载 det+rec 两个 ONNX，代价大于收益。改为 `ppocr::TABLE_DEADLINE_MS = 2500` 软截止：超时返回已识别的部分并记 warn，把最坏耗时压在 Windows「未响应」阈值（5s）之下 |
| P2-5 8MB 静默截断 | ✅ | `http.rs` 超限改为返回 `Err("响应超过 8 MB 上限")`；按 `Content-Length` 预分配，省掉 ~9 次重分配；抽出 `header_text` 复用 |
| P2-6 密钥反复解密 | ✅ | `secrets.rs` 加进程内缓存（`Mutex<Option<BTreeMap>>`），落盘成功才更新；明文改用 `from_slice` 反序列化，去掉 `from_utf8_lossy` 产生的不可清零副本 |
| P3 pick_lang 临时量 | ✅ | 先 `let name_w = wide(name);` 再传指针 |
| P3 with_win 重入 | ✅ | 加 `WIN_DEPTH` 计数，debug 下 `debug_assert` 拦住嵌套（release 不改变行为） |
| P3 死变量 | ✅ | 删 `let _ = gap;` / `let _ = hwnd;` |
| P3 CI 闸门 | ✅ 部分 | `release.yml` 加 `cargo clippy … -- -D warnings`（硬闸门）+ `rustsec/audit-check`（先 `continue-on-error` 观察）。**没加 `cargo fmt --check`**：本机 rustfmt 未安装、无法确认全仓已格式化，盲加会让 CI 直接红 |
| P3 其余 | ⏳ 未做 | `Canvas` 线程断言、`overlay.rs`/`panel.rs` 拆文件、面板轮询改推送、`--dump-config`。属长期清理，不影响正确性 |

### 仍需人工确认的两件事

1. **翻译功能的端到端手测**：`ACT_GEN`/`ACT_SEQ` 的时序只在单测里验证过，真机请试
   「点翻译 → 立刻改两字 → 等结果」应看到底栏出现「点这行替换」而不是正文被覆盖；
   「连点翻译 + AI 整理」应两条结果都能落到正文（后者覆盖前者，但不丢消息）。
2. **提权重启**：未提权状态下跑 `zhuxia.exe --open-settings`，应在 UAC 后真的打开设置页。
