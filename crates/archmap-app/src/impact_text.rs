//! Compact text rendering of `impact`, for agents and people.
//!
//! The answer reads as `query` writes its own: the target's first lines,
//! `file:line` locations with their marks, lists capped with the rest
//! counted, and what could not be traced at the end. `verbose` lifts the
//! caps; JSON carries the same lists as fields.

use std::collections::BTreeMap;
use std::fmt::Write;

use archmap_core::{ArchitectureGraph, ComponentId};

use crate::query_text::{
    component_head, count, display, file_head, import_counts, import_location, marks, names_capped,
    namesakes, not_traced, package_name_head, place, plural, shell_word, statements_title,
    symbol_line, used_at, with_more, UsedAt, MAX_USE_FILES, SHOWN_NAMES,
};
use crate::views::{
    About, Dependent, ImpactResult, ImportSites, TestRouteView, TestWayView, MAX_IMPORT_SITES,
    MAX_TEST_FILES,
};

/// Default caps, lifted by `verbose`; statements and test files are capped
/// as the JSON caps them.
const MAX_COMPONENTS: usize = 30;
const MAX_LOCATIONS: usize = 3;

struct Caps {
    components: usize,
    statements: usize,
    tests: usize,
    locations: usize,
    use_files: usize,
    /// The files a component that holds the target names.
    holder_files: usize,
    /// The names a statement's line shows.
    names: usize,
}

impl Caps {
    fn new(verbose: bool) -> Self {
        if verbose {
            Caps {
                components: usize::MAX,
                statements: usize::MAX,
                tests: usize::MAX,
                locations: usize::MAX,
                use_files: usize::MAX,
                holder_files: usize::MAX,
                names: usize::MAX,
            }
        } else {
            Caps {
                components: MAX_COMPONENTS,
                statements: MAX_IMPORT_SITES,
                tests: MAX_TEST_FILES,
                locations: MAX_LOCATIONS,
                use_files: MAX_USE_FILES,
                holder_files: MAX_FILES,
                names: SHOWN_NAMES,
            }
        }
    }
}

pub(crate) fn render(
    result: &ImpactResult,
    full: &ArchitectureGraph,
    rolled: &ArchitectureGraph,
    verbose: bool,
) -> String {
    let caps = Caps::new(verbose);
    let mut out = String::new();
    let mut truncated = head(&mut out, result, full, rolled, &caps);
    truncated |= direct(&mut out, result, rolled, &caps);
    truncated |= importers(&mut out, result, full, rolled, &caps);
    if let Some(below) = &result.imports_below {
        let note = Some("they run it first");
        let list = List {
            title: "Imports below",
            note,
            taken: "from",
            names: false,
        };
        truncated |= statements(&mut out, list, below, rolled, &caps);
    }
    if let Some(may_use) = result.may_use.as_ref().filter(|s| s.total > 0) {
        let note = Some("imports the whole module");
        let list = List {
            title: "May use",
            note,
            taken: "whole",
            names: false,
        };
        truncated |= statements(&mut out, list, may_use, rolled, &caps);
    }
    if let (Some(uses), About::Symbol(symbol)) = (&result.used_at, &result.about) {
        let language = full
            .component(&symbol.component)
            .and_then(|c| c.language.as_deref());
        let view = UsedAt {
            uses,
            instance_method: result.instance_method,
            global: crate::not_traced::is_global(full, symbol),
            language,
            left_out: Some(&result.unnamed),
        };
        truncated |= used_at(&mut out, &view, caps.use_files, caps.locations);
    }
    truncated |= transitive(&mut out, result, rolled, &caps);
    truncated |= tests(&mut out, result, &caps);
    if let Some(section) = &result.co_change {
        truncated |= crate::co_change::render(&mut out, section, verbose);
    }
    let mut tail = String::new();
    if let Some(found) = &result.not_traced {
        truncated |= not_traced(&mut tail, found, caps.locations, true, true, true);
    }
    marks(&mut out, &tail);
    out.push_str(&tail);
    if truncated {
        let _ = writeln!(
            out,
            "\nLists are capped; verbose lists every entry, and JSON every entry with all evidence."
        );
    }
    out
}

/// The target's first lines, as `query` writes them for the same target,
/// and the components that share its name or path.
fn head(
    out: &mut String,
    result: &ImpactResult,
    full: &ArchitectureGraph,
    rolled: &ArchitectureGraph,
    caps: &Caps,
) -> bool {
    let depth = result.depth;
    let component = result.target.as_ref().and_then(|id| rolled.component(id));
    let folded_from = result.folded_from.as_ref();
    match (&result.about, component) {
        (About::Component, Some(c)) => {
            component_head(out, c, depth, folded_from, result.subpath.as_deref(), full)
        }
        (About::File(file), _) => {
            file_head(out, file, component, depth);
            if let Some(from) = folded_from {
                let _ = writeln!(out, "folded from: {}", display(full, from));
            }
        }
        (About::Symbol(symbol), _) => {
            let mut line = symbol_line(symbol);
            if let Some(id) = &result.target {
                let _ = write!(line, "  in {}", display(rolled, id));
            }
            let _ = writeln!(out, "{line}, depth {depth}");
            let _ = writeln!(out, "id: {}", symbol.id);
            if let Some(from) = folded_from {
                let _ = writeln!(out, "folded from: {}", display(full, from));
            }
        }
        (About::ImportName, _) => {
            let module = result.module.as_deref().unwrap_or(result.requested);
            let _ = writeln!(out, "{module}: imports without an edge, depth {depth}");
        }
        (About::PackageName(package, name), _) => package_name_head(out, package, name, depth),
        (About::Component, None) => {
            let _ = writeln!(out, "{}, depth {depth}", result.requested);
        }
    }
    let also_named: Vec<&ComponentId> = result.also_named.iter().collect();
    let also_at_path: Vec<&ComponentId> = result.also_at_path.iter().collect();
    namesakes(out, &also_named, &also_at_path, caps.components)
}

/// The components that import the target. None of them may still mean
/// importers, inside the target's own component or in test code: the
/// heading says which.
fn direct(
    out: &mut String,
    result: &ImpactResult,
    rolled: &ArchitectureGraph,
    caps: &Caps,
) -> bool {
    if result.direct.is_empty() {
        let lists = || result.importers.iter().chain(&result.may_use);
        // production code comes first in each list
        let shown = || lists().flat_map(|sites| &sites.shown);
        let inside = shown().any(|s| !s.test && Some(&s.component) == result.target.as_ref());
        let any = lists().any(|sites| sites.total > 0);
        let why = if inside {
            " outside its own component"
        } else if any && shown().all(|s| s.test) {
            " (only test code imports it)"
        } else {
            ""
        };
        let _ = writeln!(out, "\nDirect dependents: none{why}");
        return false;
    }
    let shown = result.direct.len().min(caps.components);
    let _ = writeln!(
        out,
        "\nDirect dependents: {}",
        count(result.direct.len(), shown)
    );
    // those with the most statements into the target first
    for dependent in &result.direct[..shown] {
        let mut line = format!("  {}", dependent_name(rolled, dependent, caps));
        let counted = dependent.imports.unwrap_or_default();
        if let Some(counts) = import_counts(counted.production, counted.tests) {
            let _ = write!(line, "  {counts}");
        }
        let _ = writeln!(out, "{line}");
    }
    shown < result.direct.len()
}

/// The statements that import the target, where they are recorded.
fn importers(
    out: &mut String,
    result: &ImpactResult,
    full: &ArchitectureGraph,
    rolled: &ArchitectureGraph,
    caps: &Caps,
) -> bool {
    let language = || {
        let declared = match &result.about {
            About::Symbol(symbol) => full.component(&symbol.component),
            _ => result.target.as_ref().and_then(|id| rolled.component(id)),
        };
        declared
            .and_then(|c| c.language.clone())
            .unwrap_or_else(|| "this language".to_owned())
    };
    match (&result.importers, &result.target) {
        (None, Some(id)) if matches!(result.about, About::Component) => {
            let _ = writeln!(
                out,
                "\nImported by: not listed for a whole component: `query {}` shows where it \
                 is imported",
                shell_word(id.as_str())
            );
            false
        }
        (Some(sites), _) if sites.recorded && sites.total == 0 => {
            let _ = writeln!(out, "\nImported by: none");
            false
        }
        (Some(sites), _) if sites.recorded => {
            let list = List {
                title: "Imported by",
                note: None,
                taken: "from",
                // a symbol's importers take it: the names say nothing more
                names: !matches!(result.about, About::Symbol(_) | About::PackageName(..)),
            };
            statements(out, list, sites, rolled, caps)
        }
        // not recorded for the language, or a symbol whose importers are
        // unknown
        _ => {
            let _ = writeln!(
                out,
                "\nImported by: unknown (no evidence names imported files for {})",
                language()
            );
            false
        }
    }
}

/// A list of statements as `statements` writes it.
struct List<'a> {
    title: &'a str,
    note: Option<&'a str>,
    /// How a statement through a barrel takes it: `from` for the name,
    /// `whole`.
    taken: &'a str,
    /// Whether a line says which names its statement takes.
    names: bool,
}

/// One statement per line, as `query` locates it, with the barrel it went
/// through and the component it is in unless that component is its file.
fn statements(
    out: &mut String,
    list: List,
    sites: &ImportSites,
    rolled: &ArchitectureGraph,
    caps: &Caps,
) -> bool {
    let List {
        title,
        note,
        taken,
        names: show_names,
    } = list;
    let shown = sites.shown.len().min(caps.statements);
    let mut truncated = shown < sites.total;
    let heading = statements_title(title, note, sites.total, shown, sites.exports);
    let _ = writeln!(out, "\n{heading}");
    let rest = &sites.shown[shown..];
    for site in &sites.shown[..shown] {
        // a type's taker names the type below
        let names = match show_names && site.takes_type.is_none() {
            true => caps.names,
            false => 0,
        };
        truncated |= names_capped(site.evidence, names);
        let mut line = import_location(site.evidence, 0, false, names);
        if let Some(barrel) = site.through {
            let _ = write!(line, " ({taken} {barrel}, which passes it on)");
        }
        if let Some(name) = site.takes_type {
            let _ = write!(line, " (takes {name}, whose methods the target holds)");
        }
        let path = rolled
            .component(&site.component)
            .and_then(|c| c.path.as_deref());
        if path != Some(site.file.as_str()) {
            let _ = write!(line, "  in {}", display(rolled, &site.component));
        }
        let _ = writeln!(out, "  {line}");
    }
    // where the statements not shown are, the components with most first
    if !rest.is_empty() {
        let mut by_component: BTreeMap<&ComponentId, usize> = BTreeMap::new();
        for site in rest {
            *by_component.entry(&site.component).or_default() += 1;
        }
        let mut most: Vec<(&ComponentId, usize)> = by_component.into_iter().collect();
        most.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        let places: Vec<String> = most
            .iter()
            .take(caps.locations)
            .map(|(id, n)| format!("{} {n}", display(rolled, id)))
            .collect();
        let mut line = format!("  {} more in: {}", rest.len(), places.join(", "));
        if most.len() > places.len() {
            let _ = write!(
                line,
                ", +{} more {}",
                most.len() - places.len(),
                if most.len() - places.len() == 1 {
                    "component"
                } else {
                    "components"
                }
            );
        }
        let _ = writeln!(out, "{line}");
    }
    truncated
}

/// The components reached only through others: `direct` is not repeated.
fn transitive(
    out: &mut String,
    result: &ImpactResult,
    rolled: &ArchitectureGraph,
    caps: &Caps,
) -> bool {
    // nearest first, each with the file it was reached from
    let further: Vec<&Dependent> = result
        .transitive
        .iter()
        .filter(|d| d.distance > 1)
        .collect();
    if result.transitive.is_empty() {
        let _ = writeln!(out, "\nTransitive dependents: none");
        return false;
    }
    if further.is_empty() {
        let _ = writeln!(out, "\nTransitive dependents: none beyond the direct ones");
        return false;
    }
    let shown = further.len().min(caps.components);
    let heading = if result.direct.is_empty() {
        count(further.len(), shown)
    } else {
        let mut heading = format!(
            "{} more ({} in all)",
            further.len(),
            result.transitive.len()
        );
        if shown < further.len() {
            let _ = write!(heading, ", showing {shown}");
        }
        heading
    };
    let _ = writeln!(out, "\nTransitive dependents: {heading}");
    for dependent in &further[..shown] {
        let mut line = format!(
            "  {}  {} steps",
            dependent_name(rolled, dependent, caps),
            dependent.distance
        );
        if let Some(through) = &dependent.through_shown {
            let _ = write!(line, ", through {through}");
        }
        if let Some(declared) = &dependent.declared_in {
            let _ = write!(
                line,
                " (declared in {})",
                place(&declared.file, declared.line)
            );
        }
        if let Some(imported) = &dependent.imported_in {
            let _ = write!(
                line,
                " (imported in {})",
                place(&imported.file, imported.line)
            );
        }
        let _ = writeln!(out, "{line}");
    }
    shown < further.len()
}

/// A dependent as the lists name it: a component that holds the target with
/// the files of its own it is reached through (`ts-shop (src/index.ts)`).
fn dependent_name(rolled: &ArchitectureGraph, dependent: &Dependent, caps: &Caps) -> String {
    let name = display(rolled, &dependent.id);
    match dependent.files.len() {
        0 => name.to_owned(),
        n => {
            let shown = &dependent.files[..n.min(caps.holder_files)];
            format!("{name} ({})", with_more(shown, n))
        }
    }
}

/// How many files a component that holds the target names.
const MAX_FILES: usize = 3;

/// The test files to run again after the change: a changed test file is
/// one of them. Those left out because their mocks replace a module on the
/// way are counted at the end, located at their first such mock.
fn tests(out: &mut String, result: &ImpactResult, caps: &Caps) -> bool {
    let total = result.tests.total;
    let shown = result.tests.files.len().min(caps.tests);
    if total == 0 {
        let _ = writeln!(out, "\nTests to run again: none");
    } else {
        let _ = writeln!(out, "\nTests to run again: {}", count(total, shown));
    }
    // each with its nearest way that takes values, or where none does, its
    // nearest; a conftest.py as the tests it is loaded for
    for test in &result.tests.files[..shown] {
        match &test.stands_for {
            Some(dir) => {
                let _ = writeln!(
                    out,
                    "  {dir} (conftest.py: pytest loads it for every test below)"
                );
            }
            None => match way_text(&test.ways, test.types_only, &result.about, &test.file) {
                Some(how) => {
                    let _ = writeln!(out, "  {} ({how})", test.file);
                }
                None => {
                    let _ = writeln!(out, "  {}", test.file);
                }
            },
        }
    }
    // test code that no runner runs as a test, not counted
    let others = &result.tests.not_tests;
    let mut truncated = shown < total;
    if !others.is_empty() {
        let places: Vec<String> = others
            .iter()
            .take(caps.locations)
            .map(|other| {
                let mut notes = vec![other.kind.to_owned()];
                if let Some(TestWayView::Target) = other.ways.first().map(|r| &r.way) {
                    notes.push(match &result.about {
                        About::Component => "in the target".to_owned(),
                        About::File(target) if *target != other.file => "in its package".to_owned(),
                        _ => "the target itself".to_owned(),
                    });
                }
                if !other.for_tests.is_empty() {
                    let n = other.for_tests.len();
                    notes.push(format!("for {} listed", plural(n, "test")));
                }
                format!("{} ({})", other.file, notes.join(", "))
            })
            .collect();
        truncated |= places.len() < others.len();
        let _ = writeln!(out, "  not tests: {}", with_more(&places, others.len()));
    }
    let left = &result.tests.left_out;
    if left.total == 0 {
        return truncated;
    }
    let what = match left.total {
        1 => "1 test file reaches it only through a module its mock replaces".to_owned(),
        n => format!("{n} test files reach it only through modules their mocks replace"),
    };
    let places: Vec<String> = left
        .files
        .iter()
        .take(caps.locations)
        .map(|test| match test.mocks.first() {
            Some(mock) => format!("{} (mocks {})", place(&mock.file, mock.line), mock.target),
            None => test.file.clone(),
        })
        .collect();
    let _ = writeln!(
        out,
        "  left out: {what}: {}",
        with_more(&places, left.total)
    );
    truncated || places.len() < left.total
}

/// How a test reaches the target, as its line says it: its nearest way
/// that takes values, or where none does, its nearest.
fn way_text(ways: &[TestRouteView], types_only: bool, about: &About, file: &str) -> Option<String> {
    let route = ways.iter().find(|r| !r.types_only).or(ways.first());
    let mock = route.is_some_and(|r| r.mock);
    let mut how = match route.map(|r| &r.way)? {
        TestWayView::Target => match about {
            About::Component => "in the target".to_owned(),
            About::Symbol(_) => "defines it".to_owned(),
            // a package's manifest changes the files of its package
            About::File(target) if target != file => "in its package".to_owned(),
            _ => "the target itself".to_owned(),
        },
        // a call that puts a mock in its place, and nothing else
        TestWayView::Takes { via: None } if mock => "mocks it".to_owned(),
        TestWayView::Takes { via: None } => "takes it".to_owned(),
        TestWayView::Takes { via: Some(via) } => format!("takes it, via {via}"),
        TestWayView::Whole if mock => "mocks it".to_owned(),
        TestWayView::Whole => "takes its module whole".to_owned(),
        TestWayView::RunsFirst { file } => format!("runs first: {file}"),
        TestWayView::Through { file } if mock => format!("through {file}, by its mock"),
        TestWayView::Through { file } => format!("through {file}"),
    };
    if types_only {
        how.push_str(", types only");
    }
    Some(how)
}
