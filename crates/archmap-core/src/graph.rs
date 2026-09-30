use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};

use crate::{
    Component, ComponentId, Edge, EdgeKind, Evidence, GraphFragment, Symbol, SymbolId,
    UnresolvedImport, SCHEMA_VERSION,
};

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
    /// Imports that resolve to nothing internal, standard or declared.
    /// Observations for `check`, never edges.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unresolved_imports: Vec<UnresolvedImport>,
}

impl Default for ArchitectureGraph {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            meta: GraphMeta::default(),
            components: BTreeMap::new(),
            symbols: BTreeMap::new(),
            edges: Vec::new(),
            unresolved_imports: Vec::new(),
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
    ///
    /// This is linear in the number of edges. Use [`Self::add_edges`] (or
    /// [`Self::merge`]) to insert many edges.
    pub fn add_edge(&mut self, edge: Edge) {
        if let Some(existing) = self.edges.iter_mut().find(|e| e.same_relationship(&edge)) {
            merge_evidence(&mut existing.evidence, edge.evidence);
        } else {
            self.edges.push(edge);
        }
    }

    /// Insert many edges at once. Each relationship collapses into one edge
    /// holding the union of its evidence, as with repeated
    /// [`Self::add_edge`], but in O(n log n) rather than O(n²). Edges and
    /// their evidence come out in normalized order.
    pub fn add_edges(&mut self, edges: impl IntoIterator<Item = Edge>) {
        let mut merged: BTreeMap<(ComponentId, ComponentId, EdgeKind), BTreeSet<Evidence>> =
            BTreeMap::new();
        for edge in std::mem::take(&mut self.edges).into_iter().chain(edges) {
            merged
                .entry((edge.from, edge.to, edge.kind))
                .or_default()
                .extend(edge.evidence);
        }
        self.edges = merged
            .into_iter()
            .map(|((from, to, kind), evidence)| Edge {
                from,
                to,
                kind,
                evidence: evidence.into_iter().collect(),
            })
            .collect();
    }

    /// Merge an analyzer fragment into the graph.
    pub fn merge(&mut self, fragment: GraphFragment) {
        for component in fragment.components {
            self.add_component(component);
        }
        for symbol in fragment.symbols {
            self.add_symbol(symbol);
        }
        self.add_edges(fragment.edges);
        self.unresolved_imports.extend(fragment.unresolved_imports);
    }

    /// Sort edges so that serialized output is stable regardless of the
    /// order analyzers ran in.
    pub fn normalize(&mut self) {
        self.edges
            .sort_by(|a, b| (&a.from, &a.to, a.kind).cmp(&(&b.from, &b.to, b.kind)));
        for edge in &mut self.edges {
            edge.evidence.sort();
        }
        self.unresolved_imports.sort();
        self.unresolved_imports.dedup();
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

    /// Groups of components that reach each other through dependency
    /// cycles, over every edge kind. Each group has at least two members and
    /// is sorted; groups are sorted too.
    pub fn cycles(&self) -> Vec<Vec<ComponentId>> {
        let mut forward: BTreeMap<&ComponentId, BTreeSet<&ComponentId>> = BTreeMap::new();
        let mut backward: BTreeMap<&ComponentId, BTreeSet<&ComponentId>> = BTreeMap::new();
        for edge in &self.edges {
            if edge.from != edge.to {
                forward.entry(&edge.from).or_default().insert(&edge.to);
                backward.entry(&edge.to).or_default().insert(&edge.from);
            }
        }
        let forward: BTreeMap<&ComponentId, Vec<&ComponentId>> = forward
            .into_iter()
            .map(|(k, v)| (k, v.into_iter().collect()))
            .collect();
        let nodes: BTreeSet<&ComponentId> =
            forward.keys().chain(backward.keys()).copied().collect();

        // Kosaraju, iteratively so that long chains cannot exhaust the stack:
        // finish order on the graph, then components on the reversed graph.
        let mut visited: BTreeSet<&ComponentId> = BTreeSet::new();
        let mut order: Vec<&ComponentId> = Vec::new();
        for &start in &nodes {
            if !visited.insert(start) {
                continue;
            }
            let mut stack: Vec<(&ComponentId, usize)> = vec![(start, 0)];
            while let Some(top) = stack.last_mut() {
                let node = top.0;
                let next = forward.get(node).and_then(|succ| succ.get(top.1)).copied();
                top.1 += 1;
                match next {
                    Some(next) => {
                        if visited.insert(next) {
                            stack.push((next, 0));
                        }
                    }
                    None => {
                        order.push(node);
                        stack.pop();
                    }
                }
            }
        }

        let mut assigned: BTreeSet<&ComponentId> = BTreeSet::new();
        let mut groups = Vec::new();
        for &start in order.iter().rev() {
            if !assigned.insert(start) {
                continue;
            }
            let mut group = vec![start.clone()];
            let mut stack = vec![start];
            while let Some(node) = stack.pop() {
                for &prev in backward.get(node).into_iter().flatten() {
                    if assigned.insert(prev) {
                        group.push(prev.clone());
                        stack.push(prev);
                    }
                }
            }
            if group.len() > 1 {
                group.sort();
                groups.push(group);
            }
        }
        groups.sort();
        groups
    }

    /// Components from the containment root down to `id`, following
    /// `parent`. The walk stops at unknown ids and at cycles, so malformed
    /// input cannot loop.
    pub fn containment_path(&self, id: &ComponentId) -> Vec<ComponentId> {
        let mut path = vec![id.clone()];
        let mut current = id.clone();
        while let Some(parent) = self.components.get(&current).and_then(|c| c.parent.clone()) {
            if path.contains(&parent) || !self.components.contains_key(&parent) {
                break;
            }
            path.push(parent.clone());
            current = parent;
        }
        path.reverse();
        path
    }

    /// Depth in the containment tree; components without a parent are 0.
    pub fn depth_of(&self, id: &ComponentId) -> usize {
        self.containment_path(id).len() - 1
    }

    /// The ancestor of `id` at `depth`, or `id` itself when it is not deeper.
    pub fn ancestor_at(&self, id: &ComponentId, depth: usize) -> ComponentId {
        let path = self.containment_path(id);
        path[depth.min(path.len() - 1)].clone()
    }

    /// Structural roll-up: fold every component deeper than `depth` into its
    /// ancestor at `depth`.
    ///
    /// Edges are re-pointed to the folded components and merged per
    /// relationship with all their evidence, so a rolled-up edge still leads
    /// back to every import statement or manifest entry behind it. Edges that
    /// end up inside one component are dropped. Symbols move to their folded
    /// component. Nothing is renamed or grouped by meaning.
    pub fn rollup(&self, depth: usize) -> ArchitectureGraph {
        let fold: BTreeMap<&ComponentId, ComponentId> = self
            .components
            .keys()
            .map(|id| (id, self.ancestor_at(id, depth)))
            .collect();
        let folded = |id: &ComponentId| fold.get(id).cloned().unwrap_or_else(|| id.clone());

        let mut out = ArchitectureGraph::new(self.meta.clone());
        for component in self.components.values() {
            if folded(&component.id) == component.id {
                out.add_component(component.clone());
            }
        }
        for symbol in self.symbols.values() {
            let mut symbol = symbol.clone();
            symbol.component = folded(&symbol.component);
            out.add_symbol(symbol);
        }

        out.unresolved_imports = self
            .unresolved_imports
            .iter()
            .map(|import| UnresolvedImport {
                from: folded(&import.from),
                ..import.clone()
            })
            .collect();
        out.unresolved_imports.sort();
        out.add_edges(self.edges.iter().filter_map(|edge| {
            let (from, to) = (folded(&edge.from), folded(&edge.to));
            (from != to).then(|| Edge {
                from,
                to,
                kind: edge.kind,
                evidence: edge.evidence.clone(),
            })
        }));
        out
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

fn merge_evidence(into: &mut Vec<Evidence>, from: Vec<Evidence>) {
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

    /// pkg <- a <- a.b <- {a.b.c, a.b.d} ; pkg <- x <- x.y <- x.y.z ; ext:serde
    fn tree() -> ArchitectureGraph {
        let mut graph = ArchitectureGraph::default();
        graph.add_component(component("pkg"));
        graph.add_component(Component::new(
            "ext:serde",
            "serde",
            ComponentKind::External,
        ));
        for (id, parent) in [
            ("a", "pkg"),
            ("a.b", "a"),
            ("a.b.c", "a.b"),
            ("a.b.d", "a.b"),
            ("x", "pkg"),
            ("x.y", "x"),
            ("x.y.z", "x.y"),
        ] {
            let mut c = Component::new(id, id, ComponentKind::Module);
            c.parent = Some(parent.into());
            graph.add_component(c);
        }
        graph
    }

    #[test]
    fn ancestor_at_walks_the_containment_tree() {
        let graph = tree();
        let at = |id: &str, depth| graph.ancestor_at(&id.into(), depth);
        assert_eq!(at("a.b.c", 0), "pkg".into());
        assert_eq!(at("a.b.c", 1), "a".into());
        assert_eq!(at("a.b.c", 2), "a.b".into());
        assert_eq!(at("a.b.c", 9), "a.b.c".into());
        assert_eq!(at("x", 2), "x".into());
        assert_eq!(at("unknown", 1), "unknown".into());
        assert_eq!(graph.depth_of(&"a.b.c".into()), 3);
        assert_eq!(graph.depth_of(&"pkg".into()), 0);
    }

    #[test]
    fn containment_cycles_terminate() {
        let mut graph = ArchitectureGraph::default();
        let mut p = component("p");
        p.parent = Some("q".into());
        let mut q = component("q");
        q.parent = Some("p".into());
        graph.add_component(p);
        graph.add_component(q);
        assert_eq!(graph.containment_path(&"p".into()).len(), 2);
        assert_eq!(graph.ancestor_at(&"p".into(), 0), "q".into());
    }

    #[test]
    fn rollup_folds_components_and_merges_edges_with_evidence() {
        let mut graph = tree();
        graph.add_edge(edge("a.b.c", "x.y.z", "a/b/c.py", 1));
        graph.add_edge(edge("a.b.d", "x.y", "a/b/d.py", 2));
        graph.add_edge(edge("a.b.c", "a.b.d", "a/b/c.py", 3)); // becomes internal
        graph.add_edge(edge("a.b.c", "ext:serde", "a/b/c.py", 4));

        let rolled = graph.rollup(2);
        let ids: Vec<&str> = rolled.components.keys().map(|c| c.as_str()).collect();
        assert_eq!(ids, vec!["a", "a.b", "ext:serde", "pkg", "x", "x.y"]);

        let pairs: Vec<(&str, &str, usize)> = rolled
            .edges
            .iter()
            .map(|e| (e.from.as_str(), e.to.as_str(), e.evidence.len()))
            .collect();
        assert_eq!(pairs, vec![("a.b", "ext:serde", 1), ("a.b", "x.y", 2)]);

        // every original import statement is still there
        let lines: Vec<Option<u32>> = rolled.edges[1].evidence.iter().map(|e| e.line).collect();
        assert_eq!(lines, vec![Some(1), Some(2)]);
    }

    #[test]
    fn rollup_to_depth_zero_keeps_only_roots() {
        let mut graph = tree();
        graph.add_edge(edge("a.b.c", "x.y.z", "a/b/c.py", 1));
        graph.add_edge(edge("a.b.c", "ext:serde", "a/b/c.py", 4));
        let rolled = graph.rollup(0);
        let ids: Vec<&str> = rolled.components.keys().map(|c| c.as_str()).collect();
        assert_eq!(ids, vec!["ext:serde", "pkg"]);
        assert_eq!(rolled.edges.len(), 1);
        assert_eq!(rolled.edges[0].from, "pkg".into());
    }

    #[test]
    fn rollup_dedups_shared_evidence_and_keeps_kinds_apart() {
        let mut graph = tree();
        // one statement importing two modules that fold together
        graph.add_edge(edge("a.b.c", "x.y", "a/b/c.py", 7));
        graph.add_edge(edge("a.b.c", "x.y.z", "a/b/c.py", 7));
        graph.add_edge(Edge::new("a.b.c", "x.y.z", EdgeKind::Dependency));
        let rolled = graph.rollup(2);
        assert_eq!(rolled.edges.len(), 2);
        let import = rolled
            .edges
            .iter()
            .find(|e| e.kind == EdgeKind::Import)
            .unwrap();
        assert_eq!(import.evidence.len(), 1);
    }

    #[test]
    fn rollup_moves_symbols_to_folded_component() {
        let mut graph = tree();
        graph.add_symbol(Symbol {
            id: SymbolId::new("pkg::a.b.c::run"),
            name: "run".into(),
            kind: SymbolKind::Function,
            component: "a.b.c".into(),
            signature: None,
            evidence: Vec::new(),
        });
        let rolled = graph.rollup(1);
        assert_eq!(rolled.symbols_of(&"a".into()).count(), 1);
        assert_eq!(rolled.symbols.len(), 1);
    }

    fn ids(groups: Vec<Vec<ComponentId>>) -> Vec<Vec<String>> {
        groups
            .into_iter()
            .map(|g| g.into_iter().map(|c| c.0).collect())
            .collect()
    }

    #[test]
    fn cycles_finds_each_strongly_connected_group() {
        let mut graph = ArchitectureGraph::default();
        for (from, to) in [
            ("a", "b"),
            ("b", "c"),
            ("c", "a"),
            ("c", "d"),
            ("x", "y"),
            ("y", "x"),
            ("s", "s"),
        ] {
            graph.add_edge(Edge::new(from, to, EdgeKind::Import));
        }
        assert_eq!(
            ids(graph.cycles()),
            vec![vec!["a", "b", "c"], vec!["x", "y"]]
        );
    }

    #[test]
    fn acyclic_graphs_have_no_cycles_even_when_long() {
        // edges are pushed directly: `add_edge` deduplicates linearly
        let mut graph = ArchitectureGraph::default();
        for i in 0..20_000 {
            let (from, to) = (format!("m{i}"), format!("m{}", i + 1));
            graph.edges.push(Edge::new(from, to, EdgeKind::Import));
        }
        assert!(graph.cycles().is_empty());
        graph
            .edges
            .push(Edge::new("m20000", "m0", EdgeKind::Dependency));
        assert_eq!(graph.cycles()[0].len(), 20_001);
    }

    #[test]
    fn add_edges_agrees_with_repeated_add_edge() {
        let edges = vec![
            edge("b", "c", "x.py", 2),
            edge("a", "b", "x.py", 1),
            edge("a", "b", "y.py", 5),
            edge("a", "b", "x.py", 1),
            Edge::new("a", "b", EdgeKind::Dependency),
            edge("b", "c", "x.py", 1),
        ];
        let mut one_by_one = ArchitectureGraph::default();
        for e in edges.clone() {
            one_by_one.add_edge(e);
        }
        one_by_one.normalize();

        let mut bulk = ArchitectureGraph::default();
        bulk.add_edge(edges[0].clone());
        bulk.add_edges(edges[1..].to_vec());
        assert_eq!(bulk, one_by_one);
    }

    #[test]
    fn merging_a_large_fragment_is_not_quadratic() {
        // 100k distinct edges: quadratic insertion would take minutes here
        let mut fragment = GraphFragment::new();
        for i in 0..100_000 {
            let (from, to) = (format!("m{}", i % 1000), format!("m{}", i / 1000 + 1000));
            fragment.push_edge(Edge::new(from, to, EdgeKind::Import));
        }
        let mut graph = ArchitectureGraph::default();
        graph.merge(fragment);
        assert_eq!(graph.edges.len(), 100_000);
    }

    #[test]
    fn rollup_keeps_unresolved_imports_on_the_folded_component() {
        let mut graph = tree();
        graph.unresolved_imports.push(UnresolvedImport {
            from: "a.b.c".into(),
            module: "scipy".into(),
            provided_by: vec![],
            evidence: Evidence::new("a/b/c.py").at_line(1),
        });
        let rolled = graph.rollup(1);
        assert_eq!(rolled.unresolved_imports.len(), 1);
        assert_eq!(rolled.unresolved_imports[0].from, "a".into());
        assert!(
            rolled.edges.is_empty(),
            "an unresolved import is never an edge"
        );
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
