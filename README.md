# revu

A fast review window for agent handoffs. When an agent would say "open X and review it", it runs
`revu X` instead: a window opens with the markdown or diff, you comment on lines, click **Done**, and
your comments come back to the agent as markdown on stdout, anchored to `file:line`.

## Usage

```
revu docs/design.md          # markdown
revu change.diff             # diff or patch file
revu --git main...HEAD       # runs `git diff main...HEAD` in the current directory
git show HEAD | revu -       # stdin; diff or markdown is detected
revu last                    # print the most recent review again
```

`revu` blocks until you click **Done** or close the window. Every review is also saved to
`%LOCALAPPDATA%\revu\reviews\`, so a later session can pick it up with `revu last`.

## Keys

| Key | Action |
|-----|--------|
| `j` / `k` | Next / previous line or block |
| `n` / `p` | Next / previous file (diff) or heading (markdown) |
| `c` or `Enter` | Comment on the focused line or block |
| `Ctrl+Enter` / `Esc` | Save / cancel the comment being written |
| `v` | Toggle unified / side-by-side diff |
| `Ctrl+Shift+Enter` | Done |

You can also click the `+` next to a line or block, or click a diff line number.

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

`cargo test` runs the formatter tests. `REVU_AUTOCLOSE=1 revu <file>` exits as soon as the page has
rendered and prints the time to stderr, for startup measurements.
