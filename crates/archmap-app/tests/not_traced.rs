//! What `query` and `impact` could not follow for their target, counted
//! from what the analyzers record, and only when something applies.

use std::path::{Path, PathBuf};

use archmap_app::{Format, ImpactRequest, QueryRequest, ScanMode, Workspace, DEFAULT_DEPTH};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name)
}

fn scan(root: &Path) -> Workspace {
    Workspace::scan(root, ScanMode::Full).unwrap()
}

fn query(ws: &Workspace, target: &str, format: Format) -> String {
    ws.query(&QueryRequest {
        target,
        depth: DEFAULT_DEPTH,
        format,
        verbose: false,
    })
    .unwrap()
    .output
}

fn impact(ws: &Workspace, target: &str) -> serde_json::Value {
    let json = ws
        .impact(&ImpactRequest {
            target,
            depth: DEFAULT_DEPTH,
            verbose: false,
        })
        .unwrap()
        .output;
    serde_json::from_str(&json).unwrap()
}

#[test]
fn a_python_file_lists_dynamic_imports_elsewhere_and_imports_named_like_it() {
    let ws = scan(&fixture("simple-python-project"));
    let text = query(&ws, "scripts/helpers.py", Format::Text);
    let section = text.split_once("\nNot traced:\n").map(|(_, s)| s);
    assert_eq!(
        section,
        Some(
            "  dynamic: 1 call loads a module by a computed name, which may be this: \
             scripts/plugins.py:5\n  \
             named like it: 1 import of `helpers` maps to no file: tests/test_billing.py:3 \
             (local name)\n"
        ),
        "{text}"
    );
    let json: serde_json::Value =
        serde_json::from_str(&query(&ws, "scripts/helpers.py", Format::Json)).unwrap();
    assert_eq!(json["not_traced"]["dynamic"]["total"], 1);
    assert_eq!(json["not_traced"]["named_like"]["name"], "helpers");
    assert_eq!(
        json["not_traced"]["named_like"]["shown"][0]["module"],
        "helpers"
    );
}

#[test]
fn a_ts_file_counts_the_dynamic_imports_of_ts_and_js_and_an_alias_named_like_it() {
    let ws = scan(&fixture("simple-ts-project"));
    let text = query(&ws, "src/components/button.tsx", Format::Text);
    assert!(
        text.contains(
            "\n  dynamic: 2 calls load modules by computed names, which may be this: \
             scripts/report.cjs:4, src/app/lazy.tsx:7\n"
        ),
        "{text}"
    );
    assert!(
        text.contains(
            "\n  named like it: 1 import of `button` maps to no file: src/app/page.tsx:14 \
             (local name)\n"
        ),
        "{text}"
    );
}

#[test]
fn the_dynamic_imports_of_the_target_itself_stay_under_not_mapped() {
    let ws = scan(&fixture("simple-ts-project"));
    let text = query(&ws, "src/app/lazy.tsx", Format::Text);
    assert!(
        text.contains("\n  dynamic: 1 call loads a module by a computed name, which may be this: scripts/report.cjs:4\n"),
        "{text}"
    );
}

#[test]
fn a_rust_target_counts_the_files_its_analyzer_did_not_read() {
    let ws = scan(&fixture("simple-rust-workspace"));
    let text = query(&ws, "crates/app/src/config.rs", Format::Text);
    assert!(
        text.ends_with("\nNot traced:\n  not read: 1 of 9 rust files\n"),
        "{text}"
    );
    let json = impact(&ws, "crates/app/src/config.rs");
    assert_eq!(
        json["not_traced"]["not_read"],
        serde_json::json!({"language": "rust", "files": 9, "read": 8})
    );
}

#[test]
fn impact_says_when_no_import_can_show_who_uses_a_file() {
    let ws = scan(&fixture("simple-ts-project"));
    // a script's globals: no import names them
    assert_eq!(impact(&ws, "src/global.d.ts")["not_traced"]["script"], true);
    // an entry file that a framework loads by name: nothing imports it
    let page = impact(&ws, "src/app/page.tsx");
    assert_eq!(page["importers"]["total"], 0, "{page}");
    assert_eq!(page["not_traced"]["unreached"], true, "{page}");
}

#[test]
fn nothing_to_report_adds_nothing() {
    let dir = std::env::temp_dir().join(format!("archmap-not-traced-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("pkg")).unwrap();
    std::fs::write(dir.join("pkg/__init__.py"), "from pkg import core\n").unwrap();
    std::fs::write(dir.join("pkg/core.py"), "def run():\n    pass\n").unwrap();
    let ws = scan(&dir);
    let text = query(&ws, "pkg/core.py", Format::Text);
    let json = query(&ws, "pkg/core.py", Format::Json);
    let impact = impact(&ws, "pkg/core.py");
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(!text.contains("Not traced"), "{text}");
    assert!(!json.contains("not_traced"), "{json}");
    assert!(impact.get("not_traced").is_none(), "{impact}");
}
