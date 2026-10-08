//! What `query` and `impact` could not follow for their target, counted
//! from what the analyzers record, and only when something applies.

use std::path::{Path, PathBuf};

use archmap_app::{
    BySymbolRequest, Format, ImpactRequest, QueryRequest, ScanMode, Workspace, DEFAULT_DEPTH,
};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name)
}

/// Fixtures sit inside archmap's own repository: its history stays out of
/// their answers, which read them as roots without one.
fn scan(root: &Path) -> Workspace {
    static CEILING: std::sync::Once = std::sync::Once::new();
    CEILING.call_once(|| {
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures");
        std::env::set_var("GIT_CEILING_DIRECTORIES", fixtures);
    });
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
    let section = text
        .split_once("\nNot traced (what this answer may miss):\n")
        .map(|(_, s)| s);
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
        json["not_traced"]["named_like"]["locations"][0]["module"],
        "helpers"
    );
}

#[test]
fn a_symbol_lists_the_imports_named_like_its_file() {
    // a test that imports the module by a name a `sys.path` entry added at
    // runtime makes it reach: no uses read there, so the answer hedges
    let ws = scan(&fixture("python-uses"));
    let text = query(&ws, "refund", Format::Text);
    assert!(
        text.contains(
            "\n  named like it: 1 import of `charge` maps to no file: \
             bazaar/tests/test_flat.py:1 (local name)\n"
        ),
        "{text}"
    );
    let json = impact(&ws, "refund");
    assert_eq!(
        json["not_traced"]["named_like"]["locations"][0]["file"],
        "bazaar/tests/test_flat.py"
    );
}

#[test]
fn a_ts_file_counts_the_dynamic_imports_of_ts_and_js_and_an_alias_named_like_it() {
    let ws = scan(&fixture("simple-ts-project"));
    let text = query(&ws, "src/components/button.tsx", Format::Text);
    // `import(`./${name}`)` in src/app/ loads nothing of src/components/
    assert!(
        text.contains(
            "\n  dynamic: 1 call loads a module by a computed name, which may be this: \
             scripts/report.cjs:4\n"
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
    let text = query(&ws, "src/app/page.tsx", Format::Text);
    assert!(
        text.contains(
            "\n  dynamic: 2 calls load modules by computed names, which may be this: \
             scripts/report.cjs:4, src/app/lazy.tsx:7 (below src/app/)\n"
        ),
        "{text}"
    );
}

#[test]
fn a_computed_name_with_a_static_start_counts_only_for_targets_below_it() {
    // `import_module(f"store.{name}")` loads a module of the package `store`
    let ws = scan(&fixture("python-bindings"));
    let text = query(&ws, "store/billing/charge.py", Format::Text);
    assert!(
        text.contains(
            "\n  dynamic: 1 call loads a module by a computed name, which may be this: \
             store/loader.py:17 (below store/)\n"
        ),
        "{text}"
    );
    let text = query(&ws, "spec/test_pay.py", Format::Text);
    assert!(!text.contains("dynamic:"), "{text}");
    let charge = query(&ws, "store/billing/charge.py", Format::Json);
    let json: serde_json::Value = serde_json::from_str(&charge).unwrap();
    assert_eq!(
        json["not_traced"]["dynamic"]["locations"][0]["below"], "store/",
        "{json}"
    );
}

#[test]
fn a_component_at_the_root_holds_every_prefix() {
    // its path is `.`: a call that loads only below src/app/ may load it
    let ws = scan(&fixture("simple-ts-project"));
    let json = impact(&ws, "ts-shop");
    let calls: Vec<&str> = json["not_traced"]["dynamic"]["locations"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c["file"].as_str())
        .collect();
    assert!(calls.contains(&"src/app/lazy.tsx"), "{json}");
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
fn a_macro_call_not_read_that_names_the_target_is_listed() {
    let ws = scan(&fixture("rust-cargo-targets"));
    let text = query(&ws, "kiosk/src/stamp.rs", Format::Text);
    assert!(
        text.contains(
            "\nNot traced (what this answer may miss):\n  macros: 1 macro call whose arguments are not read names `stamp`: \
             kiosk/src/lib.rs:27 (tally!)\n"
        ),
        "{text}"
    );
    // a module no such call names has no such line
    let util = query(&ws, "kiosk/src/util.rs", Format::Text);
    assert!(!util.contains("macros:"), "{util}");
    // the library's root goes by its crate's name, a module by its own; a
    // test's root is a crate no path names
    let lib = query(&ws, "kiosk/src/lib.rs", Format::Text);
    assert!(
        lib.contains("  macros: 1 macro call whose arguments are not read names `kiosk`: kiosk/src/bin/report.rs:18 (tally2!)\n"),
        "{lib}"
    );
    let till = query(&ws, "kiosk/src/till.rs", Format::Text);
    assert!(
        till.contains("names `till`: kiosk/src/bin/report.rs:18 (tally2!)"),
        "{till}"
    );
    let test = query(&ws, "kiosk/tests/total.rs", Format::Text);
    assert!(!test.contains("macros:"), "{test}");
}

#[test]
fn a_rust_target_counts_the_files_its_analyzer_did_not_read() {
    let ws = scan(&fixture("rust-cargo-targets"));
    let text = query(&ws, "kiosk/src/util.rs", Format::Text);
    // test data no Cargo target loads is not read, by design: say which
    // files these are
    assert!(
        text.ends_with(
            "\nNot traced (what this answer may miss):\n  not read: 3 of 26 rust files: the Rust analyzer reads src/ and what \
             the other Cargo targets load, so files outside src/ that no target loads, such as \
             test data, are among them, as is any file that failed to parse\n"
        ),
        "{text}"
    );
    let json = impact(&ws, "kiosk/src/util.rs");
    let not_read = &json["not_traced"]["not_read"];
    assert_eq!(not_read["languages"], serde_json::json!(["rust"]));
    assert_eq!(
        (&not_read["files"], &not_read["read"]),
        (&26.into(), &23.into())
    );
    assert!(not_read["note"]
        .as_str()
        .is_some_and(|n| n.contains("no target loads")));
}

const NO_IMPORTERS: &str = "no import of it was found: only import statements are read, \
     so a file that a framework, a test runner or a command loads by name or path has none";
const GLOBAL: &str =
    "declarations in `declare global` are global, so no import names what uses them";

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
        text.contains(&format!(
            "\nNot traced (what this answer may miss):\n  no importers: {NO_IMPORTERS}\n"
        )),
        "{text}"
    );
    // the script's text says it where it lists importers
    let script = query(&ws, "src/global.d.ts", Format::Text);
    assert!(!script.contains("no importers:"), "{script}");
}

#[test]
fn what_declare_global_declares_says_that_no_import_names_its_uses() {
    let ws = scan(&fixture("ts-globals"));
    let not_traced = |text: &str| {
        text.split_once("\nNot traced (what this answer may miss):\n")
            .map(|(_, s)| s.to_owned())
    };
    // a symbol: no import names it, and the uses pass reads its own file,
    // where code reaches it as a global
    let text = query(&ws, "registry", Format::Text);
    assert!(
        text.contains(
            "\nImported by: none (`declare global` declares it: what uses it is not traced)\n"
        ),
        "{text}"
    );
    assert!(
        text.contains(
            "\nUsed at: in its own file only: 1 in 1 file (1 read)\n  \
             src/setup.ts:5 (read) as globalThis.registry\n"
        ),
        "{text}"
    );
    assert_eq!(not_traced(&text), None, "{text}");
    // a file that declares one, imported for what it runs: its importers
    // stay, and the line says what they do not show
    let text = query(&ws, "src/setup.ts", Format::Text);
    assert!(text.contains("\nImported by: 2\n"), "{text}");
    assert_eq!(
        not_traced(&text).as_deref(),
        Some(&*format!("  {GLOBAL}\n"))
    );
    // one nothing imports: not a file a framework loads by name
    let text = query(&ws, "src/global.d.ts", Format::Text);
    assert_eq!(
        not_traced(&text).as_deref(),
        Some(&*format!("  {GLOBAL}\n"))
    );
    // impact on the symbol: an import that loads its file for what it runs
    // takes no name, so neither lists it nor leaves it out as never naming
    // it, and the line says why the reach is empty
    let symbol = impact(&ws, "registry");
    assert_eq!(symbol["not_traced"]["global"], GLOBAL, "{symbol}");
    assert_eq!(symbol["may_use"]["total"], 0, "{symbol}");
    assert!(symbol.get("unnamed").is_none(), "{symbol}");
    assert!(
        symbol["not_traced"].get("no_importers").is_none(),
        "{symbol}"
    );
    let file = impact(&ws, "src/global.d.ts");
    assert_eq!(file["not_traced"]["global"], GLOBAL, "{file}");
    assert!(file["not_traced"].get("no_importers").is_none(), "{file}");
    // a module that only exports declares nothing global
    let plain = impact(&ws, "src/main.ts");
    assert!(plain["not_traced"].get("global").is_none(), "{plain}");
    // the package that holds them
    let package = impact(&ws, "ts-globals");
    assert_eq!(package["not_traced"]["global"], GLOBAL, "{package}");
    let text = query(&ws, "ts-globals", Format::Text);
    assert!(text.contains(&format!("\n  {GLOBAL}\n")), "{text}");
}

#[test]
fn a_computed_import_counts_for_the_packages_it_runs_first_and_says_so() {
    let root = std::env::temp_dir().join(format!("archmap-first-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for (path, text) in [
        ("pyproject.toml", "[project]\nname = \"loaders\"\n"),
        ("app/__init__.py", ""),
        ("app/plugins/__init__.py", ""),
        (
            "tools/loader.py",
            "import importlib\n\n\ndef load(name):\n    return importlib.import_module(f\"app.plugins.{name}\")\n",
        ),
    ] {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let ws = scan(&root);
    let text = query(&ws, "app/__init__.py", Format::Text);
    std::fs::remove_dir_all(&root).unwrap();
    assert!(
        text.contains("tools/loader.py:5 (below app/plugins/, which runs it first)"),
        "{text}"
    );
}

#[test]
fn a_global_says_so_where_importers_are_unknown() {
    // no import of this language names a file, so importers are unknown
    let root = std::env::temp_dir().join(format!("archmap-globals-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("package.json"),
        "{ \"name\": \"g\", \"private\": true }",
    )
    .unwrap();
    std::fs::write(
        root.join("src/env.d.ts"),
        "export {};\ndeclare global {\n  var CONFIG: string;\n}\n",
    )
    .unwrap();
    let ws = scan(&root);
    let text = query(&ws, "CONFIG", Format::Text);
    std::fs::remove_dir_all(&root).unwrap();
    assert!(
        text.contains(
            "\nImported by: unknown (no evidence names imported files for typescript; \
             `declare global` declares it: what uses it is not traced)\n"
        ),
        "{text}"
    );
}

#[test]
fn a_script_that_lists_importers_still_says_it_is_one() {
    // `import './polyfill.js'` loads the script for what it runs; its
    // globals are used elsewhere all the same
    let ws = scan(&fixture("ts-globals"));
    let text = query(&ws, "src/polyfill.js", Format::Text);
    assert!(text.contains("\nImported by: 1\n"), "{text}");
    assert!(
        text.ends_with(
            "\nNot traced (what this answer may miss):\n  \
             a script: its declarations are global, so no import names what uses them\n"
        ),
        "{text}"
    );
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

#[test]
fn a_static_member_names_where_its_class_is_used_as_a_value() {
    // `make(Wallet)`, `const W = Wallet`, `make(w.Wallet)`: code there may
    // call `Wallet.open` unseen, so their statements are no negative fact
    let ws = scan(&fixture("ts-static-members"));
    let text = query(&ws, "Wallet.open", Format::Text);
    assert!(
        text.ends_with(
            "\nNot traced (what this answer may miss):\n  \
             class values: 3 places use the class as a value, which may call this: \
             src/alias.ts:3, src/boot.ts:4, src/registry.ts:7\n"
        ),
        "{text}"
    );
    assert!(
        text.contains("\n  never used (1 import): src/plain.ts:1\n"),
        "{text}"
    );
    // the statement that takes the module whole stays in the reach
    let impact = impact(&ws, "Wallet.open");
    assert_eq!(impact["not_traced"]["class_values"]["total"], 3, "{impact}");
    assert!(impact.get("unnamed").is_none(), "{impact}");
    assert_eq!(
        impact["tests"]["files"][0]["file"], "tests/boot.test.ts",
        "{impact}"
    );
}

#[test]
fn a_rust_trait_lists_the_statements_that_bring_it_into_scope() {
    // `use crate::method::Method as _;` lets `card.fee()` call it unseen;
    // `use crate::method;` binds the module whole and calls only `flat`
    let ws = scan(&fixture("rust-uses"));
    let text = query(&ws, "wallet::method::Method", Format::Text);
    assert!(
        text.contains(
            "  values: calls of its methods through values of the types that implement it \
             are not read; brought into scope by 1 import: wallet/src/fees.rs:3\n"
        ),
        "{text}"
    );
    let rows = ws
        .by_symbol(&BySymbolRequest {
            target: "wallet/src/method.rs",
            depth: DEFAULT_DEPTH,
            format: Format::Text,
            verbose: false,
        })
        .unwrap()
        .output;
    assert!(
        rows.contains("Method  imported by 3; may use 1; used at 6 in 2 files; calls of its methods through values are not read"),
        "{rows}"
    );
    let impact = impact(&ws, "wallet::method::Method");
    assert_eq!(impact["not_traced"]["values"]["total"], 1, "{impact}");
}
