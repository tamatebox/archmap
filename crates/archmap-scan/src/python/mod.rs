//! Python analyzer: `pyproject.toml` / `requirements*.txt` + packages + imports.
//!
//! Facts extracted:
//! - every `pyproject.toml` (or `setup.py` / `setup.cfg` directory) becomes a
//!   `Package` component; a repository with `.py` files but no manifest gets
//!   one root component named after the directory
//! - declared dependencies (`[project] dependencies`, poetry, requirements
//!   files) become `Dependency` edges to `ext:*` components
//! - every directory with `__init__.py` becomes a `Module` component whose
//!   name is its dotted import path relative to the project (a `src/` without
//!   `__init__.py` is treated as the source root)
//! - importable directories without `__init__.py` that contain Python files
//!   become namespace `Module` components (PEP 420), so that `tests/` or
//!   `experiments/` are components of their own instead of being folded into
//!   the project
//! - `import` / `from ... import` statements become `Import` edges between
//!   modules, or to a declared external dependency (see [`resolve`] for how
//!   import names are matched to distributions; undeclared imports are not
//!   edges)
//! - public top-level `def` / `class` / `CONSTANT` and public methods become
//!   symbols for files inside a regular package tree (a namespace directory
//!   nested in a regular package still counts); test files and namespace
//!   trees outside any regular package, such as `experiments/`, contribute
//!   imports only
//!
//! Source files are scanned structurally (see [`source`]); bodies are not
//! parsed.

mod manifest;
mod resolve;
mod source;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use archmap_core::{
    Component, ComponentId, ComponentKind, Edge, EdgeKind, Evidence, Symbol, SymbolId,
};

use crate::analyzer::AnalyzerOutput;
use crate::context::display_path;
use crate::{Analyzer, RepoContext, ScanError};

pub use manifest::{PyDependency, PyProject};
pub use source::{PyDef, PyFile, PyImport};

pub const LANGUAGE: &str = "python";
pub const EXTERNAL_PREFIX: &str = "ext:";

#[derive(Debug, Default, Clone)]
pub struct PythonAnalyzer;

struct Project {
    id: ComponentId,
    name: String,
    /// Relative directory; empty for the repository root.
    dir: PathBuf,
    evidence: Vec<Evidence>,
    /// Normalized distribution name -> external component id.
    dependencies: BTreeMap<String, ComponentId>,
    dependency_evidence: Vec<(ComponentId, Evidence)>,
}

struct Module {
    id: ComponentId,
    dotted: String,
    dir: PathBuf,
    project: usize,
    parent: ComponentId,
    /// Has `__init__.py`. Namespace packages (PEP 420) do not.
    regular: bool,
    /// This directory or one of its ancestors is a regular package, so its
    /// files are part of an importable library rather than loose scripts.
    in_package_tree: bool,
}

impl Analyzer for PythonAnalyzer {
    fn name(&self) -> &'static str {
        LANGUAGE
    }

    fn detect(&self, ctx: &RepoContext) -> bool {
        ctx.files_named("pyproject.toml").next().is_some()
            || ctx.files_with_extension("py").next().is_some()
    }

    fn analyze(&self, ctx: &RepoContext) -> Result<AnalyzerOutput, ScanError> {
        let mut output = AnalyzerOutput::default();
        let py_files: Vec<&Path> = ctx.files_with_extension("py").collect();

        let projects = discover_projects(ctx, &py_files, &mut output.warnings)?;
        let root_name = dir_name(ctx.root(), Path::new(""));
        let modules = discover_modules(&py_files, &projects, &root_name, &mut output.warnings);
        let by_dotted: BTreeMap<&str, usize> = {
            let mut map = BTreeMap::new();
            for (idx, m) in modules.iter().enumerate() {
                map.entry(m.dotted.as_str()).or_insert(idx);
            }
            map
        };

        emit_components(&projects, &modules, &mut output);

        if ctx.options().manifests_only {
            return Ok(output);
        }

        let declared: Vec<BTreeSet<String>> = projects
            .iter()
            .map(|p| p.dependencies.keys().cloned().collect())
            .collect();
        let installed = load_installed(ctx, &projects);

        for file in py_files {
            let (owner, project_idx, base_dotted, in_package_tree) =
                match owning_module(&modules, file) {
                    Some(m) => (
                        m.id.clone(),
                        m.project,
                        Some(m.dotted.as_str()),
                        m.in_package_tree,
                    ),
                    None => match owning_project(&projects, file) {
                        Some((idx, p)) => (p.id.clone(), idx, None, false),
                        None => continue,
                    },
                };
            let text = match ctx.read_to_string(file) {
                Ok(text) => text,
                Err(err) => {
                    output
                        .warnings
                        .push(format!("{}: {err}", display_path(file)));
                    continue;
                }
            };
            let scanned = source::scan_source(&text);
            let file_display = display_path(file);

            if in_package_tree && !is_test_file(file) {
                emit_symbols(
                    &owner,
                    &symbol_scope(file),
                    &file_display,
                    &scanned,
                    &mut output,
                );
            }
            let resolver = resolve::Resolver {
                declared: &declared[project_idx],
                installed: &installed[project_idx],
            };
            emit_imports(
                &owner,
                base_dotted,
                &projects[project_idx],
                &resolver,
                &modules,
                &by_dotted,
                &file_display,
                &scanned,
                &mut output,
            );
        }

        Ok(output)
    }
}

// ---------------------------------------------------------------------------
// discovery

fn discover_projects(
    ctx: &RepoContext,
    py_files: &[&Path],
    warnings: &mut Vec<String>,
) -> Result<Vec<Project>, ScanError> {
    let mut projects: Vec<Project> = Vec::new();

    for rel in ctx.files_named("pyproject.toml") {
        let text = ctx.read_to_string(rel)?;
        match manifest::parse_pyproject(&text, rel) {
            Ok(parsed) => {
                let name = parsed
                    .name
                    .clone()
                    .unwrap_or_else(|| dir_name(ctx.root(), &parsed.dir));
                let mut project = new_project(
                    name,
                    parsed.dir.clone(),
                    Evidence::new(display_path(rel)).with_note("pyproject.toml"),
                );
                add_dependencies(&mut project, &parsed.dependencies, &display_path(rel));
                projects.push(project);
            }
            Err(err) => warnings.push(format!(
                "{}: failed to parse pyproject: {err}",
                display_path(rel)
            )),
        }
    }

    // setup.py / setup.cfg without a pyproject in the same directory.
    for marker in ["setup.py", "setup.cfg"] {
        for rel in ctx.files_named(marker) {
            let dir = rel.parent().map(Path::to_path_buf).unwrap_or_default();
            if projects.iter().any(|p| p.dir == dir) {
                continue;
            }
            projects.push(new_project(
                dir_name(ctx.root(), &dir),
                dir,
                Evidence::new(display_path(rel)).with_note(marker),
            ));
        }
    }

    // Requirements files attach to the closest enclosing project, creating a
    // root project when nothing encloses them.
    let requirements: Vec<PathBuf> = ctx
        .files()
        .iter()
        .filter(|f| manifest::is_requirements_file(f))
        .cloned()
        .collect();
    let needs_root = requirements
        .iter()
        .any(|f| owning_project(&projects, f).is_none())
        || py_files
            .iter()
            .any(|f| owning_project(&projects, f).is_none());
    if needs_root
        && !projects.iter().any(|p| p.dir.as_os_str().is_empty())
        && (!py_files.is_empty() || !requirements.is_empty())
    {
        projects.push(new_project(
            dir_name(ctx.root(), Path::new("")),
            PathBuf::new(),
            Evidence::new(".").with_note("python files without a manifest"),
        ));
    }
    for rel in requirements {
        let Some((idx, _)) = owning_project(&projects, &rel) else {
            continue;
        };
        let text = ctx.read_to_string(&rel)?;
        let deps = manifest::parse_requirements(&text);
        add_dependencies(&mut projects[idx], &deps, &display_path(&rel));
    }

    Ok(projects)
}

fn new_project(name: String, dir: PathBuf, evidence: Evidence) -> Project {
    Project {
        id: ComponentId::new(&name),
        name,
        dir,
        evidence: vec![evidence],
        dependencies: BTreeMap::new(),
        dependency_evidence: Vec::new(),
    }
}

fn add_dependencies(project: &mut Project, deps: &[PyDependency], file: &str) {
    for dep in deps {
        let target = ComponentId::new(format!("{EXTERNAL_PREFIX}{}", dep.name));
        project
            .dependencies
            .insert(dep.name.clone(), target.clone());
        let mut evidence = Evidence::new(file).with_note(&dep.section);
        if let Some(line) = dep.line {
            evidence = evidence.at_line(line);
        }
        project.dependency_evidence.push((target, evidence));
    }
}

fn dir_name(root: &Path, dir: &Path) -> String {
    let full = if dir.as_os_str().is_empty() {
        root
    } else {
        dir
    };
    full.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "root".to_owned())
}

fn discover_modules(
    py_files: &[&Path],
    projects: &[Project],
    root_name: &str,
    warnings: &mut Vec<String>,
) -> Vec<Module> {
    let package_dirs: BTreeSet<PathBuf> = py_files
        .iter()
        .filter(|f| f.file_name().is_some_and(|n| n == "__init__.py"))
        .filter_map(|f| f.parent().map(Path::to_path_buf))
        .collect();

    // Directory -> is it a regular package. Namespace packages are every
    // directory between a Python file and its source root.
    let mut module_dirs: BTreeMap<PathBuf, bool> =
        package_dirs.iter().map(|d| (d.clone(), true)).collect();
    for file in py_files {
        let Some((_, project)) = owning_project(projects, file) else {
            continue;
        };
        let root = source_root(file, project, &package_dirs);
        for dir in file.ancestors().skip(1) {
            if root.as_deref() == Some(dir) || !dir.starts_with(&project.dir) {
                break;
            }
            module_dirs.entry(dir.to_path_buf()).or_insert(false);
        }
    }

    let mut resolved: BTreeMap<PathBuf, (usize, String, bool)> = BTreeMap::new();
    for (dir, regular) in &module_dirs {
        let Some((project_idx, project)) = owning_project(projects, dir) else {
            continue;
        };
        let Some(dotted) = dotted_path(dir, project, &package_dirs, root_name) else {
            continue;
        };
        // A namespace directory is only a module if `import a.b.c` could
        // name it.
        if !regular && !dotted.split('.').all(is_identifier) {
            continue;
        }
        resolved.insert(dir.clone(), (project_idx, dotted, *regular));
    }

    let mut seen: BTreeMap<String, PathBuf> = BTreeMap::new();
    let mut modules = Vec::new();
    for (dir, (project_idx, dotted, regular)) in &resolved {
        let project = &projects[*project_idx];
        if let Some(other) = seen.get(dotted) {
            warnings.push(format!(
                "{}: package `{dotted}` also found at {}; imports resolve to the first",
                display_path(dir),
                display_path(other)
            ));
        } else {
            seen.insert(dotted.clone(), dir.clone());
        }

        // Enclosing modules, nearest first.
        let enclosing: Vec<&(usize, String, bool)> = dir
            .ancestors()
            .skip(1)
            .take_while(|a| a.starts_with(&project.dir))
            .filter_map(|a| resolved.get(a))
            .collect();
        let parent = enclosing
            .first()
            .map(|(_, d, _)| ComponentId::new(format!("{}::{d}", project.name)))
            .unwrap_or_else(|| project.id.clone());
        let in_package_tree = *regular || enclosing.iter().any(|(_, _, r)| *r);

        modules.push(Module {
            id: ComponentId::new(format!("{}::{dotted}", project.name)),
            dotted: dotted.clone(),
            dir: dir.clone(),
            project: *project_idx,
            parent,
            regular: *regular,
            in_package_tree,
        });
    }
    modules
}

/// The directory whose children are top-level import names for `file`:
/// `src/` in a `src/` layout, otherwise the project directory. `None` when
/// the project directory is itself a package, whose own name is then the
/// top-level segment.
fn source_root(
    file: &Path,
    project: &Project,
    package_dirs: &BTreeSet<PathBuf>,
) -> Option<PathBuf> {
    if package_dirs.contains(&project.dir) {
        return None;
    }
    let src = project.dir.join("src");
    if file.starts_with(&src) && !package_dirs.contains(&src) {
        Some(src)
    } else {
        Some(project.dir.clone())
    }
}

fn is_identifier(segment: &str) -> bool {
    let mut chars = segment.chars();
    chars.next().is_some_and(|c| c == '_' || c.is_alphabetic())
        && chars.all(|c| c == '_' || c.is_alphanumeric())
}

/// Import path of a package directory.
///
/// The source root is the project directory (the directory on `sys.path`
/// when tools run from the project root), except for the `src/` layout: a
/// top-level `src/` without `__init__.py` is itself the source root.
/// Directories without `__init__.py` between the source root and the package
/// are PEP 420 namespace packages and stay part of the path.
fn dotted_path(
    dir: &Path,
    project: &Project,
    package_dirs: &BTreeSet<PathBuf>,
    root_name: &str,
) -> Option<String> {
    if package_dirs.contains(&project.dir) {
        // The project directory is itself a package (scanning
        // `site-packages/pip` directly): its name is the top-level segment.
        let top = project
            .dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| root_name.to_owned());
        let rel = dir.strip_prefix(&project.dir).ok()?;
        return Some(
            std::iter::once(top)
                .chain(segments(rel))
                .collect::<Vec<_>>()
                .join("."),
        );
    }

    let mut rel = dir.strip_prefix(&project.dir).ok()?;
    let src = project.dir.join("src");
    if rel.starts_with("src") && !package_dirs.contains(&src) {
        rel = rel.strip_prefix("src").ok()?;
    }
    let parts = segments(rel);
    (!parts.is_empty()).then(|| parts.join("."))
}

fn segments(path: &Path) -> Vec<String> {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect()
}

fn owning_project<'a>(projects: &'a [Project], path: &Path) -> Option<(usize, &'a Project)> {
    projects
        .iter()
        .enumerate()
        .filter(|(_, p)| p.dir.as_os_str().is_empty() || path.starts_with(&p.dir))
        .max_by_key(|(_, p)| p.dir.components().count())
}

fn owning_module<'a>(modules: &'a [Module], file: &Path) -> Option<&'a Module> {
    let dir = file.parent()?;
    modules
        .iter()
        .filter(|m| dir.starts_with(&m.dir))
        .max_by_key(|m| m.dir.components().count())
}

// ---------------------------------------------------------------------------
// emission

fn emit_components(projects: &[Project], modules: &[Module], output: &mut AnalyzerOutput) {
    for project in projects {
        let mut component =
            Component::new(project.id.clone(), &project.name, ComponentKind::Package);
        component.language = Some(LANGUAGE.to_owned());
        component.path = Some(display_path(&project.dir));
        component.evidence = project.evidence.clone();
        output.fragment.push_component(component);

        for (target, evidence) in &project.dependency_evidence {
            let mut external = Component::new(
                target.clone(),
                target.as_str().trim_start_matches(EXTERNAL_PREFIX),
                ComponentKind::External,
            );
            external.language = Some(LANGUAGE.to_owned());
            external.evidence.push(evidence.clone());
            output.fragment.push_component(external);
            output.fragment.push_edge(
                Edge::new(project.id.clone(), target.clone(), EdgeKind::Dependency)
                    .with_evidence(evidence.clone()),
            );
        }
    }

    for module in modules {
        let mut component =
            Component::new(module.id.clone(), &module.dotted, ComponentKind::Module);
        component.language = Some(LANGUAGE.to_owned());
        component.path = Some(display_path(&module.dir));
        component.parent = Some(module.parent.clone());
        component.evidence.push(if module.regular {
            Evidence::new(display_path(&module.dir.join("__init__.py"))).with_note("package")
        } else {
            Evidence::new(display_path(&module.dir)).with_note("namespace package")
        });
        output.fragment.push_component(component);
    }
}

/// Module path used inside symbol ids: the file stem, or nothing for
/// `__init__.py`.
fn symbol_scope(file: &Path) -> Option<String> {
    let stem = file.file_stem()?.to_string_lossy();
    (stem != "__init__").then(|| stem.into_owned())
}

/// Test code is not public interface: its symbols are skipped, but its
/// imports are kept so that impact analysis still reaches tests. Uses the
/// pytest discovery conventions.
fn is_test_file(file: &Path) -> bool {
    let name = file.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let in_test_dir = file.parent().is_some_and(|p| {
        p.components()
            .any(|c| matches!(c.as_os_str().to_str(), Some("tests" | "test")))
    });
    in_test_dir || name.starts_with("test_") || name.ends_with("_test.py") || name == "conftest.py"
}

fn emit_symbols(
    owner: &ComponentId,
    scope: &Option<String>,
    file: &str,
    scanned: &PyFile,
    output: &mut AnalyzerOutput,
) {
    for def in &scanned.defs {
        let mut id = owner.as_str().to_owned();
        if let Some(scope) = scope {
            id.push_str("::");
            id.push_str(scope);
        }
        id.push_str("::");
        id.push_str(&def.name);
        output.fragment.push_symbol(Symbol {
            id: SymbolId::new(id),
            name: def.name.clone(),
            kind: def.kind,
            component: owner.clone(),
            signature: def.signature.clone(),
            evidence: vec![Evidence::new(file).at_line(def.line)],
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_imports(
    owner: &ComponentId,
    base_dotted: Option<&str>,
    project: &Project,
    resolver: &resolve::Resolver,
    modules: &[Module],
    by_dotted: &BTreeMap<&str, usize>,
    file: &str,
    scanned: &PyFile,
    output: &mut AnalyzerOutput,
) {
    for import in &scanned.imports {
        let full = match resolve_base(base_dotted, import) {
            Some(full) => full,
            None => continue,
        };

        let mut candidates: Vec<String> = import
            .names
            .iter()
            .map(|n| {
                if full.is_empty() {
                    n.clone()
                } else {
                    format!("{full}.{n}")
                }
            })
            .collect();
        if !full.is_empty() {
            candidates.push(full.clone());
        }

        // Target -> evidence note. Internal modules win; externals are only
        // considered when nothing in the repository matches.
        let mut targets: BTreeMap<ComponentId, String> = BTreeMap::new();
        let internal_note = if import.level > 0 {
            "relative import"
        } else {
            "import"
        };
        for candidate in &candidates {
            if let Some(idx) = longest_known_prefix(candidate, by_dotted) {
                targets.insert(modules[idx].id.clone(), internal_note.to_owned());
            }
        }

        if targets.is_empty() && import.level == 0 {
            for candidate in &candidates {
                let Some(resolved) = resolver.resolve(candidate) else {
                    continue;
                };
                if let Some(external) = project.dependencies.get(&resolved.distribution) {
                    targets
                        .entry(external.clone())
                        .or_insert_with(|| resolved.note());
                }
            }
        }

        for (target, note) in targets {
            if target == *owner {
                continue;
            }
            output.fragment.push_edge(
                Edge::new(owner.clone(), target, EdgeKind::Import)
                    .with_evidence(Evidence::new(file).at_line(import.line).with_note(note)),
            );
        }
    }
}

/// Installed metadata for each project: the `.venv` in the project
/// directory, else the one at the scanned root. Projects sharing a
/// virtualenv share one index.
fn load_installed(ctx: &RepoContext, projects: &[Project]) -> Vec<resolve::InstalledIndex> {
    let mut loaded: BTreeMap<PathBuf, resolve::InstalledIndex> = BTreeMap::new();
    projects
        .iter()
        .map(|project| {
            let venv = [
                ctx.absolute(&project.dir).join(".venv"),
                ctx.root().join(".venv"),
            ]
            .into_iter()
            .find(|v| v.is_dir());
            match venv {
                Some(venv) => loaded
                    .entry(venv.clone())
                    .or_insert_with(|| resolve::InstalledIndex::load(ctx.root(), &venv))
                    .clone(),
                None => resolve::InstalledIndex::default(),
            }
        })
        .collect()
}

/// Absolute dotted path an import refers to, resolving leading dots against
/// the importing file's package. `None` when a relative import climbs above
/// the top-level package.
fn resolve_base(base_dotted: Option<&str>, import: &PyImport) -> Option<String> {
    if import.level == 0 {
        return Some(import.module.clone());
    }
    let mut base: Vec<&str> = base_dotted?.split('.').collect();
    for _ in 1..import.level {
        base.pop()?;
    }
    if !import.module.is_empty() {
        base.extend(import.module.split('.'));
    }
    Some(base.join("."))
}

fn longest_known_prefix(candidate: &str, by_dotted: &BTreeMap<&str, usize>) -> Option<usize> {
    let mut parts: Vec<&str> = candidate.split('.').collect();
    while !parts.is_empty() {
        if let Some(idx) = by_dotted.get(parts.join(".").as_str()) {
            return Some(*idx);
        }
        parts.pop();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn import(module: &str, level: usize, names: &[&str]) -> PyImport {
        PyImport {
            module: module.into(),
            level,
            names: names.iter().map(|s| s.to_string()).collect(),
            line: 1,
        }
    }

    #[test]
    fn relative_imports_resolve_against_package() {
        assert_eq!(
            resolve_base(Some("shop.billing"), &import("", 1, &["x"])).as_deref(),
            Some("shop.billing")
        );
        assert_eq!(
            resolve_base(Some("shop.billing"), &import("users", 2, &["User"])).as_deref(),
            Some("shop.users")
        );
        assert_eq!(resolve_base(Some("shop"), &import("x", 3, &[])), None);
        assert_eq!(resolve_base(None, &import("x", 1, &[])), None);
        assert_eq!(
            resolve_base(None, &import("os.path", 0, &[])).as_deref(),
            Some("os.path")
        );
    }

    #[test]
    fn test_files_follow_pytest_conventions() {
        assert!(is_test_file(Path::new("tests/pipeline/helpers.py")));
        assert!(is_test_file(Path::new("pkg/test_core.py")));
        assert!(is_test_file(Path::new("pkg/core_test.py")));
        assert!(is_test_file(Path::new("conftest.py")));
        assert!(!is_test_file(Path::new("src/testing_tools/core.py")));
        assert!(!is_test_file(Path::new("src/contest.py")));
    }

    #[test]
    fn longest_prefix_lookup() {
        let map: BTreeMap<&str, usize> = [("shop", 0), ("shop.billing", 1)].into_iter().collect();
        assert_eq!(
            longest_known_prefix("shop.billing.charge.Payment", &map),
            Some(1)
        );
        assert_eq!(longest_known_prefix("shop.users", &map), Some(0));
        assert_eq!(longest_known_prefix("requests", &map), None);
    }
}
