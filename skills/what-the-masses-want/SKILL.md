---
name: what-the-masses-want
description: Use when asked what users of a tool (Claude Code, Codex, an editor, a CLI) most want or complain about, to "scour the interwebs" for most-requested features or UX/DX gripes, or to find which of them one of Kimmo's own products (vjuu or another) could address. Also /what-the-masses-want, and re-runs of an earlier demand scan.
disable-model-invocation: true
---

# What the masses want

Demand research for **a host tool** (default: Claude Code) judged against **a product** (default: the repo you are in).
The output is a ranked list of gaps the product could fill, with links, which Kimmo picks from.

## Steps

0. **Pin down** `<host>` (slug, e.g. `claude-code`, `codex`), the host's issue tracker `<host-repo>` (e.g. `anthropics/claude-code`, `openai/codex`), the product and its repo `<product-repo>`.
1. **Brief.** Read the product's README and skill/plugin docs, and grep the code for each host's integration (hooks, skills, MCP): the README overstates. Write a 5-10 line brief: what it is, what it does, and **what it does for THIS host specifically** (e.g. "Claude Code: 4 hooks; Codex: skill only, no hooks"). Without that line agents credit one host's integration to another.
2. **Previous runs.** Read every report in `~/.claude/research/<product>/` and list `gh issue list -R <product-repo> --state all --limit 200`. KNOWN = their gaps, cheap wins and out-of-lane items plus the issue titles. Only a report for the same host counts as "last run" for section 7.
3. **Fan out.** Three `general-purpose` agents in ONE message, prompts from `agent-briefs.md` in this directory (placeholders and shared tail expanded verbatim): tracker, community, neighbours. Wait for all three; do not research yourself meanwhile.
4. **Verify against the code.** For every "the product lacks X" claim, grep the product before calling it a gap. Agents guess what the product does; several of their "gaps" are usually already there.
5. **Write the report** to `~/.claude/research/<product>/<host>-<YYYY-MM-DD>.md` with exactly these sections:
   1. The short version (≤5 bullets: where demand is, the main competitor, the product's position)
   2. Already solved (table: pain, evidence links, how the product solves it); for positioning
   3. Gaps, top 4 in a ```` ```choice ```` block, recommended first; then ≤5 cheap wins
   4. Deliberately out of lane
   5. Risks
   6. Checked in the code (what step 4 confirmed or ruled out)
   7. New since last run (only if step 2 found one for this host)
6. **Open it in vjuu** (`vjuu <report>` in the background) and act on the picks and comments.

## Follow-ups Kimmo usually asks for

| Ask | Do |
|---|---|
| "make issues of these" | One issue per gap, skipping ones step 2 found. Short body: the want, demand links, open question. For UI features, mention the keyboard shortcut if the repo's CLAUDE.md requires one. |
| "draft comments promoting it" | At most 3, on OPEN, unlocked, high-vote issues where the product really answers the ask. Say plainly what it does not do. Disclose authorship. Use the sound-like-me skill. Drafts go to vjuu first. |
| "post them" | Write the `gh` calls to a `.ps1` in the scratchpad and run it. Auto mode blocks GitHub writes: if it refuses, put `! & "<full path to the .ps1>"` on the clipboard and in the reply. |

## Common mistakes

- Agents briefed without the product brief rate everything PARTIAL.
- Reddit refuses direct fetches; the community agent must use search snippets, mirrors and HN Algolia instead of retrying.
- Ranking by total votes surfaces billing, model quality and terminal bugs. Rank gaps by fit × demand, and say once that the top-voted items are out of reach.
- Commenting on closed or locked issues, or on more than 3, reads as spam.
