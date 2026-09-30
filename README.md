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

## Relation to coding agents

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

An MCP adapter is planned, but the engine and CLI come first.

### Agent plugin

`plugins/archmap/` is a plugin whose skill tells a coding agent how to use
archmap around a change: start from `summary`, drill down with `query` and
`impact`, edit only the source that matters, then run `check` when the
repository declares rules in `archmap.toml`. It also says what the graph
cannot see. It only calls the CLI, so install both:

```bash
cargo install --path crates/archmap-cli   # puts `archmap` on PATH
claude plugin marketplace add ./          # Claude Code, from the root of a clone
claude plugin install archmap@archmap
```

`cargo install` copies the binary, so the `archmap` on PATH does not follow
the source. Re-run it after pulling or changing the code. While developing,
use `cargo run -p archmap-cli -- ...` as in [Usage](#usage), which always
builds the current tree.

The skill directory, `plugins/archmap/skills/archmap/`, follows the
[Agent Skills](https://agentskills.io/specification) format, so other agents
can use a copy of it.

## Core ideas

1. **Code Graph is not Architecture Graph.** Nodes are components and public
   symbols, not every function.
2. **Coarse globally, precise locally.** File layout, manifests, public items
   and imports are extracted for the whole repository; deeper passes (calls,
   types, data flow) come later and run only for the part an agent chooses.
3. **Facts, not inference.** The MVP records only what the code and
   manifests literally say. Semantic labels ("this is the Billing service")
   are a separate future layer.
4. **Compression is structural.** A summary folds modules into their
   ancestors at a chosen depth and merges edges, keeping the evidence. It
   never renames, groups by meaning, or adds prose.
5. **Many inputs, one model.** Rust, TypeScript, Python, OpenAPI... are
   analyzed differently but normalized into one graph.
6. **MCP is an adapter.** The core is the graph engine and the CLI.

## MVP scope (current)

- Cargo workspace: `archmap-core` (model), `archmap-scan` (extraction), `archmap-cli`
- Rust analyzer:
  - every `Cargo.toml` package becomes a `package` component; `[dependencies]` become `dependency` edges
  - path / workspace dependencies resolve to internal packages, others become `ext:*` components
  - following `mod` declarations from `src/lib.rs` and `src/main.rs` (`mod a;` loads `a.rs` or
    `a/mod.rs`), every module file becomes a `module` component named by its path as a `use` writes
    it (`archmap_core::graph`, starting with the `[lib] name` when there is one; a binary's modules
    start with the package name) and contained in the component of the file that declares it; an
    inline `mod a { .. }` stays part of its file but keeps its place in the path, and a package's
    library and binary are resolved as the separate crates they are
  - `pub` items and `pub` inherent methods under `src/` become symbols with signatures
  - `use` declarations become `import` edges to the module that defines what they name, through
    `crate::`, `self::`, `super::`, other internal crates, re-exports, globs (which bring in only
    what the importing module can see) and `#[macro_export]` macros; the evidence names that
    module's file and the scope (`local` inside a function body, `module` elsewhere), and the note
    names the first `use` in another file the path went through, usually a re-export
    (`use via crates/archmap-core/src/lib.rs:22`); an external crate is an edge without a file
  - a module path written in code, in a signature, a type, a pattern, an expression or
    `#[derive(..)]` (`crate::summary::render(..)`, `child::run()`, `serde_json::to_string(..)`), is an
    `import` too, noted `path`: one piece of evidence per file and target (the first at module scope,
    else the first), added to the edge a `use` may already give; a path whose first name a `use`
    brought in is that `use`'s dependency and adds nothing, and what a module does with the names it
    imported (calls, references) is not recorded
  - a re-export from the subtree of the file's own module (`pub use graph::ArchitectureGraph` in
    `lib.rs`, also inside an inline `pub mod prelude { .. }` there) is how the module presents what it
    contains, a relation other than an import: it is followed when resolving and never becomes an
    edge, so a crate root and the modules it re-exports form no cycle
  - `use` declarations and paths in `#[cfg(test)]` and `#[test]` code are not dependencies of their
    package on itself, so unit tests add no edges or cycles between the modules of a crate
  - a `use` of a `[dev-dependencies]` crate (in a test module) is an import without an edge
  - only files under `src/` are read, so `tests/`, `benches/`, `examples/` and `build.rs` are not
- Python analyzer:
  - `pyproject.toml` (PEP 621 or poetry), `setup.py` / `setup.cfg` directories become `package` components;
    a tree of `.py` files without any manifest gets one root component named after the directory
  - `[project] dependencies`, `[tool.poetry.dependencies]` and `requirements*.txt` (or `*-requirements.txt`)
    become `dependency` edges to `ext:*` components (names normalized per PEP 503); a requirements file
    whose name has the word `dev`, `test`, `tests`, `testing`, `lint` or `docs` (`requirements-dev.txt`,
    `test_requirements.txt`, `requirements/lint.txt`), or whose directory is named by one of them
    (`docs/requirements.txt`), declares dev dependencies instead, like the extras, dependency groups and
    dev dependencies of `pyproject.toml`
  - a declaration covers the files below its manifest: `pyproject.toml` the whole project, a requirements
    file the closest directory at or above it with Python code below it, so `functions/notify/requirements.txt`
    covers `functions/notify/` while `requirements/prod.txt` and `docker/requirements.txt` cover the project;
    an import resolves against the declarations that cover its file, including those of enclosing directories
  - every directory with `__init__.py` becomes a `module` component named by its dotted import path
    relative to the project; a `src/` without `__init__.py` is the source root
  - importable directories without `__init__.py` that hold Python files become namespace `module`
    components (PEP 420), so `tests/`, `scripts/` or `experiments/` are components of their own
  - `import` / `from ... import` (including relative imports) become `import` edges between modules,
    or to a declared external dependency
  - the evidence of each import names the file it loads (`pkg/sub.py`, otherwise `pkg/__init__.py`)
    and its scope: `local` inside a function body, `module` elsewhere (including under `if`, `try` and
    `class`); imports between files of one component are kept as self edges, which roll-up hides
  - a bare import that matches no module but a `.py` file next to the importing file (`import helpers`
    beside `helpers.py`) loads that file, as it does when the directory is on `sys.path` for a script run
    directly or a function deployed from it; its evidence note says so
  - import names are matched to declared distributions by name (`pandas_gbq`), by dotted name
    (`google.cloud.bigquery`), through installed `RECORD` files in a `.venv`, and finally through a small
    table of well-known names (`sklearn`, `yaml`); the evidence note of each import says which one matched
  - an import that maps to no component, standard library aside, is recorded without an edge and with
    its reason: `undeclared` (no manifest declares it), `declared_not_required` (declared only as an
    extra, a dependency group or a dev dependency; the evidence note says where) or `local_name` (a
    file or directory of that name exists, but not as a file next to the importer, probably reached
    through a `sys.path` entry added at runtime); a name imported from a package that an installed
    distribution provides as a module of its own (`from google.cloud import bigquery`) is recorded as
    that module, each name on its own
  - calls to `import_module`, `__import__` and `spec_from_file_location` are recorded as dynamic imports,
    which no edge can follow
  - public top-level `def` / `class` / `CONSTANT` and public methods of public classes become symbols
    for files inside a regular package tree; test files (pytest conventions) and namespace trees outside
    any regular package contribute imports only
  - source files are scanned structurally line by line, not parsed; function bodies are read only for imports
- JSON output with evidence on every node and edge, written to `<root>/.archmap/graph.json` by default
- structural roll-up and a deterministic, line-oriented summary printed to stdout, starting with
  what the scan could not see
- `query` on top of the rolled-up graph, for a component, a symbol or a single file, including the
  imports no edge shows, and `impact` that follows imports file by file; both take a directory for
  the component that owns it, and `query` takes an import name that no component carries (`torch`
  declared as an extra) for the imports of it that no edge shows
- `check` compares the graph with a declared architecture in `archmap.toml`: forbidden
  dependencies, layers, allow lists, coverage, cycles, undeclared imports, and declarations
  that match nothing; it also reports structural signals, with or without `archmap.toml`

Known gaps: imports of the standard library, undeclared packages, extras and
dev dependencies produce no edges by design, though `query` lists all but the
standard library as not mapped (`query <import name>` lists where one module
is imported) and `check` can report the undeclared ones;
dynamic imports are recorded but not followed, and `sys.path` changes made at
runtime are not seen; `impact` does not follow the parent `__init__.py` that
Python loads implicitly before a submodule. For Rust, `use` declarations and
module paths in code are imports, but code inside macro calls
(`vec![Box::new(rust::RustAnalyzer)]`, `print!("{}", crate::query_text::render(..))`)
is not read, and neither is a module's own use of what it re-exports, so
`query` and `impact` miss those dependents. Files under
`src/bin/`, `#[path]` modules and targets that `Cargo.toml` places elsewhere
belong to their package without a module tree, and the crate-relative `use`
paths of Rust 2015 resolve only in the crate root. A path through a `mod`
whose file was not read points at no file; any other name the scan cannot
place (a module generated by a macro, say) points at the deepest module the
path reached. A prelude file (`src/prelude.rs`) that re-exports modules which
glob-import it forms a cycle with them, because only re-exports from a
module's own subtree are not edges. Unit tests are left out of dependencies
within their crate, so `impact` does not list them. Rust components are finer
than Python's: a module file rather than a package directory.

## Usage

```bash
cargo run -p archmap-cli -- summary .                 # prints to stdout
cargo run -p archmap-cli -- summary . --depth 1       # coarser
cargo run -p archmap-cli -- summary . --verbose       # every component and dependency
cargo run -p archmap-cli -- summary . -o summary.md   # saves to a file instead
cargo run -p archmap-cli -- scan .                    # writes ./.archmap/graph.json
cargo run -p archmap-cli -- scan . -o graph.json      # explicit file
cargo run -p archmap-cli -- scan . -o - | jq .edges   # stdout
cargo run -p archmap-cli -- scan . --manifests-only
cargo run -p archmap-cli -- query archmap-core               # compact text, capped lists
cargo run -p archmap-cli -- query archmap-core --verbose     # every symbol and location
cargo run -p archmap-cli -- query archmap-core --format json # complete, with all evidence
cargo run -p archmap-cli -- query scan            # by symbol name
cargo run -p archmap-cli -- impact archmap-core
cargo run -p archmap-cli -- impact crates/archmap-scan/src/lib.rs
cargo run -p archmap-cli -- query src.pipeline.components --depth 3 --path ../some-python-repo
cargo run -p archmap-cli -- check                 # rules from ./archmap.toml; exit 1 on findings
cargo run -p archmap-cli -- check --format json --config ci/rules.toml
cargo run -p archmap-cli -- check --path ../some-python-repo   # no archmap.toml: signals only

# any Python project or package directory works the same way
cargo run -p archmap-cli -- scan ../some-python-repo
cargo run -p archmap-cli -- query shop.billing --path ../some-python-repo   # by dotted name
cargo run -p archmap-cli -- impact src/shop/users.py --path ../some-python-repo
cargo run -p archmap-cli -- query src/shop/users.py --path ../some-python-repo  # one file
cargo run -p archmap-cli -- query shop.users --path ../some-python-repo         # the same file
```

Example edge from the output:

```json
{
  "from": "archmap-cli",
  "to": "archmap-scan",
  "kind": "import",
  "evidence": [
    { "file": "crates/archmap-cli/src/commands.rs", "line": 6, "note": "use" }
  ]
}
```

## Architecture Graph model

```text
ArchitectureGraph
├── meta:       { root, analyzers, tool_version, coverage: { language -> { files, read? } } }
├── components: { id -> Component { kind: package | module | external, language, path, parent?, evidence } }
├── symbols:    { id -> Symbol { kind: function | struct | enum | trait | ..., component, signature, evidence } }
├── edges:      [ Edge { from, to, kind: import | dependency | call | http | database | event | unknown, evidence } ]
├── unmapped_imports: [ UnmappedImport { from, module, reason: undeclared | declared_not_required | local_name, provided_by?, evidence } ]
└── dynamic_imports:  [ DynamicImport { from, call, evidence } ]

Evidence { file, line?, note?, target?, scope?: module | local }
```

`target` is the repository file a dependency points at and `scope` says
where the statement sits: at module level (`module`) or inside a function
body (`local`). For Python that decides whether it runs when its file loads
or only when the function is called; for Rust it is only where the statement
is written. Roll-up hides which files of a component are involved; `impact`
and the cycle check read `target` and `scope` to recover it. Python and Rust
record both.

An unmapped import is an import that maps to no component, standard-library
imports aside, and `reason` says why. A dynamic import is a call that loads a
module by a name computed at runtime. Both are observations, never edges:
they mark where a dependency may exist that no edge shows. `query` lists them
and `check` reports the undeclared ones. `meta.coverage` counts the files of
each recognized source language and how many an analyzer read; a language
without `read` has no analyzer. Configuration, data and documentation files
are not counted. The JSON carries `schema_version: 2`.

Each analyzer produces a `GraphFragment`; the graph merges fragments,
collapses edges that describe the same relationship, and keeps all of their
evidence. Output is deterministic (sorted, no timestamps) so graphs can be
diffed.

`archmap scan` writes only its own output file, and `summary` writes a file
only when `-o` names one. Neither adds a `.gitignore` or otherwise decides
whether the output is committed; add `.archmap/` to your repository's ignore
rules if you do not want it tracked. `summary`, `query`, `impact` and
`check` re-scan instead of reading the saved graph, so they are never stale.

## Summary

`archmap summary` rolls the graph up and prints it to stdout, one fact per
line under a few Markdown headings. Depth counts containment levels below a
package: depth 0 keeps only packages, depth 2 keeps packages and two levels
of modules, and anything deeper is folded into its ancestor. The summary
contains:

- a header of `key: value` lines: `root`, `depth`, `components: N shown, M in
  the full graph` (`N shown of K at depth` when the tree is capped), counts,
  the `source` of the facts and the `next` commands
- `## Coverage`, before the map: for each analyzed language `files`, `read`
  and `imports without an edge`, counted in statements per reason; the languages no analyzer reads, such as
  `not analyzed  sql: 145  notebook: 68`; the number of `dynamic imports` and
  the components that make them; and a fixed line naming the runtime coupling
  no analyzer reads (HTTP, databases, queues, subprocesses,
  configuration-driven loading)
- the component tree, indented by containment, with kind, language, path,
  `symbols: N` and `folded: N` for submodules folded into the component
- internal dependencies as `a -> b  imports: N`, plus `declared: yes` when a
  manifest also declares the dependency; N counts distinct `file:line`
  statements, and imports between files of one component are not listed
- external dependencies with the manifests that declare them (`declared:`),
  the number of importing components (`importers:`) and the top importers
- the components depended on by the most others, with `dependents`,
  `dependencies` and `rank`

The summary is an index for choosing what to `query` next, so it stays
small however large the repository is. The component tree lists the
packages first and then the modules with the most dependents plus
dependencies, each only when it fits with its ancestors, up to 30 lines.
Internal dependencies keep 30: those between packages first, then those
into components more others depend on, then those with more import
statements. External dependencies keep the 20 imported by the most
components. A capped list ends in an `omitted:` line that counts the rest,
says where they are and names the query that shows them:

```text
omitted: 12 modules  in: shop.billing 7, shop 5  next: archmap query <component>
```

The whole summary then aims at 8 KiB: over it, the largest list gives up
its lowest-ranked entries, down to 10 each. The header, coverage, `omitted:`
lines and the most depended on list are never trimmed, so very long names
can exceed the target, but the size does not grow with the repository.
`--verbose` lists everything. On a 380-file Python repository, depth 2 turns
a 790 KB graph into a summary of about 8 KB (11 KB with `--verbose`).

`summary`, `query` and `impact` share one default depth, so they always
describe the same components. Asking `query` or `impact` about a component
that is folded at that depth answers for the component it is folded into and
says so, and `query` lists the children to ask about with a larger
`--depth`.

`query` prints compact text by default: public symbols with their location,
and each neighboring component with its import count and a few example
locations. A location names the file the statement loads when archmap knows
it, as in `src/core/raw_data.py:6 -> src/utils/log.py`, and ends in
`(local)` when the import sits inside a function body and so runs only when
the function is called; the others run when their file loads. A `Not mapped`
section then lists the imports of the component that no edge shows, one line
per module with the reason (`local name`, `extra or dev dependency`,
`undeclared`, or `dynamic` for a call that loads modules by name) and where
they are, so an absent edge is never mistaken for an absent dependency.
Lists are capped at 30 entries and 3 locations, and the rest is counted. On
the repository above, its busiest component takes 10 KB as text and 118 KB
as JSON. `--verbose` lifts the caps and `--format json` adds every piece of
evidence.

`query` also takes a single file, by path (`src/shop/users.py`) or as
`<component>.<file stem>` (`shop.users`), and answers with the file-level
facts behind its component: the file's public symbols, what it imports
(`Imports`), the statements elsewhere that import it (`Imported by`), and its
imports without an edge. Where no evidence names imported files for the
file's language, `Imported by` says it is unknown rather than showing none.

`impact` follows imports file by file where the evidence names the imported
file: a component is affected only when one of its files imports what
changed, directly or through other files, not merely because it imports some
file of the same component. A file target starts from that file; a component
target starts from all of its files. Dependencies without a target file
(manifests, external packages) are followed component by component, and the
result is still reported at the roll-up depth. It does not follow the parent
`__init__.py` that Python runs before a submodule, nor Rust code inside macro
calls, and a path that names no component or file is an error. For a file target, `importers`
lists the statements that import the file directly, up to 5 with the total,
so the next read can go straight to them. On the repository above, a cycle
between its two most shared components made a change to either reach 29
components; following files, a single changed file in them reaches 8 to 27
components depending on the file.

## Rules

`archmap check` compares the observed graph with a declared architecture in
`archmap.toml` at the repository root:

```toml
[components]            # declared name = selectors
domain = ["src/core", "src/models"]
pipeline = ["src/pipeline"]

[[deny]]
from = "domain"         # a declared name or a selector
to = "pipeline"
reason = "domain code must not know about orchestration"

[cycles]
forbid = true           # cycles between components at the roll-up depth
scope = ["src"]         # only cycles with a member under these selectors

[undeclared_imports]
forbid = true           # imports of packages no manifest declares
ignore = ["ujson"]      # dotted prefixes to accept, e.g. optional imports

[layers]
order = ["pipeline", "domain"]   # top to bottom: never depend on a layer above

[[allow]]
from = "domain"
to = []                 # the declared components domain may depend on

[coverage]
require = ["src"]       # everything under src must be declared
```

A selector is a path prefix, where `src/core` covers everything below it, or
an external id such as `ext:requests` or `ext:google-*`. When selectors
overlap, the most specific one owns a component. `deny` sides take declared
names or selectors; `layers` and `allow` take declared names only.

Layers go from the top down, and a dependency on a layer above is a
violation. An allow list turns a declared component's dependencies into a
closed set: any other dependency on a declared component is unexpected, and
an allowed dependency the code no longer has is stale. Coverage requires
every component under its selectors to belong to a declaration, judged at
the roll-up depth and for leaves only, so a container such as `src` counts
as covered by what it contains.

`check` reports forbidden, upward and unexpected dependencies with the
evidence behind them, stale allowances, uncovered components, dependency
cycles at the roll-up depth (`depth` in the file or `--depth`, default 2),
undeclared imports, and declarations, rule sides or `ignore` entries that
match nothing, so a typo never silently disables a rule. Text output shows
up to 3 locations per finding; `--format json` lists all of them. It
exits 0 without findings, 1 with findings, and 2 when the rules or the
repository cannot be read, including a `--config` file that does not exist.
Without `--config` and without an `archmap.toml`, `check` reports signals
only and exits 0. archmap checks its own `cli -> scan -> core` direction
this way; see `archmap.toml`.

`[cycles] scope` limits cycle findings to cycles with at least one member
under its selectors, such as product code but not fixtures. Every cycle
finding also says what the files behind it show, because roll-up joins the
files of each component and different files can close the loop:

- `file level: no cycle; different files form each direction`: only the
  components form a cycle
- `file level: cycle through <files>`: files of at least two of the
  components form a cycle, and the line ends with `closes at module scope`
  or `closes only through local-scope imports` (it disappears without
  imports inside function bodies)
- `file level: unknown, no import targets recorded`: no evidence behind the
  cycle names imported files, as when only manifests declare it

None of these says whether the program fails at runtime.

For Python, an import counts as declared when a runtime dependency, an extra,
a dependency group or a dev dependency declares its distribution for the
file's directory. When only a requirements file for another directory
declares it, such as the one next to a separately deployed function, the
finding names that file. Without a
`.venv`, archmap cannot match every import name to its distribution; add
such names to `ignore`. With a `.venv`, the finding also names the installed
distribution that provides the module, which is usually a transitive
dependency.

The declared architecture never changes what `scan`, `summary`, `query` or
`impact` report.

## Signals

`check` also reports signals: deterministic observations about the shape of
the code, with the files behind them. A signal is not a violation. It never
changes the exit code and needs no `archmap.toml`; text output prints it as
a `signal:` line and JSON lists it under `signals`.

One kind exists today, `mixed_directions`: a component and a partner
depend on each other, but the files of the component that the partner uses
are not the files that use the partner.

```text
signal: app.utils mixes dependency directions with app.core, app.models
  used by them: app/utils/log.py (2)
  using them: app/utils/registry.py -> app.models; app/utils/store.py -> app.core
```

It often explains a component cycle that has no file cycle behind it: one
directory holds both shared helpers and code built on top of other
components. Whether that is a problem is a judgement: archmap attaches one
only where a threshold for it is declared, and none can be declared yet.

## Roadmap

Phases describe capability layers, not a strict order of work. The agent
plugin and skill already ship because they only control how the CLI is
used; deeper knowledge layers can be added independently.

The near-term direction is to broaden the deterministic facts archmap can
observe while keeping agent-facing context bounded. Semantic inference
comes later.

| Phase | Scope | Status |
|---|---|---|
| 0 Discovery | languages, manifests, packages; report detected languages even without an analyzer | Rust and Python; Rust targets other than `src/lib.rs` and `src/main.rs` are not discovered; other languages are counted in `summary`, not analyzed |
| 1 Structural Facts | modules, public symbols, imports with their target file and scope, dependencies | Rust and Python, target files and scope included; Rust imports are `use` declarations and module paths in code, not code inside macro calls |
| 2 Structural Compression & Agent Context | roll-up; `summary`, `query` and `impact` small enough for an agent and at one granularity; file and module queries whose evidence leads directly to source; full detail with `--format json` | done for Python: `impact` follows files, `query` accepts components, symbols, files and directories, direct importers point to `file:line`, `summary` caps its lists to an 8 KiB budget, and agent-facing commands say what the graph does not map; the same for Rust, except code inside macro calls |
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
selecting a small relevant subgraph.

The roadmap therefore grows in three directions:

- **depth**: Phase 4 adds finer relationships inside selected code;
- **breadth**: Phase 5 connects code to the surrounding software system;
- **time and work**: Phase 6 connects the current structure to changes
  and explicit development activity.

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

Coding-agent trials are a development feedback loop, not a gate between
phases. The question is not only whether archmap reduces tool calls
overall, but what an agent still searches for after using it.

Early trials already changed the interface. With the first skill, agents
read `summary` and went straight back to ordinary search. Rewriting the
skill as rules (run `query` before searching inside a component) got
agents to call `query` on most non-local tasks, and in some runs to open
the files it listed without searching for them first. One intermediate
wording lost that again, so which command an agent picks still depends
heavily on the skill. File-level queries and the statements that import a
file were added after a remaining search showed that navigation path was
missing.

The searches that remain set priorities for later phases. Seen so far:

- callers of, or references to, a symbol: Phase 4 call and reference
  analysis;
- values such as weights followed through several functions: Phase 4
  selective data flow.

Later phases are meant to answer searches such as:

- crossing from code into infrastructure, APIs, databases or events:
  Phase 5;
- how a suspicious area changed, what tends to change with it, or which
  issue and PR introduced it: Phase 6.

Ordinary text search still has a place. Error messages, arbitrary
configuration values, prose and other information with no deterministic
graph relationship do not need to be absorbed into archmap merely to
eliminate `grep`.

The development loop is therefore:

```text
add an observable relationship
        ↓
let agents use it on real changes
        ↓
inspect the searches they still perform
        ↓
decide whether the missing information is
a deterministic fact archmap should expose
        ↓
add the smallest useful layer or query
```

Semantic expansion waits until the deterministic structure, cross-system
and change/work layers are broad enough to exercise in real repositories.
More knowledge should not mean proportionally more agent context: the
graph may grow, while each task receives only the part it needs.

Rust has module-level components and target files in its evidence, as
Python does, so Rust repositories can get the same treatment. Module paths
written in code count as imports too; what stays invisible to `query` and
`impact` is code inside macro calls, and which items a module uses after
importing them, which Phase 4 references would cover. Extending language and manifest
discovery, including Cargo targets other than `src/lib.rs` and
`src/main.rs`, completes Phase 0 for additional ecosystems.

## Development

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo check --workspace
```

See `CLAUDE.md` for design principles and contribution rules.
