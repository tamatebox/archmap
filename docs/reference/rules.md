# Rules and signals

`check` compares the graph with a declared architecture in `archmap.toml`:
forbidden dependencies, layers, allow lists, coverage, cycles, undeclared
imports, and declarations that match nothing; it also reports structural
signals, with or without `archmap.toml`. Command examples are in
[commands.md](commands.md#check).

## Rules

`archmap check` compares the observed graph with a declared architecture in
`archmap.toml` at the repository root:

```toml
[components]            # declared name = selectors
shop = ["src/shop"]
jobs = ["scripts"]

[[deny]]
from = "shop"           # a declared name or a selector
to = "jobs"
reason = "library code must not know about the scripts that run it"

[cycles]
forbid = true           # cycles between components at the roll-up depth
scope = ["src"]         # only cycles with a member under these selectors

[undeclared_imports]
forbid = true           # imports of packages no manifest declares
ignore = ["ujson"]      # dotted prefixes to accept, e.g. optional imports

[layers]
order = ["jobs", "shop"]   # top to bottom: never depend on a layer above

[[allow]]
from = "shop"
to = []                 # the declared components shop may depend on

[coverage]
require = ["src"]       # everything under src must be declared
```

A selector is a path prefix, where `src/shop` covers everything below it, or
an external id such as `ext:pypi:requests` or `ext:pypi:google-*`. An
external selector without an ecosystem (`ext:requests`, as written before
external ids carried one) matches nothing and is reported. When selectors
overlap, the most specific one owns a component. `deny` sides take declared
names or selectors; `layers` and `allow` take declared names only.

Layers go from the top down, and a dependency on a layer above is a
violation. An allow list turns a declared component's dependencies into a
closed set: any other dependency on a declared component is unexpected, and
an allowed dependency the code no longer has is stale. Coverage requires
every component under its selectors to belong to a declaration, judged at
the roll-up depth and for leaves only, so a container such as `src` counts
as covered by what it contains.

Rules are about production code. `deny`, `layers` and `allow` leave out
imports in test code (test files and directories, `#[cfg(test)]`; see
[graph.md](graph.md)), so a test may import what its code may not, and an
allowance that only tests use is stale. Undeclared imports count in test
code too: a missing declaration breaks the test run as well. A route
directory named `test` or `tests` (`app/test/page.tsx` in frameworks whose
directories are URLs) is test code by that rule, so rules miss its imports.

`check` reports forbidden, upward and unexpected dependencies with the
evidence behind them, stale allowances, uncovered components, dependency
cycles at the roll-up depth (`depth` in the file or `--depth`, default 2),
undeclared imports, and declarations, rule sides or `ignore` entries that
match nothing, so a typo never silently disables a rule. Text output shows
up to 3 locations per finding, marked `(type)`, `(test)` and `(local)` as
`query` marks them, and a rule finding whose imports all take types only says so
(`(import, types only)`); `--format json` lists all of them. It
exits 0 without findings, 1 with findings, and 2 when the rules or the
repository cannot be read, including a `--config` file that does not exist.
Without `--config` and without an `archmap.toml`, `check` reports signals
only and exits 0. archmap checks its own `{cli, mcp} -> app -> scan -> core` direction
this way; see `archmap.toml`.

`[cycles] scope` limits cycle findings to cycles with at least one member
under its selectors, such as product code but not fixtures. Cycles count
only production imports that run: a TS/JS import of types only (`import
type`), a Python import under `if TYPE_CHECKING:` and an import in test code
close none, while `deny`, `layers` and
`allow` count imports of types like any other.
Every cycle finding also says what the files behind it show, because
roll-up joins the files of each component and different files can close the
loop:

- `file level: no cycle; different files form each direction`: only the
  components form a cycle
- `file level: cycle through <files>`: files of at least two of the
  components form a cycle, and the line ends with `closes at module scope`
  or `closes only through local-scope imports` (it disappears without
  imports inside function bodies)
- `file level: unknown, no import targets recorded`: no evidence behind the
  cycle names imported files, as when only manifests declare it

None of these says whether the program fails at runtime.

For Python, an import counts as declared when a runtime dependency, an extra,
a dependency group or a dev dependency declares its distribution for the
file's directory. When only a requirements file for another directory
declares it, such as the one next to a separately deployed function, the
finding names that file. Without a
`.venv`, archmap cannot match every import name to its distribution; add
such names to `ignore`. With a `.venv`, the finding also names the installed
distribution that provides the module, which is usually a transitive
dependency.

The declared architecture never changes what `scan`, `summary`, `query` or
`impact` report.

## Signals

`check` also reports signals: deterministic observations about the shape of
the code, with the files behind them. A signal is not a violation. It never
changes the exit code and needs no `archmap.toml`; text output prints it as
a `signal:` line and JSON lists it under `signals`.

One kind exists today, `mixed_directions`: a component and a partner
depend on each other, but the files of the component that the partner uses
are not the files that use the partner. Like cycles, it counts only
production imports that run.

```text
signal: app.utils mixes dependency directions with app.core, app.models
  used by them: app/utils/log.py (2)
  using them: app/utils/registry.py -> app.models; app/utils/store.py -> app.core
```

It often explains a component cycle that has no file cycle behind it: one
directory holds both shared helpers and code built on top of other
components. Whether that is a problem is a judgement: archmap attaches one
only where a threshold for it is declared, and none can be declared yet.
