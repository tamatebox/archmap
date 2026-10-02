//! A symbol followed through names that barrels and its own file give it:
//! `fixtures/ts-renames` renames `formatPrice` to `fp` where it is
//! defined, to `price` in a barrel, to `cost` in a second, and back to
//! `formatPrice` in a third, and imports it under each name, through a
//! namespace of the second barrel, and through a barrel whose `export *`
//! sources disagree on a name.

use std::path::{Path, PathBuf};

use archmap_core::ArchitectureGraph;
use archmap_scan::{scan, symbol_uses, ScanOptions};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/ts-renames")
}

fn graph() -> ArchitectureGraph {
    scan(&fixture(), &ScanOptions::default()).unwrap().graph
}

#[test]
fn every_name_a_barrel_or_the_file_gives_the_symbol_leads_to_its_importers() {
    let graph = graph();
    let symbol = graph.symbols_named("formatPrice").next().unwrap();
    let found = graph.symbol_importers(symbol).unwrap();
    let place = |e: &archmap_core::Evidence| format!("{}:{}", e.file, e.line.unwrap_or(0));
    let mut by_name: Vec<String> = found.by_name.iter().map(|(_, e)| place(e)).collect();
    by_name.sort();
    by_name.dedup();
    assert_eq!(
        by_name,
        [
            // the defining file's own other name
            "src/alias.ts:1",
            // back to the first name, through three barrels
            "src/back-again.ts:1",
            "src/back.ts:1",
            "src/chain.ts:1",
            "src/chained.ts:1",
            "src/index.ts:1",
            // a barrel whose `export *` sources disagree on `cost`: no walk
            // reaches a definition, so the statement counts at the barrel,
            // with another name the walk led elsewhere or not
            "src/onename.ts:1",
            "src/rename.ts:1",
            "src/twonames.ts:1",
        ]
    );
    let mut may_use: Vec<String> = found.may_use.iter().map(|(_, e)| place(e)).collect();
    may_use.sort();
    // a namespace of the second barrel, and a barrel of it whole
    assert_eq!(may_use, ["src/both.ts:1", "src/namespace.ts:1"]);
    let through = |file: &str| found.through.get(&(file, Some(1))).copied();
    assert_eq!(through("src/namespace.ts"), Some("src/chain.ts"));
    assert_eq!(through("src/back.ts"), Some("src/chain.ts"));
    assert_eq!(through("src/chain.ts"), Some("src/index.ts"));
}

#[test]
fn the_uses_under_every_name_are_read() {
    let root = fixture();
    let graph = scan(&root, &ScanOptions::default()).unwrap();
    let symbol = graph.graph.symbols_named("formatPrice").next().unwrap();
    let found = symbol_uses(&graph, symbol);
    let mut shown: Vec<String> = found
        .uses
        .iter()
        .map(|u| {
            format!(
                "{}:{} {}",
                u.evidence.file,
                u.evidence.line.unwrap_or(0),
                u.binding.as_deref().unwrap_or("formatPrice")
            )
        })
        .collect();
    shown.sort();
    assert_eq!(
        shown,
        [
            "src/alias.ts:2 fp",
            "src/back-again.ts:2 formatPrice",
            "src/chained.ts:2 cost",
            "src/namespace.ts:2 all.cost",
            "src/onename.ts:2 cost",
            "src/rename.ts:2 price",
            "src/twonames.ts:2 cost",
        ]
    );
    assert!(
        found.unread.is_empty() && found.unused.is_empty(),
        "{found:#?}"
    );
}
