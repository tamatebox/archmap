//! Output formats the CLI's flags name. Only JSON for the graph for now;
//! YAML / Markdown / Mermaid / Graphviz are intended to be added here as
//! further variants.

use anyhow::Result;
use clap::ValueEnum;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    Json,
}

/// Output of `archmap query`, `archmap impact` and `archmap check`: compact
/// text for agents, people and CI logs; JSON for tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ReportFormat {
    Text,
    Json,
}

impl From<ReportFormat> for archmap_app::Format {
    fn from(format: ReportFormat) -> Self {
        match format {
            ReportFormat::Text => archmap_app::Format::Text,
            ReportFormat::Json => archmap_app::Format::Json,
        }
    }
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
