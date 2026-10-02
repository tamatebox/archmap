//! Each command: scan through `archmap-app`, print its answer, pick the exit
//! code.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use archmap_app::{Answer, CheckRequest, Found, ImpactRequest, QueryRequest, ScanMode, Workspace};

use crate::output::{render, OutputFormat, ReportFormat};

/// Scan `path` and print the scan's warnings to stderr.
fn run_scan(path: &str, mode: ScanMode) -> Result<Workspace> {
    let workspace = Workspace::scan(Path::new(path), mode)?;
    for warning in workspace.warnings() {
        eprintln!("warning: {warning}");
    }
    Ok(workspace)
}

/// Directory, relative to the scanned root, that holds generated output.
pub const OUTPUT_DIR: &str = ".archmap";

pub fn scan(
    path: &str,
    format: OutputFormat,
    output: Option<&Path>,
    manifests_only: bool,
) -> Result<ExitCode> {
    let mode = if manifests_only {
        ScanMode::ManifestsOnly
    } else {
        ScanMode::Full
    };
    let workspace = run_scan(path, mode)?;
    let graph = workspace.graph();
    let rendered = render(graph, format)? + "\n";
    let default_name = format!("graph.{}", format.extension());
    if let Some(file) = write_output(path, &default_name, output, &rendered)? {
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
    let markdown = run_scan(path, ScanMode::Full)?.summary(depth, verbose);
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

pub fn query(
    path: &str,
    target: &str,
    depth: usize,
    format: ReportFormat,
    verbose: bool,
    snapshot: Option<&Path>,
) -> Result<ExitCode> {
    // fails before scanning; the shared layer checks again for every interface
    archmap_app::reject_outside(Path::new(path), target)?;
    let mut workspace = run_scan(path, ScanMode::Full)?;
    if let Some(snapshot) = snapshot {
        workspace = workspace.with_snapshot(snapshot);
    }
    let answer = workspace.query(&QueryRequest {
        target,
        depth,
        format: format.into(),
        verbose,
    })?;
    Ok(print_answer(answer))
}

pub fn impact(
    path: &str,
    target: &str,
    depth: usize,
    format: ReportFormat,
    verbose: bool,
) -> Result<ExitCode> {
    // fails before scanning; the shared layer checks again for every interface
    archmap_app::reject_outside(Path::new(path), target)?;
    let answer = run_scan(path, ScanMode::Full)?.impact(&ImpactRequest {
        target,
        depth,
        format: format.into(),
        verbose,
    })?;
    Ok(print_answer(answer))
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
    let rules = match archmap_app::load_rules(Path::new(path), config) {
        Ok(rules) => rules,
        Err(err) => {
            eprintln!("error: {err:#}");
            return Ok(ExitCode::from(2));
        }
    };
    let workspace = match run_scan(path, ScanMode::Full) {
        Ok(workspace) => workspace,
        Err(err) => {
            eprintln!("error: {err:#}");
            return Ok(ExitCode::from(2));
        }
    };
    let answer = workspace.check(
        &rules,
        &CheckRequest {
            depth,
            format: format.into(),
        },
    )?;
    print!("{}", answer.output);
    Ok(if answer.findings == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

/// Print an answer of `query` or `impact`: exit 0 for one target, 1 when it
/// lists candidates to choose from.
fn print_answer(answer: Answer) -> ExitCode {
    print!("{}", answer.output);
    match answer.found {
        Found::One => ExitCode::SUCCESS,
        Found::Candidates => ExitCode::from(1),
    }
}

pub fn fetch_github(path: &str, request: &archmap_app::FetchRequest) -> Result<ExitCode> {
    let written = archmap_app::fetch_github(Path::new(path), request)?;
    print!("{written}");
    Ok(ExitCode::SUCCESS)
}
