//! Architecture Graph model and graph operations.
//!
//! This crate is the stable center of archmap. It knows nothing about
//! languages, files or manifests: analyzers in `archmap-scan` produce
//! [`GraphFragment`]s, and this crate normalizes them into an
//! [`ArchitectureGraph`] that CLI, MCP or other adapters can consume.
//!
//! Everything here is *fact*. Semantic inference (for example "this module
//! belongs to the Billing component") is deliberately out of scope for now.

mod evidence;
mod fragment;
mod graph;
mod model;

pub use evidence::Evidence;
pub use fragment::GraphFragment;
pub use graph::{ArchitectureGraph, GraphMeta};
pub use model::{
    Component, ComponentId, ComponentKind, Edge, EdgeKind, Symbol, SymbolId, SymbolKind,
};

/// Version of the JSON schema emitted by [`ArchitectureGraph`].
///
/// Bump when a breaking change is made to the serialized shape.
pub const SCHEMA_VERSION: u32 = 1;
