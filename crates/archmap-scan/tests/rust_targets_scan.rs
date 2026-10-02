//! What the Rust analyzer marks as test code, by attribute and, for the
//! files of Cargo targets, by the kind of target; and the targets of
//! `fixtures/rust-cargo-targets`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use archmap_core::{ArchitectureGraph, ChangeSeed, ComponentId, EdgeKind, LanguageCoverage};
use archmap_scan::{scan, ScanOptions};

fn fixture() -> ArchitectureGraph {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/rust-cargo-targets");
    scan_repo(&root.canonicalize().expect("fixture exists"))
}

/// The import evidence written in `file`: (line, target file, test).
fn imports_in(graph: &ArchitectureGraph, file: &str) -> BTreeSet<(u32, String, bool)> {
    graph
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Import)
        .flat_map(|e| &e.evidence)
        .filter(|e| e.file == file)
        .map(|e| {
            (
                e.line.unwrap_or_default(),
                e.target.clone().unwrap_or_default(),
                e.test,
            )
        })
        .collect()
}

#[test]
fn every_cargo_target_is_read_and_test_data_is_not() {
    let graph = fixture();
    // kiosk/tests/ui/broken.rs, which no target reaches, is neither parsed
    // nor warned about
    assert_eq!(
        graph.meta.coverage["rust"],
        LanguageCoverage {
            files: 11,
            read: Some(10),
            scripts: 0
        }
    );
    // the roots belong to their package; a module a test loads is a
    // component named by its path, under the package
    let components: Vec<(&str, &str, &str, Option<&str>)> = graph
        .components
        .values()
        .map(|c| {
            (
                c.id.as_str(),
                c.name.as_str(),
                c.path.as_deref().unwrap_or_default(),
                c.parent.as_ref().map(ComponentId::as_str),
            )
        })
        .collect();
    assert_eq!(
        components,
        [
            ("depot", "depot", "depot", None),
            ("kiosk", "kiosk", "kiosk", None),
            (
                "kiosk::tests/common/mod.rs",
                "tests/common/mod.rs",
                "kiosk/tests/common/mod.rs",
                Some("kiosk")
            ),
            (
                "kiosk::till",
                "kiosk::till",
                "kiosk/src/till.rs",
                Some("kiosk")
            ),
        ]
    );
}

#[test]
fn the_symbols_of_other_targets_take_their_files() {
    let graph = fixture();
    assert_eq!(
        symbol_marks(&graph),
        [
            ("depot::idle".to_owned(), false),
            ("kiosk::helper".to_owned(), false),
            // a test's `pub fn helper` meets neither the library's nor the
            // other test's
            ("kiosk::tests/common/mod.rs::call".to_owned(), true),
            ("kiosk::tests/common/mod.rs::setup".to_owned(), true),
            ("kiosk::tests/common/mod.rs::support".to_owned(), true),
            ("kiosk::tests/common/mod.rs::support::aid".to_owned(), true),
            ("kiosk::tests/shared.rs::warm".to_owned(), true),
            ("kiosk::tests/stock.rs::helper".to_owned(), true),
            ("kiosk::tests/total.rs::helper".to_owned(), true),
            // `pub mod till;` is the module's component
            ("kiosk::till".to_owned(), false),
            ("kiosk::till::sum".to_owned(), false),
            ("kiosk::total".to_owned(), false),
        ]
    );
}

#[test]
fn a_test_target_keeps_every_import_of_its_package_as_test_code() {
    let graph = fixture();
    let row = |line: u32, target: &str| (line, target.to_owned(), true);
    // `mod common;` and `mod shared;` are its own modules; the library by
    // its crate name, through `use` and through paths inside `#[test] fn`
    assert_eq!(
        imports_in(&graph, "kiosk/tests/total.rs"),
        BTreeSet::from([
            row(4, "kiosk/src/lib.rs"),
            row(11, "kiosk/tests/common/mod.rs"),
            row(12, "kiosk/tests/shared.rs"),
        ])
    );
    assert_eq!(
        imports_in(&graph, "kiosk/tests/stock.rs"),
        BTreeSet::from([
            row(7, "kiosk/src/till.rs"),
            row(8, "kiosk/tests/common/mod.rs"),
        ])
    );
    // a helper two tests load resolves `crate::` in each of them
    assert_eq!(
        imports_in(&graph, "kiosk/tests/common/mod.rs"),
        BTreeSet::from([
            row(4, "kiosk/tests/stock.rs"),
            row(4, "kiosk/tests/total.rs"),
        ])
    );
    // examples and benches are test code too; the build script is not
    assert_eq!(
        imports_in(&graph, "kiosk/examples/demo.rs"),
        BTreeSet::from([row(1, "kiosk/src/lib.rs")])
    );
    assert_eq!(
        imports_in(&graph, "kiosk/benches/speed.rs"),
        BTreeSet::from([row(1, "kiosk/src/till.rs")])
    );
}

#[test]
fn impact_lists_the_tests_examples_and_benches_that_reach_a_change() {
    let graph = fixture();
    let reach = graph.change_impact(ChangeSeed::File("kiosk/src/till.rs"), 2);
    assert_eq!(
        reach.tests.iter().map(String::as_str).collect::<Vec<_>>(),
        [
            "kiosk/benches/speed.rs",
            "kiosk/examples/demo.rs",
            // the helper calls `crate::helper` of stock.rs, which reaches the
            // change; listing helpers apart is #70
            "kiosk/tests/common/mod.rs",
            "kiosk/tests/stock.rs",
            "kiosk/tests/total.rs",
        ]
    );
    // depot links the library; its manifest is no test (#77)
    assert!(reach.transitive.contains(&ComponentId::new("depot")));
    let helper = graph.change_impact(ChangeSeed::File("kiosk/tests/common/mod.rs"), 2);
    assert_eq!(
        helper.tests.iter().map(String::as_str).collect::<Vec<_>>(),
        [
            "kiosk/tests/common/mod.rs",
            "kiosk/tests/stock.rs",
            "kiosk/tests/total.rs",
        ]
    );
    assert!(helper.direct.is_empty() && helper.transitive.is_empty());
}

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
