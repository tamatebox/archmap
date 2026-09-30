//! Repository scanning: walk files, detect projects, extract facts.
//!
//! The output of this crate is always a normalized
//! [`ArchitectureGraph`](archmap_core::ArchitectureGraph). Each language or
//! manifest format is handled by an [`Analyzer`] that emits a
//! [`GraphFragment`](archmap_core::GraphFragment); the [`scan`] function
//! merges the fragments.
//!
//! Analyzers extract *facts* (what the code and manifests literally say).
//! They must not guess architectural meaning.

mod analyzer;
mod context;
mod error;
pub mod rust;
mod walk;

pub use analyzer::Analyzer;
pub use context::RepoContext;
pub use error::ScanError;

use std::path::Path;

use archmap_core::{ArchitectureGraph, GraphMeta};

/// Options controlling a scan.
#[derive(Debug, Clone, Default)]
pub struct ScanOptions {
    /// Skip source parsing and only extract manifest-level facts.
    pub manifests_only: bool,
}

/// Result of a scan: the graph plus non-fatal problems encountered.
#[derive(Debug)]
pub struct ScanReport {
    pub graph: ArchitectureGraph,
    /// Non-fatal problems (unparseable file, unreadable manifest, ...).
    pub warnings: Vec<String>,
}

/// The analyzers archmap ships with, in the order they run.
///
/// Adding a language means adding an entry here and a module implementing
/// [`Analyzer`]; nothing in `archmap-core` needs to change.
pub fn default_analyzers() -> Vec<Box<dyn Analyzer>> {
    vec![Box::new(rust::RustAnalyzer)]
}

/// Scan a repository with the default analyzers.
pub fn scan(root: &Path, options: &ScanOptions) -> Result<ScanReport, ScanError> {
    scan_with(root, options, &default_analyzers())
}

/// Scan a repository with an explicit analyzer set.
pub fn scan_with(
    root: &Path,
    options: &ScanOptions,
    analyzers: &[Box<dyn Analyzer>],
) -> Result<ScanReport, ScanError> {
    let ctx = RepoContext::load(root, options.clone())?;
    let mut graph = ArchitectureGraph::new(GraphMeta {
        root: ctx.root_display(),
        analyzers: Vec::new(),
        tool_version: Some(env!("CARGO_PKG_VERSION").to_owned()),
    });
    let mut warnings = Vec::new();

    for analyzer in analyzers {
        if !analyzer.detect(&ctx) {
            continue;
        }
        let output = analyzer.analyze(&ctx)?;
        graph.meta.analyzers.push(analyzer.name().to_owned());
        graph.merge(output.fragment);
        warnings.extend(output.warnings);
    }

    graph.normalize();
    Ok(ScanReport { graph, warnings })
}
