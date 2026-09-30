//! Rust analyzer: Cargo manifests + `pub` items + `use` statements.
//!
//! Facts extracted:
//! - every `Cargo.toml` with a `[package]` becomes a `Package` component
//! - `[dependencies]` / `[build-dependencies]` become `Dependency` edges
//!   (path / workspace dependencies are resolved to internal packages,
//!   everything else becomes an `External` component)
//! - `pub` items and `pub` inherent methods under `src/` become symbols
//! - `use` statements whose first segment names another package become
//!   `Import` edges
//!
//! Not extracted (yet): call graphs, trait impls, macros, `dev-dependencies`.

mod manifest;
mod source;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use archmap_core::{Component, ComponentId, ComponentKind, Edge, EdgeKind, Evidence};

use crate::analyzer::AnalyzerOutput;
use crate::context::display_path;
use crate::{Analyzer, RepoContext, ScanError};

pub use manifest::{CargoDependency, CargoPackage, CargoWorkspace, DependencyKind, ParsedManifest};

pub const LANGUAGE: &str = "rust";

/// Prefix used for component ids of dependencies outside the repository.
pub const EXTERNAL_PREFIX: &str = "ext:";

#[derive(Debug, Default, Clone)]
pub struct RustAnalyzer;

/// One internal package, after manifest parsing, with everything the
/// source pass needs to resolve names.
#[derive(Debug, Clone)]
pub(crate) struct ResolvedPackage {
    pub id: ComponentId,
    pub name: String,
    /// Directory containing `Cargo.toml`, relative to the repo root.
    pub dir: PathBuf,
    /// Crate name as used in source (`archmap-core` -> `archmap_core`) mapped
    /// to the component it refers to. Includes internal packages and declared
    /// external dependencies.
    pub import_targets: BTreeMap<String, ComponentId>,
}

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

        if !ctx.options().manifests_only {
            source::source_pass(ctx, &packages, &mut output);
        }

        Ok(output)
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
        let workspace = nearest_workspace(&workspaces, &pkg.dir);

        for dep in &pkg.dependencies {
            if dep.kind == DependencyKind::Dev {
                continue;
            }
            let dep = resolve_workspace_dep(dep, workspace, &manifest_file, &mut output.warnings);
            let target = resolve_dependency_target(&dep, &by_dir, &by_name);

            if target.as_str().starts_with(EXTERNAL_PREFIX) {
                let mut external =
                    Component::new(target.clone(), &dep.name, ComponentKind::External);
                external.language = Some(LANGUAGE.to_owned());
                external
                    .evidence
                    .push(Evidence::new(&manifest_file).with_note(dep.kind.section()));
                output.fragment.push_component(external);
            }

            import_targets.insert(dep.import_name(), target.clone());
            output.fragment.push_edge(
                Edge::new(id.clone(), target, EdgeKind::Dependency)
                    .with_evidence(Evidence::new(&manifest_file).with_note(dep.kind.section())),
            );
        }

        // Other internal packages are import targets even without a declared
        // dependency; a `use` of them is still a fact worth recording.
        for other in &packages {
            if other.name != pkg.name {
                import_targets
                    .entry(other.name.replace('-', "_"))
                    .or_insert_with(|| ComponentId::new(&other.name));
            }
        }

        resolved.push(ResolvedPackage {
            id,
            name: pkg.name.clone(),
            dir: pkg.dir.clone(),
            import_targets,
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

fn resolve_dependency_target(
    dep: &CargoDependency,
    by_dir: &BTreeMap<&Path, &CargoPackage>,
    by_name: &BTreeMap<&str, &CargoPackage>,
) -> ComponentId {
    let package_name = dep.package.as_deref().unwrap_or(&dep.name);
    if let Some(path) = &dep.path {
        if let Some(pkg) = by_dir.get(path.as_path()) {
            return ComponentId::new(&pkg.name);
        }
    }
    if let Some(pkg) = by_name.get(package_name) {
        return ComponentId::new(&pkg.name);
    }
    ComponentId::new(format!("{EXTERNAL_PREFIX}{package_name}"))
}

/// Find the package whose directory is the longest prefix of `file`.
pub(crate) fn owning_package<'a>(
    packages: &'a [ResolvedPackage],
    file: &Path,
) -> Option<&'a ResolvedPackage> {
    packages
        .iter()
        .filter(|p| file.starts_with(&p.dir))
        .max_by_key(|p| p.dir.components().count())
}
