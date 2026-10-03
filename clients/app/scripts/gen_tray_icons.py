#!/usr/bin/env python3
"""生成托盘与任务栏角标图标 → src-tauri/icons/tray/。

造型取自应用图标（雪花网格）并简化，保证 16~22px 下仍清晰：
中心六角星 + 六条辐射 + 外圈六节点及连环。

  python3 clients/app/scripts/gen_tray_icons.py
"""
import math
import os

from PIL import Image, ImageDraw, ImageFont

OUT = os.path.join(os.path.dirname(__file__), "..", "src-tauri", "icons", "tray")
S = 1024  # 绘制分辨率，最终缩小以获得抗锯齿
NAVY = (18, 44, 102, 255)
WHITE = (255, 255, 255, 255)
RED = (239, 68, 68, 255)
AMBER = (245, 158, 11, 255)


def glyph(size, color, scale=1.0):
    """透明底上的雪花网格图形。"""
    im = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    d = ImageDraw.Draw(im)
    c = S / 2
    r_out = S * 0.39 * scale
    lw = int(S * 0.07 * scale)
    node = S * 0.095 * scale
    pts_out = [(c + r_out * math.cos(math.radians(90 + 60 * i)), c - r_out * math.sin(math.radians(90 + 60 * i))) for i in range(6)]
    for i in range(6):
        d.line([pts_out[i], pts_out[(i + 1) % 6]], fill=color, width=lw)
        d.line([(c, c), pts_out[i]], fill=color, width=lw)
    for (x, y) in pts_out:
        d.ellipse([x - node, y - node, x + node, y + node], fill=color)
    hub = S * 0.17 * scale
    star = []
    for i in range(12):
        rr = hub if i % 2 == 0 else hub * 0.55
        a = math.radians(90 + 30 * i)
        star.append((c + rr * math.cos(a), c - rr * math.sin(a)))
    d.polygon(star, fill=color)
    return im


def tile(bg):
    """彩色托盘底：圆角方块 + 白色图形。"""
    im = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    d = ImageDraw.Draw(im)
    pad = S * 0.04
    d.rounded_rectangle([pad, pad, S - pad, S - pad], radius=S * 0.22, fill=bg)
    im.alpha_composite(glyph(S, WHITE, 0.92))
    return im


def badge_dot(im, color, cutout=True):
    """右上角状态圆点（带透明描边，任何底色上都清晰）。"""
    d = ImageDraw.Draw(im)
    r = S * 0.17
    cx, cy = S - r - S * 0.02, r + S * 0.02
    if cutout:
        ring = r * 1.28
        mask = Image.new("L", (S, S), 0)
        ImageDraw.Draw(mask).ellipse([cx - ring, cy - ring, cx + ring, cy + ring], fill=255)
        clear = Image.new("RGBA", (S, S), (0, 0, 0, 0))
        im.paste(clear, (0, 0), mask)
    d.ellipse([cx - r, cy - r, cx + r, cy + r], fill=color)
    return im


def moon(im, color, cutout=True):
    """右下角月亮（勿扰）。"""
    r = S * 0.20
    cx, cy = S - r - S * 0.01, S - r - S * 0.01
    if cutout:
        ring = r * 1.2
        mask = Image.new("L", (S, S), 0)
        ImageDraw.Draw(mask).ellipse([cx - ring, cy - ring, cx + ring, cy + ring], fill=255)
        im.paste(Image.new("RGBA", (S, S), (0, 0, 0, 0)), (0, 0), mask)
    m = Image.new("L", (S, S), 0)
    md = ImageDraw.Draw(m)
    md.ellipse([cx - r, cy - r, cx + r, cy + r], fill=255)
    off = r * 0.55
    md.ellipse([cx - r + off, cy - r - off * 0.5, cx + r + off, cy + r - off * 0.5], fill=0)
    im.paste(Image.new("RGBA", (S, S), color), (0, 0), m)
    return im


def gray(im):
    g = im.convert("LA").convert("RGBA")
    px = g.load()
    for y in range(g.height):
        for x in range(g.width):
            r, gg, b, a = px[x, y]
            px[x, y] = (r, gg, b, int(a * 0.75))
    return g


def save(im, name, size):
    im.resize((size, size), Image.LANCZOS).save(os.path.join(OUT, name), optimize=True)


def font(px):
    for p in ["/System/Library/Fonts/SFNS.ttf", "/System/Library/Fonts/Helvetica.ttc", "/Library/Fonts/Arial Bold.ttf"]:
        if os.path.exists(p):
            try:
                return ImageFont.truetype(p, px)
            except OSError:
                pass
    return ImageFont.load_default()


def count_badge(text):
    """Windows 任务栏叠加角标：红底白字圆。"""
    im = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    d = ImageDraw.Draw(im)
    d.ellipse([S * 0.02, S * 0.02, S * 0.98, S * 0.98], fill=RED, outline=WHITE, width=int(S * 0.06))
    f = font(int(S * (0.62 if len(text) == 1 else 0.46)))
    try:
        f.set_variation_by_name("Bold")
    except Exception:
        pass
    bb = d.textbbox((0, 0), text, font=f)
    w, h = bb[2] - bb[0], bb[3] - bb[1]
    d.text(((S - w) / 2 - bb[0], (S - h) / 2 - bb[1]), text, font=f, fill=WHITE)
    return im


def main():
    os.makedirs(OUT, exist_ok=True)
    # macOS 菜单栏模板图（黑色 + alpha，系统按明暗自动着色）
    black = (0, 0, 0, 255)
    save(glyph(S, black), "mac-normal.png", 44)
    save(badge_dot(glyph(S, black), black), "mac-unread.png", 44)
    off = glyph(S, black)
    off.putalpha(off.getchannel("A").point(lambda a: int(a * 0.4)))
    save(off, "mac-offline.png", 44)
    save(moon(glyph(S, black), black), "mac-dnd.png", 44)
    # Windows / Linux 彩色托盘图
    save(tile(NAVY), "normal.png", 64)
    save(badge_dot(tile(NAVY), RED), "unread.png", 64)
    save(gray(tile(NAVY)), "offline.png", 64)
    save(moon(tile(NAVY), AMBER), "dnd.png", 64)
    Image.new("RGBA", (64, 64), (0, 0, 0, 0)).save(os.path.join(OUT, "blank.png"))
    # Windows 任务栏叠加角标
    for n in range(1, 10):
        save(count_badge(str(n)), f"badge-{n}.png", 32)
    save(count_badge("9+"), "badge-more.png", 32)
    print("written to", os.path.abspath(OUT))


if __name__ == "__main__":
    main()
