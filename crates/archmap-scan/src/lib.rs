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
pub mod history;
mod ids;
mod languages;
mod lines;
mod options;
pub mod python;
pub mod rust;
mod stamp;
mod test_code;
pub mod typescript;
mod uses;
mod walk;
pub mod work;

pub use analyzer::Analyzer;
pub use context::RepoContext;
pub use error::ScanError;
pub use languages::language_of;
pub use options::ScanOptions;
pub use rust::uses::takes_self;
pub use stamp::{stamp, Stamp};
pub use test_code::{is_test_code, TestKind};
pub use typescript::is_mock_call;
pub use uses::{package_name_uses, symbol_uses};

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use archmap_core::{ArchitectureGraph, GraphMeta, LanguageCoverage};

/// Result of a scan: the graph plus non-fatal problems encountered.
#[derive(Debug)]
pub struct ScanReport {
    pub graph: ArchitectureGraph,
    /// The root that was scanned, canonical. The graph leaves it out, so
    /// that two checkouts of a commit give the same graph.
    pub root: PathBuf,
    /// Non-fatal problems (unparseable file, unreadable manifest, ...).
    pub warnings: Vec<String>,
    /// What the Rust analyzer read, for the passes that run on demand.
    pub(crate) rust: Option<rust::Index>,
    /// What the TS/JS analyzer read, for [`route_files`] and
    /// [`env_uses`].
    pub(crate) typescript: Option<typescript::Kept>,
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
    let (mut rust, mut typescript) = (None, None);

    for analyzer in analyzers {
        if !analyzer.detect(&ctx) {
            continue;
        }
        let mut output = analyzer.analyze(&ctx)?;
        if let Some(kept) = output.kept.take() {
            match kept.downcast::<rust::Index>() {
                Ok(index) => rust = Some(*index),
                Err(kept) => typescript = kept.downcast::<typescript::Kept>().ok().map(|k| *k),
            }
        }
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
        rust,
        typescript,
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

/// Where the code reads and writes the environment variable `name`, read on
/// demand from the TS/JS files the scan read whose text names it or
/// `process.env`: `process.env.X`, `process.env["X"]`, a destructuring of
/// `process.env`, `import.meta.env.X`, and the places that may read it
/// unseen. Other languages are not read for it.
pub fn env_uses(report: &ScanReport, name: &str) -> archmap_core::EnvUses {
    let marks = report.graph.test_code();
    let test = |file: &str| {
        marks
            .get(file)
            .copied()
            .unwrap_or_else(|| is_test_code(Path::new(file)))
    };
    report
        .typescript
        .as_ref()
        .map(|kept| kept.env_uses(&report.root, name, test))
        .unwrap_or_default()
}

/// The files among `paths` that a framework runs before the requests of
/// every URL they match, so a test of any URL may reach them: Next.js's
/// `middleware.ts`, or `proxy.ts` since Next.js 16, in the directory of a
/// package whose manifest declares `next` or in its `src/`.
pub fn before_routes<'a>(
    report: &ScanReport,
    paths: impl IntoIterator<Item = &'a str>,
) -> BTreeSet<&'a str> {
    report
        .typescript
        .as_ref()
        .map(|kept| kept.routes.before(paths))
        .unwrap_or_default()
}

/// The files among `paths` that a framework loads for a URL, so a test may
/// reach them through it: in a package whose manifest declares `next`, the
/// route files below `app/` or `src/app/` outside private folders
/// (`_name`), and every file below `pages/` or `src/pages/`, `pages/api/`
/// included. Test code is none, by the rule the scan reads it with, nor are
/// the files Next.js loads by name for every request (`middleware.ts`,
/// `instrumentation.ts`).
pub fn route_files<'a>(
    report: &ScanReport,
    paths: impl IntoIterator<Item = &'a str>,
) -> BTreeSet<&'a str> {
    report
        .typescript
        .as_ref()
        .map(|kept| kept.routes.files(paths))
        .unwrap_or_default()
}

/// What each of `paths`, files of test code, is to a test runner: for
/// Python and TS/JS by the runners' default names (a test file's name, a
/// file below `__tests__`), for Rust by the kind of Cargo target whose tree
/// holds it (the root of a test is a test, a module of one a helper). A
/// file it cannot tell stays a test.
pub fn test_kinds<'a>(
    report: &ScanReport,
    paths: impl IntoIterator<Item = &'a str>,
) -> BTreeMap<&'a str, TestKind> {
    let rust = report
        .rust
        .as_ref()
        .map(|index| index.targets_of_files())
        .unwrap_or_default();
    paths
        .into_iter()
        .map(|path| {
            let kind = match rust.get(path) {
                Some(targets) => test_code::rust_kind(targets),
                None => test_code::kind_by_name(Path::new(path)),
            };
            (path, kind)
        })
        .collect()
}
