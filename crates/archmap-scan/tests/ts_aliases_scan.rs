//! End-to-end scan of `fixtures/ts-aliases`: imports through the aliases
//! of a jsconfig.json, a bundler's configuration and a Deno import map.

use std::collections::BTreeSet;
use std::path::Path;

use archmap_core::{ArchitectureGraph, UnmappedReason};
use archmap_scan::{scan, ScanOptions};

fn scan_fixture() -> ArchitectureGraph {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/ts-aliases")
        .canonicalize()
        .expect("fixture exists");
    let report = scan(&root, &ScanOptions::default()).expect("scan succeeds");
    assert!(
        report.warnings.is_empty(),
        "warnings: {:?}",
        report.warnings
    );
    report.graph
}

/// The files the statements of `file` load, as `line -> target`.
fn loads(graph: &ArchitectureGraph, file: &str) -> BTreeSet<String> {
    graph
        .edges
        .iter()
        .flat_map(|e| &e.evidence)
        .filter(|e| e.file == file && e.via().is_none())
        .filter_map(|e| Some(format!("{} -> {}", e.line?, e.target.as_deref()?)))
        .collect()
}

/// The imports of `file` that map to no component, as `line name reason`.
fn unmapped(graph: &ArchitectureGraph, file: &str) -> BTreeSet<String> {
    graph
        .unmapped_imports
        .iter()
        .filter(|u| u.evidence.file == file)
        .map(|u| {
            let reason = match u.reason {
                UnmappedReason::Unresolved => "unresolved",
                UnmappedReason::Undeclared => "undeclared",
                UnmappedReason::LocalName => "local name",
                _ => "other",
            };
            format!("{} {} {reason}", u.evidence.line.unwrap_or(0), u.module)
        })
        .collect()
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

#[test]
fn a_jsconfig_resolves_the_files_below_it_where_it_is_the_nearest_config() {
    let graph = scan_fixture();
    assert_eq!(
        loads(&graph, "js-app/src/pages/home.js"),
        set(&[
            "1 -> js-app/src/lib/format.js",
            "2 -> js-app/legacy/index.js",
        ])
    );
    // its pattern, leading to no file, is an alias all the same
    assert_eq!(
        unmapped(&graph, "js-app/src/pages/home.js"),
        set(&["3 @/lib/gone unresolved"])
    );
    // the tsconfig above it still answers for its own files
    assert_eq!(
        loads(&graph, "shared/use.ts"),
        set(&["1 -> shared/util.ts"])
    );
    // a jsconfig's `baseUrl` alone
    assert_eq!(
        loads(&graph, "js-base/src/app.js"),
        set(&["1 -> js-base/src/components/Button.js"])
    );
    // in one directory, the tsconfig wins
    assert_eq!(loads(&graph, "both/main.ts"), set(&["1 -> both/ts/x.ts"]));
}

#[test]
fn a_vite_config_rewrites_its_package_s_imports_before_the_tsconfig() {
    let graph = scan_fixture();
    // `@` goes to src/, not to the tsconfig's legacy/
    assert_eq!(
        loads(&graph, "vite-app/src/main.ts"),
        set(&[
            "1 -> vite-app/src/a.ts",
            "2 -> vite-app/src/icons/star.ts",
            "3 -> vite-app/src/shared/index.ts",
            "4 -> vite-app/src/app/entry.ts",
        ])
    );
    // a relative replacement is left as Vite leaves it, and an alias that
    // leads to no file is one all the same
    assert_eq!(
        unmapped(&graph, "vite-app/src/main.ts"),
        set(&["5 rel/x undeclared", "6 @/gone unresolved",])
    );
    // a package below keeps its own config
    assert_eq!(
        loads(&graph, "vite-app/packages/inner/main.ts"),
        set(&["1 -> vite-app/packages/inner/src/b.ts"])
    );
}

#[test]
fn a_vite_config_without_aliases_changes_nothing() {
    let graph = scan_fixture();
    assert_eq!(
        loads(&graph, "paths-app/src/main.ts"),
        set(&["1 -> paths-app/src/x.ts"])
    );
}

#[test]
fn a_webpack_alias_matches_a_prefix_or_with_dollar_the_name_alone() {
    let graph = scan_fixture();
    assert_eq!(
        loads(&graph, "webpack-app/src/index.js"),
        set(&[
            "1 -> webpack-app/src/utilities/file.js",
            "2 -> webpack-app/src/templates/main.js",
        ])
    );
    assert_eq!(
        unmapped(&graph, "webpack-app/src/index.js"),
        set(&["3 Templates/other undeclared"])
    );
}

#[test]
fn babel_s_module_resolver_rewrites_its_package_s_imports() {
    let graph = scan_fixture();
    // a root holds bare names, an alias a prefix; a regex key and a
    // package replacement are left out
    assert_eq!(
        loads(&graph, "rn-app/src/App.tsx"),
        set(&[
            "1 -> rn-app/src/components/Card.tsx",
            "2 -> rn-app/assets/logo.ts",
        ])
    );
    assert_eq!(
        unmapped(&graph, "rn-app/src/App.tsx"),
        set(&["3 @features/home undeclared", "4 lodash undeclared"])
    );
    // a `.babelrc` with comments and trailing commas, and the `babel` key
    // of a package.json
    assert_eq!(
        loads(&graph, "legacy-app/main.js"),
        set(&["1 -> legacy-app/lib/x.js"])
    );
    assert_eq!(
        loads(&graph, "pkgbabel-app/index.js"),
        set(&["1 -> pkgbabel-app/app/helpers/util.js"])
    );
}

#[test]
fn an_alias_that_leads_to_no_scanned_file_is_unresolved_and_names_its_config() {
    let graph = scan_fixture();
    // above the root
    assert_eq!(
        unmapped(&graph, "vite-app/src/far.ts"),
        set(&["1 outside/x unresolved"])
    );
    let note = graph
        .unmapped_imports
        .iter()
        .find(|u| u.evidence.file == "vite-app/src/far.ts")
        .and_then(|u| u.evidence.note.clone());
    assert_eq!(
        note.as_deref(),
        Some(
            "import outside/x: no file matches; vite-app/vite.config.ts declares the alias \
             `outside`, to a path outside the scan"
        )
    );
    // no file below the alias: no package of the name stands in for it
    assert_eq!(
        loads(&graph, "shim-app/src/main.ts"),
        set(&["1 -> shim-app/src/shims/lodash/index.ts"])
    );
    assert_eq!(
        unmapped(&graph, "shim-app/src/main.ts"),
        set(&["2 lodash/fp unresolved"])
    );
}

#[test]
fn a_package_json_without_a_name_is_no_package_for_a_bundler_s_aliases() {
    let graph = scan_fixture();
    assert_eq!(
        loads(&graph, "vite-app/src/workers/w.ts"),
        set(&["1 -> vite-app/src/a.ts"])
    );
}

#[test]
fn babel_s_rewrites_come_before_a_bundler_s_and_any_babelrc_is_read() {
    let graph = scan_fixture();
    // rn-app's webpack alias `components` loses to Babel's root
    assert!(loads(&graph, "rn-app/src/App.tsx").contains("1 -> rn-app/src/components/Card.tsx"));
    // a `.babelrc.js`
    assert_eq!(
        loads(&graph, "rcjs-app/main.js"),
        set(&["1 -> rcjs-app/lib/y.js"])
    );
}
