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

mod by_symbol;
mod check;
mod co_change;
mod fetch;
mod impact;
mod impact_text;
mod not_traced;
mod pairs;
mod query;
mod query_text;
mod resolve;
mod summary;
mod target;
mod views;
mod work;
mod work_code;
mod work_line;
mod work_section;
mod work_text;

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result};
use archmap_core::history::History;
use archmap_core::work::Snapshot;
use archmap_core::ArchitectureGraph;
use archmap_scan::{ScanOptions, ScanReport};
use serde::Serialize;

pub use by_symbol::BySymbolRequest;
pub use check::{load_rules, CheckAnswer, Rules, RULES_FILE};
pub use fetch::{fetch_github, FetchRequest, DEFAULT_MAX_ITEMS};
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

/// How `query`, `impact` and `check` print: compact text with capped
/// lists, or JSON.
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
    pub format: Format,
    /// Every entry instead of capped lists: every list of the text, and
    /// every importer, test file and location of the JSON.
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
    /// The root's committed git history, read on the first command that
    /// needs it.
    history: OnceLock<History>,
    /// Where the work snapshot is read from, and what it held when first
    /// read.
    snapshot_path: PathBuf,
    snapshot: OnceLock<Result<Option<Snapshot>, String>>,
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
            history: OnceLock::new(),
            snapshot_path: root.join(archmap_scan::work::DEFAULT_PATH),
            snapshot: OnceLock::new(),
        })
    }

    /// Read the work snapshot from `path` instead of the root's default
    /// (`.archmap/github.json`).
    pub fn with_snapshot(mut self, path: &Path) -> Workspace {
        self.snapshot_path = path.to_path_buf();
        self.snapshot = OnceLock::new();
        self
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

    /// The committed history of the root, read once (bounded as
    /// `archmap_scan::history::DEFAULT_BOUND` says).
    fn history(&self) -> &History {
        self.history.get_or_init(|| {
            archmap_scan::history::read(&self.root, archmap_scan::history::DEFAULT_BOUND)
        })
    }

    /// Where the work snapshot is read from, relative to the root when it
    /// is inside it.
    fn snapshot_path(&self) -> String {
        let path = self
            .snapshot_path
            .strip_prefix(&self.root)
            .unwrap_or(&self.snapshot_path);
        path.to_string_lossy().replace('\\', "/")
    }

    /// The work snapshot, read once: `Ok(None)` when there is none.
    fn snapshot(&self) -> &Result<Option<Snapshot>, String> {
        self.snapshot
            .get_or_init(|| archmap_scan::work::read(&self.snapshot_path))
    }

    /// The Markdown summary at `depth`; `verbose` lists everything.
    pub fn summary(&self, depth: usize, verbose: bool) -> String {
        let work = match self.snapshot() {
            Ok(Some(snapshot)) => {
                let mut line = work_line::range_line(snapshot);
                let unmatched = work_code::unmatched(snapshot, self.history());
                if let Some(unmatched) = unmatched.filter(|u| u.unmatched > 0) {
                    line.push_str("; ");
                    line.push_str(&work_code::unmatched_line("its ", unmatched));
                }
                Some(line)
            }
            Ok(None) => None,
            Err(error) => Some(format!("unreadable: {error}")),
        };
        summary::render(
            self.graph(),
            &self.report.root,
            depth,
            verbose,
            work.as_deref(),
        )
    }
}

/// What a scan of a root would read, as far as sizes and times tell: an
/// interface that keeps a [`Workspace`] compares stamps to know when to
/// scan again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stamp(archmap_scan::Stamp);

/// Stamp `root` the way [`Workspace::scan`] reads it. Take it before the
/// scan, so an edit made during the scan shows in the next stamp.
pub fn stamp(root: &Path) -> Result<Stamp> {
    archmap_scan::stamp(root)
        .map(Stamp)
        .with_context(|| format!("scanning {}", root.display()))
}

/// Pretty JSON with a closing newline, as every command prints it.
fn json<T: Serialize>(value: &T) -> Result<String> {
    Ok(serde_json::to_string_pretty(value)? + "\n")
}
