//! Rust analyzer: Cargo manifests, module trees, `pub` items and `use`
//! declarations.
//!
//! Facts extracted:
//! - every `Cargo.toml` with a `[package]` becomes a `Package` component
//! - `[dependencies]` / `[build-dependencies]` become `Dependency` edges
//!   (path / workspace dependencies are resolved to internal packages,
//!   everything else becomes an `External` component)
//! - every file that a `mod` declaration loads, following the module trees
//!   of `src/lib.rs`, `src/main.rs` and the binaries, tests, examples,
//!   benches and build script Cargo finds beside them or `Cargo.toml`
//!   declares, becomes a `Module` component whose
//!   parent is the component of the declaring file (see [`tree`]); crate
//!   roots and files no root reaches belong to the package
//! - `pub` items and `pub` inherent methods become symbols
//! - the files of tests, examples and benches are test code, by the kind of
//!   their Cargo target
//! - `use` declarations become `Import` edges to the module that defines
//!   what they name, through re-exports and globs, with the evidence naming
//!   that module's file; an external crate is named without a file
//! - a module path written in code (`crate::graph::build(..)`, `child::run()`)
//!   is an `Import` too, noted `path`, once per file and target, unless its
//!   first name came from a `use`, whose edge already shows the dependency
//! - a re-export from the subtree of the file's own module (`pub use
//!   child::Item`) is how the module presents its contents, a relation other
//!   than an import: it is followed when resolving other paths, never an edge
//! - `use` in `#[cfg(test)]` code is no dependency of its crate on itself
//! - a `use` of a `[dev-dependencies]` crate becomes an [`UnmappedImport`]
//!
//! - a module path in the arguments of a macro call is an `Import` too when
//!   they are expressions, an expression and a pattern, or items; a call
//!   whose arguments are none of these is an [`UnreadMacro`]
//!
//! Not extracted (yet): which items a module uses after importing them (call
//! and reference graphs), trait impls, `#[path]` modules, and edition 2015's
//! rules for finding targets.

mod manifest;
mod source;
mod tree;

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use archmap_core::{
    Component, ComponentId, ComponentKind, Edge, EdgeKind, Evidence, Scope, Symbol, SymbolId,
    SymbolKind, UnmappedImport, UnmappedReason, UnreadMacro, WHOLE_MODULE,
};

use crate::analyzer::AnalyzerOutput;
use crate::context::display_path;
use crate::{Analyzer, RepoContext, ScanError};
use source::UseDecl;
use tree::{Resolved, ResolvedPackage, SourceFile, Target};

pub use manifest::{
    CargoDependency, CargoPackage, CargoTarget, CargoWorkspace, DeclaredTargets, DependencyKind,
    ParsedManifest, TargetKind,
};

pub const LANGUAGE: &str = "rust";

/// Prefix of the component ids of crates outside the repository. The
/// ecosystem keeps a Cargo package apart from a PyPI or npm package of the
/// same name.
pub const EXTERNAL_PREFIX: &str = "ext:cargo:";

/// Whether `file` holds the module of the directory it is in: a `mod.rs`,
/// which `mod name;` loads for `name/`.
pub fn holds_its_directory(file: &Path) -> bool {
    file.file_name().is_some_and(|name| name == "mod.rs")
}

#[derive(Debug, Default, Clone)]
pub struct RustAnalyzer;

impl Analyzer for RustAnalyzer {
    fn name(&self) -> &'static str {
        LANGUAGE
    }

    fn detect(&self, ctx: &RepoContext) -> bool {
        ctx.files_named("Cargo.toml").next().is_some()
    }

    fn analyze(&self, ctx: &RepoContext) -> Result<AnalyzerOutput, ScanError> {
        let mut output = AnalyzerOutput::default();

        let manifests = load_manifests(ctx, &mut output.warnings)?;
        let packages = manifest_pass(&manifests, &mut output);

        output.read.insert(LANGUAGE.to_owned(), 0);
        if !ctx.options().manifests_only {
            source_pass(ctx, &packages, &mut output);
        }

        Ok(output)
    }
}

/// Read every file under a package's `src/`, the roots of its other Cargo
/// targets and the files their `mod` declarations load, place them in the
/// module trees, and emit modules, symbols and imports. Nothing else outside
/// `src/` is read: what no target reaches there is test data or input.
fn source_pass(ctx: &RepoContext, packages: &[ResolvedPackage], output: &mut AnalyzerOutput) {
    let on_disk: BTreeSet<&Path> = ctx.files_with_extension("rs").collect();
    // the package that owns each file, found once
    let owner: BTreeMap<&Path, usize> = on_disk
        .iter()
        .filter_map(|rel| owning_package(packages, rel).map(|p| (*rel, p)))
        .collect();
    let mut owned: Vec<Vec<&Path>> = vec![Vec::new(); packages.len()];
    for (rel, &p) in &owner {
        owned[p].push(rel);
    }
    let targets: Vec<Vec<Target>> = packages
        .iter()
        .zip(&owned)
        .map(|(package, own)| tree::targets(package, own.iter().copied()))
        .collect();

    let mut sources = Sources::default();
    let roots = targets.iter().flatten().map(|t| t.root.as_path());
    let under_src = owner
        .iter()
        .filter(|(rel, &p)| tree::module_path(&packages[p].dir, rel).is_some())
        .map(|(rel, _)| *rel);
    for rel in under_src.chain(roots) {
        sources.read(ctx, rel, owner[rel], output);
    }
    // the modules outside `src/` that roots reach, read as the trees grow
    let forest = loop {
        let forest = tree::build(
            &sources.files,
            &sources.unreadable,
            packages,
            &targets,
            &on_disk,
        );
        let before = sources.tried.len();
        for rel in &forest.missing {
            if let Some(&p) = owner.get(rel.as_path()) {
                sources.read(ctx, rel, p, output);
            }
        }
        if sources.tried.len() == before {
            break forest;
        }
    };
    let files = sources.files;
    output.warnings.extend(forest.warnings.iter().cloned());
    // the library's root is what the package's dependents link
    for (package, own) in packages.iter().zip(&targets) {
        if let Some(lib) = own.iter().find(|t| t.kind == TargetKind::Lib) {
            let mut component =
                Component::new(package.id.clone(), &package.name, ComponentKind::Package);
            component
                .evidence
                .push(Evidence::new(display_path(&lib.root)).with_note("entry"));
            output.fragment.push_component(component);
        }
    }
    for module in &forest.modules {
        let mut component = Component::new(module.id.clone(), &module.name, ComponentKind::Module);
        component.language = Some(LANGUAGE.to_owned());
        component.path = Some(display_path(&files[module.file].rel));
        component.parent = Some(module.parent.clone());
        component.evidence.push(
            Evidence::new(display_path(&files[module.declared_in].rel))
                .at_line(module.line)
                .with_note("mod"),
        );
        output.fragment.push_component(component);
    }

    let resolver = tree::Resolver::new(&forest, &files, packages);
    let mut paths: BTreeMap<(usize, PathTarget), PathHit> = BTreeMap::new();
    // the names all paths from a file to a file take
    let mut path_names: BTreeMap<(usize, PathTarget), BTreeSet<String>> = BTreeMap::new();
    // the modules whose symbols are out, once for a file several crates load
    let mut emitted: BTreeSet<(usize, usize)> = BTreeSet::new();
    for (n, node) in forest.nodes.iter().enumerate() {
        let package = &packages[node.package];
        let file = display_path(&files[node.file].rel);
        let facts = &files[node.file].parsed.modules[node.module];
        let owner = &forest.owners[node.file];
        // all code of a test, an example or a bench is test code
        let test_target = forest.in_test_target(n);

        if facts.public && emitted.insert((node.file, node.module)) {
            // a symbol of a target other than the library and `src/main.rs`
            // takes its file's place in the package, as no module path names
            // it apart from the library's
            let base: Vec<String> = if forest.tree(n).is_some_and(|t| t.by_path) {
                let rel = &files[node.file].rel;
                let rel = display_path(rel.strip_prefix(&package.dir).unwrap_or(rel));
                let in_file = forest.nodes[forest.file_module(n)].path.len();
                std::iter::once(format!("{}::{rel}", package.name))
                    .chain(node.path[in_file..].iter().cloned())
                    .collect()
            } else {
                std::iter::once(package.name.clone())
                    .chain(node.path.iter().cloned())
                    .collect()
            };
            for symbol in &facts.symbols {
                // a `pub mod` with a file of its own is that file's component
                let module_file = (symbol.kind == SymbolKind::Module)
                    .then(|| node.children.get(&symbol.name))
                    .flatten()
                    .map(|&child| forest.nodes[child].file)
                    .filter(|&f| f != node.file && forest.owners[f] != package.id);
                let id = match module_file {
                    Some(f) => forest.owners[f].as_str().to_owned(),
                    None => base
                        .iter()
                        .chain(std::iter::once(&symbol.name))
                        .map(String::as_str)
                        .collect::<Vec<_>>()
                        .join("::"),
                };
                // test code defines it: a test target, a `#[cfg(test)]`
                // module, or its own mark
                let test = node.test || symbol.test || test_target;
                let mut evidence = vec![Evidence::new(&file).at_line(symbol.line).in_test(test)];
                // a method is reached through its type, which an inherent impl
                // may take from another file of its crate
                let reached = symbol.owner.as_ref().and_then(|ty| {
                    let found = resolver.resolve_type(n, &ty.segments, ty.leading_colon)?;
                    Some((ty.line, found))
                });
                if let Some((line, (type_file, name))) = reached {
                    if type_file != node.file {
                        evidence.push(
                            Evidence::new(&file)
                                .at_line(line)
                                .with_note("impl")
                                .in_test(test)
                                .pointing_at(display_path(&files[type_file].rel))
                                .taking([name]),
                        );
                    }
                }
                output.fragment.push_symbol(Symbol {
                    id: SymbolId::new(id),
                    name: symbol.name.clone(),
                    kind: symbol.kind,
                    component: owner.clone(),
                    signature: symbol.signature.clone(),
                    evidence,
                });
            }
        }

        // the macro calls whose arguments were not read: what they name is
        // unseen
        for call in &facts.unread_macros {
            output.fragment.push_unread_macro(UnreadMacro {
                from: owner.clone(),
                name: call.name.clone(),
                names: call.names.iter().cloned().collect(),
                evidence: Evidence::new(&file)
                    .at_line(call.line)
                    .in_scope(call.scope)
                    .in_test(node.test || call.test || test_target),
            });
        }

        // the leaves of a declaration that reach one file (`use a::{X, Y}`)
        // share one piece of evidence, with the names they take together
        let mut taken: BTreeMap<(u32, Scope, String, bool, usize), BTreeSet<String>> =
            BTreeMap::new();
        // the declarations that bring in a module whole, by the name they
        // bind (`use crate::graph;`), with their evidence
        let mut brought: BTreeMap<&str, (&UseDecl, bool)> = BTreeMap::new();
        for decl in &facts.uses {
            let in_test = node.test || decl.test || test_target;
            let evidence = Evidence::new(&file)
                .at_line(decl.line)
                .in_scope(decl.scope)
                .in_test(in_test);
            let note = |via| note(decl.note, via, &files);
            match resolver.resolve(n, decl) {
                Resolved::Module {
                    node: target,
                    via,
                    name,
                } => {
                    let target_file = forest.nodes[target].file;
                    let within_file = target_file == node.file;
                    // what the file's module re-exports from its own subtree
                    let reexport =
                        decl.reexport && forest.is_descendant(target, forest.file_module(n));
                    // a crate's unit tests are no dependency of the crate on
                    // itself; a test target is a crate of its own
                    let test =
                        (node.test || decl.test) && !test_target && same_crate(&forest, n, target);
                    if within_file || reexport || test {
                        continue;
                    }
                    let key = (decl.line, decl.scope, note(via), in_test, target_file);
                    if let (None, Some(binds)) = (&name, decl.binds.as_deref()) {
                        // one at module scope over one inside a function
                        let kept = brought.get(binds).map(|(d, _)| d.scope);
                        if kept != Some(Scope::Module) {
                            brought.insert(binds, (decl, in_test));
                        }
                    }
                    taken
                        .entry(key)
                        .or_default()
                        .insert(name.unwrap_or_else(|| WHOLE_MODULE.to_owned()));
                }
                Resolved::Crate { id, via } => {
                    if id != package.id {
                        output.fragment.push_edge(
                            Edge::new(owner.clone(), id, EdgeKind::Import)
                                .with_evidence(evidence.with_note(note(via))),
                        );
                    }
                }
                // declared only in `[dev-dependencies]`, for tests: an import
                // without an edge
                Resolved::DevOnly(module) => {
                    let note = dev_note(decl.note, &module, package);
                    output.fragment.push_unmapped_import(UnmappedImport {
                        from: owner.clone(),
                        module,
                        reason: UnmappedReason::DeclaredNotRequired,
                        provided_by: Vec::new(),
                        evidence: evidence.with_note(note),
                    });
                }
                Resolved::Nothing => {}
            }
        }
        // a path in code through such a module (`graph::build()`) takes what
        // it names as part of that declaration, resolved as if the
        // declaration had named it, so a re-export leads to the file that
        // defines it; a path in test code adds nothing to production's
        for path in &facts.paths {
            let Some((first, rest)) = path.segments.split_first() else {
                continue;
            };
            let Some(&(decl, in_test)) = brought.get(first.as_str()) else {
                continue;
            };
            if rest.is_empty() || ((node.test || path.test) && !in_test) {
                continue;
            }
            // the whole path, else its first item (`Node` of `graph::Node::new`)
            let reached = [rest, &rest[..1]].into_iter().find_map(|tail| {
                let leaf = UseDecl {
                    path: decl.path.iter().chain(tail).cloned().collect(),
                    ..decl.clone()
                };
                match resolver.resolve(n, &leaf) {
                    Resolved::Module {
                        node: target,
                        via,
                        name,
                    } => Some((target, via, name)),
                    _ => None,
                }
            });
            let Some((target, via, name)) = reached else {
                continue;
            };
            let target_file = forest.nodes[target].file;
            if target_file == node.file {
                continue;
            }
            taken
                .entry((
                    decl.line,
                    decl.scope,
                    note(decl.note, via, &files),
                    in_test,
                    target_file,
                ))
                .or_default()
                .insert(name.unwrap_or_else(|| WHOLE_MODULE.to_owned()));
        }
        for ((line, scope, note, in_test, target_file), names) in taken {
            output.fragment.push_edge(
                Edge::new(
                    owner.clone(),
                    forest.owners[target_file].clone(),
                    EdgeKind::Import,
                )
                .with_evidence(
                    Evidence::new(&file)
                        .at_line(line)
                        .in_scope(scope)
                        .in_test(in_test)
                        .with_note(note)
                        .pointing_at(display_path(&files[target_file].rel))
                        .taking(names),
                ),
            );
        }

        for path in &facts.paths {
            let (target, via, name) = match resolver.resolve_path(n, path) {
                Resolved::Module {
                    node: target,
                    via,
                    name,
                } => {
                    let target_file = forest.nodes[target].file;
                    let test =
                        (node.test || path.test) && !test_target && same_crate(&forest, n, target);
                    if target_file == node.file || test {
                        continue;
                    }
                    (PathTarget::File(target_file), via, name)
                }
                Resolved::Crate { id, via } if id != package.id => {
                    (PathTarget::Crate(id), via, None)
                }
                Resolved::DevOnly(module) => (PathTarget::DevOnly(module), None, None),
                Resolved::Crate { .. } | Resolved::Nothing => continue,
            };
            if let PathTarget::File(_) = target {
                path_names
                    .entry((node.file, target.clone()))
                    .or_default()
                    .insert(name.unwrap_or_else(|| WHOLE_MODULE.to_owned()));
            }
            let hit = PathHit {
                rank: (
                    node.test || path.test || test_target,
                    path.scope != Scope::Module,
                    path.line,
                ),
                scope: path.scope,
                via,
            };
            match paths.entry((node.file, target)) {
                Entry::Vacant(entry) => {
                    entry.insert(hit);
                }
                Entry::Occupied(mut entry) if hit.rank < entry.get().rank => {
                    entry.insert(hit);
                }
                Entry::Occupied(_) => {}
            }
        }
    }

    // however many paths lead from a file to a target, one piece of evidence
    // with the names they take together
    for ((file, target), hit) in paths {
        let names = path_names
            .remove(&(file, target.clone()))
            .unwrap_or_default();
        let owner = forest.owners[file].clone();
        // test code ranks last, so the path chosen is test code only when
        // every path is
        let evidence = Evidence::new(display_path(&files[file].rel))
            .at_line(hit.rank.2)
            .in_scope(hit.scope)
            .in_test(hit.rank.0);
        match target {
            PathTarget::File(target_file) => output.fragment.push_edge(
                Edge::new(owner, forest.owners[target_file].clone(), EdgeKind::Import)
                    .with_evidence(
                        evidence
                            .with_note(note("path", hit.via, &files))
                            .pointing_at(display_path(&files[target_file].rel))
                            .taking(names),
                    ),
            ),
            PathTarget::Crate(id) => output.fragment.push_edge(
                Edge::new(owner, id, EdgeKind::Import)
                    .with_evidence(evidence.with_note(note("path", hit.via, &files))),
            ),
            PathTarget::DevOnly(module) => {
                let note = dev_note("path", &module, &packages[files[file].package]);
                output.fragment.push_unmapped_import(UnmappedImport {
                    from: owner,
                    module,
                    reason: UnmappedReason::DeclaredNotRequired,
                    provided_by: Vec::new(),
                    evidence: evidence.with_note(note),
                })
            }
        }
    }
}

/// The note of an import of the dev-dependency `module`, written as `kind`:
/// `use assert_cmd, declared in crates/app/Cargo.toml:10 ([dev-dependencies])`.
fn dev_note(kind: &str, module: &str, package: &ResolvedPackage) -> String {
    match package.dev_imports.get(module) {
        Some(declared) => format!("{kind} {module}, {declared}"),
        None => kind.to_owned(),
    }
}

/// Whether module `target` is in the crate of module `node`: the same root
/// of the same package, the files no root reaches counting as one crate.
fn same_crate(forest: &tree::Forest, node: usize, target: usize) -> bool {
    let (a, b) = (&forest.nodes[node], &forest.nodes[target]);
    a.package == b.package && a.root == b.root
}

/// The Rust files read so far.
#[derive(Default)]
struct Sources {
    files: Vec<SourceFile>,
    /// The files that could not be read or parsed.
    unreadable: BTreeSet<PathBuf>,
    /// Every file tried, read or not.
    tried: BTreeSet<PathBuf>,
}

impl Sources {
    /// Read and parse `rel`, a file of package `package`, once; warn and
    /// keep it among the unreadable files when that fails.
    fn read(&mut self, ctx: &RepoContext, rel: &Path, package: usize, output: &mut AnalyzerOutput) {
        if !self.tried.insert(rel.to_path_buf()) {
            return;
        }
        let parsed = match ctx.read_to_string(rel) {
            Ok(text) => source::parse_file(&text).map_err(|err| format!("parse error: {err}")),
            Err(err) => Err(err.to_string()),
        };
        match parsed {
            Ok(parsed) => {
                *output.read.entry(LANGUAGE.to_owned()).or_default() += 1;
                self.files.push(SourceFile {
                    rel: rel.to_path_buf(),
                    package,
                    parsed,
                });
            }
            Err(err) => {
                output
                    .warnings
                    .push(format!("{}: {err}", display_path(rel)));
                self.unreadable.insert(rel.to_path_buf());
            }
        }
    }
}

/// Where a module path in code leads, to merge the paths of one file.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum PathTarget {
    /// A file of the repository.
    File(usize),
    /// A crate without a module tree.
    Crate(ComponentId),
    /// A crate that only a dev-dependency provides.
    DevOnly(String),
}

/// The path that stands for all paths from one file to one target: the
/// first at module scope, else the first, so that a cycle check still sees
/// a dependency at module scope.
struct PathHit {
    /// Test code last, local scope after module scope, then by line.
    rank: (bool, bool, u32),
    scope: Scope,
    via: Option<tree::Via>,
}

/// An evidence note: how a name was resolved, and through which other `use`.
fn note(kind: &str, via: Option<tree::Via>, files: &[SourceFile]) -> String {
    match via {
        Some((file, line)) => format!("{kind} via {}:{line}", display_path(&files[file].rel)),
        None => kind.to_owned(),
    }
}

fn load_manifests(
    ctx: &RepoContext,
    warnings: &mut Vec<String>,
) -> Result<Vec<(PathBuf, ParsedManifest)>, ScanError> {
    let mut manifests = Vec::new();
    for rel in ctx.files_named("Cargo.toml") {
        let text = ctx.read_to_string(rel)?;
        match manifest::parse_manifest(&text, rel) {
            Ok(parsed) => manifests.push((rel.to_path_buf(), parsed)),
            Err(err) => warnings.push(format!(
                "{}: failed to parse manifest: {err}",
                display_path(rel)
            )),
        }
    }
    Ok(manifests)
}

/// Turn parsed manifests into components and dependency edges.
fn manifest_pass(
    manifests: &[(PathBuf, ParsedManifest)],
    output: &mut AnalyzerOutput,
) -> Vec<ResolvedPackage> {
    let packages: Vec<&CargoPackage> = manifests
        .iter()
        .filter_map(|(_, m)| m.package.as_ref())
        .collect();
    let workspaces: Vec<&CargoWorkspace> = manifests
        .iter()
        .filter_map(|(_, m)| m.workspace.as_ref())
        .collect();

    let by_dir: BTreeMap<&Path, &CargoPackage> =
        packages.iter().map(|p| (p.dir.as_path(), *p)).collect();
    let by_name: BTreeMap<&str, &CargoPackage> =
        packages.iter().map(|p| (p.name.as_str(), *p)).collect();

    let mut resolved = Vec::new();

    for pkg in &packages {
        let id = ComponentId::new(&pkg.name);
        let manifest_file = display_path(&pkg.manifest_path);
        if let Some(problem) = &pkg.declared.problem {
            output.warnings.push(format!(
                "{manifest_file}: targets not read: {problem}; Cargo's default targets assumed"
            ));
        }

        let mut component = Component::new(id.clone(), &pkg.name, ComponentKind::Package);
        component.language = Some(LANGUAGE.to_owned());
        component.path = Some(display_path(&pkg.dir));
        component
            .evidence
            .push(Evidence::new(&manifest_file).with_note("[package]"));
        output.fragment.push_component(component);

        let mut import_targets: BTreeMap<String, ComponentId> = BTreeMap::new();
        let mut dev_imports: BTreeMap<String, String> = BTreeMap::new();
        let workspace = nearest_workspace(&workspaces, &pkg.dir);

        for dep in &pkg.dependencies {
            if dep.kind == DependencyKind::Dev {
                // where it is declared, for the notes of its imports
                let at = match dep.line {
                    Some(line) => format!("{manifest_file}:{line}"),
                    None => manifest_file.clone(),
                };
                let name = match dep.name == dep.import_name() {
                    true => String::new(),
                    false => format!(" as {}", dep.name),
                };
                let declared = format!("declared{name} in {at} ({})", dep.kind.section());
                dev_imports.insert(dep.import_name(), declared);
                continue;
            }
            let dep = resolve_workspace_dep(dep, workspace, &manifest_file, &mut output.warnings);
            let internal = internal_package(&dep, &by_dir, &by_name);
            let target = match internal {
                Some(p) => ComponentId::new(&p.name),
                None => ComponentId::new(format!(
                    "{EXTERNAL_PREFIX}{}",
                    dep.package.as_deref().unwrap_or(&dep.name)
                )),
            };
            // Source names a renamed dependency by its key, any other by its
            // library name.
            let import_name = match internal {
                Some(p) if dep.package.is_none() => p.crate_name(),
                _ => dep.import_name(),
            };

            let mut declared = Evidence::new(&manifest_file).with_note(dep.kind.section());
            if let Some(line) = dep.line {
                declared = declared.at_line(line);
            }
            if internal.is_none() {
                let mut external =
                    Component::new(target.clone(), &dep.name, ComponentKind::External);
                external.language = Some(LANGUAGE.to_owned());
                external.evidence.push(declared.clone());
                output.fragment.push_component(external);
            }

            import_targets.insert(import_name, target.clone());
            output.fragment.push_edge(
                Edge::new(id.clone(), target, EdgeKind::Dependency).with_evidence(declared),
            );
        }

        // Other internal packages are import targets even without a declared
        // dependency (the manifest reader skips target-specific tables); a
        // `use` of them is still a fact worth recording.
        let other_packages = packages
            .iter()
            .filter(|other| other.name != pkg.name)
            .map(|other| (other.crate_name(), ComponentId::new(&other.name)))
            .filter(|(name, _)| !import_targets.contains_key(name))
            .collect();

        resolved.push(ResolvedPackage {
            id,
            name: pkg.name.clone(),
            crate_name: pkg.crate_name(),
            dir: pkg.dir.clone(),
            import_targets,
            other_packages,
            dev_imports,
            declared: pkg.declared.clone(),
        });
    }

    resolved
}

fn nearest_workspace<'a>(
    workspaces: &[&'a CargoWorkspace],
    dir: &Path,
) -> Option<&'a CargoWorkspace> {
    workspaces
        .iter()
        .filter(|w| dir.starts_with(&w.dir))
        .max_by_key(|w| w.dir.components().count())
        .copied()
}

/// Replace `dep = { workspace = true }` with the workspace definition.
fn resolve_workspace_dep(
    dep: &CargoDependency,
    workspace: Option<&CargoWorkspace>,
    manifest_file: &str,
    warnings: &mut Vec<String>,
) -> CargoDependency {
    if !dep.workspace {
        return dep.clone();
    }
    match workspace.and_then(|w| w.dependencies.get(&dep.name)) {
        Some(ws_dep) => CargoDependency {
            name: dep.name.clone(),
            package: ws_dep.package.clone().or_else(|| dep.package.clone()),
            path: ws_dep.path.clone(),
            workspace: false,
            kind: dep.kind,
            // where the member declares it
            line: dep.line,
        },
        None => {
            warnings.push(format!(
                "{manifest_file}: dependency `{}` uses `workspace = true` but no workspace definition was found",
                dep.name
            ));
            CargoDependency {
                workspace: false,
                ..dep.clone()
            }
        }
    }
}

/// The internal package a dependency points at, by path or by name.
fn internal_package<'a>(
    dep: &CargoDependency,
    by_dir: &BTreeMap<&Path, &'a CargoPackage>,
    by_name: &BTreeMap<&str, &'a CargoPackage>,
) -> Option<&'a CargoPackage> {
    let package_name = dep.package.as_deref().unwrap_or(&dep.name);
    dep.path
        .as_deref()
        .and_then(|path| by_dir.get(path))
        .or_else(|| by_name.get(package_name))
        .copied()
}

/// Index of the package whose directory is the longest prefix of `file`.
fn owning_package(packages: &[ResolvedPackage], file: &Path) -> Option<usize> {
    packages
        .iter()
        .enumerate()
        .filter(|(_, p)| file.starts_with(&p.dir))
        .max_by_key(|(_, p)| p.dir.components().count())
        .map(|(i, _)| i)
}
