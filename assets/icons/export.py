# /// script
# dependencies = ["pillow", "resvg-py"]
# ///
"""Exports the app icons, the tray bitmap, and the site's favicons and logo, from vjuu.svg: `uv run assets/icons/export.py`."""

import io
import re
from pathlib import Path

import resvg_py
from PIL import Image

_DIR = Path(__file__).parent
_SITE = _DIR.parent.parent / 'site'
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
    _png(tile, 512).save(_DIR / 'vjuu-512.png')
    (_SITE / 'favicon.svg').write_text(tile, encoding='utf-8')
    (_SITE / 'img' / 'vjuu.svg').write_text(_mark, encoding='utf-8')
    _ico(tile, '../../site/favicon.ico', [16, 32, 48])
    # iOS rounds the corners itself and turns transparency black
    _png(_tile(inset=0, radius=0), 180).save(_SITE / 'apple-touch-icon.png')
    # macOS draws app icons at ~80% of the canvas with a drop shadow margin
    _png(_tile(inset=100, radius=185), 1024).save(_DIR / 'vjuu.icns')
    # raw RGBA, so the binary needs no PNG decoder; src/tray.rs loads it as 32x32
    (_DIR / 'tray.rgba').write_bytes(_png(tile, 32).convert('RGBA').tobytes())


if __name__ == '__main__':
    main()
