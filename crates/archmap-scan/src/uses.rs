//! Where a symbol is used, read on demand for one symbol: the file that
//! defines it and the files whose statements import it, by name or whole,
//! as [`ArchitectureGraph::symbol_importers`] lists them. Nothing here runs
//! during a scan, and nothing it finds enters the graph.

use std::collections::BTreeSet;
use std::path::Path;

use archmap_core::{ArchitectureGraph, Evidence, Symbol, SymbolUses, Unread, UnreadReason};

use crate::typescript::uses as ts;

/// Where `symbol` is used, read from the files under `root` that the graph
/// says define and import it. The files of a language that no pass reads
/// yet are listed as unread.
pub fn symbol_uses(root: &Path, graph: &ArchitectureGraph, symbol: &Symbol) -> SymbolUses {
    let mut found = SymbolUses::default();
    let Some(location) = symbol.location() else {
        return found;
    };
    let statements: Vec<&Evidence> = graph
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
    let language = graph
        .component(&symbol.component)
        .and_then(|c| c.language.as_deref());
    match language {
        Some("typescript" | "javascript") => {
            let request = ts::Request {
                root,
                graph,
                defining: &location.file,
                name: &symbol.name,
                test: location.test,
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
