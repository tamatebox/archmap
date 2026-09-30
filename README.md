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

- `archmap summary` gives an agent a few kilobytes of structure to read before exploring
- `archmap scan` writes the full graph for tools, export and debugging; agents never need to read it,
  because `summary`, `query` and `impact` return the parts they need
- `archmap query` answers "what does component X expose and depend on"
- `archmap impact` answers "if I touch this file or component, what else might be affected"
- `archmap check` tells an agent or CI whether a change broke a declared dependency rule
- every fact points to `file:line` evidence, so an agent can verify and jump to the source

An MCP adapter is planned, but the engine and CLI come first.

### Agent plugin

`plugins/archmap/` is a plugin whose skill tells a coding agent how to read
archmap output: start from `summary`, drill down with `query` and `impact`,
and keep in mind what the graph cannot see. It only calls the CLI, so
install both:

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
2. **Cheap structural scan first.** File layout, manifests, public items and
   imports are extracted first; deeper semantic passes are opt-in and later.
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
  - `use` statements pointing at other packages become `import` edges
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
  - import names are matched to declared distributions by name (`pandas_gbq`), by dotted name
    (`google.cloud.bigquery`), through installed `RECORD` files in a `.venv`, and finally through a small
    table of well-known names (`sklearn`, `yaml`); the evidence note of each import says which one matched,
    and imports of undeclared packages are not edges
  - public top-level `def` / `class` / `CONSTANT` and public methods of public classes become symbols
    for files inside a regular package tree; test files (pytest conventions) and namespace trees outside
    any regular package contribute imports only
  - source files are scanned structurally line by line, not parsed; bodies are ignored
- JSON output with evidence on every node and edge, written to `<root>/.archmap/graph.json` by default
- structural roll-up and a deterministic Markdown summary, written to `<root>/.archmap/summary.md`
- `query` and `impact` implemented on top of the scanned graph
- `check` compares the graph with a declared architecture in `archmap.toml`: forbidden
  dependencies, cycles, and declarations that match nothing

Known gaps: imports of undeclared packages and the standard library produce
no edges by design, so an undeclared dependency is invisible until rules
exist; dynamic imports are not seen; Rust components are package-level while
Python components are module-level.

## Usage

```bash
cargo run -p archmap-cli -- summary .                 # writes ./.archmap/summary.md
cargo run -p archmap-cli -- summary . --depth 1 -o -  # coarser, to stdout
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
├── components: { id -> Component { kind: package | module | external, language, path, parent?, evidence } }
├── symbols:    { id -> Symbol { kind: function | struct | enum | trait | ..., component, signature, evidence } }
├── edges:      [ Edge { from, to, kind: import | dependency | call | http | database | event | unknown, evidence } ]
└── unresolved_imports: [ UnresolvedImport { from, module, provided_by?, evidence } ]
```

An unresolved import is an import that matches no internal module, no
standard-library module, no declared distribution and no file or directory
name in the project. It is an observation for `check`, never an edge.

Each analyzer produces a `GraphFragment`; the graph merges fragments,
collapses edges that describe the same relationship, and keeps all of their
evidence. Output is deterministic (sorted, no timestamps) so graphs can be
diffed.

`archmap scan` and `archmap summary` write only their own output files. They
do not add a `.gitignore` or otherwise decide whether the output is
committed; add `.archmap/` to your repository's ignore rules if you do not
want it tracked. `summary`, `query` and `impact` re-scan instead of reading
the saved graph, so they are never stale.

## Summary

`archmap summary` rolls the graph up and renders it as Markdown. Depth
counts containment levels below a package: depth 0 keeps only packages,
depth 2 keeps packages and two levels of modules, and anything deeper is
folded into its ancestor. The summary lists:

- the component tree with paths, public symbol counts and how many
  submodules were folded
- internal dependencies with the number of import statements behind each
- external dependencies with where they are declared and who imports them
- the components depended on by the most others

On a 380-file Python repository, depth 2 turns a 528 KB graph into a
summary of about 10 KB.

`summary`, `query` and `impact` share one default depth, so they always
describe the same components. Asking `query` or `impact` about a component
that is folded at that depth answers for the component it is folded into and
says so, and `query` lists the children to ask about with a larger
`--depth`.

`query` prints compact text by default: public symbols with their location,
and each neighboring component with its import count and a few example
locations. Lists are capped at 30 entries and 3 locations, and the rest is
counted. On the repository above, its busiest component takes 7 KB as text
and 94 KB as JSON. `--verbose` lifts the caps and `--format json` adds every
piece of evidence.

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

[undeclared_imports]
forbid = true           # imports of packages no manifest declares
ignore = ["ujson"]      # dotted prefixes to accept, e.g. optional imports
```

A selector is a path prefix, where `src/core` covers everything below it, or
an external id such as `ext:requests` or `ext:google-*`. When selectors
overlap, the most specific one owns a component.

`check` reports forbidden dependencies with the evidence behind them,
dependency cycles at the roll-up depth (`depth` in the file or `--depth`,
default 2), undeclared imports, and declarations, rule sides or `ignore`
entries that match nothing, so a typo never silently disables a rule.

For Python, an import counts as declared when a runtime dependency, an extra,
a dependency group or a dev dependency declares its distribution. Without a
`.venv`, archmap cannot match every import name to its distribution; add
such names to `ignore`. With a `.venv`, the finding also names the installed
distribution that provides the module, which is usually a transitive
dependency. It exits 0 without findings, 1 with findings,
and 2 when the rules or the repository cannot be read. archmap checks its own
`cli -> scan -> core` direction this way; see `archmap.toml`.

The declared architecture never changes what `scan`, `summary`, `query` or
`impact` report.

## Roadmap

| Phase | Scope | Status |
|---|---|---|
| 0 Discovery | languages, manifests, packages; report detected languages even without an analyzer | Rust and Python only |
| 1 Structural Facts | modules, public symbols, imports, dependencies | Rust and Python |
| 2 Structural Compression & Agent Context | roll-up; `summary`, `query` and `impact` small enough for an agent and at one granularity; full detail with `--format json` | done for Python |
| 3 Rules & Declared Architecture | declared components and layers, cycles, forbidden dependencies, drift, CI `check` | deny rules, cycles, undeclared imports, stale declarations; layers and wider drift open |
| 4 Cross-system Graph | OpenAPI, Terraform, databases, HTTP, events | planned |
| 5 Semantic Enrichment | LLM naming and responsibilities, stored as inferred facts | planned |
| 6 Agent Interface | plugin and skill for agents, MCP adapter over the same engine | plugin and skill exist; MCP planned |
| 7 Incremental / Runtime | diff scans, cache, runtime traces | planned |

Phase 2 is done when every agent-facing output is small and consistent:
about 10 KB for `summary`, a few KB to a few tens of KB for `query`, a few KB
for `impact`, with complete data one `--format json` away. Shrinking
`graph.json`, caches, databases, LLM enrichment and MCP are not part of it.

Python is evaluation-ready: import names resolve to declared distributions,
`query` and `impact` see the same components as `summary`, and the outputs
meet those sizes. The next step
is an evaluation with a coding agent, comparing the same tasks without
archmap and with archmap as a whole: the summary up front, plus `query` and
`impact` on demand. Correctness is compared first, then tokens, tool calls
and turns.

Cross-system graphs, LLM enrichment and MCP do not change what an agent
learns about a repository, so they wait for that evaluation. Rules started
early for the same reason: they never change what `summary`, `query` or
`impact` report. Still open in Phase 3: ordered layers, and drift beyond
declarations that match nothing. Before
Rust repositories are evaluated, Rust needs module-level components; generic
discovery for unsupported languages completes Phase 0.

## Development

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo check --workspace
```

See `CLAUDE.md` for design principles and contribution rules.
