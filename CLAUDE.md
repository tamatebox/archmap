# archmap

Rust CLI that scans a codebase and produces an **Architecture Graph**: a
coarse, evidence-backed map of components, public interfaces and
dependencies, so that coding agents and humans can reason about structure
and impact without re-reading the whole repository.

## Operating principles

- Plan briefly, implement a small vertical slice, run the checks, get the review `.claude/rules/development-workflow.md` calls for, then report.
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
- **History facts != history views.** Commits, their own parents, committer times and the files each changed under the root (renames with both paths and git's score) are facts, read from committed history only, never uncommitted changes, and never with authors, emails or messages. Co-change, churn and shared files are views computed when a command asks, with settings of their own (`max_files`, renames followed), never stored and never called inferred; `archmap.toml` never changes them. Reading history never reaches the network and runs no program the repository configures, and every answer says what was read, so a shallow clone or a bounded read never reads as "no co-change".
- **Work links are observed, never derived.** Issues, pull requests and the links GitHub records between them are facts of a snapshot that one explicit fetch writes (`fetch github`, through gh, which keeps the token); every other command and the MCP server read the snapshot and never reach the network. Each link keeps its type and the field or event it was observed in, the ends that record it, and the snapshot its range, so "no link" is said only within the range and types fetched and from an end that records the type. A pull request meets local commits by SHA only. No bodies, comments, authors or tokens are kept; archmap parses no text to make a link. Paths across history and work (a file's commits, pull requests and issues in `query` and `impact`, an item's files in `query '#N'`) are views computed when a command asks, each step an observed link or a SHA match; never stored, never an edge.
- **Signals are measurements, not verdicts.** A signal reports what the graph shows and never fails `check`. A judgement label comes only from a threshold the user declares.
- **Full graph != agent context.** Every agent-facing command and MCP tool, those added later included, answers in capped text by default and stays small however large the repository is: cap lists and count the rest. Each answer has one job: `summary` says which component to look at and carries no line numbers, `query` which `file:line` to open, and the source the rest. Complete detail, every piece of evidence, lives behind `--format json` (MCP `format: json`), and `--verbose` lifts the text's caps only. Commands never read `graph.json`; it is an export.
- **Cheap structural scan first, deep analysis only for chosen targets.** Do not parse bodies or docstrings by default. Reading a file's syntax for facts of its own imports (what loads a module, the names read through a module binding, whether an imported name is written only in types) is part of the structural scan; it records no other fact of bodies. Deeper passes (calls, types, data flow; Phase 4) run on demand for a component, never over the whole repository by default. The first, where a symbol is used (`archmap_scan::symbol_uses`), reads only the defining file and the files of its importers for the one symbol a command asks about; its facts (`SymbolUse`) never enter the graph, `scan` or `graph.json`, so rules, cycles and roll-up never see them. The second, where an environment variable is read and written (`archmap_scan::env_uses`), reads the TS/JS files of the scan whose text names it or the environment (`process.env`, `import.meta.env`, the `process` module), for the one name a command asks about; its facts (`EnvUses`) never enter the graph either.
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

Never add a dependency that points against the arrow. Never make `archmap-core` aware of Cargo, `syn`, files or paths beyond plain strings. An interface depends on `archmap-app` alone: never re-export items of `archmap-scan` or `archmap-core` to it, and give `archmap-app` its own type instead. `check` counts only the crate a `use` loads, so it does not catch such a re-export; `query` shows it, followed to the defining crate.

## Distributed plugin

- `plugins/archmap/` is what users install, for agents that *use* archmap in their own repositories; this file is for agents that develop archmap. `.claude-plugin/marketplace.json` lists it.
- The plugin only calls the `archmap` binary: `.mcp.json` declares `archmap mcp --path ${CLAUDE_PROJECT_DIR:-.}`, and the binary is installed separately. Keep `crates/` vendor-neutral: nothing in them knows about any agent, and the server takes its root from `--path` and each call's `path`, never from an agent's environment variables.
- `archmap` on PATH is a copy from the last `cargo install`, not the working tree. Develop and verify with `cargo run`; before trying a change through the plugin, reinstall with `cargo install --path crates/archmap-cli`.
- `plugins/archmap/skills/archmap/SKILL.md` and the MCP texts (`crates/archmap-mcp/src/text.rs`, with the tools' parameter descriptions) are read before any answer, the MCP texts in every session, so they hold only what no answer can say: how to call archmap, when it helps, and the principles for judging an answer. What a mark, heading or count means is said in the answer (its `Marks` line, headings, `Not traced`) and in `docs/reference/`; a new mark, list or gap goes there, never into these texts, and a gap an answer can name goes into that answer's `Not traced`. Update them when a command, flag or parameter they name changes or a principle changes. They never name a capability archmap lacks or prescribe an order of tools. A test in `archmap-mcp` caps what a client receives, and SKILL.md stays under 4,500 bytes (`wc -c`); raising a cap is the user's decision.
- README is the overview; `docs/reference/` (analyzers, graph, commands, mcp, rules, history, work) is where behavior is documented. A change that alters behavior updates the page that states it in the same change. README's "Support by language" marks and gap notes summarize `analyzers.md`; a change that closes or opens a gap updates both, and one that closes a gap only a README note names updates the note. README's Roadmap table states each phase's status: a change that ships part of a phase updates its row. Facts shared by every language go in `graph.md` or `commands.md`, not under each language.
- Keep the skill a short guide to judging output, not a manual. Its frontmatter follows the [Agent Skills](https://agentskills.io/specification) spec, so the skill directory also works outside Claude Code.
- `plugin.json` omits `version` on purpose so installs follow commits. After editing, run `claude plugin validate .`; it warns about the missing version and must otherwise pass.

## Versions and releases

- One version for the workspace, `version` in `[workspace.package]`; no crate sets its own. A release is a tag `vX.Y.Z` on main. Every untagged build so far reports `0.1.0`, so the first tag is `v0.2.0`.
- While 0.x, a release that breaks the CLI's commands or flags, `check`'s exit codes, the JSON (`SCHEMA_VERSION`), the MCP tools' parameters or `archmap.toml` raises the minor version; any other release raises the patch. Text output is no compatibility surface.
- Mark a commit that breaks one of those with `!` (`feat!:`, `fix!:`) and say in its body what breaks, so a release can name it.
- A release is one commit `chore: release vX.Y.Z` that bumps the version and the tag README's Install names for pinning (`cargo install --git ... --tag vX.Y.Z`), plus the tag on it; `plugin.json` stays without a version. Release notes go on the GitHub release, written from `git log` since the last tag, breaking changes first; no CHANGELOG file. Agents never push the commit or the tag.

## Conventions

- Conventions that only code needs live in `.claude/rules/` and load when you open the files they govern: ids (`ids.md`), what analyzers record and what evidence notes mean (`analysis-conventions.md`), and adding an analyzer (`adding-an-analyzer.md`). Open them yourself before planning a change there.
- A `Module` component sets `parent` to its enclosing component. Containment is a field, not an edge.
- `summary`, `query` and `impact` share `DEFAULT_DEPTH` and roll up the same way. Never let them describe different components.
- A rule selector or declaration that matches no component is a finding, never silently skipped, so a typo cannot disable a rule.
- `deny` sides accept declared names or selectors; `layers` and `allow` accept declared names only. Membership goes to the most specific matching selector.
- External dependency edges point only at required dependencies. An import is not a declaration: an import that maps to no component goes to `unmapped_imports` with its reason, never to an edge, and `check` reports the undeclared ones. Evidence notes record how an import name was resolved.
- Prefer missing a finding to raising a false one: rules end up in CI, and a noisy rule gets switched off. The Python analyzer treats any file or directory name in the project as local code because `sys.path` changes at runtime.
- Bulk-insert edges with `add_edges` / `merge`; `add_edge` is linear per call.
- Paths in evidence are relative to the scanned root with `/` separators.
- Output must be deterministic: sort collections, no timestamps. Evidence and the summary never contain absolute paths.
- Text output is written alike, with `query_text`'s helpers rather than new forms: locations as `file:line` with the marks of `query_text::MARKS` (a new mark gets its entry there, a few words long, and a test fails on a one-word mark the table lacks), each explained in the answer by a `Marks` line before `Not traced` that names only the marks the answer shows; a heading that says how to read what follows when its words alone would not; a capped list counted in its heading (`N, showing M`) or, in `summary`, by an `omitted:` line naming the query for the rest; a closing line that says how to see every entry, naming no flag of one interface; `Not traced` last, when anything applies.
- `SCHEMA_VERSION` in `archmap-core` is bumped on breaking JSON changes.
- Avoid abstractions without a second concrete use. Three similar lines beat one premature trait.
- Structural scanning (line-based, as in `python/source.rs`) is acceptable when it stays behind the analyzer boundary and is covered by tests; swap in a real parser only when a fixture shows the need.
- `scan` writes `<root>/.archmap/graph.<ext>` by default and `summary` prints to stdout; `-o <file>` saves either elsewhere and `-o -` is stdout. `fetch github` writes `<root>/.archmap/github.json` the same way, the only command that reaches the network. Write nothing else into the scanned repository: no `.gitignore`, no config. Do not design features that assume graphs are committed to git.
- Nothing is kept between runs: every command scans again, and the MCP server holds its workspaces in memory only. A synthetic 10,000-file Python repository answers `impact` in under a second, so a cache, an index or a database waits until scans of real repositories take seconds (roadmap Phase 9); whatever it becomes, the commands, their flags and their output stay as they are.

## Navigating this repository

Ask archmap before searching by hand, the way the plugin skill teaches, running the current source with `cargo run -q -p archmap-cli --` (the MCP tools serve the last installed binary):

- `summary .` first; fixtures appear as components too, so check paths.
- Before searching inside a crate, module or file, `query <crate|module|file>` and open the `file:line` it gives.
- Before changing a public item, `impact <file>`.
- In Rust, `use` declarations and module paths in code (noted `path`) are imports, the arguments of macro calls that are expressions included (`vec![Box::new(rust::RustAnalyzer)]`), but a macro whose arguments are no expressions (`json!`, `quote!`) is not read, and calls through an imported name are not imports: `query <symbol>` lists them under `Used at`, apart from method calls through a value (`x.m()`), so search for those before changing a method.
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
