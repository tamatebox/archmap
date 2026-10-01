//! The shared layer seen from an interface: scan a repository once, then ask.

use std::path::{Path, PathBuf};

use archmap_app::{ScanMode, Workspace, DEFAULT_DEPTH};

fn python_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/simple-python-project")
}

#[test]
fn a_workspace_summarizes_the_repository_it_scanned() {
    let ws = Workspace::scan(&python_fixture(), ScanMode::Full).unwrap();
    let summary = ws.summary(DEFAULT_DEPTH, false);
    assert!(
        summary.starts_with("# archmap summary\nroot: simple-python-project\ndepth: 2\n"),
        "{summary}"
    );
    assert_eq!(ws.root(), python_fixture().as_path());
}

#[test]
fn a_manifests_only_scan_reads_no_source() {
    let ws = Workspace::scan(&python_fixture(), ScanMode::ManifestsOnly).unwrap();
    assert!(ws.graph().symbols.is_empty());
    assert!(ws.warnings().is_empty());
}

#[test]
fn a_missing_root_names_it_in_the_error() {
    let missing = python_fixture().join("no-such-dir");
    let err = Workspace::scan(&missing, ScanMode::Full).err().unwrap();
    assert!(format!("{err:#}").starts_with("scanning "), "{err:#}");
}
