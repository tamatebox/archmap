//! What `query` and `impact` could not follow for their target, counted
//! from what the analyzers record, and only when something applies.

use std::path::{Path, PathBuf};

use archmap_app::{Format, ImpactRequest, QueryRequest, ScanMode, Workspace, DEFAULT_DEPTH};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name)
}

fn scan(root: &Path) -> Workspace {
    Workspace::scan(root, ScanMode::Full).unwrap()
}

fn query(ws: &Workspace, target: &str, format: Format) -> String {
    ws.query(&QueryRequest {
        target,
        depth: DEFAULT_DEPTH,
        format,
        verbose: false,
    })
    .unwrap()
    .output
}

fn impact(ws: &Workspace, target: &str) -> serde_json::Value {
    let json = ws
        .impact(&ImpactRequest {
            target,
            depth: DEFAULT_DEPTH,
            format: Format::Json,
            verbose: false,
        })
        .unwrap()
        .output;
    serde_json::from_str(&json).unwrap()
}

#[test]
fn a_python_file_lists_dynamic_imports_elsewhere_and_imports_named_like_it() {
    let ws = scan(&fixture("simple-python-project"));
    let text = query(&ws, "scripts/helpers.py", Format::Text);
    let section = text.split_once("\nNot traced:\n").map(|(_, s)| s);
    assert_eq!(
        section,
        Some(
            "  dynamic: 1 call loads a module by a computed name, which may be this: \
             scripts/plugins.py:5\n  \
             named like it: 1 import of `helpers` maps to no file: tests/test_billing.py:3 \
             (local name)\n"
        ),
        "{text}"
    );
    let json: serde_json::Value =
        serde_json::from_str(&query(&ws, "scripts/helpers.py", Format::Json)).unwrap();
    assert_eq!(json["not_traced"]["dynamic"]["total"], 1);
    assert_eq!(json["not_traced"]["named_like"]["name"], "helpers");
    assert_eq!(
        json["not_traced"]["named_like"]["shown"][0]["module"],
        "helpers"
    );
}

#[test]
fn a_ts_file_counts_the_dynamic_imports_of_ts_and_js_and_an_alias_named_like_it() {
    let ws = scan(&fixture("simple-ts-project"));
    let text = query(&ws, "src/components/button.tsx", Format::Text);
    assert!(
        text.contains(
            "\n  dynamic: 2 calls load modules by computed names, which may be this: \
             scripts/report.cjs:4, src/app/lazy.tsx:7\n"
        ),
        "{text}"
    );
    assert!(
        text.contains(
            "\n  named like it: 1 import of `button` maps to no file: src/app/page.tsx:14 \
             (local name)\n"
        ),
        "{text}"
    );
}

#[test]
fn the_dynamic_imports_of_the_target_itself_stay_under_not_mapped() {
    let ws = scan(&fixture("simple-ts-project"));
    let text = query(&ws, "src/app/lazy.tsx", Format::Text);
    assert!(
        text.contains("\n  dynamic: 1 call loads a module by a computed name, which may be this: scripts/report.cjs:4\n"),
        "{text}"
    );
}

#[test]
fn a_rust_target_counts_the_files_its_analyzer_did_not_read() {
    let ws = scan(&fixture("rust-cargo-targets"));
    let text = query(&ws, "kiosk/src/till.rs", Format::Text);
    // test data no Cargo target loads is not read, by design: say which
    // files these are
    assert!(
        text.ends_with(
            "\nNot traced:\n  not read: 1 of 17 rust files: the Rust analyzer reads src/ and what \
             the other Cargo targets load, so files outside src/ that no target loads, such as \
             test data, are among them, as is any file that failed to parse\n"
        ),
        "{text}"
    );
    let json = impact(&ws, "kiosk/src/till.rs");
    let not_read = &json["not_traced"]["not_read"];
    assert_eq!(not_read["languages"], serde_json::json!(["rust"]));
    assert_eq!(
        (&not_read["files"], &not_read["read"]),
        (&17.into(), &16.into())
    );
    assert!(not_read["note"]
        .as_str()
        .is_some_and(|n| n.contains("no target loads")));
}

const NO_IMPORTERS: &str = "no import of it was found: only import statements are read, \
     so a file that a framework, a test runner or a command loads by name or path has none";

#[test]
fn impact_says_in_words_why_no_import_shows_who_uses_a_file() {
    let ws = scan(&fixture("simple-ts-project"));
    // a script's globals: no import names them
    let script = impact(&ws, "src/global.d.ts");
    assert!(
        script["not_traced"]["script"]
            .as_str()
            .is_some_and(|s| s.starts_with("a script: its declarations are global")),
        "{script}"
    );
    // an entry file that a framework loads by name: nothing imports it
    let page = impact(&ws, "src/app/page.tsx");
    assert_eq!(page["importers"]["total"], 0, "{page}");
    assert_eq!(page["not_traced"]["no_importers"], NO_IMPORTERS, "{page}");
}

#[test]
fn a_file_nothing_imports_says_so_in_query_text_too() {
    let ws = scan(&fixture("simple-ts-project"));
    let text = query(&ws, "src/app/page.tsx", Format::Text);
    assert!(
        text.contains(&format!("\nNot traced:\n  no importers: {NO_IMPORTERS}\n")),
        "{text}"
    );
    // the script's text says it where it lists importers
    let script = query(&ws, "src/global.d.ts", Format::Text);
    assert!(!script.contains("no importers:"), "{script}");
}

#[test]
fn a_test_file_has_no_importers_by_design_and_says_nothing_of_it() {
    // a test runner loads test files; that nothing imports them is no news
    let ws = scan(&fixture("simple-ts-project"));
    let impact = impact(&ws, "tests/money.test.ts");
    assert!(
        impact["not_traced"].get("no_importers").is_none(),
        "{impact}"
    );
    let text = query(&ws, "tests/money.test.ts", Format::Text);
    assert!(!text.contains("no importers:"), "{text}");
}

#[test]
fn nothing_to_report_adds_nothing() {
    let dir = std::env::temp_dir().join(format!("archmap-not-traced-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("pkg")).unwrap();
    std::fs::write(dir.join("pkg/__init__.py"), "from pkg import core\n").unwrap();
    std::fs::write(dir.join("pkg/core.py"), "def run():\n    pass\n").unwrap();
    let ws = scan(&dir);
    let text = query(&ws, "pkg/core.py", Format::Text);
    let json = query(&ws, "pkg/core.py", Format::Json);
    let impact = impact(&ws, "pkg/core.py");
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(!text.contains("Not traced"), "{text}");
    assert!(!json.contains("not_traced"), "{json}");
    assert!(impact.get("not_traced").is_none(), "{impact}");
}

/// A throwaway Python repository, removed when the guard drops.
struct Repo(PathBuf);

impl Repo {
    fn new(name: &str, files: &[(&str, &str)]) -> Repo {
        let dir = std::env::temp_dir().join(format!("archmap-nt-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (file, text) in files {
            let path = dir.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        Repo(dir)
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn dynamic_imports_in_test_code_come_last_and_say_so() {
    let repo = Repo::new(
        "dynamic-tests",
        &[
            (
                "tests/test_x.py",
                "import importlib\ndef test_x(n):\n    importlib.import_module(n)\n",
            ),
            // by path the test file would come first: production code goes first
            ("zz/__init__.py", ""),
            (
                "zz/a.py",
                "import importlib\ndef load(name):\n    return importlib.import_module(name)\n",
            ),
            ("pkg/__init__.py", ""),
            ("pkg/b.py", "def f():\n    pass\n"),
        ],
    );
    let ws = scan(&repo.0);
    let text = query(&ws, "pkg/b.py", Format::Text);
    assert!(
        text.contains(
            "\n  dynamic: 2 calls load modules by computed names, which may be this: \
             zz/a.py:3, tests/test_x.py:3 (test)\n"
        ),
        "{text}"
    );
}

#[test]
fn a_relative_import_is_named_like_only_the_file_it_points_at() {
    let repo = Repo::new(
        "named-relative",
        &[
            ("package.json", "{\"name\": \"web\"}\n"),
            ("src/lib/utils.ts", "export const u = 1;\n"),
            // `./utils` points at src/app/utils, which is not there; the
            // wrong extension points at the target
            (
                "src/app/x.ts",
                "import { u } from './utils';\nimport { m } from '../lib/utils.mjs';\nexport const x = u + m;\n",
            ),
        ],
    );
    let ws = scan(&repo.0);
    let text = query(&ws, "src/lib/utils.ts", Format::Text);
    assert!(
        text.contains(
            "\n  named like it: 1 import of `utils` maps to no file: src/app/x.ts:2 (unresolved)\n"
        ),
        "{text}"
    );
}

#[test]
fn a_dotted_import_is_named_like_the_files_its_path_ends_in() {
    let repo = Repo::new(
        "named-dotted",
        &[
            (
                "pyproject.toml",
                "[project]\nname = \"p\"\nversion = \"0.1.0\"\n",
            ),
            ("scripts/lib/helpers.py", "X = 1\n"),
            ("scripts/helpers.py", "Y = 1\n"),
            // run with scripts/ on sys.path
            ("scripts/run.py", "from lib.helpers import X\n"),
        ],
    );
    let ws = scan(&repo.0);
    let lib = query(&ws, "scripts/lib/helpers.py", Format::Text);
    assert!(
        lib.contains("\n  named like it: 1 import of `helpers` maps to no file: scripts/run.py:1 (local name)\n"),
        "{lib}"
    );
    // only the last name matches here: lib.helpers is not scripts/helpers.py
    let other = query(&ws, "scripts/helpers.py", Format::Text);
    assert!(!other.contains("named like it"), "{other}");
}

#[test]
fn an_alias_is_named_like_files_of_its_own_package_only() {
    let repo = Repo::new(
        "named-alias",
        &[
            (
                "package.json",
                "{\"name\": \"root\", \"private\": true, \"workspaces\": [\"packages/*\"]}\n",
            ),
            ("packages/a/package.json", "{\"name\": \"a\"}\n"),
            ("packages/a/src/lib/utils.ts", "export const u = 1;\n"),
            (
                "packages/a/src/y.ts",
                "import { u } from '@/lib/utils';\nexport const y = u;\n",
            ),
            ("packages/b/package.json", "{\"name\": \"b\"}\n"),
            (
                "packages/b/src/x.ts",
                "import { u } from '@/lib/utils';\nexport const x = u;\n",
            ),
        ],
    );
    let ws = scan(&repo.0);
    let text = query(&ws, "packages/a/src/lib/utils.ts", Format::Text);
    assert!(
        text.contains(
            "\n  named like it: 1 import of `utils` maps to no file: packages/a/src/y.ts:1 (unresolved)\n"
        ),
        "{text}"
    );
}
