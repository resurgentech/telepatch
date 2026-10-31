"""Build docs/hero.svg from telepath simple.jpg and computer.jpg.

Run from an isolated venv with pillow, numpy and potracer:
    uv venv .venv && uv pip install --python .venv/bin/python pillow numpy potracer
    .venv/bin/python -I docs/hero.py docs docs/hero.svg
"""
import sys
import numpy as np
from PIL import Image, ImageFilter
import potrace

DOCS = sys.argv[1]
OUT = sys.argv[2]
GREEN = "#36e836"


UP = 3  # trace at 3x: the JPEGs are 1152 px wide and their edges are ragged


def channel(arr):
    """A 0..255 channel, upscaled and lightly blurred so the threshold edge is smooth."""
    im = Image.fromarray(np.clip(arr, 0, 255).astype(np.uint8), "L")
    im = im.resize((im.width * UP, im.height * UP), Image.LANCZOS)
    im = im.filter(ImageFilter.GaussianBlur(UP * 0.7))
    return np.asarray(im).astype(int)


def layers(path):
    im = np.asarray(Image.open(path).convert("RGB")).astype(int)
    r, g, b = im[..., 0], im[..., 1], im[..., 2]
    lum = channel((r * 299 + g * 587 + b * 114) // 1000)
    greenness = channel(g - np.maximum(r, b))
    green = greenness > 110  # the glow around the text stays below this
    black = (lum < 140) & ~green
    return black, green


def frame_box(black):
    """Inner box of the drawn border: rows/cols that are nearly all ink."""
    h, w = black.shape
    rows = np.where(black.mean(axis=1) > 0.6)[0]
    cols = np.where(black.mean(axis=0) > 0.6)[0]
    top = rows[rows < h // 4].max() + 1 if (rows < h // 4).any() else 0
    bot = rows[rows > 3 * h // 4].min() if (rows > 3 * h // 4).any() else h
    left = cols[cols < w // 4].max() + 1 if (cols < w // 4).any() else 0
    right = cols[cols > 3 * w // 4].min() if (cols > 3 * w // 4).any() else w
    return left, top, right, bot


def content_box(mask, pad=8):
    ys, xs = np.where(mask)
    return max(xs.min() - pad, 0), max(ys.min() - pad, 0), xs.max() + pad, ys.max() + pad


def trace(mask, turd=4 * UP * UP):
    bmp = potrace.Bitmap(~mask)  # potracer: zero is ink
    path = bmp.trace(turdsize=turd, alphamax=1.0, opticurve=True, opttolerance=0.6)
    parts = []
    for curve in path:
        sp = curve.start_point
        d = [f"M{sp.x:.1f} {sp.y:.1f}"]
        for seg in curve:
            e = seg.end_point
            if seg.is_corner:
                c = seg.c
                d.append(f"L{c.x:.1f} {c.y:.1f}L{e.x:.1f} {e.y:.1f}")
            else:
                c1, c2 = seg.c1, seg.c2
                d.append(f"C{c1.x:.1f} {c1.y:.1f} {c2.x:.1f} {c2.y:.1f} {e.x:.1f} {e.y:.1f}")
        d.append("Z")
        parts.append("".join(d))
    return "".join(parts)


def crop(mask, box):
    l, t, r, b = box
    return mask[t:b, l:r]


# Man: drop the drawn frame.
m_black, m_green = layers(f"{DOCS}/telepath simple.jpg")
mb = frame_box(m_black)
mb = (mb[0] + 6 * UP, mb[1] + 6 * UP, mb[2] - 6 * UP, mb[3] - 6 * UP)
m_black, m_green = crop(m_black, mb), crop(m_green, mb)
m_h, m_w = m_black.shape

# Computer: crop to its ink.
c_black, c_green = layers(f"{DOCS}/computer.jpg")
cb = content_box(c_black | c_green, pad=12 * UP)
c_black, c_green = crop(c_black, cb), crop(c_green, cb)
c_h, c_w = c_black.shape

print(f"man {m_w}x{m_h} box {mb}; computer {c_w}x{c_h} box {cb}", file=sys.stderr)

man_black = trace(m_black)
man_green = trace(m_green, turd=2 * UP * UP)
comp_black = trace(c_black)
comp_green = trace(c_green, turd=2 * UP * UP)

# Canvas.
W, H = 2200, 1000
M = 40  # margin
IN = 150  # both figures step in from the edges toward the middle

# Man fills the height on the left.
ms = (H - 2 * M) / m_h
mx, my = M + IN, M
man_right = mx + m_w * ms

# Computer sits on the right, a little smaller, floor-aligned with the man.
cs = (H - 2 * M) * 0.9 / c_h
cx = W - M - IN - c_w * cs
cy = H - M - c_h * cs

# Temple: where the fingers touch, in man-image pixels.
tx, ty = m_w * 0.74, m_h * 0.46
TX, TY = mx + tx * ms, my + ty * ms
# Screen centre in computer pixels.
sx, sy = c_w * 0.5, c_h * 0.36
SX, SY = cx + sx * cs, cy + sy * cs

# Waves: arcs centred on the temple, opening toward the screen.
import math
ang = math.atan2(SY - TY, SX - TX)
waves = []
n = 7
gap = ((cx - TX) - 30) / n
for i in range(1, n + 1):
    rr = gap * i
    half = math.radians(26 - i * 1.2)
    a0, a1 = ang - half, ang + half
    x0, y0 = TX + rr * math.cos(a0), TY + rr * math.sin(a0)
    x1, y1 = TX + rr * math.cos(a1), TY + rr * math.sin(a1)
    op = 0.95 - i * 0.09
    sw = 9 - i * 0.6
    waves.append(
        f'<path d="M{x0:.1f} {y0:.1f}A{rr:.1f} {rr:.1f} 0 0 1 {x1:.1f} {y1:.1f}" '
        f'stroke="{GREEN}" stroke-width="{sw:.1f}" fill="none" stroke-linecap="round" opacity="{op:.2f}"/>'
    )

svg = f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}" width="{W}" height="{H}">
  <title>telepatch: a man controlling a computer by thought</title>
  <defs>
    <filter id="glow" x="-30%" y="-30%" width="160%" height="160%">
      <feGaussianBlur stdDeviation="14" result="b"/>
      <feMerge><feMergeNode in="b"/><feMergeNode in="SourceGraphic"/></feMerge>
    </filter>
    <radialGradient id="screenglow" cx="50%" cy="50%" r="50%">
      <stop offset="0" stop-color="{GREEN}" stop-opacity="0.35"/>
      <stop offset="1" stop-color="{GREEN}" stop-opacity="0"/>
    </radialGradient>
  </defs>
  <rect width="{W}" height="{H}" fill="#ffffff"/>
  <g id="waves" filter="url(#glow)">
    {"".join(waves)}
  </g>
  <g id="man" transform="translate({mx:.1f} {my:.1f}) scale({ms:.5f})">
    <path fill="#000" fill-rule="evenodd" d="{man_black}"/>
    <path fill="{GREEN}" filter="url(#glow)" d="{man_green}"/>
  </g>
  <g id="computer" transform="translate({cx:.1f} {cy:.1f}) scale({cs:.5f})">
    <path fill="#000" fill-rule="evenodd" d="{comp_black}"/>
    <ellipse cx="{sx:.1f}" cy="{sy:.1f}" rx="{c_w * 0.3:.1f}" ry="{c_h * 0.16:.1f}" fill="url(#screenglow)"/>
    <path fill="{GREEN}" filter="url(#glow)" d="{comp_green}"/>
  </g>
</svg>
'''
open(OUT, "w").write(svg)
print(f"wrote {OUT} ({len(svg) // 1024} KB)", file=sys.stderr)
