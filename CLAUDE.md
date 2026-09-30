# archmap

Rust CLI that scans a codebase and produces an **Architecture Graph**: a
coarse, evidence-backed map of components, public interfaces and
dependencies, so that coding agents and humans can reason about structure
and impact without re-reading the whole repository.

## Operating principles

- Plan briefly, implement a small vertical slice, run the checks, then report.
- Prefer deterministic facts (AST, manifests, file layout) over inference. No LLM in the core pipeline.
- Keep the graph *usefully coarse*. Components and public interfaces belong in it; private helpers do not.
- Every node and edge must carry `Evidence` (file, line, note) explaining why it exists.
- Small, testable changes. Add or extend a fixture under `fixtures/` for new extraction behavior.
- Verify before claiming done: run the commands below and read the output.

## Non-negotiable distinctions

- **Code Graph != Architecture Graph.** Do not turn every function or call into a node. Compress to components, modules, public symbols and dependencies.
- **Fact Extraction != Semantic Inference.** Analyzers record what code and manifests literally say. Guesses such as "this module is the Billing component" must live in a separate, clearly labeled layer (not yet built). Never mix the two in one type.
- **Cheap structural scan first, selective semantic scan later.** Do not parse bodies or docstrings by default; design so deeper passes can be added for chosen targets.
- **Many inputs, one model.** Each language / manifest / schema may be analyzed differently, but everything normalizes into `archmap-core` types.
- **MCP is an adapter, not the core.** CLI is the first interface; MCP, if added, is a thin layer over the same engine.

## Crates and dependency direction

```
archmap-cli  ->  archmap-scan  ->  archmap-core
```

- `archmap-core`: graph model (`Component`, `Symbol`, `Edge`, `Evidence`, `GraphFragment`, `ArchitectureGraph`), merge/normalize, query and impact primitives. No I/O, no language knowledge, no dependency on other workspace crates.
- `archmap-scan`: repo walking, project detection, the `Analyzer` trait and concrete analyzers (`rust/`, `python/`). Emits `GraphFragment`s; `scan()` merges them.
- `archmap-cli`: `clap` commands and output rendering only. No analysis logic.

Never add a dependency that points against the arrow. Never make `archmap-core` aware of Cargo, `syn`, files or paths beyond plain strings.

## Adding an analyzer

1. Create `crates/archmap-scan/src/<lang>/` implementing `Analyzer` (`name`, cheap `detect`, `analyze -> AnalyzerOutput`).
2. Register it in `default_analyzers()`. Static registration only; no dynamic plugin system.
3. Per-file problems go into `AnalyzerOutput::warnings`, not `Err`.
4. Add a fixture under `fixtures/` and an integration test in `crates/archmap-scan/tests/`.
5. Do not change `archmap-core` unless a genuinely new *kind* of fact appears. Prefer a new `EdgeKind` / `SymbolKind` variant over new structs.

## Conventions

- Component ids: internal packages use the package name; sub-units (Python packages) use `<package>::<dotted.path>`; external dependencies use the `ext:` prefix. `Component.name` is the short, human-typed form (`shop.billing`) and `query` / `impact` accept it when unique.
- Symbol ids: `<component>::<module path>::<name>`; methods are `Type::method` (Rust) or `Class.method` (Python).
- A `Module` component sets `parent` to its enclosing component. Containment is a field, not an edge.
- Paths in evidence are relative to the scanned root with `/` separators.
- Output must be deterministic: sort collections, no timestamps in the graph.
- `SCHEMA_VERSION` in `archmap-core` is bumped on breaking JSON changes.
- Avoid abstractions without a second concrete use. Three similar lines beat one premature trait.
- Structural scanning (line-based, as in `python/source.rs`) is acceptable when it stays behind the analyzer boundary and is covered by tests; swap in a real parser only when a fixture shows the need.
- `scan` writes `<root>/.archmap/graph.<ext>` by default (`-o <file>` overrides, `-o -` is stdout). Write nothing else into the scanned repository: no `.gitignore`, no config. Do not design features that assume graphs are committed to git.

## Commands to run after every change

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo check --workspace
```

Try the tool on itself as a smoke test without writing files: `cargo run -p archmap-cli -- scan . -o -`
