//! Deterministic summary of a rolled-up architecture graph.
//!
//! The summary is the first map an agent reads, so it is a small text IR
//! rather than a report: one fact per line, `key: value` fields, dependency
//! direction spelled `a -> b`, measured values instead of judgments, and no
//! prose. Every line is computed from the graph, so the same scan always
//! yields the same text.
//!
//! It is an index for choosing what to `query` next, not a listing. Long
//! lists keep their structurally busiest entries, an `omitted:` line counts
//! the rest and names the query that shows them, and the text aims to stay
//! within a byte budget however large the repository is. `--verbose` lifts
//! the caps.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::path::Path;

use archmap_core::{
    ArchitectureGraph, Component, ComponentId, ComponentKind, EdgeKind, UnmappedReason,
};

use crate::query_text::reason_label;

/// How many components the "most depended on" section lists.
const TOP_DEPENDED_ON: usize = 10;

/// How many importers an external dependency names before counting the
/// rest. Widely used libraries would otherwise dominate the summary without
/// telling an agent where to look.
const MAX_IMPORTERS: usize = 5;

/// How many components the dynamic-imports line names before counting the
/// rest.
const MAX_DYNAMIC_IMPORTERS: usize = 5;

/// How many names an `omitted:` line gives before counting the rest.
const MAX_OMITTED_NAMES: usize = 5;

/// Coupling that no analyzer reads, whatever the repository contains.
const RUNTIME_COUPLING: &str = "runtime coupling: not analyzed \
     (HTTP, databases, queues, subprocesses, configuration-driven loading)";

/// Default list caps, lifted by `--verbose`.
const MAX_COMPONENTS: usize = 30;
const MAX_INTERNAL: usize = 30;
const MAX_EXTERNAL: usize = 20;

/// Bytes the default summary aims to stay within. Lists shrink to fit, but
/// the header, coverage, `omitted:` lines and the most depended on list are
/// never trimmed and each list keeps `MIN_LISTED` entries, so very long names
/// can still exceed it. What never happens is growth with the repository.
const BUDGET: usize = 8 * 1024;
const MIN_LISTED: usize = 10;

/// List caps and the byte budget of one summary.
#[derive(Clone, Copy)]
struct Limits {
    components: usize,
    internal: usize,
    external: usize,
    budget: usize,
}

impl Limits {
    fn new(verbose: bool) -> Self {
        if verbose {
            Limits {
                components: usize::MAX,
                internal: usize::MAX,
                external: usize::MAX,
                budget: usize::MAX,
            }
        } else {
            Limits {
                components: MAX_COMPONENTS,
                internal: MAX_INTERNAL,
                external: MAX_EXTERNAL,
                budget: BUDGET,
            }
        }
    }
}

/// One internal dependency at the summary's depth.
#[derive(Default)]
struct Dependency {
    imports: usize,
    declared: bool,
    other: BTreeMap<&'static str, usize>,
}

/// A rendered list with its heading.
struct Section {
    text: String,
    /// Entries listed, `omitted:` lines aside.
    listed: usize,
}

pub fn render(graph: &ArchitectureGraph, depth: usize, verbose: bool) -> String {
    render_with(graph, depth, Limits::new(verbose))
}

fn render_with(graph: &ArchitectureGraph, depth: usize, limits: Limits) -> String {
    let rolled = graph.rollup(depth);
    let internal: Vec<&Component> = rolled
        .components
        .values()
        .filter(|c| is_internal(c))
        .collect();

    let mut dependencies: BTreeMap<(&ComponentId, &ComponentId), Dependency> = BTreeMap::new();
    for edge in &rolled.edges {
        if !internal_id(&rolled, &edge.from) || !internal_id(&rolled, &edge.to) {
            continue;
        }
        let dep = dependencies.entry((&edge.from, &edge.to)).or_default();
        match edge.kind {
            EdgeKind::Import => dep.imports += edge.statements(),
            EdgeKind::Dependency => dep.declared = true,
            other => *dep.other.entry(other.as_str()).or_default() += edge.statements().max(1),
        }
    }
    let externals: Vec<&Component> = rolled
        .components
        .values()
        .filter(|c| c.kind == ComponentKind::External)
        .collect();

    // Distinct internal dependencies in each direction: the structural rank
    // that the lists and "most depended on" share.
    let mut dependents: BTreeMap<&ComponentId, usize> = BTreeMap::new();
    let mut uses: BTreeMap<&ComponentId, usize> = BTreeMap::new();
    for (from, to) in dependencies.keys() {
        *dependents.entry(*to).or_default() += 1;
        *uses.entry(*from).or_default() += 1;
    }

    let mut coverage_text = String::new();
    coverage(&mut coverage_text, graph, &rolled);
    let mut most = String::new();
    most_depended_on(&mut most, &rolled, internal.len(), &dependents, &uses);

    let tree = Tree::new(graph, &rolled, &internal, depth, &dependents, &uses);
    let ranked_dependencies = rank_dependencies(&rolled, &dependencies, &dependents);
    let ranked_externals = rank_externals(&rolled, &externals);
    let list = |i: usize, cap: usize| match i {
        0 => tree.render(&rolled, cap),
        1 => internal_dependencies(&rolled, &ranked_dependencies, cap),
        _ => external_dependencies(&rolled, &ranked_externals, cap),
    };
    let mut lists: Vec<Section> = [limits.components, limits.internal, limits.external]
        .into_iter()
        .enumerate()
        .map(|(i, cap)| list(i, cap))
        .collect();

    loop {
        let mut out = header(
            graph,
            depth,
            internal.len(),
            lists[0].listed,
            dependencies.len(),
            externals.len(),
        );
        out.push_str(&coverage_text);
        for section in &lists {
            out.push_str(&section.text);
        }
        out.push_str(&most);

        // Over the budget, the largest list still above its floor gives up
        // its lowest-ranked entry.
        let largest = (0..lists.len())
            .filter(|&i| lists[i].listed > MIN_LISTED)
            .max_by_key(|&i| (lists[i].text.len(), Reverse(i)));
        match largest {
            Some(i) if out.len() > limits.budget => lists[i] = list(i, lists[i].listed - 1),
            _ => return out,
        }
    }
}

fn header(
    graph: &ArchitectureGraph,
    depth: usize,
    at_depth: usize,
    listed: usize,
    internal_dependencies: usize,
    external_dependencies: usize,
) -> String {
    let mut out = String::new();
    let name = Path::new(&graph.meta.root)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| graph.meta.root.clone());
    let full_internal = graph.components.values().filter(|c| is_internal(c)).count();
    let _ = writeln!(out, "# archmap summary");
    let _ = writeln!(out, "root: {name}");
    let _ = writeln!(out, "depth: {depth}");
    if listed < at_depth {
        let _ = writeln!(
            out,
            "components: {listed} shown of {at_depth} at depth, {full_internal} in the full graph"
        );
    } else {
        let _ = writeln!(
            out,
            "components: {at_depth} shown, {full_internal} in the full graph"
        );
    }
    let _ = writeln!(out, "public symbols: {}", graph.symbols.len());
    let _ = writeln!(out, "internal dependencies: {internal_dependencies}");
    let _ = writeln!(out, "external dependencies: {external_dependencies}");
    let _ = writeln!(
        out,
        "source: manifests and import statements; nothing is inferred"
    );
    let _ = writeln!(
        out,
        "next: archmap query <component> --depth {depth}; archmap impact <component-or-file> --depth {depth}"
    );
    out
}

/// What the map leaves out, before the map: files no analyzer read, imports
/// without an edge, modules loaded by computed names, and coupling that no
/// analyzer reads. An agent can then tell an absent edge from an unseen one.
fn coverage(out: &mut String, graph: &ArchitectureGraph, rolled: &ArchitectureGraph) {
    let _ = writeln!(out, "\n## Coverage");

    // Language -> why imports have no edge -> the statements, so that
    // `from torch import nn, Tensor` counts once.
    type Statements<'a> = BTreeSet<(&'a str, Option<u32>)>;
    let mut without_edge: BTreeMap<&str, BTreeMap<UnmappedReason, Statements>> = BTreeMap::new();
    for import in &graph.unmapped_imports {
        if let Some(language) = graph
            .component(&import.from)
            .and_then(|c| c.language.as_deref())
        {
            without_edge
                .entry(language)
                .or_default()
                .entry(import.reason)
                .or_default()
                .insert((import.evidence.file.as_str(), import.evidence.line));
        }
    }
    let mut not_analyzed = Vec::new();
    for (language, c) in &graph.meta.coverage {
        match c.read {
            Some(read) => {
                let reasons = without_edge.get(language.as_str());
                let total: usize = reasons
                    .into_iter()
                    .flat_map(|r| r.values())
                    .map(BTreeSet::len)
                    .sum();
                let mut line = format!(
                    "{language}  files: {}  read: {read}  imports without an edge: {total}",
                    c.files
                );
                if let Some(reasons) = reasons {
                    let parts: Vec<String> = reasons
                        .iter()
                        .map(|(reason, s)| format!("{} {}", reason_label(*reason), s.len()))
                        .collect();
                    let _ = write!(line, " ({})", parts.join(", "));
                }
                let _ = writeln!(out, "{line}");
            }
            None => not_analyzed.push(format!("{language}: {}", c.files)),
        }
    }
    if not_analyzed.is_empty() {
        let _ = writeln!(out, "not analyzed: none");
    } else {
        let _ = writeln!(out, "not analyzed  {}", not_analyzed.join("  "));
    }
    let scripts: usize = graph.meta.coverage.values().map(|c| c.scripts).sum();
    if scripts > 0 {
        let _ = writeln!(
            out,
            "scripts: {scripts} (no import or export: what uses their declarations is not traced)"
        );
    }

    let mut importers: BTreeMap<&ComponentId, usize> = BTreeMap::new();
    for import in &rolled.dynamic_imports {
        *importers.entry(&import.from).or_default() += 1;
    }
    let mut line = format!("dynamic imports: {}", rolled.dynamic_imports.len());
    if !importers.is_empty() {
        let _ = write!(
            line,
            "  in: {}",
            top_counts(rolled, importers, MAX_DYNAMIC_IMPORTERS)
        );
    }
    let _ = writeln!(out, "{line}");
    let _ = writeln!(out, "{RUNTIME_COUPLING}");
}

/// The component tree at the summary's depth, ranked so that a cap keeps the
/// packages and then the busiest modules.
struct Tree<'a> {
    /// Components without a parent at this depth, in id order.
    roots: Vec<&'a Component>,
    children: BTreeMap<&'a ComponentId, Vec<&'a Component>>,
    parent: BTreeMap<&'a ComponentId, &'a ComponentId>,
    roots_by_rank: Vec<&'a Component>,
    modules_by_rank: Vec<&'a Component>,
    /// How many original components each kept component absorbed.
    folded: BTreeMap<ComponentId, usize>,
}

impl<'a> Tree<'a> {
    fn new(
        graph: &ArchitectureGraph,
        rolled: &'a ArchitectureGraph,
        internal: &[&'a Component],
        depth: usize,
        dependents: &BTreeMap<&ComponentId, usize>,
        uses: &BTreeMap<&ComponentId, usize>,
    ) -> Self {
        let mut folded: BTreeMap<ComponentId, usize> = BTreeMap::new();
        for id in graph.components.keys() {
            let ancestor = graph.ancestor_at(id, depth);
            if ancestor != *id {
                *folded.entry(ancestor).or_default() += 1;
            }
        }
        let mut children: BTreeMap<&ComponentId, Vec<&Component>> = BTreeMap::new();
        let mut parent = BTreeMap::new();
        let mut roots = Vec::new();
        for c in internal {
            match c
                .parent
                .as_ref()
                .filter(|p| rolled.components.contains_key(*p))
            {
                Some(p) => {
                    children.entry(p).or_default().push(*c);
                    parent.insert(&c.id, p);
                }
                None => roots.push(*c),
            }
        }

        let degree = |c: &Component| {
            dependents.get(&c.id).copied().unwrap_or(0) + uses.get(&c.id).copied().unwrap_or(0)
        };
        let by_rank = |mut list: Vec<&'a Component>| {
            list.sort_by(|a, b| degree(b).cmp(&degree(a)).then_with(|| a.id.cmp(&b.id)));
            list
        };
        let roots_by_rank = by_rank(roots.clone());
        let modules_by_rank = by_rank(
            internal
                .iter()
                .copied()
                .filter(|c| parent.contains_key(&c.id))
                .collect(),
        );
        Tree {
            roots,
            children,
            parent,
            roots_by_rank,
            modules_by_rank,
            folded,
        }
    }

    /// Packages by rank up to `cap`, then modules by rank, each taken only
    /// when it fits together with its ancestors not yet shown.
    fn select(&self, cap: usize) -> BTreeSet<&'a ComponentId> {
        let mut shown: BTreeSet<&ComponentId> =
            self.roots_by_rank.iter().take(cap).map(|c| &c.id).collect();
        let mut room = cap.saturating_sub(shown.len());
        for c in &self.modules_by_rank {
            if room == 0 {
                break;
            }
            let missing: Vec<&ComponentId> = self
                .ancestry(&c.id)
                .take_while(|id| !shown.contains(id))
                .collect();
            if missing.len() <= room {
                room -= missing.len();
                shown.extend(missing);
            }
        }
        shown
    }

    /// `id` and its ancestors, innermost first.
    fn ancestry(&self, id: &'a ComponentId) -> impl Iterator<Item = &'a ComponentId> + '_ {
        std::iter::successors(Some(id), |id| self.parent.get(id).copied())
    }

    fn render(&self, rolled: &ArchitectureGraph, cap: usize) -> Section {
        let shown = self.select(cap);
        let mut text = String::from("\n## Components\n");
        if self.roots.is_empty() {
            text.push_str("none\n");
        }
        for root in &self.roots {
            if shown.contains(&root.id) {
                self.walk(&mut text, root, 0, rolled, &shown);
            }
        }

        let hidden_packages: Vec<&str> = self
            .roots_by_rank
            .iter()
            .filter(|c| !shown.contains(&c.id))
            .map(|c| c.name.as_str())
            .collect();
        if !hidden_packages.is_empty() {
            let _ = writeln!(
                text,
                "omitted: {}  names: {}  next: archmap query <package>",
                count(hidden_packages.len(), "package", "packages"),
                names(&hidden_packages, MAX_OMITTED_NAMES)
            );
        }
        // A hidden module is counted under its nearest shown ancestor, the
        // component whose query lists it.
        let mut hidden: BTreeMap<&ComponentId, usize> = BTreeMap::new();
        for c in &self.modules_by_rank {
            if !shown.contains(&c.id) {
                let ancestors: Vec<&ComponentId> = self.ancestry(&c.id).skip(1).collect();
                let anchor = ancestors
                    .iter()
                    .find(|id| shown.contains(*id))
                    .or(ancestors.last())
                    .copied()
                    .unwrap_or(&c.id);
                *hidden.entry(anchor).or_default() += 1;
            }
        }
        if !hidden.is_empty() {
            let n = hidden.values().sum();
            let _ = writeln!(
                text,
                "omitted: {}  in: {}  next: archmap query <component>",
                count(n, "module", "modules"),
                top_counts(rolled, hidden, MAX_OMITTED_NAMES)
            );
        }
        Section {
            text,
            listed: shown.len(),
        }
    }

    fn walk(
        &self,
        out: &mut String,
        c: &Component,
        level: usize,
        rolled: &ArchitectureGraph,
        shown: &BTreeSet<&ComponentId>,
    ) {
        let mut line = format!("{}{}", "  ".repeat(level), c.name);
        if c.kind == ComponentKind::Package {
            line.push_str("  package");
            if let Some(language) = &c.language {
                let _ = write!(line, "  language: {language}");
            }
        }
        if let Some(path) = &c.path {
            let _ = write!(line, "  path: {path}");
        }
        let symbols = rolled.symbols_of(&c.id).count();
        if symbols > 0 {
            let _ = write!(line, "  symbols: {symbols}");
        }
        if let Some(n) = self.folded.get(&c.id) {
            let _ = write!(line, "  folded: {n}");
        }
        let _ = writeln!(out, "{line}");
        for child in self.children.get(&c.id).into_iter().flatten() {
            if shown.contains(&child.id) {
                self.walk(out, child, level + 1, rolled, shown);
            }
        }
    }
}

/// Internal dependencies in the order a cap keeps them: those between
/// packages first, then those into components more others depend on, then
/// those with more import statements.
fn rank_dependencies<'a>(
    rolled: &ArchitectureGraph,
    dependencies: &'a BTreeMap<(&'a ComponentId, &'a ComponentId), Dependency>,
    dependents: &BTreeMap<&ComponentId, usize>,
) -> Vec<(&'a ComponentId, &'a ComponentId, &'a Dependency)> {
    let within =
        |from: &ComponentId, to: &ComponentId| package_of(rolled, from) == package_of(rolled, to);
    let dependents = |id: &ComponentId| dependents.get(id).copied().unwrap_or(0);
    // The keys walk the containment tree, so each is computed once.
    let mut ranked: Vec<_> = dependencies
        .iter()
        .map(|((from, to), dep)| {
            let key = (
                within(from, to),
                std::cmp::Reverse(dependents(to)),
                std::cmp::Reverse(dep.imports),
            );
            (key, *from, *to, dep)
        })
        .collect();
    ranked.sort_by(|a, b| (&a.0, a.1, a.2).cmp(&(&b.0, b.1, b.2)));
    ranked
        .into_iter()
        .map(|(_, from, to, dep)| (from, to, dep))
        .collect()
}

/// The outermost component containing `id` at the summary's depth.
fn package_of<'a>(rolled: &'a ArchitectureGraph, mut id: &'a ComponentId) -> &'a ComponentId {
    while let Some(parent) = rolled
        .component(id)
        .and_then(|c| c.parent.as_ref())
        .filter(|p| rolled.components.contains_key(*p))
    {
        id = parent;
    }
    id
}

fn internal_dependencies(
    rolled: &ArchitectureGraph,
    ranked: &[(&ComponentId, &ComponentId, &Dependency)],
    cap: usize,
) -> Section {
    let mut text = String::from("\n## Internal dependencies\n");
    if ranked.is_empty() {
        text.push_str("none\n");
        return Section { text, listed: 0 };
    }
    let (kept, omitted) = ranked.split_at(cap.min(ranked.len()));
    let mut lines: Vec<(&str, usize, &str, &Dependency)> = kept
        .iter()
        .map(|(from, to, dep)| {
            (
                name_of(rolled, from),
                dep.imports,
                name_of(rolled, to),
                *dep,
            )
        })
        .collect();
    lines.sort_by(|a, b| {
        a.0.cmp(b.0)
            .then_with(|| b.1.cmp(&a.1))
            .then_with(|| a.2.cmp(b.2))
    });
    for (from, imports, to, dep) in lines {
        let mut line = format!("{from} -> {to}");
        if imports > 0 {
            let _ = write!(line, "  imports: {imports}");
        }
        if dep.declared {
            line.push_str("  declared: yes");
        }
        for (kind, n) in &dep.other {
            let _ = write!(line, "  {kind}: {n}");
        }
        let _ = writeln!(text, "{line}");
    }
    if !omitted.is_empty() {
        let mut sources: BTreeMap<&ComponentId, usize> = BTreeMap::new();
        for (from, _, _) in omitted {
            *sources.entry(*from).or_default() += 1;
        }
        let _ = writeln!(
            text,
            "omitted: {}  from: {}  next: archmap query <component>",
            count(omitted.len(), "dependency", "dependencies"),
            top_counts(rolled, sources, MAX_OMITTED_NAMES)
        );
    }
    Section {
        text,
        listed: kept.len(),
    }
}

/// An external dependency with the manifests that declare it and the
/// components that import it.
struct External<'a> {
    component: &'a Component,
    declared_in: BTreeSet<&'a str>,
    importers: Vec<(&'a ComponentId, usize)>,
}

/// External dependencies in the order a cap keeps them: imported by more
/// components first, then by more statements.
fn rank_externals<'a>(
    rolled: &'a ArchitectureGraph,
    externals: &[&'a Component],
) -> Vec<External<'a>> {
    let mut ranked: Vec<External> = externals
        .iter()
        .map(|ext| {
            let mut declared_in = BTreeSet::new();
            let mut importers = Vec::new();
            for edge in rolled.incoming(&ext.id) {
                match edge.kind {
                    EdgeKind::Dependency => {
                        declared_in.extend(edge.evidence.iter().map(|e| e.file.as_str()));
                    }
                    EdgeKind::Import => importers.push((&edge.from, edge.statements())),
                    _ => {}
                }
            }
            External {
                component: ext,
                declared_in,
                importers,
            }
        })
        .collect();
    let statements = |e: &External| e.importers.iter().map(|(_, n)| n).sum::<usize>();
    ranked.sort_by(|a, b| {
        b.importers
            .len()
            .cmp(&a.importers.len())
            .then_with(|| statements(b).cmp(&statements(a)))
            .then_with(|| a.component.id.cmp(&b.component.id))
    });
    ranked
}

fn external_dependencies(rolled: &ArchitectureGraph, ranked: &[External], cap: usize) -> Section {
    let mut text = String::from("\n## External dependencies\n");
    if ranked.is_empty() {
        text.push_str("none\n");
        return Section { text, listed: 0 };
    }
    let (kept, omitted) = ranked.split_at(cap.min(ranked.len()));
    let mut by_id: Vec<&External> = kept.iter().collect();
    by_id.sort_by(|a, b| a.component.id.cmp(&b.component.id));
    for ext in by_id {
        let mut line = ext.component.name.clone();
        if !ext.declared_in.is_empty() {
            let files: Vec<&str> = ext.declared_in.iter().copied().collect();
            let _ = write!(line, "  declared: {}", files.join(", "));
        }
        if ext.importers.is_empty() {
            line.push_str("  importers: none resolved");
        } else {
            let _ = write!(
                line,
                "  importers: {}  top: {}",
                ext.importers.len(),
                top_counts(rolled, ext.importers.iter().copied(), MAX_IMPORTERS)
            );
        }
        let _ = writeln!(text, "{line}");
    }
    if !omitted.is_empty() {
        let hidden: Vec<&str> = omitted.iter().map(|e| e.component.name.as_str()).collect();
        let _ = writeln!(
            text,
            "omitted: {}  names: {}  next: archmap query <name>",
            count(
                omitted.len(),
                "external dependency",
                "external dependencies"
            ),
            names(&hidden, MAX_OMITTED_NAMES)
        );
    }
    Section {
        text,
        listed: kept.len(),
    }
}

fn most_depended_on(
    out: &mut String,
    rolled: &ArchitectureGraph,
    total: usize,
    dependents: &BTreeMap<&ComponentId, usize>,
    uses: &BTreeMap<&ComponentId, usize>,
) {
    let _ = writeln!(out, "\n## Most depended on");
    let mut ranked: Vec<(&ComponentId, usize, usize)> = dependents
        .iter()
        .map(|(id, n)| (*id, *n, uses.get(id).copied().unwrap_or(0)))
        .collect();
    ranked.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| b.2.cmp(&a.2))
            .then_with(|| a.0.cmp(b.0))
    });
    if ranked.is_empty() {
        let _ = writeln!(out, "none");
        return;
    }
    for (id, n_in, n_out) in ranked.iter().take(TOP_DEPENDED_ON) {
        // Competition ranking: ties share a rank.
        let rank = 1 + ranked.iter().filter(|(_, other, _)| other > n_in).count();
        let _ = writeln!(
            out,
            "{}  dependents: {n_in}  dependencies: {n_out}  rank: {rank}/{total}",
            name_of(rolled, id)
        );
    }
}

/// `a 3, b 2, +4 more`: the largest counts first, ties by id, at most `max`
/// of them named.
fn top_counts<'a>(
    rolled: &ArchitectureGraph,
    counts: impl IntoIterator<Item = (&'a ComponentId, usize)>,
    max: usize,
) -> String {
    let mut counts: Vec<(&ComponentId, usize)> = counts.into_iter().collect();
    counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    let mut top: Vec<String> = counts
        .iter()
        .take(max)
        .map(|(id, n)| format!("{} {n}", name_of(rolled, id)))
        .collect();
    if counts.len() > max {
        top.push(format!("+{} more", counts.len() - max));
    }
    top.join(", ")
}

/// `a, b, +3 more`: at most `max` names, in the given order.
fn names(names: &[&str], max: usize) -> String {
    let mut shown: Vec<String> = names.iter().take(max).map(|n| n.to_string()).collect();
    if names.len() > max {
        shown.push(format!("+{} more", names.len() - max));
    }
    shown.join(", ")
}

/// `1 module`, `2 modules`.
fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn is_internal(c: &Component) -> bool {
    c.kind != ComponentKind::External
}

fn internal_id(graph: &ArchitectureGraph, id: &ComponentId) -> bool {
    graph.component(id).is_some_and(is_internal)
}

fn name_of<'a>(graph: &'a ArchitectureGraph, id: &'a ComponentId) -> &'a str {
    graph.component(id).map_or(id.as_str(), |c| c.name.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use archmap_core::{Edge, Evidence};

    fn package(id: &str) -> Component {
        let mut c = Component::new(id, id, ComponentKind::Package);
        c.path = Some(id.to_owned());
        c
    }

    fn module(id: &str, parent: &str) -> Component {
        let mut c = Component::new(id, id, ComponentKind::Module);
        c.parent = Some(ComponentId::new(parent));
        c
    }

    fn external(name: &str) -> Component {
        Component::new(format!("ext:cargo:{name}"), name, ComponentKind::External)
    }

    /// An import edge backed by `statements` distinct statements.
    fn import(from: &str, to: &str, statements: u32) -> Edge {
        (1..=statements).fold(Edge::new(from, to, EdgeKind::Import), |edge, line| {
            edge.with_evidence(Evidence::new(from).at_line(line))
        })
    }

    fn graph(components: Vec<Component>, edges: Vec<Edge>) -> ArchitectureGraph {
        let mut graph = ArchitectureGraph::default();
        graph.meta.root = "repo".to_owned();
        for c in components {
            graph.add_component(c);
        }
        graph.add_edges(edges);
        graph
    }

    fn limits(components: usize, internal: usize, external: usize) -> Limits {
        Limits {
            components,
            internal,
            external,
            ..Limits::new(false)
        }
    }

    /// The lines under `## <heading>`, without the blank line that ends them.
    fn section<'a>(out: &'a str, heading: &str) -> &'a str {
        let marker = format!("## {heading}\n");
        let rest = &out[out.find(&marker).unwrap() + marker.len()..];
        rest.find("\n## ").map_or(rest, |end| &rest[..end])
    }

    /// Entries of a section, leaving out `omitted:` lines.
    fn listed(out: &str, heading: &str) -> usize {
        section(out, heading)
            .lines()
            .filter(|l| !l.starts_with("omitted: "))
            .count()
    }

    /// Two packages and four modules. Dependents plus dependencies:
    /// p::a 3, q::d 2, p::b 1, p::c 0.
    fn two_packages() -> ArchitectureGraph {
        graph(
            vec![
                package("p"),
                package("q"),
                module("p::a", "p"),
                module("p::b", "p"),
                module("p::c", "p"),
                module("q::d", "q"),
            ],
            vec![
                import("p::a", "q::d", 1),
                import("p::a", "p::b", 1),
                import("p::a", "q", 1),
                import("q::d", "p", 1),
            ],
        )
    }

    #[test]
    fn packages_come_first_and_modules_fill_the_cap_by_rank() {
        let out = render_with(&two_packages(), 2, limits(4, 30, 20));
        assert_eq!(
            section(&out, "Components"),
            "p  package  path: p\n  p::a\nq  package  path: q\n  q::d\n\
             omitted: 2 modules  in: p 2  next: archmap query <component>\n"
        );
        assert!(
            out.contains("\ncomponents: 4 shown of 6 at depth, 6 in the full graph\n"),
            "{out}"
        );
    }

    #[test]
    fn verbose_lists_every_component() {
        let out = render_with(&two_packages(), 2, Limits::new(true));
        assert_eq!(
            section(&out, "Components"),
            "p  package  path: p\n  p::a\n  p::b\n  p::c\nq  package  path: q\n  q::d\n"
        );
        assert!(out.contains("\ncomponents: 6 shown, 6 in the full graph\n"));
        assert!(!out.contains("omitted: "), "{out}");
    }

    /// `p::x::deep` ranks first but needs its parent `p::x` shown with it.
    fn nested() -> ArchitectureGraph {
        graph(
            vec![
                package("p"),
                module("p::x", "p"),
                module("p::x::deep", "p::x"),
                module("p::y", "p"),
            ],
            vec![
                import("p::x::deep", "p::y", 1),
                import("p::x::deep", "p", 1),
            ],
        )
    }

    #[test]
    fn a_module_that_cannot_fit_with_its_ancestors_is_skipped() {
        let out = render_with(&nested(), 2, limits(2, 30, 20));
        assert_eq!(
            section(&out, "Components"),
            "p  package  path: p\n  p::y\n\
             omitted: 2 modules  in: p 2  next: archmap query <component>\n"
        );

        let out = render_with(&nested(), 2, limits(3, 30, 20));
        assert_eq!(
            section(&out, "Components"),
            "p  package  path: p\n  p::x\n    p::x::deep\n\
             omitted: 1 module  in: p 1  next: archmap query <component>\n"
        );
    }

    #[test]
    fn packages_are_ranked_too_when_they_exceed_the_cap() {
        let components = ["a", "b", "c", "d", "e"].map(package).into_iter();
        let graph = graph(
            components.chain([module("a::m", "a")]).collect(),
            vec![
                import("b", "a", 1),
                import("c", "a", 1),
                import("d", "a", 1),
                import("b", "c", 1),
            ],
        );
        let out = render_with(&graph, 2, limits(3, 30, 20));
        assert_eq!(
            section(&out, "Components"),
            "a  package  path: a\nb  package  path: b\nc  package  path: c\n\
             omitted: 2 packages  names: d, e  next: archmap query <package>\n\
             omitted: 1 module  in: a 1  next: archmap query <component>\n"
        );
    }

    #[test]
    fn dependencies_across_packages_come_first_then_busy_targets_then_imports() {
        let graph = graph(
            vec![
                package("p"),
                package("q"),
                package("r"),
                module("p::a", "p"),
                module("p::b", "p"),
            ],
            vec![
                import("p::a", "p::b", 5),
                import("p::a", "q", 1),
                import("p::b", "q", 1),
                import("q", "p::b", 2),
                import("p::b", "p", 1),
                import("p::a", "r", 3),
            ],
        );
        let out = render_with(&graph, 2, limits(30, 3, 20));
        assert_eq!(
            section(&out, "Internal dependencies"),
            "p::a -> q  imports: 1\np::b -> q  imports: 1\nq -> p::b  imports: 2\n\
             omitted: 3 dependencies  from: p::a 2, p::b 1  next: archmap query <component>\n"
        );
    }

    #[test]
    fn external_dependencies_used_by_the_most_components_come_first() {
        let graph = graph(
            vec![
                package("p"),
                module("p::a", "p"),
                module("p::b", "p"),
                module("p::c", "p"),
                external("x1"),
                external("x2"),
                external("x3"),
                external("x4"),
                external("x5"),
            ],
            vec![
                import("p::a", "ext:cargo:x1", 1),
                import("p::b", "ext:cargo:x1", 1),
                import("p::c", "ext:cargo:x1", 1),
                import("p::a", "ext:cargo:x2", 2),
                import("p::b", "ext:cargo:x2", 1),
                import("p::a", "ext:cargo:x3", 5),
                import("p::b", "ext:cargo:x5", 1),
                Edge::new("p", "ext:cargo:x4", EdgeKind::Dependency)
                    .with_evidence(Evidence::new("Cargo.toml").at_line(3)),
            ],
        );
        let out = render_with(&graph, 2, limits(30, 30, 2));
        assert_eq!(
            section(&out, "External dependencies"),
            "x1  importers: 3  top: p::a 1, p::b 1, p::c 1\n\
             x2  importers: 2  top: p::a 2, p::b 1\n\
             omitted: 3 external dependencies  names: x3, x5, x4  next: archmap query <name>\n"
        );
    }

    /// One package of long-named modules, each importing the next three.
    fn long_named(modules: usize) -> ArchitectureGraph {
        let name =
            |i: usize| format!("p::a_module_with_a_deliberately_long_name_for_budget_tests_{i:02}");
        let mut components = vec![package("p")];
        for i in 0..modules {
            let mut m = module(&name(i), "p");
            m.path = Some(format!(
                "src/a/deeply/nested/directory/that/makes/every/path/long/{i:02}.rs"
            ));
            components.push(m);
        }
        let edges = (0..modules)
            .flat_map(|i| (1..=3).map(move |d| (i, (i + d) % modules)))
            .map(|(i, j)| import(&name(i), &name(j), 1))
            .collect();
        graph(components, edges)
    }

    #[test]
    fn the_budget_shrinks_the_largest_lists_first() {
        let graph = long_named(40);
        let capped_only = render_with(
            &graph,
            2,
            Limits {
                budget: usize::MAX,
                ..Limits::new(false)
            },
        );
        assert!(capped_only.len() > BUDGET, "the test needs a long summary");

        let out = render_with(&graph, 2, Limits::new(false));
        assert!(out.len() <= BUDGET, "{} bytes:\n{out}", out.len());
        for heading in ["Components", "Internal dependencies"] {
            let n = listed(&out, heading);
            assert!((MIN_LISTED..MAX_COMPONENTS).contains(&n), "{heading}: {n}");
        }
        assert_eq!(listed(&out, "Most depended on"), TOP_DEPENDED_ON);
        assert!(out.contains("\n## Coverage\n") && out.contains(RUNTIME_COUPLING));
    }

    #[test]
    fn lists_keep_their_floor_even_over_the_budget() {
        let out = render_with(
            &long_named(40),
            2,
            Limits {
                budget: 1000,
                ..Limits::new(false)
            },
        );
        assert!(out.len() > 1000);
        assert_eq!(listed(&out, "Components"), MIN_LISTED);
        assert_eq!(listed(&out, "Internal dependencies"), MIN_LISTED);
        assert!(section(&out, "Components").contains("\nomitted: 31 modules  in: p 31"));
    }

    #[test]
    fn a_large_graph_stays_within_the_budget() {
        let name = |p: usize, m: usize| format!("pkg{p}::module{m:02}");
        let mut components: Vec<Component> =
            (0..25).map(|i| external(&format!("lib{i}"))).collect();
        let mut edges = Vec::new();
        for p in 0..10 {
            components.push(package(&format!("pkg{p}")));
            for m in 0..49 {
                components.push(module(&name(p, m), &format!("pkg{p}")));
                edges.push(import(&name(p, m), &name(p, (m + 1) % 49), 1));
                edges.push(import(&name(p, m), &name((p + 1) % 10, m), 2));
                edges.push(import(&name(p, m), &format!("ext:cargo:lib{}", m % 25), 1));
            }
        }
        let out = render_with(&graph(components, edges), 2, Limits::new(false));
        assert!(out.len() <= BUDGET, "{} bytes:\n{out}", out.len());
        assert!(
            out.contains(" shown of 500 at depth, 500 in the full graph\n"),
            "{out}"
        );
        for heading in [
            "Components",
            "Internal dependencies",
            "External dependencies",
        ] {
            assert!(section(&out, heading).contains("\nomitted: "), "{heading}");
        }
    }
}
