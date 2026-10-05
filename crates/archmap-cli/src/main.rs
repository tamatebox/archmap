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
        /// name no component carries (`torch`), a file name or the last
        /// part of a component's name. Several matches, or for a word that
        /// names nothing the names that contain it, are listed as
        /// candidates, with exit code 1.
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
        /// The work snapshot that `'#N'` targets read, instead of the
        /// root's `.archmap/github.json`.
        #[arg(long)]
        snapshot: Option<PathBuf>,
    },
    /// List components that may be affected when a component, file, symbol
    /// or import name changes.
    ///
    /// Components are rolled up to `--depth` like in `summary`.
    Impact {
        /// Anything `query` takes but an issue or pull request (`'#N'`);
        /// several matches are listed as candidates, with exit code 1.
        target: String,
        /// Repository root to scan.
        #[arg(long, default_value = ".")]
        path: String,
        /// Containment depth to roll modules up to, as in `summary`.
        #[arg(long, default_value_t = archmap_app::DEFAULT_DEPTH)]
        depth: usize,
        /// Compact text with capped lists, or JSON with every entry.
        #[arg(long, value_enum, default_value_t = ReportFormat::Text)]
        format: ReportFormat,
        /// List every entry in the text instead of capped lists.
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
    /// Fetch a snapshot of the work behind changes into the root.
    ///
    /// The one command that reaches the network; every other command reads
    /// the snapshot it writes.
    Fetch {
        #[command(subcommand)]
        source: FetchSource,
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

#[derive(Debug, Subcommand)]
enum FetchSource {
    /// Issues, pull requests and the links GitHub records between them,
    /// through the gh CLI and its login, into `.archmap/github.json`.
    ///
    /// Reads the items updated since a date, newest first, up to a bound:
    /// each issue and pull request's number, title, state and times, a pull
    /// request's commits, and the links by type. No bodies, comments or
    /// authors; no token is written. Writes nothing unless the whole fetch
    /// succeeds.
    Github {
        /// Repository root to write into, and whose `origin` names the
        /// repository on github.com.
        #[arg(long, default_value = ".")]
        path: String,
        /// The repository: OWNER/NAME on github.com, or HOST/OWNER/NAME for
        /// a host gh is logged in to.
        #[arg(long)]
        repo: Option<String>,
        /// Items updated since this date (2026-01-31) or UTC time; default:
        /// the oldest commit of the local history read, else 365 days ago.
        #[arg(long, conflicts_with = "all")]
        since: Option<String>,
        /// Every item, whatever its date.
        #[arg(long)]
        all: bool,
        /// Items read per kind at most, newest first.
        #[arg(long, default_value_t = archmap_app::DEFAULT_MAX_ITEMS)]
        max_items: usize,
        /// Keep numbers, states and times only: titles may be confidential
        /// and reach whatever reads the answers.
        #[arg(long)]
        no_titles: bool,
        /// Write here instead of `<path>/.archmap/github.json`.
        #[arg(short, long)]
        output: Option<PathBuf>,
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
            snapshot,
        } => commands::query(&path, &target, depth, format, verbose, snapshot.as_deref()),
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
        Command::Fetch {
            source:
                FetchSource::Github {
                    path,
                    repo,
                    since,
                    all,
                    max_items,
                    no_titles,
                    output,
                },
        } => commands::fetch_github(
            &path,
            &archmap_app::FetchRequest {
                repo: repo.as_deref(),
                since: since.as_deref(),
                all,
                max_items,
                titles: !no_titles,
                output: output.as_deref(),
            },
        ),
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
