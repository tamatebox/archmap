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
the source. Re-run it after pulling or changing the code. Building needs
Rust 1.97 or later, which Ruff's Python parser requires; an older toolchain
stops with cargo's message naming the version (`rustup update`).

### Agent plugin

`plugins/archmap/` is a plugin for coding agents. It declares archmap's MCP
server, which gives an agent `summary`, `query`, `impact` and `check` as
tools, and a skill on how to read their output, including what the graph
cannot see. The plugin starts `archmap mcp` but ships no binary: install
archmap as above first, and again after pulling a newer plugin. In Claude
Code:

```bash
cargo install --git https://github.com/tamatebox/archmap archmap-cli   # the binary the plugin starts
claude plugin marketplace add tamatebox/archmap   # or ./ from the root of a clone
claude plugin install archmap@archmap
```

Without the binary, `/mcp` shows the server as failed and the skill tells
the agent to install archmap. The skill directory,
`plugins/archmap/skills/archmap/`, follows the
[Agent Skills](https://agentskills.io/specification) format, so other agents
can use a copy of it, and any MCP client can start the server itself:
`archmap mcp --path <repository>` speaks MCP over stdio
([mcp.md](docs/reference/mcp.md)).

## Quick start

From the root of a repository:

```bash
archmap summary .
archmap query <component|directory|file|symbol>
archmap impact <component|directory|file|symbol>
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
- `archmap query` answers "what does component X expose and depend on", for a Rust or
  TS/JS symbol "where is it used", and for an issue or pull request (`'#123'`) the links
  GitHub records for it and its commits in the local history
- `archmap impact` answers "if I touch this file or component, what else might be affected",
  with the import statements to open, the tests to run again and the files changed in the
  same commits in the git history
- `archmap fetch github` writes a snapshot of the issues, pull requests and links behind
  changes, the one command that reaches the network
- `archmap check` tells an agent or CI whether a change broke a declared dependency rule,
  and points out structural signals that are observations, not failures
- every fact points to `file:line` evidence, so an agent can verify and jump to the source

Agent-facing output stays small however large the repository is: `summary`,
`query`, `impact` and `check` print capped text by default. `summary` aims
at about 8 KiB, lists are capped at 30 entries, and a capped list counts
what it leaves out: `summary` ends it in an `omitted:` line naming the query
that shows the rest, and `query` and `impact` write `N, showing M` in its
heading. `--verbose` lifts the caps and `--format json` adds every piece of
evidence.

The same four commands are MCP tools: `archmap mcp` serves them over stdio
with the same answers, since the CLI and the server share one layer
(`archmap-app`), and keeps the graph of each repository in memory until its
files change.

Details: [commands](docs/reference/commands.md), [MCP server](docs/reference/mcp.md),
[rules and signals](docs/reference/rules.md), [graph model and JSON](docs/reference/graph.md).

## What archmap reads

| Language | Read from |
|---|---|
| [Rust](docs/reference/analyzers.md#rust) | `Cargo.toml` packages and dependencies; module files reached from every Cargo target: `src/lib.rs`, `src/main.rs`, the binaries, tests, examples, benches and build script Cargo finds beside them, and those `Cargo.toml` declares; `pub` items; `use` declarations and module paths in code |
| [Python](docs/reference/analyzers.md#python) | `pyproject.toml`, `setup.py` / `setup.cfg` and requirements files; packages and namespace packages; public top-level definitions; `import` statements and the `import_module` and `__import__` calls that name their module with a literal, scanned line by line and followed through the names modules bind from others, those under `if TYPE_CHECKING:` apart |
| [TypeScript / JavaScript](docs/reference/analyzers.md#typescript-and-javascript) | `package.json` packages, workspaces and dependencies; directories and files; exported declarations, CommonJS exports and the globals of scripts and of `declare global`; `import`, `export ... from`, `require`, `import()` and test mocks, resolved through tsconfig and jsconfig paths, Vite, webpack and Babel aliases, workspace links and re-exports, imports of types only apart |

Other languages are counted in `summary`, not analyzed. Each analyzer's
behavior and known gaps are in [analyzers.md](docs/reference/analyzers.md).
Besides code, `impact` reads the root's committed git history with the git
CLI ([history.md](docs/reference/history.md)): commits, their parents and
times, and the files each changed, renames included; never uncommitted
changes, authors or messages, and never the network. `query '#123'` and
`summary` read the issues, pull requests and links that `archmap fetch
github` wrote ([work.md](docs/reference/work.md)); no other command
fetches.
No analyzer sees runtime coupling (HTTP, databases, queues, subprocesses,
configuration-driven loading) or follows a module loaded by a name computed
at runtime, and nothing is inferred; `summary`, `query` and `impact` say what
the scan could not see.

### Support by language

What works today, by the [roadmap](#roadmap) phase it belongs to:
✅ implemented, ◐ implemented with a gap that common code runs into,
— not implemented.

| Capability | Phase | Rust | Python | TS / JS |
|---|:-:|:-:|:-:|:-:|
| Packages and declared dependencies | 0 | ✅ | ✅ | ✅ |
| Module and file components | 1 | ✅ | ✅ | ✅ |
| Public symbols with signatures | 1 | ✅ | ✅ | ✅ |
| Imports resolved to the file they load | 1 | ✅ | ✅ | ✅ |
| Re-exports followed to the defining file | 1 | ✅ | ✅ | ✅ |
| Names each import takes | 1 | ✅ | ✅ | ✅ |
| Imports without an edge, with the reason | 1 | ✅ | ✅ | ✅ |
| `summary` and `query`, down to one file | 2 | ✅ | ✅ | ✅ |
| `query` and `impact` on a symbol, by the names imports take | 2 | ✅ | ✅ | ✅ |
| `impact` file by file | 2 | ✅ | ✅ | ✅ |
| Test code counted apart from production code | 2 | ✅ | ✅ | ✅ |
| `check` rules and cycles | 3 | ✅ | ✅ | ✅ |
| Callers of a symbol | 4 | ◐ | ◐ | ◐ |
| References to a symbol | 4 | ◐ | ◐ | ◐ |
| Type relationships | 4 | — | — | — |
| Selective data flow | 4 | — | — | — |
| Test-to-code links | 4 | — | — | — |

The gaps behind the marks:

- Callers and references: `query` and `impact` list where a symbol is used
  through the names imports bind and, for a method, through its class or
  type and `this` or `self`; a call through a value of a type
  (`wallet.pay()`) is not read, nor in Python a name its scope binds again
  or the uses in a file that reaches names by computed ones (`globals()`),
  and the uses of a TS/JS script's or `declare global` declaration are read
  in its own file only.

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
6. **Interfaces share one engine.** The CLI and the MCP server offer the
   same capabilities through one shared layer; neither has logic of its own.

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
| 2 Structural Compression & Agent Context | roll-up; `summary`, `query` and `impact` small enough for an agent and at one granularity; file and module queries whose evidence leads directly to source; full detail with `--format json` | done for Rust, Python and TS/JS, test code counted apart |
| 3 Rules & Declared Architecture | declared components and layers, cycles, forbidden dependencies, drift, CI `check` | done: deny rules, layers, allow lists, coverage, cycles with a file-level reading, undeclared imports, stale declarations; structural signals |
| 4 Deep Static Analysis | precise symbol resolution, callers and reference graph, type relationships, selective data flow, test-to-code links; on demand for one selected area | started: where a Rust, Python or TS/JS symbol is used, read on demand by `query`, and by `impact` to leave out the imports of its file that never name it; agent traces point first to callers and references, then selective data flow |
| 5 Cross-system Graph | OpenAPI, Terraform, databases, HTTP, events, CI/build/deploy relationships | planned |
| 6 Change & Work Graph | 6A local git history: commits, renames and the files changed together; 6B issues, pull requests and their explicit links from a fetched GitHub snapshot; 6C code to work and work to code at query time, Issue → PR → Commit → File | 6A: files changed in the same commits in `impact`; 6B: `fetch github` and `query '#123'`, issues, pull requests and their links from a snapshot ([work.md](docs/reference/work.md)); 6C planned |
| 7 Semantic Enrichment | LLM naming, responsibilities, intent and other semantic interpretations, stored separately as inferred facts | planned |
| 8 Agent Interface | plugin and skill for agents; MCP server over the same engine | MCP server (`archmap mcp`), plugin and skill |
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

Phase 6 keeps facts and views apart. The facts are what the history and
the work tracker record: commits with their parents, times and changed
files, and later issues, pull requests and the links GitHub records between
them. Co-change, churn, shared files and the paths from code to work are
views computed when a command asks, with settings of their own, never
stored and never called inferred. Every answer carries what was read (a
shallow clone, a bounded read, a snapshot's range), so a missing link never
reads as no link. The value lies in linking code and work: why an area
changed, what changes with it, and which issue and pull request did it.

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

The Cargo workspace has five crates: `archmap-core` (model), `archmap-scan`
(extraction), `archmap-app` (the commands and their output, shared by every
interface), `archmap-cli` and `archmap-mcp` (the MCP server). While developing, run
`cargo run -p archmap-cli -- <command>` instead of the installed `archmap`;
it always builds the current tree.

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo check --workspace
cargo run -q -p archmap-cli -- check
```

The last command checks archmap's own `{cli, mcp} -> app -> scan -> core` direction
against `archmap.toml`. Behavior is documented in `docs/reference/`; see
`CLAUDE.md` for design principles and contribution rules.
