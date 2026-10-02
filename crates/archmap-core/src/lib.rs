//! Architecture Graph model and graph operations.
//!
//! This crate is the stable center of archmap. It knows nothing about
//! languages, files or manifests: analyzers in `archmap-scan` produce
//! [`GraphFragment`]s, and this crate normalizes them into an
//! [`ArchitectureGraph`] that CLI, MCP or other adapters can consume.
//!
//! Everything in the graph is *fact*. Semantic inference (for example "this
//! module belongs to the Billing component") is deliberately out of scope.
//! Declared architecture lives in [`rules`] and is only ever compared with
//! the graph, never merged into it.

pub mod co_change;
mod evidence;
mod fragment;
mod graph;
pub mod history;
mod model;
pub mod rules;
pub mod signals;
mod uses;

pub use evidence::{via_place, Evidence, Scope, WHOLE_MODULE};
pub use fragment::GraphFragment;
pub use graph::{ArchitectureGraph, ChangeSeed, FileFacts, GraphMeta, Hop, Reach, SymbolImporters};
pub use model::{
    Component, ComponentId, ComponentKind, DynamicImport, Edge, EdgeKind, LanguageCoverage, Symbol,
    SymbolId, SymbolKind, UnmappedImport, UnmappedReason, UnreadMacro,
};
pub use uses::{ImportPlace, Renamed, SymbolUse, SymbolUses, Unread, UnreadReason, UseRole};

/// Version of the JSON schema emitted by [`ArchitectureGraph`].
///
/// Bump when a breaking change is made to the serialized shape.
pub const SCHEMA_VERSION: u32 = 4;
