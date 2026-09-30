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
- `archmap scan` writes the full graph for tools and drill-down
- `archmap query` answers "what does component X expose and depend on"
- `archmap impact` answers "if I touch this file or component, what else might be affected"
- every fact points to `file:line` evidence, so an agent can verify and jump to the source

An MCP adapter is planned, but the engine and CLI come first.

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
  - public top-level `def` / `class` / `CONSTANT` and public methods of public classes become symbols
    for files inside a regular package tree; test files (pytest conventions) and namespace trees outside
    any regular package contribute imports only
  - source files are scanned structurally line by line, not parsed; bodies are ignored
- JSON output with evidence on every node and edge, written to `<root>/.archmap/graph.json` by default
- structural roll-up and a deterministic Markdown summary, written to `<root>/.archmap/summary.md`
- `query` and `impact` implemented on top of the scanned graph; `check` is a stub

Known gaps: `import yaml` is not linked to the `PyYAML` distribution (import
name and distribution name differ); imports of undeclared third-party
packages and the standard library produce no edges; Rust components are
package-level while Python components are module-level.

## Usage

```bash
cargo run -p archmap-cli -- summary .                 # writes ./.archmap/summary.md
cargo run -p archmap-cli -- summary . --depth 1 -o -  # coarser, to stdout
cargo run -p archmap-cli -- scan .                    # writes ./.archmap/graph.json
cargo run -p archmap-cli -- scan . -o graph.json      # explicit file
cargo run -p archmap-cli -- scan . -o - | jq .edges   # stdout
cargo run -p archmap-cli -- scan . --manifests-only
cargo run -p archmap-cli -- query archmap-core
cargo run -p archmap-cli -- query scan            # by symbol name
cargo run -p archmap-cli -- impact archmap-core
cargo run -p archmap-cli -- impact crates/archmap-scan/src/lib.rs
cargo run -p archmap-cli -- check                 # not implemented yet, exits 2

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
└── edges:      [ Edge { from, to, kind: import | dependency | call | http | database | event | unknown, evidence } ]
```

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

## Roadmap

| Phase | Scope | Status |
|---|---|---|
| 0 Discovery | languages, manifests, packages; report detected languages even without an analyzer | Rust and Python only |
| 1 Structural Facts | modules, public symbols, imports, dependencies | Rust and Python |
| 2 Structural Compression | roll-up, summary, query, impact | in progress |
| 3 Rules & Declared Architecture | declared components and layers, cycles, forbidden dependencies, drift, CI `check` | planned |
| 4 Cross-system Graph | OpenAPI, Terraform, databases, HTTP, events | planned |
| 5 Semantic Enrichment | LLM naming and responsibilities, stored as inferred facts | planned |
| 6 Agent Interface | MCP adapter over the same engine | planned |
| 7 Incremental / Runtime | diff scans, cache, runtime traces | planned |

Before going past Phase 2, the summary is evaluated with a coding agent:
the same tasks run with and without it, comparing correctness first and
tokens, tool calls and turns second. Next items inside Phase 2:

- resolve import names to declared distributions (`google.cloud.bigquery`
  to `google-cloud-bigquery`, installed `RECORD` files, a small alias table)
  and record how each was resolved
- generic discovery so that repositories in unsupported languages still get
  a language and manifest overview

## Development

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo check --workspace
```

See `CLAUDE.md` for design principles and contribution rules.
