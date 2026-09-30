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

/// What `archmap query` returns for a component.
#[derive(Debug, Serialize)]
pub struct ComponentView<'a> {
    pub component: &'a Component,
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

pub fn query(path: &str, target: &str, format: OutputFormat) -> Result<ExitCode> {
    let report = run_scan(path, false)?;
    let graph = &report.graph;

    let result = if let Some(component) = find_component(graph, target) {
        QueryResult::Component(component_view(graph, component))
    } else {
        let symbols: Vec<&Symbol> = graph
            .symbol(&SymbolId::new(target))
            .into_iter()
            .chain(graph.symbols_named(target))
            .collect();
        if symbols.is_empty() {
            bail!("no component or symbol named `{target}`");
        }
        QueryResult::Symbols(symbols)
    };

    println!("{}", render(&result, format)?);
    Ok(ExitCode::SUCCESS)
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

fn component_view<'a>(graph: &'a ArchitectureGraph, component: &'a Component) -> ComponentView<'a> {
    ComponentView {
        component,
        symbols: graph.symbols_of(&component.id).collect(),
        outgoing: graph.outgoing(&component.id).collect(),
        incoming: graph.incoming(&component.id).collect(),
    }
}

#[derive(Debug, Serialize)]
pub struct ImpactResult<'a> {
    pub target: &'a ComponentId,
    /// Components that directly depend on the target.
    pub direct: Vec<ComponentId>,
    /// Every component that transitively depends on the target.
    pub transitive: Vec<ComponentId>,
}

pub fn impact(path: &str, target: &str, format: OutputFormat) -> Result<ExitCode> {
    // Import edges come from source, so impact needs a full scan.
    let report = run_scan(path, false)?;
    let graph = &report.graph;

    let component = find_component(graph, target)
        .or_else(|| graph.component_for_path(target))
        .with_context(|| format!("no component or file `{target}` in graph"))?;

    let result = ImpactResult {
        target: &component.id,
        direct: graph.dependents_of(&component.id).into_iter().collect(),
        transitive: graph
            .transitive_dependents(&component.id)
            .into_iter()
            .collect(),
    };
    println!("{}", render(&result, format)?);
    Ok(ExitCode::SUCCESS)
}

pub fn check(_path: &str) -> Result<ExitCode> {
    eprintln!("archmap check: architecture rules are not implemented yet");
    Ok(ExitCode::from(2))
}
