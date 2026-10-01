//! TypeScript / JavaScript analyzer: `package.json` packages, directory and
//! file modules, ES module imports and exports.
//!
//! Facts extracted:
//! - a `package.json` with a `name` whose directory holds TS/JS files of its
//!   own, or that declares `workspaces`, becomes a `Package` component; TS/JS
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
//!   statements become `Import` edges,
//!   resolved by `oxc_resolver` through each file's tsconfig over the
//!   scanned files only (see [`fs`]); an import of a stylesheet, image or
//!   JSON file is an edge of the importer to itself whose evidence names the
//!   file
//! - a bare specifier that resolves to no file is matched by package name to
//!   the closest `package.json` above the importing file that declares it
//!   (`@types/x` covers `x`): a required declaration gives an edge, another
//!   an [`UnmappedImport`] saying where it is declared, and none an
//!   undeclared one, unless a tsconfig or jsconfig declares it as an alias
//!   (`unresolved`) or it is the package's own name or one of its own
//!   directories or files (`local_name`); a path or alias that matches no
//!   file is `unresolved`; Node built-ins are left out
//! - exported declarations become symbols (see [`source`]), except in test,
//!   story and mock files
//!
//! Not read yet: `require`, `import()`, test mocks, type-only scope, the
//! file that defines a name imported through a re-export, CommonJS exports,
//! scripts and workspaces.

mod fs;
mod language;
mod layout;
mod package;
mod resolve;
mod source;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use archmap_core::{
    Component, ComponentId, ComponentKind, Edge, EdgeKind, Evidence, Scope, Symbol, SymbolId,
    UnmappedImport, UnmappedReason,
};

use crate::analyzer::AnalyzerOutput;
use crate::context::display_path;
use crate::{Analyzer, RepoContext, ScanError};
use layout::{Layout, Owner, Package};
use package::{Declaration, PackageJson};
use resolve::Resolved;
use source::{ImportStatement, ParsedFile};

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
        let layout = layout::discover(&code, &manifests, ctx.files(), &root_name(ctx.root()));
        emit_components(&layout, &manifests, &mut output);
        for file in &code {
            if let Some(language) = language_of(file) {
                output.read.entry(language.to_owned()).or_insert(0);
            }
        }
        if ctx.options().manifests_only {
            return Ok(output);
        }

        let resolver =
            resolve::ImportResolver::new(ctx.root(), fs::ViewFs::new(ctx, &mut output.warnings));
        let aliases = resolve::Aliases::collect(ctx);
        let mut problems = BTreeSet::new();
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
            if !layout::is_test_file(file) {
                emit_symbols(owner, file, &parsed, &mut output);
            }
            let package = &layout.packages[owner.package];
            let imports = Imports {
                layout: &layout,
                owner,
                package,
                own_name: package
                    .manifest
                    .as_ref()
                    .and_then(|dir| manifests.get(dir))
                    .and_then(|m| m.name.as_deref()),
                manifests: file
                    .ancestors()
                    .skip(1)
                    .filter_map(|dir| manifests.get_key_value(dir))
                    .map(|(dir, m)| (dir.as_path(), m))
                    .collect(),
                aliases: &aliases,
                file,
            };
            for import in &parsed.imports {
                imports.emit(import, &resolver, &mut problems, &mut output);
            }
        }
        output.warnings.extend(problems);
        Ok(output)
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
    output: &mut AnalyzerOutput,
) {
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

fn emit_symbols(owner: &Owner, file: &Path, parsed: &ParsedFile, output: &mut AnalyzerOutput) {
    for symbol in &parsed.symbols {
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
    file: &'a Path,
}

impl Imports<'_> {
    fn evidence(&self, import: &ImportStatement) -> Evidence {
        Evidence::new(display_path(self.file))
            .at_line(import.line)
            .in_scope(Scope::Module)
    }

    fn unmapped(
        &self,
        import: &ImportStatement,
        reason: UnmappedReason,
        note: String,
    ) -> UnmappedImport {
        UnmappedImport {
            from: self.owner.component.clone(),
            module: import.specifier.clone(),
            reason,
            provided_by: Vec::new(),
            evidence: self.evidence(import).with_note(note),
        }
    }

    /// Why a package name that nothing declares has no edge: an alias of a
    /// tsconfig or jsconfig, a name of the package's own code (`components`
    /// for `components/button`, and for `@components/button` as bundler
    /// aliases write it), else undeclared.
    fn undeclared(&self, import: &ImportStatement, package: &str) -> (UnmappedReason, String) {
        let spec = &import.specifier;
        if let Some((pattern, file)) = self.aliases.matching(spec) {
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
        resolver: &resolve::ImportResolver,
        problems: &mut BTreeSet<String>,
        output: &mut AnalyzerOutput,
    ) {
        let from = &self.owner.component;
        let spec = &import.specifier;
        match resolver.resolve(self.file, spec, problems) {
            Resolved::Builtin => {}
            Resolved::File(target) => {
                if target == self.file {
                    return;
                }
                // A stylesheet, image or JSON file is no component: the
                // importer depends on it as a file.
                let to = if is_code(&target) {
                    self.layout
                        .owners
                        .get(&target)
                        .map_or_else(|| from.clone(), |owner| owner.component.clone())
                } else {
                    from.clone()
                };
                output.fragment.push_edge(
                    Edge::new(from.clone(), to, EdgeKind::Import).with_evidence(
                        self.evidence(import)
                            .with_note(import.note)
                            .pointing_at(display_path(&target)),
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
                        UnmappedReason::LocalName,
                        note,
                    ));
                    return;
                }
                let declared = self
                    .manifests
                    .iter()
                    .find_map(|(dir, m)| m.declaration_of(package).map(|d| (*dir, d)));
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
                                place(dir, d)
                            ),
                            (false, false) => {
                                format!("{} {spec}, declared in {}", import.note, place(dir, d))
                            }
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
                                .with_evidence(self.evidence(import).with_note(note)),
                        );
                    }
                    Some((dir, d)) => {
                        let note = format!(
                            "{} {spec}, declared as {} in {} ({})",
                            import.note,
                            d.name,
                            place(dir, d),
                            d.section.key()
                        );
                        output.fragment.push_unmapped_import(self.unmapped(
                            import,
                            UnmappedReason::DeclaredNotRequired,
                            note,
                        ));
                    }
                    None => {
                        let (reason, note) = self.undeclared(import, package);
                        output
                            .fragment
                            .push_unmapped_import(self.unmapped(import, reason, note));
                    }
                }
            }
        }
    }
}
