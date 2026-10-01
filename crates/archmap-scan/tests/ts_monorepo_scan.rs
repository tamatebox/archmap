//! The TS/JS analyzer on `fixtures/ts-monorepo`: workspace members and
//! `file:` dependencies linked by name, as an install links them.

use std::collections::BTreeSet;
use std::path::Path;

use archmap_core::{ArchitectureGraph, ComponentId, ComponentKind, EdgeKind};
use archmap_scan::{scan, ScanOptions};

fn scan_fixture() -> (ArchitectureGraph, Vec<String>) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/ts-monorepo");
    let report = scan(&root, &ScanOptions::default()).expect("scan succeeds");
    (report.graph, report.warnings)
}

#[test]
fn members_and_path_dependencies_are_linked_by_name() {
    let (graph, warnings) = scan_fixture();
    // web's tsconfig extends a member's: it loads through the link
    assert!(warnings.is_empty(), "{warnings:?}");
    // a dependency on a member is internal, whatever its version says
    for (from, to) in [
        ("web", "@acme/ui"),
        ("web", "@acme/core"),
        ("web", "local-lib"),
        // a member with no code, only a JSON file
        ("web", "@acme/i18n"),
    ] {
        assert!(
            graph.edges.iter().any(|e| e.from.as_str() == from
                && e.to.as_str() == to
                && e.kind == EdgeKind::Dependency),
            "no dependency {from} -> {to}"
        );
    }
    let imports: BTreeSet<(&str, Option<&str>)> = graph
        .edges
        .iter()
        .filter(|e| e.from.as_str() == "web::src/page.tsx" && e.kind == EdgeKind::Import)
        .flat_map(|e| {
            e.evidence
                .iter()
                .map(move |ev| (e.to.as_str(), ev.target.as_deref()))
        })
        .collect();
    for expected in [
        // by name to a member's files, through its `exports`
        ("@acme/ui", Some("packages/ui/src/index.ts")),
        (
            "@acme/ui::src/button.tsx",
            Some("packages/ui/src/button.tsx"),
        ),
        // a `file:` dependency, through its `main`
        ("local-lib", Some("libs/local/index.js")),
        // the member's own alias
        ("web::src/lib/format.ts", Some("apps/web/src/lib/format.ts")),
        // a member whose entry is built (dist/), which the scan does not hold
        ("@acme/core", None),
    ] {
        assert!(
            imports.contains(&expected),
            "missing {expected:?} in {imports:?}"
        );
    }
    // so is a member that only holds a tsconfig, which web's devDependency
    // names
    assert_eq!(
        graph
            .component(&ComponentId::new("@acme/tsconfig"))
            .map(|c| c.kind),
        Some(ComponentKind::Package)
    );
    // a member's file that is no code goes to that member
    assert!(
        graph
            .edges
            .iter()
            .any(|e| e.from.as_str() == "web::src/lib/format.ts"
                && e.to.as_str() == "@acme/i18n"
                && e.kind == EdgeKind::Import
                && e.evidence[0].target.as_deref() == Some("packages/i18n/en.json")),
        "no import of the member's JSON file"
    );
    // no member points at npm, and the example named react takes over no
    // dependency of that name
    let externals: BTreeSet<&str> = graph
        .components
        .values()
        .filter(|c| c.kind == ComponentKind::External)
        .map(|c| c.id.as_str())
        .collect();
    assert_eq!(externals, BTreeSet::from(["ext:npm:react"]));
    assert!(graph.edges.iter().any(|e| e.from.as_str() == "@acme/ui"
        && e.to.as_str() == "ext:npm:react"
        && e.kind == EdgeKind::Dependency));
}

#[test]
fn a_declaration_of_an_enclosing_package_says_so() {
    let (graph, _) = scan_fixture();
    let note = graph
        .unmapped_imports
        .iter()
        .find(|u| u.module == "typescript")
        .and_then(|u| u.evidence.note.as_deref());
    assert_eq!(
        note,
        Some(
            "import typescript, declared as typescript in the enclosing package.json:6 \
             (devDependencies)"
        )
    );
}
