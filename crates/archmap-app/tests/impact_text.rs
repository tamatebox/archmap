//! `impact` as text, its default: query's headings, locations and marks,
//! capped lists that say how many they show, and what could not be traced.

use std::path::{Path, PathBuf};

use archmap_app::{Format, ImpactRequest, QueryRequest, ScanMode, Workspace, DEFAULT_DEPTH};

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
            "export function load(name: string) {\n  return import(`../lib/${name}`);\n}\n"
                .to_owned(),
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
           ts-shop (src/index.ts)  1 import\n\
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
         Tests to run again: 1\n  \
           tests/money.test.ts (mocks it)\n  \
           not tests: tests/helpers.ts (helper, for 1 test listed)\n\
         \n\
         Changed in the same commits: not read (not a git repository)\n\
         \n\
         Marks: (via file:line) reached through that re-export; (export) a re-export, passes \
         names on; (type) types only, never runs\n\
         \n\
         Not traced (what this answer may miss):\n  \
           dynamic: 1 call loads a module by a computed name, which may be this: \
           scripts/report.cjs:4\n  \
           barrels: 1 file passes on what may change, and only what takes it from there is followed; a \
           rename, a removal or an error on load also breaks whatever else loads that file: \
           src/index.ts:8 (+2 more re-exports on the way)\n\
         \n\
         Lists are capped; verbose lists every entry, and JSON every entry with all evidence.\n"
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
    let not_traced = section(&out, "Not traced (what this answer may miss):");
    assert_eq!(
        not_traced,
        [
            "  dynamic: 4 calls load modules by computed names, which may be this: \
             src/app/load0.ts:2 (below src/lib/), src/app/load1.ts:2 (below src/lib/), \
             src/app/load2.ts:2 (below src/lib/), +1 more"
        ]
    );
    assert!(
        out.ends_with("\n\nLists are capped; verbose lists every entry, and JSON every entry with all evidence.\n"),
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
    assert!(section(&out, "Not traced (what this answer may miss):")[0]
        .ends_with("src/app/load3.ts:2 (below src/lib/)"));
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
fn a_symbol_leaves_out_the_whole_module_imports_that_never_name_it() {
    // TS: a namespace import that reads only another export leaves; a
    // type of the module whole (`typeof import(..)` in a test) stays
    let ws = scan(&fixture("ts-uses"));
    let out = impact(&ws, "formatPrice", DEFAULT_DEPTH, true);
    assert!(
        out.contains(
            "  never named (1 import of the whole module, left out of the reach): \
             src/rates.ts:1\n"
        ),
        "{out}"
    );
    assert!(!out.contains("src/rates.ts:1  in"), "{out}");
    assert!(!out.contains("  rates.ts  "), "{out}");
    assert!(out.contains("  tests/actual.test.ts ("), "{out}");

    // Python: a star import that never names it leaves; one whose module
    // is used as a value stays, and so does one the pass could not read
    let ws = scan(&fixture("python-uses"));
    let out = text(&ws, "refund");
    assert!(
        out.contains(
            "  never named (1 import of the whole module, left out of the reach): \
             bazaar/starred.py:1\n"
        ),
        "{out}"
    );
    assert_eq!(
        section(&out, "May use: 2 (imports the whole module)"),
        [
            "  bazaar/dunder.py:1  in bazaar::bazaar",
            "  bazaar/escaped.py:1  in bazaar::bazaar"
        ]
    );
    let duty = text(&ws, "duty");
    assert_eq!(
        section(&duty, "May use: 1 (imports the whole module)"),
        ["  bazaar/built.py:1  in bazaar::bazaar"]
    );
    // an import that takes it by name stays without a use: it loads the
    // file all the same
    let pay = impact(&ws, "pay", DEFAULT_DEPTH, true);
    assert!(
        pay.contains("  never used (2 imports): bazaar/evaluated.py:1, bazaar/unused.py:1\n"),
        "{pay}"
    );
    assert!(
        pay.contains("bazaar/unused.py:1  in bazaar::bazaar"),
        "{pay}"
    );

    // JSON names the statements that left
    let json = ws
        .impact(&ImpactRequest {
            target: "refund",
            depth: DEFAULT_DEPTH,
            format: Format::Json,
            verbose: false,
        })
        .unwrap()
        .output;
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(
        value["unnamed"],
        serde_json::json!([{"file": "bazaar/starred.py", "line": 1}])
    );
    assert!(value["used_at"]["uses"].is_array(), "{value}");
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
        section(&out, "Not traced (what this answer may miss):").contains(
            &"  a script: its declarations are global, so no import names what uses them"
        ),
        "{out}"
    );
}

#[test]
fn a_changed_helper_is_listed_apart_as_the_target_itself() {
    let ws = scan(&fixture("simple-ts-project"));
    let out = text(&ws, "tests/helpers.ts");
    // no runner runs a helper as a test: the tests that import it are
    assert_eq!(
        section(&out, "Tests to run again: 1"),
        [
            "  tests/money.test.ts (takes it)",
            "  not tests: tests/helpers.ts (helper, the target itself, for 1 test listed)"
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
        ["  tests/link.test.ts (takes it, via src/storage.ts:1)"]
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
            "  ts-reexports (src/index.ts)  2 steps, through src/shop/index.ts"
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
            "  tests/actual.test.ts (through src/orders.ts)",
            "  tests/auto.test.ts (through src/orders.ts, by its mock)",
            "  tests/both.test.ts (through src/orders.ts)",
            "  tests/helper.test.ts (through src/orders.ts, by its mock)",
            "  tests/inside.test.ts (through src/orders.ts, by its mock)",
            "  tests/original.test.ts (through src/orders.ts, by its mock)",
            "  tests/passed.test.ts (through src/orders.ts, by its mock)",
            "  left out: 5 test files reach it only through modules their mocks replace: \
             tests/barrel.test.ts:4 (mocks src/index.ts), tests/jest.test.ts:3 (mocks \
             src/orders.ts), tests/replaced.test.ts:4 (mocks src/orders.ts), +2 more",
        ]
    );
    // a test that takes types of the mocked module: a mock replaces no type
    let types = text(&ws, "src/types.ts");
    assert_eq!(
        section(&types, "Tests to run again: 1"),
        ["  tests/typed.test.ts (through src/lines.ts, types only)"]
    );
    // a module that re-exports a name: only the mock that gives that name
    // depends on it
    let url = text(&ws, "src/url.ts");
    assert_eq!(
        section(&url, "Tests to run again: 1"),
        ["  tests/link.test.ts (takes it, via src/storage.ts:1)"]
    );
    // a mock of the changed file, or of a barrel of it, that gives it a
    // name it exports, the default by its declared name too; not one that
    // gives it none of its names
    let audio = text(&ws, "src/audio.ts");
    assert_eq!(
        section(&audio, "Tests to run again: 2"),
        [
            "  tests/default.test.ts (mocks it)",
            "  tests/media.test.ts (takes it, via src/media.ts:1)",
            "  left out: 1 test file reaches it only through a module its mock replaces: \
             tests/unrelated.test.ts:4 (mocks src/audio.ts)",
        ]
    );
    // a symbol of it names the same mock
    let symbol = text(&ws, "ts-mocks::src/audio.ts::getAudioUrl");
    assert!(
        symbol.contains(
            "  left out: 1 test file reaches it only through a module its mock replaces: \
             tests/unrelated.test.ts:4 (mocks src/audio.ts)\n"
        ),
        "{symbol}"
    );
    // a test left out is no test to run again, though it re-exports a
    // module on another way its mock cuts as well
    let files: Vec<(String, String)> = [
        (
            "package.json",
            r#"{"name": "relays", "devDependencies": {"vitest": "1.0.0"}}"#,
        ),
        ("src/api.ts", "export function get() {\n  return 1;\n}\n"),
        (
            "src/data.ts",
            "import { get } from './api';\n\nexport const data = get();\n",
        ),
        (
            "src/render.ts",
            "import { data } from './data';\n\nexport function render() {\n  return data;\n}\n",
        ),
        (
            "tests/utils.test.ts",
            "import { vi } from 'vitest';\nimport { data } from '../src/data';\n\
             export { render } from '../src/render';\n\n\
             vi.mock('../src/data', () => ({ other: 1 }));\n\nexport const value = data;\n",
        ),
    ]
    .into_iter()
    .map(|(file, text)| (file.to_owned(), text.to_owned()))
    .collect();
    let repo = Repo::new("left-out-relays", &files);
    let relays = text(&scan(&repo.0), "src/api.ts");
    assert_eq!(
        section(&relays, "Tests to run again: none"),
        [
            "  left out: 1 test file reaches it only through a module its mock replaces: \
             tests/utils.test.ts:5 (mocks src/data.ts)"
        ],
        "{relays}"
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
        ["  spec/test_pay.py (takes it)"]
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
        [
            "  spec/test_pay.py (through store/billing/charge.py)",
            "  spec/test_refund.py (through store/billing/charge.py)"
        ]
    );
    assert!(
        out.contains(
            "store/billing/__init__.py:2 (+2 more re-exports on the way; runs first; 1 test \
             file that loads it or a module below it is not listed)"
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
        out.contains(
            "that file: store/billing/__init__.py:2 (+2 more re-exports on the way; runs first;"
        ),
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
        value["not_traced"]["barrels"]["locations"][0]["lines"],
        serde_json::json!([2, 3, 6])
    );
}

#[test]
fn not_traced_names_every_barrel_the_reach_stopped_at() {
    let ws = scan(&fixture("ts-reexports"));
    // src/shop/index.ts passes money.ts on, and src/index.ts what
    // src/shop/index.ts passes on
    let out = text(&ws, "src/money.ts");
    assert!(
        out.contains("loads them: src/index.ts:1, src/shop/index.ts:1 ("),
        "{out}"
    );
    // a component's barrels too
    let out = text(&ws, "ts-reexports::src/shop");
    assert!(out.contains("loads that file: src/index.ts:1\n"), "{out}");
}

#[test]
fn a_rust_glob_stays_in_the_reach_where_its_file_may_use_the_symbol() {
    let manifest = |name: &str, dependency: &str| {
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n\n[dependencies]\n{dependency}")
    };
    let files: Vec<(String, String)> = [
        ("Cargo.toml", "[workspace]\nmembers = [\"lib\", \"app\"]\n".to_owned()),
        ("lib/Cargo.toml", manifest("lib", "")),
        ("lib/src/lib.rs", "pub mod shapes;\n".to_owned()),
        (
            "lib/src/shapes.rs",
            "pub fn square(n: u32) -> u32 {\n    n * n\n}\n\npub fn cube(n: u32) -> u32 {\n    n\n}\n"
                .to_owned(),
        ),
        ("app/Cargo.toml", manifest("app", "lib = { path = \"../lib\" }")),
        ("app/src/lib.rs", "pub mod nested;\npub mod tallied;\n".to_owned()),
        (
            // used through an inline module's `use super::*`
            "app/src/nested.rs",
            "use lib::shapes::*;\n\nmod inner {\n    use super::*;\n\n    pub fn area(n: u32) -> u32 {\n        square(n)\n    }\n}\n\npub fn area(n: u32) -> u32 {\n    inner::area(n)\n}\n"
                .to_owned(),
        ),
        (
            // used only inside a macro call that is not read
            "app/src/tallied.rs",
            "use lib::shapes::*;\n\nmacro_rules! tally {\n    ($f:ident => $n:expr) => {\n        $f($n)\n    };\n}\n\npub fn four() -> u32 {\n    tally!(square => 2)\n}\n"
                .to_owned(),
        ),
        (
            "app/tests/area.rs",
            "#[test]\nfn area() {\n    app::nested::area(2);\n}\n".to_owned(),
        ),
        (
            "app/tests/four.rs",
            "#[test]\nfn four() {\n    app::tallied::four();\n}\n".to_owned(),
        ),
    ]
    .into_iter()
    .map(|(file, text)| (file.to_owned(), text))
    .collect();
    let repo = Repo::new("rust-globs", &files);
    let out = text(&scan(&repo.0), "square");
    assert!(!out.contains("left out of the reach"), "{out}");
    assert_eq!(
        section(&out, "Tests to run again: 2"),
        [
            "  app/tests/area.rs (through app/src/nested.rs)",
            "  app/tests/four.rs (through app/src/tallied.rs)"
        ],
        "{out}"
    );
}

#[test]
fn a_python_package_that_only_relays_a_symbol_leads_on_by_its_name() {
    let files = |init: &str| -> Vec<(String, String)> {
        [
            ("pyproject.toml", "[project]\nname = \"relay\"\n"),
            ("store/__init__.py", init),
            ("store/charge.py", "def pay(x):\n    return x\n"),
            ("store/report.py", "def report():\n    return 1\n"),
            ("app.py", "from store import pay\n\npay(1)\n"),
            (
                "tests/test_report.py",
                "from store.report import report\n\n\ndef test_report():\n    assert report() == 1\n",
            ),
            (
                "tests/test_pay.py",
                "import store\n\n\ndef test_pay():\n    assert store.pay(1) == 1\n",
            ),
        ]
        .into_iter()
        .map(|(file, text)| (file.to_owned(), text.to_owned()))
        .collect()
    };
    // no __all__ and no use of `pay` there: what loads a module below the
    // package runs nothing of `pay`
    let repo = Repo::new("relay-only", &files("from .charge import pay\n"));
    let out = text(&scan(&repo.0), "pay");
    assert_eq!(
        section(&out, "Tests to run again: 1"),
        ["  tests/test_pay.py (takes it, via store/__init__.py:1)"],
        "{out}"
    );
    assert!(out.contains("app.py"), "{out}");
    assert!(out.contains("barrels: 1 file"), "{out}");
    // one whose own code calls it keeps the whole reach
    let repo = Repo::new(
        "relay-used",
        &files("from .charge import pay\n\nTOTAL = pay(0)\n"),
    );
    let out = text(&scan(&repo.0), "pay");
    assert!(out.contains("tests/test_report.py"), "{out}");
}

#[test]
fn a_python_name_a_barrel_renames_reaches_its_star_importers() {
    let files = |barrels: &[(&str, &str)]| -> Vec<(String, String)> {
        let mut files: Vec<(&str, &str)> = vec![
            ("pyproject.toml", "[project]\nname = \"ren\"\n"),
            (
                "star_user/m.py",
                "from pkg import *\n\n\ndef go():\n    return total(1)\n",
            ),
            (
                "tests/test_star.py",
                "from pkg import *\n\n\ndef test_t():\n    assert total(1) == 1\n",
            ),
        ];
        files.extend_from_slice(barrels);
        files
            .into_iter()
            .map(|(file, text)| (file.to_owned(), text.to_owned()))
            .collect()
    };
    // one barrel renames it and lists the new name
    let repo = Repo::new(
        "renamed-star",
        &files(&[
            (
                "pkg/__init__.py",
                "from .impl import amount as total\n\n__all__ = [\"total\"]\n",
            ),
            ("pkg/impl.py", "def amount(x):\n    return x\n"),
        ]),
    );
    let out = text(&scan(&repo.0), "amount");
    assert!(
        out.contains("tests/test_star.py:5 (call) (test) as total"),
        "{out}"
    );
    assert!(out.contains("\nTests to run again: 1\n"), "{out}");
    // an outer barrel passes the new name on
    let repo = Repo::new(
        "renamed-twice",
        &files(&[
            ("pkg/__init__.py", "from .sub import total as total\n"),
            (
                "pkg/sub/__init__.py",
                "from .impl import amount as total\n\n__all__ = [\"total\"]\n",
            ),
            ("pkg/sub/impl.py", "def amount(x):\n    return x\n"),
        ]),
    );
    let out = text(&scan(&repo.0), "amount");
    assert!(out.contains("\nTests to run again: 1\n"), "{out}");
}

#[test]
fn a_mocked_module_still_passes_types_on() {
    // a test that takes a type through a module its mock replaces: a mock
    // replaces no type, for the file and for the symbol alike
    let files: Vec<(String, String)> = [
        ("package.json", r#"{"name": "mockstart", "devDependencies": {"vitest": "1.0.0"}}"#),
        ("src/core.ts", "export class Wallet {}\n"),
        (
            "src/service.ts",
            "import { Wallet } from './core';\n\nexport type Svc = Wallet;\nexport const run = () => 1;\n",
        ),
        (
            // a test that mocks the changed file and takes a type of it
            "tests/core.test.ts",
            "import { vi } from 'vitest';\nimport type { Wallet } from '../src/core';\n\n\
             vi.mock('../src/core', () => ({}));\n\nexport const w: Wallet | null = null;\n",
        ),
        (
            "tests/svc.test.ts",
            "import { vi } from 'vitest';\nimport type { Svc } from '../src/service';\n\n\
             vi.mock('../src/service', () => ({ run: vi.fn() }));\n\nexport const s: Svc | null = null;\n",
        ),
    ]
    .into_iter()
    .map(|(file, text)| (file.to_owned(), text.to_owned()))
    .collect();
    let repo = Repo::new("mock-start", &files);
    let ws = scan(&repo.0);
    for target in ["src/core.ts", "Wallet"] {
        let out = text(&ws, target);
        assert_eq!(
            section(&out, "Tests to run again: 2"),
            [
                "  tests/core.test.ts (takes it, types only)",
                "  tests/svc.test.ts (through src/service.ts, types only)"
            ],
            "{target}: {out}"
        );
    }
}

#[test]
fn small_lists_say_what_they_hold() {
    // a component's bench, example and helper are in the target
    let ws = scan(&fixture("rust-cargo-targets"));
    let out = text(&ws, "kiosk");
    assert!(
        out.contains("kiosk/benches/speed.rs (bench, in the target)"),
        "{out}"
    );
    // verbose names every file of a component that holds the target
    let ws = scan(&fixture("python-bindings"));
    let every = impact(&ws, "store/billing/money.py", DEFAULT_DEPTH, true);
    assert!(
        every.contains("store/refunds.py, store/unused.py)  2 steps"),
        "{every}"
    );
    // a test that defines the symbol changes with it
    let files: Vec<(String, String)> = [
        ("Cargo.toml", "[package]\nname = \"p\"\nversion = \"0.1.0\"\n"),
        ("src/lib.rs", "pub fn work() -> u32 {\n    1\n}\n"),
        (
            "tests/it.rs",
            "pub fn expected() -> u32 {\n    1\n}\n\n#[test]\nfn works() {\n    assert_eq!(p::work(), expected());\n}\n",
        ),
    ]
    .into_iter()
    .map(|(file, text)| (file.to_owned(), text.to_owned()))
    .collect();
    let repo = Repo::new("test-symbol", &files);
    let out = text(&scan(&repo.0), "expected");
    assert_eq!(
        section(&out, "Tests to run again: 1"),
        ["  tests/it.rs (defines it)"],
        "{out}"
    );
}

#[test]
fn a_helper_whose_mock_cuts_its_way_is_no_test_left_out() {
    let files: Vec<(String, String)> = [
        (
            "package.json",
            r#"{"name": "helper-mock", "devDependencies": {"vitest": "1.0.0"}}"#,
        ),
        ("src/wrap.ts", "export const wrap = (): number => 1;\n"),
        (
            "tests/util.ts",
            "import { vi } from 'vitest';\nimport { wrap } from '../src/wrap';\n\n\
             vi.mock('../src/wrap', () => ({ zzz: 1 }));\n\n\
             export const fromUtil = (): number => wrap();\n",
        ),
        (
            "tests/a.test.ts",
            "import { fromUtil } from './util';\n\nfromUtil();\n",
        ),
    ]
    .into_iter()
    .map(|(file, text)| (file.to_owned(), text.to_owned()))
    .collect();
    let repo = Repo::new("helper-mock", &files);
    let out = text(&scan(&repo.0), "src/wrap.ts");
    assert_eq!(
        section(&out, "Tests to run again: 1"),
        ["  tests/a.test.ts (through tests/util.ts)"],
        "{out}"
    );
}

#[test]
fn an_import_name_names_the_barrels_its_reach_stopped_at() {
    let files: Vec<(String, String)> = [
        ("package.json", r#"{"name": "names-barrel"}"#),
        (
            "src/schema.ts",
            "import { z } from 'undeclared-lib';\n\nexport const schema = z;\n",
        ),
        ("src/extra.ts", "export const extra = 5;\n"),
        (
            "src/index.ts",
            "export * from './schema';\nexport * from './extra';\n",
        ),
        (
            "src/other.ts",
            "import { extra } from './index';\n\nexport const other = extra;\n",
        ),
    ]
    .into_iter()
    .map(|(file, text)| (file.to_owned(), text.to_owned()))
    .collect();
    let repo = Repo::new("names-barrel", &files);
    let out = text(&scan(&repo.0), "undeclared-lib");
    assert!(
        out.contains("whatever else loads that file: src/index.ts:1\n"),
        "{out}"
    );
}

#[test]
fn a_changed_barrel_is_no_dependent_of_the_change_nor_a_barrel_it_passes() {
    // the package's barrels change with it: none of its files is a
    // dependent of the package
    let ws = scan(&fixture("ts-reexports"));
    let out = text(&ws, "ts-reexports");
    assert!(out.contains("\nDirect dependents: none"), "{out}");
    // a changed `__init__.py` is the target, not a barrel on the way
    let ws = scan(&fixture("python-bindings"));
    let out = text(&ws, "store/billing/__init__.py");
    assert!(!out.contains("barrels:"), "{out}");
}

#[test]
fn a_rust_file_of_methods_reaches_what_takes_their_type() {
    // ops.rs holds the methods of a type lib.rs defines: what takes the
    // type uses them
    let manifest = |name: &str, dependency: &str| {
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n\n[dependencies]\n{dependency}")
    };
    let files = [
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"p\", \"q\"]\n".to_owned(),
        ),
        ("p/Cargo.toml", manifest("p", "")),
        (
            "p/src/lib.rs",
            "mod fixtures;\nmod ops;\n\npub struct Calc;\n".to_owned(),
        ),
        (
            "p/src/fixtures.rs",
            "#[cfg(test)]\nimpl crate::Calc {\n    pub fn sample() -> crate::Calc {\n        crate::Calc\n    }\n}\n"
                .to_owned(),
        ),
        (
            "p/src/ops.rs",
            "use crate::Calc;\n\nimpl Calc {\n    pub fn add(&self, a: u32, b: u32) -> u32 {\n        a + b\n    }\n}\n"
                .to_owned(),
        ),
        (
            "p/tests/add.rs",
            "use p::Calc;\n\n#[test]\nfn adds() {\n    assert_eq!(Calc.add(1, 2), 3);\n}\n".to_owned(),
        ),
        ("q/Cargo.toml", manifest("q", "p = { path = \"../p\" }")),
        (
            "q/src/main.rs",
            "use p::Calc;\n\nfn main() {\n    println!(\"{}\", Calc.add(1, 2));\n}\n".to_owned(),
        ),
    ];
    let files: Vec<(String, String)> = files
        .into_iter()
        .map(|(file, text)| (file.to_owned(), text))
        .collect();
    let repo = Repo::new("impl-file", &files);
    let ws = scan(&repo.0);
    let out = text(&ws, "p/src/ops.rs");
    assert_eq!(
        section(&out, "Direct dependents: 1"),
        ["  q  1 import"],
        "{out}"
    );
    assert_eq!(
        section(&out, "Imported by: 2"),
        [
            "  q/src/main.rs:1 (takes Calc, whose methods the target holds)  in q",
            "  p/tests/add.rs:1 (test) (takes Calc, whose methods the target holds)  in p",
        ],
        "{out}"
    );
    assert!(!out.contains("no importers"), "{out}");
    assert_eq!(
        section(&out, "Tests to run again: 1"),
        ["  p/tests/add.rs (takes it)"],
        "{out}"
    );
    // query lists them apart from the statements that import the file
    let query = ws
        .query(&QueryRequest {
            target: "p/src/ops.rs",
            depth: DEFAULT_DEPTH,
            format: Format::Text,
            verbose: false,
        })
        .unwrap()
        .output;
    assert!(query.contains("\nImported by: none\n"), "{query}");
    assert_eq!(
        section(&query, "Take the type of its methods: 2"),
        [
            "  q  1 import: q/src/main.rs:1",
            "  p  1 import in tests: p/tests/add.rs:1 (test)"
        ],
        "{query}"
    );
    assert!(!query.contains("no importers"), "{query}");
    // production code cannot call what a `#[cfg(test)]` impl defines
    let out = text(&ws, "p/src/fixtures.rs");
    assert!(out.contains("\nDirect dependents: none"), "{out}");
}

#[test]
fn a_python_star_import_outside_a_package_entry_passes_its_names_on() {
    // api.py offers what its star import binds to whatever star-imports it
    let files: Vec<(String, String)> = [
        ("pyproject.toml", "[project]\nname = \"stars\"\n"),
        ("pkg/__init__.py", "from .api import *\n"),
        ("pkg/api.py", "from ._impl import *\n"),
        ("pkg/_impl.py", "def refund(x):\n    return x\n"),
        (
            "app/star_user.py",
            "from pkg import *\n\n\ndef go():\n    return refund(1)\n",
        ),
        (
            "tests/test_star.py",
            "from pkg import *\n\n\ndef test_refund():\n    assert refund(1) == 1\n",
        ),
    ]
    .into_iter()
    .map(|(file, text)| (file.to_owned(), text.to_owned()))
    .collect();
    let repo = Repo::new("star-chain", &files);
    let ws = scan(&repo.0);
    let out = text(&ws, "refund");
    assert_eq!(
        section(&out, "Transitive dependents: 1"),
        ["  app  3 steps, through pkg/__init__.py"],
        "{out}"
    );
    assert_eq!(
        section(&out, "Tests to run again: 1"),
        ["  tests/test_star.py (through pkg/__init__.py)"],
        "{out}"
    );
    assert!(!out.contains("never named"), "{out}");
}

#[test]
fn an_import_name_reaches_the_tests_that_load_a_helper_importing_it() {
    // a dev-dependency only a test helper imports
    let files: Vec<(String, String)> = [
        (
            "Cargo.toml",
            "[package]\nname = \"p\"\nversion = \"0.1.0\"\n\n[dev-dependencies]\ntempfile = \"3\"\n",
        ),
        ("src/lib.rs", "pub fn answer() -> u32 {\n    42\n}\n"),
        (
            "tests/common/mod.rs",
            "use tempfile::TempDir;\n\npub fn scratch() -> TempDir {\n    TempDir::new().unwrap()\n}\n",
        ),
        (
            "tests/disk.rs",
            "mod common;\n\n#[test]\nfn writes() {\n    let _dir = common::scratch();\n}\n",
        ),
    ]
    .into_iter()
    .map(|(file, text)| (file.to_owned(), text.to_owned()))
    .collect();
    let repo = Repo::new("dev-dependency", &files);
    let out = text(&scan(&repo.0), "tempfile");
    assert_eq!(
        section(&out, "Tests to run again: 1"),
        [
            "  tests/disk.rs (through tests/common/mod.rs)",
            "  not tests: tests/common/mod.rs (helper, for 1 test listed)"
        ],
        "{out}"
    );
}

#[test]
fn a_symbol_reaches_through_a_file_that_uses_it_without_a_statement_of_it() {
    // lib.rs re-exports Engine from its own module and calls it: the
    // re-export is no import, the call a use
    let manifest = |name: &str, dependency: &str| {
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n\n[dependencies]\n{dependency}")
    };
    let files: Vec<(String, String)> = [
        ("Cargo.toml", "[workspace]\nmembers = [\"p\", \"q\"]\n".to_owned()),
        ("p/Cargo.toml", manifest("p", "")),
        (
            "p/src/engine.rs",
            "pub struct Engine;\n\nimpl Engine {\n    pub fn new() -> Self {\n        Engine\n    }\n}\n"
                .to_owned(),
        ),
        (
            "p/src/lib.rs",
            "mod engine;\n\npub use engine::Engine;\n\npub fn start() {\n    Engine::new();\n}\n"
                .to_owned(),
        ),
        (
            "p/tests/start.rs",
            "#[test]\nfn starts() {\n    p::start();\n}\n".to_owned(),
        ),
        ("q/Cargo.toml", manifest("q", "p = { path = \"../p\" }")),
        ("q/src/main.rs", "fn main() {\n    p::start();\n}\n".to_owned()),
    ]
    .into_iter()
    .map(|(file, text)| (file.to_owned(), text))
    .collect();
    let repo = Repo::new("use-only", &files);
    let out = text(&scan(&repo.0), "Engine::new");
    assert_eq!(
        section(&out, "Direct dependents: 1"),
        ["  p (p/src/lib.rs)"],
        "{out}"
    );
    assert_eq!(
        section(&out, "Tests to run again: 1"),
        ["  p/tests/start.rs (through p/src/lib.rs)"],
        "{out}"
    );
}

#[test]
fn no_importers_is_said_only_when_nothing_imports_the_target() {
    // statements of the whole module that never name the symbol import it
    let ws = scan(&fixture("rust-uses"));
    let out = text(&ws, "qualified");
    assert!(out.contains("never named (3 imports"), "{out}");
    assert!(!out.contains("no importers"), "{out}");
    // a package's entry file is imported through the package
    let ws = scan(&fixture("ts-monorepo"));
    let out = text(&ws, "packages/core/src/index.ts");
    assert!(out.contains("\nDirect dependents: 2\n"), "{out}");
    assert!(!out.contains("no importers"), "{out}");
}

#[test]
fn a_component_that_holds_the_target_names_the_files_of_it_reached() {
    let ws = scan(&fixture("simple-ts-project"));
    // the package re-exports the symbol from its own barrel: not the whole
    // package is reached
    let out = text(&ws, "formatPrice");
    assert_eq!(
        section(&out, "Direct dependents: 4"),
        [
            "  app/checkout.ts  2 imports",
            "  ts-shop (src/index.ts)  2 imports",
            "  app/page.tsx  1 import",
            "  scripts/report.cjs  1 import",
        ]
    );
    let json = ws
        .impact(&ImpactRequest {
            target: "formatPrice",
            depth: DEFAULT_DEPTH,
            format: Format::Json,
            verbose: false,
        })
        .unwrap()
        .output;
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(
        value["direct"][1],
        serde_json::json!({
            "id": "ts-shop",
            "distance": 1,
            "files": ["src/index.ts"],
            "imports": {"production": 2, "tests": 0}
        })
    );
    // a package that holds the changed module, reached through its own
    // modules, further away too
    let ws = scan(&fixture("python-bindings"));
    assert_eq!(
        section(
            &text(&ws, "store/billing/money.py"),
            "Transitive dependents: 1"
        ),
        [
            "  store::store (store/aliased.py, store/annotated.py, store/app.py, +5 more)  \
          2 steps, through store/billing/charge.py"
        ]
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
            "through": "mid",
            "declared_in": {"file": "top/Cargo.toml", "line": 6}
        })
    );
}

#[test]
fn each_test_to_run_again_says_how_it_reaches_the_symbol() {
    let files: Vec<(String, String)> = [
        ("package.json", r#"{"name": "shop"}"#),
        (
            "src/money.ts",
            "export interface Price {\n  amount: number;\n}\n\
             export function price(): Price {\n  return { amount: 1 };\n}\n",
        ),
        (
            "tests/named.test.ts",
            "import { price } from '../src/money';\nprice();\n",
        ),
        (
            "tests/whole.test.ts",
            "import * as money from '../src/money';\nmoney.price();\n",
        ),
        (
            "tests/typed.test.ts",
            "import type { Price } from '../src/money';\nexport const p: Price = { amount: 1 };\n",
        ),
    ]
    .into_iter()
    .map(|(path, text)| (path.to_owned(), text.to_owned()))
    .collect();
    let repo = Repo::new("test-ways", &files);
    let ws = scan(&repo.0);
    assert_eq!(
        section(&text(&ws, "price"), "Tests to run again: 2"),
        [
            "  tests/named.test.ts (takes it)",
            "  tests/whole.test.ts (takes its module whole)"
        ]
    );
    // `whole.test.ts` reads only `money.price`: it never names `Price`, so
    // it takes nothing of it
    let typed = text(&ws, "Price");
    assert_eq!(
        section(&typed, "Tests to run again: 1"),
        ["  tests/typed.test.ts (takes it, types only)"]
    );
    assert!(
        typed.contains(
            "  never named (1 import of the whole module, left out of the reach): \
             tests/whole.test.ts:1\n"
        ),
        "{typed}"
    );
}

#[test]
fn a_conftest_stands_for_its_tests_and_a_helper_is_no_test() {
    let files: Vec<(String, String)> = [
        ("pyproject.toml", "[project]\nname = \"shop\"\n"),
        ("shop/__init__.py", ""),
        ("shop/core.py", "def rate():\n    return 1\n"),
        (
            "tests/conftest.py",
            "from shop.core import rate\n\n\ndef pytest_configure():\n    rate()\n",
        ),
        (
            "tests/test_core.py",
            "from shop.core import rate\n\n\ndef test_rate():\n    assert rate() == 1\n",
        ),
        (
            "tests/helpers.py",
            "from shop.core import rate\n\n\ndef make():\n    return rate()\n",
        ),
        (
            "tests/test_make.py",
            "from tests.helpers import make\n\n\ndef test_make():\n    assert make() == 1\n",
        ),
    ]
    .into_iter()
    .map(|(path, text)| (path.to_owned(), text.to_owned()))
    .collect();
    let repo = Repo::new("conftest", &files);
    let ws = scan(&repo.0);
    assert_eq!(
        section(&text(&ws, "shop/core.py"), "Tests to run again: 3"),
        [
            "  tests/ (conftest.py: pytest loads it for every test below)",
            "  tests/test_core.py (takes it)",
            "  tests/test_make.py (through tests/helpers.py)",
            "  not tests: tests/helpers.py (helper, for 1 test listed)",
        ]
    );
}

#[test]
fn rust_examples_benches_and_modules_of_tests_are_no_tests_to_run() {
    let ws = scan(&fixture("rust-cargo-targets"));
    assert_eq!(
        section(&text(&ws, "kiosk/src/till.rs"), "Tests to run again: 2"),
        [
            "  kiosk/tests/stock.rs (takes it)",
            "  kiosk/tests/total.rs (through kiosk/src/lib.rs)",
            "  not tests: kiosk/benches/speed.rs (bench), kiosk/examples/demo.rs (example), \
             kiosk/tests/common/mod.rs (helper, for 2 tests listed)",
        ]
    );
}

#[test]
fn a_barrel_passes_a_change_on_under_the_name_it_exports_it_as() {
    let ws = scan(&fixture("ts-renames"));
    // renamed (`export { price as cost }`), passed on whole and taken by a
    // namespace import, each barrel by the name it gives
    assert_eq!(
        section(
            &text(&ws, "src/money.ts"),
            "Transitive dependents: 6 more (11 in all)"
        ),
        [
            "  chain.ts  2 steps, through src/index.ts",
            "  back.ts  3 steps, through src/chain.ts",
            "  both.ts  3 steps, through src/chain.ts",
            "  namespace.ts  3 steps, through src/chain.ts",
            "  onename.ts  4 steps, through src/both.ts",
            "  twonames.ts  4 steps, through src/both.ts",
        ]
    );
}

#[test]
fn a_line_that_takes_a_module_both_as_a_type_and_by_value_runs_it() {
    let ws = scan(&fixture("ts-uses"));
    // `vi.importActual<typeof import('../src/money')>('../src/money')`
    let out = text(&ws, "formatPrice");
    assert!(
        out.contains("\n  tests/actual.test.ts (takes its module whole)\n"),
        "{out}"
    );
}
