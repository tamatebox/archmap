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
    ArchitectureGraph, Component, ComponentId, ComponentKind, EdgeKind, Evidence, UnmappedReason,
};

use crate::pairs::{Counted, Pairs};

use crate::query_text::{display, reason_label};

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

/// How many manifests an external dependency's `declared:` names before
/// counting the rest: a monorepo can declare one package in dozens.
const MAX_DECLARATIONS: usize = 3;

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
    /// Import statements in production code.
    imports: usize,
    /// Import statements in test code.
    tests: usize,
    declared: bool,
    other: BTreeMap<&'static str, usize>,
}

impl Dependency {
    /// Production code makes it, or a manifest declares it.
    fn in_production(&self) -> bool {
        self.imports > 0 || self.declared || !self.other.is_empty()
    }
}

/// A rendered list with its heading.
struct Section {
    text: String,
    /// Entries listed, `omitted:` lines aside.
    listed: usize,
}

/// `work`: the work snapshot's line for Coverage, when there is one.
pub fn render(
    graph: &ArchitectureGraph,
    root: &Path,
    depth: usize,
    verbose: bool,
    work: Option<&str>,
) -> String {
    render_with(graph, &root_name(root), depth, Limits::new(verbose), work)
}

/// The name `summary` gives the root: its directory's, or the path as it
/// is for a root without one (`/`).
fn root_name(root: &Path) -> String {
    root.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.display().to_string())
}

fn render_with(
    graph: &ArchitectureGraph,
    root: &str,
    depth: usize,
    limits: Limits,
    work: Option<&str>,
) -> String {
    let rolled = graph.rollup(depth);
    let internal: Vec<&Component> = rolled
        .components
        .values()
        .filter(|c| is_internal(c))
        .collect();

    let mut dependencies: BTreeMap<(&ComponentId, &ComponentId), Dependency> = BTreeMap::new();
    // How each statement counts for its pair, as `query` counts it: entry
    // files into their own submodules and imports that only pass through a
    // barrel count for no pair here.
    let pairs = Pairs::new(graph);
    let mut entry_statements: BTreeSet<(&str, Option<u32>)> = BTreeSet::new();
    for edge in &rolled.edges {
        if !internal_id(&rolled, &edge.from) || !internal_id(&rolled, &edge.to) {
            continue;
        }
        if edge.kind != EdgeKind::Import {
            let dep = dependencies.entry((&edge.from, &edge.to)).or_default();
            match edge.kind {
                EdgeKind::Dependency => dep.declared = true,
                other => *dep.other.entry(other.as_str()).or_default() += edge.statements().max(1),
            }
            continue;
        }
        let pair = pairs.pair(&rolled, &edge.from, &edge.to);
        let mut statements: BTreeMap<(&str, Option<u32>), Counted> = BTreeMap::new();
        for e in &edge.evidence {
            let counted = pair.counted(e);
            statements
                .entry((e.file.as_str(), e.line))
                .and_modify(|c| *c = (*c).min(counted))
                .or_insert(counted);
        }
        let count = |kind: Counted| statements.values().filter(|c| **c == kind).count();
        let (production, tests) = (count(Counted::Production), count(Counted::Test));
        entry_statements.extend(
            statements
                .iter()
                .filter(|(_, c)| **c == Counted::Entry)
                .map(|(at, _)| *at),
        );
        if production + tests == 0 && !edge.evidence.is_empty() {
            continue;
        }
        let dep = dependencies.entry((&edge.from, &edge.to)).or_default();
        dep.imports += production;
        dep.tests += tests;
    }
    let externals: Vec<&Component> = rolled
        .components
        .values()
        .filter(|c| c.kind == ComponentKind::External)
        .collect();

    // Distinct internal dependencies of production code in each direction:
    // the structural rank that the lists and "most depended on" share.
    let mut dependents: BTreeMap<&ComponentId, usize> = BTreeMap::new();
    let mut uses: BTreeMap<&ComponentId, usize> = BTreeMap::new();
    for ((from, to), _) in dependencies.iter().filter(|(_, d)| d.in_production()) {
        *dependents.entry(*to).or_default() += 1;
        *uses.entry(*from).or_default() += 1;
    }

    let mut coverage_text = String::new();
    coverage(&mut coverage_text, graph, &rolled, work);
    let mut most = String::new();
    most_depended_on(&mut most, &rolled, internal.len(), &dependents, &uses);

    let tree = Tree::new(graph, &rolled, &internal, depth, &dependents, &uses);
    let ranked_dependencies = rank_dependencies(&rolled, &dependencies);
    let ranked_externals = rank_externals(&rolled, &externals);
    let list = |i: usize, cap: usize| match i {
        0 => tree.render(&rolled, cap),
        1 => internal_dependencies(&rolled, &ranked_dependencies, entry_statements.len(), cap),
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
            root,
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
    root: &str,
    depth: usize,
    at_depth: usize,
    listed: usize,
    internal_dependencies: usize,
    external_dependencies: usize,
) -> String {
    let mut out = String::new();
    let full_internal = graph.components.values().filter(|c| is_internal(c)).count();
    let _ = writeln!(out, "# archmap summary");
    let _ = writeln!(out, "root: {root}");
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
        "next: query <component> at depth {depth}; impact <component-or-file> at depth {depth}"
    );
    out
}

/// What the map leaves out, before the map: files no analyzer read, imports
/// without an edge, modules loaded by computed names, and coupling that no
/// analyzer reads. An agent can then tell an absent edge from an unseen one.
fn coverage(
    out: &mut String,
    graph: &ArchitectureGraph,
    rolled: &ArchitectureGraph,
    work: Option<&str>,
) {
    let _ = writeln!(out, "\n## Coverage");

    // Language -> why imports have no edge -> the statements, so that
    // `from torch import nn, Tensor` counts once, and whether each is test
    // code.
    type Statements<'a> = BTreeSet<(&'a str, Option<u32>, bool)>;
    let mut without_edge: BTreeMap<&str, BTreeMap<UnmappedReason, Statements>> = BTreeMap::new();
    for import in &graph.unmapped_imports {
        // the language of the importing file, as Coverage counts files: a
        // package's own `eslint.config.js` is JavaScript in a TypeScript
        // package
        let file = archmap_scan::language_of(std::path::Path::new(&import.evidence.file));
        if let Some(language) = file.or_else(|| {
            graph
                .component(&import.from)
                .and_then(|c| c.language.as_deref())
        }) {
            without_edge
                .entry(language)
                .or_default()
                .entry(import.reason)
                .or_default()
                .insert((
                    import.evidence.file.as_str(),
                    import.evidence.line,
                    import.evidence.test,
                ));
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
                        .map(|(reason, s)| {
                            let tests = s.iter().filter(|(.., test)| *test).count();
                            let mut part = format!("{} {}", reason_label(*reason), s.len());
                            if tests > 0 {
                                let _ = write!(part, " ({tests} in tests)");
                            }
                            part
                        })
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
    // macro calls whose arguments were not read, by the component, as rolled
    // up, that makes them
    if !graph.unread_macros.is_empty() {
        let mut callers: BTreeMap<&ComponentId, usize> = BTreeMap::new();
        for call in &graph.unread_macros {
            let shown = graph
                .containment_path(&call.from)
                .iter()
                .rev()
                .find_map(|id| rolled.components.get_key_value(id).map(|(id, _)| id));
            if let Some(id) = shown {
                *callers.entry(id).or_default() += 1;
            }
        }
        let _ = writeln!(
            out,
            "macro calls not read: {}  in: {}",
            graph.unread_macros.len(),
            top_counts(rolled, callers, MAX_DYNAMIC_IMPORTERS)
        );
    }
    if let Some(work) = work {
        let _ = writeln!(out, "work: {work}");
    }
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
                "omitted: {}  names: {}  next: query <package>",
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
                "omitted: {}  in: {}  next: query <component>",
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
/// Dependencies in the order a cap keeps them: those between packages
/// first, then those with more import statements in production code, then
/// more in tests. Statements, not dependents, so that the heavy flows show
/// rather than one-statement lines into a popular target.
fn rank_dependencies<'a>(
    rolled: &ArchitectureGraph,
    dependencies: &'a BTreeMap<(&'a ComponentId, &'a ComponentId), Dependency>,
) -> Vec<(&'a ComponentId, &'a ComponentId, &'a Dependency)> {
    let within =
        |from: &ComponentId, to: &ComponentId| package_of(rolled, from) == package_of(rolled, to);
    // The keys walk the containment tree, so each is computed once.
    let mut ranked: Vec<_> = dependencies
        .iter()
        .map(|((from, to), dep)| {
            let key = (
                within(from, to),
                std::cmp::Reverse(dep.imports),
                std::cmp::Reverse(dep.tests),
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

/// Distinct statements among `evidence`, in production code and in test
/// code. Test code is a property of a file, so a statement is one or the
/// other.
fn statements(evidence: &[&Evidence]) -> (usize, usize) {
    let (tests, production): (BTreeSet<_>, BTreeSet<_>) = evidence
        .iter()
        .map(|e| (e.test, e.file.as_str(), e.line))
        .partition(|(test, ..)| *test);
    (production.len(), tests.len())
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
    entry_statements: usize,
    cap: usize,
) -> Section {
    let mut text = String::from("\n## Internal dependencies\n");
    let not_listed = |text: &mut String| {
        if entry_statements > 0 {
            let _ = writeln!(
                text,
                "not listed: {} of entry files into their own component's submodules",
                count(entry_statements, "statement", "statements")
            );
        }
    };
    if ranked.is_empty() {
        text.push_str("none\n");
        not_listed(&mut text);
        return Section { text, listed: 0 };
    }
    let (kept, omitted) = ranked.split_at(cap.min(ranked.len()));
    let mut lines: Vec<(&str, usize, &str, &Dependency)> = kept
        .iter()
        .map(|(from, to, dep)| {
            (
                display(rolled, from),
                dep.imports,
                display(rolled, to),
                *dep,
            )
        })
        .collect();
    lines.sort_by(|a, b| {
        a.0.cmp(b.0)
            .then_with(|| b.1.cmp(&a.1))
            .then_with(|| b.3.tests.cmp(&a.3.tests))
            .then_with(|| a.2.cmp(b.2))
    });
    for (from, imports, to, dep) in lines {
        let mut line = format!("{from} -> {to}");
        if imports > 0 {
            let _ = write!(line, "  imports: {imports}");
        }
        if dep.tests > 0 {
            let _ = write!(line, "  tests: {}", dep.tests);
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
            "omitted: {}  from: {}  next: query <component>",
            count(omitted.len(), "dependency", "dependencies"),
            top_counts(rolled, sources, MAX_OMITTED_NAMES)
        );
    }
    not_listed(&mut text);
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
    /// Components whose production code imports it, with their statements.
    importers: Vec<(&'a ComponentId, usize)>,
    /// Components that import it only in test code.
    tests: usize,
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
            let mut tests = 0;
            for edge in rolled.incoming(&ext.id) {
                match edge.kind {
                    EdgeKind::Dependency => {
                        declared_in.extend(edge.evidence.iter().map(|e| e.file.as_str()));
                    }
                    EdgeKind::Import => {
                        let evidence: Vec<&Evidence> = edge.evidence.iter().collect();
                        match statements(&evidence) {
                            (0, n) if n > 0 => tests += 1,
                            (production, _) => importers.push((&edge.from, production)),
                        }
                    }
                    _ => {}
                }
            }
            External {
                component: ext,
                declared_in,
                importers,
                tests,
            }
        })
        .collect();
    let statements = |e: &External| e.importers.iter().map(|(_, n)| n).sum::<usize>();
    ranked.sort_by(|a, b| {
        b.importers
            .len()
            .cmp(&a.importers.len())
            .then_with(|| statements(b).cmp(&statements(a)))
            .then_with(|| b.tests.cmp(&a.tests))
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
            let _ = write!(line, "  declared: {}", names(&files, MAX_DECLARATIONS));
        }
        if ext.importers.is_empty() && ext.tests == 0 {
            line.push_str("  importers: none resolved");
        } else if ext.importers.is_empty() {
            line.push_str("  importers: 0");
        } else {
            let _ = write!(
                line,
                "  importers: {}  top: {}",
                ext.importers.len(),
                top_counts(rolled, ext.importers.iter().copied(), MAX_IMPORTERS)
            );
        }
        if ext.tests > 0 {
            let _ = write!(line, "  test importers: {}", ext.tests);
        }
        let _ = writeln!(text, "{line}");
    }
    if !omitted.is_empty() {
        let hidden: Vec<&str> = omitted.iter().map(|e| e.component.name.as_str()).collect();
        let _ = writeln!(
            text,
            "omitted: {}  names: {}  next: query <name>",
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
            display(rolled, id)
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
        .map(|(id, n)| format!("{} {n}", display(rolled, id)))
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

#[cfg(test)]
mod tests {
    use super::*;
    use archmap_core::{Edge, Symbol, SymbolId};

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

    /// An import edge backed by `statements` statements in a test file.
    fn test_import(from: &str, to: &str, statements: u32) -> Edge {
        (1..=statements).fold(Edge::new(from, to, EdgeKind::Import), |edge, line| {
            edge.with_evidence(
                Evidence::new(format!("{from}.test"))
                    .at_line(line)
                    .in_test(true),
            )
        })
    }

    fn graph(components: Vec<Component>, edges: Vec<Edge>) -> ArchitectureGraph {
        let mut graph = ArchitectureGraph::default();
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
        let out = render_with(&two_packages(), "repo", 2, limits(4, 30, 20), None);
        assert_eq!(
            section(&out, "Components"),
            "p  package  path: p\n  p::a\nq  package  path: q\n  q::d\n\
             omitted: 2 modules  in: p 2  next: query <component>\n"
        );
        assert!(
            out.contains("\ncomponents: 4 shown of 6 at depth, 6 in the full graph\n"),
            "{out}"
        );
    }

    #[test]
    fn verbose_lists_every_component() {
        let out = render_with(&two_packages(), "repo", 2, Limits::new(true), None);
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
        let out = render_with(&nested(), "repo", 2, limits(2, 30, 20), None);
        assert_eq!(
            section(&out, "Components"),
            "p  package  path: p\n  p::y\n\
             omitted: 2 modules  in: p 2  next: query <component>\n"
        );

        let out = render_with(&nested(), "repo", 2, limits(3, 30, 20), None);
        assert_eq!(
            section(&out, "Components"),
            "p  package  path: p\n  p::x\n    p::x::deep\n\
             omitted: 1 module  in: p 1  next: query <component>\n"
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
        let out = render_with(&graph, "repo", 2, limits(3, 30, 20), None);
        assert_eq!(
            section(&out, "Components"),
            "a  package  path: a\nb  package  path: b\nc  package  path: c\n\
             omitted: 2 packages  names: d, e  next: query <package>\n\
             omitted: 1 module  in: a 1  next: query <component>\n"
        );
    }

    #[test]
    fn dependencies_across_packages_come_first_then_more_statements() {
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
        let out = render_with(&graph, "repo", 2, limits(30, 3, 20), None);
        assert_eq!(
            section(&out, "Internal dependencies"),
            "p::a -> r  imports: 3\np::a -> q  imports: 1\nq -> p::b  imports: 2\n\
             omitted: 3 dependencies  from: p::b 2, p::a 1  next: query <component>\n"
        );
    }

    #[test]
    fn test_statements_are_counted_apart_and_rank_after_production() {
        let graph = graph(
            vec![
                package("p"),
                module("p::a", "p"),
                module("p::b", "p"),
                module("p::c", "p"),
                module("p::d", "p"),
                module("p::e", "p"),
            ],
            vec![
                import("p::a", "p::b", 1),
                import("p::c", "p::b", 3),
                // only tests: more statements, ranked last all the same
                test_import("p::d", "p::b", 4),
                import("p::e", "p::b", 2),
                test_import("p::e", "p::b", 2),
            ],
        );
        let out = render_with(&graph, "repo", 1, limits(30, 2, 20), None);
        assert_eq!(
            section(&out, "Internal dependencies"),
            "p::c -> p::b  imports: 3\np::e -> p::b  imports: 2  tests: 2\n\
             omitted: 2 dependencies  from: p::a 1, p::d 1  next: query <component>\n"
        );
        let out = render_with(&graph, "repo", 1, limits(30, 30, 20), None);
        assert!(
            section(&out, "Internal dependencies").contains("p::d -> p::b  tests: 4\n"),
            "{out}"
        );
        // tests make no dependents
        assert!(
            section(&out, "Most depended on").starts_with("p::b  dependents: 3  dependencies: 0"),
            "{out}"
        );
    }

    #[test]
    fn entry_files_that_import_their_own_submodules_are_counted_not_listed() {
        // `p/index.ts` is p's own file, as a barrel or an `__init__.py` is
        let mut p = package("p");
        p.evidence
            .push(Evidence::new("p/index.ts").with_note("index"));
        let from = |file: &str, line: u32| {
            Edge::new("p", "p::a", EdgeKind::Import)
                .with_evidence(Evidence::new(file).at_line(line))
        };
        let graph = graph(
            vec![p, module("p::a", "p"), module("p::b", "p")],
            vec![
                from("p/index.ts", 1),
                from("p/index.ts", 2),
                import("p::b", "p::a", 1),
            ],
        );
        let out = render_with(&graph, "repo", 1, limits(30, 30, 20), None);
        assert_eq!(
            section(&out, "Internal dependencies"),
            "p::b -> p::a  imports: 1\n\
             not listed: 2 statements of entry files into their own component's submodules\n"
        );
        assert!(
            section(&out, "Most depended on").starts_with("p::a  dependents: 1"),
            "{out}"
        );
        // another file of p that imports p::a depends on it
        let graph = graph_with(&graph, vec![from("p/config.ts", 1)]);
        let out = render_with(&graph, "repo", 1, limits(30, 30, 20), None);
        assert!(
            section(&out, "Internal dependencies").contains("p -> p::a  imports: 1\n"),
            "{out}"
        );
    }

    #[test]
    fn a_statement_that_reaches_through_a_barrel_counts_for_the_defining_file() {
        let mut lib = module("lib", "p");
        lib.evidence
            .push(Evidence::new("lib/index.ts").with_note("index"));
        let at = |line: u32, target: &str, note: &str, names: &[&str]| {
            Evidence::new("app/a.ts")
                .at_line(line)
                .with_note(note)
                .pointing_at(target)
                .taking(names.iter().copied())
        };
        let mut graph = graph(
            vec![
                package("p"),
                module("app", "p"),
                lib,
                module("lib::money", "lib"),
            ],
            vec![
                // `import { formatPrice } from './lib'`, which lib/index.ts
                // re-exports from lib/money.ts
                Edge::new("app", "lib", EdgeKind::Import).with_evidence(at(
                    1,
                    "lib/index.ts",
                    "import",
                    &["formatPrice"],
                )),
                Edge::new("app", "lib::money", EdgeKind::Import).with_evidence(at(
                    1,
                    "lib/money.ts",
                    "import via lib/index.ts:1",
                    &["formatPrice"],
                )),
                // `import * as lib from './lib'` takes the barrel itself
                Edge::new("app", "lib", EdgeKind::Import).with_evidence(at(
                    2,
                    "lib/index.ts",
                    "import",
                    &["*"],
                )),
            ],
        );
        graph.add_symbol(Symbol {
            id: SymbolId::new("lib::money::formatPrice"),
            name: "formatPrice".into(),
            kind: archmap_core::SymbolKind::Function,
            component: ComponentId::new("lib::money"),
            signature: None,
            evidence: vec![Evidence::new("lib/money.ts").at_line(1)],
        });
        let out = render_with(&graph, "repo", 2, limits(30, 30, 20), None);
        assert_eq!(
            section(&out, "Internal dependencies"),
            "app -> lib  imports: 1\napp -> lib::money  imports: 1\n"
        );
    }

    #[test]
    fn a_rust_use_that_also_reaches_through_a_re_export_still_counts() {
        // `use crate::shapes::{area_of, Circle};`: area_of is the module's
        // own, Circle a re-export of its submodule
        let at = |target: &str, note: &str, name: &str| {
            Evidence::new("src/app/mod.rs")
                .at_line(1)
                .with_note(note)
                .pointing_at(target)
                .taking([name])
        };
        let graph = graph(
            vec![
                package("p"),
                module("app", "p"),
                module("shapes", "p"),
                module("shapes::circle", "shapes"),
            ],
            vec![
                Edge::new("app", "shapes", EdgeKind::Import).with_evidence(at(
                    "src/shapes/mod.rs",
                    "use",
                    "area_of",
                )),
                Edge::new("app", "shapes::circle", EdgeKind::Import).with_evidence(at(
                    "src/shapes/circle.rs",
                    "use via src/shapes/mod.rs:2",
                    "Circle",
                )),
            ],
        );
        let out = render_with(&graph, "repo", 3, limits(30, 30, 20), None);
        assert_eq!(
            section(&out, "Internal dependencies"),
            "app -> shapes  imports: 1\napp -> shapes::circle  imports: 1\n"
        );
    }

    #[test]
    fn the_root_is_named_by_its_directory() {
        assert_eq!(root_name(Path::new("/work/shop")), "shop");
        // a root without a name of its own
        assert_eq!(root_name(Path::new("/")), "/");
    }

    #[test]
    fn a_name_that_several_components_share_reads_as_the_id() {
        // two packages each hold a `types.ts`
        let named = |id: &str, parent: &str| {
            let mut c = module(id, parent);
            c.name = "types.ts".into();
            c
        };
        let graph = graph(
            vec![
                package("p"),
                package("q"),
                named("p::types", "p"),
                named("q::types", "q"),
                module("p::a", "p"),
            ],
            vec![import("p::a", "p::types", 2), import("p::a", "q::types", 1)],
        );
        let out = render_with(&graph, "repo", 1, limits(30, 30, 20), None);
        assert_eq!(
            section(&out, "Internal dependencies"),
            "p::a -> p::types  imports: 2\np::a -> q::types  imports: 1\n"
        );
        assert!(
            section(&out, "Most depended on").starts_with("p::types  dependents: 1"),
            "{out}"
        );
    }

    #[test]
    fn an_external_dependency_names_its_first_declarations() {
        let mut graph = graph(
            (1..=5)
                .map(|i| package(&format!("p{i}")))
                .chain([external("serde")])
                .collect(),
            Vec::new(),
        );
        graph.add_edges((1..=5).map(|i| {
            Edge::new(format!("p{i}"), "ext:cargo:serde", EdgeKind::Dependency)
                .with_evidence(Evidence::new(format!("p{i}/Cargo.toml")))
        }));
        let out = render_with(&graph, "repo", 1, limits(30, 30, 20), None);
        assert!(
            section(&out, "External dependencies").starts_with(
                "serde  declared: p1/Cargo.toml, p2/Cargo.toml, p3/Cargo.toml, +2 more  importers"
            ),
            "{out}"
        );
    }

    /// `graph` with `edges` added.
    fn graph_with(graph: &ArchitectureGraph, edges: Vec<Edge>) -> ArchitectureGraph {
        let mut graph = graph.clone();
        graph.add_edges(edges);
        graph
    }

    #[test]
    fn external_importers_in_test_code_are_counted_apart() {
        let graph = graph(
            vec![
                package("p"),
                module("p::a", "p"),
                module("p::t", "p"),
                external("serde"),
                external("proptest"),
            ],
            vec![
                import("p::a", "ext:cargo:serde", 2),
                test_import("p::t", "ext:cargo:serde", 1),
                test_import("p::t", "ext:cargo:proptest", 1),
            ],
        );
        let out = render_with(&graph, "repo", 1, limits(30, 30, 20), None);
        assert_eq!(
            section(&out, "External dependencies"),
            "proptest  importers: 0  test importers: 1\nserde  importers: 1  top: p::a 2  test importers: 1\n"
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
        let out = render_with(&graph, "repo", 2, limits(30, 30, 2), None);
        assert_eq!(
            section(&out, "External dependencies"),
            "x1  importers: 3  top: p::a 1, p::b 1, p::c 1\n\
             x2  importers: 2  top: p::a 2, p::b 1\n\
             omitted: 3 external dependencies  names: x3, x5, x4  next: query <name>\n"
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
            "repo",
            2,
            Limits {
                budget: usize::MAX,
                ..Limits::new(false)
            },
            None,
        );
        assert!(capped_only.len() > BUDGET, "the test needs a long summary");

        let out = render_with(&graph, "repo", 2, Limits::new(false), None);
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
            "repo",
            2,
            Limits {
                budget: 1000,
                ..Limits::new(false)
            },
            None,
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
        let out = render_with(
            &graph(components, edges),
            "repo",
            2,
            Limits::new(false),
            None,
        );
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
