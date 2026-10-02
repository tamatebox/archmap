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
- Agent trials are a feedback loop, not a gate between phases: navigate this repository with archmap itself (see "Navigating this repository"), watch what you and other agents still search for after using it on real changes, and add the smallest deterministic fact or query that would have answered it.
- Nothing from a private repository used for dogfooding enters anything pushed or posted (code, comments, fixtures, tests, docs, commits, issues): not its name, paths, packages, counts or layout, not even in general terms. Rebuild a finding as a minimal fixture with invented names.
- Planned work is tracked in GitHub issues; use the `backlog` skill to pick the next task, record deferred work, or close an issue with a change.

## Non-negotiable distinctions

- **Code Graph != Architecture Graph.** Do not turn every function or call into a node. Compress to components, modules, public symbols and dependencies.
- **Fact Extraction != Semantic Inference.** Analyzers record what code and manifests literally say. Guesses such as "this module is the Billing component" must live in a separate, clearly labeled layer (not yet built). Never mix the two in one type.
- **Compression is structural.** Roll-up maps components to their ancestor at a depth through `parent` and merges edges while keeping every piece of evidence. Naming, grouping by meaning and layering belong to declared config (Phase 3) or the inferred layer (Phase 7), never to roll-up or `summary`.
- **Declared != Observed.** `archmap.toml` is only ever compared with the observed graph by `check`. It never changes what `scan`, `summary`, `query` or `impact` report, and declared names never appear in their output.
- **Roll-up hides detail; evidence keeps it.** Which files a dependency connects stays in `Evidence.target` and `scope`. `impact` and cycle checks read it there instead of adding file nodes.
- **Signals are measurements, not verdicts.** A signal reports what the graph shows and never fails `check`. A judgement label comes only from a threshold the user declares.
- **Full graph != agent context.** Every agent-facing command and MCP tool, those added later included, answers in capped text by default and stays small however large the repository is: cap lists and count the rest. Complete detail, every piece of evidence, lives behind `--format json` (MCP `format: json`); `impact`'s JSON is the one exception for now: it caps statements, test files and locations unless `--verbose`, and gives statements without their marks. Commands never read `graph.json`; it is an export.
- **Cheap structural scan first, deep analysis only for chosen targets.** Do not parse bodies or docstrings by default. Deeper passes (calls, types, data flow; Phase 4) run on demand for a component, never over the whole repository by default. The first, where a symbol is used (`archmap_scan::symbol_uses`), reads only the defining file and the files of its importers for the one symbol a command asks about; its facts (`SymbolUse`) never enter the graph, `scan` or `graph.json`, so rules, cycles and roll-up never see them.
- **Many inputs, one model.** Each language / manifest / schema may be analyzed differently, but everything normalizes into `archmap-core` types.
- **Interfaces share one layer.** `archmap-core`, `archmap-scan` and `archmap-app` know no interface. The CLI and the MCP server are first-class interfaces to the same capabilities: each gets scanning, target lookup, the commands and their capped output from `archmap-app`, and holds no analysis or rendering of its own. The CLI depends on `archmap-mcp` only to start it (`archmap mcp`).

## Crates and dependency direction

```
archmap-cli ─┬───────────────> archmap-app  ->  archmap-scan  ->  archmap-core
             └─> archmap-mcp ─┘
```

- `archmap-core`: graph model (`Component`, `Symbol`, `Edge`, `Evidence`, `GraphFragment`, `ArchitectureGraph`), merge/normalize, roll-up, cycles, query and impact primitives, declared rules (`rules`), the facts of on-demand passes (`uses`), and the facts of the committed history (`history`: commits, parents, times, changed files), apart from the graph. No I/O, no language knowledge, no dependency on other workspace crates.
- `archmap-scan`: repo walking, project detection, the `Analyzer` trait and concrete analyzers (`rust/`, `python/`, `typescript/`). Emits `GraphFragment`s; `scan()` merges them. On-demand passes read files for one target (`symbol_uses`, per language in `<lang>/uses.rs`). `history.rs` reads the committed git history with the git CLI, every config-sensitive option pinned, no fetch and no program the repository configures.
- `archmap-app`: what every interface shares: scanning into a `Workspace`, path targets and target lookup, reading `archmap.toml`, and `summary`, `query`, `impact` and `check` with their capped text and JSON, returned as finished strings. It never prints, exits or parses arguments.
- `archmap-cli`: `clap` commands, stdout and stderr, exit codes and `scan`'s file output only, and `archmap mcp`, which only starts the server. No analysis, lookup or rendering.
- `archmap-mcp`: the MCP server (rmcp, stdio): tool parameters and descriptions, server instructions (`src/text.rs`), the root of a call, and one workspace per root, scanned again when `archmap_app::stamp` changes. Answers come from `archmap-app` only; it never runs the CLI.

Never add a dependency that points against the arrow. Never make `archmap-core` aware of Cargo, `syn`, files or paths beyond plain strings. An interface depends on `archmap-app` alone: never re-export items of `archmap-scan` or `archmap-core` to it, since archmap's Rust analyzer follows a `pub use` to the defining crate and `check` sees the edge; give `archmap-app` its own type instead.

## Adding an analyzer

1. Create `crates/archmap-scan/src/<lang>/` implementing `Analyzer` (`name`, cheap `detect`, `analyze -> AnalyzerOutput`).
2. Register it in `default_analyzers()`. Static registration only; no dynamic plugin system.
3. Per-file problems go into `AnalyzerOutput::warnings`, not `Err`.
4. Report source files read per language in `AnalyzerOutput::read`, and record every import that maps to no component (standard library aside) as an `UnmappedImport` with its reason. `summary`, `query` and `impact` rely on both to say what the graph does not show (Coverage, `Not mapped`, `Not traced`).
5. Add a fixture under `fixtures/` and an integration test in `crates/archmap-scan/tests/`.
6. Describe what the analyzer reads and its known gaps in `docs/reference/analyzers.md`, and add its row to README's "What archmap reads" and its column to "Support by language".
6. Do not change `archmap-core` unless a genuinely new *kind* of fact appears. Prefer a new `EdgeKind` / `SymbolKind` variant over new structs.

## Distributed plugin

- `plugins/archmap/` is what users install, for agents that *use* archmap in their own repositories; this file is for agents that develop archmap. `.claude-plugin/marketplace.json` lists it.
- The plugin only calls the `archmap` binary: `.mcp.json` declares `archmap mcp --path ${CLAUDE_PROJECT_DIR:-.}`, and the binary is installed separately. Keep `crates/` vendor-neutral: nothing in them knows about any agent, and the server takes its root from `--path` and each call's `path`, never from an agent's environment variables.
- `archmap` on PATH is a copy from the last `cargo install`, not the working tree. Develop and verify with `cargo run`; before trying a change through the plugin, reinstall with `cargo install --path crates/archmap-cli`.
- `plugins/archmap/skills/archmap/SKILL.md` and the MCP texts in `crates/archmap-mcp/src/text.rs` restate behavior. When a change alters commands, flags, output wording or a known gap they name, update them in the same change. The texts say what a tool gives, when it helps and what it cannot see; they never name a capability archmap lacks or prescribe an order of tools.
- README is the overview; `docs/reference/` (analyzers, graph, commands, mcp, rules) is where behavior is documented. A change that alters behavior updates the page that states it in the same change. README's "Support by language" marks and gap notes summarize `analyzers.md`; a change that closes or opens a gap updates both. Facts shared by every language go in `graph.md` or `commands.md`, not under each language.
- Keep the skill a short guide to reading output and choosing the next command, not a manual. Its frontmatter follows the [Agent Skills](https://agentskills.io/specification) spec, so the skill directory also works outside Claude Code.
- `plugin.json` omits `version` on purpose so installs follow commits. After editing, run `claude plugin validate .`; it warns about the missing version and must otherwise pass.

## Conventions

- Component ids: internal packages use the package name; sub-units use `<package>::<path>`, the dotted path of a Python package or the module path of a Rust module file of the library or of a `src/main.rs` beside a library under `src/` (`archmap-core::graph`), and a TS/JS directory or file, or a module file of another Rust target (a test's `tests/common/mod.rs`), its path relative to the package directory (`ts-shop::src/lib/money.ts`, `kiosk::tests/common/mod.rs`); every Rust target's root belongs to its package; external dependencies use `ext:<ecosystem>:<name>` (`ext:cargo:serde`, `ext:pypi:requests`, `ext:npm:react`). An id that an earlier analyzer already gave a component at another path is renamed `<id>+<analyzer>`, together with every id that starts with `<id>::`, and the scan warns; equal ids at the same path merge. Within TS/JS, of packages that share a name a workspace member (or path dependency) keeps it, then the first by path, and the others become `<name>+<directory>`; TS/JS files that no package owns take the root directory's name, or `<name>+.` when a package has it. `Component.name` is the short, human-typed form, the path an import writes (`shop.billing`, `archmap_core::graph`); TS/JS names are paths from the source root with the file extension (`lib/money.ts`), since imports omit extensions or write `.js` for `.ts`, and a module of another Rust target is named by its path in the package (`tests/common/mod.rs`), which meets no package or library name, and `query` / `impact` accept it when unique and otherwise list the candidates' ids. Components of different analyzers may share a path (a Python package with scripts below it): a file goes to the one with evidence in it or beside it in a file of the same kind, a directory to the one with evidence directly inside, and a name they share to that owner.
- Symbol ids: `<component>::<module path>::<name>`; methods are `Type::method` (Rust) or `Class.method` (Python and TS/JS). A TS/JS symbol is `<file component>::<name>` (`ts-shop::src/lib/money.ts::formatPrice`), with the file name between for a file the package owns directly (`ts-shop::next.config.ts::config`); a Rust symbol of a target other than the library and `src/main.rs` takes its file the same way (`kiosk::tests/total.rs::helper`), and `pub mod a;` is the symbol of the module it loads.
- A `Module` component sets `parent` to its enclosing component. Containment is a field, not an edge.
- `summary`, `query` and `impact` share `DEFAULT_DEPTH` and roll up the same way. Never let them describe different components.
- A rule selector or declaration that matches no component is a finding, never silently skipped, so a typo cannot disable a rule.
- `deny` sides accept declared names or selectors; `layers` and `allow` accept declared names only. Membership goes to the most specific matching selector.
- A Rust re-export from the subtree of its file's module (`pub use child::Item`) shapes what the module offers; it is a relation other than an import. Resolution follows it to the file that defines the item, and it never becomes an edge. Likewise `#[cfg(test)]` code is no dependency of its crate on itself; a test, example or bench is a crate of its own.
- A TS/JS file is a `Script` when TypeScript reads it as one (no import or export; the rules are in `typescript/mod.rs` `is_script`, after tsc's `moduleDetection: "auto"`): its top-level declarations are its symbols, and Coverage counts scripts per language because files a package owns directly are no components.
- Import evidence in test code carries `test`, and so does the evidence of a symbol test code defines: for Python and TS/JS one shared rule (`crates/archmap-scan/src/test_code.rs`: test file names, and files below `test`, `tests`, `__tests__` or `__mocks__`; TS/JS reads `test` and `tests` below a Next.js package's routes as URL segments), apart from the rule that decides which files give symbols; for Rust `#[cfg(test)]` and `#[test]`, and the files of tests, examples and benches by the kind of Cargo target, never a path rule. Rules, cycles and signals count production code only (`Edge::in_production`; `Evidence::runs_in_production` combines both marks per evidence, never per edge), `summary` counts test statements apart, and undeclared imports count in tests too.
- The TS/JS resolver sees only the scanned files, plus what an install would link: workspace members and the directories of `file:`/`link:`/`portal:` dependencies as `node_modules/<name>` links, in the workspace root that names a member or the package that declares a path (`typescript/workspace.rs`, `fs.rs`), so code reaches the nearest one as Node does. Nothing else of `node_modules` exists for it, so the graph is the same before and after an install, and a package that no workspace names never takes over a dependency's name.
- A TS/JS re-export (`export ... from`, `export *`) loads its target, so it is an `Import` edge noted `export`. A named or default import that reaches a name through re-exports also gets evidence for the file that defines it, noted `import via <file>:<line>` with the first re-export on the way, as Rust notes `use via`; a name not found, `export *` sources that disagree, a cycle or more than 32 hops leave only the loaded file. A note of one word, then ` via <file>:<line>`, is reserved for that meaning (`archmap_core::via_place`): `query` shows it as `(via <file>:<line>)`. The note `export` is reserved too: a statement noted `export` passes the names it takes on (`Evidence::passes_on`), and `impact` follows a changed file or symbol through such barrels only to the statements that may take what they pass on (`Node::Passes` in `reach`). A re-export of a package or of an unresolved path keeps `export` as its note's first word (`Evidence::re_exports`), so core knows the file passes on names the graph does not list.
- A Python import that takes a name from a file which binds it by importing it from another (`pkg/__init__.py` with `from .charge import pay`) also gets `import via <file>:<line>` evidence for the file that defines it (`python/reexports.rs`). A Python `from` import binds a name its own file may use as well as pass on, so it is noted `export` only when nothing in the file can use it: it binds definitions, not submodules (imported for what loading them registers), each listed in a literal `__all__` or written `x as x`, none written anywhere else in the file's text, and the file reaches no name by a computed one (`python/source.rs` `relays`). Then a package's `__init__.py` is a barrel like a TS/JS one, and past it `impact` follows names only, the imports of a module below the package included; `Not traced` names such barrels (`barrels:`), since a rename, a removal or an error on load breaks whatever else loads them.
- The component evidence note `package` is reserved too: it names an entry file that runs before the files below it are loaded (`Evidence::runs_first`, a Python `__init__.py`), so `impact` reaches it from the statements outside its component and the modules below it that import a file below it, apart from imports of types only and `via` evidence, and from every file below it that imports something or defines a public symbol. The note `entry` is reserved too: it names a file the component's dependents load (`Evidence::is_entry`: a Rust library's root; what a TS/JS `package.json` names as `main`, `module`, `types`, `exports`, else Node's `index.js`, and the source root's `index.*`), and a component with such evidence is stood for in `impact` only by the files its evidence names, its manifest included.
- A Python statement that binds a module (`import pkg.sub`, `from pkg import sub`, with `as` too) records the names the rest of its file reads through it (`sub.pay`, `pkg.sub.pay`; `python/reads.rs`, a pass over the text that knows strings and comments), and `*` when the file uses the module itself, mentions it in a string or reads nothing through it, so `query` lists it by name and `via` evidence follows those names through a package's `__init__.py`.
- Evidence of an import that points at a file records the names the statement takes from it (`names`), as that file exports them: a default export by the name that file's declaration gives (`default` when it declares none or re-exports it), `*` for the whole module, none for a side-effect import; `via` evidence records the defining file's names. A statement gives one evidence per target and `via`, with the union of its names; core never merges names. Symbol evidence with a `target` says how a symbol is reached (`impl`: a Rust method whose type another file defines); `Symbol::location()` is the evidence without one.
- A test's mock that replaces its module for the file's whole run carries `replaces` (TS/JS: a hoisted `vi.mock` or `jest.mock` whose factory loads nothing; `typescript/source.rs` `stands_in`). Its `names` are those the factory gives the module. The test file depends on those names but runs nothing that reaches it only through that module, so `impact` follows the file only along ways that pass none of its replaced modules, apart from one whose mock gives a name the change may alter, and lists the files it leaves out (`Reach::left_out`); statements of types only pass a replaced module, since a mock replaces no type. `ChangeSeed::Importers`, an import name's importers, did not change, so their mocks hide the change.
- An import of types only (`type_only`: TS/JS `import type`, `export type ... from`, a statement whose names all carry `type`; a Python import under `if TYPE_CHECKING:`) never runs. It is still a dependency, so `deny`, `layers`, `allow`, `query` and `impact` count it; cycles and signals count only what runs in production (`Edge::runs_in_production`), so it closes no cycle, while a local import, which runs when its function is called, still does. A statement that takes values and types gives one evidence for each.
- External dependency edges point only at required dependencies. An import is not a declaration: an import that maps to no component goes to `unmapped_imports` with its reason, never to an edge, and `check` reports the undeclared ones. Evidence notes record how an import name was resolved.
- Prefer missing a finding to raising a false one: rules end up in CI, and a noisy rule gets switched off. The Python analyzer treats any file or directory name in the project as local code because `sys.path` changes at runtime.
- Bulk-insert edges with `add_edges` / `merge`; `add_edge` is linear per call.
- Paths in evidence are relative to the scanned root with `/` separators.
- Output must be deterministic: sort collections, no timestamps. Evidence and the summary never contain absolute paths.
- Text output is written alike, with `query_text`'s helpers rather than new forms: locations as `file:line` with the shared marks (`(local)`, `(type)`, `(test)`, `(export)`, `(mock)`, `(via <file>:<line>)`, and for a use `(call)`, `(new)`, `(jsx)`, `(read)`); a capped list counted in its heading (`N, showing M`) or, in `summary`, by an `omitted:` line naming the query for the rest; a closing line that says how to see every entry, naming no flag of one interface; `Not traced` last, when anything applies.
- `SCHEMA_VERSION` in `archmap-core` is bumped on breaking JSON changes.
- Avoid abstractions without a second concrete use. Three similar lines beat one premature trait.
- Structural scanning (line-based, as in `python/source.rs`) is acceptable when it stays behind the analyzer boundary and is covered by tests; swap in a real parser only when a fixture shows the need.
- `scan` writes `<root>/.archmap/graph.<ext>` by default and `summary` prints to stdout; `-o <file>` saves either elsewhere and `-o -` is stdout. Write nothing else into the scanned repository: no `.gitignore`, no config. Do not design features that assume graphs are committed to git.

## Navigating this repository

Ask archmap before searching by hand, the way the plugin skill teaches, running the current source with `cargo run -q -p archmap-cli --` (the MCP tools serve the last installed binary):

- `summary .` first; fixtures appear as components too, so check paths.
- Before searching inside a crate, module or file, `query <crate|module|file>` and open the `file:line` it gives.
- Before changing a public item, `impact <file>`.
- In Rust, `use` declarations and module paths in code (noted `path`) are imports, the arguments of macro calls that are expressions included (`vec![Box::new(rust::RustAnalyzer)]`), but a macro whose arguments are no expressions (`json!`, `quote!`) is not read, and calls through an imported name are not recorded, so also search for callers of what you change.
- When grep, the compiler or a test answers what archmap did not (a caller, a construction site, text pinned in tests or docs), note the question and what answered it: a gap that recurs is the next smallest fact or query to add.

## Commands to run after every change

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo check --workspace
cargo run -q -p archmap-cli -- check
```

The last command enforces archmap's own `{cli, mcp} -> app -> scan -> core` direction from `archmap.toml`.

Try the tool on itself as a smoke test without writing files: `cargo run -p archmap-cli -- scan . -o -`
