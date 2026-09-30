use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{bail, Context, Result};
use archmap_core::{ArchitectureGraph, Component, ComponentId, Edge, Symbol, SymbolId};
use archmap_scan::{ScanOptions, ScanReport};
use serde::Serialize;

use crate::output::{render, OutputFormat};

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
    if let Some(file) = write_output(path, "summary.md", output, &markdown)? {
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
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum QueryResult<'a> {
    Component(ComponentView<'a>),
    Symbols(Vec<&'a Symbol>),
}

pub fn query(path: &str, target: &str, depth: usize, format: OutputFormat) -> Result<ExitCode> {
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
        })
    } else {
        let symbols: Vec<&Symbol> = rolled
            .symbol(&SymbolId::new(target))
            .into_iter()
            .chain(rolled.symbols_named(target))
            .collect();
        if symbols.is_empty() {
            bail!("no component or symbol named `{target}`");
        }
        QueryResult::Symbols(symbols)
    };

    println!("{}", render(&result, format)?);
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
}

pub fn impact(path: &str, target: &str, depth: usize, format: OutputFormat) -> Result<ExitCode> {
    // Import edges come from source, so impact needs a full scan.
    let report = run_scan(path, false)?;
    let full = &report.graph;
    let rolled = full.rollup(depth);

    let at = resolve_at_depth(full, &rolled, depth, target)
        .or_else(|| {
            full.component_for_path(target)
                .map(|c| fold(full, depth, &c.id))
        })
        .with_context(|| format!("no component or file `{target}` in graph"))?;

    let result = ImpactResult {
        requested: target,
        depth,
        direct: rolled.dependents_of(&at.id).into_iter().collect(),
        transitive: rolled.transitive_dependents(&at.id).into_iter().collect(),
        target: at.id,
        folded_from: at.folded_from,
    };
    println!("{}", render(&result, format)?);
    Ok(ExitCode::SUCCESS)
}

pub fn check(_path: &str) -> Result<ExitCode> {
    eprintln!("archmap check: architecture rules are not implemented yet");
    Ok(ExitCode::from(2))
}
