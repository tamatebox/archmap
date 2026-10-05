---
name: backlog
description: Pick the next archmap task, and file, update or close GitHub issues on tamatebox/archmap. Use when asked what to do next or what is left, when deferring a finding from review or dogfooding, when starting work an issue may already cover, and when suggesting the commit that finishes an issue.
---

# Backlog

Planned work lives in GitHub issues on tamatebox/archmap. The repository is public: whatever an issue, a comment or a commit says is published at once. Every create, edit, comment, tick or close is shown to the user first and done only after approval.

## Pick the next task

1. A plan issue is optional. When there is one (pinned, its title starts with `Plan:`; `gh issue list --state open --search '"Plan:" in:title'`), it is the only place that records the order, and the first unchecked item is next, once its boxes are brought up to date (see "Keep a plan's boxes ticked"); items without an issue number (a review round, a milestone) are steps the user runs: ask. Lines of work that run in parallel each have a plan of their own, and a session follows one of them. Without one, propose candidates from the open issues and let the user choose.
2. Check whether the item is already in progress: uncommitted changes in the working tree that belong to it, or a plan for it under `docs/superpowers/plans/`, mean another session may be on it. Ask the user before starting.
3. Issues that share a milestone are decided together at that step, not one by one.
4. An issue labeled `needs-decision` is not started, and not skipped when later items build on it: bring its open questions and options to the user. Once decided, comment the decision on the issue in a few lines and remove the label; the decision itself lands in CLAUDE.md, README or `docs/reference/` with the change that implements it.
5. Open issues that are neither in a plan nor labeled `later` are taken only when the user asks.
6. Read the issue, then the parts of `docs/reference/`, `README.md` and `plugins/archmap/skills/archmap/SKILL.md` it touches. Local design notes under `docs/superpowers/` (never committed) may hold more; whatever is copied from them into an issue or a commit follows the private-repository rule in CLAUDE.md.

## File or edit an issue

- Work started right away at the user's request needs no issue; issues record work that waits.
- Search first for an existing one: `gh issue list --state all --search "<words>"`.
- Body: what is wrong or missing, why it matters, and when it is done, in a few lines. Leave design detail to the change.
- Reproduce with fixtures under `fixtures/` or a few inline files. A finding from a private repository is rebuilt as such a repro with invented names; never write its name, paths, packages, counts or layout, not even in general terms, nor the sessions that found it.
- Labels: `bug`, `enhancement` or `documentation` for work; `needs-decision` alone for a design question; `later` for anything recorded but not planned. Remove `later` when the issue is planned.
- Add a milestone only when the user puts the issue into that review.
- A body names only technical dependencies (`Builds on #1`), never the order, which only a plan issue records.
- Pass bodies with `--body-file - <<'EOF'`, never `--body "..."`: the shell would run the backticks.
- Not issues: the roadmap phases (README), principles and settled rulings (CLAUDE.md, commit messages), and agent trials.

## Close an issue

- The change that closes it also updates `docs/reference/` and `README.md` wherever behavior changed, and the plugin skill and the MCP texts only as CLAUDE.md says. None of them ever cites issue numbers.
- Suggest `Closes #<n>` in the body of the commit message, on the last commit when the work spans several, so the issue closes on push and links to the commit.
- When the user says the push is done, tick the items it closed in the plan issue, as the next section says.
- Update any local design note that links the issue.

## Keep a plan's boxes ticked

A plan issue lists its steps in order as task-list items, `- [ ] #12 title` for an issue and `- [ ] A review round over ...` for a step the user runs. Its boxes are the record of what is done, and they go stale on their own: agents never push, so the push that closes an issue happens outside any agent's turn. Bring them up to date at each of these moments, showing the user which boxes change before ticking:

- When the user says a push is done: the items whose issues it closed.
- When the user says a step they run is done (a review round, a decision): that step.
- Whenever a plan is read, picking the next task included: first every item whose issue is already closed, and a milestone step once all of its issues are closed. The unticked items whose issues are closed: `gh issue view <plan> --json body -q .body | sed -n 's/^- \[ \] #\([0-9]*\).*/\1/p' | xargs -I{} gh issue view {} --json number,state -q 'select(.state == "CLOSED") | .number'`.

To tick, read the body, change `- [ ]` to `- [x]` on those lines only, write it back with `gh issue edit <plan> --body-file - <<'EOF'`, and read it again to see the boxes changed. A ticked item whose issue is open is reported to the user, not unticked. When the last item is ticked, close the plan issue and propose what comes next.
