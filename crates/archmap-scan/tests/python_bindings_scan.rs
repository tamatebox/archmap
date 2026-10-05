//! The Python analyzer on `fixtures/python-bindings`: a statement that binds
//! a module records the names its file reads through it.

use std::path::Path;

use archmap_scan::{scan, ScanOptions};

#[test]
fn a_module_binding_takes_the_names_its_file_reads_through_it() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/python-bindings");
    let report = scan(&root, &ScanOptions::default()).expect("scan succeeds");
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let mut taken: Vec<String> = report
        .graph
        .edges
        .iter()
        .flat_map(|e| &e.evidence)
        .filter(|e| {
            e.target
                .as_deref()
                .is_some_and(|t| t.starts_with("store/billing/"))
        })
        .map(|e| {
            let names: Vec<&str> = e.names.iter().map(String::as_str).collect();
            format!(
                "{} {} -> {} {names:?}",
                e.file,
                e.note.as_deref().unwrap_or(""),
                e.target.as_deref().unwrap()
            )
        })
        .collect();
    taken.sort();
    assert_eq!(
        taken,
        [
            r#"spec/test_pay.py import -> store/billing/charge.py ["pay"]"#,
            r#"spec/test_rates.py import -> store/billing/rates.py ["rate"]"#,
            r#"spec/test_refund.py import -> store/billing/__init__.py ["refund"]"#,
            r#"spec/test_refund.py import via store/billing/__init__.py:3 -> store/billing/charge.py ["refund"]"#,
            // through an `as` name, a package's dotted path, a submodule
            r#"store/aliased.py import -> store/billing/charge.py ["pay"]"#,
            // used in a string, an f-string or an annotation, itself, or not
            // at all: anything of it
            r#"store/annotated.py import -> store/billing/charge.py ["*"]"#,
            r#"store/app.py import -> store/billing/__init__.py ["pay"]"#,
            // the package binds the name from its submodule
            r#"store/app.py import via store/billing/__init__.py:2 -> store/billing/charge.py ["pay"]"#,
            // and only passes it on: listed in `__all__`, or written `x as x`
            r#"store/billing/__init__.py export -> store/billing/charge.py ["pay"]"#,
            r#"store/billing/__init__.py export -> store/billing/charge.py ["refund"]"#,
            r#"store/billing/__init__.py export -> store/billing/duty.py ["charge_duty"]"#,
            // a submodule, which code may import for what loading it does
            r#"store/billing/__init__.py relative import -> store/billing/rates.py ["*"]"#,
            r#"store/billing/charge.py import -> store/billing/money.py ["cents"]"#,
            r#"store/billing/duty.py import -> store/billing/levy.py ["rate_of"]"#,
            r#"store/billing/duty.py import -> store/billing/money.py ["cents"]"#,
            // what an f-string formats is code
            r#"store/formatted.py import -> store/billing/charge.py ["pay", "refund"]"#,
            // a call that names its module with a literal, absolute or
            // relative to its package
            r#"store/loader.py __import__ -> store/billing/rates.py ["*"]"#,
            r#"store/loader.py import_module -> store/billing/rates.py ["*"]"#,
            r#"store/loader.py import_module -> store/billing/rates.py ["*"]"#,
            r#"store/loader.py import_module -> store/billing/rates.py ["*"]"#,
            r#"store/loader.py import_module -> store/billing/rates.py ["*"]"#,
            r#"store/loader.py import_module -> store/billing/rates.py ["*"]"#,
            r#"store/other.py import -> store/billing/charge.py ["pay"]"#,
            r#"store/passed.py import -> store/billing/charge.py ["*"]"#,
            r#"store/rated.py import -> store/billing/rates.py ["rate"]"#,
            r#"store/refunds.py import -> store/billing/charge.py ["refund"]"#,
            r#"store/unused.py import -> store/billing/charge.py ["*"]"#,
        ]
    );
}

#[test]
fn a_definition_with_its_body_on_one_line_ends_there() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/python-bindings");
    let graph = scan(&root, &ScanOptions::default()).unwrap().graph;
    let mut defined: Vec<(&str, &str)> = graph
        .symbols
        .values()
        .filter(|s| s.location().is_some_and(|at| at.file == "store/errors.py"))
        .map(|s| (s.name.as_str(), s.signature.as_deref().unwrap_or("")))
        .collect();
    defined.sort();
    assert_eq!(
        defined,
        [
            ("Conflict", "class Conflict(Exception)"),
            ("NotFound", "class NotFound(Exception)"),
            ("check", "def check(order)"),
        ]
    );
}

#[test]
fn a_computed_name_stays_a_call_that_loads_a_module_by_name() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/python-bindings");
    let graph = scan(&root, &ScanOptions::default()).unwrap().graph;
    let dynamic: Vec<(&str, Option<u32>)> = graph
        .dynamic_imports
        .iter()
        .filter(|d| d.evidence.file == "store/loader.py")
        .map(|d| (d.call.as_str(), d.evidence.line))
        .collect();
    assert_eq!(dynamic, [("import_module", Some(17))]);
}

#[test]
fn a_barrel_that_passes_on_another_barrels_name_gives_no_via_evidence() {
    // top/__init__.py passes on what top/mid/__init__.py passes on from
    // charge.py: a re-export leads on through its barrel, so only a
    // statement that takes the name gets evidence for the defining file
    let root = std::env::temp_dir().join(format!("archmap-nested-barrels-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for (file, text) in [
        ("pyproject.toml", "[project]\nname = \"nest\"\n"),
        (
            "top/__init__.py",
            "from .mid import pay\n\n__all__ = [\"pay\"]\n",
        ),
        (
            "top/mid/__init__.py",
            "from .charge import pay\n\n__all__ = [\"pay\"]\n",
        ),
        ("top/mid/charge.py", "def pay(x):\n    return x\n"),
        ("app.py", "from top import pay\n\npay(1)\n"),
    ] {
        let path = root.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    let report = scan(&root, &ScanOptions::default()).expect("scan succeeds");
    std::fs::remove_dir_all(&root).unwrap();
    let mut via: Vec<String> = report
        .graph
        .edges
        .iter()
        .flat_map(|e| &e.evidence)
        .filter_map(|e| Some(format!("{}:{} {}", e.file, e.line?, e.via()?)))
        .collect();
    via.sort();
    assert_eq!(via, ["app.py:1 top/__init__.py:1"]);
}
