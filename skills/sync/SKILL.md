---
name: sync
description: Use when asked to sync, rebase or update the current branch onto the default branch and push it, and as the push step of ship. Rebases onto origin's default branch and force-pushes with lease, without asking; stops only for a conflict, a stale lease or the default branch.
---

<!-- Shipped with vjuu; `vjuu setup` overwrites this file. To keep your own, delete this line. -->

# sync

Rebase the current branch onto origin's default branch and push it. Saying "sync" is the permission to push, so do
not ask before pushing. Silent on success: report one line, then stop.

## 1. Check where you are

Stop and report the reason if any of these hold; never work around them.

- Not a git repository: `git rev-parse --is-inside-work-tree` fails.
- A rebase is already in progress: `git rev-parse --git-path rebase-merge` or `rebase-apply` exists.
- Detached HEAD: `git branch --show-current` prints nothing.
- On the default branch. Find it with `git symbolic-ref --short refs/remotes/origin/HEAD` (prints `origin/main`).
  If that fails, run `git remote set-head origin --auto` once and ask again. Pushing the default branch is refused,
  always.

## 2. Commit what is left

Untracked files never block. For tracked changes (`git status --porcelain --untracked-files=no`):

- Files you edited in this session: stage them with `git add -u` and commit with a real message, a brief WHAT plus
  any WHY that isn't obvious.
- Anyone else's changes, or changes you cannot place: `git commit -m "wip"`.

Never add `.env` files, scratch folders or virtualenvs.

## 3. Rebase

```
git fetch origin
git rebase origin/<default>
```

On a conflict, leave the rebase in progress. List the conflicted files (`git diff --name-only --diff-filter=U`).
Resolve them only when the resolution is unambiguous, then `git -c core.editor=true rebase --continue`. Otherwise tell
the user which files need them and stop. Never `git rebase --abort` on your own.

## 4. Push

```
git push --force-with-lease -u origin HEAD
```

If the push is rejected because the lease is stale, someone else pushed to this branch: show the output and stop.
Never fall back to plain `--force`.

## 5. Report

One line: `synced <branch> onto origin/<default>, <n> commits ahead, pushed`. Nothing else: no diff summary, no
follow-up question.
