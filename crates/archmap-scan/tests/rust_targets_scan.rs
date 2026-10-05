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
            files: 26,
            read: Some(23),
            scripts: 0
        }
    );
    // the roots belong to their package; a module a test or a binary under
    // `src/bin/` loads is a component named by its path, under the package
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
                "kiosk::clock",
                "kiosk::clock",
                "kiosk/src/clock.rs",
                Some("kiosk")
            ),
            // a binary's `util` and the library's, apart
            (
                "kiosk::src/bin/tool/util.rs",
                "src/bin/tool/util.rs",
                "kiosk/src/bin/tool/util.rs",
                Some("kiosk")
            ),
            (
                "kiosk::stamp",
                "kiosk::stamp",
                "kiosk/src/stamp.rs",
                Some("kiosk")
            ),
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
            (
                "kiosk::util",
                "kiosk::util",
                "kiosk/src/util.rs",
                Some("kiosk")
            ),
            ("ledger", "ledger", "ledger", None),
            // modules of the library at `[lib] path`
            (
                "ledger::cli",
                "ledger::cli",
                "ledger/lib/cli.rs",
                Some("ledger")
            ),
            (
                "ledger::entry",
                "ledger::entry",
                "ledger/lib/entry.rs",
                Some("ledger")
            ),
            // the binary's `mod cli;`, which is another file there, by its path
            (
                "ledger::src/cli.rs",
                "src/cli.rs",
                "ledger/src/cli.rs",
                Some("ledger")
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
            ("kiosk::clock".to_owned(), false),
            ("kiosk::clock::now".to_owned(), false),
            ("kiosk::counted".to_owned(), false),
            ("kiosk::helper".to_owned(), false),
            ("kiosk::named".to_owned(), false),
            ("kiosk::report".to_owned(), false),
            // a binary's symbols are production code under their files too
            ("kiosk::src/bin/report.rs::helper".to_owned(), false),
            ("kiosk::src/bin/tool/util.rs::run".to_owned(), false),
            ("kiosk::stamp".to_owned(), false),
            ("kiosk::stamp::mark".to_owned(), false),
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
            ("kiosk::util".to_owned(), false),
            ("kiosk::util::shared".to_owned(), false),
            ("ledger::cli".to_owned(), false),
            ("ledger::cli::lib_side".to_owned(), false),
            ("ledger::entry".to_owned(), false),
            ("ledger::entry::post".to_owned(), false),
            ("ledger::src/cli.rs::run".to_owned(), false),
        ]
    );
}

#[test]
fn the_targets_a_manifest_declares_take_the_place_of_the_ones_cargo_finds() {
    let graph = fixture();
    let row = |line: u32, target: &str, test: bool| (line, target.to_owned(), test);
    // `[[bin]]` at its path, the library at `[lib] path`
    assert_eq!(
        imports_in(&graph, "ledger/tools/audit.rs"),
        BTreeSet::from([row(1, "ledger/lib/entry.rs", false)])
    );
    // a `[[test]]` at its path is test code
    assert_eq!(
        imports_in(&graph, "ledger/checks/books.rs"),
        BTreeSet::from([row(1, "ledger/lib/entry.rs", true)])
    );
    // `src/main.rs` stays the package's binary under its new name
    assert_eq!(
        imports_in(&graph, "ledger/src/main.rs"),
        BTreeSet::from([
            row(4, "ledger/src/cli.rs", false),
            row(5, "ledger/lib/entry.rs", false),
        ])
    );
    // `autotests = false` and `build = false`: tests/ignored.rs and
    // build.rs are no targets, so not read
    for file in ["ledger/tests/ignored.rs", "ledger/build.rs"] {
        assert!(imports_in(&graph, file).is_empty(), "{file}");
    }
    let reach = graph.change_impact(ChangeSeed::File("ledger/lib/entry.rs"), 2);
    assert_eq!(
        reach.tests.iter().map(String::as_str).collect::<Vec<_>>(),
        ["ledger/checks/books.rs"]
    );
}

#[test]
fn a_binary_under_src_bin_is_a_production_crate_of_its_own() {
    let graph = fixture();
    let row = |line: u32, target: &str, test: bool| (line, target.to_owned(), test);
    // its own `mod util;` and the library's `util`, apart
    assert_eq!(
        imports_in(&graph, "kiosk/src/bin/tool/main.rs"),
        BTreeSet::from([
            row(4, "kiosk/src/bin/tool/util.rs", false),
            row(5, "kiosk/src/util.rs", false),
        ])
    );
    // its unit tests use the library, another crate, as test code
    assert_eq!(
        imports_in(&graph, "kiosk/src/bin/report.rs"),
        BTreeSet::from([
            row(1, "kiosk/src/clock.rs", false),
            row(23, "kiosk/src/stamp.rs", true),
        ])
    );
    // reached through its unit tests alone, the binary is a test to run
    // again, and stands for no package whose dependents would follow
    let reach = graph.change_impact(ChangeSeed::File("kiosk/src/stamp.rs"), 2);
    assert_eq!(
        reach.tests.iter().map(String::as_str).collect::<Vec<_>>(),
        ["kiosk/src/bin/report.rs"]
    );
    assert!(reach.direct.is_empty() && reach.transitive.is_empty());
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

#[test]
fn targets_the_manifest_cannot_give_are_reported() {
    let root = repo("unread-targets");
    write(
        &root,
        "Cargo.toml",
        "[package]\nname = \"odd\"\nversion = \"0.1.0\"\n\n[[bin]]\nname = \"x\"\npath = 7\n",
    );
    write(&root, "src/main.rs", "fn main() {}\n");
    let report = scan(&root, &ScanOptions::default()).expect("scan succeeds");
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.starts_with("Cargo.toml: targets not read: ")
                && w.ends_with("; Cargo's default targets assumed")),
        "{:?}",
        report.warnings
    );
    // the package and its default targets stay
    assert!(report.graph.component(&ComponentId::new("odd")).is_some());
}

#[test]
fn only_the_library_stands_for_its_package_toward_dependents() {
    let graph = fixture();
    let ids = |set: &std::collections::BTreeSet<ComponentId>| {
        set.iter().map(|c| c.to_string()).collect::<Vec<_>>()
    };
    // depot links the library, so a change the library's root reaches
    // reaches depot
    let till = graph.change_impact(ChangeSeed::File("kiosk/src/till.rs"), 2);
    assert!(ids(&till.transitive).contains(&"depot".to_owned()));
    // a module only a binary uses, and the binary itself, do not
    let clock = graph.change_impact(ChangeSeed::File("kiosk/src/clock.rs"), 2);
    assert_eq!(ids(&clock.transitive), ["kiosk"]);
    let report = graph.change_impact(ChangeSeed::File("kiosk/src/bin/report.rs"), 2);
    assert!(report.direct.is_empty() && report.transitive.is_empty());
}

#[test]
fn macro_arguments_are_read_and_the_rest_recorded() {
    let graph = fixture();
    // `format!("{:?}", util::shared())`: a path in a macro's expressions
    assert!(imports_in(&graph, "kiosk/src/lib.rs").contains(&(
        22,
        "kiosk/src/util.rs".to_owned(),
        false
    )));
    // `tally!(stamp::mark => 1)`: no expressions, so recorded as not read,
    // with the names its paths write
    let calls: Vec<(&str, &str, Option<u32>, Vec<&str>)> = graph
        .unread_macros
        .iter()
        .map(|m| {
            (
                m.from.as_str(),
                m.name.as_str(),
                m.evidence.line,
                m.names.iter().map(String::as_str).collect(),
            )
        })
        .collect();
    assert_eq!(
        calls,
        [
            // `macro_rules!` definitions, whose bodies are not read
            ("kiosk", "macro_rules", Some(5), vec![]),
            ("kiosk", "macro_rules", Some(6), vec![]),
            // tokens to print, never read
            (
                "kiosk",
                "stringify",
                Some(32),
                vec!["clock", "crate", "now"]
            ),
            ("kiosk", "tally", Some(27), vec!["mark", "stamp"]),
            ("kiosk", "tally2", Some(18), vec!["kiosk", "sum", "till"]),
        ]
    );
}
