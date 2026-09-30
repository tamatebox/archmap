---
name: archmap
description: Explains how to run archmap, a CLI that maps a repository's components and dependencies, and how to read its output. Use when orienting in an unfamiliar Rust or Python repository, locating where code lives, tracing dependencies, judging what a change could affect, or reading archmap output (summary, query, impact, .archmap/ files).
license: MIT
compatibility: Requires the archmap CLI on PATH.
---

# archmap

archmap maps a repository into components (packages, modules, external dependencies) and the dependencies observed in manifests and imports. Use it to decide what to read; the code stays the source of truth.

## Workflow

Run from the repository root.

1. `archmap --version`. If missing, tell the user archmap is not installed and continue without it.
2. `archmap summary . -o -` for the overview (`-o -` prints instead of writing `.archmap/summary.md`). `Full graph: 0 components` means archmap cannot read these languages: continue without it.
3. `archmap query <component>` for one component: public symbols, incoming and outgoing edges with `file:line` evidence, and `children` to query next.
4. `archmap impact <component-or-file>` before a change: the components that depend on the target.
5. Open the evidence lines, then the source you need.
6. With an `archmap.toml`, run `archmap check` after a change; exit 1 lists broken rules and cycles with evidence.

Options: `--depth N` (default 2; 0 keeps only packages) applies to `summary`, `query` and `impact`; use one value for all three. `query` also takes a symbol name; methods are `Type::method` (Rust) and `Class.method` (Python). For another root, `summary <root>`, but `query`/`impact` take `--path <root>`. Do not read the full graph (`archmap scan`, `.archmap/graph.json`); the commands above return what you need.

## Reading the output

- **Observed, not inferred.** Names are package and directory names, not responsibilities; label any role you infer as inference.
- **A missing edge is not a missing dependency.** No edges exist for the standard library or undeclared packages (`check` lists the latter); dynamic imports, runtime coupling (HTTP, database, events, config, subprocess) and languages other than Rust and Python are invisible. `no resolved imports` does not mean unused: Rust paths used without `use` (`serde_json::to_string`, `#[derive(thiserror::Error)]`), packages used without an import (pytest plugin fixtures, servers run as commands) and import names archmap cannot match without a `.venv` are missed. Search the code before calling anything unused.
- **Everything is rolled up to one depth.** Modules deeper than `--depth` are folded into their ancestor (`N submodules folded`), whose edges and counts then include theirs. Numbers count import statements; `declared` means a manifest also declares the dependency. On a folded name, `query` and `impact` answer for its ancestor and report `folded_from`; go deeper with `children` and a larger `--depth`.
- **Check what a name resolved to.** Names repeat (a package and its top module can both be `shop`), and an exact id beats a name. Read `component.id` (query) or `target` (impact), and `folded_from` if present. If a result is empty or surprising, retry with the id (`shop::shop`) or a file inside it (`src/shop/__init__.py`).
- **impact is structural reachability, not a verdict.** It lists components that import or declare the target, directly or transitively (`transitive` includes `direct`), at component granularity: a whole Rust crate, or a Python package at the chosen depth. Listed components may not use what you change; unlisted ones may still depend on it. Search each for the changed symbol.
- **Components cover everything under the root.** Check `path` before treating a component as product code: fixtures, examples and vendored code appear too.
