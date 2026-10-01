//! Structural signals: deterministic observations about the shape of the
//! code. A signal reports what the graph shows, with the files behind it; it
//! does not judge the design, and it never fails `check`.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::{ArchitectureGraph, ComponentId, ComponentKind};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Signal {
    /// `component` and each partner depend on each other, but through
    /// different files of `component`: the files the partners use are not
    /// the files that use the partners. Roll-up joins the two groups into
    /// one component, which is what makes the dependency look mutual.
    MixedDirections {
        component: ComponentId,
        partners: Vec<ComponentId>,
        /// Files of `component` that partners import, with how many do.
        used_by_partners: Vec<FileUse>,
        /// Files of `component` that import partners, with which ones.
        depending_on_partners: Vec<FileDependence>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileUse {
    pub file: String,
    pub partners: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileDependence {
    pub file: String,
    pub partners: Vec<ComponentId>,
}

/// Signals for the components of `graph` rolled up to `depth`.
pub fn signals(graph: &ArchitectureGraph, depth: usize) -> Vec<Signal> {
    let rolled = graph.rollup(depth);
    let internal = |id: &ComponentId| {
        rolled
            .component(id)
            .is_some_and(|c| c.kind != ComponentKind::External)
    };
    let pairs: BTreeSet<(&ComponentId, &ComponentId)> = rolled
        .edges
        .iter()
        .filter(|e| e.from != e.to && internal(&e.from) && internal(&e.to))
        .filter(|e| e.runs_in_production())
        .map(|e| (&e.from, &e.to))
        .collect();
    let mut partners: BTreeMap<ComponentId, BTreeSet<ComponentId>> = BTreeMap::new();
    for (a, b) in &pairs {
        if pairs.contains(&(*b, *a)) {
            partners
                .entry((*a).clone())
                .or_default()
                .insert((*b).clone());
        }
    }

    // Component -> its file -> partners that use it / that it uses.
    let mut used: BTreeMap<ComponentId, BTreeMap<String, BTreeSet<ComponentId>>> = BTreeMap::new();
    let mut uses: BTreeMap<ComponentId, BTreeMap<String, BTreeSet<ComponentId>>> = BTreeMap::new();
    for edge in &graph.edges {
        let (from, to) = (
            graph.ancestor_at(&edge.from, depth),
            graph.ancestor_at(&edge.to, depth),
        );
        if !partners.get(&from).is_some_and(|p| p.contains(&to)) {
            continue;
        }
        for e in edge.evidence.iter().filter(|e| e.runs_in_production()) {
            uses.entry(from.clone())
                .or_default()
                .entry(e.file.clone())
                .or_default()
                .insert(to.clone());
            if let Some(target) = &e.target {
                used.entry(to.clone())
                    .or_default()
                    .entry(target.clone())
                    .or_default()
                    .insert(from.clone());
            }
        }
    }

    let mut out = Vec::new();
    for (component, component_partners) in partners {
        let (Some(used), Some(uses)) = (used.get(&component), uses.get(&component)) else {
            continue;
        };
        if used.keys().any(|file| uses.contains_key(file)) {
            continue;
        }
        let mut used_by_partners: Vec<FileUse> = used
            .iter()
            .map(|(file, p)| FileUse {
                file: file.clone(),
                partners: p.len(),
            })
            .collect();
        used_by_partners.sort_by(|a, b| {
            b.partners
                .cmp(&a.partners)
                .then_with(|| a.file.cmp(&b.file))
        });
        let depending_on_partners = uses
            .iter()
            .map(|(file, p)| FileDependence {
                file: file.clone(),
                partners: p.iter().cloned().collect(),
            })
            .collect();
        out.push(Signal::MixedDirections {
            component,
            partners: component_partners.into_iter().collect(),
            used_by_partners,
            depending_on_partners,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Component, Edge, EdgeKind, Evidence};

    fn graph() -> ArchitectureGraph {
        let mut g = ArchitectureGraph::default();
        for id in ["core", "util", "models"] {
            let mut c = Component::new(id, id, ComponentKind::Module);
            c.path = Some(id.into());
            g.add_component(c);
        }
        let dep = |from: &str, to: &str, file: &str, target: &str| {
            Edge::new(from, to, EdgeKind::Import)
                .with_evidence(Evidence::new(file).at_line(1).pointing_at(target))
        };
        g.add_edges([
            dep("core", "util", "core/a.py", "util/log.py"),
            dep("models", "util", "models/m.py", "util/log.py"),
            dep("util", "core", "util/store.py", "core/a.py"),
            dep("util", "models", "util/registry.py", "models/m.py"),
        ]);
        g
    }

    #[test]
    fn a_component_whose_used_and_using_files_differ_is_mixed() {
        let found = signals(&graph(), 9);
        let util = found
            .iter()
            .find(|Signal::MixedDirections { component, .. }| component.as_str() == "util")
            .expect("util mixes directions");
        let Signal::MixedDirections {
            partners,
            used_by_partners,
            depending_on_partners,
            ..
        } = util;
        assert_eq!(
            partners,
            &vec![ComponentId::new("core"), ComponentId::new("models")]
        );
        assert_eq!(
            used_by_partners,
            &vec![FileUse {
                file: "util/log.py".into(),
                partners: 2
            }]
        );
        let depending: Vec<&str> = depending_on_partners
            .iter()
            .map(|d| d.file.as_str())
            .collect();
        assert_eq!(depending, vec!["util/registry.py", "util/store.py"]);
    }

    #[test]
    fn imports_of_types_only_mix_no_directions() {
        let mut g = ArchitectureGraph::default();
        for id in ["core", "util", "models"] {
            let mut c = Component::new(id, id, ComponentKind::Module);
            c.path = Some(id.into());
            g.add_component(c);
        }
        let dep = |from: &str, to: &str, file: &str, target: &str, types: bool| {
            Edge::new(from, to, EdgeKind::Import).with_evidence(
                Evidence::new(file)
                    .at_line(1)
                    .pointing_at(target)
                    .type_only(types),
            )
        };
        g.add_edges([
            dep("core", "util", "core/a.py", "util/log.py", false),
            dep("models", "util", "models/m.py", "util/log.py", false),
            dep("util", "core", "util/store.py", "core/a.py", true),
            dep("util", "models", "util/registry.py", "models/m.py", true),
        ]);
        assert!(signals(&g, 9).is_empty());
    }

    #[test]
    fn imports_in_test_code_mix_no_directions() {
        let mut g = ArchitectureGraph::default();
        for id in ["core", "util", "models"] {
            let mut c = Component::new(id, id, ComponentKind::Module);
            c.path = Some(id.into());
            g.add_component(c);
        }
        let dep = |from: &str, to: &str, file: &str, target: &str, test: bool| {
            Edge::new(from, to, EdgeKind::Import).with_evidence(
                Evidence::new(file)
                    .at_line(1)
                    .pointing_at(target)
                    .in_test(test),
            )
        };
        g.add_edges([
            dep("core", "util", "core/a.py", "util/log.py", false),
            dep("models", "util", "models/m.py", "util/log.py", false),
            dep("util", "core", "util/test_store.py", "core/a.py", true),
            dep(
                "util",
                "models",
                "util/test_registry.py",
                "models/m.py",
                true,
            ),
        ]);
        assert!(signals(&g, 9).is_empty());
    }

    #[test]
    fn a_file_on_both_sides_is_not_mixed() {
        let mut g = graph();
        // util/log.py now also imports core: one file plays both roles
        g.add_edges([Edge::new("util", "core", EdgeKind::Import).with_evidence(
            Evidence::new("util/log.py")
                .at_line(2)
                .pointing_at("core/a.py"),
        )]);
        let found = signals(&g, 9);
        assert!(found
            .iter()
            .all(|Signal::MixedDirections { component, .. }| component.as_str() != "util"));
    }
}
