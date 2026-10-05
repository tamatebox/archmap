//! One lookup for `query` and `impact`: what a target names, in one order
//! for both, and every match when it names several things.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::Path;

use anyhow::{bail, Result};
use archmap_core::{
    ArchitectureGraph, Component, ComponentId, ComponentKind, Evidence, Symbol, SymbolId,
    SymbolKind,
};
use serde::Serialize;

use crate::query_text::{component_kind, count, shell_word, symbol_kind};
use crate::target::{
    component_file, directory_target, file_target, find_component, owner_of_shared_path, test_files,
};
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

/// Everything a target names when it names several things, by kind; or,
/// when it names nothing, the names that contain it.
#[derive(Default)]
pub(crate) struct Candidates<'g> {
    components: Vec<&'g Component>,
    /// Components whose name ends in the target as its last segment.
    segments: Vec<&'g Component>,
    symbols: Vec<&'g Symbol>,
    files: Vec<String>,
    directories: Vec<String>,
    /// Names that contain the target, ignoring case, best first, for a
    /// target that names nothing.
    contains: Vec<Near<'g>>,
}

/// A name that contains a target which names nothing, and how.
pub(crate) struct Near<'g> {
    thing: Thing<'g>,
    rank: Rank,
}

enum Thing<'g> {
    Component(&'g Component),
    Symbol(&'g Symbol),
    File(String),
}

/// How a name contains the target, best first: equal to it ignoring case,
/// starting with it, holding it from a word's start (after `_`, `-`, `.`,
/// `/`, `:` or where a capital follows a small letter), anywhere, or with
/// `_` and `-` left out of both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Rank {
    Case,
    Start,
    Word,
    Inside,
    Squashed,
}

/// The shortest target whose containing names are looked for: shorter ones
/// are inside too many names to list.
const MIN_CONTAINED: usize = 3;

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
    let segments = components_by_segment(full, root, target);
    match (files.as_slice(), segments.as_slice()) {
        ([], []) => {}
        ([only], []) => return Ok(Resolved::File(only.clone())),
        ([], [only]) => return Ok(Resolved::Component(only)),
        _ => return Ok(Resolved::Candidates(every_match(full, root, target)?)),
    }

    // a word that names nothing: the names that contain it
    if target.contains('/') {
        bail!(
            "no component, file, symbol or import named `{target}`: a path names a file or \
             directory from the root; query the directory above it to see what is there"
        )
    }
    if target.chars().count() < MIN_CONTAINED {
        bail!("no component, file, symbol or import named `{target}`")
    }
    let contains = containing(full, root, target);
    if contains.is_empty() {
        bail!(
            "no component, file, symbol or import named `{target}`, nor a name that contains it, \
             ignoring case: for a word from an error message, a log or a screen, search the code \
             for it and query the file it finds"
        )
    }
    Ok(Resolved::Candidates(Candidates {
        contains,
        ..Candidates::default()
    }))
}

/// The last segment of a component's name: after its last `/` for a name
/// that is a path (`lib/notify`), else after its last `.` or `::`
/// (`shop.billing`, `archmap_core::graph`).
fn last_segment(name: &str) -> &str {
    let split: &[char] = if name.contains('/') {
        &['/']
    } else {
        &['.', ':']
    };
    name.rsplit(split).next().unwrap_or(name)
}

/// Components whose name ends in `target` as its last segment, for a
/// target that is one word, apart from those that are one file, which their
/// file's stem finds.
fn components_by_segment<'g>(
    full: &'g ArchitectureGraph,
    root: &Path,
    target: &str,
) -> Vec<&'g Component> {
    if target.contains(['/', '.', ':']) {
        return Vec::new();
    }
    full.components
        .values()
        .filter(|c| c.kind != ComponentKind::External)
        .filter(|c| c.name != target && last_segment(&c.name) == target)
        .filter(|c| component_file(full, root, c).is_none())
        .collect()
}

/// How `name` contains `target` (both compared in ASCII lower case), if it
/// does; `squashed` is `target` without `_` and `-`.
fn rank(name: &str, target: &str, squashed: &str) -> Option<Rank> {
    let lower = name.to_ascii_lowercase();
    if lower == target {
        return Some(Rank::Case);
    }
    if lower.starts_with(target) {
        return Some(Rank::Start);
    }
    let bytes = name.as_bytes();
    let mut inside = None;
    for (i, _) in lower.match_indices(target) {
        let prev = bytes[i - 1];
        let word = matches!(prev, b'_' | b'-' | b'.' | b'/' | b':')
            || ((prev.is_ascii_lowercase() || prev.is_ascii_digit())
                && bytes[i].is_ascii_uppercase());
        if word {
            return Some(Rank::Word);
        }
        inside = Some(Rank::Inside);
    }
    if inside.is_some() {
        return inside;
    }
    let lower: String = lower.chars().filter(|c| !matches!(c, '_' | '-')).collect();
    (!squashed.is_empty() && lower.contains(squashed)).then_some(Rank::Squashed)
}

/// What near matches sort by: rank, in test code, external, the name's
/// length, the kind (component, symbol, file), then the id or path.
type NearKey = (Rank, bool, bool, usize, u8, String);

/// The components, symbols and files whose names contain `target`,
/// ignoring case, best first: by how they contain it, production code
/// before tests, components of the repository before external ones, shorter
/// names first, then components, symbols and files, each by id or path.
fn containing<'g>(full: &'g ArchitectureGraph, root: &Path, target: &str) -> Vec<Near<'g>> {
    let target = target.to_ascii_lowercase();
    let squashed: String = target.chars().filter(|c| !matches!(c, '_' | '-')).collect();
    let best = |names: &[&str]| {
        names
            .iter()
            .filter_map(|n| rank(n, &target, &squashed))
            .min()
    };
    let mut found: Vec<(NearKey, Thing<'g>)> = Vec::new();
    let parents: BTreeSet<&ComponentId> = full
        .components
        .values()
        .filter_map(|c| c.parent.as_ref())
        .collect();
    for c in full.components.values() {
        let Some(rank) = best(&[&c.name, last_segment(&c.name)]) else {
            continue;
        };
        // a component that is one file is found as that file
        let leaf = !parents.contains(&c.id);
        if leaf
            && c.path
                .as_deref()
                .and_then(|p| file_target(root, p))
                .is_some()
        {
            continue;
        }
        // test code when a file in it would be by the path rule of the
        // languages that have one (Rust goes by the kind of Cargo target)
        let by_path = matches!(
            c.language.as_deref(),
            Some("python" | "typescript" | "javascript")
        );
        let test = by_path
            && c.path
                .as_deref()
                .is_some_and(|p| archmap_scan::is_test_code(Path::new(&format!("{p}/_"))));
        let external = c.kind == ComponentKind::External;
        let key = (rank, test, external, c.name.len(), 0, c.id.to_string());
        found.push((key, Thing::Component(c)));
    }
    for s in full.symbols.values() {
        let short = s.name.rsplit(['.', ':']).next().unwrap_or(&s.name);
        let Some(rank) = best(&[&s.name, short]) else {
            continue;
        };
        let test = s.location().is_some_and(|e| e.test);
        let key = (rank, test, false, s.name.len(), 1, s.id.to_string());
        found.push((key, Thing::Symbol(s)));
    }
    let files: Vec<&str> = full
        .known_files()
        .into_iter()
        .filter(|f| best(&[f.rsplit('/').next().unwrap_or(f)]).is_some())
        .collect();
    let tests: BTreeSet<&str> = test_files(full, files.iter().copied())
        .into_iter()
        .collect();
    for f in files {
        let name = f.rsplit('/').next().unwrap_or(f);
        if let Some(rank) = best(&[name]) {
            let key = (rank, tests.contains(f), false, name.len(), 2, f.to_owned());
            found.push((key, Thing::File(f.to_owned())));
        }
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found
        .into_iter()
        .map(|((rank, ..), thing)| Near { thing, rank })
        .collect()
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

/// Whether `evidence` is of a statement that imports `spec`, a package
/// subpath (`react-dom/client`), or a module below it: the analyzers write
/// the import name after the statement's kind (`import react-dom/client`,
/// `export react-dom/client, declared in packages/web/package.json:4`).
pub(crate) fn imports_subpath(evidence: &Evidence, spec: &str) -> bool {
    let Some(written) = evidence
        .note
        .as_deref()
        .and_then(|note| note.split_whitespace().nth(1))
    else {
        return false;
    };
    let (written, spec) = (
        module_path(written.trim_end_matches([',', ':'])),
        module_path(spec),
    );
    written == spec
        || written
            .strip_prefix(spec)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// An import name without a file extension or an `index` file at its end,
/// which name the same module (`kit/sub.js`, `kit/sub/index` -> `kit/sub`).
fn module_path(name: &str) -> &str {
    const EXTENSIONS: [&str; 9] = [
        ".js", ".mjs", ".cjs", ".jsx", ".ts", ".mts", ".cts", ".tsx", ".json",
    ];
    let name = EXTENSIONS
        .iter()
        .find_map(|ext| name.strip_suffix(ext))
        .unwrap_or(name);
    name.strip_suffix("/index").unwrap_or(name)
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
    production_first(full, &mut files);
    files
}

/// Production files before test files, each by path, so a capped list of
/// candidates shows the code first.
fn production_first(full: &ArchitectureGraph, files: &mut [String]) {
    let tests: BTreeSet<String> = test_files(full, files.iter().map(String::as_str))
        .into_iter()
        .map(str::to_owned)
        .collect();
    files.sort_by(|a, b| (tests.contains(a), a).cmp(&(tests.contains(b), b)));
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
    production_first(full, &mut files);
    Ok(Candidates {
        components: full.components_named(target).collect(),
        segments: components_by_segment(full, root, target),
        symbols: full.symbols_named(target).collect(),
        files,
        directories: directory.into_iter().collect(),
        contains: Vec::new(),
    })
}

/// Candidates listed in text; JSON lists all of them.
const MAX_CANDIDATES: usize = 10;

impl Candidates<'_> {
    fn total(&self) -> usize {
        self.components.len()
            + self.segments.len()
            + self.symbols.len()
            + self.files.len()
            + self.directories.len()
            + self.contains.len()
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
            Format::Text if self.contains.is_empty() => Ok(self.text(full, target)),
            Format::Text => Ok(self.contains_text(full, target)),
        }
    }

    fn text(&self, full: &ArchitectureGraph, target: &str) -> String {
        let mut kinds = Vec::new();
        for (n, one, many) in [
            (
                self.components.len() + self.segments.len(),
                "a component",
                "components",
            ),
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
        let mut out = format!("`{target}` names {names}; retry with one of them by id or path:\n");
        let mut lines = Vec::new();
        for c in self.components.iter().chain(&self.segments) {
            lines.push(component_row(c));
        }
        for s in &self.symbols {
            lines.push(symbol_row(full, s));
        }
        for f in &self.files {
            lines.push(file_row(f));
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

    /// The names that contain a target that names nothing, best first.
    fn contains_text(&self, full: &ArchitectureGraph, target: &str) -> String {
        let total = self.contains.len();
        let shown = total.min(MAX_CANDIDATES);
        let mut out = format!(
            "No name is `{target}`. Names that contain it, ignoring case: {}\n",
            count(total, shown)
        );
        for near in self.contains.iter().take(shown) {
            let row = match &near.thing {
                Thing::Component(c) => component_row(c),
                Thing::Symbol(s) => symbol_row(full, s),
                Thing::File(f) => file_row(f),
            };
            let _ = writeln!(out, "{row}");
        }
        out.push_str("Retry with one of them by id or path");
        out.push_str(match shown < total {
            true => "; JSON lists every one.\n",
            false => ".\n",
        });
        out
    }

    fn views(&self, full: &ArchitectureGraph) -> Vec<CandidateView<'_>> {
        let components = self.components.iter().map(|c| component_view(c, "exact"));
        let segments = self.segments.iter().map(|c| component_view(c, "segment"));
        let symbols = self.symbols.iter().map(|s| symbol_view(full, s, "exact"));
        let files = self.files.iter().map(|f| CandidateView {
            kind: "file",
            path: Some(f.clone()),
            ..CandidateView::of("exact")
        });
        let directories = self.directories.iter().map(|d| CandidateView {
            kind: "directory",
            path: Some(format!("./{d}")),
            ..CandidateView::of("exact")
        });
        let contains = self.contains.iter().map(|near| {
            let matched = match near.rank {
                Rank::Case => "case",
                _ => "contains",
            };
            match &near.thing {
                Thing::Component(c) => component_view(c, matched),
                Thing::Symbol(s) => symbol_view(full, s, matched),
                Thing::File(f) => CandidateView {
                    kind: "file",
                    path: Some(f.clone()),
                    ..CandidateView::of(matched)
                },
            }
        });
        components
            .chain(segments)
            .chain(symbols)
            .chain(files)
            .chain(directories)
            .chain(contains)
            .collect()
    }
}

/// A component as a candidate row: id, path and kind.
fn component_row(c: &Component) -> String {
    format!(
        "  {}  {}  {}",
        shell_word(c.id.as_str()),
        c.path.as_deref().unwrap_or("-"),
        component_kind(c.kind)
    )
}

/// A symbol as a candidate row: id, location, kind and, where its language
/// records names, how many statements take it.
fn symbol_row(full: &ArchitectureGraph, s: &Symbol) -> String {
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
    line
}

fn file_row(f: &str) -> String {
    format!("  {}  file", shell_word(f))
}

fn component_view<'a>(c: &'a Component, matched: &'static str) -> CandidateView<'a> {
    CandidateView {
        kind: "component",
        id: Some(c.id.as_str()),
        path: c.path.clone(),
        ..CandidateView::of(matched)
    }
}

fn symbol_view<'a>(
    full: &ArchitectureGraph,
    s: &'a Symbol,
    matched: &'static str,
) -> CandidateView<'a> {
    let counts = importer_counts(full, s);
    CandidateView {
        kind: "symbol",
        symbol_kind: Some(symbol_kind(s.kind)),
        id: Some(s.id.as_str()),
        file: s.location().map(|e| e.file.as_str()),
        line: s.location().and_then(|e| e.line),
        imported_by: counts.map(|c| c.0),
        may_use: counts.map(|c| c.1),
        ..CandidateView::of(matched)
    }
}

/// What `query` and `impact` return when a target names several things.
#[derive(Serialize)]
struct CandidatesView<'a> {
    requested: &'a str,
    total: usize,
    candidates: Vec<CandidateView<'a>>,
}

#[derive(Clone, Serialize)]
struct CandidateView<'a> {
    kind: &'static str,
    /// How it matches the target: `exact`, `segment` (the last segment of a
    /// component's name), or for a target that names nothing `case` (equal
    /// ignoring case) or `contains`.
    #[serde(rename = "match")]
    matched: &'static str,
    /// For a symbol: what it is (`function`, `struct`, ...).
    #[serde(skip_serializing_if = "Option::is_none")]
    symbol_kind: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<&'a str>,
    /// A component's path, or a file or directory as written to retry
    /// with (`./helper` for a directory).
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
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

impl CandidateView<'_> {
    fn of(matched: &'static str) -> Self {
        CandidateView {
            kind: "",
            matched,
            symbol_kind: None,
            id: None,
            path: None,
            file: None,
            line: None,
            imported_by: None,
            may_use: None,
        }
    }
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
    use archmap_core::{Edge, EdgeKind, Evidence};

    use super::*;
    use crate::target::unquote;

    #[test]
    fn candidates_list_files_their_analyzer_read_as_production_first() {
        // a Next.js route below app/test/, which the path rule alone calls test code
        let full = ArchitectureGraph {
            edges: vec![Edge::new("web", "ext:npm:react", EdgeKind::Import)
                .with_evidence(Evidence::new("app/test/page.tsx").at_line(1))],
            ..Default::default()
        };
        let mut files =
            ["tests/page.test.tsx", "src/page.tsx", "app/test/page.tsx"].map(str::to_owned);
        production_first(&full, &mut files);
        assert_eq!(
            files,
            ["app/test/page.tsx", "src/page.tsx", "tests/page.test.tsx"]
        );
    }

    #[test]
    fn one_pair_of_matching_quotes_is_dropped() {
        assert_eq!(unquote("'a::b'"), "a::b");
        assert_eq!(unquote("\"a\""), "a");
        assert_eq!(unquote("'a\""), "'a\"");
        assert_eq!(unquote("''a''"), "'a'");
        assert_eq!(unquote("'"), "'");
    }

    #[test]
    fn a_subpath_matches_its_statements_with_or_without_an_extension() {
        let noted = |note: &str| Evidence::new("a.ts").at_line(1).with_note(note);
        for note in [
            "import kit/sub",
            "import kit/sub.js",
            "import kit/sub/index.ts",
            "import kit/sub/deep",
            "export kit/sub, declared in web/package.json:4",
        ] {
            assert!(imports_subpath(&noted(note), "kit/sub"), "{note}");
            assert!(imports_subpath(&noted(note), "kit/sub.js"), "{note}");
        }
        for note in [
            "import kit/subway",
            "import kit/sub.browser",
            "import kit",
            "import",
        ] {
            assert!(!imports_subpath(&noted(note), "kit/sub"), "{note}");
        }
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
