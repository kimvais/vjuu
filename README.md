# [vjuu](https://vjuu.it)

A fast review window for agent handoffs. When an agent would say "open X and review it", it runs
`vjuu X` instead: a window opens with the markdown or diff, you comment on lines, click **Done**, and
your comments come back to the agent as markdown on stdout, anchored to `file:line`.

## Usage

```
vjuu docs/design.md          # markdown
vjuu a.md b.md change.diff   # several files in one window, stacked
vjuu change.diff             # diff or patch file
vjuu site/index.html         # HTML page or claude.ai artifact, rendered as it looks once loaded; file:// URLs work too
vjuu src/main.rs             # any other file: line-numbered, syntax-highlighted source
vjuu --git main...HEAD       # runs `git diff main...HEAD` in the current directory
git show HEAD | vjuu -       # stdin; diff or markdown is detected
vjuu --session "fix login" plan.md  # name the asking session in the title bar (automatic under Claude Code)
vjuu last                    # print the most recent review again
vjuu stop                    # stop the resident instance (before upgrading the exe)
vjuu setup                   # install the skill for Claude Code and Codex
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
| `t` | Next theme: Auto, Light, Graphite |
| `f` | Hide / show the sidebar (file list, or table of contents for markdown) |
| `Ctrl+Shift+Enter` | Done, from anywhere |

Click any line or block to comment on it. To comment on a range, select text (across lines, paragraphs or diff
rows) and click **Comment** or press `c`. Links still open, except on HTML pages, where only in-page links work; clicks
inside a comment edit it.

Themes: **Light** (GitHub) and **Graphite** (Monokai code colours on neutral dark grey). **Auto** picks one from the
system's light/dark setting. The choice is remembered.

## Output

```
# Review: docs/design.md (2 comments)

## docs/design.md:12-14
> quoted source lines

The comment.

## Overall
General comment, if any.
```

Deleted diff lines are marked `(old)` and use the old file's line number. A rendered HTML page has no line numbers:
its comments quote the text and say where it is, as `## site/index.html (#install)` (the nearest id) or
`## site/index.html (under "Install")` (the nearest heading).

## Install

Runs on Windows 10/11 (WebView2), Linux (WebKitGTK 4.1: Debian 12+, Ubuntu 22.04+) and macOS (WKWebView). On
Windows the window is frameless with its own title bar; macOS and Linux keep the native frame. macOS builds are
compiled and tested in CI but not by hand, and are not signed or notarised.

| | Install |
|---|---|
| Windows | `winget install vjuu` |
| macOS | `brew install kimvais/tap/vjuu` |
| Debian, Ubuntu | add the apt source below once, then `sudo apt install vjuu` |
| Anything else | a `.zip`, `.tar.gz` or `.deb` from [GitHub Releases](https://github.com/kimvais/vjuu/releases) |

```
curl -fsSL https://vjuu.it/apt/vjuu.gpg | sudo tee /usr/share/keyrings/vjuu.gpg >/dev/null
echo "deb [signed-by=/usr/share/keyrings/vjuu.gpg] https://vjuu.it/apt stable main" | sudo tee /etc/apt/sources.list.d/vjuu.list
sudo apt update && sudo apt install vjuu
```

A macOS binary downloaded by hand needs `xattr -d com.apple.quarantine vjuu` once, since it is not notarised
(Homebrew installs don't). To build from source you need a Rust toolchain, and on Linux also
`libwebkit2gtk-4.1-dev` and `libgtk-3-dev`:
```
cargo install --locked --git https://github.com/kimvais/vjuu
```

Then, however you installed it:
```
vjuu setup
```

It installs the `vjuu` skill, which tells agents to run `vjuu` in the background instead of asking you to open
files, for Claude Code (`~/.claude/skills`) and Codex (`~/.agents/skills`), whichever you have. Run it again after
an upgrade. The skill is also a Claude Code plugin (`claude plugin install vjuu@vjuu` after
`claude plugin marketplace add kimvais/vjuu`); use one or the other, not both.

## Development

- `uvx pre-commit install` once: formats with rustfmt and ruff on every commit.
- `cargo test`: review formatter and diff detection.
- `uv run --no-project --with playwright python tests/ui_test.py`: drives `ui/` in headless Edge with a
  stubbed `window.ipc` and checks the review the page reports.
- `tests/keyboard_focus.ps1 -HomeHwnd <hwnd>`: manual check that a freshly opened window takes keyboard focus from
  another app (cold and resident). It types real keys, so run it by hand.
- `site/` is vjuu.it, deployed with the apt repository by `.github/workflows/pages.yml`.
  `uv run --no-project --with playwright python tests/screenshots.py` re-renders its screenshots.
- `VJUU_AUTOCLOSE=1 vjuu <file>` exits as soon as the page has rendered and prints the time to
  stderr, for startup measurements.

UI libraries are vendored in `ui/vendor` (markdown-it 15.0.3, diff2html 3.4.56 base bundle, highlight.js
11.12.0 common languages, Inter and JetBrains Mono variable fonts) and embedded in the exe. Markdown is rendered with raw HTML off: documents are agent output and the page can talk to vjuu. For the same reason
an HTML page's scripts run only in a sandboxed iframe with an origin of its own, which cannot reach vjuu's page; vjuu
also ignores messages from anywhere but its own page. Once the page stops changing (or after 3 s), it sends back a copy
of itself, canvases drawn as images, and you comment on that copy in a second iframe with scripts off. So you review
one state of the page: buttons, tabs and timers do not work, and network requests the page makes still go out.

## License

MIT, see [LICENSE](LICENSE). Bundled libraries and fonts keep their own licenses, in [ui/vendor/licenses](ui/vendor/licenses): markdown-it and diff2html (MIT), highlight.js (BSD-3-Clause), Inter and JetBrains Mono (SIL Open Font License 1.1).
