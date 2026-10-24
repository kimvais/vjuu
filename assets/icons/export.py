# /// script
# dependencies = ["pillow", "resvg-py"]
# ///
"""Exports the app, favicon and tray icons from vjuu.svg: `uv run assets/icons/export.py`."""

import io
import re
from pathlib import Path

import resvg_py
from PIL import Image

_DIR = Path(__file__).parent
_TILE = '#272822'  # --brand-fg in ui/style.css

_mark = (_DIR / 'vjuu.svg').read_text(encoding='utf-8')
_viewbox = re.search(r'viewBox="([^"]+)"', _mark)[1]
_paths = ''.join(re.findall(r'<path[^>]*/>', _mark))


def _tile(*, inset: int, radius: int) -> str:
    """Returns the mark on a dark rounded square, inset px from a 1024 canvas edge."""
    side = 1024 - 2 * inset
    return (
        '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024">'
        f'<rect x="{inset}" y="{inset}" width="{side}" height="{side}" rx="{radius}" fill="{_TILE}"/>'
        f'<svg x="{inset + 24}" y="{inset + 24}" width="{side - 48}" height="{side - 48}" viewBox="{_viewbox}">'
        f'{_paths}</svg></svg>\n'
    )


def _png(svg: str, size: int) -> Image.Image:
    return Image.open(io.BytesIO(bytes(resvg_py.svg_to_bytes(svg_string=svg, width=size, height=size))))


def _ico(svg: str, name: str, sizes: list[int]) -> None:
    # render each size from the vector rather than letting Pillow downscale one bitmap
    images = [_png(svg, s) for s in sizes]
    images[-1].save(_DIR / name, sizes=[(s, s) for s in sizes], append_images=images[:-1])


def main() -> None:
    tile = _tile(inset=0, radius=224)
    (_DIR / 'vjuu-tile.svg').write_text(tile, encoding='utf-8')
    _ico(tile, 'vjuu.ico', [16, 20, 24, 32, 40, 48, 64, 128, 256])
    _ico(tile, 'favicon.ico', [16, 32, 48])
    _png(tile, 512).save(_DIR / 'vjuu-512.png')
    # iOS rounds the corners itself and turns transparency black
    _png(_tile(inset=0, radius=0), 180).save(_DIR / 'apple-touch-icon.png')
    # macOS draws app icons at ~80% of the canvas with a drop shadow margin
    _png(_tile(inset=100, radius=185), 1024).save(_DIR / 'vjuu.icns')
    # tray icons are 16-32 px: drop the mark's padding so the V gets every pixel
    x, y, side = (float(v) for v in _viewbox.split()[:3])
    tray = _mark.replace(_viewbox, f'{x + 55} {y + 55} {side - 110} {side - 110}')
    _ico(tray, 'tray.ico', [16, 20, 24, 32, 48])
    # macOS menu bar template: black + alpha, the OS tints it for light/dark
    template = re.sub(r'fill="#[0-9a-fA-F]{6}"', 'fill="#000"', tray)
    _png(template, 16).save(_DIR / 'trayTemplate.png')
    _png(template, 32).save(_DIR / 'trayTemplate@2x.png')


if __name__ == '__main__':
    main()
