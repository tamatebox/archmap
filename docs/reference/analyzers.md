# Analyzers

What each analyzer reads and what it cannot see. Every analyzer normalizes
into the model in [graph.md](graph.md); how the commands present it is in
[commands.md](commands.md).

## Gaps shared by every analyzer

- Imports of the standard library, undeclared packages, extras and dev
  dependencies produce no edges by design, though `query` lists all but the
  standard library as not mapped (`query <import name>` lists where one
  module is imported) and `check` can report the undeclared ones.

## Rust

- every `Cargo.toml` package becomes a `package` component; `[dependencies]` become `dependency` edges
- path / workspace dependencies resolve to internal packages, others become `ext:cargo:*` components
- following `mod` declarations from `src/lib.rs` and `src/main.rs` (`mod a;` loads `a.rs` or
  `a/mod.rs`), every module file becomes a `module` component named by its path as a `use` writes
  it (`archmap_core::graph`, starting with the `[lib] name` when there is one; a binary's modules
  start with the package name, which no other package has, even where a `[[bin]]` renames it) and
  contained in the component of the file that declares it; an
  inline `mod a { .. }` stays part of its file but keeps its place in the path, and a package's
  library and binary are resolved as the separate crates they are
- the other targets Cargo finds by default, the binaries under `src/bin/` (`src/bin/<n>.rs`,
  `src/bin/<n>/main.rs`), the tests (`tests/<n>.rs`, `tests/<n>/main.rs`), examples and benches
  likewise under `examples/` and `benches/`, and the build script `build.rs`, are crates too, and
  so is what `Cargo.toml` declares: `[lib] path`, `[[bin]]`, `[[test]]`, `[[example]]` and
  `[[bench]]` at their `path` or where Cargo infers it from the name, `package.build` (a path, or
  `false`), and no target of a kind turned off by `autolib`, `autobins`, `autotests`, `autoexamples`
  or `autobenches = false`; a declared target takes the place of a found one of its name or root, as
  from edition 2018, and a `[[bin]]` at `src/main.rs` only renames that binary, whose modules are
  named by their paths too when `[lib] path` puts the library's root outside `src/`, where the same
  module path is another file. Declared paths are read from the package directory, `./` and `../`
  resolved. Their roots belong
  to the package, as `src/lib.rs` does, and a module their `mod`
  declarations load is a `module` component whose id and name are its path in the package
  (`kiosk::tests/common/mod.rs`, named `tests/common/mod.rs`), the same for every test that loads it
  and apart from any module of the library (a binary's `src/bin/tool/util.rs` and the library's
  `src/util.rs`); binaries, tests, examples and benches name the library by its crate name, the
  build script does not
- `pub` items and `pub` inherent methods become symbols with signatures; one that only tests compile
  (`#[cfg(test)]` on it, its `impl` or a module around it, or a file of a test, example or bench)
  carries `test`. In the files of targets other than the library and `src/main.rs`, a symbol's id
  starts with its file's place in the package (`kiosk::tests/total.rs::helper`), so a `pub fn
  helper` in two tests and in the library are three symbols; `pub mod a;` is the symbol of the
  module it loads
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
  brought in is that `use`'s dependency, and when that `use` brings in a module whole (`use
  crate::graph;`) the path is resolved as if the `use` had named it (`graph::build()` as `use
  crate::graph::build`): the item joins the `use`'s names beside `*`, or, re-exported, goes to the file
  that defines it, noted `use via`; a path in test code adds nothing to a `use` outside it, and what a
  module does with the names it imported (calls, references) is not recorded
- import evidence records the names a statement takes from the file it points at (see
  [graph.md](graph.md)): the item it reaches, by the name its defining module gives it (`Invoice`
  through `pub use billing::invoice::Invoice`, the name before any `as`), `*` for a module itself
  (`use crate::graph;`, `self` in `use crate::graph::{self, Edge}`) or a glob of a module, and the
  enum for a glob of an enum (`use crate::model::EdgeKind::*` takes `EdgeKind`); the leaves of one
  `use` that reach one file share one piece of evidence, and path evidence takes the names of all
  paths from its file to its target
- a `pub` method of an inherent `impl` whose type another file of the crate defines has, besides its
  location, evidence noted `impl` that points at the type's file with the type's name, resolved as a
  `use` path from the `impl`'s module
- a re-export from the subtree of the file's own module (`pub use graph::ArchitectureGraph` in
  `lib.rs`, also inside an inline `pub mod prelude { .. }` there) is how the module presents what it
  contains, a relation other than an import: it is followed when resolving and never becomes an
  edge, so a crate root and the modules it re-exports form no cycle
- `use` declarations and paths in `#[cfg(test)]` and `#[test]` code are not dependencies of their
  crate on itself, so unit tests add no edges or cycles between the modules of a crate; a test, an
  example or a bench is a crate of its own, and keeps every import of the library and of its own
  modules, `#[test] fn` bodies included
- a `use` of a `[dev-dependencies]` crate (in a test module, a test, an example or a bench) is an
  import without an edge
- imports and paths in `#[cfg(test)]` and `#[test]` code, and every import of a test, an example
  or a bench, carry `test` in their evidence (see [graph.md](graph.md)): a target's kind is a fact of
  Cargo, as they may use `[dev-dependencies]` and are no part of the library and the binaries; path
  evidence carries it only when every path it stands for is test code
- every file under `src/` is read; outside it, the roots of the other targets and the files their
  `mod` declarations load, so a `.rs` file there that no target loads (test data, trybuild's
  `tests/ui/`) is neither parsed nor warned about, and Coverage counts it among the files not read

### Rust known gaps

- `use` declarations and module paths in code are imports, but code inside macro calls
  (`vec![Box::new(rust::RustAnalyzer)]`, `write!(out, "{}", crate::query_text::shell_word(..))`) is not
  read, and neither is a module's own use of what it re-exports, so `query` and `impact` miss
  those dependents.
- `#[path]` modules belong to their package without a module tree.
- Edition 2015's rule that one declared target of a kind turns off finding the others of that kind
  is not followed, so a file Cargo would not build may be read as a target. A `package.build` list
  reads as no setting, and a manifest with a target table archmap cannot read (a `path` that is no
  string) reads as one that declares no targets, with a warning.
- The crate-relative `use` paths of Rust 2015 resolve only in the crate root.
- A path through a `mod` whose file was not read points at no file; any other name the scan
  cannot place (a module generated by a macro, say) points at the deepest module the path reached.
- A prelude file (`src/prelude.rs`) that re-exports modules which glob-import it forms a cycle
  with them, because only re-exports from a module's own subtree are not edges.
- Unit tests are left out of dependencies within their crate, so `impact` does not list them.
- A module that defines no `pub` item and whose only recorded import is a dev-dependency in its unit
  tests reads as test code, so `impact` on it lists the module itself among the tests to run again.
- Rust components are finer than Python's: a module file rather than a package directory.
- Path evidence keeps one line per file and target, the first path's, whatever names the other
  paths take.

## Python

- `pyproject.toml` (PEP 621 or poetry), `setup.py` / `setup.cfg` directories become `package` components;
  a tree of `.py` files without any manifest gets one root component named after the directory
- `[project] dependencies`, `[tool.poetry.dependencies]` and `requirements*.txt` (or `*-requirements.txt`)
  become `dependency` edges to `ext:pypi:*` components (names normalized per PEP 503); a requirements file
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
- an import of a module runs the `__init__.py` of each package above it first, up to the project; a
  regular package's component records its `__init__.py` with the note `package` (see
  [graph.md](graph.md)), so `impact` on it lists the statements outside the package that import a
  module below it, apart from imports of types only and `via` evidence, and goes on from them and
  from the files below it
- `import` / `from ... import` (including relative imports) become `import` edges between modules,
  or to a declared external dependency
- the evidence of each import names the file it loads (`pkg/sub.py`, otherwise `pkg/__init__.py`)
  and its scope: `local` inside a function body, `module` elsewhere (including under `if`, `try` and
  `class`); imports between files of one component are kept as self edges, which roll-up hides
- an import under `if TYPE_CHECKING:` or `if <module>.TYPE_CHECKING:` (`typing.TYPE_CHECKING`) takes
  types only (`type_only`, see [graph.md](graph.md)), since only type checkers enter that block, so it
  closes no cycle; the `else:` branch runs
- the evidence also records the names the statement takes from that file (see [graph.md](graph.md)):
  an attribute of the statement's module by name (`VERSION` in `from pkg import VERSION`), `*` for a
  submodule (`from pkg import sub`), for `import pkg.sub` and for `from pkg import *`, and nothing for
  a package the statement only passes on the way (the parent `__init__.py` of a subpackage, or what is
  left of a module the scan did not read); when a name list cannot be read whole, the module's own
  file, if the statement points at it, also gets `*`
- a name taken from a file that binds it by importing it from another (`from shop.billing import pay`,
  where `shop/billing/__init__.py` has `from .charge import pay`) also gets evidence for the file that
  defines it, noted `import via <file>:<line>` with the first binding on the way, by the name that
  file gives it (`as` followed); the walk goes through the module-level `from` imports of any file and
  through star imports, by what their sources export (a literal `__all__`, else every name without a
  leading `_`), and when a file it reaches shows nothing more of the name, the evidence points at that
  file; the evidence is `type_only` when the statement or a binding on the way is under
  `if TYPE_CHECKING:`. A `from` import binds a name its own file may use as well as pass on, so it is
  never noted `export`
- a statement keeps only the evidence for the file it loads when a file on the way shows the name in
  ways that lead to more than one definition (its definitions, assignments, `from` imports and star
  imports; only those that run count, when any does), binds it in a way the walk does not follow
  (`import a.b as c`, a module outside the scan, a file next to the importer), may bind it through a
  name list that could not be
  read or a star import of a module outside the scan and shows it no other way, or passes it on from a
  source whose `__all__` is built at runtime, or when the walk meets a cycle or more than 32 bindings
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
- the imports and dynamic imports of test code, by the rule [graph.md](graph.md) gives for Python and
  TS/JS alike, carry `test` in their evidence
- calls to `import_module`, `__import__` and `spec_from_file_location` are recorded as dynamic imports,
  which no edge can follow
- public top-level `def` / `class` / `CONSTANT` and public methods of public classes become symbols
  for files inside a regular package tree; a file outside any regular package tree (in a namespace
  tree such as `scripts/` or a `tests/` without `__init__.py`, or at the top of the project) gives
  only the definitions that imports in other files take from it, and all of its public ones when one
  takes it whole (`import util`, `from util import *`), so its `Public symbols` in `query` are what is
  used from it; test files (`test_*.py`, `*_test.py`, `conftest.py`) give none, while a helper below
  `tests/` does, as for TS/JS; in a signature a parameter's default value reads `…`, since a default
  can hold a secret
- source files are scanned structurally line by line, not parsed; function bodies are read only for imports

### Python known gaps

- Dynamic imports are recorded but not followed, and `sys.path` changes made at runtime are not
  seen.
- An `__init__.py` is no barrel, since it may use what it imports: a statement that takes a package
  whole (`import shop.billing`, then `shop.billing.pay()`) is in neither of `query`'s lists for `pay`
  when `shop/billing/__init__.py` imports it from another file, and `impact` goes on from the
  `__init__.py` file by file, to every importer of it and whatever imports a module below it.
- A file that binds a name twice, such as `from .x import pay` and then `pay = wrap(pay)`, ends the
  walk whichever runs last, so a statement that reaches it keeps only the evidence for the file it
  loads.
- A name bound by a statement the scan does not read as an assignment (`a, b = …`, `for`, `with … as`)
  is unseen, so a star import whose source binds a name that way may lead to another source of it.
- `impact` on an `__init__.py` reaches the files below the package that import something or define
  a public name: one that imports only the standard library, or nothing, and defines no public name is
  not among them.
- A file outside any regular package tree that no import resolves to gives no symbols: a script run
  directly, a helper reached only through a `sys.path` entry added at runtime (pytest's
  `pythonpath`), or a module loaded by name (`pytest_plugins`).
- `from pkg import name` where `pkg/name.py` exists takes that submodule whole, even when
  `pkg/__init__.py` has `from .name import name`, which makes `pkg.name` the object it imports.
- Only `if TYPE_CHECKING:` and `if <module>.TYPE_CHECKING:` mark imports as types only: an import
  in the `else:` of `if not TYPE_CHECKING:`, under a condition that combines `TYPE_CHECKING` with
  others, or under an alias (`if TC:`) or `if MYPY:` counts as running.
- An import written on the line of a compound statement (`if TYPE_CHECKING: import x`,
  `try: import x`) is not read.

## TypeScript and JavaScript

Files `.ts .tsx .mts .cts .js .jsx .mjs .cjs`, `.d.ts` included, parsed with `oxc_parser`:

- a `package.json` with a `name` whose directory holds TS/JS files of its own, that declares `workspaces`,
  or that is a workspace member or a path dependency (a package of JSON or configuration included),
  becomes a `package` component; TS/JS files that no package owns go to one root component named after the
  directory (`<name>+.` with a warning when a package has that name); a `package.json` without a name is no package, but it declares dependencies all the same;
  of packages that share a name, a workspace member (or path dependency) keeps it as its id, else the first by
  path, and the others become `<name>+<directory>` (`dup+examples/dup`), with a warning
- every directory between a package and its code files becomes a `module` component, except the source root
  `src/`, and every code file is a `module` component of its own, named by its path from the source root with
  its extension (`lib/money.ts`, `app/(public)/[slug]/page.tsx`); an `index.*` is its directory's own
  file, and the source root's `index.*` and the files directly in a package directory that has `src/`
  (`next.config.ts`) belong to the package; of several `index.*` files, the one an import of the directory
  loads (`index.ts` before `index.tsx` and `index.js`, code before `index.d.ts`) is the evidence and gives
  the language
- `dependencies` and `peerDependencies` become `dependency` edges to `ext:npm:*` components;
  `devDependencies` and `optionalDependencies` give no edge
- `import` (`import x = require('m')` included) and `export ... from` become `import` edges (noted
  `import` and `export`), resolved with
  `oxc_resolver` through each file's `tsconfig.json` (`paths`, `baseUrl`, `references`) and `.js` written for
  `.ts`; the resolver sees only the scanned files, so `node_modules` and build output never change the graph,
  and an `extends` it cannot load, in a tsconfig or in a config one extends, is dropped with a warning while
  the file's own `paths` still apply
- calls with a written-out specifier (a string, or a template without substitutions) anywhere in a file
  become `import` edges too, noted with the call: `require`, `import()`, and the module calls of Vitest
  and Jest (`vi.mock`, `vi.doMock`, `vi.unmock`, `vi.importActual`, `vi.importMock`, `jest.mock`,
  `jest.doMock`, `jest.unmock`, `jest.requireActual`, `jest.requireMock`); inside a function body
  (`lazy(() => import('./chart'))`) their evidence is `local`; a `require`, or an `import()` awaited, takes
  the names its result is destructured into at once or the property read from it
  (`const { pad, trim: t } = require('./format')`, `const { run } = await import('./job')`,
  `require('./fn').default`), and any other call the whole module (`*`), a destructuring with a rest
  element or a computed key included; `require` and `import()` of a computed specifier are dynamic imports
- an `import()` type (`typeof import('./m')`, `import('./m').Wallet`) is an `import` edge that takes types
  only: `*`, or the first name after it; calls on one line that load one module with one note are one
  statement with the names of all (`import('./m').A | import('./m').B` takes `A` and `B`)
- an import of a stylesheet, image or JSON file is an edge of the importer to itself whose evidence names the
  file, so `impact` on the file lists its importers, or to the package that holds the file when that is
  another package
- a named or default import that reaches a name through re-exports (`export { a } from`, `export *`,
  `export * as ns`, `import { a } from 'm'; export { a }`) also has evidence for the file that defines the
  name, noted `import via <file>:<line>` with the first re-export on the way, so `query` and `impact` on the
  defining file list importers that go through barrels; a name not found, `export *` sources that disagree at any depth, a name that leads outside the scan,
  a cycle or more than 32 hops leave only the loaded file, and namespace and side-effect imports never walk
- every import and re-export statement records the names it takes from the file it loads (see
  [graph.md](graph.md)): the exported name for a named import (`a` for `import { a as b }`) and for
  `export { a } from`, the name the loaded file's default export declares for a default import (`limitOf`
  for `export default function limitOf`, `default` when it declares none or re-exports it), `*` for a
  namespace import, `import x = require()` and
  `export *`, none for a side-effect import; `via` evidence records the names as the defining file
  declares them, one evidence per defining file and re-export, and re-export statements are not walked
- a statement that takes types only is `type_only` (see [graph.md](graph.md)): `import type`,
  `export type ... from`, `export type *`, `import type x = require()` and `import()` types, and a statement whose names
  all carry `type` (`import { type A }`); one that takes values and types from a file
  (`import { a, type B }`) gives one evidence for the values and one for the types, a name taken both
  ways (`import { A, type A as B }`) counting as a value, while an import of a package or one without
  an edge records no names and gives one evidence, `type_only` when every name is a type; `via`
  evidence is `type_only` when the name is imported as a type or a re-export on the way passes it on
  as one (`export type { A } from`)
- the packages an install links by name are linked in the resolver's view as `node_modules/<name>`: the members
  of a workspace (`workspaces` in a `package.json`, an array or `{ "packages": [..] }`, and
  `pnpm-workspace.yaml`; `!` patterns leave members out) in its root's `node_modules`, and the directories of
  `file:`, `link:` and `portal:` dependencies inside the scanned root in the declaring package's, never another
  package of the same name; code reaches the nearest link above it, so two workspaces in one checkout keep
  members of one name apart; a bare import of one resolves to its files through its
  `exports`, or its `main`, `types` or `typings` in that order, matching the conditions the scanned tsconfigs turn on with `customConditions`,
  themselves or through a config they extend (when a
  `types` condition leads outside the scan, the next condition answers, as it does for a package's own
  `imports` (`#util`) and name; a declaration file the scan holds is what the import points at, as tsc
  reads it), one whose
  entry is outside the scan (`dist/`) is an `import` edge to the package without a file, a declaration of one
  is a `dependency` edge to that package whatever its version (`workspace:*`, `^1.0.0`), and a tsconfig
  `extends` of one loads; a `workspace:` version of a name that no member has gives a warning and no
  component, and an import of it is `unresolved`
- a bare specifier that resolves to no file is matched by package name to the closest `package.json` above
  the importing file that declares it, so a monorepo root's dependencies count for its packages, and only
  when none declares the package itself to the closest that declares its `@types` package (an import such
  as `import { Handler } from 'aws-lambda'` takes types without saying so): a required declaration gives an
  edge, another an import without an edge
  (`declared_not_required`, noting where it is declared, `the enclosing package.json:6` above the package's own), a directory at the top of the package or its source
  root, a code file at the top of the source root, or a scope named like a directory there (`components/button`,
  `App`, `@components/button`) `local_name`, and anything else `undeclared` (for an import of types only, the note
  says to declare the package, or its `@types` package if it ships no types); a path or alias that matches no file (`./gone`, `@/x` without a matching `paths` entry,
  `~/x`) is `unresolved`, and so is a bare-looking name that a tsconfig or jsconfig in a directory above the
  importing file declares as an alias, itself or through a config it extends (`@ui/card` for `@ui/*`; a
  catch-all `*` is not taken as one), so another
  package's alias hides no undeclared import; the package's own name, when its entry (`dist/`) is not scanned, is
  `local_name`; Node built-ins (`node:fs`, `fs`, `crypto`) are left out
- exported declarations become symbols with signatures, in which a parameter's default value and the
  arguments of a call in `extends` read `…` (`extends Base(…)`) and decorators are left out, line included:
  functions and arrow functions, classes and their
  public methods as `Class.method`, interfaces, type aliases (with their right-hand side), enums, namespaces
  and constants, a named default by its declared name, and the declaration `export = Engine` names, as the
  default export; a constant without a declared type shows the
  shape of its value, never the value, which may be a secret (`: string` for a literal,
  `: number` for arithmetic of numbers, `= z.object(…)` for a call, `= [… 3 items] as const`, `= {…}`); test, story and mock files (`*.test.*`, `*.spec.*`,
  `*.test-d.*`, `*.spec-d.*`, `*.stories.*`, `__mocks__/`) give imports only, while helpers in `tests/` keep their symbols
- CommonJS exports at the top level of a JavaScript file become symbols too: `exports.pad = ..` and
  `module.exports.pad = ..` as `pad`, each property of `module.exports = { .. }`, and the function,
  class or local declaration that `module.exports` or `exports.default` is, also as the default
  export; a local declaration keeps its own kind, line and signature, as with `export { a as b }`,
  and the `exports.a = void 0` placeholders that compilers write give nothing
- a file TypeScript reads as a script, whose top-level declarations are global, gives every
  top-level declaration as a symbol and is a `script` component when it is a component of its own,
  and `summary` counts scripts in Coverage; a file is a script unless it has an `import` or `export`
  declaration, `import x = require()` or `import.meta`, in JavaScript a `require` call or an
  assignment to `module.exports` or `exports`, the extension `.mjs`, `.mts`, `.cjs` or `.cts`, JSX
  where the tsconfig's `jsx` is `react-jsx` or `react-jsxdev`, `"type": "module"` in the closest
  `package.json` where the tsconfig's `module` is `node16`, `node18`, `node20` or `nodenext`, or a
  tsconfig with `"moduleDetection": "force"` (declaration files stay scripts then)
- the imports of test code carry `test` in their evidence (see [graph.md](graph.md)): `*.test.*` and
  `*.spec.*` files (`*.test-d.*` and `*.spec-d.*` too) and any file below a `test`, `tests`, `__tests__` or `__mocks__` directory, helpers
  included; stories are not test code; below the routes of a package whose own `package.json` declares
  `next` (`app/`, `pages/`, `src/app/`, `src/pages/`), a directory named `test` or `tests` is a URL
  segment (`app/test/page.tsx` is the page `/test`), while test file names, `__tests__` and `__mocks__`
  keep their meaning

### TypeScript and JavaScript known gaps

- Routes are read for Next.js only: in the route directories of Remix (`app/routes/`), SvelteKit
  (`src/routes/`), Nuxt and Astro (`pages/`), a directory named `test` or `tests` is test code by the
  rule above, so its imports carry `test`.
- The names that a `require` or an `import()` takes later (`import('./m').then((m) => m.a)`, a result
  kept in a variable and read afterwards, an assignment that destructures it, `({ a } = require('m'))`)
  and the names a mock replaces are not read: such calls take the whole module, so `query` on a symbol
  lists them under `May use`. A `vi.mock` with a factory, which
  never loads the real module, is an edge all the same, noted `vi.mock`.
- A `require` that a function takes as a parameter (a bundle's module wrapper, AMD's `define`) is
  not Node's and gives nothing, but a committed UMD bundle (`module.exports =
  factory(require('jquery'))`) reads as code that imports `jquery`; list such files in an `.ignore`
  file, which the scan honors as it does `.gitignore`.
- Scope follows where a call is written: a function called where it is defined runs when its file
  loads but is `local`, and a class field initializer runs on construction but is `module`.
- Types in JSDoc comments (`@type {import('./m').Wallet}`), `new URL('./worker.ts',
  import.meta.url)` and `new Worker(..)`, and `import x = require()` inside a namespace are not
  read.
- Only `type` written in the statement marks types: the compiler also drops an import whose names
  are used only as types (`import { Money }` for an interface), which archmap counts as running.
  Under `verbatimModuleSyntax`, `import { type A } from 'm'` still loads `m`, which archmap counts
  as types only.
- A JavaScript file that Node runs is a module of its own even without `require` or exports;
  archmap follows how TypeScript reads it, so such a file is a script, and so is a TypeScript file
  whose only module code is `require`.
- A config that a tsconfig extends from a package is read only when the package is a workspace
  member or a path dependency, by its path in the package (`@acme/tsconfig/base.json`, or
  `@acme/tsconfig/react` for `react.json`), not through `exports` that map it elsewhere; a config of
  any other package is not in the scan.
- Declarations inside `declare global { .. }`, triple-slash directives (`/// <reference types="vite/client" />`),
  spreads in `module.exports = { ...require('./a') }`, `Object.defineProperty(exports, 'a', ..)` and the
  re-exports compilers write into CommonJS output (`__exportStar(require('./a'), exports)`) are not read.
- Files loaded by a pattern (Vite's `import.meta.glob('./pages/*.ts')`, webpack's `require.context`)
  are not read.
- An import of a name that Node also has built in (`events`, `buffer`) is the built-in, as Node reads
  it, and gives nothing, even where `package.json` declares the npm package of that name for a bundler.
- Vue, Svelte and Astro components and GraphQL documents are not code to the analyzer: an import of
  one is an import of a file, as for a stylesheet, and the imports inside them are not read.
- A tsconfig's `customConditions` count for every file, not only those its config covers.
- Aliases defined only in a bundler configuration, `jsconfig.json` and Deno import maps are not
  read: an import through such an alias is `unresolved` when a tsconfig or jsconfig declares its
  pattern, `local name` when it names a top directory of the source root (`@components/button`),
  and `undeclared` otherwise (`@ui/card` defined only in `vite.config.ts`).
