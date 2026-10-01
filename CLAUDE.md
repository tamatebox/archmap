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
- Agent trials are a feedback loop, not a gate between phases: watch what agents still search for after using archmap on real changes, and add the smallest deterministic fact or query that would have answered it.
- Nothing from a private repository used for dogfooding enters anything pushed or posted (code, comments, fixtures, tests, docs, commits, issues): not its name, paths, packages, counts or layout, not even in general terms. Rebuild a finding as a minimal fixture with invented names.
- Planned work is tracked in GitHub issues; use the `backlog` skill to pick the next task, record deferred work, or close an issue with a change.

## Non-negotiable distinctions

- **Code Graph != Architecture Graph.** Do not turn every function or call into a node. Compress to components, modules, public symbols and dependencies.
- **Fact Extraction != Semantic Inference.** Analyzers record what code and manifests literally say. Guesses such as "this module is the Billing component" must live in a separate, clearly labeled layer (not yet built). Never mix the two in one type.
- **Compression is structural.** Roll-up maps components to their ancestor at a depth through `parent` and merges edges while keeping every piece of evidence. Naming, grouping by meaning and layering belong to declared config (Phase 3) or the inferred layer (Phase 7), never to roll-up or `summary`.
- **Declared != Observed.** `archmap.toml` is only ever compared with the observed graph by `check`. It never changes what `scan`, `summary`, `query` or `impact` report, and declared names never appear in their output.
- **Roll-up hides detail; evidence keeps it.** Which files a dependency connects stays in `Evidence.target` and `scope`. `impact` and cycle checks read it there instead of adding file nodes.
- **Signals are measurements, not verdicts.** A signal reports what the graph shows and never fails `check`. A judgement label comes only from a threshold the user declares.
- **Full graph != agent context.** Agent-facing defaults (`summary`, text `query`, `impact`) stay small however large the repository is: cap lists and count the rest. Complete detail lives behind `--format json`. Commands never read `graph.json`; it is an export.
- **Cheap structural scan first, deep analysis only for chosen targets.** Do not parse bodies or docstrings by default. Deeper passes (calls, types, data flow; Phase 4) run on demand for a component, never over the whole repository by default.
- **Many inputs, one model.** Each language / manifest / schema may be analyzed differently, but everything normalizes into `archmap-core` types.
- **MCP is an adapter, not the core.** CLI is the first interface; MCP, if added, is a thin layer over the same engine.

## Crates and dependency direction

```
archmap-cli  ->  archmap-scan  ->  archmap-core
```

- `archmap-core`: graph model (`Component`, `Symbol`, `Edge`, `Evidence`, `GraphFragment`, `ArchitectureGraph`), merge/normalize, roll-up, cycles, query and impact primitives, and declared rules (`rules`). No I/O, no language knowledge, no dependency on other workspace crates.
- `archmap-scan`: repo walking, project detection, the `Analyzer` trait and concrete analyzers (`rust/`, `python/`, `typescript/`). Emits `GraphFragment`s; `scan()` merges them.
- `archmap-cli`: `clap` commands, reading `archmap.toml`, and output rendering (JSON, Markdown summary, check report) only. No analysis logic.

Never add a dependency that points against the arrow. Never make `archmap-core` aware of Cargo, `syn`, files or paths beyond plain strings.

## Adding an analyzer

1. Create `crates/archmap-scan/src/<lang>/` implementing `Analyzer` (`name`, cheap `detect`, `analyze -> AnalyzerOutput`).
2. Register it in `default_analyzers()`. Static registration only; no dynamic plugin system.
3. Per-file problems go into `AnalyzerOutput::warnings`, not `Err`.
4. Report source files read per language in `AnalyzerOutput::read`, and record every import that maps to no component (standard library aside) as an `UnmappedImport` with its reason. `summary` and `query` rely on both to say what the graph does not show.
5. Add a fixture under `fixtures/` and an integration test in `crates/archmap-scan/tests/`.
6. Describe what the analyzer reads and its known gaps in `docs/reference/analyzers.md`, and add its row to README's "What archmap reads" and its column to "Support by language".
6. Do not change `archmap-core` unless a genuinely new *kind* of fact appears. Prefer a new `EdgeKind` / `SymbolKind` variant over new structs.

## Distributed plugin

- `plugins/archmap/` is what users install, for agents that *use* archmap in their own repositories; this file is for agents that develop archmap. `.claude-plugin/marketplace.json` lists it.
- The plugin only calls the `archmap` binary. Keep the CLI vendor-neutral: nothing in `crates/` knows about any agent.
- `archmap` on PATH is a copy from the last `cargo install`, not the working tree. Develop and verify with `cargo run`; before trying a change through the plugin, reinstall with `cargo install --path crates/archmap-cli`.
- `plugins/archmap/skills/archmap/SKILL.md` restates CLI behavior. When a change alters commands, flags, output wording or a known gap the skill names, update the skill in the same change.
- README is the overview; `docs/reference/` (analyzers, graph, commands, rules) is where behavior is documented. A change that alters behavior updates the page that states it in the same change. README's "Support by language" marks and gap notes summarize `analyzers.md`; a change that closes or opens a gap updates both. Facts shared by every language go in `graph.md` or `commands.md`, not under each language.
- Keep the skill a short guide to reading output and choosing the next command, not a manual. Its frontmatter follows the [Agent Skills](https://agentskills.io/specification) spec, so the skill directory also works outside Claude Code.
- `plugin.json` omits `version` on purpose so installs follow commits. After editing, run `claude plugin validate .`; it warns about the missing version and must otherwise pass.

## Conventions

- Component ids: internal packages use the package name; sub-units use `<package>::<path>`, the dotted path of a Python package or the module path of a Rust module file (`archmap-core::graph`), and a TS/JS directory or file its path relative to the package directory (`ts-shop::src/lib/money.ts`); external dependencies use `ext:<ecosystem>:<name>` (`ext:cargo:serde`, `ext:pypi:requests`, `ext:npm:react`). An id that an earlier analyzer already gave a component at another path is renamed `<id>+<analyzer>`, together with every id that starts with `<id>::`, and the scan warns; equal ids at the same path merge. `Component.name` is the short, human-typed form, the path an import writes (`shop.billing`, `archmap_core::graph`); TS/JS names are paths from the source root with the file extension (`lib/money.ts`), since imports omit extensions or write `.js` for `.ts`, and `query` / `impact` accept it when unique and otherwise list the candidates' ids. Components of different analyzers may share a path (a Python package with scripts below it): a file goes to the one with evidence in it or beside it in a file of the same kind, a directory to the one with evidence directly inside, and a name they share to that owner.
- Symbol ids: `<component>::<module path>::<name>`; methods are `Type::method` (Rust) or `Class.method` (Python and TS/JS). A TS/JS symbol is `<file component>::<name>` (`ts-shop::src/lib/money.ts::formatPrice`), with the file name between for a file the package owns directly (`ts-shop::next.config.ts::config`).
- A `Module` component sets `parent` to its enclosing component. Containment is a field, not an edge.
- `summary`, `query` and `impact` share `DEFAULT_DEPTH` and roll up the same way. Never let them describe different components.
- A rule selector or declaration that matches no component is a finding, never silently skipped, so a typo cannot disable a rule.
- `deny` sides accept declared names or selectors; `layers` and `allow` accept declared names only. Membership goes to the most specific matching selector.
- A Rust re-export from the subtree of its file's module (`pub use child::Item`) shapes what the module offers; it is a relation other than an import. Resolution follows it to the file that defines the item, and it never becomes an edge. Likewise `#[cfg(test)]` code is no dependency of its package on itself.
- A TS/JS file is a `Script` when TypeScript reads it as one (no import or export; the rules are in `typescript/mod.rs` `is_script`, after tsc's `moduleDetection: "auto"`): its top-level declarations are its symbols, and Coverage counts scripts per language because files a package owns directly are no components.
- Import evidence in test code carries `test`: for Python and TS/JS one shared rule (`crates/archmap-scan/src/test_code.rs`: test file names, and files below `test`, `tests`, `__tests__` or `__mocks__`), apart from the rule that decides which files give symbols; for Rust `#[cfg(test)]` and `#[test]` only, never a path. Nothing reads it until the output review decides how rules, cycles and summary treat test code.
- A TS/JS re-export (`export ... from`, `export *`) loads its target, so it is an `Import` edge noted `export`. A named or default import that reaches a name through re-exports also gets evidence for the file that defines it, noted `import via <file>:<line>` with the first re-export on the way, as Rust notes `use via`; a name not found, `export *` sources that disagree, a cycle or more than 32 hops leave only the loaded file. A note ending in ` via <file>:<line>` is reserved for that meaning: `query` shows it as `(via <file>:<line>)`.
- Evidence of an import that points at a file records the names the statement takes from it (`names`), as that file exports them: a default export by the name that file's declaration gives (`default` when it declares none or re-exports it), `*` for the whole module, none for a side-effect import; `via` evidence records the defining file's names. A statement gives one evidence per target and `via`, with the union of its names; core never merges names. Symbol evidence with a `target` says how a symbol is reached (`impl`: a Rust method whose type another file defines); `Symbol::location()` is the evidence without one.
- An import of types only (`type_only`: TS/JS `import type`, `export type ... from`, a statement whose names all carry `type`) is erased before the program runs. It is still a dependency, so `deny`, `layers`, `allow`, `query` and `impact` count it; cycles and signals count only what runs (`Edge::at_runtime`), so it closes no cycle, while a local import, which runs when its function is called, still does. A statement that takes values and types gives one evidence for each.
- External dependency edges point only at required dependencies. An import is not a declaration: an import that maps to no component goes to `unmapped_imports` with its reason, never to an edge, and `check` reports the undeclared ones. Evidence notes record how an import name was resolved.
- Prefer missing a finding to raising a false one: rules end up in CI, and a noisy rule gets switched off. The Python analyzer treats any file or directory name in the project as local code because `sys.path` changes at runtime.
- Bulk-insert edges with `add_edges` / `merge`; `add_edge` is linear per call.
- Paths in evidence are relative to the scanned root with `/` separators.
- Output must be deterministic: sort collections, no timestamps. Evidence and the summary never contain absolute paths.
- `SCHEMA_VERSION` in `archmap-core` is bumped on breaking JSON changes.
- Avoid abstractions without a second concrete use. Three similar lines beat one premature trait.
- Structural scanning (line-based, as in `python/source.rs`) is acceptable when it stays behind the analyzer boundary and is covered by tests; swap in a real parser only when a fixture shows the need.
- `scan` writes `<root>/.archmap/graph.<ext>` by default and `summary` prints to stdout; `-o <file>` saves either elsewhere and `-o -` is stdout. Write nothing else into the scanned repository: no `.gitignore`, no config. Do not design features that assume graphs are committed to git.

## Navigating this repository

Use archmap on itself, the way the plugin skill teaches, running the current source with `cargo run -q -p archmap-cli --`:

- `summary .` first; fixtures appear as components too, so check paths.
- Before searching inside a crate, module or file, `query <crate|module|file>` and open the `file:line` it gives.
- Before changing a public item, `impact <file>`.
- In Rust, `use` declarations and module paths in code (noted `path`) are imports, but code inside macro calls (`vec![Box::new(rust::RustAnalyzer)]`, `print!("{}", crate::query_text::render(..))`) is not read, so also search for callers of what you change.

## Commands to run after every change

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo check --workspace
cargo run -q -p archmap-cli -- check
```

The last command enforces archmap's own `cli -> scan -> core` direction from `archmap.toml`.

Try the tool on itself as a smoke test without writing files: `cargo run -p archmap-cli -- scan . -o -`
