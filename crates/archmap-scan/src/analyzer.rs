use archmap_core::GraphFragment;

use crate::{RepoContext, ScanError};

/// What an analyzer hands back: facts plus non-fatal problems.
#[derive(Debug, Default)]
pub struct AnalyzerOutput {
    pub fragment: GraphFragment,
    pub warnings: Vec<String>,
}

/// One source of architectural facts (a language, a manifest format, an
/// API schema).
///
/// Analyzers are statically registered in
/// [`default_analyzers`](crate::default_analyzers). There is intentionally no
/// dynamic plugin system.
pub trait Analyzer {
    /// Short stable name, recorded in graph metadata (`rust`, `cargo`, ...).
    fn name(&self) -> &'static str;

    /// Cheap check: does this repository contain something this analyzer
    /// understands? Should only look at file names, never parse.
    fn detect(&self, ctx: &RepoContext) -> bool;

    /// Extract facts. Problems with individual files go into
    /// [`AnalyzerOutput::warnings`]; only unrecoverable failures return `Err`.
    fn analyze(&self, ctx: &RepoContext) -> Result<AnalyzerOutput, ScanError>;
}
