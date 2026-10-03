//! Where a Rust symbol is used, read on demand from `fixtures/rust-uses`
//! with the resolver the scan built.

use std::path::{Path, PathBuf};

use archmap_core::{SymbolUses, UnreadReason};
use archmap_scan::{scan, symbol_uses, ScanOptions, ScanReport};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/rust-uses")
}

fn report() -> ScanReport {
    scan(&fixture(), &ScanOptions::default()).unwrap()
}

fn uses_of(report: &ScanReport, id: &str) -> SymbolUses {
    let symbol = report
        .graph
        .symbols
        .values()
        .find(|s| s.id.as_str() == id)
        .unwrap_or_else(|| panic!("no symbol {id}"));
    symbol_uses(report, symbol)
}

/// Each use as `file:line:column role`, with ` as <binding>`, ` via
/// <statement line>` and ` (test)` when they apply.
fn shown(found: &SymbolUses) -> Vec<String> {
    found
        .uses
        .iter()
        .map(|u| {
            let mut line = format!(
                "{}:{}:{} {}",
                u.evidence.file,
                u.evidence.line.unwrap_or(0),
                u.column,
                u.role.as_str()
            );
            if let Some(binding) = &u.binding {
                line.push_str(&format!(" as {binding}"));
            }
            if let Some(statement) = &u.statement {
                line.push_str(&format!(" via {}", statement.line));
            }
            if u.evidence.test {
                line.push_str(" (test)");
            }
            line
        })
        .collect()
}

fn places(list: &[archmap_core::Evidence]) -> Vec<String> {
    list.iter()
        .map(|e| format!("{}:{}", e.file, e.line.unwrap_or(0)))
        .collect()
}

#[test]
fn a_function_is_used_through_every_path_that_resolves_to_it() {
    let report = report();
    let found = uses_of(&report, "graphlib::graph::build");
    assert_eq!(
        shown(&found),
        [
            // a glob of the module, and a block's own glob
            "app/src/globonly.rs:4:5 call via 1",
            "app/src/globonly.rs:9:5 call via 8",
            "app/src/main.rs:7:13 call via 2",
            "app/src/main.rs:8:16 call as g::build via 1",
            // the name a re-export gives it
            "app/src/main.rs:9:13 call as make via 3",
            "app/src/main.rs:10:30 call as graphlib::graph::build",
            // a block's own `use`, which the module's resolution leaves out
            "app/src/main.rs:17:17 call as local via 16",
            // a macro argument
            "app/src/main.rs:19:18 call via 2",
            // after the bindings of `if let`, a match arm, `for` and `while
            // let`, which live in their branches, and in an item nested in a
            // function
            "app/src/scopes.rs:8:13 call via 1",
            "app/src/scopes.rs:14:13 call via 1",
            "app/src/scopes.rs:18:13 call via 1",
            "app/src/scopes.rs:28:9 call via 1",
            // a test target
            "app/tests/it.rs:5:16 call via 1 (test)",
            // `mod tests { use super::*; }`: a glob in an inline module
            "graphlib/src/graph.rs:29:20 call via 25 (test)",
            "graphlib/src/other.rs:2:19 call as crate::graph::build",
            "graphlib/src/other.rs:2:49 call as super::graph::build",
        ],
        "{:#?}",
        shown(&found)
    );
    // a closure parameter named `build` hides it at main.rs:13, and in
    // scopes.rs a `let ... else` binding (22), a block's own `fn build` (34)
    // and a block's `use` of another item (40)
    assert!(
        found.unused.is_empty() && found.unread.is_empty(),
        "{found:#?}"
    );
}

#[test]
fn a_type_is_used_by_construction_in_types_and_as_a_qualifier() {
    let report = report();
    let found = uses_of(&report, "graphlib::graph::Edge");
    assert_eq!(
        shown(&found),
        [
            "app/src/main.rs:11:13 new via 4",
            "app/src/main.rs:12:13 read as Edge::new via 4",
            "graphlib/src/graph.rs:5:6 type",
            // built as `Self { .. }` in its own impl
            "graphlib/src/graph.rs:7:9 new as Self",
            "graphlib/src/graph.rs:19:25 type",
            "graphlib/src/graph.rs:20:5 read as Edge::new",
            "graphlib/src/graph.rs:35:18 type",
            "graphlib/src/graph.rs:41:23 type",
            // `<Edge>::new(1)`
            "graphlib/src/graph.rs:42:6 read as Edge::new",
        ]
    );
    // the modules bound whole never name it
    assert_eq!(
        places(&found.unused),
        [
            "app/src/globonly.rs:1",
            "app/src/globonly.rs:8",
            "app/src/main.rs:1"
        ]
    );
}

#[test]
fn a_method_is_used_through_its_type_self_and_self_value() {
    let report = report();
    // an associated function: through the type and `Self`, in an impl of a
    // trait for the type too
    assert_eq!(
        shown(&uses_of(&report, "graphlib::graph::Edge::new")),
        [
            "app/src/main.rs:12:19 call via 4",
            "graphlib/src/graph.rs:15:15 call as Self::new",
            "graphlib/src/graph.rs:20:11 call",
            "graphlib/src/graph.rs:37:15 call as Self::new",
            "graphlib/src/graph.rs:42:13 call",
        ]
    );
    // a method taking `self`: only `self.weight()`; `a.weight()` and
    // `Self::new(..).weight()` go through values, so the type's importers
    // are values, never a negative fact
    let weight = uses_of(&report, "graphlib::graph::Edge::weight");
    assert_eq!(
        shown(&weight),
        ["graphlib/src/graph.rs:15:46 call as self.weight"]
    );
    assert!(weight.unused.is_empty());
    assert_eq!(
        places(&weight.values),
        [
            "app/src/globonly.rs:1",
            "app/src/globonly.rs:8",
            "app/src/main.rs:1",
            "app/src/main.rs:4"
        ]
    );
}

#[test]
fn a_constant_in_a_pattern_is_used_and_binds_nothing() {
    let report = report();
    let found = uses_of(&report, "graphlib::graph::LIMIT");
    assert_eq!(shown(&found), ["app/src/scopes.rs:46:9 read via 2"]);
}

#[test]
fn every_listed_statement_ends_in_one_of_the_lists() {
    let report = report();
    let graph = &report.graph;
    for symbol in graph.symbols.values() {
        let Some(importers) = graph.symbol_importers(symbol) else {
            continue;
        };
        let found = symbol_uses(&report, symbol);
        let defining = symbol.location().map(|e| e.file.as_str());
        for (_, statement) in importers.by_name.iter().chain(&importers.may_use) {
            let (file, line) = (statement.file.as_str(), statement.line);
            if Some(file) == defining {
                continue;
            }
            let at = |e: &archmap_core::Evidence| e.file == file && e.line == line;
            let ended = found.uses.iter().any(|u| {
                u.evidence.file == file && u.statement.as_ref().is_none_or(|s| Some(s.line) == line)
            }) || found.unused.iter().any(at)
                || found.passed_on.iter().any(at)
                || found.values.iter().any(at)
                || found.unread.iter().any(|u| u.file == file);
            assert!(
                ended,
                "{} at {file}:{line:?} ends in no list: {found:#?}",
                symbol.id
            );
        }
    }
}

#[test]
fn a_file_that_changed_since_the_scan_is_not_read() {
    let dir = std::env::temp_dir().join(format!("archmap-rust-uses-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let write = |path: &str, text: &str| std::fs::write(dir.join(path), text).unwrap();
    write(
        "Cargo.toml",
        "[package]\nname = \"one\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write("src/lib.rs", "pub mod a;\npub mod b;\n");
    write("src/a.rs", "pub fn f() -> u32 {\n    1\n}\n");
    write(
        "src/b.rs",
        "use crate::a::f;\n\npub fn g() -> u32 {\n    f()\n}\n",
    );
    let report = scan(&dir, &ScanOptions::default()).unwrap();
    // an inline module put in front shifts what the scan numbered
    write(
        "src/b.rs",
        "mod extra {}\nuse crate::a::f;\n\npub fn g() -> u32 {\n    f()\n}\n",
    );
    let symbol = report.graph.symbols_named("f").next().unwrap();
    let found = symbol_uses(&report, symbol);
    assert!(found.uses.is_empty(), "{found:#?}");
    assert!(found
        .unread
        .iter()
        .any(|u| u.file == "src/b.rs" && u.reason == UnreadReason::Changed));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_symbol_no_pass_reads_lists_its_files_as_unread() {
    // a module, which `query` shows as a component of its own
    let report = report();
    let symbol = report
        .graph
        .symbols
        .values()
        .find(|s| s.kind == archmap_core::SymbolKind::Module)
        .expect("a module symbol");
    let found = symbol_uses(&report, symbol);
    assert!(found.uses.is_empty());
    assert!(!found.unread.is_empty());
    assert!(found
        .unread
        .iter()
        .all(|u| u.reason == archmap_core::UnreadReason::LanguageNotRead && u.line.is_none()));
}
