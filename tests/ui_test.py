"""Drives ui/ in headless Edge with a stubbed window.ipc and checks the review the page reports.

Run: uv run --no-project --with playwright python tests/ui_test.py
"""
import json
import shutil
import tempfile
import threading
from functools import partial
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

from playwright.sync_api import sync_playwright

_UI = Path(__file__).resolve().parent.parent / 'ui'
_DIFF = """diff --git a/src/app.py b/src/app.py
--- a/src/app.py
+++ b/src/app.py
@@ -1,4 +1,4 @@
 import os
-def old():
+def new():
     return 1
 x = 2
"""
_MD = '# Title\n\nFirst paragraph\nspans two lines.\n\n- item one\n- item two\n\n```py\nprint(1)\n```\n'
_STUB = 'window.ipc = { postMessage: (m) => (window.__msgs = window.__msgs || []).push(JSON.parse(m)) };'


class _QuietHandler(SimpleHTTPRequestHandler):
    def log_message(self, *args):
        pass


def _serve(work):
    server = ThreadingHTTPServer(('127.0.0.1', 0), partial(_QuietHandler, directory=str(work)))
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return f'http://127.0.0.1:{server.server_port}/'


def _open(page, url, work, doc):
    (work / 'doc.json').write_text(json.dumps(doc))
    page.goto(url)
    page.wait_for_selector('[data-key]')


def _review(page):
    states = [m for m in page.evaluate('window.__msgs') if m['type'] == 'state']
    review = states[-1]['review']
    for c in review['comments']:
        del c['editing']
    return review


def _comment(page, text):
    page.keyboard.type(text)
    page.keyboard.press('Control+Enter')


def test_diff(page, url, work):
    _open(page, url, work, {'name': 'git diff', 'kind': 'diff', 'text': _DIFF, 'autoclose': False})
    page.locator('tr', has=page.locator('.d2h-ins')).first.locator('.d2h-code-linenumber').click()
    _comment(page, 'rename is fine')
    page.locator('tr', has=page.locator('.d2h-del')).first.locator('.d2h-code-line').click()
    page.keyboard.press('c')
    _comment(page, 'why removed?')
    assert page.locator('tr.revu-c').count() == 2
    page.keyboard.press('v')
    # Each comment row plus a spacer row in the other table; nothing orphaned.
    assert page.locator('tr.revu-c').count() == 4
    assert page.locator('#orphans').count() == 0
    page.locator('.comment .text', has_text='why removed?').click()
    page.keyboard.press('Control+a')
    _comment(page, 'why was old() removed?')
    page.locator('.comment', has_text='rename is fine').locator('button', has_text='×').click()
    review = _review(page)
    assert review == {
        'comments': [{'file': 'src/app.py', 'start': 2, 'end': 2, 'old': True, 'quote': '-def old():', 'text': 'why was old() removed?'}],
        'overall': '',
    }, review
    page.keyboard.press('Control+Shift+Enter')
    assert page.evaluate('window.__msgs.at(-1)') == {'type': 'done'}


def test_markdown(page, url, work):
    _open(page, url, work, {'name': 'plan.md', 'kind': 'md', 'text': _MD, 'autoclose': False})
    page.get_by_text('First paragraph').click()
    page.keyboard.press('c')
    _comment(page, 'tighten this')
    page.get_by_text('item two').click()
    page.keyboard.press('Enter')
    _comment(page, 'drop it')
    # Esc on a new comment leaves nothing behind.
    page.get_by_text('print(1)').click()
    page.keyboard.press('c')
    page.keyboard.press('Escape')
    page.fill('#overall', 'good start')
    review = _review(page)
    assert review == {
        'comments': [
            {'file': 'plan.md', 'start': 3, 'end': 4, 'old': False, 'quote': 'First paragraph\nspans two lines.', 'text': 'tighten this'},
            {'file': 'plan.md', 'start': 7, 'end': 7, 'old': False, 'quote': '- item two', 'text': 'drop it'},
        ],
        'overall': 'good start',
    }, review
    assert page.inner_text('#count') == '2 comments'
    # Fonts are embedded; a wrong path silently falls back to system fonts.
    loaded = page.evaluate("document.fonts.ready.then(() => [...document.fonts].filter((f) => f.status === 'loaded').map((f) => f.family).sort())")
    assert loaded == ['"Inter"', '"JetBrains Mono"'] or loaded == ['Inter', 'JetBrains Mono'], loaded


def test_highlighting_and_themes(page, url, work):
    _open(page, url, work, {'name': 'plan.md', 'kind': 'md', 'text': _MD, 'autoclose': False})
    # Fenced ```py block is highlighted; `print` is a built-in.
    assert page.locator('pre code .hljs-built_in').inner_text() == 'print'
    theme = lambda: page.evaluate('[document.documentElement.dataset.theme, localStorage.getItem("revu-theme")]')
    assert theme() == ['light', 'auto']  # headless Edge reports a light OS
    page.keyboard.press('t')
    assert theme() == ['light', 'light']
    page.keyboard.press('t')
    assert theme() == ['monokai', 'monokai']
    assert page.evaluate('getComputedStyle(document.body).backgroundColor') == 'rgb(39, 40, 34)'
    page.select_option('#theme', 'graphite')
    assert theme() == ['graphite', 'graphite']
    # The choice survives into the next window.
    _open(page, url, work, {'name': 'git diff', 'kind': 'diff', 'text': _DIFF, 'autoclose': False})
    assert theme() == ['graphite', 'graphite']
    page.wait_for_selector('.d2h-code-line-ctn .hljs-keyword')
    page.evaluate('localStorage.removeItem("revu-theme")')


def test_markdown_html_is_not_rendered(page, url, work):
    _open(page, url, work, {'name': 'x.md', 'kind': 'md', 'text': 'hi <img src=x onerror="window.__pwned=1">\n', 'autoclose': False})
    assert page.locator('#content img').count() == 0
    assert page.evaluate('window.__pwned') is None


def main():
    with tempfile.TemporaryDirectory() as tmp, sync_playwright() as p:
        work = Path(tmp) / 'ui'
        shutil.copytree(_UI, work)
        url = _serve(work)
        browser = p.chromium.launch(channel='msedge')
        errors = []
        for test in (test_diff, test_markdown, test_highlighting_and_themes, test_markdown_html_is_not_rendered):
            page = browser.new_page()
            page.add_init_script(_STUB)
            page.on('pageerror', lambda e: errors.append(e))
            test(page, url, work)
            page.close()
            print(f'ok {test.__name__}')
        browser.close()
        assert not errors, errors


if __name__ == '__main__':
    main()
