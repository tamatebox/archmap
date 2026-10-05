//! A Rust facade behind a crate root's re-export, in
//! `fixtures/rust-facade`: `pantry`'s `lib.rs` re-exports `facade::Repo`
//! (of its own subtree, no import), and `facade.rs` takes it from `infra`
//! (an import of its own); `Pool` comes from `infra` through `lib.rs` alone.

use std::path::Path;

use archmap_core::rules::{check, DenyRule, Finding, RuleSet};
use archmap_core::{ArchitectureGraph, EdgeKind};
use archmap_scan::{scan, ScanOptions};

fn fixture() -> ArchitectureGraph {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/rust-facade");
    let report = scan(
        &root.canonicalize().expect("fixture exists"),
        &ScanOptions::default(),
    )
    .expect("scan succeeds");
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    report.graph
}

/// The import evidence written in `file`: (line, target file, note).
fn imports_in(graph: &ArchitectureGraph, file: &str) -> Vec<(u32, String, String)> {
    let mut found: Vec<_> = graph
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Import)
        .flat_map(|e| &e.evidence)
        .filter(|e| e.file == file)
        .map(|e| {
            (
                e.line.unwrap_or_default(),
                e.target.clone().unwrap_or_default(),
                e.note.clone().unwrap_or_default(),
            )
        })
        .collect();
    found.sort();
    found
}

fn row(line: u32, target: &str, note: &str) -> (u32, String, String) {
    (line, target.to_owned(), note.to_owned())
}

#[test]
fn a_use_through_a_crate_root_names_the_facade_it_goes_through() {
    let graph = fixture();
    assert_eq!(
        imports_in(&graph, "pantry/src/app.rs"),
        vec![
            // through `lib.rs` alone
            row(1, "pantry/src/infra.rs", "use via pantry/src/lib.rs:6"),
            // `lib.rs`'s re-export is no import, the facade's is
            row(2, "pantry/src/infra.rs", "use via pantry/src/facade.rs:1"),
        ]
    );
    assert_eq!(
        imports_in(&graph, "kitchen/src/lib.rs"),
        vec![row(
            1,
            "pantry/src/infra.rs",
            "use via pantry/src/facade.rs:1"
        )]
    );
}

#[test]
fn rules_count_a_use_through_a_crate_root_toward_the_facade() {
    let graph = fixture();
    let deny = |from: &str, to: &str| DenyRule {
        from: from.into(),
        to: to.into(),
        reason: None,
    };
    let rules = RuleSet {
        components: [
            ("app", "pantry/src/app.rs"),
            ("facade", "pantry/src/facade.rs"),
            ("infra", "pantry/src/infra.rs"),
            ("kitchen", "kitchen"),
        ]
        .into_iter()
        .map(|(name, selector)| (name.to_owned(), vec![selector.to_owned()]))
        .collect(),
        deny: vec![
            deny("app", "infra"),
            deny("kitchen", "infra"),
            deny("kitchen", "facade"),
        ],
        ..RuleSet::default()
    };
    let forbidden: Vec<(String, String, Vec<String>)> = check(&graph, &rules, 2)
        .into_iter()
        .filter_map(|f| match f {
            Finding::Forbidden {
                from, to, evidence, ..
            } => Some((
                from.to_string(),
                to.to_string(),
                evidence
                    .iter()
                    .map(|e| format!("{}:{}", e.file, e.line.unwrap_or_default()))
                    .collect(),
            )),
            _ => None,
        })
        .collect();
    assert_eq!(
        forbidden,
        vec![
            // `Pool` passes no import on its way: it counts where it is
            // defined, and `Repo` does not
            (
                "pantry::app".to_owned(),
                "pantry::infra".to_owned(),
                vec!["pantry/src/app.rs:1".to_owned()]
            ),
            (
                "kitchen".to_owned(),
                "pantry::facade".to_owned(),
                vec!["kitchen/src/lib.rs:1".to_owned()]
            ),
        ]
    );
}
