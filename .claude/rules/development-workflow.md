# Development workflow

A change goes: pin the requirement → implement in the main conversation → run the checks → one
independent review → fix only what it blocks → suggest the commit.

## What gets a review

- Review: a change to what archmap reads, records or answers, and to what agents read before
  using it: `crates/`, `plugins/`, `archmap.toml`, a dependency in `Cargo.toml`
- No review: a change that alters no behavior (docs, README, CLAUDE.md or `.claude/` alone;
  comments; formatting; a rename the compiler checks; tests or fixtures alone), questions,
  investigation and issue work
- Ryota can ask for a review, or waive one, for any change

## The loop

1. **Pin the requirement**: state what "done" means. Requirements that conflict or leave the
   outcome open → ask before writing
2. **Implement** in the main conversation, not in subagents
3. **Check**: the commands in CLAUDE.md, "Commands to run after every change"
4. **Review** once the change is final, with `code-reviewer`. Pass no `model` or `effort`: its
   frontmatter sets them, and `CLAUDE_CODE_EFFORT_LEVEL` or `CLAUDE_CODE_SUBAGENT_MODEL_FORCE`
   would override it. Hand it the requirement and acceptance criteria verbatim; the change as a
   file (`git diff HEAD -- <paths>` written to the scratchpad) plus every untracked file, which
   `git diff` omits; and what step 3 ran and what passed or failed. Until the verdict arrives,
   nothing in the reviewed files changes and nothing is reported done. If `code-reviewer` is
   missing or fails to start, say the review was not done; never swap in another model or skip it
   silently
5. **Act on the verdict**
   - `PASS` → report, listing its `ADVISORY` items unfixed for Ryota to decide, and suggest the
     commit; the reviewer never sees its message, so check it against the private-repository rule
     in CLAUDE.md yourself
   - `BLOCK` → confirm each finding in the code before fixing it, running the mutation it names;
     a wrong finding gets the reason, naming the two things compared, not a "fix". Run step 3
     again, then send the same reviewer (`SendMessage`; a fresh one if it cannot be resumed) the
     new diff and, per finding, the fix or the reason
   - `INCONCLUSIVE` → the first time, and with fewer than three reviews run, supply what it says
     is missing and review again; otherwise stop
   - Every verdict: relay its "Not answered by archmap" list, the dogfooding feedback CLAUDE.md
     asks to note
6. **Stop and report** when a third review is still not `PASS`, after a second `INCONCLUSIVE`,
   when requirements conflict, when a design decision needs Ryota, or when a usage or permission
   limit blocks a needed check
