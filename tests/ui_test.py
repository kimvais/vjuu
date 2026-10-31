"""Drives ui/ in headless Edge with a stubbed window.ipc and checks the review the page reports.

Run: uv run --no-project --with playwright python tests/ui_test.py
"""

import json
import shutil
import sys
import tempfile
import threading
from functools import partial
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

from playwright.sync_api import sync_playwright

_UI = Path(__file__).resolve().parent.parent / 'ui'
_THEMES = _UI.parent / 'themes'
_CONFIG = {
    'tray': True,
    'focus': False,
    'hooks': {'plan': True, 'ask': False, 'turn': True, 'permission': False, 'gate': True},
    'theme': {'name': 'auto', 'light': 'light', 'dark': 'graphite'},
    'themes': [{'name': 'graphite', 'scheme': 'dark'}, {'name': 'light', 'scheme': 'light'}],
    'version': '9.9.9',
}
_WHITE, _GRAPHITE = 'rgb(255, 255, 255)', 'rgb(30, 30, 30)'
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

    def end_headers(self):
        # As vjuu serves it: theme.css changes under the same URL when a theme is applied.
        if self.path.split('?')[0] == '/theme.css':
            self.send_header('Cache-Control', 'no-store')
        super().end_headers()


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


def _background(page):
    return page.evaluate('getComputedStyle(document.body).backgroundColor')


def _set_theme(work, page, name):
    """Picks a theme in the settings panel and applies it, compiling it the way vjuu would: here, a copy."""
    (work / 'config.json').write_text(json.dumps(_CONFIG))
    page.click('#settings-open')
    page.wait_for_selector('#settings[open]')
    page.select_option('#theme', name)
    page.click('#settings-apply')
    shutil.copy(work / 'themes' / f'{name}.css', work / 'theme.css')
    page.evaluate('vjuuThemeSaved()')
    background = {'light': _WHITE, 'graphite': _GRAPHITE}[name]
    page.wait_for_function(f'getComputedStyle(document.body).backgroundColor === "{background}"', timeout=2000)


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
        'comments': [
            {
                'file': 'src/app.py',
                'start': 2,
                'end': 2,
                'old': True,
                'quote': '-def old():',
                'text': 'why was old() removed?',
            }
        ],
        'overall': '',
    }, review
    page.keyboard.press('Control+r')
    assert page.evaluate('window.__msgs.at(-1)') == {'type': 'done', 'verdict': 'redo'}


def _ctrl_r_prevented(page):
    """Whether the page cancels Ctrl+R, which would reload it; dispatched, as headless Edge has no reload key."""
    return page.evaluate(
        "() => !document.body.dispatchEvent("
        "new KeyboardEvent('keydown', {key: 'r', ctrlKey: true, cancelable: true, bubbles: true}))"
    )


def test_verdicts(page, url, work):
    (work / 'config.json').write_text(json.dumps(_CONFIG))  # the help reads the version from it
    _open(page, url, work, {'name': 'plan.md', 'kind': 'md', 'text': _MD})
    done = lambda: [m for m in page.evaluate('window.__msgs || []') if m['type'] == 'done']
    # Ctrl+Enter saves a comment and nothing else: outside one it finishes nothing.
    page.get_by_text('First paragraph').click()
    _comment(page, 'kept')
    page.keyboard.press('Control+Enter')
    page.fill('#overall', 'overall')
    page.keyboard.press('Control+Enter')
    # Ctrl+Z is the text's undo while a comment is open, and in the Overall box.
    page.get_by_text('item two').click()
    page.keyboard.type('typing')
    page.keyboard.press('Control+z')
    page.focus('#overall')
    page.keyboard.press('Control+z')
    assert done() == []
    # Outside a field, with no comment open, Ctrl+Z discards.
    page.locator('.vjuu-c textarea').press('Escape')
    page.keyboard.press('Control+z')
    assert done() == [{'type': 'done', 'verdict': 'discard'}], done()
    # Ctrl+R never reloads: with a dialog open it does nothing, otherwise it is Redo.
    _open(page, url, work, {'name': 'plan.md', 'kind': 'md', 'text': _MD})
    page.keyboard.press('F1')
    page.wait_for_selector('#help[open]')
    assert _ctrl_r_prevented(page) and done() == [], done()
    page.keyboard.press('Escape')
    assert _ctrl_r_prevented(page) and done() == [{'type': 'done', 'verdict': 'redo'}], done()
    # Each button sends its verdict.
    for verdict, label in (('approved', 'Approve'), ('redo', 'Redo'), ('discard', 'Discard')):
        _open(page, url, work, {'name': 'plan.md', 'kind': 'md', 'text': _MD})
        page.locator('#verdicts button', has_text=label).click()
        assert page.evaluate('window.__msgs.at(-1)') == {'type': 'done', 'verdict': verdict}


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
            {
                'file': 'plan.md',
                'start': 3,
                'end': 4,
                'old': False,
                'quote': 'First paragraph\nspans two lines.',
                'text': 'tighten this',
            },
            {'file': 'plan.md', 'start': 7, 'end': 7, 'old': False, 'quote': '- item two', 'text': 'drop it'},
        ],
        'overall': 'good start',
    }, review
    assert page.inner_text('#count') == '2 comments'
    # Fonts are embedded; a wrong path silently falls back to system fonts.
    loaded = page.evaluate(
        "document.fonts.ready.then(() => [...document.fonts].filter((f) => f.status === 'loaded').map((f) => f.family).sort())"
    )
    assert loaded == ['"Inter"', '"JetBrains Mono"'] or loaded == ['Inter', 'JetBrains Mono'], loaded


_CHANGES = '--- a/plan.md\n+++ b/plan.md\n@@ -1,4 +1,4 @@\n # Title\n \n-Only paragraph\n+First paragraph\n spans two lines.\n'


def test_revised_file(page, url, work):
    _open(page, url, work, {'name': 'plan.md', 'kind': 'md', 'text': _MD, 'changes': _CHANGES})
    # A revision opens on its changes; a comment there is on the file's line.
    assert page.locator('article.markdown').count() == 0 and page.inner_text('#changes') == 'Full file'
    page.locator('tr', has=page.locator('.d2h-ins')).first.click()
    _comment(page, 'better')
    assert _review(page)['comments'] == [
        {'file': 'plan.md', 'start': 3, 'end': 3, 'old': False, 'quote': '+First paragraph', 'text': 'better'}
    ]
    page.keyboard.press('d')
    assert page.locator('.d2h-file-wrapper').count() == 0 and page.locator('article.markdown h1').count() == 1
    assert page.inner_text('#changes') == 'Changes'
    page.keyboard.press('d')
    assert page.locator('.d2h-file-wrapper').count() == 1 and page.locator('#orphans').count() == 0

    # The last review's comments: on the line of the earlier version they were on, if the changes show it.
    earlier = [
        {'file': 'plan.md', 'start': 3, 'end': 3, 'old': False, 'quote': 'Only paragraph', 'text': 'say more'},
        {'file': 'plan.md', 'start': 40, 'end': 41, 'old': False, 'quote': 'Far away', 'text': 'not shown here'},
        {'file': 'plan.md', 'start': 3, 'end': 3, 'old': True, 'quote': '-Older', 'text': 'two rounds ago'},
    ]
    doc = {'name': 'plan.md', 'kind': 'md', 'text': _MD, 'changes': _CHANGES}
    (work / 'doc.json').write_text(json.dumps({'docs': [doc], 'earlier': earlier}))
    page.goto(url)
    page.wait_for_selector('[data-key]')
    placed = page.locator('tr.vjuu-c', has_text='say more')
    assert placed.evaluate('r => r.previousElementSibling.dataset.oldLine') == '3'
    assert page.locator('#earlier .comment .text').all_inner_texts() == ['not shown here', 'two rounds ago']
    # Read-only, and not part of this review.
    placed.click()
    assert page.locator('.vjuu-c textarea').count() == 0
    page.locator('tr', has=page.locator('.d2h-ins')).first.click()
    _comment(page, 'new one')
    assert [c['text'] for c in _review(page)['comments']] == ['new one']
    page.keyboard.press('d')  # the full plan has no old line numbers: all of them are listed
    assert page.locator('#earlier .comment').count() == 3

    # Without an earlier version there is nothing to toggle.
    _open(page, url, work, {'name': 'plan.md', 'kind': 'md', 'text': _MD})
    assert page.locator('#changes').is_hidden()
    page.keyboard.press('d')
    assert page.locator('article.markdown').count() == 1


def test_highlighting_and_themes(page, url, work):
    _open(page, url, work, {'name': 'plan.md', 'kind': 'md', 'text': _MD})
    # Fenced ```py block is highlighted; `print` is a built-in.
    assert page.locator('pre code .hljs-built_in').inner_text() == 'print'
    assert _background(page) == _WHITE  # the compiled theme the harness starts with
    # The theme lives in the settings: a stray `t` does not change it.
    page.keyboard.press('t')
    assert _background(page) == _WHITE
    # The panel lists the themes; the light and dark picks show only for Auto.
    (work / 'config.json').write_text(json.dumps(_CONFIG))
    page.keyboard.press('s')  # a typo, not the settings key
    page.wait_for_timeout(200)  # the panel opens after fetching config.json
    assert not page.is_visible('#settings')
    page.keyboard.press('Control+,')
    page.wait_for_selector('#settings[open]')
    options = page.eval_on_selector_all('#theme option', 'os => os.map((o) => o.value)')
    assert options == ['auto', 'graphite', 'light'] and page.is_visible('#auto-themes')
    assert page.eval_on_selector_all('#theme-dark option', 'os => os.map((o) => o.value)') == ['graphite']
    # Picking previews the theme; Esc goes back to the saved one.
    page.select_option('#theme', 'graphite')
    page.wait_for_function(f'getComputedStyle(document.body).backgroundColor === "{_GRAPHITE}"', timeout=2000)
    assert not page.is_visible('#auto-themes')
    page.keyboard.press('Escape')
    assert _background(page) == _WHITE
    _set_theme(work, page, 'graphite')
    assert _background(page) == _GRAPHITE
    # The choice is in theme.css, so the next window opens in it.
    _open(page, url, work, {'name': 'git diff', 'kind': 'diff', 'text': _DIFF})
    assert _background(page) == _GRAPHITE
    page.wait_for_selector('.d2h-code-line-ctn .hljs-keyword')


def test_session_name(page, url, work):
    (work / 'doc.json').write_text(
        json.dumps({'docs': [{'name': 'a.md', 'kind': 'md', 'text': _MD}], 'session': 'fix login'})
    )
    page.goto(url)
    page.wait_for_selector('[data-key]')
    assert page.inner_text('#session') == 'fix login'
    assert page.inner_text('#name') == 'a.md'
    # Without one, the title bar shows no empty slot.
    _open(page, url, work, {'name': 'a.md', 'kind': 'md', 'text': _MD})
    assert not page.locator('#session').is_visible()


def test_multiple_documents(page, url, work):
    _open(
        page,
        url,
        work,
        {'name': 'a.md', 'kind': 'md', 'text': _MD},
        {'name': 'git diff', 'kind': 'diff', 'text': _DIFF},
        {'name': 'a.md', 'kind': 'md', 'text': _MD},
    )
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
    assert rows == ['a.md 1', 'Title', 'app.py +1 −1 1', 'a.md 1', 'Title'], rows
    # Directories are folder rows above their files, as in GitLab's tree.
    assert page.locator('#files .folder').all_inner_texts() == ['src']
    files.nth(4).click()
    page.wait_for_function("document.querySelectorAll('#files a')[4].classList.contains('active')")
    page.keyboard.press('f')
    assert page.locator('#files').is_hidden()
    states = [m for m in page.evaluate('window.__msgs') if m['type'] == 'state']
    got = sorted((c['doc'], c['file'], c['start'], c['text']) for c in states[-1]['review']['comments'])
    assert got == [(0, 'a.md', 3, 'first copy'), (1, 'src/app.py', 2, 'in the diff'), (2, 'a.md', 3, 'second copy')], (
        got
    )


def _select(page, start, end):
    """Selects from the start of the first element matching `start` to the end of the one matching `end`."""
    page.evaluate(
        """([s, e]) => {
        const r = document.createRange();
        r.setStart(document.querySelector(s), 0);
        const last = document.querySelector(e);
        r.setEnd(last, last.childNodes.length);
        getSelection().removeAllRanges();
        getSelection().addRange(r);
        document.dispatchEvent(new MouseEvent('mouseup'));
    }""",
        [start, end],
    )


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
    assert review['comments'] == [
        {
            'file': 'n.md',
            'start': 3,
            'end': 6,
            'old': False,
            'quote': 'First para.\n\nSecond\npara.',
            'text': 'merge these',
        }
    ], review

    # Diff: select the deleted and the added line; numbered by the new file, quoting both.
    _open(page, url, work, {'name': 'git diff', 'kind': 'diff', 'text': _DIFF})
    _select(page, 'tr:has(.d2h-del) .d2h-code-line-ctn', 'tr:has(.d2h-ins) .d2h-code-line-ctn')
    page.locator('#sel-comment').click()
    _comment(page, 'rename?')
    page.keyboard.press('v')  # the range comment survives a re-render
    assert page.locator('#orphans').count() == 0
    review = _review(page)
    assert review['comments'] == [
        {
            'file': 'src/app.py',
            'start': 2,
            'end': 2,
            'old': False,
            'quote': '-def old():\n+def new():',
            'text': 'rename?',
        }
    ], review


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
    assert _review(page)['comments'] == [
        {'file': 'src/main.rs', 'start': 4, 'end': 4, 'old': False, 'quote': '    let x = "s";', 'text': 'name it'}
    ]


def test_clicks_that_do_not_comment(page, url, work):
    _open(
        page,
        url,
        work,
        {'name': 'l.md', 'kind': 'md', 'text': 'See [the docs](https://example.invalid/docs) first.\n\nSecond.\n'},
    )
    # Runs after vjuu's handler; keeps the test on the page instead of following the link.
    page.evaluate("window.addEventListener('click', (e) => e.preventDefault())")
    page.get_by_text('the docs').click()
    assert page.locator('.vjuu-c').count() == 0
    # The click that ends a drag-selection must leave the selection for the Comment button.
    _select(page, '.markdown p', '.markdown p')
    page.evaluate("document.querySelector('.markdown p').dispatchEvent(new MouseEvent('click', {bubbles: true}))")
    # The Comment button appears in a setTimeout after mouseup; wait for it instead of racing it (slow CI runners).
    page.locator('#sel-comment').wait_for(state='visible', timeout=5000)
    assert page.locator('.vjuu-c').count() == 0


def test_keyboard_navigation(page, url, work):
    _open(
        page,
        url,
        work,
        {'name': 'a.md', 'kind': 'md', 'text': _MD},
        {'name': 'git diff', 'kind': 'diff', 'text': _DIFF},
    )
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
    page.keyboard.press('Control+s')
    assert page.evaluate('window.__msgs.at(-1)') == {'type': 'done', 'verdict': 'approved'}


def test_unsaved_comment_is_kept(page, url, work):
    _open(page, url, work, {'name': 'a.md', 'kind': 'md', 'text': _MD})
    page.get_by_text('First paragraph').click()
    page.keyboard.type('half-written')
    # Approving (or closing the window) while still typing must not drop the text.
    page.keyboard.press('Control+s')
    assert page.evaluate('window.__msgs.at(-1)') == {'type': 'done', 'verdict': 'approved'}
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


def test_directory_tree(page, url, work):
    files = ['a.py', 'src/pkg/b.py', 'src/pkg/sub/c.py', 'src/d.py']
    diff = ''.join(_DIFF.replace('src/app.py', f) for f in files)
    _open(page, url, work, {'name': 'git diff', 'kind': 'diff', 'text': diff})
    rows = page.locator('#files > *')
    got = [
        (r.evaluate('e => e.tagName'), r.inner_text().split()[0], r.evaluate('e => e.style.paddingLeft'))
        for r in rows.all()
    ]
    assert got == [
        ('A', 'a.py', '12px'),
        ('DIV', 'src', '12px'),
        ('DIV', 'pkg', '26px'),
        ('A', 'b.py', '40px'),
        ('DIV', 'sub', '40px'),
        ('A', 'c.py', '54px'),
        ('A', 'd.py', '26px'),
    ], got


_REINDENTED = """diff --git a/q.py b/q.py
--- a/q.py
+++ b/q.py
@@ -1,5 +1,4 @@
 def f():
-  return 1
-old = 2
--- a removed SQL comment, not a header
+    return 1
+new = 2
 x = 3
diff --git a/r.py b/r.py
--- a/r.py
+++ b/r.py
@@ -1 +1 @@
-a  =  1
+a = 1
"""


def test_ignore_whitespace(page, url, work):
    _open(page, url, work, {'name': 'git diff', 'kind': 'diff', 'text': _REINDENTED})
    changed = lambda: page.evaluate(
        "[...document.querySelectorAll('.d2h-code-linenumber.d2h-ins, .d2h-code-linenumber.d2h-del')]"
        ".map((td) => td.parentElement.querySelector('.d2h-code-line-ctn').textContent.trim())"
    )
    everything = changed()
    assert len(everything) == 7, everything
    page.keyboard.press('w')
    # Unequal runs stay as they are: q.py's three removed lines against two added. Only r.py's reindent is hidden.
    assert changed() == everything[:5], changed()
    assert page.locator('#space').inner_text() == 'Show whitespace'
    assert page.locator('tr', has_text='a = 1').locator('.d2h-cntx').count() > 0
    page.keyboard.press('w')
    assert changed() == everything


_PAIRED = """diff --git a/p.py b/p.py
--- a/p.py
+++ b/p.py
@@ -1,3 +1,3 @@
 def f():
-  return 1
+    return 1
 x = 3
"""


def test_ignore_whitespace_pairs(page, url, work):
    _open(page, url, work, {'name': 'git diff', 'kind': 'diff', 'text': _PAIRED})
    page.keyboard.press('w')
    assert page.locator('.d2h-ins, .d2h-del').count() == 0
    # A comment on the reindented line is on the new file's line 2 either way.
    page.locator('tr', has_text='return 1').locator('.d2h-code-linenumber').click()
    _comment(page, 'ok')
    assert [(c['start'], c['old']) for c in _review(page)['comments']] == [(2, False)]


def test_folded_files(page, url, work):
    # Outside a diff there is no file to mark or fold.
    _open(page, url, work, {'name': 'a.md', 'kind': 'md', 'text': _MD})
    page.keyboard.press('x')
    page.keyboard.press('o')
    assert page.locator('.folded, .viewed').count() == 0

    files = ['src/app.py', 'Cargo.lock', 'ui/x.min.js', 'src/lib.py']
    diff = ''.join(_DIFF.replace('src/app.py', f) for f in files)
    _open(page, url, work, {'name': 'git diff', 'kind': 'diff', 'text': diff})
    folded = lambda: page.eval_on_selector_all(
        '.d2h-file-wrapper', "ws => ws.map((w) => w.classList.contains('folded'))"
    )
    ticked = lambda: page.locator('#files a.viewed .base').all_inner_texts()
    cursor = lambda: page.evaluate("""() => {
        const el = document.querySelector('.focused');
        const file = [...document.querySelectorAll('.d2h-file-wrapper')].indexOf(el.closest('.d2h-file-wrapper'));
        return el.dataset.key ? el.dataset.key.split('\\n').slice(2).join(' ') : `header ${file}`;
    }""")
    # Lock and minified files start folded, not viewed; the others open.
    assert folded() == [False, True, True, False] and ticked() == []
    # The cursor steps over a folded file's lines, onto its header.
    for _ in range(5):
        page.keyboard.press('j')
    assert cursor() == 'src/app.py 4-4', cursor()
    page.keyboard.press('j')
    assert cursor() == 'header 1', cursor()
    page.keyboard.press('Enter')  # a header takes no comment
    assert page.locator('.vjuu-c').count() == 0
    page.keyboard.press('j')
    page.keyboard.press('j')
    assert cursor() == 'src/lib.py 1-1', cursor()
    page.keyboard.press('ArrowLeft')  # previous file: folded, so its header
    assert cursor() == 'header 2', cursor()
    page.keyboard.press('k')
    page.keyboard.press('o')  # opens it, the cursor on its first line
    assert folded() == [False, False, True, False] and cursor() == 'Cargo.lock 1-1', cursor()
    # Viewed folds the file, ticks it, and leaves the cursor on its header; again unmarks and opens it.
    page.keyboard.press('x')
    assert folded() == [False, True, True, False] and ticked() == ['Cargo.lock'] and cursor() == 'header 1'
    assert page.locator('.d2h-file-wrapper').nth(1).locator('.viewed input').is_checked()
    page.keyboard.press('x')
    assert folded() == [False, False, True, False] and ticked() == []
    # The mouse: the Viewed box, and the header to fold or open; the keys still work afterwards.
    page.locator('.d2h-file-wrapper').nth(3).locator('.viewed').click()
    page.locator('.d2h-file-wrapper').nth(2).locator('.d2h-file-name').click()
    assert folded() == [False, False, False, True] and ticked() == ['lib.py']
    page.keyboard.press('v')  # survives a re-render
    assert folded() == [False, False, False, True] and ticked() == ['lib.py']
    assert page.locator('.d2h-file-wrapper').nth(3).locator('.viewed input').is_checked()


def test_comment_box_wraps_within_view(page, url, work):
    # A line wider than the view makes the comment row as wide as the table; the box must stay visible.
    long = _DIFF.replace('+def new():', '+def new(): ' + 'pass; ' * 80)
    for view in ('unified', 'side-by-side'):
        _open(page, url, work, {'name': 'git diff', 'kind': 'diff', 'text': long})
        if view == 'side-by-side':
            page.keyboard.press('v')  # the view is not remembered, so each _open starts unified
        page.locator('tr', has=page.locator('.d2h-ins')).first.click()
        page.keyboard.type('a comment long enough to wrap ' * 10)
        box, pane = page.evaluate('''() => {
            const ta = document.querySelector('.vjuu-c textarea');
            return [ta, ta.closest('.d2h-file-diff, .d2h-file-side-diff')]
                .map((e) => { const r = e.getBoundingClientRect(); return [Math.round(r.left), Math.round(r.right)]; });
        }''')
        assert pane[0] <= box[0] and box[1] <= pane[1], (view, box, pane)


def test_markdown_file_in_a_diff_uses_code_size(page, url, work):
    # diff2html puts a `markdown` class on highlighted lines of .md files; document styles must not apply to them.
    md_diff = _DIFF.replace('src/app.py', 'README.md')
    _open(
        page,
        url,
        work,
        {'name': 'git diff', 'kind': 'diff', 'text': md_diff},
        {'name': 'x.md', 'kind': 'md', 'text': _MD},
    )
    page.wait_for_selector(
        '.d2h-code-line-ctn.markdown, .d2h-code-line-ctn .hljs-section, .d2h-code-line-ctn', state='attached'
    )
    page.wait_for_timeout(300)  # highlighting runs after paint
    size = lambda sel: page.evaluate(f"getComputedStyle(document.querySelector('{sel}')).fontSize")
    assert size('.d2h-code-line-ctn') == size('.d2h-code-linenumber') or size('.d2h-code-line-ctn') == '13px', size(
        '.d2h-code-line-ctn'
    )
    assert size('article.markdown p') == '16px'


def test_markdown_html_is_not_rendered(page, url, work):
    _open(page, url, work, {'name': 'x.md', 'kind': 'md', 'text': 'hi <img src=x onerror="window.__pwned=1">\n'})
    assert page.locator('#content img').count() == 0
    assert page.evaluate('window.__pwned') is None


_HTML = """<!doctype html>
<html><head><link rel="stylesheet" href="site.css"></head><body>
<h1>Title</h1>
<section id="install"><p>Run it.</p><p>Then <b>check</b> it.</p></section>
<h2>More</h2>
<p>Plain</p>
<ul><li>one</li><li>two</li></ul>
<a href="other.html">away</a>
<style>:root { --big: 30px; } #app p { font: 700 var(--big) serif; font-variant-numeric: tabular-nums; }</style>
<style id="injected"></style>
<div id="app"></div><noscript><p>Enable JavaScript</p></noscript>
<canvas id="chart" width="20" height="10"></canvas>
<script>
document.getElementById('app').innerHTML = '<p>Built by script</p>';
document.getElementById('chart').getContext('2d').fillRect(0, 0, 20, 10);
document.getElementById('injected').sheet.insertRule('#app p { color: rgb(4, 5, 6); }');
try { parent.__ran = 1 } catch { document.body.dataset.isolated = 'yes' }
</script>
<img src="missing.png" onerror="try { parent.__pwned = 1 } catch {}">
</body></html>
"""


def test_html_page(page, url, work):
    files = work / 'files' / '0'
    files.mkdir(parents=True, exist_ok=True)
    (files / 'site.css').write_text('p { color: rgb(1, 2, 3); }')
    (work / 'doc.json').write_text(json.dumps({'docs': [{'name': 'site/index.html', 'kind': 'html', 'text': _HTML}]}))
    page.goto(url)
    frame = page.frame_locator('iframe')
    frame.locator('[data-key]').first.wait_for()
    inner = page.frames[1]
    # The page's stylesheet loads relative to the file; it stays in standards mode.
    assert inner.evaluate("getComputedStyle(document.querySelector('p')).color") == 'rgb(1, 2, 3)'
    assert inner.evaluate('document.compatMode') == 'CSS1Compat'
    # Its scripts ran, walled off from vjuu's page; what is shown is their result, with the scripts gone.
    assert page.evaluate('[window.__ran, window.__pwned]') == [None, None]
    assert inner.evaluate('document.body.dataset.isolated') == 'yes'
    assert frame.get_by_text('Built by script').is_visible()
    # Styles survive the copy: rules inserted by script, and a shorthand with var() that the CSSOM would mangle.
    built = "getComputedStyle(document.querySelector('#app p'))"
    assert inner.evaluate(f'[{built}.color, {built}.fontSize]') == ['rgb(4, 5, 6)', '30px']
    assert inner.evaluate("document.querySelectorAll('script, noscript, canvas').length") == 0
    assert inner.evaluate("document.querySelector('img[alt=chart]').src").startswith('data:image/png')
    frame.get_by_text('Run it.').click()
    _comment(page, 'say how')
    frame.get_by_text('two').click()
    _comment(page, 'drop')
    # A link neither comments nor leaves the page.
    frame.get_by_text('away').click()
    assert frame.locator('.vjuu-c').count() == 2
    assert inner.url == 'about:srcdoc'
    # Selected text is quoted on its own, under the nearest heading.
    inner.evaluate("""() => {
        const r = document.createRange();
        r.selectNodeContents(document.querySelector('b'));
        getSelection().removeAllRanges();
        getSelection().addRange(r);
        document.dispatchEvent(new MouseEvent('mouseup'));
    }""")
    page.locator('#sel-comment').wait_for(state='visible', timeout=5000)
    page.keyboard.press('c')
    _comment(page, 'how?')
    # Theme colours reach the comment boxes inside the page.
    _set_theme(work, page, 'graphite')
    assert inner.evaluate("getComputedStyle(document.querySelector('.vjuu-c')).backgroundColor") == 'rgb(46, 46, 46)'
    review = _review(page)
    assert review == {
        'comments': [
            {
                'file': 'site/index.html',
                'start': 0,
                'end': 0,
                'old': False,
                'quote': 'Run it.',
                'at': '#install',
                'text': 'say how',
            },
            {
                'file': 'site/index.html',
                'start': 0,
                'end': 0,
                'old': False,
                'quote': 'two',
                'at': 'under "More"',
                'text': 'drop',
            },
            {
                'file': 'site/index.html',
                'start': 0,
                'end': 0,
                'old': False,
                'quote': 'check',
                'at': '#install',
                'text': 'how?',
            },
        ],
        'overall': '',
    }, review
    # A page without scripts is shown as written, without the round trip.
    static = '<!doctype html><p>Static</p>'
    (work / 'doc.json').write_text(json.dumps({'docs': [{'name': 's.html', 'kind': 'html', 'text': static}]}))
    page.goto(url)
    frame.get_by_text('Static').wait_for(timeout=2000)


_CHOICE = """# Plan

```choice
How should vjuu handle artifacts?
1. Snapshot (recommended): small change, one frozen state
2: Live page: interactive, much bigger
- Both" onmouseover="window.__pwned=1: <img src=x onerror="window.__pwned=1">
```

Done.
"""


def test_choices(page, url, work):
    _open(page, url, work, {'name': 'plan.md', 'kind': 'md', 'text': _CHOICE})
    options = page.locator('.choice button')
    assert [' '.join(t.split()) for t in options.all_inner_texts()][:2] == [
        'Snapshot (recommended) small change, one frozen state',
        'Live page interactive, much bigger',
    ]
    # Option text is text, never markup.
    options.nth(2).hover()
    assert page.locator('.choice img').count() == 0
    assert page.evaluate('window.__pwned') is None
    choices = lambda: _review(page)['choices']
    options.nth(1).click()
    assert choices() == [{'question': 'How should vjuu handle artifacts?', 'answer': 'Live page'}]
    # A pick is not a comment, and picking another replaces it.
    assert page.locator('.vjuu-c').count() == 0
    options.nth(0).click()
    assert choices() == [{'question': 'How should vjuu handle artifacts?', 'answer': 'Snapshot (recommended)'}]
    assert page.locator('.choice button.picked').all_inner_texts()[0].startswith('Snapshot')
    # Picking it again takes it back; the question can still be commented on.
    options.nth(0).click()
    assert choices() == [{'question': 'How should vjuu handle artifacts?', 'answer': ''}]
    page.locator('.choice .question').click()
    _comment(page, 'neither')
    assert [c['text'] for c in _review(page)['comments']] == ['neither']


def test_choice_keys(page, url, work):
    _open(page, url, work, {'name': 'plan.md', 'kind': 'md', 'text': _CHOICE})
    answer = lambda n=0: _review(page)['choices'][n]['answer']
    # The only choice block in the review: digits pick without moving the cursor there.
    page.keyboard.press('2')
    assert answer() == 'Live page'
    # Picks are orange, apart from the blue cursor.
    picked = "getComputedStyle(document.querySelector('.choice button.picked')).borderTopColor"
    assert page.evaluate(picked) == 'rgb(188, 76, 0)'
    page.keyboard.press('2')  # again: taken back
    assert answer() == ''
    page.keyboard.press('7')  # no such option
    assert answer() == ''
    two = _CHOICE + '\n```choice\nShip it?\n- Yes\n- No\n```\n'
    _open(page, url, work, {'name': 'plan.md', 'kind': 'md', 'text': two})
    # Two blocks and the cursor on neither: a digit could mean either, so it does nothing.
    page.keyboard.press('1')
    assert page.locator('.choice button.picked').count() == 0
    page.locator('.choice').nth(1).locator('.question').click()
    page.keyboard.press('Escape')  # the click opened a comment; the cursor stays on the block
    page.keyboard.press('2')
    assert [c['answer'] for c in _review(page)['choices']] == ['', 'No']


def test_settings(page, url, work):
    (work / 'config.json').write_text(json.dumps(_CONFIG))
    msgs = lambda kind: [m for m in page.evaluate('window.__msgs || []') if m['type'] == kind]
    # A first window opens on the panel, showing the saved settings, over its document.
    (work / 'doc.json').write_text(
        json.dumps({'docs': [{'name': 'a.md', 'kind': 'md', 'text': _MD}], 'settings': True})
    )
    page.goto(url)
    page.wait_for_selector('#settings[open]')
    checked = lambda: [
        page.is_checked(f'#settings [name={h}]')
        for h in ('plan', 'ask', 'turn', 'permission', 'gate', 'tray', 'focus')
    ]
    assert checked() == [True, False, True, False, True, True, False]
    # Review keys do nothing while it is open.
    page.keyboard.press('j')
    assert page.locator('.focused').count() == 0
    # Nothing is saved until Apply; Esc throws the change away.
    page.click('#settings [name=turn]')
    assert msgs('settings') == []
    page.keyboard.press('Escape')
    assert not page.is_visible('#settings') and msgs('settings') == [] and msgs('done') == []
    # The gear opens it again with the saved settings; Apply saves and leaves the review open.
    page.click('#settings-open')
    page.wait_for_selector('#settings[open]')
    assert checked() == [True, False, True, False, True, True, False]
    page.click('#settings [name=turn]')
    page.keyboard.press('Control+Enter')  # Apply, and nothing else
    assert msgs('settings')[-1]['config'] == {
        'tray': True,
        'focus': False,
        'hooks': {'plan': True, 'ask': False, 'turn': False, 'permission': False, 'gate': True},
        'theme': {'name': 'auto', 'light': 'light', 'dark': 'graphite'},
    }
    assert not page.is_visible('#settings') and msgs('done') == []
    # `vjuu config`: the panel over a sample to preview themes on; the window finishes when the panel closes.
    sample = [{'name': 'README.md', 'kind': 'md', 'text': _MD}, {'name': 'sample.diff', 'kind': 'diff', 'text': _DIFF}]
    (work / 'doc.json').write_text(json.dumps({'docs': sample, 'settings': True, 'sample': True}))
    page.goto(url)
    page.wait_for_selector('#settings[open]')
    assert page.locator('.d2h-file-wrapper').count() == 1 and page.locator('article.markdown h1').count() == 1
    page.click('#settings-apply')
    # A dialog's close event arrives as a task of its own.
    page.wait_for_function("(window.__msgs || []).some((m) => m.type === 'done')", timeout=2000)
    assert msgs('done') == [{'type': 'done', 'verdict': 'discard'}]


def test_help(page, url, work):
    (work / 'config.json').write_text(json.dumps(_CONFIG))
    _open(page, url, work, {'name': 'plan.md', 'kind': 'md', 'text': _MD})
    page.get_by_text('First paragraph').click()
    # F1 works while typing a comment too, and the review keys wait until the help closes.
    page.keyboard.press('F1')
    page.wait_for_selector('#help[open]')
    assert page.inner_text('#version') == _CONFIG['version']
    assert page.evaluate("document.querySelector('#help img').naturalWidth") > 0
    page.keyboard.press('Control+,')
    assert not page.is_visible('#settings')
    page.keyboard.press('Escape')
    assert not page.is_visible('#help')
    # The help lists every key the README does.
    readme = (_UI.parent / 'README.md').read_text(encoding='utf-8')
    keys = readme.split('## Keys')[1].split('\n\n')[1]
    assert keys.count('\n| ') == page.locator('#help tr').count(), keys


def _tab(id, name, active=False, unseen=False, kind=None):
    return {'id': id, 'name': name, 'kind': kind, 'title': f'{name} · 1 min', 'active': active, 'unseen': unseen}


def test_tabs(page, url, work):
    _open(page, url, work, {'name': 'plan.md', 'kind': 'md', 'text': _MD})
    sent = lambda: [m for m in page.evaluate('window.__msgs || []') if m['type'] in ('show', 'close', 'quit', 'done')]
    # One review: no strip, and the tab keys do nothing.
    page.evaluate('(t) => vjuuTabs(t)', [_tab(0, 'fix login', active=True)])
    assert page.locator('#tabs').is_hidden() and page.locator('#name').is_visible()
    page.keyboard.press('Control+Tab')
    page.keyboard.press('Control+1')
    assert sent() == []
    tabs = [_tab(0, 'fix login', active=True, kind='plan'), _tab(3, 'refactor', unseen=True), _tab(5, 'a.md')]
    page.evaluate('(t) => vjuuTabs(t)', tabs)
    assert page.locator('#tabs').is_visible() and page.locator('#name').is_hidden()
    strip = page.locator('#tabs .tab')
    assert [' '.join(t.split()) for t in strip.all_inner_texts()] == ['fix login plan ×', 'refactor ×', 'a.md ×']
    assert strip.nth(1).evaluate("e => e.classList.contains('unseen')") and strip.nth(0).get_attribute('title') == (
        'fix login · 1 min (Ctrl+1)'
    )
    # Next, previous (wrapping), a digit; the tab already shown is not asked for again; plain Tab is not a tab key.
    page.keyboard.press('Tab')
    page.keyboard.press('Control+Tab')
    page.keyboard.press('Control+Shift+Tab')
    page.keyboard.press('Control+3')
    page.keyboard.press('Control+1')
    page.keyboard.press('Control+4')
    assert sent() == [{'type': 'show', 'tab': 3}, {'type': 'show', 'tab': 5}, {'type': 'show', 'tab': 5}], sent()
    # While typing a comment too, which keeps its text.
    page.get_by_text('First paragraph').click()
    page.keyboard.type('half')
    page.keyboard.press('Control+2')
    assert sent()[-1] == {'type': 'show', 'tab': 3} and page.input_value('.vjuu-c textarea') == 'half'
    # The mouse: a tab shows it, its × closes it.
    strip.nth(2).locator('.tab-name').click()
    strip.nth(1).locator('.tab-close').click()
    assert sent()[-2:] == [{'type': 'show', 'tab': 5}, {'type': 'close', 'tab': 3}], sent()
    # Ctrl+W closes this tab, discarding its review.
    page.keyboard.press('Control+w')
    assert sent()[-1] == {'type': 'done', 'verdict': 'discard'}


def main():
    with tempfile.TemporaryDirectory() as tmp, sync_playwright() as p:
        work = Path(tmp) / 'ui'
        shutil.copytree(_UI, work)
        # What vjuu serves at /themes/<name>.css is the theme's declarations; the files themselves do the same here.
        shutil.copytree(_THEMES, work / 'themes')
        shutil.copy(_UI.parent / 'assets' / 'icons' / 'vjuu.svg', work / 'logo.svg')
        url = _serve(work)
        browser = p.chromium.launch(channel='msedge')
        errors = []
        only = sys.argv[1:]
        for test in (
            test_diff,
            test_verdicts,
            test_markdown,
            test_revised_file,
            test_highlighting_and_themes,
            test_session_name,
            test_multiple_documents,
            test_selection_comments,
            test_code_file,
            test_clicks_that_do_not_comment,
            test_keyboard_navigation,
            test_unsaved_comment_is_kept,
            test_same_file_twice_in_one_diff,
            test_directory_tree,
            test_ignore_whitespace,
            test_ignore_whitespace_pairs,
            test_folded_files,
            test_comment_box_wraps_within_view,
            test_markdown_file_in_a_diff_uses_code_size,
            test_markdown_html_is_not_rendered,
            test_html_page,
            test_choices,
            test_choice_keys,
            test_settings,
            test_help,
            test_tabs,
        ):
            if only and test.__name__ not in only:
                continue
            # Every test starts in the Light theme, as compiled into theme.css.
            shutil.copy(_THEMES / 'light.css', work / 'theme.css')
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
