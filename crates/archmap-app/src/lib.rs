//! What every archmap interface shares.
//!
//! Every interface to archmap (the CLI, and an MCP server) offers the same
//! capabilities: it scans a repository into a [`Workspace`] and asks it for
//! `summary`, `query`, `impact` and `check`, which come back as finished
//! text or JSON, so interfaces cannot drift. Nothing here prints, exits or
//! parses arguments: that is each interface's own business.
//!
//! The commands are `Workspace` methods in their own modules (`query`,
//! `impact`, `check`), which depend on this file and never the other way.

mod check;
mod impact;
mod pairs;
mod query;
mod query_text;
mod resolve;
mod summary;
mod target;
mod views;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use archmap_core::ArchitectureGraph;
use archmap_scan::{ScanOptions, ScanReport};
use serde::Serialize;

pub use check::{load_rules, CheckAnswer, Rules, RULES_FILE};
pub use target::reject_outside;

/// Depth that `summary`, `query` and `impact` roll up to unless told
/// otherwise, so the three always describe the same components.
pub const DEFAULT_DEPTH: usize = 2;

/// How much of a repository a scan reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanMode {
    /// Manifests and source.
    Full,
    /// Manifests only, no source parsing.
    ManifestsOnly,
}

/// How `query` and `check` print: compact text with capped lists, or
/// complete JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Text,
    Json,
}

/// What `query` is asked.
#[derive(Debug, Clone, Copy)]
pub struct QueryRequest<'a> {
    pub target: &'a str,
    pub depth: usize,
    pub format: Format,
    /// Every entry instead of capped lists (text only; JSON has them all).
    pub verbose: bool,
}

/// What `query` and `impact` answer: their text or JSON, or the
/// candidates when the target names several things.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub output: String,
    pub found: Found,
}

/// Whether an answer is about one target or lists candidates to choose
/// from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Found {
    One,
    Candidates,
}

/// What `impact` is asked.
#[derive(Debug, Clone, Copy)]
pub struct ImpactRequest<'a> {
    pub target: &'a str,
    pub depth: usize,
    /// Every importer and test file instead of the first few.
    pub verbose: bool,
}

/// What `check` is asked, beside the rules.
#[derive(Debug, Clone, Copy)]
pub struct CheckRequest {
    /// Roll-up depth for cycles; the rules' `depth`, else the default.
    pub depth: Option<usize>,
    pub format: Format,
}

/// A scanned repository: the graph, and what the scan could not read.
#[derive(Debug)]
pub struct Workspace {
    root: PathBuf,
    report: ScanReport,
}

impl Workspace {
    /// Scan the repository at `root`.
    pub fn scan(root: &Path, mode: ScanMode) -> Result<Workspace> {
        let options = ScanOptions {
            manifests_only: mode == ScanMode::ManifestsOnly,
        };
        let report = archmap_scan::scan(root, &options)
            .with_context(|| format!("scanning {}", root.display()))?;
        Ok(Workspace {
            root: root.to_path_buf(),
            report,
        })
    }

    /// The root as given to [`Workspace::scan`]; path targets are read
    /// against it.
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn graph(&self) -> &ArchitectureGraph {
        &self.report.graph
    }

    /// Non-fatal problems of the scan (an unparseable file, an unreadable
    /// manifest).
    pub fn warnings(&self) -> &[String] {
        &self.report.warnings
    }

    /// The Markdown summary at `depth`; `verbose` lists everything.
    pub fn summary(&self, depth: usize, verbose: bool) -> String {
        summary::render(self.graph(), depth, verbose)
    }
}

/// Pretty JSON with a closing newline, as every command prints it.
fn json<T: Serialize>(value: &T) -> Result<String> {
    Ok(serde_json::to_string_pretty(value)? + "\n")
}
