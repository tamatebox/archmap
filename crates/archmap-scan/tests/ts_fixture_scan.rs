//! End-to-end scan of `fixtures/simple-ts-project`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use archmap_core::{
    ArchitectureGraph, ComponentId, ComponentKind, EdgeKind, Evidence, LanguageCoverage, Scope,
    SymbolKind, UnmappedReason,
};
use archmap_scan::{scan, ScanOptions};

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/simple-ts-project")
        .canonicalize()
        .expect("fixture exists")
}

fn scan_fixture() -> ArchitectureGraph {
    let report = scan(&fixture_root(), &ScanOptions::default()).expect("scan succeeds");
    assert!(
        report.warnings.is_empty(),
        "warnings: {:?}",
        report.warnings
    );
    report.graph
}

fn id(s: &str) -> ComponentId {
    ComponentId::new(s)
}

/// Every import edge as (from, to, file:line, target, note).
fn imports(
    graph: &ArchitectureGraph,
) -> BTreeSet<(String, String, String, Option<String>, String)> {
    graph
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Import)
        .flat_map(|e| {
            e.evidence.iter().map(move |ev| {
                (
                    e.from.to_string(),
                    e.to.to_string(),
                    format!("{}:{}", ev.file, ev.line.unwrap_or(0)),
                    ev.target.clone(),
                    ev.note.clone().unwrap_or_default(),
                )
            })
        })
        .collect()
}

/// For the evidence of the statement at `at` (`file:line`) that points at
/// `target`: its note and the names it takes.
fn names_taken(graph: &ArchitectureGraph, at: &str, target: &str) -> BTreeMap<String, Vec<String>> {
    graph
        .edges
        .iter()
        .flat_map(|e| &e.evidence)
        .filter(|e| {
            format!("{}:{}", e.file, e.line.unwrap_or(0)) == at
                && e.target.as_deref() == Some(target)
        })
        .map(|e| {
            (
                e.note.clone().unwrap_or_default(),
                e.names.iter().cloned().collect(),
            )
        })
        .collect()
}

fn noted(rows: &[(&str, &[&str])]) -> BTreeMap<String, Vec<String>> {
    rows.iter()
        .map(|(note, names)| {
            (
                (*note).to_owned(),
                names.iter().map(|n| (*n).to_owned()).collect(),
            )
        })
        .collect()
}

#[test]
fn packages_directories_and_files() {
    let graph = scan_fixture();
    let package = graph.component(&id("ts-shop")).expect("package");
    assert_eq!(package.kind, ComponentKind::Package);
    assert_eq!(package.path.as_deref(), Some("."));
    assert_eq!(package.language.as_deref(), Some("typescript"));

    let modules: BTreeSet<(&str, &str, &str, &str)> = graph
        .components
        .values()
        .filter(|c| c.kind == ComponentKind::Module)
        .map(|c| {
            (
                c.id.as_str(),
                c.name.as_str(),
                c.parent.as_ref().map_or("", |p| p.as_str()),
                c.language.as_deref().unwrap_or(""),
            )
        })
        .collect();
    assert_eq!(
        modules,
        BTreeSet::from([
            ("ts-shop::scripts", "scripts", "ts-shop", "javascript"),
            (
                "ts-shop::scripts/format.cjs",
                "scripts/format.cjs",
                "ts-shop::scripts",
                "javascript"
            ),
            (
                "ts-shop::scripts/report.cjs",
                "scripts/report.cjs",
                "ts-shop::scripts",
                "javascript"
            ),
            (
                "ts-shop::scripts/seed.mjs",
                "scripts/seed.mjs",
                "ts-shop::scripts",
                "javascript"
            ),
            ("ts-shop::src/app", "app", "ts-shop", "typescript"),
            (
                "ts-shop::src/app/checkout.ts",
                "app/checkout.ts",
                "ts-shop::src/app",
                "typescript"
            ),
            (
                "ts-shop::src/app/lazy.tsx",
                "app/lazy.tsx",
                "ts-shop::src/app",
                "typescript"
            ),
            (
                "ts-shop::src/app/page.tsx",
                "app/page.tsx",
                "ts-shop::src/app",
                "typescript"
            ),
            (
                "ts-shop::src/components",
                "components",
                "ts-shop",
                "typescript"
            ),
            (
                "ts-shop::src/components/button.tsx",
                "components/button.tsx",
                "ts-shop::src/components",
                "typescript"
            ),
            ("ts-shop::src/lib", "lib", "ts-shop", "typescript"),
            (
                "ts-shop::src/lib/__mocks__",
                "lib/__mocks__",
                "ts-shop::src/lib",
                "typescript"
            ),
            (
                "ts-shop::src/lib/__mocks__/money.ts",
                "lib/__mocks__/money.ts",
                "ts-shop::src/lib/__mocks__",
                "typescript"
            ),
            (
                "ts-shop::src/lib/limits.ts",
                "lib/limits.ts",
                "ts-shop::src/lib",
                "typescript"
            ),
            (
                "ts-shop::src/lib/money.ts",
                "lib/money.ts",
                "ts-shop::src/lib",
                "typescript"
            ),
            (
                "ts-shop::src/lib/types.ts",
                "lib/types.ts",
                "ts-shop::src/lib",
                "typescript"
            ),
            ("ts-shop::tests", "tests", "ts-shop", "typescript"),
            (
                "ts-shop::tests/helpers.ts",
                "tests/helpers.ts",
                "ts-shop::tests",
                "typescript"
            ),
            (
                "ts-shop::tests/money.test.ts",
                "tests/money.test.ts",
                "ts-shop::tests",
                "typescript"
            ),
        ])
    );
    assert_eq!(graph.meta.analyzers, ["typescript"]);
    assert_eq!(
        graph.meta.coverage.get("typescript"),
        Some(&LanguageCoverage {
            files: 14,
            read: Some(14),
            scripts: 1
        })
    );
    assert_eq!(
        graph.meta.coverage.get("javascript"),
        Some(&LanguageCoverage {
            files: 3,
            read: Some(3),
            scripts: 0
        })
    );
}

#[test]
fn required_declarations_are_dependencies_on_npm_packages() {
    let graph = scan_fixture();
    let dependencies: BTreeSet<(String, String, String)> = graph
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Dependency)
        .map(|e| {
            let ev = &e.evidence[0];
            (
                e.to.to_string(),
                format!("{}:{}", ev.file, ev.line.unwrap_or(0)),
                ev.note.clone().unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(
        dependencies,
        BTreeSet::from([
            (
                "ext:npm:react".to_owned(),
                "package.json:6".to_owned(),
                "dependencies".to_owned()
            ),
            (
                "ext:npm:react-dom".to_owned(),
                "package.json:10".to_owned(),
                "peerDependencies".to_owned()
            ),
            (
                "ext:npm:zod".to_owned(),
                "package.json:7".to_owned(),
                "dependencies".to_owned()
            ),
        ])
    );
    for not_required in [
        "ext:npm:vitest",
        "ext:npm:@types/aws-lambda",
        "ext:npm:fsevents",
    ] {
        assert!(
            graph.component(&id(not_required)).is_none(),
            "{not_required}"
        );
    }
}

#[test]
fn imports_resolve_through_aliases_relative_paths_and_extension_aliases() {
    let edges = imports(&scan_fixture());
    let expected = [
        (
            "ts-shop::src/app/page.tsx",
            "ts-shop::src/lib/money.ts",
            "src/app/page.tsx:1",
            Some("src/lib/money.ts"),
            "import",
        ),
        (
            "ts-shop::src/app/page.tsx",
            "ts-shop::src/lib/types.ts",
            "src/app/page.tsx:2",
            Some("src/lib/types.ts"),
            "import",
        ),
        (
            "ts-shop::src/app/page.tsx",
            "ts-shop::src/lib/limits.ts",
            "src/app/page.tsx:3",
            Some("src/lib/limits.ts"),
            "import",
        ),
        (
            "ts-shop::src/app/page.tsx",
            "ts-shop::src/components",
            "src/app/page.tsx:6",
            Some("src/components/index.ts"),
            "import",
        ),
        (
            "ts-shop::src/app/page.tsx",
            "ext:npm:react-dom",
            "src/app/page.tsx:13",
            None,
            "import react-dom/client",
        ),
        (
            "ts-shop::src/components/button.tsx",
            "ext:npm:react",
            "src/components/button.tsx:1",
            None,
            "import",
        ),
        (
            "ts-shop::src/lib/money.ts",
            "ext:npm:zod",
            "src/lib/money.ts:1",
            None,
            "import",
        ),
        (
            "ts-shop::src/lib/money.ts",
            "ts-shop::src/lib/types.ts",
            "src/lib/money.ts:4",
            Some("src/lib/types.ts"),
            "import",
        ),
        (
            "ts-shop::tests/money.test.ts",
            "ts-shop::src/lib/money.ts",
            "tests/money.test.ts:2",
            Some("src/lib/money.ts"),
            "import",
        ),
        (
            "ts-shop::tests/money.test.ts",
            "ts-shop::tests/helpers.ts",
            "tests/money.test.ts:3",
            Some("tests/helpers.ts"),
            "import",
        ),
        (
            "ts-shop::tests/helpers.ts",
            "ts-shop::src/lib/money.ts",
            "tests/helpers.ts:1",
            Some("src/lib/money.ts"),
            "import",
        ),
        (
            "ts-shop::scripts/seed.mjs",
            "ts-shop::src/lib/limits.ts",
            "scripts/seed.mjs:2",
            Some("src/lib/limits.ts"),
            "import",
        ),
        (
            "ts-shop",
            "ts-shop::src/lib/limits.ts",
            "next.config.ts:1",
            Some("src/lib/limits.ts"),
            "import",
        ),
    ];
    for (from, to, at, target, note) in expected {
        let row = (
            from.to_owned(),
            to.to_owned(),
            at.to_owned(),
            target.map(str::to_owned),
            note.to_owned(),
        );
        assert!(edges.contains(&row), "missing {row:?}");
    }
    // templates, strings and comments are no imports; built-ins are left out
    assert!(edges
        .iter()
        .all(|(_, _, _, _, note)| !note.contains("template")));
    assert!(edges.iter().all(|(_, to, ..)| to != "ext:npm:path"));
}

#[test]
fn every_re_export_is_an_import_edge_noted_export() {
    let edges = imports(&scan_fixture());
    for (from, to, at, target) in [
        (
            "ts-shop",
            "ts-shop::src/lib/money.ts",
            "src/index.ts:1",
            "src/lib/money.ts",
        ),
        (
            "ts-shop",
            "ts-shop::src/components",
            "src/index.ts:2",
            "src/components/index.ts",
        ),
        (
            "ts-shop::src/components",
            "ts-shop::src/components/button.tsx",
            "src/components/index.ts:1",
            "src/components/button.tsx",
        ),
        (
            "ts-shop",
            "ts-shop::src/lib/limits.ts",
            "src/index.ts:3",
            "src/lib/limits.ts",
        ),
        (
            "ts-shop",
            "ts-shop::src/lib/money.ts",
            "src/index.ts:4",
            "src/lib/money.ts",
        ),
    ] {
        let row = (
            from.to_owned(),
            to.to_owned(),
            at.to_owned(),
            Some(target.to_owned()),
            "export".to_owned(),
        );
        assert!(edges.contains(&row), "missing {row:?}");
    }
}

#[test]
fn imports_of_files_that_are_not_code_are_edges_of_the_importer_to_itself() {
    let edges = imports(&scan_fixture());
    for (component, at, target) in [
        (
            "ts-shop::src/components/button.tsx",
            "src/components/button.tsx:2",
            "src/components/button.css",
        ),
        (
            "ts-shop::src/components/button.tsx",
            "src/components/button.tsx:3",
            "src/assets/logo.svg",
        ),
        (
            "ts-shop::src/app/page.tsx",
            "src/app/page.tsx:7",
            "src/app/data.json",
        ),
    ] {
        let row = (
            component.to_owned(),
            component.to_owned(),
            at.to_owned(),
            Some(target.to_owned()),
            "import".to_owned(),
        );
        assert!(edges.contains(&row), "missing {row:?}");
    }
}

#[test]
fn imports_without_an_edge_say_why() {
    let graph = scan_fixture();
    let found: BTreeSet<(String, UnmappedReason, String, String)> = graph
        .unmapped_imports
        .iter()
        .map(|i| {
            (
                i.module.clone(),
                i.reason,
                format!("{}:{}", i.evidence.file, i.evidence.line.unwrap_or(0)),
                i.evidence.note.clone().unwrap_or_default(),
            )
        })
        .collect();
    let row = |module: &str, reason, at: &str, note: &str| {
        (module.to_owned(), reason, at.to_owned(), note.to_owned())
    };
    assert_eq!(
        found,
        BTreeSet::from([
            row("@/lib/missing", UnmappedReason::Unresolved, "src/app/page.tsx:8", "import @/lib/missing: neither a file nor a package name"),
            row("~/thing", UnmappedReason::Unresolved, "src/app/page.tsx:9", "import ~/thing: neither a file nor a package name"),
            row("./gone", UnmappedReason::Unresolved, "src/app/page.tsx:10", "import ./gone: no file matches"),
            row("left-pad", UnmappedReason::Undeclared, "src/app/page.tsx:11", "import"),
            row("aws-lambda", UnmappedReason::DeclaredNotRequired, "src/app/page.tsx:12", "import aws-lambda, declared as @types/aws-lambda in package.json:13 (devDependencies)"),
            row("components/button", UnmappedReason::LocalName, "src/app/page.tsx:14", "import components/button: no file matches, but the package has a file or directory named components"),
            row("vitest", UnmappedReason::DeclaredNotRequired, "tests/money.test.ts:1", "import vitest, declared as vitest in package.json:14 (devDependencies)"),
        ])
    );
}

#[test]
fn exported_declarations_of_files_that_are_not_tests_are_symbols() {
    let graph = scan_fixture();
    // SymbolKind has no order, so the id is the key.
    let symbols: BTreeMap<&str, (SymbolKind, String, &str)> = graph
        .symbols
        .values()
        .map(|s| {
            (
                s.id.as_str(),
                (
                    s.kind,
                    format!("{}:{}", s.evidence[0].file, s.evidence[0].line.unwrap_or(0)),
                    s.signature.as_deref().unwrap_or(""),
                ),
            )
        })
        .collect();
    let row = |id: &'static str, kind, at: &str, signature: &'static str| {
        (id, (kind, at.to_owned(), signature))
    };
    assert_eq!(
        symbols,
        BTreeMap::from([
            row(
                "ts-shop::src/lib/money.ts::CURRENCY",
                SymbolKind::Constant,
                "src/lib/money.ts:6",
                "export const CURRENCY"
            ),
            row(
                "ts-shop::src/lib/money.ts::formatPrice",
                SymbolKind::Function,
                "src/lib/money.ts:8",
                "export function formatPrice(price: Money): string"
            ),
            row(
                "ts-shop::src/lib/money.ts::Wallet",
                SymbolKind::Struct,
                "src/lib/money.ts:12",
                "export class Wallet"
            ),
            row(
                "ts-shop::src/lib/money.ts::Wallet.pay",
                SymbolKind::Function,
                "src/lib/money.ts:13",
                "pay(amount: number): void"
            ),
            row(
                "ts-shop::src/lib/money.ts::Wallet.open",
                SymbolKind::Function,
                "src/lib/money.ts:15",
                "static open(): Wallet"
            ),
            row(
                "ts-shop::src/lib/money.ts::schema",
                SymbolKind::Constant,
                "src/lib/money.ts:20",
                "export const schema"
            ),
            row(
                "ts-shop::src/lib/money.ts::RATES",
                SymbolKind::Constant,
                "src/lib/money.ts:21",
                "const rates"
            ),
            row(
                "ts-shop::src/lib/money.ts::read",
                SymbolKind::Function,
                "src/lib/money.ts:23",
                "export const read = () =>"
            ),
            row(
                "ts-shop::src/lib/types.ts::Money",
                SymbolKind::TypeAlias,
                "src/lib/types.ts:1",
                "export type Money = { amount: number; currency: string }"
            ),
            row(
                "ts-shop::src/lib/types.ts::Priced",
                SymbolKind::Trait,
                "src/lib/types.ts:3",
                "export interface Priced"
            ),
            row(
                "ts-shop::src/lib/types.ts::Unit",
                SymbolKind::Enum,
                "src/lib/types.ts:7",
                "export enum Unit"
            ),
            row(
                "ts-shop::src/lib/limits.ts::MAX_UPLOAD",
                SymbolKind::Constant,
                "src/lib/limits.ts:1",
                "const MAX_UPLOAD"
            ),
            row(
                "ts-shop::src/lib/limits.ts::limitOf",
                SymbolKind::Function,
                "src/lib/limits.ts:3",
                "function limitOf(name: string): number"
            ),
            row(
                "ts-shop::src/components/button.tsx::Button",
                SymbolKind::Function,
                "src/components/button.tsx:5",
                "export function Button({ label }: { label: string })"
            ),
            row(
                "ts-shop::src/components/button.tsx::Fragment",
                SymbolKind::Constant,
                "src/components/button.tsx:14",
                "export const Fragment"
            ),
            // CommonJS exports
            row(
                "ts-shop::scripts/format.cjs::pad",
                SymbolKind::Function,
                "scripts/format.cjs:1",
                "exports.pad = (text) =>"
            ),
            row(
                "ts-shop::scripts/report.cjs::plugin",
                SymbolKind::Function,
                "scripts/report.cjs:3",
                "function plugin(name)"
            ),
            row(
                "ts-shop::scripts/report.cjs::money",
                SymbolKind::Function,
                "scripts/report.cjs:7",
                "async function money()"
            ),
            row(
                "ts-shop::scripts/report.cjs::title",
                SymbolKind::Constant,
                "scripts/report.cjs:11",
                "title"
            ),
            // the globals of a script
            row(
                "ts-shop::src/global.d.ts::VERSION",
                SymbolKind::Constant,
                "src/global.d.ts:1",
                "declare const VERSION: string"
            ),
            row(
                "ts-shop::src/global.d.ts::Window",
                SymbolKind::Trait,
                "src/global.d.ts:3",
                "interface Window"
            ),
            row(
                "ts-shop::src/app/checkout.ts::total",
                SymbolKind::Function,
                "src/app/checkout.ts:5",
                "export function total(n: number): string"
            ),
            row(
                "ts-shop::src/app/lazy.tsx::Chart",
                SymbolKind::Constant,
                "src/app/lazy.tsx:3",
                "export const Chart"
            ),
            row(
                "ts-shop::src/app/lazy.tsx::Limits",
                SymbolKind::TypeAlias,
                "src/app/lazy.tsx:4",
                "export type Limits = typeof import('../lib/limits')"
            ),
            row(
                "ts-shop::src/app/lazy.tsx::Purse",
                SymbolKind::TypeAlias,
                "src/app/lazy.tsx:5",
                "export type Purse = import('../lib/money').Wallet"
            ),
            row(
                "ts-shop::src/app/lazy.tsx::load",
                SymbolKind::Function,
                "src/app/lazy.tsx:6",
                "export function load(name: string)"
            ),
            row(
                "ts-shop::src/app/page.tsx::Page",
                SymbolKind::Function,
                "src/app/page.tsx:20",
                "export default function Page(props: { price: Money })"
            ),
            row(
                "ts-shop::src/app/page.tsx::handler",
                SymbolKind::Function,
                "src/app/page.tsx:24",
                "export const handler: Handler = async () =>"
            ),
            row(
                "ts-shop::tests/helpers.ts::makeWallet",
                SymbolKind::Function,
                "tests/helpers.ts:3",
                "export function makeWallet(): Wallet"
            ),
            row(
                "ts-shop::scripts/seed.mjs::seed",
                SymbolKind::Function,
                "scripts/seed.mjs:4",
                "export async function seed()"
            ),
        ])
    );
}

/// A throwaway repository with `files`, canonicalized.
fn temp_repo(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("archmap-ts-scan-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for (file, text) in files {
        let path = dir.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    dir.canonicalize().unwrap()
}

#[test]
fn a_tsconfig_extends_outside_the_scan_keeps_its_own_paths() {
    let root = temp_repo(
        "extends",
        &[
            ("package.json", "{ \"name\": \"ext-demo\" }"),
            (
                "tsconfig.json",
                "{ \"extends\": \"@tsconfig/node20/tsconfig.json\", \"compilerOptions\": { \
                 \"baseUrl\": \".\", \"paths\": { \"@/*\": [\"./src/*\"] } } }",
            ),
            (
                "src/a.ts",
                "import { b } from '@/b';\nexport const a = b;\n",
            ),
            ("src/b.ts", "export const b = 1;\n"),
        ],
    );
    let report = scan(&root, &ScanOptions::default()).unwrap();
    std::fs::remove_dir_all(&root).unwrap();
    assert_eq!(
        report.warnings,
        [
            "tsconfig.json: extends `@tsconfig/node20/tsconfig.json` is not in the scanned files; \
          its options are not applied"
        ]
    );
    assert!(imports(&report.graph).contains(&(
        "ext-demo::src/a.ts".to_owned(),
        "ext-demo::src/b.ts".to_owned(),
        "src/a.ts:1".to_owned(),
        Some("src/b.ts".to_owned()),
        "import".to_owned(),
    )));
}

#[test]
fn code_without_a_package_json_gets_a_root_component() {
    let root = temp_repo(
        "rootless",
        &[
            ("a.ts", "import { b } from './b';\n"),
            ("b.ts", "export const b = 1;\n"),
        ],
    );
    let graph = scan(&root, &ScanOptions::default()).unwrap().graph;
    let name = root.file_name().unwrap().to_string_lossy().into_owned();
    std::fs::remove_dir_all(&root).unwrap();
    assert_eq!(
        graph.component(&id(&name)).unwrap().kind,
        ComponentKind::Package
    );
    assert!(imports(&graph).contains(&(
        format!("{name}::a.ts"),
        format!("{name}::b.ts"),
        "a.ts:1".to_owned(),
        Some("b.ts".to_owned()),
        "import".to_owned(),
    )));
}

#[test]
fn a_tool_only_package_json_makes_no_package() {
    let root = temp_repo(
        "tools",
        &[
            (
                "package.json",
                "{ \"name\": \"tools\", \"devDependencies\": { \"husky\": \"^9\" } }",
            ),
            ("app.py", "print('hi')\n"),
        ],
    );
    let graph = scan(&root, &ScanOptions::default()).unwrap().graph;
    std::fs::remove_dir_all(&root).unwrap();
    assert!(graph.component(&id("tools")).is_none());
    assert!(graph
        .components
        .values()
        .all(|c| c.language.as_deref() != Some("typescript")));
}

#[test]
fn a_package_json_without_a_name_is_no_package() {
    let root = temp_repo(
        "marker",
        &[
            ("package.json", "{ \"name\": \"app\" }"),
            ("src/esm/package.json", "{ \"type\": \"module\" }"),
            ("src/esm/x.js", "export const x = 1;\n"),
        ],
    );
    let graph = scan(&root, &ScanOptions::default()).unwrap().graph;
    std::fs::remove_dir_all(&root).unwrap();
    let x = graph
        .component(&id("app::src/esm/x.js"))
        .expect("file component");
    assert_eq!(x.parent.as_ref().unwrap().as_str(), "app::src/esm");
}

#[test]
fn a_package_json_without_a_name_still_declares_dependencies() {
    let root = temp_repo(
        "nameless",
        &[
            (
                "package.json",
                "{\n  \"private\": true,\n  \"dependencies\": { \"react\": \"^19\" },\n  \
                 \"devDependencies\": { \"vitest\": \"^4\" }\n}\n",
            ),
            (
                "src/a.ts",
                "import React from 'react';\nimport { it } from 'vitest';\nimport pad from 'left-pad';\n",
            ),
        ],
    );
    let graph = scan(&root, &ScanOptions::default()).unwrap().graph;
    let name = root.file_name().unwrap().to_string_lossy().into_owned();
    std::fs::remove_dir_all(&root).unwrap();
    let dependency = graph
        .edges
        .iter()
        .find(|e| e.kind == EdgeKind::Dependency && e.to == id("ext:npm:react"))
        .expect("a dependency on react");
    assert_eq!(dependency.from, id(&name));
    assert_eq!(
        (
            dependency.evidence[0].file.as_str(),
            dependency.evidence[0].line
        ),
        ("package.json", Some(3))
    );
    assert!(imports(&graph).contains(&(
        format!("{name}::src/a.ts"),
        "ext:npm:react".to_owned(),
        "src/a.ts:1".to_owned(),
        None,
        "import".to_owned(),
    )));
    let unmapped: BTreeSet<(&str, UnmappedReason, &str)> = graph
        .unmapped_imports
        .iter()
        .map(|i| {
            (
                i.module.as_str(),
                i.reason,
                i.evidence.note.as_deref().unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(
        unmapped,
        BTreeSet::from([
            ("left-pad", UnmappedReason::Undeclared, "import"),
            (
                "vitest",
                UnmappedReason::DeclaredNotRequired,
                "import vitest, declared as vitest in package.json:4 (devDependencies)"
            ),
        ])
    );
}

#[test]
fn a_nested_package_sees_the_declarations_above_it() {
    let root = temp_repo(
        "hoisted",
        &[
            (
                "package.json",
                "{\n  \"private\": true,\n  \"workspaces\": [\"packages/*\"],\n  \
                 \"dependencies\": { \"react\": \"^19\" }\n}\n",
            ),
            (
                "packages/ui/package.json",
                "{ \"name\": \"@acme/ui\", \"dependencies\": { \"clsx\": \"^2\" } }",
            ),
            (
                "packages/ui/src/button.ts",
                "import React from 'react';\nimport clsx from 'clsx';\n",
            ),
        ],
    );
    let graph = scan(&root, &ScanOptions::default()).unwrap().graph;
    std::fs::remove_dir_all(&root).unwrap();
    let edges = imports(&graph);
    for (from, to, at, note) in [
        (
            "@acme/ui::src/button.ts",
            "ext:npm:react",
            "packages/ui/src/button.ts:1",
            "import react, declared in package.json:4",
        ),
        (
            "@acme/ui::src/button.ts",
            "ext:npm:clsx",
            "packages/ui/src/button.ts:2",
            "import",
        ),
    ] {
        let row = (
            from.to_owned(),
            to.to_owned(),
            at.to_owned(),
            None,
            note.to_owned(),
        );
        assert!(edges.contains(&row), "missing {row:?} in {edges:?}");
    }
    assert_eq!(
        graph.component(&id("ext:npm:react")).map(|c| c.kind),
        Some(ComponentKind::External)
    );
    assert!(
        graph.unmapped_imports.is_empty(),
        "{:?}",
        graph.unmapped_imports
    );
}

#[test]
fn aliases_and_the_package_s_own_name_are_never_undeclared() {
    let root = temp_repo(
        "aliases",
        &[
            (
                "package.json",
                "{ \"name\": \"my-lib\", \"exports\": \"./dist/index.js\" }",
            ),
            (
                "jsconfig.json",
                "{ \"compilerOptions\": { \"baseUrl\": \".\", \"paths\": { \"@ui/*\": \
                 [\"src/components/ui/*\"] } } }",
            ),
            ("src/index.js", "export const x = 1;\n"),
            ("src/components/button.js", "export const b = 1;\n"),
            ("src/components/ui/card.js", "export const c = 1;\n"),
            (
                "examples/demo.js",
                "import { x } from 'my-lib';\nimport card from '@ui/card';\n\
                 import Button from '@components/button';\nimport pad from 'left-pad';\n",
            ),
        ],
    );
    let graph = scan(&root, &ScanOptions::default()).unwrap().graph;
    std::fs::remove_dir_all(&root).unwrap();
    let unmapped: BTreeSet<(&str, UnmappedReason, &str)> = graph
        .unmapped_imports
        .iter()
        .map(|i| {
            (
                i.module.as_str(),
                i.reason,
                i.evidence.note.as_deref().unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(
        unmapped,
        BTreeSet::from([
            (
                "@ui/card",
                UnmappedReason::Unresolved,
                "import @ui/card: no file matches; jsconfig.json declares the alias `@ui/*`"
            ),
            (
                "@components/button",
                UnmappedReason::LocalName,
                "import @components/button: no file matches, but the package has a file or \
                 directory named components"
            ),
            ("left-pad", UnmappedReason::Undeclared, "import"),
            // an edge to the package would point from inside it at itself
            (
                "my-lib",
                UnmappedReason::LocalName,
                "import my-lib: the package's own name, and its entry is no scanned file"
            ),
        ])
    );
}

#[test]
fn a_file_that_is_no_text_is_a_warning_without_absolute_paths() {
    // MPEG transport streams end in `.ts` too.
    let root = temp_repo("video", &[("a.ts", "export const a = 1;\n")]);
    std::fs::write(root.join("clip.ts"), [0x47u8, 0x40, 0x11, 0xff, 0xfe]).unwrap();
    let report = scan(&root, &ScanOptions::default()).unwrap();
    std::fs::remove_dir_all(&root).unwrap();
    let absolute = root.display().to_string();
    assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
    assert!(
        report.warnings[0].starts_with("clip.ts: "),
        "{:?}",
        report.warnings
    );
    assert!(
        !report.warnings[0].contains(&absolute),
        "{:?}",
        report.warnings
    );
}

/// Each unmapped import of a scan of `files` as (module, reason).
fn unmapped_of(name: &str, files: &[(&str, &str)]) -> BTreeSet<(String, UnmappedReason)> {
    let root = temp_repo(name, files);
    let graph = scan(&root, &ScanOptions::default()).unwrap().graph;
    std::fs::remove_dir_all(&root).unwrap();
    graph
        .unmapped_imports
        .iter()
        .map(|i| (i.module.clone(), i.reason))
        .collect()
}

#[test]
fn a_catch_all_alias_hides_no_undeclared_package() {
    let found = unmapped_of(
        "catchall",
        &[
            ("package.json", "{ \"name\": \"legacy\" }"),
            (
                "tsconfig.json",
                "{ \"compilerOptions\": { \"baseUrl\": \".\", \"paths\": { \"*\": \
                 [\"./src/*\"], \"@ui/*\": [\"./src/ui/*\"] } } }",
            ),
            (
                "src/a.ts",
                "import pad from 'left-pad';\nimport card from '@ui/card';\n",
            ),
        ],
    );
    assert_eq!(
        found,
        BTreeSet::from([
            ("left-pad".to_owned(), UnmappedReason::Undeclared),
            ("@ui/card".to_owned(), UnmappedReason::Unresolved),
        ])
    );
}

#[test]
fn a_scope_is_local_only_when_the_source_root_has_the_directory() {
    // `prisma/` holds the Prisma CLI's schema, not code an alias reaches.
    let found = unmapped_of(
        "scopes",
        &[
            ("package.json", "{ \"name\": \"app\" }"),
            ("prisma/schema.prisma", "// schema\n"),
            ("src/components/button.ts", "export const b = 1;\n"),
            (
                "src/a.ts",
                "import x from '@prisma/missing';\nimport b from '@components/button';\n",
            ),
        ],
    );
    assert_eq!(
        found,
        BTreeSet::from([
            ("@prisma/missing".to_owned(), UnmappedReason::Undeclared),
            ("@components/button".to_owned(), UnmappedReason::LocalName),
        ])
    );
}

#[test]
fn a_broken_package_json_is_a_warning() {
    let root = temp_repo(
        "broken",
        &[
            ("package.json", "{ \"name\": "),
            ("a.ts", "export const a = 1;\n"),
            ("b.ts", "export const = ;\n"),
        ],
    );
    let report = scan(&root, &ScanOptions::default()).unwrap();
    let name = root.file_name().unwrap().to_string_lossy().into_owned();
    std::fs::remove_dir_all(&root).unwrap();
    assert!(report
        .warnings
        .iter()
        .any(|w| w.starts_with("package.json: failed to parse package.json")));
    assert!(report
        .warnings
        .iter()
        .any(|w| w.starts_with("b.ts: parse error")));
    assert!(report
        .graph
        .symbol(&archmap_core::SymbolId::new(format!("{name}::a.ts::a")))
        .is_some());
}

#[test]
fn named_imports_reach_the_files_that_define_the_names() {
    let edges = imports(&scan_fixture());
    let via = |from: &str, to: &str, at: &str, target: &str, through: &str| {
        (
            from.to_owned(),
            to.to_owned(),
            at.to_owned(),
            Some(target.to_owned()),
            format!("import via {through}"),
        )
    };
    for row in [
        via(
            "ts-shop::src/app/page.tsx",
            "ts-shop::src/components/button.tsx",
            "src/app/page.tsx:6",
            "src/components/button.tsx",
            "src/components/index.ts:1",
        ),
        via(
            "ts-shop::src/app/checkout.ts",
            "ts-shop::src/lib/money.ts",
            "src/app/checkout.ts:1",
            "src/lib/money.ts",
            "src/index.ts:1",
        ),
        via(
            "ts-shop::src/app/checkout.ts",
            "ts-shop::src/components/button.tsx",
            "src/app/checkout.ts:1",
            "src/components/button.tsx",
            "src/index.ts:2",
        ),
        via(
            "ts-shop::src/app/checkout.ts",
            "ts-shop::src/lib/limits.ts",
            "src/app/checkout.ts:1",
            "src/lib/limits.ts",
            "src/index.ts:6",
        ),
    ] {
        assert!(edges.contains(&row), "missing {row:?}");
    }
    // walked: page.tsx:6 (one re-export), checkout.ts:1 through index.ts
    // lines 1, 2, 3, 4 and 6, checkout.ts:2 through line 7, checkout.ts:8
    // and :9 through line 8; namespace and side-effect imports, `Missing`
    // and the re-export statements do not
    let walked: Vec<_> = edges
        .iter()
        .filter(|(.., note)| note.contains(" via "))
        .collect();
    assert_eq!(walked.len(), 9, "{walked:#?}");
    // the loaded file keeps its own evidence
    assert!(edges.contains(&(
        "ts-shop::src/app/checkout.ts".to_owned(),
        "ts-shop".to_owned(),
        "src/app/checkout.ts:1".to_owned(),
        Some("src/index.ts".to_owned()),
        "import".to_owned(),
    )));
}

#[test]
fn imports_record_the_names_they_take() {
    let graph = scan_fixture();
    // named imports as the loaded file exports them, unknown ones included
    assert_eq!(
        names_taken(&graph, "src/app/checkout.ts:1", "src/index.ts"),
        noted(&[(
            "import",
            &[
                "Button",
                "LIMIT",
                "Missing",
                "formatPrice",
                "limitOf",
                "money"
            ]
        )])
    );
    // a default that the loaded file re-exports, beside the whole module:
    // the loaded file declares no name for it
    assert_eq!(
        names_taken(&graph, "src/app/checkout.ts:2", "src/index.ts"),
        noted(&[("import", &["*", "default"])])
    );
    // a side-effect import takes nothing
    assert_eq!(
        names_taken(&graph, "src/app/checkout.ts:3", "src/index.ts"),
        noted(&[("import", &[])])
    );
    // a file that is no code keeps the name as written
    assert_eq!(
        names_taken(&graph, "src/app/page.tsx:7", "src/app/data.json"),
        noted(&[("import", &["default"])])
    );
}

#[test]
fn via_evidence_names_what_the_defining_file_declares() {
    let graph = scan_fixture();
    // one evidence per re-export on the way
    assert_eq!(
        names_taken(&graph, "src/app/checkout.ts:1", "src/lib/money.ts"),
        noted(&[
            ("import via src/index.ts:1", &["formatPrice"]),
            ("import via src/index.ts:4", &["*"]),
        ])
    );
    assert_eq!(
        names_taken(&graph, "src/app/checkout.ts:1", "src/lib/limits.ts"),
        noted(&[
            ("import via src/index.ts:3", &["limitOf"]),
            ("import via src/index.ts:6", &["MAX_UPLOAD"]),
        ])
    );
    assert_eq!(
        names_taken(&graph, "src/app/checkout.ts:2", "src/lib/limits.ts"),
        noted(&[("import via src/index.ts:7", &["limitOf"])])
    );
    // two names through one re-export: one evidence
    assert_eq!(
        names_taken(&graph, "src/app/page.tsx:6", "src/components/button.tsx"),
        noted(&[(
            "import via src/components/index.ts:1",
            &["Button", "Fragment"]
        )])
    );
}

#[test]
fn re_export_statements_record_names_and_are_not_walked() {
    let graph = scan_fixture();
    for (at, target, names) in [
        ("src/index.ts:1", "src/lib/money.ts", &["formatPrice"][..]),
        ("src/index.ts:2", "src/components/index.ts", &["*"]),
        ("src/index.ts:3", "src/lib/limits.ts", &["limitOf"]),
        ("src/index.ts:4", "src/lib/money.ts", &["*"]),
        ("src/index.ts:7", "src/lib/limits.ts", &["limitOf"]),
        (
            "src/components/index.ts:1",
            "src/components/button.tsx",
            &["*"],
        ),
    ] {
        assert_eq!(
            names_taken(&graph, at, target),
            noted(&[("export", names)]),
            "{at}"
        );
    }
    let walked: Vec<&Evidence> = graph
        .edges
        .iter()
        .flat_map(|e| &e.evidence)
        .filter(|e| {
            e.note
                .as_deref()
                .is_some_and(|n| n.starts_with("export via"))
        })
        .collect();
    assert!(walked.is_empty(), "{walked:#?}");
}

#[test]
fn no_edge_holds_evidence_that_differs_only_in_names() {
    let graph = scan_fixture();
    for edge in &graph.edges {
        let mut seen = BTreeSet::new();
        for e in &edge.evidence {
            // values and types of one statement are two pieces of evidence
            let key = (&e.file, e.line, &e.note, &e.target, e.scope, e.type_only);
            assert!(
                seen.insert(key),
                "{} -> {}: {key:?} twice",
                edge.from,
                edge.to
            );
        }
    }
}

#[test]
fn imports_in_test_code_are_marked() {
    let graph = scan_fixture();
    let marks = |file: &str| -> BTreeSet<bool> {
        graph
            .edges
            .iter()
            .flat_map(|e| &e.evidence)
            .chain(graph.unmapped_imports.iter().map(|u| &u.evidence))
            .filter(|e| e.file == file)
            .map(|e| e.test)
            .collect()
    };
    assert_eq!(marks("tests/money.test.ts"), BTreeSet::from([true]));
    // a helper in a test directory keeps its symbols but is test code
    assert_eq!(marks("tests/helpers.ts"), BTreeSet::from([true]));
    assert_eq!(marks("src/app/page.tsx"), BTreeSet::from([false]));
    assert_eq!(marks("src/app/checkout.ts"), BTreeSet::from([false]));
}

#[test]
fn imports_of_types_only_are_marked() {
    let graph = scan_fixture();
    let kinds = |at: &str, target: &str| -> BTreeSet<(Vec<String>, bool)> {
        graph
            .edges
            .iter()
            .flat_map(|e| &e.evidence)
            .filter(|e| {
                format!("{}:{}", e.file, e.line.unwrap_or(0)) == at
                    && e.target.as_deref() == Some(target)
            })
            .map(|e| (e.names.iter().cloned().collect(), e.type_only))
            .collect()
    };
    let rows = |rows: &[(&[&str], bool)]| -> BTreeSet<(Vec<String>, bool)> {
        rows.iter()
            .map(|(names, types)| (names.iter().map(|n| (*n).to_owned()).collect(), *types))
            .collect()
    };
    // a value and a type in one statement: two pieces of evidence
    assert_eq!(
        kinds("src/app/page.tsx:1", "src/lib/money.ts"),
        rows(&[(&["formatPrice"], false), (&["Wallet"], true)])
    );
    assert_eq!(
        kinds("src/app/page.tsx:2", "src/lib/types.ts"),
        rows(&[(&["Money"], true)])
    );
    // `export type ... from`, and an `import type` through it
    assert_eq!(
        kinds("src/index.ts:8", "src/lib/types.ts"),
        rows(&[(&["Money"], true)])
    );
    assert_eq!(
        kinds("src/app/checkout.ts:8", "src/index.ts"),
        rows(&[(&["Money"], true)])
    );
    assert_eq!(
        kinds("src/app/checkout.ts:8", "src/lib/types.ts"),
        rows(&[(&["Money"], true)])
    );
    // imported without `type` from a file that re-exports it as a type: the
    // loaded file's evidence runs, the defining file's does not
    assert_eq!(
        kinds("src/app/checkout.ts:9", "src/index.ts"),
        rows(&[(&["Money"], false)])
    );
    assert_eq!(
        kinds("src/app/checkout.ts:9", "src/lib/types.ts"),
        rows(&[(&["Money"], true)])
    );
    // money.ts and types.ts import each other for types only: no cycle
    assert!(
        !graph
            .cycles()
            .iter()
            .any(|group| group.iter().any(|id| id.as_str().ends_with("types.ts"))),
        "{:?}",
        graph.cycles()
    );
}

#[test]
fn calls_that_load_modules_are_imports() {
    let graph = scan_fixture();
    type Row = (
        String,
        String,
        String,
        Option<Scope>,
        Vec<String>,
        bool,
        bool,
    );
    let rows: BTreeSet<Row> = graph
        .edges
        .iter()
        .flat_map(|e| &e.evidence)
        .filter_map(|e| {
            Some((
                format!("{}:{}", e.file, e.line?),
                e.target.clone()?,
                e.note.clone()?,
                e.scope,
                e.names.iter().cloned().collect(),
                e.type_only,
                e.test,
            ))
        })
        .collect();
    let row = |at: &str,
               target: &str,
               note: &str,
               scope: Scope,
               names: &[&str],
               types: bool,
               test: bool|
     -> Row {
        (
            at.to_owned(),
            target.to_owned(),
            note.to_owned(),
            Some(scope),
            names.iter().map(|n| (*n).to_owned()).collect(),
            types,
            test,
        )
    };
    for expected in [
        row(
            "scripts/report.cjs:1",
            "scripts/format.cjs",
            "require",
            Scope::Module,
            &["*"],
            false,
            false,
        ),
        // `.js` written for `.ts`, inside a function
        row(
            "scripts/report.cjs:8",
            "src/lib/money.ts",
            "import()",
            Scope::Local,
            &["*"],
            false,
            false,
        ),
        row(
            "src/app/lazy.tsx:3",
            "src/components/button.tsx",
            "import()",
            Scope::Local,
            &["*"],
            false,
            false,
        ),
        // `import()` types never run
        row(
            "src/app/lazy.tsx:4",
            "src/lib/limits.ts",
            "import",
            Scope::Module,
            &["*"],
            true,
            false,
        ),
        row(
            "src/app/lazy.tsx:5",
            "src/lib/money.ts",
            "import",
            Scope::Module,
            &["Wallet"],
            true,
            false,
        ),
        row(
            "tests/money.test.ts:11",
            "src/lib/limits.ts",
            "vi.importActual",
            Scope::Local,
            &["*"],
            false,
            true,
        ),
        row(
            "tests/money.test.ts:15",
            "src/lib/types.ts",
            "vi.mock",
            Scope::Module,
            &["*"],
            false,
            true,
        ),
    ] {
        assert!(rows.contains(&expected), "missing {expected:?}");
    }
    // a computed specifier loads what only the running program knows
    let dynamic: BTreeSet<(String, String, Option<Scope>)> = graph
        .dynamic_imports
        .iter()
        .map(|d| {
            (
                format!("{}:{}", d.evidence.file, d.evidence.line.unwrap_or(0)),
                d.call.clone(),
                d.evidence.scope,
            )
        })
        .collect();
    assert_eq!(
        dynamic,
        BTreeSet::from([
            (
                "scripts/report.cjs:4".to_owned(),
                "require".to_owned(),
                Some(Scope::Local)
            ),
            (
                "src/app/lazy.tsx:7".to_owned(),
                "import()".to_owned(),
                Some(Scope::Local)
            ),
        ])
    );
}

#[test]
fn a_file_without_imports_or_exports_is_a_script_of_globals() {
    let graph = scan_fixture();
    let script = graph
        .component(&ComponentId::new("ts-shop::src/global.d.ts"))
        .expect("global.d.ts is a component");
    assert_eq!(script.kind, ComponentKind::Script);
    let symbols: BTreeSet<(&str, String, String)> = graph
        .symbols_of(&script.id)
        .map(|s| {
            (
                s.name.as_str(),
                format!("{:?}", s.kind),
                s.signature.clone().unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(
        symbols,
        BTreeSet::from([
            (
                "VERSION",
                format!("{:?}", SymbolKind::Constant),
                "declare const VERSION: string".to_owned()
            ),
            (
                "Window",
                format!("{:?}", SymbolKind::Trait),
                "interface Window".to_owned()
            ),
        ])
    );
    assert_eq!(graph.meta.coverage["typescript"].scripts, 1);
    // `.cjs` is a module whatever it holds
    assert_eq!(graph.meta.coverage["javascript"].scripts, 0);
}

#[test]
fn commonjs_exports_are_symbols() {
    let graph = scan_fixture();
    let of = |file: &str| -> BTreeSet<(String, String, u32)> {
        graph
            .symbols
            .values()
            .filter(|s| s.evidence.first().is_some_and(|e| e.file == file))
            .map(|s| {
                (
                    s.name.clone(),
                    format!("{:?}", s.kind),
                    s.evidence[0].line.unwrap_or(0),
                )
            })
            .collect()
    };
    let rows = |rows: &[(&str, SymbolKind, u32)]| -> BTreeSet<(String, String, u32)> {
        rows.iter()
            .map(|(name, kind, line)| ((*name).to_owned(), format!("{kind:?}"), *line))
            .collect()
    };
    assert_eq!(
        of("scripts/format.cjs"),
        rows(&[("pad", SymbolKind::Function, 1)])
    );
    // shorthand properties point at the declarations
    assert_eq!(
        of("scripts/report.cjs"),
        rows(&[
            ("plugin", SymbolKind::Function, 3),
            ("money", SymbolKind::Function, 7),
            ("title", SymbolKind::Constant, 11),
        ])
    );
}
