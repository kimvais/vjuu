"""Renders the vjuu.it screenshots in site/img/ from ui/, in headless Edge, the way tests/ui_test.py drives it.

Run: uv run --no-project --with playwright python tests/screenshots.py
"""

import json
import shutil
import subprocess
import tempfile
import threading
from functools import partial
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

from playwright.sync_api import sync_playwright

_ROOT = Path(__file__).resolve().parent.parent
_IMG = _ROOT / 'site' / 'img'
_STUB = 'window.ipc = { postMessage: () => {} };'
_PLAN = """# Plan: resident window

The first `vjuu` opens its own window. Every later call should appear at once.

## Approach

1. Start a hidden resident instance after the first review.
2. Keep one pre-warmed window with fonts and renderers loaded.
3. Hand each new document to it over a named pipe.

## Risks

- WebView2 costs about 300 MB while resident.
- A stale pipe after a crash must not block the next call.

## Out of scope

Tabs, persistent history and remote review.
"""
# Every shipped theme, by file name, with the name the site and the README show; also their order there.
_THEMES = {
    'light': 'GitHub Light',
    'github-dark': 'GitHub Dark',
    'graphite': 'Graphite',
    'gitlab-light': 'GitLab Light',
    'gitlab-dark': 'GitLab Dark',
    'solarized-light': 'Solarized Light',
    'solarized-dark': 'Solarized Dark',
    'gruvbox-light': 'Gruvbox Light',
    'gruvbox-dark': 'Gruvbox Dark',
    'catppuccin-latte': 'Catppuccin Latte',
    'catppuccin-mocha': 'Catppuccin Mocha',
    'dracula': 'Dracula',
    'one-dark': 'One Dark',
    'nord': 'Nord',
    'tokyo-night': 'Tokyo Night',
}
_THEME_PLAN = """# Plan: pipe timeout

Give the resident instance's pipe a timeout, so a stale one cannot block the next call.

```choice
Which timeout?
1. An hour: as the hooks
2. A minute: fail fast
```
"""
_THEME_DIFF = """diff --git a/src/pipe.rs b/src/pipe.rs
--- a/src/pipe.rs
+++ b/src/pipe.rs
@@ -1,4 +1,6 @@
 /// Opens the resident instance's pipe.
-pub fn open(name: &str) -> io::Result<Pipe> {
-    Pipe::connect(name)
+pub fn open(name: &str, timeout: u64) -> io::Result<Pipe> {
+    let pipe = Pipe::connect(name)?; // "vjuu"
+    pipe.set_timeout(Duration::from_secs(timeout))?;
+    Ok(pipe)
 }
"""
_SHEET = """<!doctype html><meta charset="utf-8"><style>
body {{ margin: 0; padding: 24px; background: #1f2328; font: 600 20px system-ui, sans-serif; color: #fff; }}
main {{ display: grid; grid-template-columns: repeat(3, 600px); gap: 24px; }}
figure {{ margin: 0; }} figcaption {{ margin: 0 0 8px; }} img {{ display: block; width: 600px; border-radius: 6px; }}
</style><main>{}</main>"""


class _QuietHandler(SimpleHTTPRequestHandler):
    def log_message(self, *args):
        pass


def _comment(page, text):
    page.keyboard.type(text)
    page.keyboard.press('Control+Enter')


def _shoot(page, url, work, *docs):
    (work / 'doc.json').write_text(json.dumps({'docs': list(docs)}))
    page.goto(url)
    page.wait_for_selector('[data-key]')
    page.evaluate('document.fonts.ready')
    return page


def _themes(browser, url, work):
    """Renders one shot per shipped theme for the site's thumbnails, and tiles them into one sheet for the README."""
    shipped = {css.stem for css in (_ROOT / 'themes').glob('*.css')}
    if shipped != _THEMES.keys():
        raise ValueError(f'Themes without a shot, or shots without a theme: `{shipped ^ _THEMES.keys()}`!')
    out = _IMG / 'themes'
    out.mkdir(exist_ok=True)
    sheet = work / 'sheet'
    sheet.mkdir()
    for name in _THEMES:
        # A page of its own per theme: one page would keep the first theme.css in its cache.
        page = browser.new_page(viewport={'width': 1280, 'height': 800})
        page.add_init_script(_STUB)
        shutil.copy(_ROOT / 'themes' / f'{name}.css', work / 'theme.css')
        _shoot(
            page,
            url,
            work,
            {'name': 'docs/plan.md', 'kind': 'md', 'text': _THEME_PLAN},
            {'name': 'git diff', 'kind': 'diff', 'text': _THEME_DIFF},
        )
        page.wait_for_selector('.d2h-code-line-ctn .hljs-keyword')
        page.get_by_text('Give the resident').click()
        _comment(page, 'Which timeout do the hooks use?')
        page.keyboard.press('1')
        page.mouse.click(5, 5)
        page.screenshot(path=out / f'{name}.jpg', quality=85)
        shutil.copy(out / f'{name}.jpg', sheet)
        page.close()
    page = browser.new_page(viewport={'width': 1280, 'height': 800})
    tiles = ''.join(
        f'<figure><figcaption>{label}</figcaption><img src="{n}.jpg"></figure>' for n, label in _THEMES.items()
    )
    (sheet / 'index.html').write_text(_SHEET.format(tiles), encoding='utf-8')
    page.goto(f'{url}sheet/')
    page.wait_for_load_state('load')
    page.screenshot(path=_IMG / 'themes.jpg', full_page=True, quality=85)
    page.close()


def main():
    diff = subprocess.run(
        ['git', '-C', str(_ROOT), 'show', 'dc6bf53', '--format=', '--', 'src/main.rs', 'src/platform.rs'],
        capture_output=True,
        check=True,
        text=True,
        encoding='utf-8',
    ).stdout
    with tempfile.TemporaryDirectory() as tmp, sync_playwright() as p:
        work = Path(tmp) / 'ui'
        shutil.copytree(_ROOT / 'ui', work)
        server = ThreadingHTTPServer(('127.0.0.1', 0), partial(_QuietHandler, directory=str(work)))
        threading.Thread(target=server.serve_forever, daemon=True).start()
        url = f'http://127.0.0.1:{server.server_port}/'
        browser = p.chromium.launch(channel='msedge')
        page = browser.new_page(viewport={'width': 1280, 'height': 800}, device_scale_factor=2)
        page.add_init_script(_STUB)
        # vjuu serves the compiled theme as theme.css; a theme file on its own works the same.
        shutil.copy(_ROOT / 'themes' / 'graphite.css', work / 'theme.css')

        _shoot(page, url, work, {'name': 'docs/plan.md', 'kind': 'md', 'text': _PLAN})
        page.get_by_text('Hand each new document').click()
        _comment(page, 'What happens when two agents call vjuu at the same time?')
        page.get_by_text('A stale pipe').click()
        _comment(page, 'Add a test for this one.')
        page.mouse.click(5, 5)
        page.screenshot(path=_IMG / 'markdown.png')

        _shoot(page, url, work, {'name': 'git show dc6bf53', 'kind': 'diff', 'text': diff})
        page.keyboard.press('v')  # side-by-side
        page.wait_for_selector('.d2h-code-side-line .hljs-keyword')
        page.locator('tr', has=page.locator('.d2h-ins')).nth(6).click()
        _comment(page, 'Does this hold on a second monitor with a different DPI?')
        page.mouse.click(5, 5)
        page.screenshot(path=_IMG / 'diff.png')
        # The link preview (og:image, GitHub's social preview): the top 2:1 of the same view, 1280x640.
        page.screenshot(
            path=_IMG / 'social.jpg', clip={'x': 0, 'y': 0, 'width': 1280, 'height': 640}, scale='css', quality=90
        )

        light = browser.new_page(viewport={'width': 1280, 'height': 800}, device_scale_factor=2)
        light.add_init_script(_STUB)
        shutil.copy(_ROOT / 'themes' / 'light.css', work / 'theme.css')
        _shoot(light, url, work, {'name': 'git show dc6bf53', 'kind': 'diff', 'text': diff})
        light.wait_for_selector('.d2h-code-line-ctn .hljs-keyword')
        light.locator('tr', has=light.locator('.d2h-ins')).nth(6).click()
        _comment(light, 'Does this hold on a second monitor with a different DPI?')
        light.mouse.click(5, 5)
        light.screenshot(path=_IMG / 'light.png')
        _themes(browser, url, work)
        browser.close()
        server.shutdown()


if __name__ == '__main__':
    main()
