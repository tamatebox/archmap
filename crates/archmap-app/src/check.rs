//! `check`: the observed graph against the rules in `archmap.toml`, and
//! structural signals.

use std::path::Path;

use anyhow::{Context, Result};
use archmap_core::rules::{external_selector_lacks_ecosystem, FileLevel, Finding, RuleSet};
use archmap_core::signals::Signal;
use archmap_core::{ArchitectureGraph, ComponentId, EdgeKind, Evidence, Scope};
use serde::Serialize;

use crate::{CheckRequest, Format, Workspace};

/// Rules file read by `archmap check`, relative to the scanned root.
pub const RULES_FILE: &str = "archmap.toml";

#[derive(Debug, Serialize)]
struct CheckReport<'a> {
    /// The rules file, or `None` when there is none and only signals are
    /// reported.
    rules: Option<String>,
    depth: usize,
    findings: &'a [Finding],
    signals: &'a [Signal],
}

/// The rules `check` compares the graph with: from a rules file, or none
/// (signals only). The rule set stays inside this crate, so an interface
/// never needs `archmap-core`.
#[derive(Debug, Default)]
pub struct Rules {
    set: RuleSet,
    label: Option<String>,
}

impl Rules {
    /// The rules file as the report names it, `None` when there is none.
    pub fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    /// The same rules, named `label` in the report: an interface whose root
    /// is absolute names the file relative to it.
    pub fn with_label(self, label: impl Into<String>) -> Rules {
        Rules {
            label: Some(label.into()),
            ..self
        }
    }
}

/// The rules for `root`: from `config` when given, else from
/// `<root>/archmap.toml` when it exists, else none.
pub fn load_rules(root: &Path, config: Option<&Path>) -> Result<Rules> {
    let path = match config {
        Some(file) => Some(file.to_path_buf()),
        None => Some(root.join(RULES_FILE)).filter(|p| p.exists()),
    };
    let set = path.as_deref().map(read_rules).transpose()?;
    Ok(Rules {
        set: set.unwrap_or_default(),
        label: path.map(|p| p.display().to_string()),
    })
}

fn read_rules(path: &Path) -> Result<RuleSet> {
    let text = std::fs::read_to_string(path).with_context(|| {
        format!(
            "reading {} (write one, or name another rules file)",
            path.display()
        )
    })?;
    toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

/// What `check` reports: the text or JSON, and how many findings it holds.
/// Signals never count: they are observations.
#[derive(Debug)]
pub struct CheckAnswer {
    pub output: String,
    pub findings: usize,
}

impl Workspace {
    /// `check` against `rules`.
    pub fn check(&self, rules: &Rules, request: &CheckRequest) -> Result<CheckAnswer> {
        check(self, rules, request)
    }
}

fn check(ws: &Workspace, rules: &Rules, request: &CheckRequest) -> Result<CheckAnswer> {
    let graph = ws.graph();
    let depth = request
        .depth
        .or(rules.set.depth)
        .unwrap_or(crate::DEFAULT_DEPTH);
    let findings = archmap_core::rules::check(graph, &rules.set, depth);
    let signals = archmap_core::signals::signals(graph, depth);

    let shown = rules.label.clone();
    let output = match request.format {
        Format::Json => crate::json(&CheckReport {
            rules: shown,
            depth,
            findings: &findings,
            signals: &signals,
        })?,
        Format::Text => check_text(graph, &findings, &signals, shown.as_deref(), depth),
    };
    Ok(CheckAnswer {
        output,
        findings: findings.len(),
    })
}

/// Evidence lines per finding in text output; JSON has every one.
const MAX_CHECK_LOCATIONS: usize = 3;
/// Edges listed per cycle in text output.
const MAX_CYCLE_EDGES: usize = 10;

fn check_text(
    graph: &ArchitectureGraph,
    findings: &[Finding],
    signals: &[Signal],
    rules: Option<&str>,
    depth: usize,
) -> String {
    let name = |id: &ComponentId| {
        graph
            .component(id)
            .map_or(id.as_str().to_owned(), |c| c.name.clone())
    };
    let mut out = String::new();
    let count = |n: usize, noun: &str| match n {
        0 => format!("no {noun}s"),
        1 => format!("1 {noun}"),
        n => format!("{n} {noun}s"),
    };
    let rules = rules.unwrap_or("none, signals only");
    out.push_str(&format!(
        "archmap check: {}, {} (rules: {rules}, roll-up depth {depth})\n",
        count(findings.len(), "finding"),
        count(signals.len(), "signal")
    ));

    let mut truncated = false;
    for finding in findings {
        out.push('\n');
        match finding {
            Finding::Forbidden {
                rule,
                deny,
                from,
                to,
                edge,
                evidence,
            } => {
                out.push_str(&format!(
                    "forbidden by deny[{rule}] {} -> {}: {} -> {} ({})\n",
                    deny.from,
                    deny.to,
                    name(from),
                    name(to),
                    edge_label(*edge, evidence)
                ));
                if let Some(reason) = &deny.reason {
                    out.push_str(&format!("  reason: {reason}\n"));
                }
                truncated |= evidence_lines(&mut out, evidence);
            }
            Finding::LayerViolation {
                from_layer,
                to_layer,
                from,
                to,
                edge,
                evidence,
            } => {
                out.push_str(&format!(
                    "layer violation: {from_layer} must not depend on the higher layer {to_layer}: {} -> {} ({})\n",
                    name(from),
                    name(to),
                    edge_label(*edge, evidence)
                ));
                truncated |= evidence_lines(&mut out, evidence);
            }
            Finding::UnexpectedDependency {
                declared_from,
                declared_to,
                from,
                to,
                edge,
                evidence,
            } => {
                out.push_str(&format!(
                    "unexpected dependency: {declared_from} -> {declared_to} is not in the allow list: {} -> {} ({})\n",
                    name(from),
                    name(to),
                    edge_label(*edge, evidence)
                ));
                truncated |= evidence_lines(&mut out, evidence);
            }
            Finding::StaleAllowance { from, to } => {
                out.push_str(&format!(
                    "stale allowance: {from} -> {to} is allowed but not observed\n"
                ));
            }
            Finding::Uncovered { component, path } => {
                let at = path
                    .as_deref()
                    .map(|p| format!(" at {p}"))
                    .unwrap_or_default();
                out.push_str(&format!(
                    "uncovered: {}{at} belongs to no declared component\n",
                    name(component)
                ));
            }
            Finding::Cycle {
                components,
                edges,
                file_level,
            } => {
                let names: Vec<String> = components.iter().map(&name).collect();
                out.push_str(&format!("cycle: {}\n", names.join(", ")));
                out.push_str(&match file_level {
                    FileLevel::Unknown => {
                        "  file level: unknown, no import targets recorded\n".to_owned()
                    }
                    FileLevel::NoCycle => {
                        "  file level: no cycle; different files form each direction\n".to_owned()
                    }
                    FileLevel::Cycle {
                        files,
                        at_module_scope,
                    } => format!(
                        "  file level: cycle through {}; {}\n",
                        files.join(", "),
                        if *at_module_scope {
                            "closes at module scope"
                        } else {
                            "closes only through local-scope imports"
                        }
                    ),
                });
                for e in edges.iter().take(MAX_CYCLE_EDGES) {
                    let at = e.evidence.first().map(location).unwrap_or_default();
                    out.push_str(&format!("  {} -> {}  {at}\n", name(&e.from), name(&e.to)));
                }
                if edges.len() > MAX_CYCLE_EDGES {
                    out.push_str(&format!(
                        "  +{} more edges\n",
                        edges.len() - MAX_CYCLE_EDGES
                    ));
                    truncated = true;
                }
            }
            Finding::UndeclaredImport {
                from,
                module,
                provided_by,
                evidence,
            } => {
                let provider = if provided_by.is_empty() {
                    String::new()
                } else {
                    format!(", provided by {}", provided_by.join(", "))
                };
                out.push_str(&format!(
                    "undeclared import: {module} in {}{provider}\n  {}\n",
                    name(from),
                    location(evidence)
                ));
            }
            Finding::Unmatched { declared, selector } => {
                let hint = if external_selector_lacks_ecosystem(selector) {
                    "; external ids name their ecosystem (`ext:cargo:serde`, `ext:pypi:requests`)"
                } else {
                    ""
                };
                out.push_str(&format!(
                    "unmatched: {declared} `{selector}` matches no component{hint}\n"
                ));
            }
        }
    }
    for signal in signals {
        out.push('\n');
        match signal {
            Signal::MixedDirections {
                component,
                partners,
                used_by_partners,
                depending_on_partners,
            } => {
                let partner_names: Vec<String> = partners.iter().map(&name).collect();
                out.push_str(&format!(
                    "signal: {} mixes dependency directions with {}\n",
                    name(component),
                    partner_names.join(", ")
                ));
                let used: Vec<String> = used_by_partners
                    .iter()
                    .map(|u| format!("{} ({})", u.file, u.partners))
                    .collect();
                let using: Vec<String> = depending_on_partners
                    .iter()
                    .map(|d| {
                        let on: Vec<String> = d.partners.iter().map(&name).collect();
                        format!("{} -> {}", d.file, on.join(", "))
                    })
                    .collect();
                truncated |= capped_list(&mut out, "used by them", &used);
                truncated |= capped_list(&mut out, "using them", &using);
            }
        }
    }
    crate::query_text::marks(&mut out, "");
    if !signals.is_empty() {
        out.push_str("\nSignals are observations; they never change the exit code.\n");
    }
    if truncated {
        out.push_str("\nSome entries are left out; JSON lists every one.\n");
    }
    out
}

/// `  label: a, b, c, +N more`; returns whether entries were left out.
fn capped_list(out: &mut String, label: &str, entries: &[String]) -> bool {
    let shown: Vec<&str> = entries
        .iter()
        .take(MAX_CHECK_LOCATIONS + 2)
        .map(String::as_str)
        .collect();
    let more = entries.len().saturating_sub(shown.len());
    out.push_str(&format!("  {label}: {}", shown.join("; ")));
    if more > 0 {
        out.push_str(&format!("; +{more} more"));
    }
    out.push('\n');
    more > 0
}

/// The kind of a dependency a rule finding names, and whether all of its
/// imports take types only, which never run.
fn edge_label(edge: EdgeKind, evidence: &[Evidence]) -> String {
    if !evidence.is_empty() && evidence.iter().all(|e| e.type_only) {
        format!("{}, types only", edge.as_str())
    } else {
        edge.as_str().to_owned()
    }
}

fn location(e: &Evidence) -> String {
    let mut s = e.file.clone();
    if let Some(line) = e.line {
        s.push_str(&format!(":{line}"));
    }
    if let Some(target) = &e.target {
        s.push_str(&format!(" -> {target}"));
    }
    if let Some(note) = &e.note {
        s.push_str(&format!("  {note}"));
    }
    // as `query` marks them: types only, test code, inside a function body
    if e.type_only {
        s.push_str(" (type)");
    }
    if e.test {
        s.push_str(" (test)");
    }
    if e.scope == Some(Scope::Local) {
        s.push_str(" (local)");
    }
    s
}

/// Up to `MAX_CHECK_LOCATIONS` evidence lines; returns whether some were cut.
fn evidence_lines(out: &mut String, evidence: &[Evidence]) -> bool {
    for e in evidence.iter().take(MAX_CHECK_LOCATIONS) {
        out.push_str(&format!("  {}\n", location(e)));
    }
    let more = evidence.len().saturating_sub(MAX_CHECK_LOCATIONS);
    if more > 0 {
        out.push_str(&format!("  +{more} more\n"));
    }
    more > 0
}
