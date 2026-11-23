"""生成驻匣图标：小尺寸（16–32）用紧凑版，大尺寸沿用原设计。

两个要点：
1. 原设计在 1024 画布里留了 5% 外边距、圆角 22%，缩到 16px 时四角几乎被吃光，
   托盘里看起来比旁边的系统图标"小一圈"。紧凑版改成满幅 + 圆角 15% + 匣体放大。
2. 16–128 层写成 **32bpp BMP(DIB)**，只有 256 层用 PNG。
   Windows 的 LoadImage / 托盘对 PNG 压缩的小尺寸层支持并不可靠（会退化成空白图标）。

用法：python tools/make_icons.py
"""

import os
import struct
from PIL import Image, ImageDraw

HERE = os.path.dirname(os.path.abspath(__file__))
ICON_DIR = os.path.join(os.path.dirname(HERE), "assets", "icons")
PNG_DIR = os.path.join(ICON_DIR, "png")

INK_TOP = (27, 39, 59)
INK_BOT = (9, 14, 22)
MINT_TOP = (116, 242, 214)
MINT_BOT = (30, 186, 160)
FRAME = (234, 241, 250)


def vgrad(size, c1, c2):
    g = Image.new("RGBA", (1, size))
    for y in range(size):
        t = y / max(1, size - 1)
        g.putpixel((0, y), tuple(int(c1[i] + (c2[i] - c1[i]) * t) for i in range(3)) + (255,))
    return g.resize((size, size), Image.BILINEAR)


def rounded_mask(size, radius, box=None):
    m = Image.new("L", (size, size), 0)
    d = ImageDraw.Draw(m)
    b = box or (0, 0, size - 1, size - 1)
    d.rounded_rectangle(b, radius=radius, fill=255)
    return m


def compact(size, ss=16):
    """满幅底板 + 放大的匣体，专供 16–32px。"""
    S = size * ss
    img = Image.new("RGBA", (S, S), (0, 0, 0, 0))

    base = vgrad(S, INK_TOP, INK_BOT)
    img.paste(base, (0, 0), rounded_mask(S, int(S * 0.15)))

    d = ImageDraw.Draw(img)
    x0, x1 = int(S * 0.19), int(S * 0.81)
    w = max(1, int(S * 0.075))
    d.rounded_rectangle([x0, x0, x1, x1], radius=int(S * 0.055),
                        outline=FRAME + (255,), width=w)

    m0, m1 = int(S * 0.365), int(S * 0.635)
    mint = vgrad(m1 - m0, MINT_TOP, MINT_BOT)
    img.paste(mint, (m0, m0), rounded_mask(m1 - m0, int(S * 0.028)))

    return img.resize((size, size), Image.LANCZOS)


def dib(im):
    """32bpp BMP 层（BITMAPINFOHEADER + BGRA 自下而上 + AND mask）。"""
    w, h = im.size
    px = im.load()
    rows = []
    for y in range(h - 1, -1, -1):
        row = bytearray()
        for x in range(w):
            r, g, b, a = px[x, y]
            row += bytes((b, g, r, a))
        rows.append(bytes(row))
    body = b"".join(rows)
    # AND mask：1bpp，按 alpha 阈值置位；行按 4 字节对齐
    mask_row = ((w + 31) // 32) * 4
    mask = bytearray()
    for y in range(h - 1, -1, -1):
        line = bytearray(mask_row)
        for x in range(w):
            if px[x, y][3] < 128:
                line[x // 8] |= 0x80 >> (x % 8)
        mask += line
    hdr = struct.pack("<IiiHHIIiiII", 40, w, h * 2, 1, 32, 0, len(body), 0, 0, 0, 0)
    return hdr + body + bytes(mask)


def png_bytes(im):
    import io as _io
    buf = _io.BytesIO()
    im.save(buf, format="PNG", optimize=True)
    return buf.getvalue()


def write_ico(path, frames):
    """frames: [(size, PIL.Image)]，按尺寸升序。"""
    blobs = []
    for size, im in frames:
        data = png_bytes(im) if size >= 256 else dib(im)
        blobs.append((size, data))

    out = bytearray(struct.pack("<HHH", 0, 1, len(blobs)))
    offset = 6 + 16 * len(blobs)
    for size, data in blobs:
        dim = 0 if size >= 256 else size
        out += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
    for _, data in blobs:
        out += data
    with open(path, "wb") as f:
        f.write(out)


def main():
    small = [16, 20, 24, 32]
    big = [40, 48, 64, 128, 256]
    frames = []

    for s in small:
        im = compact(s, ss=16 if s <= 24 else 8)
        im.save(os.path.join(PNG_DIR, f"zhuxia-{s}.png"))
        frames.append((s, im))
        print(f"compact {s}x{s}")

    for s in big:
        p = os.path.join(PNG_DIR, f"zhuxia-{s}.png")
        if os.path.exists(p):
            frames.append((s, Image.open(p).convert("RGBA")))
            print(f"keep    {s}x{s}")

    frames.sort(key=lambda t: t[0])
    out = os.path.join(ICON_DIR, "zhuxia.ico")
    write_ico(out, frames)
    print("wrote", out, os.path.getsize(out), "bytes")


if __name__ == "__main__":
    main()
