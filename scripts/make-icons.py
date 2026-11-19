"""Generate app icons from icon.png.

Outputs (assets/icon/):
  icon-1024.png  full-bleed master with transparent rounded corners (window icon)
  ficsv.ico      Windows icon (16..256)
  ficsv.icns     macOS icon (content inset to Apple's 824/1024 grid)

Usage: python3 -I scripts/make-icons.py icon.png assets/icon
"""
import math
import subprocess
import sys
import tempfile
from pathlib import Path

from PIL import Image, ImageDraw

src, out = Path(sys.argv[1]), Path(sys.argv[2])
out.mkdir(parents=True, exist_ok=True)
img = Image.open(src).convert("RGB")
W, H = img.size
assert W == H, "icon must be square"

# Measure the corner radius: walk the diagonal from the top-left until we hit the dark outline.
px = img.load()
d = 0
while d < W // 2 and sum(px[d, d]) > 200 * 3:
    d += 1
radius = d / (1 - 1 / math.sqrt(2)) if d else W * 0.09
inset = 4  # drop the thin outline stroke
print(f"corner hit at {d}px -> radius ~{radius:.0f}px")

# Anti-aliased rounded-rect mask (4x supersampled).
S = 4
mask = Image.new("L", (W * S, H * S), 0)
ImageDraw.Draw(mask).rounded_rectangle(
    (inset * S, inset * S, (W - inset) * S - 1, (H - inset) * S - 1), radius=(radius - inset) * S, fill=255
)
mask = mask.resize((W, H), Image.LANCZOS)
rgba = img.convert("RGBA")
rgba.putalpha(mask)
bbox = rgba.getbbox()
rgba = rgba.crop(bbox)

master = rgba.resize((1024, 1024), Image.LANCZOS)
master.save(out / "icon-1024.png")
master.resize((256, 256), Image.LANCZOS).save(out / "icon-256.png")

# Windows .ico
master.save(out / "ficsv.ico", sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])

# macOS .icns: Apple's grid puts the rounded square at 824/1024, centred.
mac = Image.new("RGBA", (1024, 1024), (0, 0, 0, 0))
body = master.resize((824, 824), Image.LANCZOS)
mac.alpha_composite(body, (100, 100))
with tempfile.TemporaryDirectory() as t:
    iconset = Path(t) / "ficsv.iconset"
    iconset.mkdir()
    for size in (16, 32, 128, 256, 512):
        mac.resize((size, size), Image.LANCZOS).save(iconset / f"icon_{size}x{size}.png")
        mac.resize((size * 2, size * 2), Image.LANCZOS).save(iconset / f"icon_{size}x{size}@2x.png")
    subprocess.run(["iconutil", "-c", "icns", str(iconset), "-o", str(out / "ficsv.icns")], check=True)
print("wrote", ", ".join(sorted(p.name for p in out.iterdir())))
