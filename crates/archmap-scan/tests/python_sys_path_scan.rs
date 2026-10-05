//! The Python analyzer on `fixtures/python-sys-path`: an import that no
//! module and no file next to the importer gives resolves against the
//! directories a `conftest.py` above it adds to `sys.path`.

use std::path::Path;

use archmap_scan::{scan, ScanOptions};

#[test]
fn imports_resolve_against_the_directories_a_conftest_adds_to_sys_path() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/python-sys-path");
    let report = scan(&root, &ScanOptions::default()).expect("scan succeeds");
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let mut resolved: Vec<String> = report
        .graph
        .edges
        .iter()
        .flat_map(|edge| edge.evidence.iter().map(move |e| (edge, e)))
        .filter(|(_, e)| e.file == "suite/test_report.py")
        .map(|(edge, e)| {
            let names: Vec<&str> = e.names.iter().map(String::as_str).collect();
            format!(
                "{}:{} -> {} {names:?} {} | {}",
                e.file,
                e.line.unwrap(),
                e.target.as_deref().unwrap(),
                edge.to,
                e.note.as_deref().unwrap()
            )
        })
        .collect();
    resolved.sort();
    assert_eq!(
        resolved,
        [
            // a file next to the importer comes first, as before
            r#"suite/test_report.py:1 -> suite/helpers.py ["*"] almanac::suite | import helpers, next to the importing file (assumes its directory is on sys.path)"#,
            // what the file reads through the module it binds
            r#"suite/test_report.py:2 -> tasks/report.py ["total"] almanac::tasks | import report, in tasks, which suite/conftest.py:7 adds to sys.path"#,
            // a regular package in a later entry wins over a namespace
            // portion in an earlier one; a submodule taken by name
            r#"suite/test_report.py:3 -> src/vendor/tools/fmt.py ["money"] almanac::vendor.tools | import tools, in src/vendor, which suite/conftest.py:8 adds to sys.path"#,
            // a name the package's __init__.py passes on, followed to its file
            r#"suite/test_report.py:4 -> src/vendor/tools/__init__.py ["money"] almanac::vendor.tools | import tools, in src/vendor, which suite/conftest.py:8 adds to sys.path"#,
            r#"suite/test_report.py:4 -> src/vendor/tools/fmt.py ["money"] almanac::vendor.tools | import via src/vendor/tools/__init__.py:1"#,
        ]
    );

    // a path the scan cannot compute adds nothing, and a file outside the
    // conftest's directory is not below it
    let mut unmapped: Vec<String> = report
        .graph
        .unmapped_imports
        .iter()
        .map(|i| format!("{} {} {:?}", i.evidence.file, i.module, i.reason))
        .collect();
    unmapped.sort();
    assert_eq!(
        unmapped,
        [
            "bin/run.py report LocalName",
            "suite/test_report.py legacy LocalName",
        ]
    );
}
