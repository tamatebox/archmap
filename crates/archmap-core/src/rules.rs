//! Declared architecture and the rules checked against the observed graph.
//!
//! The declared side is written by people, in `archmap.toml`: named groups
//! of components, forbidden dependencies, and whether cycles are allowed. It
//! never changes the observed graph. [`check`] only compares the two and
//! reports [`Finding`]s, each with the evidence behind it.
//!
//! A selector is either a path prefix relative to the repository root
//! (`src/core` covers `src/core` and everything below it; `.` covers
//! everything) or an external component id (`ext:requests`, with a trailing
//! `*` for a prefix such as `ext:google-*`).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{ArchitectureGraph, Component, ComponentId, ComponentKind, EdgeKind, Evidence};

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

    if rules.cycles.forbid {
        let rolled = graph.rollup(depth);
        for components in rolled.cycles() {
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
            findings.push(Finding::Cycle { components, edges });
        }
    }

    findings
}

/// Does `selector` cover `component`?
pub fn selector_matches(selector: &str, component: &Component) -> bool {
    let selector = selector.trim();
    if selector.starts_with("ext:") {
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

    /// src.core, src.core.io, src.pipeline, scripts, ext:requests
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
            "ext:requests",
            "requests",
            ComponentKind::External,
        ));
        graph.add_edge(
            Edge::new("src.core.io", "src.pipeline", EdgeKind::Import)
                .with_evidence(Evidence::new("src/core/io/read.py").at_line(3)),
        );
        graph.add_edge(Edge::new("src.pipeline", "src.core", EdgeKind::Import));
        graph.add_edge(Edge::new("scripts", "ext:requests", EdgeKind::Import));
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
        assert!(!selector_matches(".", c("ext:requests")));
        assert!(selector_matches("ext:requests", c("ext:requests")));
        assert!(selector_matches("ext:req*", c("ext:requests")));
        assert!(!selector_matches("requests", c("ext:requests")));
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
                    to: "ext:requests".into(),
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
        assert!(findings
            .iter()
            .any(|f| matches!(f, Finding::Forbidden { to, .. } if to.as_str() == "ext:requests")));
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
            cycles: CycleRule { forbid: true },
            ..RuleSet::default()
        };
        // at depth 1, src.core.io folds into src.core: core <-> pipeline
        let findings = check(&g, &set, 1);
        let Some(Finding::Cycle { components, edges }) = findings.first() else {
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
