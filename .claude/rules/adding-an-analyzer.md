---
paths:
  - "crates/archmap-scan/**"
  - "crates/archmap-core/**"
  - "docs/reference/analyzers.md"
---

# Adding an analyzer

1. Create `crates/archmap-scan/src/<lang>/` implementing `Analyzer` (`name`, cheap `detect`, `analyze -> AnalyzerOutput`).
2. Register it in `default_analyzers()`. Static registration only; no dynamic plugin system.
3. Per-file problems go into `AnalyzerOutput::warnings`, not `Err`.
4. Report source files read per language in `AnalyzerOutput::read`, and record every import that maps to no component (standard library aside) as an `UnmappedImport` with its reason. `summary`, `query` and `impact` rely on both to say what the graph does not show (Coverage, `Not mapped`, `Not traced`).
5. Add a fixture under `fixtures/` and an integration test in `crates/archmap-scan/tests/`.
6. Describe what the analyzer reads and its known gaps in `docs/reference/analyzers.md`, and add its row to README's "What archmap reads" and its column to "Support by language".
7. Do not change `archmap-core` unless a genuinely new *kind* of fact appears. Prefer a new `EdgeKind` / `SymbolKind` variant over new structs.
