//! Module trees of the crate targets, and `use` path resolution over them.
//!
//! Every Cargo target of a package is a separate crate with a tree of
//! logical modules found by following `mod` declarations from its root file:
//! `mod a;` loads `a.rs` or `a/mod.rs` in the declaring module's directory.
//! Inline modules (`mod a { .. }`) are nodes too, so that paths resolve as
//! the compiler resolves them, but a component is a file: an inline module
//! belongs to the component of its file, and a file that several crates
//! declare is one component. Every root belongs to its package. The modules
//! of the library (`src/lib.rs`) and of the binary `src/main.rs` are named by
//! their module paths; those of the other targets (`tests/`, `examples/`,
//! `benches/`, `build.rs`) by their paths in the package, so that they never
//! meet a library module of the same module path. A file under `src/` that no
//! root reaches (a binary in `src/bin/`, a `#[path]` module) belongs to its
//! package, at the module path its location suggests, with no crate root to
//! resolve `crate::` against; outside `src/`, only what roots reach is read.
//!
//! Resolution follows a `use` path through modules and through the `use`
//! declarations it meets (re-exports, globs), as precisely as the facts
//! permit: to the module that defines the name, otherwise to the deepest
//! module reached, and to nothing through a `mod` whose file was not read.
//! A glob only brings in what the importing module can see, as in the
//! compiler; globs that disagree and cycles of declarations stop the walk
//! rather than guess.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use archmap_core::{ComponentId, Scope};

use super::source::{ModDecl, ModuleFacts, PathRef, RustFile, UseDecl};
use crate::context::display_path;

/// Crates of the standard distribution, which never become components.
const STANDARD: &[&str] = &["std", "core", "alloc", "proc_macro", "test"];

/// One internal package, after manifest parsing, with everything the
/// source pass needs to resolve names.
#[derive(Debug, Clone)]
pub(super) struct ResolvedPackage {
    pub id: ComponentId,
    pub name: String,
    /// Name of the library crate in source: `[lib] name`, or the package
    /// name with `-` as `_`.
    pub crate_name: String,
    /// Directory containing `Cargo.toml`, relative to the repo root.
    pub dir: PathBuf,
    /// Crate names that the manifest declares (`archmap-core` ->
    /// `archmap_core`), mapped to the component each refers to.
    pub import_targets: BTreeMap<String, ComponentId>,
    /// The other packages of the repository that the manifest does not
    /// declare, by crate name: tried last, for dependencies the manifest
    /// reader misses (target-specific tables).
    pub other_packages: BTreeMap<String, ComponentId>,
    /// Crate names of `[dev-dependencies]`, which have no edges.
    pub dev_imports: BTreeSet<String>,
}

/// The kind of a Cargo target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum TargetKind {
    Lib,
    Bin,
    Test,
    Example,
    Bench,
    Build,
}

impl TargetKind {
    /// Tests, examples and benches may use `[dev-dependencies]` and are no
    /// part of the package's library and binaries: their code is test code.
    pub fn is_test(self) -> bool {
        matches!(
            self,
            TargetKind::Test | TargetKind::Example | TargetKind::Bench
        )
    }
}

/// A target of a package other than its library and its `src/main.rs`
/// binary: its kind, its root file (relative to the repository root) and
/// the name of its crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Target {
    pub kind: TargetKind,
    pub root: PathBuf,
    pub crate_name: String,
}

/// The targets Cargo finds by default among the files of the package in
/// `dir`, besides `src/lib.rs` and `src/main.rs`: `tests/<n>.rs` and
/// `tests/<n>/main.rs`, the same under `examples/` and `benches/`, and
/// `build.rs`. Sorted by root.
pub(super) fn default_targets<'a>(
    dir: &Path,
    files: impl IntoIterator<Item = &'a Path>,
) -> Vec<Target> {
    let mut targets: Vec<Target> = files
        .into_iter()
        .filter_map(|file| {
            let rel = file.strip_prefix(dir).ok()?;
            let parts: Vec<&str> = rel.iter().filter_map(|p| p.to_str()).collect();
            let (kind, name) = match parts.as_slice() {
                ["build.rs"] => (TargetKind::Build, "build_script_build"),
                [kind, file] => (kind_of(kind)?, file.strip_suffix(".rs")?),
                [kind, name, "main.rs"] => (kind_of(kind)?, *name),
                _ => return None,
            };
            Some(Target {
                kind,
                root: file.to_path_buf(),
                crate_name: name.replace('-', "_"),
            })
        })
        .collect();
    targets.sort_by(|a, b| a.root.cmp(&b.root));
    targets
}

/// The kind of the targets Cargo finds in a directory of a package.
fn kind_of(directory: &str) -> Option<TargetKind> {
    match directory {
        "tests" => Some(TargetKind::Test),
        "examples" => Some(TargetKind::Example),
        "benches" => Some(TargetKind::Bench),
        _ => None,
    }
}

/// What a crate's tree is: its target's kind, and whether its modules are
/// named by their paths in the package (every target but the library and
/// `src/main.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Tree {
    pub kind: TargetKind,
    pub by_path: bool,
}

/// A parsed source file of an internal package.
pub(super) struct SourceFile {
    /// Path relative to the repository root.
    pub rel: PathBuf,
    /// Index of the owning package.
    pub package: usize,
    pub parsed: RustFile,
}

/// A logical module: a file, or an inline module inside one.
#[derive(Debug, Clone)]
pub(super) struct Node {
    pub package: usize,
    /// Root of the node's crate; `None` for a file that no root reaches.
    pub root: Option<usize>,
    pub parent: Option<usize>,
    /// Module path within the crate, inline modules included.
    pub path: Vec<String>,
    /// The file that holds the module, and the module's index in it.
    pub file: usize,
    pub module: usize,
    /// Compiled only for tests: marked `#[cfg(test)]`, or inside such a
    /// module.
    pub test: bool,
    /// Child modules by name, inline or in files of their own.
    pub children: BTreeMap<String, usize>,
    /// `mod name;` declarations whose file was not read.
    pub unloaded: BTreeSet<String>,
}

/// A file module that a `mod` declaration loads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ModuleComponent {
    pub id: ComponentId,
    pub name: String,
    pub file: usize,
    /// Component of the file with the declaration.
    pub parent: ComponentId,
    /// The declaration: file and line.
    pub declared_in: usize,
    pub line: u32,
}

#[derive(Debug, Default)]
pub(super) struct Forest {
    pub nodes: Vec<Node>,
    /// Package index -> root node of its library.
    pub libs: BTreeMap<usize, usize>,
    /// Root node -> what its crate's tree is.
    pub trees: BTreeMap<usize, Tree>,
    /// Component of each file, by file index.
    pub owners: Vec<ComponentId>,
    pub modules: Vec<ModuleComponent>,
    /// `#[macro_export]` macros by crate root and name -> defining module.
    pub macros: BTreeMap<(usize, String), usize>,
    /// Files a `mod` declaration loads that exist but were not read: the
    /// modules of roots outside `src/`, read only once a root reaches them.
    pub missing: BTreeSet<PathBuf>,
    pub warnings: Vec<String>,
}

impl Forest {
    /// The tree `node` belongs to; `None` for a file that no root reaches.
    pub fn tree(&self, node: usize) -> Option<Tree> {
        self.trees.get(&self.nodes[node].root?).copied()
    }

    /// Whether `node` is in a test, example or bench target, all of whose
    /// code is test code.
    pub fn in_test_target(&self, node: usize) -> bool {
        self.tree(node).is_some_and(|t| t.kind.is_test())
    }

    /// Is `node` inside the subtree of `ancestor` (and not `ancestor` itself)?
    pub fn is_descendant(&self, node: usize, ancestor: usize) -> bool {
        let mut current = node;
        while let Some(parent) = self.nodes[current].parent {
            if parent == ancestor {
                return true;
            }
            current = parent;
        }
        false
    }

    /// The node of the file that holds `node`: `node` itself for a file
    /// module, the enclosing file module for an inline one.
    pub fn file_module(&self, node: usize) -> usize {
        let mut current = node;
        while self.nodes[current].module != 0 {
            match self.nodes[current].parent {
                Some(parent) => current = parent,
                None => break,
            }
        }
        current
    }

    fn push(&mut self, node: Node) -> usize {
        self.nodes.push(node);
        self.nodes.len() - 1
    }

    fn add_child(
        &mut self,
        parent: usize,
        name: &str,
        file: usize,
        module: usize,
        test: bool,
    ) -> usize {
        let p = &self.nodes[parent];
        let mut path = p.path.clone();
        path.push(name.to_owned());
        let child = Node {
            package: p.package,
            root: p.root,
            parent: Some(parent),
            path,
            file,
            module,
            test: p.test || test,
            children: BTreeMap::new(),
            unloaded: BTreeSet::new(),
        };
        let index = self.push(child);
        self.nodes[parent].children.insert(name.to_owned(), index);
        index
    }
}

/// Build the module trees of every package's targets, and give every file
/// its component. `targets` holds, by package, the targets other than the
/// library and `src/main.rs`. `unreadable` holds the files that could not be
/// read or parsed, which a `mod` declaration may still name; `on_disk`, every
/// `.rs` file of the repository, so that a module outside `src/` that was
/// not read yet is named in `missing`.
pub(super) fn build(
    files: &[SourceFile],
    unreadable: &BTreeSet<PathBuf>,
    packages: &[ResolvedPackage],
    targets: &[Vec<Target>],
    on_disk: &BTreeSet<&Path>,
) -> Forest {
    let index: BTreeMap<&Path, usize> = files
        .iter()
        .enumerate()
        .map(|(i, f)| (f.rel.as_path(), i))
        .collect();
    let shared = Shared {
        files,
        index: &index,
        unreadable,
        on_disk,
        packages,
    };
    let mut forest = Forest::default();
    let mut owners: Vec<Option<ComponentId>> = vec![None; files.len()];
    // the roots of the targets, which stay their package's files
    let mut root_files: BTreeSet<usize> = BTreeSet::new();
    let mut later: Vec<(usize, String)> = Vec::new();

    for (p, package) in packages.iter().enumerate() {
        let own = [
            (TargetKind::Lib, "lib.rs", package.crate_name.clone()),
            (TargetKind::Bin, "main.rs", package.name.replace('-', "_")),
        ];
        let other = targets
            .get(p)
            .into_iter()
            .flatten()
            .map(|t| (t.kind, t.root.clone(), t.crate_name.clone()));
        for (kind, root_path, crate_name) in own
            .into_iter()
            .map(|(kind, file, name)| (kind, package.dir.join("src").join(file), name))
            .chain(other)
        {
            let Some(&file) = index.get(root_path.as_path()) else {
                continue;
            };
            let by_path = !matches!(kind, TargetKind::Lib)
                && !(kind == TargetKind::Bin && root_path == package.dir.join("src/main.rs"));
            owners[file].get_or_insert_with(|| package.id.clone());
            root_files.insert(file);
            let root = forest.push(Node {
                package: p,
                root: None,
                parent: None,
                path: Vec::new(),
                file,
                module: 0,
                test: false,
                children: BTreeMap::new(),
                unloaded: BTreeSet::new(),
            });
            forest.nodes[root].root = Some(root);
            forest.trees.insert(root, Tree { kind, by_path });
            if kind == TargetKind::Lib {
                forest.libs.insert(p, root);
            }
            // the library's and the binary's modules first, then, once every
            // root has its owner, the other targets'
            if by_path {
                later.push((root, crate_name));
            } else {
                Grower {
                    shared,
                    root_files: &root_files,
                    crate_name,
                    by_path,
                    owners: &mut owners,
                    forest: &mut forest,
                    seen: BTreeSet::from([file]),
                }
                .grow(root, true);
            }
        }
    }
    for (root, crate_name) in later {
        let file = forest.nodes[root].file;
        Grower {
            shared,
            root_files: &root_files,
            crate_name,
            by_path: true,
            owners: &mut owners,
            forest: &mut forest,
            seen: BTreeSet::from([file]),
        }
        .grow(root, true);
    }

    for (f, file) in files.iter().enumerate() {
        if owners[f].is_some() {
            continue;
        }
        let package = &packages[file.package];
        owners[f] = Some(package.id.clone());
        let root = forest.push(Node {
            package: file.package,
            root: None,
            parent: None,
            path: module_path(&package.dir, &file.rel).unwrap_or_default(),
            file: f,
            module: 0,
            test: false,
            children: BTreeMap::new(),
            unloaded: BTreeSet::new(),
        });
        Grower {
            shared,
            root_files: &root_files,
            crate_name: String::new(),
            by_path: false,
            owners: &mut owners,
            forest: &mut forest,
            seen: BTreeSet::from([f]),
        }
        .grow(root, false);
    }

    for (n, node) in forest.nodes.iter().enumerate() {
        let Some(root) = node.root else {
            continue;
        };
        for name in &files[node.file].parsed.modules[node.module].exported_macros {
            forest.macros.entry((root, name.clone())).or_insert(n);
        }
    }

    forest.owners = owners
        .into_iter()
        .map(|o| o.expect("every file has an owner"))
        .collect();
    forest
}

/// What growing every tree reads.
#[derive(Clone, Copy)]
struct Shared<'a> {
    files: &'a [SourceFile],
    index: &'a BTreeMap<&'a Path, usize>,
    unreadable: &'a BTreeSet<PathBuf>,
    on_disk: &'a BTreeSet<&'a Path>,
    packages: &'a [ResolvedPackage],
}

struct Grower<'a> {
    shared: Shared<'a>,
    /// The roots of the targets, which stay their package's files.
    root_files: &'a BTreeSet<usize>,
    /// Crate name that module names start with.
    crate_name: String,
    /// Whether modules are named by their paths in the package.
    by_path: bool,
    owners: &'a mut Vec<Option<ComponentId>>,
    forest: &'a mut Forest,
    /// Files already in this tree.
    seen: BTreeSet<usize>,
}

impl Grower<'_> {
    /// Add the inline modules of `node` and, when `follow` is set, the files
    /// its `mod` declarations load.
    fn grow(&mut self, node: usize, follow: bool) {
        let (files, packages) = (self.shared.files, self.shared.packages);
        let (file, module) = (self.forest.nodes[node].file, self.forest.nodes[node].module);
        let facts = &files[file].parsed.modules[module];
        for (name, &inline) in &facts.inline {
            let test = files[file].parsed.modules[inline].test;
            let child = self.forest.add_child(node, name, file, inline, test);
            self.grow(child, follow);
        }
        for decl in &facts.declared {
            let loaded = if follow { self.load(node, decl) } else { None };
            let Some(loaded) = loaded else {
                self.forest.nodes[node].unloaded.insert(decl.name.clone());
                continue;
            };
            if !self.seen.insert(loaded) {
                continue;
            }

            let child = self
                .forest
                .add_child(node, &decl.name, loaded, 0, decl.test);
            // a root that another root loads (an old-style `tests/common.rs`)
            // stays its package's file
            if !self.root_files.contains(&loaded) {
                let package = &packages[self.forest.nodes[node].package];
                let (id, name) = if self.by_path {
                    let rel = &files[loaded].rel;
                    let rel = display_path(rel.strip_prefix(&package.dir).unwrap_or(rel));
                    (format!("{}::{rel}", package.name), rel)
                } else {
                    let path = self.forest.nodes[child].path.join("::");
                    let name = format!("{}::{path}", self.crate_name);
                    (format!("{}::{path}", package.name), name)
                };
                // A file that another crate of the package declared already
                // keeps its component.
                let id = self.owners[loaded]
                    .get_or_insert_with(|| ComponentId::new(id))
                    .clone();
                let parent = self.owners[file]
                    .clone()
                    .expect("a declaring file has an owner");
                self.forest.modules.push(ModuleComponent {
                    id,
                    name,
                    file: loaded,
                    parent,
                    declared_in: file,
                    line: decl.line,
                });
            }
            self.grow(child, true);
        }
    }

    /// The file that `mod name;` in `node` loads, warning when there is none.
    fn load(&mut self, node: usize, decl: &ModDecl) -> Option<usize> {
        let file = self.forest.nodes[node].file;
        let at = format!(
            "{}:{}",
            display_path(&self.shared.files[file].rel),
            decl.line
        );
        if decl.path_attr {
            self.forest.warnings.push(format!(
                "{at}: `mod {}` has a #[path] attribute, which is not followed",
                decl.name
            ));
            return None;
        }
        let dir = self.module_dir(node);
        let candidates = [
            dir.join(format!("{}.rs", decl.name)),
            dir.join(&decl.name).join("mod.rs"),
        ];
        let found: Vec<usize> = candidates
            .iter()
            .filter_map(|c| self.shared.index.get(c.as_path()).copied())
            .collect();
        if found.is_empty() {
            let unreadable = self.shared.unreadable;
            // a module outside `src/`, read once a root reaches it
            let unread = candidates
                .iter()
                .find(|c| self.shared.on_disk.contains(c.as_path()) && !unreadable.contains(*c));
            if let Some(unread) = unread {
                self.forest.missing.insert(unread.clone());
                return None;
            }
            // a file that failed to parse has a warning of its own
            if !candidates.iter().any(|c| unreadable.contains(c)) {
                self.forest.warnings.push(format!(
                    "{at}: no file for `mod {}` (expected {} or {})",
                    decl.name,
                    display_path(&candidates[0]),
                    display_path(&candidates[1])
                ));
            }
            return None;
        }
        if found.len() > 1 {
            self.forest.warnings.push(format!(
                "{at}: both {} and {} exist for `mod {}`; reading the first",
                display_path(&candidates[0]),
                display_path(&candidates[1]),
                decl.name
            ));
        }
        Some(found[0])
    }

    /// Directory in which `mod name;` inside `node` looks for its file: the
    /// file's directory for a crate root or `mod.rs`, `a/` for `a.rs`, and
    /// one level further for each inline module on the way.
    fn module_dir(&self, node: usize) -> PathBuf {
        let n = &self.forest.nodes[node];
        if n.module != 0 {
            let parent = n.parent.expect("an inline module has a parent");
            let name = n.path.last().expect("an inline module has a name");
            return self.module_dir(parent).join(name);
        }
        let rel = &self.shared.files[n.file].rel;
        let dir = rel.parent().unwrap_or(Path::new(""));
        if n.parent.is_none() || rel.file_name().is_some_and(|f| f == "mod.rs") {
            dir.to_path_buf()
        } else {
            dir.join(rel.file_stem().unwrap_or_default())
        }
    }
}

/// Module path of a file from its location in the package: `src/lib.rs` ->
/// `[]`, `src/a/mod.rs` -> `[a]`, `src/a/b.rs` -> `[a, b]`. Files outside
/// `src/` yield `None`.
pub(super) fn module_path(pkg_dir: &Path, file: &Path) -> Option<Vec<String>> {
    let rel = file.strip_prefix(pkg_dir).ok()?;
    let mut parts = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned());
    if parts.next()? != "src" {
        return None;
    }
    let mut parts: Vec<String> = parts.collect();
    let last = parts.pop()?;
    let stem = last.strip_suffix(".rs")?;
    match stem {
        "lib" | "main" if parts.is_empty() => {}
        "mod" => {}
        other => parts.push(other.to_owned()),
    }
    Some(parts)
}

/// Where a `use` path leads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Resolved {
    /// A module in the repository: the one that defines the name, or the
    /// deepest one the path reached. `name` is the item reached there, as
    /// that module defines it; `None` when the path names the module itself.
    Module {
        node: usize,
        via: Option<Via>,
        name: Option<String>,
    },
    /// A component without a module tree: an external crate, or a package
    /// without a library.
    Crate { id: ComponentId, via: Option<Via> },
    /// A crate that only a dev-dependency provides.
    DevOnly(String),
    /// The standard library, a module whose file was not read, or a name the
    /// scan cannot place.
    Nothing,
}

/// The first `use` declaration in another file that a path went through:
/// file index and line.
pub(super) type Via = (usize, u32);

/// Resolution state of one path: where it started, the declarations being
/// followed (so that a cycle of them ends), and the glob lookups made so
/// far (so that each is made once, however many globs lead to it).
struct Walk {
    origin: usize,
    active: Vec<(usize, String)>,
    globs: BTreeMap<(usize, String, bool), GlobLookup>,
}

#[derive(Debug, Clone)]
enum GlobLookup {
    /// In progress further up: counts as not found.
    Pending,
    Done(Option<(Pos, Option<Via>)>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Pos {
    /// A module; the next segment is looked up inside it.
    Module(usize),
    /// Something defined in (or not found in) a module, by its name there;
    /// later segments, such as enum variants, stay there.
    Item(usize, String),
    Crate(ComponentId),
    DevOnly(String),
    Nothing,
}

pub(super) struct Resolver<'a> {
    forest: &'a Forest,
    files: &'a [SourceFile],
    packages: &'a [ResolvedPackage],
    by_id: BTreeMap<&'a ComponentId, usize>,
}

impl<'a> Resolver<'a> {
    pub fn new(
        forest: &'a Forest,
        files: &'a [SourceFile],
        packages: &'a [ResolvedPackage],
    ) -> Self {
        let by_id = packages
            .iter()
            .enumerate()
            .map(|(i, p)| (&p.id, i))
            .collect();
        Self {
            forest,
            files,
            packages,
            by_id,
        }
    }

    /// Resolve `decl`, written in module `node`.
    pub fn resolve(&self, node: usize, decl: &UseDecl) -> Resolved {
        let mut walk = self.walk(node);
        let mut via = None;
        let pos = self.path(node, &decl.path, decl.leading_colon, &mut via, &mut walk);
        resolved(pos, via)
    }

    /// Resolve a module path written in code in module `node`. It counts only
    /// when it names a module by itself: when its first segment is a name
    /// that a `use` of `node` brought in, or that `node` defines, the
    /// dependency is that `use`'s, and the path resolves to nothing.
    pub fn resolve_path(&self, node: usize, path: &PathRef) -> Resolved {
        let Some((first, rest)) = path.segments.split_first() else {
            return Resolved::Nothing;
        };
        let mut walk = self.walk(node);
        let mut via = None;
        let pos = if path.leading_colon {
            self.crate_named(node, first)
        } else {
            self.first_direct(node, first, &mut walk)
        };
        let pos = self.rest(pos, rest, node, &mut via, &mut walk);
        resolved(pos, via)
    }

    /// The type an `impl` in module `node` is for, as the file that defines
    /// it and its name there. `None` when the path reaches no item of the
    /// repository.
    pub fn resolve_type(
        &self,
        node: usize,
        segments: &[String],
        leading_colon: bool,
    ) -> Option<(usize, String)> {
        let mut walk = self.walk(node);
        let mut via = None;
        match self.path(node, segments, leading_colon, &mut via, &mut walk) {
            Pos::Item(m, name) => Some((self.forest.nodes[m].file, name)),
            _ => None,
        }
    }

    fn walk(&self, node: usize) -> Walk {
        Walk {
            origin: self.forest.nodes[node].file,
            active: Vec::new(),
            globs: BTreeMap::new(),
        }
    }

    fn facts(&self, node: usize) -> &'a ModuleFacts {
        let n = &self.forest.nodes[node];
        &self.files[n.file].parsed.modules[n.module]
    }

    /// Resolve `segments`, written in module `at`.
    fn path(
        &self,
        at: usize,
        segments: &[String],
        leading_colon: bool,
        via: &mut Option<Via>,
        walk: &mut Walk,
    ) -> Pos {
        let Some((first, rest)) = segments.split_first() else {
            return Pos::Module(at);
        };
        let pos = if leading_colon {
            self.crate_named(at, first)
        } else {
            self.first(at, first, via, walk)
        };
        self.rest(pos, rest, at, via, walk)
    }

    /// Follow the segments after the first from `pos`, for a path written in
    /// module `at`.
    fn rest(
        &self,
        mut pos: Pos,
        segments: &[String],
        at: usize,
        via: &mut Option<Via>,
        walk: &mut Walk,
    ) -> Pos {
        for segment in segments {
            pos = match pos {
                Pos::Module(m) => self.step(m, segment, at, via, walk),
                other => return other,
            };
        }
        pos
    }

    /// A crate by name alone, as after `::`.
    fn crate_named(&self, at: usize, name: &str) -> Pos {
        self.extern_crate(at, name)
            .or_else(|| self.other_package(at, name))
            .unwrap_or_else(|| self.unknown_crate(at, name))
    }

    /// The first segment of a path in code when it names a module or crate by
    /// itself: `crate`, `self`, `super`, a child module or a crate. Nothing
    /// for a name that `at` defines or that one of its `use` declarations
    /// (an alias or a glob) brought in.
    fn first_direct(&self, at: usize, name: &str, walk: &mut Walk) -> Pos {
        let node = &self.forest.nodes[at];
        match name {
            "crate" => return node.root.map_or(Pos::Nothing, Pos::Module),
            "self" => return Pos::Module(at),
            "super" => return node.parent.map_or(Pos::Nothing, Pos::Module),
            _ => {}
        }
        if let Some(&child) = node.children.get(name) {
            return Pos::Module(child);
        }
        let facts = self.facts(at);
        let imported = facts.uses.iter().any(|u| u.binds.as_deref() == Some(name));
        let defined = facts.items.contains(name) || node.unloaded.contains(name);
        if imported || defined || STANDARD.contains(&name) {
            return Pos::Nothing;
        }
        if let Some(pos) = self.extern_crate(at, name) {
            return pos;
        }
        if self.glob(at, name, at, &mut None, walk).is_some() {
            return Pos::Nothing;
        }
        self.other_package(at, name)
            .unwrap_or_else(|| self.unknown_crate(at, name))
    }

    fn first(&self, at: usize, name: &str, via: &mut Option<Via>, walk: &mut Walk) -> Pos {
        let node = &self.forest.nodes[at];
        match name {
            "crate" => node.root.map_or(Pos::Nothing, Pos::Module),
            "self" => Pos::Module(at),
            "super" => node.parent.map_or(Pos::Nothing, Pos::Module),
            // A name both a glob and a declared crate provide is an error in
            // the compiler, so the order of those two does not matter.
            _ => self
                .local(at, name, at, via, walk)
                .or_else(|| self.extern_crate(at, name))
                .or_else(|| STANDARD.contains(&name).then_some(Pos::Nothing))
                .or_else(|| self.glob(at, name, at, via, walk))
                .or_else(|| self.other_package(at, name))
                .unwrap_or_else(|| self.unknown_crate(at, name)),
        }
    }

    /// Look `name` up in module `m` for a path written in module `from`.
    fn step(
        &self,
        m: usize,
        name: &str,
        from: usize,
        via: &mut Option<Via>,
        walk: &mut Walk,
    ) -> Pos {
        match name {
            "super" => self.forest.nodes[m]
                .parent
                .map_or(Pos::Nothing, Pos::Module),
            "self" => Pos::Module(m),
            _ => self
                .local(m, name, from, via, walk)
                .or_else(|| self.glob(m, name, from, via, walk))
                .unwrap_or_else(|| Pos::Item(m, name.to_owned())),
        }
    }

    /// Whether code in `from` sees what `m` keeps private: it is `m` or
    /// inside it.
    fn sees_private(&self, from: usize, m: usize) -> bool {
        from == m || self.forest.is_descendant(from, m)
    }

    /// A name that module `m` declares itself: a child module, a `use`, an
    /// item, or at a crate root a `#[macro_export]` macro.
    fn local(
        &self,
        m: usize,
        name: &str,
        from: usize,
        via: &mut Option<Via>,
        walk: &mut Walk,
    ) -> Option<Pos> {
        let node = &self.forest.nodes[m];
        if let Some(&child) = node.children.get(name) {
            return Some(Pos::Module(child));
        }
        if node.unloaded.contains(name) {
            return Some(Pos::Nothing);
        }
        let facts = self.facts(m);
        let open = self.sees_private(from, m);
        let alias = facts.uses.iter().find(|u| {
            u.scope == Scope::Module && (open || u.reexport) && u.binds.as_deref() == Some(name)
        });
        let key = (m, name.to_owned());
        // A declaration does not resolve through itself (`extern crate a;`),
        // and a cycle of declarations falls back to what `m` defines.
        if let Some(decl) = alias.filter(|_| !walk.active.contains(&key)) {
            if via.is_none() && node.file != walk.origin {
                *via = Some((node.file, decl.line));
            }
            walk.active.push(key);
            let pos = self.path(m, &decl.path, decl.leading_colon, via, walk);
            walk.active.pop();
            return Some(pos);
        }
        if facts.items.contains(name) {
            return Some(Pos::Item(m, name.to_owned()));
        }
        if node.root != Some(m) {
            return None;
        }
        self.forest
            .macros
            .get(&(m, name.to_owned()))
            .map(|&defined| Pos::Item(defined, name.to_owned()))
    }

    /// A name that a glob import in `m` brings in, for a path written in
    /// `from`. `None` when no glob has it; `m` itself when globs disagree.
    fn glob(
        &self,
        m: usize,
        name: &str,
        from: usize,
        via: &mut Option<Via>,
        walk: &mut Walk,
    ) -> Option<Pos> {
        let key = (m, name.to_owned(), self.sees_private(from, m));
        let found = match walk.globs.get(&key) {
            Some(GlobLookup::Pending) => return None,
            Some(GlobLookup::Done(found)) => found.clone(),
            None => {
                walk.globs.insert(key.clone(), GlobLookup::Pending);
                let found = self.glob_lookup(m, name, key.2, walk);
                walk.globs.insert(key, GlobLookup::Done(found.clone()));
                found
            }
        };
        let (pos, this) = found?;
        if via.is_none() {
            *via = this;
        }
        Some(pos)
    }

    fn glob_lookup(
        &self,
        m: usize,
        name: &str,
        open: bool,
        walk: &mut Walk,
    ) -> Option<(Pos, Option<Via>)> {
        let file = self.forest.nodes[m].file;
        let mut found: Option<(Pos, Option<Via>)> = None;
        for decl in &self.facts(m).uses {
            // only what the path's module can see: private globs of another
            // module import nothing for it
            if !decl.glob || decl.scope != Scope::Module || !(open || decl.reexport) {
                continue;
            }
            let mut inner = None;
            let hit = match self.path(m, &decl.path, decl.leading_colon, &mut inner, walk) {
                Pos::Module(g) if g != m => self
                    .local(g, name, m, &mut inner, walk)
                    .or_else(|| self.glob(g, name, m, &mut inner, walk)),
                _ => None,
            };
            let Some(pos) = hit else {
                continue;
            };
            let this = (file != walk.origin).then_some((file, decl.line)).or(inner);
            match &found {
                None => found = Some((pos, this)),
                Some((earlier, _)) if *earlier == pos => {}
                Some(_) => return Some((Pos::Item(m, name.to_owned()), None)),
            }
        }
        found
    }

    /// A crate the manifest declares, or the package's own library.
    fn extern_crate(&self, at: usize, name: &str) -> Option<Pos> {
        let p = self.forest.nodes[at].package;
        let package = &self.packages[p];
        if let Some(target) = package.import_targets.get(name) {
            return Some(self.crate_root(target));
        }
        // a binary, a test, an example or a bench names its own package's
        // library; a build script does not link it
        let links = self
            .forest
            .tree(at)
            .is_none_or(|t| t.kind != TargetKind::Build);
        (links && name == package.crate_name)
            .then(|| self.forest.libs.get(&p).copied().map(Pos::Module))
            .flatten()
    }

    /// Another package of the repository that the manifest does not declare.
    fn other_package(&self, at: usize, name: &str) -> Option<Pos> {
        let package = &self.packages[self.forest.nodes[at].package];
        package
            .other_packages
            .get(name)
            .map(|target| self.crate_root(target))
    }

    /// The library tree of a package, or the component when it has none.
    fn crate_root(&self, target: &ComponentId) -> Pos {
        match self.by_id.get(target).and_then(|i| self.forest.libs.get(i)) {
            Some(&root) => Pos::Module(root),
            None => Pos::Crate(target.clone()),
        }
    }

    fn unknown_crate(&self, at: usize, name: &str) -> Pos {
        let package = &self.packages[self.forest.nodes[at].package];
        if package.dev_imports.contains(name) {
            Pos::DevOnly(name.to_owned())
        } else {
            Pos::Nothing
        }
    }
}

fn resolved(pos: Pos, via: Option<Via>) -> Resolved {
    match pos {
        Pos::Module(m) => Resolved::Module {
            node: m,
            via,
            name: None,
        },
        Pos::Item(m, name) => Resolved::Module {
            node: m,
            via,
            name: Some(name),
        },
        Pos::Crate(id) => Resolved::Crate { id, via },
        Pos::DevOnly(name) => Resolved::DevOnly(name),
        Pos::Nothing => Resolved::Nothing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rust::source::parse_file;

    fn package(name: &str, dir: &str, declared: &[(&str, &str)]) -> ResolvedPackage {
        ResolvedPackage {
            id: ComponentId::new(name),
            name: name.to_owned(),
            crate_name: name.replace('-', "_"),
            dir: PathBuf::from(dir),
            import_targets: declared
                .iter()
                .map(|(n, id)| (n.to_string(), ComponentId::new(*id)))
                .collect(),
            other_packages: BTreeMap::new(),
            dev_imports: BTreeSet::new(),
        }
    }

    fn files(sources: &[(&str, &str)], packages: &[ResolvedPackage]) -> Vec<SourceFile> {
        sources
            .iter()
            .map(|(rel, text)| SourceFile {
                rel: PathBuf::from(rel),
                package: packages
                    .iter()
                    .position(|p| Path::new(rel).starts_with(&p.dir))
                    .unwrap(),
                parsed: parse_file(text).unwrap(),
            })
            .collect()
    }

    fn forest(files: &[SourceFile], packages: &[ResolvedPackage]) -> Forest {
        build(files, &BTreeSet::new(), packages, &[], &BTreeSet::new())
    }

    fn describe(forest: &Forest, files: &[SourceFile], resolved: Resolved) -> String {
        match resolved {
            Resolved::Module { node, via, .. } => {
                let mut to = display_path(&files[forest.nodes[node].file].rel);
                if let Some((f, line)) = via {
                    to.push_str(&format!(" via {}:{line}", display_path(&files[f].rel)));
                }
                to
            }
            Resolved::Crate { id, .. } => id.to_string(),
            Resolved::DevOnly(name) => format!("dev {name}"),
            Resolved::Nothing => "nothing".into(),
        }
    }

    /// Where each `use` of `file` resolves, as (path, where).
    fn resolved(
        forest: &Forest,
        files: &[SourceFile],
        packages: &[ResolvedPackage],
        file: &str,
    ) -> Vec<(String, String)> {
        let resolver = Resolver::new(forest, files, packages);
        let mut out = Vec::new();
        for (n, node) in forest.nodes.iter().enumerate() {
            let source = &files[node.file];
            if source.rel != Path::new(file) {
                continue;
            }
            for decl in &source.parsed.modules[node.module].uses {
                let to = describe(forest, files, resolver.resolve(n, decl));
                out.push((decl.path.join("::"), to));
            }
        }
        out
    }

    /// Where each module path in the code of `file` resolves.
    fn resolved_paths(
        forest: &Forest,
        files: &[SourceFile],
        packages: &[ResolvedPackage],
        file: &str,
    ) -> Vec<(String, String)> {
        let resolver = Resolver::new(forest, files, packages);
        let mut out = Vec::new();
        for (n, node) in forest.nodes.iter().enumerate() {
            let source = &files[node.file];
            if source.rel != Path::new(file) {
                continue;
            }
            for path in &source.parsed.modules[node.module].paths {
                let to = describe(forest, files, resolver.resolve_path(n, path));
                out.push((path.segments.join("::"), to));
            }
        }
        out
    }

    fn row(path: &str, to: &str) -> (String, String) {
        (path.to_owned(), to.to_owned())
    }

    #[test]
    fn module_path_follows_rust_conventions() {
        let dir = Path::new("crates/a");
        let mp = |f: &str| module_path(dir, Path::new(f));
        assert_eq!(mp("crates/a/src/lib.rs"), Some(vec![]));
        assert_eq!(mp("crates/a/src/main.rs"), Some(vec![]));
        assert_eq!(mp("crates/a/src/x.rs"), Some(vec!["x".into()]));
        assert_eq!(mp("crates/a/src/x/mod.rs"), Some(vec!["x".into()]));
        assert_eq!(
            mp("crates/a/src/x/y.rs"),
            Some(vec!["x".into(), "y".into()])
        );
        assert_eq!(
            mp("crates/a/src/bin/tool.rs"),
            Some(vec!["bin".into(), "tool".into()])
        );
        assert_eq!(mp("crates/a/tests/it.rs"), None);
        assert_eq!(mp("crates/a/build.rs"), None);
    }

    #[test]
    fn cargo_finds_the_targets_beside_src_by_their_place() {
        let files = [
            "pkg/build.rs",
            "pkg/tests/total.rs",
            "pkg/tests/multi-file/main.rs",
            "pkg/tests/common/mod.rs",
            "pkg/tests/ui/broken.rs",
            "pkg/examples/demo.rs",
            "pkg/benches/speed.rs",
            "pkg/src/lib.rs",
            "pkg/scripts/tool.rs",
        ];
        let targets = default_targets(Path::new("pkg"), files.iter().map(Path::new));
        let found: Vec<(TargetKind, &str, &str)> = targets
            .iter()
            .map(|t| (t.kind, t.root.to_str().unwrap(), t.crate_name.as_str()))
            .collect();
        // neither a module of a test nor a file deeper in `tests/` is one
        assert_eq!(
            found,
            [
                (TargetKind::Bench, "pkg/benches/speed.rs", "speed"),
                (TargetKind::Build, "pkg/build.rs", "build_script_build"),
                (TargetKind::Example, "pkg/examples/demo.rs", "demo"),
                (
                    TargetKind::Test,
                    "pkg/tests/multi-file/main.rs",
                    "multi_file"
                ),
                (TargetKind::Test, "pkg/tests/total.rs", "total"),
            ]
        );
    }

    #[test]
    fn a_build_script_does_not_name_the_library() {
        let packages = [package("kiosk", "", &[])];
        let files = files(
            &[
                ("src/lib.rs", "pub fn total() {}\n"),
                ("tests/total.rs", "use kiosk::total;\n"),
                ("build.rs", "use kiosk::total;\n"),
            ],
            &packages,
        );
        let targets = [default_targets(
            Path::new(""),
            files.iter().map(|f| f.rel.as_path()),
        )];
        let forest = build(
            &files,
            &BTreeSet::new(),
            &packages,
            &targets,
            &BTreeSet::new(),
        );
        assert_eq!(
            resolved(&forest, &files, &packages, "tests/total.rs"),
            vec![row("kiosk::total", "src/lib.rs")]
        );
        assert_eq!(
            resolved(&forest, &files, &packages, "build.rs"),
            vec![row("kiosk::total", "nothing")]
        );
    }

    #[test]
    fn library_and_binary_are_separate_trees_over_shared_files() {
        let packages = [package("app-cli", "", &[])];
        let files = files(
            &[
                ("src/lib.rs", "pub mod config;\npub struct Settings;\n"),
                (
                    "src/main.rs",
                    "mod config;\nmod cli;\nuse app_cli::config as shared;\nuse crate::cli::Args;\nstruct Settings;\n",
                ),
                ("src/config.rs", "use crate::Settings;\n"),
                ("src/cli.rs", "pub struct Args;\n"),
                ("src/bin/tool.rs", "use crate::x;\nuse app_cli::config;\n"),
            ],
            &packages,
        );
        let forest = forest(&files, &packages);
        assert!(forest.warnings.is_empty(), "{:?}", forest.warnings);
        let owners: Vec<&str> = forest.owners.iter().map(|o| o.as_str()).collect();
        assert_eq!(
            owners,
            vec![
                "app-cli",
                "app-cli",
                "app-cli::config",
                "app-cli::cli",
                "app-cli"
            ]
        );
        // config.rs is declared by both crates: one component, two declarations
        let declared: Vec<(&str, &str, usize)> = forest
            .modules
            .iter()
            .map(|m| (m.id.as_str(), m.name.as_str(), m.declared_in))
            .collect();
        assert_eq!(
            declared,
            vec![
                ("app-cli::config", "app_cli::config", 0),
                ("app-cli::config", "app_cli::config", 1),
                ("app-cli::cli", "app_cli::cli", 1),
            ]
        );
        // the binary reaches the library by its crate name, `crate::` stays
        // in the binary
        assert_eq!(
            resolved(&forest, &files, &packages, "src/main.rs"),
            vec![
                row("app_cli::config", "src/config.rs"),
                row("crate::cli::Args", "src/cli.rs"),
            ]
        );
        // a file of both crates resolves in each of them
        assert_eq!(
            resolved(&forest, &files, &packages, "src/config.rs"),
            vec![
                row("crate::Settings", "src/lib.rs"),
                row("crate::Settings", "src/main.rs"),
            ]
        );
        // a file no root reaches has no `crate::` to resolve against
        assert_eq!(
            resolved(&forest, &files, &packages, "src/bin/tool.rs"),
            vec![
                row("crate::x", "nothing"),
                row("app_cli::config", "src/config.rs")
            ]
        );
    }

    #[test]
    fn the_library_name_names_the_crate() {
        let mut foo = package("foo-cli", "", &[]);
        foo.crate_name = "foo".into();
        let packages = [foo];
        let files = files(
            &[
                (
                    "src/lib.rs",
                    "extern crate self as foo;\npub struct Engine;\nmod inner;\n",
                ),
                ("src/inner.rs", "use foo::Engine;\n"),
                ("src/main.rs", "use foo::Engine;\n"),
            ],
            &packages,
        );
        let forest = forest(&files, &packages);
        assert_eq!(forest.modules[0].id.as_str(), "foo-cli::inner");
        assert_eq!(forest.modules[0].name, "foo::inner");
        for file in ["src/inner.rs", "src/main.rs"] {
            assert_eq!(
                resolved(&forest, &files, &packages, file),
                vec![row("foo::Engine", "src/lib.rs")],
                "{file}"
            );
        }
    }

    #[test]
    fn inline_modules_stay_in_the_path_of_what_they_declare() {
        let packages = [package("p", "", &[])];
        let files = files(
            &[
                (
                    "src/lib.rs",
                    "mod outer {\n    pub mod inner;\n    pub struct Helper;\n}\n",
                ),
                ("src/outer/inner.rs", "use super::Helper;\n"),
            ],
            &packages,
        );
        let forest = forest(&files, &packages);
        assert_eq!(
            forest.modules,
            vec![ModuleComponent {
                id: ComponentId::new("p::outer::inner"),
                name: "p::outer::inner".into(),
                file: 1,
                parent: ComponentId::new("p"),
                declared_in: 0,
                line: 2,
            }]
        );
        // `super` of the file module is the inline module, in lib.rs
        assert_eq!(
            resolved(&forest, &files, &packages, "src/outer/inner.rs"),
            vec![row("super::Helper", "src/lib.rs")]
        );
    }

    #[test]
    fn resolution_follows_reexports_and_globs_to_the_defining_file() {
        let packages = [
            package("a", "a", &[("b", "b"), ("serde", "ext:cargo:serde")]),
            package("b", "b", &[]),
        ];
        let files = files(
            &[
                (
                    "a/src/lib.rs",
                    "\
mod x;
use b::Thing;
use b::sub::Deep;
use b::Glob;
use b::Twice;
use b::Loop;
use serde::Serialize;
use std::fmt;
use b::nope::Gone;
extern crate serde;
",
                ),
                ("a/src/x.rs", "use super::fmt::Write;\n"),
                (
                    "b/src/lib.rs",
                    "\
pub mod sub;
mod hidden;
pub use sub::Thing;
pub use hidden::*;
pub use sub::*;
pub use self::Loop as Loop2;
pub use self::Loop2 as Loop;
",
                ),
                (
                    "b/src/sub.rs",
                    "pub struct Thing;\npub struct Deep;\npub struct Twice;\n",
                ),
                ("b/src/hidden.rs", "pub struct Glob;\npub struct Twice;\n"),
            ],
            &packages,
        );
        let forest = forest(&files, &packages);
        assert_eq!(
            resolved(&forest, &files, &packages, "a/src/lib.rs"),
            vec![
                row("b::Thing", "b/src/sub.rs via b/src/lib.rs:3"),
                row("b::sub::Deep", "b/src/sub.rs"),
                row("b::Glob", "b/src/hidden.rs via b/src/lib.rs:4"),
                // two globs name different files: stay where the path is
                row("b::Twice", "b/src/lib.rs"),
                // a cycle of `use` declarations ends
                row("b::Loop", "b/src/lib.rs via b/src/lib.rs:7"),
                row("serde::Serialize", "ext:cargo:serde"),
                row("std::fmt", "nothing"),
                // an unknown name: the deepest module reached
                row("b::nope::Gone", "b/src/lib.rs"),
                // a declaration does not resolve through itself
                row("serde", "ext:cargo:serde"),
            ]
        );
        // a private `use` in the parent is visible to its children
        assert_eq!(
            resolved(&forest, &files, &packages, "a/src/x.rs"),
            vec![row("super::fmt::Write", "nothing")]
        );
    }

    /// The item name each `use` of `file` reaches (`None`: a module itself).
    fn names(
        forest: &Forest,
        files: &[SourceFile],
        packages: &[ResolvedPackage],
        file: &str,
    ) -> Vec<(String, Option<String>)> {
        let resolver = Resolver::new(forest, files, packages);
        let mut out = Vec::new();
        for (n, node) in forest.nodes.iter().enumerate() {
            let source = &files[node.file];
            if source.rel != Path::new(file) {
                continue;
            }
            for decl in &source.parsed.modules[node.module].uses {
                if let Resolved::Module { name, .. } = resolver.resolve(n, decl) {
                    out.push((decl.path.join("::"), name));
                }
            }
        }
        out.sort();
        out
    }

    #[test]
    fn a_use_names_the_item_it_reaches_where_the_item_is_defined() {
        let packages = [package("a", "a", &[])];
        let files = files(
            &[
                ("a/src/lib.rs", "mod model;\nmod user;\n"),
                (
                    "a/src/model.rs",
                    "pub enum Kind { A }\npub struct Real;\npub use self::Real as Renamed;\n",
                ),
                (
                    "a/src/user.rs",
                    "use crate::model::Kind;\nuse crate::model::Kind::*;\n\
                     use crate::model::{self, Renamed};\n",
                ),
            ],
            &packages,
        );
        let forest = forest(&files, &packages);
        let named = |path: &str, name: Option<&str>| (path.to_owned(), name.map(str::to_owned));
        assert_eq!(
            names(&forest, &files, &packages, "a/src/user.rs"),
            vec![
                // `self` names the module itself
                named("crate::model", None),
                // a use and a glob of an enum both take the enum
                named("crate::model::Kind", Some("Kind")),
                named("crate::model::Kind", Some("Kind")),
                // a rename resolves to the defining name
                named("crate::model::Renamed", Some("Real")),
            ]
        );
        let resolver = Resolver::new(&forest, &files, &packages);
        let root = forest
            .nodes
            .iter()
            .position(|n| files[n.file].rel == Path::new("a/src/lib.rs") && n.path.is_empty())
            .unwrap();
        let ty = resolver
            .resolve_type(root, &["model".into(), "Renamed".into()], false)
            .map(|(file, name)| (display_path(&files[file].rel), name));
        assert_eq!(ty, Some(("a/src/model.rs".to_owned(), "Real".to_owned())));
    }

    #[test]
    fn a_glob_brings_in_only_what_the_importing_module_sees() {
        let packages = [package("p", "", &[])];
        let files = files(
            &[
                ("src/lib.rs", "mod a;\nmod b;\nmod c;\npub use a::*;\n"),
                (
                    "src/a.rs",
                    "use crate::b::Hidden;\npub use crate::b::Shown;\n",
                ),
                ("src/b.rs", "pub struct Hidden;\npub struct Shown;\n"),
                ("src/c.rs", "use crate::Hidden;\nuse crate::Shown;\n"),
            ],
            &packages,
        );
        let forest = forest(&files, &packages);
        assert_eq!(
            resolved(&forest, &files, &packages, "src/c.rs"),
            vec![
                // `a` keeps its own `use` private: nothing is found
                row("crate::Hidden", "src/lib.rs"),
                row("crate::Shown", "src/b.rs via src/lib.rs:4"),
            ]
        );
    }

    #[test]
    fn a_glob_name_comes_before_an_undeclared_package() {
        let mut app = package("app", "app", &[]);
        app.other_packages
            .insert("config".into(), ComponentId::new("config"));
        let packages = [app, package("config", "config", &[])];
        let files = files(
            &[
                ("app/src/lib.rs", "mod config;\npub mod service;\n"),
                ("app/src/config.rs", "pub struct Settings;\n"),
                (
                    "app/src/service.rs",
                    "use super::*;\nuse config::Settings;\n",
                ),
                ("config/src/lib.rs", "pub struct Settings;\n"),
            ],
            &packages,
        );
        let forest = forest(&files, &packages);
        assert_eq!(
            resolved(&forest, &files, &packages, "app/src/service.rs"),
            vec![
                row("super", "app/src/lib.rs"),
                row("config::Settings", "app/src/config.rs"),
            ]
        );
    }

    #[test]
    fn exported_macros_and_unread_modules_resolve_where_they_are() {
        let packages = [package("p", "", &[])];
        let files = files(
            &[
                (
                    "src/lib.rs",
                    "\
mod macros;
mod report;
#[path = \"sys_unix.rs\"]
mod sys;
mod missing;
mod r#type;
use crate::sys::raw_open;
use missing::Gone;
use self::r#type::Kind;
",
                ),
                (
                    "src/macros.rs",
                    "#[macro_export]\nmacro_rules! shout { () => {} }\n",
                ),
                ("src/report.rs", "use crate::shout;\n"),
                ("src/type.rs", "pub struct Kind;\n"),
            ],
            &packages,
        );
        let forest = forest(&files, &packages);
        // `#[macro_export]` puts the macro at the crate root
        assert_eq!(
            resolved(&forest, &files, &packages, "src/report.rs"),
            vec![row("crate::shout", "src/macros.rs")]
        );
        // a module whose file was not read points nowhere, not at lib.rs
        assert_eq!(
            resolved(&forest, &files, &packages, "src/lib.rs"),
            vec![
                row("crate::sys::raw_open", "nothing"),
                row("missing::Gone", "nothing"),
                row("self::type::Kind", "src/type.rs"),
            ]
        );
    }

    #[test]
    fn test_modules_are_marked_through_the_tree() {
        let packages = [package("p", "", &[])];
        let files = files(
            &[
                (
                    "src/lib.rs",
                    "mod a;\n#[cfg(test)]\nmod fixtures;\n#[cfg(test)]\nmod tests {\n    mod nested {}\n}\n",
                ),
                ("src/a.rs", ""),
                ("src/fixtures.rs", "mod deep;\n"),
                ("src/fixtures/deep.rs", ""),
            ],
            &packages,
        );
        let forest = forest(&files, &packages);
        let test: BTreeMap<String, bool> = forest
            .nodes
            .iter()
            .map(|n| (n.path.join("::"), n.test))
            .collect();
        let expected: BTreeMap<String, bool> = [
            ("", false),
            ("a", false),
            ("fixtures", true),
            ("fixtures::deep", true),
            ("tests", true),
            ("tests::nested", true),
        ]
        .into_iter()
        .map(|(p, t)| (p.to_owned(), t))
        .collect();
        assert_eq!(test, expected);
    }

    #[test]
    fn glob_lookups_are_made_once_per_path() {
        // Every module globs its parent and a prelude that globs every
        // module: a search that retried each order of globs would not end.
        let n = 40;
        let mut lib = String::from("pub mod prelude;\n");
        let mut prelude = String::new();
        for i in 0..n {
            lib.push_str(&format!("mod m{i};\npub use m{i}::*;\n"));
            prelude.push_str(&format!("pub use crate::m{i}::*;\n"));
        }
        let mut sources = vec![
            ("src/lib.rs".to_owned(), lib),
            ("src/prelude.rs".to_owned(), prelude),
        ];
        for i in 0..n {
            sources.push((
                format!("src/m{i}.rs"),
                format!(
                    "use super::*;\npub use crate::prelude::*;\nuse std::fmt;\nuse crate::S{};\npub struct S{i};\n",
                    (i + 1) % n
                ),
            ));
        }
        let sources: Vec<(&str, &str)> = sources
            .iter()
            .map(|(p, t)| (p.as_str(), t.as_str()))
            .collect();
        let packages = [package("p", "", &[])];
        let files = files(&sources, &packages);
        let forest = forest(&files, &packages);

        let started = std::time::Instant::now();
        let rows = resolved(&forest, &files, &packages, "src/m6.rs");
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        assert_eq!(rows[2], row("std::fmt", "nothing"));
        assert_eq!(rows[3].0, "crate::S7");
        assert!(rows[3].1.starts_with("src/m7.rs"), "{:?}", rows[3]);
    }

    #[test]
    fn a_module_path_counts_only_when_it_names_the_module_itself() {
        let packages = [package("p", "", &[("serde", "ext:cargo:serde")])];
        let files = files(
            &[
                (
                    "src/lib.rs",
                    "mod a;\nmod b;\nmod prelude;\npub use b::Thing;\n",
                ),
                (
                    "src/a.rs",
                    "\
mod child;
use crate::b;
use crate::prelude::*;
pub fn f() {
    crate::b::make();
    b::make();
    helper::x();
    child::go();
    serde::de::Error::custom(1);
    std::mem::drop(1);
    self::g();
}
fn g() {}
pub fn h(_: crate::Thing) {}
",
                ),
                ("src/a/child.rs", "pub fn go() {}\n"),
                ("src/b.rs", "pub struct Thing;\npub fn make() {}\n"),
                ("src/prelude.rs", "pub mod helper {\n    pub fn x() {}\n}\n"),
            ],
            &packages,
        );
        let forest = forest(&files, &packages);
        assert_eq!(
            resolved_paths(&forest, &files, &packages, "src/a.rs"),
            vec![
                row("crate::b::make", "src/b.rs"),
                // `use crate::b` brought `b` in: that `use` is the dependency
                row("b::make", "nothing"),
                // and so is a name a glob brought in
                row("helper::x", "nothing"),
                row("child::go", "src/a/child.rs"),
                row("serde::de::Error::custom", "ext:cargo:serde"),
                row("std::mem::drop", "nothing"),
                row("self::g", "src/a.rs"),
                // a re-export on the way is followed, as for `use`
                row("crate::Thing", "src/b.rs via src/lib.rs:4"),
            ]
        );
    }

    #[test]
    fn problems_with_mod_declarations_are_warnings() {
        let packages = [package("p", "", &[])];
        let files = files(
            &[
                (
                    "src/lib.rs",
                    "mod missing;\n#[path = \"elsewhere.rs\"]\nmod moved;\nmod both;\nmod broken;\n",
                ),
                ("src/both.rs", ""),
                ("src/both/mod.rs", ""),
            ],
            &packages,
        );
        // broken.rs exists but did not parse: its own warning says so
        let unreadable = BTreeSet::from([PathBuf::from("src/broken.rs")]);
        let forest = build(&files, &unreadable, &packages, &[], &BTreeSet::new());
        assert_eq!(
            forest.warnings,
            vec![
                "src/lib.rs:1: no file for `mod missing` (expected src/missing.rs or src/missing/mod.rs)",
                "src/lib.rs:3: `mod moved` has a #[path] attribute, which is not followed",
                "src/lib.rs:4: both src/both.rs and src/both/mod.rs exist for `mod both`; reading the first",
            ]
        );
        assert_eq!(forest.owners[1].as_str(), "p::both");
        // the file nobody loads still belongs to the package
        assert_eq!(forest.owners[2].as_str(), "p");
    }
}
