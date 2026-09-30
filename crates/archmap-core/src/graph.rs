use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};

use crate::{Component, ComponentId, Edge, GraphFragment, Symbol, SymbolId, SCHEMA_VERSION};

/// Information about how a graph was produced.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphMeta {
    /// Repository root as given to the scanner.
    #[serde(default)]
    pub root: String,
    /// Names of the analyzers that contributed fragments.
    #[serde(default)]
    pub analyzers: Vec<String>,
    /// Version of the tool that produced the graph.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_version: Option<String>,
}

/// The normalized architecture graph.
///
/// Components and symbols are keyed by id so that fragments from several
/// analyzers can be merged deterministically. Edges describing the same
/// relationship are collapsed into one edge with combined evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchitectureGraph {
    pub schema_version: u32,
    #[serde(default)]
    pub meta: GraphMeta,
    #[serde(default)]
    pub components: BTreeMap<ComponentId, Component>,
    #[serde(default)]
    pub symbols: BTreeMap<SymbolId, Symbol>,
    #[serde(default)]
    pub edges: Vec<Edge>,
}

impl Default for ArchitectureGraph {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            meta: GraphMeta::default(),
            components: BTreeMap::new(),
            symbols: BTreeMap::new(),
            edges: Vec::new(),
        }
    }
}

impl ArchitectureGraph {
    pub fn new(meta: GraphMeta) -> Self {
        Self {
            meta,
            ..Self::default()
        }
    }

    /// Insert a component. If it already exists, evidence is merged and
    /// unknown fields are filled in from the new value.
    pub fn add_component(&mut self, component: Component) {
        match self.components.get_mut(&component.id) {
            Some(existing) => {
                if existing.language.is_none() {
                    existing.language = component.language;
                }
                if existing.path.is_none() {
                    existing.path = component.path;
                }
                if existing.parent.is_none() {
                    existing.parent = component.parent;
                }
                merge_evidence(&mut existing.evidence, component.evidence);
            }
            None => {
                self.components.insert(component.id.clone(), component);
            }
        }
    }

    /// Insert a symbol. Later symbols with the same id replace earlier ones,
    /// keeping combined evidence.
    pub fn add_symbol(&mut self, mut symbol: Symbol) {
        if let Some(existing) = self.symbols.remove(&symbol.id) {
            let mut evidence = existing.evidence;
            merge_evidence(&mut evidence, symbol.evidence);
            symbol.evidence = evidence;
            if symbol.signature.is_none() {
                symbol.signature = existing.signature;
            }
        }
        self.symbols.insert(symbol.id.clone(), symbol);
    }

    /// Insert an edge, collapsing it into an existing edge of the same
    /// relationship when there is one.
    pub fn add_edge(&mut self, edge: Edge) {
        if let Some(existing) = self.edges.iter_mut().find(|e| e.same_relationship(&edge)) {
            merge_evidence(&mut existing.evidence, edge.evidence);
        } else {
            self.edges.push(edge);
        }
    }

    /// Merge an analyzer fragment into the graph.
    pub fn merge(&mut self, fragment: GraphFragment) {
        for component in fragment.components {
            self.add_component(component);
        }
        for symbol in fragment.symbols {
            self.add_symbol(symbol);
        }
        for edge in fragment.edges {
            self.add_edge(edge);
        }
    }

    /// Sort edges so that serialized output is stable regardless of the
    /// order analyzers ran in.
    pub fn normalize(&mut self) {
        self.edges
            .sort_by(|a, b| (&a.from, &a.to, a.kind).cmp(&(&b.from, &b.to, b.kind)));
        for edge in &mut self.edges {
            edge.evidence.sort();
        }
    }

    pub fn component(&self, id: &ComponentId) -> Option<&Component> {
        self.components.get(id)
    }

    /// Components whose display name matches exactly (a Python module's
    /// dotted path, for example), for lookups by something shorter than
    /// the full id.
    pub fn components_named<'a>(&'a self, name: &str) -> impl Iterator<Item = &'a Component> + 'a {
        let name = name.to_owned();
        self.components.values().filter(move |c| c.name == name)
    }

    pub fn symbol(&self, id: &SymbolId) -> Option<&Symbol> {
        self.symbols.get(id)
    }

    /// Symbols owned by a component, in id order.
    pub fn symbols_of<'a>(&'a self, id: &ComponentId) -> impl Iterator<Item = &'a Symbol> + 'a {
        let id = id.clone();
        self.symbols.values().filter(move |s| s.component == id)
    }

    /// Symbols whose short name matches exactly.
    pub fn symbols_named<'a>(&'a self, name: &str) -> impl Iterator<Item = &'a Symbol> + 'a {
        let name = name.to_owned();
        self.symbols.values().filter(move |s| s.name == name)
    }

    /// Edges leaving a component (what it depends on).
    pub fn outgoing<'a>(&'a self, id: &ComponentId) -> impl Iterator<Item = &'a Edge> + 'a {
        let id = id.clone();
        self.edges.iter().filter(move |e| e.from == id)
    }

    /// Edges entering a component (who depends on it).
    pub fn incoming<'a>(&'a self, id: &ComponentId) -> impl Iterator<Item = &'a Edge> + 'a {
        let id = id.clone();
        self.edges.iter().filter(move |e| e.to == id)
    }

    /// Direct dependencies of a component.
    pub fn dependencies_of(&self, id: &ComponentId) -> BTreeSet<ComponentId> {
        self.outgoing(id).map(|e| e.to.clone()).collect()
    }

    /// Direct dependents of a component.
    pub fn dependents_of(&self, id: &ComponentId) -> BTreeSet<ComponentId> {
        self.incoming(id).map(|e| e.from.clone()).collect()
    }

    /// Every component that transitively depends on `id` (excluding `id`).
    ///
    /// This is the basis of `archmap impact`: if `id` changes, these are the
    /// components that may be affected.
    pub fn transitive_dependents(&self, id: &ComponentId) -> BTreeSet<ComponentId> {
        let mut seen = BTreeSet::new();
        let mut queue: VecDeque<ComponentId> = VecDeque::from([id.clone()]);
        while let Some(current) = queue.pop_front() {
            for dependent in self.dependents_of(&current) {
                if dependent != *id && seen.insert(dependent.clone()) {
                    queue.push_back(dependent);
                }
            }
        }
        seen
    }

    /// Find the component that owns a file path (relative to the repo root),
    /// choosing the component with the longest matching `path` prefix.
    pub fn component_for_path(&self, file: &str) -> Option<&Component> {
        let file = file.trim_start_matches("./");
        self.components
            .values()
            .filter(|c| {
                c.path.as_deref().is_some_and(|p| {
                    let p = p.trim_start_matches("./").trim_end_matches('/');
                    p.is_empty() || p == "." || file == p || file.starts_with(&format!("{p}/"))
                })
            })
            .max_by_key(|c| c.path.as_deref().map(str::len).unwrap_or(0))
    }
}

fn merge_evidence(into: &mut Vec<crate::Evidence>, from: Vec<crate::Evidence>) {
    for e in from {
        if !into.contains(&e) {
            into.push(e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ComponentKind, EdgeKind, Evidence, SymbolKind};

    fn component(id: &str) -> Component {
        Component::new(id, id, ComponentKind::Package)
    }

    fn edge(from: &str, to: &str, file: &str, line: u32) -> Edge {
        Edge::new(from, to, EdgeKind::Import).with_evidence(Evidence::new(file).at_line(line))
    }

    #[test]
    fn merging_same_relationship_collapses_edges_and_keeps_evidence() {
        let mut graph = ArchitectureGraph::default();
        graph.add_edge(edge("a", "b", "src/x.rs", 1));
        graph.add_edge(edge("a", "b", "src/y.rs", 2));
        graph.add_edge(edge("a", "b", "src/x.rs", 1)); // duplicate evidence

        assert_eq!(graph.edges.len(), 1);
        assert_eq!(graph.edges[0].evidence.len(), 2);
    }

    #[test]
    fn different_kinds_are_different_edges() {
        let mut graph = ArchitectureGraph::default();
        graph.add_edge(Edge::new("a", "b", EdgeKind::Import));
        graph.add_edge(Edge::new("a", "b", EdgeKind::Dependency));
        assert_eq!(graph.edges.len(), 2);
    }

    #[test]
    fn merging_components_fills_missing_fields() {
        let mut graph = ArchitectureGraph::default();
        graph.add_component(component("a"));
        let mut richer = component("a");
        richer.language = Some("rust".into());
        richer.path = Some("crates/a".into());
        graph.add_component(richer);

        let a = graph.component(&"a".into()).unwrap();
        assert_eq!(a.language.as_deref(), Some("rust"));
        assert_eq!(a.path.as_deref(), Some("crates/a"));
        assert_eq!(graph.components.len(), 1);
    }

    #[test]
    fn transitive_dependents_walks_reverse_edges() {
        // cli -> scan -> core ; cli -> core
        let mut graph = ArchitectureGraph::default();
        for id in ["cli", "scan", "core", "unrelated"] {
            graph.add_component(component(id));
        }
        graph.add_edge(Edge::new("cli", "scan", EdgeKind::Dependency));
        graph.add_edge(Edge::new("scan", "core", EdgeKind::Dependency));
        graph.add_edge(Edge::new("cli", "core", EdgeKind::Dependency));

        let affected = graph.transitive_dependents(&"core".into());
        let affected: Vec<&str> = affected.iter().map(|c| c.as_str()).collect();
        assert_eq!(affected, vec!["cli", "scan"]);

        assert!(graph.transitive_dependents(&"cli".into()).is_empty());
    }

    #[test]
    fn transitive_dependents_terminates_on_cycles() {
        let mut graph = ArchitectureGraph::default();
        graph.add_edge(Edge::new("a", "b", EdgeKind::Import));
        graph.add_edge(Edge::new("b", "a", EdgeKind::Import));
        let affected = graph.transitive_dependents(&"a".into());
        assert_eq!(affected.len(), 1);
        assert!(affected.contains(&"b".into()));
    }

    #[test]
    fn component_for_path_prefers_longest_prefix() {
        let mut graph = ArchitectureGraph::default();
        let mut root = component("root");
        root.path = Some(".".into());
        let mut nested = component("nested");
        nested.path = Some("crates/nested".into());
        graph.add_component(root);
        graph.add_component(nested);

        assert_eq!(
            graph
                .component_for_path("crates/nested/src/lib.rs")
                .unwrap()
                .id,
            "nested".into()
        );
        assert_eq!(
            graph
                .component_for_path("crates/nestedx/lib.rs")
                .unwrap()
                .id,
            "root".into()
        );
    }

    #[test]
    fn normalize_makes_edge_order_deterministic() {
        let mut g1 = ArchitectureGraph::default();
        g1.add_edge(Edge::new("b", "c", EdgeKind::Import));
        g1.add_edge(Edge::new("a", "c", EdgeKind::Import));
        let mut g2 = ArchitectureGraph::default();
        g2.add_edge(Edge::new("a", "c", EdgeKind::Import));
        g2.add_edge(Edge::new("b", "c", EdgeKind::Import));
        g1.normalize();
        g2.normalize();
        assert_eq!(g1, g2);
    }

    #[test]
    fn json_roundtrip_preserves_graph() {
        let mut graph = ArchitectureGraph::default();
        let mut c = component("a");
        c.language = Some("rust".into());
        graph.add_component(c);
        graph.add_symbol(Symbol {
            id: SymbolId::new("a::run"),
            name: "run".into(),
            kind: SymbolKind::Function,
            component: "a".into(),
            signature: Some("pub fn run()".into()),
            evidence: vec![Evidence::new("src/lib.rs").at_line(3)],
        });
        graph.add_edge(edge("a", "ext:serde", "src/lib.rs", 1));

        let json = serde_json::to_string(&graph).unwrap();
        let back: ArchitectureGraph = serde_json::from_str(&json).unwrap();
        assert_eq!(graph, back);
        assert!(json.contains("\"schema_version\":1"));
        assert!(json.contains("\"kind\":\"import\""));
    }
}
