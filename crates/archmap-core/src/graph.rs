use std::cell::OnceCell;
use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};

use crate::{
    Component, ComponentId, ComponentKind, DynamicImport, Edge, EdgeKind, Evidence, GraphFragment,
    ImportPlace, LanguageCoverage, Symbol, SymbolId, UnmappedImport, UnreadMacro, SCHEMA_VERSION,
    WHOLE_MODULE,
};

/// Information about how a graph was produced.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphMeta {
    /// Names of the analyzers that contributed fragments. No root: the
    /// graph of a commit is the same in every checkout.
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
    /// Macro calls whose arguments were not read. Observations of what the
    /// scan could not see.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unread_macros: Vec<UnreadMacro>,
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
            unread_macros: Vec::new(),
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
        self.unread_macros.extend(fragment.unread_macros);
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
        self.unread_macros.sort();
        self.unread_macros.dedup();
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
    /// cycles, over every edge kind, counting only dependencies that are there
    /// when the program runs (not imports of types only, nor test code).
    /// Each group has at least two members and is sorted; groups are sorted
    /// too.
    pub fn cycles(&self) -> Vec<Vec<ComponentId>> {
        strongly_connected(
            self.edges
                .iter()
                .filter(|e| e.runs_in_production())
                .map(|e| (&e.from, &e.to)),
        )
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
    /// by component. `direct` and `transitive` follow production code;
    /// `tests` holds the files only test code reaches, the tests to run
    /// again.
    pub fn change_impact(&self, seed: ChangeSeed, depth: usize) -> Reach {
        let test_code: BTreeSet<&str> = self
            .test_code()
            .into_iter()
            .filter(|(_, test)| *test)
            .map(|(file, _)| file)
            .collect();
        let (mut reach, production, _) = self.reach(seed, depth, false, &test_code);
        let (through_tests, with_tests, seeds) = self.reach(seed, depth, true, &test_code);
        reach.left_out = through_tests.left_out;
        let mut test_ways = through_tests.test_ways;
        for (entry, from) in through_tests.relayed {
            let known = reach.relayed.entry(entry).or_default();
            for file in from {
                if !known.contains(&file) {
                    known.push(file);
                }
            }
        }
        // a changed test is a test to run again too
        reach.tests = with_tests
            .difference(&production)
            .chain(seeds.iter().filter(|f| test_code.contains(*f)))
            .map(|f| (*f).to_owned())
            .collect();
        test_ways.retain(|file, _| reach.tests.contains(file));
        reach.test_ways = test_ways;
        reach
    }

    /// For every file the graph records something in (an import, with an
    /// edge or without, a dynamic import, a macro call not read, a symbol it
    /// defines), whether it
    /// is test code: everything recorded in it carries `test`. A file that
    /// defines a symbol of production code is production code, whatever
    /// its imports, since a Rust `#[cfg(test)]` module marks only the
    /// statements inside it.
    pub fn test_code(&self) -> BTreeMap<&str, bool> {
        // per file: something recorded in it is production code
        let mut production: BTreeMap<&str, bool> = BTreeMap::new();
        let recorded = self
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Import)
            .flat_map(|e| &e.evidence)
            .chain(self.unmapped_imports.iter().map(|i| &i.evidence))
            .chain(self.dynamic_imports.iter().map(|d| &d.evidence))
            .chain(self.unread_macros.iter().map(|m| &m.evidence))
            .chain(self.symbols.values().flat_map(|s| &s.evidence));
        for e in recorded {
            *production.entry(e.file.as_str()).or_default() |= !e.test;
        }
        production
            .into_iter()
            .map(|(file, production)| (file, !production))
            .collect()
    }

    /// The reach of a change through production code, and through test
    /// code too when `tests`, with the files reached and the files the
    /// change starts from. `test_code` holds the files of test code.
    fn reach<'s>(
        &'s self,
        seed: ChangeSeed<'s>,
        depth: usize,
        tests: bool,
        test_code: &BTreeSet<&'s str>,
    ) -> (Reach, BTreeSet<&'s str>, BTreeSet<&'s str>) {
        // what depends on each node, and how
        let mut dependents: BTreeMap<Node, BTreeMap<Node, Link>> = BTreeMap::new();
        let mut depend = |target: Node<'s>, importer: Node<'s>, link: Link| {
            dependents
                .entry(target)
                .or_default()
                .entry(importer)
                .or_default()
                .add(link);
        };
        let mut files: BTreeSet<&str> = BTreeSet::new();
        // the statements that pass on the names they take from a file, by
        // that file: what loads a barrel of a changed file is followed only
        // where it may take those names
        let mut passing: BTreeMap<&str, BTreeMap<&str, Link>> = BTreeMap::new();
        // the statements that a walk through re-exports led to a file, by
        // that file, with the files they load: a mock that replaces one of
        // those hides the file from them
        let mut walked: BTreeMap<&str, BTreeMap<&str, BTreeMap<&str, Link>>> = BTreeMap::new();
        // the first re-export on the way of each such statement, by the file
        // it leads to and its own file
        let mut via_at: BTreeMap<(&str, &str), &str> = BTreeMap::new();
        // a declaration says a package is installed, not that a symbol of it
        // is used
        let symbol = matches!(seed, ChangeSeed::Symbol(..));
        // where a file names a component without naming a file of it (a
        // manifest's declaration, an import of the package), by the
        // component and the file: the first line, and whether a manifest
        // declares it
        let mut named_in: BTreeMap<(&ComponentId, &str), (Option<u32>, bool)> = BTreeMap::new();
        for edge in self
            .edges
            .iter()
            .filter(|edge| !(symbol && edge.kind == EdgeKind::Dependency))
        {
            for e in edge.evidence.iter().filter(|e| tests || !e.test) {
                let importer = Node::File(e.file.as_str());
                files.insert(e.file.as_str());
                let target = match e.target.as_deref() {
                    Some(t) => {
                        files.insert(t);
                        Node::File(t)
                    }
                    None => {
                        if matches!(edge.kind, EdgeKind::Dependency | EdgeKind::Import) {
                            let declares = edge.kind == EdgeKind::Dependency;
                            let first = named_in
                                .entry((&edge.to, e.file.as_str()))
                                .or_insert((e.line, declares));
                            first.0 = match (first.0, e.line) {
                                (Some(a), Some(b)) => Some(a.min(b)),
                                (a, b) => a.or(b),
                            };
                            first.1 |= declares;
                        }
                        Node::Component(&edge.to)
                    }
                };
                if importer == target {
                    continue;
                }
                let loaded = e.via().and_then(|place| Some(place.rsplit_once(':')?.0));
                match (target, loaded) {
                    (Node::File(t), Some(loaded)) => {
                        if let Some(place) = e.via() {
                            let at = via_at.entry((t, e.file.as_str())).or_insert(place);
                            *at = (*at).min(place);
                        }
                        walked
                            .entry(t)
                            .or_default()
                            .entry(e.file.as_str())
                            .or_default()
                            .entry(loaded)
                            .or_default()
                            .add(Link::of(e));
                    }
                    (Node::File(t), None) if e.passes_on() => {
                        passing
                            .entry(t)
                            .or_default()
                            .entry(e.file.as_str())
                            .or_default()
                            .add(Link::of(e));
                    }
                    _ => depend(target, importer, Link::of(e)),
                }
            }
            if edge.evidence.is_empty() && edge.from != edge.to {
                let link = Link::running(true);
                depend(Node::Component(&edge.to), Node::Component(&edge.from), link);
            }
        }
        // a Rust method whose type another file defines is reached through
        // the type: what takes the type from its file may call it
        for (methods, takers) in self.method_takers() {
            for (_, e, _) in takers.into_iter().filter(|(_, e, _)| tests || !e.test) {
                files.insert(methods);
                depend(
                    Node::File(methods),
                    Node::File(e.file.as_str()),
                    Link::of(e),
                );
            }
        }
        let index = PathIndex::new(self);
        // an entry file that runs first is reached from what loads a file
        // below it from outside, and from the files below it, which need it
        let statements = self
            .statements_loading_first(&index)
            .into_iter()
            .map(|(entry, _, e)| (entry, e.file.as_str(), e.test));
        // kept apart: a file that only passes names on runs none of them
        let mut runs_first: BTreeMap<&str, BTreeMap<&str, Link>> = BTreeMap::new();
        for (entry, importer, test) in statements.chain(self.files_below_entries(&index)) {
            if tests || !test {
                files.insert(entry);
                if entry != importer {
                    runs_first
                        .entry(entry)
                        .or_default()
                        .entry(importer)
                        .or_default()
                        .add(Link::running(!test));
                }
            }
        }
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
        // a component that names the files its dependents load stands for
        // them and the other files of its evidence (its manifest) only
        let named: BTreeMap<&ComponentId, BTreeSet<&str>> = self
            .components
            .values()
            .filter(|c| c.evidence.iter().any(Evidence::is_entry))
            .map(|c| (&c.id, c.evidence.iter().map(|e| e.file.as_str()).collect()))
            .collect();
        let stands_for =
            |f: &str, c: &ComponentId| named.get(c).is_none_or(|files| files.contains(f));

        // where the walk starts, at what distance, and for a statement that
        // takes a symbol, the file it loads and whether it takes types only
        let mut start: Vec<Start> = Vec::new();
        // the files reached through production code, which stand for their
        // components; a seed does when it is no test
        let mut production: BTreeSet<Node> = BTreeSet::new();
        // the files that changed, whose barrels pass their names on, and for
        // a symbol, the file it is reached through and the name it goes by
        let mut changed: BTreeSet<&str> = BTreeSet::new();
        let mut symbol_name: Option<(&str, &str)> = None;
        // for a symbol, how the files of its statements take it, whether the
        // statement takes types only, and the file it loads
        let mut start_ways: BTreeMap<&str, Vec<StartWay>> = BTreeMap::new();
        let target = match seed {
            ChangeSeed::File(file) => {
                start.push((Node::File(file), 0, None));
                changed.insert(file);
                if !test_code.contains(file) {
                    production.insert(Node::File(file));
                }
                owner_of(file).map(|c| self.ancestor_at(c, depth))
            }
            ChangeSeed::Importers(files) => {
                for &file in files {
                    start.push((Node::File(file), 0, None));
                    changed.insert(file);
                    if !test_code.contains(file) {
                        production.insert(Node::File(file));
                    }
                }
                None
            }
            ChangeSeed::Symbol(symbol, unnamed) => {
                // the first step goes only through the statements that take
                // the symbol by name or take its file whole, apart from those
                // of the latter that never name it; dependencies without file
                // detail on its component stay, like the latter
                start.push((Node::Component(&symbol.component), 0, None));
                // the file and name a mock replaces it by
                let reached = symbol
                    .evidence
                    .iter()
                    .find_map(|e| Some((e.target.as_deref()?, e.names.iter().next()?.as_str())));
                symbol_name = reached.or_else(|| {
                    let at = symbol.location()?;
                    Some((at.file.as_str(), reached_name(&symbol.name)))
                });
                if let Some(found) = self.symbol_importers(symbol) {
                    // a statement that takes the file whole and never names
                    // the symbol takes nothing of it
                    let names = |e: &&Evidence| {
                        e.line.is_none_or(|line| {
                            let place = ImportPlace {
                                file: e.file.clone(),
                                line,
                            };
                            !unnamed.contains(&place)
                        })
                    };
                    let may_use: Vec<&Evidence> = found
                        .may_use
                        .iter()
                        .map(|(_, e)| *e)
                        .filter(names)
                        .collect();
                    let by_name = found.by_name.iter().map(|(_, e)| (Way::Takes(e.via()), *e));
                    let whole = may_use.iter().map(|e| (Way::Whole, *e));
                    for (way, e) in by_name.chain(whole).filter(|(_, e)| tests || !e.test) {
                        start_ways.entry(e.file.as_str()).or_default().push((
                            way,
                            e.type_only,
                            e.target.as_deref(),
                        ));
                    }
                    let statements: Vec<&Evidence> = found
                        .by_name
                        .iter()
                        .map(|(_, e)| *e)
                        .chain(may_use)
                        .filter(|e| tests || !e.test)
                        .collect();
                    // a file whose every such statement passes the name on
                    // is a barrel, whose statements that may take the name
                    // are in the start already
                    let mut only_passes: BTreeMap<&str, bool> = BTreeMap::new();
                    for e in &statements {
                        *only_passes.entry(e.file.as_str()).or_insert(true) &= e.passes_on();
                    }
                    let node = |e: &&'s Evidence| match only_passes[e.file.as_str()] {
                        true => Node::Passes(e.file.as_str()),
                        false => Node::File(e.file.as_str()),
                    };
                    start.extend(statements.iter().map(|e| {
                        let loads = e.target.as_deref().map(|t| (t, e.type_only));
                        (node(e), 1, loads)
                    }));
                    production.extend(statements.iter().filter(|e| !e.test).map(node));
                }
                Some(self.ancestor_at(&symbol.component, depth))
            }
            ChangeSeed::Component(component) => {
                let subtree: BTreeSet<&ComponentId> = self
                    .components
                    .keys()
                    .filter(|id| self.containment_path(id).contains(component))
                    .collect();
                start.extend(subtree.iter().map(|id| (Node::Component(id), 0, None)));
                // its files that a statement loads or is written in, and its
                // tests, which are tests to run again whatever they import
                let tests = test_code.iter().filter(|file| {
                    !owners.contains_key(*file)
                        && index.owner(file).is_some_and(|c| subtree.contains(&c.id))
                });
                let own: Vec<&str> = owners
                    .iter()
                    .filter(|(_, owner)| subtree.contains(*owner))
                    .map(|(file, _)| *file)
                    .chain(tests.copied())
                    .collect();
                start.extend(own.iter().map(|file| (Node::File(file), 0, None)));
                changed.extend(own.iter().copied());
                production.extend(
                    own.iter()
                        .filter(|file| !test_code.contains(*file))
                        .map(|file| Node::File(file)),
                );
                Some(self.ancestor_at(component, depth))
            }
        };
        // what the barrels of the changed files lead to
        let barrels = self.passed_on(&changed, tests);
        let passed = &barrels.links;

        // 0-1 BFS: a file reached through production code puts its component
        // in reach at no cost. One reached through test code alone (a test,
        // or a Rust file through its unit tests) is no part of what the
        // component's dependents load: a test a package owns would reach the
        // manifests that declare the package. The files in `cut` run for
        // nothing: the walk enters and leaves them only through statements
        // that take types, and takes no symbol from them otherwise.
        // how a link from `node`, `d` steps from the change, leads into a
        // test file: an importer of a module that changed outside the graph
        // takes nothing of the change itself
        let taken = !matches!(seed, ChangeSeed::Importers(_));
        let way_of = |node: Node<'s>, kind: Kind<'s>, d: usize| match (node, kind) {
            (Node::File(entry), Kind::RunsFirst) => Way::RunsFirst(entry),
            (Node::File(_) | Node::Passes(_) | Node::Relays(_), Kind::Via(place))
                if d == 0 && taken =>
            {
                Way::Takes(Some(place))
            }
            (Node::File(_) | Node::Passes(_) | Node::Relays(_), _) if d == 0 && taken => {
                Way::Takes(None)
            }
            // a statement that names the symbol's package and no file of it
            (Node::Component(_), _) if d == 0 && symbol => Way::Whole,
            (Node::Component(_), _) if d == 0 => Way::Takes(None),
            (Node::File(f) | Node::Passes(f) | Node::Relays(f), _) => Way::Through(f),
            (Node::Component(c), _) => Way::Through(c.as_str()),
        };
        let walk = |cut: &BTreeSet<&'s str>| -> Walk<'s> {
            let blocked = |node: &Node| match node {
                Node::File(f) | Node::Passes(f) | Node::Relays(f) => cut.contains(f),
                Node::Component(_) => false,
            };
            let mut production = production.clone();
            let mut distance: BTreeMap<Node, usize> = BTreeMap::new();
            // the node each was reached from at its distance
            let mut parent: BTreeMap<Node, Node> = BTreeMap::new();
            let mut feeds: BTreeMap<&str, BTreeMap<&str, usize>> = BTreeMap::new();
            // the ways into test files, for the walks that count test code
            let record = tests;
            let mut ways: BTreeMap<&str, Vec<(usize, Way, bool)>> = BTreeMap::new();
            // the nodes the change reaches through statements that load
            // values all the way, which run what changed
            let mut runs: BTreeSet<Node> = BTreeSet::new();
            let mut queue: VecDeque<Node> = VecDeque::new();
            let mut first: Vec<(Node, usize, bool)> = start
                .iter()
                .filter(|(node, _, loads)| {
                    !blocked(node)
                        && loads.is_none_or(|(loaded, types)| types || !cut.contains(loaded))
                })
                .map(|(node, d, loads)| (*node, *d, *d == 0 || loads.is_none_or(|(_, t)| !t)))
                .collect();
            first.sort_by_key(|(_, d, _)| *d);
            for (node, d, running) in first {
                if running {
                    runs.insert(node);
                }
                if let Entry::Vacant(slot) = distance.entry(node) {
                    slot.insert(d);
                    queue.push_back(node);
                }
            }
            while let Some(node) = queue.pop_front() {
                let d = distance[&node];
                let here = blocked(&node);
                let node_runs = runs.contains(&node);
                let mut next: Vec<(Node, usize, Node, bool)> = Vec::new();
                if let (Node::File(f) | Node::Passes(f) | Node::Relays(f), false) = (node, here) {
                    let stands =
                        |owner: &&ComponentId| production.contains(&node) && stands_for(f, owner);
                    if let Some(owner) = owner_of(f).filter(stands) {
                        next.push((Node::Component(owner), d, node, node_runs));
                    }
                }
                let mut links: Vec<(Node, Link, Kind)> = Vec::new();
                match node {
                    Node::Passes(barrel) => {
                        links.extend(
                            passed
                                .get(barrel)
                                .into_iter()
                                .flatten()
                                .map(|(n, p)| (*n, *p, Kind::Import)),
                        );
                    }
                    Node::Component(_) => {
                        links.extend(
                            dependents
                                .get(&node)
                                .into_iter()
                                .flatten()
                                .map(|(n, p)| (*n, *p, Kind::Import)),
                        );
                    }
                    Node::File(f) | Node::Relays(f) => {
                        links.extend(
                            dependents
                                .get(&Node::File(f))
                                .into_iter()
                                .flatten()
                                .map(|(n, p)| (*n, *p, Kind::Import)),
                        );
                        // what runs a package entry file first, when its own
                        // code may be affected
                        if let Node::File(_) = node {
                            links.extend(
                                runs_first
                                    .get(f)
                                    .into_iter()
                                    .flatten()
                                    .map(|(importer, p)| {
                                        (Node::File(importer), *p, Kind::RunsFirst)
                                    }),
                            );
                        }
                        // a barrel of a changed file passes its names on, and
                        // any other barrel the names of what it loads
                        {
                            for (importer, through) in walked.get(f).into_iter().flatten() {
                                let mut open = through
                                    .iter()
                                    .filter(|(loaded, link)| link.types || !cut.contains(*loaded))
                                    .map(|(_, link)| *link);
                                if let Some(mut link) = open.next() {
                                    open.for_each(|other| link.add(other));
                                    let place = via_at.get(&(f, *importer)).copied().unwrap_or("");
                                    links.push((Node::File(importer), link, Kind::Via(place)));
                                }
                            }
                            let pass = |importer: &'s str| match changed.contains(f)
                                && !changed.contains(importer)
                            {
                                true => Node::Passes(importer),
                                false => Node::Relays(importer),
                            };
                            links.extend(
                                passing
                                    .get(f)
                                    .into_iter()
                                    .flatten()
                                    .map(|(importer, p)| (pass(importer), *p, Kind::Import)),
                            );
                        }
                    }
                }
                let open = |(n, link, _): &(Node, Link, Kind)| link.types || !(here || blocked(n));
                for (n, link, kind) in links.into_iter().filter(open) {
                    // a module a mock replaces runs for nothing
                    let n_runs = node_runs && link.values && !here && !blocked(&n);
                    if let (true, Node::File(t) | Node::Passes(t) | Node::Relays(t)) = (record, n) {
                        if test_code.contains(t) {
                            let way = way_of(node, kind, d);
                            ways.entry(t).or_default().push((d, way, n_runs));
                        }
                    }
                    // a barrel passes on the names of each file it is
                    // reached from
                    if let (
                        Node::Passes(barrel) | Node::Relays(barrel),
                        Node::File(f) | Node::Passes(f) | Node::Relays(f),
                    ) = (n, node)
                    {
                        let at = feeds.entry(barrel).or_default().entry(f).or_insert(d);
                        *at = (*at).min(d);
                    }
                    // reached through production code only now: it stands for
                    // its component from where it was reached before
                    if link.production && production.insert(n) && !blocked(&n) {
                        if let (Node::File(f) | Node::Passes(f) | Node::Relays(f), Some(&at)) =
                            (n, distance.get(&n))
                        {
                            if let Some(owner) = owner_of(f).filter(|c| stands_for(f, c)) {
                                let running = n_runs || runs.contains(&n);
                                next.push((Node::Component(owner), at, n, running));
                            }
                        }
                    }
                    next.push((n, d + 1, node, n_runs));
                }
                for (n, nd, from, running) in next {
                    let starts_running = running && runs.insert(n);
                    if distance.get(&n).is_none_or(|&old| nd < old) {
                        distance.insert(n, nd);
                        parent.insert(n, from);
                        if nd == d {
                            queue.push_front(n);
                        } else {
                            queue.push_back(n);
                        }
                    } else if starts_running {
                        // reached before: what it leads to runs as well now
                        queue.push_back(n);
                    }
                }
            }
            Walk {
                distance,
                parent,
                feeds,
                ways,
                runs,
            }
        };
        let Walk {
            mut distance,
            parent,
            feeds,
            ways,
            runs,
        } = walk(&BTreeSet::new());

        // A test file whose mock replaces a module for its whole run (a
        // `vi.mock` with a factory) reaches the change only along a way that
        // passes none of those modules, apart from one whose mock gives it a
        // name the change may alter (of a changed file, of a barrel that
        // passes a changed file's names on, the symbol), which the test
        // depends on. One that a statement taking a symbol starts from
        // depends on the symbol's name the same way.
        let mut out: Vec<(&str, Vec<&Evidence>)> = Vec::new();
        // the walks with replaced modules cut, and the set each test's mocks
        // cut
        type Walked<'a> = (
            BTreeMap<Node<'a>, usize>,
            BTreeMap<&'a str, Vec<(usize, Way<'a>, bool)>>,
            BTreeSet<Node<'a>>,
        );
        let mut cut_walks: BTreeMap<BTreeSet<&str>, Walked> = BTreeMap::new();
        let mut cut_of: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        if tests {
            let alters = |target: &str, names: &BTreeSet<String>| {
                let any = names.contains(WHOLE_MODULE);
                let passes = distance.contains_key(&Node::Passes(target));
                match (seed, symbol_name) {
                    // the importers of a module that changed keep their names
                    (ChangeSeed::Importers(_), _) => false,
                    (ChangeSeed::Symbol(..), Some((file, name))) => {
                        (target == file || passes) && (any || names.contains(name))
                    }
                    (ChangeSeed::Symbol(..), None) => false,
                    _ if changed.contains(target) => any || barrels.exported.may_have(names),
                    _ => passes && (any || barrels.may_pass(target, names)),
                }
            };
            // the names a statement gives what it loads, as its file and, through
            // re-exports, the files that define them declare them
            let mut given: BTreeMap<(&str, Option<u32>), BTreeSet<String>> = BTreeMap::new();
            for e in self
                .edges
                .iter()
                .filter(|e| e.kind == EdgeKind::Import)
                .flat_map(|e| &e.evidence)
                .filter(|e| e.replaces || e.via().is_some())
            {
                given
                    .entry((e.file.as_str(), e.line))
                    .or_default()
                    .extend(e.names.iter().cloned());
            }
            let mut mocks: BTreeMap<&str, Vec<&Evidence>> = BTreeMap::new();
            for e in self
                .edges
                .iter()
                .filter(|e| e.kind == EdgeKind::Import)
                .flat_map(|e| &e.evidence)
                .filter(|e| e.replaces)
            {
                let names = &given[&(e.file.as_str(), e.line)];
                if e.target.as_deref().is_some_and(|t| !alters(t, names)) {
                    mocks.entry(e.file.as_str()).or_default().push(e);
                }
            }
            let starts: BTreeSet<&str> = start
                .iter()
                .filter_map(|(node, _, _)| match node {
                    Node::File(f) | Node::Passes(f) | Node::Relays(f) => Some(*f),
                    Node::Component(_) => None,
                })
                .collect();
            // one walk per set of replaced modules
            for (file, mocks) in mocks {
                if starts.contains(file) || !distance.contains_key(&Node::File(file)) {
                    continue;
                }
                let cut: BTreeSet<&str> =
                    mocks.iter().filter_map(|e| e.target.as_deref()).collect();
                let other = &cut_walks
                    .entry(cut.clone())
                    .or_insert_with(|| {
                        let found = walk(&cut);
                        (found.distance, found.ways, found.runs)
                    })
                    .0;
                cut_of.insert(file, cut);
                if !other.contains_key(&Node::File(file)) {
                    // the mocks of the modules the change reaches, in order,
                    // a symbol's own file among them
                    let reached = |e: &&Evidence| {
                        e.target.as_deref().is_some_and(|t| {
                            distance.contains_key(&Node::File(t))
                                || distance.contains_key(&Node::Passes(t))
                                || distance.contains_key(&Node::Relays(t))
                                || symbol && symbol_name.is_some_and(|(file, _)| file == t)
                        })
                    };
                    let mut mocks: Vec<&Evidence> = mocks.into_iter().filter(reached).collect();
                    mocks.sort_by_key(|e| e.line);
                    out.push((file, mocks));
                }
            }
            for (file, _) in &out {
                distance.remove(&Node::File(file));
                distance.remove(&Node::Passes(file));
                distance.remove(&Node::Relays(file));
            }
        }
        let left_out = out
            .into_iter()
            .map(|(file, mocks)| (file.to_owned(), mocks.into_iter().cloned().collect()))
            .collect();

        let mut reach = Reach {
            left_out,
            ..Reach::default()
        };
        // a changed file that passes names on is reached again as a barrel:
        // it changed itself, so it is none of what the change reaches
        let changed_barrel = |node: &Node| match node {
            Node::Passes(f) | Node::Relays(f) => distance.get(&Node::File(f)) == Some(&0),
            _ => false,
        };
        let (mut files, mut seeds) = (BTreeSet::new(), BTreeSet::new());
        for (node, d) in distance.iter().filter(|(node, _)| !changed_barrel(node)) {
            if let Node::File(f) | Node::Passes(f) | Node::Relays(f) = node {
                match d {
                    0 => seeds.insert(*f),
                    _ => files.insert(*f),
                };
            }
        }
        // how each test file reaches the change, in the walk its mocks give:
        // every way at its fewest steps, by precedence, then where none of
        // those takes values, the nearest that do
        if tests {
            let mut found: BTreeSet<&str> = ways.keys().copied().collect();
            found.extend(start_ways.keys().copied().filter(|t| test_code.contains(t)));
            found.extend(seeds.iter().copied().filter(|t| test_code.contains(t)));
            for t in found {
                let (walked_distance, walked_ways, walked_runs, cut) = match cut_of.get(t) {
                    Some(cut) => {
                        let (d, w, r) = &cut_walks[cut];
                        (d, w, r, Some(cut))
                    }
                    None => (&distance, &ways, &runs, None),
                };
                let at = [Node::File(t), Node::Passes(t), Node::Relays(t)]
                    .iter()
                    .filter_map(|n| distance.get(n).and(walked_distance.get(n)).copied())
                    .min();
                let Some(dt) = at else {
                    continue;
                };
                if dt == 0 {
                    let target = TestRoute {
                        way: TestWay::Target,
                        steps: 0,
                        types_only: false,
                    };
                    reach.test_ways.insert(
                        t.to_owned(),
                        TestReach {
                            ways: vec![target],
                            types_only: false,
                        },
                    );
                    continue;
                }
                // each way at its fewest steps, which runs what changed when
                // a link on it there does
                let mut best: BTreeMap<Way, (usize, bool)> = BTreeMap::new();
                let mut keep = |way: Way<'s>, steps: usize, running: bool| {
                    let at = best.entry(way).or_insert((steps, running));
                    if steps < at.0 {
                        *at = (steps, running);
                    } else if steps == at.0 {
                        at.1 |= running;
                    }
                };
                for (way, types, loads) in start_ways.get(t).into_iter().flatten() {
                    let open = loads.is_none_or(|l| *types || cut.is_none_or(|c| !c.contains(l)));
                    if open {
                        keep(*way, 1, !types);
                    }
                }
                for (d, way, running) in walked_ways.get(t).into_iter().flatten() {
                    keep(*way, d + 1, *running);
                }
                let fewest = best.values().map(|(steps, _)| *steps).min().unwrap_or(dt);
                let nearest_running = best
                    .values()
                    .filter(|(_, running)| *running)
                    .map(|(steps, _)| *steps)
                    .min();
                let shown = |steps: usize, running: bool| {
                    steps == fewest || (Some(steps) == nearest_running && running)
                };
                let mut routes: Vec<TestRoute> = best
                    .into_iter()
                    .filter(|(_, (steps, running))| shown(*steps, *running))
                    .map(|(way, (steps, running))| TestRoute {
                        way: way.public(),
                        steps,
                        types_only: !running,
                    })
                    .collect();
                routes.sort_by_key(|r| r.steps);
                let running = [Node::File(t), Node::Passes(t), Node::Relays(t)]
                    .iter()
                    .any(|n| walked_runs.contains(n));
                reach.test_ways.insert(
                    t.to_owned(),
                    TestReach {
                        ways: routes,
                        types_only: !running,
                    },
                );
            }
        }
        // package entry files reached only through their re-exports, whose
        // modules below them were not followed, each with the files whose
        // names it passed on, nearest first
        reach.relayed = runs_first
            .keys()
            .filter(|entry| {
                let only = |node: Node| distance.get(&node).is_some_and(|d| *d > 0);
                !distance.contains_key(&Node::File(entry))
                    && (only(Node::Relays(entry)) || only(Node::Passes(entry)))
            })
            .map(|entry| {
                let mut from: Vec<(usize, &str)> = feeds
                    .get(entry)
                    .into_iter()
                    .flatten()
                    .map(|(file, d)| (*d, *file))
                    .collect();
                from.sort();
                let from = from.into_iter().map(|(_, file)| file.to_owned()).collect();
                ((*entry).to_owned(), from)
            })
            .collect();
        let folded_of = |node: &Node| match node {
            Node::File(f) | Node::Passes(f) | Node::Relays(f) => {
                owner_of(f).map(|c| self.ancestor_at(c, depth))
            }
            Node::Component(c) => Some(self.ancestor_at(c, depth)),
        };
        // the first node on the way to `node` that `folded` does not hold:
        // the file or the component it was reached from, with the file of
        // its own that declares or imports that component
        let hop = |node: Node, folded: &ComponentId| {
            let mut at = node;
            while let Some(&from) = parent.get(&at) {
                if folded_of(&from).as_ref() != Some(folded) {
                    return Some(match (from, at) {
                        (Node::File(f) | Node::Passes(f) | Node::Relays(f), _) => {
                            Hop::File(f.to_owned())
                        }
                        (Node::Component(c), at) => {
                            let place = match at {
                                Node::File(f) => named_in
                                    .get(&(c, f))
                                    .map(|(line, declares)| ((f.to_owned(), *line), *declares)),
                                _ => None,
                            };
                            let (declared_in, imported_in) = match place {
                                Some((place, true)) => (Some(place), None),
                                Some((place, false)) => (None, Some(place)),
                                None => (None, None),
                            };
                            Hop::Component {
                                id: c.clone(),
                                declared_in,
                                imported_in,
                            }
                        }
                    });
                }
                at = from;
            }
            None
        };
        // the files of each component the walk reached, at their distances
        let mut reached: BTreeMap<ComponentId, BTreeMap<&str, usize>> = BTreeMap::new();
        for (node, d) in distance.iter().filter(|(node, _)| !changed_barrel(node)) {
            let Some(folded) = folded_of(node) else {
                continue;
            };
            if *d == 0 || Some(&folded) == target.as_ref() {
                continue;
            }
            if *d == 1 {
                reach.direct.insert(folded.clone());
            }
            if reach.distance.get(&folded).is_none_or(|&at| *d < at) {
                reach.distance.insert(folded.clone(), *d);
                match hop(*node, &folded) {
                    Some(from) => reach.from.insert(folded.clone(), from),
                    None => reach.from.remove(&folded),
                };
            }
            if let Node::File(f) | Node::Passes(f) | Node::Relays(f) = node {
                let at = reached
                    .entry(folded.clone())
                    .or_default()
                    .entry(f)
                    .or_insert(*d);
                *at = (*at).min(*d);
            }
            reach.transitive.insert(folded);
        }
        reach.files = reached
            .into_iter()
            .map(|(id, files)| {
                let mut files: Vec<(usize, &str)> =
                    files.into_iter().map(|(f, d)| (d, f)).collect();
                files.sort();
                (id, files.into_iter().map(|(_, f)| f.to_owned()).collect())
            })
            .collect();
        (reach, files, seeds)
    }

    /// For each barrel of the files in `changed` (a file that re-exports
    /// from one, or from another such barrel), what loads it and may take
    /// their names through it: the statements that take the barrel whole or
    /// only load it, that take a name it passes on from them, or whose first
    /// re-export on the way loads one of them. Each leads to its file as one
    /// that uses what it takes, or as a barrel that passes it on again, with
    /// whether a statement is production code; test code counts when
    /// `tests`. The other statements take the barrel's own names or names
    /// defined elsewhere; one that takes a name a changed file defines
    /// points at the file through its `via` evidence anyway.
    fn passed_on<'s>(&'s self, changed: &BTreeSet<&'s str>, tests: bool) -> Barrels<'s> {
        let mut links: BTreeMap<&str, BTreeMap<Node, Link>> = BTreeMap::new();
        if changed.is_empty() {
            return Barrels::default();
        }
        // the statements that load each file, and the `via` evidence of each
        // statement
        let mut loading: BTreeMap<&str, Vec<&Evidence>> = BTreeMap::new();
        let mut via: BTreeMap<(&str, Option<u32>), Vec<&Evidence>> = BTreeMap::new();
        for e in self
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Import)
            .flat_map(|e| &e.evidence)
            .filter(|e| tests || !e.test)
        {
            let Some(target) = e.target.as_deref() else {
                continue;
            };
            match e.via() {
                Some(_) => via.entry((e.file.as_str(), e.line)).or_default().push(e),
                None => loading.entry(target).or_default().push(e),
            }
        }
        // the names each file defines
        let mut own: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for s in self.symbols.values() {
            if let Some(at) = s.location() {
                own.entry(at.file.as_str())
                    .or_default()
                    .insert(reached_name(&s.name));
            }
        }
        let exported = self.exports_of(changed, &own);
        // the re-exports that load a changed file, and the names each file
        // re-exports from a file by name
        let mut into_changed: BTreeSet<(&str, u32)> = BTreeSet::new();
        let mut by_name: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for e in loading.values().flatten().filter(|e| e.passes_on()) {
            if let (Some(target), Some(line)) = (e.target.as_deref(), e.line) {
                if changed.contains(target) {
                    into_changed.insert((e.file.as_str(), line));
                }
            }
            if !e.names.contains(WHOLE_MODULE) {
                by_name
                    .entry(e.file.as_str())
                    .or_default()
                    .extend(e.names.iter().map(String::as_str));
            }
        }

        let mut passes: BTreeMap<&str, Passed> = BTreeMap::new();
        let mut queue: VecDeque<&str> = VecDeque::new();
        for e in changed
            .iter()
            .flat_map(|file| loading.get(file).into_iter().flatten())
            .filter(|e| e.passes_on() && !changed.contains(e.file.as_str()))
        {
            let mut taken = Passed::default();
            match e.names.contains(WHOLE_MODULE) {
                true => taken.any = true,
                false => taken.exact.extend(e.names.iter().map(String::as_str)),
            }
            if passes
                .entry(e.file.as_str())
                .or_default()
                .extend(exported_as(e, taken))
            {
                queue.push_back(e.file.as_str());
            }
        }
        while let Some(barrel) = queue.pop_front() {
            let passed = passes[barrel].clone();
            let defined = own.get(barrel);
            let defines = |name: &str| defined.is_some_and(|names| names.contains(name));
            let elsewhere = |name: &str| by_name.get(barrel).is_some_and(|n| n.contains(name));
            // a name the barrel may pass on from a changed file: one it takes
            // from a barrel that passes them whole, or that its own
            // `export *` of one may pass, which never passes a default on
            let possible = |name: &str| {
                passed.possible.contains(name)
                    || passed.any && name != "default" && !defines(name) && !elsewhere(name)
            };
            for e in loading.get(barrel).into_iter().flatten() {
                let file = e.file.as_str();
                if file == barrel || changed.contains(file) {
                    continue;
                }
                let walked = via.get(&(file, e.line)).into_iter().flatten();
                // the names a walk through re-exports found defined
                let found: BTreeSet<&str> = walked
                    .clone()
                    .flat_map(|v| &v.names)
                    .map(String::as_str)
                    .collect();
                let mut taken = Passed::default();
                if e.names.contains(WHOLE_MODULE) {
                    taken = passed.clone();
                }
                for name in e.names.iter().map(String::as_str) {
                    if passed.exact.contains(name) {
                        taken.exact.insert(name);
                        continue;
                    }
                    // where the walk found it defined, a changed file
                    // passes it on itself; where it found nothing, it may
                    // be any name of theirs, or one the barrel renames
                    let theirs = match found.contains(name) {
                        true => possible(name) && exported.names.contains(name),
                        false => {
                            name != WHOLE_MODULE
                                && name != "default"
                                && !defines(name)
                                && !elsewhere(name)
                                && (exported.open
                                    || possible(name) && exported.names.contains(name))
                        }
                    };
                    if theirs {
                        taken.possible.insert(name);
                    }
                }
                // a statement that only loads the barrel runs what it loads
                let loads = e.names.is_empty() && !e.passes_on();
                let first = walked
                    .filter_map(|v| v.via())
                    .filter_map(|place| {
                        let (file, line) = place.rsplit_once(':')?;
                        Some((file, line.parse().ok()?))
                    })
                    .any(|place| into_changed.contains(&place));
                if taken.is_empty() && !loads && !first {
                    continue;
                }
                let node = match e.passes_on() {
                    true => Node::Passes(file),
                    false => Node::File(file),
                };
                links
                    .entry(barrel)
                    .or_default()
                    .entry(node)
                    .or_default()
                    .add(Link::of(e));
                if e.passes_on()
                    && passes
                        .entry(file)
                        .or_default()
                        .extend(exported_as(e, taken))
                {
                    queue.push_back(file);
                }
            }
        }
        Barrels {
            links,
            passes,
            exported,
        }
    }

    /// The names the files in `changed` may export: those they define,
    /// re-export by name and that a statement takes from them, and those of
    /// the files they re-export whole; `open` when one re-exports from
    /// outside the scan (a package, a path that matches no file), whose
    /// names the graph does not know.
    fn exports_of<'s>(
        &'s self,
        changed: &BTreeSet<&'s str>,
        own: &BTreeMap<&'s str, BTreeSet<&'s str>>,
    ) -> Exported<'s> {
        let mut taken: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        let mut written: BTreeMap<&str, Vec<&Evidence>> = BTreeMap::new();
        let imports = self
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Import)
            .flat_map(|e| &e.evidence)
            .chain(self.unmapped_imports.iter().map(|i| &i.evidence));
        for e in imports {
            // what a mock gives a module says nothing of what it exports
            if let Some(target) = e.target.as_deref().filter(|_| !e.replaces) {
                taken
                    .entry(target)
                    .or_default()
                    .extend(e.names.iter().map(String::as_str));
            }
            if e.re_exports() {
                written.entry(e.file.as_str()).or_default().push(e);
            }
        }
        let mut exported = Exported::default();
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        let mut stack: Vec<&str> = changed.iter().copied().collect();
        while let Some(file) = stack.pop() {
            if !seen.insert(file) {
                continue;
            }
            exported.names.extend(own.get(file).into_iter().flatten());
            exported.names.extend(taken.get(file).into_iter().flatten());
            for e in written.get(file).into_iter().flatten() {
                match e.target.as_deref() {
                    Some(target) if e.names.contains(WHOLE_MODULE) => stack.push(target),
                    Some(_) => exported.names.extend(e.names.iter().map(String::as_str)),
                    None => exported.open = true,
                }
            }
        }
        exported.names.remove(WHOLE_MODULE);
        exported
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

    /// The statements that import `symbol`: those that take its name from
    /// the file it is reached through, and, apart from them, those that take
    /// that file whole. One entry per statement, the first in edge order.
    /// `None` for a symbol without a location, and for one that is itself a
    /// component (a Rust module, whose imports name its own file): the
    /// component answers for it.
    pub fn symbol_importers(&self, symbol: &Symbol) -> Option<SymbolImporters<'_>> {
        if self
            .component(&ComponentId::new(symbol.id.as_str()))
            .is_some()
        {
            return None;
        }
        let reached = symbol
            .evidence
            .iter()
            .find_map(|e| Some((e.target.as_deref()?, e.names.iter().next()?.as_str())));
        let (file, name) = match reached {
            Some((file, name)) => (file.to_owned(), name.to_owned()),
            None => (
                symbol.location()?.file.clone(),
                reached_name(&symbol.name).to_owned(),
            ),
        };
        let language = self
            .component_for_path(&file)
            .and_then(|c| c.language.as_deref());
        let imports = || self.edges.iter().filter(|e| e.kind == EdgeKind::Import);
        let mut recorded = false;
        // the statements that load each file, and those that a walk through
        // re-exports led to the file that defines what they take
        let mut loading: BTreeMap<&str, Vec<(&Edge, &Evidence)>> = BTreeMap::new();
        // where each statement's walks through re-exports led, by the
        // files they reached and the names there: at a barrel, a statement
        // whose walk for the name it takes found another definition does
        // not take this symbol (one that found this symbol's is counted at
        // its definition); one whose walk found nothing for it, while it
        // found another name of the statement elsewhere, still may.
        let mut walked: BTreeMap<(&str, Option<u32>), Vec<&Evidence>> = BTreeMap::new();
        for edge in imports() {
            let to_language = self.component(&edge.to).and_then(|c| c.language.as_deref());
            for e in &edge.evidence {
                recorded |= e.target.is_some() && to_language == language;
                if let Some(target) = e.target.as_deref() {
                    loading.entry(target).or_default().push((edge, e));
                }
                if e.via().is_some() {
                    walked.entry((e.file.as_str(), e.line)).or_default().push(e);
                }
            }
        }
        // From the file, then from each barrel that passes the name on, by
        // the name it exports it under: a statement noted `export` that
        // takes it, or its file whole. A statement that the file itself
        // answers for is recorded there.
        let mut seen: BTreeSet<(&str, Option<u32>)> = BTreeSet::new();
        let (mut by_name, mut may_use) = (Vec::new(), Vec::new());
        let mut through = BTreeMap::new();
        // `None` for the file itself
        let mut barrels: VecDeque<(Option<&str>, PassedAs)> =
            VecDeque::from([(None, PassedAs::Name(name.clone()))]);
        let mut visited: BTreeSet<(&str, PassedAs)> = BTreeSet::new();
        while let Some((barrel, passed)) = barrels.pop_front() {
            let at = barrel.unwrap_or(file.as_str());
            for &(edge, e) in loading.get(at).into_iter().flatten() {
                // a namespace a barrel exports takes the symbol along whole
                let (named, whole) = match &passed {
                    PassedAs::Name(n) => (e.names.contains(n), e.names.contains(WHOLE_MODULE)),
                    PassedAs::Namespace(ns) => (
                        false,
                        e.names.contains(ns) || e.names.contains(WHOLE_MODULE),
                    ),
                };
                if !named && !whole {
                    continue;
                }
                let statement = (e.file.as_str(), e.line);
                // the name from a barrel counts where its walk found no
                // definition; one it found is that definition's
                let elsewhere = |taken: &str| {
                    walked.get(&statement).is_some_and(|reached| {
                        reached.iter().any(|via| {
                            via.target.as_deref() != Some(file.as_str())
                                && via.names.contains(taken)
                        })
                    })
                };
                if let (true, Some(_), PassedAs::Name(taken)) = (named, barrel, &passed) {
                    if elsewhere(taken) {
                        continue;
                    }
                }
                if seen.insert(statement) {
                    match named {
                        true => by_name.push((edge, e)),
                        false => may_use.push((edge, e)),
                    }
                    if let Some(barrel) = barrel {
                        through.insert(statement, barrel);
                    }
                } else if !e.type_only {
                    // a line that also takes values stands for the line, not
                    // its statement of types only
                    let types_only = |(_, kept): &&mut (&Edge, &Evidence)| {
                        (kept.file.as_str(), kept.line) == statement && kept.type_only
                    };
                    let list = if named { &mut by_name } else { &mut may_use };
                    if let Some(kept) = list.iter_mut().find(types_only) {
                        *kept = (edge, e);
                    }
                }
                if !e.passes_on() || e.file == file {
                    continue;
                }
                for next in passed.through(e, named) {
                    if visited.insert((e.file.as_str(), next.clone())) {
                        barrels.push_back((Some(e.file.as_str()), next));
                    }
                }
            }
        }
        let recorded = recorded || !seen.is_empty();
        Some(SymbolImporters {
            file,
            name,
            by_name,
            may_use,
            through,
            recorded,
        })
    }

    /// The statements that run `entry` first without naming it, when it is
    /// the entry file of a component that runs before its files are loaded
    /// (see [`Evidence::runs_first`]): those outside the component that
    /// import a file below it, run (not types only) and name the file they
    /// load (not evidence a walk through re-exports led to). Edge order.
    pub fn imports_below(&self, entry: &str) -> Vec<(&Edge, &Evidence)> {
        let index = PathIndex::new(self);
        self.statements_loading_first(&index)
            .into_iter()
            .filter(|(first, _, _)| *first == entry)
            .map(|(_, edge, e)| (edge, e))
            .collect()
    }

    /// The entry files that run before a file of `component` is loaded:
    /// its own and those of the modules above it, up to the first component
    /// that is no module, each with its component.
    fn entries_above(&self, component: &ComponentId) -> Vec<(ComponentId, &str)> {
        let mut found = Vec::new();
        // from the component up
        for id in self.containment_path(component).into_iter().rev() {
            let Some(c) = self.component(&id) else {
                break;
            };
            if c.kind != ComponentKind::Module {
                break;
            }
            if let Some(entry) = c.evidence.iter().find(|e| e.runs_first()) {
                found.push((id, entry.file.as_str()));
            }
        }
        found
    }

    /// Each statement of [`Self::imports_below`], with the entry file it
    /// runs first.
    fn statements_loading_first<'g>(
        &'g self,
        index: &PathIndex<'g>,
    ) -> Vec<(&'g str, &'g Edge, &'g Evidence)> {
        let mut above: BTreeMap<&ComponentId, Vec<(ComponentId, &str)>> = BTreeMap::new();
        let mut found = Vec::new();
        for edge in self.edges.iter().filter(|e| e.kind == EdgeKind::Import) {
            for e in &edge.evidence {
                let Some(target) = e.target.as_deref() else {
                    continue;
                };
                if e.type_only || e.via().is_some() {
                    continue;
                }
                let Some(owner) = index.owner(target) else {
                    continue;
                };
                let importer = index.owner(&e.file).map(|c| &c.id);
                for id in std::iter::once(&owner.id).chain(importer) {
                    above.entry(id).or_insert_with(|| self.entries_above(id));
                }
                // an entry that runs before the importer itself is loaded
                // ran before this statement too
                let ran = importer.map(|id| &above[id]);
                for (component, entry) in &above[&owner.id] {
                    let before = ran.is_some_and(|ran| ran.iter().any(|(c, _)| c == component));
                    if *entry != target && !before {
                        found.push((*entry, edge, e));
                    }
                }
            }
        }
        found
    }

    /// Each file the graph knows below an entry file that runs first, with
    /// that entry file and whether the file is test code: one that imports
    /// something, whatever its import loads (an import without an edge such
    /// as `import pytest`, a dynamic one), or that defines a public symbol,
    /// as a module that imports only the standard library does.
    fn files_below_entries<'g>(&'g self, index: &PathIndex<'g>) -> Vec<(&'g str, &'g str, bool)> {
        let mut importers: BTreeMap<&str, bool> = BTreeMap::new();
        let evidence = self
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Import)
            .flat_map(|e| &e.evidence)
            .chain(self.unmapped_imports.iter().map(|i| &i.evidence))
            .chain(self.dynamic_imports.iter().map(|i| &i.evidence));
        for e in evidence {
            *importers.entry(e.file.as_str()).or_insert(true) &= e.test;
        }
        // a file whose imports the scan records nothing of is test code by
        // its symbols' mark
        for e in self.symbols.values().flat_map(|s| s.location()) {
            importers.entry(e.file.as_str()).or_insert(e.test);
        }
        let mut above: BTreeMap<&ComponentId, Vec<(ComponentId, &str)>> = BTreeMap::new();
        let mut found = Vec::new();
        for (file, test) in importers {
            let Some(owner) = index.owner(file) else {
                continue;
            };
            let entries = above
                .entry(&owner.id)
                .or_insert_with(|| self.entries_above(&owner.id));
            for (_, entry) in entries.iter() {
                if *entry != file {
                    found.push((*entry, file, test));
                }
            }
        }
        found
    }

    /// For each file that holds Rust methods of a type another file defines
    /// (a symbol's `impl` evidence), the statements that take that type from
    /// its file, which may call them, each with the type's name. Only test
    /// code can call a method test code defines (`#[cfg(test)]`).
    pub fn method_takers(&self) -> BTreeMap<&str, Vec<(&Edge, &Evidence, &str)>> {
        let mut types: BTreeMap<&str, BTreeSet<(&str, &str, bool)>> = BTreeMap::new();
        for e in self
            .symbols
            .values()
            .flat_map(|s| &s.evidence)
            .filter(|e| e.note.as_deref() == Some("impl"))
        {
            if let Some(type_file) = e.target.as_deref() {
                for name in &e.names {
                    types.entry(type_file).or_default().insert((
                        e.file.as_str(),
                        name.as_str(),
                        e.test,
                    ));
                }
            }
        }
        let mut takers: BTreeMap<&str, Vec<(&Edge, &Evidence, &str)>> = BTreeMap::new();
        if types.is_empty() {
            return takers;
        }
        for edge in self.edges.iter().filter(|e| e.kind == EdgeKind::Import) {
            for e in &edge.evidence {
                let Some(methods) = e.target.as_deref().and_then(|t| types.get(t)) else {
                    continue;
                };
                let whole = e.names.is_empty() || e.names.contains(WHOLE_MODULE);
                // one entry per file of methods, by the first type it takes
                let mut seen: BTreeSet<&str> = BTreeSet::new();
                for (file, name, test) in methods {
                    let callable = !test || e.test;
                    if *file != e.file
                        && callable
                        && (whole || e.names.contains(*name))
                        && seen.insert(file)
                    {
                        takers.entry(file).or_default().push((edge, e, name));
                    }
                }
            }
        }
        takers
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

    /// The owner of each of `files`, as [`Self::component_for_path`] finds
    /// it, with one index for them all.
    pub fn components_for_paths<'p>(
        &self,
        files: impl IntoIterator<Item = &'p str>,
    ) -> Vec<(&'p str, Option<&Component>)> {
        let index = PathIndex::new(self);
        files.into_iter().map(|f| (f, index.owner(f))).collect()
    }
}

/// The statements that import a symbol, for a symbol query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolImporters<'a> {
    /// The file the symbol is reached through, relative to the root: where
    /// it is defined, or where its type is for a Rust method.
    pub file: String,
    /// The name it is reached by in that file: its own, or its type's.
    pub name: String,
    /// Statements that take that name from that file, with their edge.
    pub by_name: Vec<(&'a Edge, &'a Evidence)>,
    /// Statements that take that file whole (`*`), the others aside.
    pub may_use: Vec<(&'a Edge, &'a Evidence)>,
    /// The statements of both lists that reach the name through a barrel
    /// that passes it on, by file and line, with that barrel's file.
    pub through: BTreeMap<(&'a str, Option<u32>), &'a str>,
    /// Whether evidence names imported files for the file's language at all.
    /// Without it, both lists are unknown rather than empty.
    pub recorded: bool,
}

/// The name a member is imported by: its type's, without generics and
/// module path (`Edge` for `crate:: model:: Edge::kind`, `Wrapper` for
/// `Wrapper<crate::a::B>::new`, `Class` for `Class.method`); any other
/// symbol goes by its own name.
fn reached_name(name: &str) -> &str {
    let Some((owner, _)) = name.rsplit_once("::").or_else(|| name.rsplit_once('.')) else {
        return name;
    };
    let owner = owner.split('<').next().unwrap_or(owner);
    owner.rsplit("::").next().unwrap_or(owner).trim()
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
    /// The files that import a module that changed outside them (one that
    /// no component carries), relative to the repository root, with no
    /// component of their own left out. They did not change, so a test
    /// whose mock replaces one of them does not run what changed through it.
    Importers(&'a [&'a str]),
    /// A symbol: its first step goes only through the statements that take
    /// it by name or take its file whole (see
    /// [`ArchitectureGraph::symbol_importers`]), then file by file. The set
    /// holds statements of the latter that a uses pass read and found never
    /// naming the symbol, which take nothing of it and leave the first step;
    /// a statement that takes it by name stays, as it loads the file.
    Symbol(&'a Symbol, &'a BTreeSet<ImportPlace>),
}

/// Components that may be affected by a change.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reach {
    /// Components with a file that depends on the change directly.
    pub direct: BTreeSet<ComponentId>,
    /// Every component reached, `direct` included.
    pub transitive: BTreeSet<ComponentId>,
    /// Files that reach the change only through test code, directly or
    /// not: the tests to run again, those beside production code included.
    pub tests: BTreeSet<String>,
    /// Test files left out of `tests`: they reach the change only through
    /// modules their mocks replace for their whole run, each with those
    /// mocks.
    pub left_out: BTreeMap<String, Vec<Evidence>>,
    /// For each component of `transitive`, the fewest steps from the change
    /// (1 for one of `direct`).
    pub distance: BTreeMap<ComponentId, usize>,
    /// For each component of `transitive`, what the walk reached it from at
    /// that distance.
    pub from: BTreeMap<ComponentId, Hop>,
    /// For each component of `transitive`, its files the walk reached,
    /// nearest first.
    pub files: BTreeMap<ComponentId, Vec<String>>,
    /// For each file of `tests`, how it reaches the change.
    pub test_ways: BTreeMap<String, TestReach>,
    /// Package entry files (a Python `__init__.py`) the walk reached only
    /// through their re-exports, each with the files whose names it passed
    /// on, nearest first: what imports a module below them, which runs them
    /// first, was not followed. One the walk starts from, which takes a
    /// symbol's name, has none.
    pub relayed: BTreeMap<String, Vec<String>>,
}

/// How a test file reaches the change, in the walk its mocks leave: every
/// way at its fewest steps, by precedence, then where none of those takes
/// values, the nearest ways that do.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TestReach {
    pub ways: Vec<TestRoute>,
    /// No way of it runs what changed: each takes types only somewhere,
    /// so running the test runs none of the change.
    pub types_only: bool,
}

/// One way a test file reaches the change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestRoute {
    pub way: TestWay,
    /// Steps from the change.
    pub steps: usize,
    /// The way runs none of what changed: a statement on it, the test's
    /// own or one further, takes types only.
    pub types_only: bool,
}

/// One way a test file reaches the change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestWay {
    /// It changed: it is the target, or a file of the changed component.
    Target,
    /// A statement of it takes the change: the changed file, or the
    /// symbol by name, through the re-export at `via` when barrels pass it.
    Takes { via: Option<String> },
    /// A statement of it takes the symbol's module whole.
    Whole,
    /// It loads a module below a package whose entry file `entry`, which
    /// the change reaches, runs first.
    RunsFirst { entry: String },
    /// Through other files: `from` is the first one on the way, or a
    /// component's id where its package was.
    Through { from: String },
}

/// What the walk reached a component from: a file of another component,
/// which a file of it imports, or another component it depends on as a
/// whole (a manifest's declaration, an import that names no file).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hop {
    File(String),
    Component {
        id: ComponentId,
        /// The manifest of its own that declares that component, and the
        /// line, when a declaration was the way.
        declared_in: Option<(String, Option<u32>)>,
        /// The file of its own that imports that component without naming
        /// a file of it, and the line, when that import was the way.
        imported_in: Option<(String, Option<u32>)>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Node<'a> {
    File(&'a str),
    Component(&'a ComponentId),
    /// A file reached only as a barrel that passes names of what changed
    /// on: what loads it is followed only through the statements that may
    /// take those names. Reached otherwise, it is a [`Node::File`] too.
    Passes(&'a str),
    /// A file reached only as a barrel of a file that did not change: what
    /// loads it is followed, but not what a package entry file it is runs
    /// before (the imports of a module below the package), since none of
    /// its own code is affected. Reached otherwise, it is a [`Node::File`].
    Relays(&'a str),
}

/// What one walk of the reach found.
struct Walk<'a> {
    distance: BTreeMap<Node<'a>, usize>,
    /// The node each was reached from at its distance.
    parent: BTreeMap<Node<'a>, Node<'a>>,
    /// For each file reached as a barrel ([`Node::Passes`], [`Node::Relays`]),
    /// the files it was reached from, each at its distance.
    feeds: BTreeMap<&'a str, BTreeMap<&'a str, usize>>,
    /// For each test file, every link into it: the distance it leads from,
    /// the way it gives, and whether it runs what changed (it loads values
    /// from a node that does).
    ways: BTreeMap<&'a str, Vec<(usize, Way<'a>, bool)>>,
    /// The nodes that run what changed: reached through statements that
    /// load values all the way.
    runs: BTreeSet<Node<'a>>,
}

/// How a statement that takes a symbol takes it, whether it takes types
/// only, and the file it loads.
type StartWay<'a> = (Way<'a>, bool, Option<&'a str>);

/// Where a walk starts, at what distance, and for a statement that takes a
/// symbol, the file it loads and whether it takes types only.
type Start<'a> = (Node<'a>, usize, Option<(&'a str, bool)>);

/// How an importer depends on what it loads: through production code,
/// through a statement that takes types only, of which a mock replaces
/// nothing, and through one that takes values.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Link {
    production: bool,
    types: bool,
    values: bool,
}

impl Link {
    fn of(e: &Evidence) -> Self {
        Link {
            production: !e.test,
            types: e.type_only,
            values: !e.type_only,
        }
    }

    /// A dependency that runs, through production code when `production`.
    fn running(production: bool) -> Self {
        Link {
            production,
            types: false,
            values: true,
        }
    }

    fn add(&mut self, other: Link) {
        self.production |= other.production;
        self.types |= other.types;
        self.values |= other.values;
    }
}

/// How a link leads from what it loads to its importer.
#[derive(Debug, Clone, Copy)]
enum Kind<'a> {
    Import,
    /// An import that takes a name through re-exports, at the first of
    /// them.
    Via(&'a str),
    /// A file that runs a package's entry file first.
    RunsFirst,
}

/// How a test file reaches the change, by precedence: variants in order,
/// a statement without re-exports before one through them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Way<'a> {
    Takes(Option<&'a str>),
    Whole,
    RunsFirst(&'a str),
    Through(&'a str),
}

impl Way<'_> {
    fn public(self) -> TestWay {
        match self {
            Way::Takes(via) => TestWay::Takes {
                via: via.map(str::to_owned),
            },
            Way::Whole => TestWay::Whole,
            Way::RunsFirst(entry) => TestWay::RunsFirst {
                entry: entry.to_owned(),
            },
            Way::Through(from) => TestWay::Through {
                from: from.to_owned(),
            },
        }
    }
}

/// What a re-export `e` passes on of what it takes, `taken` by the names it
/// takes them by, under the names its file exports them as: renamed
/// (`export { price as cost }`), or a namespace that holds them all
/// (`export * as money`).
fn exported_as<'a>(e: &'a Evidence, taken: Passed<'a>) -> Passed<'a> {
    if let Some(names) = e.exported_as.get(WHOLE_MODULE) {
        if e.names.iter().any(|n| n == WHOLE_MODULE) && !taken.is_empty() {
            return Passed {
                exact: names.iter().map(String::as_str).collect(),
                ..Passed::default()
            };
        }
    }
    let renamed = |names: BTreeSet<&'a str>| -> BTreeSet<&'a str> {
        names
            .into_iter()
            .flat_map(|n| e.exported_names(n))
            .collect()
    };
    Passed {
        exact: renamed(taken.exact),
        possible: renamed(taken.possible),
        any: taken.any,
    }
}

/// What a barrel passes on of the changed files, by the names it exports
/// them under.
#[derive(Debug, Clone, Default)]
struct Passed<'a> {
    /// Names it re-exports from them by name.
    exact: BTreeSet<&'a str>,
    /// Names that may be theirs: it takes them by name from a barrel that
    /// passes them on whole.
    possible: BTreeSet<&'a str>,
    /// It passes them on whole (`export *`): any name it does not define.
    any: bool,
}

/// What the barrels of the changed files lead to (see
/// [`ArchitectureGraph::passed_on`]), and what they pass on.
#[derive(Debug, Default)]
struct Barrels<'a> {
    links: BTreeMap<&'a str, BTreeMap<Node<'a>, Link>>,
    passes: BTreeMap<&'a str, Passed<'a>>,
    exported: Exported<'a>,
}

impl Barrels<'_> {
    /// Whether `barrel` may pass on one of `names` from a changed file.
    fn may_pass(&self, barrel: &str, names: &BTreeSet<String>) -> bool {
        let Some(passed) = self.passes.get(barrel) else {
            return false;
        };
        names.iter().map(String::as_str).any(|name| {
            passed.exact.contains(name)
                || passed.possible.contains(name)
                || passed.any
                    && name != "default"
                    && (self.exported.open || self.exported.names.contains(name))
        })
    }
}

/// What the changed files may export, by name.
#[derive(Debug, Default)]
struct Exported<'a> {
    names: BTreeSet<&'a str>,
    /// One re-exports from outside the scan, whose names are not known.
    open: bool,
}

impl Exported<'_> {
    /// Whether one of `names` may be one the changed files export.
    fn may_have(&self, names: &BTreeSet<String>) -> bool {
        self.open || names.iter().any(|n| self.names.contains(n.as_str()))
    }
}

impl<'a> Passed<'a> {
    fn is_empty(&self) -> bool {
        self.exact.is_empty() && self.possible.is_empty() && !self.any
    }

    /// Adds `other`; whether anything was new.
    fn extend(&mut self, other: Passed<'a>) -> bool {
        let before = (self.exact.len(), self.possible.len(), self.any);
        self.exact.extend(other.exact);
        self.possible.extend(other.possible);
        self.any |= other.any;
        before != (self.exact.len(), self.possible.len(), self.any)
    }
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

/// What a barrel passes a symbol on as: a name, or a namespace that holds
/// it (`export * as money from`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum PassedAs {
    Name(String),
    Namespace(String),
}

impl PassedAs {
    /// What the file of re-export evidence `e`, which takes this by name
    /// (`named`) or whole, exports it as: a name under each name the
    /// statement gives it, a namespace under the namespace's names, or
    /// itself through `export *`.
    fn through(&self, e: &Evidence, named: bool) -> Vec<PassedAs> {
        let namespaces = || -> Vec<PassedAs> {
            match e.exported_as.get(WHOLE_MODULE) {
                Some(names) => names.iter().cloned().map(PassedAs::Namespace).collect(),
                None => vec![self.clone()],
            }
        };
        match (self, named) {
            (PassedAs::Name(n), true) => e
                .exported_names(n)
                .into_iter()
                .map(|n| PassedAs::Name(n.to_owned()))
                .collect(),
            (PassedAs::Namespace(ns), _) if e.names.contains(ns) => e
                .exported_names(ns)
                .into_iter()
                .map(|n| PassedAs::Namespace(n.to_owned()))
                .collect(),
            _ => namespaces(),
        }
    }
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
    fn cycles_count_only_imports_that_run() {
        let mut graph = ArchitectureGraph::default();
        graph.add_edge(
            Edge::new("a", "b", EdgeKind::Import).with_evidence(Evidence::new("a.ts").at_line(1)),
        );
        // b uses a only as a type, which is erased before the program runs
        graph.add_edge(
            Edge::new("b", "a", EdgeKind::Import)
                .with_evidence(Evidence::new("b.ts").at_line(1).type_only(true)),
        );
        assert!(graph.cycles().is_empty());
        // an import that runs beside it closes the cycle
        graph
            .add_edges([Edge::new("b", "a", EdgeKind::Import)
                .with_evidence(Evidence::new("b.ts").at_line(2))]);
        assert_eq!(ids(graph.cycles()), vec![vec!["a", "b"]]);
    }

    #[test]
    fn cycles_leave_out_test_code() {
        let mut graph = ArchitectureGraph::default();
        graph.add_edges([
            Edge::new("a", "b", EdgeKind::Import).with_evidence(Evidence::new("a.ts").at_line(1)),
            // only a test of b imports a
            Edge::new("b", "a", EdgeKind::Import)
                .with_evidence(Evidence::new("b/b.test.ts").at_line(1).in_test(true)),
        ]);
        assert!(graph.cycles().is_empty());
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

    fn symbol(id: &str, name: &str, evidence: Vec<Evidence>) -> Symbol {
        Symbol {
            id: SymbolId::new(id),
            name: name.into(),
            kind: SymbolKind::Function,
            component: "lib".into(),
            signature: None,
            evidence,
        }
    }

    fn app_and_lib() -> ArchitectureGraph {
        let mut graph = ArchitectureGraph::default();
        for id in ["app", "lib"] {
            let mut c = Component::new(id, id, ComponentKind::Module);
            c.path = Some(id.into());
            graph.add_component(c);
        }
        graph
    }

    fn at(list: &[(&Edge, &Evidence)]) -> Vec<String> {
        list.iter()
            .map(|(_, e)| format!("{}:{}", e.file, e.line.unwrap_or(0)))
            .collect()
    }

    #[test]
    fn symbol_importers_take_the_name_or_the_whole_file() {
        let mut graph = app_and_lib();
        let import = |file: &str, line: u32, note: &str, names: &[&str]| {
            Edge::new("app", "lib", EdgeKind::Import).with_evidence(
                Evidence::new(file)
                    .at_line(line)
                    .with_note(note)
                    .pointing_at("lib/money.ts")
                    .taking(names.iter().copied()),
            )
        };
        graph.add_edges([
            import("app/a.ts", 1, "import", &["formatPrice"]),
            // the same statement through another re-export, whole: once
            import("app/a.ts", 1, "import via app/index.ts:4", &["*"]),
            import("app/b.ts", 2, "import", &["*"]),
            // a side-effect import and another name
            import("app/c.ts", 3, "import", &[]),
            import("app/d.ts", 4, "import", &["Wallet"]),
            // the name beside the whole module: by name
            import("app/e.ts", 5, "import", &["*", "formatPrice"]),
        ]);
        let price = symbol(
            "lib::formatPrice",
            "formatPrice",
            vec![Evidence::new("lib/money.ts").at_line(8)],
        );
        let found = graph.symbol_importers(&price).unwrap();
        assert_eq!(
            (found.file.as_str(), found.name.as_str()),
            ("lib/money.ts", "formatPrice")
        );
        assert_eq!(at(&found.by_name), ["app/a.ts:1", "app/e.ts:5"]);
        assert_eq!(at(&found.may_use), ["app/b.ts:2"]);
        assert!(found.recorded);
    }

    #[test]
    fn a_member_is_reached_through_its_type() {
        let mut graph = app_and_lib();
        graph.add_edges([Edge::new("app", "lib", EdgeKind::Import).with_evidence(
            Evidence::new("app/main.rs")
                .at_line(2)
                .with_note("use")
                .pointing_at("lib/model.rs")
                .taking(["Edge"]),
        )]);
        // the impl sits in another file than its type: evidence says where
        let kind = symbol(
            "lib::Edge::kind",
            "Edge::kind",
            vec![
                Evidence::new("lib/impls.rs").at_line(9),
                Evidence::new("lib/impls.rs")
                    .at_line(8)
                    .with_note("impl")
                    .pointing_at("lib/model.rs")
                    .taking(["Edge"]),
            ],
        );
        let found = graph.symbol_importers(&kind).unwrap();
        assert_eq!(
            (found.file.as_str(), found.name.as_str()),
            ("lib/model.rs", "Edge")
        );
        assert_eq!(at(&found.by_name), ["app/main.rs:2"]);
        // without such evidence: the type's name, without generics and path
        for (name, reached) in [
            ("Wrapper<crate::a::B>::new", "Wrapper"),
            ("crate:: model:: Edge::kind", "Edge"),
            ("Resolver<'a>::new", "Resolver"),
            ("Class.method", "Class"),
            ("greet", "greet"),
        ] {
            let s = symbol("lib::x", name, vec![Evidence::new("lib/x.rs").at_line(1)]);
            assert_eq!(graph.symbol_importers(&s).unwrap().name, reached, "{name}");
        }
    }

    /// A package `lib` whose `money.ts` defines `formatPrice`, behind two
    /// barrels (`index.ts` re-exports it, `all.ts` re-exports `index.ts`
    /// whole), and an importer per way of reaching it, each a component of
    /// its own.
    fn behind_barrels() -> ArchitectureGraph {
        let mut graph = ArchitectureGraph::default();
        let mut lib = Component::new("lib", "lib", ComponentKind::Package);
        lib.path = Some("lib".into());
        graph.add_component(lib);
        let files = ["ns", "deep", "other", "failed", "named", "after", "shadow"];
        for name in files {
            let mut c = Component::new(format!("app::{name}"), name, ComponentKind::Module);
            c.path = Some(format!("app/{name}.ts"));
            graph.add_component(c);
        }
        // a package that only declares `lib`
        let mut site = Component::new("site", "site", ComponentKind::Package);
        site.path = Some("site".into());
        graph.add_component(site);
        // the component that owns a file: `lib` or the file's own
        let owner = |file: &str| match file.strip_prefix("app/") {
            Some(rest) => format!("app::{}", rest.trim_end_matches(".ts")),
            None => "lib".to_owned(),
        };
        let statement = |file: &str, line: u32, note: &str, target: &str, names: &[&str]| {
            Edge::new(owner(file), owner(target), EdgeKind::Import).with_evidence(
                Evidence::new(file)
                    .at_line(line)
                    .with_note(note)
                    .pointing_at(target)
                    .taking(names.iter().copied()),
            )
        };
        graph.add_edges([
            // the barrels pass the name on
            statement(
                "lib/index.ts",
                1,
                "export",
                "lib/money.ts",
                &["formatPrice"],
            ),
            statement("lib/all.ts", 1, "export", "lib/index.ts", &["*"]),
            // whole, through each barrel
            statement("app/ns.ts", 1, "import", "lib/index.ts", &["*"]),
            statement("app/deep.ts", 2, "import", "lib/all.ts", &["*"]),
            // another name of a barrel
            statement("app/other.ts", 3, "import", "lib/index.ts", &["Wallet"]),
            // the name, where the walk through the barrel found nothing
            statement(
                "app/failed.ts",
                4,
                "import",
                "lib/index.ts",
                &["formatPrice"],
            ),
            // the name, walked to the file that defines it
            statement("app/named.ts", 5, "import", "lib/all.ts", &["formatPrice"]),
            statement(
                "app/named.ts",
                5,
                "import via lib/all.ts:1",
                "lib/money.ts",
                &["formatPrice"],
            ),
            // further on
            statement("app/after.ts", 6, "import", "app/ns.ts", &["ns"]),
            statement("app/after.ts", 7, "import", "app/other.ts", &["other"]),
            // a barrel whose own export shadows its star: the name it takes
            // is defined in other.ts, which its walk found
            statement("lib/shade.ts", 1, "export", "lib/money.ts", &["*"]),
            statement(
                "lib/shade.ts",
                2,
                "export",
                "lib/other.ts",
                &["formatPrice"],
            ),
            statement(
                "app/shadow.ts",
                8,
                "import",
                "lib/shade.ts",
                &["formatPrice"],
            ),
            statement(
                "app/shadow.ts",
                8,
                "import via lib/shade.ts:2",
                "lib/other.ts",
                &["formatPrice"],
            ),
            // a declaration is no use
            Edge::new("site", "lib", EdgeKind::Dependency)
                .with_evidence(Evidence::new("site/package.json").at_line(3)),
        ]);
        graph
    }

    fn price() -> Symbol {
        symbol(
            "lib::formatPrice",
            "formatPrice",
            vec![Evidence::new("lib/money.ts").at_line(8)],
        )
    }

    #[test]
    fn symbol_importers_follow_the_barrels_that_pass_a_name_on() {
        let graph = behind_barrels();
        let found = graph.symbol_importers(&price()).unwrap();
        let sorted = |list: &[(&Edge, &Evidence)]| {
            let mut list = at(list);
            list.sort();
            list
        };
        // not what a walk through a barrel found defined elsewhere
        assert_eq!(
            sorted(&found.by_name),
            ["app/failed.ts:4", "app/named.ts:5", "lib/index.ts:1"]
        );
        // whole: what takes a barrel that passes the name on, a barrel that
        // re-exports it whole included; another name of a barrel is none
        assert_eq!(
            sorted(&found.may_use),
            [
                "app/deep.ts:2",
                "app/ns.ts:1",
                "lib/all.ts:1",
                "lib/shade.ts:1"
            ]
        );
        // and the barrel each went through
        let through: Vec<(&str, &str)> = found
            .through
            .iter()
            .map(|((file, _), barrel)| (*file, *barrel))
            .collect();
        assert_eq!(
            through,
            [
                ("app/deep.ts", "lib/all.ts"),
                ("app/failed.ts", "lib/index.ts"),
                ("app/ns.ts", "lib/index.ts"),
                ("lib/all.ts", "lib/index.ts"),
            ]
        );
    }

    #[test]
    fn change_impact_from_a_symbol_goes_no_further_than_its_barrels_pass_it() {
        let graph = behind_barrels();
        let reach = graph.change_impact(ChangeSeed::Symbol(&price(), &BTreeSet::new()), 2);
        let ids = |set: &BTreeSet<ComponentId>| -> Vec<String> {
            set.iter().map(|c| c.to_string()).collect()
        };
        assert_eq!(
            ids(&reach.direct),
            ["app::deep", "app::failed", "app::named", "app::ns"]
        );
        // `other` takes another name of a barrel: not reached, nor what
        // only it leads to; `after` imports `ns`; `shadow` takes another
        // definition, and `site` only declares the package
        assert_eq!(
            ids(&reach.transitive),
            [
                "app::after",
                "app::deep",
                "app::failed",
                "app::named",
                "app::ns"
            ]
        );
    }

    #[test]
    fn symbol_importers_are_unknown_where_no_evidence_names_imported_files() {
        let mut graph = ArchitectureGraph::default();
        for id in ["a", "b"] {
            let mut c = Component::new(id, id, ComponentKind::Package);
            c.path = Some(id.into());
            c.language = Some("rust".into());
            graph.add_component(c);
        }
        graph.add_edge(edge("a", "b", "a/src/lib.rs", 3));
        let s = symbol("b::f", "f", vec![Evidence::new("b/src/lib.rs").at_line(1)]);
        let found = graph.symbol_importers(&s).unwrap();
        assert!(found.by_name.is_empty() && found.may_use.is_empty());
        assert!(!found.recorded);
        // no location, no answer
        assert!(graph
            .symbol_importers(&symbol("b::g", "g", Vec::new()))
            .is_none());
    }

    #[test]
    fn change_impact_from_a_symbol_starts_at_the_statements_that_take_it() {
        let mut graph = ArchitectureGraph::default();
        for id in ["named", "whole", "other", "next", "lib"] {
            let mut c = Component::new(id, id, ComponentKind::Module);
            c.path = Some(id.into());
            graph.add_component(c);
        }
        let import = |from: &str, file: &str, target: &str, names: &[&str]| {
            Edge::new(
                from,
                if target.starts_with("lib") {
                    "lib"
                } else {
                    "named"
                },
                EdgeKind::Import,
            )
            .with_evidence(
                Evidence::new(file)
                    .at_line(1)
                    .pointing_at(target)
                    .taking(names.iter().copied()),
            )
        };
        graph.add_edges([
            import("named", "named/a.ts", "lib/money.ts", &["formatPrice"]),
            import("whole", "whole/b.ts", "lib/money.ts", &["*"]),
            // another name of the same file: not affected
            import("other", "other/c.ts", "lib/money.ts", &["Wallet"]),
            // from there on, file by file
            import("next", "next/d.ts", "named/a.ts", &["total"]),
        ]);
        let price = symbol(
            "lib::formatPrice",
            "formatPrice",
            vec![Evidence::new("lib/money.ts").at_line(8)],
        );
        let reach = graph.change_impact(ChangeSeed::Symbol(&price, &BTreeSet::new()), 2);
        let ids =
            |set: &BTreeSet<ComponentId>| set.iter().map(|c| c.to_string()).collect::<Vec<_>>();
        assert_eq!(ids(&reach.direct), ["named", "whole"]);
        assert_eq!(ids(&reach.transitive), ["named", "next", "whole"]);
        // the whole file reaches the other importer too
        let file = graph.change_impact(ChangeSeed::File("lib/money.ts"), 2);
        assert_eq!(ids(&file.direct), ["named", "other", "whole"]);
        // a statement that takes the file whole and never names the symbol
        // leaves the first step; one that takes it by name stays, as it
        // loads the file all the same
        let place = |file: &str| ImportPlace {
            file: file.into(),
            line: 1,
        };
        let unnamed = BTreeSet::from([place("whole/b.ts"), place("named/a.ts")]);
        let reach = graph.change_impact(ChangeSeed::Symbol(&price, &unnamed), 2);
        assert_eq!(ids(&reach.direct), ["named"]);
        assert_eq!(ids(&reach.transitive), ["named", "next"]);
    }

    #[test]
    fn a_test_that_takes_the_file_whole_and_never_names_the_symbol_is_no_test_to_run() {
        let mut graph = ArchitectureGraph::default();
        let mut lib = Component::new("lib", "lib", ComponentKind::Module);
        lib.path = Some("lib".into());
        graph.add_component(lib);
        let import = |file: &str, names: &[&str]| {
            Edge::new("lib", "lib", EdgeKind::Import).with_evidence(
                Evidence::new(file)
                    .at_line(1)
                    .in_test(true)
                    .pointing_at("lib/money.ts")
                    .taking(names.iter().copied()),
            )
        };
        graph.add_edges([
            import("lib/whole.test.ts", &["*"]),
            import("lib/named.test.ts", &["formatPrice"]),
        ]);
        let price = symbol(
            "lib::formatPrice",
            "formatPrice",
            vec![Evidence::new("lib/money.ts").at_line(8)],
        );
        let reach = graph.change_impact(ChangeSeed::Symbol(&price, &BTreeSet::new()), 2);
        let tests: Vec<&str> = reach.tests.iter().map(String::as_str).collect();
        assert_eq!(tests, ["lib/named.test.ts", "lib/whole.test.ts"]);
        let unnamed = BTreeSet::from([ImportPlace {
            file: "lib/whole.test.ts".into(),
            line: 1,
        }]);
        let reach = graph.change_impact(ChangeSeed::Symbol(&price, &unnamed), 2);
        let tests: Vec<&str> = reach.tests.iter().map(String::as_str).collect();
        assert_eq!(tests, ["lib/named.test.ts"]);
        let ways: Vec<&str> = reach.test_ways.keys().map(String::as_str).collect();
        assert_eq!(ways, ["lib/named.test.ts"]);
    }

    #[test]
    fn change_impact_lists_apart_what_only_test_code_reaches() {
        let mut graph = ArchitectureGraph::default();
        for id in ["lib", "app", "spec", "both"] {
            let mut c = Component::new(id, id, ComponentKind::Module);
            c.path = Some(id.into());
            graph.add_component(c);
        }
        let import = |from: &str, to: &str, file: &str, target: &str, test: bool| {
            Edge::new(from, to, EdgeKind::Import).with_evidence(
                Evidence::new(file)
                    .at_line(1)
                    .pointing_at(target)
                    .in_test(test),
            )
        };
        graph.add_edges([
            import("app", "lib", "app/page.ts", "lib/money.ts", false),
            import("spec", "lib", "spec/money.test.ts", "lib/money.ts", true),
            // production code and a test of `both`
            import("both", "lib", "both/a.ts", "lib/money.ts", false),
            import("both", "lib", "both/a.test.ts", "lib/money.ts", true),
            // a test of app, which production code reaches anyway
            import("spec", "app", "spec/page.test.ts", "app/page.ts", true),
        ]);
        let reach = graph.change_impact(ChangeSeed::File("lib/money.ts"), 2);
        let ids =
            |set: &BTreeSet<ComponentId>| set.iter().map(|c| c.to_string()).collect::<Vec<_>>();
        assert_eq!(ids(&reach.direct), ["app", "both"]);
        assert_eq!(ids(&reach.transitive), ["app", "both"]);
        // the test files to run again, those beside production code included
        assert_eq!(
            reach.tests.iter().collect::<Vec<_>>(),
            ["both/a.test.ts", "spec/money.test.ts", "spec/page.test.ts"]
        );
        // a component's own tests are what to run after changing it
        let reach = graph.change_impact(ChangeSeed::Component(&ComponentId::new("both")), 2);
        assert_eq!(reach.tests.iter().collect::<Vec<_>>(), ["both/a.test.ts"]);
    }

    /// Files that define names (`money.ts`, `date.ts`, `url.ts`), a barrel
    /// that re-exports two of them whole, one name renamed and a package
    /// (`index.ts`), a barrel of that barrel (`all.ts`), a module that
    /// re-exports one name beside its own code (`storage.ts`), one that
    /// imports what it re-exports (`cart.ts`), and an importer per way of
    /// taking names through them, each file a component of its own.
    fn behind_re_exports() -> ArchitectureGraph {
        let mut graph = ArchitectureGraph::default();
        let files = [
            "lib/money.ts",
            "lib/date.ts",
            "lib/url.ts",
            "lib/storage.ts",
            "lib/index.ts",
            "lib/all.ts",
            "lib/cart.ts",
            "app/sale.ts",
            "app/calendar.ts",
            "app/press.ts",
            "app/tag.ts",
            "app/whole.ts",
            "app/boot.ts",
            "app/price.ts",
            "app/deep.ts",
            "app/far.ts",
            "app/upload.ts",
            "app/link.ts",
            "app/basket.ts",
            "spec/upload.test.ts",
            "spec/link.test.ts",
        ];
        for file in files {
            let mut c = Component::new(file, file, ComponentKind::Module);
            c.path = Some(file.into());
            graph.add_component(c);
        }
        graph.add_component(Component::new(
            "ext:npm:aria",
            "aria",
            ComponentKind::External,
        ));
        let statement = |file: &str, note: &str, target: &str, names: &[&str]| {
            Edge::new(file, target, EdgeKind::Import).with_evidence(
                Evidence::new(file)
                    .at_line(1)
                    .with_note(note)
                    .pointing_at(target)
                    .taking(names.iter().copied())
                    .in_test(file.starts_with("spec/")),
            )
        };
        // a statement at another line of a barrel
        let at = |line: u32, edge: Edge| {
            let mut edge = edge;
            edge.evidence[0].line = Some(line);
            edge
        };
        graph.add_edges([
            statement("lib/storage.ts", "export", "lib/url.ts", &["getUrl"]),
            statement("lib/index.ts", "export", "lib/money.ts", &["*"]),
            at(
                2,
                statement("lib/index.ts", "export", "lib/date.ts", &["*"]),
            ),
            at(
                4,
                statement("lib/index.ts", "export", "lib/money.ts", &["formatPrice"]),
            ),
            statement("lib/all.ts", "export", "lib/index.ts", &["*"]),
            statement("lib/cart.ts", "import", "lib/money.ts", &["formatPrice"]),
            // the names a changed file defines, through a barrel
            statement("app/sale.ts", "import", "lib/index.ts", &["formatPrice"]),
            statement(
                "app/sale.ts",
                "import via lib/index.ts:1",
                "lib/money.ts",
                &["formatPrice"],
            ),
            statement("app/price.ts", "import", "lib/index.ts", &["price"]),
            statement(
                "app/price.ts",
                "import via lib/index.ts:4",
                "lib/money.ts",
                &["formatPrice"],
            ),
            statement("app/deep.ts", "import", "lib/all.ts", &["formatPrice"]),
            statement(
                "app/deep.ts",
                "import via lib/all.ts:1",
                "lib/money.ts",
                &["formatPrice"],
            ),
            // another file's names
            statement("app/calendar.ts", "import", "lib/index.ts", &["formatDate"]),
            statement(
                "app/calendar.ts",
                "import via lib/index.ts:2",
                "lib/date.ts",
                &["formatDate"],
            ),
            statement("app/far.ts", "import", "lib/all.ts", &["formatDate"]),
            statement(
                "app/far.ts",
                "import via lib/all.ts:1",
                "lib/date.ts",
                &["formatDate"],
            ),
            // a name of the package, and one both files define, whose walks
            // found nothing
            statement("app/press.ts", "import", "lib/index.ts", &["useButton"]),
            statement("app/tag.ts", "import", "lib/index.ts", &["label"]),
            // the barrel whole, or only loaded
            statement("app/whole.ts", "import", "lib/index.ts", &["*"]),
            statement("app/boot.ts", "import", "lib/index.ts", &[]),
            // the module's own name, and the one it re-exports
            statement("app/upload.ts", "import", "lib/storage.ts", &["save"]),
            statement("app/link.ts", "import", "lib/storage.ts", &["getUrl"]),
            statement(
                "app/link.ts",
                "import via lib/storage.ts:1",
                "lib/url.ts",
                &["getUrl"],
            ),
            statement("spec/upload.test.ts", "import", "lib/storage.ts", &["save"]),
            statement("spec/link.test.ts", "import", "lib/storage.ts", &["getUrl"]),
            statement(
                "spec/link.test.ts",
                "import via lib/storage.ts:1",
                "lib/url.ts",
                &["getUrl"],
            ),
            statement("app/basket.ts", "import", "lib/cart.ts", &["total"]),
            // the package the barrel re-exports whole
            Edge::new("lib/index.ts", "ext:npm:aria", EdgeKind::Import)
                .with_evidence(Evidence::new("lib/index.ts").at_line(3).with_note("export")),
        ]);
        for (file, name) in [
            ("lib/money.ts", "formatPrice"),
            ("lib/money.ts", "label"),
            ("lib/date.ts", "formatDate"),
            ("lib/date.ts", "label"),
            ("lib/url.ts", "getUrl"),
            ("lib/storage.ts", "save"),
            ("lib/cart.ts", "total"),
        ] {
            graph.add_symbol(symbol(
                &format!("{file}::{name}"),
                name,
                vec![Evidence::new(file).at_line(1)],
            ));
        }
        graph
    }

    fn reached(set: &BTreeSet<ComponentId>) -> Vec<&str> {
        set.iter().map(|c| c.as_str()).collect()
    }

    #[test]
    fn a_barrel_is_followed_only_where_it_may_pass_a_changed_files_names_on() {
        let graph = behind_re_exports();
        let reach = graph.change_impact(ChangeSeed::File("lib/money.ts"), 9);
        // the barrel itself, what takes the file's names through it, and
        // what uses them
        assert_eq!(
            reached(&reach.direct),
            [
                "app/deep.ts",
                "app/price.ts",
                "app/sale.ts",
                "lib/cart.ts",
                "lib/index.ts"
            ]
        );
        // through the barrel: what takes it whole or only loads it, a name
        // the file defines that the walk could not place, and the barrel
        // above it; not another file's names, nor the package's
        assert_eq!(
            reached(&reach.transitive),
            [
                "app/basket.ts",
                "app/boot.ts",
                "app/deep.ts",
                "app/price.ts",
                "app/sale.ts",
                "app/tag.ts",
                "app/whole.ts",
                "lib/all.ts",
                "lib/cart.ts",
                "lib/index.ts"
            ]
        );
    }

    #[test]
    fn a_module_passes_on_only_the_name_it_re_exports() {
        let graph = behind_re_exports();
        let reach = graph.change_impact(ChangeSeed::File("lib/url.ts"), 9);
        assert_eq!(reached(&reach.direct), ["app/link.ts", "lib/storage.ts"]);
        assert_eq!(
            reached(&reach.transitive),
            ["app/link.ts", "lib/storage.ts"]
        );
        // not the test of the module's own code
        assert_eq!(
            reach.tests.iter().collect::<Vec<_>>(),
            ["spec/link.test.ts"]
        );
    }

    #[test]
    fn a_changed_barrel_reaches_what_takes_names_through_it() {
        let graph = behind_re_exports();
        let reach = graph.change_impact(ChangeSeed::File("lib/index.ts"), 9);
        // what loads the barrel, whatever it takes; through the barrel
        // above it, what a walk led through the changed one
        for file in [
            "app/calendar.ts",
            "app/press.ts",
            "app/far.ts",
            "app/deep.ts",
        ] {
            assert!(reach.transitive.contains(&ComponentId::new(file)), "{file}");
        }
        assert!(!reach
            .transitive
            .contains(&ComponentId::new("app/upload.ts")));
    }

    #[test]
    fn a_name_a_changed_file_passes_on_is_followed_through_barrels_above() {
        let mut graph = behind_re_exports();
        // the barrel re-exports a name it does not define, below two others
        let mut top = Component::new("lib/top.ts", "lib/top.ts", ComponentKind::Module);
        top.path = Some("lib/top.ts".into());
        graph.add_component(top);
        let mut far = Component::new("app/top.ts", "app/top.ts", ComponentKind::Module);
        far.path = Some("app/top.ts".into());
        graph.add_component(far);
        let statement = |file: &str, note: &str, target: &str, names: &[&str]| {
            Edge::new(file, target, EdgeKind::Import).with_evidence(
                Evidence::new(file)
                    .at_line(1)
                    .with_note(note)
                    .pointing_at(target)
                    .taking(names.iter().copied()),
            )
        };
        graph.add_edges([
            statement("lib/top.ts", "export", "lib/all.ts", &["*"]),
            statement(
                "app/top.ts",
                "import",
                "lib/top.ts",
                &["formatPrice", "formatDate"],
            ),
            statement(
                "app/top.ts",
                "import via lib/top.ts:1",
                "lib/money.ts",
                &["formatPrice"],
            ),
            statement(
                "app/top.ts",
                "import via lib/top.ts:1",
                "lib/date.ts",
                &["formatDate"],
            ),
        ]);
        let reach = graph.change_impact(ChangeSeed::File("lib/index.ts"), 9);
        assert!(reach.transitive.contains(&ComponentId::new("app/top.ts")));
        // the file whose name it takes, but not the other one's
        let reach = graph.change_impact(ChangeSeed::File("lib/date.ts"), 9);
        assert!(reach.transitive.contains(&ComponentId::new("app/top.ts")));
        assert!(!reach.transitive.contains(&ComponentId::new("app/sale.ts")));
    }

    #[test]
    fn a_barrel_that_uses_what_it_re_exports_is_followed_whole() {
        let mut graph = behind_re_exports();
        graph.add_edge(
            Edge::new("lib/storage.ts", "lib/url.ts", EdgeKind::Import).with_evidence(
                Evidence::new("lib/storage.ts")
                    .at_line(2)
                    .with_note("import")
                    .pointing_at("lib/url.ts")
                    .taking(["getUrl"]),
            ),
        );
        let reach = graph.change_impact(ChangeSeed::File("lib/url.ts"), 9);
        assert!(reach
            .transitive
            .contains(&ComponentId::new("app/upload.ts")));
        assert_eq!(
            reach.tests.iter().collect::<Vec<_>>(),
            ["spec/link.test.ts", "spec/upload.test.ts"]
        );
    }

    #[test]
    fn a_barrel_of_a_file_that_re_exports_a_package_passes_any_name_on() {
        let mut graph = behind_re_exports();
        // the changed file re-exports a package, whose names nothing lists
        graph.add_edge(
            Edge::new("lib/money.ts", "ext:npm:aria", EdgeKind::Import).with_evidence(
                Evidence::new("lib/money.ts")
                    .at_line(9)
                    .with_note("export aria, declared in package.json:4"),
            ),
        );
        let reach = graph.change_impact(ChangeSeed::File("lib/money.ts"), 9);
        assert!(reach.transitive.contains(&ComponentId::new("app/press.ts")));
        // a name a walk placed in another file still is that file's
        assert!(!reach
            .transitive
            .contains(&ComponentId::new("app/calendar.ts")));
    }

    #[test]
    fn a_barrel_a_symbol_passes_through_is_followed_whole_where_code_uses_it() {
        let price = symbol(
            "lib/money.ts::formatPrice",
            "formatPrice",
            vec![Evidence::new("lib/money.ts").at_line(1)],
        );
        let mut graph = behind_re_exports();
        // through the barrel alone, only what may take the name
        let reach = graph.change_impact(ChangeSeed::Symbol(&price, &BTreeSet::new()), 9);
        assert!(!reach
            .transitive
            .contains(&ComponentId::new("app/calendar.ts")));
        // the barrel's own code uses what the symbol's importer gives it
        let mut pay = Component::new("app/pay.ts", "app/pay.ts", ComponentKind::Module);
        pay.path = Some("app/pay.ts".into());
        graph.add_component(pay);
        graph.add_edges([
            Edge::new("lib/index.ts", "lib/cart.ts", EdgeKind::Import).with_evidence(
                Evidence::new("lib/index.ts")
                    .at_line(5)
                    .with_note("import")
                    .pointing_at("lib/cart.ts")
                    .taking(["total"]),
            ),
            Edge::new("app/pay.ts", "lib/index.ts", EdgeKind::Import).with_evidence(
                Evidence::new("app/pay.ts")
                    .at_line(1)
                    .with_note("import")
                    .pointing_at("lib/index.ts")
                    .taking(["checkout"]),
            ),
        ]);
        let reach = graph.change_impact(ChangeSeed::Symbol(&price, &BTreeSet::new()), 9);
        assert!(reach.transitive.contains(&ComponentId::new("app/pay.ts")));
        assert!(reach
            .transitive
            .contains(&ComponentId::new("app/calendar.ts")));
    }

    /// `orders.ts` imports `pricing.ts` and a type of `types.ts`,
    /// `checkout.ts` imports `orders.ts` and `index.ts` re-exports it; tests
    /// mock `orders.ts` or `index.ts` with a factory and reach them
    /// directly, through `checkout.ts`, through the barrel or by a type of
    /// `orders.ts`, beside one that loads `orders.ts` for real.
    fn mocked_orders() -> ArchitectureGraph {
        let mut graph = ArchitectureGraph::default();
        let files = [
            "lib/pricing.ts",
            "lib/types.ts",
            "lib/orders.ts",
            "lib/checkout.ts",
            "lib/index.ts",
            "spec/direct.test.ts",
            "spec/typed.test.ts",
            "spec/through.test.ts",
            "spec/barrel.test.ts",
            "spec/real.test.ts",
        ];
        for file in files {
            let mut c = Component::new(file, file, ComponentKind::Module);
            c.path = Some(file.into());
            graph.add_component(c);
        }
        let statement = |file: &str, line: u32, note: &str, target: &str, names: &[&str]| {
            Edge::new(file, target, EdgeKind::Import).with_evidence(
                Evidence::new(file)
                    .at_line(line)
                    .with_note(note)
                    .pointing_at(target)
                    .taking(names.iter().copied())
                    .in_test(file.starts_with("spec/"))
                    .replacing(note == "vi.mock"),
            )
        };
        let typed = |file: &str, line: u32, target: &str, names: &[&str]| {
            let mut edge = statement(file, line, "import", target, names);
            edge.evidence[0].type_only = true;
            edge
        };
        graph.add_edges([
            statement("lib/orders.ts", 1, "import", "lib/pricing.ts", &["price"]),
            typed("lib/orders.ts", 2, "lib/types.ts", &["Price"]),
            typed("spec/typed.test.ts", 1, "lib/orders.ts", &["Line"]),
            statement("spec/typed.test.ts", 2, "vi.mock", "lib/orders.ts", &["*"]),
            statement(
                "lib/checkout.ts",
                1,
                "import",
                "lib/orders.ts",
                &["placeOrder"],
            ),
            statement("lib/index.ts", 1, "export", "lib/orders.ts", &["*"]),
            statement(
                "spec/direct.test.ts",
                1,
                "import",
                "lib/orders.ts",
                &["placeOrder"],
            ),
            statement("spec/direct.test.ts", 2, "vi.mock", "lib/orders.ts", &["*"]),
            statement(
                "spec/through.test.ts",
                1,
                "import",
                "lib/checkout.ts",
                &["checkout"],
            ),
            statement(
                "spec/through.test.ts",
                2,
                "vi.mock",
                "lib/orders.ts",
                &["*"],
            ),
            statement(
                "spec/barrel.test.ts",
                1,
                "import",
                "lib/index.ts",
                &["placeOrder"],
            ),
            statement(
                "spec/barrel.test.ts",
                1,
                "import via lib/index.ts:1",
                "lib/orders.ts",
                &["placeOrder"],
            ),
            statement("spec/barrel.test.ts", 2, "vi.mock", "lib/index.ts", &["*"]),
            statement(
                "spec/real.test.ts",
                1,
                "import",
                "lib/checkout.ts",
                &["checkout"],
            ),
        ]);
        graph
    }

    #[test]
    fn a_test_reaches_nothing_through_a_module_its_mock_replaces() {
        let graph = mocked_orders();
        let reach = |seed: ChangeSeed| graph.change_impact(seed, 9);
        let tests = |reach: &Reach| reach.tests.iter().cloned().collect::<Vec<_>>();
        // what the mocked module imports reaches only the test that loads it
        let pricing = reach(ChangeSeed::File("lib/pricing.ts"));
        assert_eq!(tests(&pricing), ["spec/real.test.ts"]);
        // while its types are the real module's, which a mock replaces not
        let types = reach(ChangeSeed::File("lib/types.ts"));
        assert!(types.tests.contains("spec/typed.test.ts"));
        // each test left out names its mock
        let left: Vec<(&str, Vec<String>)> = pricing
            .left_out
            .iter()
            .map(|(file, mocks)| {
                let at = mocks
                    .iter()
                    .map(|m| format!("{}:{}", m.file, m.line.unwrap()))
                    .collect();
                (file.as_str(), at)
            })
            .collect();
        assert_eq!(
            left,
            [
                (
                    "spec/barrel.test.ts",
                    vec!["spec/barrel.test.ts:2".to_owned()]
                ),
                (
                    "spec/direct.test.ts",
                    vec!["spec/direct.test.ts:2".to_owned()]
                ),
                (
                    "spec/through.test.ts",
                    vec!["spec/through.test.ts:2".to_owned()]
                ),
                (
                    "spec/typed.test.ts",
                    vec!["spec/typed.test.ts:2".to_owned()]
                ),
            ]
        );
        // the mocked module itself: its names are what the mocks replace
        let orders = reach(ChangeSeed::File("lib/orders.ts"));
        assert_eq!(
            tests(&orders),
            [
                "spec/barrel.test.ts",
                "spec/direct.test.ts",
                "spec/real.test.ts",
                "spec/through.test.ts",
                "spec/typed.test.ts"
            ]
        );
        assert!(orders.left_out.is_empty());
        // and a symbol of it, which the direct test takes by name
        let place = Symbol {
            component: "lib/orders.ts".into(),
            ..symbol(
                "lib/orders.ts::placeOrder",
                "placeOrder",
                vec![Evidence::new("lib/orders.ts").at_line(2)],
            )
        };
        let symbol = reach(ChangeSeed::Symbol(&place, &BTreeSet::new()));
        assert_eq!(
            tests(&symbol),
            [
                "spec/barrel.test.ts",
                "spec/direct.test.ts",
                "spec/real.test.ts",
                "spec/through.test.ts",
                "spec/typed.test.ts"
            ]
        );
        // a module that imports what changed outside the scan did not change
        let importers = reach(ChangeSeed::Importers(&["lib/orders.ts"]));
        assert_eq!(tests(&importers), ["spec/real.test.ts"]);
    }

    #[test]
    fn a_mock_of_a_barrel_hides_the_change_unless_it_gives_a_name_passed_on() {
        let mut graph = ArchitectureGraph::default();
        let files = [
            "lib/url.ts",
            "lib/storage.ts",
            "spec/keyed.test.ts",
            "spec/other.test.ts",
        ];
        for file in files {
            let mut c = Component::new(file, file, ComponentKind::Module);
            c.path = Some(file.into());
            graph.add_component(c);
        }
        let statement = |file: &str, line: u32, note: &str, names: &[&str]| {
            let target = match file {
                "lib/storage.ts" => "lib/url.ts",
                _ => "lib/storage.ts",
            };
            Edge::new(file, target, EdgeKind::Import).with_evidence(
                Evidence::new(file)
                    .at_line(line)
                    .with_note(note)
                    .pointing_at(target)
                    .taking(names.iter().copied())
                    .in_test(file.starts_with("spec/"))
                    .replacing(note == "vi.mock"),
            )
        };
        // the module re-exports one name beside its own; both tests take it
        // whole and mock it, one with that name, one with another
        graph.add_edges([
            statement("lib/storage.ts", 1, "export", &["fileUrl"]),
            statement("spec/keyed.test.ts", 1, "import", &["*"]),
            statement("spec/keyed.test.ts", 2, "vi.mock", &["fileUrl"]),
            statement("spec/other.test.ts", 1, "import", &["*"]),
            statement("spec/other.test.ts", 2, "vi.mock", &["upload"]),
        ]);
        let reach = graph.change_impact(ChangeSeed::File("lib/url.ts"), 9);
        assert_eq!(
            reach.tests.iter().collect::<Vec<_>>(),
            ["spec/keyed.test.ts"]
        );
        assert_eq!(
            reach.left_out.keys().collect::<Vec<_>>(),
            ["spec/other.test.ts"]
        );
    }

    #[test]
    fn each_dependent_keeps_its_fewest_steps_and_the_file_it_came_from() {
        let mut graph = ArchitectureGraph::default();
        for file in ["lib/a.ts", "lib/b.ts", "app/c.ts", "app/d.ts", "app/e.ts"] {
            let mut c = Component::new(file, file, ComponentKind::Module);
            c.path = Some(file.into());
            graph.add_component(c);
        }
        let import = |file: &str, target: &str| {
            Edge::new(file, target, EdgeKind::Import).with_evidence(
                Evidence::new(file)
                    .at_line(1)
                    .pointing_at(target)
                    .taking(["x"]),
            )
        };
        // c reaches a through b; d through c and, nearer, a itself; e
        // through d only
        graph.add_edges([
            import("lib/b.ts", "lib/a.ts"),
            import("app/c.ts", "lib/b.ts"),
            import("app/d.ts", "app/c.ts"),
            import("app/d.ts", "lib/a.ts"),
            import("app/e.ts", "app/d.ts"),
        ]);
        let reach = graph.change_impact(ChangeSeed::File("lib/a.ts"), 9);
        let steps: Vec<(&str, usize, Option<String>)> = reach
            .distance
            .iter()
            .map(|(id, d)| (id.as_str(), *d, reach.from.get(id).map(hop_text)))
            .collect();
        assert_eq!(
            steps,
            [
                ("app/c.ts", 2, Some("lib/b.ts".to_owned())),
                ("app/d.ts", 1, Some("lib/a.ts".to_owned())),
                ("app/e.ts", 2, Some("app/d.ts".to_owned())),
                ("lib/b.ts", 1, Some("lib/a.ts".to_owned())),
            ]
        );
    }

    #[test]
    fn a_dependent_an_import_without_a_file_reaches_names_where_it_imports() {
        let mut graph = ArchitectureGraph::default();
        for name in ["core", "mid", "top"] {
            let mut c = Component::new(name, name, ComponentKind::Package);
            c.path = Some(name.into());
            graph.add_component(c);
        }
        graph.add_edges([
            Edge::new("mid", "core", EdgeKind::Dependency)
                .with_evidence(Evidence::new("mid/package.json").at_line(5)),
            // an import of the package whose entry is no scanned file
            Edge::new("top", "mid", EdgeKind::Import)
                .with_evidence(Evidence::new("top/src/page.tsx").at_line(2)),
        ]);
        let reach = graph.change_impact(ChangeSeed::File("core/src/index.ts"), 9);
        let steps: Vec<(&str, usize, Option<String>)> = reach
            .distance
            .iter()
            .map(|(id, d)| (id.as_str(), *d, reach.from.get(id).map(hop_text)))
            .collect();
        assert_eq!(
            steps,
            [
                (
                    "mid",
                    1,
                    Some("core (declared in mid/package.json:5)".to_owned())
                ),
                (
                    "top",
                    2,
                    Some("mid (imported in top/src/page.tsx:2)".to_owned())
                ),
            ]
        );
        // the files of each that the walk reached
        assert_eq!(
            reach.files,
            BTreeMap::from([
                (ComponentId::new("mid"), vec!["mid/package.json".to_owned()]),
                (ComponentId::new("top"), vec!["top/src/page.tsx".to_owned()]),
            ])
        );
    }

    /// A hop as `impact`'s text gives it.
    fn hop_text(hop: &Hop) -> String {
        match hop {
            Hop::File(file) => file.clone(),
            Hop::Component {
                id,
                declared_in,
                imported_in,
            } => match (declared_in, imported_in) {
                (Some((file, line)), _) => {
                    format!("{id} (declared in {file}:{})", line.unwrap_or(0))
                }
                (None, Some((file, line))) => {
                    format!("{id} (imported in {file}:{})", line.unwrap_or(0))
                }
                (None, None) => id.to_string(),
            },
        }
    }

    #[test]
    fn a_dependent_a_declaration_reaches_came_from_what_it_declares() {
        let mut graph = ArchitectureGraph::default();
        for name in ["core", "mid", "top"] {
            let mut c = Component::new(name, name, ComponentKind::Package);
            c.path = Some(name.into());
            graph.add_component(c);
        }
        let declares = |from: &str, to: &str| {
            Edge::new(from, to, EdgeKind::Dependency)
                .with_evidence(Evidence::new(format!("{from}/Cargo.toml")).at_line(7))
        };
        graph.add_edges([declares("mid", "core"), declares("top", "mid")]);
        let reach = graph.change_impact(ChangeSeed::File("core/src/lib.rs"), 9);
        let steps: Vec<(&str, usize, Option<String>)> = reach
            .distance
            .iter()
            .map(|(id, d)| (id.as_str(), *d, reach.from.get(id).map(hop_text)))
            .collect();
        // the component it declares, and where it declares it
        assert_eq!(
            steps,
            [
                (
                    "mid",
                    1,
                    Some("core (declared in mid/Cargo.toml:7)".to_owned())
                ),
                (
                    "top",
                    2,
                    Some("mid (declared in top/Cargo.toml:7)".to_owned())
                ),
            ]
        );
    }

    #[test]
    fn a_changed_test_is_one_to_run_again_whatever_it_imports() {
        let mut graph = ArchitectureGraph::default();
        let mut web = Component::new("web", "web", ComponentKind::Package);
        web.path = Some("web".into());
        graph.add_component(web);
        let module = ComponentId::new("web::tests/only.test.ts");
        let mut test = Component::new(module.clone(), "tests/only.test.ts", ComponentKind::Module);
        test.path = Some("web/tests/only.test.ts".into());
        test.parent = Some("web".into());
        graph.add_component(test);
        // a test that imports only a package it declares for development
        graph.unmapped_imports.push(UnmappedImport {
            from: module.clone(),
            module: "vitest".into(),
            reason: UnmappedReason::DeclaredNotRequired,
            provided_by: Vec::new(),
            evidence: Evidence::new("web/tests/only.test.ts")
                .at_line(1)
                .in_test(true),
        });
        let reach = graph.change_impact(ChangeSeed::Component(&module), 2);
        assert_eq!(
            reach.tests.iter().collect::<Vec<_>>(),
            ["web/tests/only.test.ts"]
        );
    }

    #[test]
    fn change_impact_lists_no_manifest_among_the_tests() {
        let mut graph = ArchitectureGraph::default();
        for (id, path, parent) in [
            ("kiosk", "kiosk", None),
            (
                "kiosk::src/billing.ts",
                "kiosk/src/billing.ts",
                Some("kiosk"),
            ),
            ("other", "other", None),
        ] {
            let mut c = Component::new(id, id, ComponentKind::Package);
            c.path = Some(path.into());
            c.parent = parent.map(ComponentId::new);
            graph.add_component(c);
        }
        graph.add_edges([
            // a test the package owns directly reaches the package itself
            Edge::new("kiosk", "kiosk::src/billing.ts", EdgeKind::Import).with_evidence(
                Evidence::new("kiosk/billing.test.ts")
                    .at_line(1)
                    .pointing_at("kiosk/src/billing.ts")
                    .in_test(true),
            ),
            // from there a manifest's declaration, which names no file
            Edge::new("other", "kiosk", EdgeKind::Dependency)
                .with_evidence(Evidence::new("other/package.json").with_note("dependencies")),
        ]);
        let reach = graph.change_impact(ChangeSeed::File("kiosk/src/billing.ts"), 2);
        assert_eq!(
            reach.tests.iter().collect::<Vec<_>>(),
            ["kiosk/billing.test.ts"]
        );
        // nor does the test, changed, stand for the package
        let reach = graph.change_impact(ChangeSeed::File("kiosk/billing.test.ts"), 2);
        assert!(reach.direct.is_empty(), "{:?}", reach.direct);
        assert!(reach.transitive.is_empty(), "{:?}", reach.transitive);
    }

    #[test]
    fn a_file_reached_through_its_tests_alone_stands_for_no_package() {
        // a binary whose unit tests use one module of its library and whose
        // code uses another; a dependent declares the package only
        let mut graph = ArchitectureGraph::default();
        for (id, kind, path, parent) in [
            ("kiosk", ComponentKind::Package, "kiosk", None),
            (
                "kiosk::till",
                ComponentKind::Module,
                "kiosk/src/till.rs",
                Some("kiosk"),
            ),
            (
                "kiosk::clock",
                ComponentKind::Module,
                "kiosk/src/clock.rs",
                Some("kiosk"),
            ),
            ("depot", ComponentKind::Package, "depot", None),
        ] {
            let mut c = Component::new(id, id, kind);
            c.path = Some(path.into());
            c.parent = parent.map(ComponentId::new);
            graph.add_component(c);
        }
        let import = |to: &str, line: u32, target: &str, test: bool| {
            Edge::new("kiosk", to, EdgeKind::Import).with_evidence(
                Evidence::new("kiosk/src/main.rs")
                    .at_line(line)
                    .pointing_at(target)
                    .in_test(test),
            )
        };
        graph.add_edges([
            import("kiosk::clock", 1, "kiosk/src/clock.rs", false),
            import("kiosk::till", 9, "kiosk/src/till.rs", true),
            Edge::new("depot", "kiosk", EdgeKind::Dependency)
                .with_evidence(Evidence::new("depot/Cargo.toml").with_note("[dependencies]")),
        ]);
        let reach = graph.change_impact(ChangeSeed::File("kiosk/src/till.rs"), 2);
        // its unit tests run again; the package's dependents are not reached
        assert_eq!(
            reach.tests.iter().collect::<Vec<_>>(),
            ["kiosk/src/main.rs"]
        );
        assert!(reach.direct.is_empty(), "{:?}", reach.direct);
    }

    #[test]
    fn files_changed_together_reach_their_tests_in_one_pass() {
        // x.rs uses b.rs in its code and a.rs in its unit tests
        let mut graph = ArchitectureGraph::default();
        for id in ["a", "b", "x"] {
            let mut c = Component::new(id, id, ComponentKind::Module);
            c.path = Some(format!("{id}.rs"));
            graph.add_component(c);
        }
        let import = |to: &str, line: u32, test: bool| {
            Edge::new("x", to, EdgeKind::Import).with_evidence(
                Evidence::new("x.rs")
                    .at_line(line)
                    .pointing_at(format!("{to}.rs"))
                    .in_test(test),
            )
        };
        graph.add_edges([import("b", 1, false), import("a", 9, true)]);
        // a file one of them reaches through production code is no test
        let reach = graph.change_impact(ChangeSeed::Importers(&["a.rs", "b.rs"]), 2);
        assert!(reach.tests.is_empty(), "{:?}", reach.tests);
        assert_eq!(
            reach.direct.iter().map(|c| c.as_str()).collect::<Vec<_>>(),
            ["x"]
        );
    }

    #[test]
    fn only_the_files_dependents_load_stand_for_a_package() {
        // the library's root is the entry; the binary beside it is not
        let mut graph = ArchitectureGraph::default();
        for (id, kind, path, parent) in [
            ("kiosk", ComponentKind::Package, "kiosk", None),
            (
                "kiosk::till",
                ComponentKind::Module,
                "kiosk/src/till.rs",
                Some("kiosk"),
            ),
            (
                "kiosk::clock",
                ComponentKind::Module,
                "kiosk/src/clock.rs",
                Some("kiosk"),
            ),
            ("depot", ComponentKind::Package, "depot", None),
        ] {
            let mut c = Component::new(id, id, kind);
            c.path = Some(path.into());
            c.parent = parent.map(ComponentId::new);
            if id == "kiosk" {
                c.evidence = vec![
                    Evidence::new("kiosk/Cargo.toml").with_note("[package]"),
                    Evidence::new("kiosk/src/lib.rs").with_note("entry"),
                ];
            }
            graph.add_component(c);
        }
        let import = |from_file: &str, to: &str, target: &str| {
            Edge::new("kiosk", to, EdgeKind::Import)
                .with_evidence(Evidence::new(from_file).at_line(1).pointing_at(target))
        };
        graph.add_edges([
            import("kiosk/src/lib.rs", "kiosk::till", "kiosk/src/till.rs"),
            import("kiosk/src/main.rs", "kiosk::clock", "kiosk/src/clock.rs"),
            Edge::new("depot", "kiosk", EdgeKind::Dependency)
                .with_evidence(Evidence::new("depot/Cargo.toml").with_note("[dependencies]")),
        ]);
        let ids =
            |set: &BTreeSet<ComponentId>| set.iter().map(|c| c.to_string()).collect::<Vec<_>>();
        // through the library's root, depot, which links it, is reached
        let till = graph.change_impact(ChangeSeed::File("kiosk/src/till.rs"), 2);
        assert_eq!(ids(&till.transitive), ["depot", "kiosk"]);
        // through the binary, which no dependent loads, it is not
        let clock = graph.change_impact(ChangeSeed::File("kiosk/src/clock.rs"), 2);
        assert_eq!(ids(&clock.transitive), ["kiosk"]);
        let binary = graph.change_impact(ChangeSeed::File("kiosk/src/main.rs"), 2);
        assert!(binary.direct.is_empty(), "{:?}", binary.direct);
    }

    #[test]
    fn a_file_that_defines_production_code_is_no_test() {
        // a Rust module whose only recorded import is in its unit tests
        let mut graph = ArchitectureGraph::default();
        let mut module = Component::new("leaf::a", "leaf::a", ComponentKind::Module);
        module.path = Some("src/a.rs".into());
        graph.add_component(module);
        graph.add_symbol(Symbol {
            id: SymbolId::new("leaf::a::f"),
            name: "f".into(),
            kind: SymbolKind::Function,
            component: ComponentId::new("leaf::a"),
            signature: None,
            evidence: vec![Evidence::new("src/a.rs").at_line(1)],
        });
        graph.unmapped_imports.push(UnmappedImport {
            from: ComponentId::new("leaf::a"),
            module: "pretty_assertions".into(),
            reason: UnmappedReason::DeclaredNotRequired,
            provided_by: vec![],
            evidence: Evidence::new("src/a.rs").at_line(5).in_test(true),
        });
        assert_eq!(graph.test_code().get("src/a.rs"), Some(&false));
        let reach = graph.change_impact(ChangeSeed::File("src/a.rs"), 2);
        assert!(reach.tests.is_empty(), "{:?}", reach.tests);
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

    /// A Python layout: `shop` and its subpackages, with entry files that
    /// run first, a nested project below `shop`, and code outside it.
    fn packages_graph() -> ArchitectureGraph {
        let mut graph = ArchitectureGraph::default();
        let module = |id: &str, path: &str, parent: &str, entry: bool| {
            let mut c = Component::new(id, id, ComponentKind::Module);
            c.path = Some(path.into());
            c.parent = Some(parent.into());
            if entry {
                c.evidence
                    .push(Evidence::new(format!("{path}/__init__.py")).with_note("package"));
            }
            c
        };
        let mut project = component("proj");
        project.path = Some(".".into());
        graph.add_component(project);
        graph.add_component(module("shop", "src/shop", "proj", true));
        graph.add_component(module("shop.billing", "src/shop/billing", "shop", true));
        graph.add_component(module(
            "shop.billing.tests",
            "src/shop/billing/tests",
            "shop.billing",
            true,
        ));
        let mut vendored = component("lib");
        vendored.path = Some("src/shop/vendor".into());
        vendored.parent = Some("shop".into());
        graph.add_component(vendored);
        graph.add_component(module("lib.core", "src/shop/vendor/core", "lib", true));
        graph.add_component(module("app", "app", "proj", true));
        graph.add_component(module("tests", "tests", "proj", false));
        let import = |from: &str, to: &str, file: &str, target: &str| {
            Edge::new(from, to, EdgeKind::Import)
                .with_evidence(Evidence::new(file).at_line(1).pointing_at(target))
        };
        let charge = "src/shop/billing/charge.py";
        graph.add_edges([
            import("app", "shop.billing", "app/main.py", charge),
            Edge::new("app", "shop.billing", EdgeKind::Import).with_evidence(
                Evidence::new("app/types.py")
                    .at_line(1)
                    .pointing_at(charge)
                    .type_only(true),
            ),
            Edge::new("app", "shop.billing", EdgeKind::Import).with_evidence(
                Evidence::new("app/through.py")
                    .at_line(1)
                    .pointing_at(charge)
                    .with_note("import via src/shop/__init__.py:1"),
            ),
            import(
                "app",
                "lib.core",
                "app/vendored.py",
                "src/shop/vendor/core/x.py",
            ),
            // a nested project is outside the package it sits in
            import(
                "lib.core",
                "shop.billing",
                "src/shop/vendor/core/y.py",
                charge,
            ),
            import("shop", "shop.billing", "src/shop/orders.py", charge),
            import(
                "shop.billing",
                "shop.billing",
                "src/shop/billing/ledger.py",
                charge,
            ),
            Edge::new("shop.billing.tests", "shop.billing", EdgeKind::Import).with_evidence(
                Evidence::new("src/shop/billing/tests/test_charge.py")
                    .at_line(1)
                    .pointing_at(charge)
                    .in_test(true),
            ),
            Edge::new("tests", "shop.billing", EdgeKind::Import).with_evidence(
                Evidence::new("tests/test_pay.py")
                    .at_line(1)
                    .pointing_at(charge)
                    .in_test(true),
            ),
        ]);
        graph
    }

    #[test]
    fn imports_below_an_entry_file_that_runs_first_come_from_outside_it() {
        let graph = packages_graph();
        let below = |entry: &str| -> Vec<String> {
            graph
                .imports_below(entry)
                .iter()
                .map(|(_, e)| e.file.clone())
                .collect()
        };
        // neither types only, nor a walk through re-exports, nor a file of
        // the package itself, nor code below a nested project
        assert_eq!(
            below("src/shop/__init__.py"),
            [
                "app/main.py",
                "src/shop/vendor/core/y.py",
                "tests/test_pay.py"
            ]
        );
        assert_eq!(
            below("src/shop/billing/__init__.py"),
            [
                "app/main.py",
                "src/shop/vendor/core/y.py",
                "src/shop/orders.py",
                "tests/test_pay.py"
            ]
        );
        assert_eq!(
            below("src/shop/vendor/core/__init__.py"),
            ["app/vendored.py"]
        );
        // a file that runs nothing first has none
        assert!(below("src/shop/orders.py").is_empty());
    }

    #[test]
    fn change_impact_reaches_what_runs_an_entry_file_first() {
        let graph = packages_graph();
        let ids = |s: &BTreeSet<ComponentId>| s.iter().map(|c| c.0.clone()).collect::<Vec<_>>();
        // a module that imports only the standard library, which the scan
        // records nothing of, still defines its public names
        let mut graph = graph;
        let mut utils = Component::new("shop.utils", "shop.utils", ComponentKind::Module);
        utils.path = Some("src/shop/utils".into());
        utils.parent = Some("shop".into());
        utils
            .evidence
            .push(Evidence::new("src/shop/utils/__init__.py").with_note("package"));
        graph.add_component(utils);
        graph.add_symbol(Symbol {
            id: SymbolId::new("shop.utils::clock::now"),
            name: "now".into(),
            kind: SymbolKind::Function,
            component: "shop.utils".into(),
            signature: None,
            evidence: vec![Evidence::new("src/shop/utils/clock.py").at_line(3)],
        });
        // a test helper that imports only the standard library: test code by
        // its symbol's mark
        graph.add_symbol(Symbol {
            id: SymbolId::new("shop.billing.tests::factories::make_user"),
            name: "make_user".into(),
            kind: SymbolKind::Function,
            component: "shop.billing.tests".into(),
            signature: None,
            evidence: vec![Evidence::new("src/shop/billing/tests/factories.py")
                .at_line(2)
                .in_test(true)],
        });
        // a test that imports only what maps to no component, for its fixtures
        graph.unmapped_imports.push(crate::UnmappedImport {
            from: "shop.billing.tests".into(),
            module: "pytest".into(),
            reason: crate::UnmappedReason::Undeclared,
            provided_by: Vec::new(),
            evidence: Evidence::new("src/shop/billing/tests/test_fixtures.py")
                .at_line(1)
                .in_test(true),
        });
        let reach = graph.change_impact(ChangeSeed::File("src/shop/__init__.py"), 9);
        // app and the nested project import a module below it; a file of a
        // subpackage needs it run
        assert_eq!(
            ids(&reach.direct),
            ["app", "lib.core", "shop.billing", "shop.utils"]
        );
        // the tests below the package as well as outside it
        assert_eq!(
            reach.tests.iter().map(String::as_str).collect::<Vec<_>>(),
            [
                "src/shop/billing/tests/factories.py",
                "src/shop/billing/tests/test_charge.py",
                "src/shop/billing/tests/test_fixtures.py",
                "tests/test_pay.py"
            ]
        );
        // each needs the changed entry file run first
        let runs_first = TestReach {
            ways: vec![TestRoute {
                way: TestWay::RunsFirst {
                    entry: "src/shop/__init__.py".into(),
                },
                steps: 1,
                types_only: false,
            }],
            types_only: false,
        };
        assert!(
            reach.test_ways.values().all(|way| *way == runs_first),
            "{:?}",
            reach.test_ways
        );
        assert_eq!(reach.test_ways.len(), 4);

        // a change to a module that the package's entry file imports reaches
        // what runs that entry file
        let mut graph = packages_graph();
        graph.add_edges([edge_to(
            "shop",
            "shop.billing",
            "src/shop/__init__.py",
            "src/shop/billing/rates.py",
        )]);
        let reach = graph.change_impact(ChangeSeed::File("src/shop/billing/rates.py"), 9);
        assert!(
            reach.transitive.contains(&ComponentId::new("app")),
            "{reach:?}"
        );

        // a symbol's first step still goes by the statements that take it
        let mut graph = packages_graph();
        graph.add_symbol(Symbol {
            id: SymbolId::new("shop::VERSION"),
            name: "VERSION".into(),
            kind: SymbolKind::Constant,
            component: "shop".into(),
            signature: None,
            evidence: vec![Evidence::new("src/shop/__init__.py").at_line(3)],
        });
        let symbol = graph.symbols[&SymbolId::new("shop::VERSION")].clone();
        let reach = graph.change_impact(ChangeSeed::Symbol(&symbol, &BTreeSet::new()), 9);
        assert!(reach.direct.is_empty(), "{reach:?}");
    }

    #[test]
    fn each_test_says_how_it_reaches_the_change() {
        let mut graph = ArchitectureGraph::default();
        for file in ["lib/a.ts", "lib/b.ts", "lib/index.ts"] {
            let mut c = Component::new(file, file, ComponentKind::Module);
            c.path = Some(file.into());
            graph.add_component(c);
        }
        let import = |file: &str, target: &str, test: bool| {
            Edge::new(file, target, EdgeKind::Import).with_evidence(
                Evidence::new(file)
                    .at_line(1)
                    .pointing_at(target)
                    .taking(["x"])
                    .in_test(test),
            )
        };
        graph.add_edges([
            import("lib/b.ts", "lib/a.ts", false),
            Edge::new("lib/index.ts", "lib/a.ts", EdgeKind::Import).with_evidence(
                Evidence::new("lib/index.ts")
                    .at_line(2)
                    .pointing_at("lib/a.ts")
                    .taking(["x"])
                    .with_note("export"),
            ),
            import("tests/direct.test.ts", "lib/a.ts", true),
            Edge::new("tests/typed.test.ts", "lib/a.ts", EdgeKind::Import).with_evidence(
                Evidence::new("tests/typed.test.ts")
                    .at_line(1)
                    .pointing_at("lib/a.ts")
                    .taking(["X"])
                    .type_only(true)
                    .in_test(true),
            ),
            import("tests/through.test.ts", "lib/b.ts", true),
            // a name taken through the barrel, as the scan records it: the
            // barrel loaded, and the file that defines the name
            import("tests/via.test.ts", "lib/index.ts", true),
            Edge::new("tests/via.test.ts", "lib/a.ts", EdgeKind::Import).with_evidence(
                Evidence::new("tests/via.test.ts")
                    .at_line(1)
                    .pointing_at("lib/a.ts")
                    .taking(["x"])
                    .with_note("import via lib/index.ts:2")
                    .in_test(true),
            ),
        ]);
        let reach = graph.change_impact(ChangeSeed::File("lib/a.ts"), 9);
        let ways: Vec<(&str, Vec<TestWay>, bool)> = reach
            .test_ways
            .iter()
            .map(|(file, at)| {
                let ways = at.ways.iter().map(|r| r.way.clone()).collect();
                (file.as_str(), ways, at.types_only)
            })
            .collect();
        let takes = |via: Option<&str>| TestWay::Takes {
            via: via.map(str::to_owned),
        };
        assert_eq!(
            ways,
            [
                ("tests/direct.test.ts", vec![takes(None)], false),
                (
                    "tests/through.test.ts",
                    vec![TestWay::Through {
                        from: "lib/b.ts".into()
                    }],
                    false
                ),
                ("tests/typed.test.ts", vec![takes(None)], true),
                (
                    "tests/via.test.ts",
                    vec![takes(Some("lib/index.ts:2"))],
                    false
                ),
            ]
        );
    }

    #[test]
    fn a_test_shows_the_nearest_way_that_takes_values_and_its_mocks_cut_others() {
        let mut graph = ArchitectureGraph::default();
        for file in [
            "src/x.ts",
            "src/cart.ts",
            "src/m.ts",
            "src/u.ts",
            "src/item.ts",
        ] {
            let mut c = Component::new(file, file, ComponentKind::Module);
            c.path = Some(file.into());
            graph.add_component(c);
        }
        let statement = |file: &str, target: &str, types: bool| {
            Edge::new(file, target, EdgeKind::Import).with_evidence(
                Evidence::new(file)
                    .at_line(1)
                    .pointing_at(target)
                    .taking(["v"])
                    .type_only(types)
                    .in_test(file.starts_with("tests/")),
            )
        };
        graph.add_edges([
            statement("src/cart.ts", "src/x.ts", false),
            // types only, which a mock replaces nothing of
            statement("src/m.ts", "src/x.ts", true),
            statement("src/u.ts", "src/m.ts", false),
            // values of a module that takes only a type of the target
            statement("src/item.ts", "src/x.ts", true),
            statement("tests/item.test.ts", "src/item.ts", false),
            // a type of the target, and values through a module that uses it
            statement("tests/mixed.test.ts", "src/x.ts", true),
            statement("tests/mixed.test.ts", "src/cart.ts", false),
            // values only through a module its mock replaces, a type beside
            statement("tests/u.test.ts", "src/u.ts", false),
            statement("tests/u.test.ts", "src/m.ts", true),
            Edge::new("tests/u.test.ts", "src/m.ts", EdgeKind::Import).with_evidence(
                Evidence::new("tests/u.test.ts")
                    .at_line(4)
                    .pointing_at("src/m.ts")
                    .taking(["other"])
                    .replacing(true)
                    .in_test(true),
            ),
        ]);
        let reach = graph.change_impact(ChangeSeed::File("src/x.ts"), 9);
        let route = |way: TestWay, steps: usize, types_only: bool| TestRoute {
            way,
            steps,
            types_only,
        };
        let through = |from: &str| TestWay::Through { from: from.into() };
        // the type taken at one step, and the values its run loads at two
        assert_eq!(
            reach.test_ways["tests/mixed.test.ts"],
            TestReach {
                ways: vec![
                    route(TestWay::Takes { via: None }, 1, true),
                    route(through("src/cart.ts"), 2, false),
                ],
                types_only: false,
            }
        );
        // a type further on: running the test runs none of the change
        assert_eq!(
            reach.test_ways["tests/item.test.ts"],
            TestReach {
                ways: vec![route(through("src/item.ts"), 2, true)],
                types_only: true,
            }
        );
        // its mock cuts the values: only the type is left
        assert_eq!(
            reach.test_ways["tests/u.test.ts"],
            TestReach {
                ways: vec![route(through("src/m.ts"), 2, true)],
                types_only: true,
            }
        );
    }

    fn edge_to(from: &str, to: &str, file: &str, target: &str) -> Edge {
        Edge::new(from, to, EdgeKind::Import)
            .with_evidence(Evidence::new(file).at_line(1).pointing_at(target))
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
        assert!(json.contains("\"schema_version\":4"));
        assert!(json.contains("\"kind\":\"import\""));
    }
}
