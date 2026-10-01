//! Ids that two analyzers give to different things.
//!
//! Analyzers choose component and symbol ids on their own, and the graph
//! merges equal ids. That is right when both describe one unit, such as a
//! maturin project with `Cargo.toml` and `pyproject.toml` side by side, and
//! wrong when the ids are equal by chance, such as a Cargo package and a
//! Python project of one name in different directories. Before a fragment
//! is merged, [`ContributedIds::separate`] renames such components, with
//! every id under them (`<id>::...`), to `<id>+<analyzer>`.

use std::collections::BTreeMap;

use archmap_core::{ComponentId, GraphFragment, SymbolId};

/// The analyzer that contributed each id so far, and where its component
/// lives.
#[derive(Debug, Default)]
pub(crate) struct ContributedIds {
    components: BTreeMap<ComponentId, (&'static str, Option<String>)>,
    symbols: BTreeMap<SymbolId, &'static str>,
}

impl ContributedIds {
    /// Rename what `fragment`, from `analyzer`, would wrongly merge into
    /// components and symbols of earlier analyzers, then record its ids.
    /// Returns one warning per rename.
    pub(crate) fn separate(
        &mut self,
        analyzer: &'static str,
        fragment: &mut GraphFragment,
    ) -> Vec<String> {
        let mut warnings = Vec::new();

        // A colliding component from another analyzer at another path, with
        // the paths of both sides, outermost first: renaming one renames
        // every id under it. Each side is judged by the path the merged
        // graph keeps, the first one given.
        let mut colliding: Vec<(ComponentId, &'static str, Option<String>, Option<String>)> =
            merged_paths(fragment)
                .into_iter()
                .filter_map(|(id, path)| {
                    let (other, other_path) = self.components.get(&id)?;
                    (*other != analyzer && *other_path != path)
                        .then(|| (id, *other, other_path.clone(), path))
                })
                .collect();
        colliding.sort();
        let mut renamed: Vec<ComponentId> = Vec::new();
        for (id, other, other_path, path) in colliding {
            if renamed.iter().any(|r| is_under(&id, r)) {
                continue;
            }
            let new = ComponentId::new(format!("{id}+{analyzer}"));
            fragment.rename_component(&id, &new);
            warnings.push(format!(
                "`{id}` is also the id of a {other} component at {}; \
                 the {analyzer} component at {} is renamed `{new}`",
                other_path.as_deref().unwrap_or("no path"),
                path.as_deref().unwrap_or("no path"),
            ));
            renamed.push(id);
        }

        for symbol in &mut fragment.symbols {
            let Some(other) = self
                .symbols
                .get(&symbol.id)
                .copied()
                .filter(|other| *other != analyzer)
            else {
                continue;
            };
            let new = SymbolId::new(format!("{}+{analyzer}", symbol.id));
            warnings.push(format!(
                "symbol `{}` is also a {other} symbol; the {analyzer} one is renamed `{new}`",
                symbol.id
            ));
            symbol.id = new;
        }

        for (id, path) in merged_paths(fragment) {
            let entry = self.components.entry(id).or_insert((analyzer, None));
            if entry.1.is_none() {
                entry.1 = path;
            }
        }
        for symbol in &fragment.symbols {
            self.symbols.entry(symbol.id.clone()).or_insert(analyzer);
        }
        warnings
    }
}

/// Each component id of `fragment` with the path the graph keeps for it
/// when merging: the first one given.
fn merged_paths(fragment: &GraphFragment) -> BTreeMap<ComponentId, Option<String>> {
    let mut paths: BTreeMap<ComponentId, Option<String>> = BTreeMap::new();
    for component in &fragment.components {
        let path = paths.entry(component.id.clone()).or_insert(None);
        if path.is_none() {
            path.clone_from(&component.path);
        }
    }
    paths
}

/// `id` is `ancestor` or an id under it (`ancestor::...`).
fn is_under(id: &ComponentId, ancestor: &ComponentId) -> bool {
    id == ancestor
        || id
            .as_str()
            .strip_prefix(ancestor.as_str())
            .is_some_and(|rest| rest.starts_with("::"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use archmap_core::{Component, ComponentKind, Symbol, SymbolKind};

    fn component(id: &str, path: Option<&str>) -> Component {
        let mut c = Component::new(id, id, ComponentKind::Package);
        c.path = path.map(str::to_owned);
        c
    }

    fn fragment(components: Vec<Component>) -> GraphFragment {
        GraphFragment {
            components,
            ..GraphFragment::default()
        }
    }

    fn ids(fragment: &GraphFragment) -> Vec<&str> {
        fragment.components.iter().map(|c| c.id.as_str()).collect()
    }

    #[test]
    fn a_different_path_renames_the_later_component_and_everything_under_it() {
        let mut seen = ContributedIds::default();
        assert!(seen
            .separate("rust", &mut fragment(vec![component("dup", Some("rs"))]))
            .is_empty());

        let mut module = component("dup::dup", Some("py/dup"));
        module.parent = Some("dup".into());
        let mut python = fragment(vec![component("dup", Some("py")), module]);
        let warnings = seen.separate("python", &mut python);

        assert_eq!(ids(&python), ["dup+python", "dup+python::dup"]);
        assert_eq!(
            python.components[1].parent.as_ref().map(|p| p.as_str()),
            Some("dup+python")
        );
        assert_eq!(
            warnings,
            ["`dup` is also the id of a rust component at rs; the python component at py is renamed `dup+python`"]
        );
    }

    #[test]
    fn the_same_path_keeps_merging() {
        let mut seen = ContributedIds::default();
        seen.separate("rust", &mut fragment(vec![component("mix", Some("both"))]));
        let mut python = fragment(vec![component("mix", Some("both"))]);
        assert!(seen.separate("python", &mut python).is_empty());
        assert_eq!(ids(&python), ["mix"]);
    }

    #[test]
    fn a_path_and_no_path_differ() {
        let mut seen = ContributedIds::default();
        seen.separate("rust", &mut fragment(vec![component("x", None)]));
        let mut python = fragment(vec![component("x", Some("x"))]);
        assert_eq!(seen.separate("python", &mut python).len(), 1);
        assert_eq!(ids(&python), ["x+python"]);
    }

    #[test]
    fn the_same_analyzer_is_never_renamed() {
        let mut seen = ContributedIds::default();
        seen.separate("python", &mut fragment(vec![component("a", Some("one"))]));
        let mut again = fragment(vec![component("a", Some("two"))]);
        assert!(seen.separate("python", &mut again).is_empty());
        assert_eq!(ids(&again), ["a"]);
    }

    #[test]
    fn duplicates_in_a_fragment_are_renamed_once() {
        let mut seen = ContributedIds::default();
        seen.separate("rust", &mut fragment(vec![component("ext:cargo:x", None)]));
        // The Python analyzer pushes an external component once per
        // declaration; here a made-up collision with a path.
        let mut python = fragment(vec![
            component("ext:cargo:x", Some("p")),
            component("ext:cargo:x", Some("p")),
        ]);
        let warnings = seen.separate("python", &mut python);
        assert_eq!(ids(&python), ["ext:cargo:x+python", "ext:cargo:x+python"]);
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn a_component_is_judged_by_the_path_it_ends_up_with() {
        // The graph keeps the first path a component is given, as a fragment
        // can name an id without a path before naming it with one.
        let mut seen = ContributedIds::default();
        seen.separate("rust", &mut fragment(vec![component("x", Some("p"))]));
        let mut later = fragment(vec![component("x", None), component("x", Some("p"))]);
        assert!(seen.separate("python", &mut later).is_empty());
        assert_eq!(ids(&later), ["x", "x"]);

        // Recorded the same way: a later analyzer is compared with `p`.
        let mut first = ContributedIds::default();
        first.separate(
            "rust",
            &mut fragment(vec![component("y", None), component("y", Some("q"))]),
        );
        let mut same_place = fragment(vec![component("y", Some("q"))]);
        assert!(first.separate("python", &mut same_place).is_empty());
    }

    #[test]
    fn symbols_that_collide_on_their_own_are_renamed() {
        let symbol = |component: &str| Symbol {
            id: "app::lib::run".into(),
            name: "run".into(),
            kind: SymbolKind::Function,
            component: component.into(),
            signature: None,
            evidence: Vec::new(),
        };
        let mut seen = ContributedIds::default();
        let mut rust = fragment(vec![component("app::lib", Some("app/src/lib.rs"))]);
        rust.symbols.push(symbol("app::lib"));
        seen.separate("rust", &mut rust);

        let mut python = fragment(vec![component("app", Some("app"))]);
        python.symbols.push(symbol("app"));
        let warnings = seen.separate("python", &mut python);

        assert_eq!(python.symbols[0].id.as_str(), "app::lib::run+python");
        assert_eq!(
            warnings,
            ["symbol `app::lib::run` is also a rust symbol; the python one is renamed `app::lib::run+python`"]
        );
    }
}
