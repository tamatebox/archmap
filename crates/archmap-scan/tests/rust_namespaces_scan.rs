//! A module and a function of one name, in `fixtures/rust-namespaces`:
//! `bakery`'s `lib.rs` declares `mod parse;` (private) and `pub mod split;`,
//! and re-exports the function of each module's own name. The compiler keeps
//! types and values apart, so a call names the function, and a `use` takes
//! every namespace the name has that it can see. `lib.rs` also defines `pub
//! mod bake;` beside `pub fn bake()`, `fee.rs` a trait and a function `Pay`,
//! `rate.rs` a constant and a struct `Rate` on one line, and `read.rs` a
//! method of `impl model::Order`.

use std::path::Path;

use archmap_core::{EdgeKind, SymbolUses};
use archmap_scan::{scan, symbol_uses, ScanOptions, ScanReport};

fn report() -> ScanReport {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/rust-namespaces");
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

/// Each use as `file:line role`, with ` as <binding>` and ` via <statement
/// line>` when they apply.
fn uses_of(report: &ScanReport, id: &str) -> Vec<String> {
    let symbol = report
        .graph
        .symbols
        .values()
        .find(|s| s.id.as_str() == id)
        .unwrap_or_else(|| panic!("no symbol {id}"));
    let found: SymbolUses = symbol_uses(report, symbol);
    assert!(found.unused.is_empty(), "{found:#?}");
    found
        .uses
        .iter()
        .map(|u| {
            let mut line = format!(
                "{}:{} {}",
                u.evidence.file,
                u.evidence.line.unwrap_or_default(),
                u.role.as_str()
            );
            if let Some(binding) = &u.binding {
                line.push_str(&format!(" as {binding}"));
            }
            if let Some(statement) = &u.statement {
                line.push_str(&format!(" via {}", statement.line));
            }
            line
        })
        .collect()
}

#[test]
fn a_use_takes_each_namespace_of_a_name_it_can_see() {
    let report = report();
    assert_eq!(
        imports_in(&report, "tests/t.rs"),
        [
            // the module is private to the crate: the function alone
            "1 src/parse.rs use via src/lib.rs:4 [parse]",
            // the module and the function, which `split::helper()` and
            // `split(..)` use
            "2 src/split.rs use [*, helper]",
            "2 src/split.rs use via src/lib.rs:5 [split]",
            // a path in an expression names the function
            "7 src/parse.rs path via src/lib.rs:4 [parse]",
        ]
    );
}

#[test]
fn a_call_reaches_the_function_a_module_shares_its_name_with() {
    let report = report();
    assert_eq!(
        uses_of(&report, "bakery::parse::parse"),
        [
            // the crate root's own call, through its re-export
            "src/lib.rs:8 call via 4",
            "tests/t.rs:6 call via 1",
            "tests/t.rs:7 call as bakery::parse",
        ]
    );
    assert_eq!(
        uses_of(&report, "bakery::split::split"),
        ["tests/t.rs:8 call via 2"]
    );
    assert_eq!(
        uses_of(&report, "bakery::split::helper"),
        ["tests/t.rs:9 call as split::helper via 2"]
    );
}

#[test]
fn a_type_and_a_value_of_one_name_are_two_symbols() {
    let report = report();
    let symbols = |name: &str| -> Vec<String> {
        report
            .graph
            .symbols
            .values()
            .filter(|s| s.name == name)
            .map(|s| format!("{} {:?}", s.id.as_str(), s.kind))
            .collect()
    };
    // the value's id names its kind; the type keeps its own
    assert_eq!(
        symbols("Pay"),
        ["bakery::fee::Pay Trait", "bakery::fee::Pay+fn Function"]
    );
    assert_eq!(
        symbols("Rate"),
        [
            "bakery::rate::Rate Struct",
            "bakery::rate::Rate+const Constant"
        ]
    );
    // a module with a file of its own is its component's id
    assert_eq!(
        symbols("bake"),
        ["bakery::bake Module", "bakery::bake+fn Function"]
    );
    // the namespace a path reads the name in tells them apart
    assert_eq!(
        uses_of(&report, "bakery::fee::Pay"),
        ["src/read.rs:11 type via 1"]
    );
    assert_eq!(
        uses_of(&report, "bakery::fee::Pay+fn"),
        ["src/read.rs:7 call via 1"]
    );
    assert_eq!(
        uses_of(&report, "bakery::bake+fn"),
        ["src/lib.rs:24 call", "src/lib.rs:24 call"]
    );
    // each is found by its kind on the line they share
    assert_eq!(
        uses_of(&report, "bakery::rate::Rate"),
        [
            "src/bill.rs:3 type via 1",
            "src/rate.rs:2 type",
            "src/rate.rs:2 new"
        ]
    );
    assert_eq!(
        uses_of(&report, "bakery::rate::Rate+const"),
        ["src/bill.rs:4 read via 1"]
    );
}

#[test]
fn a_method_is_its_type_s_however_the_impl_writes_its_path() {
    let report = report();
    let ids: Vec<&str> = report
        .graph
        .symbols
        .values()
        .filter(|s| s.name.ends_with("::make"))
        .map(|s| s.id.as_str())
        .collect();
    assert_eq!(ids, ["bakery::read::Order::make"]);
}
