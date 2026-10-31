---
name: ship
description: Use when the user says "ship", runs /ship, approves finished work in vjuu, or when committed work in a worktree is finished. Commits what is left, rebases and pushes the branch, opens a ready pull request or merge request if there is none, and watches CI. Never merges.
---

<!-- Shipped with vjuu; `vjuu setup` overwrites this file. To keep your own, delete this line. -->

# ship

Take the current branch to a pull request ready for review (GitHub) or merge request (GitLab). Saying "ship", or an
`approved` verdict in vjuu, is the permission for every step below, so do not ask again. In a worktree under the
worktree folder (the `worktrees` setting, `vjuu config list` shows it), finished committed work ships without being
asked.

## 1. Sync

Follow the `sync` skill: commit what is left, rebase onto origin's default branch, push with `--force-with-lease`. If
it stops, stop here too, with its reason.

## 2. Find the forge

`git remote get-url origin`:

| origin contains | Forge | CLI |
|---|---|---|
| `github` | GitHub | `gh` |
| `gitlab` | GitLab | `glab` |
| anything else | unknown | none: report the push and stop |

## 3. Open a pull request or merge request, only if none is open

- GitHub: `gh pr list --head <branch> --state open --json number,url`
- GitLab: `glab mr list --source-branch <branch>`

**One is open:** leave it alone. Never edit its title or description, never comment, never change its draft state.
The push already updated it.

**None:** open one as ready for review, not draft, against the default branch.

- **Title:** the issue's title when the branch is named `<number>-...` (`gh issue view <n>` or `glab issue view <n>`),
  otherwise a short imperative summary.
- **Description:** the approach, the result and why it matters, what changes outside the code under review, and what is
  deliberately left out. Short sections, no implementation detail. End with `Closes #<n>` for an issue branch. Write it
  to a file in your scratch directory and pass the file.

```
gh pr create --base <default> --head <branch> --title "<title>" --body-file <file>
glab mr create --source-branch <branch> --target-branch <default> --title "<title>" --description "$(cat <file>)" --remove-source-branch --yes
```

## 4. Watch CI

Run the watcher in the background and wait for it to finish; do not poll.

- GitHub: `gh pr checks <number> --watch`
- GitLab: `glab ci status --live --branch <branch>`

Then:

- **Passed:** print the PR or MR link and `CI passed`. Stop.
- **Failed, caused by this branch** (a test, lint or build failure in code it touches): fix it, run the same check
  locally, commit, and ship again. After two failed rounds on the same job, stop and hand it to the user with what you
  tried.
- **Failed, not caused by this branch** (it fails on the default branch too, a flaky runner, a missing secret): do not
  touch the code. Report the job, the cause in one line and whether a retry would help.
- **No CI:** print the link. Stop.

## Never

Merge, approve, resolve threads, post comments, change an existing PR's draft state, or push to the default branch or
to someone else's branch.
