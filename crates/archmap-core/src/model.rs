use serde::{Deserialize, Serialize};

use crate::Evidence;

/// Stable identifier for a [`Component`].
///
/// Analyzers choose the string. Convention so far: internal packages use the
/// package name (`archmap-core`), their modules `<package>::<path>`
/// (`archmap-core::graph`, `shop::shop.billing`), and external dependencies
/// are prefixed with `ext:` (`ext:serde`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ComponentId(pub String);

impl ComponentId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for ComponentId {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl std::fmt::Display for ComponentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Stable identifier for a [`Symbol`], unique within a graph.
///
/// Convention so far: `<component>::<module path>::<name>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SymbolId(pub String);

impl SymbolId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for SymbolId {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl std::fmt::Display for SymbolId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What kind of architectural unit a component is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentKind {
    /// A buildable unit owned by the repository (Cargo package, npm package, ...).
    Package,
    /// A sub-unit of a package (Rust module, Python module, Go package, ...).
    Module,
    /// A dependency that lives outside the repository.
    External,
}

/// A coarse architectural unit: a package, a module, an external dependency.
///
/// Components are the primary nodes of the architecture graph. They are
/// intentionally coarser than functions or classes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Component {
    pub id: ComponentId,
    pub name: String,
    pub kind: ComponentKind,
    /// Primary language of the component, when known (`rust`, `typescript`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Root path relative to the repository, when the component lives in it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Enclosing component (a module's package), when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<ComponentId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<Evidence>,
}

impl Component {
    pub fn new(id: impl Into<ComponentId>, name: impl Into<String>, kind: ComponentKind) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            kind,
            language: None,
            path: None,
            parent: None,
            evidence: Vec::new(),
        }
    }
}

impl From<String> for ComponentId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

/// What kind of public interface element a symbol is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Function,
    Struct,
    Enum,
    Trait,
    TypeAlias,
    Constant,
    Module,
    /// Anything an analyzer exports but does not classify yet.
    Other,
}

/// A public interface element of a component.
///
/// Symbols are only recorded when they matter architecturally: public
/// functions, types, traits. Private helpers are deliberately left out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Symbol {
    pub id: SymbolId,
    pub name: String,
    pub kind: SymbolKind,
    pub component: ComponentId,
    /// Human-readable signature (`pub fn scan(root: &Path) -> Result<Graph>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<Evidence>,
}

/// An import that maps to no component, standard-library imports aside.
///
/// It is an observation, not a dependency: it never becomes an edge, and
/// `reason` says why. It tells readers of the graph where a dependency may
/// exist that no edge shows; `check` reports the undeclared ones.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct UnmappedImport {
    /// The component whose source contains the import.
    pub from: ComponentId,
    /// Dotted module path as imported (`google.api_core.exceptions`).
    pub module: String,
    pub reason: UnmappedReason,
    /// Installed distributions that provide the module, when a virtualenv
    /// shows it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provided_by: Vec<String>,
    pub evidence: Evidence,
}

impl UnmappedImport {
    /// Whether the dotted `prefix` names this import's module or a module
    /// above it: `google.api_core` covers `google.api_core.exceptions`,
    /// `google.api` does not.
    pub fn covered_by(&self, prefix: &str) -> bool {
        let prefix = prefix.trim();
        self.module == prefix || self.module.starts_with(&format!("{prefix}."))
    }
}

/// Why an import maps to no component.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnmappedReason {
    /// No manifest declares the package.
    Undeclared,
    /// A manifest declares the package, but not as a required dependency:
    /// an extra, a dependency group or a development dependency. Only
    /// required dependencies become edges.
    DeclaredNotRequired,
    /// Matches no module, but a file or directory of that name exists in
    /// the repository: probably local code reached through `sys.path`.
    LocalName,
}

/// A module loaded by a name computed at runtime (`importlib.import_module`,
/// `__import__`). No edge can follow it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DynamicImport {
    /// The component whose source makes the call.
    pub from: ComponentId,
    /// The function called (`import_module`).
    pub call: String,
    pub evidence: Evidence,
}

/// What a scan saw of one language.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LanguageCoverage {
    /// Files of the language under the root, after ignore rules.
    pub files: usize,
    /// Files an analyzer read. `None` when no analyzer reads the language.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read: Option<usize>,
}

/// The nature of a relationship between two components.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    /// Source-level import (`use`, `import`, `require`).
    Import,
    /// Declared dependency in a manifest (Cargo.toml, package.json, ...).
    Dependency,
    /// A call across component boundaries.
    Call,
    /// HTTP request / route relationship.
    Http,
    /// Database access.
    Database,
    /// Event publish / subscribe.
    Event,
    /// Relationship found but not classified.
    Unknown,
}

impl EdgeKind {
    /// The serialized name (`import`, `dependency`, ...).
    pub fn as_str(self) -> &'static str {
        match self {
            EdgeKind::Import => "import",
            EdgeKind::Dependency => "dependency",
            EdgeKind::Call => "call",
            EdgeKind::Http => "http",
            EdgeKind::Database => "database",
            EdgeKind::Event => "event",
            EdgeKind::Unknown => "unknown",
        }
    }
}

/// A directed relationship between two components with supporting evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    pub from: ComponentId,
    pub to: ComponentId,
    pub kind: EdgeKind,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<Evidence>,
}

impl Edge {
    pub fn new(from: impl Into<ComponentId>, to: impl Into<ComponentId>, kind: EdgeKind) -> Self {
        Self {
            from: from.into(),
            to: to.into(),
            kind,
            evidence: Vec::new(),
        }
    }

    pub fn with_evidence(mut self, evidence: Evidence) -> Self {
        self.evidence.push(evidence);
        self
    }

    /// Two edges are the same relationship when they connect the same
    /// components with the same kind; evidence is merged, not compared.
    pub fn same_relationship(&self, other: &Edge) -> bool {
        self.from == other.from && self.to == other.to && self.kind == other.kind
    }

    /// Distinct source locations behind the edge. One statement can point
    /// at several files (`from pkg import a, b`), so this can be smaller
    /// than `evidence.len()`.
    pub fn statements(&self) -> usize {
        self.evidence
            .iter()
            .map(|e| (&e.file, e.line))
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    }
}
