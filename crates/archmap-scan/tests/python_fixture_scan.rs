//! End-to-end scan of `fixtures/simple-python-project`.

use std::path::{Path, PathBuf};

use archmap_core::{ArchitectureGraph, ComponentId, ComponentKind, EdgeKind, SymbolKind};
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
    // `import yaml` cannot be matched to the `PyYAML` distribution: no edge
    assert!(find("shop::shop.billing", "ext:pyyaml").is_none());
    // stdlib and self-imports are not edges
    assert!(graph.edges.iter().all(|e| e.from != e.to));
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
