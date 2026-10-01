//! `impact`: what may be affected when a target changes.

use std::collections::BTreeSet;

use anyhow::{Context, Result};
use archmap_core::{
    ArchitectureGraph, ChangeSeed, ComponentId, Edge, Evidence, SymbolId, UnmappedImport,
};
use serde::Serialize;

use crate::resolve::{resolve, unquote, Resolved};
use crate::target::{component_file, fold, namesakes, reject_outside};
use crate::{Answer, Format, Found, ImpactRequest, Workspace};

#[derive(Debug, Serialize)]
pub struct ImpactResult<'a> {
    /// The target as given on the command line.
    pub requested: &'a str,
    pub depth: usize,
    /// The component that changes; `null` for an import name that no
    /// component carries, given in `module`.
    pub target: Option<ComponentId>,
    /// For an import name that no component carries: that name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    /// The component that owns the request, when it is folded into `target`
    /// at this depth.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folded_from: Option<ComponentId>,
    /// For a symbol: its id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<SymbolId>,
    /// For a package subpath (`react-dom/client`): the part after the
    /// package name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subpath: Option<String>,
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
    /// `impact` as JSON, or the candidates when the target names several
    /// things.
    pub fn impact(&self, request: &ImpactRequest) -> Result<Answer> {
        impact(self, request)
    }
}

fn impact(ws: &Workspace, request: &ImpactRequest) -> Result<Answer> {
    let ImpactRequest {
        target,
        depth,
        verbose,
    } = *request;
    let target = unquote(target);
    let root = ws.root();
    let caps = match verbose {
        true => Caps {
            sites: usize::MAX,
            tests: usize::MAX,
        },
        false => Caps {
            sites: MAX_IMPORT_SITES,
            tests: MAX_TEST_FILES,
        },
    };
    reject_outside(root, target)?;
    let full = ws.graph();
    let rolled = full.rollup(depth);

    let mut importers = None;
    let (mut symbol_id, mut may_use, mut subpath) = (None, None, None);
    let (at, reach) = match resolve(full, &rolled, root, target)? {
        Resolved::Candidates(candidates) => {
            return Ok(Answer {
                output: candidates.render(full, target, Format::Json)?,
                found: Found::Candidates,
            })
        }
        Resolved::ImportName(module) => {
            return Ok(Answer {
                output: import_name_impact(full, depth, target, module, caps)?,
                found: Found::One,
            })
        }
        Resolved::Component(component) => {
            let reach = full.change_impact(ChangeSeed::Component(&component.id), depth);
            importers = component_file(full, root, component)
                .map(|file| import_sites(full, depth, &file, caps.sites));
            (fold(full, depth, &component.id), reach)
        }
        Resolved::Package {
            component,
            subpath: after,
        } => {
            subpath = Some(after);
            let reach = full.change_impact(ChangeSeed::Component(&component.id), depth);
            (fold(full, depth, &component.id), reach)
        }
        Resolved::File(file) => {
            let owner = full
                .component_for_path(&file)
                .with_context(|| format!("no component contains `{target}`"))?;
            let reach = full.change_impact(ChangeSeed::File(&file), depth);
            importers = Some(import_sites(full, depth, &file, caps.sites));
            (fold(full, depth, &owner.id), reach)
        }
        Resolved::Symbol(symbol) => match full.component(&ComponentId::new(symbol.id.as_str())) {
            // a Rust module's symbol stands for its component
            Some(module) => {
                let reach = full.change_impact(ChangeSeed::Component(&module.id), depth);
                importers = component_file(full, root, module)
                    .map(|file| import_sites(full, depth, &file, caps.sites));
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
                        caps.sites,
                    ));
                    may_use = Some(sites_of(
                        full,
                        depth,
                        &found.may_use,
                        found.recorded,
                        caps.sites,
                    ));
                }
                symbol_id = Some(symbol.id.clone());
                (fold(full, depth, &symbol.component), reach)
            }
        },
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
            shown: reach.tests.into_iter().take(caps.tests).collect(),
        },
        target: Some(at.id),
        module: None,
        folded_from: at.folded_from,
        symbol: symbol_id,
        subpath,
        importers,
        may_use,
    };
    Ok(Answer {
        output: crate::json(&result)?,
        found: Found::One,
    })
}

/// How many import sites and test files `impact` shows.
#[derive(Clone, Copy)]
struct Caps {
    sites: usize,
    tests: usize,
}

/// What changing a module that no component carries may reach: the
/// components whose production files import it, everything that reaches
/// those files, and the test files that import it or reach them.
fn import_name_impact(
    full: &ArchitectureGraph,
    depth: usize,
    target: &str,
    module: String,
    caps: Caps,
) -> Result<String> {
    let imports: Vec<&UnmappedImport> = full.unmapped_imports_of(&module).collect();
    let (mut direct, mut transitive, mut tests) =
        (BTreeSet::new(), BTreeSet::new(), BTreeSet::new());
    let mut followed: BTreeSet<&str> = BTreeSet::new();
    for import in &imports {
        let file = import.evidence.file.as_str();
        if import.evidence.test {
            tests.insert(file.to_owned());
            continue;
        }
        let component = full.ancestor_at(&import.from, depth);
        direct.insert(component.clone());
        transitive.insert(component);
        if followed.insert(file) {
            let reach = full.change_impact(ChangeSeed::File(file), depth);
            transitive.extend(reach.transitive);
            tests.extend(reach.tests);
        }
    }
    let mut sites: Vec<ImportSite> = Vec::new();
    for import in &imports {
        let e = &import.evidence;
        if !sites.iter().any(|s| s.file == e.file && s.line == e.line) {
            sites.push(ImportSite {
                file: e.file.clone(),
                line: e.line,
                component: full.ancestor_at(&import.from, depth),
                test: e.test,
            });
        }
    }
    // production code first
    sites.sort_by(|a, b| (a.test, &a.file, a.line).cmp(&(b.test, &b.file, b.line)));
    let total = sites.len();
    sites.truncate(caps.sites);
    let result = ImpactResult {
        requested: target,
        depth,
        target: None,
        module: Some(module),
        folded_from: None,
        symbol: None,
        subpath: None,
        also_named: Vec::new(),
        also_at_path: Vec::new(),
        direct: direct.into_iter().collect(),
        transitive: transitive.into_iter().collect(),
        tests: TestFiles {
            total: tests.len(),
            shown: tests.into_iter().take(caps.tests).collect(),
        },
        importers: Some(ImportSites {
            recorded: true,
            total,
            shown: sites,
        }),
        may_use: None,
    };
    crate::json(&result)
}
