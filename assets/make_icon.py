#!/usr/bin/env python3
"""生成 logview 的应用图标（PNG / ICNS / ICO）。

设计意图：深色圆角方块是一扇日志窗口，里面几条横线是日志行——
一条标红代表 ERROR，一条带高亮段代表搜索命中。
元素刻意做得很少，因为图标在标题栏和任务栏里只有 16~32 像素，
细节在这个尺度上只会糊成一团。

PIL 自身不做抗锯齿，所以先在 4 倍尺寸上绘制，再缩回目标尺寸。

用法：python3 assets/make_icon.py
"""

import os

from PIL import Image, ImageDraw

OUT = os.path.dirname(os.path.abspath(__file__))
BASE = 512
SS = 4
W = BASE * SS

# 背景自上而下由浅入深，避免纯色显得扁平
BG_TOP = (37, 56, 71)
BG_BOTTOM = (18, 29, 38)

ROW = (94, 133, 164)  # 普通日志行
ERROR = (233, 76, 81)  # ERROR 行
HIT = (48, 196, 148)  # 搜索命中高亮

# 每条行的宽度占比；顺序即从上到下。长短刻意拉开，避免看起来像等长列表
ROW_FRACTIONS = [0.98, 0.70, 0.90, 0.62, 0.40]
ERROR_ROW = 1
HIT_ROW = 2


def rounded_mask(size: int, radius: int) -> Image.Image:
    """圆角矩形遮罩。

    没有直接用 ImageDraw.rounded_rectangle：Pillow 9.4 在从 (0,0) 起画时
    左上角会漏成直角。用两个矩形加四个圆拼出来，各版本行为一致。
    """
    m = Image.new("L", (size, size), 0)
    d = ImageDraw.Draw(m)
    d.rectangle([radius, 0, size - radius, size], fill=255)
    d.rectangle([0, radius, size, size - radius], fill=255)
    for cx, cy in (
        (radius, radius),
        (size - radius, radius),
        (radius, size - radius),
        (size - radius, size - radius),
    ):
        d.ellipse([cx - radius, cy - radius, cx + radius, cy + radius], fill=255)
    return m


def build() -> Image.Image:
    img = Image.new("RGBA", (W, W), (0, 0, 0, 0))

    # 渐变底
    grad = Image.new("RGBA", (W, W), (0, 0, 0, 0))
    gd = ImageDraw.Draw(grad)
    for y in range(W):
        t = y / (W - 1)
        c = tuple(round(BG_TOP[i] + (BG_BOTTOM[i] - BG_TOP[i]) * t) for i in range(3))
        gd.line([(0, y), (W, y)], fill=c + (255,))

    img.paste(grad, (0, 0), rounded_mask(W, int(W * 0.225)))

    d = ImageDraw.Draw(img)

    bar_h = W * 0.056
    gap = W * 0.070
    left = W * 0.140
    right = W * 0.860
    full = right - left
    block_h = len(ROW_FRACTIONS) * bar_h + (len(ROW_FRACTIONS) - 1) * gap
    top = (W - block_h) / 2

    def bar(y: float, x0: float, x1: float, color) -> None:
        if x1 - x0 <= 1:
            return
        d.rounded_rectangle(
            [x0, y, x1, y + bar_h], radius=bar_h / 2, fill=color + (255,)
        )

    joint = gap * 0.42  # 高亮段两侧的留白
    for i, frac in enumerate(ROW_FRACTIONS):
        y = top + i * (bar_h + gap)
        width = full * frac
        if i == HIT_ROW:
            # 一行中间挖出一段高亮，代表搜索命中的词
            hit_w = width * 0.24
            pre = width * 0.30
            bar(y, left, left + pre, ROW)
            bar(y, left + pre + joint, left + pre + joint + hit_w, HIT)
            bar(y, left + pre + joint + hit_w + joint, left + width, ROW)
        else:
            bar(y, left, left + width, ERROR if i == ERROR_ROW else ROW)

    return img


def write_icns(master: Image.Image) -> None:
    """macOS 图标包（iconutil 需要 iconset 目录结构）"""
    iconset = os.path.join(OUT, "icon.iconset")
    os.makedirs(iconset, exist_ok=True)
    for size in (16, 32, 128, 256, 512):
        master.resize((size, size), Image.LANCZOS).save(
            os.path.join(iconset, f"icon_{size}x{size}.png")
        )
        master.resize((size * 2, size * 2), Image.LANCZOS).save(
            os.path.join(iconset, f"icon_{size}x{size}@2x.png")
        )
    rc = os.system(f'iconutil -c icns "{iconset}" -o "{os.path.join(OUT, "icon.icns")}"')
    if rc != 0:
        print("iconutil 不可用，跳过 .icns")
    # iconset 只是 iconutil 的输入，产物已生成，别留在仓库里
    for name in sorted(os.listdir(iconset)):
        os.remove(os.path.join(iconset, name))
    os.rmdir(iconset)


def main() -> None:
    master = build()
    master.resize((BASE, BASE), Image.LANCZOS).save(os.path.join(OUT, "icon.png"))

    # 程序内嵌用的原始 RGBA。存原始像素而非 PNG，是为了不引入 PNG 解码器——
    # 那会让二进制多出约 2 MB。代价是这文件不能直接预览，
    # 想看效果就打开同目录的 icon-128.png。
    icon128 = master.resize((128, 128), Image.LANCZOS).convert("RGBA")
    with open(os.path.join(OUT, "icon-128.rgba"), "wb") as f:
        f.write(icon128.tobytes())

    for size in (16, 32, 64, 128, 256, 512):
        master.resize((size, size), Image.LANCZOS).save(
            os.path.join(OUT, f"icon-{size}.png")
        )
    master.resize((256, 256), Image.LANCZOS).save(
        os.path.join(OUT, "icon.ico"),
        sizes=[(16, 16), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)],
    )
    write_icns(master)
    print("图标已生成：icon.png / icon-*.png / icon.ico / icon.icns")


if __name__ == "__main__":
    main()
