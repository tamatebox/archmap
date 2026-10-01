---
name: archmap
description: Explains how to run archmap, a CLI that maps a repository's components and dependencies, and how to read its output. Use when orienting in an unfamiliar Rust, Python or TypeScript/JavaScript repository, locating where code lives, tracing dependencies, judging what a change could affect, checking a finished change against the rules in an archmap.toml, or reading archmap output (summary, query, impact, check, .archmap/ files).
license: MIT
compatibility: Requires the archmap CLI on PATH.
---

# archmap

archmap maps a repository into components (packages, modules, external dependencies) and the dependencies observed in manifests and imports. Use archmap to replace structural searches (where code lives, what imports what): run the command, read its output, then open the `file:line` it gives. Fall back to ordinary search for what it does not map, such as error messages, configuration values, dynamic loading and relations beyond imports. The code decides what is true; archmap decides where to look.

## Rules

1. Run `archmap summary .` by itself and read it before any search. Do not chain it with `grep` or other commands in one call.
2. When the task names a function, class or file, run `archmap query <name>` before any search for that name (a method as `Class.method` or `Type::method`).
3. Before searching inside a component or a file, run `archmap query` on it. Open the `file:line` it shows instead of grepping for the same symbol.
4. Before changing a public symbol, a signature, a dependency, or code used across components, run `archmap impact` on the symbol, file or component.
5. Search normally only when `query` lacks what you need, `Not mapped` or `dynamic imports` cover the code, a command fails, or a result is surprising.

## Commands

Run from the repository root.

- `archmap --version`: if missing, tell the user and continue without archmap.
- `archmap summary .`: the components, what the map misses (`## Coverage`), and dependencies. A capped list ends in `omitted:`, which says where the rest are: `query` those components rather than rerunning with `--verbose`. `components: 0 shown`: archmap cannot read these languages; continue without it.
- `archmap query <component|directory|file|symbol>`: public symbols with `file:line`, dependencies both ways with example locations (`file:line -> loaded file`; `(local)`: inside a function; `(type)`: types only, which never run; `(test)`: test code), and imports without an edge (`Not mapped`). Give a file by path or as `<component>.<file stem>` (`src/shop/users.py`, `shop.users`), and a directory by path for the component that owns it; a Rust module is its own component, named as `use` writes it (`archmap_core::graph`), and one without submodules is queried as its file; an external dependency goes by its name or its id (`serde`, `ext:cargo:serde`). A TS/JS file is a component too, named by its path from `src/` (from the package directory outside `src/`) with its extension (`lib/money.ts`), and by name or path it is queried as a file; quote names with `(` or `[` (`'app/(public)/page.tsx'`). An import of a stylesheet, image or JSON file shows under the importer's own component. A file query also lists the statements that import it (`Imported by`), and a symbol query the statements that import it by name (`Imported by`) apart from those that import its whole module (`May use`); `(export)` marks a re-export, which passes the name on. An import name that no component carries, such as an extra (`torch`), lists where it is imported without an edge; use the name the code imports, not the distribution name.
- `archmap impact <component|directory|file|symbol>`: components whose production code depends on the target, directly or transitively, and under `tests` those that only test code reaches (re-run these); for a file, `importers` lists the importing statements, including those inside the target's own component, which `direct` leaves out. For a symbol, the first step takes only the statements that import it by name (`importers`) or its whole module (`may_use`).
- `archmap check`: with an `archmap.toml`, after a change; exit 1 lists broken rules with evidence. Cycles count only imports that run: `(local)` ones close cycles, `(type)` ones do not. `signal:` lines are observations, never failures.

Options: lists are capped (`--verbose` shows all, `--format json` all evidence). `--depth N` (default 2; 0 keeps only packages) applies to `summary`, `query` and `impact`; use one value throughout. For another root, `summary <root>`, but `query` and `impact` take `--path <root>`. Do not read the full graph (`archmap scan`, `.archmap/graph.json`); `scan` also writes `.archmap/graph.json` into the repository, while the other commands print to stdout.

## Reading the output

- **Observed, not inferred.** Names are package, directory and module names, not responsibilities; label any role you infer as inference.
- **A missing edge is not a missing dependency.** `## Coverage` and `Not mapped` show what the map misses, and runtime coupling (HTTP, databases, queues, subprocesses) is unseen. `importers: none resolved` does not mean unused; search the code before calling anything unused. In TS/JS, `unresolved` is a path or alias that matches no file, and framework entry files (`page.tsx`, `route.ts`, configs) and scripts started from a command or a config string have no importers by design, nor do the globals of a `script` (a file without imports or exports, counted in Coverage).
- **Everything is rolled up to one depth.** Deeper modules fold into their ancestor (`folded: N`), and on a folded name `query` and `impact` answer for the ancestor (`folded from`); a name of one file (a TS/JS file, a Rust module without submodules) still gets the file's `Imported by` and `importers`. `imports: N` counts statements in production code and `tests: M` those in test code, which `check` rules and cycles leave out; `declared: yes` means a manifest declares the dependency too.
- **Check what a name resolved to.** Names repeat: read the `id:` line (query) or `target` (impact), and retry with the id or a file inside it when a result is surprising. A name that several components share stops with their ids and paths, unless they sit at one path (a directory two languages map); rerun with one of the ids or with `./<path>`.
- **impact is reachability, not a verdict.** Python and Rust are traced file by file. In Python, imports that `Not mapped` lists as `local name` or `dynamic` are missing from `Imported by` and `impact`, so search for a file's module name before calling it unused. In Rust, `use` declarations and module paths in code count, but code inside macro calls (`vec![..]`, `println!(..)`) and unit tests are missed, so search for callers before changing a Rust signature. TS/JS is traced file by file through tsconfig paths, `require`, `import()` and test mocks (`vi.mock`) included, and an import through re-exports also counts for the file that defines the name (`(via <re-export>)` in `query`); a `require` or `import()` of a computed path is only listed as `dynamic`, so search for a file's path before calling it unused. In repositories with barrels, read `direct` in `impact` first: `transitive` widens through every barrel. Listed components may not use what you change; unlisted ones may still depend on it.
- **Components cover everything under the root**, fixtures and vendored code included: check the path before treating one as product code.
