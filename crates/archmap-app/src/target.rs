//! Finding what a target names: a file or directory under the scanned root,
//! a component, a symbol. `query` and `impact` share these lookups.

use std::fmt::Write as _;
use std::path::{Component as PathPart, Path};

use anyhow::{bail, Context, Result};
use archmap_core::{ArchitectureGraph, Component, ComponentId, Symbol, SymbolId};

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
fn owner_of_shared_path<'a>(
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

/// Components listed when a name is shared; the rest are counted.
const MAX_CANDIDATES: usize = 10;

/// Stop when `target` is no component id but the name of several
/// components, listing their ids and paths. It runs on the full graph
/// before any lookup, since roll-up can leave only one of them visible.
pub(crate) fn reject_ambiguous(full: &ArchitectureGraph, target: &str) -> Result<()> {
    if full.component(&ComponentId::new(target)).is_some() {
        return Ok(());
    }
    let named: Vec<&Component> = full.components_named(target).collect();
    if named.len() < 2 || owner_of_shared_path(full, &named).is_some() {
        return Ok(());
    }
    let mut message = format!(
        "`{target}` names {} components; give an id, or a path as ./<path>:",
        named.len()
    );
    for component in named.iter().take(MAX_CANDIDATES) {
        let _ = write!(
            message,
            "\n  {}  {}",
            crate::query_text::shell_word(component.id.as_str()),
            component.path.as_deref().unwrap_or("-")
        );
    }
    if named.len() > MAX_CANDIDATES {
        let _ = write!(message, "\n  +{} more", named.len() - MAX_CANDIDATES);
    }
    bail!(message)
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

/// The one symbol `target` names, by id or by name; an error that lists the
/// candidates when several share the name.
pub(crate) fn symbols_for<'a>(full: &'a ArchitectureGraph, target: &str) -> Vec<&'a Symbol> {
    match full.symbol(&SymbolId::new(target)) {
        Some(symbol) => vec![symbol],
        None => full.symbols_named(target).collect(),
    }
}

/// The error for a name that several symbols share: their ids, quoted for
/// the shell where needed, and where they are.
pub(crate) fn ambiguous_symbols(target: &str, symbols: &[&Symbol]) -> String {
    let mut message = format!("`{target}` names {} symbols; give an id:", symbols.len());
    for symbol in symbols.iter().take(MAX_CANDIDATES) {
        let at = symbol
            .location()
            .map(|e| format!("{}:{}", e.file, e.line.unwrap_or(0)))
            .unwrap_or_default();
        let _ = write!(
            message,
            "\n  {}  {at}",
            crate::query_text::shell_word(symbol.id.as_str())
        );
    }
    if symbols.len() > MAX_CANDIDATES {
        let _ = write!(message, "\n  +{} more", symbols.len() - MAX_CANDIDATES);
    }
    message
}
