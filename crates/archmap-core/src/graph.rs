use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};

use crate::{
    Component, ComponentId, DynamicImport, Edge, EdgeKind, Evidence, GraphFragment,
    LanguageCoverage, Symbol, SymbolId, UnmappedImport, SCHEMA_VERSION,
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
    /// What the scan saw of each language, including languages no analyzer
    /// reads.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub coverage: BTreeMap<String, LanguageCoverage>,
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
    /// Imports that map to no component, standard library aside.
    /// Observations, never edges.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unmapped_imports: Vec<UnmappedImport>,
    /// Modules loaded by names computed at runtime. Observations, never
    /// edges.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dynamic_imports: Vec<DynamicImport>,
}

impl Default for ArchitectureGraph {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            meta: GraphMeta::default(),
            components: BTreeMap::new(),
            symbols: BTreeMap::new(),
            edges: Vec::new(),
            unmapped_imports: Vec::new(),
            dynamic_imports: Vec::new(),
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
        self.unmapped_imports.extend(fragment.unmapped_imports);
        self.dynamic_imports.extend(fragment.dynamic_imports);
    }

    /// Sort edges so that serialized output is stable regardless of the
    /// order analyzers ran in.
    pub fn normalize(&mut self) {
        self.edges
            .sort_by(|a, b| (&a.from, &a.to, a.kind).cmp(&(&b.from, &b.to, b.kind)));
        for edge in &mut self.edges {
            edge.evidence.sort();
        }
        self.unmapped_imports.sort();
        self.unmapped_imports.dedup();
        self.dynamic_imports.sort();
        self.dynamic_imports.dedup();
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
        strongly_connected(self.edges.iter().map(|e| (&e.from, &e.to)))
            .into_iter()
            .map(|group| group.into_iter().cloned().collect())
            .collect()
    }

    /// Components that may be affected by a change, folded to `depth`.
    ///
    /// Dependencies whose evidence names the imported file are followed
    /// file by file, so a component is reached only through files that
    /// import what changed, not through any file of a shared component.
    /// Dependencies without that detail (manifests, external packages,
    /// languages that do not record target files) are followed component
    /// by component.
    pub fn change_impact(&self, seed: ChangeSeed, depth: usize) -> Reach {
        let mut dependents: BTreeMap<Node, BTreeSet<Node>> = BTreeMap::new();
        let mut files: BTreeSet<&str> = BTreeSet::new();
        for edge in &self.edges {
            for e in &edge.evidence {
                let importer = Node::File(e.file.as_str());
                files.insert(e.file.as_str());
                let target = match e.target.as_deref() {
                    Some(t) => {
                        files.insert(t);
                        Node::File(t)
                    }
                    None => Node::Component(&edge.to),
                };
                if importer != target {
                    dependents.entry(target).or_default().insert(importer);
                }
            }
            if edge.evidence.is_empty() && edge.from != edge.to {
                dependents
                    .entry(Node::Component(&edge.to))
                    .or_default()
                    .insert(Node::Component(&edge.from));
            }
        }
        let index = PathIndex::new(self);
        let owners: BTreeMap<&str, &ComponentId> = files
            .iter()
            .filter_map(|f| index.owner(f).map(|c| (*f, &c.id)))
            .collect();
        let owner_of = |f: &str| {
            owners
                .get(f)
                .copied()
                .or_else(|| index.owner(f).map(|c| &c.id))
        };

        let mut start: Vec<Node> = Vec::new();
        let target = match seed {
            ChangeSeed::File(file) => {
                start.push(Node::File(file));
                owner_of(file).map(|c| self.ancestor_at(c, depth))
            }
            ChangeSeed::Component(component) => {
                let subtree: BTreeSet<&ComponentId> = self
                    .components
                    .keys()
                    .filter(|id| self.containment_path(id).contains(component))
                    .collect();
                start.extend(subtree.iter().map(|id| Node::Component(id)));
                start.extend(
                    owners
                        .iter()
                        .filter(|(_, owner)| subtree.contains(*owner))
                        .map(|(file, _)| Node::File(file)),
                );
                Some(self.ancestor_at(component, depth))
            }
        };

        // 0-1 BFS: a reached file puts its component in reach at no cost.
        let mut distance: BTreeMap<Node, usize> = BTreeMap::new();
        let mut queue: VecDeque<Node> = VecDeque::new();
        for node in start {
            if distance.insert(node, 0).is_none() {
                queue.push_back(node);
            }
        }
        while let Some(node) = queue.pop_front() {
            let d = distance[&node];
            let mut next: Vec<(Node, usize)> = Vec::new();
            if let Node::File(f) = node {
                if let Some(owner) = owner_of(f) {
                    next.push((Node::Component(owner), d));
                }
            }
            next.extend(
                dependents
                    .get(&node)
                    .into_iter()
                    .flatten()
                    .map(|n| (*n, d + 1)),
            );
            for (n, nd) in next {
                if distance.get(&n).is_none_or(|&old| nd < old) {
                    distance.insert(n, nd);
                    if nd == d {
                        queue.push_front(n);
                    } else {
                        queue.push_back(n);
                    }
                }
            }
        }

        let mut reach = Reach::default();
        for (node, d) in &distance {
            let component = match node {
                Node::File(f) => owner_of(f),
                Node::Component(c) => Some(*c),
            };
            let Some(component) = component else {
                continue;
            };
            let folded = self.ancestor_at(component, depth);
            if *d == 0 || Some(&folded) == target.as_ref() {
                continue;
            }
            if *d == 1 {
                reach.direct.insert(folded.clone());
            }
            reach.transitive.insert(folded);
        }
        reach
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

        out.unmapped_imports = self
            .unmapped_imports
            .iter()
            .map(|import| UnmappedImport {
                from: folded(&import.from),
                ..import.clone()
            })
            .collect();
        out.unmapped_imports.sort();
        out.dynamic_imports = self
            .dynamic_imports
            .iter()
            .map(|import| DynamicImport {
                from: folded(&import.from),
                ..import.clone()
            })
            .collect();
        out.dynamic_imports.sort();
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

    /// Files the graph holds evidence about: where public symbols are defined,
    /// where imports are written, and which files imports load.
    pub fn known_files(&self) -> BTreeSet<&str> {
        let mut files: BTreeSet<&str> = BTreeSet::new();
        for symbol in self.symbols.values() {
            files.extend(symbol.evidence.iter().map(|e| e.file.as_str()));
        }
        for edge in self.edges.iter().filter(|e| e.kind == EdgeKind::Import) {
            for e in &edge.evidence {
                files.insert(e.file.as_str());
                files.extend(e.target.as_deref());
            }
        }
        files.extend(
            self.unmapped_imports
                .iter()
                .map(|i| i.evidence.file.as_str()),
        );
        files.extend(
            self.dynamic_imports
                .iter()
                .map(|i| i.evidence.file.as_str()),
        );
        files
    }

    /// The known file addressed as `<component name>.<file stem>`, such as
    /// `shop.billing.charge` for `src/shop/billing/charge.py` directly inside
    /// the component `shop.billing`. `None` when no file or several match.
    pub fn file_for_dotted_name(&self, name: &str) -> Option<&str> {
        let (prefix, stem) = name.rsplit_once('.')?;
        let index = PathIndex::new(self);
        let mut found = self.known_files().into_iter().filter(|file| {
            let (dir, file_name) = file.rsplit_once('/').unwrap_or((".", file));
            let file_stem = file_name.rsplit_once('.').map_or(file_name, |(s, _)| s);
            file_stem == stem
                && index.owner(file).is_some_and(|c| {
                    c.name == prefix
                        && c.path.as_deref().map(|p| p.trim_end_matches('/')) == Some(dir)
                })
        });
        match (found.next(), found.next()) {
            (Some(file), None) => Some(file),
            _ => None,
        }
    }

    /// Everything the graph records about `file`: its public symbols, the
    /// imports it writes, the imports elsewhere that load it, and the imports
    /// in it that map to no component.
    pub fn file_facts(&self, file: &str) -> FileFacts<'_> {
        let file = file.trim_start_matches("./");
        let component = self.component_for_path(file);
        let language = component.and_then(|c| c.language.as_deref());
        let (mut imports, mut importers, mut recorded) = (Vec::new(), Vec::new(), false);
        for edge in self.edges.iter().filter(|e| e.kind == EdgeKind::Import) {
            let to_language = self.component(&edge.to).and_then(|c| c.language.as_deref());
            for e in &edge.evidence {
                if e.file == file {
                    imports.push((edge, e));
                }
                if e.target.as_deref() == Some(file) {
                    importers.push((edge, e));
                }
                recorded |= e.target.is_some() && to_language == language;
            }
        }
        FileFacts {
            file: file.to_owned(),
            component: component.map(|c| &c.id),
            symbols: self
                .symbols
                .values()
                .filter(|s| s.evidence.iter().any(|e| e.file == file))
                .collect(),
            importers_recorded: recorded || !importers.is_empty(),
            imports,
            importers,
            unmapped_imports: self
                .unmapped_imports
                .iter()
                .filter(|i| i.evidence.file == file)
                .collect(),
            dynamic_imports: self
                .dynamic_imports
                .iter()
                .filter(|i| i.evidence.file == file)
                .collect(),
        }
    }

    /// Imports without an edge of `module` or of a module below it, the
    /// way an import names them: `torch` covers `torch` and `torch.nn`, not
    /// `torchvision`.
    pub fn unmapped_imports_of<'a>(
        &'a self,
        module: &'a str,
    ) -> impl Iterator<Item = &'a UnmappedImport> + 'a {
        self.unmapped_imports
            .iter()
            .filter(move |i| i.covered_by(module))
    }

    /// Find the component that owns a file path (relative to the repo root),
    /// choosing the component with the longest matching `path` prefix.
    /// Components of different analyzers can share a path (a Python package
    /// that keeps scripts below it); evidence then decides (see
    /// [`PathIndex::owner`]).
    pub fn component_for_path(&self, file: &str) -> Option<&Component> {
        PathIndex::new(self).owner(file)
    }
}

/// What the graph records about one file, for a file-level query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileFacts<'a> {
    /// The file, relative to the root.
    pub file: String,
    /// The component whose path contains the file.
    pub component: Option<&'a ComponentId>,
    /// Public symbols defined in the file.
    pub symbols: Vec<&'a Symbol>,
    /// Import statements written in the file, with their edge.
    pub imports: Vec<(&'a Edge, &'a Evidence)>,
    /// Import statements elsewhere that load the file, with their edge.
    pub importers: Vec<(&'a Edge, &'a Evidence)>,
    /// Whether evidence names imported files for the file's language at all.
    /// Without it, `importers` is unknown rather than empty.
    pub importers_recorded: bool,
    pub unmapped_imports: Vec<&'a UnmappedImport>,
    pub dynamic_imports: Vec<&'a DynamicImport>,
}

/// Where a change starts, for [`ArchitectureGraph::change_impact`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeSeed<'a> {
    /// A component and everything it contains.
    Component(&'a ComponentId),
    /// One file, relative to the repository root.
    File(&'a str),
}

/// Components that may be affected by a change.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reach {
    /// Components with a file that depends on the change directly.
    pub direct: BTreeSet<ComponentId>,
    /// Every component reached, `direct` included.
    pub transitive: BTreeSet<ComponentId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Node<'a> {
    File(&'a str),
    Component(&'a ComponentId),
}

/// Strongly connected groups of at least two nodes, each sorted, in sorted
/// order. Iterative Kosaraju, so long chains cannot exhaust the stack.
pub(crate) fn strongly_connected<N: Ord + Copy>(
    pairs: impl IntoIterator<Item = (N, N)>,
) -> Vec<Vec<N>> {
    let mut forward: BTreeMap<N, BTreeSet<N>> = BTreeMap::new();
    let mut backward: BTreeMap<N, BTreeSet<N>> = BTreeMap::new();
    for (a, b) in pairs {
        if a != b {
            forward.entry(a).or_default().insert(b);
            backward.entry(b).or_default().insert(a);
        }
    }
    let forward: BTreeMap<N, Vec<N>> = forward
        .into_iter()
        .map(|(k, v)| (k, v.into_iter().collect()))
        .collect();
    let nodes: BTreeSet<N> = forward.keys().chain(backward.keys()).copied().collect();

    let mut visited: BTreeSet<N> = BTreeSet::new();
    let mut order: Vec<N> = Vec::new();
    for &start in &nodes {
        if !visited.insert(start) {
            continue;
        }
        let mut stack: Vec<(N, usize)> = vec![(start, 0)];
        while let Some(top) = stack.last_mut() {
            let node = top.0;
            let next = forward.get(&node).and_then(|succ| succ.get(top.1)).copied();
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

    let mut assigned: BTreeSet<N> = BTreeSet::new();
    let mut groups = Vec::new();
    for &start in order.iter().rev() {
        if !assigned.insert(start) {
            continue;
        }
        let mut group = vec![start];
        let mut stack = vec![start];
        while let Some(node) = stack.pop() {
            for &prev in backward.get(&node).into_iter().flatten() {
                if assigned.insert(prev) {
                    group.push(prev);
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

/// Components by the path they cover, to find the owners of many files: a
/// lookup walks the file's ancestors rather than every component.
struct PathIndex<'a> {
    graph: &'a ArchitectureGraph,
    /// Components by [`normalized`] path, in id order.
    by_path: BTreeMap<&'a str, Vec<&'a Component>>,
    /// What breaks a tie, gathered on the first one.
    ties: OnceCell<TieEvidence<'a>>,
}

impl<'a> PathIndex<'a> {
    fn new(graph: &'a ArchitectureGraph) -> Self {
        let mut by_path: BTreeMap<&str, Vec<&Component>> = BTreeMap::new();
        for c in graph.components.values() {
            if let Some(path) = c.path.as_deref() {
                by_path.entry(normalized(path)).or_default().push(c);
            }
        }
        PathIndex {
            graph,
            by_path,
            ties: OnceCell::new(),
        }
    }

    /// The component whose path is the longest prefix of `file`. Among
    /// components that share that path, the one with evidence in `file`,
    /// else one with evidence in a file of the same kind beside it, else
    /// the last id; for the shared directory itself, evidence in the files
    /// directly inside it decides.
    fn owner(&self, file: &str) -> Option<&'a Component> {
        let file = normalized(file);
        let mut at = file;
        loop {
            if let Some(tied) = self.by_path.get(at) {
                return match tied.as_slice() {
                    [only] => Some(*only),
                    _ => Some(self.owner_among(tied, file, at == file)),
                };
            }
            if at.is_empty() {
                return None;
            }
            at = parent_dir(at);
        }
    }

    fn owner_among(&self, tied: &[&'a Component], file: &str, directory: bool) -> &'a Component {
        let ties = self.ties.get_or_init(|| TieEvidence::of(self.graph));
        let has = |set: Option<&BTreeSet<&ComponentId>>, id: &ComponentId| {
            set.is_some_and(|s| s.contains(id))
        };
        let score = |c: &Component| -> u8 {
            if directory {
                u8::from(has(ties.in_dir.get(file), &c.id))
            } else if has(ties.in_file.get(file), &c.id) {
                2
            } else {
                let beside = match extension(file) {
                    Some(extension) => ties.in_dir_ext.get(&(parent_dir(file), extension)),
                    None => ties.in_dir.get(parent_dir(file)),
                };
                u8::from(has(beside, &c.id))
            }
        };
        tied.iter()
            .copied()
            .max_by_key(|c| score(c))
            .unwrap_or(tied[0])
    }
}

/// Which components have evidence (their own, their symbols', their edges'
/// and imports') in each file, and in the files of each directory.
struct TieEvidence<'a> {
    in_file: BTreeMap<&'a str, BTreeSet<&'a ComponentId>>,
    in_dir: BTreeMap<&'a str, BTreeSet<&'a ComponentId>>,
    in_dir_ext: BTreeMap<(&'a str, &'a str), BTreeSet<&'a ComponentId>>,
}

impl<'a> TieEvidence<'a> {
    fn of(graph: &'a ArchitectureGraph) -> Self {
        let mut in_file: BTreeMap<&str, BTreeSet<&ComponentId>> = BTreeMap::new();
        let mut add = |id: &'a ComponentId, file: &'a str| {
            in_file.entry(normalized(file)).or_default().insert(id);
        };
        for c in graph.components.values() {
            for e in &c.evidence {
                add(&c.id, &e.file);
            }
        }
        for s in graph.symbols.values() {
            for e in &s.evidence {
                add(&s.component, &e.file);
            }
        }
        for edge in &graph.edges {
            for e in &edge.evidence {
                add(&edge.from, &e.file);
            }
        }
        for i in &graph.unmapped_imports {
            add(&i.from, &i.evidence.file);
        }
        for i in &graph.dynamic_imports {
            add(&i.from, &i.evidence.file);
        }
        let mut in_dir: BTreeMap<&str, BTreeSet<&ComponentId>> = BTreeMap::new();
        let mut in_dir_ext: BTreeMap<(&str, &str), BTreeSet<&ComponentId>> = BTreeMap::new();
        for (&file, ids) in &in_file {
            let dir = parent_dir(file);
            in_dir.entry(dir).or_default().extend(ids.iter().copied());
            if let Some(extension) = extension(file) {
                in_dir_ext
                    .entry((dir, extension))
                    .or_default()
                    .extend(ids.iter().copied());
            }
        }
        TieEvidence {
            in_file,
            in_dir,
            in_dir_ext,
        }
    }
}

/// A path as `component_for_path` compares it: without `./` or a trailing
/// `/`, and the root as `""`.
fn normalized(path: &str) -> &str {
    let path = path.trim_start_matches("./").trim_end_matches('/');
    if path == "." {
        ""
    } else {
        path
    }
}

/// The directory of a `/`-separated path, `""` at the root.
fn parent_dir(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// The extension of a `/`-separated path's file name, if any.
fn extension(path: &str) -> Option<&str> {
    let name = path.rsplit_once('/').map_or(path, |(_, name)| name);
    name.rsplit_once('.')
        .map(|(_, extension)| extension)
        .filter(|e| !e.is_empty())
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
    use crate::{ComponentKind, DynamicImport, EdgeKind, Evidence, SymbolKind, UnmappedReason};

    fn component(id: &str) -> Component {
        Component::new(id, id, ComponentKind::Package)
    }

    fn edge(from: &str, to: &str, file: &str, line: u32) -> Edge {
        Edge::new(from, to, EdgeKind::Import).with_evidence(Evidence::new(file).at_line(line))
    }

    #[test]
    fn imports_without_an_edge_are_found_by_dotted_prefix() {
        let mut g = ArchitectureGraph::default();
        for module in [
            "torch",
            "torch.nn",
            "torchvision",
            "google.api_core.exceptions",
        ] {
            g.unmapped_imports.push(UnmappedImport {
                from: "p".into(),
                module: module.into(),
                reason: UnmappedReason::DeclaredNotRequired,
                provided_by: vec![],
                evidence: Evidence::new("a.py"),
            });
        }
        let of = |m: &str| -> Vec<String> {
            g.unmapped_imports_of(m).map(|i| i.module.clone()).collect()
        };
        assert_eq!(of("torch"), vec!["torch", "torch.nn"]);
        assert_eq!(of("google.api_core"), vec!["google.api_core.exceptions"]);
        assert!(of("google.api").is_empty());
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
    fn component_for_path_gives_a_shared_path_to_the_component_with_evidence_there() {
        // A Python package and a TS/JS directory at one path, as when an
        // app keeps its scripts in `myapp/static/`. The later id must not
        // take the Python files.
        let mut graph = ArchitectureGraph::default();
        for id in ["py::myapp", "ts::myapp"] {
            let mut c = component(id);
            c.path = Some("myapp".into());
            graph.add_component(c);
        }
        // a TS/JS directory names itself as its evidence
        graph
            .components
            .get_mut(&ComponentId::new("ts::myapp"))
            .unwrap()
            .evidence
            .push(Evidence::new("myapp").with_note("directory"));
        let mut ts_file = component("ts::myapp/static/app.js");
        ts_file.path = Some("myapp/static/app.js".into());
        graph.add_component(ts_file);
        graph.add_symbol(Symbol {
            id: "py::myapp::index".into(),
            name: "index".into(),
            kind: SymbolKind::Function,
            component: "py::myapp".into(),
            signature: None,
            evidence: vec![Evidence::new("myapp/views.py").at_line(1)],
        });
        graph.add_edge(edge("py::myapp", "ext:pypi:django", "myapp/urls.py", 1));
        graph.add_edge(edge(
            "ts::myapp/static/app.js",
            "ts::myapp/static/app.js",
            "myapp/static/app.js",
            1,
        ));
        let owner = |file: &str| graph.component_for_path(file).unwrap().id.clone();
        assert_eq!(owner("myapp/views.py"), "py::myapp".into());
        assert_eq!(owner("myapp/urls.py"), "py::myapp".into());
        // no evidence names it, but its neighbours of the same kind do
        assert_eq!(owner("myapp/apps.py"), "py::myapp".into());
        // the directory goes to the component with files directly in it
        assert_eq!(owner("myapp"), "py::myapp".into());
        assert_eq!(
            owner("myapp/static/app.js"),
            "ts::myapp/static/app.js".into()
        );
    }

    #[test]
    fn change_impact_gives_files_at_a_shared_path_to_the_component_with_evidence_there() {
        let mut graph = ArchitectureGraph::default();
        for (id, path) in [
            ("py::myapp", "myapp"),
            ("ts::myapp", "myapp"),
            ("py::util", "util"),
        ] {
            let mut c = component(id);
            c.path = Some(path.into());
            graph.add_component(c);
        }
        graph.add_edge(
            Edge::new("py::myapp", "py::util", EdgeKind::Import).with_evidence(
                Evidence::new("myapp/views.py")
                    .at_line(1)
                    .pointing_at("util/log.py"),
            ),
        );
        let reach = graph.change_impact(ChangeSeed::File("util/log.py"), 9);
        assert_eq!(reach.direct, BTreeSet::from(["py::myapp".into()]));
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

    /// pkg <- a <- a.b <- {a.b.c, a.b.d} ; pkg <- x <- x.y <- x.y.z ; ext:cargo:serde
    fn tree() -> ArchitectureGraph {
        let mut graph = ArchitectureGraph::default();
        graph.add_component(component("pkg"));
        graph.add_component(Component::new(
            "ext:cargo:serde",
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
        graph.add_edge(edge("a.b.c", "ext:cargo:serde", "a/b/c.py", 4));

        let rolled = graph.rollup(2);
        let ids: Vec<&str> = rolled.components.keys().map(|c| c.as_str()).collect();
        assert_eq!(ids, vec!["a", "a.b", "ext:cargo:serde", "pkg", "x", "x.y"]);

        let pairs: Vec<(&str, &str, usize)> = rolled
            .edges
            .iter()
            .map(|e| (e.from.as_str(), e.to.as_str(), e.evidence.len()))
            .collect();
        assert_eq!(
            pairs,
            vec![("a.b", "ext:cargo:serde", 1), ("a.b", "x.y", 2)]
        );

        // every original import statement is still there
        let lines: Vec<Option<u32>> = rolled.edges[1].evidence.iter().map(|e| e.line).collect();
        assert_eq!(lines, vec![Some(1), Some(2)]);
    }

    #[test]
    fn rollup_to_depth_zero_keeps_only_roots() {
        let mut graph = tree();
        graph.add_edge(edge("a.b.c", "x.y.z", "a/b/c.py", 1));
        graph.add_edge(edge("a.b.c", "ext:cargo:serde", "a/b/c.py", 4));
        let rolled = graph.rollup(0);
        let ids: Vec<&str> = rolled.components.keys().map(|c| c.as_str()).collect();
        assert_eq!(ids, vec!["ext:cargo:serde", "pkg"]);
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
    fn rollup_keeps_unmapped_imports_on_the_folded_component() {
        let mut graph = tree();
        graph.unmapped_imports.push(UnmappedImport {
            from: "a.b.c".into(),
            module: "scipy".into(),
            reason: UnmappedReason::Undeclared,
            provided_by: vec![],
            evidence: Evidence::new("a/b/c.py").at_line(1),
        });
        let rolled = graph.rollup(1);
        assert_eq!(rolled.unmapped_imports.len(), 1);
        assert_eq!(rolled.unmapped_imports[0].from, "a".into());
        assert!(
            rolled.edges.is_empty(),
            "an unmapped import is never an edge"
        );
    }

    /// app.py -> util/log.py ; util/store.py -> core/types.py ;
    /// core/types.py -> util/log.py ; pipeline.py -> util/store.py
    fn files_graph() -> ArchitectureGraph {
        let mut graph = ArchitectureGraph::default();
        for (id, path) in [
            ("app", "app"),
            ("util", "util"),
            ("core", "core"),
            ("pipeline", "pipeline"),
        ] {
            let mut c = Component::new(id, id, ComponentKind::Module);
            c.path = Some(path.into());
            graph.add_component(c);
        }
        let dep = |from: &str, to: &str, file: &str, target: &str| {
            Edge::new(from, to, EdgeKind::Import)
                .with_evidence(Evidence::new(file).at_line(1).pointing_at(target))
        };
        graph.add_edges([
            dep("app", "util", "app/main.py", "util/log.py"),
            dep("util", "core", "util/store.py", "core/types.py"),
            dep("core", "util", "core/types.py", "util/log.py"),
            dep("pipeline", "util", "pipeline/run.py", "util/store.py"),
        ]);
        graph
    }

    #[test]
    fn file_facts_collect_what_a_file_imports_and_who_imports_it() {
        let graph = files_graph();
        let log = graph.file_facts("util/log.py");
        assert_eq!(log.component, Some(&ComponentId::new("util")));
        assert!(log.imports.is_empty());
        let importers: Vec<(&str, Option<u32>)> = log
            .importers
            .iter()
            .map(|(_, e)| (e.file.as_str(), e.line))
            .collect();
        assert_eq!(
            importers,
            vec![("app/main.py", Some(1)), ("core/types.py", Some(1))]
        );
        assert!(log.importers_recorded);

        let store = graph.file_facts("util/store.py");
        let imports: Vec<(&str, Option<&str>)> = store
            .imports
            .iter()
            .map(|(edge, e)| (edge.to.as_str(), e.target.as_deref()))
            .collect();
        assert_eq!(imports, vec![("core", Some("core/types.py"))]);
        assert_eq!(store.importers.len(), 1);
        assert_eq!(store.importers[0].1.file, "pipeline/run.py");
    }

    #[test]
    fn importers_are_unknown_when_no_evidence_names_imported_files() {
        let mut graph = ArchitectureGraph::default();
        for id in ["a", "b"] {
            let mut c = Component::new(id, id, ComponentKind::Package);
            c.path = Some(id.into());
            c.language = Some("rust".into());
            graph.add_component(c);
        }
        graph.add_edge(edge("a", "b", "a/src/lib.rs", 3));
        let facts = graph.file_facts("b/src/lib.rs");
        assert!(facts.importers.is_empty());
        assert!(
            !facts.importers_recorded,
            "no target files recorded: unknown, not none"
        );
        assert_eq!(graph.file_facts("a/src/lib.rs").imports.len(), 1);
    }

    #[test]
    fn dotted_names_address_files_directly_inside_a_component() {
        let graph = files_graph();
        assert_eq!(graph.file_for_dotted_name("util.log"), Some("util/log.py"));
        assert_eq!(
            graph.file_for_dotted_name("core.types"),
            Some("core/types.py")
        );
        assert_eq!(graph.file_for_dotted_name("util.nope"), None);
        assert_eq!(
            graph.file_for_dotted_name("util"),
            None,
            "a component is not a file"
        );
        assert!(graph.known_files().contains("pipeline/run.py"));
    }

    #[test]
    fn change_impact_follows_files_not_whole_components() {
        let graph = files_graph();
        // component level: core <-> util makes everything reach everything
        assert_eq!(graph.transitive_dependents(&"core".into()).len(), 3);

        // a change to core/types.py only reaches util/store.py and what uses it
        let reach = graph.change_impact(ChangeSeed::File("core/types.py"), 9);
        let ids = |s: &BTreeSet<ComponentId>| s.iter().map(|c| c.0.clone()).collect::<Vec<_>>();
        assert_eq!(ids(&reach.direct), vec!["util"]);
        assert_eq!(ids(&reach.transitive), vec!["pipeline", "util"]);

        // util/log.py is used by app and core; core/types.py does not reach app
        let reach = graph.change_impact(ChangeSeed::File("util/log.py"), 9);
        assert_eq!(ids(&reach.direct), vec!["app", "core"]);
        assert_eq!(ids(&reach.transitive), vec!["app", "core", "pipeline"]);

        // a whole component starts from all of its files
        let reach = graph.change_impact(ChangeSeed::Component(&"core".into()), 9);
        assert_eq!(ids(&reach.transitive), vec!["pipeline", "util"]);
    }

    #[test]
    fn change_impact_falls_back_to_components_without_file_detail() {
        let mut graph = ArchitectureGraph::default();
        for id in ["a", "b", "c"] {
            let mut c = component(id);
            c.path = Some(id.into());
            graph.add_component(c);
        }
        graph.add_edges([
            Edge::new("b", "a", EdgeKind::Import)
                .with_evidence(Evidence::new("b/lib.rs").at_line(1)),
            Edge::new("c", "b", EdgeKind::Dependency).with_evidence(Evidence::new("c/Cargo.toml")),
        ]);
        let reach = graph.change_impact(ChangeSeed::File("a/lib.rs"), 9);
        let ids: Vec<&str> = reach.transitive.iter().map(|c| c.as_str()).collect();
        assert_eq!(ids, vec!["b", "c"]);
        assert_eq!(reach.direct.len(), 1);
    }

    #[test]
    fn dynamic_imports_merge_sorted_and_fold_like_unmapped_imports() {
        let dynamic = |from: &str, line: u32| DynamicImport {
            from: from.into(),
            call: "import_module".into(),
            evidence: Evidence::new("a/b/c.py").at_line(line),
        };
        let mut graph = tree();
        let mut fragment = GraphFragment::new();
        for import in [
            dynamic("a.b.c", 9),
            dynamic("a.b.c", 2),
            dynamic("a.b.c", 9),
        ] {
            fragment.push_dynamic_import(import);
        }
        graph.merge(fragment);
        graph.normalize();
        assert_eq!(
            graph.dynamic_imports,
            vec![dynamic("a.b.c", 2), dynamic("a.b.c", 9)]
        );

        let rolled = graph.rollup(1);
        assert_eq!(
            rolled.dynamic_imports,
            vec![dynamic("a", 2), dynamic("a", 9)]
        );
        assert!(rolled.edges.is_empty(), "a dynamic import is never an edge");
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
        graph.add_edge(edge("a", "ext:cargo:serde", "src/lib.rs", 1));

        let json = serde_json::to_string(&graph).unwrap();
        let back: ArchitectureGraph = serde_json::from_str(&json).unwrap();
        assert_eq!(graph, back);
        assert!(json.contains("\"schema_version\":3"));
        assert!(json.contains("\"kind\":\"import\""));
    }
}
