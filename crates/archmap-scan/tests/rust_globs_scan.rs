//! Globs of modules that re-export from their own subtree, in
//! `fixtures/rust-globs`: `market`'s `lib.rs` re-exports `charge::pay` and
//! `charge::Receipt`, and its inline `prelude` re-exports `charge::refund`,
//! none of which is an import of its own. A glob takes the names its file
//! writes as if it had named them, and the names of the macros it calls
//! only where they are exported macros.

use std::path::Path;

use archmap_core::EdgeKind;
use archmap_scan::{scan, symbol_uses, ScanOptions, ScanReport};

fn report() -> ScanReport {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/rust-globs");
    let report = scan(
        &root.canonicalize().expect("fixture exists"),
        &ScanOptions::default(),
    )
    .expect("scan succeeds");
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    report
}

/// The import evidence written in `file`: `line target note [names]`.
fn imports_in(report: &ScanReport, file: &str) -> Vec<String> {
    let mut found: Vec<String> = report
        .graph
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Import)
        .flat_map(|e| &e.evidence)
        .filter(|e| e.file == file)
        .map(|e| {
            let names: Vec<&str> = e.names.iter().map(String::as_str).collect();
            format!(
                "{} {} {} [{}]",
                e.line.unwrap_or_default(),
                e.target.as_deref().unwrap_or_default(),
                e.note.as_deref().unwrap_or_default(),
                names.join(", ")
            )
        })
        .collect();
    found.sort();
    found
}

#[test]
fn a_glob_takes_the_names_its_file_writes_from_the_files_that_define_them() {
    let report = report();
    assert_eq!(
        imports_in(&report, "tests/root_glob.rs"),
        [
            // `charge::refund(2)`: through the module the glob brings in
            "1 src/charge.rs use [refund]",
            // `pay(1)` and `Receipt::new()`: through the crate root's
            // re-export of its subtree
            "1 src/charge.rs use via src/lib.rs:7 [Receipt, pay]",
            "1 src/lib.rs use [*]",
            // `settle!(3)`, an exported macro; `concat!` is no call of the
            // function `concat` the glob brings in
            "1 src/macros.rs use [settle]",
        ]
    );
    // `audit!(1)` calls a macro, not the module `audit` the glob brings in
    assert_eq!(
        imports_in(&report, "src/report.rs"),
        ["1 src/lib.rs use [*]"]
    );
    // a glob of an inline module
    assert_eq!(
        imports_in(&report, "tests/prelude_glob.rs"),
        [
            "1 src/charge.rs use via src/lib.rs:4 [refund]",
            "1 src/lib.rs use [*]",
        ]
    );
    // a `let` and a closure's parameter named `pay` hide the glob's
    assert_eq!(
        imports_in(&report, "tests/shadowed.rs"),
        ["1 src/lib.rs use [*]"]
    );
}

#[test]
fn a_name_a_glob_brings_in_is_used_through_the_glob() {
    let report = report();
    let uses = |id: &str| -> Vec<String> {
        let symbol = report
            .graph
            .symbols
            .values()
            .find(|s| s.id.as_str() == id)
            .unwrap_or_else(|| panic!("no symbol {id}"));
        let found = symbol_uses(&report, symbol);
        assert!(found.unused.is_empty(), "{found:#?}");
        found
            .uses
            .iter()
            .map(|u| {
                let line = u.statement.as_ref().map_or(0, |s| s.line);
                format!(
                    "{}:{} via {line}",
                    u.evidence.file,
                    u.evidence.line.unwrap_or_default()
                )
            })
            .collect()
    };
    assert_eq!(uses("market::charge::pay"), ["tests/root_glob.rs:5 via 1"]);
    assert_eq!(
        uses("market::charge::refund"),
        [
            "tests/prelude_glob.rs:5 via 1",
            "tests/root_glob.rs:7 via 1"
        ]
    );
}
