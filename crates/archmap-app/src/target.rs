//! Finding what a target names: a file or directory under the scanned root,
//! a component, a symbol. `query` and `impact` share these lookups.

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
/// the same for `query` and `impact`. `Err` when no component contains it.
pub(crate) fn directory_target<'a>(
    full: &'a ArchitectureGraph,
    root: &Path,
    target: &str,
) -> Option<Result<&'a Component>> {
    let relative = root_relative(root, target)?;
    root.join(&relative).is_dir().then(|| {
        full.component_for_path(&relative)
            .with_context(|| format!("no component contains `{target}`"))
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
