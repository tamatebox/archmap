//! The four tools over MCP. Every answer comes from `archmap-app`, as the
//! CLI's does; this file only maps arguments in and answers out.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{bail, Context};
use archmap_app::{
    load_rules, BySymbolRequest, CheckRequest, Format, ImpactRequest, QueryRequest, Rules,
    Workspace, DEFAULT_DEPTH, RULES_FILE,
};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::{schemars, tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler};
use serde::Deserialize;

use crate::cache::Cache;
use crate::text;

/// archmap's MCP server. Tools read `root` unless a call gives `path`.
#[derive(Clone)]
pub struct Server {
    root: Arc<PathBuf>,
    cache: Arc<Mutex<Cache>>,
    tool_router: ToolRouter<Self>,
}

/// `text` (the default): compact, capped lists; `json`: every entry and all
/// evidence.
#[derive(Debug, Clone, Copy, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
enum OutputFormat {
    Text,
    Json,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SummaryArgs {
    /// Another repository root, or a directory to map as one: absolute or
    /// relative to the project directory. Default: the project directory.
    #[serde(default)]
    path: Option<String>,
    /// Containment depth that modules roll up to; 0 keeps only packages.
    /// Default 2, the same for every tool.
    #[serde(default)]
    depth: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct QueryArgs {
    /// What to look up: a file or directory path (absolute or relative to
    /// the root), a component name or id, a symbol (`Class.method`,
    /// `Type::method`) or its id, `<component>.<file stem>`, a package
    /// subpath, an import name no component carries, a file name, the last
    /// part of a component's name, a name taken from a package, or an
    /// environment variable (`env:APP_REGION`).
    target: String,
    /// Another repository root, absolute or relative to the project
    /// directory. Default: the project directory.
    #[serde(default)]
    path: Option<String>,
    /// Containment depth that modules roll up to. Default 2, as `summary`.
    #[serde(default)]
    depth: Option<u32>,
    /// `text` (default) or `json`.
    #[serde(default)]
    format: Option<OutputFormat>,
    /// For a file: each public symbol with the statements that take it and
    /// where it is used.
    #[serde(default)]
    by_symbol: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ImpactArgs {
    /// What changes: anything `query` takes but `#N`.
    target: String,
    /// Another repository root, absolute or relative to the project
    /// directory. Default: the project directory.
    #[serde(default)]
    path: Option<String>,
    /// Containment depth that modules roll up to. Default 2, as `summary`.
    #[serde(default)]
    depth: Option<u32>,
    /// `text` (default) or `json`.
    #[serde(default)]
    format: Option<OutputFormat>,
    /// List every entry in the text instead of capped lists.
    #[serde(default)]
    verbose: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct CheckArgs {
    /// Another repository root, absolute or relative to the project
    /// directory. Default: the project directory.
    #[serde(default)]
    path: Option<String>,
    /// Rules file inside the root, relative to it. Default: `archmap.toml`
    /// when there is one.
    #[serde(default)]
    config: Option<String>,
    /// Roll-up depth for cycles. Default: the rules' `depth`, else 2.
    #[serde(default)]
    depth: Option<u32>,
    /// `text` (default) or `json`.
    #[serde(default)]
    format: Option<OutputFormat>,
}

#[tool_router]
impl Server {
    #[tool(
        description = text::SUMMARY,
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    async fn summary(
        &self,
        Parameters(args): Parameters<SummaryArgs>,
    ) -> Result<CallToolResult, McpError> {
        let depth = depth_or_default(args.depth);
        self.answer(args.path, None, move |ws, _| Ok(ws.summary(depth, false)))
            .await
    }

    #[tool(
        description = text::QUERY,
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    async fn query(
        &self,
        Parameters(args): Parameters<QueryArgs>,
    ) -> Result<CallToolResult, McpError> {
        let QueryArgs {
            target,
            path,
            depth,
            format,
            by_symbol,
        } = args;
        let depth = depth_or_default(depth);
        let outside_check = target.clone();
        self.answer(path, Some(&outside_check), move |ws, _| {
            let format = format_or_text(format);
            let answer = if by_symbol.unwrap_or(false) {
                ws.by_symbol(&BySymbolRequest {
                    target: &target,
                    depth,
                    format,
                    verbose: false,
                })?
            } else {
                ws.query(&QueryRequest {
                    target: &target,
                    depth,
                    format,
                    verbose: false,
                })?
            };
            Ok(answer.output)
        })
        .await
    }

    #[tool(
        description = text::IMPACT,
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    async fn impact(
        &self,
        Parameters(args): Parameters<ImpactArgs>,
    ) -> Result<CallToolResult, McpError> {
        let ImpactArgs {
            target,
            path,
            depth,
            format,
            verbose,
        } = args;
        let depth = depth_or_default(depth);
        let outside_check = target.clone();
        self.answer(path, Some(&outside_check), move |ws, _| {
            let answer = ws.impact(&ImpactRequest {
                target: &target,
                depth,
                format: format_or_text(format),
                verbose: verbose.unwrap_or(false),
            })?;
            Ok(answer.output)
        })
        .await
    }

    #[tool(
        description = text::CHECK,
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    async fn check(
        &self,
        Parameters(args): Parameters<CheckArgs>,
    ) -> Result<CallToolResult, McpError> {
        let CheckArgs {
            path,
            config,
            depth,
            format,
        } = args;
        self.answer(path, None, move |ws, root| {
            // read on every call, never kept: the rules may change apart from the code
            let rules = rules_in(root, config.as_deref())?;
            let answer = ws.check(
                &rules,
                &CheckRequest {
                    depth: depth.map(|d| d as usize),
                    format: format_or_text(format),
                },
            )?;
            Ok(answer.output)
        })
        .await
    }
}

impl Server {
    /// A server whose tools read `root` (canonical) by default.
    pub fn new(root: PathBuf) -> Server {
        Server {
            root: Arc::new(root),
            cache: Arc::default(),
            tool_router: Self::tool_router(),
        }
    }

    /// How many scans the server has made: one per root, and another only
    /// after its files change.
    pub fn scans(&self) -> usize {
        self.cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .scans()
    }

    /// Answer from the workspace of the call's root on a blocking thread,
    /// one call at a time, so a second call on a root waits for its scan
    /// instead of scanning it again. An error is a tool error the client
    /// sees; scan warnings follow the answer in a block of their own, so a
    /// JSON answer stays JSON.
    async fn answer<F>(
        &self,
        path: Option<String>,
        target: Option<&str>,
        ask: F,
    ) -> Result<CallToolResult, McpError>
    where
        F: FnOnce(&Workspace, &Path) -> anyhow::Result<String> + Send + 'static,
    {
        let root = match self.root_for(path.as_deref()) {
            Ok(root) => root,
            Err(message) => return Ok(CallToolResult::error(vec![ContentBlock::text(message)])),
        };
        // a path outside the root needs no scan, and must not wait for one
        if let Some(Err(err)) = target.map(|t| archmap_app::reject_outside(&root, t)) {
            return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                "{err:#}"
            ))]));
        }
        let cache = self.cache.clone();
        let done = tokio::task::spawn_blocking(move || {
            let mut cache = cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let workspace = cache.workspace(&root)?;
            let output = ask(&workspace, &root)?;
            anyhow::Ok((output, workspace.warnings().to_vec()))
        })
        .await
        .map_err(|err| McpError::internal_error(err.to_string(), None))?;
        Ok(match done {
            Ok((output, warnings)) => {
                let mut content = vec![ContentBlock::text(output)];
                if !warnings.is_empty() {
                    content.push(ContentBlock::text(warnings_text(&warnings)));
                }
                CallToolResult::success(content)
            }
            Err(err) => CallToolResult::error(vec![ContentBlock::text(format!("{err:#}"))]),
        })
    }

    /// The root a call reads: `path`, absolute or relative to the default
    /// root, else the default root.
    fn root_for(&self, path: Option<&str>) -> Result<PathBuf, String> {
        let Some(path) = path else {
            return Ok(self.root.as_ref().clone());
        };
        let given = Path::new(path);
        let joined = if given.is_absolute() {
            given.to_path_buf()
        } else {
            self.root.join(given)
        };
        match std::fs::canonicalize(&joined) {
            Ok(dir) if dir.is_dir() => Ok(dir),
            _ => Err(format!(
                "`{path}` is no directory (a relative path starts at {})",
                self.root.display()
            )),
        }
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("archmap", env!("CARGO_PKG_VERSION")))
            .with_instructions(text::INSTRUCTIONS)
    }
}

fn depth_or_default(depth: Option<u32>) -> usize {
    depth.map_or(DEFAULT_DEPTH, |d| d as usize)
}

fn format_or_text(format: Option<OutputFormat>) -> Format {
    match format {
        Some(OutputFormat::Json) => Format::Json,
        Some(OutputFormat::Text) | None => Format::Text,
    }
}

/// The rules of `root`: from `config` when given, which must be a file
/// inside the root, else from `archmap.toml` when there is one. The report
/// names the file as the call did, relative to the root.
fn rules_in(root: &Path, config: Option<&str>) -> anyhow::Result<Rules> {
    let Some(config) = config else {
        let rules = load_rules(root, None)?;
        return Ok(match rules.label() {
            Some(_) => rules.with_label(RULES_FILE),
            None => rules,
        });
    };
    let file = std::fs::canonicalize(root.join(config))
        .with_context(|| format!("no rules file `{config}` under {}", root.display()))?;
    if !file.starts_with(root) {
        bail!("`{config}` is outside the root {}", root.display());
    }
    Ok(load_rules(root, Some(&file))?.with_label(config))
}

/// Warnings listed after an answer; the rest are counted.
const MAX_WARNINGS: usize = 5;

fn warnings_text(warnings: &[String]) -> String {
    let mut out = format!("Scan warnings: {}\n", warnings.len());
    for warning in warnings.iter().take(MAX_WARNINGS) {
        out.push_str(&format!("  {warning}\n"));
    }
    if warnings.len() > MAX_WARNINGS {
        out.push_str(&format!("  +{} more\n", warnings.len() - MAX_WARNINGS));
    }
    out
}
