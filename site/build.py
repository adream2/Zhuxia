#!/usr/bin/env python3
"""驻匣宣传页构建脚本。

输入（全部来自仓库本身，没有人工维护的数据文件）：
  · 版本号        → Cargo.toml 的 [workspace.package] version
  · 发布列表/日期  → `gh api .../releases`（有 GH_TOKEN 时）或 git 标签
  · 更新日志       → git 提交历史（相邻标签之间的提交标题）
  · 视觉素材       → site/art.py 现场生成 SVG + OG 位图
  · 站点图标       → assets/icons/ 下的软件图标（ico 与各尺寸 png）

输出：_site/ —— index.html、style.css、图标、素材，以及给搜索引擎/生成式引擎的
robots.txt、sitemap.xml、llms.txt、feed.xml、site.webmanifest。

用法：
  python3 site/build.py [--out _site]
  SITE_DOMAIN=zhuxia.ztsite.com python3 site/build.py     # 绑了自定义域名（CI 里就这么传）
  SITE_URL=https://example.com/ python3 site/build.py     # 显式指定站点根地址（带协议）
"""

from __future__ import annotations

import argparse
import html
import json
import os
import re
import shutil
import struct
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path
from urllib.parse import urlsplit

sys.path.insert(0, str(Path(__file__).resolve().parent))

import art  # noqa: E402
import content  # noqa: E402

ROOT = Path(__file__).resolve().parent.parent
SITE = Path(__file__).resolve().parent
ICONS = ROOT / "assets" / "icons"


# ---------------------------------------------------------------- 基础工具


def run(cmd: list[str], default: str = "") -> str:
    try:
        out = subprocess.run(
            cmd, cwd=ROOT, capture_output=True, text=True, encoding="utf-8", errors="replace"
        )
    except OSError:
        return default
    return out.stdout.strip() if out.returncode == 0 else default


def esc(s: str) -> str:
    return html.escape(s, quote=True)


def workspace_version() -> str:
    """唯一真源：Cargo.toml 的 [workspace.package] version。"""
    text = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    m = re.search(r"^\[workspace\.package\]\s*(.*?)(?=^\[)", text, re.S | re.M)
    block = m.group(1) if m else text
    m2 = re.search(r'^version\s*=\s*"([^"]+)"', block, re.M)
    if not m2:
        raise SystemExit("Cargo.toml 里找不到 [workspace.package] version")
    return m2.group(1)


def repo_slug() -> str:
    env = os.environ.get("GITHUB_REPOSITORY")
    if env:
        return env
    url = run(["git", "remote", "get-url", "origin"])
    m = re.search(r"github\.com[:/](.+?)(?:\.git)?$", url)
    return m.group(1) if m else "adream2/Zhuxia"


def site_url(slug: str) -> str:
    """站点根地址（带结尾斜杠）。canonical / og:url / sitemap 都要绝对地址。

    优先级：
      1. `SITE_DOMAIN`（自定义域名，如 `zhuxia.ztsite.com`）——绑了域名就一定用它：
         GitHub 会把 `*.github.io/<repo>/` 301 到自定义域名，canonical 指过去才算规范；
      2. `SITE_URL`（带协议的完整地址，本地调试/特殊场景用）；
      3. GitHub Pages 归一化地址：用户页 `owner.github.io`、项目页 `owner.github.io/repo`。
    非 http(s) 的值一律当无效（CI 里 configure-pages 取不到配置时会给出空串或 "/"，
    照抄会把整页的相对地址变成 "/"，比直接回退到推断值更糟）。
    """
    raw = os.environ.get("SITE_DOMAIN", "").strip()
    if raw:
        host = (urlsplit(raw if "://" in raw else "https://" + raw).hostname or "").lower()
        if host:
            return f"https://{host}/"

    env = os.environ.get("SITE_URL", "").strip()
    if env.startswith(("http://", "https://")):
        return env if env.endswith("/") else env + "/"

    owner, _, name = slug.partition("/")
    if name.lower() == f"{owner.lower()}.github.io":
        return f"https://{owner}.github.io/"
    return f"https://{owner.lower()}.github.io/{name}/"


def custom_domain(site: str) -> str:
    """站点地址里的自定义域名；还是 `*.github.io` 就返回空串（说明没绑域名）。"""
    host = (urlsplit(site).hostname or "").lower()
    if not host or host.endswith(".github.io"):
        return ""
    return host


def check_pages_host(domain: str) -> None:
    """CI 里拿 configure-pages 读到的 host 跟声明的域名对账。

    为什么要硬失败：canonical 用 `SITE_DOMAIN` 拼，实际访问域名来自仓库
    Settings → Pages；两边一旦不同，搜索引擎看到的地址就不是用户打开的地址。
    域名还没在 Pages 设置里生效（读到 `*.github.io`）只告警——那是刚绑域名的
    过渡态，等设置生效后下一次部署自然对齐。
    """
    host = os.environ.get("PAGES_HOST", "").strip().lower()
    if not host or not domain:
        return
    if host.endswith(".github.io"):
        print(
            f"警告：Settings → Pages 里的自定义域名还没生效（读到 {host}），"
            f"产物仍按 {domain} 生成",
            file=sys.stderr,
        )
        return
    if host != domain:
        raise SystemExit(
            f"域名不一致：工作流 SITE_DOMAIN={domain}，Settings → Pages 的域名={host}。\n"
            "改成同一个再部署，否则 canonical 与实际访问域名对不上。"
        )


def parse_version(tag: str) -> tuple[int, int, int] | None:
    m = re.fullmatch(r"v?(\d+)\.(\d+)\.(\d+)", tag.strip())
    return (int(m.group(1)), int(m.group(2)), int(m.group(3))) if m else None


def channel_of(ver: tuple[int, int, int]) -> str:
    """最右位 0=正式、非 0=预览（docs/ARCHITECTURE.md「版本号」）。"""
    return "stable" if ver[2] == 0 else "preview"


def vstr(ver: tuple[int, int, int]) -> str:
    return ".".join(str(x) for x in ver)


# ---------------------------------------------------------------- 数据采集


def releases_from_api(slug: str) -> list[dict]:
    raw = run(["gh", "api", f"repos/{slug}/releases?per_page=100"])
    if not raw:
        return []
    try:
        data = json.loads(raw)
    except json.JSONDecodeError:
        return []
    out = []
    for r in data:
        ver = parse_version(r.get("tag_name", ""))
        if not ver:
            continue
        assets = {a.get("name", ""): a.get("browser_download_url", "") for a in r.get("assets", [])}
        sizes = {a.get("name", ""): int(a.get("size") or 0) for a in r.get("assets", [])}
        out.append(
            {
                "tag": r["tag_name"],
                "ver": ver,
                "name": r.get("name") or r["tag_name"],
                "date": (r.get("published_at") or "")[:10],
                "draft": bool(r.get("draft")),
                "url": r.get("html_url", ""),
                "assets": assets,
                "sizes": sizes,
            }
        )
    return out


def releases_from_tags() -> list[dict]:
    tags = [t for t in run(["git", "tag", "-l", "v*"]).splitlines() if parse_version(t)]
    out = []
    for tag in tags:
        ver = parse_version(tag)
        date = run(["git", "log", "-1", "--format=%ad", "--date=short", tag])
        out.append(
            {
                "tag": tag,
                "ver": ver,
                "name": tag,
                "date": date,
                "draft": False,
                "url": "",
                "assets": {},
                "sizes": {},
            }
        )
    return out


def collect_releases(slug: str) -> list[dict]:
    rel = releases_from_api(slug)
    if not rel:
        rel = releases_from_tags()
    rel = [r for r in rel if not r["draft"]]
    rel.sort(key=lambda r: r["ver"], reverse=True)
    return rel


def commits_between(prev_tag: str | None, tag: str, limit: int = 14) -> list[str]:
    rng = f"{prev_tag}..{tag}" if prev_tag else tag
    raw = run(["git", "log", rng, "--no-merges", "--pretty=%s", f"-n{limit}"])
    return [ln.strip() for ln in raw.splitlines() if ln.strip()]


def head_commits(limit: int = 8) -> list[str]:
    raw = run(["git", "log", "--no-merges", "--pretty=%s", f"-n{limit}"])
    return [ln.strip() for ln in raw.splitlines() if ln.strip()]


def release_notes(rel: list[dict]) -> list[tuple[dict, list[str]]]:
    """[(release, 该版本的提交标题)]，更新日志、RSS、llms.txt 共用。"""
    out = []
    for i, r in enumerate(rel):
        prev_tag = rel[i + 1]["tag"] if i + 1 < len(rel) else None
        out.append((r, commits_between(prev_tag, r["tag"])))
    return out


# ---------------------------------------------------------------- 图标


ICONS_SVG = {
    "monitor": '<rect x="2.5" y="4" width="19" height="13" rx="2"/><path d="M7 21h10M12 17v4"/>'
    '<path d="M6 12l3-4 3 3 3-5 3 6"/>',
    "clipboard": '<rect x="6" y="4" width="12" height="17" rx="2"/>'
    '<path d="M9.5 4V3h5v1"/><path d="M9 10h6M9 14h6M9 18h3"/>',
    "capture": '<path d="M4 8V5a1 1 0 011-1h3M16 4h3a1 1 0 011 1v3M20 16v3a1 1 0 01-1 1h-3M8 20H5a1 1 0 01-1-1v-3"/>'
    '<rect x="8.5" y="8.5" width="7" height="7" rx="1"/>',
    "gesture": '<path d="M4 16c4 0 4-8 8-8s4 8 8 8"/><path d="M16 16h4v-4"/>',
    "record": '<circle cx="12" cy="12" r="8"/><circle cx="12" cy="12" r="3.2"/>',
    "convert": '<path d="M4 8h13l-3-3M20 16H7l3 3"/>',
    "icon": '<rect x="3.5" y="3.5" width="7" height="7" rx="1.5"/>'
    '<rect x="13.5" y="3.5" width="7" height="7" rx="1.5"/>'
    '<rect x="3.5" y="13.5" width="7" height="7" rx="1.5"/>'
    '<circle cx="17" cy="17" r="3.6"/>',
    "imgbed": '<path d="M12 16V5"/><path d="M8 9l4-4 4 4"/><path d="M4 16v2a2 2 0 002 2h12a2 2 0 002-2v-2"/>',
}


# ---------------------------------------------------------------- 片段渲染


def render_stats() -> str:
    return "".join(
        f'<li title="{esc(hint)}"><b>{esc(num)}</b><span>{esc(label)}</span></li>'
        for num, label, hint in content.STATS
    )


def render_features() -> str:
    out = []
    for key, title, desc in content.FEATURES:
        icon = ICONS_SVG.get(key, ICONS_SVG["monitor"])
        out.append(
            f'<article class="card"><div class="ico"><svg viewBox="0 0 24 24" aria-hidden="true" '
            f'stroke-linecap="round" stroke-linejoin="round">{icon}</svg></div>'
            f"<h3>{esc(title)}</h3><p>{esc(desc)}</p></article>"
        )
    return "".join(out)


def png_size(path: Path) -> tuple[int, int]:
    """读 PNG 的 IHDR，给 img 补上真实宽高，避免加载时抖动。"""
    head = path.read_bytes()[:24]
    return struct.unpack(">II", head[16:24])


def shot_files() -> list[str]:
    """实际存在、会被渲染进页面的截图（按 content.SHOTS 的顺序）。"""
    return [k for k, _, _ in content.SHOTS if (SITE / "shots" / f"{k}.png").is_file()]


def hero_pick() -> tuple[str, str]:
    """首屏用哪张：按 HERO_SHOTS 的顺序取第一张存在的。

    这样"先拍出一张能用的"就能立刻换掉首屏，不必等整套配图凑齐。
    """
    for key, alt in content.HERO_SHOTS:
        if (SITE / "shots" / f"{key}.png").is_file():
            return key, alt
    raise SystemExit(
        "首屏截图缺失：site/shots/ 下至少要有 "
        + " 或 ".join(f"{k}.png" for k, _ in content.HERO_SHOTS)
    )


def render_shots() -> str:
    out = []
    for key, title, desc in content.SHOTS:
        path = SITE / "shots" / f"{key}.png"
        if not path.is_file():
            # 缺图不拦构建：截图是手工素材，需求先进清单，拍到哪张上哪张
            print(f"  · 截图缺失，跳过：shots/{key}.png（{title}）")
            continue
        w, h = png_size(path)
        # 任务栏条带只有 176×56，套一个浅色底衬托，不然在深色卡片里只剩一条细线
        if key == "taskbar":
            inner = (
                f'<div class="tkbar"><img src="shots/{key}.png" alt="{esc(title)}：{esc(desc)}" '
                f'width="{w}" height="{h}" loading="lazy" decoding="async"></div>'
            )
        else:
            inner = (
                f'<a href="shots/{key}.png" target="_blank" rel="noopener">'
                f'<img src="shots/{key}.png" alt="{esc(title)}：{esc(desc)}" '
                f'width="{w}" height="{h}" loading="lazy" decoding="async"></a>'
            )
        out.append(
            f'<figure class="shot">{inner}'
            f"<figcaption><strong>{esc(title)}</strong><span>{esc(desc)}</span></figcaption></figure>"
        )
    return "".join(out)


def render_hero() -> str:
    """首屏就放一张真机截图，不再叠装饰条——叠上去会盖住窗口内容。"""
    key, alt = hero_pick()
    w, h = png_size(SITE / "shots" / f"{key}.png")
    return (
        f'<div class="hero-art"><img class="win" src="shots/{key}.png" '
        f'alt="{esc(alt)}界面截图" width="{w}" height="{h}"></div>'
    )


def render_highlights() -> str:
    return "".join(
        f'<div class="hl"><h3>{esc(t)}</h3><p>{esc(d)}</p></div>' for t, d in content.HIGHLIGHTS
    )


def render_faq() -> str:
    out = []
    for q, a in content.FAQ:
        out.append(
            f'<div class="qa"><h3>{esc(q)}</h3><p>{esc(a)}</p></div>'
        )
    return "".join(out)


def human_size(n: int) -> str:
    if n <= 0:
        return ""
    for unit in ("B", "KB", "MB", "GB"):
        if n < 1024 or unit == "GB":
            return f"{n:.0f} {unit}" if unit == "B" else f"{n:.1f} {unit}"
        n /= 1024.0
    return ""


def latest_by_channel(rel: list[dict]) -> dict[str, dict]:
    latest: dict[str, dict] = {}
    for r in rel:  # 已按版本降序
        latest.setdefault(channel_of(r["ver"]), r)
    return latest


def download_url(r: dict | None, slug: str) -> str:
    if not r:
        return f"https://github.com/{slug}/releases"
    zip_name = f"zhuxia-{vstr(r['ver'])}-win-x64.zip"
    return r["assets"].get(zip_name) or next(
        (u for n, u in r["assets"].items() if n.endswith("win-x64.zip")), r["url"]
    )


def render_downloads(rel: list[dict], slug: str, version: str) -> str:
    latest = latest_by_channel(rel)
    cards = []
    for ch, title, tag, klass in (
        ("stable", "正式版", "稳定", "stable"),
        ("preview", "预览版", "尝鲜", "preview"),
    ):
        r = latest.get(ch)
        if r:
            ver = "v" + vstr(r["ver"])
            zip_name = f"zhuxia-{vstr(r['ver'])}-win-x64.zip"
            url = download_url(r, slug)
            size = human_size(r.get("sizes", {}).get(zip_name, 0))
            meta = ["Windows 10 / 11 x64", r["date"] or "—"]
            if size:
                meta.append(size)
            cards.append(
                f'<div class="dl {klass}"><header><h3>{title}</h3><span class="tag">{tag}</span></header>'
                f'<div class="ver">{esc(ver)}</div>'
                f'<div class="meta">{"".join(f"<span>{esc(m)}</span>" for m in meta)}</div>'
                f'<a class="btn {"primary" if klass == "stable" else "ghost"}" href="{esc(url)}" '
                f'target="_blank" rel="noopener" download>下载 {esc(zip_name)}</a>'
                f'<p class="note">源码包与 SHA256SUMS.txt 见同一发布页</p></div>'
            )
        else:
            cards.append(
                f'<div class="dl {klass}"><header><h3>{title}</h3><span class="tag">{tag}</span></header>'
                f'<div class="ver">—</div>'
                f'<div class="meta"><span>还没有{"正式" if klass == "stable" else "预览"}版本</span></div>'
                f'<a class="btn ghost" aria-disabled="true" href="{esc(f"https://github.com/{slug}/releases")}" '
                f'target="_blank" rel="noopener">等待首次发布</a>'
                f'<p class="note">当前开发版本 v{esc(version)}</p></div>'
            )
    return "".join(cards)


def render_mirror() -> str:
    """下载区下面的网盘分流。放在两张正式卡片之外，视觉上明确是"备选"。"""
    m = content.MIRROR
    return (
        f'<p class="dl-mirror">国内访问 GitHub 慢？也可以用 '
        f'<a href="{esc(m["url"])}" target="_blank" rel="noopener">{esc(m["label"])}</a> 下载——'
        f'{esc(m["note"])}</p>'
    )


def render_changelog(rel: list[dict], version: str) -> str:
    if not rel:
        commits = head_commits()
        items = "".join(f"<li>{esc(c)}</li>" for c in commits) or "<li>仓库还没有可汇总的提交</li>"
        return (
            '<li class="new"><div class="rel"><div class="rel-head">'
            f'<h3>v{esc(version)}</h3><span class="tag">开发中</span>'
            '<span class="tag">预览</span></div>'
            f"<ul>{items}</ul></div></li>"
        )

    out = []
    for i, (r, commits) in enumerate(release_notes(rel)):
        ver = "v" + vstr(r["ver"])
        label = "正式版" if channel_of(r["ver"]) == "stable" else "预览版"
        items = "".join(f"<li>{esc(c)}</li>" for c in commits)
        if not items:
            items = "<li>该版本没有可汇总的变更条目</li>"
        link = (
            f'<a class="link" href="{esc(r["url"])}" target="_blank" rel="noopener">发布页</a>'
            if r["url"]
            else ""
        )
        date = r["date"] or ""
        when = f'<time datetime="{esc(date)}">{esc(date or "—")}</time>' if date else ""
        out.append(
            f'<li class="{"new" if i == 0 else ""}"><div class="rel"><div class="rel-head">'
            f'<h3>{esc(ver)}</h3><span class="tag">{label}</span>'
            f"{when}{link}</div><ul>{items}</ul></div></li>"
        )
    return "".join(out)


# ---------------------------------------------------------------- 结构化数据


def render_jsonld(version: str, site: str, slug: str, rel: list[dict]) -> str:
    repo = f"https://github.com/{slug}"
    latest = latest_by_channel(rel)
    # 有正式版就指向正式版，否则指向预览版，都没有就指向发布页
    target = latest.get("stable") or latest.get("preview")
    app = {
        "@context": "https://schema.org",
        "@type": "SoftwareApplication",
        "name": "驻匣",
        "alternateName": ["Zhuxia", "驻匣 Zhuxia"],
        "description": content.SUB,
        "applicationCategory": "UtilitiesApplication",
        "applicationSubCategory": "系统工具",
        "operatingSystem": "Windows 10, Windows 11",
        "processorRequirements": "x64",
        "softwareVersion": vstr(target["ver"]) if target else version,
        "inLanguage": "zh-CN",
        "isAccessibleForFree": True,
        "license": "https://www.apache.org/licenses/LICENSE-2.0",
        "url": site,
        "downloadUrl": download_url(target, slug),
        "installUrl": f"{repo}/releases",
        "screenshot": f"{site}shots/{hero_pick()[0]}.png",
        "image": f"{site}assets/og.png",
        "featureList": [f[1] for f in content.FEATURES],
        "offers": {
            "@type": "Offer",
            "price": "0",
            "priceCurrency": "CNY",
            "availability": "https://schema.org/InStock",
        },
        "author": {"@type": "Organization", "name": "Zhuxia", "url": repo},
        "sameAs": [repo],
    }
    website = {
        "@context": "https://schema.org",
        "@type": "WebSite",
        "name": "驻匣 Zhuxia",
        "alternateName": "Zhuxia",
        "url": site,
        "inLanguage": "zh-CN",
        "about": {"@type": "SoftwareApplication", "name": "驻匣"},
    }
    faq = {
        "@context": "https://schema.org",
        "@type": "FAQPage",
        "mainEntity": [
            {
                "@type": "Question",
                "name": q,
                "acceptedAnswer": {"@type": "Answer", "text": a},
            }
            for q, a in content.FAQ
        ],
    }
    data = json.dumps([app, website, faq], ensure_ascii=False, indent=2)
    return f'<script type="application/ld+json">\n{data}\n</script>'


# ---------------------------------------------------------------- 站点级文件


def write_robots(out: Path, site: str) -> None:
    """显式放行主流生成式引擎的抓取器——GEO 的前提是能被抓。"""
    bots = [
        "GPTBot", "OAI-SearchBot", "ChatGPT-User",
        "ClaudeBot", "Claude-Web", "anthropic-ai",
        "PerplexityBot", "Perplexity-User",
        "Google-Extended", "Applebot-Extended",
        "CCBot", "Bytespider", "Amazonbot", "cohere-ai",
        "Bingbot", "Googlebot", "Baiduspider",
    ]
    lines = ["User-agent: *", "Allow: /", ""]
    for b in bots:
        lines += [f"User-agent: {b}", "Allow: /", ""]
    lines += [f"Sitemap: {site}sitemap.xml", ""]
    (out / "robots.txt").write_text("\n".join(lines), encoding="utf-8")


def write_sitemap(out: Path, site: str, today: str) -> None:
    xml = (
        '<?xml version="1.0" encoding="UTF-8"?>\n'
        '<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">\n'
        "  <url>\n"
        f"    <loc>{esc(site)}</loc>\n"
        f"    <lastmod>{today}</lastmod>\n"
        "    <changefreq>weekly</changefreq>\n"
        "    <priority>1.0</priority>\n"
        "  </url>\n"
        "</urlset>\n"
    )
    (out / "sitemap.xml").write_text(xml, encoding="utf-8")


def write_llms(out: Path, site: str, slug: str, version: str, rel: list[dict]) -> None:
    """llms.txt：给大模型看的站点摘要（llmstxt.org 约定）。"""
    repo = f"https://github.com/{slug}"
    latest = latest_by_channel(rel)
    stable = latest.get("stable")
    preview = latest.get("preview")
    lines = [
        "# 驻匣 Zhuxia",
        "",
        f"> {content.SUB}",
        "",
        "## 基本信息",
        "",
        f"- 名称：{content.NAME}（{content.NAME_EN}）",
        "- 平台：Windows 10 / Windows 11 x64",
        "- 技术栈：Rust + Win32 原生绘制；设置面板用 Tauri 2（按需启动，不常驻）",
        "- 授权：Apache License 2.0，免费使用，可商用",
        f"- 源码：{repo}",
        f"- 问题反馈：{repo}/issues",
        f"- 官网：{site}",
        f"- 当前开发版本：{version}",
        "",
        "## 功能",
        "",
    ]
    for _, title, desc in content.FEATURES:
        lines.append(f"- **{title}**：{desc}")
    lines += ["", "## 下载", ""]
    if stable:
        lines.append(f"- 正式版 v{vstr(stable['ver'])}：{download_url(stable, slug)}")
    if preview:
        lines.append(f"- 预览版 v{vstr(preview['ver'])}：{download_url(preview, slug)}")
    if not stable and not preview:
        lines.append(f"- 尚未发布：{repo}/releases")
    lines += ["", "## 版本号规则", "",
              "`major.minor.patch`：最右位 0 为正式版、1 为预览版；中间位功能增减时 +1 并归零最右位；"
              "最左位仅在发生不兼容的重大改变时 +1。", ""]
    notes = release_notes(rel)
    if notes:
        lines += ["## 最近版本", ""]
        for r, commits in notes[:5]:
            head = f"### v{vstr(r['ver'])}（{'正式版' if channel_of(r['ver']) == 'stable' else '预览版'}）"
            lines.append(head)
            if r["date"]:
                lines.append(f"发布日期：{r['date']}")
            lines.append("")
            for c in commits[:8]:
                lines.append(f"- {c}")
            lines.append("")
    lines += ["## 常见问题", ""]
    for q, a in content.FAQ:
        lines += [f"### {q}", "", a, ""]
    (out / "llms.txt").write_text("\n".join(lines).rstrip() + "\n", encoding="utf-8")


def write_feed(out: Path, site: str, slug: str, version: str, rel: list[dict]) -> None:
    """RSS 2.0：每个版本一条，正文是变更列表。"""
    now = datetime.now(timezone.utc).strftime("%a, %d %b %Y %H:%M:%S +0000")
    items = []
    notes = release_notes(rel)
    if not notes:
        body = "".join(f"<li>{esc(c)}</li>" for c in head_commits())
        notes = [({"tag": f"v{version}", "ver": parse_version(version) or (0, 0, 0),
                   "date": "", "url": f"https://github.com/{slug}/releases"}, [body])]
    for r, commits in notes:
        ver = "v" + vstr(r["ver"])
        link = r["url"] or f"https://github.com/{slug}/releases/tag/{r['tag']}"
        desc = "".join(f"<li>{esc(c)}</li>" for c in commits) or "<li>无变更条目</li>"
        pub = (
            f"<pubDate>{r['date']}T00:00:00Z</pubDate>" if r["date"] else ""
        )
        items.append(
            "    <item>\n"
            f"      <title>{esc(ver)}</title>\n"
            f"      <link>{esc(link)}</link>\n"
            f"      <guid isPermaLink=\"false\">{esc(link)}</guid>\n"
            f"      {pub}\n"
            f"      <description>{esc('<ul>' + desc + '</ul>')}</description>\n"
            "    </item>"
        )
    xml = (
        '<?xml version="1.0" encoding="UTF-8"?>\n'
        '<rss version="2.0" xmlns:atom="http://www.w3.org/2005/Atom">\n'
        "  <channel>\n"
        "    <title>驻匣 Zhuxia 更新日志</title>\n"
        f"    <link>{esc(site)}</link>\n"
        f"    <atom:link href=\"{esc(site)}feed.xml\" rel=\"self\" type=\"application/rss+xml\"/>\n"
        "    <description>驻匣每个版本的变更条目</description>\n"
        "    <language>zh-CN</language>\n"
        f"    <lastBuildDate>{now}</lastBuildDate>\n"
        + "\n".join(items)
        + "\n  </channel>\n</rss>\n"
    )
    (out / "feed.xml").write_text(xml, encoding="utf-8")


def write_manifest(out: Path) -> None:
    data = {
        "name": "驻匣 Zhuxia",
        "short_name": "驻匣",
        "description": content.SUB,
        "lang": "zh-CN",
        "start_url": "./",
        "scope": "./",
        "display": "standalone",
        "background_color": "#0b0d10",
        "theme_color": "#0b0d10",
        "icons": [
            {"src": "assets/icon-256.png", "sizes": "256x256", "type": "image/png", "purpose": "any"},
            {"src": "assets/icon-512.png", "sizes": "512x512", "type": "image/png", "purpose": "any"},
        ],
    }
    (out / "site.webmanifest").write_text(
        json.dumps(data, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )


def copy_icons(out: Path) -> None:
    """站点图标直接用软件自己的图标，不再单独画一套。"""
    src = ICONS / "png"
    shutil.copyfile(ICONS / "zhuxia.ico", out / "favicon.ico")
    for size in (32, 64, 256, 512):
        shutil.copyfile(src / f"zhuxia-{size}.png", out / "assets" / f"icon-{size}.png")


# ---------------------------------------------------------------- 主流程


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default="_site")
    args = ap.parse_args()

    version = workspace_version()
    slug = repo_slug()
    site = site_url(slug)
    check_pages_host(custom_domain(site))
    repo = f"https://github.com/{slug}"
    releases = collect_releases(slug)
    ver_tuple = parse_version(version) or (0, 0, 0)
    channel = "正式版" if channel_of(ver_tuple) == "stable" else "预览版"
    today = datetime.now(timezone.utc).strftime("%Y-%m-%d")

    out = (ROOT / args.out).resolve()
    if out.exists():
        # 逐个删而不是整目录 rmtree：输出目录可能正被预览服务器占用，
        # 整目录删除在 Windows 上会因为句柄没释放而失败。
        for item in sorted(out.rglob("*"), key=lambda p: len(p.parts), reverse=True):
            try:
                item.unlink() if item.is_file() else item.rmdir()
            except OSError:
                pass
    (out / "assets").mkdir(parents=True, exist_ok=True)

    # 视觉素材：界面截图是真机截的（site/shots/），这里只生成分享图
    shots_dir = out / "shots"
    shots_dir.mkdir(parents=True, exist_ok=True)
    for png in (SITE / "shots").glob("*.png"):
        shutil.copyfile(png, shots_dir / png.name)

    # 打赏二维码：不是脚本生成的图，原样搬过去（文件名统一小写，Linux CI 区分大小写）
    pay_dir = out / "pay"
    pay_dir.mkdir(parents=True, exist_ok=True)
    for png in (SITE / "pay").glob("*.png"):
        shutil.copyfile(png, pay_dir / png.name)
    art.og_image(ICONS / "png" / "zhuxia-1024.png", out / "assets" / "og.png")
    copy_icons(out)
    shutil.copyfile(SITE / "style.css", out / "style.css")

    # 站点级文件：SEO / GEO
    write_robots(out, site)
    write_sitemap(out, site, today)
    write_llms(out, site, slug, version, releases)
    write_feed(out, site, slug, version, releases)
    write_manifest(out)

    tpl = (SITE / "template.html").read_text(encoding="utf-8")
    fields = {
        "TITLE": f"{content.NAME} {content.NAME_EN} · {content.TAGLINE}",
        "DESC": content.SUB,
        "SUB": esc(content.SUB),
        "SITE_URL": site,
        "REPO": repo,
        "RELEASES": f"{repo}/releases",
        "JSONLD": render_jsonld(version, site, slug, releases),
        "VERSION": version,
        "CHANNEL": channel,
        "CHANNEL_CLASS": "stable" if channel == "正式版" else "",
        "STATS": render_stats(),
        "FEATURES": render_features(),
        "HERO": render_hero(),
        "SHOTS": render_shots(),
        "HIGHLIGHTS": render_highlights(),
        "FAQ": render_faq(),
        "DOWNLOADS": render_downloads(releases, slug, version),
        "MIRROR": render_mirror(),
        "CHANGELOG": render_changelog(releases, version),
        "BUILT_AT": datetime.now(timezone.utc).astimezone().strftime("%Y-%m-%d %H:%M"),
    }
    page = tpl
    for key, value in fields.items():
        page = page.replace("{{" + key + "}}", value)
    leftover = re.findall(r"\{\{[A-Z_]+\}\}", page)
    if leftover:
        raise SystemExit(f"模板里还有未替换的占位符：{sorted(set(leftover))}")

    (out / "index.html").write_text(page, encoding="utf-8")

    # CNAME：绑了自定义域名就写一份进产物。
    # 用 Actions 发布时 GitHub **忽略**这个文件（域名存在仓库 Settings → Pages 里），
    # 它不会让域名"丢"或"变"；写它有两个用处：产物自带域名记录、将来若改成
    # 「从分支发布」也不用再补。没绑域名（*.github.io）时不写，免得 fork 出去乱指。
    domain = custom_domain(site)
    if domain:
        (out / "CNAME").write_text(domain + "\n", encoding="utf-8", newline="\n")

    verify(out, site=site)
    print(f"已生成 {out / 'index.html'}（版本 {version} / {channel}，{len(releases)} 个发布，站点 {site}）")
    return 0


# 产物清单：构建完自己验一遍。这条检查以前写在 workflow 里，写死了 assets/hero.svg；
# 换成真机截图后那个文件没了，CI 每次都在那儿失败。清单放在生成脚本里才不会再脱节。
REQUIRED = [
    "index.html",
    "style.css",
    "favicon.ico",
    "site.webmanifest",
    "robots.txt",
    "sitemap.xml",
    "llms.txt",
    "feed.xml",
    "assets/og.png",
    "assets/icon-32.png",
    "assets/icon-64.png",
    "assets/icon-256.png",
    "assets/icon-512.png",
    # 打赏二维码（站点唯一的"非生成图"）
    "pay/wechat.png",
    "pay/alipay.png",
]


def verify(out: Path, site: str = "") -> None:
    domain = custom_domain(site)
    # 只校验真正会被渲染的截图：清单里那些还没拍的不算缺件，
    # 但首屏那张必须有（否则页面顶部是空的）。
    need = REQUIRED + [f"shots/{k}.png" for k in shot_files()]
    hero_file = f"shots/{hero_pick()[0]}.png"
    if hero_file not in need:
        need.append(hero_file)
    if domain:
        need.append("CNAME")
    missing = [f for f in need if not (out / f).is_file()]
    if missing:
        raise SystemExit(f"产物缺失 {len(missing)} 个：{', '.join(missing)}")
    empty = [f for f in need if (out / f).stat().st_size == 0]
    if empty:
        raise SystemExit(f"产物是空文件：{', '.join(empty)}")

    # CNAME 只能有域名本身：带协议、带路径、带换行以外的字符都会被 GitHub 判无效
    if domain:
        cname = (out / "CNAME").read_text(encoding="utf-8").strip()
        if cname != domain:
            raise SystemExit(f"CNAME 内容不对：{cname!r}，应为 {domain!r}")

    page = (out / "index.html").read_text(encoding="utf-8")
    left = sorted(set(re.findall(r"\{\{[A-Z_]+\}\}", page)))
    if left:
        raise SystemExit(f"模板占位符没替换：{' '.join(left)}")

    # 绑了域名之后，页面里不该再出现 github.io：那是 301 前的旧地址，
    # canonical / og:url / sitemap 指过去等于把权重让给一个跳转。
    if domain:
        stale = []
        for name in ("index.html", "sitemap.xml", "robots.txt", "feed.xml", "llms.txt", "site.webmanifest"):
            f = out / name
            if f.is_file() and "github.io" in f.read_text(encoding="utf-8"):
                stale.append(name)
        if stale:
            raise SystemExit(f"这些文件里还残留 github.io 的绝对地址：{', '.join(stale)}")
        if site not in page:
            raise SystemExit(f"index.html 里找不到站点地址 {site}（canonical / og:url 没跟上）")


if __name__ == "__main__":
    raise SystemExit(main())
