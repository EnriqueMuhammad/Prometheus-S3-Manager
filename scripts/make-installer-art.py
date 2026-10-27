"""Draws the installer bitmaps (WiX dialog 493x312 and banner 493x58) in the app's colors.

    python scripts/make-installer-art.py

Writes installer/dialog.bmp and installer/banner.bmp. Needs Pillow and the Segoe UI fonts of
Windows. The bitmaps are committed, so builds do not need this script.
"""

from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parent.parent
ICON = ROOT / "apps" / "prometheus" / "assets" / "icon-256.png"
NAME = "Prometheus"
SUBTITLE = ["S3Manager"]
# Theme accent (crates/s3m-ui/src/theme.rs).
TOP = (0x0B, 0x63, 0xCE)  # accent
BOTTOM = (0x07, 0x3E, 0x82)  # darker accent
ACCENT = (0x0B, 0x63, 0xCE)
TEXT = (0xFF, 0xFF, 0xFF)
TEXT_DIM = (0xD6, 0xE4, 0xF7)

FONTS = Path("C:/Windows/Fonts")


def font(name: str, size: int) -> ImageFont.FreeTypeFont:
    return ImageFont.truetype(str(FONTS / name), size)


def icon(size: int) -> Image.Image:
    return Image.open(ICON).convert("RGBA").resize((size, size), Image.LANCZOS)


def dialog() -> Image.Image:
    # Welcome and finish pages: the left 164 px are the picture, the rest stays white for text.
    img = Image.new("RGB", (493, 312), "white")
    panel = Image.new("RGB", (164, 312))
    draw = ImageDraw.Draw(panel)
    for y in range(312):
        t = y / 311
        draw.line(
            [(0, y), (163, y)],
            fill=tuple(round(a + (b - a) * t) for a, b in zip(TOP, BOTTOM)),
        )
    logo = icon(76)
    panel.paste(logo, ((164 - 76) // 2, 44), logo)
    title = font("segoeuib.ttf", 22)
    w = draw.textlength(NAME, font=title)
    draw.text(((164 - w) / 2, 134), NAME, font=title, fill=TEXT)
    sub = font("segoeui.ttf", 12)
    for i, line in enumerate(SUBTITLE):
        w = draw.textlength(line, font=sub)
        draw.text(((164 - w) / 2, 168 + i * 17), line, font=sub, fill=TEXT_DIM)
    draw.rectangle([(0, 309), (163, 311)], fill=ACCENT)
    img.paste(panel, (0, 0))
    return img


def banner() -> Image.Image:
    # Inner pages: title text is drawn on the left by Windows Installer; the icon sits right.
    img = Image.new("RGB", (493, 58), "white")
    logo = icon(40)
    img.paste(logo, (493 - 40 - 12, 9), logo)
    ImageDraw.Draw(img).rectangle([(0, 56), (492, 57)], fill=ACCENT)
    return img


if __name__ == "__main__":
    out = ROOT / "installer"
    dialog().save(out / "dialog.bmp")
    banner().save(out / "banner.bmp")
    print("wrote", out / "dialog.bmp", "and", out / "banner.bmp")
