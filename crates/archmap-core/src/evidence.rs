use serde::{Deserialize, Serialize};

/// Why a node or edge exists in the graph.
///
/// Every fact we extract should point back to the place it was derived from,
/// so that a human or an agent can verify it and jump to the source. For
/// dependencies on source code, evidence also keeps the file the statement
/// points at: the fine detail that roll-up hides and that impact and cycle
/// checks recover when they need it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Evidence {
    /// Path relative to the scanned repository root.
    pub file: String,
    /// 1-based line number, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// Free-form note explaining the derivation (for example `use` or
    /// `Cargo.toml [dependencies]`). A note of one word names only the kind
    /// of statement (`import`, `use`); longer notes say how a name was
    /// resolved or where it is declared, and views may show only those.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// For a dependency on a file in the repository: that file, relative to
    /// the root. `None` when the target is external or not resolved to a
    /// file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// Where the statement sits, for languages where it matters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<Scope>,
}

/// Where an import statement sits in its file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// Outside any function body. In Python it runs when the file is loaded.
    Module,
    /// Inside a function body. In Python it runs only when the function is
    /// called.
    Local,
}

impl Evidence {
    pub fn new(file: impl Into<String>) -> Self {
        Self {
            file: file.into(),
            line: None,
            note: None,
            target: None,
            scope: None,
        }
    }

    pub fn at_line(mut self, line: u32) -> Self {
        self.line = Some(line);
        self
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }

    pub fn pointing_at(mut self, target: impl Into<String>) -> Self {
        self.target = Some(target.into());
        self
    }

    pub fn in_scope(mut self, scope: Scope) -> Self {
        self.scope = Some(scope);
        self
    }
}
