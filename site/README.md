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

| 文件 | 内容 |
| --- | --- |
| `capture.png` | 设置面板 · 截图工具页（首屏） |
| `appearance.png` | 设置面板 · 外观设置页 |
| `about.png` | 设置面板 · 关于与许可页 |
| `taskbar.png` | 任务栏监视条（2× 放大） |

这几张是**真机截图**，不是脚本画的。刷新办法（在装有本程序的 Windows 上）：

```bash
# 设置面板各页：打开后 5 秒内截图，裁剪到窗口区域
zhuxia.exe --settings capture     # → capture.png
zhuxia.exe --settings appearance  # → appearance.png
zhuxia.exe --settings about       # → about.png

# 任务栏条带：截任务栏左下角 232×56，再放大 2 倍
```

注意：`--settings` 依赖常驻进程存活，先确保驻匣在运行；窗口位置固定时裁剪矩形也是固定的
（2560×1440 / 100% 缩放下为 `780,345,1770,1040`）。换分辨率就要重新量一次。

为什么不在 CI 里自动截：抓 GUI 要拉起 WebView2、要有交互式桌面会话，失败一次整个
发布就断了。所以截图文件随仓库提交，只有界面改动时才需要重截；其余内容（下载链接、
更新日志、结构化数据、og 图）仍然全自动。
