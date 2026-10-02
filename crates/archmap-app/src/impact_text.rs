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
    component_head, count, display, file_head, import_counts, import_location, namesakes,
    not_traced, place, shell_word, statements_title, symbol_line, with_more,
};
use crate::views::{About, Dependent, ImpactResult, ImportSites, MAX_IMPORT_SITES, MAX_TEST_FILES};

/// Default caps, lifted by `verbose`; statements and test files are capped
/// as the JSON caps them.
const MAX_COMPONENTS: usize = 30;
const MAX_LOCATIONS: usize = 3;

struct Caps {
    components: usize,
    statements: usize,
    tests: usize,
    locations: usize,
}

impl Caps {
    fn new(verbose: bool) -> Self {
        if verbose {
            Caps {
                components: usize::MAX,
                statements: usize::MAX,
                tests: usize::MAX,
                locations: usize::MAX,
            }
        } else {
            Caps {
                components: MAX_COMPONENTS,
                statements: MAX_IMPORT_SITES,
                tests: MAX_TEST_FILES,
                locations: MAX_LOCATIONS,
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
        truncated |= statements(
            &mut out,
            "Imports below",
            note,
            "from",
            below,
            rolled,
            &caps,
        );
    }
    if let Some(may_use) = result.may_use.as_ref().filter(|s| s.total > 0) {
        let note = Some("imports the whole module");
        truncated |= statements(&mut out, "May use", note, "whole", may_use, rolled, &caps);
    }
    truncated |= transitive(&mut out, result, rolled, &caps);
    truncated |= tests(&mut out, result, &caps);
    if let Some(found) = &result.not_traced {
        truncated |= not_traced(&mut out, found, caps.locations, true, true);
    }
    if truncated {
        let _ = writeln!(out, "\nLists are capped; verbose lists every entry.");
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
        let mut line = format!("  {}", display(rolled, &dependent.id));
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
            statements(out, "Imported by", None, "from", sites, rolled, caps)
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

/// One statement per line, as `query` locates it, with the barrel it went
/// through (`taken`: `from` for the name, `whole`) and the component it is
/// in unless that component is its file.
fn statements(
    out: &mut String,
    title: &str,
    note: Option<&str>,
    taken: &str,
    sites: &ImportSites,
    rolled: &ArchitectureGraph,
    caps: &Caps,
) -> bool {
    let shown = sites.shown.len().min(caps.statements);
    let heading = statements_title(title, note, sites.total, shown, sites.exports);
    let _ = writeln!(out, "\n{heading}");
    let rest = &sites.shown[shown..];
    for site in &sites.shown[..shown] {
        let mut line = import_location(site.evidence, 0, false);
        if let Some(barrel) = site.through {
            let _ = write!(line, " ({taken} {barrel}, which passes it on)");
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
    shown < sites.total
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
            display(rolled, &dependent.id),
            dependent.distance
        );
        if let Some(from) = &dependent.from {
            let _ = write!(line, ", through {from}");
        }
        let _ = writeln!(out, "{line}");
    }
    shown < further.len()
}

/// The test files to run again after the change: a changed test file is
/// one of them. Those left out because their mocks replace a module on the
/// way are counted at the end, located at their first such mock.
fn tests(out: &mut String, result: &ImpactResult, caps: &Caps) -> bool {
    let total = result.tests.total;
    let shown = result.tests.shown.len().min(caps.tests);
    if total == 0 {
        let _ = writeln!(out, "\nTests to run again: none");
    } else {
        let _ = writeln!(out, "\nTests to run again: {}", count(total, shown));
    }
    for file in &result.tests.shown[..shown] {
        match &result.about {
            About::File(target) if target == file => {
                let _ = writeln!(out, "  {file} (the target itself)");
            }
            _ => {
                let _ = writeln!(out, "  {file}");
            }
        }
    }
    let left = &result.tests.left_out;
    if left.total == 0 {
        return shown < total;
    }
    let what = match left.total {
        1 => "1 test file reaches it only through a module its mock replaces".to_owned(),
        n => format!("{n} test files reach it only through modules their mocks replace"),
    };
    let places: Vec<String> = left
        .shown
        .iter()
        .take(caps.locations)
        .filter_map(|test| test.mocks.first())
        .map(|mock| format!("{} (mocks {})", place(&mock.file, mock.line), mock.target))
        .collect();
    let _ = writeln!(
        out,
        "  left out: {what}: {}",
        with_more(&places, left.total)
    );
    shown < total || places.len() < left.total
}
