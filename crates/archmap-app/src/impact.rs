//! `impact`: what may be affected when a target changes.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use archmap_core::{
    ArchitectureGraph, ChangeSeed, Component, ComponentId, ComponentKind, Edge, Evidence, Hop,
    ImportPlace, Symbol, SymbolImporters, SymbolUses, TestReach, TestRoute, TestWay,
    UnmappedImport,
};

use archmap_scan::ScanReport;

use crate::co_change::{self, Changed};
use crate::not_traced::{
    barrels, declares_global, not_traced, with_uses, Narrowed, NotTraced, Own, Place, Subject,
};
use crate::query::{instance_method, uses_of};
use crate::resolve::{resolve, Resolved};
use crate::target::{component_file, fold, namesakes, reject_outside, unquote};
use crate::views::{
    About, Dependent, ImpactResult, ImportSite, ImportSites, LeftOut, Location, MockCall,
    MockingTest, NotTest, Statements, TestFile, TestFiles, TestRouteView, TestWayView,
};
use crate::{Answer, Format, Found, ImpactRequest, Workspace};

/// What a resolved target is, for what could not be traced to it.
enum Traced<'g> {
    Component(&'g Component),
    /// A file, with the component it is or that holds it.
    File(String, Option<&'g Component>),
    Symbol(&'g Symbol),
}

/// The statements that import `file`, and for a Rust file of methods of a
/// type another file defines, those that take the type, which may call
/// them.
fn import_sites<'a>(
    full: &'a ArchitectureGraph,
    depth: usize,
    file: &str,
    cap: usize,
) -> ImportSites<'a> {
    let facts = full.file_facts(file);
    let takers = full.method_takers().remove(file).unwrap_or_default();
    let mut statements = facts.importers;
    statements.extend(takers.iter().map(|(edge, e, _)| (*edge, *e)));
    let recorded = facts.importers_recorded || !takers.is_empty();
    let mut sites = sites_of(full, depth, &statements, recorded, cap);
    for site in &mut sites.shown {
        site.takes_type = takers
            .iter()
            .find(|(_, e, _)| std::ptr::eq(*e, site.evidence))
            .map(|(_, _, name)| *name);
    }
    sites
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
                takes_type: None,
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
    let (mut used_at, mut unnamed) = (None, BTreeSet::new());
    // statements that take a symbol's file whole, those that never name it
    // included
    let mut takes_whole = false;
    let traced: Traced;
    let (at, reach) = match resolve(full, &rolled, root, target)? {
        Resolved::Candidates(candidates) => {
            return Ok(Answer {
                output: candidates.render(full, target, format)?,
                found: Found::Candidates,
            })
        }
        Resolved::ImportName(module) => {
            let result = import_name_impact(full, &ws.report, depth, target, &module, caps);
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
                // where it is used: a statement that takes its file whole
                // and never names it leaves the first step
                let found = full.symbol_importers(symbol);
                used_at = uses_of(full, &ws.report, symbol);
                if let (Some(found), Some(uses)) = (&found, &used_at) {
                    unnamed = never_named(found, uses);
                }
                let reach = full.change_impact(ChangeSeed::Symbol(symbol, &unnamed), depth);
                if let Some(found) = found {
                    importers = Some(symbol_sites(
                        full,
                        depth,
                        &found.by_name,
                        &found.through,
                        found.recorded,
                        caps.sites,
                    ));
                    takes_whole = !found.may_use.is_empty();
                    let whole: Vec<(&Edge, &Evidence)> = found
                        .may_use
                        .iter()
                        .copied()
                        .filter(|(_, e)| !in_set(&unnamed, e))
                        .collect();
                    may_use = Some(symbol_sites(
                        full,
                        depth,
                        &whole,
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
    // a file reaches its dependents through the package it is an entry of
    // too, which names no file
    let reached =
        !(reach.direct.is_empty() && reach.transitive.is_empty() && reach.tests.is_empty());
    let subject = match &traced {
        Traced::Component(component) => Subject {
            language: component.language.as_deref(),
            place: component.path.as_deref().map(Place::Directory),
            own: Own::Component(&at.id, depth),
            script: component.kind == ComponentKind::Script,
            global: full
                .symbols_of(&component.id)
                .any(|s| s.location().is_some_and(Evidence::declares_global)),
            unreached: false,
        },
        Traced::File(file, owner) => Subject {
            language: owner.and_then(|c| c.language.as_deref()),
            place: Some(Place::File(file)),
            own: Own::File(file),
            script: owner.is_some_and(|c| c.kind == ComponentKind::Script),
            global: declares_global(full, file),
            unreached: none_found(&importers) && imports_below.is_none() && !reached,
        },
        Traced::Symbol(symbol) => {
            let declared = full.component(&symbol.component);
            Subject {
                language: declared.and_then(|c| c.language.as_deref()),
                // an import that may be of its file, unresolved, may take it
                place: symbol.location().map(|e| Place::File(&e.file)),
                own: Own::File(symbol.location().map_or("", |e| e.file.as_str())),
                script: declared.is_some_and(|c| c.kind == ComponentKind::Script),
                global: symbol.location().is_some_and(Evidence::declares_global),
                unreached: none_found(&importers) && !takes_whole && !reached,
            }
        }
    };
    let mut not_traced = not_traced(full, &subject, usize::MAX);
    // what the uses pass could not follow, as `query` names it
    let instance = match &traced {
        Traced::Symbol(symbol) => used_at.is_some() && instance_method(symbol),
        _ => false,
    };
    if let Some(uses) = &used_at {
        not_traced = with_uses(not_traced, uses, instance);
    }
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

    // the file a test that takes the target loads
    let taken: Option<String> = match &traced {
        Traced::File(file, _) => Some(file.clone()),
        Traced::Symbol(symbol) => symbol.location().map(|e| e.file.clone()),
        Traced::Component(_) => None,
    };
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
        tests: test_files(
            full,
            &ws.report,
            taken.as_deref(),
            reach.tests,
            reach.test_ways,
            reach.left_out,
            caps.tests,
        ),
        target: Some(at.id),
        module: None,
        folded_from: at.folded_from,
        symbol: symbol_id,
        subpath,
        importers,
        imports_below,
        may_use,
        used_at,
        unnamed,
        instance_method: instance,
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

/// The statements of `found` that take the symbol's file whole and that the
/// uses pass read and found never naming it: they take nothing of it. Not a
/// statement that takes it by name, which loads the file all the same.
fn never_named(found: &SymbolImporters, uses: &SymbolUses) -> BTreeSet<ImportPlace> {
    let whole: BTreeSet<(&str, Option<u32>)> = found
        .may_use
        .iter()
        .map(|(_, e)| (e.file.as_str(), e.line))
        .collect();
    uses.unused
        .iter()
        .filter(|e| whole.contains(&(e.file.as_str(), e.line)))
        .filter_map(|e| {
            Some(ImportPlace {
                file: e.file.clone(),
                line: e.line?,
            })
        })
        .collect()
}

/// Whether `set` holds the statement `e` is evidence of.
fn in_set(set: &BTreeSet<ImportPlace>, e: &Evidence) -> bool {
    e.line.is_some_and(|line| {
        set.contains(&ImportPlace {
            file: e.file.clone(),
            line,
        })
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
        let files = match holders.contains(id) {
            false => Vec::new(),
            true => reach.files.get(id).cloned().unwrap_or_default(),
        };
        let hop = (distance > 1).then(|| reach.from.get(id)).flatten();
        let (through, through_shown, declared_in, imported_in) = match hop {
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
            files,
            imports: (distance == 1).then(|| counted.get(id).copied()).flatten(),
            through,
            declared_in,
            imported_in,
            through_shown,
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
    report: &ScanReport,
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
    // a test that imports the name takes it, before any other way
    let mut ways = std::mem::take(&mut reach.test_ways);
    for file in &tests {
        let way = ways.entry(file.clone()).or_default();
        let takes = TestRoute {
            way: TestWay::Takes { via: None },
            steps: 1,
            types_only: false,
        };
        way.ways.insert(0, takes);
        way.types_only = false;
    }
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
        tests: test_files(full, report, None, tests, ways, left_out, caps.tests),
        importers: Some(importers),
        imports_below: None,
        may_use: None,
        used_at: None,
        unnamed: BTreeSet::new(),
        instance_method: false,
        co_change: None,
        not_traced: None,
        about: About::ImportName,
    }
}

/// The test files to run again, and those left out with the mocks that
/// replace a module on their way, the first `cap` of each by path.
fn test_files(
    full: &ArchitectureGraph,
    report: &ScanReport,
    taken: Option<&str>,
    tests: BTreeSet<String>,
    mut ways: BTreeMap<String, TestReach>,
    left_out: BTreeMap<String, Vec<Evidence>>,
    cap: usize,
) -> TestFiles {
    // for each listed test and file it loads: whether every statement of
    // it there puts a mock in place of the file
    let mut mocks: BTreeMap<(&str, &str), bool> = BTreeMap::new();
    for e in full
        .edges
        .iter()
        .filter(|e| e.kind == archmap_core::EdgeKind::Import)
        .flat_map(|e| &e.evidence)
        .filter(|e| e.via().is_none() && tests.contains(&e.file))
    {
        if let Some(target) = e.target.as_deref() {
            let mock = archmap_scan::is_mock_call(e.note.as_deref().unwrap_or(""));
            *mocks.entry((e.file.as_str(), target)).or_insert(true) &= mock;
        }
    }
    // the test files that load each file
    let mut loaded_by: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for (test, target) in mocks.keys() {
        loaded_by.entry(target).or_default().insert(test);
    }
    let by_mock = |test: &str, file: Option<&str>| {
        file.is_some_and(|file| mocks.get(&(test, file)).copied().unwrap_or(false))
    };
    let view = |test: &str, route: TestRoute| {
        let (way, mock) = match route.way {
            TestWay::Target => (TestWayView::Target, false),
            TestWay::Takes { via } => {
                let mock = via.is_none() && by_mock(test, taken);
                (TestWayView::Takes { via }, mock)
            }
            TestWay::Whole => (TestWayView::Whole, false),
            TestWay::RunsFirst { entry } => (TestWayView::RunsFirst { file: entry }, false),
            TestWay::Through { from } => {
                let mock = by_mock(test, Some(&from));
                (TestWayView::Through { file: from }, mock)
            }
        };
        TestRouteView {
            way,
            steps: route.steps,
            types_only: route.types_only,
            mock,
        }
    };
    // what a runner runs, and the test code it runs no test of
    let kinds = archmap_scan::test_kinds(report, tests.iter().map(String::as_str));
    let mut files: Vec<TestFile> = Vec::new();
    let mut not_tests: Vec<NotTest> = Vec::new();
    for file in &tests {
        let reach = ways.remove(file).unwrap_or_default();
        let routes: Vec<TestRouteView> = reach.ways.into_iter().map(|r| view(file, r)).collect();
        let kind = kinds
            .get(file.as_str())
            .copied()
            .unwrap_or(archmap_scan::TestKind::Test);
        let other = match kind {
            archmap_scan::TestKind::Test | archmap_scan::TestKind::Conftest => None,
            archmap_scan::TestKind::Helper => Some("helper"),
            archmap_scan::TestKind::Example => Some("example"),
            archmap_scan::TestKind::Bench => Some("bench"),
        };
        match other {
            Some(kind) => not_tests.push(NotTest {
                file: file.clone(),
                kind,
                ways: routes,
                types_only: reach.types_only,
                for_tests: Vec::new(),
            }),
            None => files.push(TestFile {
                file: file.clone(),
                stands_for: (kind == archmap_scan::TestKind::Conftest).then(|| {
                    match file.rsplit_once('/') {
                        Some((dir, _)) => format!("{dir}/"),
                        None => "./".to_owned(),
                    }
                }),
                ways: routes,
                types_only: reach.types_only,
            }),
        }
    }
    // a helper counts the tests listed that load it
    let listed: BTreeSet<&str> = files.iter().map(|t| t.file.as_str()).collect();
    for helper in &mut not_tests {
        helper.for_tests = loaded_by
            .get(helper.file.as_str())
            .map(|tests| {
                tests
                    .intersection(&listed)
                    .map(|t| (*t).to_owned())
                    .collect()
            })
            .unwrap_or_default();
    }
    TestFiles {
        total: files.len(),
        files: files.into_iter().take(cap).collect(),
        not_tests,
        left_out: LeftOut {
            total: left_out.len(),
            files: left_out
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
