"""生成页面用的位图素材。

界面截图是**真机截图**，放在 site/shots/ 下（见那里的 README）：截图必须和软件
长得一样，画出来的示意图迟早和实际界面跑偏。

这里只负责现场生成社交分享图（og:image）：CI 上不能指望有 Pillow / ImageMagick，
也不该为了中文标题往仓库里塞几 MB 字体，所以用标准库自己写 PNG：
渐变底 + 网格 + 居中的应用图标，标题交给 og:title。
"""

from __future__ import annotations

import binascii
import struct
import zlib


def _png_write(path, w: int, h: int, buf: bytearray) -> None:
    raw = bytearray()
    stride = w * 4
    for y in range(h):
        raw.append(0)  # filter: None
        raw += buf[y * stride:(y + 1) * stride]

    def chunk(tag: bytes, data: bytes) -> bytes:
        return (
            struct.pack(">I", len(data))
            + tag
            + data
            + struct.pack(">I", binascii.crc32(tag + data) & 0xFFFFFFFF)
        )

    png = b"\x89PNG\r\n\x1a\n"
    png += chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(bytes(raw), 9))
    png += chunk(b"IEND", b"")
    with open(path, "wb") as f:
        f.write(png)


def _png_read(path) -> tuple[int, int, bytearray]:
    """只支持 8bit RGBA、非隔行的 PNG —— 仓库里的图标正好是这种。"""
    d = open(path, "rb").read()
    w, h, depth, ctype, _, _, inter = struct.unpack(">IIBBBBB", d[16:29])
    if depth != 8 or ctype != 6 or inter != 0:
        raise ValueError("图标 PNG 格式不在支持范围（需要 8bit RGBA 非隔行）")
    idat = bytearray()
    off = 8
    while off < len(d):
        ln = struct.unpack(">I", d[off:off + 4])[0]
        tag = d[off + 4:off + 8]
        body = d[off + 8:off + 8 + ln]
        if tag == b"IDAT":
            idat += body
        off += 12 + ln
    raw = zlib.decompress(bytes(idat))
    stride = w * 4
    out = bytearray(stride * h)
    prev = bytearray(stride)
    pos = 0
    for y in range(h):
        f = raw[pos]
        pos += 1
        row = bytearray(raw[pos:pos + stride])
        pos += stride
        if f == 1:
            for i in range(4, stride):
                row[i] = (row[i] + row[i - 4]) & 0xFF
        elif f == 2:
            for i in range(stride):
                row[i] = (row[i] + prev[i]) & 0xFF
        elif f == 3:
            for i in range(stride):
                left = row[i - 4] if i >= 4 else 0
                row[i] = (row[i] + ((left + prev[i]) >> 1)) & 0xFF
        elif f == 4:
            for i in range(stride):
                a = row[i - 4] if i >= 4 else 0
                b = prev[i]
                c = prev[i - 4] if i >= 4 else 0
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                pr = a if (pa <= pb and pa <= pc) else (b if pb <= pc else c)
                row[i] = (row[i] + pr) & 0xFF
        out[y * stride:(y + 1) * stride] = row
        prev = row
    return w, h, out


class _Raster:
    def __init__(self, w: int, h: int):
        self.w, self.h = w, h
        self.buf = bytearray(w * h * 4)

    def px(self, x: int, y: int, r: int, g: int, b: int, a: int = 255) -> None:
        if a <= 0 or not (0 <= x < self.w and 0 <= y < self.h):
            return
        i = (y * self.w + x) * 4
        if a >= 255:
            self.buf[i:i + 4] = bytes((r, g, b, 255))
        else:
            k = a / 255.0
            self.buf[i] = int(self.buf[i] * (1 - k) + r * k)
            self.buf[i + 1] = int(self.buf[i + 1] * (1 - k) + g * k)
            self.buf[i + 2] = int(self.buf[i + 2] * (1 - k) + b * k)
            self.buf[i + 3] = 255

    def vgrad(self, y0: int, y1: int, c0, c1) -> None:
        for y in range(max(0, y0), min(self.h, y1)):
            t = (y - y0) / max(1, y1 - y0)
            row = bytes(
                (
                    int(c0[0] + (c1[0] - c0[0]) * t),
                    int(c0[1] + (c1[1] - c0[1]) * t),
                    int(c0[2] + (c1[2] - c0[2]) * t),
                    255,
                )
            ) * self.w
            i = y * self.w * 4
            self.buf[i:i + self.w * 4] = row

    def glow(self, cx: float, cy: float, radius: float, color, strength: float) -> None:
        r2 = radius * radius
        for y in range(max(0, int(cy - radius)), min(self.h, int(cy + radius))):
            dy = y - cy
            for x in range(max(0, int(cx - radius)), min(self.w, int(cx + radius))):
                dx = x - cx
                d2 = dx * dx + dy * dy
                if d2 >= r2:
                    continue
                f = (1 - (d2 / r2) ** 0.5) ** 2 * strength
                i = (y * self.w + x) * 4
                self.buf[i] = min(255, int(self.buf[i] + color[0] * f))
                self.buf[i + 1] = min(255, int(self.buf[i + 1] + color[1] * f))
                self.buf[i + 2] = min(255, int(self.buf[i + 2] + color[2] * f))

    def paste(self, src: bytearray, sw: int, sh: int, x0: int, y0: int, size: int) -> None:
        """最近邻缩放 + alpha 合成（图标是矢量导出的，缩小后边缘仍然干净）。"""
        for y in range(size):
            sy = min(sh - 1, y * sh // size)
            for x in range(size):
                sx = min(sw - 1, x * sw // size)
                si = (sy * sw + sx) * 4
                a = src[si + 3]
                if a:
                    self.px(x0 + x, y0 + y, src[si], src[si + 1], src[si + 2], a)


def og_image(icon_png, out_path, width: int = 1200, height: int = 630) -> None:
    """生成社交分享图：深色渐变 + 两团色光 + 网格 + 居中的应用图标。"""
    r = _Raster(width, height)
    r.vgrad(0, height, (10, 12, 16), (16, 22, 33))
    r.glow(width * 0.16, -height * 0.1, 620, (46, 144, 250), 0.55)
    r.glow(width * 0.9, height * 0.1, 560, (168, 85, 247), 0.34)

    step = 60
    for x in range(0, width, step):
        for y in range(height):
            i = (y * width + x) * 4
            r.buf[i] = min(255, r.buf[i] + 6)
            r.buf[i + 1] = min(255, r.buf[i + 1] + 6)
            r.buf[i + 2] = min(255, r.buf[i + 2] + 8)
    for y in range(0, height, step):
        for x in range(width):
            i = (y * width + x) * 4
            r.buf[i] = min(255, r.buf[i] + 6)
            r.buf[i + 1] = min(255, r.buf[i + 1] + 6)
            r.buf[i + 2] = min(255, r.buf[i + 2] + 8)

    size = 300
    cx, cy = width // 2, height // 2
    r.glow(cx, cy, 340, (46, 144, 250), 0.30)
    sw, sh, src = _png_read(icon_png)
    r.paste(src, sw, sh, cx - size // 2, cy - size // 2, size)

    _png_write(out_path, width, height, r.buf)
