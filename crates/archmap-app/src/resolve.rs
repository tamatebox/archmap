//! One lookup for `query` and `impact`: what a target names, in one order
//! for both, and every match when it names several things.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::Path;

use anyhow::{bail, Result};
use archmap_core::{
    ArchitectureGraph, Component, ComponentId, ComponentKind, Symbol, SymbolId, SymbolKind,
};
use serde::Serialize;

use crate::query_text::{component_kind, shell_word, symbol_kind};
use crate::target::{directory_target, file_target, find_component, owner_of_shared_path};
use crate::Format;

/// What a target names.
pub(crate) enum Resolved<'g> {
    Component(&'g Component),
    /// A file under the root, relative with `/` separators.
    File(String),
    Symbol(&'g Symbol),
    /// A package and the subpath after its name (`react-dom/client`).
    Package {
        component: &'g Component,
        subpath: String,
    },
    /// An import name that no component carries.
    ImportName(String),
    Candidates(Candidates<'g>),
}

/// Everything a target names when it names several things, by kind.
#[derive(Default)]
pub(crate) struct Candidates<'g> {
    components: Vec<&'g Component>,
    symbols: Vec<&'g Symbol>,
    files: Vec<String>,
    directories: Vec<String>,
}

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

/// Resolve `target` (already unquoted and inside the root): the first kind
/// that matches decides; several matches of it give every match of every
/// kind as candidates.
pub(crate) fn resolve<'g>(
    full: &'g ArchitectureGraph,
    rolled: &'g ArchitectureGraph,
    root: &Path,
    target: &str,
) -> Result<Resolved<'g>> {
    // a path written as one: the file, or the component owning the directory
    if written_as_path(target) {
        if let Some(file) = file_target(root, target) {
            return Ok(Resolved::File(file));
        }
        if let Some(owner) = directory_target(full, root, target).transpose()? {
            return Ok(Resolved::Component(owner));
        }
    }

    // ids; a module's own symbol is that module, any other symbol of the
    // same id is another thing
    let component = full.component(&ComponentId::new(target));
    let symbol = full
        .symbol(&SymbolId::new(target))
        .filter(|s| !(component.is_some() && s.kind == SymbolKind::Module));
    match (component, symbol) {
        (Some(component), Some(symbol)) => {
            return Ok(Resolved::Candidates(Candidates {
                components: vec![component],
                symbols: vec![symbol],
                ..Candidates::default()
            }))
        }
        (Some(component), None) => return Ok(Resolved::Component(component)),
        (None, Some(symbol)) => return Ok(Resolved::Symbol(symbol)),
        (None, None) => {}
    }

    let named: Vec<&Component> = full.components_named(target).collect();
    if named.len() > 1 && owner_of_shared_path(full, &named).is_none() {
        return Ok(Resolved::Candidates(every_match(full, root, target)?));
    }
    if let Some(component) =
        find_component(rolled, full, target).or_else(|| find_component(full, full, target))
    {
        // a name with `/` that is also a path under the root naming another
        // file or directory
        if let Some(other) = other_path(root, target, component) {
            let mut candidates = Candidates {
                components: vec![component],
                ..Candidates::default()
            };
            other.add_to(&mut candidates);
            return Ok(Resolved::Candidates(candidates));
        }
        return Ok(Resolved::Component(component));
    }

    if let Some(file) = file_target(root, target) {
        return Ok(Resolved::File(file));
    }

    let symbols: Vec<&Symbol> = full.symbols_named(target).collect();
    match symbols.as_slice() {
        [] => {}
        [only] => return Ok(Resolved::Symbol(only)),
        _ => return Ok(Resolved::Candidates(every_match(full, root, target)?)),
    }

    if let Some(file) = full.file_for_dotted_name(target) {
        return Ok(Resolved::File(file.to_owned()));
    }

    // late: every directory has an owner, the root at worst, so a bare
    // word naming one must not shadow a symbol
    if let Some(owner) = directory_target(full, root, target).transpose()? {
        return Ok(Resolved::Component(owner));
    }

    if let Some((name, subpath)) = package_and_subpath(target) {
        let packages: Vec<&Component> = full
            .components_named(name)
            .filter(|c| takes_subpaths(c))
            .collect();
        match packages.as_slice() {
            [] => {}
            [component] => {
                return Ok(Resolved::Package {
                    component,
                    subpath: subpath.to_owned(),
                })
            }
            several => {
                return Ok(Resolved::Candidates(Candidates {
                    components: several.to_vec(),
                    ..Candidates::default()
                }))
            }
        }
    }

    if full.unmapped_imports_of(target).next().is_some() {
        return Ok(Resolved::ImportName(target.to_owned()));
    }

    let files = files_named(full, target);
    match files.as_slice() {
        [] => {}
        [only] => return Ok(Resolved::File(only.clone())),
        _ => return Ok(Resolved::Candidates(every_match(full, root, target)?)),
    }

    bail!("no component, file, symbol or import named `{target}`")
}

/// `./x`, `../x` or an absolute path: a path whatever names it also matches.
fn written_as_path(target: &str) -> bool {
    target == "."
        || target == ".."
        || target.starts_with("./")
        || target.starts_with("../")
        || Path::new(target).is_absolute()
}

/// A file or directory under the root at `target`, when it is no part of
/// `component`, which the same words name.
enum OtherPath {
    File(String),
    Directory(String),
}

impl OtherPath {
    fn add_to(self, candidates: &mut Candidates) {
        match self {
            OtherPath::File(file) => candidates.files.push(file),
            OtherPath::Directory(dir) => candidates.directories.push(dir),
        }
    }
}

fn other_path(root: &Path, target: &str, component: &Component) -> Option<OtherPath> {
    if !target.contains('/') {
        return None;
    }
    let path = target.trim_end_matches('/');
    if component.path.as_deref() == Some(path) {
        return None;
    }
    let at = root.join(path);
    if at.is_file() {
        Some(OtherPath::File(path.to_owned()))
    } else if at.is_dir() {
        Some(OtherPath::Directory(path.to_owned()))
    } else {
        None
    }
}

/// Whether an import names files below `component` by subpath, as npm
/// packages do (`react-dom/client`, `@acme/ui/button`); a Python package's
/// files are paths, never subpaths.
fn takes_subpaths(component: &Component) -> bool {
    match component.kind {
        ComponentKind::External => component.id.as_str().starts_with("ext:npm:"),
        ComponentKind::Package => {
            matches!(
                component.language.as_deref(),
                Some("typescript" | "javascript")
            )
        }
        _ => false,
    }
}

/// `react-dom/client` -> (`react-dom`, `client`); `@scope/pkg/sub` ->
/// (`@scope/pkg`, `sub`). `None` without a subpath.
fn package_and_subpath(target: &str) -> Option<(&str, &str)> {
    let split = if target.starts_with('@') {
        let first = target.find('/')?;
        first + 1 + target[first + 1..].find('/')?
    } else {
        target.find('/')?
    };
    let (name, rest) = target.split_at(split);
    let subpath = &rest[1..];
    (!name.is_empty() && !subpath.is_empty()).then_some((name, subpath))
}

/// Files the graph knows whose name or stem is `target` (`actions.ts`,
/// `actions`), sorted.
fn files_named(full: &ArchitectureGraph, target: &str) -> Vec<String> {
    if target.is_empty() || target.contains('/') {
        return Vec::new();
    }
    let mut files: Vec<String> = full
        .known_files()
        .into_iter()
        .filter(|file| {
            let name = file.rsplit('/').next().unwrap_or(file);
            let stem = name.split_once('.').map_or(name, |(stem, _)| stem);
            name == target || stem == target
        })
        .map(str::to_owned)
        .collect();
    production_first(&mut files);
    files
}

/// Production files before test files, each by path, so a capped list of
/// candidates shows the code first.
fn production_first(files: &mut [String]) {
    files.sort_by(|a, b| {
        let test = |f: &str| archmap_scan::is_test_code(Path::new(f));
        (test(a), a).cmp(&(test(b), b))
    });
}

/// Every match of every kind, for a target whose deciding kind has several.
fn every_match<'g>(
    full: &'g ArchitectureGraph,
    root: &Path,
    target: &str,
) -> Result<Candidates<'g>> {
    let mut files: BTreeSet<String> = files_named(full, target).into_iter().collect();
    files.extend(file_target(root, target));
    files.extend(full.file_for_dotted_name(target).map(str::to_owned));
    let directory = directory_target(full, root, target)
        .is_some()
        .then(|| target.trim_end_matches('/').to_owned());
    let mut files: Vec<String> = files.into_iter().collect();
    production_first(&mut files);
    Ok(Candidates {
        components: full.components_named(target).collect(),
        symbols: full.symbols_named(target).collect(),
        files,
        directories: directory.into_iter().collect(),
    })
}

/// Candidates listed in text; JSON lists all of them.
const MAX_CANDIDATES: usize = 10;

impl Candidates<'_> {
    fn total(&self) -> usize {
        self.components.len() + self.symbols.len() + self.files.len() + self.directories.len()
    }

    /// The candidates, in text or JSON, the same for `query` and `impact`.
    pub(crate) fn render(
        &self,
        full: &ArchitectureGraph,
        target: &str,
        format: Format,
    ) -> Result<String> {
        match format {
            Format::Json => crate::json(&CandidatesView {
                requested: target,
                total: self.total(),
                candidates: self.views(full),
            }),
            Format::Text => Ok(self.text(full, target)),
        }
    }

    fn text(&self, full: &ArchitectureGraph, target: &str) -> String {
        let mut kinds = Vec::new();
        for (n, one, many) in [
            (self.components.len(), "a component", "components"),
            (self.symbols.len(), "a symbol", "symbols"),
            (self.files.len(), "a file", "files"),
            (self.directories.len(), "a directory", "directories"),
        ] {
            match n {
                0 => {}
                1 => kinds.push(one.to_owned()),
                n => kinds.push(format!("{n} {many}")),
            }
        }
        let names = match kinds.split_last() {
            Some((last, [])) => last.clone(),
            Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
            None => String::new(),
        };
        let mut out = format!("`{target}` names {names}; query one of them by id or path:\n");
        let mut lines = Vec::new();
        for c in &self.components {
            lines.push(format!(
                "  {}  {}  {}",
                shell_word(c.id.as_str()),
                c.path.as_deref().unwrap_or("-"),
                component_kind(c.kind)
            ));
        }
        for s in &self.symbols {
            let at = s
                .location()
                .map(|e| match e.line {
                    Some(line) => format!("{}:{line}", e.file),
                    None => e.file.clone(),
                })
                .unwrap_or_default();
            let mut line = format!(
                "  {}  {at}  {}",
                shell_word(s.id.as_str()),
                symbol_kind(s.kind)
            );
            if let Some((by_name, may_use)) = importer_counts(full, s) {
                let _ = write!(line, "  imported by {by_name}, may use {may_use}");
            }
            lines.push(line);
        }
        for f in &self.files {
            lines.push(format!("  {}  file", shell_word(f)));
        }
        for d in &self.directories {
            lines.push(format!("  {}  directory", shell_word(&format!("./{d}"))));
        }
        for line in lines.iter().take(MAX_CANDIDATES) {
            let _ = writeln!(out, "{line}");
        }
        if lines.len() > MAX_CANDIDATES {
            let _ = writeln!(out, "  +{} more", lines.len() - MAX_CANDIDATES);
        }
        out
    }

    fn views(&self, full: &ArchitectureGraph) -> Vec<CandidateView<'_>> {
        let blank = CandidateView {
            kind: "",
            symbol_kind: None,
            id: None,
            path: None,
            file: None,
            line: None,
            imported_by: None,
            may_use: None,
        };
        let components = self.components.iter().map(|c| CandidateView {
            kind: "component",
            id: Some(c.id.as_str()),
            path: c.path.as_deref(),
            ..blank
        });
        let symbols = self.symbols.iter().map(|s| {
            let counts = importer_counts(full, s);
            CandidateView {
                kind: "symbol",
                symbol_kind: Some(symbol_kind(s.kind)),
                id: Some(s.id.as_str()),
                file: s.location().map(|e| e.file.as_str()),
                line: s.location().and_then(|e| e.line),
                imported_by: counts.map(|c| c.0),
                may_use: counts.map(|c| c.1),
                ..blank
            }
        });
        let files = self.files.iter().map(|f| CandidateView {
            kind: "file",
            path: Some(f.as_str()),
            ..blank
        });
        let directories = self.directories.iter().map(|d| CandidateView {
            kind: "directory",
            path: Some(d.as_str()),
            ..blank
        });
        components
            .chain(symbols)
            .chain(files)
            .chain(directories)
            .collect()
    }
}

/// What `query` and `impact` return when a target names several things.
#[derive(Serialize)]
struct CandidatesView<'a> {
    requested: &'a str,
    total: usize,
    candidates: Vec<CandidateView<'a>>,
}

#[derive(Clone, Copy, Serialize)]
struct CandidateView<'a> {
    kind: &'static str,
    /// For a symbol: what it is (`function`, `struct`, ...).
    #[serde(skip_serializing_if = "Option::is_none")]
    symbol_kind: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    file: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<u32>,
    /// For a symbol: how many statements take its name, and how many its
    /// whole module, where its language records names.
    #[serde(skip_serializing_if = "Option::is_none")]
    imported_by: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    may_use: Option<usize>,
}

/// The statements that take `symbol` by name and those that take its file
/// whole, counted as `query` lists them; `None` where names are unknown.
fn importer_counts(full: &ArchitectureGraph, symbol: &Symbol) -> Option<(usize, usize)> {
    full.symbol_importers(symbol)
        .filter(|i| i.recorded)
        .map(|i| (i.by_name.len(), i.may_use.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_pair_of_matching_quotes_is_dropped() {
        assert_eq!(unquote("'a::b'"), "a::b");
        assert_eq!(unquote("\"a\""), "a");
        assert_eq!(unquote("'a\""), "'a\"");
        assert_eq!(unquote("''a''"), "'a'");
        assert_eq!(unquote("'"), "'");
    }

    #[test]
    fn a_package_name_and_its_subpath_are_split_at_the_right_slash() {
        assert_eq!(
            package_and_subpath("react-dom/client"),
            Some(("react-dom", "client"))
        );
        assert_eq!(
            package_and_subpath("@acme/ui/button/x"),
            Some(("@acme/ui", "button/x"))
        );
        assert_eq!(package_and_subpath("@acme/ui"), None);
        assert_eq!(package_and_subpath("react"), None);
        assert_eq!(package_and_subpath("react/"), None);
    }
}
