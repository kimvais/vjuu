# vjuu

A fast review window for agent handoffs. When an agent would say "open X and review it", it runs
`vjuu X` instead: a window opens with the markdown or diff, you comment on lines, click **Done**, and
your comments come back to the agent as markdown on stdout, anchored to `file:line`.

## Usage

```
vjuu docs/design.md          # markdown
vjuu a.md b.md change.diff   # several files in one window, stacked
vjuu change.diff             # diff or patch file
vjuu src/main.rs             # any other file: line-numbered, syntax-highlighted source
vjuu --git main...HEAD       # runs `git diff main...HEAD` in the current directory
git show HEAD | vjuu -       # stdin; diff or markdown is detected
vjuu last                    # print the most recent review again
vjuu stop                    # stop the resident instance (before upgrading the exe)
```

`vjuu` blocks until you click **Done** or close the window. Every review is also saved to
`%LOCALAPPDATA%\vjuu\reviews\`, so a later session can pick it up with `vjuu last`.

## Why it opens fast

The first `vjuu` opens its window itself (~400 ms, mostly WebView2 starting) and starts a hidden
resident instance that keeps a pre-warmed window. Later calls hand the document to it over a named
pipe and the window appears almost at once. The spare window preloads fonts and renderers. The resident instance costs roughly 300 MB working set across
its WebView2 processes; `vjuu stop` ends it. `VJUU_COLD=1` skips it.

| Case | First paint |
|------|-------------|
| Markdown or diff, cold | ~400-450 ms |
| Markdown, resident | ~15 ms |
| 6,600-line diff, resident | ~100-130 ms (highlighting runs after paint) |

## Keys

| Key | Action |
|-----|--------|
| `↑` / `↓` (or `k` / `j`) | Previous / next line or block |
| `←` / `→` (or `p` / `n`) | Previous / next file; with a single markdown file, previous / next heading |
| `Enter` (or `c`) | Comment on the current line, or on the selected text |
| `Ctrl+Enter` | In a comment: save it. Anywhere else: **Done** |
| `Esc` | Cancel the comment being written |
| `v` | Toggle unified / side-by-side diff |
| `t` | Next theme: Auto, Light, Monokai, Graphite |
| `f` | Hide / show the sidebar (file list, or table of contents for markdown) |
| `Ctrl+Shift+Enter` | Done, from anywhere |

Click any line or block to comment on it. To comment on a range, select text (across lines, paragraphs or diff
rows) and click **Comment** or press `c`. Links still open; clicks inside a comment edit it.

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

Runs on Windows 10/11 (WebView2), Linux (WebKitGTK 4.1: Debian 12+, Ubuntu 22.04+) and macOS (WKWebView). On
Windows the window is frameless with its own title bar; macOS and Linux keep the native frame. macOS builds are
compiled and tested in CI but not by hand, and are not signed or notarised.

Download a build from [GitHub Releases](https://github.com/kimvais/vjuu/releases): a `.zip` for Windows, a `.deb` or
`.tar.gz` for Linux (`sudo apt install ./vjuu_*.deb` pulls in WebKitGTK), a `.tar.gz` for macOS (run
`xattr -d com.apple.quarantine vjuu` once, since the binary is not notarised). Or build it with a Rust toolchain
(Linux also needs `libwebkit2gtk-4.1-dev` and `libgtk-3-dev`):
```
cargo install --locked --git https://github.com/kimvais/vjuu
claude plugin marketplace add kimvais/vjuu
claude plugin install vjuu@vjuu
```

The plugin adds the `vjuu` skill, which tells agents to run `vjuu` in the background instead of
asking you to open files.

## Development

- `cargo test`: review formatter and diff detection.
- `uv run --no-project --with playwright python tests/ui_test.py`: drives `ui/` in headless Edge with a
  stubbed `window.ipc` and checks the review the page reports.
- `tests/keyboard_focus.ps1 -HomeHwnd <hwnd>`: manual check that a freshly opened window takes keyboard focus from
  another app (cold and resident). It types real keys, so run it by hand.
- `VJUU_AUTOCLOSE=1 vjuu <file>` exits as soon as the page has rendered and prints the time to
  stderr, for startup measurements.

UI libraries are vendored in `ui/vendor` (markdown-it 15.0.3, diff2html 3.4.56 base bundle, highlight.js
11.12.0 common languages, Inter and JetBrains Mono variable fonts) and embedded in the exe. Markdown is rendered with raw HTML off: documents are agent output and the page can talk to vjuu.

## License

MIT, see [LICENSE](LICENSE). Bundled libraries and fonts keep their own licenses, in [ui/vendor/licenses](ui/vendor/licenses): markdown-it and diff2html (MIT), highlight.js (BSD-3-Clause), Inter and JetBrains Mono (SIL Open Font License 1.1).
