//! `impact`: what may be affected when a target changes.

use std::collections::BTreeSet;

use anyhow::{Context, Result};
use archmap_core::{
    ArchitectureGraph, ChangeSeed, Component, ComponentId, ComponentKind, Edge, Evidence, Symbol,
    UnmappedImport,
};

use crate::not_traced::{not_traced, Own, Place, Subject};
use crate::resolve::{resolve, Resolved};
use crate::target::{component_file, fold, namesakes, reject_outside, unquote};
use crate::views::{
    About, ImpactResult, ImportSite, ImportSites, TestFiles, MAX_IMPORT_SITES, MAX_TEST_FILES,
};
use crate::{Answer, Format, Found, ImpactRequest, Workspace};

/// What a resolved target is, for what could not be traced to it.
enum Traced<'g> {
    Component(&'g Component),
    /// A file, with the component it is or that holds it.
    File(String, Option<&'g Component>),
    Symbol(&'g Symbol),
}

fn import_sites<'a>(
    full: &'a ArchitectureGraph,
    depth: usize,
    file: &str,
    cap: usize,
) -> ImportSites<'a> {
    let facts = full.file_facts(file);
    sites_of(full, depth, &facts.importers, facts.importers_recorded, cap)
}

fn sites_of<'a>(
    full: &ArchitectureGraph,
    depth: usize,
    statements: &[(&'a Edge, &'a Evidence)],
    recorded: bool,
    cap: usize,
) -> ImportSites<'a> {
    let statements = statements.iter().map(|(edge, e)| (&edge.from, *e));
    sites(full, depth, statements, recorded, cap)
}

/// One site per statement, production code first, then by place; the first
/// `cap` shown. A statement that takes values and types from the file shows
/// as what runs, as `query` shows it.
fn sites<'a>(
    full: &ArchitectureGraph,
    depth: usize,
    statements: impl Iterator<Item = (&'a ComponentId, &'a Evidence)>,
    recorded: bool,
    cap: usize,
) -> ImportSites<'a> {
    let mut sites: Vec<ImportSite> = Vec::new();
    for (from, e) in statements {
        match sites
            .iter_mut()
            .find(|s| s.file == e.file && s.line == e.line)
        {
            Some(site) => {
                if site.evidence.type_only && !e.type_only {
                    site.evidence = e;
                }
            }
            None => sites.push(ImportSite {
                file: e.file.clone(),
                line: e.line,
                component: full.ancestor_at(from, depth),
                test: e.test,
                evidence: e,
            }),
        }
    }
    // production code first
    sites.sort_by(|a, b| (a.test, &a.file, a.line).cmp(&(b.test, &b.file, b.line)));
    let total = sites.len();
    let exports = sites
        .iter()
        .filter(|s| s.evidence.note.as_deref() == Some("export"))
        .count();
    sites.truncate(cap);
    ImportSites {
        recorded,
        total,
        shown: sites,
        exports,
    }
}

impl Workspace {
    /// `impact` as text or JSON, or the candidates when the target names
    /// several things.
    pub fn impact(&self, request: &ImpactRequest) -> Result<Answer> {
        impact(self, request)
    }
}

fn impact(ws: &Workspace, request: &ImpactRequest) -> Result<Answer> {
    let ImpactRequest {
        target,
        depth,
        format,
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
    let traced: Traced;
    let (at, reach) = match resolve(full, &rolled, root, target)? {
        Resolved::Candidates(candidates) => {
            return Ok(Answer {
                output: candidates.render(full, target, format)?,
                found: Found::Candidates,
            })
        }
        Resolved::ImportName(module) => {
            let result = import_name_impact(full, depth, target, &module, caps);
            return Ok(Answer {
                output: render(&result, format, full, &rolled, verbose)?,
                found: Found::One,
            });
        }
        Resolved::Component(component) => {
            let reach = full.change_impact(ChangeSeed::Component(&component.id), depth);
            traced = match component_file(full, root, component) {
                Some(file) => {
                    importers = Some(import_sites(full, depth, &file, caps.sites));
                    Traced::File(file, Some(component))
                }
                None => Traced::Component(component),
            };
            (fold(full, depth, &component.id), reach)
        }
        Resolved::Package {
            component,
            subpath: after,
        } => {
            subpath = Some(after);
            traced = Traced::Component(component);
            let reach = full.change_impact(ChangeSeed::Component(&component.id), depth);
            (fold(full, depth, &component.id), reach)
        }
        Resolved::File(file) => {
            let owner = full
                .component_for_path(&file)
                .with_context(|| format!("no component contains `{target}`"))?;
            let reach = full.change_impact(ChangeSeed::File(&file), depth);
            importers = Some(import_sites(full, depth, &file, caps.sites));
            let at = fold(full, depth, &owner.id);
            traced = Traced::File(file, Some(owner));
            (at, reach)
        }
        Resolved::Symbol(symbol) => match full.component(&ComponentId::new(symbol.id.as_str())) {
            // a Rust module's symbol stands for its component
            Some(module) => {
                let reach = full.change_impact(ChangeSeed::Component(&module.id), depth);
                traced = match component_file(full, root, module) {
                    Some(file) => {
                        importers = Some(import_sites(full, depth, &file, caps.sites));
                        Traced::File(file, Some(module))
                    }
                    None => Traced::Component(module),
                };
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
                traced = Traced::Symbol(symbol);
                (fold(full, depth, &symbol.component), reach)
            }
        },
    };

    let none_found =
        |sites: &Option<ImportSites>| sites.as_ref().is_some_and(|s| s.recorded && s.total == 0);
    let subject = match &traced {
        Traced::Component(component) => Subject {
            language: component.language.as_deref(),
            place: component.path.as_deref().map(Place::Directory),
            own: Own::Component(&at.id, depth),
            script: component.kind == ComponentKind::Script,
            unreached: false,
        },
        Traced::File(file, owner) => Subject {
            language: owner.and_then(|c| c.language.as_deref()),
            place: Some(Place::File(file)),
            own: Own::File(file),
            script: owner.is_some_and(|c| c.kind == ComponentKind::Script),
            unreached: none_found(&importers),
        },
        Traced::Symbol(symbol) => {
            let declared = full.component(&symbol.component);
            Subject {
                language: declared.and_then(|c| c.language.as_deref()),
                place: None,
                own: Own::File(symbol.location().map_or("", |e| e.file.as_str())),
                script: declared.is_some_and(|c| c.kind == ComponentKind::Script),
                unreached: none_found(&importers) && none_found(&may_use),
            }
        }
    };
    let not_traced = not_traced(full, &subject, caps.sites);

    let about = match traced {
        Traced::Component(_) => About::Component,
        Traced::File(file, _) => About::File(file),
        Traced::Symbol(symbol) => About::Symbol(symbol),
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
        not_traced,
        about,
    };
    Ok(Answer {
        output: render(&result, format, full, &rolled, verbose)?,
        found: Found::One,
    })
}

/// `result` as JSON, or as text that lists every entry when `verbose`.
fn render(
    result: &ImpactResult,
    format: Format,
    full: &ArchitectureGraph,
    rolled: &ArchitectureGraph,
    verbose: bool,
) -> Result<String> {
    Ok(match format {
        Format::Json => crate::json(result)?,
        Format::Text => crate::impact_text::render(result, full, rolled, verbose),
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
fn import_name_impact<'a>(
    full: &'a ArchitectureGraph,
    depth: usize,
    target: &'a str,
    module: &'a str,
    caps: Caps,
) -> ImpactResult<'a> {
    let imports: Vec<&UnmappedImport> = full.unmapped_imports_of(module).collect();
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
    let statements = imports.iter().map(|i| (&i.from, &i.evidence));
    ImpactResult {
        requested: target,
        depth,
        target: None,
        module: Some(module.to_owned()),
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
        importers: Some(sites(full, depth, statements, true, caps.sites)),
        may_use: None,
        not_traced: None,
        about: About::ImportName,
    }
}
