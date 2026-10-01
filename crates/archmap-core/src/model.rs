use serde::{Deserialize, Serialize};

use crate::Evidence;

/// Stable identifier for a [`Component`].
///
/// Analyzers choose the string. Convention so far: internal packages use the
/// package name (`archmap-core`), their modules `<package>::<path>`
/// (`archmap-core::graph`, `shop::shop.billing`), and external dependencies
/// are `ext:<ecosystem>:<name>` (`ext:cargo:serde`, `ext:pypi:requests`).
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
    /// A file TypeScript reads as a script (a TS/JS file without imports or
    /// exports): it checks the top-level declarations as global, so other
    /// files use them without importing the file.
    Script,
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

impl Symbol {
    /// Where the symbol is defined: its first evidence without a `target`.
    /// Evidence with a `target` says how the symbol is reached instead (a
    /// Rust method whose type another file defines).
    pub fn location(&self) -> Option<&Evidence> {
        self.evidence.iter().find(|e| e.target.is_none())
    }
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
    /// The module as the import names it: a dotted module path
    /// (`google.api_core.exceptions`) or a specifier as written
    /// (`lodash/fp`, `@/lib/missing`).
    pub module: String,
    pub reason: UnmappedReason,
    /// Installed distributions that provide the module, when a virtualenv
    /// shows it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provided_by: Vec<String>,
    pub evidence: Evidence,
}

impl UnmappedImport {
    /// Whether `prefix` names this import's module or a module above it,
    /// with `.` or `/` between segments: `google.api_core` covers
    /// `google.api_core.exceptions`, `lodash` covers `lodash/fp`,
    /// `google.api` covers neither.
    pub fn covered_by(&self, prefix: &str) -> bool {
        let prefix = prefix.trim();
        self.module == prefix
            || self.module.starts_with(&format!("{prefix}."))
            || self.module.starts_with(&format!("{prefix}/"))
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
    /// the project: probably local code reached through `sys.path`
    /// (Python) or an alias the scan does not read (TS/JS).
    LocalName,
    /// A path or alias that matches no scanned file and is no package
    /// name: a missing or generated file, or an alias defined outside the
    /// configuration the scan reads.
    Unresolved,
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
    /// Files read as scripts: without imports or exports, so no import
    /// shows who uses their top-level declarations.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub scripts: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
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

    /// Whether the dependency is there when the program runs: some of its
    /// evidence takes more than types, or it has no evidence to say.
    pub fn at_runtime(&self) -> bool {
        self.evidence.is_empty() || self.evidence.iter().any(|e| !e.type_only)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn import(module: &str) -> UnmappedImport {
        UnmappedImport {
            from: "a".into(),
            module: module.into(),
            reason: UnmappedReason::Unresolved,
            provided_by: Vec::new(),
            evidence: Evidence::new("a.ts"),
        }
    }

    #[test]
    fn a_symbol_is_where_its_evidence_names_no_target() {
        let symbol = Symbol {
            id: SymbolId::new("a::T::m"),
            name: "T::m".into(),
            kind: SymbolKind::Function,
            component: "a".into(),
            signature: None,
            evidence: vec![
                // how it is reached comes first here, on purpose
                Evidence::new("src/impls.rs")
                    .at_line(3)
                    .with_note("impl")
                    .pointing_at("src/t.rs")
                    .taking(["T"]),
                Evidence::new("src/impls.rs").at_line(4),
            ],
        };
        assert_eq!(symbol.location().and_then(|e| e.line), Some(4));
    }

    #[test]
    fn prefixes_cover_dotted_and_slashed_modules() {
        assert!(import("lodash/fp").covered_by("lodash"));
        assert!(import("@scope/pkg/sub").covered_by("@scope/pkg"));
        assert!(!import("lodash-es").covered_by("lodash"));
        assert!(import("google.api_core.exceptions").covered_by("google.api_core"));
        assert!(!import("google.api_core").covered_by("google.api"));
    }

    #[test]
    fn scripts_are_a_kind_and_counted_only_when_there_are_some() {
        assert_eq!(
            serde_json::to_value(ComponentKind::Script).unwrap(),
            serde_json::json!("script")
        );
        let none = LanguageCoverage {
            files: 2,
            read: Some(2),
            scripts: 0,
        };
        assert_eq!(
            serde_json::to_value(&none).unwrap(),
            serde_json::json!({ "files": 2, "read": 2 })
        );
        let some = LanguageCoverage { scripts: 1, ..none };
        assert_eq!(
            serde_json::to_value(&some).unwrap(),
            serde_json::json!({ "files": 2, "read": 2, "scripts": 1 })
        );
        let read: LanguageCoverage = serde_json::from_str(r#"{ "files": 2 }"#).unwrap();
        assert_eq!(read.scripts, 0);
    }

    #[test]
    fn the_unresolved_reason_serializes_in_snake_case() {
        let json = serde_json::to_string(&UnmappedReason::Unresolved).unwrap();
        assert_eq!(json, "\"unresolved\"");
    }
}
