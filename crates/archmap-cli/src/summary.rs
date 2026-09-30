//! Deterministic Markdown summary of a rolled-up architecture graph.
//!
//! Every line is computed from the graph. There is no generated prose, so
//! the same scan always yields the same summary. The summary is meant to be
//! read by coding agents before they explore a repository: short lines,
//! exact component names they can pass to `query` and `impact`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::path::Path;

use archmap_core::{ArchitectureGraph, Component, ComponentId, ComponentKind, EdgeKind};

/// How many components the "most depended-on" section lists.
const TOP_DEPENDED_ON: usize = 10;

/// How many importers an external dependency lists before summarizing the
/// rest as a count. Widely used libraries would otherwise dominate the
/// summary without telling an agent where to look.
const MAX_IMPORTERS: usize = 5;

pub fn render(graph: &ArchitectureGraph, depth: usize) -> String {
    let rolled = graph.rollup(depth);
    let mut out = String::new();
    header(&mut out, graph, depth);
    components(&mut out, graph, &rolled, depth);
    internal_dependencies(&mut out, &rolled);
    external_dependencies(&mut out, &rolled);
    most_depended_on(&mut out, &rolled);
    out
}

fn header(out: &mut String, graph: &ArchitectureGraph, depth: usize) {
    let name = Path::new(&graph.meta.root)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| graph.meta.root.clone());
    let version = graph.meta.tool_version.as_deref().unwrap_or("unknown");
    let _ = writeln!(out, "# Architecture summary: {name}\n");
    let _ = writeln!(
        out,
        "Structural summary by archmap {version}, rolled up to depth {depth}. \
         Every line is derived from manifests and import statements; nothing is inferred."
    );
    let _ = writeln!(
        out,
        "Full graph: {}, {}, {}.",
        plural(graph.components.len(), "component"),
        plural(graph.symbols.len(), "public symbol"),
        plural(graph.edges.len(), "edge"),
    );
    let _ = writeln!(
        out,
        "Drill down with `archmap query <component>` and `archmap impact <component-or-file>`.\n"
    );
}

fn components(
    out: &mut String,
    graph: &ArchitectureGraph,
    rolled: &ArchitectureGraph,
    depth: usize,
) {
    let _ = writeln!(out, "## Components\n");

    // How many original components each kept component absorbed.
    let mut folded: BTreeMap<ComponentId, usize> = BTreeMap::new();
    for id in graph.components.keys() {
        let ancestor = graph.ancestor_at(id, depth);
        if ancestor != *id {
            *folded.entry(ancestor).or_default() += 1;
        }
    }

    let internal: Vec<&Component> = rolled
        .components
        .values()
        .filter(|c| is_internal(c))
        .collect();
    let mut children: BTreeMap<&ComponentId, Vec<&Component>> = BTreeMap::new();
    let mut roots = Vec::new();
    for c in &internal {
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
        let mut details = Vec::new();
        if c.kind == ComponentKind::Package {
            details.push(match &c.language {
                Some(lang) => format!("{lang} package"),
                None => "package".to_owned(),
            });
        }
        if let Some(path) = &c.path {
            details.push(format!("`{path}`"));
        }
        let symbols = rolled.symbols_of(&c.id).count();
        if symbols > 0 {
            details.push(plural(symbols, "public symbol"));
        }
        if let Some(n) = folded.get(&c.id) {
            details.push(format!("{} folded", plural(*n, "submodule")));
        }
        let _ = writeln!(
            out,
            "{}- {}: {}",
            "  ".repeat(level),
            c.name,
            details.join(", ")
        );
        for child in children.get(&c.id).into_iter().flatten() {
            walk(out, child, level + 1, rolled, children, folded);
        }
    }

    for root in roots {
        walk(out, root, 0, rolled, &children, &folded);
    }
    out.push('\n');
}

fn internal_dependencies(out: &mut String, rolled: &ArchitectureGraph) {
    let _ = writeln!(out, "## Internal dependencies\n");
    let _ = writeln!(
        out,
        "Numbers count the import statements behind an edge. \
         `declared` means a manifest also declares the dependency.\n"
    );

    #[derive(Default)]
    struct Target {
        imports: usize,
        declared: bool,
        other: BTreeSet<&'static str>,
    }
    let mut by_source: BTreeMap<&ComponentId, BTreeMap<&ComponentId, Target>> = BTreeMap::new();
    for edge in &rolled.edges {
        if !internal_id(rolled, &edge.from) || !internal_id(rolled, &edge.to) {
            continue;
        }
        let target = by_source
            .entry(&edge.from)
            .or_default()
            .entry(&edge.to)
            .or_default();
        match edge.kind {
            EdgeKind::Import => target.imports += edge.evidence.len(),
            EdgeKind::Dependency => target.declared = true,
            other => {
                target.other.insert(other.as_str());
            }
        }
    }

    if by_source.is_empty() {
        let _ = writeln!(out, "No internal dependencies.\n");
        return;
    }
    for (source, targets) in by_source {
        let mut targets: Vec<(&ComponentId, Target)> = targets.into_iter().collect();
        targets.sort_by(|a, b| b.1.imports.cmp(&a.1.imports).then_with(|| a.0.cmp(b.0)));
        let rendered: Vec<String> = targets
            .iter()
            .map(|(id, t)| {
                let mut notes = Vec::new();
                if t.imports > 0 {
                    notes.push(t.imports.to_string());
                }
                if t.declared {
                    notes.push("declared".to_owned());
                }
                notes.extend(t.other.iter().map(|k| k.to_string()));
                format!("{} ({})", name_of(rolled, id), notes.join(", "))
            })
            .collect();
        let _ = writeln!(
            out,
            "- {} -> {}",
            name_of(rolled, source),
            rendered.join(", ")
        );
    }
    out.push('\n');
}

fn external_dependencies(out: &mut String, rolled: &ArchitectureGraph) {
    let _ = writeln!(out, "## External dependencies\n");
    let externals: Vec<&Component> = rolled
        .components
        .values()
        .filter(|c| c.kind == ComponentKind::External)
        .collect();
    if externals.is_empty() {
        let _ = writeln!(out, "No external dependencies.\n");
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
                EdgeKind::Import => importers.push((&edge.from, edge.evidence.len())),
                _ => {}
            }
        }
        importers.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));

        let mut parts = Vec::new();
        if !declared_in.is_empty() {
            let files: Vec<String> = declared_in.iter().map(|f| format!("`{f}`")).collect();
            parts.push(format!("declared in {}", files.join(", ")));
        }
        if importers.is_empty() {
            parts.push("no resolved imports".to_owned());
        } else {
            let mut list: Vec<String> = importers
                .iter()
                .take(MAX_IMPORTERS)
                .map(|(id, n)| format!("{} ({n})", name_of(rolled, id)))
                .collect();
            if importers.len() > MAX_IMPORTERS {
                list.push(format!("and {} more", importers.len() - MAX_IMPORTERS));
            }
            parts.push(format!(
                "imported by {}: {}",
                plural(importers.len(), "component"),
                list.join(", ")
            ));
        }
        let _ = writeln!(out, "- {}: {}", ext.name, parts.join("; "));
    }
    out.push('\n');
}

fn most_depended_on(out: &mut String, rolled: &ArchitectureGraph) {
    let _ = writeln!(out, "## Most depended-on\n");
    let mut incoming: BTreeMap<&ComponentId, BTreeSet<&ComponentId>> = BTreeMap::new();
    let mut outgoing: BTreeMap<&ComponentId, BTreeSet<&ComponentId>> = BTreeMap::new();
    for edge in &rolled.edges {
        if internal_id(rolled, &edge.from) && internal_id(rolled, &edge.to) {
            incoming.entry(&edge.to).or_default().insert(&edge.from);
            outgoing.entry(&edge.from).or_default().insert(&edge.to);
        }
    }
    let mut ranked: Vec<(&ComponentId, usize, usize)> = incoming
        .iter()
        .map(|(id, from)| (*id, from.len(), outgoing.get(id).map_or(0, BTreeSet::len)))
        .collect();
    ranked.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| b.2.cmp(&a.2))
            .then_with(|| a.0.cmp(b.0))
    });

    if ranked.is_empty() {
        let _ = writeln!(out, "No internal dependencies.");
        return;
    }
    let _ = writeln!(
        out,
        "Components depended on by the most other components at this depth.\n"
    );
    for (id, n_in, n_out) in ranked.into_iter().take(TOP_DEPENDED_ON) {
        let _ = writeln!(
            out,
            "- {}: used by {}, uses {}",
            name_of(rolled, id),
            plural(n_in, "component"),
            plural(n_out, "component")
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

fn plural(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}
