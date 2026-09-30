use serde::{Deserialize, Serialize};

use crate::{Component, Edge, Symbol};

/// A partial graph produced by a single analyzer.
///
/// Fragments are unindexed and may contain duplicates; the
/// [`ArchitectureGraph`](crate::ArchitectureGraph) is responsible for
/// normalizing them when merging.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphFragment {
    #[serde(default)]
    pub components: Vec<Component>,
    #[serde(default)]
    pub symbols: Vec<Symbol>,
    #[serde(default)]
    pub edges: Vec<Edge>,
}

impl GraphFragment {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.components.is_empty() && self.symbols.is_empty() && self.edges.is_empty()
    }

    pub fn push_component(&mut self, component: Component) {
        self.components.push(component);
    }

    pub fn push_symbol(&mut self, symbol: Symbol) {
        self.symbols.push(symbol);
    }

    pub fn push_edge(&mut self, edge: Edge) {
        self.edges.push(edge);
    }

    /// Append another fragment into this one without normalizing.
    pub fn extend(&mut self, other: GraphFragment) {
        self.components.extend(other.components);
        self.symbols.extend(other.symbols);
        self.edges.extend(other.edges);
    }
}
