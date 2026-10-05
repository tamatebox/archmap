//! `query`: one target in detail, as a component, a file, symbols or an
//! import name without a component.

use std::borrow::Cow;
use std::collections::BTreeSet;

use anyhow::{Context, Result};
use archmap_core::{
    ArchitectureGraph, ComponentId, ComponentKind, Edge, EdgeKind, Evidence, Symbol, SymbolUses,
    UnreadReason,
};

use crate::not_traced::{holds_global, not_traced, with_uses, Own, Place, Subject};
use crate::resolve::{imports_subpath, resolve, Resolved};
use crate::target::{component_file, fold, namesakes, reject_outside, unquote, AtDepth};
use crate::views::{ComponentView, FileView, Importer, QueryResult, SymbolView, UnmappedView};
use archmap_scan::ScanReport;

use crate::{Answer, Format, Found, QueryRequest, Workspace};

/// Group a file's import evidence by the component at `depth` on the other side.
fn edges_at_depth(
    full: &ArchitectureGraph,
    depth: usize,
    pairs: &[(&Edge, &Evidence)],
    other_side: impl Fn(&Edge) -> &ComponentId,
    edge: impl Fn(ComponentId, Vec<Evidence>) -> Edge,
) -> Vec<Edge> {
    let mut grouped: std::collections::BTreeMap<ComponentId, Vec<Evidence>> = Default::default();
    for (e, evidence) in pairs {
        grouped
            .entry(full.ancestor_at(other_side(e), depth))
            .or_default()
            .push((*evidence).clone());
    }
    grouped
        .into_iter()
        .map(|(id, evidence)| edge(id, evidence))
        .collect()
}

fn file_view<'a>(
    full: &'a ArchitectureGraph,
    depth: usize,
    requested: &'a str,
    file: &str,
) -> FileView<'a> {
    let facts = full.file_facts(file);
    let owner = facts.component.map(|c| full.ancestor_at(c, depth));
    let here = owner.clone().unwrap_or_else(|| ComponentId::new(file));
    let imports = edges_at_depth(
        full,
        depth,
        &facts.imports,
        |e| &e.to,
        |to, evidence| Edge {
            from: here.clone(),
            to,
            kind: EdgeKind::Import,
            evidence,
        },
    );
    let importers = facts.importers_recorded.then(|| {
        edges_at_depth(
            full,
            depth,
            &facts.importers,
            |e| &e.from,
            |from, evidence| Edge {
                from,
                to: here.clone(),
                kind: EdgeKind::Import,
                evidence,
            },
        )
    });
    // a Rust file of methods is reached through their type
    let takers: Vec<(&Edge, &Evidence)> = full
        .method_takers()
        .remove(facts.file.as_str())
        .unwrap_or_default()
        .into_iter()
        .map(|(edge, e, _)| (edge, e))
        .collect();
    let method_takers = edges_at_depth(
        full,
        depth,
        &takers,
        |e| &e.from,
        |from, evidence| Edge {
            from,
            to: here.clone(),
            kind: EdgeKind::Import,
            evidence,
        },
    );
    let below = full.imports_below(&facts.file);
    let imports_below = edges_at_depth(
        full,
        depth,
        &below,
        |e| &e.from,
        |from, evidence| Edge {
            from,
            to: here.clone(),
            kind: EdgeKind::Import,
            evidence,
        },
    );
    // the component the file is, or that holds it, before roll-up
    let own = facts.component.and_then(|c| full.component(c));
    let (also_named, also_at_path) = own.map(|c| namesakes(full, c)).unwrap_or_default();
    let script = own.is_some_and(|c| c.kind == ComponentKind::Script);
    let directive = own.and_then(|c| {
        c.evidence
            .iter()
            .filter(|e| e.file == facts.file)
            .find_map(Evidence::directive)
    });
    let not_traced = not_traced(
        full,
        &Subject {
            language: own.and_then(|c| c.language.as_deref()),
            place: Some(Place::File(&facts.file)),
            own: Own::File(&facts.file),
            script,
            global: facts
                .symbols
                .iter()
                .any(|s| s.location().is_some_and(Evidence::declares_global)),
            // a package's entry file runs before the modules below it
            unreached: importers.as_ref().is_some_and(Vec::is_empty)
                && below.is_empty()
                && takers.is_empty(),
        },
        usize::MAX,
    );
    FileView {
        requested,
        depth,
        file: facts.file,
        component: owner,
        also_named,
        also_at_path,
        symbols: facts.symbols,
        imports,
        importers,
        method_takers,
        imports_below,
        not_mapped: facts.unmapped_imports,
        dynamic_imports: facts.dynamic_imports,
        script,
        directive,
        not_traced,
    }
}

impl Workspace {
    /// `query` as text or JSON, or the candidates when the target names
    /// several things.
    pub fn query(&self, request: &QueryRequest) -> Result<Answer> {
        query(self, request)
    }
}

fn query(ws: &Workspace, request: &QueryRequest) -> Result<Answer> {
    let QueryRequest {
        target,
        depth,
        format,
        verbose,
    } = *request;
    let target = unquote(target);
    let root = ws.root();
    // an issue or a pull request of the work snapshot
    if let Some(item) = crate::work::target(root, target) {
        return Ok(Answer {
            output: crate::work::answer(ws, item, target, format, verbose)?,
            found: Found::One,
        });
    }
    reject_outside(root, target)?;
    let full = ws.graph();
    let rolled = full.rollup(depth);

    let result = match resolve(full, &rolled, root, target)? {
        Resolved::Candidates(candidates) => {
            return Ok(Answer {
                output: candidates.render(full, target, format)?,
                found: Found::Candidates,
            })
        }
        Resolved::Component(component) => match component_file(full, root, component) {
            Some(file) => QueryResult::File(file_view(full, depth, target, &file)),
            None => {
                let at = fold(full, depth, &component.id);
                component_view(full, &rolled, depth, target, at, None)?
            }
        },
        Resolved::Package { component, subpath } => {
            let at = fold(full, depth, &component.id);
            component_view(full, &rolled, depth, target, at, Some(subpath))?
        }
        Resolved::File(file) => QueryResult::File(file_view(full, depth, target, &file)),
        Resolved::Symbol(symbol) => {
            // as the rolled-up graph holds it, in its folded component
            let symbol = rolled.symbol(&symbol.id).unwrap_or(symbol);
            QueryResult::Symbols(vec![symbol_view(full, &ws.report, symbol)])
        }
        // an import name that no component carries, such as an extra
        Resolved::ImportName(_) => QueryResult::NotMapped(UnmappedView {
            requested: target,
            module: target,
            depth,
            not_mapped: full.unmapped_imports_of(target).collect(),
        }),
    };

    let output = match format {
        Format::Json => crate::json(&result)?,
        Format::Text => crate::query_text::render(&result, target, full, &rolled, verbose),
    };
    Ok(Answer {
        output,
        found: Found::One,
    })
}

/// `edge` with the evidence of the statements that import `spec`, a
/// package subpath; a manifest's declaration of the package whole.
fn importing_subpath<'a>(edge: &'a Edge, spec: &str) -> Option<Cow<'a, Edge>> {
    if edge.kind != EdgeKind::Import {
        return Some(Cow::Borrowed(edge));
    }
    let evidence: Vec<Evidence> = edge
        .evidence
        .iter()
        .filter(|e| imports_subpath(e, spec))
        .cloned()
        .collect();
    (!evidence.is_empty()).then(|| {
        Cow::Owned(Edge {
            evidence,
            ..edge.clone()
        })
    })
}

/// A symbol with the statements that import it, read in the full graph,
/// production code first, as `impact` lists them, and where it is used.
fn symbol_view<'a>(
    full: &'a ArchitectureGraph,
    report: &ScanReport,
    symbol: &'a Symbol,
) -> SymbolView<'a> {
    let importers = full.symbol_importers(symbol).filter(|i| i.recorded);
    let list =
        |pairs: Vec<(&'a Edge, &'a Evidence)>,
         through: &std::collections::BTreeMap<(&'a str, Option<u32>), &'a str>| {
            let mut list: Vec<Importer> = pairs
                .into_iter()
                .map(|(edge, evidence)| Importer {
                    from: &edge.from,
                    evidence,
                    through: through
                        .get(&(evidence.file.as_str(), evidence.line))
                        .copied(),
                })
                .collect();
            list.sort_by_key(|i| (i.evidence.test, &i.evidence.file, i.evidence.line));
            list
        };
    let (imported_by, may_use) = match importers {
        Some(found) => (
            Some(list(found.by_name, &found.through)),
            Some(list(found.may_use, &found.through)),
        ),
        None => (None, None),
    };
    // the component that declares it, before roll-up
    let declared = full
        .symbol(&symbol.id)
        .and_then(|s| full.component(&s.component));
    let location = symbol.location().map(|e| e.file.as_str()).unwrap_or("");
    let language = declared.and_then(|c| c.language.as_deref());
    let mut not_traced = not_traced(
        full,
        &Subject {
            language,
            // an import that may be of its file, unresolved, may take it
            place: symbol.location().map(|e| Place::File(&e.file)),
            own: Own::File(location),
            script: declared.is_some_and(|c| c.kind == ComponentKind::Script),
            global: symbol.location().is_some_and(Evidence::declares_global),
            unreached: imported_by.as_ref().is_some_and(Vec::is_empty)
                && may_use.as_ref().is_some_and(Vec::is_empty),
        },
        usize::MAX,
    );
    let used_at = uses_of(full, report, symbol);
    let instance_method = used_at.is_some() && instance_method(symbol);
    if let Some(found) = &used_at {
        not_traced = with_uses(not_traced, found, instance_method);
    }
    SymbolView {
        symbol,
        imported_by,
        may_use,
        used_at,
        instance_method,
        not_traced,
    }
}

/// Where a symbol is used, or `None` for a language that no uses pass
/// reads yet.
pub(crate) fn uses_of(
    full: &ArchitectureGraph,
    report: &ScanReport,
    symbol: &Symbol,
) -> Option<SymbolUses> {
    // the symbol as scan recorded it, in the component that declares it
    let symbol = full.symbol(&symbol.id).unwrap_or(symbol);
    let mut found = archmap_scan::symbol_uses(report, symbol);
    // a file that uses the symbol through none of its statements of the
    // symbol (through a Rust inline module's `use super::*`) or holds macro
    // calls the scan does not read may take it through any of them: none of
    // them is unused
    let defining = symbol.location().map(|e| e.file.as_str());
    let listed: BTreeSet<(String, u32)> = full
        .symbol_importers(symbol)
        .into_iter()
        .flat_map(|found| found.by_name.into_iter().chain(found.may_use))
        .filter_map(|(_, e)| Some((e.file.clone(), e.line?)))
        .collect();
    let open: BTreeSet<String> = found
        .uses
        .iter()
        .filter(|u| Some(u.evidence.file.as_str()) != defining)
        .filter(|u| {
            u.statement
                .as_ref()
                .is_none_or(|at| !listed.contains(&(at.file.clone(), at.line)))
        })
        .map(|u| u.evidence.file.clone())
        .chain(full.unread_macros.iter().map(|m| m.evidence.file.clone()))
        .collect();
    found.unused.retain(|e| !open.contains(&e.file));
    let unread_language = found
        .unread
        .iter()
        .any(|u| u.reason == UnreadReason::LanguageNotRead);
    (!unread_language).then_some(found)
}

/// A method called through values of its type: a TS/JS class member that
/// is not static (`Wallet.pay`, not `static open()`), or a Rust method that
/// takes `self` (`Edge::weight`, not `Edge::new`).
pub(crate) fn instance_method(symbol: &Symbol) -> bool {
    let signature = symbol.signature.as_deref().unwrap_or("");
    match (symbol.name.contains('.'), symbol.name.contains("::")) {
        // TS/JS `Class.method`, not `static`
        (true, _) => !signature.starts_with("static "),
        // Rust `Type::method` whose first parameter is `self`
        (_, true) => archmap_scan::takes_self(signature),
        _ => false,
    }
}

/// The component `at` points to, as `query` shows it.
fn component_view<'a>(
    full: &'a ArchitectureGraph,
    rolled: &'a ArchitectureGraph,
    depth: usize,
    requested: &'a str,
    at: AtDepth,
    subpath: Option<String>,
) -> Result<QueryResult<'a>> {
    let component = rolled
        .component(&at.id)
        .with_context(|| format!("`{}` is missing after roll-up", at.id))?;
    let (also_named, also_at_path) = namesakes(full, component);
    let incoming = rolled
        .incoming(&component.id)
        .filter_map(|edge| match &subpath {
            // `requested` is the package name and the subpath
            Some(_) => importing_subpath(edge, requested),
            None => Some(Cow::Borrowed(edge)),
        })
        .collect();
    let not_traced = not_traced(
        full,
        &Subject {
            language: component.language.as_deref(),
            place: component.path.as_deref().map(Place::Directory),
            own: Own::Component(&component.id, depth),
            script: component.kind == ComponentKind::Script,
            global: holds_global(full, component),
            unreached: false,
        },
        usize::MAX,
    );
    Ok(QueryResult::Component(ComponentView {
        requested,
        depth,
        folded_from: at.folded_from,
        subpath,
        component,
        also_named,
        also_at_path,
        children: full
            .components
            .values()
            .filter(|c| c.parent.as_ref() == Some(&component.id))
            .map(|c| &c.id)
            .collect(),
        symbols: rolled.symbols_of(&component.id).collect(),
        outgoing: rolled.outgoing(&component.id).collect(),
        incoming,
        not_mapped: rolled
            .unmapped_imports
            .iter()
            .filter(|i| i.from == component.id)
            .collect(),
        dynamic_imports: rolled
            .dynamic_imports
            .iter()
            .filter(|i| i.from == component.id)
            .collect(),
        not_traced,
    }))
}
