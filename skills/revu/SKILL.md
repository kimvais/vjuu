---
name: revu
description: Use whenever you would ask the user to open, read, look over or review a file, document, plan, spec, diff, patch, commit or branch, and when the user says "let me review", "show me", "/revu" or "review it in revu". Opens the content in the revu window, where the user comments on lines; the comments come back as the command's output.
---

# revu

`revu` shows a markdown file or a diff in a window where the user leaves line comments. It blocks
until they click Done, then prints the review as markdown on stdout. Use it instead of saying "open X
and review it" or pasting long content into chat for review.

## Run it

Always run it with the Bash tool's `run_in_background: true`. A review can take longer than any
foreground timeout, and you are notified when it exits. Do not poll; wait for the notification.

| What to review | Command |
|---|---|
| A markdown file | `revu path/to/file.md` |
| Uncommitted changes | `revu --git` (add `--staged`, `HEAD`, `main...HEAD`, paths: any `git diff` args) |
| A commit | `git show <sha> \| revu -` |
| A diff or patch file | `revu change.diff` |
| Text you generated | write it to a `.md` file first, then `revu` that file |

Run `--git` from inside the repository. Tell the user in one line that the review window is open.

## Read the result

Output looks like:

```
# Review: docs/plan.md (2 comments)

## docs/plan.md:12-14
> quoted lines the comment is on

the user's comment

## Overall
general comment
```

- Each `## file:line` section is one comment; `(old)` means a deleted line, numbered in the old file.
- Treat comments as the user's instructions for that spot: act on each one, then say per comment what
  you did. A question in a comment gets an answer, not an edit.
- `No comments.` means the user closed it without remarks: ask whether they approve rather than assume.
- `No changes to review.` means the git diff was empty: check the arguments.

If the session restarted or the output was lost, `revu last` prints the most recent review again.

If `revu` is not on PATH, tell the user to install it: `cargo install --path C:\Users\k\p\revu`.
