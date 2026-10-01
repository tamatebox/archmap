//! `query`: one target in detail, as a component, a file, symbols or an
//! import name without a component.

use anyhow::{bail, Context, Result};
use archmap_core::{
    ArchitectureGraph, ComponentId, ComponentKind, Edge, EdgeKind, Evidence, Symbol, SymbolId,
    UnmappedImport,
};

use crate::target::{
    component_file, directory_target, file_target, find_component, fold, namesakes,
    reject_ambiguous, reject_outside, AtDepth,
};
use crate::views::{ComponentView, FileView, Importer, QueryResult, SymbolView, UnmappedView};
use crate::{Format, QueryRequest, Workspace};

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
    // the component the file is, or that holds it, before roll-up
    let own = facts.component.and_then(|c| full.component(c));
    let (also_named, also_at_path) = own.map(|c| namesakes(full, c)).unwrap_or_default();
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
        not_mapped: facts.unmapped_imports,
        dynamic_imports: facts.dynamic_imports,
        script: own.is_some_and(|c| c.kind == ComponentKind::Script),
    }
}

impl Workspace {
    /// `query` as text or JSON.
    pub fn query(&self, request: &QueryRequest) -> Result<String> {
        query(self, request)
    }
}

fn query(ws: &Workspace, request: &QueryRequest) -> Result<String> {
    let QueryRequest {
        target,
        depth,
        format,
        verbose,
    } = *request;
    let root = ws.root();
    reject_outside(root, target)?;
    let full = ws.graph();
    let rolled = full.rollup(depth);
    reject_ambiguous(full, target)?;

    let named =
        find_component(&rolled, full, target).or_else(|| find_component(full, full, target));
    let result = if let Some(component) = named {
        match component_file(full, root, component) {
            Some(file) => QueryResult::File(file_view(full, depth, target, &file)),
            None => {
                let at = fold(full, depth, &component.id);
                component_view(full, &rolled, depth, target, at)?
            }
        }
    } else if let Some(file) = file_target(root, target) {
        QueryResult::File(file_view(full, depth, target, &file))
    } else {
        let symbols: Vec<&Symbol> = rolled
            .symbol(&SymbolId::new(target))
            .into_iter()
            .chain(rolled.symbols_named(target))
            .collect();
        if !symbols.is_empty() {
            QueryResult::Symbols(
                symbols
                    .into_iter()
                    .map(|symbol| symbol_view(full, symbol))
                    .collect(),
            )
        } else if let Some(file) = full.file_for_dotted_name(target) {
            QueryResult::File(file_view(full, depth, target, file))
        } else if let Some(owner) = directory_target(full, root, target).transpose()? {
            // Late: every directory has an owner, the root at worst, so a
            // bare word naming one must not shadow a symbol.
            let at = fold(full, depth, &owner.id);
            component_view(full, &rolled, depth, target, at)?
        } else {
            // An import name that no component carries, such as an extra.
            let not_mapped: Vec<&UnmappedImport> = full.unmapped_imports_of(target).collect();
            if not_mapped.is_empty() {
                bail!("no component, file, symbol or import named `{target}`");
            }
            QueryResult::NotMapped(UnmappedView {
                requested: target,
                module: target,
                depth,
                not_mapped,
            })
        }
    };

    Ok(match format {
        Format::Json => crate::json(&result)?,
        Format::Text => crate::query_text::render(&result, target, full, &rolled, verbose),
    })
}

/// A symbol with the statements that import it, read in the full graph,
/// production code first, as `impact` lists them.
fn symbol_view<'a>(full: &'a ArchitectureGraph, symbol: &'a Symbol) -> SymbolView<'a> {
    let importers = full.symbol_importers(symbol).filter(|i| i.recorded);
    let list = |pairs: Vec<(&'a Edge, &'a Evidence)>| {
        let mut list: Vec<Importer> = pairs
            .into_iter()
            .map(|(edge, evidence)| Importer {
                from: &edge.from,
                evidence,
            })
            .collect();
        list.sort_by_key(|i| (i.evidence.test, &i.evidence.file, i.evidence.line));
        list
    };
    match importers {
        Some(found) => SymbolView {
            symbol,
            imported_by: Some(list(found.by_name)),
            may_use: Some(list(found.may_use)),
        },
        None => SymbolView {
            symbol,
            imported_by: None,
            may_use: None,
        },
    }
}

/// The component `at` points to, as `query` shows it.
fn component_view<'a>(
    full: &'a ArchitectureGraph,
    rolled: &'a ArchitectureGraph,
    depth: usize,
    requested: &'a str,
    at: AtDepth,
) -> Result<QueryResult<'a>> {
    let component = rolled
        .component(&at.id)
        .with_context(|| format!("`{}` is missing after roll-up", at.id))?;
    let (also_named, also_at_path) = namesakes(full, component);
    Ok(QueryResult::Component(ComponentView {
        requested,
        depth,
        folded_from: at.folded_from,
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
        incoming: rolled.incoming(&component.id).collect(),
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
    }))
}
