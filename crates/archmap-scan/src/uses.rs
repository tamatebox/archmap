//! Where a symbol is used, read on demand for one symbol: the file that
//! defines it and the files whose statements import it, by name or whole,
//! as [`ArchitectureGraph::symbol_importers`] lists them. Nothing here runs
//! during a scan, and nothing it finds enters the graph.

use std::collections::{BTreeMap, BTreeSet};

use archmap_core::{ArchitectureGraph, Evidence, Symbol, SymbolUses, Unread, UnreadReason};

use crate::typescript::uses as ts;
use crate::ScanReport;

/// The statements `listed` names, and the others on their lines that load
/// the same file: the lists keep one statement per line, while a line can
/// hold an import of types and a call that loads the module.
fn with_siblings<'g>(graph: &'g ArchitectureGraph, listed: Vec<&'g Evidence>) -> Vec<&'g Evidence> {
    let places: BTreeSet<(&str, Option<u32>, Option<&str>)> = listed
        .iter()
        .map(|e| (e.file.as_str(), e.line, e.target.as_deref()))
        .collect();
    let mut statements = listed;
    for edge in &graph.edges {
        for evidence in &edge.evidence {
            let place = (
                evidence.file.as_str(),
                evidence.line,
                evidence.target.as_deref(),
            );
            if places.contains(&place) && !statements.contains(&evidence) {
                statements.push(evidence);
            }
        }
    }
    statements
}

/// The names under which each file offers a symbol `name` that `file`
/// defines: its own there, then for each barrel that passes it on (a
/// statement noted `export`), the names it passes it on under.
fn offered_as(
    graph: &ArchitectureGraph,
    file: &str,
    name: &str,
) -> BTreeMap<String, BTreeSet<String>> {
    let mut offered: BTreeMap<String, BTreeSet<String>> =
        BTreeMap::from([(file.to_owned(), BTreeSet::from([name.to_owned()]))]);
    let passing: Vec<&Evidence> = graph
        .edges
        .iter()
        .flat_map(|edge| &edge.evidence)
        .filter(|e| e.passes_on() && e.target.is_some())
        .collect();
    // at most one round per file, as a walk through barrels
    for _ in 0..=passing.len().min(32) {
        let mut grew = false;
        for e in &passing {
            let Some(names) = e.target.as_deref().and_then(|t| offered.get(t)) else {
                continue;
            };
            let passed: BTreeSet<String> = names
                .iter()
                .filter(|n| e.names.contains(*n))
                .flat_map(|n| e.exported_names(n))
                .map(str::to_owned)
                .collect();
            let known = offered.entry(e.file.clone()).or_default();
            for n in passed {
                grew |= known.insert(n);
            }
        }
        if !grew {
            break;
        }
    }
    offered
}

/// Where `symbol` is used, read from the files of the scanned root that the
/// graph says define and import it. The files of a language that no pass
/// reads yet are listed as unread.
pub fn symbol_uses(report: &ScanReport, symbol: &Symbol) -> SymbolUses {
    let (root, graph) = (report.root.as_path(), &report.graph);
    let mut found = SymbolUses::default();
    let Some(location) = symbol.location() else {
        return found;
    };
    let listed: Vec<&Evidence> = graph
        .symbol_importers(symbol)
        .map(|importers| {
            importers
                .by_name
                .iter()
                .chain(&importers.may_use)
                .map(|(_, evidence)| *evidence)
                .collect()
        })
        .unwrap_or_default();
    let statements = with_siblings(graph, listed);
    let language = graph
        .component(&symbol.component)
        .and_then(|c| c.language.as_deref());
    match (language, &report.rust) {
        // a module is a component of its own, which `query` points at
        (Some("rust"), Some(index)) if symbol.kind != archmap_core::SymbolKind::Module => {
            crate::rust::uses::read(index, root, symbol, &statements, &mut found);
        }
        (Some("python"), _) => {
            // a star import passes its names on to what imports its file
            let imported = graph
                .edges
                .iter()
                .flat_map(|edge| &edge.evidence)
                .filter_map(|e| e.target.as_deref().filter(|t| *t != e.file))
                .collect();
            let request = crate::python::uses::Request {
                root,
                symbol,
                statements,
                imported,
                // a method goes by its class
                offered: offered_as(
                    graph,
                    &location.file,
                    symbol.name.split('.').next().unwrap_or(&symbol.name),
                ),
            };
            crate::python::uses::read(&request, &mut found);
        }
        (Some("typescript" | "javascript"), _) => {
            let request = ts::Request {
                root,
                graph,
                defining: &location.file,
                name: &symbol.name,
                test: location.test,
                global: location.declares_global(),
                script: graph
                    .component(&symbol.component)
                    .is_some_and(|c| c.kind == archmap_core::ComponentKind::Script),
                statements,
            };
            ts::read(&request, &mut found);
        }
        _ => {
            let files: BTreeSet<&str> = std::iter::once(location.file.as_str())
                .chain(statements.iter().map(|e| e.file.as_str()))
                .collect();
            found.unread = files
                .into_iter()
                .map(|file| Unread {
                    file: file.to_owned(),
                    line: None,
                    reason: UnreadReason::LanguageNotRead,
                })
                .collect();
        }
    }
    found.normalize();
    found
}
