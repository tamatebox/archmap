//! `archmap` command line interface.
//!
//! This crate is a thin adapter: it parses arguments, calls `archmap-scan`
//! and `archmap-core`, and renders the result. No analysis logic lives here.

mod commands;
mod output;
mod summary;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use crate::output::{CheckFormat, OutputFormat};

#[derive(Debug, Parser)]
#[command(name = "archmap", version, about = "Architecture graph for codebases")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Scan a repository and write its architecture graph.
    ///
    /// The graph is written to `<path>/.archmap/graph.json` unless
    /// `--output` says otherwise.
    Scan {
        /// Repository root (defaults to the current directory).
        #[arg(default_value = ".")]
        path: String,
        #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
        format: OutputFormat,
        /// File to write the graph to; `-` writes to stdout.
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Only read manifests; skip source parsing.
        #[arg(long)]
        manifests_only: bool,
    },
    /// Write a compact, deterministic Markdown summary for agents and humans.
    ///
    /// Modules are rolled up to `--depth` levels below their package. The
    /// summary is written to `<path>/.archmap/summary.md` unless `--output`
    /// says otherwise.
    Summary {
        /// Repository root (defaults to the current directory).
        #[arg(default_value = ".")]
        path: String,
        /// Containment depth to roll modules up to; 0 keeps only packages.
        #[arg(long, default_value_t = commands::DEFAULT_DEPTH)]
        depth: usize,
        /// File to write the summary to; `-` writes to stdout.
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Show a component or symbol with its relationships.
    ///
    /// Components are rolled up to `--depth` like in `summary`; a deeper
    /// component resolves to the component it is folded into.
    Query {
        /// Component id (e.g. `archmap-core`) or symbol name (e.g. `scan`).
        target: String,
        /// Repository root to scan.
        #[arg(long, default_value = ".")]
        path: String,
        /// Containment depth to roll modules up to, as in `summary`.
        #[arg(long, default_value_t = commands::DEFAULT_DEPTH)]
        depth: usize,
        #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
        format: OutputFormat,
    },
    /// List components that may be affected when a component or file changes.
    ///
    /// Components are rolled up to `--depth` like in `summary`.
    Impact {
        /// Component id or a file path relative to the repository root.
        target: String,
        /// Repository root to scan.
        #[arg(long, default_value = ".")]
        path: String,
        /// Containment depth to roll modules up to, as in `summary`.
        #[arg(long, default_value_t = commands::DEFAULT_DEPTH)]
        depth: usize,
        #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
        format: OutputFormat,
    },
    /// Check the observed graph against the declared rules in `archmap.toml`.
    ///
    /// Reports forbidden dependencies, dependency cycles and declarations
    /// that match nothing. Exits 0 without findings, 1 with findings, and 2
    /// when the rules or the repository cannot be read.
    Check {
        /// Repository root to scan.
        #[arg(long, default_value = ".")]
        path: String,
        /// Rules file; defaults to `<path>/archmap.toml`.
        #[arg(long)]
        config: Option<PathBuf>,
        /// Roll-up depth for cycle detection; overrides `depth` in the rules.
        #[arg(long)]
        depth: Option<usize>,
        #[arg(long, value_enum, default_value_t = CheckFormat::Text)]
        format: CheckFormat,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Scan {
            path,
            format,
            output,
            manifests_only,
        } => commands::scan(&path, format, output.as_deref(), manifests_only),
        Command::Summary {
            path,
            depth,
            output,
        } => commands::summary(&path, depth, output.as_deref()),
        Command::Query {
            target,
            path,
            depth,
            format,
        } => commands::query(&path, &target, depth, format),
        Command::Impact {
            target,
            path,
            depth,
            format,
        } => commands::impact(&path, &target, depth, format),
        Command::Check {
            path,
            config,
            depth,
            format,
        } => commands::check(&path, config.as_deref(), depth, format),
    };

    match result {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}
