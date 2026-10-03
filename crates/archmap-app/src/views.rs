//! What `query` and `impact` return: built by [`crate::query`] and
//! [`crate::impact`], rendered as JSON there or as text by
//! [`crate::query_text`] and [`crate::impact_text`].

use archmap_core::co_change::CoChange;
use archmap_core::history::{HistoryState, Renames};
use archmap_core::{
    Component, ComponentId, DynamicImport, Edge, Evidence, Symbol, SymbolId, SymbolUses,
    UnmappedImport,
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
    /// For the entry file of a package that runs before its modules: the
    /// statements outside the package that import a module below it, which
    /// run it first, one edge per importing component.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub imports_below: Vec<Edge>,
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
    /// Where it is used, read from the files that define and import it;
    /// `None` for a language no uses pass reads yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub used_at: Option<SymbolUses>,
    /// Its kind's calls through a value are not read: a method that is not
    /// static.
    #[serde(skip)]
    pub instance_method: bool,
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
    /// Components that directly depend on the target, those with the most
    /// statements into it in production code first, ties by name.
    pub direct: Vec<Dependent>,
    /// Every component that transitively depends on the target, `direct`
    /// included, nearest first, ties by name.
    pub transitive: Vec<Dependent>,
    /// Files that reach the target only through test code, and a changed
    /// component's own test files: the tests to run again.
    pub tests: TestFiles,
    /// For a file, or a component that is one file: the statements that
    /// import the file directly. For a symbol: those that take its name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub importers: Option<ImportSites<'a>>,
    /// For the entry file of a package that runs before its modules: the
    /// statements outside the package that import a module below it, which
    /// run it first.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub imports_below: Option<ImportSites<'a>>,
    /// For a symbol: the statements that take its file whole.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub may_use: Option<ImportSites<'a>>,
    /// Files changed in the same commits as the target, from the committed
    /// history.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub co_change: Option<CoChangeSection<'a>>,
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

/// A component that depends on the target.
#[derive(Debug, Clone, Serialize)]
pub struct Dependent {
    pub id: ComponentId,
    /// The fewest steps from the target: 1 for a direct dependent.
    pub distance: usize,
    /// For a component that holds the target: the files of it the change
    /// reaches, nearest first.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    /// For a direct dependent: its statements that `importers`, `imports_below`
    /// and `may_use` list, counted in production code and in tests.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub imports: Option<Statements>,
    /// For one beyond the direct dependents: what the walk reached it
    /// through at that distance, a file its file imports or the id of a
    /// component it depends on as a whole.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub through: Option<String>,
    /// Where a manifest of its own declares the component of `through`,
    /// when that declaration was the way.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub declared_in: Option<Location>,
    /// Where a file of its own imports the component of `through` without
    /// naming a file of it, when that import was the way.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub imported_in: Option<Location>,
    /// `through` as the text writes it: a file's path, a component's name.
    #[serde(skip)]
    pub(crate) through_shown: Option<String>,
}

/// Where a statement is written.
#[derive(Debug, Clone, Serialize)]
pub struct Location {
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
}

/// Statements counted in production code and in tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Statements {
    pub production: usize,
    pub tests: usize,
}

#[derive(Debug, Serialize)]
pub struct TestFiles {
    pub total: usize,
    /// Every one, by path.
    pub files: Vec<TestFile>,
    /// The test files left out, which reach the target only through
    /// modules their mocks replace for their whole run.
    #[serde(skip_serializing_if = "LeftOut::is_empty")]
    pub left_out: LeftOut,
}

/// A test file to run again, with how it reaches the target.
#[derive(Debug, Serialize)]
pub struct TestFile {
    pub file: String,
    /// Every way at its fewest steps from the target, by precedence, then
    /// where none of those takes values, the nearest that do: the text
    /// shows the first that takes values.
    pub ways: Vec<TestRouteView>,
    /// No way of it takes values: its run loads none of what the change
    /// reaches.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub types_only: bool,
}

/// One way a test file reaches the target, with its steps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TestRouteView {
    #[serde(flatten)]
    pub way: TestWayView,
    pub steps: usize,
    /// The test's statements on it take types only.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub types_only: bool,
    /// The test's statements on it are calls that put a mock in place of
    /// the module.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub mock: bool,
}

/// What a way of a test file goes by.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TestWayView {
    /// It is the target, or a file of the target component.
    Target,
    /// A statement of it takes the target, through the re-export at `via`
    /// when barrels pass it on.
    Takes {
        #[serde(skip_serializing_if = "Option::is_none")]
        via: Option<String>,
    },
    /// A statement of it takes the target's module whole.
    Whole,
    /// It loads a module below the package whose entry `file` runs first.
    RunsFirst { file: String },
    /// Through other files, `file` the first on the way (a component's id
    /// where its package was).
    Through { file: String },
}

#[derive(Debug, Default, Serialize)]
pub struct LeftOut {
    pub total: usize,
    /// Every one, by path.
    pub files: Vec<MockingTest>,
}

impl LeftOut {
    fn is_empty(&self) -> bool {
        self.total == 0
    }
}

/// A test file, with its mocks that replace a module on the way.
#[derive(Debug, Serialize)]
pub struct MockingTest {
    pub file: String,
    pub mocks: Vec<MockCall>,
}

/// Where a mock is called, and the module it replaces.
#[derive(Debug, Serialize)]
pub struct MockCall {
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    pub target: String,
}

#[derive(Debug, Serialize)]
pub struct ImportSites<'a> {
    /// False when no evidence names imported files for the file's language:
    /// the importers are unknown, not absent.
    pub recorded: bool,
    pub total: usize,
    /// Every statement, production code first; the text shows the first.
    #[serde(rename = "statements")]
    pub shown: Vec<ImportSite<'a>>,
    /// How many of all the statements are re-exports, for the text.
    #[serde(skip)]
    pub(crate) exports: usize,
}

#[derive(Debug, Serialize)]
pub struct ImportSite<'a> {
    #[serde(skip)]
    pub file: String,
    #[serde(skip)]
    pub line: Option<u32>,
    /// The importing component, at the roll-up depth.
    pub component: ComponentId,
    /// The statement is test code.
    #[serde(skip)]
    pub test: bool,
    /// For a symbol: the barrel the statement reaches it through, a file
    /// that passes it on, when the statement takes that file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub through: Option<&'a str>,
    /// The statement's evidence, every field of it in JSON.
    #[serde(flatten)]
    pub(crate) evidence: &'a Evidence,
}

/// The files changed in the same commits as the target, and the history
/// they come from.
#[derive(Debug, Serialize)]
pub struct CoChangeSection<'a> {
    /// The history read, apart from its commits and files.
    pub history: HistoryCoverage<'a>,
    /// The target's paths at HEAD that the view starts from.
    pub target_paths: Vec<String>,
    /// The view; absent when the history was not read.
    #[serde(flatten)]
    pub view: Option<CoChange>,
    /// How the heading names the target.
    #[serde(skip)]
    pub(crate) label: String,
}

/// How the history was read, for Coverage.
#[derive(Debug, Serialize)]
pub struct HistoryCoverage<'a> {
    #[serde(flatten)]
    pub state: &'a HistoryState,
    /// The root's path from the repository's top.
    #[serde(skip_serializing_if = "str::is_empty")]
    pub prefix: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_version: Option<&'a str>,
    /// Commits asked for, and whether the read stopped there.
    pub bound: usize,
    pub reached_bound: bool,
    #[serde(flatten)]
    pub renames: &'a Renames,
    /// Paths that are not UTF-8, skipped.
    pub skipped_paths: usize,
}
