//! The TS/JS analyzer on `fixtures/ts-mocks`: which mock calls replace the
//! module they name for their file's whole run.

use std::path::Path;

use archmap_scan::{scan, ScanOptions};

#[test]
fn a_hoisted_mock_whose_factory_loads_nothing_replaces_its_module() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/ts-mocks");
    let report = scan(&root, &ScanOptions::default()).expect("scan succeeds");
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let replacing: Vec<String> = report
        .graph
        .edges
        .iter()
        .flat_map(|e| &e.evidence)
        .filter(|e| e.replaces)
        .map(|e| {
            let names: Vec<&str> = e.names.iter().map(String::as_str).collect();
            format!(
                "{}:{} -> {} {names:?}",
                e.file,
                e.line.unwrap(),
                e.target.as_deref().unwrap()
            )
        })
        .collect();
    // not a factory that loads the real module, one that the file loads for
    // real elsewhere, a mock without a factory or one inside a function; and
    // only the mock itself, not the imports that get its stand-in, with the
    // names its factory gives the module: a default by the name the module
    // declares, or `default` where it declares none
    assert_eq!(
        replacing,
        [
            r#"tests/barrel.test.ts:4 -> src/index.ts ["placeOrder"]"#,
            r#"tests/default.test.ts:4 -> src/audio.ts ["getAudioUrl"]"#,
            r#"tests/jest.test.ts:3 -> src/orders.ts ["placeOrder"]"#,
            r#"tests/link.test.ts:4 -> src/storage.ts ["fileUrl"]"#,
            r#"tests/media.test.ts:4 -> src/media.ts ["default"]"#,
            r#"tests/replaced.test.ts:4 -> src/orders.ts ["placeOrder"]"#,
            r#"tests/through.test.ts:4 -> src/orders.ts ["placeOrder"]"#,
            r#"tests/typed.test.ts:5 -> src/lines.ts ["total"]"#,
            r#"tests/unrelated.test.ts:4 -> src/audio.ts ["unrelated"]"#,
            r#"tests/upload.test.ts:4 -> src/storage.ts ["upload"]"#,
        ]
    );
}
