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
   not available, and the account the fetch runs as (`viewer`);
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
ends per type, timeline events, items). The snapshot records the account
that fetched (its login and numeric id, never a token), so that an update
by another account fetches whole; answers never show it. A closing GitHub names no closer for is no `closed_by` link,
whether the issue was closed by hand or the closer is hidden from the
account: a missing `closed_by` never means "closed by hand".

### Updates

`archmap fetch github --update` reads only what changed since the
snapshot's fetch: the issues and pull requests updated since it began, less
five minutes, through the same steps 1 to 5, each item whole with every
nested list. Each item read replaces the one held, with every link it
records; the links the other items record stay. Items are kept by number,
the result is sorted as a whole fetch's is, and the counts of what the
account could not see are summed again from the items, so updating twice
with the same answers gives the same snapshot.

An update reads nothing new of an item whose `updatedAt` did not change,
and GitHub leaves it unchanged when another item cross-references it or a
commit references it: those two types are recorded on the referenced
item's timeline. An item deleted, transferred or hidden from the account
since is not seen either. So the snapshot keeps the time of its last whole
fetch apart (`whole_fetched_at`): answers name both times (`fetched whole
2026-10-03 09:00 UTC, updated 2026-10-04 09:30 UTC`), and `query '#N'`'s
`Not traced` says that cross-references and commit references to items
that did not change, and items deleted or hidden, are as of the whole fetch;
a Work section's last line says the same (`states as of the update, and
links only an item's own timeline records as of the whole fetch`), and
JSON's coverage gives `whole_fetched_at`. `closed_by` and `duplicate_of`
change the item's `updatedAt`. A link of a type both ends record (`closes`,
`linked`, sub-issues, blocking) goes when either end is read again, and
comes back when that end still records it: a link one end no longer
records is missed until the other end is read, never kept from that end's
older read.

An update fetches whole instead, and says why, when there is no snapshot or
it cannot be read, when it records no whole fetch or no account (written
before updates existed), when its host, repository, link types, title
setting or bound differ, when the account answering differs (by numeric id,
else by login), when more items changed than the bound reads, and when its
last whole fetch began longer ago than `--full-if-older-than` (24 hours by
default). The range's start and bound stay those of the whole fetch, and
a whole fetch in place of an update reads the snapshot's own range. A
failed update leaves the snapshot as it was. A plain `archmap fetch github`
always fetches whole.

### Automatic refreshes

`archmap fetch github --if-older-than 30m` is for a hook or a scheduler
that runs it often, from several sessions at once. It updates as
`--update` does, whole fetches and `--full-if-older-than` included, but
only when the snapshot's last fetch (`fetched_at`, whole or update) is
older than the limit and no wait after a failure runs; otherwise it asks
GitHub nothing and says why (`fresh: .archmap/github.json was fetched
2026-10-04 09:30 UTC, within 30m: nothing fetched`). A snapshot is fresh
only as what the fetch asks for: one of another source, host or
repository, or fetched with the other title setting or another bound, is
updated however recent, which fetches it whole and says why, as
`--update` does. One whose fetch time cannot be read (edited by hand) is
never fresh either, and is fetched whole. With no snapshot it fetches one
whole. It never overwrites a snapshot it cannot read, whether
of a newer format, corrupt or unreadable: it says which and that a fetch
asked for replaces it.

**One fetch at a time.** Every fetch of a snapshot, plain, `--update` or
automatic, first takes an OS advisory lock on the file beside it
(`.archmap/github.json.lock`), held until the process ends however it
ends; the file stays. What a fetch decides (fresh, waiting, update or
whole) it decides under the lock, from the snapshot and the status as they
are then, so a refresh that waited for nothing never repeats one that just
ran. When another fetch holds the lock, nothing is asked: an automatic
refresh says `busy: another archmap fetch is updating
.archmap/github.json: nothing fetched`, while a fetch asked for says the
same without `busy:` and fails. A lock that cannot be taken (its directory
cannot be created) is an error, never taken for another fetch.

**Status.** Each attempt is recorded beside the snapshot, after it, in
`.archmap/github.status.json` (`<name>.status.json` beside `-o <name>.json`),
which only a fetch writes: the source, host and repository, the account
the attempt ran as when it learned it, when the attempt began, and after a
failure its kind (`rate_limit`, `secondary_rate_limit`, `auth`, `network`,
`other`), what it left that is safe to keep (`message`), `next_retry_at`
and `consecutive_failures`. The message holds facts, never gh's or
GitHub's words, which may name local paths, accounts or more: for a
request the HTTP status and the codes of the errors GraphQL names
(`GitHub answered HTTP 200 with errors RATE_LIMITED`, `no answer came from
GitHub`, `gh could not be run`); for a login gh cannot confirm, the host;
for a snapshot that could not be written, the file system's own message,
which names no path. The fetch prints gh's words, and reads the kind from
them, with tokens, the credentials of URLs and absolute paths taken out
(`[redacted]`, `[path]`; a path runs to a quote, a bracket, `,`, `;`, `:`
or ` (`, spaces included). Answers take the same out of a status written
another way. A success clears the failure; a failure leaves the
snapshot as it was. A status of another host or repository, as after `-o`
names one file for another `--repo`, is no wait of this one, and the next
attempt replaces it. A failure that began no later than the snapshot's
fetch is past, as after a fetch wrote the snapshot and stopped before its
status: no refresh waits on it or counts it. A failure cannot be placed
against a snapshot whose fetch time cannot be read, so with such a
snapshot no failure is waited on, counted or shown: every refresh tries a
whole fetch until one succeeds.

**Waiting after a failure.** Automatic refreshes wait 5 minutes after a
failure, doubled with each failure in a row up to 6 hours, and at least a
minute. For a rate limit they follow GitHub's guidance: the failed
response's `retry-after` first; for a limit spent (`x-ratelimit-remaining:
0`) its `x-ratelimit-reset`; for the primary limit without those headers,
the reset `gh api rate_limit` gives, which counts against no primary
limit. A time these give stands as it is, sooner than the backoff when it
says so; one already past is no time, and the next source is read. gh
prints the headers of a failed response too, an HTTP error's and a
GraphQL error's alike. That endpoint is never asked after a secondary limit: it may count
toward one and cannot tell when one ends, so a secondary limit without
headers waits as other failures do, exponentially longer each time. A
fetch asked for (`archmap fetch github`, with or without `--update`)
ignores the wait and is recorded the same way.

**Logins.** A host other than github.com is asked `gh auth status` first.
When gh reports no login for it, the attempt fails as an authentication
failure; when gh fails otherwise, the failure is of no known kind, or the
network's after a timeout: gh reports a host it cannot reach as an invalid
token, so its words do not tell the two apart.

**Accounts.** Failures in a row count per account: a failure as another
account than the last one's counts from one. The account is the one the
host names in the first answer (`viewer`); a failure before that answer is
recorded with no account, never as the previous account's, and keeps the
count. Nothing is asked while the snapshot is fresh or a wait runs, so a
change of gh's account is not seen until the next attempt (the `waiting:`
line says so); a fetch asked for sees it at once.

**What answers say.** A failed attempt that began after the snapshot's
fetch, for the snapshot's host and repository, shows beside it: in
`summary`'s Coverage (`work refresh: the last attempt, 2026-10-04 10:00
UTC, failed (rate limit): GitHub answered HTTP 200 with errors
RATE_LIMITED; no automatic attempt before 2026-10-04 11:00 UTC`, after `work: none (no
snapshot at .archmap/github.json)` when no fetch has succeeded yet), as a
`refresh:` line in `query '#N'` and in the Work section, and as `refresh`
in their JSON (`last_attempt_at`, `error` with `kind` and `message`,
`next_retry_at`, `consecutive_failures`). Since the status is written after
the snapshot, an attempt stopped between the two leaves a failure older
than the snapshot, which shows nowhere and delays no refresh. Answers give times, never an age,
and never the account. They describe the snapshot and do not change what
it holds; the MCP server reads them and never fetches.

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
  (UTC) and, apart, the last whole fetch's (`whole_fetched_at`), the
  account that fetched (`account`: `login`, `id`), the range (`updated_since`, what set that date, the bound, whether
  it was reached and then the effective date), the link types fetched,
  those the host does not offer, and what the fetching account could not
  see.

No bodies, comments or authors are kept, and no token: titles are the one
free text, and another repository's `owner/name` (whose owner may be a
person's account) is kept only where a link names it. In a private repository titles may be
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

Code: 3 files in 2 components
  by commit list: config/rates.yaml, src/pricing/price.ts
  by merge commit: src/pricing/round.ts
  components: pricing 2 files, shop 1 file

Not traced (what this answer may miss):
  closings: seen on the closed issue's timeline, so issues outside the range that #15 closed are not seen
  cross-references: seen on the referenced item's timeline, so references from #15 to items outside the range are not seen
  unmatched: 3 of the snapshot's 40 merged pull requests match no commit of the history read, so their code is not shown
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
see. For an item the snapshot does not hold, its own end was not fetched:
`Not traced` names the types only that end records (`own end: the snapshot
does not hold #40, so its closed_by, cross_referenced, referenced,
duplicate_of links ... are not seen`), its other links are seen only where
the other end is in the snapshot, and `Code` says that no pull request in
the snapshot closes it or is linked to it, never that no link exists.

`Code` follows: the files the item's work changed, as far as the history
read holds its commits. A pull request lists the files of the commits its
commit list names and of its merge commit, apart (`by commit list:`, `by
merge commit:`); a true merge's own changes are not read, so its merge
commit shows as `merged as 9f541d5, a merge commit, whose own changes are
not read`. An issue lists them through each pull request it is linked to by
`closes`, `linked`, `closed_by` or `cross_referenced`, and each commit by
`closed_by` or `referenced`, other link types and items outside the range
aside. Those that close it or are linked to it come first, then those that
only mention it, each newest first, and the heading counts apart the files
only a mention reaches (`; 4 of them only through cross_referenced or
referenced links`):

```text
Code: 3 files in 2 components, through 1 pull request and 1 commit
  through #15 pull request (closes, closed_by), 2 of its 3 commits in the local history, and its merge commit:
    config/rates.yaml, src/pricing/price.ts, src/pricing/round.ts
  through commit 010f709 2026-01-02 (closed_by):
    src/pricing/price.ts
  components: pricing 2 files, shop 1 file
```

Every file a commit changed counts, large commits included, once by the
path HEAD holds it at: a file a later commit moved shows the path written
and `now <path>`, and one HEAD no longer holds `not in HEAD`. A line shows 10
files and `Code` 10 pull requests and commits; `components:` counts the
files per component at the roll-up depth, and stays whole. `Not traced`
counts the snapshot's merged pull requests that match no commit of the
history read, by commit list or merge commit: their code is not shown, as
after a squash or rebase merge of a branch never fetched, or beyond the
history read.

JSON (`--format json`) gives the item, every link with its type, its end,
the other end and what observed it, every commit with `matched_by` and its
time or a `reason` (`no_local_commit_with_same_sha`, `beyond_history_read`,
`not_in_head`, `foreign`, `no_history`, `not_looked_up`), the snapshot's
coverage (`whole_fetched_at` once an update came after the whole fetch),
`not_seen` (each link type the item's end cannot see, with the
end the item would be: for an item the snapshot does not hold, the types
only its own end records), `code` (the files, each once, `components`, `only_mentions`,
and `through`: a pull request with its `links`, `commits`,
`by_commit_list`, `by_merge_commit` and `merged_as`, a commit with its
`links`, each commit the history read holds with its `time` and `files`,
each with `path`, `now` and `in_head`) and `unmatched` (the snapshot's
`merged` pull requests and how many match no commit).

`summary`'s Coverage names the snapshot when there is one (`work: github
acme/shop, 2 issues and 1 pull request updated since 2025-10-03, fetched
2026-10-03 09:00 UTC, as visible to the account that fetched`), and how
many of its merged pull requests match no commit of the history read, when
any (`; 3 of its 40 merged pull requests match no commit of the history
read`); the history is read for it only when a snapshot exists.

## `Work` in `impact` and `query`

`impact`, and `query` on a file or a component, end with the pull requests
and items the snapshot links to the commits that changed the target, just
before `Not traced`:

```text
Work: 3 of the 4 commits that changed src/pricing/price.ts are linked to 2 pull requests and 2 issues
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
one line, `Work: none (no snapshot at .archmap/github.json)`, followed by
a `refresh:` line when a fetch failed (see [Automatic
refreshes](#automatic-refreshes)), and `query` reads no history for it. JSON gives `work` with `state` (`read`,
`no_snapshot`, `unreadable`, `no_history`), the counts, every pull request
with its connecting commits by kind, `merged_as` and its `items` (each
with its `other` end, when the snapshot holds it its `kind`, `state` and
`title`, and its `links` by `type` and the `end` the pull request is),
`from_commits` with each linking commit by `type`,
`unlinked`, `older`, `large` and the snapshot's coverage
(`whole_fetched_at` once updated).
