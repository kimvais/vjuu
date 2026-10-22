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
_STUB = 'window.ipc = { postMessage: () => {} }; localStorage.setItem("vjuu-theme", "%s");'
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
        page.add_init_script(_STUB % 'graphite')

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

        light = browser.new_page(viewport={'width': 1280, 'height': 800}, device_scale_factor=2)
        light.add_init_script(_STUB % 'light')
        _shoot(light, url, work, {'name': 'git show dc6bf53', 'kind': 'diff', 'text': diff})
        light.wait_for_selector('.d2h-code-line-ctn .hljs-keyword')
        light.locator('tr', has=light.locator('.d2h-ins')).nth(6).click()
        _comment(light, 'Does this hold on a second monitor with a different DPI?')
        light.mouse.click(5, 5)
        light.screenshot(path=_IMG / 'light.png')
        browser.close()
        server.shutdown()


if __name__ == '__main__':
    main()
