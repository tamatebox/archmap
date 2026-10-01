//! archmap's MCP server: `summary`, `query`, `impact` and `check` as tools.
//!
//! It is the other first-class interface beside the CLI, with the same
//! capabilities: every answer comes from `archmap-app`, and nothing here
//! runs the CLI. The server keeps one scanned workspace per root and scans
//! a root again when its files change. Its root comes from its options and
//! each call's `path`, never from an agent's environment.

mod cache;
mod server;
mod text;

pub use server::Server;

use std::path::PathBuf;

use anyhow::{ensure, Context, Result};
use rmcp::ServiceExt;

/// What the server starts with.
#[derive(Debug, Clone)]
pub struct Options {
    /// The repository the tools read unless a call names another.
    pub root: PathBuf,
}

/// Serve the tools over stdin and stdout until the client closes them.
/// stdout carries JSON-RPC only.
pub fn serve_stdio(options: Options) -> Result<()> {
    let root = std::fs::canonicalize(&options.root)
        .with_context(|| format!("reading the root {}", options.root.display()))?;
    ensure!(root.is_dir(), "the root {} is no directory", root.display());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let running = Server::new(root).serve(rmcp::transport::stdio()).await?;
        running.waiting().await?;
        Ok(())
    })
}
