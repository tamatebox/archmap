//! What `query` and `impact` return: built by [`crate::query`] and
//! [`crate::impact`], rendered as JSON there or as text by
//! [`crate::query_text`] and [`crate::impact_text`].

use archmap_core::{
    Component, ComponentId, DynamicImport, Edge, Evidence, Symbol, SymbolId, UnmappedImport,
};
use serde::Serialize;

use crate::not_traced::NotTraced;

/// What `archmap query` returns for a component.
#[derive(Debug, Serialize)]
pub struct ComponentView<'a> {
    /// The target as given on the command line.
    pub requested: &'a str,
    pub depth: usize,
    /// The requested component, when it is folded into `component` at this
    /// depth.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folded_from: Option<ComponentId>,
    /// For a package subpath (`react-dom/client`): the part after the
    /// package name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subpath: Option<String>,
    pub component: &'a Component,
    /// Other components with the component's name: its id answered, and
    /// theirs pick them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub also_named: Vec<&'a ComponentId>,
    /// Other components at the component's path, as when two analyzers map
    /// one directory.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub also_at_path: Vec<&'a ComponentId>,
    /// Direct children in the unrolled graph, to query with a larger depth.
    pub children: Vec<&'a ComponentId>,
    pub symbols: Vec<&'a Symbol>,
    pub outgoing: Vec<&'a Edge>,
    pub incoming: Vec<&'a Edge>,
    /// Imports in the component that map to no component: dependencies
    /// that no edge shows.
    pub not_mapped: Vec<&'a UnmappedImport>,
    /// Modules the component loads by names computed at runtime.
    pub dynamic_imports: Vec<&'a DynamicImport>,
    /// What could reach the target unseen, from what analyzers record.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub not_traced: Option<NotTraced>,
}

/// What `archmap query` returns for a file: the file-level facts behind a
/// component, rolled up to the same depth.
#[derive(Debug, Serialize)]
pub struct FileView<'a> {
    /// The target as given on the command line (a path or a dotted module name).
    pub requested: &'a str,
    pub depth: usize,
    pub file: String,
    /// The component that contains the file, at this depth.
    pub component: Option<ComponentId>,
    /// Other components with the name of the file's own component (a TS
    /// file, a Rust module without submodules), as for a component.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub also_named: Vec<&'a ComponentId>,
    /// Other components at its path.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub also_at_path: Vec<&'a ComponentId>,
    pub symbols: Vec<&'a Symbol>,
    /// The file's import statements, one edge per imported component.
    pub imports: Vec<Edge>,
    /// Statements elsewhere that import the file, one edge per importing
    /// component. `None` when no evidence names imported files for the
    /// file's language, so importers are unknown rather than absent.
    pub importers: Option<Vec<Edge>>,
    pub not_mapped: Vec<&'a UnmappedImport>,
    pub dynamic_imports: Vec<&'a DynamicImport>,
    /// The file is a script, whose declarations are global: no import
    /// shows what uses them.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub script: bool,
    /// What could reach the target unseen, from what analyzers record.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub not_traced: Option<NotTraced>,
}

/// What `archmap query` returns for an import name that no component
/// carries (`torch` declared only as an extra, `helpers` reached through
/// `sys.path`): every import without an edge of that module or a module
/// below it.
#[derive(Debug, Serialize)]
pub struct UnmappedView<'a> {
    /// The target as given on the command line.
    pub requested: &'a str,
    /// The import name looked up, the dotted prefix of every module below.
    pub module: &'a str,
    pub depth: usize,
    pub not_mapped: Vec<&'a UnmappedImport>,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum QueryResult<'a> {
    Component(ComponentView<'a>),
    File(FileView<'a>),
    Symbols(Vec<SymbolView<'a>>),
    NotMapped(UnmappedView<'a>),
}

/// A symbol `query` found, with the statements that import it.
#[derive(Debug, Serialize)]
pub struct SymbolView<'a> {
    #[serde(flatten)]
    pub symbol: &'a Symbol,
    /// Statements that take the symbol's name from the file it is reached
    /// through (its own, or its type's for a Rust method). `None` when no
    /// evidence names imported files for its language: unknown, not none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub imported_by: Option<Vec<Importer<'a>>>,
    /// Statements that take that file whole, the others aside.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub may_use: Option<Vec<Importer<'a>>>,
    /// What could reach the target unseen, from what analyzers record.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub not_traced: Option<NotTraced>,
}

/// An importing statement: the component it is in, and its evidence.
#[derive(Debug, Serialize)]
pub struct Importer<'a> {
    pub from: &'a ComponentId,
    #[serde(flatten)]
    pub evidence: &'a Evidence,
    /// The barrel the statement reaches the symbol through, a file that
    /// passes it on, when the statement takes that file rather than the
    /// symbol's own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub through: Option<&'a str>,
}

/// What `archmap impact` returns: rendered as JSON by [`crate::impact`],
/// or as text by [`crate::impact_text`] from the same lists.
#[derive(Debug, Serialize)]
pub struct ImpactResult<'a> {
    /// The target as given on the command line.
    pub requested: &'a str,
    pub depth: usize,
    /// The component that changes; `null` for an import name that no
    /// component carries, given in `module`.
    pub target: Option<ComponentId>,
    /// For an import name that no component carries: that name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    /// The component that owns the request, when it is folded into `target`
    /// at this depth.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folded_from: Option<ComponentId>,
    /// For a symbol: its id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<SymbolId>,
    /// For a package subpath (`react-dom/client`): the part after the
    /// package name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subpath: Option<String>,
    /// Other components with the target's name, and at its path.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub also_named: Vec<ComponentId>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub also_at_path: Vec<ComponentId>,
    /// Components that directly depend on the target.
    pub direct: Vec<ComponentId>,
    /// Every component that transitively depends on the target.
    pub transitive: Vec<ComponentId>,
    /// Files that reach the target only through test code, and a changed
    /// component's own test files: the tests to run again.
    pub tests: TestFiles,
    /// For a file, or a component that is one file: the statements that
    /// import the file directly. For a symbol: those that take its name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub importers: Option<ImportSites<'a>>,
    /// For a symbol: the statements that take its file whole.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub may_use: Option<ImportSites<'a>>,
    /// What could reach the target unseen, from what analyzers record.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub not_traced: Option<NotTraced>,
    /// What the answer is about, for the first lines of the text.
    #[serde(skip)]
    pub(crate) about: About<'a>,
}

/// What an answer is about, for the first lines of the text.
#[derive(Debug)]
pub(crate) enum About<'a> {
    /// The component that `target` names.
    Component,
    /// A file, which `target` holds.
    File(String),
    Symbol(&'a Symbol),
    /// An import name that no component carries, given in `module`.
    ImportName,
}

/// How many import statements `impact` shows of each list, in text and
/// JSON alike; the rest is counted.
pub(crate) const MAX_IMPORT_SITES: usize = 5;

/// How many test files `impact` shows, in text and JSON alike; the rest is
/// counted.
pub(crate) const MAX_TEST_FILES: usize = 20;

#[derive(Debug, Serialize)]
pub struct TestFiles {
    pub total: usize,
    /// The first ones by path.
    pub shown: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ImportSites<'a> {
    /// False when no evidence names imported files for the file's language:
    /// the importers are unknown, not absent.
    pub recorded: bool,
    pub total: usize,
    pub shown: Vec<ImportSite<'a>>,
    /// How many of all the statements are re-exports, for the text.
    #[serde(skip)]
    pub(crate) exports: usize,
}

#[derive(Debug, Serialize)]
pub struct ImportSite<'a> {
    pub file: String,
    pub line: Option<u32>,
    /// The importing component, at the roll-up depth.
    pub component: ComponentId,
    /// The statement is test code.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub test: bool,
    /// The statement's evidence, for the marks of the text.
    #[serde(skip)]
    pub(crate) evidence: &'a Evidence,
}
