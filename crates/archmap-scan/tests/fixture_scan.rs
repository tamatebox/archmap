//! End-to-end scan of `fixtures/simple-rust-workspace`.

use std::path::{Path, PathBuf};

use archmap_core::{ArchitectureGraph, ComponentId, ComponentKind, EdgeKind, SymbolKind};
use archmap_scan::{scan, ScanOptions};

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/simple-rust-workspace")
        .canonicalize()
        .expect("fixture exists")
}

fn scan_fixture() -> ArchitectureGraph {
    let report = scan(&fixture_root(), &ScanOptions::default()).expect("scan succeeds");
    assert!(
        report.warnings.is_empty(),
        "unexpected warnings: {:?}",
        report.warnings
    );
    report.graph
}

fn id(s: &str) -> ComponentId {
    ComponentId::new(s)
}

#[test]
fn detects_packages_and_external_dependencies() {
    let graph = scan_fixture();

    let app = graph.component(&id("app")).expect("app component");
    assert_eq!(app.kind, ComponentKind::Package);
    assert_eq!(app.language.as_deref(), Some("rust"));
    assert_eq!(app.path.as_deref(), Some("crates/app"));

    let serde = graph.component(&id("ext:serde")).expect("serde external");
    assert_eq!(serde.kind, ComponentKind::External);

    // dev-dependencies are not architecture facts for now
    assert!(graph.component(&id("ext:assert_cmd")).is_none());
    assert_eq!(graph.meta.analyzers, vec!["rust".to_owned()]);
}

#[test]
fn manifest_dependency_resolves_through_workspace_to_internal_package() {
    let graph = scan_fixture();
    let dep = graph
        .edges
        .iter()
        .find(|e| e.from == id("app") && e.to == id("lib_core") && e.kind == EdgeKind::Dependency)
        .expect("app depends on lib_core via manifest");
    assert_eq!(dep.evidence[0].file, "crates/app/Cargo.toml");
}

#[test]
fn use_statements_become_one_import_edge_with_all_evidence() {
    let graph = scan_fixture();
    let imports: Vec<_> = graph
        .edges
        .iter()
        .filter(|e| e.from == id("app") && e.to == id("lib_core") && e.kind == EdgeKind::Import)
        .collect();
    assert_eq!(
        imports.len(),
        1,
        "same relationship collapses into one edge"
    );

    let mut locations: Vec<(String, Option<u32>)> = imports[0]
        .evidence
        .iter()
        .map(|e| (e.file.clone(), e.line))
        .collect();
    locations.sort();
    assert_eq!(
        locations,
        vec![
            ("crates/app/src/config.rs".to_owned(), Some(1)),
            ("crates/app/src/main.rs".to_owned(), Some(1)),
            ("crates/app/src/main.rs".to_owned(), Some(2)),
        ]
    );
}

#[test]
fn only_public_items_become_symbols() {
    let graph = scan_fixture();
    let names: Vec<&str> = graph
        .symbols_of(&id("lib_core"))
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(
        names,
        vec![
            "User",
            "User::new",
            "billing",
            "CURRENCY",
            "Charge",
            "greet"
        ]
    );

    let greet = graph.symbol(&"lib_core::greet".into()).unwrap();
    assert_eq!(greet.kind, SymbolKind::Function);
    assert_eq!(
        greet.signature.as_deref(),
        Some("pub fn greet(user: &User) -> String")
    );
    assert_eq!(greet.evidence[0].file, "crates/lib_core/src/lib.rs");
    assert_eq!(greet.evidence[0].line, Some(21));

    let charge = graph.symbol(&"lib_core::billing::Charge".into()).unwrap();
    assert_eq!(charge.kind, SymbolKind::Trait);

    // private fn, private method, `pub fn` inside a private module: excluded
    assert!(graph.symbols_named("private_helper").next().is_none());
    assert!(graph.symbols_named("User::secret").next().is_none());
    assert!(graph.symbols_named("hidden").next().is_none());
}

#[test]
fn impact_of_lib_core_reaches_app() {
    let graph = scan_fixture();
    let affected = graph.transitive_dependents(&id("lib_core"));
    let affected: Vec<&str> = affected.iter().map(|c| c.as_str()).collect();
    assert_eq!(affected, vec!["app"]);
    assert_eq!(
        graph
            .component_for_path("crates/app/src/config.rs")
            .unwrap()
            .id,
        id("app")
    );
}

#[test]
fn manifests_only_skips_symbols_but_keeps_dependencies() {
    let options = ScanOptions {
        manifests_only: true,
    };
    let graph = scan(&fixture_root(), &options).unwrap().graph;
    assert!(graph.symbols.is_empty());
    assert!(graph.edges.iter().all(|e| e.kind == EdgeKind::Dependency));
    assert_eq!(graph.components.len(), 3);
}

#[test]
fn scan_is_deterministic() {
    let a = scan_fixture();
    let b = scan_fixture();
    assert_eq!(a, b);
}

#[test]
fn missing_root_is_an_error() {
    let result = scan(Path::new("/definitely/not/here"), &ScanOptions::default());
    assert!(result.is_err());
}
