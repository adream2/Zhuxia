"""生成页面用的视觉素材（纯 SVG，无外部依赖、无字体下载）。

为什么是「生成」而不是「截图」：发布流水线跑在 CI 上，抓真实截图要拉起 GUI、
要 WebView2、要有人盯着桌面，失败一次整个发布就断了。这里按产品真实的设计
token（配色、圆角、任务栏尺寸比例）画出界面示意，每次构建重新生成，
版本号一变素材跟着变，不需要人工维护。

所有坐标都写在 1x 逻辑像素上，SVG 天然缩放，桌面和手机都清晰。
"""

from __future__ import annotations

# 与 crates/capture/src/design.rs、crates/zx-ui 里的设计 token 对齐
BG = "#0E1013"
PANEL = "#16181D"
PANEL_2 = "#1C1F25"
SUNKEN = "#101216"
BORDER = "#2A2E37"
TEXT = "#E8EAEE"
DIM = "#9AA1AD"
FAINT = "#6B7280"
ACCENT = "#2E90FA"
GREEN = "#22C55E"
RED = "#FF4D4F"
AMBER = "#FFA940"
PURPLE = "#A855F7"

FONT = "'Microsoft YaHei UI','PingFang SC','Hiragino Sans GB','Noto Sans SC',system-ui,sans-serif"
MONO = "'JetBrains Mono','Cascadia Mono',ui-monospace,monospace"


def _e(s: str) -> str:
    return (s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;"))


def svg(w: float, h: float, body: str, label: str = "") -> str:
    aria = f' role="img" aria-label="{_e(label)}"' if label else ' aria-hidden="true"'
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w:g} {h:g}" '
        f'width="{w:g}" height="{h:g}"{aria} font-family="{FONT}">{body}</svg>'
    )


def rect(x, y, w, h, fill="none", rx=0, stroke=None, sw=1, op=None) -> str:
    a = f'<rect x="{x:g}" y="{y:g}" width="{w:g}" height="{h:g}" rx="{rx:g}" fill="{fill}"'
    if stroke:
        a += f' stroke="{stroke}" stroke-width="{sw:g}"'
    if op is not None:
        a += f' opacity="{op:g}"'
    return a + "/>"


def circle(cx, cy, r, fill="none", stroke=None, sw=1, op=None) -> str:
    a = f'<circle cx="{cx:g}" cy="{cy:g}" r="{r:g}" fill="{fill}"'
    if stroke:
        a += f' stroke="{stroke}" stroke-width="{sw:g}"'
    if op is not None:
        a += f' opacity="{op:g}"'
    return a + "/>"


def text(x, y, s, size=12, fill=TEXT, weight=400, anchor="start", mono=False, op=None) -> str:
    a = (
        f'<text x="{x:g}" y="{y:g}" font-size="{size:g}" fill="{fill}" '
        f'font-weight="{weight}" text-anchor="{anchor}"'
    )
    if mono:
        a += f' font-family="{MONO}"'
    if op is not None:
        a += f' opacity="{op:g}"'
    return a + f">{_e(s)}</text>"


def line(x1, y1, x2, y2, stroke, sw=1, op=None) -> str:
    a = (
        f'<line x1="{x1:g}" y1="{y1:g}" x2="{x2:g}" y2="{y2:g}" '
        f'stroke="{stroke}" stroke-width="{sw:g}" stroke-linecap="round"'
    )
    if op is not None:
        a += f' opacity="{op:g}"'
    return a + "/>"


def path(d, fill="none", stroke=None, sw=1, op=None) -> str:
    a = f'<path d="{d}" fill="{fill}"'
    if stroke:
        a += f' stroke="{stroke}" stroke-width="{sw:g}" stroke-linecap="round" stroke-linejoin="round"'
    if op is not None:
        a += f' opacity="{op:g}"'
    return a + "/>"


def spark(x, y, w, h, series, color) -> str:
    """迷你折线：把 0..1 的序列映射到 w×h 的框里。"""
    n = len(series)
    pts = []
    for i, v in enumerate(series):
        px = x + w * i / (n - 1)
        py = y + h - v * h
        pts.append(f"{px:.1f} {py:.1f}")
    d = "M" + " L".join(pts)
    area = d + f" L{x + w:g} {y + h:g} L{x:g} {y + h:g} Z"
    return (
        path(area, fill=color, op=0.14)
        + path(d, stroke=color, sw=1.6)
    )


def _series(seed: int, n: int = 24) -> list[float]:
    """确定性伪随机，保证每次构建的素材完全一致。"""
    v = seed * 9301 + 49297
    out = []
    for _ in range(n):
        v = (v * 9301 + 49297) % 233280
        out.append(0.25 + 0.7 * (v / 233280))
    return out


# ---------------------------------------------------------------- 标识


def logo(size: int = 40) -> str:
    body = (
        '<defs><linearGradient id="lg" x1="0" y1="0" x2="1" y2="1">'
        f'<stop offset="0" stop-color="{ACCENT}"/>'
        f'<stop offset="1" stop-color="{PURPLE}"/>'
        "</linearGradient></defs>"
        f'<rect x="2" y="2" width="36" height="36" rx="10" fill="url(#lg)"/>'
        f'<rect x="12" y="13" width="16" height="14" rx="3" fill="none" stroke="#fff" stroke-width="2"/>'
        f'<path d="M12 19h16" stroke="#fff" stroke-width="2" stroke-linecap="round"/>'
        f'<circle cx="21.5" cy="24" r="1.6" fill="#fff"/>'
    )
    return svg(size, size, body, "驻匣图标")


# ---------------------------------------------------------------- 首屏主视觉


def hero_monitor() -> str:
    """任务栏监视条 + 两张卡片：按进程网速、剪贴板。

    画布按「页面上实际显示宽度」设计（约 540px），这样文字不会被缩到看不清。
    """
    w, h = 560, 300
    b = [rect(0, 0, w, h, PANEL, 16, BORDER)]

    # 任务栏条（真实比例 224x48，这里 560 宽 5 列）
    bw, bh = 560, 104
    cols = [
        ("CPU", "12%", ACCENT, 1, False),
        ("内存", "8.4G", GREEN, 2, False),
        ("磁盘", "3%", AMBER, 3, False),
        ("GPU", "21%", PURPLE, 4, False),
        ("网络", "↑1.2 ↓8.6", "#38BDF8", 5, True),
    ]
    step = bw / len(cols)
    for i, (label, value, color, seed, mono) in enumerate(cols):
        cx = step * i + step / 2
        b.append(text(cx, 26, label, 11, FAINT, 500, "middle"))
        b.append(text(cx, 50, value, 17 if not mono else 15, TEXT, 700, "middle", mono=mono))
        b.append(spark(cx - step / 2 + 14, 62, step - 28, 26, _series(seed, 20), color))
        if i:
            b.append(line(step * i, 16, step * i, bh - 16, BORDER))

    # 卡片 A：按进程网速
    ax, ay, aw = 0, 120, 296
    b.append(rect(ax, ay, aw, 180, PANEL_2, 12, BORDER))
    b.append(text(ax + 16, ay + 28, "按进程网速", 13, TEXT, 700))
    b.append(rect(ax + aw - 52, ay + 14, 36, 20, "#12293F", 6))
    b.append(text(ax + aw - 34, ay + 28, "ETW", 10, ACCENT, 700, "middle"))
    rows = [
        ("chrome.exe", "1.8 MB/s", 0.86, "#38BDF8"),
        ("zhuxia.exe", "412 KB/s", 0.48, ACCENT),
        ("Code.exe", "96 KB/s", 0.24, GREEN),
        ("explorer.exe", "8 KB/s", 0.09, FAINT),
    ]
    for i, (name, val, ratio, color) in enumerate(rows):
        ry = ay + 56 + i * 30
        b.append(text(ax + 16, ry, name, 11.5, DIM, 400, mono=True))
        b.append(rect(ax + 16, ry + 9, aw - 32, 5, BORDER, 2.5))
        b.append(rect(ax + 16, ry + 9, (aw - 32) * ratio, 5, color, 2.5))
        b.append(text(ax + aw - 16, ry, val, 10.5, FAINT, 400, "end", mono=True))

    # 卡片 B：剪贴板
    bx, by, bwd = 312, 120, 248
    b.append(rect(bx, by, bwd, 180, PANEL_2, 12, BORDER))
    b.append(text(bx + 16, by + 28, "剪贴板历史", 13, TEXT, 700))
    b.append(text(bx + bwd - 16, by + 28, "12 条", 10.5, FAINT, 400, "end"))
    items = [
        ("https://github.com/adream2/Zhuxia", "刚刚", ACCENT),
        ("Clipboard_Screenshot.png", "2 分钟", GREEN),
        ("cargo build --release", "1 小时", PURPLE),
    ]
    for i, (body, when, color) in enumerate(items):
        ry = by + 52 + i * 40
        b.append(rect(bx + 12, ry, bwd - 24, 32, SUNKEN, 8))
        b.append(rect(bx + 20, ry + 9, 14, 14, color, 4, op=0.9))
        b.append(text(bx + 42, ry + 21, body[:22], 10.5, DIM, 400, mono=True))
        b.append(text(bx + bwd - 20, ry + 21, when, 9.5, FAINT, 400, "end"))

    return svg(w, h, "".join(b), "任务栏系统监视、按进程网速与剪贴板历史")


# ---------------------------------------------------------------- 截图编辑态


def _tool_icon(x, y, kind, color=TEXT) -> str:
    """工具条上的小图标，与 app 里的图标语义一致。"""
    c = color
    if kind == "pointer":
        return path(
            f"M{x + 1} {y - 7} L{x + 9} {y - 2.5} L{x + 5.5} {y - 1.6} L{x + 4.4} {y + 2.6} "
            f"L{x + 2} {y - 1.2} L{x + 1} {y + 0.2} Z",
            fill=c,
        )
    if kind == "rect":
        return rect(x - 5, y - 5, 10, 10, "none", 2, c, 1.6)
    if kind == "ellipse":
        return circle(x, y, 5.2, "none", c, 1.6)
    if kind == "arrow":
        return path(f"M{x - 5} {y + 5} L{x + 5} {y - 5} M{x} {y - 5} L{x + 5} {y - 5} L{x + 5} {y}", stroke=c, sw=1.6)
    if kind == "line":
        return line(x - 5, y + 5, x + 5, y - 5, c, 1.6)
    if kind == "pen":
        return path(f"M{x - 5} {y + 5} L{x - 4} {y + 1} L{x + 3} {y - 6} L{x + 6} {y - 3} L{x - 1} {y + 4} Z", stroke=c, sw=1.4)
    if kind == "hl":
        return rect(x - 5, y - 2, 10, 5, c, 1, op=0.85)
    if kind == "mosaic":
        return "".join(
            rect(x - 5 + dx, y - 5 + dy, 4, 4, c, 1, op=0.9 if (dx + dy) % 8 else 0.4)
            for dx in (0, 6)
            for dy in (0, 6)
        )
    if kind == "text":
        return (
            line(x - 4.5, y - 5, x + 4.5, y - 5, c, 1.6)
            + line(x, y - 5, x, y + 5, c, 1.6)
            + line(x - 2.5, y + 5, x + 2.5, y + 5, c, 1.6)
        )
    if kind == "counter":
        return circle(x, y, 5.2, "none", c, 1.5) + text(x, y + 2.6, "1", 7, c, 600, "middle")
    return ""


def shot_capture() -> str:
    w, h = 900, 520
    b = [rect(0, 0, w, h, "#EEF0F4")]

    # 被截的"页面"：一块浅色文档
    b.append(rect(40, 28, 820, 464, "#FFFFFF", 10, "#DDE1E8"))
    b.append(rect(40, 28, 820, 40, "#F5F6F9", 10, "#DDE1E8"))
    for i, c in enumerate(["#FF5F57", "#FEBC2E", "#28C840"]):
        b.append(circle(62 + i * 16, 48, 5, c))
    b.append(text(150, 53, "docs.example.com/report", 11, "#8A93A2", 400, mono=True))
    b.append(text(72, 108, "季度报告 · 摘要", 20, "#1A1D23", 700))
    for i, wid in enumerate([420, 500, 360, 470, 300]):
        b.append(rect(72, 128 + i * 18, wid, 8, "#E4E8EF", 4))
    b.append(rect(72, 236, 200, 92, "#EAF2FF", 8))
    b.append(rect(292, 236, 200, 92, "#FFF3E6", 8))

    # 选区 + 暗化
    sx, sy, sw, sh = 118, 92, 620, 312
    b.append(rect(0, 0, w, sy, "#000", 0, op=0.42))
    b.append(rect(0, sy + sh, w, h - sy - sh, "#000", 0, op=0.42))
    b.append(rect(0, sy, sx, sh, "#000", 0, op=0.42))
    b.append(rect(sx + sw, sy, w - sx - sw, sh, "#000", 0, op=0.42))
    b.append(rect(sx, sy, sw, sh, "none", 0, ACCENT, 2))

    # 标注：矩形 / 箭头 / 序号 / 荧光笔
    b.append(rect(150, 122, 210, 108, "none", 4, RED, 3))
    b.append(line(392, 300, 556, 214, RED, 3))
    b.append(path("M556 214 L536 214 M556 214 L552 234", stroke=RED, sw=3))
    b.append(circle(600, 336, 15, ACCENT) + text(600, 341, "1", 13, "#FFFFFF", 700, "middle"))
    b.append(rect(430, 122, 240, 16, AMBER, 3, op=0.5))

    # 控制点
    for hx, hy in [
        (sx, sy), (sx + sw / 2, sy), (sx + sw, sy), (sx + sw, sy + sh / 2),
        (sx + sw, sy + sh), (sx + sw / 2, sy + sh), (sx, sy + sh), (sx, sy + sh / 2),
    ]:
        b.append(circle(hx, hy, 5, "#FFFFFF", ACCENT, 1.8))

    # 工具条
    tx, ty, tw, th = 168, 424, 566, 50
    b.append(rect(tx, ty, tw, th, "#1B1E24", 12, "#333843"))
    kinds = ["pointer", "rect", "ellipse", "arrow", "line", "pen", "hl", "mosaic", "text", "counter"]
    for i, k in enumerate(kinds):
        ix = tx + 30 + i * 36
        active = k == "rect"
        if active:
            b.append(rect(ix - 16, ty + 9, 32, 32, "#2E90FA", 8))
        b.append(_tool_icon(ix, ty + 25, k, "#FFFFFF" if active else DIM))
        if i == 0:
            b.append(line(ix + 18, ty + 12, ix + 18, ty + 38, "#3A404C"))
    b.append(line(tx + 402, ty + 12, tx + 402, ty + 38, "#3A404C"))
    b.append(rect(tx + 418, ty + 11, 76, 28, ACCENT, 8))
    b.append(text(tx + 456, ty + 30, "复制", 12, "#FFFFFF", 600, "middle"))
    b.append(circle(tx + 522, ty + 25, 13, "none", "#4B5262", 1.4))
    b.append(text(tx + 522, ty + 29, "×", 12, DIM, 600, "middle"))

    return svg(w, h, "".join(b), "截图编辑：选区、标注与工具条")


# ---------------------------------------------------------------- 剪贴板面板


def shot_clipboard() -> str:
    w, h = 540, 420
    b = [rect(0, 0, w, h, PANEL, 16, BORDER)]
    b.append(text(20, 34, "剪贴板", 16, TEXT, 700))
    b.append(text(86, 34, "12 条", 11, FAINT, 400))
    b.append(text(w - 20, 34, "设置", 11, DIM, 500, "end"))

    # 搜索框
    b.append(rect(20, 50, w - 40, 36, SUNKEN, 9, BORDER))
    b.append(circle(40, 68, 5, "none", FAINT, 1.5) + line(44, 72, 48, 76, FAINT, 1.5))
    b.append(text(58, 73, "搜索剪贴板内容…", 11, FAINT))

    # 分页
    for i, (t, active) in enumerate([("全部", True), ("文本", False), ("图片", False), ("文件", False)]):
        x = 20 + i * 56
        b.append(text(x, 112, t, 11, TEXT if active else FAINT, 600 if active else 400))
        if active:
            b.append(rect(x - 2, 118, 28, 2, ACCENT, 1))

    rows = [
        ("文本", "https://github.com/adream2/Zhuxia", "刚刚", ACCENT),
        ("图片", "Clipboard_Screenshot.png · 1280×720", "2 分钟前", GREEN),
        ("文件", "zhuxia-0.0.1-win-x64.zip · 4.0 MB", "10 分钟前", AMBER),
        ("文本", "cargo build --release --locked", "1 小时前", PURPLE),
    ]
    for i, (kind, body, when, color) in enumerate(rows):
        y = 132 + i * 64
        b.append(rect(16, y, w - 32, 56, PANEL_2 if i % 2 == 0 else "#1A1D23", 10, BORDER))
        b.append(rect(28, y + 14, 28, 28, color, 8, op=0.16))
        b.append(rect(34, y + 22, 16, 12, "none", 3, color, 1.5))
        b.append(text(70, y + 24, kind, 10, color, 600))
        b.append(text(70, y + 41, body, 11, TEXT, 400, mono=True))
        b.append(text(w - 30, y + 24, when, 10, FAINT, 400, "end"))
        if i == 0:
            b.append(text(w - 30, y + 41, "已收藏", 10, AMBER, 500, "end"))

    return svg(w, h, "".join(b), "剪贴板历史面板")


# ---------------------------------------------------------------- 设置面板


def shot_settings() -> str:
    w, h = 540, 420
    b = [rect(0, 0, w, h, PANEL, 16, BORDER)]
    # 左侧导航
    b.append(rect(0, 0, 148, h, PANEL_2, 16))
    b.append(rect(0, 0, 16, h, PANEL_2, 16))
    b.append(text(24, 40, "驻匣 · 设置", 12, TEXT, 700))
    nav = ["通用", "外观", "采样", "截图", "关于"]
    for i, item in enumerate(nav):
        y = 70 + i * 40
        active = i == 0
        if active:
            b.append(rect(14, y - 16, 120, 30, "#252A33", 8))
        b.append(text(28, y + 3, item, 12, TEXT if active else DIM, 600 if active else 400))

    # 右侧条目
    rows = [
        ("开机自启", "随系统启动常驻进程", "on"),
        ("启动时检查更新", "读取发布页最新版本，留空则完全不联网", "on"),
        ("采样间隔", "1 秒（影响任务栏刷新频率）", "value"),
        ("任务栏形态", "悬浮卡片 / 任务栏条", "value"),
    ]
    for i, (title, hint, kind) in enumerate(rows):
        y = 46 + i * 74
        b.append(text(176, y, title, 12, TEXT, 600))
        b.append(text(176, y + 18, hint, 10, FAINT))
        if kind == "on":
            b.append(rect(452, y - 14, 44, 22, ACCENT, 11))
            b.append(circle(485, y - 3, 8, "#FFFFFF"))
        elif kind == "value":
            b.append(rect(452, y - 14, 44, 22, BORDER, 11))
            b.append(circle(463, y - 3, 8, "#8A93A2"))
        if i < len(rows) - 1:
            b.append(line(176, y + 40, 500, y + 40, BORDER))
    b.append(rect(0, 380, w, 40, PANEL_2, 0))
    b.append(text(24, 405, "v0.0.1", 10, FAINT, 400, mono=True))
    b.append(text(w - 24, 405, "Apache-2.0", 10, FAINT, 400, "end"))

    return svg(w, h, "".join(b), "设置面板")


ART = {
    "hero": hero_monitor,
    "capture": shot_capture,
    "clipboard": shot_clipboard,
    "settings": shot_settings,
}
