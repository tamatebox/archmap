//! End-to-end scan of `fixtures/simple-python-project`.

use std::path::{Path, PathBuf};

use archmap_core::{
    ArchitectureGraph, ComponentId, ComponentKind, DynamicImport, EdgeKind, Evidence,
    LanguageCoverage, Scope, SymbolKind, UnmappedReason,
};
use archmap_scan::{scan, ScanOptions};

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/simple-python-project")
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
fn project_and_packages_become_components() {
    let graph = scan_fixture();
    assert_eq!(graph.meta.analyzers, vec!["python".to_owned()]);

    let project = graph.component(&id("shop")).expect("project");
    assert_eq!(project.kind, ComponentKind::Package);
    assert_eq!(project.language.as_deref(), Some("python"));
    assert_eq!(project.path.as_deref(), Some("."));

    // src/ layout: dotted path starts at the top-most package, not at `src`
    let billing = graph
        .component(&id("shop::shop.billing"))
        .expect("billing module");
    assert_eq!(billing.kind, ComponentKind::Module);
    assert_eq!(billing.name, "shop.billing");
    assert_eq!(billing.path.as_deref(), Some("src/shop/billing"));
    assert_eq!(billing.parent, Some(id("shop::shop")));
    assert_eq!(
        graph.component(&id("shop::shop")).unwrap().parent,
        Some(id("shop"))
    );

    // lookup by dotted name works and is unique
    assert_eq!(graph.components_named("shop.billing").count(), 1);
}

#[test]
fn declared_dependencies_from_pyproject_and_requirements() {
    let graph = scan_fixture();
    let requests = graph
        .edges
        .iter()
        .find(|e| {
            e.from == id("shop") && e.to == id("ext:requests") && e.kind == EdgeKind::Dependency
        })
        .expect("requests dependency");
    let files: Vec<&str> = requests.evidence.iter().map(|e| e.file.as_str()).collect();
    assert_eq!(files, vec!["pyproject.toml", "requirements.txt"]);

    // PEP 503 normalization: SQLAlchemy[asyncio] -> sqlalchemy, PyYAML -> pyyaml
    assert!(graph.component(&id("ext:sqlalchemy")).is_some());
    assert_eq!(
        graph.component(&id("ext:pyyaml")).unwrap().kind,
        ComponentKind::External
    );
    // optional-dependencies are not recorded
    assert!(graph.component(&id("ext:pytest")).is_none());
}

#[test]
fn imports_resolve_between_modules_and_to_declared_externals() {
    let graph = scan_fixture();
    let find = |from: &str, to: &str| {
        graph
            .edges
            .iter()
            .find(|e| e.from == id(from) && e.to == id(to) && e.kind == EdgeKind::Import)
    };

    let internal = find("shop::shop.billing", "shop::shop").expect("billing imports users/shop");
    let notes: Vec<(Option<u32>, &str)> = internal
        .evidence
        .iter()
        .map(|e| (e.line, e.note.as_deref().unwrap_or("")))
        .collect();
    assert_eq!(
        notes,
        vec![(Some(5), "relative import"), (Some(6), "import")]
    );

    assert!(find("shop::shop.billing", "ext:requests").is_some());
    assert!(find("shop::shop.billing", "ext:sqlalchemy").is_some());
    // import names that differ from distribution names still resolve, and
    // the evidence says how
    let note = |from: &str, to: &str| {
        find(from, to).map(|e| e.evidence[0].note.clone().unwrap_or_default())
    };
    assert_eq!(
        note("shop::shop.billing", "ext:pyyaml").as_deref(),
        Some("import yaml, matched by known import name")
    );
    assert_eq!(
        note("shop::shop", "ext:google-cloud-bigquery").as_deref(),
        Some("import google.cloud.bigquery, matched by dotted name")
    );
    assert_eq!(
        note("shop::shop", "ext:scikit-learn").as_deref(),
        Some("import sklearn, matched by known import name")
    );
    // google.api_core is imported but not declared: not a dependency edge
    assert!(graph
        .components
        .keys()
        .all(|k| !k.as_str().contains("api-core")));
    // imports between files of one component are self-edges that always
    // point at another file; stdlib imports are not edges at all
    for edge in graph.edges.iter().filter(|e| e.from == e.to) {
        for e in &edge.evidence {
            let target = e
                .target
                .as_deref()
                .expect("intra-component imports name a file");
            assert_ne!(target, e.file);
        }
    }
    let intra = graph
        .edges
        .iter()
        .find(|e| e.from == id("shop::shop.billing") && e.to == e.from)
        .expect("billing/__init__.py imports billing/charge.py");
    assert_eq!(
        intra.evidence[0].target.as_deref(),
        Some("src/shop/billing/charge.py")
    );
    assert!(graph.components.keys().all(|k| !k.as_str().contains("os")));

    // tests/ has no __init__.py: its files belong to the `tests` namespace
    // module, not to the project component
    assert!(find("shop::tests", "shop::shop.billing").is_some());
    assert!(find("shop", "shop::shop.billing").is_none());
}

#[test]
fn namespace_directories_stay_in_the_dotted_path() {
    let graph = scan_fixture();
    // src/shop/integrations has no __init__.py; src/shop/integrations/slack does
    let slack = graph
        .component(&id("shop::shop.integrations.slack"))
        .expect("namespace child");
    assert_eq!(slack.parent, Some(id("shop::shop.integrations")));
    assert!(graph.component(&id("shop::integrations.slack")).is_none());
    assert!(graph.component(&id("shop::slack")).is_none());

    // the namespace directory is a module of its own (PEP 420)
    let namespace = graph
        .component(&id("shop::shop.integrations"))
        .expect("namespace module");
    assert_eq!(namespace.kind, ComponentKind::Module);
    assert_eq!(namespace.parent, Some(id("shop::shop")));
    assert_eq!(namespace.evidence[0].file, "src/shop/integrations");
    assert_eq!(
        namespace.evidence[0].note.as_deref(),
        Some("namespace package")
    );
    // `src` is the source root of a src/ layout, not a namespace module
    assert!(graph.component(&id("shop::src")).is_none());

    // a namespace directory inside a regular package is still library code
    let webhook = graph
        .symbol(&"shop::shop.integrations::webhooks::send_webhook".into())
        .expect("symbols from namespace dirs inside a package");
    assert_eq!(webhook.component, id("shop::shop.integrations"));

    let edge = graph
        .edges
        .iter()
        .find(|e| e.from == id("shop::shop") && e.to == id("shop::shop.integrations.slack"))
        .expect("import resolves to the namespace child, not to `shop`");
    assert_eq!(edge.evidence[0].line, Some(3));

    // tests/ has no __init__.py; tests/unit does
    assert_eq!(
        graph.component(&id("shop::tests.unit")).unwrap().parent,
        Some(id("shop::tests"))
    );
    assert_eq!(
        graph.component(&id("shop::tests")).unwrap().parent,
        Some(id("shop"))
    );
}

#[test]
fn tests_and_loose_scripts_contribute_imports_but_no_symbols() {
    let graph = scan_fixture();
    assert!(graph.symbols_named("test_pay").next().is_none());
    assert!(graph.symbols_named("make_user").next().is_none());
    assert_eq!(graph.symbols_of(&id("shop")).count(), 0);
    assert!(graph.edges.iter().any(|e| e.from == id("shop::tests.unit")
        && e.to == id("shop::shop")
        && e.kind == EdgeKind::Import));

    // scripts/ is a namespace module: imports yes, symbols no
    assert_eq!(
        graph.component(&id("shop::scripts")).unwrap().kind,
        ComponentKind::Module
    );
    assert!(graph.symbols_named("backfill_payments").next().is_none());
    assert!(graph.edges.iter().any(|e| e.from == id("shop::scripts")
        && e.to == id("shop::shop.billing")
        && e.kind == EdgeKind::Import));
}

#[test]
fn directories_that_cannot_be_imported_are_not_modules() {
    let graph = scan_fixture();
    // `2024-01-migration` is not a valid identifier, so its file belongs to
    // the nearest importable ancestor
    assert!(graph
        .components
        .keys()
        .all(|k| !k.as_str().contains("2024")));
    let edge = graph
        .edges
        .iter()
        .find(|e| e.from == id("shop::scripts") && e.to == id("shop::shop"))
        .expect("import from the migration script");
    assert_eq!(edge.evidence[0].file, "scripts/2024-01-migration/fix.py");
    assert!(graph.symbols_named("fix_users").next().is_none());
}

#[test]
fn public_defs_become_symbols() {
    let graph = scan_fixture();
    let mut names: Vec<&str> = graph
        .symbols_of(&id("shop::shop.billing"))
        .map(|s| s.name.as_str())
        .collect();
    names.sort();
    assert_eq!(names, vec!["CURRENCY", "Payment", "Payment.charge", "pay"]);

    let pay = graph
        .symbol(&"shop::shop.billing::charge::pay".into())
        .unwrap();
    assert_eq!(pay.kind, SymbolKind::Function);
    assert_eq!(
        pay.signature.as_deref(),
        Some("def pay(user: User, amount: int,) -> Payment")
    );
    assert_eq!(pay.evidence[0].file, "src/shop/billing/charge.py");
    assert_eq!(pay.evidence[0].line, Some(25));

    let user = graph.symbol(&"shop::shop::users::User".into()).unwrap();
    assert_eq!(user.kind, SymbolKind::Struct);
    assert!(graph.symbols_named("_helper").next().is_none());
    assert!(graph.symbols_named("User._internal").next().is_none());
    assert!(graph.symbols_named("Payment.__init__").next().is_none());
}

#[test]
fn impact_of_users_module_reaches_billing_and_tests() {
    let graph = scan_fixture();
    let affected = graph.transitive_dependents(&id("shop::shop"));
    let affected: Vec<&str> = affected.iter().map(|c| c.as_str()).collect();
    // test directories are components too, so impact reaches them
    assert_eq!(
        affected,
        vec![
            "shop::scripts",
            "shop::shop.billing",
            "shop::tests",
            "shop::tests.unit"
        ]
    );
    assert_eq!(
        graph
            .component_for_path("src/shop/billing/charge.py")
            .unwrap()
            .id,
        id("shop::shop.billing")
    );
}

#[test]
fn repo_without_manifest_gets_root_component() {
    let dir = std::env::temp_dir().join(format!("archmap-py-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("tool")).unwrap();
    std::fs::write(dir.join("tool/__init__.py"), "").unwrap();
    std::fs::write(
        dir.join("tool/main.py"),
        "import tool\ndef run():\n    pass\n",
    )
    .unwrap();

    let graph = scan(&dir, &ScanOptions::default()).unwrap().graph;
    std::fs::remove_dir_all(&dir).unwrap();

    let root_name = dir.file_name().unwrap().to_string_lossy().into_owned();
    let root = graph
        .component(&ComponentId::new(&root_name))
        .expect("root project");
    assert_eq!(root.kind, ComponentKind::Package);
    assert!(graph
        .component(&ComponentId::new(format!("{root_name}::tool")))
        .is_some());
    assert_eq!(graph.symbols_named("run").count(), 1);
}

#[test]
fn scanning_a_package_directory_uses_its_name_as_top_level() {
    let dir = std::env::temp_dir().join(format!("archmap-pkg-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("__init__.py"), "").unwrap();
    std::fs::write(dir.join("core.py"), "def run():\n    pass\n").unwrap();
    std::fs::write(dir.join("sub/__init__.py"), "").unwrap();
    let root_name = dir.file_name().unwrap().to_string_lossy().into_owned();
    std::fs::write(
        dir.join("sub/impl.py"),
        format!("from {root_name}.core import run\n"),
    )
    .unwrap();

    let graph = scan(&dir, &ScanOptions::default()).unwrap().graph;
    std::fs::remove_dir_all(&dir).unwrap();

    let top = ComponentId::new(format!("{root_name}::{root_name}"));
    let sub = ComponentId::new(format!("{root_name}::{root_name}.sub"));
    assert_eq!(graph.component(&top).unwrap().name, root_name);
    assert_eq!(graph.component(&sub).unwrap().parent, Some(top.clone()));
    // absolute import of the root package resolves to the top-level module
    assert!(graph
        .edges
        .iter()
        .any(|e| e.from == sub && e.to == top && e.kind == EdgeKind::Import));
}

#[test]
fn undeclared_imports_are_observations_not_edges() {
    let graph = scan_fixture();
    let unresolved: Vec<(&str, &str, &str, Option<u32>)> = graph
        .unmapped_imports
        .iter()
        .filter(|u| u.reason == UnmappedReason::Undeclared)
        .map(|u| {
            (
                u.from.as_str(),
                u.module.as_str(),
                u.evidence.file.as_str(),
                u.evidence.line,
            )
        })
        .collect();
    // scripts/report.py also imports json (standard library), pytest (a dev
    // extra) and helpers / backfill (files in the project): none of those is
    // undeclared
    assert_eq!(
        unresolved,
        vec![(
            "shop::shop",
            "google.api_core.exceptions",
            "src/shop/analytics.py",
            Some(3)
        )]
    );
    assert!(graph
        .edges
        .iter()
        .all(|e| !e.to.as_str().contains("api-core")));
}

#[test]
fn installed_record_files_resolve_import_names() {
    let dir = std::env::temp_dir().join(format!("archmap-venv-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let dist_info = dir.join(".venv/lib/python3.12/site-packages/fancy_lib-1.0.dist-info");
    std::fs::create_dir_all(&dist_info).unwrap();
    std::fs::create_dir_all(dir.join("app")).unwrap();
    std::fs::write(
        dir.join("pyproject.toml"),
        "[project]\nname = \"demo\"\ndependencies = [\"fancy-lib\", \"other-lib\"]\n",
    )
    .unwrap();
    std::fs::write(
        dist_info.join("RECORD"),
        "fancylib/__init__.py,sha256=x,10\nfancy_lib-1.0.dist-info/RECORD,,\n",
    )
    .unwrap();
    let transitive = dir.join(".venv/lib/python3.12/site-packages/transitive_dep-2.0.dist-info");
    std::fs::create_dir_all(&transitive).unwrap();
    std::fs::write(transitive.join("RECORD"), "transitive/__init__.py,,\n").unwrap();
    std::fs::write(
        dir.join("app/__init__.py"),
        "import fancylib\nimport otherlib\nimport transitive.core\n",
    )
    .unwrap();

    let graph = scan(&dir, &ScanOptions::default()).unwrap().graph;
    std::fs::remove_dir_all(&dir).unwrap();

    let edge = graph
        .edges
        .iter()
        .find(|e| e.from == id("demo::app") && e.to == id("ext:fancy-lib"))
        .expect("resolved through the installed RECORD");
    assert_eq!(
        edge.evidence[0].note.as_deref(),
        Some(
            "import fancylib, provided per \
             .venv/lib/python3.12/site-packages/fancy_lib-1.0.dist-info/RECORD"
        )
    );
    // nothing says which distribution provides `otherlib`: no edge
    assert!(!graph
        .edges
        .iter()
        .any(|e| e.to == id("ext:other-lib") && e.kind == EdgeKind::Import));
    // both remain unresolved; installed metadata names the provider of the
    // undeclared one
    let unresolved: Vec<(&str, Vec<String>)> = graph
        .unmapped_imports
        .iter()
        .map(|u| (u.module.as_str(), u.provided_by.clone()))
        .collect();
    assert_eq!(
        unresolved,
        vec![
            ("otherlib", vec![]),
            ("transitive.core", vec!["transitive-dep".to_owned()])
        ]
    );
    // the virtualenv itself is never scanned as source
    assert!(graph
        .components
        .keys()
        .all(|k| !k.as_str().contains("fancylib")));
}

/// A temporary project whose `.venv` holds `google-cloud-bigquery`,
/// `google-cloud-storage` and `pyyaml`, with `dependencies` declared and the
/// given files. The caller removes the directory.
fn namespace_project(name: &str, dependencies: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("archmap-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let site = dir.join(".venv/lib/python3.12/site-packages");
    for (dist_info, record) in [
        (
            "google_cloud_bigquery-3.0.dist-info",
            "google/cloud/bigquery/__init__.py,,\n",
        ),
        (
            "google_cloud_storage-2.0.dist-info",
            "google/cloud/storage/__init__.py,,\n",
        ),
        ("PyYAML-6.0.dist-info", "yaml/__init__.py,,\n"),
    ] {
        std::fs::create_dir_all(site.join(dist_info)).unwrap();
        std::fs::write(site.join(dist_info).join("RECORD"), record).unwrap();
    }
    std::fs::write(
        dir.join("pyproject.toml"),
        format!("[project]\nname = \"demo\"\ndependencies = [{dependencies}]\n"),
    )
    .unwrap();
    for (file, text) in files {
        let path = dir.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    dir
}

#[test]
fn names_imported_from_a_package_that_are_modules_count_on_their_own() {
    let dir = namespace_project(
        "names",
        "",
        &[
            ("app/__init__.py", ""),
            ("app/a.py", "from google.cloud import bigquery, storage\n"),
            ("app/b.py", "from google.cloud import bigquery, SomeAttr\n"),
            ("app/c.py", "from yaml import safe_load\n"),
        ],
    );
    let graph = scan(&dir, &ScanOptions::default()).unwrap().graph;
    std::fs::remove_dir_all(&dir).unwrap();

    let unmapped: Vec<(&str, &str, Vec<&str>)> = graph
        .unmapped_imports
        .iter()
        .map(|u| {
            (
                u.evidence.file.as_str(),
                u.module.as_str(),
                u.provided_by.iter().map(String::as_str).collect(),
            )
        })
        .collect();
    assert_eq!(
        unmapped,
        vec![
            // a name that is not a module is an attribute of the package
            (
                "app/b.py",
                "google.cloud",
                vec!["google-cloud-bigquery", "google-cloud-storage"]
            ),
            (
                "app/a.py",
                "google.cloud.bigquery",
                vec!["google-cloud-bigquery"]
            ),
            (
                "app/b.py",
                "google.cloud.bigquery",
                vec!["google-cloud-bigquery"]
            ),
            (
                "app/a.py",
                "google.cloud.storage",
                vec!["google-cloud-storage"]
            ),
            ("app/c.py", "yaml", vec!["pyyaml"]),
        ]
    );
}

#[test]
fn a_declared_name_does_not_hide_an_undeclared_one_beside_it() {
    let dir = namespace_project(
        "partial",
        "\"google-cloud-bigquery\"",
        &[
            ("app/__init__.py", ""),
            ("app/a.py", "from google.cloud import bigquery, storage\n"),
        ],
    );
    let graph = scan(&dir, &ScanOptions::default()).unwrap().graph;
    std::fs::remove_dir_all(&dir).unwrap();

    assert!(graph.edges.iter().any(|e| e.from == id("demo::app")
        && e.to == id("ext:google-cloud-bigquery")
        && e.kind == EdgeKind::Import));
    let unmapped: Vec<(&str, UnmappedReason)> = graph
        .unmapped_imports
        .iter()
        .map(|u| (u.module.as_str(), u.reason))
        .collect();
    assert_eq!(
        unmapped,
        vec![("google.cloud.storage", UnmappedReason::Undeclared)]
    );
}

#[test]
fn coverage_counts_files_read_per_language() {
    let graph = scan_fixture();
    let coverage: Vec<(&str, &LanguageCoverage)> = graph
        .meta
        .coverage
        .iter()
        .map(|(language, c)| (language.as_str(), c))
        .collect();
    assert_eq!(
        coverage,
        vec![
            (
                "python",
                &LanguageCoverage {
                    files: 15,
                    read: Some(15)
                }
            ),
            // scripts/deploy.sh: seen, but no analyzer reads shell
            (
                "shell",
                &LanguageCoverage {
                    files: 1,
                    read: None
                }
            ),
        ]
    );
}

#[test]
fn imports_without_an_edge_say_why() {
    let graph = scan_fixture();
    let unmapped: Vec<(&str, &str, UnmappedReason, Option<u32>)> = graph
        .unmapped_imports
        .iter()
        .map(|u| {
            (
                u.from.as_str(),
                u.module.as_str(),
                u.reason,
                u.evidence.line,
            )
        })
        .collect();
    assert_eq!(
        unmapped,
        vec![
            // declared, but only as the `dev` extra
            (
                "shop::scripts",
                "pytest",
                UnmappedReason::DeclaredNotRequired,
                Some(2)
            ),
            (
                "shop::shop",
                "google.api_core.exceptions",
                UnmappedReason::Undeclared,
                Some(3)
            ),
            // tests/test_billing.py: scripts/helpers.py, reached through a
            // sys.path entry that a static scan cannot see
            ("shop::tests", "helpers", UnmappedReason::LocalName, Some(3)),
        ]
    );
}

#[test]
fn imports_of_files_next_to_the_importer_resolve_to_them() {
    let graph = scan_fixture();
    // scripts/report.py imports helpers.py and backfill.py beside it by bare
    // name, as a script run directly can
    let intra = graph
        .edges
        .iter()
        .find(|e| e.from == id("shop::scripts") && e.to == e.from && e.kind == EdgeKind::Import)
        .expect("self-edge inside scripts");
    let statements: Vec<_> = intra
        .evidence
        .iter()
        .map(|e| {
            (
                e.file.as_str(),
                e.line,
                e.target.as_deref(),
                e.note.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        statements,
        vec![
            (
                "scripts/report.py",
                Some(3),
                Some("scripts/helpers.py"),
                Some("import helpers, next to the importing file (assumes its directory is on sys.path)")
            ),
            (
                "scripts/report.py",
                Some(4),
                Some("scripts/backfill.py"),
                Some("import backfill, next to the importing file (assumes its directory is on sys.path)")
            ),
        ]
    );
}

#[test]
fn dynamic_imports_are_recorded_where_they_are_called() {
    let graph = scan_fixture();
    assert_eq!(
        graph.dynamic_imports,
        vec![DynamicImport {
            from: id("shop::scripts"),
            call: "import_module".into(),
            evidence: Evidence::new("scripts/plugins.py")
                .at_line(5)
                .in_scope(Scope::Local),
        }]
    );
}
