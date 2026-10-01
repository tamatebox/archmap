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
//! (`src/core` covers `src/core` and everything below it; `.` covers
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
/// of each component, so different files can close the loop. None of these
/// states says that a program fails at runtime.
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
        for edge in &graph.edges {
            let (Some(source), Some(target)) =
                (graph.component(&edge.from), graph.component(&edge.to))
            else {
                continue;
            };
            if from.contains(source, &membership) && to.contains(target, &membership) {
                findings.push(Finding::Forbidden {
                    rule: index,
                    deny: rule.clone(),
                    from: edge.from.clone(),
                    to: edge.to.clone(),
                    edge: edge.kind,
                    evidence: edge.evidence.clone(),
                });
            }
        }
    }

    findings.extend(check_layers(graph, rules, &membership));
    findings.extend(check_allow(graph, rules, &membership));
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
            let edges = rolled
                .edges
                .iter()
                .filter(|e| members.contains(&e.from) && members.contains(&e.to))
                .map(|e| CycleEdge {
                    from: e.from.clone(),
                    to: e.to.clone(),
                    kind: e.kind,
                    evidence: e.evidence.clone(),
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

/// Edges whose both ends belong to (different) declared components, with
/// the names of those components.
fn declared_edges<'g>(
    graph: &'g ArchitectureGraph,
    membership: &'g BTreeMap<ComponentId, &'g str>,
) -> impl Iterator<Item = (&'g str, &'g str, &'g crate::Edge)> + 'g {
    graph.edges.iter().filter_map(move |edge| {
        let (from, to) = (*membership.get(&edge.from)?, *membership.get(&edge.to)?);
        (from != to).then_some((from, to, edge))
    })
}

fn check_layers(
    graph: &ArchitectureGraph,
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
    for (from, to, edge) in declared_edges(graph, membership) {
        if let (Some(&upper), Some(&lower)) = (rank.get(from), rank.get(to)) {
            if lower < upper {
                findings.push(Finding::LayerViolation {
                    from_layer: from.to_owned(),
                    to_layer: to.to_owned(),
                    from: edge.from.clone(),
                    to: edge.to.clone(),
                    edge: edge.kind,
                    evidence: edge.evidence.clone(),
                });
            }
        }
    }
    findings
}

fn check_allow(
    graph: &ArchitectureGraph,
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
    for (from, to, edge) in declared_edges(graph, membership) {
        observed.insert((from, to));
        if allowed
            .get(from)
            .is_some_and(|targets| !targets.contains(to))
        {
            findings.push(Finding::UnexpectedDependency {
                declared_from: from.to_owned(),
                declared_to: to.to_owned(),
                from: edge.from.clone(),
                to: edge.to.clone(),
                edge: edge.kind,
                evidence: edge.evidence.clone(),
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
        for e in &edge.evidence {
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

    /// src.core, src.core.io, src.pipeline, scripts, ext:pypi:requests
    fn graph() -> ArchitectureGraph {
        let mut graph = ArchitectureGraph::default();
        let mut root = Component::new("app", "app", ComponentKind::Package);
        root.path = Some(".".into());
        graph.add_component(root);
        for (id, path) in [
            ("src.core", "src/core"),
            ("src.core.io", "src/core/io"),
            ("src.pipeline", "src/pipeline"),
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
            Edge::new("src.core.io", "src.pipeline", EdgeKind::Import)
                .with_evidence(Evidence::new("src/core/io/read.py").at_line(3)),
        );
        graph.add_edge(Edge::new("src.pipeline", "src.core", EdgeKind::Import));
        graph.add_edge(Edge::new("scripts", "ext:pypi:requests", EdgeKind::Import));
        graph
    }

    #[test]
    fn selectors_match_path_prefixes_and_external_ids() {
        let g = graph();
        let c = |id: &str| g.component(&id.into()).unwrap();
        assert!(selector_matches("src/core", c("src.core.io")));
        assert!(selector_matches("./src/core/", c("src.core")));
        assert!(!selector_matches("src/co", c("src.core")));
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
                ("domain".to_owned(), vec!["src/core".to_owned()]),
                ("pipeline".to_owned(), vec!["src/pipeline".to_owned()]),
            ]),
            deny: vec![DenyRule {
                from: "domain".into(),
                to: "pipeline".into(),
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
            ("src.core.io", "src.pipeline", 0)
        );
        assert_eq!(evidence[0].file, "src/core/io/read.py");
    }

    #[test]
    fn the_most_specific_declaration_wins() {
        // `src` would also cover src/pipeline; the longer selector decides
        let set = RuleSet {
            components: BTreeMap::from([
                ("everything".to_owned(), vec!["src".to_owned()]),
                ("pipeline".to_owned(), vec!["src/pipeline".to_owned()]),
            ]),
            deny: vec![DenyRule {
                from: "pipeline".into(),
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
        assert_eq!(forbidden, vec![("src.pipeline", "src.core")]);
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
                    to: "src/pipeline".into(),
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
        // src.pipeline -> src.core, and src.core.io -> src.pipeline
        let set = RuleSet {
            components: declared(&[("domain", "src/core"), ("pipeline", "src/pipeline")]),
            layers: LayerRule {
                order: vec!["pipeline".into(), "domain".into(), "nowhere".into()],
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
            ("domain", "pipeline", "src.core.io")
        );
    }

    #[test]
    fn allow_lists_report_unexpected_and_stale_dependencies() {
        let set = RuleSet {
            components: declared(&[
                ("domain", "src/core"),
                ("pipeline", "src/pipeline"),
                ("scripts", "scripts"),
            ]),
            allow: vec![
                // domain may depend on nothing declared
                AllowRule {
                    from: "domain".into(),
                    to: vec![],
                },
                AllowRule {
                    from: "pipeline".into(),
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
                    if declared_from == "domain" && declared_to == "pipeline"
            )),
            "{findings:?}"
        );
        assert!(findings.contains(&Finding::StaleAllowance {
            from: "pipeline".into(),
            to: "scripts".into()
        }));
        // pipeline -> domain is allowed and observed: not reported
        assert_eq!(findings.len(), 2, "{findings:?}");
    }

    #[test]
    fn coverage_requires_leaf_components_to_be_declared() {
        let set = RuleSet {
            components: declared(&[("domain", "src/core")]),
            coverage: CoverageRule {
                require: vec!["src".into(), "lib".into()],
            },
            ..RuleSet::default()
        };
        let findings = check(&graph(), &set, 2);
        assert!(findings.contains(&Finding::Uncovered {
            component: "src.pipeline".into(),
            path: Some("src/pipeline".into())
        }));
        assert!(findings.contains(&Finding::Unmatched {
            declared: "coverage.require[1]".into(),
            selector: "lib".into()
        }));
        // src.core and src.core.io are declared; scripts is not required
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
            ("core", "src/core"),
            ("util", "src/util"),
            ("fa", "fixtures/a"),
            ("fb", "fixtures/b"),
        ]);
        g.add_edges([
            file_dep(
                "core",
                "util",
                "src/core/a.py",
                "src/util/b.py",
                Scope::Module,
            ),
            file_dep(
                "util",
                "core",
                "src/util/b.py",
                "src/core/a.py",
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
        assert_eq!(cycles, vec![vec!["core", "util"]]);
        assert!(findings.contains(&Finding::Unmatched {
            declared: "cycles.scope[1]".into(),
            selector: "vendor".into()
        }));
    }

    #[test]
    fn cycles_are_reported_at_the_requested_depth() {
        let mut g = graph();
        for id in ["src.core", "src.pipeline"] {
            let mut c = g.component(&id.into()).unwrap().clone();
            c.parent = Some("app".into());
            g.components.insert(c.id.clone(), c);
        }
        let mut io = g.component(&"src.core.io".into()).unwrap().clone();
        io.parent = Some("src.core".into());
        g.components.insert(io.id.clone(), io);

        let set = RuleSet {
            cycles: CycleRule {
                forbid: true,
                scope: vec![],
            },
            ..RuleSet::default()
        };
        // at depth 1, src.core.io folds into src.core: core <-> pipeline
        let findings = check(&g, &set, 1);
        let Some(Finding::Cycle {
            components, edges, ..
        }) = findings.first()
        else {
            panic!("expected a cycle: {findings:?}");
        };
        let members: Vec<&str> = components.iter().map(|c| c.as_str()).collect();
        assert_eq!(members, vec!["src.core", "src.pipeline"]);
        assert_eq!(edges.len(), 2);
        // at full depth there is no cycle: io -> pipeline -> core
        assert!(check(&g, &set, 9).is_empty());
        // and without the rule nothing is reported
        assert!(check(&g, &RuleSet::default(), 1).is_empty());
    }
}
