//! A Cargo package and a Python project with the same name.

use std::path::{Path, PathBuf};

use archmap_core::ComponentId;
use archmap_scan::{scan, ScanOptions};

fn write(root: &Path, file: &str, text: &str) {
    let path = root.join(file);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn repo(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("archmap-ids-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

const CARGO: &str = "[package]\nname = \"NAME\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";
const PYPROJECT: &str = "[project]\nname = \"NAME\"\nversion = \"0.1.0\"\n";

#[test]
fn packages_of_one_name_in_different_directories_stay_apart() {
    let root = repo("apart");
    write(&root, "rs/Cargo.toml", &CARGO.replace("NAME", "dup"));
    write(&root, "rs/src/lib.rs", "pub fn run() {}\n");
    write(
        &root,
        "py/pyproject.toml",
        &PYPROJECT.replace("NAME", "dup"),
    );
    write(&root, "py/dup/__init__.py", "def run():\n    pass\n");

    let report = scan(&root, &ScanOptions::default()).unwrap();
    std::fs::remove_dir_all(&root).unwrap();
    let graph = report.graph;

    let rust = graph.component(&ComponentId::new("dup")).unwrap();
    assert_eq!(rust.language.as_deref(), Some("rust"));
    assert_eq!(rust.path.as_deref(), Some("rs"));
    let python = graph.component(&ComponentId::new("dup+python")).unwrap();
    assert_eq!(python.language.as_deref(), Some("python"));
    assert_eq!(python.path.as_deref(), Some("py"));
    let module = graph
        .component(&ComponentId::new("dup+python::dup"))
        .unwrap();
    assert_eq!(module.parent.as_ref().unwrap().as_str(), "dup+python");
    assert_eq!(
        report.warnings,
        ["`dup` is also the id of a rust component at rs; the python component at py is renamed `dup+python`"]
    );
}

#[test]
fn a_cargo_and_a_python_manifest_side_by_side_are_one_package() {
    let root = repo("together");
    write(&root, "both/Cargo.toml", &CARGO.replace("NAME", "mix"));
    write(&root, "both/src/lib.rs", "pub fn run() {}\n");
    write(
        &root,
        "both/pyproject.toml",
        &PYPROJECT.replace("NAME", "mix"),
    );
    write(&root, "both/mix/__init__.py", "def run():\n    pass\n");

    let report = scan(&root, &ScanOptions::default()).unwrap();
    std::fs::remove_dir_all(&root).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert!(report.graph.component(&ComponentId::new("mix")).is_some());
    assert!(report
        .graph
        .components
        .keys()
        .all(|id| !id.as_str().contains('+')));
}
