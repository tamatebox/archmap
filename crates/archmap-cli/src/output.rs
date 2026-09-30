//! Rendering of results. Only JSON for now; YAML / Markdown / Mermaid /
//! Graphviz are intended to be added here as further variants.

use anyhow::Result;
use clap::ValueEnum;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    Json,
}

/// Output of `archmap query` and `archmap check`: compact text for agents,
/// people and CI logs; complete JSON for tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ReportFormat {
    Text,
    Json,
}

impl OutputFormat {
    /// File extension used when the graph is written to a file.
    pub fn extension(self) -> &'static str {
        match self {
            OutputFormat::Json => "json",
        }
    }
}

pub fn render<T: Serialize>(value: &T, format: OutputFormat) -> Result<String> {
    match format {
        OutputFormat::Json => Ok(serde_json::to_string_pretty(value)?),
    }
}
