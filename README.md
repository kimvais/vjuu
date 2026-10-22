# revu

A fast review window for agent handoffs. When an agent would say "open X and review it", it runs
`revu X` instead: a window opens with the markdown or diff, you comment on lines, click **Done**, and
your comments come back to the agent as markdown on stdout, anchored to `file:line`.

## Usage

```
revu docs/design.md          # markdown
revu a.md b.md change.diff   # several files in one window, stacked
revu change.diff             # diff or patch file
revu --git main...HEAD       # runs `git diff main...HEAD` in the current directory
git show HEAD | revu -       # stdin; diff or markdown is detected
revu last                    # print the most recent review again
revu stop                    # stop the resident instance (before upgrading the exe)
```

`revu` blocks until you click **Done** or close the window. Every review is also saved to
`%LOCALAPPDATA%\revu\reviews\`, so a later session can pick it up with `revu last`.

## Why it opens fast

The first `revu` opens its window itself (~400 ms, mostly WebView2 starting) and starts a hidden
resident instance that keeps a pre-warmed window. Later calls hand the document to it over a named
pipe and the window appears almost at once. The spare window preloads fonts and renderers. The resident instance costs roughly 300 MB working set across
its WebView2 processes; `revu stop` ends it. `REVU_COLD=1` skips it.

| Case | First paint |
|------|-------------|
| Markdown or diff, cold | ~400-450 ms |
| Markdown, resident | ~15 ms |
| 6,600-line diff, resident | ~100-130 ms (highlighting runs after paint) |

## Keys

| Key | Action |
|-----|--------|
| `j` / `k` | Next / previous line or block |
| `n` / `p` | Next / previous file (diff) or heading (markdown) |
| `c` or `Enter` | Comment on the focused line or block |
| `Ctrl+Enter` / `Esc` | Save / cancel the comment being written |
| `v` | Toggle unified / side-by-side diff |
| `t` | Next theme: Auto, Light, Monokai, Graphite |
| `f` | Hide / show the sidebar (file list, or table of contents for markdown) |
| `Ctrl+Shift+Enter` | Done |

Fastest: select text (a word, a paragraph, several diff lines) and click **Comment** or press `c`; the
comment covers every line the selection touches. You can also click the `+` that appears on hover, or a diff
line number.

Themes: **Light** (GitHub), **Monokai** (its own warm dark grey), **Graphite** (Monokai code colours on
neutral grey). **Auto** picks Light or Monokai from the Windows setting. The choice is remembered.

## Output

```
# Review: docs/design.md (2 comments)

## docs/design.md:12-14
> quoted source lines

The comment.

## Overall
General comment, if any.
```

Deleted diff lines are marked `(old)` and use the old file's line number.

## Install

```
cargo install --path .
claude plugin marketplace add C:\Users\k\p\revu
claude plugin install revu@revu
```

The plugin adds the `revu` skill, which tells agents to run `revu` in the background instead of
asking you to open files.

## Development

- `cargo test`: review formatter and diff detection.
- `uv run --no-project --with playwright python tests/ui_test.py`: drives `ui/` in headless Edge with a
  stubbed `window.ipc` and checks the review the page reports.
- `REVU_AUTOCLOSE=1 revu <file>` exits as soon as the page has rendered and prints the time to
  stderr, for startup measurements.

UI libraries are vendored in `ui/vendor` (markdown-it 15.0.3, diff2html 3.4.56 base bundle, highlight.js
11.12.0 common languages, Inter and JetBrains Mono variable fonts) and embedded in the exe. Markdown is rendered with raw HTML off: documents are agent output and the page can talk to revu.
