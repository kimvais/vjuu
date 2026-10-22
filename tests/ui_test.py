"""Drives ui/ in headless Edge with a stubbed window.ipc and checks the review the page reports.

Run: uv run --no-project --with playwright python tests/ui_test.py
"""
import json
import sys
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


def _open(page, url, work, *docs):
    (work / 'doc.json').write_text(json.dumps({'docs': list(docs)}))
    page.goto(url)
    page.wait_for_selector('[data-key]')


def _review(page):
    states = [m for m in page.evaluate('window.__msgs') if m['type'] == 'state']
    review = states[-1]['review']
    for c in review['comments']:
        del c['editing'], c['doc'], c['anchorKey']
        c.pop('part', None)
    return review


def _comment(page, text):
    page.keyboard.type(text)
    page.keyboard.press('Control+Enter')


def test_diff(page, url, work):
    _open(page, url, work, {'name': 'git diff', 'kind': 'diff', 'text': _DIFF})
    page.locator('tr', has=page.locator('.d2h-ins')).first.locator('.d2h-code-linenumber').click()
    _comment(page, 'rename is fine')
    # Clicking anywhere on a line comments on it, not just the line number.
    page.locator('tr', has=page.locator('.d2h-del')).first.locator('.d2h-code-line').click()
    _comment(page, 'why removed?')
    assert page.locator('tr.vjuu-c').count() == 2
    page.keyboard.press('v')
    # Each comment row plus a spacer row in the other table; nothing orphaned.
    assert page.locator('tr.vjuu-c').count() == 4
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
    _open(page, url, work, {'name': 'plan.md', 'kind': 'md', 'text': _MD})
    page.get_by_text('First paragraph').click()
    _comment(page, 'tighten this')
    page.get_by_text('item two').click()
    _comment(page, 'drop it')
    # Esc on a new comment leaves nothing behind.
    page.get_by_text('print(1)').click()
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
    _open(page, url, work, {'name': 'plan.md', 'kind': 'md', 'text': _MD})
    # Fenced ```py block is highlighted; `print` is a built-in.
    assert page.locator('pre code .hljs-built_in').inner_text() == 'print'
    theme = lambda: page.evaluate('[document.documentElement.dataset.theme, localStorage.getItem("vjuu-theme")]')
    assert theme() == ['light', 'auto']  # headless Edge reports a light OS
    page.keyboard.press('t')
    assert theme() == ['light', 'light']
    page.keyboard.press('t')
    assert theme() == ['graphite', 'graphite']
    assert page.evaluate('getComputedStyle(document.body).backgroundColor') == 'rgb(30, 30, 30)'
    page.keyboard.press('t')  # wraps around
    assert theme() == ['light', 'auto']
    page.select_option('#theme', 'graphite')
    assert theme() == ['graphite', 'graphite']
    # The choice survives into the next window.
    _open(page, url, work, {'name': 'git diff', 'kind': 'diff', 'text': _DIFF})
    assert theme() == ['graphite', 'graphite']
    page.wait_for_selector('.d2h-code-line-ctn .hljs-keyword')
    # Monokai was folded into Graphite: an old saved choice still gives a dark theme, not an unknown one.
    page.evaluate('localStorage.setItem("vjuu-theme", "monokai")')
    _open(page, url, work, {'name': 'plan.md', 'kind': 'md', 'text': _MD})
    assert theme() == ['graphite', 'graphite']
    page.evaluate('localStorage.removeItem("vjuu-theme")')


def test_multiple_documents(page, url, work):
    _open(page, url, work, {'name': 'a.md', 'kind': 'md', 'text': _MD}, {'name': 'git diff', 'kind': 'diff', 'text': _DIFF},
          {'name': 'a.md', 'kind': 'md', 'text': _MD})
    assert page.inner_text('#name') == 'a.md, git diff, a.md'
    assert page.locator('.doc-header').all_inner_texts() == ['a.md', 'a.md']
    # Same name, same line, different documents: two separate comments.
    for n, text in ((0, 'first copy'), (1, 'second copy')):
        page.get_by_text('First paragraph').nth(n).click()
        _comment(page, text)
    page.locator('tr', has=page.locator('.d2h-ins')).first.locator('.d2h-code-linenumber').click()
    _comment(page, 'in the diff')
    files = page.locator('#files a')
    rows = [' '.join(t.split()) for t in files.all_inner_texts()]
    # Markdown files list their headings under them.
    assert rows == ['a.md 1', 'Title', 'app.py src +1 −1 1', 'a.md 1', 'Title'], rows
    files.nth(4).click()
    page.wait_for_function("document.querySelectorAll('#files a')[4].classList.contains('active')")
    page.keyboard.press('f')
    assert page.locator('#files').is_hidden()
    states = [m for m in page.evaluate('window.__msgs') if m['type'] == 'state']
    got = sorted((c['doc'], c['file'], c['start'], c['text']) for c in states[-1]['review']['comments'])
    assert got == [(0, 'a.md', 3, 'first copy'), (1, 'src/app.py', 2, 'in the diff'), (2, 'a.md', 3, 'second copy')], got


def _select(page, start, end):
    """Selects from the start of the first element matching `start` to the end of the one matching `end`."""
    page.evaluate("""([s, e]) => {
        const r = document.createRange();
        r.setStart(document.querySelector(s), 0);
        const last = document.querySelector(e);
        r.setEnd(last, last.childNodes.length);
        getSelection().removeAllRanges();
        getSelection().addRange(r);
        document.dispatchEvent(new MouseEvent('mouseup'));
    }""", [start, end])


def test_selection_comments(page, url, work):
    text = '# One\n\nFirst para.\n\nSecond\npara.\n\n## Two\n\n```\ncargo install --path .   # build it\nvjuu last\n```\n\n```\nimport os\ndef f(x):\n    return os.sep  # sep\n```\n\n```\n# Review: a.md (1 comment)\n> quoted\n```\n'
    _open(page, url, work, {'name': 'n.md', 'kind': 'md', 'text': text})
    # A single markdown file with two or more headings gets a table of contents.
    assert [' '.join(t.split()) for t in page.locator('#files a.toc').all_inner_texts()] == ['One', 'Two']
    # Unlabelled fences: command lines are highlighted as shell, example output stays plain.
    assert page.locator('pre code').nth(0).locator('[class^=hljs-]').count() > 0
    assert page.locator('pre code').nth(0).locator('.hljs-title').all_inner_texts() == ['cargo', 'vjuu']
    assert page.locator('pre code').nth(1).locator('.hljs-keyword').count() > 0  # python, not shell
    assert page.locator('pre code').nth(2).locator('[class^=hljs-]').count() == 0
    # Selecting across two paragraphs comments on both, as one range.
    _select(page, '.markdown p', '.markdown p:nth-of-type(2)')
    page.keyboard.press('c')
    _comment(page, 'merge these')
    review = _review(page)
    assert review['comments'] == [{'file': 'n.md', 'start': 3, 'end': 6, 'old': False,
                                   'quote': 'First para.\n\nSecond\npara.', 'text': 'merge these'}], review

    # Diff: select the deleted and the added line; numbered by the new file, quoting both.
    _open(page, url, work, {'name': 'git diff', 'kind': 'diff', 'text': _DIFF})
    _select(page, 'tr:has(.d2h-del) .d2h-code-line-ctn', 'tr:has(.d2h-ins) .d2h-code-line-ctn')
    page.locator('#sel-comment').click()
    _comment(page, 'rename?')
    page.keyboard.press('v')  # the range comment survives a re-render
    assert page.locator('#orphans').count() == 0
    review = _review(page)
    assert review['comments'] == [{'file': 'src/app.py', 'start': 2, 'end': 2, 'old': False, 'quote': '-def old():\n+def new():',
                                   'text': 'rename?'}], review


def test_code_file(page, url, work):
    text = 'fn main() {\r\n    /* spans\r\n       lines */\r\n    let x = "s";\r\n}\r\n'
    _open(page, url, work, {'name': 'src/main.rs', 'kind': 'code', 'text': text})
    rows = page.locator('table.code tr')
    assert rows.count() == 5
    # A comment crossing a line break stays a comment on both lines.
    assert page.locator('table.code tr').nth(2).locator('.hljs-comment').inner_text() == '       lines */'
    assert page.locator('table.code .hljs-keyword').first.inner_text() == 'fn'
    rows.nth(3).locator('.code-ln').click()
    _comment(page, 'name it')
    assert _review(page)['comments'] == [{'file': 'src/main.rs', 'start': 4, 'end': 4, 'old': False, 'quote': '    let x = "s";', 'text': 'name it'}]


def test_clicks_that_do_not_comment(page, url, work):
    _open(page, url, work, {'name': 'l.md', 'kind': 'md', 'text': 'See [the docs](https://example.invalid/docs) first.\n\nSecond.\n'})
    # Runs after vjuu's handler; keeps the test on the page instead of following the link.
    page.evaluate("window.addEventListener('click', (e) => e.preventDefault())")
    page.get_by_text('the docs').click()
    assert page.locator('.vjuu-c').count() == 0
    # The click that ends a drag-selection must leave the selection for the Comment button.
    _select(page, '.markdown p', '.markdown p')
    page.evaluate("document.querySelector('.markdown p').dispatchEvent(new MouseEvent('click', {bubbles: true}))")
    assert page.locator('.vjuu-c').count() == 0
    assert page.locator('#sel-comment').is_visible()


def test_keyboard_navigation(page, url, work):
    _open(page, url, work, {'name': 'a.md', 'kind': 'md', 'text': _MD}, {'name': 'git diff', 'kind': 'diff', 'text': _DIFF})
    focused = lambda: page.evaluate("document.querySelector('.focused')?.dataset.key.split('\\n').slice(2).join(' ')")
    page.keyboard.press('ArrowDown')  # nothing focused yet: the first line on screen
    assert focused() == 'a.md 1-1', focused()
    page.keyboard.press('ArrowDown')
    assert focused() == 'a.md 3-4', focused()
    page.keyboard.press('Enter')
    _comment(page, 'on the paragraph')  # Ctrl+Enter inside the comment saves it
    page.keyboard.press('ArrowRight')  # next file: the diff, cursor on its first line
    assert focused() == 'src/app.py 1-1', focused()
    page.keyboard.press('ArrowLeft')
    assert focused() == 'a.md 1-1', focused()
    page.keyboard.press('ArrowRight')
    page.keyboard.press('ArrowDown')
    page.keyboard.press('Enter')
    _comment(page, 'on the diff')
    got = sorted((c['file'], c['start'], c['text']) for c in _review(page)['comments'])
    assert got == [('a.md', 3, 'on the paragraph'), ('src/app.py', 2, 'on the diff')], got
    assert page.evaluate('window.__msgs.at(-1).type') == 'state'
    page.keyboard.press('Control+Enter')  # outside a comment: Done
    assert page.evaluate('window.__msgs.at(-1)') == {'type': 'done'}


def test_unsaved_comment_is_kept(page, url, work):
    _open(page, url, work, {'name': 'a.md', 'kind': 'md', 'text': _MD})
    page.get_by_text('First paragraph').click()
    page.keyboard.type('half-written')
    # Done (or closing the window) while still typing must not drop the text.
    page.keyboard.press('Control+Shift+Enter')
    assert [c['text'] for c in _review(page)['comments']] == ['half-written']
    # Editing a saved comment and finishing without saving sends the edited text.
    _open(page, url, work, {'name': 'a.md', 'kind': 'md', 'text': _MD})
    page.get_by_text('First paragraph').click()
    _comment(page, 'first')
    page.locator('.comment .text', has_text='first').click()
    page.keyboard.press('Control+a')
    page.keyboard.type('second')
    assert [c['text'] for c in _review(page)['comments']] == ['second']


def test_same_file_twice_in_one_diff(page, url, work):
    # `git log -p` shows the same file once per commit.
    twice = _DIFF + _DIFF.replace('def new', 'def newer')
    _open(page, url, work, {'name': 'git log -p', 'kind': 'diff', 'text': twice})
    for n in (0, 1):
        page.locator('.d2h-file-wrapper').nth(n).locator('tr', has=page.locator('.d2h-ins')).first.click()
        _comment(page, f'commit {n}')
    assert sorted(c['text'] for c in _review(page)['comments']) == ['commit 0', 'commit 1']
    assert page.locator('#orphans').count() == 0


def test_markdown_file_in_a_diff_uses_code_size(page, url, work):
    # diff2html puts a `markdown` class on highlighted lines of .md files; document styles must not apply to them.
    md_diff = _DIFF.replace('src/app.py', 'README.md')
    _open(page, url, work, {'name': 'git diff', 'kind': 'diff', 'text': md_diff}, {'name': 'x.md', 'kind': 'md', 'text': _MD})
    page.wait_for_selector('.d2h-code-line-ctn.markdown, .d2h-code-line-ctn .hljs-section, .d2h-code-line-ctn', state='attached')
    page.wait_for_timeout(300)  # highlighting runs after paint
    size = lambda sel: page.evaluate(f"getComputedStyle(document.querySelector('{sel}')).fontSize")
    assert size('.d2h-code-line-ctn') == size('.d2h-code-linenumber') or size('.d2h-code-line-ctn') == '13px', size('.d2h-code-line-ctn')
    assert size('article.markdown p') == '16px'


def test_markdown_html_is_not_rendered(page, url, work):
    _open(page, url, work, {'name': 'x.md', 'kind': 'md', 'text': 'hi <img src=x onerror="window.__pwned=1">\n'})
    assert page.locator('#content img').count() == 0
    assert page.evaluate('window.__pwned') is None


def main():
    with tempfile.TemporaryDirectory() as tmp, sync_playwright() as p:
        work = Path(tmp) / 'ui'
        shutil.copytree(_UI, work)
        url = _serve(work)
        browser = p.chromium.launch(channel='msedge')
        errors = []
        only = sys.argv[1:]
        for test in (test_diff, test_markdown, test_highlighting_and_themes, test_multiple_documents, test_selection_comments, test_code_file, test_clicks_that_do_not_comment, test_keyboard_navigation, test_unsaved_comment_is_kept,
                     test_same_file_twice_in_one_diff, test_markdown_file_in_a_diff_uses_code_size,
                     test_markdown_html_is_not_rendered):
            if only and test.__name__ not in only:
                continue
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
