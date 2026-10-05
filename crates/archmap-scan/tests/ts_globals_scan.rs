//! End-to-end scan of `fixtures/ts-globals`: the declarations inside
//! `declare global { .. }` of a module.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use archmap_core::{ArchitectureGraph, SymbolKind};
use archmap_scan::{scan, symbol_uses, ScanOptions, ScanReport};

fn scan_fixture() -> ArchitectureGraph {
    report().graph
}

fn report() -> ScanReport {
    let root: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/ts-globals")
        .canonicalize()
        .expect("fixture exists");
    let report = scan(&root, &ScanOptions::default()).expect("scan succeeds");
    assert!(
        report.warnings.is_empty(),
        "warnings: {:?}",
        report.warnings
    );
    report
}

/// Every symbol as (id, kind, signature, file:line, declared globally).
fn symbols(graph: &ArchitectureGraph) -> BTreeSet<(&str, String, &str, String, bool)> {
    graph
        .symbols
        .values()
        .map(|s| {
            let at = s.location().expect("a symbol has a location");
            (
                s.id.as_str(),
                format!("{:?}", s.kind),
                s.signature.as_deref().unwrap_or_default(),
                format!("{}:{}", at.file, at.line.unwrap_or_default()),
                at.declares_global(),
            )
        })
        .collect()
}

#[test]
fn declarations_in_declare_global_are_global_symbols_of_their_file() {
    let graph = scan_fixture();
    let kind = |k: SymbolKind| format!("{k:?}");
    let symbol =
        |id, k, signature, at: &str, global| (id, kind(k), signature, at.to_owned(), global);
    assert_eq!(
        symbols(&graph),
        BTreeSet::from([
            // a name the file exports keeps the export
            symbol(
                "ts-globals::src/env.ts::buildMode",
                SymbolKind::Constant,
                "const buildMode: string",
                "src/env.ts:5",
                true,
            ),
            symbol(
                "ts-globals::src/env.ts::describeBuild",
                SymbolKind::Function,
                "export function describeBuild(): string",
                "src/env.ts:8",
                false,
            ),
            symbol(
                "ts-globals::src/env.ts::mode",
                SymbolKind::Constant,
                "export const mode: string",
                "src/env.ts:1",
                false,
            ),
            // augmentations of what TypeScript's libraries declare
            symbol(
                "ts-globals::src/global.d.ts::Flags",
                SymbolKind::Trait,
                "interface Flags",
                "src/global.d.ts:9",
                true,
            ),
            symbol(
                "ts-globals::src/global.d.ts::NodeJS",
                SymbolKind::Module,
                "namespace NodeJS",
                "src/global.d.ts:12",
                true,
            ),
            symbol(
                "ts-globals::src/global.d.ts::Window",
                SymbolKind::Trait,
                "interface Window",
                "src/global.d.ts:4",
                true,
            ),
            symbol(
                "ts-globals::src/global.d.ts::__BUILD__",
                SymbolKind::Other,
                "var __BUILD__: string",
                "src/global.d.ts:8",
                true,
            ),
            // a script's own globals; its `declare global` is an error
            symbol(
                "ts-globals::src/legacy.d.ts::LEGACY",
                SymbolKind::Constant,
                "declare const LEGACY: string",
                "src/legacy.d.ts:1",
                false,
            ),
            symbol(
                "ts-globals::src/main.ts::start",
                SymbolKind::Function,
                "export function start(): string",
                "src/main.ts:5",
                false,
            ),
            // a script that a module imports for what it runs
            symbol(
                "ts-globals::src/polyfill.js::installPolyfill",
                SymbolKind::Function,
                "function installPolyfill()",
                "src/polyfill.js:1",
                false,
            ),
            symbol(
                "ts-globals::src/setup.ts::registry",
                SymbolKind::Other,
                "var registry: Map<string, number>",
                "src/setup.ts:2",
                true,
            ),
        ])
    );
}

#[test]
fn a_global_is_used_in_its_own_file_by_its_name_and_through_global_this() {
    let report = report();
    let uses = |name: &str| -> Vec<String> {
        let symbol = report.graph.symbols_named(name).next().expect("a symbol");
        symbol_uses(&report, symbol)
            .uses
            .iter()
            .map(|u| {
                let at = format!("{}:{}", u.evidence.file, u.evidence.line.unwrap_or(0));
                match &u.binding {
                    Some(binding) => format!("{at} {} as {binding}", u.role.as_str()),
                    None => format!("{at} {}", u.role.as_str()),
                }
            })
            .collect()
    };
    // unresolved to the analysis, which keeps `declare global` apart; a
    // parameter of the same name is no use of the global
    assert_eq!(uses("buildMode"), ["src/env.ts:9 read"]);
    assert_eq!(
        uses("registry"),
        ["src/setup.ts:5 read as globalThis.registry"]
    );
    // the uses inside the block resolve to it
    assert_eq!(uses("Flags"), ["src/global.d.ts:6 type"]);
}
