//! `impact`: what may be affected when a target changes.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use archmap_core::{
    ArchitectureGraph, ChangeSeed, Component, ComponentId, ComponentKind, Edge, Evidence, Hop,
    Symbol, UnmappedImport,
};

use crate::co_change::{self, Changed};
use crate::not_traced::{barrels, not_traced, Narrowed, NotTraced, Own, Place, Subject};
use crate::resolve::{resolve, Resolved};
use crate::target::{component_file, fold, namesakes, reject_outside, unquote};
use crate::views::{
    About, Dependent, ImpactResult, ImportSite, ImportSites, LeftOut, Location, MockCall,
    MockingTest, Statements, TestFiles,
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

/// The statements outside a package that import a module below `file`,
/// its entry file, which run it first; `None` when there are none.
fn below_sites<'a>(
    full: &'a ArchitectureGraph,
    depth: usize,
    file: &str,
    cap: usize,
) -> Option<ImportSites<'a>> {
    let below = full.imports_below(file);
    (!below.is_empty()).then(|| sites_of(full, depth, &below, true, cap))
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

/// The statements of a symbol, each with the barrel it went through.
fn symbol_sites<'a>(
    full: &ArchitectureGraph,
    depth: usize,
    statements: &[(&'a Edge, &'a Evidence)],
    through: &BTreeMap<(&'a str, Option<u32>), &'a str>,
    recorded: bool,
    cap: usize,
) -> ImportSites<'a> {
    let mut sites = sites_of(full, depth, statements, recorded, cap);
    for site in &mut sites.shown {
        site.through = through
            .get(&(site.evidence.file.as_str(), site.evidence.line))
            .copied();
    }
    sites
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
                through: None,
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
    // the answer lists everything; the text shows the first of each list
    let caps = Caps {
        sites: usize::MAX,
        tests: usize::MAX,
    };
    reject_outside(root, target)?;
    let full = ws.graph();
    let rolled = full.rollup(depth);

    let (mut importers, mut imports_below) = (None, None);
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
                    imports_below = below_sites(full, depth, &file, caps.sites);
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
            imports_below = below_sites(full, depth, &file, caps.sites);
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
                    importers = Some(symbol_sites(
                        full,
                        depth,
                        &found.by_name,
                        &found.through,
                        found.recorded,
                        caps.sites,
                    ));
                    may_use = Some(symbol_sites(
                        full,
                        depth,
                        &found.may_use,
                        &found.through,
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
            unreached: none_found(&importers) && imports_below.is_none(),
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
    let mut not_traced = not_traced(full, &subject, usize::MAX);
    // the barrels past which the reach went on by names only
    let narrowed = match &traced {
        Traced::File(file, _) => Some(Narrowed::File(file)),
        Traced::Symbol(symbol) => Some(Narrowed::Symbol(symbol)),
        Traced::Component(_) => None,
    };
    if let Some(found) =
        narrowed.and_then(|n| barrels(full, n, &reach.relayed, &reach.tests, usize::MAX))
    {
        not_traced.get_or_insert_with(NotTraced::default).barrels = Some(found);
    }

    // the files changed in the same commits, from the committed history
    let changed = match &traced {
        Traced::File(file, _) => Some(Changed::File {
            path: file,
            symbol: false,
        }),
        Traced::Symbol(symbol) => symbol.location().map(|e| Changed::File {
            path: &e.file,
            symbol: true,
        }),
        Traced::Component(component) => component
            .path
            .is_some()
            .then_some(Changed::Component(component)),
    };
    let co_change = changed.map(|changed| {
        let history = ws.history();
        if let Some(gaps) = co_change::gaps(history) {
            not_traced.get_or_insert_with(NotTraced::default).history = Some(gaps);
        }
        co_change::section(history, full, changed)
    });

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
    // the statements of each direct dependent into the target: those the
    // lists hold, or for a whole component those into its files
    let lists = importers.iter().chain(&imports_below).chain(&may_use);
    let mut counted = counts(lists.flat_map(|sites| &sites.shown));
    if let About::Component = about {
        counted = into_component(full, depth, &at.id);
    }
    // the components that hold the target
    let holders: BTreeSet<ComponentId> = full
        .containment_path(&at.id)
        .into_iter()
        .filter(|c| *c != at.id)
        .collect();
    let (direct, transitive) = dependents(full, &reach, &counted, &holders, 0);
    let result = ImpactResult {
        also_named: owned(also_named),
        also_at_path: owned(also_at_path),
        requested: target,
        depth,
        direct,
        transitive,
        tests: test_files(reach.tests, reach.left_out, caps.tests),
        target: Some(at.id),
        module: None,
        folded_from: at.folded_from,
        symbol: symbol_id,
        subpath,
        importers,
        imports_below,
        may_use,
        co_change,
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

/// How many import sites and test files `impact` lists.
#[derive(Clone, Copy)]
struct Caps {
    sites: usize,
    tests: usize,
}

/// The statements of `sites` by the component each is in.
fn counts<'s>(
    sites: impl Iterator<Item = &'s ImportSite<'s>>,
) -> BTreeMap<ComponentId, Statements> {
    let mut counted: BTreeMap<ComponentId, Statements> = BTreeMap::new();
    let mut seen: BTreeSet<(&str, Option<u32>)> = BTreeSet::new();
    for site in sites {
        if !seen.insert((site.file.as_str(), site.line)) {
            continue;
        }
        let entry = counted.entry(site.component.clone()).or_default();
        match site.test {
            true => entry.tests += 1,
            false => entry.production += 1,
        }
    }
    counted
}

/// The statements into the files of `component` and below from outside it,
/// by the component each is in at `depth`.
fn into_component(
    full: &ArchitectureGraph,
    depth: usize,
    component: &ComponentId,
) -> BTreeMap<ComponentId, Statements> {
    let inside = |file: &str| {
        full.component_for_path(file)
            .is_some_and(|c| full.containment_path(&c.id).contains(component))
    };
    let mut counted: BTreeMap<ComponentId, Statements> = BTreeMap::new();
    let mut seen: BTreeSet<(&str, Option<u32>)> = BTreeSet::new();
    for (edge, e) in full
        .edges
        .iter()
        .filter(|e| e.kind == archmap_core::EdgeKind::Import)
        .flat_map(|edge| edge.evidence.iter().map(move |e| (edge, e)))
    {
        let into = e.target.as_deref().is_some_and(inside);
        if !into || inside(&e.file) || !seen.insert((e.file.as_str(), e.line)) {
            continue;
        }
        let entry = counted
            .entry(full.ancestor_at(&edge.from, depth))
            .or_default();
        match e.test {
            true => entry.tests += 1,
            false => entry.production += 1,
        }
    }
    counted
}

/// The direct and the transitive dependents of `reach`, with their
/// statements and distances: the direct ones by their statements in
/// production code, then all, the transitive ones nearest first, each tie
/// by the name the text shows, then id; `beyond` steps added to the walk's
/// distances.
fn dependents(
    full: &ArchitectureGraph,
    reach: &archmap_core::Reach,
    counted: &BTreeMap<ComponentId, Statements>,
    holders: &BTreeSet<ComponentId>,
    beyond: usize,
) -> (Vec<Dependent>, Vec<Dependent>) {
    let name = |id: &ComponentId| {
        full.component(id)
            .map_or_else(|| id.to_string(), |c| c.name.clone())
    };
    let location = |place: &Option<(String, Option<u32>)>| {
        place.as_ref().map(|(file, line)| Location {
            file: file.clone(),
            line: *line,
        })
    };
    let dependent = |id: &ComponentId| {
        let distance = match reach.direct.contains(id) {
            true => 1,
            false => reach.distance.get(id).map_or(1, |d| d + beyond),
        };
        // a component that holds the target is reached through files of
        // its own, not as a whole
        let through = match holders.contains(id) {
            false => Vec::new(),
            true => reach.files.get(id).cloned().unwrap_or_default(),
        };
        let hop = (distance > 1).then(|| reach.from.get(id)).flatten();
        let (from, from_shown, declared_in, imported_in) = match hop {
            Some(Hop::File(file)) => (Some(file.clone()), Some(file.clone()), None, None),
            Some(Hop::Component {
                id,
                declared_in,
                imported_in,
            }) => (
                Some(id.to_string()),
                Some(name(id)),
                location(declared_in),
                location(imported_in),
            ),
            None => (None, None, None, None),
        };
        Dependent {
            id: id.clone(),
            distance,
            through,
            imports: (distance == 1).then(|| counted.get(id).copied()).flatten(),
            from,
            declared_in,
            imported_in,
            from_shown,
        }
    };
    let mut direct: Vec<Dependent> = reach.direct.iter().map(dependent).collect();
    direct.sort_by(|a, b| {
        let key = |d: &Dependent| {
            let s = d.imports.unwrap_or_default();
            (
                std::cmp::Reverse(s.production),
                std::cmp::Reverse(s.production + s.tests),
            )
        };
        key(a)
            .cmp(&key(b))
            .then_with(|| name(&a.id).cmp(&name(&b.id)))
            .then_with(|| a.id.cmp(&b.id))
    });
    let mut transitive: Vec<Dependent> = reach.transitive.iter().map(dependent).collect();
    transitive
        .sort_by(|a, b| (a.distance, name(&a.id), &a.id).cmp(&(b.distance, name(&b.id), &b.id)));
    (direct, transitive)
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
    let (mut direct, mut tests) = (BTreeSet::new(), BTreeSet::new());
    let mut seeds: BTreeSet<&str> = BTreeSet::new();
    for import in &imports {
        let file = import.evidence.file.as_str();
        if import.evidence.test {
            tests.insert(file.to_owned());
        } else {
            direct.insert(full.ancestor_at(&import.from, depth));
            seeds.insert(file);
        }
    }
    // every production importer at once, so that a file one of them reaches
    // through production code is no test of another
    let seeds: Vec<&str> = seeds.into_iter().collect();
    let mut reach = full.change_impact(ChangeSeed::Importers(&seeds), depth);
    // the importers' components are the direct dependents of the name, and
    // what reaches them is one step further
    reach.transitive.extend(direct.iter().cloned());
    reach.direct = direct;
    tests.extend(reach.tests.iter().cloned());
    // a test that imports the name itself is one to run again anyway
    let mut left_out = std::mem::take(&mut reach.left_out);
    left_out.retain(|file, _| !tests.contains(file));
    let statements = imports.iter().map(|i| (&i.from, &i.evidence));
    let importers = sites(full, depth, statements, true, caps.sites);
    let counted = counts(importers.shown.iter());
    let (direct, transitive) = dependents(full, &reach, &counted, &BTreeSet::new(), 1);
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
        direct,
        transitive,
        tests: test_files(tests, left_out, caps.tests),
        importers: Some(importers),
        imports_below: None,
        may_use: None,
        co_change: None,
        not_traced: None,
        about: About::ImportName,
    }
}

/// The test files to run again, and those left out with the mocks that
/// replace a module on their way, the first `cap` of each by path.
fn test_files(
    tests: BTreeSet<String>,
    left_out: BTreeMap<String, Vec<Evidence>>,
    cap: usize,
) -> TestFiles {
    TestFiles {
        total: tests.len(),
        shown: tests.into_iter().take(cap).collect(),
        left_out: LeftOut {
            total: left_out.len(),
            shown: left_out
                .into_iter()
                .take(cap)
                .map(|(file, mocks)| MockingTest {
                    file,
                    mocks: mocks
                        .into_iter()
                        .map(|m| MockCall {
                            file: m.file,
                            line: m.line,
                            target: m.target.unwrap_or_default(),
                        })
                        .collect(),
                })
                .collect(),
        },
    }
}
