# archmap

`archmap` is a Rust CLI that scans a codebase and produces an
**Architecture Graph**: a compact, evidence-backed description of the
components in a repository, their public interfaces, and how they depend
on each other.

## Why

Coding agents (and new team members) spend a lot of tokens and time
re-reading a repository to answer the same questions: *what are the parts,
what does this part expose, what breaks if I change it?* Full code graphs
answer that at the wrong granularity: every function and call becomes a
node, and the result is as large as the code itself.

archmap aims for the *useful coarseness* of an architecture diagram, but
derived deterministically from the code, kept fresh by a scan, and
queryable by a machine.

## Install

```bash
cargo install --git https://github.com/tamatebox/archmap archmap-cli   # puts `archmap` on PATH
```

From a clone, `cargo install --path crates/archmap-cli` does the same.
`cargo install` copies the binary, so the `archmap` on PATH does not follow
the source. Re-run it after pulling or changing the code.

### Agent plugin

`plugins/archmap/` is a plugin whose skill tells a coding agent how to use
archmap around a change: start from `summary`, drill down with `query` and
`impact`, edit only the source that matters, then run `check` when the
repository declares rules in `archmap.toml`. It also says what the graph
cannot see. It only calls the CLI, so install both. In Claude Code:

```bash
claude plugin marketplace add tamatebox/archmap   # or ./ from the root of a clone
claude plugin install archmap@archmap
```

The skill directory, `plugins/archmap/skills/archmap/`, follows the
[Agent Skills](https://agentskills.io/specification) format, so other agents
can use a copy of it.

## Quick start

From the root of a repository:

```bash
archmap summary .
archmap query <component|directory|file|symbol>
archmap impact <component|directory|file>
archmap check
```

`summary`, `query`, `impact` and `check` print to stdout. `archmap scan`
writes the full graph to `.archmap/graph.json` inside the repository; add
`.archmap/` to its ignore rules if you do not want it tracked.

## Commands

The graph is meant to be consumed by agents as much as by humans:

- `archmap summary` gives an agent a few kilobytes of structure to read before exploring,
  starting with what the scan could not see
- `archmap scan` writes the full graph for tools, export and debugging; agents never need to read it,
  because `summary`, `query` and `impact` return the parts they need
- `archmap query` answers "what does component X expose and depend on"
- `archmap impact` answers "if I touch this file or component, what else might be affected"
- `archmap check` tells an agent or CI whether a change broke a declared dependency rule,
  and points out structural signals that are observations, not failures
- every fact points to `file:line` evidence, so an agent can verify and jump to the source

Agent-facing output stays small however large the repository is: `summary`
aims at about 8 KiB, lists are capped at 30 entries, and a capped list ends
in an `omitted:` line that names the query showing the rest. `--verbose`
lifts the caps and `--format json` adds every piece of evidence.

An MCP adapter is planned, but the engine and CLI come first.

Details: [commands](docs/reference/commands.md), [rules and signals](docs/reference/rules.md),
[graph model and JSON](docs/reference/graph.md).

## What archmap reads

| Language | Read from |
|---|---|
| [Rust](docs/reference/analyzers.md#rust) | `Cargo.toml` packages and dependencies; module files reached from `src/lib.rs` and `src/main.rs`; `pub` items; `use` declarations and module paths in code |
| [Python](docs/reference/analyzers.md#python) | `pyproject.toml`, `setup.py` / `setup.cfg` and requirements files; packages and namespace packages; public top-level definitions; `import` statements, scanned line by line |
| [TypeScript / JavaScript](docs/reference/analyzers.md#typescript-and-javascript) | `package.json` packages and dependencies; directories and files; exported declarations; `import` and `export ... from` resolved through tsconfig paths and re-exports |

Other languages are counted in `summary`, not analyzed. Each analyzer's
behavior and known gaps are in [analyzers.md](docs/reference/analyzers.md).
No analyzer sees runtime coupling (HTTP, databases, queues, subprocesses,
configuration-driven loading) or follows a module loaded by a name computed
at runtime, and nothing is inferred; `summary` and `query` say what the scan
could not see.

### Support by language

What works today, by the [roadmap](#roadmap) phase it belongs to:
✅ implemented, ◐ implemented with a gap that common code runs into,
— not implemented.

| Capability | Phase | Rust | Python | TS / JS |
|---|:-:|:-:|:-:|:-:|
| Packages and declared dependencies | 0 | ✅ | ✅ | ◐ |
| Module and file components | 1 | ◐ | ✅ | ✅ |
| Public symbols with signatures | 1 | ✅ | ✅ | ◐ |
| Imports resolved to the file they load | 1 | ◐ | ✅ | ◐ |
| Re-exports followed to the defining file | 1 | ✅ | — | ✅ |
| Names each import takes | 1 | ◐ | ✅ | ✅ |
| Imports without an edge, with the reason | 1 | ✅ | ✅ | ✅ |
| `summary` and `query`, down to one file | 2 | ✅ | ✅ | ✅ |
| `query` and `impact` on a symbol, by the names imports take | 2 | ◐ | ◐ | ✅ |
| `impact` file by file | 2 | ✅ | ◐ | ✅ |
| `check` rules and cycles | 3 | ✅ | ✅ | ◐ |
| Callers of a symbol | 4 | — | — | — |
| References to a symbol | 4 | — | — | — |
| Type relationships | 4 | — | — | — |
| Selective data flow | 4 | — | — | — |
| Test-to-code links | 4 | — | — | — |

The gaps behind the marks:

- Rust modules: only the module tree reached from `src/lib.rs` and
  `src/main.rs`; other Cargo targets have none, and `tests/`, `benches/`,
  `examples/` and `build.rs` are not read.
- Rust imports: code inside macro calls (`vec![..]`, `println!(..)`) is not
  read.
- Rust names: a function called through a module that a `use` brought in
  (`use crate::graph;`, then `graph::build()`) shows only as the whole
  module (`*`).
- Python re-exports: `from pkg import name` stops at `pkg/__init__.py`, not
  at the file that defines `name`.
- Python `impact`: the parent `__init__.py` that Python loads before a
  submodule is not followed.
- TS/JS packages: the packages of a monorepo workspace are not linked to
  each other.
- TS/JS symbols: CommonJS exports give none.
- TS/JS imports: `require` and `import()` are not read yet.
- TS/JS `check`: only `type` written in an import marks it as types only,
  so a type imported without it (`import { Money }` for an interface) can
  close a cycle that `cycles.forbid` reports.

The commands read one merged graph, so a gap in what an analyzer reads
shows in all of them: an import that is not read is missing from `query`,
`impact` and `check` alike. [analyzers.md](docs/reference/analyzers.md)
lists every known gap.

## Core ideas

1. **Code Graph is not Architecture Graph.** Nodes are components and public
   symbols, not every function.
2. **Coarse globally, precise locally.** File layout, manifests, public items
   and imports are extracted for the whole repository; deeper passes (calls,
   types, data flow) come later and run only for the part an agent chooses.
3. **Facts, not inference.** archmap records only what the code and
   manifests literally say. Semantic labels ("this is the Billing service")
   are a separate future layer.
4. **Compression is structural.** A summary folds modules into their
   ancestors at a chosen depth and merges edges, keeping the evidence. It
   never renames, groups by meaning, or adds prose.
5. **Many inputs, one model.** Rust, TypeScript, Python, OpenAPI... are
   analyzed differently but normalized into one graph.
6. **MCP is an adapter.** The core is the graph engine and the CLI.

## Roadmap

Phases describe capability layers, not a strict order of work. The agent
plugin and skill already ship because they only control how the CLI is
used; deeper knowledge layers can be added independently.

The near-term direction is to broaden the deterministic facts archmap can
observe while keeping agent-facing context bounded. Semantic inference
comes later.

| Phase | Scope | Status |
|---|---|---|
| 0 Discovery | languages, manifests, packages; report detected languages even without an analyzer | Rust, Python and TypeScript/JavaScript; other languages are counted in `summary`, not analyzed |
| 1 Structural Facts | modules, public symbols, imports with their target file and scope, dependencies | Rust, Python and TypeScript/JavaScript, target files and scope included |
| 2 Structural Compression & Agent Context | roll-up; `summary`, `query` and `impact` small enough for an agent and at one granularity; file and module queries whose evidence leads directly to source; full detail with `--format json` | done for Python and Rust; TS/JS file by file through tsconfig paths and re-exports |
| 3 Rules & Declared Architecture | declared components and layers, cycles, forbidden dependencies, drift, CI `check` | done: deny rules, layers, allow lists, coverage, cycles with a file-level reading, undeclared imports, stale declarations; structural signals |
| 4 Deep Static Analysis | precise symbol resolution, callers and reference graph, type relationships, selective data flow, test-to-code links; on demand for one selected area | planned; agent traces so far point first to callers and references, then selective data flow |
| 5 Cross-system Graph | OpenAPI, Terraform, databases, HTTP, events, CI/build/deploy relationships | planned |
| 6 Change & Work Graph | Git history, churn and co-change; Issue → PR → Commit → File; PR overlap and other explicit work links | planned |
| 7 Semantic Enrichment | LLM naming, responsibilities, intent and other semantic interpretations, stored separately as inferred facts | planned |
| 8 Agent Interface | plugin and skill for agents; MCP adapter over the same engine | plugin and skill exist; MCP planned |
| 9 Incremental / Runtime | incremental scans and caches; runtime traces and other observed execution relationships | planned |

Phase 4 never runs over the whole repository by default. The cheap scan
maps everything; a deeper pass runs only for the file, symbol or component
an agent is investigating. Its results remain observed facts with
evidence, such as calls, references, types and data-flow relationships.
The purpose is local precision, not a repository-wide code graph.

The same principle applies as later phases broaden the graph.
Cross-system, history and work data add new kinds of observed
relationships, but not all of those facts need to enter an agent's
context. `summary`, `query`, `impact` and later task-oriented views keep
selecting a small relevant subgraph: the graph may grow, while each task
receives only the part it needs.

The roadmap therefore grows in three directions:

- **depth**: Phase 4 adds finer relationships inside selected code, such
  as the callers of a symbol or a value followed through several functions;
- **breadth**: Phase 5 connects code to the surrounding software system:
  infrastructure, APIs, databases and events;
- **time and work**: Phase 6 connects the current structure to changes
  and explicit development activity: how an area changed, what tends to
  change with it, and which issue and PR introduced it.

Through Phase 6, the emphasis stays on relationships that can be extracted
deterministically and attached to evidence. Semantic interpretation begins
in Phase 7 and remains a separate kind of information, never mixed with
observed or declared facts.

Phase 2 is complete when the common structural questions an agent asks
lead directly to source without the full graph: what exists, what a
component or file exposes, what it imports, what imports it, and what may
be structurally affected. Each answer stays small (about 8 KB for
`summary`, a few KB to a few tens of KB for `query`, a few KB for
`impact`), with complete detail one `--format json` away.

Ordinary text search still has a place. Error messages, arbitrary
configuration values, prose and other information with no deterministic
graph relationship do not need to be absorbed into archmap merely to
eliminate `grep`.

## Development

The Cargo workspace has three crates: `archmap-core` (model), `archmap-scan`
(extraction) and `archmap-cli`. While developing, run
`cargo run -p archmap-cli -- <command>` instead of the installed `archmap`;
it always builds the current tree.

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo check --workspace
cargo run -q -p archmap-cli -- check
```

The last command checks archmap's own `cli -> scan -> core` direction
against `archmap.toml`. Behavior is documented in `docs/reference/`; see
`CLAUDE.md` for design principles and contribution rules.
