# Commands

What each command prints and how its output stays small. The examples run
on archmap's own repository unless they name another root: any Rust, Python
or TypeScript/JavaScript project or package directory works the same way,
given as the argument of `summary` and `scan` or as `--path` of the others. The graph
they read is described in [graph.md](graph.md); `check` and its rules are in
[rules.md](rules.md). `summary`, `query`, `impact` and `check` are also MCP
tools with the same answers ([mcp.md](mcp.md)).

## Depth and roll-up

archmap rolls the graph up structurally and prints a deterministic,
line-oriented summary to stdout, starting with what the scan could not see.

`summary`, `query` and `impact` share one default depth, so they always
describe the same components. Asking `query` or `impact` about a component
that is folded at that depth answers for the component it is folded into and
says so, and `query` lists the children to ask about with a larger
`--depth`.

## summary

```bash
archmap summary .                 # prints to stdout
archmap summary . --depth 1       # coarser
archmap summary . --verbose       # every component and dependency
archmap summary . -o summary.md   # saves to a file instead
```

`archmap summary` rolls the graph up and prints it to stdout, one fact per
line under a few Markdown headings. Depth counts containment levels below a
package: depth 0 keeps only packages, depth 2 keeps packages and two levels
of modules, and anything deeper is folded into its ancestor. The summary
contains:

- a header of `key: value` lines: `root`, `depth`, `components: N shown, M in
  the full graph` (`N shown of K at depth` when the tree is capped), counts,
  the `source` of the facts and the `next` commands
- `## Coverage`, before the map: for each analyzed language `files`, `read`
  and `imports without an edge`, counted in statements per reason with those
  in test code (`extra or dev dependency 2 (1 in tests)`), each under the
  language of the file that writes it; the languages no analyzer reads, such as
  `not analyzed  sql: 4  notebook: 2`; the number of TS/JS `scripts`, files
  without imports or exports whose declarations are used without an import;
  the number of `dynamic imports` and
  the components that make them; the number of Rust `macro calls not read`,
  when there are any, and the components that make them; and a fixed line
  naming the runtime coupling
  no analyzer reads (HTTP, databases, queues, subprocesses,
  configuration-driven loading)
- the component tree, indented by containment, with kind, language, path,
  `symbols: N` and `folded: N` for submodules folded into the component
- internal dependencies as `a -> b  imports: N`, plus `tests: M` for
  statements in test code and `declared: yes` when a manifest also declares
  the dependency; N and M count distinct `file:line` statements, a pair that
  only tests make shows `tests: M` alone, and imports between files of one
  component are not listed. A statement that reaches a name through a
  re-export counts for the file that defines the name, and for the barrel it
  loads unless re-exports lead every name it takes elsewhere: it counts for
  the barrel when it takes the barrel whole, a name the barrel declares (an
  anonymous default included), or a name that leads outside the scan or
  nowhere. Statements of a component's own entry file (an
  `index.*`, an `__init__.py`) into its own submodules say what it holds
  rather than what it depends on: they are counted in a `not listed:` line
- external dependencies with the manifests that declare them (`declared:`,
  the first 3 and a count of the rest),
  the number of components whose production code imports them
  (`importers:`), the top importers, and `test importers: K` for components
  that import them only in test code
- the components depended on by the most others in production code, with
  `dependents`, `dependencies` and `rank`

The summary is an index for choosing what to `query` next, so it stays
small however large the repository is. A list shows a component by its name,
or by its id where several components share the name (`types.ts` in each
package of a monorepo), so that every entry can be queried as written. The component tree lists the
packages first and then the modules with the most dependents plus
dependencies, each only when it fits with its ancestors, up to 30 lines.
Internal dependencies keep 30: those between packages first, then those
with more import statements in production code, then in tests, so the
heavy flows show rather than one-statement lines into a popular target.
External dependencies keep the 20 imported by the most components. A capped list ends in an `omitted:` line that counts the rest,
says where they are and names the query that shows them:

```text
omitted: 12 modules  in: shop.billing 7, shop 5  next: query <component>
```

The whole summary then aims at 8 KiB: over it, the largest list gives up
its lowest-ranked entries, down to 10 each. The header, coverage, `omitted:`
lines and the most depended on list are never trimmed, so very long names
can exceed the target, but the size does not grow with the repository.
`--verbose` lists everything. On archmap's own repository (130 source files,
fixtures included), depth 2 turns a 364 KB graph into a summary of about
8 KB (28 KB with `--verbose`).

## scan

```bash
archmap scan .                    # writes ./.archmap/graph.json
archmap scan . -o graph.json      # explicit file
archmap scan . -o - | jq .edges   # stdout
archmap scan . --manifests-only
archmap scan ../some-python-repo
```

`scan` writes the full graph for tools, export and debugging; the other
commands re-scan and never read it. Where it writes and what it leaves
alone is in [graph.md](graph.md#output).

Every command reads every file below the root except hidden ones, those
that `.gitignore` or `.ignore` leave out, `.git`, `node_modules` and
`__pycache__`, and build output: `dist/` and `build/` beside a
`package.json`, `pyproject.toml`, `setup.py` or `setup.cfg`, `build/` and
`target/` beside a `pom.xml`, `build.gradle(.kts)` or `build.sbt`, and
`target/` beside a `Cargo.toml`, unless it holds an `__init__.py`. A
directory of those names anywhere else, such as a package named `build`,
is read.

## query

```bash
archmap query archmap-core               # compact text, capped lists
archmap query archmap-core --verbose     # every symbol and location
archmap query archmap-core --format json # complete, with all evidence
archmap query scan                       # by symbol name
archmap query shop --depth 3 --path ../some-python-repo
archmap query shop.billing --path ../some-python-repo                      # by dotted name
archmap query src/shop/users.py --path ../some-python-repo                 # one file
archmap query shop.users --path ../some-python-repo                        # the same file
archmap query src/lib/money.ts --path fixtures/simple-ts-project
archmap query lib/money.ts --path fixtures/simple-ts-project               # the same file by name
archmap query src/lib/money.ts --by-symbol --path fixtures/simple-ts-project
archmap query '#123'                     # an issue or pull request of the work snapshot
archmap query acme/shop#123 --snapshot ../shop-github.json
```

`query` works on top of the rolled-up graph, for a component, a symbol or a
single file, including the imports no edge shows. For a name taken from a
package it lists the statements that take it, under `Imported by` by the
import name each writes (`(from next/cache; 39 in tests)`), those that take that module
whole under `May use`, and `Used at` as for a symbol, read from their files;
a statement that passes the name on (`export { x } from 'pkg'`) is marked
`(export)`, and `Not traced` says its file's importers are not read
(`relays`). `impact` starts from the same statements, as from those of an
import name.

With `--by-symbol` (MCP `by_symbol`), `query` on a file lists each public
symbol of it, in source order, with how many statements take it by name
(`imported by 2, 1 in tests`), how many take its file whole (`may use 1`)
and where it is used, read as for `query <symbol>` (`used at 4 in 3 files,
1 in tests, 1 in this file`), or `none found`; a capped list says how many
of the rest are `none found`, and a method that takes a value says its
calls through a value are not read. It runs the uses pass for every symbol,
on up to 8 threads, and takes no `--snapshot`. JSON gives each symbol with
its statements and `used_at`.

For an environment variable (`env:APP_REGION`, or a name in capitals that
nothing else has) `query` reads on demand the TS/JS files the scan read
whose text names it or `process.env`, parsed, so a string or a comment that
names it counts for nothing: `Read at` lists `process.env.APP_REGION`,
`process.env["APP_REGION"]`, a destructuring of `process.env` that takes it
and `import.meta.env.APP_REGION`, through `process` imported from Node and
a binding of the environment too (`import { env } from 'node:process'`,
`const { env } = process`, `const env = process.env`), and `Written at` an
assignment, `delete` and a test's `vi.stubEnv`. `Not traced` names reads by
a computed key (`process.env[key]`) and of the environment whole (spread,
passed), which may read it, that where its value is set is not read (`.env`
files, deployment settings, a framework's config), the forms not read
(`globalThis.process.env`, `Bun.env`, `Deno.env.get()`, a helper that wraps
the environment), and the languages of the scan whose reads are not read. JSON gives `reads`, `writes`, `computed`, `whole`
and `files_read`. `impact` starts from the files that read it, as from the
statements of an import name, under `Read at`. It also takes a directory
for the component that owns it, a package subpath for its package with
the statements that import that subpath (`react-dom/client`), and an
import name that no component carries (`torch` declared as an extra) for
the imports of it that no edge shows. A component
that is one file (a TS/JS file, a Rust module without submodules) answers as
that file, with the statements that import it, even where it folds into an
ancestor. How a target is looked up, and what happens when it names several
things, is the same for `query` and `impact`: see
[How a target is found](#how-a-target-is-found).

An issue or a pull request, `'#123'` (quoted: after a space, a shell reads
`#` as the start of a comment) or `owner/name#123`, answers from the work
snapshot, `.archmap/github.json` under the root unless `--snapshot` names
another (see [work.md](work.md)): its state, the links GitHub records for it
by type and by the end it is, a pull request's commits matched to the local
git history by SHA, or why one is not (`no local commit with the same sha`,
`an ancestor of HEAD beyond the history read`, `in the repository, not in
HEAD's history`), and in `Not traced` the links its end cannot see. An item
outside the snapshot's range says so with the links that name it; another
repository's item, `not in this snapshot (it holds acme/shop)`; without a
snapshot, where it looked. A file of the same name is read as the file.

`query` prints compact text by default: public symbols with their location
in source order (by file, then line; JSON keeps them by id),
and each neighboring component with its import count and a few example
locations. A location names the file the statement loads when archmap knows
it, as in `src/shop/billing/charge.py:5 -> src/shop/users.py`, and ends in
`(local)` when the import sits inside a function body and so runs only when
the function is called; the others run when their file loads. In a file's
`Imports` and `Imported by`, a package's `Used by`, and in `impact`'s
`Imported by` for a target that is no symbol or name taken from a package,
a location says which names the statement takes, as its
evidence records them, in their order ignoring case: `(names formatPrice,
Money, +2 more)`, three at most and every one with `--verbose`, or `(whole
module)` for a namespace import, a module bound whole or `export *`; a
neighbor with more than one such statement lists them one to a line below
it. `(type)` marks
a statement that takes types only, so it never runs (a TS/JS `import type`,
which the compiler erases, or a Python import under `if TYPE_CHECKING:`),
`(server reference)` a statement in a `"use client"` file that takes server
functions from a `"use server"` file, which calls the server and loads none
of its code, and `(test)` a statement in test code. A file's query says its
React directive under its head (`directive: "use server"`; JSON
`directive`). An answer that shows marks says
what they mean in a `Marks` line before `Not traced`, naming only the marks
it shows, always in the same order (`Marks: (type) types only, never runs;
(test) in test code`); `impact` and `check` do the same. Each
neighbor counts its statements as `summary` counts the pair: in production
code and in tests apart (`2 imports, 1 in tests`), and apart from those
statements of the component's entry file into its own submodules (`6 of its
entry file`) and those that load a barrel only to reach names it re-exports,
every name they take (`3 through re-exports`, located `(through)`), which
count for the files that define the names. Production code is listed first, and neighbors with
more production statements come first. A `Not mapped`
section then lists the imports of the component that no edge shows, one line
per module with the reason (`local name`, `extra or dev dependency`,
`undeclared`, or `dynamic` for a call that loads modules by name) and where
they are, so an absent edge is never mistaken for an absent dependency.
Lists are capped at 30 entries and 3 locations, and the rest is counted. On
archmap's own repository, its busiest component (`archmap_core::graph`)
takes 7 KB as text and 24 KB as JSON. `--verbose` lifts the caps and
`--format json` adds every piece of evidence.

`query` also takes a single file, by path (`src/shop/users.py`, relative to
the root or absolute; a path outside the root is an error) or as
`<component>.<file stem>` (`shop.users`), and answers with the file-level
facts behind its component: the file's public symbols, what it imports
(`Imports`), the statements elsewhere that import it (`Imported by`), and its
imports without an edge. Where no evidence names imported files for the
file's language, `Imported by` says it is unknown rather than showing none,
and for a script, whose declarations are global, it says that what uses them
is not traced; `query` on a symbol a script declares says the same, and so
does `query` on a symbol declared in a module's `declare global`.
A TS/JS re-export statement (`export ... from`) is marked `(export)`
wherever a location is shown: it passes names on rather than uses them. A
test's mock that replaces a module for the test's whole run (a `vi.mock`
with a factory that never loads the real module) is marked `(mock)`.
For a Python package's `__init__.py`, which runs before any module below it
is loaded, a line counts the statements outside the package that import a
module below it and names the `impact` that lists them (`Imports below: 5
(they run it first): ...`); JSON lists them as `imports_below`. For a
Rust file that holds methods of a type another file defines, `Take the type
of its methods` lists the statements that take the type from that file,
which may call the methods, as `Imported by` lists its importers; JSON lists
them as `method_takers`.

`query` on a symbol (by name, `Class.method` / `Type::method`, or by id)
lists the statements that import it, from the names their evidence records
(see [graph.md](graph.md)). `Imported by` lists the statements that take the
symbol's name from the file that defines it; a method goes by its type's
name, and a Rust method whose type another file defines, by that file. `May
use` lists, apart from those, the statements that take that file whole (a
namespace import, a glob, `import pkg.sub`). Through a TS/JS barrel that
passes the name on (a statement noted `export` that takes the name or the
file whole, and so on up a chain of barrels), both lists also hold what
takes the barrel: the name it exports the symbol under, where the walk
through the barrel found no definition, under `Imported by`, the barrel
whole under `May use`, each with the barrel it went through
(`src/app/checkout.ts:2 (whole src/index.ts, which passes it on)`). A
barrel that renames the name (`export { formatPrice as price } from`) is
followed by the new name, and a namespace it exports (`export * as money
from`) by what takes that namespace, under `May use`; a name the defining
file exports under another (`export { formatPrice as fp }`) counts as its
own, its importers going `(via src/money.ts:5)`. Statements that only load
the file take no name and are in neither list. Both lists show 5 statements,
production code first, then by place, and count the rest, and their heading
counts the re-export statements among them
(`Imported by: 4 (2 re-exports)`); nothing found reads `none resolved`, with
a reminder that only import statements are read, so it does not mean
unused: an entry point that a framework or runtime loads by name or path,
such as a route or a handler, shows the same. When several symbols match,
they are candidates, each counting its importers (`imported by 3, may use
1`), and querying one by its id lists them. A
Rust module (`pub mod invoice;`) is also a symbol of the file that declares
it, but its imports name its own file, so `query` points at its component
instead, and `impact` answers for that component. What the lists miss:

- A Python `__init__.py` that imports a name without listing it in
  `__all__` may use it as well as pass it on, so it is no barrel, and
  `impact` goes on from it to whatever imports it or a module below it. A
  statement that binds the package (`import pkg`, then `pkg.pay()`) takes
  `pay` from it, and through its `via` evidence from the file that defines
  it.
- Rust path evidence shows the first path from its file to the target, not
  always one that names the symbol.
- Code that runs when a file loads (a side-effect import) is listed for
  neither.

For a symbol, `query` also lists where code uses it, under `Used at`. The uses are read when the query asks, from the file
that defines the symbol and the files of the statements the two lists
name, and never enter the graph, `scan` or `graph.json`. A use is an
identifier that the language's scoping, within its file, resolves to the
symbol: through the binding an import statement makes for it (an alias, a
default import, `import x = require()`, a CommonJS `require`, an `import()`
or `vi.importActual()` awaited, and a destructuring of a module bound
whole), through a static member of a module bound whole (`m.formatPrice`,
`m['formatPrice']`, `(m as any).formatPrice`), through the names a barrel
or the defining file passes the symbol on as, renamed or not, and the
namespaces a barrel passes on (`money.formatPrice`), or to the symbol's own
declaration in the file that defines it. For a class member, it is an
access through the class (`Wallet.open()`) or `this.m` inside the class's
own members of the same kind, static or not, and the arrow functions in
them. A type query of what holds the symbol, its module or its class
(`typeof m`, `keyof typeof m`, `typeof import('./money')`), is a use as a
type too, since that type holds the type of every export or member. Names
in comments and strings are no uses, nor is an export specifier, which
passes a name on. Each use is marked by what it does:
`(call)`, `(new)`, `(jsx)` for a JSX element's name, `(type)`, or `(read)`
for any other (passed as a value, assigned, compared), then `(test)`; it
ends in `as <name>` when the code names it otherwise. The heading counts the
uses in test code apart (`5 in tests`, or `all in tests`):

```text
Used at: 24 in 16 files, 5 in tests, showing 10 (20 calls, 4 types)
  src/app.ts:11 (call) as fp, src/app.ts:11:20 (call) as m.formatPrice, src/app.ts:11:38 (call) as m.formatPrice, +1 more in this file
  src/view.tsx:9 (call) as money.formatPrice, src/view.tsx:10 (call), src/view.tsx:11 (call) as all.money.formatPrice
  ...
  never used (1 import): src/unused.ts:1
  never named (1 import of the whole module): tests/actual.test.ts:2
  mocked (2 places, keys of tests' mock factories, no use): tests/mocked.test.ts:4 (test), tests/partial.test.ts:5 (test)
```

There is a line per file, production code first, then the files with the
most uses, then by path; 10 files and 3 locations per file are shown, and
the heading counts every use, file and role. Two uses that would read
alike give their column (`src/app.ts:11:20`), counted in characters from 1;
editors that count UTF-16 code units differ on characters outside the
Basic Multilingual Plane. `never used` lists the import statements whose
binding of the symbol nothing uses, and `never named` those that take the
module whole and never name the symbol. `mocked` lists where a test's mock
stands in for the symbol, which is no use but a place to edit when it is
renamed or removed: the key that names it, or its class for a member, in
the object a `vi.mock`, `vi.doMock`, `jest.mock` or `jest.doMock` factory
returns (`formatPrice: vi.fn()`), beside a spread of the real module or
not, `as` the key when it names the symbol otherwise (`as Wallet` for
`Wallet.pay`), and the mock call itself when a mock that replaces the
module gives keys that cannot be read. A method that is not static gets
only the uses through its class and `this` (`Used at: through the class and
this only: ...`), and `Not traced` says that calls through a value of its
type (`wallet.pay()`) are not read (`values`), with the imports of the type
or its module that may make them; those are never `never used`. A symbol
that a script or a module's `declare global` declares gets only the uses in
its own file (`Used at: in its own file only: ...`), since code anywhere
uses it without an import, there through `globalThis`, `window` or `self`
too (`as globalThis.registry`). For any
member, `Not traced` names the places that extend its class
(`subclasses`): a subclass reaches its members, statics included
(`Rich.open()`, `super.open()`), and the pass does not read those calls, so
a file that extends the class is never `never used` either. `Not traced` also names the
places that use the symbol's module as a value (`whole module`: passed as an
argument, `ns[key]`, the promise of an `import()` not awaited), which may use
it unseen; and the statements whose
uses were not read, with why (`uses`): two statements on one line that load
different files (`ambiguous statement`), a line that no longer holds the
statement because the file changed since the scan (`statement not found`),
a module that offers no path to the symbol the pass can follow, a parse
error, or a file gone. `--format json` gives everything under `used_at`:
every use with its column, its role, the name it goes by and the import
statement it goes through (`uses`), and the statements that end otherwise:
`unused`, `escapes`, `renamed` (passed on under another name), `passed_on`
(only re-exported), `values` (a
member's class bound, which values or subclasses may reach it through),
`subclasses`, `mocked` and `unread`.
In Rust, each path in code is resolved where it is written, in the
module that encloses it, by the resolver the scan built (inline modules,
`use` declarations and globs of blocks and modules, and re-exports, as the
scan resolves them), so a call through a name a re-export gives it
(`make()` for `pub use graph::build as make`), through a glob, and
`crate::`, `super::`, full and `<Type>::` paths count, each going through
the `use` declaration that brings its first name in. A parameter or a
pattern's binding hides the name only where the language binds it (an
`if let`, `while let`, match arm or `for` binding in its branch, a `let`
binding after the statement), and so do a block's own items and a
function's or `impl`'s generic parameters; a constant or a struct named
alone in a pattern is a use. The file that defines the symbol and every
file of its crate are read as well as the importers, since unit tests
import nothing from their own crate, and only those that hold one of its
names are parsed. Inside an `impl` of a type, its own or a trait's,
`Self { .. }` and `Self(..)` are uses of the type. A method taking `self`
gets the uses through its type and `self` (`Used at: through the type and
self only: ...`): `self.m()`, `Self::m()`, `Type::m()`. A file whose text
changed since the scan is not read (`changed since the scan`).
In Python, the files are parsed when the query asks, with Ruff's parser,
and each name is resolved by Python's scope rules: a name bound anywhere in
a function is local to all of it unless declared `global` or `nonlocal`, a
class body's names are not seen by its methods, comprehensions, lambdas
and type parameters have scopes of their own, and defaults, annotations,
decorators and a comprehension's first iterable belong to the scope around
them. A use goes through the name a statement binds (`from charge import
pay as settle`), a module bound whole (`charge.pay`,
`store.billing.charge.pay`, `from store.billing import charge`), the name a
package's `__init__.py` passes the symbol on as, renamed or not, a star
import of a module whose `__all__` lists it (or that has none), or the
symbol's own definition. A method gets the uses through its class and the
first parameter of its class's other methods, `self` or `cls`, apart from
a static method's (`Used at: through the class and self only: ...`);
calling a class is `(new)`; an annotation is `(type)`, a string annotation
(`w: "Wallet"`) too, apart from the strings of `Literal[...]`, and the
fields of an f-string are code. Where nothing tells which binding code
reads, a statement's otherwise unused binding is no negative fact but
`unread`: a name its scope binds again (an import and a later `def` of the
name, `global pay` and an assignment, an import in `try` and another
binding in `except ImportError`: `name bound again`), a file with a syntax
error, whose code around it is still read (`parse error`), a file that may
reach names by computed ones (a call of the builtin `globals()`,
`locals()`, `vars()` without arguments, `eval` or `exec`, not a method of
that name, or `sys.modules`) or a star of a module whose `__all__` code
builds (`names reached dynamically`), and a file over 4 MB (`file too
large`). A name a
module-level import binds in a package's `__init__.py`, or that a
module's literal `__all__` lists, is offered to whoever imports the
module, and so is a name a module-level star import binds, in a module
that some statement imports and whose star import takes the name too, so
its statement is `passed_on`, never `never used`. A module bound
whole that the code passes as a value or reads a dunder of
(`charge.__dict__`) is a `whole module` place in `Not traced`, and a string
that names the symbol by its module's dotted path
(`mock.patch("store.billing.charge.pay")`, or one ending in `charge.pay`)
a `strings` place, which code may look up.

## impact

```bash
archmap impact archmap-core
archmap impact crates/archmap-scan/src/lib.rs
archmap impact src/shop/users.py --path ../some-python-repo
archmap impact formatPrice --path fixtures/simple-ts-project              # a symbol
archmap impact src/lib/types.ts --path fixtures/simple-ts-project --format json
```

`impact` follows imports file by file where the evidence names the imported
file: a component is affected only when one of its files imports what
changed, directly or through other files, not merely because it imports some
file of the same component. A file target starts from that file; a component
target starts from all of its files. Like `query`, it takes a file as
`<component>.<file stem>` too, a directory for the component that owns
it and a package subpath for its package, starting from the statements
that import that subpath as from those of an import name, and a component
that is one file answers as that file. Dependencies without a target file
(manifests, external packages) are followed component by component, and the
result is still reported at the roll-up depth. A Rust module file that
several crates compile (a test's `tests/common/mod.rs`, a module a library
and its binary both declare) leads on only within the crates the walk
reached it in: a `crate::` path inside it names each crate's own root, so
reached through one test it leads to no other test that declares it, and
reached only inside a binary it leads to no crate that imports the library;
once it is reached in a crate, every way out in that crate is followed. A
file reached through production code stands for its component there; one reached through test
code alone (a test, or a Rust file through its unit tests) does not, since no
dependent loads it that way: a test that a package owns reaches no manifest
that declares the package. A package that names the files its dependents
load (a Rust package by its library's root, a TS/JS package by what its
`package.json` names and its source root's `index.*`) is stood for by those
and its manifest only, so a binary, a build script or a configuration file
reaches none of the packages that declare it; one that names none (a Python
package, a Rust package without a library) still is by every file it owns.
A TS/JS file that only passes a changed file's names on (a barrel, or a
module that re-exports one of them beside its own code, `export { getUrl }
from './url'`) leads on only through the statements that may take those
names: those that take it whole or only load it, that take a name it
re-exports from the changed file, that a walk through re-exports led
through the changed file, or that take a name the walk could not place and
the changed file may export (any name, when it re-exports a package). A
statement that takes the barrel's own names, or names defined elsewhere,
does not, although loading the barrel runs the changed file too; one that
takes a name the changed file defines points at it through its `via`
evidence anyway. A barrel that also imports the changed file for its own
use, or that code depending on the change imports, leads on through every
statement that loads it. A Python package's `__init__.py` that only passes
names on (its `from` import noted `export`, see
[analyzers.md](analyzers.md#python)) is such a barrel too, and so, for a
symbol, is one whose statement takes the symbol's name and that the uses
pass finds passing it on with no use in its file; past it the
reach follows names only: not the imports of a module below the package,
which run the `__init__.py` first. Nor are they followed past one reached
only through what it re-exports from a file that did not change, since none
of its own code is affected. `Not traced` names the files the reach went on
from by names only, and never from as files, for any target, each at its
re-export on the nearest way: of the target, else of the nearest file the
reach came from (`barrels: 1 file passes
on what may change, and only what takes it from there is followed; a
rename, a removal or an error on load also breaks whatever else loads that
file:
src/shop/__init__.py:2 (runs first; 2 test files that load it or a module
below it are not listed)`), with how many more re-exports of each are on a
way (`+2 more re-exports on the way`), which the JSON lists as `lines`, and
the test files that load each, and for a package's entry file a module
below it, that the tests to run again leave out, apart from those that load
it for types only or replace it with a mock. For a symbol, the
re-exports that pass on its whole module (`export *`) count too. A test file whose mock replaces a module for its
whole run (marked `(mock)`) reaches the change only along a way that passes
none of the modules it replaces, since every module its run loads gets the
mock in their place: it is left out of the tests to run again when every way
passes one, and the section ends by counting such files at their mocks
(`left out: 4 test files reach it only through modules their mocks
replace: tests/replaced.test.ts:4 (mocks src/orders.ts), ...`). A mock that
gives a module a name the change may alter does not hide it, as the test
depends on that name: one that a changed file exports, one a barrel passes
on from such a file, the symbol's name, a default by the name the module
declares for it. Nor does the mock of a module from which the test itself
takes such a name by name, or of a barrel on the way of such a statement:
the test type-checks against the real modules, so a rename or a new
signature breaks it. A way through re-exports passes every barrel on it, so
a mock of any of them hides the change from what goes through it. A test that
another test imports, and a mock in a setup file, are not followed this way.
An `export * as ns` reads like `export *` there,
and a named re-export of a package like one of all its names, so such a
barrel leads on to what takes any name it may pass on. It does not follow the arguments of a Rust macro call
that are no expressions (`json!`), and a path that
names no component or file is an error. Direct and
transitive dependents follow production code; the tests to run again are the
files that reach the target only through test code, and a changed
component's own test files, those beside production code included. A file
that production code reaches is not repeated there: its unit tests run with
its package. For Rust, a crate's own unit tests are not recorded, while
tests, examples and benches, crates of their own, are listed. In
`fixtures/mixed-utils-project`, `app.utils` and `app.core` depend on each
other, so following components a change anywhere in `app.utils` reaches
`app.core` and `app.models`; following files, `app/utils/log.py` reaches
both and `app/utils/registry.py` reaches neither.

`impact` prints compact text by default, written the way `query` writes its
answers (here without `Changed in the same commits`, whose files depend on
the history of the repository the fixture sits in):

```text
src/lib/types.ts (file) in lib/types.ts (module, typescript), depth 2
id: ts-shop::src/lib/types.ts

Direct dependents: 4
  app/checkout.ts  2 imports
  app/page.tsx  1 import
  lib/money.ts  1 import
  ts-shop (src/index.ts)  1 import

Imported by: 6, showing 5 (1 re-export)
  src/app/checkout.ts:8 (via src/index.ts:8) (names Money) (type)
  src/app/checkout.ts:9 (via src/index.ts:8) (names Money) (type)
  src/app/page.tsx:2 (names Money) (type)
  src/index.ts:8 (names Money) (export) (type)  in ts-shop
  src/lib/money.ts:4 (names Money) (type)
  1 more in: tests/money.test.ts 1

Transitive dependents: 2 more (6 in all)
  app/lazy.tsx  2 steps, through src/lib/money.ts
  scripts/report.cjs  2 steps, through src/lib/money.ts

Tests to run again: 1
  tests/money.test.ts (mocks it)
  not tests: tests/helpers.ts (helper, for 1 test listed)

Marks: (via file:line) reached through that re-export; (names a, b) the names it takes; (export) a re-export, passes names on; (type) types only, never runs

Not traced (what this answer may miss):
  dynamic: 1 call loads a module by a computed name, which may be this: scripts/report.cjs:4
  barrels: 1 file passes on what may change, and only what takes it from there is followed; a rename, a removal or an error on load also breaks whatever else loads that file: src/index.ts:8 (+2 more re-exports on the way)

Lists are capped; verbose lists every entry, and JSON every entry with all evidence.
```

The first lines name the target as `query` names it: a component, a file
with the component that holds it, a symbol's line with its component and its
id, or an import name. `Direct dependents` are the components with a file
that imports the target, and for a Python package's `__init__.py` also those
with a file below it, which needs it run. The target's own component is
never one of them: when none is left, the heading says whether the target's
importers are all inside its own component or all in test code. `Imported by` lists the
statements that import a file, or a component that is one file, and for a
symbol those that take its name, with `May use` for those that take its file
whole. A Rust file that holds methods of a type another file defines is
reached through the type: the statements that take the type from that file
count as importing it, marked `(takes Calc, whose methods the target
holds)`, since they may call the methods. One statement per line,
production code first, located and marked as `query` marks them, and
followed by the component it is in unless that component is the file
itself. They include the statements inside the
target's own component. For a symbol, `Used at` follows, as `query` gives
it for Rust, Python and TS/JS, the uses in the file that defines it
included, which no import list shows, and the first step leaves out the statements that take the symbol's
file whole and that the uses pass read and found never naming it: they
take nothing of it, so neither they nor what only they lead to are
reached, and `Used at` lists them as `never named (1 import of the whole
module, left out of the reach)`. One whose module the code uses as a
value, or whose uses were not read, stays, as does one in a file that uses
the symbol through another binding (a Rust inline module's `use super::*`)
or holds macro calls the scan does not read, and so does a statement that
takes the symbol by name without a use, which loads the file all the
same; `--format json` gives `used_at` as `query` does and the statements
left out as `unnamed`. For a Python package's `__init__.py`, which runs
before any module below it is loaded, `Imports below` lists in the same way
the statements outside the package that import a module below it
(`Imports below: 5 (they run it first)`), and their components are direct
dependents; imports of types only, which never run, and evidence that a walk
through re-exports led to are not among them. Every later step goes on from
such a file the same way, and from the files below the package, so a change
to a file that an `__init__.py` imports reaches whatever imports a module
below that package, and a top-level package's `__init__.py` can reach most
of the repository. A whole component (a package, a directory, an
external dependency) gets no statement list: the answer names the `query`
that shows where it is imported. `Transitive dependents` are the
components reached only through others, so the direct ones are not repeated,
and the heading counts everything reached. Each test to run again says
how it reaches the target: a statement of it takes the target (`(takes
it)`: imports the file, or takes the symbol by name, with `via
<file>:<line>` when it takes the name through a re-export), takes the
symbol's module whole (`(takes its module whole)`), loads a module below a
package whose entry file the change reaches and that runs it first (`(runs
first: src/shop/__init__.py)`), or reaches through other files, the first
one on the way (`(through src/app.py)`). Of its ways it names the nearest
that runs what changed, every statement on it taking values, in the walk
that its mocks leave, and of ways as near the first named here. A test
whose statements toward the target (for a symbol, those that take it) are
all calls that put a mock in its place reads `(mocks it)`, and a way through another file that only such
calls load ends `, by its mock` (`(through src/orders.ts, by its mock)`).
`types only` follows when no way runs what changed: running the test runs
none of the change, though its type checks may break. A test file that is
the target is marked `(the target itself)`, and one of a changed component
`(in the target)`. A package's manifest (`Cargo.toml`, `package.json`,
`pyproject.toml`), which holds no code, changes how every file of its
package builds, so `impact` on it reaches as far as the package does, and
the package's tests read `(in its package)`. The list holds what a test runner runs by its defaults:
pytest's `test_*.py` and `*_test.py` and the `tests.py` of Django and
unittest (not unittest's wider `test*.py`), TS/JS `*.test.*` and `*.spec.*`
files and those below `__tests__` (Jest's default; Vitest collects only
the named ones, so a helper there reads as a test), and Rust tests' root
files.
A reached `conftest.py` reads as its directory, whose tests and those
below pytest loads it for (`tests/unit/ (conftest.py: pytest loads it for
every test below)`). The rest of the test code it reaches ends the
section, not counted (`not tests: tests/unit/factories.py (helper, for 2
tests listed), examples/demo.rs (example)`): a helper, other test code
below `tests/`, `test/` or `__mocks__` and a module of a Rust test, with
the listed tests that import it; and a Rust example or bench. A runner's
own configuration (pytest's `python_files`, Jest's `testMatch`, Vitest's
`include`) is not read. `Changed in the
same commits (history, not imports)` follows, from the root's committed git
history: the files committed together with the target (a file, a symbol's
file, or a component's files), each with the commits it shares with the
target out of the target's and out of its own (`2 of the target's 4, 2 of
its own 2`, as the heading says to read them) and its newest shared
commits, those that change mostly with the target first, then a `history:`
line
saying what was read (`history: HEAD 6bf7b15, full clone; 6 commits read, 5
counted; left out 1 over 30 files; renames -M50%`), or why nothing was
(`not read (not a git repository)`); see [history.md](history.md).
Changing together is a fact of the history, never proof of a dependency.
`Not traced` ends the answer as in `query`, and gives a script's note and a
`declare global` one too,
and `history:` when the history read may hide files changed with the
target (a shallow clone, older commits not read, renames not detected).

Direct dependents come with their statements into the target that the
lists below hold, those with most in production code first, written as
`query` counts neighbors (`app/checkout.ts  2 imports, 1 in tests`), and
transitive dependents nearest first, with their steps from the target and
what the walk reached them from: a file they import (`app/lazy.tsx  2
steps, through src/lib/money.ts`), or a component they depend on as a
whole, with the line of their manifest that declares it (`cli  2 steps,
through app (declared in crates/cli/Cargo.toml:9)`) or of their file that
imports it without naming a file of it (`(imported in src/page.tsx:2)`);
ties go by the name shown. A component that holds the target, such as a
package whose own barrel re-exports it, names the files of it the change
reaches, nearest first (`ts-shop (src/index.ts)  2 imports`), since the
target is inside it and the rest of it may not be reached. Lists show 30
components, 5 statements, 20 test files, 5 files changed in the same commits
with 2 commits each, 3 files of a component that holds the target, 3 entries
each of `not tests` and `left out`, and 3 locations per kind of `Not
traced`, and their headings count the rest (`6, showing 5`); a capped
statement list ends with the 3 components the rest are in, most first (`13
more in: app/x 5, app/y 4, app/z 2, +2 more components`); an answer with a
capped list ends by saying so, and
`--verbose` lists every entry. `--format json` lists every entry with all
its evidence, whatever `--verbose` says, and for a busy target runs large:
the target as given as `requested`, the roll-up `depth`, the component that
changes as `target`, and when it is folded into that at the depth the
component that owns the request as `folded_from`, for a symbol its id as
`symbol`, for a package subpath the part after the package name as
`subpath`; `direct` and `transitive` (which includes `direct`) as objects with the
component's `id`, its `distance` (1 for a direct one), for one that holds
the target the files of it reached as `files`, for a direct one its
`imports` (`{"production", "tests"}`), and for one further what it was
reached `through` (a file's path or a component's id) and, where a
declaration or an import that names no file was the way, `declared_in` or
`imported_in` (`{"file", "line"}`), in the text's order (direct ones by
most statements in production code, then most statements, then name; the
others by distance, then name);
`importers`, `imports_below` and `may_use` as `{"recorded", "total",
"statements"}`, each statement its evidence (`file`, `line`, `note`,
`target`, `scope`, `names`, `exported_as` for a re-export that renames what
it passes on, `test`, `type_only`, `replaces`) with the
`component` it is in and, for a symbol, the barrel it went through as
`through`, for a Rust file of methods the type a statement takes as
`takes_type`, `recorded` being false when no evidence names imported files for
the language; for a symbol, `used_at` as `query` gives it and `unnamed`,
the statements that take its file whole and never name it, by `file` and
`line`; `tests` as `{"total", "files"}` with every file by path, each
with its `ways` (those at its fewest steps and, where none of them runs
what changed, the nearest that do: `{"kind": "takes", "via"?}`, `whole`,
`runs_first` and `through` with their `file`, `target`, each with its
`steps`, `types_only` when it runs none of the change and `mock` when only
mock calls load its file), `types_only`, and for a `conftest.py` the
directory it `stands_for`; `not_tests` with each such file, its `kind`
(`helper`, `example`, `bench`), its `ways`, `types_only` and, for a helper, the paths of
the listed tests that load it as `for_tests`; and `left_out` as `{"total",
"files"}` with every test file and the `mocks` of
each, by `file`, `line` and the `target` it replaces, when a mock left one
out; `co_change` with the history read and every file with every shared
commit (see [history.md](history.md#files-changed-in-the-same-commits));
`not_traced` with every location of each kind, a barrel with every
re-export of it on a way as `lines`; and for an import name
`module`, with `target` `null`. Earlier versions capped this JSON unless
`--verbose`, listed `direct` and `transitive` as ids, the statements as
`shown`, the tests to run again as `shown` paths, and each kind of
`not_traced` (in `query` too) as `shown`.

`impact` also takes a symbol, by name or by id, and an import name that no
component carries, which starts from the files that import it, all at once:
the direct dependents are their components, `Imported by` lists the
statements, and a file one of them reaches through production code is no test
to run again for another. Those files did not change, so a test that mocks
one of them is left out. For
a symbol, the first step goes only through the statements that `query` lists
for the symbol: those that take its name (`Imported by`) and those that take
its file whole (`May use`), and the files other than its own where `Used at`
finds a use through no such statement (a Rust module that re-exports it from
its subtree and calls it); every later step is file by file as above, and
imports without a target file on the symbol's component are kept, while a
declaration in a manifest carries no part of it, since it says a package is
installed, not that a symbol of it is used. So a file that imports another
name from the same file is not affected. Two things widen or narrow it:

- A re-export takes the name, so the re-exporting file is in the first step
  (a direct dependent, unless it sits in the symbol's own component, as a
  Python `__init__.py` usually does). A barrel that only passes the name
  on (a TS/JS re-export, a Python `from .m import X` noted `export`) goes no
  further than the statements `query` lists through it, which are in the
  first step too, unless code that depends on the symbol imports it as
  well; a Python `__init__.py`'s `from .m import X` that may use `X` leads
  to every importer of the `__init__.py`, and to whatever imports a module
  below it, as transitive dependents, those that take other names
  included, while those that take `X` from it (`from pkg import X`) are in
  the first step through their `via` evidence.
- A statement that only loads the file (a side-effect import) is not in the
  first step, although code that runs on load may call the symbol.

## fetch github

```bash
archmap fetch github                          # the root's origin on github.com, into .archmap/github.json
archmap fetch github --repo acme/shop --since 2026-01-01
archmap fetch github --repo ghe.example.com/acme/shop --no-titles -o ../shop-github.json
```

`fetch github` writes the work snapshot that `query '#N'` and `summary`'s
Coverage read: the issues and pull requests updated since a date, newest
first up to `--max-items` (5,000) of each, and the links GitHub records
between them, through GitHub's GraphQL API with the gh CLI as transport.
gh holds the token (`GH_TOKEN`, `GITHUB_TOKEN` or its login, per host, for
its active account: with several accounts, choose with `GH_CONFIG_DIR` or
`GH_TOKEN`); archmap never reads it. The repository is `--repo
OWNER/NAME`, else the root's `origin` when it is on github.com; another
host (an SSH alias included) is named with `--repo HOST/OWNER/NAME` and
must be one gh is logged in to, since a clone can name any `origin`. The
date is `--since`, else the committer date of the oldest commit the local
history read holds, else (in a shallow clone, or with fewer than 100
commits) 365 days before the fetch; `--all` reads every item. The snapshot
is written whole or not at all, beside `scan`'s output unless `-o` names
another file, and the command prints what it wrote. `--no-titles` keeps
numbers, states and times only. What a snapshot holds, the link types and
what each fetch asks GitHub are in [work.md](work.md). It is the only
command that reaches the network; the MCP server does not offer it.

## How a target is found

`query` and `impact` look a target up the same way, and the first kind that
matches decides:

1. a path written as one (`./x`, `../x`, absolute): the file, or the
   component that owns the directory; a path outside the root is an error;
   `env:` and a name: that environment variable (below)
2. a component id, or a symbol id
3. a component name
4. a file by its path from the root (`manage.py`, `src/lib/money.ts`)
5. a symbol name (`Class.method`, `Type::method`)
6. a file as `<component>.<file stem>` (`shop.users`)
7. a directory, for the component that owns it
8. a package subpath of an npm or TS/JS package (`react-dom/client`,
   `@acme/ui/button`), for that package and the statements that import
   that subpath or a module below it, with or without a file extension
   (`react-dom/client.js`) or an `index` file; JSON keeps the same
   statements
9. an import name that no component carries
10. a file name or stem anywhere under the root (`users.py`, `users`), or a
    component whose name ends in the word as its last part, after `/`, `.`
    or `::` (`pantry` for `components/pantry`, `billing` for
    `shop.billing`), unless that component is one file, which its stem
    finds
11. a name that TS/JS statements take from a package (`revalidatePath`),
    apart from `default` and `*`, which only its id reaches: the package's
    id, `::` and the name (`ext:npm:next::revalidatePath`, also at step 2);
    a default import goes by its package subpath instead (`next/link`)
12. a name in capitals, digits and `_` (`APP_REGION`) that TS/JS code reads
    or writes as an environment variable (below)

A directory that no component has for its path answers for the component
whose files it holds: a Rust module whose `mod.rs` sits in it with its
submodules (`src/rust/`), or whose file sits beside it under its name
(`src/billing/` for `src/billing.rs`), when every file recorded in the
directory is that module's or below it. Any other directory answers for the
component that contains it, so `src/` stands for its package.

One pair of matching quotes around a target is dropped, so an id copied
from a shell-quoted list works where no shell removes them. Components that
share a name and sit at one path (a directory that two analyzers map) answer
for the one with evidence there.

When the deciding kind has several matches, the answer lists every match of
every kind as candidates instead: components with their path and kind,
symbols with their location, kind and importer counts, files (production
code before tests), and directories as `./<path>`. A component id that is also the id of a symbol other than the
module itself, and a name with `/` that is also another path under the root,
give candidates too. Text shows the first 10 and counts the rest, every one
with `--verbose`; JSON (`--format json`) has every one as
`{"requested", "total", "candidates": [{"kind", "id" or "path", ...}]}`,
a directory's `path` written as `./<path>`, a name taken from a package's
`id` as `<package id>::<name>` with its `package`, and each with `match`:
`exact`, or `segment` for a component found by the last part of its name.
Retry with one of the ids, or with the path as `./<path>`.

A target of three characters or more without a `/` that names nothing
lists instead the components, symbols and files whose names contain it,
ignoring case (`No name is ...`): those equal to it first, then those that
start with it, hold it from the start of a word (after `_`, `-`, `.`, `/`,
`:`, or a capital after a small letter), hold it anywhere, and hold it with
`_` and `-` left out of both (`hold_until` for `holdUntil`); among them
production code before tests, the repository's components before external
ones, shorter names first. A component that is one file is listed as its
file, and a name taken from a package as its id, after the repository's
names. Symbols that share a name are one candidate (`tick  3 symbols of
that name (query tick)`; JSON `kind` `symbols` with their `count`), and a
name taken from a package says how many statements take it. JSON gives these candidates `match` `case` or `contains`. When nothing
contains the word either, the error says so, with what to do next.

Both commands exit 0 with an answer, 1 with candidates, and 2 when they
cannot answer (nothing has that name, a path is outside the root). Every
command exits 2 when it cannot run (the repository cannot be read, an
argument is wrong), and 1 only for a result to act on: candidates here,
findings in `check`.

When an id or a path answers for one component while others share its name
(`dup` and `dup+typescript` after an id collision) or its path, `query` and
`impact` name them on `also named:` and `also at this path:` lines (for a
file, those of the component it is), and their JSON in `also_named` and
`also_at_path`.

## What a result could not trace

`query` and `impact` end with what could reach their target without an
edge showing it, from what the analyzers record and only when something
applies: `Not traced (what this answer may miss)` in their text and
`not_traced` in their JSON. The
target's own imports without an edge stay under `Not mapped`.

- `dynamic`: calls elsewhere in the target's language (TypeScript and
  JavaScript count as one) that load modules by computed names
  (`importlib.import_module(name)`, `require(path)`); any of them may load
  the target. A call whose name starts with text that leads to a known path
  (`` import(`./pages/${name}`) ``, `import_module(f"plugins.{name}")`)
  counts only for a target at or below that path, or a directory that holds
  it, and says so (`(below src/pages/)`, JSON `below`), and for a file that
  loading one runs first, a Python package's `__init__.py` above the path
  (`(below app/plugins/, which runs it first)`, JSON `runs_first`). Production code comes
  first, and test code is marked `(test)`.
- `named_like`: imports without an edge (`local name`, `unresolved`) that
  may be the target unresolved, or for a symbol its file (a test that
  imports the module by a name a `sys.path` entry added at runtime
  resolves, whose uses are not read): a relative specifier that, resolved against
  the importer's directory, lands on the target's path (extension aside,
  an entry file by its directory); a path or dotted name that the target's
  path ends in, an alias such as `@/lib/utils` only within the target's own
  package; a bare name that is the target's name. It is a name match, not an
  import of the target.
- `not_read`: files of the target's language (TypeScript and JavaScript
  together) that no analyzer read, counted from Coverage. For Rust the
  answer says why: the analyzer reads `src/` and what the other Cargo
  targets load, so the files outside `src/` that no target loads, such as
  test data, are among them.
- `script`: the target is a script, whose globals no import names; the value
  says so. `impact`'s text gives it here, and so does `query`'s when it
  lists importers of the target; otherwise `query` says it where the
  importers would be.
- `global`: the target is a symbol a module declares in `declare global`,
  or a file or component that holds one, whose uses no import names; the
  value says so. `query`'s text gives it here for a file or a component,
  and where the importers would be for a symbol.
- `no_importers`: the target's importers are recorded and none exists; the
  value says why that is no proof of no use (only import statements are
  read, so a file that a framework, a test runner or a command loads by
  name or path has none). `query`'s text shows it for a file, `impact`'s for
  a file or a symbol. It is left out for a test file, which its runner
  loads, for a script or what holds a `declare global` declaration, and
  for a Python package's `__init__.py` that an
  import of a module below it runs first.
- `macros`: Rust macro calls whose arguments were not read (`json!`, a DSL)
  and whose `a::b` paths write the target's name (its module's, or its
  crate's for a crate root). A name match, not a use of it.
- `whole_module`: for a symbol, places where a binding of its module whole
  is used other than by a static name (passed as a value, `ns[key]`), where
  code may use it unseen (see `Used at` under [query](#query)).
- `strings`: for a symbol, strings that name it by its dotted path
  (`mock.patch("shop.charge.pay")`), where code that looks the name up may
  use it.
- `uses`: for a symbol, statements and files whose uses of it were not
  read, with why.
- `values`: for a method that is not static, what reading calls through a
  value of its type needs, and the statements that bind its class without
  another use read.
- `subclasses`: for a class member, the places that extend its class, whose
  calls through a subclass are not read.
- `barrels`: for `impact`, the re-exports past which it follows only what
  takes the changed names (see [impact](#impact)).
- `relays`: for a name taken from a package, the statements that pass it
  on (`export { x } from 'pkg'`): what imports it from their files is not
  read.
- `history`: for `impact`, what in the history read may hide files changed
  in the same commits as the target (see
  [history.md](history.md#files-changed-in-the-same-commits)).
- `routes`: for `impact`, the files the change starts from or reaches
  through statements that run (none that takes types only), in any
  component, that a framework loads for a URL: in a package whose manifest
  declares `next`, the route files below `app/` or `src/app/` (`page`,
  `layout`, `loading`, `route`, metadata routes such as `sitemap` and the
  rest of Next.js's file conventions) outside private `_folders`, and every
  file below `pages/` or `src/pages/`, apart from test code as the scan
  reads it there. A test that loads one through a URL, such as an
  end-to-end test's `goto`, imports nothing of it, so it is not among the
  tests to run again; search the tests for the URLs it serves. JSON gives
  them as `files`.
- `middleware`: for `impact`, the files among those that run before the
  requests of every URL they match, in such a package's directory or its
  `src/`: Next.js's `middleware.ts`, and `proxy.ts` where the version the
  manifest declares may be Next.js 16 or later, which renamed it (a range
  whose leading major is 16 or more, one open upwards, or a tag such as
  `latest`): tests of any URL may reach the change through them. JSON gives
  them as `files`.

In JSON each kind gives every place as `locations`, with their `total`.
Gaps that no analyzer records yet are not counted: imports in a Rust crate's
own unit tests.

## check

```bash
archmap check                                     # rules from ./archmap.toml; exit 1 on findings
archmap check --format json --config ci/rules.toml
archmap check --path ../some-python-repo          # no archmap.toml: signals only
```

See [rules.md](rules.md).
