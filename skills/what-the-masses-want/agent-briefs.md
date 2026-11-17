# Agent briefs

Fill `<HOST>` (name, e.g. Claude Code), `<REPO>` (its tracker, e.g. `anthropics/claude-code`), `<TODAY>`, `<BRIEF>` (step 1) and `<KNOWN>` (step 2). Replace `<shared tail>` with the block below, verbatim.

## Shared tail

```
Context for judging fit, the product:
<BRIEF>

Already known (do not report these as new; say "known" if you meet them):
<KNOWN>

For each item give: title, URL, votes/points and comment count, state (open, closed, shipped), one-line gist,
and fit: STRONG (squarely the product's lane), PARTIAL, or NONE, with one sentence why. Flag items the product
ALREADY solves. Concise markdown, cite every claim with a URL, no padding. Web only, change no files.
```

## 1. Tracker

```
Research task. Today is <TODAY>. Find the most-requested features and most common UX/DX complaints about <HOST>
from its issue tracker. Use `gh api 'search/issues?q=repo:<REPO>+is:issue&sort=reactions&order=desc&per_page=100'`
(works better than WebFetch of the HTML), then keyword searches per theme (review, diff, plan, permission,
notification, multi-session, rendering, copy/paste, input, images, hooks). Include closed issues and say whether
closed means shipped, duplicate or not planned. Up to 40 items, grouped by theme, most relevant to the product
first; stop when relevance runs out rather than pad. First, say what integration surface <HOST> offers a
third-party tool (hooks or events for plan, approval, question, end of turn; notify; MCP), with links: it decides
fit more than any single request.
<shared tail>
```

## 2. Community

```
Research task. Today is <TODAY>. Find recurring gripes and wishes about <HOST> in community discussion, NOT its
issue tracker: Reddit, Hacker News, blogs ("my workflow", "what I wish it had", comparisons), X/Bluesky, dev.to.
Prefer the last 12 months. Reddit refuses direct fetches: use WebSearch snippets and mirrors, and the HN Algolia
API (https://hn.algolia.com/api/v1/search?query=...&tags=story). Rank gripes by how often and how loudly they
recur. Also list tools people built themselves for these pains (they show demand).
<shared tail>
```

## 3. Neighbours

```
Research task. Today is <TODAY>. Map third-party tools built around <HOST> (and its closest rivals) that overlap
the product's lane. For each: URL, GitHub stars, what it does, overlap, what it has that the product lacks. Then
read the most-voted issues in the closest ones' trackers and list requests in the product's lane. End with gaps
nobody fills well that the product is positioned for, and things it lacks that matter.
<shared tail>
```
