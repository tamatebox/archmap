//! Declared architecture and the rules checked against the observed graph.
//!
//! The declared side is written by people, in `archmap.toml`: named groups
//! of components, forbidden dependencies, ordered layers, the dependencies
//! each component is expected to have, which parts of the repository must
//! be declared at all, whether cycles are allowed, and whether imports of
//! undeclared packages are allowed. It
//! never changes the observed graph. [`check`] only compares the two and
//! reports [`Finding`]s, each with the evidence behind it.
//!
//! A selector is either a path prefix relative to the repository root
//! (`src/domain` covers `src/domain` and everything below it; `.` covers
//! everything) or an external component id (`ext:pypi:requests`, with a
//! trailing `*` for a prefix such as `ext:pypi:google-*`). An external
//! selector without an ecosystem (`ext:requests`) matches nothing.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::graph::strongly_connected;
use crate::{
    ArchitectureGraph, Component, ComponentId, ComponentKind, EdgeKind, Evidence, Scope,
    UnmappedImport, UnmappedReason,
};

/// The rules file as written by its authors.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleSet {
    /// Roll-up depth for cycle detection. `None` leaves the choice to the
    /// caller.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<usize>,
    /// Declared component name to the selectors it covers.
    #[serde(default)]
    pub components: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub deny: Vec<DenyRule>,
    #[serde(default)]
    pub cycles: CycleRule,
    #[serde(default)]
    pub undeclared_imports: UndeclaredImportRule,
    #[serde(default)]
    pub layers: LayerRule,
    #[serde(default)]
    pub allow: Vec<AllowRule>,
    #[serde(default)]
    pub coverage: CoverageRule,
}

/// Declared components from the top layer down. A layer may depend on the
/// layers below it, never on one above.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LayerRule {
    #[serde(default)]
    pub order: Vec<String>,
}

/// The declared components that `from` may depend on. Once a component has
/// an allow entry, any other dependency on a declared component is
/// unexpected, and an allowed dependency that no longer exists is stale.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AllowRule {
    pub from: String,
    #[serde(default)]
    pub to: Vec<String>,
}

/// Selectors whose components must all belong to a declared component.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoverageRule {
    #[serde(default)]
    pub require: Vec<String>,
}

/// A dependency that must not exist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DenyRule {
    /// A declared component name, or a selector.
    pub from: String,
    /// A declared component name, or a selector.
    pub to: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CycleRule {
    /// Report components that depend on each other through a cycle.
    #[serde(default)]
    pub forbid: bool,
    /// Selectors that limit reporting to cycles with at least one member
    /// they cover, such as product code but not fixtures. Empty: all.
    #[serde(default)]
    pub scope: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UndeclaredImportRule {
    /// Report imports that resolve to nothing internal, standard or declared.
    #[serde(default)]
    pub forbid: bool,
    /// Module prefixes to accept anyway, `.` or `/` separated: `ujson`
    /// covers `ujson` and `ujson.*`, `lodash` covers `lodash/fp`. Useful for
    /// optional imports behind `try` / `except`.
    #[serde(default)]
    pub ignore: Vec<String>,
}

/// What the files behind a component cycle show. A component cycle can be
/// made of files that never form a cycle themselves: roll-up joins the files
/// of each component, so different files can close the loop. Like the
/// cycle, it counts only imports that run, not those of types only. None of
/// these states says that a program fails at runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum FileLevel {
    /// No evidence behind the cycle names the imported files.
    Unknown,
    /// Different files form each direction; only the components form a
    /// cycle.
    NoCycle,
    /// Files of at least two of the components form a cycle.
    Cycle {
        files: Vec<String>,
        /// The cycle also closes when local (function-level) imports are
        /// left out.
        at_module_scope: bool,
    },
}

/// One dependency inside a cycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CycleEdge {
    pub from: ComponentId,
    pub to: ComponentId,
    pub kind: EdgeKind,
    pub evidence: Vec<Evidence>,
}

/// A difference between what is declared and what is observed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Finding {
    /// An observed dependency that a deny rule forbids.
    Forbidden {
        /// Position of the rule in `deny`, starting at 0.
        rule: usize,
        deny: DenyRule,
        from: ComponentId,
        to: ComponentId,
        edge: EdgeKind,
        evidence: Vec<Evidence>,
    },
    /// Components that depend on each other through a cycle.
    Cycle {
        components: Vec<ComponentId>,
        edges: Vec<CycleEdge>,
        /// Whether the files behind the component cycle form a cycle too.
        file_level: FileLevel,
    },
    /// A dependency from a layer on a layer above it.
    LayerViolation {
        from_layer: String,
        to_layer: String,
        from: ComponentId,
        to: ComponentId,
        edge: EdgeKind,
        evidence: Vec<Evidence>,
    },
    /// A dependency between declared components that the allow list of
    /// `declared_from` does not name.
    UnexpectedDependency {
        declared_from: String,
        declared_to: String,
        from: ComponentId,
        to: ComponentId,
        edge: EdgeKind,
        evidence: Vec<Evidence>,
    },
    /// An allowed dependency that the code no longer has.
    StaleAllowance { from: String, to: String },
    /// A component that a coverage selector requires to be declared, but
    /// that belongs to no declared component.
    Uncovered {
        component: ComponentId,
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
    /// An import that resolves to nothing internal, standard or declared.
    UndeclaredImport {
        from: ComponentId,
        module: String,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        provided_by: Vec<String>,
        evidence: Evidence,
    },
    /// A declared selector or rule side that matches no observed component,
    /// usually a typo or code that moved. Reported so that a rule never
    /// silently stops applying.
    Unmatched { declared: String, selector: String },
}

/// Compare the observed `graph` with `rules`.
///
/// Deny rules are evaluated on the graph as scanned. Cycles are looked for
/// among the components of `graph.rollup(depth)`.
pub fn check(graph: &ArchitectureGraph, rules: &RuleSet, depth: usize) -> Vec<Finding> {
    let mut findings = Vec::new();

    for (name, selectors) in &rules.components {
        for selector in selectors {
            if !graph
                .components
                .values()
                .any(|c| selector_matches(selector, c))
            {
                findings.push(Finding::Unmatched {
                    declared: format!("components.{name}"),
                    selector: selector.clone(),
                });
            }
        }
    }

    let membership = declared_membership(graph, rules);
    let loaded = loaded_dependencies(graph);
    for (index, rule) in rules.deny.iter().enumerate() {
        let from = Side::new(&rule.from, rules);
        let to = Side::new(&rule.to, rules);
        for (field, side, text) in [("from", &from, &rule.from), ("to", &to, &rule.to)] {
            if !graph
                .components
                .values()
                .any(|c| side.contains(c, &membership))
            {
                findings.push(Finding::Unmatched {
                    declared: format!("deny[{index}].{field}"),
                    selector: text.clone(),
                });
            }
        }
        for dependency in &loaded {
            let (Some(source), Some(target)) = (
                graph.component(dependency.from),
                graph.component(dependency.to),
            ) else {
                continue;
            };
            if from.contains(source, &membership) && to.contains(target, &membership) {
                findings.push(Finding::Forbidden {
                    rule: index,
                    deny: rule.clone(),
                    from: dependency.from.clone(),
                    to: dependency.to.clone(),
                    edge: dependency.kind,
                    evidence: dependency.evidence.clone(),
                });
            }
        }
    }

    findings.extend(check_layers(&loaded, rules, &membership));
    findings.extend(check_allow(&loaded, rules, &membership));
    findings.extend(check_coverage(graph, rules, &membership, depth));

    if rules.undeclared_imports.forbid {
        let ignore = &rules.undeclared_imports.ignore;
        let undeclared: Vec<&UnmappedImport> = graph
            .unmapped_imports
            .iter()
            .filter(|i| i.reason == UnmappedReason::Undeclared)
            .collect();
        for import in &undeclared {
            if !ignore.iter().any(|prefix| import.covered_by(prefix)) {
                let UnmappedImport {
                    from,
                    module,
                    provided_by,
                    evidence,
                    ..
                } = (*import).clone();
                findings.push(Finding::UndeclaredImport {
                    from,
                    module,
                    provided_by,
                    evidence,
                });
            }
        }
        for prefix in ignore {
            if !undeclared.iter().any(|i| i.covered_by(prefix)) {
                findings.push(Finding::Unmatched {
                    declared: "undeclared_imports.ignore".into(),
                    selector: prefix.clone(),
                });
            }
        }
    }

    if rules.cycles.forbid {
        let rolled = graph.rollup(depth);
        let scope = &rules.cycles.scope;
        for (i, selector) in scope.iter().enumerate() {
            if !graph
                .components
                .values()
                .any(|c| selector_matches(selector, c))
            {
                findings.push(Finding::Unmatched {
                    declared: format!("cycles.scope[{i}]"),
                    selector: selector.clone(),
                });
            }
        }
        for components in rolled.cycles() {
            let in_scope = scope.is_empty()
                || components.iter().any(|id| {
                    rolled
                        .component(id)
                        .is_some_and(|c| scope.iter().any(|s| selector_matches(s, c)))
                });
            if !in_scope {
                continue;
            }
            let members: BTreeSet<&ComponentId> = components.iter().collect();
            // what runs closes the cycle: imports of types only and test
            // code are left out
            let edges = rolled
                .edges
                .iter()
                .filter(|e| members.contains(&e.from) && members.contains(&e.to))
                .filter(|e| e.runs_in_production())
                .map(|e| CycleEdge {
                    from: e.from.clone(),
                    to: e.to.clone(),
                    kind: e.kind,
                    evidence: e
                        .evidence
                        .iter()
                        .filter(|e| e.runs_in_production())
                        .cloned()
                        .collect(),
                })
                .collect();
            let file_level = file_level(graph, &members, depth);
            findings.push(Finding::Cycle {
                components,
                edges,
                file_level,
            });
        }
    }

    findings
}

/// Report `names` that are not declared components.
fn unknown_names<'a>(
    rules: &RuleSet,
    names: impl IntoIterator<Item = (String, &'a String)>,
) -> Vec<Finding> {
    names
        .into_iter()
        .filter(|(_, name)| !rules.components.contains_key(*name))
        .map(|(declared, name)| Finding::Unmatched {
            declared,
            selector: name.clone(),
        })
        .collect()
}

/// A dependency as `deny`, `layers` and `allow` count it: from a component
/// to the component that holds a file its statements load.
struct Loaded<'g> {
    from: &'g ComponentId,
    to: &'g ComponentId,
    kind: EdgeKind,
    evidence: Vec<Evidence>,
}

/// The dependencies in production code that rules count: what only tests
/// import is no violation, and a statement counts toward the file it loads.
/// Evidence through a re-export (`via`) counts toward the component of the
/// re-export's file, which the statement loads, when the graph records the
/// re-export as an import of that component (a TS/JS `export ... from`, a
/// Python `from .x import y`, a Rust `pub use` from outside the module's
/// subtree), so a rule can require going through a facade, which a rule
/// of its own then covers. A Rust `pub use` of the module's own subtree is
/// no import, so the statement counts toward the component that defines
/// the name. A statement with evidence of its own for the re-export's
/// component is shown by it alone.
fn loaded_dependencies(graph: &ArchitectureGraph) -> Vec<Loaded<'_>> {
    fn place(at: &str) -> Option<(&str, u32)> {
        let (file, line) = at.rsplit_once(':')?;
        Some((file, line.parse().ok()?))
    }
    // the component of each statement that imports something
    let statements: BTreeMap<(&str, u32), &ComponentId> = graph
        .edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::Import)
        .flat_map(|edge| {
            edge.evidence
                .iter()
                .filter(|e| !e.test)
                .filter_map(move |e| Some(((e.file.as_str(), e.line?), &edge.from)))
        })
        .collect();
    // each file's statements that load a file, with their component: a
    // barrel that imports a name on one line and passes it on from another
    // (`import { x } from './m'`, then `export { x }`) re-exports it through
    // that import, which takes the same name
    let mut loading: BTreeMap<(&str, &str), Vec<(&Evidence, &ComponentId)>> = BTreeMap::new();
    for edge in graph
        .edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::Import)
    {
        for e in edge.evidence.iter().filter(|e| !e.test) {
            if let Some(target) = e.target.as_deref() {
                loading
                    .entry((e.file.as_str(), target))
                    .or_default()
                    .push((e, &edge.from));
            }
        }
    }
    let through_import = |at: (&str, u32), e: &Evidence| {
        loading
            .get(&(at.0, e.target.as_deref()?))?
            .iter()
            .find(|(i, _)| i.names.iter().any(|n| e.names.contains(n)))
            .map(|(_, from)| *from)
    };
    let mut loaded: BTreeMap<(&ComponentId, &ComponentId, EdgeKind), Vec<&Evidence>> =
        BTreeMap::new();
    for edge in graph.edges.iter().filter(|edge| edge.in_production()) {
        if edge.evidence.is_empty() {
            loaded.entry((&edge.from, &edge.to, edge.kind)).or_default();
        }
        for e in edge.evidence.iter().filter(|e| !e.test) {
            let to = e
                .via()
                .and_then(place)
                .and_then(|at| {
                    statements
                        .get(&at)
                        .copied()
                        .or_else(|| through_import(at, e))
                })
                .unwrap_or(&edge.to);
            if to != &edge.from {
                loaded
                    .entry((&edge.from, to, edge.kind))
                    .or_default()
                    .push(e);
            }
        }
    }
    loaded
        .into_iter()
        .map(|((from, to, kind), evidence)| {
            let own: BTreeSet<(&str, Option<u32>)> = evidence
                .iter()
                .filter(|e| e.via().is_none())
                .map(|e| (e.file.as_str(), e.line))
                .collect();
            let evidence = evidence
                .into_iter()
                .filter(|e| e.via().is_none() || !own.contains(&(e.file.as_str(), e.line)))
                .cloned()
                .collect();
            Loaded {
                from,
                to,
                kind,
                evidence,
            }
        })
        .collect()
}

/// Dependencies whose both ends belong to (different) declared components,
/// with the names of those components.
fn declared_dependencies<'a>(
    loaded: &'a [Loaded<'a>],
    membership: &'a BTreeMap<ComponentId, &'a str>,
) -> impl Iterator<Item = (&'a str, &'a str, &'a Loaded<'a>)> + 'a {
    loaded.iter().filter_map(move |dependency| {
        let from = *membership.get(dependency.from)?;
        let to = *membership.get(dependency.to)?;
        (from != to).then_some((from, to, dependency))
    })
}

fn check_layers(
    loaded: &[Loaded<'_>],
    rules: &RuleSet,
    membership: &BTreeMap<ComponentId, &str>,
) -> Vec<Finding> {
    let order = &rules.layers.order;
    let mut findings = unknown_names(
        rules,
        order
            .iter()
            .enumerate()
            .map(|(i, n)| (format!("layers.order[{i}]"), n)),
    );
    let rank: BTreeMap<&str, usize> = order
        .iter()
        .enumerate()
        .map(|(i, n)| (n.as_str(), i))
        .collect();
    for (from, to, dependency) in declared_dependencies(loaded, membership) {
        if let (Some(&upper), Some(&lower)) = (rank.get(from), rank.get(to)) {
            if lower < upper {
                findings.push(Finding::LayerViolation {
                    from_layer: from.to_owned(),
                    to_layer: to.to_owned(),
                    from: dependency.from.clone(),
                    to: dependency.to.clone(),
                    edge: dependency.kind,
                    evidence: dependency.evidence.clone(),
                });
            }
        }
    }
    findings
}

fn check_allow(
    loaded: &[Loaded<'_>],
    rules: &RuleSet,
    membership: &BTreeMap<ComponentId, &str>,
) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut allowed: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for (i, rule) in rules.allow.iter().enumerate() {
        let mut names = vec![(format!("allow[{i}].from"), &rule.from)];
        names.extend(rule.to.iter().map(|to| (format!("allow[{i}].to"), to)));
        findings.extend(unknown_names(rules, names));
        allowed
            .entry(rule.from.as_str())
            .or_default()
            .extend(rule.to.iter().map(String::as_str));
    }

    let mut observed: BTreeSet<(&str, &str)> = BTreeSet::new();
    for (from, to, dependency) in declared_dependencies(loaded, membership) {
        observed.insert((from, to));
        if allowed
            .get(from)
            .is_some_and(|targets| !targets.contains(to))
        {
            findings.push(Finding::UnexpectedDependency {
                declared_from: from.to_owned(),
                declared_to: to.to_owned(),
                from: dependency.from.clone(),
                to: dependency.to.clone(),
                edge: dependency.kind,
                evidence: dependency.evidence.clone(),
            });
        }
    }
    for (from, targets) in &allowed {
        for to in targets {
            let known = rules.components.contains_key(*from) && rules.components.contains_key(*to);
            if known && from != to && !observed.contains(&(*from, *to)) {
                findings.push(Finding::StaleAllowance {
                    from: (*from).to_owned(),
                    to: (*to).to_owned(),
                });
            }
        }
    }
    findings
}

/// Components at the roll-up `depth` that a coverage selector covers must
/// belong to a declared component. Only leaves count: a container such as
/// `src` is covered by the declarations of what it contains.
fn check_coverage(
    graph: &ArchitectureGraph,
    rules: &RuleSet,
    membership: &BTreeMap<ComponentId, &str>,
    depth: usize,
) -> Vec<Finding> {
    let require = &rules.coverage.require;
    if require.is_empty() {
        return Vec::new();
    }
    let mut findings = Vec::new();
    for (i, selector) in require.iter().enumerate() {
        if !graph
            .components
            .values()
            .any(|c| selector_matches(selector, c))
        {
            findings.push(Finding::Unmatched {
                declared: format!("coverage.require[{i}]"),
                selector: selector.clone(),
            });
        }
    }
    let rolled = graph.rollup(depth);
    let parents: BTreeSet<&ComponentId> = rolled
        .components
        .values()
        .filter_map(|c| c.parent.as_ref())
        .collect();
    for component in rolled.components.values() {
        let leaf = component.kind != ComponentKind::External && !parents.contains(&component.id);
        if leaf
            && require.iter().any(|s| selector_matches(s, component))
            && !membership.contains_key(&component.id)
        {
            findings.push(Finding::Uncovered {
                component: component.id.clone(),
                path: component.path.clone(),
            });
        }
    }
    findings
}

/// Look for a file cycle behind a component cycle whose members are
/// `members` at `depth`: a strongly connected group of files that spans at
/// least two of the members.
fn file_level(
    graph: &ArchitectureGraph,
    members: &BTreeSet<&ComponentId>,
    depth: usize,
) -> FileLevel {
    let mut owner: BTreeMap<&str, ComponentId> = BTreeMap::new();
    let mut pairs: Vec<(&str, &str, Option<Scope>)> = Vec::new();
    for edge in &graph.edges {
        let (from, to) = (
            graph.ancestor_at(&edge.from, depth),
            graph.ancestor_at(&edge.to, depth),
        );
        if !members.contains(&from) || !members.contains(&to) {
            continue;
        }
        for e in edge.evidence.iter().filter(|e| e.runs_in_production()) {
            if let Some(target) = e.target.as_deref() {
                owner.insert(e.file.as_str(), from.clone());
                owner.insert(target, to.clone());
                pairs.push((e.file.as_str(), target, e.scope));
            }
        }
    }
    if pairs.is_empty() {
        return FileLevel::Unknown;
    }
    let spans = |group: &Vec<&str>| {
        group
            .iter()
            .filter_map(|f| owner.get(f))
            .collect::<BTreeSet<_>>()
            .len()
            >= 2
    };
    let Some(files) = strongly_connected(pairs.iter().map(|(a, b, _)| (*a, *b)))
        .into_iter()
        .find(|group| spans(group))
    else {
        return FileLevel::NoCycle;
    };
    let at_module_scope = strongly_connected(
        pairs
            .iter()
            .filter(|(_, _, scope)| *scope != Some(Scope::Local))
            .map(|(a, b, _)| (*a, *b)),
    )
    .iter()
    .any(spans);
    FileLevel::Cycle {
        files: files.into_iter().map(str::to_owned).collect(),
        at_module_scope,
    }
}

/// Whether `selector` is an external selector written without an ecosystem
/// (`ext:requests`, `ext:py*`), as before external ids carried one. It
/// matches nothing, so the check reports it instead of reading `ext:py*` as
/// every PyPI package. The bare `ext:*` still covers every external
/// dependency.
pub fn external_selector_lacks_ecosystem(selector: &str) -> bool {
    selector
        .trim()
        .strip_prefix("ext:")
        .is_some_and(|rest| rest != "*" && !rest.contains(':'))
}

/// Does `selector` cover `component`?
pub fn selector_matches(selector: &str, component: &Component) -> bool {
    let selector = selector.trim();
    if selector.starts_with("ext:") {
        if external_selector_lacks_ecosystem(selector) {
            return false;
        }
        let id = component.id.as_str();
        return match selector.strip_suffix('*') {
            Some(prefix) => id.starts_with(prefix),
            None => id == selector,
        };
    }
    if component.kind == ComponentKind::External {
        return false;
    }
    let Some(path) = component.path.as_deref() else {
        return false;
    };
    let (selector, path) = (normalize(selector), normalize(path));
    selector.is_empty() || path == selector || path.starts_with(&format!("{selector}/"))
}

fn normalize(path: &str) -> &str {
    let path = path.trim_start_matches("./").trim_end_matches('/');
    if path == "." {
        ""
    } else {
        path
    }
}

/// Each component belongs to at most one declared component: the one with
/// the most specific (longest) matching selector.
fn declared_membership<'a>(
    graph: &ArchitectureGraph,
    rules: &'a RuleSet,
) -> BTreeMap<ComponentId, &'a str> {
    let mut membership = BTreeMap::new();
    for component in graph.components.values() {
        let best = rules
            .components
            .iter()
            .flat_map(|(name, selectors)| selectors.iter().map(move |s| (name, s)))
            .filter(|(_, s)| selector_matches(s, component))
            .max_by_key(|(name, s)| (normalize(s.trim()).len(), std::cmp::Reverse(name.as_str())));
        if let Some((name, _)) = best {
            membership.insert(component.id.clone(), name.as_str());
        }
    }
    membership
}

/// One side of a deny rule.
enum Side<'a> {
    Declared(&'a str),
    Selector(&'a str),
}

impl<'a> Side<'a> {
    fn new(text: &'a str, rules: &RuleSet) -> Self {
        if rules.components.contains_key(text) {
            Side::Declared(text)
        } else {
            Side::Selector(text)
        }
    }

    fn contains(&self, component: &Component, membership: &BTreeMap<ComponentId, &str>) -> bool {
        match self {
            Side::Declared(name) => membership.get(&component.id) == Some(name),
            Side::Selector(selector) => selector_matches(selector, component),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Edge, Evidence};

    fn module(id: &str, path: &str) -> Component {
        let mut c = Component::new(id, id, ComponentKind::Module);
        c.path = Some(path.into());
        c
    }

    /// src.domain, src.domain.io, src.jobs, scripts, ext:pypi:requests
    fn graph() -> ArchitectureGraph {
        let mut graph = ArchitectureGraph::default();
        let mut root = Component::new("app", "app", ComponentKind::Package);
        root.path = Some(".".into());
        graph.add_component(root);
        for (id, path) in [
            ("src.domain", "src/domain"),
            ("src.domain.io", "src/domain/io"),
            ("src.jobs", "src/jobs"),
            ("scripts", "scripts"),
        ] {
            graph.add_component(module(id, path));
        }
        graph.add_component(Component::new(
            "ext:pypi:requests",
            "requests",
            ComponentKind::External,
        ));
        graph.add_edge(
            Edge::new("src.domain.io", "src.jobs", EdgeKind::Import)
                .with_evidence(Evidence::new("src/domain/io/read.py").at_line(3)),
        );
        graph.add_edge(Edge::new("src.jobs", "src.domain", EdgeKind::Import));
        graph.add_edge(Edge::new("scripts", "ext:pypi:requests", EdgeKind::Import));
        graph
    }

    #[test]
    fn selectors_match_path_prefixes_and_external_ids() {
        let g = graph();
        let c = |id: &str| g.component(&id.into()).unwrap();
        assert!(selector_matches("src/domain", c("src.domain.io")));
        assert!(selector_matches("./src/domain/", c("src.domain")));
        assert!(!selector_matches("src/dom", c("src.domain")));
        assert!(selector_matches(".", c("scripts")));
        assert!(!selector_matches(".", c("ext:pypi:requests")));
        assert!(selector_matches(
            "ext:pypi:requests",
            c("ext:pypi:requests")
        ));
        assert!(selector_matches("ext:pypi:req*", c("ext:pypi:requests")));
        assert!(!selector_matches("requests", c("ext:pypi:requests")));
    }

    #[test]
    fn deny_rules_report_forbidden_edges_with_evidence() {
        let set = RuleSet {
            components: BTreeMap::from([
                ("domain".to_owned(), vec!["src/domain".to_owned()]),
                ("jobs".to_owned(), vec!["src/jobs".to_owned()]),
            ]),
            deny: vec![DenyRule {
                from: "domain".into(),
                to: "jobs".into(),
                reason: Some("domain stays independent".into()),
            }],
            ..RuleSet::default()
        };
        let findings = check(&graph(), &set, 2);
        assert_eq!(findings.len(), 1);
        let Finding::Forbidden {
            from,
            to,
            evidence,
            rule,
            ..
        } = &findings[0]
        else {
            panic!("expected a forbidden edge: {findings:?}");
        };
        assert_eq!(
            (from.as_str(), to.as_str(), *rule),
            ("src.domain.io", "src.jobs", 0)
        );
        assert_eq!(evidence[0].file, "src/domain/io/read.py");
    }

    #[test]
    fn the_most_specific_declaration_wins() {
        // `src` would also cover src/jobs; the longer selector decides
        let set = RuleSet {
            components: BTreeMap::from([
                ("everything".to_owned(), vec!["src".to_owned()]),
                ("jobs".to_owned(), vec!["src/jobs".to_owned()]),
            ]),
            deny: vec![DenyRule {
                from: "jobs".into(),
                to: "everything".into(),
                reason: None,
            }],
            ..RuleSet::default()
        };
        let findings = check(&graph(), &set, 2);
        let forbidden: Vec<(&str, &str)> = findings
            .iter()
            .filter_map(|f| match f {
                Finding::Forbidden { from, to, .. } => Some((from.as_str(), to.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(forbidden, vec![("src.jobs", "src.domain")]);
    }

    #[test]
    fn selectors_work_without_declarations_and_typos_are_reported() {
        let set = RuleSet {
            deny: vec![
                DenyRule {
                    from: "scripts".into(),
                    to: "ext:pypi:requests".into(),
                    reason: None,
                },
                DenyRule {
                    from: "domian".into(),
                    to: "src/jobs".into(),
                    reason: None,
                },
            ],
            components: BTreeMap::from([("legacy".to_owned(), vec!["src/legacy".to_owned()])]),
            ..RuleSet::default()
        };
        let findings = check(&graph(), &set, 2);
        assert!(findings.contains(&Finding::Unmatched {
            declared: "components.legacy".into(),
            selector: "src/legacy".into()
        }));
        assert!(findings.contains(&Finding::Unmatched {
            declared: "deny[1].from".into(),
            selector: "domian".into()
        }));
        assert!(findings.iter().any(
            |f| matches!(f, Finding::Forbidden { to, .. } if to.as_str() == "ext:pypi:requests")
        ));
    }

    #[test]
    fn external_selectors_name_the_ecosystem() {
        // An `archmap.toml` written before external ids carried their
        // ecosystem says `ext:requests`. It matches nothing now, and the
        // check says so instead of passing silently.
        let set = RuleSet {
            deny: vec![DenyRule {
                from: "scripts".into(),
                to: "ext:requests".into(),
                reason: None,
            }],
            ..RuleSet::default()
        };
        let findings = check(&graph(), &set, 2);
        assert!(findings.contains(&Finding::Unmatched {
            declared: "deny[0].to".into(),
            selector: "ext:requests".into()
        }));
    }

    #[test]
    fn external_selectors_without_an_ecosystem_match_nothing() {
        // `ext:py*` was written for names such as `ext:pyyaml`. Read against
        // ids that name their ecosystem, it would cover every PyPI package
        // and quietly change what a rule means.
        let g = graph();
        let c = |id: &str| g.component(&id.into()).unwrap();
        assert!(!selector_matches("ext:py*", c("ext:pypi:requests")));
        assert!(!selector_matches("ext:req*", c("ext:pypi:requests")));
        assert!(selector_matches("ext:*", c("ext:pypi:requests")));
        assert!(selector_matches("ext:pypi:*", c("ext:pypi:requests")));
        let set = RuleSet {
            deny: vec![DenyRule {
                from: "scripts".into(),
                to: "ext:py*".into(),
                reason: None,
            }],
            ..RuleSet::default()
        };
        assert!(check(&g, &set, 2).contains(&Finding::Unmatched {
            declared: "deny[0].to".into(),
            selector: "ext:py*".into()
        }));
    }

    #[test]
    fn undeclared_imports_are_reported_unless_ignored() {
        let mut g = graph();
        for (module, line) in [("scipy.stats", 1), ("ujson", 2)] {
            g.unmapped_imports.push(UnmappedImport {
                from: "scripts".into(),
                module: module.into(),
                reason: UnmappedReason::Undeclared,
                provided_by: vec![],
                evidence: Evidence::new("scripts/run.py").at_line(line),
            });
        }
        let mut set = RuleSet::default();
        // off by default
        assert!(check(&g, &set, 2).is_empty());

        set.undeclared_imports = UndeclaredImportRule {
            forbid: true,
            ignore: vec!["ujson".into(), "orjson".into()],
        };
        let findings = check(&g, &set, 2);
        assert_eq!(
            findings,
            vec![
                Finding::UndeclaredImport {
                    from: "scripts".into(),
                    module: "scipy.stats".into(),
                    provided_by: vec![],
                    evidence: Evidence::new("scripts/run.py").at_line(1),
                },
                // an ignore entry that matches nothing is stale
                Finding::Unmatched {
                    declared: "undeclared_imports.ignore".into(),
                    selector: "orjson".into(),
                },
            ]
        );
    }

    #[test]
    fn only_undeclared_imports_are_findings() {
        let mut g = graph();
        for (module, reason, line) in [
            ("scipy", UnmappedReason::Undeclared, 1),
            ("pytest", UnmappedReason::DeclaredNotRequired, 2),
            ("helpers", UnmappedReason::LocalName, 3),
        ] {
            g.unmapped_imports.push(UnmappedImport {
                from: "scripts".into(),
                module: module.into(),
                reason,
                provided_by: vec![],
                evidence: Evidence::new("scripts/run.py").at_line(line),
            });
        }
        let set = RuleSet {
            undeclared_imports: UndeclaredImportRule {
                forbid: true,
                // covers only an import that is declared, so it is stale
                ignore: vec!["pytest".into()],
            },
            ..RuleSet::default()
        };
        assert_eq!(
            check(&g, &set, 2),
            vec![
                Finding::UndeclaredImport {
                    from: "scripts".into(),
                    module: "scipy".into(),
                    provided_by: vec![],
                    evidence: Evidence::new("scripts/run.py").at_line(1),
                },
                Finding::Unmatched {
                    declared: "undeclared_imports.ignore".into(),
                    selector: "pytest".into(),
                },
            ]
        );
    }

    fn declared(pairs: &[(&str, &str)]) -> BTreeMap<String, Vec<String>> {
        pairs
            .iter()
            .map(|(name, selector)| (name.to_string(), vec![selector.to_string()]))
            .collect()
    }

    #[test]
    fn layers_forbid_depending_on_a_higher_layer() {
        // src.jobs -> src.domain, and src.domain.io -> src.jobs
        let set = RuleSet {
            components: declared(&[("domain", "src/domain"), ("jobs", "src/jobs")]),
            layers: LayerRule {
                order: vec!["jobs".into(), "domain".into(), "nowhere".into()],
            },
            ..RuleSet::default()
        };
        let findings = check(&graph(), &set, 2);
        assert_eq!(findings.len(), 2, "{findings:?}");
        assert!(findings.contains(&Finding::Unmatched {
            declared: "layers.order[2]".into(),
            selector: "nowhere".into()
        }));
        let Some(Finding::LayerViolation {
            from_layer,
            to_layer,
            from,
            ..
        }) = findings
            .iter()
            .find(|f| matches!(f, Finding::LayerViolation { .. }))
        else {
            panic!("{findings:?}");
        };
        assert_eq!(
            (from_layer.as_str(), to_layer.as_str(), from.as_str()),
            ("domain", "jobs", "src.domain.io")
        );
    }

    #[test]
    fn allow_lists_report_unexpected_and_stale_dependencies() {
        let set = RuleSet {
            components: declared(&[
                ("domain", "src/domain"),
                ("jobs", "src/jobs"),
                ("scripts", "scripts"),
            ]),
            allow: vec![
                // domain may depend on nothing declared
                AllowRule {
                    from: "domain".into(),
                    to: vec![],
                },
                AllowRule {
                    from: "jobs".into(),
                    to: vec!["domain".into(), "scripts".into()],
                },
            ],
            ..RuleSet::default()
        };
        let findings = check(&graph(), &set, 2);
        assert!(
            findings.iter().any(|f| matches!(
                f,
                Finding::UnexpectedDependency { declared_from, declared_to, .. }
                    if declared_from == "domain" && declared_to == "jobs"
            )),
            "{findings:?}"
        );
        assert!(findings.contains(&Finding::StaleAllowance {
            from: "jobs".into(),
            to: "scripts".into()
        }));
        // jobs -> domain is allowed and observed: not reported
        assert_eq!(findings.len(), 2, "{findings:?}");
    }

    /// app imports a name from the facade src/shop, whose `__init__.py`
    /// re-exports it from src/shop/billing; cli reaches the same name with
    /// only `via` evidence, as a Rust `use` through a crate root's `pub use`.
    fn facade_graph() -> ArchitectureGraph {
        let mut g = modules(&[
            ("app", "app"),
            ("cli", "cli"),
            ("shop", "src/shop"),
            ("shop.billing", "src/shop/billing"),
        ]);
        let charge = "src/shop/billing/charge.py";
        g.add_edges([
            Edge::new("app", "shop", EdgeKind::Import).with_evidence(
                Evidence::new("app/main.py")
                    .at_line(1)
                    .pointing_at("src/shop/__init__.py"),
            ),
            Edge::new("app", "shop.billing", EdgeKind::Import).with_evidence(
                Evidence::new("app/main.py")
                    .at_line(1)
                    .pointing_at(charge)
                    .with_note("import via src/shop/__init__.py:1"),
            ),
            Edge::new("cli", "shop.billing", EdgeKind::Import).with_evidence(
                Evidence::new("cli/main.rs")
                    .at_line(2)
                    .pointing_at(charge)
                    .with_note("use via src/shop/__init__.py:1"),
            ),
            Edge::new("shop", "shop.billing", EdgeKind::Import).with_evidence(
                Evidence::new("src/shop/__init__.py")
                    .at_line(1)
                    .pointing_at(charge),
            ),
        ]);
        g
    }

    #[test]
    fn rules_count_the_file_a_statement_loads_not_the_one_a_re_export_leads_to() {
        let set = RuleSet {
            components: declared(&[
                ("app", "app"),
                ("cli", "cli"),
                ("facade", "src/shop"),
                ("internals", "src/shop/billing"),
            ]),
            deny: ["app", "cli", "facade"]
                .into_iter()
                .flat_map(|from| {
                    ["facade", "internals"].map(|to| DenyRule {
                        from: from.into(),
                        to: to.into(),
                        reason: None,
                    })
                })
                .filter(|rule| rule.from != rule.to)
                .collect(),
            ..RuleSet::default()
        };
        let findings = check(&facade_graph(), &set, 2);
        let forbidden: Vec<(&str, &str, Vec<String>)> = findings
            .iter()
            .filter_map(|f| match f {
                Finding::Forbidden {
                    from, to, evidence, ..
                } => Some((
                    from.as_str(),
                    to.as_str(),
                    evidence
                        .iter()
                        .map(|e| format!("{}:{}", e.file, e.line.unwrap_or(0)))
                        .collect(),
                )),
                _ => None,
            })
            .collect();
        assert_eq!(
            forbidden,
            vec![
                // the statement loads src/shop/__init__.py, shown once
                ("app", "shop", vec!["app/main.py:1".to_owned()]),
                // only `via` evidence: the crate root it goes through counts
                ("cli", "shop", vec!["cli/main.rs:2".to_owned()]),
                (
                    "shop",
                    "shop.billing",
                    vec!["src/shop/__init__.py:1".to_owned()]
                ),
            ],
            "{findings:?}"
        );

        // layers and allow count the same dependencies
        let set = RuleSet {
            components: declared(&[
                ("app", "app"),
                ("facade", "src/shop"),
                ("internals", "src/shop/billing"),
            ]),
            layers: LayerRule {
                order: vec!["internals".into(), "app".into(), "facade".into()],
            },
            allow: vec![AllowRule {
                from: "app".into(),
                to: vec!["facade".into()],
            }],
            ..RuleSet::default()
        };
        let findings = check(&facade_graph(), &set, 2);
        assert!(
            matches!(
                findings.as_slice(),
                [Finding::LayerViolation { from_layer, to_layer, .. }]
                    if from_layer == "facade" && to_layer == "internals"
            ),
            "{findings:?}"
        );
    }

    #[test]
    fn a_barrel_that_passes_on_what_it_imported_on_another_line_is_the_facade() {
        // src/shop/index.ts: `import { pay } from './billing/pay';` on line
        // 1, `export { pay };` on line 3, which the walk names
        let mut g = modules(&[
            ("app", "app"),
            ("shop", "src/shop"),
            ("billing", "src/shop/billing"),
        ]);
        let pay = "src/shop/billing/pay.ts";
        g.add_edges([
            Edge::new("shop", "billing", EdgeKind::Import).with_evidence(
                Evidence::new("src/shop/index.ts")
                    .at_line(1)
                    .pointing_at(pay)
                    .taking(["pay"]),
            ),
            Edge::new("app", "billing", EdgeKind::Import).with_evidence(
                Evidence::new("app/main.ts")
                    .at_line(1)
                    .pointing_at(pay)
                    .with_note("import via src/shop/index.ts:3")
                    .taking(["pay"]),
            ),
        ]);
        let set = RuleSet {
            components: declared(&[
                ("app", "app"),
                ("facade", "src/shop"),
                ("internals", "src/shop/billing"),
            ]),
            deny: vec![DenyRule {
                from: "app".into(),
                to: "internals".into(),
                reason: None,
            }],
            ..RuleSet::default()
        };
        assert_eq!(check(&g, &set, 2), Vec::new());
    }

    #[test]
    fn a_re_export_that_is_no_import_leaves_the_dependency_on_the_definition() {
        // src/lib.rs: `pub mod domain; pub mod infra; pub use infra::Repo;`,
        // and src/domain.rs: `use crate::Repo;`. A `pub use` of the
        // module's own subtree is no edge of the crate root.
        let mut g = modules(&[
            ("ledger", "."),
            ("ledger::domain", "src/domain.rs"),
            ("ledger::infra", "src/infra.rs"),
        ]);
        g.add_edges([
            Edge::new("ledger::domain", "ledger::infra", EdgeKind::Import).with_evidence(
                Evidence::new("src/domain.rs")
                    .at_line(1)
                    .pointing_at("src/infra.rs")
                    .with_note("use via src/lib.rs:4"),
            ),
        ]);
        let set = RuleSet {
            components: declared(&[("domain", "src/domain.rs"), ("infra", "src/infra.rs")]),
            deny: vec![DenyRule {
                from: "domain".into(),
                to: "infra".into(),
                reason: None,
            }],
            ..RuleSet::default()
        };
        let findings = check(&g, &set, 2);
        assert!(
            matches!(
                findings.as_slice(),
                [Finding::Forbidden { from, to, .. }]
                    if from.as_str() == "ledger::domain" && to.as_str() == "ledger::infra"
            ),
            "{findings:?}"
        );
    }

    #[test]
    fn coverage_requires_leaf_components_to_be_declared() {
        let set = RuleSet {
            components: declared(&[("domain", "src/domain")]),
            coverage: CoverageRule {
                require: vec!["src".into(), "lib".into()],
            },
            ..RuleSet::default()
        };
        let findings = check(&graph(), &set, 2);
        assert!(findings.contains(&Finding::Uncovered {
            component: "src.jobs".into(),
            path: Some("src/jobs".into())
        }));
        assert!(findings.contains(&Finding::Unmatched {
            declared: "coverage.require[1]".into(),
            selector: "lib".into()
        }));
        // src.domain and src.domain.io are declared; scripts is not required
        assert_eq!(findings.len(), 2, "{findings:?}");
    }

    fn modules(ids: &[(&str, &str)]) -> ArchitectureGraph {
        let mut g = ArchitectureGraph::default();
        for (id, path) in ids {
            g.add_component(module(id, path));
        }
        g
    }

    fn file_dep(from: &str, to: &str, file: &str, target: &str, scope: Scope) -> crate::Edge {
        crate::Edge::new(from, to, EdgeKind::Import).with_evidence(
            Evidence::new(file)
                .at_line(1)
                .pointing_at(target)
                .in_scope(scope),
        )
    }

    fn file_level_of(g: &ArchitectureGraph) -> FileLevel {
        let set = RuleSet {
            cycles: CycleRule {
                forbid: true,
                scope: vec![],
            },
            ..RuleSet::default()
        };
        match check(g, &set, 9).into_iter().next() {
            Some(Finding::Cycle { file_level, .. }) => file_level,
            other => panic!("expected a cycle, got {other:?}"),
        }
    }

    #[test]
    fn component_cycles_are_refined_with_file_evidence() {
        // core -> util/log.py and util/store.py -> core: different files
        let mut g = modules(&[("core", "core"), ("util", "util")]);
        g.add_edges([
            file_dep("core", "util", "core/a.py", "util/log.py", Scope::Module),
            file_dep("util", "core", "util/store.py", "core/a.py", Scope::Module),
        ]);
        assert_eq!(file_level_of(&g), FileLevel::NoCycle);

        // util/log.py imports core/a.py back, but only inside a function
        g.add_edges([file_dep(
            "util",
            "core",
            "util/log.py",
            "core/a.py",
            Scope::Local,
        )]);
        assert_eq!(
            file_level_of(&g),
            FileLevel::Cycle {
                files: vec!["core/a.py".into(), "util/log.py".into()],
                at_module_scope: false
            }
        );

        // without file targets nothing can be said
        let mut untargeted = modules(&[("a", "a"), ("b", "b")]);
        untargeted.add_edges([
            crate::Edge::new("a", "b", EdgeKind::Import).with_evidence(Evidence::new("a/lib.rs")),
            crate::Edge::new("b", "a", EdgeKind::Import).with_evidence(Evidence::new("b/lib.rs")),
        ]);
        assert_eq!(file_level_of(&untargeted), FileLevel::Unknown);
    }

    #[test]
    fn cycle_scope_limits_which_cycles_are_reported() {
        let mut g = modules(&[
            ("domain", "src/domain"),
            ("util", "src/util"),
            ("fa", "fixtures/a"),
            ("fb", "fixtures/b"),
        ]);
        g.add_edges([
            file_dep(
                "domain",
                "util",
                "src/domain/a.py",
                "src/util/b.py",
                Scope::Module,
            ),
            file_dep(
                "util",
                "domain",
                "src/util/b.py",
                "src/domain/a.py",
                Scope::Module,
            ),
            file_dep(
                "fa",
                "fb",
                "fixtures/a/x.py",
                "fixtures/b/y.py",
                Scope::Module,
            ),
            file_dep(
                "fb",
                "fa",
                "fixtures/b/y.py",
                "fixtures/a/x.py",
                Scope::Module,
            ),
        ]);
        let set = RuleSet {
            cycles: CycleRule {
                forbid: true,
                scope: vec!["src".into(), "vendor".into()],
            },
            ..RuleSet::default()
        };
        let findings = check(&g, &set, 9);
        let cycles: Vec<Vec<&str>> = findings
            .iter()
            .filter_map(|f| match f {
                Finding::Cycle { components, .. } => {
                    Some(components.iter().map(|c| c.as_str()).collect())
                }
                _ => None,
            })
            .collect();
        assert_eq!(cycles, vec![vec!["domain", "util"]]);
        assert!(findings.contains(&Finding::Unmatched {
            declared: "cycles.scope[1]".into(),
            selector: "vendor".into()
        }));
    }

    #[test]
    fn rules_leave_out_imports_in_test_code() {
        let mut g = ArchitectureGraph::default();
        for id in ["a", "b", "c"] {
            g.add_component(module(id, id));
        }
        let import = |from: &str, to: &str, file: &str, test: bool| {
            Edge::new(from, to, EdgeKind::Import)
                .with_evidence(Evidence::new(file).at_line(1).in_test(test))
        };
        g.add_edges([
            // only a test of a imports b
            import("a", "b", "a/a.test.ts", true),
            // production and a test of c import b
            import("c", "b", "c/c.ts", false),
            import("c", "b", "c/c.test.ts", true),
        ]);
        let declared = BTreeMap::from([
            ("a".to_owned(), vec!["a".to_owned()]),
            ("b".to_owned(), vec!["b".to_owned()]),
            ("c".to_owned(), vec!["c".to_owned()]),
        ]);
        let deny = |from: &str| DenyRule {
            from: from.into(),
            to: "b".into(),
            reason: None,
        };
        let set = RuleSet {
            components: declared.clone(),
            deny: vec![deny("a"), deny("c")],
            ..RuleSet::default()
        };
        let findings = check(&g, &set, 9);
        let [Finding::Forbidden { from, evidence, .. }] = findings.as_slice() else {
            panic!("expected only c's finding: {findings:?}");
        };
        assert_eq!(from.as_str(), "c");
        // the test's import is no location of the finding
        let files: Vec<&str> = evidence.iter().map(|e| e.file.as_str()).collect();
        assert_eq!(files, ["c/c.ts"]);
        // the layers say the same
        let set = RuleSet {
            components: declared.clone(),
            layers: LayerRule {
                order: vec!["b".into(), "a".into(), "c".into()],
            },
            ..RuleSet::default()
        };
        let findings = check(&g, &set, 9);
        assert!(
            matches!(findings.as_slice(), [Finding::LayerViolation { from, .. }] if from.as_str() == "c"),
            "{findings:?}"
        );
        // an allowance that only tests use is stale
        let set = RuleSet {
            components: declared,
            allow: vec![AllowRule {
                from: "a".into(),
                to: vec!["b".into()],
            }],
            ..RuleSet::default()
        };
        let findings = check(&g, &set, 9);
        assert!(
            findings
                .iter()
                .any(|f| matches!(f, Finding::StaleAllowance { from, .. } if from == "a")),
            "{findings:?}"
        );
    }

    #[test]
    fn cycles_leave_out_imports_of_types_only() {
        let mut g = ArchitectureGraph::default();
        for id in ["a", "b"] {
            g.add_component(module(id, id));
        }
        let import = |from: &str, to: &str, file: &str, target: &str, types: bool| {
            Edge::new(from, to, EdgeKind::Import).with_evidence(
                Evidence::new(file)
                    .at_line(1)
                    .pointing_at(target)
                    .type_only(types),
            )
        };
        g.add_edges([
            import("a", "b", "a/x.ts", "b/y.ts", false),
            import("b", "a", "b/y.ts", "a/x.ts", true),
        ]);
        let set = RuleSet {
            cycles: CycleRule {
                forbid: true,
                scope: vec![],
            },
            ..RuleSet::default()
        };
        assert!(check(&g, &set, 9).is_empty(), "only a type closes it");
        g.add_edges([import("b", "a", "b/z.ts", "a/x.ts", false)]);
        let findings = check(&g, &set, 9);
        let Some(Finding::Cycle {
            edges, file_level, ..
        }) = findings.first()
        else {
            panic!("expected a cycle: {findings:?}");
        };
        // the type-only import is left out of what the finding lists
        let listed: Vec<&str> = edges
            .iter()
            .flat_map(|e| &e.evidence)
            .map(|e| e.file.as_str())
            .collect();
        assert_eq!(listed, ["a/x.ts", "b/z.ts"]);
        // and of the file-level reading: these files form no cycle
        assert_eq!(*file_level, FileLevel::NoCycle);
    }

    #[test]
    fn cycles_are_reported_at_the_requested_depth() {
        let mut g = graph();
        for id in ["src.domain", "src.jobs"] {
            let mut c = g.component(&id.into()).unwrap().clone();
            c.parent = Some("app".into());
            g.components.insert(c.id.clone(), c);
        }
        let mut io = g.component(&"src.domain.io".into()).unwrap().clone();
        io.parent = Some("src.domain".into());
        g.components.insert(io.id.clone(), io);

        let set = RuleSet {
            cycles: CycleRule {
                forbid: true,
                scope: vec![],
            },
            ..RuleSet::default()
        };
        // at depth 1, src.domain.io folds into src.domain: domain <-> jobs
        let findings = check(&g, &set, 1);
        let Some(Finding::Cycle {
            components, edges, ..
        }) = findings.first()
        else {
            panic!("expected a cycle: {findings:?}");
        };
        let members: Vec<&str> = components.iter().map(|c| c.as_str()).collect();
        assert_eq!(members, vec!["src.domain", "src.jobs"]);
        assert_eq!(edges.len(), 2);
        // at full depth there is no cycle: io -> jobs -> domain
        assert!(check(&g, &set, 9).is_empty());
        // and without the rule nothing is reported
        assert!(check(&g, &RuleSet::default(), 1).is_empty());
    }
}
