//! Python analyzer: `pyproject.toml` / `requirements*.txt` + packages + imports.
//!
//! Facts extracted:
//! - every `pyproject.toml` (or `setup.py` / `setup.cfg` directory) becomes a
//!   `Package` component; a repository with `.py` files but no manifest gets
//!   one root component named after the directory
//! - declared dependencies (`[project] dependencies`, poetry, requirements
//!   files) become `Dependency` edges to `ext:pypi:*` components. A declaration
//!   covers the files below its manifest: `pyproject.toml` the whole
//!   project, a requirements file the closest directory at or above it with
//!   Python code (`functions/notify/`, but the project for
//!   `requirements/prod.txt` or `docker/requirements.txt`). Imports resolve
//!   against the declarations that cover their file, and the edge comes
//!   from the module of the covered directory. A requirements file named for
//!   development (`requirements-dev.txt`, `docs/requirements.txt`) declares
//!   dev dependencies, which make no edges, like the extras and groups of
//!   `pyproject.toml`
//! - every directory with `__init__.py` becomes a `Module` component whose
//!   name is its dotted import path relative to the project (a `src/` without
//!   `__init__.py` is treated as the source root)
//! - importable directories without `__init__.py` that contain Python files
//!   become namespace `Module` components (PEP 420), so that `tests/` or
//!   `experiments/` are components of their own instead of being folded into
//!   the project
//! - `import` / `from ... import` statements become `Import` edges between
//!   modules, or to a required external dependency (see [`resolve`] for how
//!   import names are matched to distributions)
//! - a name taken from a file that binds it by importing it from another
//!   (`pkg/__init__.py` with `from .charge import pay`) also gets evidence
//!   for the file that defines it, noted `import via <file>:<line>` with the
//!   first binding on the way (see [`reexports`])
//! - a bare import that matches no module but a `.py` file next to the
//!   importing file (`import helpers` beside `helpers.py`) loads that file,
//!   as it does when the directory is on `sys.path` for a script run
//!   directly or a function deployed from it; the evidence note says so
//! - an absolute import that maps to no component and is not in the standard
//!   library is recorded as an [`UnmappedImport`], never as an edge, with
//!   its reason: declared only as an extra, group or dev dependency; a file
//!   or directory name in the project (tests and scripts often extend
//!   `sys.path` at runtime, which a static scan cannot see); or undeclared,
//!   which `check` can report. A name imported from a package that an
//!   installed distribution provides as a module of its own (`from
//!   google.cloud import bigquery`) is recorded as that module
//! - calls that load a module by a computed name (`import_module`,
//!   `__import__`, `spec_from_file_location`) become [`DynamicImport`]s
//! - public top-level `def` / `class` / `CONSTANT` and public methods become
//!   symbols for files inside a regular package tree (a namespace directory
//!   nested in a regular package still counts); a file outside any regular
//!   package tree (in a namespace tree such as `experiments/`, or at the top
//!   of the project) gives those that other files import from it, all of
//!   them when one takes it whole; test files by name give none, while a
//!   helper below `tests/` does
//!
//! Source files are scanned structurally (see [`source`]); bodies are not
//! parsed.

mod manifest;
mod reads;
mod reexports;
mod resolve;
mod source;
mod stdlib;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use archmap_core::{
    Component, ComponentId, ComponentKind, DynamicImport, Edge, EdgeKind, Evidence, Scope, Symbol,
    SymbolId, UnmappedImport, UnmappedReason, WHOLE_MODULE,
};

use crate::analyzer::AnalyzerOutput;
use crate::context::display_path;
use crate::test_code::{is_test_code, is_test_named};
use crate::{Analyzer, RepoContext, ScanError};

pub use manifest::{PyDependency, PyProject};
pub use source::{PyDef, PyFile, PyImport};

pub const LANGUAGE: &str = "python";
/// Prefix of the component ids of distributions outside the repository.
/// The ecosystem keeps a PyPI distribution apart from a Cargo or npm package
/// of the same name.
pub const EXTERNAL_PREFIX: &str = "ext:pypi:";

#[derive(Debug, Default, Clone)]
pub struct PythonAnalyzer;

struct Project {
    id: ComponentId,
    name: String,
    /// Relative directory; empty for the repository root.
    dir: PathBuf,
    evidence: Vec<Evidence>,
    /// Declared dependencies, each with the directory it is declared for.
    declarations: Vec<Declaration>,
}

/// A dependency a manifest declares for the files under `scope`.
struct Declaration {
    /// PEP 503 normalized distribution name.
    name: String,
    /// The project directory for `pyproject.toml`; for a requirements file,
    /// see [`requirements_scope`].
    scope: PathBuf,
    /// A runtime dependency. Extras, groups and dev dependencies are
    /// declared too, but they are not edges.
    required: bool,
    evidence: Evidence,
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

        let modules_by_dir: BTreeMap<&Path, &Module> =
            modules.iter().map(|m| (m.dir.as_path(), m)).collect();
        emit_components(&projects, &modules, &modules_by_dir, &mut output);

        output.read.insert(LANGUAGE.to_owned(), 0);
        if ctx.options().manifests_only {
            return Ok(output);
        }

        // What each project declares anywhere, to say where an import that
        // is undeclared for its own file is declared instead.
        let declared_anywhere: Vec<BTreeSet<String>> = projects
            .iter()
            .map(|p| p.declarations.iter().map(|d| d.name.clone()).collect())
            .collect();
        // Directory -> what is declared for its files: required, optional.
        let mut declared_for: BTreeMap<(usize, PathBuf), [BTreeSet<String>; 2]> = BTreeMap::new();
        let installed = load_installed(ctx, &projects);
        let names = local_names(&py_files);
        let local_names: Vec<BTreeSet<String>> = projects
            .iter()
            .map(|p| {
                names
                    .iter()
                    .filter(|(dir, _)| dir.starts_with(&p.dir))
                    .flat_map(|(_, n)| n.iter().cloned())
                    .collect()
            })
            .collect();

        let known_files: BTreeSet<&Path> = py_files.iter().copied().collect();
        let mut reads: Vec<ReadFile> = Vec::new();
        for file in py_files {
            let (owner, project_idx, base_dotted, in_package_tree) =
                match owning_module(&modules_by_dir, file) {
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
            *output.read.entry(LANGUAGE.to_owned()).or_default() += 1;
            let file_display = display_path(file);

            // a test file is no public interface, while a helper below
            // `tests/` keeps its symbols, as for TS/JS; the imports of both
            // count, so that impact still reaches tests
            if in_package_tree && !is_test_named(file) {
                emit_symbols(
                    &owner,
                    &symbol_scope(file),
                    &file_display,
                    &scanned,
                    None,
                    &mut output,
                );
            }
            let project = &projects[project_idx];
            let dir = file.parent().unwrap_or(Path::new(""));
            let [required, optional] = declared_for
                .entry((project_idx, dir.to_path_buf()))
                .or_insert_with(|| {
                    let mut sets: [BTreeSet<String>; 2] = Default::default();
                    for d in project
                        .declarations
                        .iter()
                        .filter(|d| dir.starts_with(&d.scope))
                    {
                        sets[usize::from(!d.required)].insert(d.name.clone());
                    }
                    sets
                });
            let scope = ImportScope {
                project,
                declared: resolve::Resolver {
                    declared: required,
                    installed: &installed[project_idx],
                },
                anywhere: resolve::Resolver {
                    declared: &declared_anywhere[project_idx],
                    installed: &installed[project_idx],
                },
                optional: resolve::Resolver {
                    declared: optional,
                    installed: &installed[project_idx],
                },
                installed: &installed[project_idx],
                local_names: &local_names[project_idx],
                known_files: &known_files,
            };
            let resolved = emit_imports(
                &owner,
                base_dotted,
                &scope,
                &modules,
                &by_dotted,
                file,
                &scanned,
                &mut output,
            );
            reads.push(ReadFile {
                file: file.to_path_buf(),
                display: file_display,
                owner,
                in_package_tree,
                scanned,
                resolved,
            });
        }
        let tables: BTreeMap<String, reexports::Table> = reads
            .iter()
            .map(|read| (read.display.clone(), binding_table(read)))
            .collect();
        emit_definitions(&reads, &tables, &mut output);

        // a file outside a package tree declares no interface: what other
        // files import from it is one, all of it when they take it whole
        let mut imported: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for e in output.fragment.edges.iter().flat_map(|e| &e.evidence) {
            if let Some(target) = e.target.as_deref().filter(|t| *t != e.file) {
                if !e.names.is_empty() {
                    imported
                        .entry(target.to_owned())
                        .or_default()
                        .extend(e.names.iter().cloned());
                }
            }
        }
        for read in &reads {
            if read.in_package_tree || is_test_named(&read.file) {
                continue;
            }
            if let Some(names) = imported.get(&read.display) {
                let scope = symbol_scope(&read.file);
                let only = (!names.contains(WHOLE_MODULE)).then_some(names);
                let file = &read.display;
                emit_symbols(&read.owner, &scope, file, &read.scanned, only, &mut output);
            }
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
                let file = display_path(rel);
                add_dependencies(&mut project, &parsed.dependencies, &file, &parsed.dir, true);
                add_dependencies(
                    &mut project,
                    &parsed.optional_dependencies,
                    &file,
                    &parsed.dir,
                    false,
                );
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
    // root project when nothing encloses them, and declare for the files
    // near them.
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
    let file_projects: Vec<(&Path, Option<usize>)> = py_files
        .iter()
        .map(|f| (*f, owning_project(&projects, f).map(|(idx, _)| idx)))
        .collect();
    for rel in requirements {
        let Some((idx, _)) = owning_project(&projects, &rel) else {
            continue;
        };
        let text = ctx.read_to_string(&rel)?;
        let mut deps = manifest::parse_requirements(&text);
        let dev = manifest::dev_requirements(&rel, &projects[idx].dir);
        if let Some(why) = dev {
            for dep in &mut deps {
                dep.section = why.to_owned();
            }
        }
        let own_files: Vec<&Path> = file_projects
            .iter()
            .filter(|(_, owner)| *owner == Some(idx))
            .map(|(f, _)| *f)
            .collect();
        let scope = requirements_scope(&rel, &projects[idx].dir, &own_files);
        let required = dev.is_none();
        add_dependencies(
            &mut projects[idx],
            &deps,
            &display_path(&rel),
            &scope,
            required,
        );
    }

    Ok(projects)
}

/// The directory whose files a requirements file declares dependencies for:
/// the closest directory at or above it with some of the project's own
/// Python files below it, never above the project.
/// `functions/notify/requirements.txt` next to a `main.py` declares for
/// `functions/notify/`; `requirements/prod.txt`, `docker/requirements.txt`
/// and a requirements file whose directory holds only nested projects
/// declare for the whole project.
fn requirements_scope(file: &Path, project_dir: &Path, own_files: &[&Path]) -> PathBuf {
    file.ancestors()
        .skip(1)
        .take_while(|dir| dir.starts_with(project_dir))
        .find(|dir| own_files.iter().any(|f| f.starts_with(dir)))
        .unwrap_or(project_dir)
        .to_path_buf()
}

fn new_project(name: String, dir: PathBuf, evidence: Evidence) -> Project {
    Project {
        id: ComponentId::new(&name),
        name,
        dir,
        evidence: vec![evidence],
        declarations: Vec::new(),
    }
}

fn add_dependencies(
    project: &mut Project,
    deps: &[PyDependency],
    file: &str,
    scope: &Path,
    required: bool,
) {
    for dep in deps {
        let mut evidence = Evidence::new(file).with_note(&dep.section);
        if let Some(line) = dep.line {
            evidence = evidence.at_line(line);
        }
        project.declarations.push(Declaration {
            name: dep.name.clone(),
            scope: scope.to_path_buf(),
            required,
            evidence,
        });
    }
}

fn external_id(distribution: &str) -> ComponentId {
    ComponentId::new(format!("{EXTERNAL_PREFIX}{distribution}"))
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
        if !regular && !dotted.split('.').all(resolve::is_identifier) {
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

/// The innermost module whose directory contains `file`.
fn owning_module<'a>(by_dir: &BTreeMap<&Path, &'a Module>, file: &Path) -> Option<&'a Module> {
    file.parent()?
        .ancestors()
        .find_map(|dir| by_dir.get(dir).copied())
}

// ---------------------------------------------------------------------------
// emission

fn emit_components(
    projects: &[Project],
    modules: &[Module],
    modules_by_dir: &BTreeMap<&Path, &Module>,
    output: &mut AnalyzerOutput,
) {
    for (idx, project) in projects.iter().enumerate() {
        let mut component =
            Component::new(project.id.clone(), &project.name, ComponentKind::Package);
        component.language = Some(LANGUAGE.to_owned());
        component.path = Some(display_path(&project.dir));
        component.evidence = project.evidence.clone();
        output.fragment.push_component(component);

        for declaration in project.declarations.iter().filter(|d| d.required) {
            let target = external_id(&declaration.name);
            let mut external =
                Component::new(target.clone(), &declaration.name, ComponentKind::External);
            external.language = Some(LANGUAGE.to_owned());
            external.evidence.push(declaration.evidence.clone());
            output.fragment.push_component(external);
            let from = declaring_component(modules_by_dir, idx, project, &declaration.scope);
            output.fragment.push_edge(
                Edge::new(from, target, EdgeKind::Dependency)
                    .with_evidence(declaration.evidence.clone()),
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

/// The component that declares dependencies for `scope`: the innermost
/// module of the project at or above it, else the project itself.
fn declaring_component(
    modules_by_dir: &BTreeMap<&Path, &Module>,
    project_idx: usize,
    project: &Project,
    scope: &Path,
) -> ComponentId {
    if scope == project.dir {
        return project.id.clone();
    }
    scope
        .ancestors()
        .take_while(|dir| dir.starts_with(&project.dir))
        .filter_map(|dir| modules_by_dir.get(dir))
        .find(|m| m.project == project_idx)
        .map_or_else(|| project.id.clone(), |m| m.id.clone())
}

/// Module path used inside symbol ids: the file stem, or nothing for
/// `__init__.py`.
fn symbol_scope(file: &Path) -> Option<String> {
    let stem = file.file_stem()?.to_string_lossy();
    (stem != "__init__").then(|| stem.into_owned())
}

/// The symbols of `scanned`'s public definitions, or of those among
/// `only` (a method goes with its class).
fn emit_symbols(
    owner: &ComponentId,
    scope: &Option<String>,
    file: &str,
    scanned: &PyFile,
    only: Option<&BTreeSet<String>>,
    output: &mut AnalyzerOutput,
) {
    for def in &scanned.defs {
        let named = def.name.split('.').next().unwrap_or(&def.name);
        if only.is_some_and(|names| !names.contains(named)) {
            continue;
        }
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
            evidence: vec![Evidence::new(file)
                .at_line(def.line)
                .in_test(is_test_code(Path::new(file)))],
        });
    }
}

/// What an import of one file can resolve against.
struct ImportScope<'a> {
    project: &'a Project,
    /// Required dependencies declared for the file's directory.
    declared: resolve::Resolver<'a>,
    /// Required dependencies declared anywhere in the project.
    anywhere: resolve::Resolver<'a>,
    optional: resolve::Resolver<'a>,
    installed: &'a resolve::InstalledIndex,
    /// Every file stem and directory name with Python code in the project.
    local_names: &'a BTreeSet<String>,
    /// Every Python file in the repository, for resolving import targets.
    known_files: &'a BTreeSet<&'a Path>,
}

/// Where each name of one import statement comes from, as `emit_imports`
/// placed it: where the walk through re-exports starts and what it follows.
#[derive(Debug, Default)]
struct Resolved {
    /// For each name other than `*`, in order: its file and its name there,
    /// [`WHOLE_MODULE`] for a submodule taken whole; `None` for a name the
    /// scan did not place in a file it read.
    names: Vec<Option<(String, String)>>,
    /// The file of the statement's own module, when the scan read it.
    own: Option<String>,
    /// The names the file reads through a module the statement binds
    /// (`charge.pay`), each with that module's file.
    read: Vec<(String, String)>,
}

/// A Python file once read, with how its imports resolved.
struct ReadFile {
    file: PathBuf,
    display: String,
    owner: ComponentId,
    /// Inside a regular package tree, where public definitions are
    /// symbols whether or not anything imports them.
    in_package_tree: bool,
    scanned: PyFile,
    resolved: Vec<Resolved>,
}

#[allow(clippy::too_many_arguments)]
fn emit_imports(
    owner: &ComponentId,
    base_dotted: Option<&str>,
    ctx: &ImportScope,
    modules: &[Module],
    by_dotted: &BTreeMap<&str, usize>,
    file: &Path,
    scanned: &PyFile,
    output: &mut AnalyzerOutput,
) -> Vec<Resolved> {
    let file_display = display_path(file);
    let test = is_test_code(file);
    let mut resolved = Vec::with_capacity(scanned.imports.len());
    for import in &scanned.imports {
        let full = match resolve_base(base_dotted, import) {
            Some(full) => full,
            None => {
                resolved.push(Resolved::default());
                continue;
            }
        };

        // `*`, from a star import or a list that could not be read, is no
        // name to look up: it takes the statement's module whole
        let star = import.names.iter().any(|n| n == WHOLE_MODULE);
        let named: Vec<&String> = import.names.iter().filter(|n| *n != WHOLE_MODULE).collect();
        let mut candidates: Vec<String> = named
            .iter()
            .map(|n| {
                if full.is_empty() {
                    (*n).clone()
                } else {
                    format!("{full}.{n}")
                }
            })
            .collect();
        if !full.is_empty() {
            candidates.push(full.clone());
        }

        let evidence = || {
            Evidence::new(&file_display)
                .at_line(import.line)
                .in_scope(scope(import.local))
                .in_test(test)
                .type_only(import.type_only)
        };

        // Internal module -> the files the statement loads in it, with the
        // names it takes from each: an attribute of the statement's module by
        // name, a submodule whole, and nothing from a package it only passes.
        // The trailing `full` candidate only contributes a file when no
        // imported name resolved into the same module (`from pkg import
        // VERSION`), or when the statement takes the module whole.
        let mut internal: BTreeMap<ComponentId, BTreeMap<Option<String>, BTreeSet<String>>> =
            BTreeMap::new();
        let own = own_file(&full, modules, by_dotted, ctx.known_files);
        let mut placed: Vec<Option<(String, String)>> = vec![None; named.len()];
        let mut read: Vec<(String, String)> = Vec::new();
        // what the file reads through each name the statement binds
        let reads_of = |i: usize| -> Option<&BTreeSet<String>> {
            let (j, _) = import
                .names
                .iter()
                .enumerate()
                .filter(|(_, n)| *n != WHOLE_MODULE)
                .nth(i)?;
            import.reads.get(j)?.as_ref()
        };
        for (i, candidate) in candidates.iter().enumerate() {
            let Some(idx) = longest_known_prefix(candidate, by_dotted) else {
                continue;
            };
            let module = &modules[idx];
            let is_full = i == named.len();
            let files = internal.entry(module.id.clone()).or_default();
            if is_full && !files.is_empty() {
                // a list that could not be read may take more from the
                // module's own file, but adds no file to the statement
                if star && own.is_some() {
                    if let Some(names) = files.get_mut(&own) {
                        names.insert(WHOLE_MODULE.to_owned());
                    }
                }
                continue;
            }
            let (target, fallback) = target_file(module, candidate, ctx.known_files);
            let names = files.entry(target.clone()).or_default();
            // a module the statement binds: the names the file reads
            // through it, else all of it
            let mut through =
                |taken: Option<&BTreeSet<String>>, names: &mut BTreeSet<String>| match (
                    taken, &target,
                ) {
                    (Some(taken), Some(file)) => {
                        names.extend(taken.iter().cloned());
                        read.extend(taken.iter().map(|n| (file.clone(), n.clone())));
                    }
                    _ => {
                        names.insert(WHOLE_MODULE.to_owned());
                    }
                };
            if is_full {
                // `import m` takes what the file reads through it and `from m
                // import *` the whole module; the package a `from` import
                // passes on the way, or the one left for a module the scan
                // did not read, gives no name
                if import.names.is_empty() && !fallback {
                    through(import.reads.first().and_then(Option::as_ref), names);
                } else if star && !fallback {
                    names.insert(WHOLE_MODULE.to_owned());
                }
            } else if target.is_some() && target == own {
                names.insert(named[i].clone());
                placed[i] = target.map(|t| (t, named[i].clone()));
            } else if !fallback {
                // a submodule
                through(reads_of(i), names);
                placed[i] = target.map(|t| (t, WHOLE_MODULE.to_owned()));
            }
        }
        resolved.push(Resolved {
            names: placed,
            own: own.clone(),
            read,
        });

        let note = if import.level > 0 {
            "relative import"
        } else {
            "import"
        };
        for (target, files) in &internal {
            for (target_file, names) in files {
                // Imports between files of one component are kept as a
                // self-edge: roll-up hides them, but impact needs them to
                // follow a change through the component.
                let same_file = target_file.as_deref() == Some(file_display.as_str());
                if target == owner && (target_file.is_none() || same_file) {
                    continue;
                }
                let mut e = evidence().with_note(note);
                if let Some(t) = target_file {
                    e = e.pointing_at(t).taking(names.iter().cloned());
                }
                output.fragment.push_edge(
                    Edge::new(owner.clone(), target.clone(), EdgeKind::Import).with_evidence(e),
                );
            }
        }
        if !internal.is_empty() || import.level > 0 {
            continue;
        }

        let mut externals: BTreeMap<ComponentId, String> = BTreeMap::new();
        for candidate in &candidates {
            let Some(resolved) = ctx.declared.resolve(candidate) else {
                continue;
            };
            externals
                .entry(external_id(&resolved.distribution))
                .or_insert_with(|| resolved.note());
        }
        for (target, note) in &externals {
            output.fragment.push_edge(
                Edge::new(owner.clone(), target.clone(), EdgeKind::Import)
                    .with_evidence(evidence().with_note(note)),
            );
        }

        if full.is_empty() {
            continue;
        }
        let top = full.split('.').next().unwrap_or_default();
        if stdlib::is_stdlib(top) {
            continue;
        }
        // What no edge covers: each imported name that an installed
        // distribution provides as a module of its own (`from google.cloud
        // import bigquery, storage`), and the statement's module for the
        // other names. Without a virtualenv every name is one of the others.
        let names = &candidates[..named.len()];
        let (modules, others): (Vec<&String>, Vec<&String>) =
            names.iter().partition(|c| ctx.installed.is_module(c));
        let mut uncovered: Vec<(&str, Vec<&String>)> = modules
            .into_iter()
            .filter(|m| ctx.declared.resolve(m).is_none())
            .map(|m| (m.as_str(), vec![m]))
            .collect();
        if names.is_empty() || !others.is_empty() {
            let rest: Vec<&String> = others.into_iter().chain([&full]).collect();
            if rest.iter().all(|c| ctx.declared.resolve(c).is_none()) {
                uncovered.push((full.as_str(), rest));
            }
        }
        for (module, looked_up) in uncovered {
            let reason = if looked_up.iter().any(|c| ctx.optional.resolve(c).is_some()) {
                UnmappedReason::DeclaredNotRequired
            } else if ctx.local_names.contains(top) {
                UnmappedReason::LocalName
            } else {
                UnmappedReason::Undeclared
            };
            if reason == UnmappedReason::LocalName {
                if let Some(sibling) = sibling_file(file, top, ctx.known_files) {
                    let note = format!(
                        "import {top}, next to the importing file \
                         (assumes its directory is on sys.path)"
                    );
                    // the names taken from the file itself, else all of it
                    let names: Vec<String> = if full == top && !named.is_empty() {
                        named.iter().map(|n| (*n).clone()).collect()
                    } else {
                        vec![WHOLE_MODULE.to_owned()]
                    };
                    output.fragment.push_edge(
                        Edge::new(owner.clone(), owner.clone(), EdgeKind::Import).with_evidence(
                            evidence()
                                .with_note(note)
                                .pointing_at(sibling)
                                .taking(names),
                        ),
                    );
                    continue;
                }
            }
            let (provided_by, note) = match reason {
                UnmappedReason::Undeclared => (
                    ctx.installed.providers_of(module),
                    declared_elsewhere(ctx.project, &ctx.anywhere, &looked_up),
                ),
                UnmappedReason::DeclaredNotRequired => {
                    let dir = file.parent().unwrap_or(Path::new(""));
                    let note = declared_optionally(ctx.project, &ctx.optional, &looked_up, dir);
                    (Vec::new(), note)
                }
                UnmappedReason::LocalName | UnmappedReason::Unresolved => (Vec::new(), None),
            };
            output.fragment.push_unmapped_import(UnmappedImport {
                from: owner.clone(),
                module: module.to_owned(),
                reason,
                provided_by,
                evidence: evidence().with_note(note.unwrap_or_else(|| "import".to_owned())),
            });
        }
    }

    for dynamic in &scanned.dynamic_imports {
        output.fragment.push_dynamic_import(DynamicImport {
            from: owner.clone(),
            call: dynamic.call.to_owned(),
            evidence: Evidence::new(&file_display)
                .at_line(dynamic.line)
                .in_scope(scope(dynamic.local))
                .in_test(test),
        });
    }
    resolved
}

fn scope(local: bool) -> Scope {
    if local {
        Scope::Local
    } else {
        Scope::Module
    }
}

/// What `read` binds at module level, for the walk through re-exports:
/// the names it defines, and those its `from` imports bind, from the files
/// they resolved to, apart from names bound in a way the walk does not
/// follow (`import a.b as c`, a module outside the scan, a file next to
/// the importer).
fn binding_table(read: &ReadFile) -> reexports::Table {
    let scanned = &read.scanned;
    let mut table = reexports::Table {
        defined: scanned
            .defs
            .iter()
            .filter(|d| !d.name.contains('.'))
            .map(|d| d.name.clone())
            .chain(scanned.module_names.iter().cloned())
            .collect(),
        exported: match &scanned.all {
            None => reexports::Exported::Public,
            Some(source::DunderAll::Listed(names)) => {
                reexports::Exported::Listed(names.iter().cloned().collect())
            }
            Some(source::DunderAll::Built) => reexports::Exported::Built,
        },
        ..reexports::Table::default()
    };
    for (import, resolved) in scanned.imports.iter().zip(&read.resolved) {
        // what a function or a class body binds is no name of the module
        if import.local || import.in_class {
            continue;
        }
        let mut unfollowed = |name: &String| {
            *table.unfollowed.entry(name.clone()).or_insert(true) &= import.type_only;
        };
        if import.names.is_empty() {
            import.bound.iter().for_each(&mut unfollowed);
            continue;
        }
        let bound = import
            .names
            .iter()
            .zip(&import.bound)
            .filter(|(name, _)| *name != WHOLE_MODULE)
            .map(|(_, bound)| bound);
        for (i, bound) in bound.enumerate() {
            match resolved.names.get(i).cloned().flatten() {
                Some((file, name)) => {
                    table
                        .bound
                        .entry(bound.clone())
                        .or_default()
                        .push(reexports::Binding {
                            file,
                            name,
                            line: import.line,
                            type_only: import.type_only,
                        })
                }
                None => unfollowed(bound),
            }
        }
        if import.names.iter().any(|n| n == WHOLE_MODULE) {
            match (&resolved.own, import.unread) {
                (Some(own), false) => table.stars.push(reexports::Binding {
                    file: own.clone(),
                    name: WHOLE_MODULE.to_owned(),
                    line: import.line,
                    type_only: import.type_only,
                }),
                _ => table.opaque = true,
            }
        }
    }
    table
}

/// Evidence for the files that define the names each statement takes from
/// a file that binds them by importing them, by the names those files give
/// them. The loaded file keeps its own evidence from [`emit_imports`].
fn emit_definitions(
    reads: &[ReadFile],
    tables: &BTreeMap<String, reexports::Table>,
    output: &mut AnalyzerOutput,
) {
    let owners: BTreeMap<&str, &ComponentId> = reads
        .iter()
        .map(|read| (read.display.as_str(), &read.owner))
        .collect();
    let mut definitions = reexports::Definitions::new(tables);
    for read in reads {
        let test = is_test_code(&read.file);
        for (import, resolved) in read.scanned.imports.iter().zip(&read.resolved) {
            // by defining file, first binding and whether only types travel
            let mut found: BTreeMap<(String, (String, u32), bool), BTreeSet<String>> =
                BTreeMap::new();
            let taken = resolved.names.iter().flatten().chain(&resolved.read);
            for (loaded, name) in taken {
                if name == WHOLE_MODULE || *loaded == read.display {
                    continue;
                }
                let Some(definition) = definitions.of(loaded, name) else {
                    continue;
                };
                if definition.file == read.display || definition.file == *loaded {
                    continue;
                }
                let type_only = import.type_only || definition.type_only;
                found
                    .entry((definition.file, definition.via, type_only))
                    .or_default()
                    .insert(definition.name);
            }
            for ((file, (via, line), type_only), names) in found {
                let Some(owner) = owners.get(file.as_str()) else {
                    continue;
                };
                output.fragment.push_edge(
                    Edge::new(read.owner.clone(), (*owner).clone(), EdgeKind::Import)
                        .with_evidence(
                            Evidence::new(&read.display)
                                .at_line(import.line)
                                .in_scope(scope(import.local))
                                .in_test(test)
                                .type_only(type_only)
                                .with_note(format!("import via {via}:{line}"))
                                .pointing_at(file)
                                .taking(names),
                        ),
                );
            }
        }
    }
}

/// `<name>.py` beside `file`, other than `file` itself: what `import <name>`
/// loads when the file's directory is on `sys.path`, as it is for a script
/// run directly or a function deployed from that directory. The file shares
/// its directory, so it belongs to the importer's own component.
fn sibling_file(file: &Path, name: &str, known: &BTreeSet<&Path>) -> Option<String> {
    let sibling = file.parent()?.join(format!("{name}.py"));
    (sibling != file && known.contains(sibling.as_path())).then(|| display_path(&sibling))
}

/// Evidence note for an import that is undeclared for its file although the
/// project declares it for other directories: `import slack_sdk, declared
/// as slack-sdk only for functions/notify/ in
/// functions/notify/requirements.txt:2`.
fn declared_elsewhere(
    project: &Project,
    anywhere: &resolve::Resolver,
    candidates: &[&String],
) -> Option<String> {
    let resolved = candidates.iter().find_map(|c| anywhere.resolve(c))?;
    let places: BTreeSet<String> = project
        .declarations
        .iter()
        .filter(|d| d.name == resolved.distribution)
        .map(|d| format!("for {}/ in {}", display_path(&d.scope), declared_at(d)))
        .collect();
    Some(declaration_note(&resolved, "only", places))
}

/// Evidence note for an import of an extra, group or dev dependency, saying
/// where it is declared for the importing directory: `import pytest,
/// declared as pytest in pyproject.toml:7 ([project.optional-dependencies] dev)`.
fn declared_optionally(
    project: &Project,
    optional: &resolve::Resolver,
    candidates: &[&String],
    dir: &Path,
) -> Option<String> {
    let resolved = candidates.iter().find_map(|c| optional.resolve(c))?;
    let places: BTreeSet<String> = project
        .declarations
        .iter()
        .filter(|d| !d.required && d.name == resolved.distribution && dir.starts_with(&d.scope))
        .map(|d| format!("in {}", declared_at(d)))
        .collect();
    Some(declaration_note(&resolved, "", places))
}

/// `import yaml, declared as pyyaml (matched by known import name)`, then
/// `qualifier` and the places.
fn declaration_note(
    resolved: &resolve::Resolved,
    qualifier: &str,
    places: BTreeSet<String>,
) -> String {
    let method = resolved
        .method_note()
        .map(|m| format!(" ({m})"))
        .unwrap_or_default();
    let places: Vec<String> = places.into_iter().collect();
    let mut note = format!(
        "import {}, declared as {}{method}",
        resolved.matched, resolved.distribution
    );
    for part in [qualifier.to_owned(), places.join(", ")] {
        if !part.is_empty() {
            note.push(' ');
            note.push_str(&part);
        }
    }
    note
}

/// Where a declaration is written: `functions/notify/requirements.txt:2`,
/// `requirements-dev.txt:1 (dev by file name)`, `pyproject.toml:7
/// ([project.optional-dependencies] dev)`, or without a line `pyproject.toml
/// [project.optional-dependencies] dev`.
fn declared_at(declaration: &Declaration) -> String {
    let e = &declaration.evidence;
    match (e.line, e.note.as_deref()) {
        (Some(line), Some(note)) if note != manifest::REQUIREMENTS => {
            format!("{}:{line} ({note})", e.file)
        }
        (Some(line), _) => format!("{}:{line}", e.file),
        (None, Some(note)) => format!("{} {note}", e.file),
        (None, None) => e.file.clone(),
    }
}

/// The file a dotted import path loads inside `module`: `pkg/sub.py` for
/// `pkg.sub` or `pkg.sub.name`, otherwise the package's own `__init__.py`.
/// `None` for a namespace package, which has no file of its own. The flag
/// says that the path named something below the package that is no file
/// the scan read, so the `__init__.py` is only loaded on the way.
fn target_file(
    module: &Module,
    candidate: &str,
    known: &BTreeSet<&Path>,
) -> (Option<String>, bool) {
    let rest = candidate
        .strip_prefix(module.dotted.as_str())
        .unwrap_or_default()
        .trim_start_matches('.');
    if let Some(first) = rest.split('.').next().filter(|s| !s.is_empty()) {
        let file = module.dir.join(format!("{first}.py"));
        if known.contains(file.as_path()) {
            return (Some(display_path(&file)), false);
        }
    }
    let init = module.dir.join("__init__.py");
    (
        known.contains(init.as_path()).then(|| display_path(&init)),
        !rest.is_empty(),
    )
}

/// The file of module `dotted` itself, when the scan read it: the
/// `__init__.py` of the package it names, or `<name>.py` one level below the
/// package it resolves into. `None` for a module without a file of its own.
fn own_file(
    dotted: &str,
    modules: &[Module],
    by_dotted: &BTreeMap<&str, usize>,
    known: &BTreeSet<&Path>,
) -> Option<String> {
    let module = &modules[longest_known_prefix(dotted, by_dotted)?];
    let rest = dotted
        .strip_prefix(module.dotted.as_str())
        .unwrap_or_default()
        .trim_start_matches('.');
    let file = if rest.is_empty() {
        module.dir.join("__init__.py")
    } else if !rest.contains('.') {
        module.dir.join(format!("{rest}.py"))
    } else {
        return None;
    };
    known.contains(file.as_path()).then(|| display_path(&file))
}

/// Directory -> the names importable from it: `.py` file stems and
/// subdirectories that hold Python files at any depth.
fn local_names(py_files: &[&Path]) -> BTreeMap<PathBuf, BTreeSet<String>> {
    let mut names: BTreeMap<PathBuf, BTreeSet<String>> = BTreeMap::new();
    for file in py_files {
        let Some(dir) = file.parent() else {
            continue;
        };
        if let Some(stem) = file.file_stem().and_then(|s| s.to_str()) {
            if stem != "__init__" {
                names
                    .entry(dir.to_path_buf())
                    .or_default()
                    .insert(stem.to_owned());
            }
        }
        let mut child = dir;
        while let Some(parent) = child.parent() {
            if let Some(name) = child.file_name().and_then(|n| n.to_str()) {
                names
                    .entry(parent.to_path_buf())
                    .or_default()
                    .insert(name.to_owned());
            }
            child = parent;
        }
    }
    names
}

/// The `site-packages` directories a scan of `root` may read: those of the
/// `.venv` beside each Python manifest among `files` and of the one at the
/// root, where `load_installed` looks.
pub(crate) fn installed_dirs(root: &Path, files: &[PathBuf]) -> Vec<PathBuf> {
    let mut venvs: BTreeSet<PathBuf> = BTreeSet::from([root.join(".venv")]);
    for file in files {
        let name = file.file_name().and_then(|n| n.to_str());
        if matches!(name, Some("pyproject.toml" | "setup.py" | "setup.cfg")) {
            let dir = file.parent().unwrap_or(Path::new(""));
            venvs.insert(root.join(dir).join(".venv"));
        }
    }
    venvs
        .iter()
        .filter(|venv| venv.is_dir())
        .flat_map(|venv| resolve::site_packages_dirs(venv))
        .collect()
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
            bound: names.iter().map(|s| s.to_string()).collect(),
            line: 1,
            local: false,
            type_only: false,
            in_class: false,
            unread: false,
            end_line: 1,
            reads: Vec::new(),
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
    fn test_files_go_by_their_names() {
        // a helper below `tests/` keeps its symbols, as for TS/JS
        assert!(!is_test_named(Path::new("tests/pipeline/helpers.py")));
        assert!(is_test_named(Path::new("pkg/test_core.py")));
        assert!(is_test_named(Path::new("pkg/core_test.py")));
        assert!(is_test_named(Path::new("conftest.py")));
        assert!(!is_test_named(Path::new("src/testing_tools/core.py")));
        assert!(!is_test_named(Path::new("src/contest.py")));
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
