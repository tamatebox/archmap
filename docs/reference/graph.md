# Graph model

The Architecture Graph that every analyzer ([analyzers.md](analyzers.md))
feeds and every command ([commands.md](commands.md)) reads.

## Model

```text
ArchitectureGraph
├── meta:       { root, analyzers, tool_version, coverage: { language -> { files, read? } } }
├── components: { id -> Component { kind: package | module | external, language, path, parent?, evidence } }
├── symbols:    { id -> Symbol { kind: function | struct | enum | trait | ..., component, signature, evidence } }
├── edges:      [ Edge { from, to, kind: import | dependency | call | http | database | event | unknown, evidence } ]
├── unmapped_imports: [ UnmappedImport { from, module, reason: undeclared | declared_not_required | local_name | unresolved, provided_by?, evidence } ]
└── dynamic_imports:  [ DynamicImport { from, call, evidence } ]

Evidence { file, line?, note?, target?, scope?: module | local, names?, test?, type_only? }
```

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
`target` means the statement only loads the file (a side-effect import).
Through re-exports, evidence noted `via <file>:<line>` names what the
defining file declares. A statement gives one piece of evidence per file
it points at and re-export it goes through, with all of its names. Every
analyzer records `names`.

`test` marks a statement in test code, which runs only for tests: for
Python and TS/JS a file named `*.test.*`, `*.spec.*`, `test_*.py`,
`*_test.py` or `conftest.py`, or any file below a directory named `test`,
`tests`, `__tests__` or `__mocks__`; for Rust, code under `#[cfg(test)]` or
`#[test]`, never a path. It is on the evidence of edges, imports without an
edge and dynamic imports. No command reads it yet: `impact` counts test
code like any other, and so do rules and cycles.

`type_only` marks a statement that takes types only, which the compiler
erases, so it never runs: TS/JS `import type`, `export type ... from`, and
a statement whose names all carry `type`. A statement that takes values and
types from a file gives one piece of evidence for each; evidence without a
`target` records no names and is one, `type_only` when the statement takes
only types. An edge is a dependency however
it is taken, so `deny`, `layers`, `allow`, `query` and `impact` count every
import, but cycles and signals count only imports that run: an edge whose
every piece of evidence is `type_only` closes no cycle. Only the TS/JS
analyzer sets it.

A symbol's evidence with a `target` says how the symbol is reached rather
than where it is: a Rust method whose type another file defines carries
evidence noted `impl` that points at that file, with the type's name.

An unmapped import is an import that maps to no component, standard-library
imports aside, and `reason` says why. A dynamic import is a call that loads a
module by a name computed at runtime. Both are observations, never edges:
they mark where a dependency may exist that no edge shows. `query` lists them
and `check` reports the undeclared ones. `meta.coverage` counts the files of
each recognized source language and how many an analyzer read; a language
without `read` has no analyzer. Configuration, data and documentation files
are not counted. The JSON carries `schema_version: 3`.

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
