#!/usr/bin/env python3
"""Draws the "RTX VSR" mark used for the app icon, the window icon and the
extension's toolbar icon. Needs Pillow and a bold sans-serif font.

    python app/assets/make_icons.py

Writes app/assets/icon-256.png, app/assets/icon-64.rgba (raw RGBA for the
window) and extension/icons/icon-{16,32,48,128}.png.
"""
import os

from PIL import Image, ImageDraw, ImageFont

HERE = os.path.dirname(os.path.abspath(__file__))
EXT = os.path.join(HERE, "..", "..", "extension", "icons")
GREEN = (118, 185, 0, 255)
FONTS = ["/usr/share/fonts/opentype/fira/FiraSans-Bold.otf",
         "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf"]
FONT = next(f for f in FONTS if os.path.exists(f))


def mark(size):
    """Green rounded square with RTX over VSR, drawn at 8x and scaled down."""
    s = size * 8
    img = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    pad = s * 0.04
    d.rounded_rectangle([pad, pad, s - pad, s - pad], radius=s * 0.19, fill=GREEN)
    font = ImageFont.truetype(FONT, int(s * 0.36))
    for text, y in (("RTX", 0.31), ("VSR", 0.69)):
        d.text((s / 2, s * y), text, font=font, fill="white", anchor="mm")
    return img.resize((size, size), Image.LANCZOS)


mark(256).save(os.path.join(HERE, "icon-256.png"))
with open(os.path.join(HERE, "icon-64.rgba"), "wb") as f:
    f.write(mark(64).tobytes())
os.makedirs(EXT, exist_ok=True)
for n in (16, 32, 48, 128):
    mark(n).save(os.path.join(EXT, f"icon-{n}.png"))
