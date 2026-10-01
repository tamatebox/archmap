use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{bail, Context, Result};
use archmap_core::rules::{external_selector_lacks_ecosystem, FileLevel, Finding, RuleSet};
use archmap_core::signals::Signal;
use archmap_core::{
    ArchitectureGraph, ChangeSeed, Component, ComponentId, ComponentKind, Edge, EdgeKind, Evidence,
    Scope, Symbol, SymbolId, UnmappedImport,
};
use archmap_scan::{ScanOptions, ScanReport};
use serde::Serialize;

use crate::output::{render, OutputFormat, ReportFormat};
use crate::views::{ComponentView, FileView, Importer, QueryResult, SymbolView, UnmappedView};

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

pub fn summary(path: &str, depth: usize, verbose: bool, output: Option<&Path>) -> Result<ExitCode> {
    let report = run_scan(path, false)?;
    let markdown = crate::summary::render(&report.graph, depth, verbose);
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

/// `target` as a file under the scanned root, relative with `/` separators.
fn file_target(path: &str, target: &str) -> Option<String> {
    let relative = target.trim_start_matches("./").trim_end_matches('/');
    Path::new(path)
        .join(relative)
        .is_file()
        .then(|| relative.replace('\\', "/"))
}

/// The file a component stands for: its path, when that is a file under
/// the scanned root and no component is inside it (a TS/JS file, a Rust
/// module without submodules). `query` and `impact` answer for such a
/// component as for its file, even where it folds into an ancestor.
fn component_file(full: &ArchitectureGraph, path: &str, component: &Component) -> Option<String> {
    let has_children = full
        .components
        .values()
        .any(|c| c.parent.as_ref() == Some(&component.id));
    if has_children {
        return None;
    }
    file_target(path, component.path.as_deref()?)
}

/// The component that owns `target` as a directory under the scanned root,
/// the same for `query` and `impact`. `Err` when no component contains it.
fn directory_target<'a>(
    full: &'a ArchitectureGraph,
    path: &str,
    target: &str,
) -> Option<Result<&'a Component>> {
    let relative = target.trim_start_matches("./").trim_end_matches('/');
    Path::new(path).join(relative).is_dir().then(|| {
        full.component_for_path(&relative.replace('\\', "/"))
            .with_context(|| format!("no component contains `{target}`"))
    })
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
        script: facts
            .component
            .and_then(|c| full.component(c))
            .is_some_and(|c| c.kind == ComponentKind::Script),
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
    reject_ambiguous(full, target)?;

    let named =
        find_component(&rolled, full, target).or_else(|| find_component(full, full, target));
    let result = if let Some(component) = named {
        match component_file(full, path, component) {
            Some(file) => QueryResult::File(file_view(full, depth, target, &file)),
            None => {
                let at = fold(full, depth, &component.id);
                component_view(full, &rolled, depth, target, at)?
            }
        }
    } else if let Some(file) = file_target(path, target) {
        QueryResult::File(file_view(full, depth, target, &file))
    } else {
        let symbols: Vec<&Symbol> = rolled
            .symbol(&SymbolId::new(target))
            .into_iter()
            .chain(rolled.symbols_named(target))
            .collect();
        if !symbols.is_empty() {
            QueryResult::Symbols(
                symbols
                    .into_iter()
                    .map(|symbol| symbol_view(full, symbol))
                    .collect(),
            )
        } else if let Some(file) = full.file_for_dotted_name(target) {
            QueryResult::File(file_view(full, depth, target, file))
        } else if let Some(owner) = directory_target(full, path, target).transpose()? {
            // Late: every directory has an owner, the root at worst, so a
            // bare word naming one must not shadow a symbol.
            let at = fold(full, depth, &owner.id);
            component_view(full, &rolled, depth, target, at)?
        } else {
            // An import name that no component carries, such as an extra.
            let not_mapped: Vec<&UnmappedImport> = full.unmapped_imports_of(target).collect();
            if not_mapped.is_empty() {
                bail!("no component, file, symbol or import named `{target}`");
            }
            QueryResult::NotMapped(UnmappedView {
                requested: target,
                module: target,
                depth,
                not_mapped,
            })
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

/// A symbol with the statements that import it, read in the full graph.
fn symbol_view<'a>(full: &'a ArchitectureGraph, symbol: &'a Symbol) -> SymbolView<'a> {
    let importers = full.symbol_importers(symbol).filter(|i| i.recorded);
    let list = |pairs: Vec<(&'a Edge, &'a Evidence)>| {
        pairs
            .into_iter()
            .map(|(edge, evidence)| Importer {
                from: &edge.from,
                evidence,
            })
            .collect()
    };
    match importers {
        Some(found) => SymbolView {
            symbol,
            imported_by: Some(list(found.by_name)),
            may_use: Some(list(found.may_use)),
        },
        None => SymbolView {
            symbol,
            imported_by: None,
            may_use: None,
        },
    }
}

/// The component `at` points to, as `query` shows it.
fn component_view<'a>(
    full: &'a ArchitectureGraph,
    rolled: &'a ArchitectureGraph,
    depth: usize,
    requested: &'a str,
    at: AtDepth,
) -> Result<QueryResult<'a>> {
    let component = rolled
        .component(&at.id)
        .with_context(|| format!("`{}` is missing after roll-up", at.id))?;
    let (also_named, also_at_path) = namesakes(full, component);
    Ok(QueryResult::Component(ComponentView {
        requested,
        depth,
        folded_from: at.folded_from,
        component,
        also_named,
        also_at_path,
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
    }))
}

/// A component as seen at a roll-up depth.
struct AtDepth {
    id: ComponentId,
    folded_from: Option<ComponentId>,
}

/// A component as seen at `depth`: the ancestor it folds into, if any.
fn fold(full: &ArchitectureGraph, depth: usize, id: &ComponentId) -> AtDepth {
    let ancestor = full.ancestor_at(id, depth);
    let folded_from = (ancestor != *id).then(|| id.clone());
    AtDepth {
        id: ancestor,
        folded_from,
    }
}

/// Exact id first, then a unique match on the display name. Components
/// that share a name and a path, one directory that two analyzers see,
/// resolve to the one that owns the path in `full`: roll-up moves evidence
/// to ancestors, so only the full graph decides the owner.
fn find_component<'a>(
    graph: &'a ArchitectureGraph,
    full: &ArchitectureGraph,
    target: &str,
) -> Option<&'a Component> {
    graph.component(&ComponentId::new(target)).or_else(|| {
        let named: Vec<&Component> = graph.components_named(target).collect();
        match named.as_slice() {
            [] => None,
            [only] => Some(*only),
            several => {
                owner_of_shared_path(full, several).and_then(|owner| graph.component(&owner.id))
            }
        }
    })
}

/// The component of `full` that owns the path all of `named` share, if
/// they share one.
fn owner_of_shared_path<'a>(
    full: &'a ArchitectureGraph,
    named: &[&Component],
) -> Option<&'a Component> {
    let path = named.first()?.path.as_deref()?;
    if named.iter().any(|c| c.path.as_deref() != Some(path)) {
        return None;
    }
    full.component_for_path(path)
        .filter(|owner| named.iter().any(|c| c.id == owner.id))
}

/// Components listed when a name is shared; the rest are counted.
const MAX_CANDIDATES: usize = 10;

/// Stop when `target` is no component id but the name of several
/// components, listing their ids and paths. It runs on the full graph
/// before any lookup, since roll-up can leave only one of them visible.
fn reject_ambiguous(full: &ArchitectureGraph, target: &str) -> Result<()> {
    if full.component(&ComponentId::new(target)).is_some() {
        return Ok(());
    }
    let named: Vec<&Component> = full.components_named(target).collect();
    if named.len() < 2 || owner_of_shared_path(full, &named).is_some() {
        return Ok(());
    }
    let mut message = format!(
        "`{target}` names {} components; give an id, or a path as ./<path>:",
        named.len()
    );
    for component in named.iter().take(MAX_CANDIDATES) {
        let _ = write!(
            message,
            "\n  {}  {}",
            crate::query_text::shell_word(component.id.as_str()),
            component.path.as_deref().unwrap_or("-")
        );
    }
    if named.len() > MAX_CANDIDATES {
        let _ = write!(message, "\n  +{} more", named.len() - MAX_CANDIDATES);
    }
    bail!(message)
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
    /// For a symbol: its id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<SymbolId>,
    /// Other components with the target's name, and at its path.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub also_named: Vec<ComponentId>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub also_at_path: Vec<ComponentId>,
    /// Components that directly depend on the target.
    pub direct: Vec<ComponentId>,
    /// Every component that transitively depends on the target.
    pub transitive: Vec<ComponentId>,
    /// Components that only test code reaches: the tests to run again.
    pub tests: Vec<ComponentId>,
    /// For a file, or a component that is one file: the statements that
    /// import the file directly. For a symbol: those that take its name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub importers: Option<ImportSites>,
    /// For a symbol: the statements that take its file whole.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub may_use: Option<ImportSites>,
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
    /// The statement is test code.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub test: bool,
}

fn import_sites(full: &ArchitectureGraph, depth: usize, file: &str) -> ImportSites {
    let facts = full.file_facts(file);
    sites_of(full, depth, &facts.importers, facts.importers_recorded)
}

/// One site per statement, sorted by place; the first few shown.
fn sites_of(
    full: &ArchitectureGraph,
    depth: usize,
    statements: &[(&Edge, &Evidence)],
    recorded: bool,
) -> ImportSites {
    let mut sites: Vec<ImportSite> = Vec::new();
    for (edge, e) in statements {
        if !sites.iter().any(|s| s.file == e.file && s.line == e.line) {
            sites.push(ImportSite {
                file: e.file.clone(),
                line: e.line,
                component: full.ancestor_at(&edge.from, depth),
                test: e.test,
            });
        }
    }
    // production code first
    sites.sort_by(|a, b| (a.test, &a.file, a.line).cmp(&(b.test, &b.file, b.line)));
    let total = sites.len();
    sites.truncate(MAX_IMPORT_SITES);
    ImportSites {
        recorded,
        total,
        shown: sites,
    }
}

/// Other components that share `component`'s name, and those that share
/// its path: an id or a path answered for one of them, and theirs pick the
/// others.
fn namesakes<'a>(
    full: &'a ArchitectureGraph,
    component: &Component,
) -> (Vec<&'a ComponentId>, Vec<&'a ComponentId>) {
    let at_path: Vec<&ComponentId> = full
        .components
        .values()
        .filter(|c| c.id != component.id && c.path.is_some() && c.path == component.path)
        .map(|c| &c.id)
        .collect();
    let named = full
        .components_named(&component.name)
        .filter(|c| c.id != component.id && !at_path.contains(&&c.id))
        .map(|c| &c.id)
        .collect();
    (named, at_path)
}

/// The one symbol `target` names, by id or by name; an error that lists the
/// candidates when several share the name.
fn symbols_for<'a>(full: &'a ArchitectureGraph, target: &str) -> Vec<&'a Symbol> {
    match full.symbol(&SymbolId::new(target)) {
        Some(symbol) => vec![symbol],
        None => full.symbols_named(target).collect(),
    }
}

/// The error for a name that several symbols share: their ids, quoted for
/// the shell where needed, and where they are.
fn ambiguous_symbols(target: &str, symbols: &[&Symbol]) -> String {
    let mut message = format!("`{target}` names {} symbols; give an id:", symbols.len());
    for symbol in symbols.iter().take(MAX_CANDIDATES) {
        let at = symbol
            .location()
            .map(|e| format!("{}:{}", e.file, e.line.unwrap_or(0)))
            .unwrap_or_default();
        let _ = write!(
            message,
            "\n  {}  {at}",
            crate::query_text::shell_word(symbol.id.as_str())
        );
    }
    if symbols.len() > MAX_CANDIDATES {
        let _ = write!(message, "\n  +{} more", symbols.len() - MAX_CANDIDATES);
    }
    message
}

pub fn impact(path: &str, target: &str, depth: usize, format: OutputFormat) -> Result<ExitCode> {
    // Import edges come from source, so impact needs a full scan.
    let report = run_scan(path, false)?;
    let full = &report.graph;
    let rolled = full.rollup(depth);
    reject_ambiguous(full, target)?;

    let component =
        find_component(&rolled, full, target).or_else(|| find_component(full, full, target));
    let symbols = symbols_for(full, target);
    let mut importers = None;
    let (mut symbol_id, mut may_use) = (None, None);
    let (at, reach) = if let Some(component) = component {
        let reach = full.change_impact(ChangeSeed::Component(&component.id), depth);
        importers =
            component_file(full, path, component).map(|file| import_sites(full, depth, &file));
        (fold(full, depth, &component.id), reach)
    } else if let Some(file) = file_target(path, target) {
        let owner = full
            .component_for_path(&file)
            .with_context(|| format!("no component contains `{target}`"))?;
        let reach = full.change_impact(ChangeSeed::File(&file), depth);
        importers = Some(import_sites(full, depth, &file));
        (fold(full, depth, &owner.id), reach)
    } else if let [symbol] = symbols.as_slice() {
        match full.component(&ComponentId::new(symbol.id.as_str())) {
            // a Rust module's symbol stands for its component
            Some(module) => {
                let reach = full.change_impact(ChangeSeed::Component(&module.id), depth);
                importers =
                    component_file(full, path, module).map(|file| import_sites(full, depth, &file));
                (fold(full, depth, &module.id), reach)
            }
            None => {
                let reach = full.change_impact(ChangeSeed::Symbol(symbol), depth);
                if let Some(found) = full.symbol_importers(symbol) {
                    importers = Some(sites_of(full, depth, &found.by_name, found.recorded));
                    may_use = Some(sites_of(full, depth, &found.may_use, found.recorded));
                }
                symbol_id = Some(symbol.id.clone());
                (fold(full, depth, &symbol.component), reach)
            }
        }
    } else if let Some(owner) = directory_target(full, path, target).transpose()? {
        // a directory answers before names that several symbols share fail
        let reach = full.change_impact(ChangeSeed::Component(&owner.id), depth);
        (fold(full, depth, &owner.id), reach)
    } else if symbols.len() > 1 {
        bail!(ambiguous_symbols(target, &symbols));
    } else {
        bail!("no component, file or symbol `{target}` in graph");
    };

    let (also_named, also_at_path) = full
        .component(&at.id)
        .map(|c| namesakes(full, c))
        .unwrap_or_default();
    let owned = |ids: Vec<&ComponentId>| ids.into_iter().cloned().collect();
    let result = ImpactResult {
        also_named: owned(also_named),
        also_at_path: owned(also_at_path),
        requested: target,
        depth,
        direct: reach.direct.into_iter().collect(),
        transitive: reach.transitive.into_iter().collect(),
        tests: reach.tests.into_iter().collect(),
        target: at.id,
        folded_from: at.folded_from,
        symbol: symbol_id,
        importers,
        may_use,
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
