//! `impact`: what may be affected when a target changes.

use anyhow::{bail, Context, Result};
use archmap_core::{ArchitectureGraph, ChangeSeed, ComponentId, Edge, Evidence, SymbolId};
use serde::Serialize;

use crate::target::{
    ambiguous_symbols, component_file, directory_target, file_target, find_component, fold,
    namesakes, reject_ambiguous, reject_outside, symbols_for,
};
use crate::{ImpactRequest, Workspace};

#[derive(Debug, Serialize)]
pub struct ImpactResult<'a> {
    /// The target as given on the command line.
    pub requested: &'a str,
    pub depth: usize,
    pub target: ComponentId,
    /// The component that owns the request, when it is folded into `target`
    /// at this depth.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folded_from: Option<ComponentId>,
    /// For a symbol: its id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<SymbolId>,
    /// Other components with the target's name, and at its path.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub also_named: Vec<ComponentId>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub also_at_path: Vec<ComponentId>,
    /// Components that directly depend on the target.
    pub direct: Vec<ComponentId>,
    /// Every component that transitively depends on the target.
    pub transitive: Vec<ComponentId>,
    /// Files that reach the target only through test code, and a changed
    /// component's own test files: the tests to run again.
    pub tests: TestFiles,
    /// For a file, or a component that is one file: the statements that
    /// import the file directly. For a symbol: those that take its name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub importers: Option<ImportSites>,
    /// For a symbol: the statements that take its file whole.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub may_use: Option<ImportSites>,
}

/// How many import sites `impact` shows for a file; the rest is counted.
const MAX_IMPORT_SITES: usize = 5;

/// How many test files `impact` shows; the rest is counted.
const MAX_TEST_FILES: usize = 20;

#[derive(Debug, Serialize)]
pub struct TestFiles {
    pub total: usize,
    /// The first ones by path.
    pub shown: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ImportSites {
    /// False when no evidence names imported files for the file's language:
    /// the importers are unknown, not absent.
    pub recorded: bool,
    pub total: usize,
    pub shown: Vec<ImportSite>,
}

#[derive(Debug, Serialize)]
pub struct ImportSite {
    pub file: String,
    pub line: Option<u32>,
    /// The importing component, at the roll-up depth.
    pub component: ComponentId,
    /// The statement is test code.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub test: bool,
}

fn import_sites(full: &ArchitectureGraph, depth: usize, file: &str, cap: usize) -> ImportSites {
    let facts = full.file_facts(file);
    sites_of(full, depth, &facts.importers, facts.importers_recorded, cap)
}

/// One site per statement, sorted by place; the first few shown.
fn sites_of(
    full: &ArchitectureGraph,
    depth: usize,
    statements: &[(&Edge, &Evidence)],
    recorded: bool,
    cap: usize,
) -> ImportSites {
    let mut sites: Vec<ImportSite> = Vec::new();
    for (edge, e) in statements {
        if !sites.iter().any(|s| s.file == e.file && s.line == e.line) {
            sites.push(ImportSite {
                file: e.file.clone(),
                line: e.line,
                component: full.ancestor_at(&edge.from, depth),
                test: e.test,
            });
        }
    }
    // production code first
    sites.sort_by(|a, b| (a.test, &a.file, a.line).cmp(&(b.test, &b.file, b.line)));
    let total = sites.len();
    sites.truncate(cap);
    ImportSites {
        recorded,
        total,
        shown: sites,
    }
}

impl Workspace {
    /// `impact` as JSON.
    pub fn impact(&self, request: &ImpactRequest) -> Result<String> {
        impact(self, request)
    }
}

fn impact(ws: &Workspace, request: &ImpactRequest) -> Result<String> {
    let ImpactRequest {
        target,
        depth,
        verbose,
    } = *request;
    let root = ws.root();
    let (sites_cap, tests_cap) = match verbose {
        true => (usize::MAX, usize::MAX),
        false => (MAX_IMPORT_SITES, MAX_TEST_FILES),
    };
    reject_outside(root, target)?;
    let full = ws.graph();
    let rolled = full.rollup(depth);
    reject_ambiguous(full, target)?;

    let component =
        find_component(&rolled, full, target).or_else(|| find_component(full, full, target));
    let symbols = symbols_for(full, target);
    let mut importers = None;
    let (mut symbol_id, mut may_use) = (None, None);
    let (at, reach) = if let Some(component) = component {
        let reach = full.change_impact(ChangeSeed::Component(&component.id), depth);
        importers = component_file(full, root, component)
            .map(|file| import_sites(full, depth, &file, sites_cap));
        (fold(full, depth, &component.id), reach)
    } else if let Some(file) = file_target(root, target) {
        let owner = full
            .component_for_path(&file)
            .with_context(|| format!("no component contains `{target}`"))?;
        let reach = full.change_impact(ChangeSeed::File(&file), depth);
        importers = Some(import_sites(full, depth, &file, sites_cap));
        (fold(full, depth, &owner.id), reach)
    } else if let [symbol] = symbols.as_slice() {
        match full.component(&ComponentId::new(symbol.id.as_str())) {
            // a Rust module's symbol stands for its component
            Some(module) => {
                let reach = full.change_impact(ChangeSeed::Component(&module.id), depth);
                importers = component_file(full, root, module)
                    .map(|file| import_sites(full, depth, &file, sites_cap));
                (fold(full, depth, &module.id), reach)
            }
            None => {
                let reach = full.change_impact(ChangeSeed::Symbol(symbol), depth);
                if let Some(found) = full.symbol_importers(symbol) {
                    importers = Some(sites_of(
                        full,
                        depth,
                        &found.by_name,
                        found.recorded,
                        sites_cap,
                    ));
                    may_use = Some(sites_of(
                        full,
                        depth,
                        &found.may_use,
                        found.recorded,
                        sites_cap,
                    ));
                }
                symbol_id = Some(symbol.id.clone());
                (fold(full, depth, &symbol.component), reach)
            }
        }
    } else if let Some(file) = full.file_for_dotted_name(target) {
        // a file by its component's name and its stem, as query takes it
        let owner = full
            .component_for_path(file)
            .with_context(|| format!("no component contains `{target}`"))?;
        let reach = full.change_impact(ChangeSeed::File(file), depth);
        importers = Some(import_sites(full, depth, file, sites_cap));
        (fold(full, depth, &owner.id), reach)
    } else if let Some(owner) = directory_target(full, root, target).transpose()? {
        // a directory answers before names that several symbols share fail
        let reach = full.change_impact(ChangeSeed::Component(&owner.id), depth);
        (fold(full, depth, &owner.id), reach)
    } else if symbols.len() > 1 {
        bail!(ambiguous_symbols(target, &symbols));
    } else {
        bail!("no component, file or symbol `{target}` in graph");
    };

    let (also_named, also_at_path) = full
        .component(&at.id)
        .map(|c| namesakes(full, c))
        .unwrap_or_default();
    let owned = |ids: Vec<&ComponentId>| ids.into_iter().cloned().collect();
    let result = ImpactResult {
        also_named: owned(also_named),
        also_at_path: owned(also_at_path),
        requested: target,
        depth,
        direct: reach.direct.into_iter().collect(),
        transitive: reach.transitive.into_iter().collect(),
        tests: TestFiles {
            total: reach.tests.len(),
            shown: reach.tests.into_iter().take(tests_cap).collect(),
        },
        target: at.id,
        folded_from: at.folded_from,
        symbol: symbol_id,
        importers,
        may_use,
    };
    crate::json(&result)
}
