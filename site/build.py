#!/usr/bin/env python3
"""驻匣宣传页构建脚本。

输入（全部来自仓库本身，没有人工维护的数据文件）：
  · 版本号        → Cargo.toml 的 [workspace.package] version
  · 发布列表/日期  → `gh api .../releases`（有 GH_TOKEN 时）或 git 标签
  · 更新日志       → git 提交历史（相邻标签之间的提交标题）
  · 视觉素材       → site/art.py 现场生成 SVG

输出：_site/（index.html + style.css + favicon.svg + assets/*.svg）

用法：
  python3 site/build.py [--out _site]
"""

from __future__ import annotations

import argparse
import html
import json
import os
import re
import shutil
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import art  # noqa: E402
import content  # noqa: E402

ROOT = Path(__file__).resolve().parent.parent
SITE = Path(__file__).resolve().parent


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


def parse_version(tag: str) -> tuple[int, int, int] | None:
    m = re.fullmatch(r"v?(\d+)\.(\d+)\.(\d+)", tag.strip())
    return (int(m.group(1)), int(m.group(2)), int(m.group(3))) if m else None


def channel_of(ver: tuple[int, int, int]) -> str:
    """最右位 0=正式、1=预览（docs/ARCHITECTURE.md「版本号」）。"""
    return "stable" if ver[2] == 0 else "preview"


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


# ---------------------------------------------------------------- 图标


ICONS = {
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
        icon = ICONS.get(key, ICONS["monitor"])
        out.append(
            f'<article class="card"><div class="ico"><svg viewBox="0 0 24 24" '
            f'stroke-linecap="round" stroke-linejoin="round">{icon}</svg></div>'
            f"<h3>{esc(title)}</h3><p>{esc(desc)}</p></article>"
        )
    return "".join(out)


def render_shots() -> str:
    out = []
    for i, (key, title, desc) in enumerate(content.SHOTS):
        cls = "shot wide" if i == 0 else "shot"
        out.append(
            f'<figure class="{cls}"><img src="assets/{key}.svg" alt="{esc(title)}" loading="lazy">'
            f"<figcaption><strong>{esc(title)}</strong><span>{esc(desc)}</span></figcaption></figure>"
        )
    return "".join(out)


def render_highlights() -> str:
    return "".join(
        f'<div class="hl"><h3>{esc(t)}</h3><p>{esc(d)}</p></div>' for t, d in content.HIGHLIGHTS
    )


def human_size(n: int) -> str:
    if n <= 0:
        return ""
    for unit in ("B", "KB", "MB", "GB"):
        if n < 1024 or unit == "GB":
            return f"{n:.0f} {unit}" if unit == "B" else f"{n:.1f} {unit}"
        n /= 1024.0
    return ""


def render_downloads(rel: list[dict], slug: str, version: str) -> str:
    latest = {}
    for r in rel:  # rel 已按版本降序
        latest.setdefault(channel_of(r["ver"]), r)

    cards = []
    for ch, title, tag, klass in (
        ("stable", "正式版", "稳定", "stable"),
        ("preview", "预览版", "尝鲜", "preview"),
    ):
        r = latest.get(ch)
        if r:
            ver = "v" + ".".join(str(x) for x in r["ver"])
            zip_name = f'zhuxia-{".".join(str(x) for x in r["ver"])}-win-x64.zip'
            url = r["assets"].get(zip_name) or next(
                (u for n, u in r["assets"].items() if n.endswith("win-x64.zip")), r["url"]
            )
            size = human_size(r.get("sizes", {}).get(zip_name, 0))
            meta = [f"Windows 10 / 11 x64", r["date"] or "—"]
            if size:
                meta.append(size)
            cards.append(
                f'<div class="dl {klass}"><header><h3>{title}</h3><span class="tag">{tag}</span></header>'
                f'<div class="ver">{esc(ver)}</div>'
                f'<div class="meta">{"".join(f"<span>{esc(m)}</span>" for m in meta)}</div>'
                f'<a class="btn {"primary" if klass == "stable" else "ghost"}" href="{esc(url)}" '
                f'target="_blank" rel="noopener">下载 {esc(zip_name)}</a>'
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


def render_changelog(rel: list[dict], version: str) -> str:
    if not rel:
        commits = head_commits()
        items = "".join(f"<li>{esc(c)}</li>" for c in commits) or "<li>仓库还没有可汇总的提交</li>"
        return (
            '<li class="new"><div class="rel"><div class="rel-head">'
            f"<h3>v{esc(version)}</h3><span class=\"tag\">开发中</span>"
            "<span class=\"tag\">预览</span></div>"
            f"<ul>{items}</ul></div></li>"
        )

    out = []
    for i, r in enumerate(rel):
        prev_tag = rel[i + 1]["tag"] if i + 1 < len(rel) else None
        commits = commits_between(prev_tag, r["tag"])
        ver = "v" + ".".join(str(x) for x in r["ver"])
        ch = channel_of(r["ver"])
        label = "正式版" if ch == "stable" else "预览版"
        items = "".join(f"<li>{esc(c)}</li>" for c in commits)
        if not items:
            items = "<li>该版本没有可汇总的变更条目</li>"
        link = (
            f'<a class="link" href="{esc(r["url"])}" target="_blank" rel="noopener">发布页</a>'
            if r["url"]
            else ""
        )
        out.append(
            f'<li class="{"new" if i == 0 else ""}"><div class="rel"><div class="rel-head">'
            f"<h3>{esc(ver)}</h3><span class=\"tag\">{label}</span>"
            f'<time>{esc(r["date"] or "—")}</time>{link}</div><ul>{items}</ul></div></li>'
        )
    return "".join(out)


# ---------------------------------------------------------------- 主流程


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default="_site")
    args = ap.parse_args()

    version = workspace_version()
    slug = repo_slug()
    repo = f"https://github.com/{slug}"
    releases = collect_releases(slug)
    ver_tuple = parse_version(version) or (0, 0, 0)
    channel = "正式版" if channel_of(ver_tuple) == "stable" else "预览版"

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

    # 视觉素材
    for name in ("hero", "capture", "clipboard", "settings"):
        (out / "assets" / f"{name}.svg").write_text(art.ART[name](), encoding="utf-8")
    (out / "favicon.svg").write_text(art.logo(40), encoding="utf-8")
    shutil.copyfile(SITE / "style.css", out / "style.css")

    tpl = (SITE / "template.html").read_text(encoding="utf-8")
    fields = {
        "TITLE": f"{content.NAME} {content.NAME_EN} · {content.TAGLINE}",
        "DESC": content.SUB,
        "SUB": esc(content.SUB),
        "LOGO": art.logo(40),
        "REPO": repo,
        "RELEASES": f"{repo}/releases",
        "VERSION": version,
        "CHANNEL": channel,
        "CHANNEL_CLASS": "stable" if channel == "正式版" else "",
        "STATS": render_stats(),
        "FEATURES": render_features(),
        "SHOTS": render_shots(),
        "HIGHLIGHTS": render_highlights(),
        "DOWNLOADS": render_downloads(releases, slug, version),
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
    print(f"已生成 {out / 'index.html'}（版本 {version} / {channel}，{len(releases)} 个发布）")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
