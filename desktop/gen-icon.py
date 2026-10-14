#!/usr/bin/env python3
"""生成应用图标源图 icons/icon.png（1024x1024，圆角方块 + AH 字样）。
依赖 Pillow；产物交由 `tauri icon` 生成全套（含 icns）。"""
from PIL import Image, ImageDraw, ImageFont

SIZE = 1024
RADIUS = 232  # 近 macOS squircle 比例
TOP = (44, 52, 72)      # 深蓝灰
BOTTOM = (24, 28, 40)   # 近黑
ACCENT = (86, 196, 187) # 青

img = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))
# 垂直渐变（逐行插值）+ 圆角遮罩
grad = Image.new("RGBA", (SIZE, SIZE))
gd = ImageDraw.Draw(grad)
for y in range(SIZE):
    t = y / SIZE
    c = tuple(int(TOP[i] + (BOTTOM[i] - TOP[i]) * t) for i in range(3)) + (255,)
    gd.line([(0, y), (SIZE, y)], fill=c)
mask = Image.new("L", (SIZE, SIZE), 0)
ImageDraw.Draw(mask).rounded_rectangle([0, 0, SIZE - 1, SIZE - 1], RADIUS, fill=255)
img.paste(grad, (0, 0), mask)

d = ImageDraw.Draw(img)
font = None
for cand, idx in [("/System/Library/Fonts/Helvetica.ttc", 1),
                  ("/System/Library/Fonts/SFNS.ttf", 0)]:
    try:
        font = ImageFont.truetype(cand, 430, index=idx)
        break
    except OSError:
        continue
assert font is not None, "找不到可用系统字体"
bbox = d.textbbox((0, 0), "AH", font=font)
w, h = bbox[2] - bbox[0], bbox[3] - bbox[1]
x, y = (SIZE - w) / 2 - bbox[0], (SIZE - h) / 2 - bbox[1] - 30
d.text((x, y), "AH", font=font, fill=(245, 247, 250, 255))
# 字下青色短线点缀
d.rounded_rectangle([(SIZE - 300) / 2, y + h + 60, (SIZE + 300) / 2, y + h + 92], 16, fill=ACCENT)

import pathlib
out = pathlib.Path(__file__).parent / "src-tauri" / "icons" / "icon.png"
out.parent.mkdir(parents=True, exist_ok=True)
img.save(out)
print(f"icon → {out}")
