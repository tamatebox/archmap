use serde::{Deserialize, Serialize};

use crate::{
    Component, ComponentId, DynamicImport, Edge, Symbol, SymbolId, UnmappedImport, UnreadMacro,
};

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
    #[serde(default)]
    pub unread_macros: Vec<UnreadMacro>,
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
            && self.unread_macros.is_empty()
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

    pub fn push_unread_macro(&mut self, call: UnreadMacro) {
        self.unread_macros.push(call);
    }

    /// Append another fragment into this one without normalizing.
    pub fn extend(&mut self, other: GraphFragment) {
        self.components.extend(other.components);
        self.symbols.extend(other.symbols);
        self.edges.extend(other.edges);
        self.unmapped_imports.extend(other.unmapped_imports);
        self.dynamic_imports.extend(other.dynamic_imports);
        self.unread_macros.extend(other.unread_macros);
    }

    /// Give the component `from`, and every id under it (`from::...`), the
    /// id `to` instead, wherever this fragment names them: components and
    /// their parents, symbols and their components, edges, unmapped and
    /// dynamic imports, and unread macro calls. Evidence holds only paths and
    /// stays as it is.
    pub fn rename_component(&mut self, from: &ComponentId, to: &ComponentId) {
        let rename = |id: &mut ComponentId| {
            if let Some(renamed) = renamed(id.as_str(), from.as_str(), to.as_str()) {
                *id = ComponentId::new(renamed);
            }
        };
        for component in &mut self.components {
            rename(&mut component.id);
            if let Some(parent) = &mut component.parent {
                rename(parent);
            }
        }
        for symbol in &mut self.symbols {
            rename(&mut symbol.component);
            if let Some(renamed) = renamed(symbol.id.as_str(), from.as_str(), to.as_str()) {
                symbol.id = SymbolId::new(renamed);
            }
        }
        for edge in &mut self.edges {
            rename(&mut edge.from);
            rename(&mut edge.to);
        }
        for import in &mut self.unmapped_imports {
            rename(&mut import.from);
        }
        for import in &mut self.dynamic_imports {
            rename(&mut import.from);
        }
        for call in &mut self.unread_macros {
            rename(&mut call.from);
        }
    }
}

/// `id` with its leading `from` replaced by `to`, when `id` is `from` or an
/// id under it (`from::...`).
fn renamed(id: &str, from: &str, to: &str) -> Option<String> {
    if id == from {
        return Some(to.to_owned());
    }
    let rest = id.strip_prefix(from)?;
    rest.starts_with("::").then(|| format!("{to}{rest}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ComponentId, ComponentKind, EdgeKind, Evidence, SymbolId, SymbolKind, UnmappedReason,
    };

    #[test]
    fn rename_component_rewrites_the_id_and_every_id_under_it() {
        let mut fragment = GraphFragment::new();
        fragment.push_component(Component::new("app", "app", ComponentKind::Package));
        let mut lib = Component::new("app::lib", "lib", ComponentKind::Module);
        lib.parent = Some(ComponentId::new("app"));
        fragment.push_component(lib);
        fragment.push_component(Component::new(
            "application",
            "application",
            ComponentKind::Package,
        ));
        fragment.push_symbol(Symbol {
            id: SymbolId::new("app::lib::run"),
            name: "run".into(),
            kind: SymbolKind::Function,
            component: ComponentId::new("app::lib"),
            signature: None,
            evidence: vec![Evidence::new("lib/run.py").at_line(1)],
        });
        fragment.push_edge(Edge::new("app::lib", "application", EdgeKind::Import));
        fragment.push_unmapped_import(UnmappedImport {
            from: "app".into(),
            module: "requests".into(),
            reason: UnmappedReason::Undeclared,
            provided_by: Vec::new(),
            evidence: Evidence::new("main.py").at_line(1),
        });
        fragment.push_dynamic_import(DynamicImport {
            from: "app::lib".into(),
            call: "import_module".into(),
            evidence: Evidence::new("lib/run.py").at_line(2),
        });

        fragment.rename_component(&ComponentId::new("app"), &ComponentId::new("app+python"));

        let ids: Vec<&str> = fragment.components.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["app+python", "app+python::lib", "application"]);
        assert_eq!(
            fragment.components[1]
                .parent
                .as_ref()
                .map(ComponentId::as_str),
            Some("app+python")
        );
        assert_eq!(fragment.symbols[0].id.as_str(), "app+python::lib::run");
        assert_eq!(fragment.symbols[0].component.as_str(), "app+python::lib");
        assert_eq!(fragment.edges[0].from.as_str(), "app+python::lib");
        assert_eq!(fragment.edges[0].to.as_str(), "application");
        assert_eq!(fragment.unmapped_imports[0].from.as_str(), "app+python");
        assert_eq!(fragment.dynamic_imports[0].from.as_str(), "app+python::lib");
    }
}
