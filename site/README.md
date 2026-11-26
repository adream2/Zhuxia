# 宣传页（GitHub Pages）

`site/` 是宣传页的全部源码：一个构建脚本 + 一份文案 + 一组真机截图。
页面在发布时由 CI 自动重建并部署，日常不需要手工改 HTML。

## 触发方式

| 触发 | 入口 | 做什么 |
| --- | --- | --- |
| 发版 | `release.yml` 的 `site` job | 建好标签、发完 Release 后，用新版本重建页面并部署 |
| 手动 | Actions →「宣传页」→ Run workflow | 首次上线、或改完站点想立刻看效果 |
| 改站点 | push 到 `main` 且改动在 `site/**` | 自动重建部署 |

`release.yml` 里的 `site` job 只在 `create_release=true 且 draft=false` 时执行。
用草稿发布的话，等人工点了「发布」之后再手动跑一次「宣传页」。

> 为什么不用 `on: release: published`：Release 是用默认 `GITHUB_TOKEN` 创建的，
> GitHub 不会因为 `GITHUB_TOKEN` 的动作再触发另一个 workflow。所以这一步必须
> 写在发布流程内部。

## 功能演示区块（#demos）

页面中间那块「先动手，再决定要不要装」不是视频，也不是截图，是**纯 CSS + 内联 JS 的
可交互演示**，12 个标签页对应 8 个模块的全部能力：

| 标签 | 能干什么 |
| --- | --- |
| 任务栏监视 | 数字每 900ms 走一次，趋势柱状图跟着长；点任一项展开详情 |
| 剪贴板历史 | 搜索框真的过滤，点条目切选中态 |
| 截图与贴图 | 点「框选」出选区动画并生成贴图，贴图可**拖动**、双击关闭 |
| 元素识别 | 悬停高亮元素，滚轮在 元素 → 所在行 → 内容区 → 窗口 之间换层级 |
| OCR 与翻译 | 扫描动画 + 逐行出结果，再点「翻译」切成英文 |
| 表格识别 | 转成 Markdown 源码，按显示宽度对齐（中文双宽单独算） |
| 标注 | 矩形 / 箭头 / 马赛克三种工具，坐标按卡片真实矩形算 |
| 鼠标手势 | 真的能画：按住拖一笔，松手按方向判定并高亮对应动作 |
| 录屏 / GIF | 红框 + 计时在跑，停止后算出 GIF 体积 |
| 格式转换 | 点格式跑进度条，算出体积与节省比例（原图体积不被覆盖） |
| 图标提取 | 点文件渲染 16→256 六档图标，尺寸按比例缩 |
| 图床上传 | 选平台 → 进度 → 出现链接并复制到剪贴板 |

约束（改的时候别破坏）：

- **改完必须跑冒烟测试**：`python tools/site_smoke.py`。它会依次切 12 个页签、
  触发真实交互，然后打印 `key=value`。全部 true / 非 0 才算过。
  这一条不是走形式：12 个演示共用一个 IIFE，顶层 `var` 重名会**静默**互相覆盖——
  之前手势里的 `pad`（元素）覆盖了表格的 `pad()` 函数，表现就是"点转表格什么也没有"；
  手势里的 `box` 覆盖了元素高亮框，表现是"元素识别不响应"。冒烟测试就是为了逮这个。
- **不引外部依赖**：没有字体、没有图片、没有 CDN，全部 CSS/JS/HTML；
- 演示脚本内联在 `template.html` 的 script 标签里，`build.py` 原样带过去；
  样式在 `style.css` 末尾的「功能演示」分区；
- **模板里不能出现双花括号开头的片段**：`verify()` 会把大写占位符当成没替换直接报错；
- 每个 `pane` 必须带上 `.demo-split`（左演示 / 右文案），窄屏会自动堆叠；
- 自动轮播 8 秒一次，鼠标移入或手动点击后停止——别去掉，
  首屏停留时间不够的人看不到后面的演示；
- 演示里的滚轮只在自己那一块拦（`preventDefault` 绑在 mock 上），页面滚动不受影响；
- 标注、截图贴图的坐标全部由 JS 按真实矩形算，改卡片尺寸不用动代码，改结构要回归一次；
- 桌面预览用无头 Edge 截图核对：

```bash
"/c/Program Files (x86)/Microsoft/Edge/Application/msedge.exe" \
  --headless=new --disable-gpu --hide-scrollbars --force-device-scale-factor=1 \
  --virtual-time-budget=2500 --window-size=1280,3400 \
  --screenshot=out.png "file:///E:/WorkBuddy/Zhuxia/_site/index.html"
```

  注意无头窗口有最小宽度（设 430 实际视口是 477），窄屏核对要用 `scrollWidth`
  判断有没有真溢出，别只看截图裁切。

## 使用场景区块（#scenes）

`template.html` 里三张卡片，每张是「场景标签 + 标题 + 四步流程 + 一句收尾」。
步骤用 CSS 计数器画编号圆点和竖线，没有图片。文案要和演示区的功能对得上——
改了一条流程，记得回头看一眼对应的演示还在不在。

## 各环节实现

```text
Cargo.toml 版本号 ─┐
gh api releases  ─┼─▶ site/build.py ─▶ _site/index.html ─▶ upload-pages-artifact ─▶ deploy-pages
git log 提交历史 ─┤            │
site/shots/*.png ─┘            ├─▶ _site/shots/*.png
site/art.py      ─┘            └─▶ _site/assets/og.png
```

- **版本号**：只读 `Cargo.toml` 的 `[workspace.package] version`，和 exe 的 VERSIONINFO、
  界面显示同源，不存在第二处需要同步的地方。
- **下载渠道**：`gh api repos/{repo}/releases` 取发布列表，按标签最右位分流——
  `0` 归正式版、`1` 归预览版（规则见 `docs/ARCHITECTURE.md`「版本号」）。
  每个渠道取该渠道里版本最高的一个，链接指向 Release 里的
  `zhuxia-<版本>-win-x64.zip`；没有发布时显示「等待首次发布」占位。
- **更新日志**：`git log <上一个标签>..<本标签> --no-merges --pretty=%s`，
  逐版本汇总提交标题。没有标签时退化成「开发中」条目，列出当前分支最近提交。
- **界面截图**：`site/shots/*.png`，都是从真实进程里截的图（见下节）。
  早先这里画过一套 SVG 示意图，好看但和实际界面对不上，已经全部删掉——
  截图必须和软件长得一样，宁可朴素也别失真。
- **文案**：`site/content.py`（功能清单、卖点、页脚），不随版本变化。
- **页面**：`site/template.html` 骨架 + `site/style.css`，无外部字体与 CDN，
  响应式断点 960 / 640。

## 需要的一次性配置

1. 仓库 **Settings → Pages → Build and deployment → Source** 选 **GitHub Actions**。
   没开这一步，`configure-pages` 会直接报错提示。
2. 工作流权限用默认的 `GITHUB_TOKEN` 即可，不需要额外 Secret：
   - `pages: write` + `id-token: write` 用于部署；
   - `contents: read` 用于读发布列表。
3. 自定义域名（当前是 `zhuxia.ztsite.com`）见下一节，**域名本身不靠仓库文件生效**。

## 自定义域名

线上域名：**https://zhuxia.ztsite.com/**（`adream2.github.io/Zhuxia/` 会 301 过来）。

域名有两个落点，别混：

| 落点 | 位置 | 说明 |
| --- | --- | --- |
| 页面地址（生效的那份） | 仓库 **Settings → Pages → Custom domain** | 用 Actions 发布时 GitHub **忽略**产物里的 CNAME 文件，域名存在这里 |
| DNS | 域名商后台 | `CNAME  zhuxia  →  adream2.github.io`（不要带仓库名） |

所以"部署后域名丢失"在 Actions 发布下不会发生：部署只换产物，不碰 Pages 设置。
真正会错的是**页面里的绝对地址**——那由 `SITE_DOMAIN` 决定。

`SITE_DOMAIN` 写在工作流顶层（唯一来源）：

```yaml
env:
  SITE_DOMAIN: zhuxia.ztsite.com
```

`.github/workflows/pages.yml` 和 `.github/workflows/release.yml` 的 site job 各有一份，
**改域名要同时改这两处**。构建脚本据此产出：

- `SITE_DOMAIN` 优先于 `configure-pages` 的 `base_url`：后者读的是 Pages 设置，
  没保存成功时会给出 `github.io` 甚至空串，照抄就会把 canonical 指到跳转前的旧地址；
- 产物根目录写一份 `CNAME`（纯域名、无协议、无斜杠）。Actions 发布下它被忽略，
  留着是为了"万一改成从分支发布"不用再补；
- 绑了域名后 `verify()` 会拦住两种情况：产物里还有 `github.io` 的绝对地址、
  `CNAME` 内容不是纯域名。

上线后自检（各返回 200 且内容为域名即正确）：

```bash
curl -s https://zhuxia.ztsite.com/ | grep -o 'rel="canonical" href="[^"]*"'
curl -s https://zhuxia.ztsite.com/sitemap.xml | head -4
curl -sI https://adream2.github.io/Zhuxia/ | grep -i location   # 应为 301 → 域名
```

HTTPS 证书由 GitHub 自动签发，Pages 设置里勾上 **Enforce HTTPS**；
若域名刚解析好，证书可能要等几分钟到一小时。

## 本地预览

```bash
python site/build.py --out _site      # 生成到 _site/
python -m http.server -d _site 8787   # 打开 http://127.0.0.1:8787/
```

本地没有 `GH_TOKEN` 时，发布列表退化成用 git 标签，链接按命名规则拼出来，
所以没发过版的仓库也能完整预览。

## 目录

```text
site/
  build.py        # 入口：取数据 → 生成素材 → 渲染 HTML
  art.py          # 只生成 og.png（标准库写 PNG）
  shots/          # 真机截图（见下节）
  pay/            # 打赏二维码原图（wechat.png / alipay.png，400×400，原样拷进产物 pay/）
  content.py      # 文案与功能清单
  template.html   # 页面骨架（{{占位符}} 由 build.py 替换）
  style.css       # 样式
```

`pay/` 是页面里唯一"不是生成出来的"图片目录：换收款码直接替换这两个文件即可，
文件名保持小写（Linux CI 区分大小写），`verify()` 会检查它们在产物里存在且非空。

## SEO / GEO

页面除了给人看，也要让搜索引擎和生成式引擎读得懂。以下产物全部在构建时生成：

| 产物 | 作用 |
| --- | --- |
| title / description / canonical | 基础收录信息，canonical 用 SITE_URL 拼绝对地址 |
| Open Graph + Twitter Card | 分享卡片，配 `assets/og.png`（1200×630） |
| JSON-LD | `SoftwareApplication` / `WebSite` / `FAQPage` 三段结构化数据 |
| `robots.txt` | 显式放行 GPTBot、ClaudeBot、PerplexityBot、Google-Extended 等抓取器 |
| `sitemap.xml` | 单页站点地图，lastmod = 构建日期 |
| `llms.txt` | 给大模型看的站点摘要（llmstxt.org 约定）：基本信息、功能、下载、版本规则、FAQ |
| `feed.xml` | 每个版本一条 RSS，更新日志的机器可读版本 |
| `site.webmanifest` | 名称、主题色、图标，支持添加到桌面 |

GEO 的写法约定：每条 FAQ 都是「一句问 + 一段能直接摘走的事实陈述」，不写「可能」「或许」；
首屏摘要一句话讲清平台、功能范围、技术栈与授权，方便被整段引用。

`og.png` 由 `art.og_image()` 用标准库现场生成（渐变 + 网格 + 居中的应用图标），
不依赖 Pillow，也不为中文标题往仓库里塞字体——标题交给 `og:title`。

## 站点图标

直接用软件自己的图标，不另画一套，避免与 exe 图标不一致：

| 用途 | 文件 |
| --- | --- |
| 浏览器标签（多尺寸） | `favicon.ico` ← `assets/icons/zhuxia.ico` |
| 标签页 / 导航栏 | `assets/icon-32.png`、`assets/icon-64.png` |
| apple-touch-icon | `assets/icon-256.png` |
| manifest / 桌面图标 | `assets/icon-256.png`、`assets/icon-512.png` |

## 站点根地址

`SITE_DOMAIN`（绑了自定义域名）> `SITE_URL`（带协议的完整地址）> 按仓库推断
`https://<owner>.github.io/<repo>/`。它决定 canonical / og:url / og:image /
sitemap / robots / feed / llms.txt 里的绝对地址，**只在这一个函数里推导**，
各文件都只是把它拼进去，没有第二份硬编码。

`SITE_URL` 只认 `http(s)://` 开头，别的值（空串、`/`）一律回退到推断值——
CI 里 `configure-pages` 取不到配置时会给出这些值，照抄会把整页地址变成根路径。

## 界面截图（site/shots/）

页面配图全部是**真机截图**，脚本不画图。文件名与 `site/content.py` 的 `SHOTS` 一一对应：

| 文件 | 内容 | 尺寸 |
| --- | --- | --- |
| `annotate.png` | 标注态：工具条 + 矩形 / 箭头 / 马赛克（**首屏**） | 1216×530 |
| `uielem.png` | 元素识别悬停：类型 / 名称 / 像素尺寸 + 放大镜与色值 | 1195×410 |
| `ocr.png` | 文字识别窗口：可编辑正文 + 底栏「表格 / 翻译 / AI 整理」 | 1273×506 |
| `clipboard.png` | 剪贴板面板：分栏、搜索、缩略图列表 | 460×580 |
| `imgbed.png` | 图床管理：拖放 / 粘贴、格式与长边、平台配置 | 1002×682 |
| `taskbar.png` | 任务栏监视条（2× 放大） | 464×112 |
| `appearance.png` | 设置面板 · 外观设置页 | 990×695 |
| `about.png` | 设置面板 · 关于与许可页 | 990×695 |
| `capture.png` | 设置面板 · 截图工具页（仅作首屏兜底，不在图集里） | 990×695 |

前五张是"用起来"的实拍，排序也按这个优先级——产品页此前四张里三张是设置页，
缺的正是这些画面。

**缺图不拦构建**：`render_shots()` 跳过没有文件的槽位并打印提示，可以先拍一张上一张。

**首屏自动升级**：`HERO_SHOTS` 按 `selection.png` → `capture.png` 取第一张存在的。
拍好 `selection` 丢进去，首屏大图与结构化数据的 `screenshot` 都会自动换掉，不用改代码。

### 三张设置页的拍法

```bash
# 先确保驻匣在运行（--settings 依赖常驻进程存活），打开后 5 秒内截图并裁到窗口区域
zhuxia.exe --settings capture     # → capture.png
zhuxia.exe --settings appearance  # → appearance.png
zhuxia.exe --settings about       # → about.png

# 任务栏条带：截任务栏左下角 232×56，再放大 2 倍
```

窗口位置固定时裁剪矩形也固定（2560×1440 / 100% 缩放下为 `780,345,1770,1040`），
换分辨率要重新量。

### 五张交互图的拍法（重拍时看这里）

这五张是交互中的状态，只能人工操作后截屏（`Win+Shift+S` 或本工具的 `Alt+A`），
拿到图按上表文件名丢进 `site/shots/`。规格：宽 1000–1280，窗口裁干净，
不加外框和阴影（页面自己会套）。

命名有个历史包袱：元素识别那张最初按"选区"叫 `selection.png`，实际内容是悬停识别，
入库时改成了 `uielem.png`；`content.py` 的 `HERO_SHOTS` 也因此按 `annotate → uielem → capture`
的顺序取首屏。

### 隐私红线（每次重截都要过一遍）

- 不要出现 `C:\Users\<用户名>\…` 这类路径 —— `capture.png` 现在就带着，应重截。
- 不要出现显示器型号（`appearance.png` 里的 `P27QBD-RG`）。
- 剪贴板、图床、OCR 三张最容易翻车：历史条目里可能有真实文件名、仓库名、bucket 名、
  token、聊天内容。发之前逐张放大看一遍。
- 用当前版本的界面 —— 现有几张还是 `v0.0.1`，与站点显示的版本号对不上。

刷新完在本地跑一次看效果：

```bash
python3 site/build.py --out _site
```

为什么不在 CI 里自动截：抓 GUI 要拉起 WebView2、要有交互式桌面会话，失败一次整个
发布就断了。所以截图文件随仓库提交，只有界面改动时才需要重截；其余内容（下载链接、
更新日志、结构化数据、og 图）仍然全自动。
