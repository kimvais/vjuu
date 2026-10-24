---
name: vjuu
description: Use whenever you would ask the user to open, read, look over or review a file, document, plan, spec, diff, patch, commit or branch, and when the user says "let me review", "show me", "/vjuu" or "review it in vjuu". MUST also be used whenever your reply to the user would be longer than 20 lines: write it to a markdown file and open that in vjuu instead of printing it. Opens the content in the vjuu window, where the user comments on lines and picks between options you offer; the comments and picks come back as the command's output.
---

# vjuu

`vjuu` shows a markdown file or a diff in a window where the user leaves line comments. It blocks
until they click Done, then prints the review as markdown on stdout. Use it instead of saying "open X
and review it" or pasting long content into chat for review.

## Replies longer than 20 lines

If what you are about to say to the user runs past 20 lines, you MUST NOT print it. Write it to a `.md` file (in
your scratch or temp directory, not the repository), run `vjuu` on it, and say in one line that it is open. Code
blocks, tables and lists count towards the 20 lines. Short replies stay in chat.

## Offer choices as buttons

When the user has to pick between options, put them in a `choice` block. vjuu shows each option as a button, and
the pick comes back with the review:

````
```choice
How should vjuu handle artifacts?
1. Snapshot (recommended): small change, one frozen state
2. Live page: interactive, much bigger
```
````

List items (`- `, `1. `, `1:`) are the options; any other line is the question. Text before the first `: ` is the
label that comes back, the rest is shown as the explanation. One block per question.

## Run it

Always run it with the Bash tool's `run_in_background: true`. A review can take longer than any
foreground timeout, and you are notified when it exits. Do not poll; wait for the notification.

| What to review | Command |
|---|---|
| A markdown file | `vjuu path/to/file.md` |
| Several files at once | `vjuu plan.md notes.md change.diff` (one window, one review) |
| Uncommitted changes | `vjuu --git` (add `--staged`, `HEAD`, `main...HEAD`, paths: any `git diff` args) |
| A commit | `git show <sha> \| vjuu -` |
| A diff or patch file | `vjuu change.diff` |
| Source files (whole files, not a diff) | `vjuu src/main.rs src/review.rs` |
| An HTML page or artifact, rendered as it looks once loaded | `vjuu site/index.html` |
| Text you generated | write it to a `.md` file first, then `vjuu` that file |

Run `--git` from inside the repository. Under Claude Code the title bar shows the session's `/rename` name by
itself; elsewhere put `--session "<name>"` first (`vjuu --session "fix login" plan.md`) so the user can
tell which session's window this is. Tell the user in one line that the review window is open.

## Read the result

Output looks like:

```
# Review: docs/plan.md (2 comments)

## Choice: How should vjuu handle artifacts?
Snapshot (recommended)

## docs/plan.md:12-14
> quoted lines the comment is on

the user's comment

## Overall
general comment
```

- `## Choice: <question>` is followed by the label the user picked, or `No answer.`: ask again rather than pick
  for them.
- Each `## file:line` section is one comment; `(old)` means a deleted line, numbered in the old file.
- A rendered HTML page has no line numbers: `## site/index.html (#install)` or `(under "Install")` names the nearest
  id or heading. Find the quoted text in the source near it.
- Treat comments as the user's instructions for that spot: act on each one, then say per comment what
  you did. A question in a comment gets an answer, not an edit.
- `No comments.` means the user closed it without remarks: ask whether they approve rather than assume.
- `No changes to review.` means the git diff was empty: check the arguments.

If the session restarted or the output was lost, `vjuu last` prints the most recent review again.

If `vjuu` is not on PATH, tell the user to install it: a build from https://github.com/kimvais/vjuu/releases, or `cargo install --locked --git https://github.com/kimvais/vjuu`.
