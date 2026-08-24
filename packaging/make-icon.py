#!/usr/bin/env python3
"""Generate the application icon.

The mark is a dual-track level meter: bars grow upward from the centre line for
one track and downward for the other, with different profiles so the two read as
independent. That is the whole point of the product, so it is what the icon says.

    python3 packaging/make-icon.py
"""

from pathlib import Path

from PIL import Image, ImageDraw

MASTER = 1024
SIZES = [16, 24, 32, 48, 64, 128, 256, 512]
OUT = Path(__file__).resolve().parent / "icons"

BG = (26, 29, 36, 255)
BG_EDGE = (44, 49, 60, 255)
UP = (104, 186, 128, 255)     # 我：与界面里电平表的绿一致
DOWN = (91, 157, 217, 255)    # 对方
AXIS = (58, 64, 78, 255)

# 上下两组高度刻意不同：一条轨说话时另一条通常是安静的
UP_H = [0.34, 0.72, 1.00, 0.52, 0.28]
DOWN_H = [0.46, 0.24, 0.62, 0.92, 0.40]


def draw_master() -> Image.Image:
    img = Image.new("RGBA", (MASTER, MASTER), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)

    pad = MASTER * 0.045
    radius = MASTER * 0.225
    d.rounded_rectangle(
        [pad, pad, MASTER - pad, MASTER - pad],
        radius=radius,
        fill=BG,
        outline=BG_EDGE,
        width=int(MASTER * 0.008),
    )

    n = len(UP_H)
    field = MASTER * 0.60
    left = (MASTER - field) / 2
    slot = field / n
    bar_w = slot * 0.54
    bar_r = bar_w / 2
    mid = MASTER / 2
    gap = MASTER * 0.018          # 中线两侧留缝，上下两组才分得开
    reach = MASTER * 0.215

    d.line([(left, mid), (left + field, mid)], fill=AXIS, width=int(MASTER * 0.006))

    for i in range(n):
        cx = left + slot * (i + 0.5)
        x0, x1 = cx - bar_w / 2, cx + bar_w / 2

        top = mid - gap - reach * UP_H[i]
        d.rounded_rectangle([x0, top, x1, mid - gap], radius=bar_r, fill=UP)

        bottom = mid + gap + reach * DOWN_H[i]
        d.rounded_rectangle([x0, mid + gap, x1, bottom], radius=bar_r, fill=DOWN)

    return img


def main() -> None:
    OUT.mkdir(parents=True, exist_ok=True)
    master = draw_master()
    master.save(OUT / "icon-1024.png")
    for s in SIZES:
        master.resize((s, s), Image.LANCZOS).save(OUT / f"icon-{s}.png")
    print(f"wrote {len(SIZES) + 1} PNGs to {OUT}")


if __name__ == "__main__":
    main()
