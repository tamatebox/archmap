//! Module trees of the crate targets, and `use` path resolution over them.
//!
//! The library (`src/lib.rs`) and the binary (`src/main.rs`) of a package
//! are separate crates, each with a tree of logical modules found by
//! following `mod` declarations from its root file: `mod a;` loads `a.rs` or
//! `a/mod.rs` in the declaring module's directory. Inline modules
//! (`mod a { .. }`) are nodes too, so that paths resolve as the compiler
//! resolves them, but a component is a file: an inline module belongs to the
//! component of its file, and a file that both crates declare is one
//! component. A file under `src/` that no root reaches (a binary in
//! `src/bin/`, a `#[path]` module) belongs to its package, at the module path
//! its location suggests, with no crate root to resolve `crate::` against.
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

use super::source::{ModDecl, ModuleFacts, RustFile, UseDecl};
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
    /// Component of each file, by file index.
    pub owners: Vec<ComponentId>,
    pub modules: Vec<ModuleComponent>,
    /// `#[macro_export]` macros by crate root and name -> defining module.
    pub macros: BTreeMap<(usize, String), usize>,
    pub warnings: Vec<String>,
}

impl Forest {
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

/// Build the module trees of every package's library and binary, and give
/// every file its component. `unreadable` holds the files under `src/` that
/// could not be read or parsed, which a `mod` declaration may still name.
pub(super) fn build(
    files: &[SourceFile],
    unreadable: &BTreeSet<PathBuf>,
    packages: &[ResolvedPackage],
) -> Forest {
    let index: BTreeMap<&Path, usize> = files
        .iter()
        .enumerate()
        .map(|(i, f)| (f.rel.as_path(), i))
        .collect();
    let mut forest = Forest::default();
    let mut owners: Vec<Option<ComponentId>> = vec![None; files.len()];

    for (p, package) in packages.iter().enumerate() {
        let targets = [
            ("lib.rs", package.crate_name.clone()),
            ("main.rs", package.name.replace('-', "_")),
        ];
        for (root_file, crate_name) in targets {
            let Some(&file) = index.get(package.dir.join("src").join(root_file).as_path()) else {
                continue;
            };
            owners[file].get_or_insert_with(|| package.id.clone());
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
            if root_file == "lib.rs" {
                forest.libs.insert(p, root);
            }
            let mut grower = Grower {
                files,
                index: &index,
                unreadable,
                packages,
                crate_name,
                owners: &mut owners,
                forest: &mut forest,
                seen: BTreeSet::from([file]),
            };
            grower.grow(root, true);
        }
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
        let mut grower = Grower {
            files,
            index: &index,
            unreadable,
            packages,
            crate_name: String::new(),
            owners: &mut owners,
            forest: &mut forest,
            seen: BTreeSet::from([f]),
        };
        grower.grow(root, false);
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

struct Grower<'a> {
    files: &'a [SourceFile],
    index: &'a BTreeMap<&'a Path, usize>,
    unreadable: &'a BTreeSet<PathBuf>,
    packages: &'a [ResolvedPackage],
    /// Crate name that module names start with.
    crate_name: String,
    owners: &'a mut Vec<Option<ComponentId>>,
    forest: &'a mut Forest,
    /// Files already in this tree.
    seen: BTreeSet<usize>,
}

impl Grower<'_> {
    /// Add the inline modules of `node` and, when `follow` is set, the files
    /// its `mod` declarations load.
    fn grow(&mut self, node: usize, follow: bool) {
        let (files, packages) = (self.files, self.packages);
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
            let path = self.forest.nodes[child].path.join("::");
            let package = &packages[self.forest.nodes[node].package];
            // A file that the other crate of the package declared already
            // keeps its component.
            let id = self.owners[loaded]
                .get_or_insert_with(|| ComponentId::new(format!("{}::{path}", package.name)))
                .clone();
            let parent = self.owners[file]
                .clone()
                .expect("a declaring file has an owner");
            self.forest.modules.push(ModuleComponent {
                id,
                name: format!("{}::{path}", self.crate_name),
                file: loaded,
                parent,
                declared_in: file,
                line: decl.line,
            });
            self.grow(child, true);
        }
    }

    /// The file that `mod name;` in `node` loads, warning when there is none.
    fn load(&mut self, node: usize, decl: &ModDecl) -> Option<usize> {
        let file = self.forest.nodes[node].file;
        let at = format!("{}:{}", display_path(&self.files[file].rel), decl.line);
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
            .filter_map(|c| self.index.get(c.as_path()).copied())
            .collect();
        if found.is_empty() {
            // a file that failed to parse has a warning of its own
            if !candidates.iter().any(|c| self.unreadable.contains(c)) {
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
        let rel = &self.files[n.file].rel;
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
    /// deepest one the path reached.
    Module { node: usize, via: Option<Via> },
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
    /// Something defined in (or not found in) a module; later segments,
    /// such as enum variants, stay there.
    Item(usize),
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
        let mut walk = Walk {
            origin: self.forest.nodes[node].file,
            active: Vec::new(),
            globs: BTreeMap::new(),
        };
        let mut via = None;
        match self.path(node, &decl.path, decl.leading_colon, &mut via, &mut walk) {
            Pos::Module(m) | Pos::Item(m) => Resolved::Module { node: m, via },
            Pos::Crate(id) => Resolved::Crate { id, via },
            Pos::DevOnly(name) => Resolved::DevOnly(name),
            Pos::Nothing => Resolved::Nothing,
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
        let mut pos = if leading_colon {
            self.extern_crate(at, first)
                .or_else(|| self.other_package(at, first))
                .unwrap_or_else(|| self.unknown_crate(at, first))
        } else {
            self.first(at, first, via, walk)
        };
        for segment in rest {
            pos = match pos {
                Pos::Module(m) => self.step(m, segment, at, via, walk),
                other => return other,
            };
        }
        pos
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
                .unwrap_or(Pos::Item(m)),
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
            return Some(Pos::Item(m));
        }
        if node.root != Some(m) {
            return None;
        }
        self.forest
            .macros
            .get(&(m, name.to_owned()))
            .map(|&defined| Pos::Item(defined))
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
                Some(_) => return Some((Pos::Item(m), None)),
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
        // a binary names its own package's library
        (name == package.crate_name)
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
        build(files, &BTreeSet::new(), packages)
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
                let to = match resolver.resolve(n, decl) {
                    Resolved::Module { node, via } => {
                        let mut to = display_path(&files[forest.nodes[node].file].rel);
                        if let Some((f, line)) = via {
                            to.push_str(&format!(" via {}:{line}", display_path(&files[f].rel)));
                        }
                        to
                    }
                    Resolved::Crate { id, .. } => id.to_string(),
                    Resolved::DevOnly(name) => format!("dev {name}"),
                    Resolved::Nothing => "nothing".into(),
                };
                out.push((decl.path.join("::"), to));
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
            package("a", "a", &[("b", "b"), ("serde", "ext:serde")]),
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
                row("serde::Serialize", "ext:serde"),
                row("std::fmt", "nothing"),
                // an unknown name: the deepest module reached
                row("b::nope::Gone", "b/src/lib.rs"),
                // a declaration does not resolve through itself
                row("serde", "ext:serde"),
            ]
        );
        // a private `use` in the parent is visible to its children
        assert_eq!(
            resolved(&forest, &files, &packages, "a/src/x.rs"),
            vec![row("super::fmt::Write", "nothing")]
        );
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
        let forest = build(&files, &unreadable, &packages);
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
