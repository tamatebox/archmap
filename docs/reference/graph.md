# Graph model

The Architecture Graph that every analyzer ([analyzers.md](analyzers.md))
feeds and every command ([commands.md](commands.md)) reads.

## Model

```text
ArchitectureGraph
├── meta:       { analyzers, tool_version, coverage: { language -> { files, read?, scripts? } } }
├── components: { id -> Component { kind: package | module | script | external, language, path, parent?, evidence } }
├── symbols:    { id -> Symbol { kind: function | struct | enum | trait | ..., component, signature, evidence } }
├── edges:      [ Edge { from, to, kind: import | dependency | call | http | database | event | unknown, evidence } ]
├── unmapped_imports: [ UnmappedImport { from, module, reason: undeclared | declared_not_required | local_name | unresolved, provided_by?, evidence } ]
├── dynamic_imports:  [ DynamicImport { from, call, prefix?: { written, path?, first? }, evidence } ]
└── unread_macros:    [ UnreadMacro { from, name, names?, evidence } ]

Evidence { file, line?, note?, target?, scope?: module | local, names?, test?, type_only?, replaces? }
```

`line` is the line a statement or a declaration is written on: a manifest's
declaration of a dependency has one in `Cargo.toml`, `pyproject.toml`, a
requirements file and `package.json` alike, where the dependency's value
starts (the table header for a `[dependencies.serde]` table), unless a table
of a `pyproject.toml` holds a value of a type the format does not allow
there, when that file's declarations have none.

`target` is the repository file a dependency points at and `scope` says
where the statement sits: at module level (`module`) or inside a function
body (`local`). For Python that decides whether it runs when its file loads
or only when the function is called; for Rust it is only where the statement
is written. Roll-up hides which files of a component are involved; `impact`
and the cycle check read `target` and `scope` to recover it. Every analyzer
records both.

`names` says what the statement takes from `target`, as `target` exports
it: a default export goes by the name its declaration in `target` gives
(`default` when `target` declares none or re-exports it), `*` stands for
the whole module (a namespace import, `export *`), and no names with a
`target` means the statement only loads the file (a side-effect import). A
Python statement that binds a module (`import pkg.sub`, `from pkg import
sub`) takes the names its file reads through it (`sub.pay`), and `*` when
the file may take anything of it.
Through re-exports (for Python, a file that binds a name by importing it
from another), evidence noted `<note> via <file>:<line>` (one word,
`import via src/index.ts:2`, `use via src/shapes/mod.rs:2`) names what the
defining file declares; a note that only contains ` via `, as a specifier
written with it does, is no such evidence. Such evidence belongs to an edge
to the component of the defining file, which `query`, `impact` and the cycle
check follow; `deny`, `layers` and `allow` count it toward the component of
the re-export it went through when that re-export is an import of its own
(see [rules.md](rules.md)). A statement noted `export`
passes the names it takes on, as a TS/JS re-export does, and a Python
`from` import that binds only definitions its file lists in `__all__` and
never writes again: `impact` follows a changed file or symbol through such
barrels only to the statements that may take what they pass on. A TS/JS re-export of a package, or of a path that
matches no file, keeps `export` as the first word of its note (`export
react-aria, declared in web/package.json:4`): the file passes on names that
the graph does not list. `exported_as` gives, for a TS/JS statement whose
file passes a taken name on under another, that name's new names (`export
{ formatPrice as price } from` records `{"formatPrice": ["price"]}`), and
`*` the names of a module passed on as a namespace (`export * as money
from`); a name the defining file itself exports under another (`export {
formatPrice as fp }`) gives its importers `via` evidence at that line. A
statement gives one piece of evidence per file it points at and re-export
it goes through, with all of its names. Every analyzer records `names`.

A component's evidence noted `package` names an entry file that runs before
any file of the component, or of a module below it, is loaded (a Python
package's `__init__.py`), unlike one noted `index` (a TS/JS `index.*`):
`impact` reaches such a file from the statements outside the component and
the modules below it that import a file below it, and from the files below
it that import something or define a public name (see
[commands.md](commands.md#impact)). Evidence noted `entry` names a file the
component's dependents load (a Rust library's root, what a TS/JS package's
`package.json` names and its source root's `index.*`), which need not be
among the scanned files (a build output, Node's default `index.js` where a
`package.json` names none): of the files such a
component owns directly, only those its evidence names (its entries, its
manifest) stand for it in `impact`, so a binary, a build script or a
configuration file reaches none of the packages that declare it.

`test` marks a statement in test code, which runs only for tests: for
Python and TS/JS a file named `*.test.*`, `*.spec.*` (Vitest's type tests
`*.test-d.*` and `*.spec-d.*` too), `test_*.py`, `*_test.py`, `tests.py` or
`conftest.py`, or any file below a directory named `test`,
`tests`, `__tests__` or `__mocks__` (not `test` or `tests` below the routes
of a Next.js package, where they are URL segments); for Rust, code under `#[cfg(test)]` or
`#[test]`, and every file of a test, an example or a bench, by the kind of
Cargo target rather than a path. It is on the evidence of edges, imports without an
edge, dynamic imports and the symbols that test code defines (a helper below
`tests/`). Rules (`deny`, `layers`, `allow`), cycles and
signals are about production code and leave it out (an edge counts when
some of its evidence is outside test code, or it has none, as a manifest
dependency), `summary` counts test statements apart, `query` marks them
`(test)`, and `impact` lists the test files that reach a change under
`tests`;
undeclared imports count in test code too.

`type_only` marks a statement that takes types only, so it never runs:
TS/JS `import type`, `export type ... from`, a statement whose names all
carry `type` and, in TypeScript, one whose names can only be types where
they are defined or that the file writes only in types, which the compiler
erases, and a Python import under
`if TYPE_CHECKING:`, which only type checkers enter. A statement that takes
values and types from a file gives one piece of evidence for each; evidence without a
`target` records no names and is one, `type_only` when the statement takes
only types. An edge is a dependency however
it is taken, so `deny`, `layers`, `allow`, `query` and `impact` count every
import, but cycles and signals count only imports that run in production:
an edge closes a cycle only through evidence that is neither `type_only` nor
`test`. Only the TS/JS and Python analyzers set it.

`replaces` marks a test's mock that puts a stand-in in place of `target` for
every module its file's run loads, so that file never runs the target's code
nor what reaches it only through the target (a TS/JS `vi.mock` or
`jest.mock` with a factory that never loads the real module). It is a fact
about the statement, apart from the test path rule, under which a file below
`__mocks__` is test code. Its `names` are those the stand-in gives the
module, which the file still depends on: `impact` follows such a test file
only along ways that pass none of the modules it replaces, apart from one
whose stand-in gives a name the change may alter, and lists the test files
it leaves out. Only the TS/JS analyzer sets it.

A symbol's evidence with a `target` says how the symbol is reached rather
than where it is: a Rust method whose type another file defines carries
evidence noted `impl` that points at that file, with the type's name. A
symbol whose evidence is noted `global` is a declaration that a module adds
to the global scope (TS/JS `declare global { .. }`): code anywhere uses it
without importing its file, so no edge shows who does. Such a declaration
often merges with one of the same name in TypeScript's libraries or in
another file (`interface Window`, `namespace NodeJS`), so the symbol is this
file's part of it, and several files may give a symbol of that name.

An unmapped import is an import that maps to no component, standard-library
imports aside, and `reason` says why. A dynamic import is a call that loads a
module by a name computed at runtime; its `prefix` is the start of the name
that the code writes out, as `written` (`./pages/`, `plugins.`), with the
`path` that every file it can load starts with, relative to the root, where
the analyzer knows it (`src/pages/`, `src/plugins/`), and the files `first`
that loading one runs first (a Python package's `__init__.py` on the way).
An unread macro is a Rust macro call
whose arguments are no code the analyzer reads (`json!({ .. })`), with the
names its `a::b` paths write. All three are observations, never edges:
they mark where a dependency may exist that no edge shows. `query` lists them
and `check` reports the undeclared ones. `meta.coverage` counts the files of
each recognized source language and how many an analyzer read; a language
without `read` has no analyzer. Configuration, data and documentation files
are not counted. A `script` is a file TypeScript reads as a script, without
imports or exports (see [analyzers.md](analyzers.md)): its top-level
declarations are global, so they are its symbols and no edge shows who uses
them. `meta.coverage` counts scripts per language, files that belong to
their package without being a component of their own included. The JSON
carries `schema_version: 4`, and no path of the machine that scanned: two
checkouts of one commit give the same graph wherever they sit, as long as
manifests name the packages (code that no manifest names takes the root
directory's name). `summary` names the root it scans by its directory.

## Ids and merging

Each analyzer produces a `GraphFragment`; the graph merges fragments,
collapses edges that describe the same relationship, and keeps all of their
evidence. External ids carry their ecosystem (`ext:cargo:serde`,
`ext:pypi:requests`), so a Cargo crate and a PyPI distribution of the same
name stay apart. When an analyzer gives a component an id that an earlier
analyzer already gave a component at another path (a Python project named
like a Cargo package elsewhere in the repository), the later component and
every id that starts with `<id>::` are renamed `<id>+<analyzer>`
(`dup+python`), and the scan warns; equal ids at the same path, such as a
`Cargo.toml` and a `pyproject.toml` side by side, stay one component.
Output is deterministic (sorted, no timestamps) so graphs can be diffed.

## Output

JSON output carries evidence on every node and edge and is written to
`<root>/.archmap/graph.json` by default. An edge from archmap's own graph:

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

`archmap scan` writes only its own output file, and `summary` writes a file
only when `-o` names one. Neither adds a `.gitignore` or otherwise decides
whether the output is committed; add `.archmap/` to your repository's ignore
rules if you do not want it tracked. `summary`, `query`, `impact` and
`check` re-scan instead of reading the saved graph, so they are never stale.
