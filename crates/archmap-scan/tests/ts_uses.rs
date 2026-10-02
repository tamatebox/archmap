//! Where a TS/JS symbol is used, read on demand from `fixtures/ts-uses`.

use std::path::{Path, PathBuf};

use archmap_core::{ArchitectureGraph, SymbolUses};
use archmap_scan::{scan, symbol_uses, ScanOptions};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/ts-uses")
}

fn graph() -> ArchitectureGraph {
    scan(&fixture(), &ScanOptions::default()).unwrap().graph
}

fn uses_of(graph: &ArchitectureGraph, name: &str) -> SymbolUses {
    let symbol = graph
        .symbols_named(name)
        .next()
        .unwrap_or_else(|| panic!("no symbol {name}"));
    symbol_uses(&fixture(), graph, symbol)
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
fn a_function_is_used_through_every_binding_that_reaches_it() {
    let graph = graph();
    let found = uses_of(&graph, "formatPrice");
    assert_eq!(
        shown(&found),
        [
            "scripts/cjs.cjs:4:23 call via 1",
            "scripts/cjs.cjs:4:48 call as money.formatPrice via 2",
            "scripts/lazy.mjs:3:10 call via 2",
            // the destructuring binds `g`, as an import binds a name
            "src/app.ts:11:11 call as fp via 1",
            "src/app.ts:11:20 call as m.formatPrice via 2",
            "src/app.ts:11:38 call as m.formatPrice via 2",
            "src/app.ts:11:57 call as g via 2",
            "src/hoisted.ts:1:28 call via 3",
            "src/money.ts:10:12 call",
            "src/types.ts:4:50 type via 4",
            "src/view.tsx:9:27 call as money.formatPrice via 1",
            "src/view.tsx:10:21 call via 1",
            "src/view.tsx:11:34 call as all.money.formatPrice via 2",
            "tests/money.test.ts:3:24 call via 1 (test)",
        ],
        "{:#?}",
        shown(&found)
    );
    // `send(m)` and the promise of `import()` may use it unseen
    assert_eq!(
        places(&found.escapes),
        ["scripts/lazy.mjs:7", "src/app.ts:8"],
        "{:?}",
        places(&found.escapes)
    );
    assert_eq!(places(&found.unused), ["src/unused.ts:1"]);
    let renamed: Vec<(String, &str)> = found
        .renamed
        .iter()
        .map(|r| {
            (
                format!("{}:{}", r.evidence.file, r.evidence.line.unwrap_or(0)),
                r.name.as_str(),
            )
        })
        .collect();
    assert_eq!(renamed, [("src/index.ts:3".to_owned(), "price")]);
    // two statements on one line that load different files
    let unread: Vec<String> = found
        .unread
        .iter()
        .map(|u| format!("{}:{} {}", u.file, u.line.unwrap_or(0), u.reason.as_str()))
        .collect();
    assert_eq!(unread, ["src/two.ts:1 ambiguous statement"]);
}

#[test]
fn a_class_is_used_by_new_by_its_static_members_and_in_types() {
    let graph = graph();
    assert_eq!(
        shown(&uses_of(&graph, "Wallet")),
        [
            "src/app.ts:9:17 type via 1",
            "src/app.ts:9:26 read via 1",
            "src/app.ts:10:7 new via 1",
            "src/money.ts:6:18 type",
            "src/money.ts:7:16 new",
            "src/money.ts:14:35 type",
            "src/money.ts:20:20 type",
            "src/types.ts:3:32 type via 1",
        ]
    );
}

#[test]
fn a_member_is_used_through_its_class_and_through_this_in_its_own_kind() {
    let graph = graph();
    // static: through the class, and `this` in a static method
    assert_eq!(
        shown(&uses_of(&graph, "Wallet.open")),
        [
            "src/app.ts:9:33 call via 1",
            "src/money.ts:21:17 call as this.open"
        ]
    );
    // instance: `this` in its own methods and their arrows, never in a
    // nested function, and never through a value (`new Wallet().pay(1)`)
    assert_eq!(
        shown(&uses_of(&graph, "Wallet.pay")),
        [
            "src/money.ts:13:10 call as this.pay",
            "src/money.ts:17:29 call as this.pay",
        ]
    );
}

#[test]
fn jsx_elements_count_once_and_statements_on_one_line_are_not_guessed() {
    let graph = graph();
    let found = uses_of(&graph, "Badge");
    assert_eq!(
        shown(&found),
        [
            "src/view.tsx:9:8 jsx via 3",
            "src/view.tsx:10:8 jsx via 3",
            "src/view.tsx:11:11 jsx as ui.Badge via 4",
        ]
    );
    let unread: Vec<String> = found
        .unread
        .iter()
        .map(|u| format!("{}:{} {}", u.file, u.line.unwrap_or(0), u.reason.as_str()))
        .collect();
    assert_eq!(unread, ["src/two.ts:1 ambiguous statement"]);
}

#[test]
fn a_default_export_and_a_renamed_export_are_used_by_their_local_names() {
    let graph = graph();
    // `export default total` is no use; the default import is
    assert_eq!(
        shown(&uses_of(&graph, "total")),
        ["src/app.ts:11:70 call via 3"]
    );
    // `export { rates as RATES }`: the uses of `rates`
    assert_eq!(
        shown(&uses_of(&graph, "RATES")),
        ["src/money.ts:29:46 read as rates"]
    );
}

#[test]
fn files_that_changed_since_the_scan_are_unread_with_the_reason() {
    let dir = std::env::temp_dir().join(format!("archmap-uses-stale-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let write = |path: &str, text: &str| std::fs::write(dir.join(path), text).unwrap();
    write("package.json", r#"{ "name": "stale" }"#);
    write(
        "src/a.ts",
        "export function f(): number {\n  return 1;\n}\n",
    );
    for name in ["moved", "broken", "gone", "kept"] {
        write(
            &format!("src/{name}.ts"),
            "import { f } from './a';\n\nexport const v = f();\n",
        );
    }
    let graph = scan(&dir, &ScanOptions::default()).unwrap().graph;
    // edited after the scan: the statement moved, the file no longer parses,
    // the file is gone
    write(
        "src/moved.ts",
        "// a new first line\nimport { f } from './a';\n\nexport const v = f();\n",
    );
    write(
        "src/broken.ts",
        "import { f } from './a';\nexport const v = f(;\n",
    );
    std::fs::remove_file(dir.join("src/gone.ts")).unwrap();
    let symbol = graph.symbols_named("f").next().unwrap();
    let found = symbol_uses(&dir, &graph, symbol);
    let unread: Vec<String> = found
        .unread
        .iter()
        .map(|u| format!("{}:{} {}", u.file, u.line.unwrap_or(0), u.reason.as_str()))
        .collect();
    assert_eq!(
        unread,
        [
            "src/broken.ts:0 parse error",
            "src/gone.ts:0 file gone",
            "src/moved.ts:1 statement not found",
        ]
    );
    assert_eq!(shown(&found), ["src/kept.ts:3:18 call via 1"]);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_language_without_a_pass_lists_its_files_as_unread() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/simple-python-project");
    let graph = scan(&root, &ScanOptions::default()).unwrap().graph;
    let symbol = graph
        .symbols
        .values()
        .find(|s| {
            graph
                .symbol_importers(s)
                .is_some_and(|i| !i.by_name.is_empty())
        })
        .expect("a python symbol that something imports");
    let found = symbol_uses(&root, &graph, symbol);
    assert!(found.uses.is_empty());
    assert!(!found.unread.is_empty());
    assert!(found
        .unread
        .iter()
        .all(|u| u.reason == archmap_core::UnreadReason::LanguageNotRead && u.line.is_none()));
}
