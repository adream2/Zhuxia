# 宣传页（GitHub Pages）

`site/` 是宣传页的全部源码：一个构建脚本 + 一份文案 + 一套 SVG 素材生成器。
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
site/art.py      ─┘            └─▶ _site/assets/*.svg
```

- **版本号**：只读 `Cargo.toml` 的 `[workspace.package] version`，和 exe 的 VERSIONINFO、
  界面显示同源，不存在第二处需要同步的地方。
- **下载渠道**：`gh api repos/{repo}/releases` 取发布列表，按标签最右位分流——
  `0` 归正式版、`1` 归预览版（规则见 `docs/ARCHITECTURE.md`「版本号」）。
  每个渠道取该渠道里版本最高的一个，链接指向 Release 里的
  `zhuxia-<版本>-win-x64.zip`；没有发布时显示「等待首次发布」占位。
- **更新日志**：`git log <上一个标签>..<本标签> --no-merges --pretty=%s`，
  逐版本汇总提交标题。没有标签时退化成「开发中」条目，列出当前分支最近提交。
- **视觉素材**：`site/art.py` 现场生成 SVG（任务栏监视条、截图编辑态、剪贴板面板、
  设置面板）。之所以不用真机截图：CI 上抓 GUI 要拉起 WebView2、要有桌面会话，
  失败一次整个发布就断了；按设计 token 画出来的矢量图确定性更强，版本一变素材跟着重建。
- **文案**：`site/content.py`（功能清单、卖点、页脚），不随版本变化。
- **页面**：`site/template.html` 骨架 + `site/style.css`，无外部字体与 CDN，
  响应式断点 960 / 640。

## 需要的一次性配置

1. 仓库 **Settings → Pages → Build and deployment → Source** 选 **GitHub Actions**。
   没开这一步，`configure-pages` 会直接报错提示。
2. 工作流权限用默认的 `GITHUB_TOKEN` 即可，不需要额外 Secret：
   - `pages: write` + `id-token: write` 用于部署；
   - `contents: read` 用于读发布列表。
3. 首次部署后地址是 `https://<owner>.github.io/Zhuxia/`，要自定义域名就在 Pages 设置里加。

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
  art.py          # SVG 素材（与 app 的设计 token 对齐）
  content.py      # 文案与功能清单
  template.html   # 页面骨架（{{占位符}} 由 build.py 替换）
  style.css       # 样式
```
