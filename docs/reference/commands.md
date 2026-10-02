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
  `not analyzed  sql: 145  notebook: 68`; the number of TS/JS `scripts`, files
  without imports or exports whose declarations are used without an import;
  the number of `dynamic imports` and
  the components that make them; and a fixed line naming the runtime coupling
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
```

`query` works on top of the rolled-up graph, for a component, a symbol or a
single file, including the imports no edge shows. It also takes a directory
for the component that owns it, a package subpath for its package
(`react-dom/client`), and an import name that no component carries (`torch`
declared as an extra) for the imports of it that no edge shows. A component
that is one file (a TS/JS file, a Rust module without submodules) answers as
that file, with the statements that import it, even where it folds into an
ancestor. How a target is looked up, and what happens when it names several
things, is the same for `query` and `impact`: see
[How a target is found](#how-a-target-is-found).

`query` prints compact text by default: public symbols with their location
in source order (by file, then line; JSON keeps them by id),
and each neighboring component with its import count and a few example
locations. A location names the file the statement loads when archmap knows
it, as in `src/shop/billing/charge.py:5 -> src/shop/users.py`, and ends in
`(local)` when the import sits inside a function body and so runs only when
the function is called; the others run when their file loads. `(type)` marks
a statement that takes types only, so it never runs (a TS/JS `import type`,
which the compiler erases, or a Python import under `if TYPE_CHECKING:`),
and `(test)` a statement in test code. Each
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
is not traced; `query` on a symbol a script declares says the same.
A TS/JS re-export statement (`export ... from`) is marked `(export)`
wherever a location is shown: it passes names on rather than uses them.
For a Python package's `__init__.py`, which runs before any module below it
is loaded, a line counts the statements outside the package that import a
module below it and names the `impact` that lists them (`Imports below: 5
(they run it first): ...`); JSON lists them as `imports_below`.

`query` on a symbol (by name, `Class.method` / `Type::method`, or by id)
lists the statements that import it, from the names their evidence records
(see [graph.md](graph.md)). `Imported by` lists the statements that take the
symbol's name from the file that defines it; a method goes by its type's
name, and a Rust method whose type another file defines, by that file. `May
use` lists, apart from those, the statements that take that file whole (a
namespace import, a glob, `import pkg.sub`). Through a TS/JS barrel that
passes the name on (a statement noted `export` that takes the name or the
file whole, and so on up a chain of barrels), both lists also hold what
takes the barrel: its name, where the walk through the barrel found no
definition, under `Imported by`, the barrel whole under `May use`, each with
the barrel it went through (`src/app/checkout.ts:2 (whole src/index.ts,
which passes it on)`); a barrel that renames the name on the way ends the
chain. Statements that only load the file take no name and are in neither
list. Both lists show 5 statements,
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

- A Python `__init__.py` may use what it imports as well as pass it on, so
  no statement of it is a barrel: a statement that takes the package whole
  (`import pkg`, then `pkg.pay()`) is in neither list for `pay` when
  `pkg/__init__.py` imports it from another file.
- Rust path evidence shows the first path from its file to the target, not
  always one that names the symbol.
- Code that runs when a file loads (a side-effect import) is listed for
  neither.

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
it and a package subpath for its package, and a component that is one file
answers as that file. Dependencies without a target file
(manifests, external packages) are followed component by component, and the
result is still reported at the roll-up depth. A file reached stands for its
component there, unless it is test code, which no dependent loads: a test
that a package owns reaches no manifest that declares the package. A
production file that dependents do not load, such as a binary's
`src/main.rs`, still stands for its package and reaches the packages that
declare it. It does not follow Rust code inside macro calls, and a path that
names no component or file is an error. Direct and
transitive dependents follow production code; the tests to run again are the
files that reach the target only through test code, and a changed
component's own test files, those beside production code included. A file
that production code reaches is not repeated there: its unit tests run with
its package. For Rust, only test code in other crates is recorded: a crate's
own unit tests are not, and integration tests under `tests/` are not read. In
`fixtures/mixed-utils-project`, `app.utils` and `app.core` depend on each
other, so following components a change anywhere in `app.utils` reaches
`app.core` and `app.models`; following files, `app/utils/log.py` reaches
both and `app/utils/registry.py` reaches neither.

`impact` prints compact text by default, written the way `query` writes its
answers:

```text
src/lib/types.ts (file) in lib/types.ts (module, typescript), depth 2
id: ts-shop::src/lib/types.ts

Direct dependents: 4
  ts-shop
  app/checkout.ts
  app/page.tsx
  lib/money.ts

Imported by: 6, showing 5 (1 re-export)
  src/app/checkout.ts:8 (via src/index.ts:8) (type)
  src/app/checkout.ts:9 (via src/index.ts:8) (type)
  src/app/page.tsx:2 (type)
  src/index.ts:8 (export) (type)  in ts-shop
  src/lib/money.ts:4 (type)

Transitive dependents: 2 more (6 in all)
  scripts/report.cjs
  app/lazy.tsx

Tests to run again: 2
  tests/helpers.ts
  tests/money.test.ts

Not traced:
  dynamic: 2 calls load modules by computed names, which may be this: scripts/report.cjs:4, src/app/lazy.tsx:7

Lists are capped; verbose lists every entry.
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
whole: one statement per line, production code first, located and marked as
`query` marks them, and followed by the component it is in unless that
component is the file itself. They include the statements inside the
target's own component. For a Python package's `__init__.py`, which runs
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
and the heading counts everything reached. A test file that is the target is
among the tests to run again, marked `(the target itself)`. `Not traced` ends
the answer as in `query`, and gives a script's note too.

Lists show 30 components, 5 statements, 20 test files and 3 locations per
kind of `Not traced`, and their headings count the rest (`6, showing 5`); an
answer with a capped list ends by saying so, and `--verbose` lists every
entry. `--format json` gives the same lists as fields, the output earlier
versions printed by default: `direct` and `transitive` (which includes
`direct`) in full; `importers`, `imports_below` and `may_use` as
`{"recorded", "total", "shown"}` with 5 statements, each with its `file`, `line`, the `component` it
is in, `"test": true` in test code and, for a symbol, the barrel it went
through as `through`, `recorded` being false when no
evidence names imported files for the language; `tests` as `{"total",
"shown"}` with 20 files by path; `not_traced` with 5 locations per kind; and
for an import name `module`, with `target` `null`. `--verbose` lists every
entry there too.

`impact` also takes a symbol, by name or by id, and an import name that no
component carries, which starts from the files that import it: the direct
dependents are their components, and `Imported by` lists the statements. For
a symbol, the first step goes only through the statements that `query` lists
for the symbol: those that take its name (`Imported by`) and those that take
its file whole (`May use`); every later step is file by file as above, and
imports without a target file on the symbol's component are kept, while a
declaration in a manifest carries no part of it, since it says a package is
installed, not that a symbol of it is used. So a file that imports another
name from the same file is not affected. Two things widen or narrow it:

- A re-export takes the name, so the re-exporting file is in the first step
  (a direct dependent, unless it sits in the symbol's own component, as a
  Python `__init__.py` usually does). A TS/JS barrel that only passes the
  name on goes no further than the statements `query` lists through it,
  which are in the first step too; a Python `__init__.py`'s
  `from .m import X` leads to every importer of the `__init__.py`, and to
  whatever imports a module below it, as transitive dependents, those that
  take other names included, while those
  that take `X` from it (`from pkg import X`) are in the first step through
  their `via` evidence.
- A statement that only loads the file (a side-effect import) is not in the
  first step, although code that runs on load may call the symbol.

## How a target is found

`query` and `impact` look a target up the same way, and the first kind that
matches decides:

1. a path written as one (`./x`, `../x`, absolute): the file, or the
   component that owns the directory; a path outside the root is an error
2. a component id, or a symbol id
3. a component name
4. a file by its path from the root (`manage.py`, `src/lib/money.ts`)
5. a symbol name (`Class.method`, `Type::method`)
6. a file as `<component>.<file stem>` (`shop.users`)
7. a directory, for the component that owns it
8. a package subpath of an npm or TS/JS package (`react-dom/client`,
   `@acme/ui/button`), for that package
9. an import name that no component carries
10. a file name or stem anywhere under the root (`users.py`, `users`)

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
give candidates too. Text shows the first 10 and counts the rest; JSON
(`--format json`) has every one as
`{"requested", "total", "candidates": [{"kind", "id" or "path", ...}]}`,
a directory's `path` written as `./<path>`. Retry with one of the ids, or
with the path as `./<path>`.

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
applies: `Not traced` in their text and `not_traced` in their JSON. The
target's own imports without an edge stay under `Not mapped`.

- `dynamic`: calls elsewhere in the target's language (TypeScript and
  JavaScript count as one) that load modules by computed names
  (`importlib.import_module(name)`, `require(path)`); any of them may load
  the target. Production code comes first, and test code is marked
  `(test)`.
- `named_like`: imports without an edge (`local name`, `unresolved`) that
  may be the target unresolved: a relative specifier that, resolved against
  the importer's directory, lands on the target's path (extension aside,
  an entry file by its directory); a path or dotted name that the target's
  path ends in, an alias such as `@/lib/utils` only within the target's own
  package; a bare name that is the target's name. It is a name match, not an
  import of the target.
- `not_read`: files of the target's language (TypeScript and JavaScript
  together) that no analyzer read, counted from Coverage. For Rust the
  answer says why: the analyzer reads only `src/`, so `tests/`, `benches/`,
  `examples/` and `build.rs` are among them.
- `script`: the target is a script, whose globals no import names; the value
  says so. `impact`'s text gives it here, and `query`'s where it lists the
  target's importers, when there are none.
- `no_importers`: the target's importers are recorded and none exists; the
  value says why that is no proof of no use (only import statements are
  read, so a file that a framework, a test runner or a command loads by
  name or path has none). `query`'s text shows it for a file, `impact`'s for
  a file or a symbol. It is left out for a test file, which its runner
  loads, for a script, and for a Python package's `__init__.py` that an
  import of a module below it runs first.

Gaps that no analyzer records yet are not counted: module paths inside Rust
macro calls, imports in a Rust crate's own unit tests.

## check

```bash
archmap check                                     # rules from ./archmap.toml; exit 1 on findings
archmap check --format json --config ci/rules.toml
archmap check --path ../some-python-repo          # no archmap.toml: signals only
```

See [rules.md](rules.md).
