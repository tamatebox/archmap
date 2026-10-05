//! End-to-end scan of `fixtures/ts-type-names`: an import whose names are
//! declared only as types where they are defined never runs.

use std::collections::BTreeSet;
use std::path::Path;

use archmap_core::ArchitectureGraph;
use archmap_scan::{scan, ScanOptions};

fn scan_fixture() -> ArchitectureGraph {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/ts-type-names")
        .canonicalize()
        .expect("fixture exists");
    let report = scan(&root, &ScanOptions::default()).expect("scan succeeds");
    assert!(
        report.warnings.is_empty(),
        "warnings: {:?}",
        report.warnings
    );
    report.graph
}

/// The evidence of the statement at `at` for `target`, as (names, types
/// only, note).
fn evidence(
    graph: &ArchitectureGraph,
    at: &str,
    target: &str,
) -> BTreeSet<(Vec<String>, bool, String)> {
    graph
        .edges
        .iter()
        .flat_map(|e| &e.evidence)
        .filter(|e| {
            format!("{}:{}", e.file, e.line.unwrap_or(0)) == at
                && e.target.as_deref() == Some(target)
        })
        .map(|e| {
            let note = e.note.clone().unwrap_or_default();
            (e.names.iter().cloned().collect(), e.type_only, note)
        })
        .collect()
}

fn rows(rows: &[(&[&str], bool, &str)]) -> BTreeSet<(Vec<String>, bool, String)> {
    rows.iter()
        .map(|(names, types, note)| {
            let names = names.iter().map(|n| (*n).to_owned()).collect();
            (names, *types, (*note).to_owned())
        })
        .collect()
}

#[test]
fn names_declared_only_as_types_are_imported_as_types() {
    let graph = scan_fixture();
    // an interface and a type alias, then a default interface by its name
    assert_eq!(
        evidence(&graph, "src/cart.ts:1", "src/money.ts"),
        rows(&[(&["Currency", "Money"], true, "import")])
    );
    assert_eq!(
        evidence(&graph, "src/cart.ts:2", "src/money.ts"),
        rows(&[(&["Price"], true, "import")])
    );
    // a type and a value from one file: two pieces of evidence
    assert_eq!(
        evidence(&graph, "src/cart.ts:3", "src/lines.ts"),
        rows(&[(&["Line"], true, "import"), (&["count"], false, "import")])
    );
    // a const merged with an interface, a class and an enum are values
    assert_eq!(
        evidence(&graph, "src/ledger.ts:1", "src/merged.ts"),
        rows(&[(&["Ledger", "Rate", "Side"], false, "import")])
    );
    // a namespace merged with an interface counts as a value even when it
    // holds only types, and a `const enum` as one, which `isolatedModules`
    // keeps
    assert_eq!(
        evidence(&graph, "src/fees.ts:1", "src/kinds.ts"),
        rows(&[(&["Fee", "Flag"], false, "import")])
    );
    // JavaScript keeps the statement it writes
    assert_eq!(
        evidence(&graph, "src/legacy.js:1", "src/money.ts"),
        rows(&[(&["Money"], false, "import")])
    );
}

#[test]
fn a_re_export_of_a_type_passes_on_a_type() {
    let graph = scan_fixture();
    assert_eq!(
        evidence(&graph, "src/barrel.ts:1", "src/money.ts"),
        rows(&[(&["Money"], true, "export")])
    );
    assert_eq!(
        evidence(&graph, "src/barrel.ts:3", "src/merged.ts"),
        rows(&[(&["Rate"], false, "export")])
    );
    // through the barrel: its own evidence and the defining files'
    assert_eq!(
        evidence(&graph, "src/shop.ts:1", "src/barrel.ts"),
        rows(&[
            (&["Line", "Money"], true, "import"),
            (&["Rate"], false, "import")
        ])
    );
    assert_eq!(
        evidence(&graph, "src/shop.ts:1", "src/money.ts"),
        rows(&[(&["Money"], true, "import via src/barrel.ts:1")])
    );
    assert_eq!(
        evidence(&graph, "src/shop.ts:1", "src/merged.ts"),
        rows(&[(&["Rate"], false, "import via src/barrel.ts:3")])
    );
}

#[test]
fn imports_of_types_close_no_cycle_and_values_still_do() {
    let graph = scan_fixture();
    let cycles: Vec<Vec<String>> = graph
        .cycles()
        .iter()
        .map(|group| group.iter().map(|id| id.to_string()).collect())
        .collect();
    assert_eq!(
        cycles,
        [[
            "ts-type-names::src/ledger.ts".to_owned(),
            "ts-type-names::src/merged.ts".to_owned(),
        ]],
        "{cycles:?}"
    );
}
