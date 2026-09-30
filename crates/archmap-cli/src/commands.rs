use std::path::Path;
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

pub fn scan(path: &str, format: OutputFormat, manifests_only: bool) -> Result<ExitCode> {
    let report = run_scan(path, manifests_only)?;
    println!("{}", render(&report.graph, format)?);
    Ok(ExitCode::SUCCESS)
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

    let result = if let Some(component) = graph.component(&ComponentId::new(target)) {
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
    let report = run_scan(path, true)?;
    let graph = &report.graph;

    let component = graph
        .component(&ComponentId::new(target))
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
