//! Rust analyzer: Cargo manifests, module trees, `pub` items and `use`
//! declarations.
//!
//! Facts extracted:
//! - every `Cargo.toml` with a `[package]` becomes a `Package` component
//! - `[dependencies]` / `[build-dependencies]` become `Dependency` edges
//!   (path / workspace dependencies are resolved to internal packages,
//!   everything else becomes an `External` component)
//! - every file that a `mod` declaration loads, following the module trees
//!   of `src/lib.rs` and `src/main.rs`, becomes a `Module` component whose
//!   parent is the component of the declaring file (see [`tree`]); crate
//!   roots and files no root reaches belong to the package
//! - `pub` items and `pub` inherent methods under `src/` become symbols
//! - `use` declarations become `Import` edges to the module that defines
//!   what they name, through re-exports and globs, with the evidence naming
//!   that module's file; an external crate is named without a file
//! - a re-export from the subtree of the file's own module (`pub use
//!   child::Item`) is how the module presents its contents, a relation other
//!   than an import: it is followed when resolving other paths, never an edge
//! - `use` in `#[cfg(test)]` code is no dependency of the package on itself
//! - a `use` of a `[dev-dependencies]` crate becomes an [`UnmappedImport`]
//!
//! Not extracted (yet): paths written without `use` (`module::f()`), call
//! graphs, trait impls, macros, `#[path]` modules, and targets other than
//! `src/lib.rs` and `src/main.rs`.

mod manifest;
mod source;
mod tree;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use archmap_core::{
    Component, ComponentId, ComponentKind, Edge, EdgeKind, Evidence, Symbol, SymbolId,
    UnmappedImport, UnmappedReason,
};

use crate::analyzer::AnalyzerOutput;
use crate::context::display_path;
use crate::{Analyzer, RepoContext, ScanError};
use tree::{Resolved, ResolvedPackage, SourceFile};

pub use manifest::{CargoDependency, CargoPackage, CargoWorkspace, DependencyKind, ParsedManifest};

pub const LANGUAGE: &str = "rust";

/// Prefix used for component ids of dependencies outside the repository.
pub const EXTERNAL_PREFIX: &str = "ext:";

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

/// Read every file under a package's `src/`, place it in the module trees,
/// and emit modules, symbols and imports.
fn source_pass(ctx: &RepoContext, packages: &[ResolvedPackage], output: &mut AnalyzerOutput) {
    let mut files = Vec::new();
    let mut unreadable = BTreeSet::new();
    for rel in ctx.files_with_extension("rs") {
        let Some(package) = owning_package(packages, rel) else {
            continue;
        };
        if tree::module_path(&packages[package].dir, rel).is_none() {
            continue; // not under src/
        }
        let text = match ctx.read_to_string(rel) {
            Ok(text) => text,
            Err(err) => {
                output
                    .warnings
                    .push(format!("{}: {err}", display_path(rel)));
                unreadable.insert(rel.to_path_buf());
                continue;
            }
        };
        let parsed = match source::parse_file(&text) {
            Ok(parsed) => parsed,
            Err(err) => {
                output
                    .warnings
                    .push(format!("{}: parse error: {err}", display_path(rel)));
                unreadable.insert(rel.to_path_buf());
                continue;
            }
        };
        *output.read.entry(LANGUAGE.to_owned()).or_default() += 1;
        files.push(SourceFile {
            rel: rel.to_path_buf(),
            package,
            parsed,
        });
    }

    let forest = tree::build(&files, &unreadable, packages);
    output.warnings.extend(forest.warnings.iter().cloned());
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
    for (n, node) in forest.nodes.iter().enumerate() {
        let package = &packages[node.package];
        let file = display_path(&files[node.file].rel);
        let facts = &files[node.file].parsed.modules[node.module];
        let owner = &forest.owners[node.file];

        if facts.public {
            for symbol in &facts.symbols {
                let id = std::iter::once(&package.name)
                    .chain(&node.path)
                    .chain(std::iter::once(&symbol.name))
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join("::");
                output.fragment.push_symbol(Symbol {
                    id: SymbolId::new(id),
                    name: symbol.name.clone(),
                    kind: symbol.kind,
                    component: owner.clone(),
                    signature: symbol.signature.clone(),
                    evidence: vec![Evidence::new(&file).at_line(symbol.line)],
                });
            }
        }

        for decl in &facts.uses {
            let evidence = Evidence::new(&file).at_line(decl.line).in_scope(decl.scope);
            // how the name was resolved: through which other `use`, if any
            let note = |via: Option<tree::Via>| match via {
                Some((f, line)) => {
                    format!("{} via {}:{line}", decl.note, display_path(&files[f].rel))
                }
                None => decl.note.to_owned(),
            };
            match resolver.resolve(n, decl) {
                Resolved::Module { node: target, via } => {
                    let target_file = forest.nodes[target].file;
                    let within_file = target_file == node.file;
                    // what the file's module re-exports from its own subtree
                    let reexport =
                        decl.reexport && forest.is_descendant(target, forest.file_module(n));
                    // test code is compiled only for tests: no dependency of
                    // the package on itself
                    let test =
                        (node.test || decl.test) && forest.nodes[target].package == node.package;
                    if within_file || reexport || test {
                        continue;
                    }
                    let evidence = evidence
                        .with_note(note(via))
                        .pointing_at(display_path(&files[target_file].rel));
                    output.fragment.push_edge(
                        Edge::new(
                            owner.clone(),
                            forest.owners[target_file].clone(),
                            EdgeKind::Import,
                        )
                        .with_evidence(evidence),
                    );
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
                    output.fragment.push_unmapped_import(UnmappedImport {
                        from: owner.clone(),
                        module,
                        reason: UnmappedReason::DeclaredNotRequired,
                        provided_by: Vec::new(),
                        evidence: evidence.with_note(decl.note),
                    });
                }
                Resolved::Nothing => {}
            }
        }
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

        let mut component = Component::new(id.clone(), &pkg.name, ComponentKind::Package);
        component.language = Some(LANGUAGE.to_owned());
        component.path = Some(display_path(&pkg.dir));
        component
            .evidence
            .push(Evidence::new(&manifest_file).with_note("[package]"));
        output.fragment.push_component(component);

        let mut import_targets: BTreeMap<String, ComponentId> = BTreeMap::new();
        let mut dev_imports: BTreeSet<String> = BTreeSet::new();
        let workspace = nearest_workspace(&workspaces, &pkg.dir);

        for dep in &pkg.dependencies {
            if dep.kind == DependencyKind::Dev {
                dev_imports.insert(dep.import_name());
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

            if internal.is_none() {
                let mut external =
                    Component::new(target.clone(), &dep.name, ComponentKind::External);
                external.language = Some(LANGUAGE.to_owned());
                external
                    .evidence
                    .push(Evidence::new(&manifest_file).with_note(dep.kind.section()));
                output.fragment.push_component(external);
            }

            import_targets.insert(import_name, target.clone());
            output.fragment.push_edge(
                Edge::new(id.clone(), target, EdgeKind::Dependency)
                    .with_evidence(Evidence::new(&manifest_file).with_note(dep.kind.section())),
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
