use serde::{Deserialize, Serialize};

use crate::{Component, DynamicImport, Edge, Symbol, UnmappedImport};

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
    #[serde(default)]
    pub unmapped_imports: Vec<UnmappedImport>,
    #[serde(default)]
    pub dynamic_imports: Vec<DynamicImport>,
}

impl GraphFragment {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.components.is_empty()
            && self.symbols.is_empty()
            && self.edges.is_empty()
            && self.unmapped_imports.is_empty()
            && self.dynamic_imports.is_empty()
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

    pub fn push_unmapped_import(&mut self, import: UnmappedImport) {
        self.unmapped_imports.push(import);
    }

    pub fn push_dynamic_import(&mut self, import: DynamicImport) {
        self.dynamic_imports.push(import);
    }

    /// Append another fragment into this one without normalizing.
    pub fn extend(&mut self, other: GraphFragment) {
        self.components.extend(other.components);
        self.symbols.extend(other.symbols);
        self.edges.extend(other.edges);
        self.unmapped_imports.extend(other.unmapped_imports);
        self.dynamic_imports.extend(other.dynamic_imports);
    }
}
