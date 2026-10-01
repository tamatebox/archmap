//! End-to-end scan of `fixtures/simple-rust-workspace`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use archmap_core::rules::{CycleRule, FileLevel, Finding, RuleSet};
use archmap_core::signals::{FileDependence, FileUse, Signal};
use archmap_core::{
    ArchitectureGraph, ChangeSeed, ComponentId, ComponentKind, EdgeKind, Evidence,
    LanguageCoverage, Scope, SymbolKind, UnmappedImport, UnmappedReason,
};
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

fn ids(list: &[&str]) -> Vec<ComponentId> {
    list.iter().map(|s| id(s)).collect()
}

#[test]
fn detects_packages_and_external_dependencies() {
    let graph = scan_fixture();

    let app = graph.component(&id("app")).expect("app component");
    assert_eq!(app.kind, ComponentKind::Package);
    assert_eq!(app.language.as_deref(), Some("rust"));
    assert_eq!(app.path.as_deref(), Some("crates/app"));

    let serde = graph
        .component(&id("ext:cargo:serde"))
        .expect("serde external");
    assert_eq!(serde.kind, ComponentKind::External);

    // dev-dependencies are not architecture facts for now
    assert!(graph.component(&id("ext:cargo:assert_cmd")).is_none());
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
fn modules_become_components_under_the_file_that_declares_them() {
    let graph = scan_fixture();
    let lib = "crates/lib_core/src/lib.rs";
    // (id, path, parent, the `mod` declaration)
    let expected = [
        (
            "app::config",
            "crates/app/src/config.rs",
            "app",
            ("crates/app/src/main.rs", 4),
        ),
        (
            "lib_core::billing",
            "crates/lib_core/src/billing.rs",
            "lib_core",
            (lib, 4),
        ),
        (
            "lib_core::billing::invoice",
            "crates/lib_core/src/billing/invoice.rs",
            "lib_core::billing",
            ("crates/lib_core/src/billing.rs", 1),
        ),
        (
            "lib_core::store",
            "crates/lib_core/src/store/mod.rs",
            "lib_core",
            (lib, 5),
        ),
        (
            "lib_core::store::memory",
            "crates/lib_core/src/store/memory.rs",
            "lib_core::store",
            ("crates/lib_core/src/store/mod.rs", 1),
        ),
        // declared inside the inline `pub mod api { .. }`: the inline module
        // is no component, but it stays in the path
        (
            "lib_core::api::v1",
            "crates/lib_core/src/api/v1.rs",
            "lib_core",
            (lib, 10),
        ),
    ];
    for (module, path, parent, (file, line)) in expected {
        let c = graph
            .component(&id(module))
            .unwrap_or_else(|| panic!("{module}"));
        assert_eq!(c.kind, ComponentKind::Module, "{module}");
        // the name is the path a `use` writes
        assert_eq!(c.name, module, "{module}");
        assert_eq!(c.language.as_deref(), Some("rust"), "{module}");
        assert_eq!(c.path.as_deref(), Some(path), "{module}");
        assert_eq!(c.parent, Some(id(parent)), "{module}");
        assert_eq!(
            c.evidence,
            vec![Evidence::new(file).at_line(line).with_note("mod")],
            "{module}"
        );
    }
    let modules = graph
        .components
        .values()
        .filter(|c| c.kind == ComponentKind::Module)
        .count();
    assert_eq!(modules, expected.len(), "inline modules are no components");

    assert_eq!(graph.depth_of(&id("lib_core::billing::invoice")), 2);
    assert_eq!(graph.depth_of(&id("lib_core::api::v1")), 1);
    let owner = |file: &str| graph.component_for_path(file).unwrap().id.clone();
    assert_eq!(
        owner("crates/lib_core/src/store/mod.rs"),
        id("lib_core::store")
    );
    assert_eq!(owner("crates/lib_core/src/lib.rs"), id("lib_core"));
    assert_eq!(owner("crates/app/src/main.rs"), id("app"));
}

/// Every import as `(from, to, file:line, target, note, scope)`.
fn imports(
    graph: &ArchitectureGraph,
) -> Vec<(String, String, String, Option<String>, String, Scope)> {
    let mut out: Vec<_> = graph
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Import)
        .flat_map(|edge| {
            edge.evidence.iter().map(move |e| {
                (
                    edge.from.to_string(),
                    edge.to.to_string(),
                    format!("{}:{}", e.file, e.line.unwrap()),
                    e.target.clone(),
                    e.note.clone().unwrap(),
                    e.scope.unwrap(),
                )
            })
        })
        .collect();
    out.sort();
    out
}

#[test]
fn every_use_and_module_path_names_the_file_it_imports() {
    let graph = scan_fixture();
    let lib = Some("crates/lib_core/src/lib.rs".to_owned());
    let billing = Some("crates/lib_core/src/billing.rs".to_owned());
    let row = |from: &str, to: &str, at: &str, target: &Option<String>, note: &str, scope| {
        (
            from.to_owned(),
            to.to_owned(),
            at.to_owned(),
            target.clone(),
            note.to_owned(),
            scope,
        )
    };
    let module = Scope::Module;
    assert_eq!(
        imports(&graph),
        vec![
            // a child module called by path, with no `use`
            row(
                "app",
                "app::config",
                "crates/app/src/main.rs:9",
                &Some("crates/app/src/config.rs".to_owned()),
                "path",
                Scope::Local
            ),
            // another crate, by its name
            row(
                "app",
                "lib_core",
                "crates/app/src/main.rs:2",
                &lib,
                "use",
                module
            ),
            row(
                "app",
                "lib_core::billing",
                "crates/app/src/main.rs:1",
                &billing,
                "use",
                module
            ),
            row(
                "app::config",
                "lib_core",
                "crates/app/src/config.rs:1",
                &lib,
                "use",
                module
            ),
            // the same dependency by `use` and by path: two pieces of evidence
            row(
                "app::config",
                "lib_core",
                "crates/app/src/config.rs:12",
                &lib,
                "path",
                Scope::Local
            ),
            // through the `pub use` in lib.rs, to the file that defines it
            row(
                "app::config",
                "lib_core::billing::invoice",
                "crates/app/src/config.rs:1",
                &Some("crates/lib_core/src/billing/invoice.rs".to_owned()),
                "use via crates/lib_core/src/lib.rs:7",
                module
            ),
            // an external crate names no file
            row(
                "lib_core",
                "ext:cargo:serde",
                "crates/lib_core/src/lib.rs:2",
                &None,
                "use",
                module
            ),
            // `crate::`
            row(
                "lib_core::api::v1",
                "lib_core",
                "crates/lib_core/src/api/v1.rs:1",
                &lib,
                "use",
                module
            ),
            // once per file: a signature at module scope wins over an earlier
            // path in a function body
            row(
                "lib_core::api::v1",
                "lib_core::billing",
                "crates/lib_core/src/api/v1.rs:11",
                &billing,
                "path",
                module
            ),
            // `self` beside an item, and a glob of an enum
            row(
                "lib_core::api::v1",
                "lib_core::billing",
                "crates/lib_core/src/api/v1.rs:15",
                &billing,
                "use",
                module
            ),
            row(
                "lib_core::api::v1",
                "lib_core::billing",
                "crates/lib_core/src/api/v1.rs:16",
                &billing,
                "use",
                module
            ),
            // a path in `#[derive(..)]`; the one in test code is left out
            row(
                "lib_core::billing",
                "ext:cargo:serde",
                "crates/lib_core/src/billing.rs:19",
                &None,
                "path",
                module
            ),
            row(
                "lib_core::billing::invoice",
                "lib_core",
                "crates/lib_core/src/billing/invoice.rs:3",
                &lib,
                "use",
                module
            ),
            // `super::`
            row(
                "lib_core::billing::invoice",
                "lib_core::billing",
                "crates/lib_core/src/billing/invoice.rs:1",
                &billing,
                "use",
                module
            ),
            // two calls of `crate::store::open()`: one piece of evidence
            row(
                "lib_core::billing::invoice",
                "lib_core::store",
                "crates/lib_core/src/billing/invoice.rs:16",
                &Some("crates/lib_core/src/store/mod.rs".to_owned()),
                "path",
                Scope::Local
            ),
            row(
                "lib_core::billing::invoice",
                "lib_core::store",
                "crates/lib_core/src/billing/invoice.rs:2",
                &Some("crates/lib_core/src/store/mod.rs".to_owned()),
                "use",
                module
            ),
            row(
                "lib_core::store",
                "lib_core::billing",
                "crates/lib_core/src/store/mod.rs:3",
                &billing,
                "use",
                module
            ),
            // `self::`, inside a function body
            row(
                "lib_core::store",
                "lib_core::store::memory",
                "crates/lib_core/src/store/mod.rs:10",
                &Some("crates/lib_core/src/store/memory.rs".to_owned()),
                "use",
                Scope::Local
            ),
        ]
    );
}

#[test]
fn a_reexport_from_the_modules_own_subtree_is_not_an_import() {
    let graph = scan_fixture();
    // `pub use billing::invoice::Invoice;` in lib.rs shapes what lib_core
    // offers; it is followed when resolving, but it is no dependency
    assert!(graph
        .outgoing(&id("lib_core"))
        .all(|e| e.to == id("ext:cargo:serde")));
    // so the crate root and its modules form no cycle
    assert!(graph.cycles().is_empty(), "{:?}", graph.cycles());
}

#[test]
fn only_public_items_become_symbols() {
    let graph = scan_fixture();
    let names = |component: &str| -> Vec<String> {
        graph
            .symbols_of(&id(component))
            .map(|s| s.name.clone())
            .collect()
    };
    assert_eq!(
        names("lib_core"),
        vec![
            "Invoice::total",
            "User",
            "User::new",
            "api",
            "v1",
            "billing",
            "greet",
            "prelude"
        ]
    );
    assert_eq!(
        names("lib_core::billing"),
        vec!["CURRENCY", "Charge", "Receipt", "Status", "invoice"]
    );
    // a private module's public items are still part of the crate's code
    assert_eq!(names("lib_core::store"), vec!["Ledger", "open"]);
    assert_eq!(
        names("lib_core::api::v1"),
        vec!["charge", "currency", "lookup", "receipt"]
    );

    let greet = graph.symbol(&"lib_core::greet".into()).unwrap();
    assert_eq!(greet.kind, SymbolKind::Function);
    assert_eq!(
        greet.signature.as_deref(),
        Some("pub fn greet(user: &User) -> String")
    );
    assert_eq!(greet.evidence[0].file, "crates/lib_core/src/lib.rs");
    assert_eq!(greet.evidence[0].line, Some(28));

    // symbol ids keep the full module path, inline modules included
    let charge = graph.symbol(&"lib_core::billing::Charge".into()).unwrap();
    assert_eq!(charge.kind, SymbolKind::Trait);
    assert_eq!(charge.component, id("lib_core::billing"));
    let lookup = graph.symbol(&"lib_core::api::v1::lookup".into()).unwrap();
    assert_eq!(lookup.component, id("lib_core::api::v1"));

    // private fn, private method, `pub fn` inside a private module: excluded
    assert!(graph.symbols_named("private_helper").next().is_none());
    assert!(graph.symbols_named("User::secret").next().is_none());
    assert!(graph.symbols_named("hidden").next().is_none());
}

#[test]
fn impact_follows_the_files_that_import_a_change() {
    let graph = scan_fixture();
    let reach = graph.change_impact(ChangeSeed::File("crates/lib_core/src/billing.rs"), 2);
    assert_eq!(
        reach.direct.into_iter().collect::<Vec<_>>(),
        ids(&[
            "app",
            "lib_core::api::v1",
            "lib_core::billing::invoice",
            "lib_core::store"
        ])
    );
    // config.rs imports the invoice module, which imports billing.rs
    assert_eq!(
        reach.transitive.into_iter().collect::<Vec<_>>(),
        ids(&[
            "app",
            "app::config",
            "lib_core::api::v1",
            "lib_core::billing::invoice",
            "lib_core::store"
        ])
    );

    // the memory module is used only inside a function of store
    let reach = graph.change_impact(ChangeSeed::File("crates/lib_core/src/store/memory.rs"), 2);
    assert_eq!(
        reach.direct.into_iter().collect::<Vec<_>>(),
        ids(&["lib_core::store"])
    );

    let affected: Vec<ComponentId> = graph
        .transitive_dependents(&id("lib_core"))
        .into_iter()
        .collect();
    assert_eq!(
        affected,
        ids(&[
            "app",
            "app::config",
            "lib_core::api::v1",
            "lib_core::billing::invoice"
        ])
    );
}

#[test]
fn cycles_and_signals_read_the_imported_files() {
    let graph = scan_fixture();
    // at depth 1 the invoice module folds into billing, which then depends
    // on store while store depends on billing
    assert_eq!(
        graph.rollup(1).cycles(),
        vec![ids(&["lib_core::billing", "lib_core::store"])]
    );
    assert!(graph.rollup(2).cycles().is_empty());

    // different files form each direction
    let rules = RuleSet {
        cycles: CycleRule {
            forbid: true,
            scope: vec![],
        },
        ..RuleSet::default()
    };
    let findings = archmap_core::rules::check(&graph, &rules, 1);
    match &findings[..] {
        [Finding::Cycle {
            components,
            file_level,
            ..
        }] => {
            assert_eq!(*components, ids(&["lib_core::billing", "lib_core::store"]));
            assert_eq!(*file_level, FileLevel::NoCycle);
        }
        other => panic!("expected one cycle, got {other:?}"),
    }

    assert_eq!(
        archmap_core::signals::signals(&graph, 1),
        vec![Signal::MixedDirections {
            component: id("lib_core::billing"),
            partners: ids(&["lib_core::store"]),
            used_by_partners: vec![FileUse {
                file: "crates/lib_core/src/billing.rs".into(),
                partners: 1,
            }],
            depending_on_partners: vec![FileDependence {
                file: "crates/lib_core/src/billing/invoice.rs".into(),
                partners: ids(&["lib_core::store"]),
            }],
        }]
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
    assert_eq!(graph.meta.coverage["rust"].read, Some(0));
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

#[test]
fn coverage_counts_only_the_files_under_src() {
    let graph = scan_fixture();
    // crates/app/tests/smoke.rs is seen but not read
    assert_eq!(
        graph.meta.coverage["rust"],
        LanguageCoverage {
            files: 9,
            read: Some(8)
        }
    );
}

#[test]
fn a_dev_dependency_used_under_src_is_an_import_without_an_edge() {
    let graph = scan_fixture();
    assert_eq!(
        graph.unmapped_imports,
        vec![UnmappedImport {
            from: id("app"),
            module: "assert_cmd".into(),
            reason: UnmappedReason::DeclaredNotRequired,
            provided_by: vec![],
            evidence: Evidence::new("crates/app/src/main.rs")
                .at_line(14)
                .with_note("use")
                .in_scope(Scope::Module),
        }]
    );
    assert!(graph.component(&id("ext:cargo:assert_cmd")).is_none());
}

/// For the evidence of the statement at `at` (`file:line`) that points at
/// `target`: its note and the names it takes.
fn names_taken(graph: &ArchitectureGraph, at: &str, target: &str) -> BTreeMap<String, Vec<String>> {
    graph
        .edges
        .iter()
        .flat_map(|e| &e.evidence)
        .filter(|e| {
            format!("{}:{}", e.file, e.line.unwrap_or(0)) == at
                && e.target.as_deref() == Some(target)
        })
        .map(|e| {
            (
                e.note.clone().unwrap_or_default(),
                e.names.iter().cloned().collect(),
            )
        })
        .collect()
}

fn noted(rows: &[(&str, &[&str])]) -> BTreeMap<String, Vec<String>> {
    rows.iter()
        .map(|(note, names)| {
            (
                (*note).to_owned(),
                names.iter().map(|n| (*n).to_owned()).collect(),
            )
        })
        .collect()
}

#[test]
fn rust_evidence_names_what_each_use_and_path_takes() {
    let graph = scan_fixture();
    let lib = "crates/lib_core/src/lib.rs";
    let billing = "crates/lib_core/src/billing.rs";
    let v1 = "crates/lib_core/src/api/v1.rs";
    // leaves of one declaration share their evidence
    assert_eq!(
        names_taken(&graph, "crates/app/src/main.rs:2", lib),
        noted(&[("use", &["User", "greet"])])
    );
    assert_eq!(
        names_taken(&graph, "crates/app/src/main.rs:1", billing),
        noted(&[("use", &["CURRENCY"])])
    );
    // through a `pub use`, the defining file and name
    assert_eq!(
        names_taken(
            &graph,
            "crates/app/src/config.rs:1",
            "crates/lib_core/src/billing/invoice.rs"
        ),
        noted(&[("use via crates/lib_core/src/lib.rs:7", &["Invoice"])])
    );
    // a path in code
    assert_eq!(
        names_taken(&graph, "crates/app/src/config.rs:12", lib),
        noted(&[("path", &["greet"])])
    );
    // two paths from one file to one target: one evidence, both names
    assert_eq!(
        names_taken(&graph, &format!("{v1}:11"), billing),
        noted(&[("path", &["CURRENCY", "Charge"])])
    );
    // `self` takes the module whole; a glob of an enum takes the enum
    assert_eq!(
        names_taken(&graph, &format!("{v1}:15"), billing),
        noted(&[("use", &["*", "Receipt"])])
    );
    assert_eq!(
        names_taken(&graph, &format!("{v1}:16"), billing),
        noted(&[("use", &["Status"])])
    );
}

#[test]
fn no_rust_edge_holds_evidence_that_differs_only_in_names() {
    let graph = scan_fixture();
    for edge in &graph.edges {
        let mut seen = BTreeSet::new();
        for e in &edge.evidence {
            let key = (&e.file, e.line, &e.note, &e.target, e.scope);
            assert!(
                seen.insert(key),
                "{} -> {}: {key:?} twice",
                edge.from,
                edge.to
            );
        }
    }
}

#[test]
fn a_method_whose_type_is_in_another_file_says_where_the_type_is() {
    let graph = scan_fixture();
    let total = graph.symbol(&"lib_core::Invoice::total".into()).unwrap();
    let at = total.location().map(|e| (e.file.as_str(), e.line));
    assert_eq!(at, Some(("crates/lib_core/src/lib.rs", Some(43))));
    let reached: Vec<_> = total
        .evidence
        .iter()
        .filter(|e| e.target.is_some())
        .map(|e| {
            (
                e.line,
                e.note.as_deref(),
                e.target.as_deref(),
                e.names.iter().cloned().collect::<Vec<_>>(),
            )
        })
        .collect();
    assert_eq!(
        reached,
        [(
            Some(42),
            Some("impl"),
            Some("crates/lib_core/src/billing/invoice.rs"),
            vec!["Invoice".to_owned()]
        )]
    );
    // a method beside its type needs none
    let new = graph.symbol(&"lib_core::User::new".into()).unwrap();
    assert!(new.evidence.iter().all(|e| e.target.is_none()));
}
