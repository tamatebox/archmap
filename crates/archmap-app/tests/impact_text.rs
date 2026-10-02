//! `impact` as text, its default: query's headings, locations and marks,
//! capped lists that say how many they show, and what could not be traced.

use std::path::{Path, PathBuf};

use archmap_app::{Format, ImpactRequest, ScanMode, Workspace, DEFAULT_DEPTH};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name)
}

/// A throwaway repository with `files`, removed when the guard drops.
struct Repo(PathBuf);

impl Repo {
    fn new(name: &str, files: &[(String, String)]) -> Repo {
        let dir =
            std::env::temp_dir().join(format!("archmap-app-impact-{name}-{}", std::process::id()));
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

fn impact(ws: &Workspace, target: &str, depth: usize, verbose: bool) -> String {
    ws.impact(&ImpactRequest {
        target,
        depth,
        format: Format::Text,
        verbose,
    })
    .unwrap()
    .output
}

fn text(ws: &Workspace, target: &str) -> String {
    impact(ws, target, DEFAULT_DEPTH, false)
}

/// The lines below the line `heading`, up to the next blank line.
fn section<'a>(text: &'a str, heading: &str) -> Vec<&'a str> {
    let mut lines = text.lines();
    lines
        .find(|l| *l == heading)
        .unwrap_or_else(|| panic!("no `{heading}` in:\n{text}"));
    lines.take_while(|l| !l.is_empty()).collect()
}

/// One TS package where every list of `impact src/lib/core.ts` passes its
/// cap: 35 files import it and 35 more import those, 25 test files import
/// it, and 4 calls load modules by computed names.
fn busy_package(name: &str) -> Repo {
    let mut files = vec![
        ("package.json".to_owned(), r#"{"name": "busy"}"#.to_owned()),
        (
            "src/lib/core.ts".to_owned(),
            "export const core = 1;\n".to_owned(),
        ),
        (
            "src/lib/only.ts".to_owned(),
            "export const only = 1;\n".to_owned(),
        ),
        (
            "tests/only.test.ts".to_owned(),
            "import { only } from '../src/lib/only';\n".to_owned(),
        ),
    ];
    for i in 0..35 {
        files.push((
            format!("src/lib/d{i}.ts"),
            format!("import {{ core }} from './core';\nexport const d{i} = core;\n"),
        ));
        files.push((
            format!("src/app/t{i}.ts"),
            format!("import {{ d{i} }} from '../lib/d{i}';\nexport const t{i} = d{i};\n"),
        ));
    }
    for i in 0..25 {
        files.push((
            format!("tests/c{i}.test.ts"),
            "import { core } from '../src/lib/core';\n".to_owned(),
        ));
    }
    for i in 0..4 {
        files.push((
            format!("src/app/load{i}.ts"),
            "export function load(name: string) {\n  return import(`./${name}`);\n}\n".to_owned(),
        ));
    }
    Repo::new(name, &files)
}

#[test]
fn a_file_answers_with_its_dependents_statements_tests_and_blind_spots() {
    let ws = scan(&fixture("simple-ts-project"));
    assert_eq!(
        text(&ws, "src/lib/types.ts"),
        "src/lib/types.ts (file) in lib/types.ts (module, typescript), depth 2\n\
         id: ts-shop::src/lib/types.ts\n\
         \n\
         Direct dependents: 4\n  \
           app/checkout.ts  2 imports\n  \
           app/page.tsx  1 import\n  \
           lib/money.ts  1 import\n  \
           ts-shop  1 import\n\
         \n\
         Imported by: 6, showing 5 (1 re-export)\n  \
           src/app/checkout.ts:8 (via src/index.ts:8) (type)\n  \
           src/app/checkout.ts:9 (via src/index.ts:8) (type)\n  \
           src/app/page.tsx:2 (type)\n  \
           src/index.ts:8 (export) (type)  in ts-shop\n  \
           src/lib/money.ts:4 (type)\n  \
           1 more in: tests/money.test.ts 1\n\
         \n\
         Transitive dependents: 2 more (6 in all)\n  \
           app/lazy.tsx  2 steps, through src/lib/money.ts\n  \
           scripts/report.cjs  2 steps, through src/lib/money.ts\n\
         \n\
         Tests to run again: 2\n  \
           tests/helpers.ts\n  \
           tests/money.test.ts\n\
         \n\
         Changed in the same commits: not read (not a git repository)\n\
         \n\
         Not traced:\n  \
           dynamic: 2 calls load modules by computed names, which may be this: \
           scripts/report.cjs:4, src/app/lazy.tsx:7\n  \
           barrels: 1 file passes on what may change, and only what takes it from there is followed; a \
           rename, a removal or an error on load also breaks whatever else loads that file: \
           src/index.ts:8\n\
         \n\
         Lists are capped; verbose lists every entry.\n"
    );
}

#[test]
fn every_list_is_capped_and_says_how_many_it_shows() {
    let repo = busy_package("capped");
    let ws = scan(&repo.0);
    let out = text(&ws, "src/lib/core.ts");

    assert_eq!(section(&out, "Direct dependents: 35, showing 30").len(), 30);
    assert_eq!(
        section(
            &out,
            "Transitive dependents: 35 more (70 in all), showing 30"
        )
        .len(),
        30
    );
    // and the components the statements not shown are in
    let importers = section(&out, "Imported by: 60, showing 5");
    assert_eq!(importers.len(), 6);
    assert!(importers[5].starts_with("  55 more in: "), "{importers:?}");
    assert_eq!(
        section(&out, "Tests to run again: 25, showing 20").len(),
        20
    );
    let not_traced = section(&out, "Not traced:");
    assert_eq!(
        not_traced,
        [
            "  dynamic: 4 calls load modules by computed names, which may be this: \
             src/app/load0.ts:2, src/app/load1.ts:2, src/app/load2.ts:2, +1 more"
        ]
    );
    assert!(
        out.ends_with("\n\nLists are capped; verbose lists every entry.\n"),
        "{out}"
    );
}

#[test]
fn verbose_lists_every_entry() {
    let repo = busy_package("verbose");
    let ws = scan(&repo.0);
    let out = impact(&ws, "src/lib/core.ts", DEFAULT_DEPTH, true);

    assert_eq!(section(&out, "Direct dependents: 35").len(), 35);
    assert_eq!(
        section(&out, "Transitive dependents: 35 more (70 in all)").len(),
        35
    );
    assert_eq!(section(&out, "Imported by: 60").len(), 60);
    assert_eq!(section(&out, "Tests to run again: 25").len(), 25);
    assert!(section(&out, "Not traced:")[0].ends_with("src/app/load3.ts:2"));
    assert!(!out.contains("Lists are capped"), "{out}");
}

#[test]
fn no_direct_dependent_says_where_the_importers_are() {
    let repo = busy_package("no-direct");
    let ws = scan(&repo.0);
    // at depth 1 every file that imports core.ts is in its own component
    let own = impact(&ws, "src/lib/core.ts", 1, false);
    assert!(
        own.contains("\nDirect dependents: none outside its own component\n"),
        "{own}"
    );
    assert!(own.contains("\nImported by: 60, showing 5\n"), "{own}");
    // only a test imports only.ts
    let tested = text(&ws, "src/lib/only.ts");
    assert!(
        tested.contains("\nDirect dependents: none (only test code imports it)\n"),
        "{tested}"
    );
    assert!(tested.contains("\nTests to run again: 1\n"), "{tested}");
}

#[test]
fn a_component_points_at_query_for_where_it_is_imported() {
    let repo = busy_package("component");
    let ws = scan(&repo.0);
    let out = text(&ws, "src/lib");
    assert!(
        out.starts_with("lib (module, typescript) at src/lib, depth 2\nid: busy::src/lib\n"),
        "{out}"
    );
    assert!(
        out.contains(
            "\nImported by: not listed for a whole component: \
             `query busy::src/lib` shows where it is imported\n"
        ),
        "{out}"
    );
}

#[test]
fn a_symbol_answers_with_its_line_and_the_statements_that_take_it() {
    let ws = scan(&fixture("simple-ts-project"));
    let out = text(&ws, "formatPrice");
    assert!(
        out.starts_with(
            "export function formatPrice(price: Money): string  src/lib/money.ts:8  \
             in lib/money.ts, depth 2\n\
             id: ts-shop::src/lib/money.ts::formatPrice\n"
        ),
        "{out}"
    );
    assert!(out.contains("\nImported by: "), "{out}");
    // checkout.ts:2 takes the barrel that passes the name on whole
    assert!(
        out.contains(
            "\nMay use: 3 (imports the whole module; 1 re-export)\n  scripts/report.cjs:8 (local)\n  \
             src/app/checkout.ts:2 (whole src/index.ts, which passes it on)\n"
        ),
        "{out}"
    );
}

#[test]
fn an_import_name_answers_with_the_statements_that_import_it() {
    let ws = scan(&fixture("simple-python-project"));
    let out = text(&ws, "pytest");
    assert!(
        out.starts_with("pytest: imports without an edge, depth 2\n"),
        "{out}"
    );
    assert!(out.contains("\nImported by: "), "{out}");
    assert!(out.contains("\nTests to run again: "), "{out}");
}

#[test]
fn a_script_says_what_uses_its_globals_is_not_traced() {
    let ws = scan(&fixture("simple-ts-project"));
    let out = text(&ws, "src/global.d.ts");
    assert!(
        section(&out, "Not traced:").contains(
            &"  a script: its declarations are global, so no import names what uses them"
        ),
        "{out}"
    );
}

#[test]
fn a_test_file_marks_itself_among_the_tests_to_run_again() {
    let ws = scan(&fixture("simple-ts-project"));
    let out = text(&ws, "tests/helpers.ts");
    assert_eq!(
        section(&out, "Tests to run again: 2"),
        [
            "  tests/helpers.ts (the target itself)",
            "  tests/money.test.ts"
        ]
    );
}

#[test]
fn a_barrel_leads_on_only_to_what_may_take_the_files_names() {
    let ws = scan(&fixture("ts-reexports"));
    // a module that re-exports one name beside its own code
    let url = text(&ws, "src/url.ts");
    assert_eq!(
        section(&url, "Direct dependents: 2"),
        ["  app/link.ts  1 import", "  storage.ts  1 import"]
    );
    assert!(
        url.contains("\nTransitive dependents: none beyond the direct ones\n"),
        "{url}"
    );
    assert_eq!(
        section(&url, "Tests to run again: 1"),
        ["  tests/link.test.ts"]
    );
    // a barrel that re-exports two files and a package whole: what takes it
    // whole or only loads it, a name both files define, and what uses the
    // file; not the other file's names, nor the package's
    let money = text(&ws, "src/money.ts");
    let transitive = section(&money, "Transitive dependents: 5 more (9 in all)");
    assert_eq!(
        transitive,
        [
            "  app/boot.ts  2 steps, through src/shop/index.ts",
            "  app/cart.ts  2 steps, through src/checkout.ts",
            "  app/report.ts  2 steps, through src/shop/index.ts",
            "  app/tag.ts  2 steps, through src/shop/index.ts",
            "  ts-reexports  2 steps, through src/shop/index.ts"
        ]
    );
    for other in [
        "app/agenda.ts",
        "app/calendar.ts",
        "app/home.ts",
        "app/press.ts",
    ] {
        assert!(
            !money.contains(&format!("  {other}  ")),
            "{other} in:\n{money}"
        );
    }
    assert!(money.contains("\nTests to run again: none\n"), "{money}");
    // the barrel itself changed: whatever takes names through it
    let shop = text(&ws, "src/shop/index.ts");
    for reached in [
        "app/calendar.ts",
        "app/home.ts",
        "app/press.ts",
        "app/sale.ts",
    ] {
        assert!(
            shop.contains(&format!("  {reached}  ")),
            "{reached} in:\n{shop}"
        );
    }
}

#[test]
fn a_test_whose_mock_replaces_a_module_on_the_way_is_left_out() {
    let ws = scan(&fixture("ts-mocks"));
    // what the mocked module imports: those tests that run it stay
    let pricing = text(&ws, "src/pricing.ts");
    assert_eq!(
        section(&pricing, "Tests to run again: 7"),
        [
            "  tests/actual.test.ts",
            "  tests/auto.test.ts",
            "  tests/both.test.ts",
            "  tests/helper.test.ts",
            "  tests/inside.test.ts",
            "  tests/original.test.ts",
            "  tests/passed.test.ts",
            "  left out: 5 test files reach it only through modules their mocks replace: \
             tests/barrel.test.ts:4 (mocks src/index.ts), tests/jest.test.ts:3 (mocks \
             src/orders.ts), tests/replaced.test.ts:4 (mocks src/orders.ts), +2 more",
        ]
    );
    // a test that takes types of the mocked module: a mock replaces no type
    let types = text(&ws, "src/types.ts");
    assert_eq!(
        section(&types, "Tests to run again: 1"),
        ["  tests/typed.test.ts"]
    );
    // a module that re-exports a name: only the mock that gives that name
    // depends on it
    let url = text(&ws, "src/url.ts");
    assert_eq!(
        section(&url, "Tests to run again: 1"),
        ["  tests/link.test.ts"]
    );
    // a mock of the changed file, or of a barrel of it, that gives it a
    // name it exports, the default by its declared name too; not one that
    // gives it none of its names
    let audio = text(&ws, "src/audio.ts");
    assert_eq!(
        section(&audio, "Tests to run again: 2"),
        [
            "  tests/default.test.ts",
            "  tests/media.test.ts",
            "  left out: 1 test file reaches it only through a module its mock replaces: \
             tests/unrelated.test.ts:4 (mocks src/audio.ts)",
        ]
    );
    // the mocked module itself: the mocks replace its names
    let orders = text(&ws, "src/orders.ts");
    assert!(orders.contains("\nTests to run again: 11\n"), "{orders}");
    assert!(!orders.contains("left out"), "{orders}");
    let every = impact(&ws, "src/orders.ts", DEFAULT_DEPTH, true);
    assert!(
        every.contains("\n  tests/replaced.test.ts:4 (mock) (test)\n"),
        "{every}"
    );
}

#[test]
fn a_python_symbol_is_taken_by_name_through_a_module_binding() {
    let ws = scan(&fixture("python-bindings"));
    let out = ws
        .query(&archmap_app::QueryRequest {
            target: "pay",
            depth: DEFAULT_DEPTH,
            format: Format::Text,
            verbose: true,
        })
        .unwrap()
        .output;
    // read through the package, a submodule or an `as` name; the package
    // only passes it on
    assert_eq!(
        section(&out, "Imported by: 6 (1 re-export)"),
        [
            "  store/aliased.py:1",
            "  store/app.py:1 (via store/billing/__init__.py:2)",
            "  store/billing/__init__.py:2 (export)",
            "  store/formatted.py:1",
            "  store/other.py:1",
            "  spec/test_pay.py:1 (test)",
        ]
    );
    // the files that may use anything of it; not one that reads another
    // name through it
    assert_eq!(
        section(&out, "May use: 3 (imports the whole module)"),
        [
            "  store/annotated.py:1",
            "  store/passed.py:1",
            "  store/unused.py:1",
        ]
    );
}

#[test]
fn a_python_package_that_only_passes_a_name_on_is_followed_by_that_name() {
    let ws = scan(&fixture("python-bindings"));
    let out = text(&ws, "pay");
    // not a test that imports a module below the package, nor one that
    // takes another name from it
    assert_eq!(
        section(&out, "Tests to run again: 1"),
        ["  spec/test_pay.py"]
    );
    assert!(
        out.contains(
            "\n  barrels: 1 file passes on what may change, and only what takes it from there is followed; \
             a rename, a removal or an error on load also breaks whatever else loads that file: \
             store/billing/__init__.py:2 (runs first; 2 test files that load it or a module \
             below it are not listed)\n"
        ),
        "{out}"
    );
}

#[test]
fn a_package_entry_reached_through_its_re_exports_runs_nothing_below_it() {
    let ws = scan(&fixture("python-bindings"));
    // money.py reaches the package's `__init__.py` only through the names it
    // passes on from charge.py: not a test that imports a module below it
    let out = text(&ws, "store/billing/money.py");
    assert_eq!(
        section(&out, "Tests to run again: 2"),
        ["  spec/test_pay.py", "  spec/test_refund.py"]
    );
    assert!(
        out.contains(
            "store/billing/__init__.py:2 (runs first; 1 test file that loads it or a module \
             below it is not listed)"
        ),
        "{out}"
    );
}

#[test]
fn a_package_entry_is_named_at_the_re_export_the_reach_came_through() {
    let ws = scan(&fixture("python-bindings"));
    // the package's `__init__.py` passes on the names of duty.py at its last
    // re-export, after those of another file
    for target in ["store/billing/levy.py", "charge_duty"] {
        let out = text(&ws, target);
        assert!(
            out.contains("that file: store/billing/__init__.py:6 (runs first;"),
            "{out}"
        );
    }
    // money.py reaches it through charge.py and duty.py: the nearest way's
    // re-export, and every one on a way in JSON
    let out = text(&ws, "store/billing/money.py");
    assert!(
        out.contains("that file: store/billing/__init__.py:2 (runs first;"),
        "{out}"
    );
    let json = ws
        .impact(&ImpactRequest {
            target: "store/billing/money.py",
            depth: DEFAULT_DEPTH,
            format: Format::Json,
            verbose: false,
        })
        .unwrap()
        .output;
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(
        value["not_traced"]["barrels"]["shown"][0]["lines"],
        serde_json::json!([2, 3, 6])
    );
}

#[test]
fn a_dependent_a_declaration_reaches_names_what_it_declares_and_where() {
    let manifest = |name: &str, dependency: &str| {
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n\n[dependencies]\n{dependency}")
    };
    let files = [
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"base\", \"mid\", \"top\"]\n".to_owned(),
        ),
        ("base/Cargo.toml", manifest("base", "")),
        (
            "base/src/lib.rs",
            "pub fn rate() -> u32 {\n    1\n}\n".to_owned(),
        ),
        (
            "mid/Cargo.toml",
            manifest("mid", "base = { path = \"../base\" }\n"),
        ),
        (
            "mid/src/lib.rs",
            "pub fn price() -> u32 {\n    base::rate()\n}\n".to_owned(),
        ),
        // top uses nothing of mid: its manifest's declaration is the way
        (
            "top/Cargo.toml",
            manifest("top", "mid = { path = \"../mid\" }\n"),
        ),
        ("top/src/main.rs", "fn main() {}\n".to_owned()),
    ];
    let files: Vec<(String, String)> = files
        .into_iter()
        .map(|(path, text)| (path.to_owned(), text))
        .collect();
    let repo = Repo::new("declared", &files);
    let ws = scan(&repo.0);
    let out = text(&ws, "base/src/lib.rs");
    assert_eq!(
        section(&out, "Transitive dependents: 1 more (2 in all)"),
        ["  top  2 steps, through mid (declared in top/Cargo.toml:6)"]
    );
    let json = ws
        .impact(&ImpactRequest {
            target: "base/src/lib.rs",
            depth: DEFAULT_DEPTH,
            format: Format::Json,
            verbose: false,
        })
        .unwrap()
        .output;
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(
        value["transitive"][1],
        serde_json::json!({
            "id": "top",
            "distance": 2,
            "from": "mid",
            "declared_in": {"file": "top/Cargo.toml", "line": 6}
        })
    );
}
