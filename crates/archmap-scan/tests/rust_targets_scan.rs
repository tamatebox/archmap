//! What the Rust analyzer marks as test code, by attribute and, for the
//! files of Cargo targets, by the kind of target.

use std::path::{Path, PathBuf};

use archmap_core::ArchitectureGraph;
use archmap_scan::{scan, ScanOptions};

fn write(root: &Path, file: &str, text: &str) {
    let path = root.join(file);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn repo(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "archmap-rust-targets-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn scan_repo(root: &Path) -> ArchitectureGraph {
    let report = scan(root, &ScanOptions::default()).expect("scan succeeds");
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    report.graph
}

/// Each symbol's id, and whether its evidence carries `test`.
fn symbol_marks(graph: &ArchitectureGraph) -> Vec<(String, bool)> {
    graph
        .symbols
        .values()
        .map(|s| (s.id.to_string(), s.evidence.iter().all(|e| e.test)))
        .collect()
}

#[test]
fn symbols_that_only_tests_compile_carry_test() {
    let root = repo("cfg-test-symbols");
    write(
        &root,
        "Cargo.toml",
        "[package]\nname = \"kiosk\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write(
        &root,
        "src/lib.rs",
        "\
pub fn production() {}

#[cfg(test)]
pub fn fixture() {}

pub struct Till;

impl Till {
    #[cfg(test)]
    pub fn sample() -> Self {
        Till
    }
}

#[cfg(test)]
pub mod support {
    pub fn helper() {}
}
",
    );
    let graph = scan_repo(&root);
    assert_eq!(
        symbol_marks(&graph),
        [
            ("kiosk::Till".to_owned(), false),
            ("kiosk::Till::sample".to_owned(), true),
            ("kiosk::fixture".to_owned(), true),
            ("kiosk::production".to_owned(), false),
            ("kiosk::support".to_owned(), true),
            ("kiosk::support::helper".to_owned(), true),
        ]
    );
    // a file that defines production code is no test, whatever else it holds
    assert_eq!(graph.test_code().get("src/lib.rs"), Some(&false));
}
