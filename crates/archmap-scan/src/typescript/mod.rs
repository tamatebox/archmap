//! TypeScript / JavaScript analyzer: `package.json` packages and the
//! workspaces that link them, directory and file modules, ES module and
//! CommonJS imports and exports, and scripts.
//!
//! Facts extracted:
//! - a `package.json` with a `name` whose directory holds TS/JS files of its
//!   own, that declares `workspaces`, or that is a workspace member or a
//!   path dependency, becomes a `Package` component; TS/JS
//!   files that no package owns go to one root component named after the
//!   directory; a `package.json` without a name is no package (see
//!   [`layout`]), but it declares dependencies all the same
//! - every directory between a package and its code files, except the
//!   source root `src/`, becomes a `Module` component, and so does every
//!   code file except an `index.*` (its directory's own file) and the files
//!   of the package itself: the source root's `index.*`, and the files
//!   directly in a package directory that has `src/`
//! - `dependencies` and `peerDependencies` become `Dependency` edges to
//!   `ext:npm:*` components; `devDependencies` and `optionalDependencies`
//!   are declared but give no edge
//! - `import` (`import x = require('m')` too) and `export ... from`
//!   statements become `Import` edges, resolved by `oxc_resolver` through
//!   each file's tsconfig over the scanned files only, in which workspace
//!   members and path dependencies are linked by name (see [`fs`] and
//!   [`workspace`]); an import of a stylesheet, image or JSON file is an
//!   edge of the importer to itself, or to the package that holds the file,
//!   whose evidence names the file
//! - so do calls with a written-out specifier anywhere in a file
//!   (`require`, `import()`, `vi.mock` and the other module calls of Vitest
//!   and Jest; `local` inside a function body) and `import()` types (types
//!   only); `require` and `import()` of a computed name become
//!   [`DynamicImport`]s
//! - a named or default import that reaches a name through re-exports gets
//!   one more edge for each file that defines a name it takes, noted with
//!   the first re-export on the way (`import via src/index.ts:2`; see
//!   [`exports`])
//! - a bare specifier that resolves to no file is matched by package name to
//!   the closest `package.json` above the importing file that declares it,
//!   or else its `@types` package: a required declaration gives an edge, another
//!   an [`UnmappedImport`] saying where it is declared, and none an
//!   undeclared one, unless a tsconfig or jsconfig declares it as an alias
//!   (`unresolved`) or it is the package's own name or one of its own
//!   directories or files (`local_name`); a path or alias that matches no
//!   file is `unresolved`; Node built-ins are left out
//! - exported declarations and CommonJS exports become symbols (see
//!   [`source`]), except in test, story and mock files; a file TypeScript
//!   reads as a script gives its global declarations, and its component is
//!   a `Script`

mod exports;
mod fs;
mod language;
mod layout;
mod package;
mod resolve;
mod source;
mod workspace;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use archmap_core::{
    Component, ComponentId, ComponentKind, DynamicImport, Edge, EdgeKind, Evidence, Scope, Symbol,
    SymbolId, UnmappedImport, UnmappedReason, WHOLE_MODULE,
};

use crate::analyzer::AnalyzerOutput;
use crate::context::display_path;
use crate::test_code::is_test_code;
use crate::{Analyzer, RepoContext, ScanError};
use layout::{Layout, Owner, Package};
use package::{Declaration, PackageJson};
use resolve::{ModuleOptions, Resolved};
use source::{ExportedSymbol, ImportStatement, ParsedFile};

use language::{is_code, language_of};
pub use language::{JAVASCRIPT, LANGUAGE};

/// Prefix of the component ids of npm packages outside the repository.
pub const EXTERNAL_PREFIX: &str = "ext:npm:";

#[derive(Debug, Default, Clone)]
pub struct TypeScriptAnalyzer;

impl Analyzer for TypeScriptAnalyzer {
    fn name(&self) -> &'static str {
        LANGUAGE
    }

    fn detect(&self, ctx: &RepoContext) -> bool {
        ctx.files_named("package.json").next().is_some() || ctx.files().iter().any(|f| is_code(f))
    }

    fn analyze(&self, ctx: &RepoContext) -> Result<AnalyzerOutput, ScanError> {
        let mut output = AnalyzerOutput::default();
        let code: Vec<&Path> = ctx
            .files()
            .iter()
            .map(PathBuf::as_path)
            .filter(|f| is_code(f))
            .collect();
        let manifests = read_manifests(ctx, &mut output.warnings);
        // the packages an install links by name: workspace members and the
        // targets of `file:` dependencies
        let pnpm: BTreeMap<PathBuf, Vec<String>> = ctx
            .files_named("pnpm-workspace.yaml")
            .filter_map(|rel| {
                let text = ctx.read_to_string(rel).ok()?;
                let dir = rel.parent().unwrap_or(Path::new("")).to_path_buf();
                Some((dir, workspace::pnpm_patterns(&text)))
            })
            .collect();
        let links = workspace::links(&manifests, &pnpm);
        let members: BTreeSet<PathBuf> = links.values().cloned().collect();
        let layout = layout::discover(
            &code,
            &manifests,
            &members,
            ctx.files(),
            &root_name(ctx.root()),
        );
        for (name, kept, renamed) in &layout.renamed {
            let kept = display_path(&kept.join("package.json"));
            output.warnings.push(if renamed.as_os_str().is_empty() {
                format!(
                    "the TS/JS files no package owns take the root's name {name}, which {kept} \
                     keeps: they are {name}+."
                )
            } else {
                format!(
                    "two packages are named {name}: {kept} keeps the id, {} is {name}+{}",
                    display_path(&renamed.join("package.json")),
                    display_path(renamed)
                )
            });
        }
        let linked: BTreeMap<String, ComponentId> = links
            .iter()
            .filter_map(|(name, dir)| {
                let package = layout
                    .packages
                    .iter()
                    .find(|p| p.manifest.as_deref() == Some(dir.as_path()))?;
                Some((name.clone(), package.id.clone()))
            })
            .collect();
        emit_components(&layout, &manifests, &linked, &mut output);
        for file in &code {
            if let Some(language) = language_of(file) {
                output.read.entry(language.to_owned()).or_insert(0);
            }
        }
        if ctx.options().manifests_only {
            return Ok(output);
        }

        let view = fs::ViewFs::new_linked(ctx, &links, &mut output.warnings);
        let aliases = resolve::Aliases::collect(ctx, &view);
        let conditions = resolve::custom_conditions(ctx, &view);
        let resolver = resolve::ImportResolver::new(ctx.root(), view, &conditions);
        let mut problems = BTreeSet::new();
        // Every file is parsed and its imports resolved before any import is
        // emitted: a walk through re-exports reads the files it passes.
        let mut files: Vec<ReadFile> = Vec::new();
        // Scripts by their owner, with the file's path: a script that is a
        // component of its own is of kind `Script`.
        let mut scripts: BTreeMap<ComponentId, String> = BTreeMap::new();
        for file in &code {
            let Some(owner) = layout.owners.get(*file) else {
                continue;
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
            let parsed = match source::parse(file, &text) {
                Ok(parsed) => parsed,
                Err(err) => {
                    output
                        .warnings
                        .push(format!("{}: parse error: {err}", display_path(file)));
                    continue;
                }
            };
            if let Some(language) = language_of(file) {
                *output.read.entry(language.to_owned()).or_default() += 1;
            }
            // a script declares its top-level names globally
            let script = !parsed.module_syntax
                && is_script(
                    file,
                    &parsed,
                    type_module(file, &manifests),
                    resolver.module_options(file),
                );
            if script {
                if let Some(language) = language_of(file) {
                    *output.scripts.entry(language.to_owned()).or_default() += 1;
                }
                scripts.insert(owner.component.clone(), display_path(file));
            }
            if !layout::is_test_file(file) {
                let symbols = if script {
                    &parsed.globals
                } else {
                    &parsed.symbols
                };
                emit_symbols(owner, file, symbols, &mut output);
            }
            for call in &parsed.dynamic {
                output.fragment.push_dynamic_import(DynamicImport {
                    from: owner.component.clone(),
                    call: call.call.to_owned(),
                    evidence: Evidence::new(display_path(file))
                        .at_line(call.line)
                        .in_scope(scope(call.local))
                        .in_test(is_test_code(file)),
                });
            }
            let own_name = layout.packages[owner.package]
                .manifest
                .as_ref()
                .and_then(|dir| manifests.get(dir))
                .and_then(|m| m.name.as_deref());
            let resolved = parsed
                .imports
                .iter()
                .map(|import| resolver.resolve(file, &import.specifier, own_name, &mut problems))
                .collect();
            files.push(ReadFile {
                file,
                owner,
                imports: parsed.imports,
                exports: parsed.exports,
                resolved,
            });
        }
        let modules: BTreeMap<PathBuf, exports::Module> = files
            .iter_mut()
            .map(|read| {
                let loads = read
                    .resolved
                    .iter()
                    .map(|resolved| match resolved {
                        Resolved::File(target) if is_code(target) => Some(target.clone()),
                        _ => None,
                    })
                    .collect();
                let exports = std::mem::take(&mut read.exports);
                (read.file.to_path_buf(), exports::Module { exports, loads })
            })
            .collect();
        let mut definitions = exports::Definitions::new(&modules);
        for read in &files {
            let package = &layout.packages[read.owner.package];
            let imports = Imports {
                layout: &layout,
                owner: read.owner,
                package,
                own_name: package
                    .manifest
                    .as_ref()
                    .and_then(|dir| manifests.get(dir))
                    .and_then(|m| m.name.as_deref()),
                manifests: read
                    .file
                    .ancestors()
                    .skip(1)
                    .filter_map(|dir| manifests.get_key_value(dir))
                    .map(|(dir, m)| (dir.as_path(), m))
                    .collect(),
                aliases: &aliases,
                linked: &linked,
                file: read.file,
                test: is_test_code(read.file),
            };
            for (import, resolved) in read.imports.iter().zip(&read.resolved) {
                match resolved {
                    // the values a statement takes, and apart from them the
                    // types, which never run
                    Resolved::File(loaded) => {
                        let (types, values): (Vec<&String>, Vec<&String>) = import
                            .names
                            .iter()
                            .partition(|name| import.types.contains(*name));
                        let recorded = |names: Vec<&String>| -> BTreeSet<String> {
                            names
                                .into_iter()
                                .map(|name| definitions.recorded(loaded, name))
                                .collect()
                        };
                        let (types, values) = (recorded(types), recorded(values));
                        if !values.is_empty() || types.is_empty() {
                            imports.emit(import, resolved, &values, false, &mut output);
                        }
                        if !types.is_empty() {
                            imports.emit(import, resolved, &types, true, &mut output);
                        }
                    }
                    _ => {
                        let all_types = !import.names.is_empty()
                            && import.names.iter().all(|n| import.types.contains(n));
                        imports.emit(import, resolved, &BTreeSet::new(), all_types, &mut output);
                    }
                }
                // a re-export passes names on without using them
                if let Resolved::File(loaded) = resolved {
                    if import.note != "export" {
                        imports.emit_definitions(import, loaded, &mut definitions, &mut output);
                    }
                }
            }
        }
        for component in &mut output.fragment.components {
            if component.path.is_some() && scripts.get(&component.id) == component.path.as_ref() {
                component.kind = ComponentKind::Script;
            }
        }
        output.warnings.extend(problems);
        Ok(output)
    }
}

/// Whether TypeScript reads `file`, which has no module syntax, as a script,
/// whose top-level declarations are global: not when its extension makes it
/// a module, when the closest `package.json` says `"type": "module"` and the
/// tsconfig's `module` reads it, when its JSX imports a runtime, or when the
/// tsconfig forces module detection on a file that declares no types only.
fn is_script(file: &Path, parsed: &ParsedFile, type_module: bool, options: ModuleOptions) -> bool {
    let name = file
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let extension = file
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default();
    // `global.d.ts`, `styles.d.css.ts`
    let declarations = name.ends_with(".ts") && name.contains(".d.");
    !(parsed.module_syntax
        || matches!(extension, "mjs" | "mts" | "cjs" | "cts")
        || (type_module && options.node)
        || (parsed.has_jsx && options.jsx_runtime)
        || (options.force && !declarations))
}

/// Whether the closest `package.json` above `file` says `"type": "module"`.
fn type_module(file: &Path, manifests: &BTreeMap<PathBuf, PackageJson>) -> bool {
    file.ancestors()
        .skip(1)
        .find_map(|dir| manifests.get(dir))
        .is_some_and(|manifest| manifest.module)
}

/// Where a statement or call sits: inside a function body or not.
fn scope(local: bool) -> Scope {
    match local {
        true => Scope::Local,
        false => Scope::Module,
    }
}

fn external_id(package: &str) -> ComponentId {
    ComponentId::new(format!("{EXTERNAL_PREFIX}{package}"))
}

/// The component of the npm package a declaration names.
fn external(declaration: &Declaration, language: &str, evidence: Evidence) -> Component {
    let mut external = Component::new(
        external_id(&declaration.name),
        &declaration.name,
        ComponentKind::External,
    );
    external.language = Some(language.to_owned());
    external.evidence.push(evidence);
    external
}

/// Evidence of a declaration in the `package.json` of `dir`.
fn declared_at(dir: &Path, declaration: &Declaration) -> Evidence {
    let evidence =
        Evidence::new(display_path(&dir.join("package.json"))).with_note(declaration.section.key());
    match declaration.line {
        Some(line) => evidence.at_line(line),
        None => evidence,
    }
}

/// `package.json:5`: where a declaration in the `package.json` of `dir` is.
fn place(dir: &Path, declaration: &Declaration) -> String {
    let manifest = display_path(&dir.join("package.json"));
    match declaration.line {
        Some(line) => format!("{manifest}:{line}"),
        None => manifest,
    }
}

fn root_name(root: &Path) -> String {
    root.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "root".to_owned())
}

/// Every readable `package.json`, by directory.
fn read_manifests(ctx: &RepoContext, warnings: &mut Vec<String>) -> BTreeMap<PathBuf, PackageJson> {
    let mut manifests = BTreeMap::new();
    for rel in ctx.files_named("package.json") {
        let parsed = ctx
            .read_to_string(rel)
            .map_err(|e| e.to_string())
            .and_then(|text| package::parse(&text));
        match parsed {
            Ok(manifest) => {
                let dir = rel.parent().unwrap_or(Path::new("")).to_path_buf();
                manifests.insert(dir, manifest);
            }
            Err(err) => warnings.push(format!(
                "{}: failed to parse package.json: {err}",
                display_path(rel)
            )),
        }
    }
    manifests
}

fn emit_components(
    layout: &Layout,
    manifests: &BTreeMap<PathBuf, PackageJson>,
    linked: &BTreeMap<String, ComponentId>,
    output: &mut AnalyzerOutput,
) {
    // The source root's `index.*` of each package: the package's own file,
    // as a directory's `index.*` is the directory's; of several, the one an
    // import of the directory loads.
    let mut indexes: BTreeMap<&ComponentId, &Path> = BTreeMap::new();
    for (file, owner) in layout.owners.iter().filter(|(_, owner)| {
        owner.symbol_scope.is_none() && layout.packages[owner.package].id == owner.component
    }) {
        let kept = indexes.entry(&owner.component).or_insert(file.as_path());
        if layout::index_rank(file) < layout::index_rank(kept) {
            *kept = file.as_path();
        }
    }
    for package in &layout.packages {
        let mut component = Component::new(
            package.id.clone(),
            package.id.as_str(),
            ComponentKind::Package,
        );
        component.language = Some(package.language.to_owned());
        component.path = Some(display_path(&package.dir));
        component.evidence.push(match &package.manifest {
            Some(dir) => {
                Evidence::new(display_path(&dir.join("package.json"))).with_note("package.json")
            }
            None => Evidence::new(".").with_note("TS/JS files without a package.json"),
        });
        if let Some(index) = indexes.get(&package.id) {
            component
                .evidence
                .push(Evidence::new(display_path(index)).with_note("index"));
        }
        output.fragment.push_component(component);

        let Some((dir, manifest)) = package
            .manifest
            .as_ref()
            .and_then(|dir| manifests.get(dir).map(|m| (dir, m)))
        else {
            continue;
        };
        for declaration in manifest
            .declarations
            .iter()
            .filter(|d| d.section.required())
        {
            let evidence = declared_at(dir, declaration);
            // a package of the repository, whatever the version says
            if let Some(member) = linked.get(&declaration.name) {
                if *member != package.id {
                    output.fragment.push_edge(
                        Edge::new(package.id.clone(), member.clone(), EdgeKind::Dependency)
                            .with_evidence(evidence),
                    );
                }
                continue;
            }
            output.fragment.push_component(external(
                declaration,
                package.language,
                evidence.clone(),
            ));
            output.fragment.push_edge(
                Edge::new(
                    package.id.clone(),
                    external_id(&declaration.name),
                    EdgeKind::Dependency,
                )
                .with_evidence(evidence),
            );
        }
    }
    for module in &layout.modules {
        output.fragment.push_component(module.clone());
    }
}

fn emit_symbols(
    owner: &Owner,
    file: &Path,
    symbols: &[ExportedSymbol],
    output: &mut AnalyzerOutput,
) {
    for symbol in symbols {
        let mut id = owner.component.to_string();
        if let Some(scope) = &owner.symbol_scope {
            id.push_str("::");
            id.push_str(scope);
        }
        id.push_str("::");
        id.push_str(&symbol.name);
        output.fragment.push_symbol(Symbol {
            id: SymbolId::new(id),
            name: symbol.name.clone(),
            kind: symbol.kind,
            component: owner.component.clone(),
            signature: symbol.signature.clone(),
            evidence: vec![Evidence::new(display_path(file)).at_line(symbol.line)],
        });
    }
}

/// A code file once parsed, its symbols already emitted, with what each of
/// its imports resolves to.
struct ReadFile<'a> {
    file: &'a Path,
    owner: &'a Owner,
    imports: Vec<ImportStatement>,
    exports: exports::ExportTable,
    resolved: Vec<Resolved>,
}

/// What the imports of one file resolve against.
struct Imports<'a> {
    layout: &'a Layout,
    owner: &'a Owner,
    package: &'a Package,
    /// The name in the package's own `package.json`, if it has one.
    own_name: Option<&'a str>,
    /// The `package.json` files above the file, with or without a name,
    /// nearest first, by directory: the closest one that declares a
    /// package declares it for the file, as installing it makes it
    /// resolvable there.
    manifests: Vec<(&'a Path, &'a PackageJson)>,
    aliases: &'a resolve::Aliases,
    /// The packages of the repository an install links by name.
    linked: &'a BTreeMap<String, ComponentId>,
    file: &'a Path,
    /// The file is test code.
    test: bool,
}

impl Imports<'_> {
    /// Where `declaration` sits, in the `package.json` of `dir`: `the
    /// enclosing package.json:6` for one above the package's own, which
    /// declares it for every package below.
    fn place(&self, dir: &Path, declaration: &Declaration) -> String {
        let at = place(dir, declaration);
        let enclosing = self.package.manifest.as_deref() != Some(dir)
            && self.package.dir.starts_with(dir)
            && self.package.dir != dir;
        if enclosing {
            format!("the enclosing {at}")
        } else {
            at
        }
    }

    fn evidence(&self, import: &ImportStatement) -> Evidence {
        Evidence::new(display_path(self.file))
            .at_line(import.line)
            .in_scope(scope(import.local))
            .in_test(self.test)
    }

    fn unmapped(
        &self,
        import: &ImportStatement,
        type_only: bool,
        reason: UnmappedReason,
        note: String,
    ) -> UnmappedImport {
        UnmappedImport {
            from: self.owner.component.clone(),
            module: import.specifier.clone(),
            reason,
            provided_by: Vec::new(),
            evidence: self.evidence(import).type_only(type_only).with_note(note),
        }
    }

    /// Why a package name that nothing declares has no edge: an alias of a
    /// tsconfig or jsconfig, a name of the package's own code (`components`
    /// for `components/button`, and for `@components/button` as bundler
    /// aliases write it), else undeclared.
    fn undeclared(&self, import: &ImportStatement, package: &str) -> (UnmappedReason, String) {
        let spec = &import.specifier;
        if let Some((pattern, file)) = self.aliases.matching(spec, self.file) {
            let note = format!(
                "{} {spec}: no file matches; {file} declares the alias `{pattern}`",
                import.note
            );
            return (UnmappedReason::Unresolved, note);
        }
        // A scope matches only the source root: a bundler alias reaches
        // code there, while `prisma/` beside `src/` holds the files of a
        // tool whose packages share the name.
        let scope = package.strip_prefix('@').and_then(|p| p.split('/').next());
        let local = if self.package.local_names.contains(package) {
            Some(package)
        } else {
            scope.filter(|s| self.package.source_names.contains(*s))
        };
        match local {
            Some(name) => (
                UnmappedReason::LocalName,
                format!(
                    "{} {spec}: no file matches, but the package has a file or directory \
                     named {name}",
                    import.note
                ),
            ),
            None => (UnmappedReason::Undeclared, import.note.to_owned()),
        }
    }

    fn emit(
        &self,
        import: &ImportStatement,
        resolved: &Resolved,
        names: &BTreeSet<String>,
        type_only: bool,
        output: &mut AnalyzerOutput,
    ) {
        let from = &self.owner.component;
        let spec = &import.specifier;
        match resolved {
            Resolved::Builtin => {}
            Resolved::File(target) => {
                if target.as_path() == self.file {
                    return;
                }
                // A stylesheet, image or JSON file is no component: the
                // importer depends on it as a file, or on the other package
                // that holds it.
                let to = if is_code(target) {
                    self.layout
                        .owners
                        .get(target)
                        .map_or_else(|| from.clone(), |owner| owner.component.clone())
                } else {
                    match self.layout.package_holding(target) {
                        Some(p) if p != self.owner.package => self.layout.packages[p].id.clone(),
                        _ => from.clone(),
                    }
                };
                output.fragment.push_edge(
                    Edge::new(from.clone(), to, EdgeKind::Import).with_evidence(
                        self.evidence(import)
                            .type_only(type_only)
                            .with_note(import.note)
                            .pointing_at(display_path(target))
                            .taking(names.iter().cloned()),
                    ),
                );
            }
            Resolved::NotFound => {
                let Some(package) = resolve::package_name(spec) else {
                    let why = if resolve::is_path(spec) {
                        "no file matches"
                    } else {
                        "neither a file nor a package name"
                    };
                    let note = format!("{} {spec}: {why}", import.note);
                    output.fragment.push_unmapped_import(self.unmapped(
                        import,
                        type_only,
                        UnmappedReason::Unresolved,
                        note,
                    ));
                    return;
                };
                if Some(package) == self.own_name {
                    // The package by its own name, with an entry outside the
                    // scan (`dist/`). An edge would point from inside the
                    // package at the package itself.
                    let note = format!(
                        "{} {spec}: the package's own name, and its entry is no scanned file",
                        import.note
                    );
                    output.fragment.push_unmapped_import(self.unmapped(
                        import,
                        type_only,
                        UnmappedReason::LocalName,
                        note,
                    ));
                    return;
                }
                if let Some(member) = self.linked.get(package) {
                    // A package of the repository whose entry is built
                    // (`dist/`): the dependency is there, the file is not.
                    let note = format!(
                        "{} {spec}: a package of the repository whose entry is no scanned file",
                        import.note
                    );
                    output.fragment.push_edge(
                        Edge::new(from.clone(), member.clone(), EdgeKind::Import).with_evidence(
                            self.evidence(import).type_only(type_only).with_note(note),
                        ),
                    );
                    return;
                }
                // the package itself in any manifest, nearest first, then
                // its `@types` package: `import { Handler } from 'aws-lambda'`
                // takes only types without saying so
                let declared_as = |name: &str| {
                    self.manifests
                        .iter()
                        .find_map(|(dir, m)| m.declaration_of(name).map(|d| (*dir, d)))
                };
                let declared =
                    declared_as(package).or_else(|| declared_as(&package::types_package(package)));
                match declared {
                    Some((dir, d)) if d.section.required() => {
                        let own = self.package.manifest.as_deref() == Some(dir);
                        let note = match (d.name != package, own) {
                            (true, true) => {
                                format!("{} {spec}, declared as {}", import.note, d.name)
                            }
                            (true, false) => format!(
                                "{} {spec}, declared as {} in {}",
                                import.note,
                                d.name,
                                self.place(dir, d)
                            ),
                            (false, false) => format!(
                                "{} {spec}, declared in {}",
                                import.note,
                                self.place(dir, d)
                            ),
                            (false, true) if spec != package => format!("{} {spec}", import.note),
                            (false, true) => import.note.to_owned(),
                        };
                        if !own {
                            // A `package.json` above the package may make no
                            // component of its own to declare the dependency.
                            output.fragment.push_component(external(
                                d,
                                self.package.language,
                                declared_at(dir, d),
                            ));
                        }
                        output.fragment.push_edge(
                            Edge::new(from.clone(), external_id(&d.name), EdgeKind::Import)
                                .with_evidence(
                                    self.evidence(import).type_only(type_only).with_note(note),
                                ),
                        );
                    }
                    Some((dir, d)) => {
                        let note = format!(
                            "{} {spec}, declared as {} in {} ({})",
                            import.note,
                            d.name,
                            self.place(dir, d),
                            d.section.key()
                        );
                        output.fragment.push_unmapped_import(self.unmapped(
                            import,
                            type_only,
                            UnmappedReason::DeclaredNotRequired,
                            note,
                        ));
                    }
                    None => {
                        let (reason, mut note) = self.undeclared(import, package);
                        // a package of types may be what is missing
                        if reason == UnmappedReason::Undeclared && type_only {
                            note = format!(
                                "{note}, types only: declare {package}, or {} if it ships no \
                                 types",
                                package::types_package(package)
                            );
                        }
                        output
                            .fragment
                            .push_unmapped_import(self.unmapped(import, type_only, reason, note));
                    }
                }
            }
        }
    }

    /// Evidence for each file that defines a name the import takes from
    /// `loaded` through re-exports: one per defining file and first
    /// re-export on the way (`import via src/index.ts:2`), with the names
    /// as the defining file declares them. The loaded file keeps its own
    /// evidence from [`Imports::emit`].
    fn emit_definitions(
        &self,
        import: &ImportStatement,
        loaded: &Path,
        definitions: &mut exports::Definitions,
        output: &mut AnalyzerOutput,
    ) {
        // a file that imports itself depends on nothing it re-exports, as
        // it gets no edge to itself
        if loaded == self.file {
            return;
        }
        // by defining file, first re-export and whether only types travel
        let mut found: BTreeMap<(PathBuf, (PathBuf, u32), bool), BTreeSet<String>> =
            BTreeMap::new();
        for name in import.names.iter().filter(|n| *n != WHOLE_MODULE) {
            let Some(definition) = definitions.of(loaded, name) else {
                continue;
            };
            if definition.file == self.file || definition.file == loaded {
                continue;
            }
            let type_only = import.types.contains(name) || definition.type_only;
            found
                .entry((definition.file, definition.via, type_only))
                .or_default()
                .insert(definition.name);
        }
        for ((file, (via, line), type_only), names) in found {
            let Some(owner) = self.layout.owners.get(&file) else {
                continue;
            };
            let note = format!("{} via {}:{line}", import.note, display_path(&via));
            output.fragment.push_edge(
                Edge::new(
                    self.owner.component.clone(),
                    owner.component.clone(),
                    EdgeKind::Import,
                )
                .with_evidence(
                    self.evidence(import)
                        .type_only(type_only)
                        .with_note(note)
                        .pointing_at(display_path(&file))
                        .taking(names),
                ),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_follow_how_typescript_reads_a_file() {
        let plain = ParsedFile::default();
        let none = ModuleOptions::default();
        assert!(is_script(Path::new("src/global.d.ts"), &plain, false, none));
        assert!(is_script(
            Path::new("public/legacy.js"),
            &plain,
            false,
            none
        ));
        let module = ParsedFile {
            module_syntax: true,
            ..ParsedFile::default()
        };
        assert!(!is_script(Path::new("a.ts"), &module, false, none));
        for file in ["a.mjs", "a.cjs", "a.mts", "a.cts", "a.d.mts"] {
            assert!(!is_script(Path::new(file), &plain, false, none), "{file}");
        }
        // `"type": "module"` counts only where the tsconfig's `module` reads it
        let node = ModuleOptions { node: true, ..none };
        assert!(is_script(Path::new("a.js"), &plain, true, none));
        assert!(!is_script(Path::new("a.js"), &plain, true, node));
        assert!(is_script(Path::new("a.js"), &plain, false, node));
        // JSX imports a runtime under `react-jsx`
        let jsx = ParsedFile {
            has_jsx: true,
            ..ParsedFile::default()
        };
        let runtime = ModuleOptions {
            jsx_runtime: true,
            ..none
        };
        assert!(is_script(Path::new("a.tsx"), &jsx, false, none));
        assert!(!is_script(Path::new("a.tsx"), &jsx, false, runtime));
        // `moduleDetection: force` leaves declaration files scripts
        let force = ModuleOptions {
            force: true,
            ..none
        };
        assert!(!is_script(Path::new("a.ts"), &plain, false, force));
        assert!(is_script(Path::new("global.d.ts"), &plain, false, force));
        assert!(is_script(
            Path::new("styles.d.css.ts"),
            &plain,
            false,
            force
        ));
    }
}
