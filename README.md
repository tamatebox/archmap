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
  - `pub` items and `pub` inherent methods under `src/` become symbols with signatures
  - `use` statements pointing at other packages become `import` edges; a `use` of a
    `[dev-dependencies]` crate (in a test module) is an import without an edge
  - only files under `src/` are read, so `tests/`, `benches/`, `examples/` and `build.rs` are not
- Python analyzer:
  - `pyproject.toml` (PEP 621 or poetry), `setup.py` / `setup.cfg` directories become `package` components;
    a tree of `.py` files without any manifest gets one root component named after the directory
  - `[project] dependencies`, `[tool.poetry.dependencies]` and `requirements*.txt` become `dependency` edges
    to `ext:*` components (names normalized per PEP 503)
  - every directory with `__init__.py` becomes a `module` component named by its dotted import path
    relative to the project; a `src/` without `__init__.py` is the source root
  - importable directories without `__init__.py` that hold Python files become namespace `module`
    components (PEP 420), so `tests/`, `scripts/` or `experiments/` are components of their own
  - `import` / `from ... import` (including relative imports) become `import` edges between modules,
    or to a declared external dependency
  - the evidence of each import names the file it loads (`pkg/sub.py`, otherwise `pkg/__init__.py`)
    and its scope: `local` inside a function body, `module` elsewhere (including under `if`, `try` and
    `class`); imports between files of one component are kept as self edges, which roll-up hides
  - import names are matched to declared distributions by name (`pandas_gbq`), by dotted name
    (`google.cloud.bigquery`), through installed `RECORD` files in a `.venv`, and finally through a small
    table of well-known names (`sklearn`, `yaml`); the evidence note of each import says which one matched
  - an import that maps to no component, standard library aside, is recorded without an edge and with
    its reason: `undeclared` (no manifest declares it), `declared_not_required` (declared only as an
    extra, a dependency group or a dev dependency) or `local_name` (a file or directory of that name
    exists, probably reached through `sys.path`)
  - calls to `import_module`, `__import__` and `spec_from_file_location` are recorded as dynamic imports,
    which no edge can follow
  - public top-level `def` / `class` / `CONSTANT` and public methods of public classes become symbols
    for files inside a regular package tree; test files (pytest conventions) and namespace trees outside
    any regular package contribute imports only
  - source files are scanned structurally line by line, not parsed; function bodies are read only for imports
- JSON output with evidence on every node and edge, written to `<root>/.archmap/graph.json` by default
- structural roll-up and a deterministic, line-oriented summary printed to stdout, starting with
  what the scan could not see
- `query` on top of the rolled-up graph, including the imports no edge shows, and `impact` that
  follows imports file by file
- `check` compares the graph with a declared architecture in `archmap.toml`: forbidden
  dependencies, layers, allow lists, coverage, cycles, undeclared imports, and declarations
  that match nothing; it also reports structural signals, with or without `archmap.toml`

Known gaps: imports of the standard library, undeclared packages, extras and
dev dependencies produce no edges by design, though `query` lists all but the
standard library as not mapped and `check` can report the undeclared ones;
dynamic imports are recorded but not followed, and `sys.path` changes made at
runtime are not seen; `impact` does not follow the parent `__init__.py` that
Python loads implicitly before a submodule; Rust components are package-level
while Python components are module-level, and Rust evidence does not name
target files yet.

## Usage

```bash
cargo run -p archmap-cli -- summary .                 # prints to stdout
cargo run -p archmap-cli -- summary . --depth 1       # coarser
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
whether the statement runs when its file loads (`module`) or only when a
function is called (`local`). Roll-up hides which files of a component are
involved; `impact` and the cycle check read `target` and `scope` to recover
it. Python records both; Rust records neither yet.

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
  the full graph`, counts, the `source` of the facts and the `next` commands
- `## Coverage`, before the map: for each analyzed language `files`, `read`
  and `imports without an edge`; the languages no analyzer reads, such as
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

On a 380-file Python repository, depth 2 turns a 790 KB graph into a
summary of about 11 KB.

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

`impact` follows imports file by file where the evidence names the imported
file: a component is affected only when one of its files imports what
changed, directly or through other files, not merely because it imports some
file of the same component. A file target starts from that file; a component
target starts from all of its files. Dependencies without a target file
(manifests, external packages, Rust) are followed component by component,
and the result is still reported at the roll-up depth. It does not follow the
parent `__init__.py` that Python runs before a submodule, and a path that
names no component or file is an error. On the repository above, a cycle
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
- `file level: unknown, no import targets recorded`: no evidence names the
  imported files, as for Rust

None of these says whether the program fails at runtime.

For Python, an import counts as declared when a runtime dependency, an extra,
a dependency group or a dev dependency declares its distribution. Without a
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

Phases describe capability layers, not a strict order of work. Phase 7
already ships a plugin and skill because they only wrap the CLI, while
Phases 4 to 6 have not started.

| Phase | Scope | Status |
|---|---|---|
| 0 Discovery | languages, manifests, packages; report detected languages even without an analyzer | Rust and Python; other languages are counted in `summary`, not analyzed |
| 1 Structural Facts | modules, public symbols, imports with their target file and scope, dependencies | Rust and Python; target files and scope for Python |
| 2 Structural Compression & Agent Context | roll-up; `summary`, `query` and `impact` small enough for an agent and at one granularity; full detail with `--format json` | done for Python: `impact` follows files, `query` locations name the imported file, and `summary` and `query` say what the graph does not map |
| 3 Rules & Declared Architecture | declared components and layers, cycles, forbidden dependencies, drift, CI `check` | done: deny rules, layers, allow lists, coverage, cycles with a file-level reading, undeclared imports, stale declarations; structural signals |
| 4 Deep Static Analysis | precise symbol resolution, call and reference graph, type relationships, selective data flow, test-to-code links; on demand for one component | planned |
| 5 Cross-system Graph | OpenAPI, Terraform, databases, HTTP, events | planned |
| 6 Semantic Enrichment | LLM naming and responsibilities, stored as inferred facts | planned |
| 7 Agent Interface | plugin and skill for agents, MCP adapter over the same engine | plugin and skill exist; MCP planned |
| 8 Incremental / Runtime | diff scans, cache, runtime traces | planned |

Phase 4 never runs over the whole repository by default. The cheap scan maps
everything; the deep pass runs for the component an agent picked from
`summary` or `query`, so the map stays coarse globally and precise locally.
Its results are facts with evidence, such as calls, references and types,
never inferred roles, and they answer questions about one part at a time
instead of turning the repository into a code graph.

Phase 2 is done when every agent-facing output is small and consistent:
about 10 KB for `summary`, a few KB to a few tens of KB for `query`, a few KB
for `impact`, with complete data one `--format json` away. Shrinking
`graph.json`, caches, databases, LLM enrichment and MCP are not part of it.

Python is evaluation-ready: import names resolve to declared distributions,
`query` and `impact` see the same components as `summary`, and the outputs
meet those sizes. The next step is an evaluation with a coding agent,
comparing the same tasks without archmap and with archmap as a whole: the
CLI plus the `plugins/archmap` plugin, whose skill starts from `summary` and
drills down with `query` and `impact`. Correctness is compared first, then
tokens, tool calls and turns.

Cross-system graphs, LLM enrichment and MCP do not change what an agent
learns about a repository, so they wait for that evaluation. Rules came
first for the same reason: they never change what `summary`, `query` or
`impact` report. Deep static analysis waits for the evaluation for the
opposite reason: it does change what an agent learns, and the evaluation's
tool-call logs show which searches agents still make after `query`, such as
callers, types or tests, and so which to answer first. Before Rust
repositories are evaluated, Rust needs module-level components and target
files in its evidence; discovering the packages and manifests of other
languages completes Phase 0.

## Development

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo check --workspace
```

See `CLAUDE.md` for design principles and contribution rules.
