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

fn scan(root: &Path) -> Workspace {
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
           ts-shop\n  \
           app/checkout.ts\n  \
           app/page.tsx\n  \
           lib/money.ts\n\
         \n\
         Imported by: 6, showing 5 (1 re-export)\n  \
           src/app/checkout.ts:8 (via src/index.ts:8) (type)\n  \
           src/app/checkout.ts:9 (via src/index.ts:8) (type)\n  \
           src/app/page.tsx:2 (type)\n  \
           src/index.ts:8 (export) (type)  in ts-shop\n  \
           src/lib/money.ts:4 (type)\n\
         \n\
         Transitive dependents: 2 more (6 in all)\n  \
           scripts/report.cjs\n  \
           app/lazy.tsx\n\
         \n\
         Tests to run again: 2\n  \
           tests/helpers.ts\n  \
           tests/money.test.ts\n\
         \n\
         Not traced:\n  \
           dynamic: 2 calls load modules by computed names, which may be this: \
           scripts/report.cjs:4, src/app/lazy.tsx:7\n\
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
    assert_eq!(section(&out, "Imported by: 60, showing 5").len(), 5);
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
