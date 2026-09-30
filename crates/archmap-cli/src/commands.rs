use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{bail, Context, Result};
use archmap_core::rules::{FileLevel, Finding, RuleSet};
use archmap_core::signals::Signal;
use archmap_core::{
    ArchitectureGraph, ChangeSeed, Component, ComponentId, DynamicImport, Edge, EdgeKind, Evidence,
    Symbol, SymbolId, UnmappedImport,
};
use archmap_scan::{ScanOptions, ScanReport};
use serde::Serialize;

use crate::output::{render, OutputFormat, ReportFormat};

fn run_scan(path: &str, manifests_only: bool) -> Result<ScanReport> {
    let options = ScanOptions { manifests_only };
    let report = archmap_scan::scan(Path::new(path), &options)
        .with_context(|| format!("scanning {path}"))?;
    for warning in &report.warnings {
        eprintln!("warning: {warning}");
    }
    Ok(report)
}

/// Directory, relative to the scanned root, that holds generated output.
pub const OUTPUT_DIR: &str = ".archmap";

pub fn scan(
    path: &str,
    format: OutputFormat,
    output: Option<&Path>,
    manifests_only: bool,
) -> Result<ExitCode> {
    let report = run_scan(path, manifests_only)?;
    let rendered = render(&report.graph, format)? + "\n";
    let default_name = format!("graph.{}", format.extension());
    if let Some(file) = write_output(path, &default_name, output, &rendered)? {
        let graph = &report.graph;
        eprintln!(
            "wrote {} ({} components, {} symbols, {} edges)",
            file.display(),
            graph.components.len(),
            graph.symbols.len(),
            graph.edges.len()
        );
    }
    Ok(ExitCode::SUCCESS)
}

pub fn summary(path: &str, depth: usize, output: Option<&Path>) -> Result<ExitCode> {
    let report = run_scan(path, false)?;
    let markdown = crate::summary::render(&report.graph, depth);
    // A summary is a view: printed by default, saved only when asked.
    let output = output.unwrap_or(Path::new("-"));
    if let Some(file) = write_output(path, "summary.md", Some(output), &markdown)? {
        eprintln!(
            "wrote {} (depth {depth}, {} bytes)",
            file.display(),
            markdown.len()
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// Write `content` to `output`, or to `<path>/.archmap/<default_name>` when
/// no output is given. `-` means stdout. Returns the file written, if any.
fn write_output(
    path: &str,
    default_name: &str,
    output: Option<&Path>,
    content: &str,
) -> Result<Option<PathBuf>> {
    let file = match output {
        Some(p) if p == Path::new("-") => {
            print!("{content}");
            return Ok(None);
        }
        Some(p) => p.to_path_buf(),
        None => Path::new(path).join(OUTPUT_DIR).join(default_name),
    };
    if let Some(parent) = file.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(&file, content).with_context(|| {
        format!(
            "writing {} (use `--output <file>` or `--output -` for stdout)",
            file.display()
        )
    })?;
    Ok(Some(file))
}

/// Depth that `summary`, `query` and `impact` roll up to unless told
/// otherwise, so the three always describe the same components.
pub const DEFAULT_DEPTH: usize = 2;

/// What `archmap query` returns for a component.
#[derive(Debug, Serialize)]
pub struct ComponentView<'a> {
    /// The target as given on the command line.
    pub requested: &'a str,
    pub depth: usize,
    /// The requested component, when it is folded into `component` at this
    /// depth.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folded_from: Option<ComponentId>,
    pub component: &'a Component,
    /// Direct children in the unrolled graph, to query with a larger depth.
    pub children: Vec<&'a ComponentId>,
    pub symbols: Vec<&'a Symbol>,
    pub outgoing: Vec<&'a Edge>,
    pub incoming: Vec<&'a Edge>,
    /// Imports in the component that map to no component: dependencies
    /// that no edge shows.
    pub not_mapped: Vec<&'a UnmappedImport>,
    /// Modules the component loads by names computed at runtime.
    pub dynamic_imports: Vec<&'a DynamicImport>,
}

/// What `archmap query` returns for a file: the file-level facts behind a
/// component, rolled up to the same depth.
#[derive(Debug, Serialize)]
pub struct FileView<'a> {
    /// The target as given on the command line (a path or a dotted module name).
    pub requested: &'a str,
    pub depth: usize,
    pub file: String,
    /// The component that contains the file, at this depth.
    pub component: Option<ComponentId>,
    pub symbols: Vec<&'a Symbol>,
    /// The file's import statements, one edge per imported component.
    pub imports: Vec<Edge>,
    /// Statements elsewhere that import the file, one edge per importing
    /// component. `None` when no evidence names imported files for the
    /// file's language, so importers are unknown rather than absent.
    pub importers: Option<Vec<Edge>>,
    pub not_mapped: Vec<&'a UnmappedImport>,
    pub dynamic_imports: Vec<&'a DynamicImport>,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum QueryResult<'a> {
    Component(ComponentView<'a>),
    File(FileView<'a>),
    Symbols(Vec<&'a Symbol>),
}

/// `target` as a file under the scanned root, relative with `/` separators.
fn file_target(path: &str, target: &str) -> Option<String> {
    let relative = target.trim_start_matches("./").trim_end_matches('/');
    Path::new(path)
        .join(relative)
        .is_file()
        .then(|| relative.replace('\\', "/"))
}

/// Group a file's import evidence by the component at `depth` on the other side.
fn edges_at_depth(
    full: &ArchitectureGraph,
    depth: usize,
    pairs: &[(&Edge, &Evidence)],
    other_side: impl Fn(&Edge) -> &ComponentId,
    edge: impl Fn(ComponentId, Vec<Evidence>) -> Edge,
) -> Vec<Edge> {
    let mut grouped: std::collections::BTreeMap<ComponentId, Vec<Evidence>> = Default::default();
    for (e, evidence) in pairs {
        grouped
            .entry(full.ancestor_at(other_side(e), depth))
            .or_default()
            .push((*evidence).clone());
    }
    grouped
        .into_iter()
        .map(|(id, evidence)| edge(id, evidence))
        .collect()
}

fn file_view<'a>(
    full: &'a ArchitectureGraph,
    depth: usize,
    requested: &'a str,
    file: &str,
) -> FileView<'a> {
    let facts = full.file_facts(file);
    let owner = facts.component.map(|c| full.ancestor_at(c, depth));
    let here = owner.clone().unwrap_or_else(|| ComponentId::new(file));
    let imports = edges_at_depth(
        full,
        depth,
        &facts.imports,
        |e| &e.to,
        |to, evidence| Edge {
            from: here.clone(),
            to,
            kind: EdgeKind::Import,
            evidence,
        },
    );
    let importers = facts.importers_recorded.then(|| {
        edges_at_depth(
            full,
            depth,
            &facts.importers,
            |e| &e.from,
            |from, evidence| Edge {
                from,
                to: here.clone(),
                kind: EdgeKind::Import,
                evidence,
            },
        )
    });
    FileView {
        requested,
        depth,
        file: facts.file,
        component: owner,
        symbols: facts.symbols,
        imports,
        importers,
        not_mapped: facts.unmapped_imports,
        dynamic_imports: facts.dynamic_imports,
    }
}

pub fn query(
    path: &str,
    target: &str,
    depth: usize,
    format: ReportFormat,
    verbose: bool,
) -> Result<ExitCode> {
    let report = run_scan(path, false)?;
    let full = &report.graph;
    let rolled = full.rollup(depth);

    let result = if let Some(at) = resolve_at_depth(full, &rolled, depth, target) {
        let component = rolled
            .component(&at.id)
            .with_context(|| format!("`{}` is missing after roll-up", at.id))?;
        QueryResult::Component(ComponentView {
            requested: target,
            depth,
            folded_from: at.folded_from,
            component,
            children: full
                .components
                .values()
                .filter(|c| c.parent.as_ref() == Some(&component.id))
                .map(|c| &c.id)
                .collect(),
            symbols: rolled.symbols_of(&component.id).collect(),
            outgoing: rolled.outgoing(&component.id).collect(),
            incoming: rolled.incoming(&component.id).collect(),
            not_mapped: rolled
                .unmapped_imports
                .iter()
                .filter(|i| i.from == component.id)
                .collect(),
            dynamic_imports: rolled
                .dynamic_imports
                .iter()
                .filter(|i| i.from == component.id)
                .collect(),
        })
    } else if let Some(file) = file_target(path, target) {
        QueryResult::File(file_view(full, depth, target, &file))
    } else {
        let symbols: Vec<&Symbol> = rolled
            .symbol(&SymbolId::new(target))
            .into_iter()
            .chain(rolled.symbols_named(target))
            .collect();
        if !symbols.is_empty() {
            QueryResult::Symbols(symbols)
        } else if let Some(file) = full.file_for_dotted_name(target) {
            QueryResult::File(file_view(full, depth, target, file))
        } else {
            bail!("no component, file or symbol named `{target}`");
        }
    };

    match format {
        ReportFormat::Json => println!("{}", render(&result, OutputFormat::Json)?),
        ReportFormat::Text => print!(
            "{}",
            crate::query_text::render(&result, target, full, &rolled, verbose)
        ),
    }
    Ok(ExitCode::SUCCESS)
}

/// A component as seen at a roll-up depth.
struct AtDepth {
    id: ComponentId,
    folded_from: Option<ComponentId>,
}

/// Find `target` among the components visible at `depth`. A component that
/// is folded at this depth resolves to the ancestor it was folded into.
fn resolve_at_depth(
    full: &ArchitectureGraph,
    rolled: &ArchitectureGraph,
    depth: usize,
    target: &str,
) -> Option<AtDepth> {
    if let Some(visible) = find_component(rolled, target) {
        return Some(AtDepth {
            id: visible.id.clone(),
            folded_from: None,
        });
    }
    find_component(full, target).map(|c| fold(full, depth, &c.id))
}

fn fold(full: &ArchitectureGraph, depth: usize, id: &ComponentId) -> AtDepth {
    let ancestor = full.ancestor_at(id, depth);
    let folded_from = (ancestor != *id).then(|| id.clone());
    AtDepth {
        id: ancestor,
        folded_from,
    }
}

/// Exact id first, then a unique match on the display name.
fn find_component<'a>(graph: &'a ArchitectureGraph, target: &str) -> Option<&'a Component> {
    graph.component(&ComponentId::new(target)).or_else(|| {
        let mut named = graph.components_named(target);
        match (named.next(), named.next()) {
            (Some(only), None) => Some(only),
            _ => None,
        }
    })
}

#[derive(Debug, Serialize)]
pub struct ImpactResult<'a> {
    /// The target as given on the command line.
    pub requested: &'a str,
    pub depth: usize,
    pub target: ComponentId,
    /// The component that owns the request, when it is folded into `target`
    /// at this depth.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folded_from: Option<ComponentId>,
    /// Components that directly depend on the target.
    pub direct: Vec<ComponentId>,
    /// Every component that transitively depends on the target.
    pub transitive: Vec<ComponentId>,
    /// For a file target: the statements that import the file directly.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub importers: Option<ImportSites>,
}

/// How many import sites `impact` shows for a file; the rest is counted.
const MAX_IMPORT_SITES: usize = 5;

#[derive(Debug, Serialize)]
pub struct ImportSites {
    /// False when no evidence names imported files for the file's language:
    /// the importers are unknown, not absent.
    pub recorded: bool,
    pub total: usize,
    pub shown: Vec<ImportSite>,
}

#[derive(Debug, Serialize)]
pub struct ImportSite {
    pub file: String,
    pub line: Option<u32>,
    /// The importing component, at the roll-up depth.
    pub component: ComponentId,
}

fn import_sites(full: &ArchitectureGraph, depth: usize, file: &str) -> ImportSites {
    let facts = full.file_facts(file);
    let mut sites: Vec<ImportSite> = Vec::new();
    for (edge, e) in &facts.importers {
        if !sites.iter().any(|s| s.file == e.file && s.line == e.line) {
            sites.push(ImportSite {
                file: e.file.clone(),
                line: e.line,
                component: full.ancestor_at(&edge.from, depth),
            });
        }
    }
    sites.sort_by(|a, b| (&a.file, a.line).cmp(&(&b.file, b.line)));
    let total = sites.len();
    sites.truncate(MAX_IMPORT_SITES);
    ImportSites {
        recorded: facts.importers_recorded,
        total,
        shown: sites,
    }
}

pub fn impact(path: &str, target: &str, depth: usize, format: OutputFormat) -> Result<ExitCode> {
    // Import edges come from source, so impact needs a full scan.
    let report = run_scan(path, false)?;
    let full = &report.graph;
    let rolled = full.rollup(depth);

    let component = find_component(&rolled, target)
        .or_else(|| find_component(full, target))
        .map(|c| c.id.clone());
    let relative = target.trim_start_matches("./").trim_end_matches('/');
    let on_disk = Path::new(path).join(relative);
    let mut importers = None;
    let (at, reach) = if let Some(id) = component {
        let reach = full.change_impact(ChangeSeed::Component(&id), depth);
        (fold(full, depth, &id), reach)
    } else if on_disk.is_file() {
        let owner = full
            .component_for_path(relative)
            .with_context(|| format!("no component contains `{target}`"))?;
        let reach = full.change_impact(ChangeSeed::File(relative), depth);
        importers = Some(import_sites(full, depth, relative));
        (fold(full, depth, &owner.id), reach)
    } else if on_disk.is_dir() {
        let owner = full
            .component_for_path(relative)
            .with_context(|| format!("no component contains `{target}`"))?;
        let id = owner.id.clone();
        let reach = full.change_impact(ChangeSeed::Component(&id), depth);
        (fold(full, depth, &id), reach)
    } else {
        bail!("no component or file `{target}` in graph");
    };

    let result = ImpactResult {
        requested: target,
        depth,
        direct: reach.direct.into_iter().collect(),
        transitive: reach.transitive.into_iter().collect(),
        target: at.id,
        folded_from: at.folded_from,
        importers,
    };
    println!("{}", render(&result, format)?);
    Ok(ExitCode::SUCCESS)
}

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

/// Exit codes: 0 without findings, 1 with findings, 2 when the rules or the
/// repository cannot be read. Signals never change the exit code. Without a
/// rules file only signals are reported.
pub fn check(
    path: &str,
    config: Option<&Path>,
    depth: Option<usize>,
    format: ReportFormat,
) -> Result<ExitCode> {
    let rules_path = match config {
        Some(file) => Some(file.to_path_buf()),
        None => Some(Path::new(path).join(RULES_FILE)).filter(|p| p.exists()),
    };
    let rules = match rules_path.as_deref().map(load_rules).transpose() {
        Ok(rules) => rules.unwrap_or_default(),
        Err(err) => {
            eprintln!("error: {err:#}");
            return Ok(ExitCode::from(2));
        }
    };
    let report = match run_scan(path, false) {
        Ok(report) => report,
        Err(err) => {
            eprintln!("error: {err:#}");
            return Ok(ExitCode::from(2));
        }
    };
    let depth = depth.or(rules.depth).unwrap_or(DEFAULT_DEPTH);
    let findings = archmap_core::rules::check(&report.graph, &rules, depth);
    let signals = archmap_core::signals::signals(&report.graph, depth);

    let shown = rules_path.map(|p| p.display().to_string());
    match format {
        ReportFormat::Json => {
            let report = CheckReport {
                rules: shown,
                depth,
                findings: &findings,
                signals: &signals,
            };
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        ReportFormat::Text => print!(
            "{}",
            check_text(&report.graph, &findings, &signals, shown.as_deref(), depth)
        ),
    }
    Ok(if findings.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

fn load_rules(path: &Path) -> Result<RuleSet> {
    let text = std::fs::read_to_string(path).with_context(|| {
        format!(
            "reading {} (write one, or pass `--config <file>`)",
            path.display()
        )
    })?;
    toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
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
                    edge.as_str()
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
                    edge.as_str()
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
                    edge.as_str()
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
                out.push_str(&format!(
                    "unmatched: {declared} `{selector}` matches no component\n"
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
    if !signals.is_empty() {
        out.push_str("\nSignals are observations; they never change the exit code.\n");
    }
    if truncated {
        out.push_str("\nSome entries are left out; --format json lists every one.\n");
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
