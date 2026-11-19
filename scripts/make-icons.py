"""Generate app icons from a square source image (a rounded tile on a plain page).

Outputs (assets/icon/):
  icon-1024.png  full-bleed master with transparent rounded corners (window icon)
  ficsv.ico      Windows icon (16..256)
  ficsv.icns     macOS icon (content inset to Apple's 824/1024 grid)

Usage: python3 -I scripts/make-icons.py assets/icon/source.png assets/icon
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

# Measure the corner radius: walk the diagonal from the top-left corner (the page
# background) until the colour changes — that's where the rounded tile begins.
px = img.load()
page = px[0, 0]
d = 0
while d < W // 2 and sum(abs(a - b) for a, b in zip(px[d, d], page)) < 30:
    d += 1
radius = d / (1 - 1 / math.sqrt(2)) if d else W * 0.09
# A dark outline stroke around the tile gets trimmed off; a plain tile edge only needs 1px.
inset = 4 if sum(px[d, d]) < 200 * 3 else 1
tile_color = px[W // 2, inset + 4]  # the tile's own background, used to fill the macOS shape
print(f"corner hit at {d}px -> radius ~{radius:.0f}px, tile colour {tile_color}")

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

# macOS .icns: macOS 26 only shows an icon as-is when its shape is Apple's continuous-corner
# rounded square (824px body on a 1024px canvas); anything else gets put in a grey tile.
# Fill that exact shape (mask rendered by scripts/squircle-mask.swift) with the artwork.
mask_path = out / "macos-mask.png"
if not mask_path.exists():
    subprocess.run(["swift", str(Path(__file__).with_name("squircle-mask.swift")), str(mask_path)], check=True)
squircle = Image.open(mask_path).getchannel("A")
# The tile (corners filled with its own background colour) scaled to Apple's 824px body;
# the squircle mask then gives it macOS's corner shape.
art = Image.new("RGBA", master.size, (*tile_color[:3], 255))
art.alpha_composite(master)
art = art.resize((824, 824), Image.LANCZOS)
mac = Image.new("RGBA", (1024, 1024), (*tile_color[:3], 255))
mac.paste(art, (100, 100))
mac.putalpha(squircle)
mac.save(out / "macos-1024.png")
with tempfile.TemporaryDirectory() as t:
    iconset = Path(t) / "ficsv.iconset"
    iconset.mkdir()
    for size in (16, 32, 128, 256, 512):
        mac.resize((size, size), Image.LANCZOS).save(iconset / f"icon_{size}x{size}.png")
        mac.resize((size * 2, size * 2), Image.LANCZOS).save(iconset / f"icon_{size}x{size}@2x.png")
    subprocess.run(["iconutil", "-c", "icns", str(iconset), "-o", str(out / "ficsv.icns")], check=True)
print("wrote", ", ".join(sorted(p.name for p in out.iterdir())))
