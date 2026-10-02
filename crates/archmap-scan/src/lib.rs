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
mod ids;
mod languages;
pub mod python;
pub mod rust;
mod stamp;
mod test_code;
pub mod typescript;
mod walk;

pub use analyzer::Analyzer;
pub use context::RepoContext;
pub use error::ScanError;
pub use languages::language_of;
pub use stamp::{stamp, Stamp};
pub use test_code::is_test_code;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use archmap_core::{ArchitectureGraph, GraphMeta, LanguageCoverage};

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
    /// The root that was scanned, canonical. The graph leaves it out, so
    /// that two checkouts of a commit give the same graph.
    pub root: PathBuf,
    /// Non-fatal problems (unparseable file, unreadable manifest, ...).
    pub warnings: Vec<String>,
}

/// The analyzers archmap ships with, in the order they run.
///
/// Adding a language means adding an entry here and a module implementing
/// [`Analyzer`]; nothing in `archmap-core` needs to change.
pub fn default_analyzers() -> Vec<Box<dyn Analyzer>> {
    vec![
        Box::new(rust::RustAnalyzer),
        Box::new(python::PythonAnalyzer),
        Box::new(typescript::TypeScriptAnalyzer),
    ]
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
        analyzers: Vec::new(),
        tool_version: Some(env!("CARGO_PKG_VERSION").to_owned()),
        coverage: BTreeMap::new(),
    });
    let mut warnings = Vec::new();
    let mut read: BTreeMap<String, usize> = BTreeMap::new();
    let mut scripts: BTreeMap<String, usize> = BTreeMap::new();
    let mut contributed = ids::ContributedIds::default();

    for analyzer in analyzers {
        if !analyzer.detect(&ctx) {
            continue;
        }
        let mut output = analyzer.analyze(&ctx)?;
        warnings.extend(contributed.separate(analyzer.name(), &mut output.fragment));
        graph.meta.analyzers.push(analyzer.name().to_owned());
        graph.merge(output.fragment);
        warnings.extend(output.warnings);
        for (language, n) in output.read {
            *read.entry(language).or_default() += n;
        }
        for (language, n) in output.scripts {
            *scripts.entry(language).or_default() += n;
        }
    }

    graph.meta.coverage = coverage(ctx.files(), read, scripts);
    graph.normalize();
    Ok(ScanReport {
        graph,
        root: ctx.root().to_path_buf(),
        warnings,
    })
}

/// Files of each recognized language, with how many an analyzer read and
/// how many of those are scripts.
fn coverage(
    files: &[PathBuf],
    read: BTreeMap<String, usize>,
    scripts: BTreeMap<String, usize>,
) -> BTreeMap<String, LanguageCoverage> {
    let mut coverage: BTreeMap<String, LanguageCoverage> = languages::count_files(files)
        .into_iter()
        .map(|(language, files)| {
            let c = LanguageCoverage {
                files,
                ..LanguageCoverage::default()
            };
            (language.to_owned(), c)
        })
        .collect();
    for (language, n) in read {
        coverage.entry(language).or_default().read = Some(n);
    }
    for (language, n) in scripts {
        coverage.entry(language).or_default().scripts = n;
    }
    coverage
}
