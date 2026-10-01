# Commands

What each command prints and how its output stays small. The examples run
on archmap's own repository unless they name another root: any Rust, Python
or TypeScript/JavaScript project or package directory works the same way,
given as the argument of `summary` and `scan` or as `--path` of the others. The graph
they read is described in [graph.md](graph.md); `check` and its rules are in
[rules.md](rules.md).

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
  in test code (`extra or dev dependency 2 (1 in tests)`); the languages no analyzer reads, such as
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
  loads only when it takes the barrel whole or a name the barrel declares. Statements of a component's own entry file (an
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
omitted: 12 modules  in: shop.billing 7, shop 5  next: archmap query <component>
```

The whole summary then aims at 8 KiB: over it, the largest list gives up
its lowest-ranked entries, down to 10 each. The header, coverage, `omitted:`
lines and the most depended on list are never trimmed, so very long names
can exceed the target, but the size does not grow with the repository.
`--verbose` lists everything. On archmap's own repository (92 source files,
fixtures included), depth 2 turns a 230 KB graph into a summary of about
8 KB (19 KB with `--verbose`).

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
for the component that owns it, and an import name that no component
carries (`torch` declared as an extra) for the imports of it that no edge
shows. A component that is one file (a TS/JS file, a Rust module without
submodules) answers as that file, with the statements that import it, even
where it folds into an ancestor.

`query` prints compact text by default: public symbols with their location
in source order (by file, then line; JSON keeps them by id),
and each neighboring component with its import count and a few example
locations. A location names the file the statement loads when archmap knows
it, as in `src/shop/billing/charge.py:5 -> src/shop/users.py`, and ends in
`(local)` when the import sits inside a function body and so runs only when
the function is called; the others run when their file loads. `(type)` marks
a TS/JS statement that takes types only (`import type`), which the compiler
erases, so it never runs, and `(test)` a statement in test code. Each
neighbor counts its statements as `summary` counts the pair: in production
code and in tests apart (`2 imports, 1 in tests`), and apart from those
statements of the component's entry file into its own submodules (`6 of its
entry file`) and those that load a barrel only to reach names it re-exports
(`3 through re-exports`, located `(through)`), which count for the files
that define the names. Production code is listed first, and neighbors with
more production statements come first. A `Not mapped`
section then lists the imports of the component that no edge shows, one line
per module with the reason (`local name`, `extra or dev dependency`,
`undeclared`, or `dynamic` for a call that loads modules by name) and where
they are, so an absent edge is never mistaken for an absent dependency.
Lists are capped at 30 entries and 3 locations, and the rest is counted. On
archmap's own repository, its busiest component (`archmap_core::graph`)
takes 5 KB as text and 18 KB as JSON. `--verbose` lifts the caps and
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

`query` on a symbol (by name, `Class.method` / `Type::method`, or by id)
lists the statements that import it, from the names their evidence records
(see [graph.md](graph.md)). `Imported by` lists the statements that take the
symbol's name from the file that defines it; a method goes by its type's
name, and a Rust method whose type another file defines, by that file. `May
use` lists, apart from those, the statements that take that file whole (a
namespace import, a glob, `import pkg.sub`). Statements that only load the
file take no name and are in neither list. Both lists show 5 statements and
count the rest, and their heading counts the re-export statements among them
(`Imported by: 4 (2 re-exports)`); nothing found reads `none resolved`, with
a reminder that only import statements are read, so it does not mean
unused: an entry point that a framework or runtime loads by name or path,
such as a route or a handler, shows the same. When several symbols match,
each line counts its importers instead (`imported by 3, may use 1`), and
querying one by its id lists them. A
Rust module (`pub mod invoice;`) is also a symbol of the file that declares
it, but its imports name its own file, so `query` points at its component
instead, and `impact` answers for that component. What the lists miss:

- Python does not follow re-exports: `from pkg import pay`, where
  `pkg/__init__.py` re-exports `pay`, is listed for `__init__.py`, not for
  the file that defines `pay`.
- A Rust function called through a module that a `use` brought in (`use
  crate::graph;`, then `graph::build()`) is only under `May use`, through
  that `use`; and Rust path evidence shows the first path from its file to
  the target, not always one that names the symbol.
- A TS/JS barrel imported as a namespace (`import * as ui from './ui'`) is
  not listed for the files behind the barrel.
- Code that runs when a file loads (a side-effect import) is listed for
  neither.

## impact

```bash
archmap impact archmap-core
archmap impact crates/archmap-scan/src/lib.rs
archmap impact src/shop/users.py --path ../some-python-repo
archmap impact formatPrice --path fixtures/simple-ts-project              # a symbol
```

`impact` follows imports file by file where the evidence names the imported
file: a component is affected only when one of its files imports what
changed, directly or through other files, not merely because it imports some
file of the same component. A file target starts from that file; a component
target starts from all of its files. Like `query`, it takes a file as
`<component>.<file stem>` too and a directory for the component that owns
it, and a component that is one file answers as that file. Dependencies without a target file
(manifests, external packages) are followed component by component, and the
result is still reported at the roll-up depth. It does not follow the parent
`__init__.py` that Python runs before a submodule, nor Rust code inside macro
calls, and a path that names no component or file is an error. `direct` and
`transitive` follow production code; `tests` counts the files that reach the
target only through test code, and a changed component's own test files: the
tests to run again after the change, those beside production code included,
the first 20 by path shown (`--verbose` lists every one, and every importer).
A file that production code reaches is not
repeated there: its unit tests run with its package. For Rust, only test
code in other crates is recorded: a crate's own unit tests are not, and
integration tests under `tests/` are not read. For a file target, `importers` lists the statements that
import the file directly, up to 5 with the total, production code first and
test code marked `"test": true`, so the next read can go straight to them;
they include statements inside the target's own component, which `direct`
leaves out. In
`fixtures/mixed-utils-project`, `app.utils` and `app.core` depend on each
other, so following components a change anywhere in `app.utils` reaches
`app.core` and `app.models`; following files, `app/utils/log.py` reaches
both and `app/utils/registry.py` reaches neither.

`impact` also takes a symbol, looked up after components and files and
before directories, by name or by id; a name that several symbols share is
an error that lists their ids. Its first step goes only through the
statements that `query` lists for the symbol: those that take its name
(`importers`) and those that take its file whole (`may_use`); every later
step is file by file as above, and dependencies without a target file on
the symbol's component are kept. So a file that imports another name from
the same file is not affected. Two things widen or narrow it:

- A re-export takes the name, so the re-exporting file is in the first step
  (`direct`, unless it sits in the symbol's own component, as a Python
  `__init__.py` usually does), and from there every importer of that file is
  `transitive`, those that take other names included: a TS/JS barrel's
  `export { X } from`, and a Python `__init__.py`'s `from .m import X`, whose
  importers (`from pkg import X`) are only `transitive` (Python does not
  follow re-exports).
- A statement that only loads the file (a side-effect import) is not in the
  first step, although code that runs on load may call the symbol.

`importers` and `may_use` show 5 statements with their total, as for a file;
`query <symbol> --format json` lists them all.

## Names that several components share

A name that several components share stops `query` and `impact` with their
ids and paths (the first 10), and an id or `./<path>` picks one, unless they
all sit at one path (a directory that two analyzers map), which answers for
the one with evidence there. When an id or a path answers for one component
while others share its name (`dup` and `dup+typescript` after an id
collision) or its path, `query` names them on `also named:` and `also at
this path:` lines, and `impact` in `also_named` and `also_at_path`.

## check

```bash
archmap check                                     # rules from ./archmap.toml; exit 1 on findings
archmap check --format json --config ci/rules.toml
archmap check --path ../some-python-repo          # no archmap.toml: signals only
```

See [rules.md](rules.md).
