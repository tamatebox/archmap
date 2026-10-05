//! The static start of a Python import's computed name, read from a small
//! tree written for the test.

use std::path::PathBuf;

use archmap_core::DynamicPrefix;
use archmap_scan::{scan, ScanOptions};

fn tree(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let root = std::env::temp_dir().join(format!("archmap-py-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for (path, text) in files {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    root
}

#[test]
fn a_prefix_names_the_packages_its_modules_load_first() {
    let root = tree(
        "prefix",
        &[
            ("pyproject.toml", "[project]\nname = \"loaders\"\n"),
            ("app/__init__.py", ""),
            ("app/plugins/__init__.py", ""),
            ("app/plugins/csv_x.py", ""),
            (
                "tools/loader.py",
                "import importlib\n\n\ndef load(name):\n    return importlib.import_module(f\"app.plugins.{name}\")\n",
            ),
        ],
    );
    let graph = scan(&root, &ScanOptions::default()).unwrap().graph;
    std::fs::remove_dir_all(&root).unwrap();
    let prefixes: Vec<&DynamicPrefix> = graph
        .dynamic_imports
        .iter()
        .filter_map(|d| d.prefix.as_ref())
        .collect();
    assert_eq!(
        prefixes,
        [&DynamicPrefix {
            written: "app.plugins.".into(),
            path: Some("app/plugins/".into()),
            first: vec!["app/__init__.py".into(), "app/plugins/__init__.py".into()],
        }]
    );
    let call = &graph.dynamic_imports[0];
    assert!(call.may_load("app/__init__.py"));
    assert!(call.may_load("app/plugins/csv_x.py"));
    assert!(!call.may_load("tools/other.py"));
}
