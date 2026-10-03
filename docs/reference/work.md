# Work: issues, pull requests and their links

archmap reads the work behind changes, issues and pull requests and the
links GitHub records between them, from a **snapshot**: a file one
explicit fetch writes. Commands read only the snapshot; `scan`, `summary`,
`query`, `impact`, `check` and the MCP server never reach the network. The
same HEAD and the same snapshot give the same answer.

## Fetching

`archmap fetch github` writes the snapshot (see
[commands.md](commands.md#fetch-github) for its flags). It asks GitHub's
GraphQL API, through the gh CLI, which keeps the token:

1. the schema, to leave out the link types whose fields the host lacks (a
   GitHub Enterprise Server without sub-issues or dependencies), recorded as
   not available;
2. the issues updated since the date, 50 a page, newest first;
3. the pull requests, 25 a page by update, until the first one updated
   before the date;
4. every nested list to its end: sub-issues, blocking links, closing
   references, timelines (20 to 100 a page). A pull request's commit list
   stops where GitHub stops listing (250); the snapshot keeps the count;
5. the issues and pull requests updated since the fetch began (less five
   minutes, for clocks that disagree), again: one updated while the walks
   ran moves ahead of their cursor, and its last read stays. This runs
   once: an item updated during it may keep its earlier state. The snapshot
   is as of the fetch's start; a link removed during the fetch may remain,
   and the counts are of the items finally held, so a bound reached can be
   passed by the few items updated during the fetch.

Owner and name go as variables, never into a query. Any error, a rate
limit included, fails the whole fetch, and the snapshot already there stays
as it was: the file is written beside the target and renamed. Measured,
50 issues cost 3 of GitHub's 5,000 points an hour and 25 pull requests 1,
so 5,000 of each cost about 500. The bound reached, the snapshot says from
when its range holds everything (`effective_since`). Links are as visible
to the account that fetched: what it cannot see GitHub answers `null` for,
and the snapshot counts it, never drops it silently (`not_visible`: link
ends per type, timeline events, items), and which account fetched is not
recorded. A closing GitHub names no closer for is no `closed_by` link,
whether the issue was closed by hand or the closer is hidden from the
account: a missing `closed_by` never means "closed by hand".

## The snapshot

`.archmap/github.json` under the root, beside `scan`'s default output; the
CLI's `query --snapshot FILE` reads another. It holds:

- the items updated since a date (`range`), newest first up to a bound:
  each issue and pull request by number, with its title, its state
  (`open`, `closed`, `merged`; for an issue the tracker's reason, such as
  `completed` or `not_planned`), its times, and for a pull request its
  commits by SHA and its merge commit;
- the links between them, each with the API field or timeline event it was
  observed in (`observed`), and the ones of other repositories by their
  repository and number;
- what was fetched: the source, host and repository, the fetch's time
  (UTC), the range (`updated_since`, what set that date, the bound, whether
  it was reached and then the effective date), the link types fetched,
  those the host does not offer, and what the fetching account could not
  see.

No bodies, comments, authors or other accounts' names are kept, and no
token: titles are the one free text. In a private repository titles may be
confidential, and they reach whatever reads `query`'s output, agents
included.

## Link types

Each link is stored once, in the direction below; an item's answer shows it
from the end the item is. A link is in a snapshot only when an item that
records it was fetched, so each type says which ends record it:

| type | from -> to | GitHub records it in | ends that record it |
|---|---|---|---|
| `sub_issue` | parent issue -> sub-issue | `Issue.subIssues`, `Issue.parent` | both |
| `blocked_by` | issue -> issue blocking it | `Issue.blockedBy`, `Issue.blocking` | both |
| `closes` | pull request -> issue | `closingIssuesReferences` / `closedByPullRequestsReferences`, not linked by hand | both |
| `linked` | pull request -> issue | the same, linked by hand | both |
| `closed_by` | issue -> commit or pull request | each `ClosedEvent`'s closer, with its time | the issue |
| `cross_referenced` | item -> item it references | `CrossReferencedEvent` | the referenced item |
| `referenced` | commit -> item | `ReferencedEvent` | the item |
| `duplicate_of` | issue -> issue it duplicates | `Issue.duplicateOf` | the duplicate |

GitHub parses closing keywords and references; archmap parses no text. A
link observed from both ends is one entry with both observations. "No link"
holds only within the snapshot's range and types, and from an end that
records the type: references from an item to items outside the range,
closings of issues outside it, and duplicates of an item outside it are not
seen, and an item's answer says so; when the range holds every item
(`--all`, the bound not reached), only those in other repositories.

## `query '#N'`

```text
#15 pull request: Round refunds half up
  merged 2026-01-04
  snapshot: github acme/shop, 2 issues and 1 pull request updated since 2025-10-03, fetched 2026-10-03 09:00 UTC, as visible to the account that fetched

Commits: 4, 2 in the local history
  f0e9bdf matched by sha (2026-01-01)
  f9396a2 matched by sha (2026-01-02)
  c1d2e3f in the repository, not in HEAD's history
  1111111 no local commit with the same sha
Merge commit: 9f541d5 matched by sha (2026-01-04)

Closes: #12 issue, open
Closed: #12 issue, open, 2026-01-04

Not traced (what this answer may miss):
  closings: seen on the closed issue's timeline, so issues outside the range that #15 closed are not seen
  cross-references: seen on the referenced item's timeline, so references from #15 to items outside the range are not seen
```

The target is `'#N'` or `owner/name#N` (the snapshot's repository only). The
first lines give the item's kind, title and state, and the snapshot it
comes from. A pull request's commits, and its merge commit apart, meet the
local history by SHA only:

- `matched by sha`, with the commit's date, when the history read holds it;
- `no local commit with the same sha`: the repository has no such object,
  such as the branch of a squash or rebase merge that was never fetched
  (`(the clone is shallow)` when it may lie beyond the clone's depth);
- `an ancestor of HEAD beyond the history read`: in HEAD's history, past the
  first 10,000 commits read;
- `in the repository, not in HEAD's history`: on another branch.

A rebase or squash merge rewrites SHAs, so a commit without a match never
means the pull request was not merged: the merge commit and `closed_by` say
that. At most 50 commits without a match are looked up per answer; lookups
fetch nothing, even in a partial clone.

The links follow, one line per type and end (`Closes`, `Closing references
from`, `Closed by`, `Closed`, `Sub-issues`, `Sub-issue of`, `Blocked by`,
`Blocks`, `Linked to`, `Linked from`, `Cross-references`, `Cross-referenced
by`, `Referenced by commits`, `Duplicate of`, `Duplicates`), 10 per line,
each end as `#12 issue, open`, `#40 (outside the fetched range)` or
`acme/web#4 (another repository)`. `Closed by` gives each closing's time
and says `(open now)` when the issue was reopened. `Not traced` names the
types its end cannot see, the links that name items outside the range, the
types the host does not offer, and the ends the fetching account could not
see.

JSON (`--format json`) gives the item, every link with its type, its end,
the other end and what observed it, every commit with `matched_by` and its
time or a `reason` (`no_local_commit_with_same_sha`, `beyond_history_read`,
`not_in_head`, `foreign`, `no_history`, `not_looked_up`), and the
snapshot's coverage.

`summary`'s Coverage names the snapshot when there is one (`work: github
acme/shop, 2 issues and 1 pull request updated since 2025-10-03, fetched
2026-10-03 09:00 UTC, as visible to the account that fetched`).

## `Work` in `impact` and `query`

`impact`, and `query` on a file or a component, end with the pull requests
and items the snapshot links to the commits that changed the target, just
before `Not traced`:

```text
Work: 3 of the 4 commits that changed src/pricing/price.ts are linked to 2 pull requests and 1 issue
  pull requests: 2
  #18 pull request, merged 2026-01-04: Rates in yaml
    by merge commit: c6677fc 2026-01-04
  #15 pull request, merged 2026-01-03: Round refunds half up
    by commit list: f0e9bdf 2026-01-02, f9396a2 2026-01-01; merged as 9f541d5, a merge commit, not among the commits counted
    closes #12 issue, closed 2026-01-03: Refunds round down; cross-referenced by #20 issue, open: Rate tables
  linked from commits: 1
  #12 issue, closed 2026-01-03: Refunds round down
    closed by commit 010f709 2026-01-02
  1 commit is linked to no pull request or item in the snapshot by SHA (after a squash or rebase merge, a pull request's own commits have other SHAs): 6bf7b15 2026-01-06
  work: github acme/shop, 3 issues and 2 pull requests updated since 2025-10-03, fetched 2026-10-03 09:00 UTC, as visible to the account that fetched; states as of the fetch
```

The commits are those `Changed in the same commits` counts (see
[history.md](history.md)): those that changed the target's files, merges,
a shallow clone's boundary and commits over 30 files under the root left
out, the last counted apart (`2 commits over 30 files not followed`). Every
step is a link the snapshot holds, never a match by content or time:

- a pull request whose commit list holds one of the commits (`by commit
  list`), or whose merge commit is one (`by merge commit`: a squash, or the
  last commit of a rebase); a true merge's merge commit is no commit
  counted, so it shows as the pull request's own fact (`merged as ...`);
- the items it links, each once with every link between them, by type
  and in the snapshot's direction: `closes`, `closed` (the item was closed
  by it), `linked to`, `cross-references` and `cross-referenced by`
  (`closes, closed: #12 issue, ...` when both);
- items a commit links straight (`linked from commits`): an issue the
  commit closed, one its message references;
- the commits linked to nothing. After a squash or rebase merge a pull
  request's own commits have other SHAs than HEAD's, so such a commit may
  belong to one; one older than the snapshot's range is counted apart,
  since the snapshot may not hold its pull request.

Pull requests come by their newest connecting commit, then the lower
number; items from commits by the newest commit that links them, then the
lower number, so the same HEAD and snapshot give the same order. The text shows 5 pull requests, 3 items from commits, 2 commits
of each kind and 3 items per pull request, 3 commits linked to nothing,
each list counted (`pull requests: 6, showing 5`); `--verbose` lifts the
caps. Item states are as of the fetch. Without a snapshot the section is
one line, `Work: none (no snapshot at .archmap/github.json)`, and `query`
reads no history for it. JSON gives `work` with `state` (`read`,
`no_snapshot`, `unreadable`, `no_history`), the counts, every pull request
with its connecting commits by kind, `merged_as` and its `items` (each
with its `other` end, when the snapshot holds it its `kind`, `state` and
`title`, and its `links` by `type` and the `end` the pull request is),
`from_commits` with each linking commit by `type`,
`unlinked`, `older`, `large` and the snapshot's coverage.
