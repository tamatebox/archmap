//! Finding what a target names: a file or directory under the scanned root,
//! a component, a symbol. `query` and `impact` share these lookups.

use std::collections::BTreeSet;
use std::path::{Component as PathPart, Path};

use anyhow::{bail, Context, Result};
use archmap_core::{ArchitectureGraph, Component, ComponentId};

/// The target without one pair of matching quotes around it, so an id
/// copied from a shell-quoted candidate works where no shell removes them.
pub(crate) fn unquote(target: &str) -> &str {
    for quote in ['\'', '"'] {
        if let Some(inner) = target
            .strip_prefix(quote)
            .and_then(|t| t.strip_suffix(quote))
        {
            return inner;
        }
    }
    target
}

/// `target` as a file under the scanned root, relative with `/` separators.
pub(crate) fn file_target(root: &Path, target: &str) -> Option<String> {
    let relative = root_relative(root, target)?;
    root.join(&relative).is_file().then_some(relative)
}

/// `target` as a path relative to the scanned root, with `/` separators: an
/// absolute path inside the root without the root, `.` and `..` resolved.
/// `None` for a path outside the root, or an absolute one that does not
/// exist.
fn root_relative(root: &Path, target: &str) -> Option<String> {
    let given = Path::new(target);
    let relative = if given.is_absolute() {
        let root = std::fs::canonicalize(root).ok()?;
        std::fs::canonicalize(given)
            .ok()?
            .strip_prefix(&root)
            .ok()?
            .to_path_buf()
    } else {
        given.to_path_buf()
    };
    let mut parts: Vec<String> = Vec::new();
    for part in relative.components() {
        match part {
            PathPart::CurDir => {}
            PathPart::ParentDir => {
                parts.pop()?;
            }
            PathPart::Normal(name) => parts.push(name.to_string_lossy().into_owned()),
            PathPart::RootDir | PathPart::Prefix(_) => return None,
        }
    }
    Some(parts.join("/"))
}

/// Stop when `target` is a path outside the scanned root that exists: the
/// graph holds nothing of it, whatever component its words would match.
pub fn reject_outside(root: &Path, target: &str) -> Result<()> {
    let target = unquote(target);
    let given = Path::new(target);
    let exists = if given.is_absolute() {
        given.exists()
    } else {
        root.join(given).exists()
    };
    if exists && root_relative(root, target).is_none() {
        bail!(
            "`{target}` is outside the scanned root `{}`",
            root.display()
        );
    }
    Ok(())
}

/// The file a component stands for: its path, when that is a file under
/// the scanned root and no component is inside it (a TS/JS file, a Rust
/// module without submodules). `query` and `impact` answer for such a
/// component as for its file, even where it folds into an ancestor.
pub(crate) fn component_file(
    full: &ArchitectureGraph,
    root: &Path,
    component: &Component,
) -> Option<String> {
    let has_children = full
        .components
        .values()
        .any(|c| c.parent.as_ref() == Some(&component.id));
    if has_children {
        return None;
    }
    file_target(root, component.path.as_deref()?)
}

/// The component that owns `target` as a directory under the scanned root,
/// the same for `query` and `impact`: the one whose path it is, else the
/// one it is the directory of (`module_of_directory`), else the one that
/// contains it. `Err` when no component contains it.
pub(crate) fn directory_target<'a>(
    full: &'a ArchitectureGraph,
    root: &Path,
    target: &str,
) -> Option<Result<&'a Component>> {
    let relative = root_relative(root, target)?;
    root.join(&relative).is_dir().then(|| {
        module_of_directory(full, root, &relative)
            .or_else(|| full.component_for_path(&relative))
            .with_context(|| format!("no component contains `{target}`"))
    })
}

/// The component whose files `dir` holds although its path is a file: the
/// one whose file sits beside `dir` under its name (a Rust `billing.rs` for
/// `billing/`), else the one whose file sits in `dir` and holds the other
/// components in it (a Rust `rust/mod.rs`). Every file the graph records in
/// `dir` must be that component's or below it. `None` when a component has
/// `dir` for its path, or none fits.
fn module_of_directory<'a>(
    full: &'a ArchitectureGraph,
    root: &Path,
    dir: &str,
) -> Option<&'a Component> {
    let path = |c: &'a Component| c.path.as_deref();
    if dir.is_empty() || full.components.values().any(|c| path(c) == Some(dir)) {
        return None;
    }
    let prefix = format!("{dir}/");
    let recorded: BTreeSet<&str> = full
        .components
        .values()
        .filter_map(path)
        .chain(
            full.edges
                .iter()
                .flat_map(|e| &e.evidence)
                .map(|e| e.file.as_str()),
        )
        .chain(
            full.symbols
                .values()
                .flat_map(|s| &s.evidence)
                .map(|e| e.file.as_str()),
        )
        .filter(|file| file.starts_with(&prefix))
        .collect();
    // every recorded file in `dir` is the candidate's or below it
    let holds_all = |candidate: &Component| {
        recorded.iter().all(|file| {
            full.component_for_path(file)
                .is_some_and(|owner| full.containment_path(&owner.id).contains(&candidate.id))
        })
    };
    let beside: Vec<&Component> = full
        .components
        .values()
        .filter(|c| {
            path(c)
                .and_then(|p| p.strip_prefix(dir)?.strip_prefix('.'))
                .is_some_and(|extension| !extension.is_empty() && !extension.contains('/'))
        })
        .collect();
    if let [only] = beside.as_slice() {
        return holds_all(only).then_some(*only);
    }
    let inside: Vec<&Component> = full
        .components
        .values()
        .filter(|c| path(c).is_some_and(|p| p.starts_with(&prefix)))
        .collect();
    inside.iter().copied().find(|head| {
        let file = path(head).unwrap_or_default();
        !file[prefix.len()..].contains('/')
            && root.join(file).is_file()
            && inside.iter().any(|c| c.parent.as_ref() == Some(&head.id))
            && holds_all(head)
    })
}

/// A component as seen at a roll-up depth.
pub(crate) struct AtDepth {
    pub(crate) id: ComponentId,
    pub(crate) folded_from: Option<ComponentId>,
}

/// A component as seen at `depth`: the ancestor it folds into, if any.
pub(crate) fn fold(full: &ArchitectureGraph, depth: usize, id: &ComponentId) -> AtDepth {
    let ancestor = full.ancestor_at(id, depth);
    let folded_from = (ancestor != *id).then(|| id.clone());
    AtDepth {
        id: ancestor,
        folded_from,
    }
}

/// Exact id first, then a unique match on the display name. Components
/// that share a name and a path, one directory that two analyzers see,
/// resolve to the one that owns the path in `full`: roll-up moves evidence
/// to ancestors, so only the full graph decides the owner.
pub(crate) fn find_component<'a>(
    graph: &'a ArchitectureGraph,
    full: &ArchitectureGraph,
    target: &str,
) -> Option<&'a Component> {
    graph.component(&ComponentId::new(target)).or_else(|| {
        let named: Vec<&Component> = graph.components_named(target).collect();
        match named.as_slice() {
            [] => None,
            [only] => Some(*only),
            several => {
                owner_of_shared_path(full, several).and_then(|owner| graph.component(&owner.id))
            }
        }
    })
}

/// The component of `full` that owns the path all of `named` share, if
/// they share one.
pub(crate) fn owner_of_shared_path<'a>(
    full: &'a ArchitectureGraph,
    named: &[&Component],
) -> Option<&'a Component> {
    let path = named.first()?.path.as_deref()?;
    if named.iter().any(|c| c.path.as_deref() != Some(path)) {
        return None;
    }
    full.component_for_path(path)
        .filter(|owner| named.iter().any(|c| c.id == owner.id))
}

/// Other components that share `component`'s name, and those that share
/// its path: an id or a path answered for one of them, and theirs pick the
/// others.
pub(crate) fn namesakes<'a>(
    full: &'a ArchitectureGraph,
    component: &Component,
) -> (Vec<&'a ComponentId>, Vec<&'a ComponentId>) {
    let at_path: Vec<&ComponentId> = full
        .components
        .values()
        .filter(|c| c.id != component.id && c.path.is_some() && c.path == component.path)
        .map(|c| &c.id)
        .collect();
    let named = full
        .components_named(&component.name)
        .filter(|c| c.id != component.id && !at_path.contains(&&c.id))
        .map(|c| &c.id)
        .collect();
    (named, at_path)
}

/// The files among `files` that are test code: those where everything
/// recorded (imports, with an edge or without, dynamic imports, the symbols
/// a file defines) carries the `test` mark its analyzer gave it, as `impact`
/// tells them apart. A file with nothing recorded goes by the analyzers'
/// shared path rule.
pub(crate) fn test_files<'a>(
    full: &ArchitectureGraph,
    files: impl IntoIterator<Item = &'a str>,
) -> BTreeSet<&'a str> {
    let marks = full.test_code();
    files
        .into_iter()
        .filter(|file| {
            marks
                .get(file)
                .copied()
                .unwrap_or_else(|| archmap_scan::is_test_code(Path::new(file)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use archmap_core::{
        ArchitectureGraph, ComponentId, DynamicImport, Edge, EdgeKind, Evidence, UnmappedImport,
        UnmappedReason,
    };

    use super::test_files;

    #[test]
    fn a_file_is_test_code_when_every_statement_recorded_in_it_is() {
        let statement = |file: &str, test: bool| Evidence::new(file).at_line(1).in_test(test);
        let full = ArchitectureGraph {
            edges: vec![
                Edge::new("a", "b", EdgeKind::Import)
                    .with_evidence(statement("tests/mixed.ts", true)),
                Edge::new("a", "c", EdgeKind::Import)
                    .with_evidence(statement("tests/mixed.ts", false)),
                Edge::new("a", "b", EdgeKind::Import).with_evidence(statement("src/all.ts", true)),
            ],
            unmapped_imports: vec![UnmappedImport {
                from: ComponentId::new("a"),
                module: "left-pad".into(),
                reason: UnmappedReason::Undeclared,
                provided_by: vec![],
                evidence: statement("tests/unmapped.ts", false),
            }],
            dynamic_imports: vec![DynamicImport {
                from: ComponentId::new("a"),
                call: "import".into(),
                evidence: statement("app/test/page.tsx", false),
            }],
            ..Default::default()
        };
        let files = [
            "tests/mixed.ts",
            "src/all.ts",
            "tests/unmapped.ts",
            "app/test/page.tsx",
            "tests/none.ts",
            "src/none.ts",
        ];
        // a statement in production code makes its file production code;
        // a file with none goes by its path
        assert_eq!(
            test_files(&full, files),
            BTreeSet::from(["src/all.ts", "tests/none.ts"])
        );
    }
}
