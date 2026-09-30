---
name: archmap
description: Explains how to run archmap, a CLI that maps a repository's components and dependencies, and how to read its output. Use when orienting in an unfamiliar Rust or Python repository, locating where code lives, tracing dependencies, judging what a change could affect, checking a finished change against the rules in an archmap.toml, or reading archmap output (summary, query, impact, check, .archmap/ files).
license: MIT
compatibility: Requires the archmap CLI on PATH.
---

# archmap

archmap maps a repository into components (packages, modules, external dependencies) and the dependencies observed in manifests and imports. Use it to decide what to read; the code stays the source of truth.

## Workflow

Run from the repository root.

1. `archmap --version`. If missing, tell the user archmap is not installed and continue without it.
2. `archmap summary .` for the overview. `components: 0 shown` means archmap cannot read these languages: continue without it.
3. `archmap query <component>`: public symbols with `file:line`, dependencies both ways with import counts and example locations, and children. Lists are capped; `--verbose` shows all, `--format json` all evidence.
4. `archmap impact <component-or-file>` before a change: components depending on the target.
5. Open the evidence lines, then the source you need.
6. With an `archmap.toml`, run `archmap check` after a change; exit 1 lists broken rules with evidence. `signal:` lines are observations, never failures.

Options: `--depth N` (default 2; 0 keeps only packages) applies to `summary`, `query` and `impact`; use one value for all three. `query` also takes a symbol name (`Type::method`, `Class.method`). For another root, `summary <root>`, but `query`/`impact` take `--path <root>`. Do not read the full graph (`archmap scan`, `.archmap/graph.json`).

## Reading the output

- **Observed, not inferred.** Names are package and directory names, not responsibilities; label any role you infer as inference.
- **A missing edge is not a missing dependency.** No edges exist for the standard library or undeclared packages (`check` lists the latter); dynamic imports, runtime coupling (HTTP, database, events, config, subprocess) and languages other than Rust and Python are invisible. `importers: none resolved` does not mean unused: Rust paths used without `use` (`serde_json::to_string`, `#[derive(thiserror::Error)]`), packages used without an import (pytest plugin fixtures, servers run as commands) and import names archmap cannot match without a `.venv` are missed. Search the code before calling anything unused.
- **Everything is rolled up to one depth.** Modules deeper than `--depth` are folded into their ancestor (`folded: N`), whose edges and counts include theirs. `imports: N` counts import statements; `declared: yes` means a manifest also declares the dependency. On a folded name, `query` and `impact` answer for its ancestor and say so (`folded from`); go deeper with the children and a larger `--depth`.
- **Check what a name resolved to.** Names repeat (a package and its top module can both be `shop`), and an exact id beats a name. Read the `id:` line (query) or `target` (impact). If a result is empty or surprising, retry with the id (`shop::shop`) or a file inside it (`src/shop/__init__.py`).
- **impact is structural reachability, not a verdict.** It lists components that import or declare the target, directly or transitively (`transitive` includes `direct`), at the chosen depth. Python is traced file by file (a file reaches only its importers, not via the implicitly loaded parent `__init__.py`); Rust per crate. Listed components may not use what you change; unlisted ones may still depend on it. Search each for the changed symbol.
- **Components cover everything under the root.** Check its path before treating it as product code: fixtures, examples and vendored code appear too.
