//! Deterministic summary of a rolled-up architecture graph.
//!
//! The summary is the first map an agent reads, so it is a small text IR
//! rather than a report: one fact per line, `key: value` fields, dependency
//! direction spelled `a -> b`, measured values instead of judgments, and no
//! prose. Every line is computed from the graph, so the same scan always
//! yields the same text.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::path::Path;

use archmap_core::{ArchitectureGraph, Component, ComponentId, ComponentKind, EdgeKind};

/// How many components the "most depended on" section lists.
const TOP_DEPENDED_ON: usize = 10;

/// How many importers an external dependency names before counting the
/// rest. Widely used libraries would otherwise dominate the summary without
/// telling an agent where to look.
const MAX_IMPORTERS: usize = 5;

/// How many components the dynamic-imports line names before counting the
/// rest.
const MAX_DYNAMIC_IMPORTERS: usize = 5;

/// Coupling that no analyzer reads, whatever the repository contains.
const RUNTIME_COUPLING: &str = "runtime coupling: not analyzed \
     (HTTP, databases, queues, subprocesses, configuration-driven loading)";

/// One internal dependency at the summary's depth.
#[derive(Default)]
struct Dependency {
    imports: usize,
    declared: bool,
    other: BTreeMap<&'static str, usize>,
}

pub fn render(graph: &ArchitectureGraph, depth: usize) -> String {
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

    let mut out = String::new();
    let name = Path::new(&graph.meta.root)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| graph.meta.root.clone());
    let full_internal = graph.components.values().filter(|c| is_internal(c)).count();
    let _ = writeln!(out, "# archmap summary");
    let _ = writeln!(out, "root: {name}");
    let _ = writeln!(out, "depth: {depth}");
    let _ = writeln!(
        out,
        "components: {} shown, {full_internal} in the full graph",
        internal.len()
    );
    let _ = writeln!(out, "public symbols: {}", graph.symbols.len());
    let _ = writeln!(out, "internal dependencies: {}", dependencies.len());
    let _ = writeln!(out, "external dependencies: {}", externals.len());
    let _ = writeln!(
        out,
        "source: manifests and import statements; nothing is inferred"
    );
    let _ = writeln!(
        out,
        "next: archmap query <component> --depth {depth}; archmap impact <component-or-file> --depth {depth}"
    );

    coverage(&mut out, graph, &rolled);
    components(&mut out, graph, &rolled, &internal, depth);
    internal_dependencies(&mut out, &rolled, &dependencies);
    external_dependencies(&mut out, &rolled, &externals);
    most_depended_on(&mut out, &rolled, &internal, &dependencies);
    out
}

/// What the map leaves out, before the map: files no analyzer read, imports
/// without an edge, modules loaded by computed names, and coupling that no
/// analyzer reads. An agent can then tell an absent edge from an unseen one.
fn coverage(out: &mut String, graph: &ArchitectureGraph, rolled: &ArchitectureGraph) {
    let _ = writeln!(out, "\n## Coverage");

    let mut without_edge: BTreeMap<&str, usize> = BTreeMap::new();
    for import in &graph.unmapped_imports {
        if let Some(language) = graph
            .component(&import.from)
            .and_then(|c| c.language.as_deref())
        {
            *without_edge.entry(language).or_default() += 1;
        }
    }
    let mut not_analyzed = Vec::new();
    for (language, c) in &graph.meta.coverage {
        match c.read {
            Some(read) => {
                let without = without_edge.get(language.as_str()).copied().unwrap_or(0);
                let _ = writeln!(
                    out,
                    "{language}  files: {}  read: {read}  imports without an edge: {without}",
                    c.files
                );
            }
            None => not_analyzed.push(format!("{language}: {}", c.files)),
        }
    }
    if not_analyzed.is_empty() {
        let _ = writeln!(out, "not analyzed: none");
    } else {
        let _ = writeln!(out, "not analyzed  {}", not_analyzed.join("  "));
    }

    let mut importers: BTreeMap<&ComponentId, usize> = BTreeMap::new();
    for import in &rolled.dynamic_imports {
        *importers.entry(&import.from).or_default() += 1;
    }
    let mut importers: Vec<(&ComponentId, usize)> = importers.into_iter().collect();
    importers.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    let mut line = format!("dynamic imports: {}", rolled.dynamic_imports.len());
    if !importers.is_empty() {
        let mut top: Vec<String> = importers
            .iter()
            .take(MAX_DYNAMIC_IMPORTERS)
            .map(|(id, n)| format!("{} {n}", name_of(rolled, id)))
            .collect();
        if importers.len() > MAX_DYNAMIC_IMPORTERS {
            top.push(format!("+{} more", importers.len() - MAX_DYNAMIC_IMPORTERS));
        }
        let _ = write!(line, "  in: {}", top.join(", "));
    }
    let _ = writeln!(out, "{line}");
    let _ = writeln!(out, "{RUNTIME_COUPLING}");
}

fn components(
    out: &mut String,
    graph: &ArchitectureGraph,
    rolled: &ArchitectureGraph,
    internal: &[&Component],
    depth: usize,
) {
    let _ = writeln!(out, "\n## Components");

    // How many original components each kept component absorbed.
    let mut folded: BTreeMap<ComponentId, usize> = BTreeMap::new();
    for id in graph.components.keys() {
        let ancestor = graph.ancestor_at(id, depth);
        if ancestor != *id {
            *folded.entry(ancestor).or_default() += 1;
        }
    }
    let mut children: BTreeMap<&ComponentId, Vec<&Component>> = BTreeMap::new();
    let mut roots = Vec::new();
    for c in internal {
        match c
            .parent
            .as_ref()
            .filter(|p| rolled.components.contains_key(*p))
        {
            Some(parent) => children.entry(parent).or_default().push(c),
            None => roots.push(*c),
        }
    }

    fn walk(
        out: &mut String,
        c: &Component,
        level: usize,
        rolled: &ArchitectureGraph,
        children: &BTreeMap<&ComponentId, Vec<&Component>>,
        folded: &BTreeMap<ComponentId, usize>,
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
        if let Some(n) = folded.get(&c.id) {
            let _ = write!(line, "  folded: {n}");
        }
        let _ = writeln!(out, "{line}");
        for child in children.get(&c.id).into_iter().flatten() {
            walk(out, child, level + 1, rolled, children, folded);
        }
    }

    if roots.is_empty() {
        let _ = writeln!(out, "none");
    }
    for root in roots {
        walk(out, root, 0, rolled, &children, &folded);
    }
}

fn internal_dependencies(
    out: &mut String,
    rolled: &ArchitectureGraph,
    dependencies: &BTreeMap<(&ComponentId, &ComponentId), Dependency>,
) {
    let _ = writeln!(out, "\n## Internal dependencies");
    if dependencies.is_empty() {
        let _ = writeln!(out, "none");
        return;
    }
    let mut lines: Vec<(&str, usize, &str, &Dependency)> = dependencies
        .iter()
        .map(|((from, to), dep)| (name_of(rolled, from), dep.imports, name_of(rolled, to), dep))
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
        let _ = writeln!(out, "{line}");
    }
}

fn external_dependencies(out: &mut String, rolled: &ArchitectureGraph, externals: &[&Component]) {
    let _ = writeln!(out, "\n## External dependencies");
    if externals.is_empty() {
        let _ = writeln!(out, "none");
        return;
    }
    for ext in externals {
        let mut declared_in: BTreeSet<&str> = BTreeSet::new();
        let mut importers: Vec<(&ComponentId, usize)> = Vec::new();
        for edge in rolled.incoming(&ext.id) {
            match edge.kind {
                EdgeKind::Dependency => {
                    declared_in.extend(edge.evidence.iter().map(|e| e.file.as_str()));
                }
                EdgeKind::Import => importers.push((&edge.from, edge.statements())),
                _ => {}
            }
        }
        importers.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));

        let mut line = ext.name.clone();
        if !declared_in.is_empty() {
            let files: Vec<&str> = declared_in.into_iter().collect();
            let _ = write!(line, "  declared: {}", files.join(", "));
        }
        if importers.is_empty() {
            line.push_str("  importers: none resolved");
        } else {
            let mut top: Vec<String> = importers
                .iter()
                .take(MAX_IMPORTERS)
                .map(|(id, n)| format!("{} {n}", name_of(rolled, id)))
                .collect();
            if importers.len() > MAX_IMPORTERS {
                top.push(format!("+{} more", importers.len() - MAX_IMPORTERS));
            }
            let _ = write!(
                line,
                "  importers: {}  top: {}",
                importers.len(),
                top.join(", ")
            );
        }
        let _ = writeln!(out, "{line}");
    }
}

fn most_depended_on(
    out: &mut String,
    rolled: &ArchitectureGraph,
    internal: &[&Component],
    dependencies: &BTreeMap<(&ComponentId, &ComponentId), Dependency>,
) {
    let _ = writeln!(out, "\n## Most depended on");
    let mut dependents: BTreeMap<&ComponentId, usize> = BTreeMap::new();
    let mut uses: BTreeMap<&ComponentId, usize> = BTreeMap::new();
    for (from, to) in dependencies.keys() {
        *dependents.entry(to).or_default() += 1;
        *uses.entry(from).or_default() += 1;
    }
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
    let total = internal.len();
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

fn is_internal(c: &Component) -> bool {
    c.kind != ComponentKind::External
}

fn internal_id(graph: &ArchitectureGraph, id: &ComponentId) -> bool {
    graph.component(id).is_some_and(is_internal)
}

fn name_of<'a>(graph: &'a ArchitectureGraph, id: &'a ComponentId) -> &'a str {
    graph.component(id).map_or(id.as_str(), |c| c.name.as_str())
}
