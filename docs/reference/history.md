# Git history and co-change

`impact` reads the root's committed git history to say which files changed
in the same commits as the target. The history gives observed facts; what
changed together is a view computed from them when `impact` asks.

## What is read

The history is HEAD and its ancestors, read with the git CLI from the root
(a root below the repository's top reads the commits that changed files
under it, with paths relative to the root). Only committed history: an
uncommitted change is never read, and a file HEAD does not hold has no
commit yet.

The facts are, for each commit: its SHA, its own parents, its committer
time, and the files it changed under the root, each added, modified,
deleted, changed in type, or renamed with its old path, new path and git's
similarity score. HEAD's files under the root are read too, to tell a file
that is not committed yet and to mark submodules. No author name, email or
commit message is read.

- **Bound:** the first 10,000 commits that change the root, in git's
  default order from HEAD (newest committer date first). A longer history
  stops there, said in the answer.
- **Renames:** git's detection, `-M50%` with at most 1,000 candidate
  pairs per commit; past that, git finds exact renames only, said in the
  answer. A merge commit is read for its parents only, never compared with
  them.
- **Shallow clones:** the clone's boundary commits show their whole tree as
  added, so their changes are not read; the history ends at the clone's
  depth.
- **Partial clones:** a clone without some file contents is read with
  rename detection off, since detecting renames would need contents git
  cannot read without fetching; a clone without trees is not readable.
- **Paths** that are not UTF-8 are skipped and counted.

Reading never reaches the network and runs no program the repository
configures: git runs with lazy fetching, filesystem monitors, external
diffs, text conversion, pagers and prompts off, replace refs ignored, its
output settings pinned (so a user's `diff.renames` or `log.follow` changes
nothing), and environment variables that point git at another repository
dropped. The user's own git configuration is kept, `safe.directory`
included. The answer for the same HEAD is the same.

Without a history the answer says why: `not a git repository`, `git not
found`, `dubious ownership: see git's safe.directory` (git refuses a
repository another user owns), `no commits`, or `unreadable` with git's
message.

## Files changed in the same commits

```text
Changed in the same commits (history, not imports): 6 files, showing 5, in the 4 commits that changed src/pricing/price.ts; per file: commits shared, of the target's and of its own
  config/rates.yaml  2 of the target's 4, 2 of its own 2: f9396a2 2026-01-02, f0e9bdf 2026-01-01
  docs/pricing.md  2 of the target's 4, 2 of its own 2: c6677fc 2026-01-04, f0e9bdf 2026-01-01 (then docs/prices.md)
  src/pricing/round.ts  2 of the target's 4, 2 of its own 2: f9396a2 2026-01-02, f0e9bdf 2026-01-01
  tests/price.test.ts  2 of the target's 4, 2 of its own 2: 9f541d5 2026-01-03, f0e9bdf 2026-01-01
  package.json  1 of the target's 4, 1 of its own 1: f0e9bdf 2026-01-01
  history: HEAD 6bf7b15, full clone; 6 commits read, 5 counted; left out 1 over 30 files; renames -M50%; files by shared commits over the mean of both counts
```

Changing together is a fact of the history, not proof of a dependency: a
feature, a format run or a rename can tie files. It shows coupling that no
import shows: configuration and the code that reads it, tests and their
data, files in different languages kept in step.

- **Counted commits** are those that are no merge, no shallow boundary,
  and change at most 30 files under the root. A commit over 30 (a format
  run, a vendoring, a mass rename) is left out, counted on the history line.
  Counting under the root means a subdirectory root does not depend on
  unrelated directories, and a repository-wide mechanical commit that
  touches a few files under it stays in.
- **Merges** are read for their parents only: the commits they merge count
  for their own changes, and what a merge changes itself (a conflict
  resolution, an edit made while merging) is not read. When the history
  holds merges, the counts say so (`in the 4 non-merge commits that
  changed ...`). Reading those changes would mean merging each merge again
  (`git log --remerge-diff`, several seconds on a merge-heavy history), and
  git's cheaper combined diff also lists the files both sides changed and
  git merged cleanly, which would count them twice.
- **Renames** are followed from every commit read, counted or not, in the
  order of the commits' ancestry: an older change to a path counts for the
  file it was renamed to, and adding a path again starts a new file. A move
  without edits is no change. `(then <path>)` gives a file's earlier path on
  the commit where it applied.
- **The target** is a file's path, a symbol's file (`its file
  src/money.ts`), or a component's files at HEAD (`pricing's 2 files`),
  configuration and data beside its code included. A file counts once per
  commit, however many of the target's files that commit changed, and the
  target's own files are never listed.
- **Each file** shows the commits it shares with the target out of the
  target's (`2 of the target's 4`) and out of its own (`2 of its own 2`),
  and its newest shared commits by short SHA and committer date (UTC).
  Files come by their shared commits over the mean of the two counts
  (code-maat's degree) highest first, so a file that changes in most
  commits (a lockfile, a changelog, docs every change touches) sinks below
  one that changes mostly with the target; then most shared commits, then
  path; no minimum. The history line names the order. A file HEAD no
  longer holds is left out of the text.
- **None:** `none: HEAD does not hold <file>, so no commit changed it yet`,
  `none: no commit changed <file>`, `none: no commit read changed <file>,
  and older commits were not read` (a bounded read or a shallow clone),
  that it changed only in commits left out, or, for commits that changed
  nothing but the target's files (a component that holds them all), `none,
  in the 7 commits that changed shop's 42 files: they changed nothing
  else`.

The text shows 5 files and 2 commits each, counted in the heading;
`verbose` lists every entry. The `history:` line always ends the section:
HEAD, whether the clone is full, shallow or partial, the root when it is
below the repository's top, the commits read and counted, those left out
and why, and how renames were detected. `Not traced` gains `history:`
when the history read may hide files changed with the target: a shallow
clone, older commits not read, renames not detected in a partial clone, or
inexact renames skipped.

JSON (`impact --format json`) gives the section as `co_change`: `history`
(its `state` and HEAD, the root's `prefix`, git's version, the `bound` and
whether it was reached, how `renames` were detected, skipped paths),
`target_paths`, `settings` (`max_files`, `follow_renames`,
`pure_moves_count`), `counts` (commits read, counted, large, merges,
boundaries), `target_commits`, `target_large` (the commits that changed
the target and were left out for their size, when any), every file with every shared commit's full
SHA and time (`files`, with `own`, `in_head`, `submodule` and `earlier`),
and `none` with why when no counted commit changed the target.

## Known limits

- A path git stores in another Unicode normalization than the file system
  shows (macOS) does not match the scan's path.
- What a merge changes itself is not read (see Merges above).
- With merges, a change on a branch merged after a rename may still count
  under the old path.
- Where the ancestry between two commits runs through commits that were not
  read (a subdirectory root), git's print order stands in for it.
- With clocks that disagree, the bound can keep a parent while leaving out
  a child.
- git's rename detection can differ between git versions; JSON gives the
  version.
