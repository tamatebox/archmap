//! `archmap` command line interface.
//!
//! One of archmap's interfaces: it parses arguments, asks `archmap-app` for
//! the answer, prints it and picks the exit code. Analysis, lookups and
//! rendering live in `archmap-app`, shared with the MCP server.

mod commands;
mod output;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use crate::output::{OutputFormat, ReportFormat};

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
    /// Print a compact, deterministic summary for agents and humans.
    ///
    /// Modules are rolled up to `--depth` levels below their package. Lists
    /// are capped to keep it small, with `omitted:` lines for the rest. The
    /// summary goes to stdout; `--output <file>` saves it instead.
    Summary {
        /// Repository root (defaults to the current directory).
        #[arg(default_value = ".")]
        path: String,
        /// Containment depth to roll modules up to; 0 keeps only packages.
        #[arg(long, default_value_t = archmap_app::DEFAULT_DEPTH)]
        depth: usize,
        /// File to save the summary to instead of printing it.
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// List every component and dependency instead of capped lists.
        #[arg(long)]
        verbose: bool,
    },
    /// Show a component or symbol with its relationships.
    ///
    /// Components are rolled up to `--depth` like in `summary`; a deeper
    /// component resolves to the component it is folded into.
    Query {
        /// A file or directory path (relative to the root or absolute), a
        /// component name or id (`archmap-core`), a symbol name or id
        /// (`scan`), `<component>.<file stem>`, a package subpath, an import
        /// name no component carries (`torch`) or a file name. Several
        /// matches are listed as candidates, with exit code 1.
        target: String,
        /// Repository root to scan.
        #[arg(long, default_value = ".")]
        path: String,
        /// Containment depth to roll modules up to, as in `summary`.
        #[arg(long, default_value_t = archmap_app::DEFAULT_DEPTH)]
        depth: usize,
        /// Compact text with capped lists, or complete JSON.
        #[arg(long, value_enum, default_value_t = ReportFormat::Text)]
        format: ReportFormat,
        /// Show every symbol, neighbor and location instead of capped lists.
        #[arg(long)]
        verbose: bool,
    },
    /// List components that may be affected when a component or file changes.
    ///
    /// Components are rolled up to `--depth` like in `summary`.
    Impact {
        /// Anything `query` takes; several matches are listed as candidates,
        /// with exit code 1.
        target: String,
        /// Repository root to scan.
        #[arg(long, default_value = ".")]
        path: String,
        /// Containment depth to roll modules up to, as in `summary`.
        #[arg(long, default_value_t = archmap_app::DEFAULT_DEPTH)]
        depth: usize,
        /// Compact text with capped lists, or JSON.
        #[arg(long, value_enum, default_value_t = ReportFormat::Text)]
        format: ReportFormat,
        /// List every entry instead of capped lists, in text and in JSON.
        #[arg(long)]
        verbose: bool,
    },
    /// Check the observed graph against the declared rules in `archmap.toml`.
    ///
    /// Reports rule findings and structural signals. Exits 0 without
    /// findings, 1 with findings, and 2 when the rules or the repository
    /// cannot be read. Signals never change the exit code; without a rules
    /// file only signals are reported.
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
        #[arg(long, value_enum, default_value_t = ReportFormat::Text)]
        format: ReportFormat,
    },
    /// Serve summary, query, impact and check as MCP tools over stdio.
    ///
    /// The tools give the same answers as these commands. They read
    /// `--path` unless a call names another root, and the server scans a
    /// root again when its files change. stdout carries JSON-RPC only.
    Mcp {
        /// Repository the tools read by default.
        #[arg(long, default_value = ".")]
        path: PathBuf,
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
            verbose,
        } => commands::summary(&path, depth, verbose, output.as_deref()),
        Command::Query {
            target,
            path,
            depth,
            format,
            verbose,
        } => commands::query(&path, &target, depth, format, verbose),
        Command::Impact {
            target,
            path,
            depth,
            format,
            verbose,
        } => commands::impact(&path, &target, depth, format, verbose),
        Command::Check {
            path,
            config,
            depth,
            format,
        } => commands::check(&path, config.as_deref(), depth, format),
        Command::Mcp { path } => archmap_mcp::serve_stdio(archmap_mcp::Options { root: path })
            .map(|()| ExitCode::SUCCESS),
    };

    match result {
        Ok(code) => code,
        // 1 is a result to act on (findings, candidates); 2 is a failure
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::from(2)
        }
    }
}
