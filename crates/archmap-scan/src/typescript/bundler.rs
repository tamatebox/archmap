//! The aliases a bundler's configuration at the top of a package declares
//! (`resolve.alias` of a `vite.config.*` or a `webpack.config.*`), read
//! from its code without running it. A bundler rewrites a specifier before
//! anything resolves it, so an alias applies to the files of that package
//! before their tsconfig does; the config itself, which the bundler loads
//! without its aliases, and a package below with a `package.json` of its
//! own are outside it.
//!
//! The config object is the one `export default` or `module.exports`
//! gives, through `defineConfig(..)`, `as` and `satisfies`, a module-level
//! `const`, and a function whose body returns one object. An alias counts
//! when its replacement is a path the scan computes inside the root:
//! `path.resolve(__dirname, 'src')` and `path.join`, with `path` or
//! `node:path` imported or required, `fileURLToPath(new URL('./src',
//! import.meta.url))`, `new URL(..).pathname`, and in Vite a path from the
//! project root (`/src`) where the config sets no `root`. A relative one,
//! which both bundlers resolve again from the importing file, a package
//! name and anything else count for nothing, so their keys hide no import.
//!
//! Babel's `babel-plugin-module-resolver`, which rewrites a specifier in
//! the source before any bundler sees it, comes first: its `alias` and its
//! `root` directories, which hold bare names, from a `babel.config.*`, a
//! `.babelrc` or the `babel` key of the package's `package.json`, a path
//! relative to the config's directory, where the plugin's working directory
//! is when Metro or Jest runs it. A regex key and a `root` glob count for
//! nothing.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    ArrayExpressionElement, ArrowFunctionBody, AssignmentTarget, BindingPattern, Expression,
    FunctionBody, ImportDeclarationSpecifier, ObjectExpression, ObjectPropertyKind, Statement,
};
use oxc_parser::Parser;

use super::fs::config as json_config;
use super::source::source_type;
use crate::RepoContext;

/// Configs read, by their name before the extension.
const VITE: &str = "vite.config";
const WEBPACK: &str = "webpack.config";

/// Module-level `const` bindings a value is followed through, at most.
const MAX_DEPTH: usize = 8;

/// Babel's project-wide configs, in the order they are read.
const BABEL_CONFIGS: [&str; 5] = [
    "babel.config.js",
    "babel.config.cjs",
    "babel.config.mjs",
    "babel.config.cts",
    "babel.config.json",
];

/// Babel's file-relative configs, which apply only up to the nearest
/// `package.json`; that file gives its `babel` key.
const BABELRC: [&str; 6] = [
    ".babelrc",
    ".babelrc.json",
    ".babelrc.js",
    ".babelrc.cjs",
    ".babelrc.mjs",
    "package.json",
];

/// Segments a path is computed below, so that one which climbs above the
/// root shows it.
const PAD: usize = 64;
const ABOVE: &str = "\u{0}";

/// The aliases of every package whose bundler or Babel config declares
/// some.
#[derive(Debug, Default)]
pub(crate) struct BundlerAliases {
    /// By the package's directory, relative to the root.
    scopes: BTreeMap<PathBuf, Scope>,
    /// The directories of a package: a `package.json` with a name, or one
    /// beside a config read here, relative to the root.
    packages: BTreeSet<PathBuf>,
    /// The directories of every `package.json`, a marker without a name
    /// (`{ "type": "module" }`) included, where Babel's file-relative
    /// configs stop.
    markers: BTreeSet<PathBuf>,
}

/// What one directory's configs rewrite, in the order Babel and then a
/// bundler apply them.
#[derive(Debug, Default)]
struct Scope {
    /// Babel's `.babelrc` and `babel` key, for the files up to the nearest
    /// `package.json`.
    babelrc: Rewrites,
    /// Babel's `babel.config.*`, for the package's files.
    babel: Rewrites,
    /// Vite's, then webpack's, for the package's files.
    bundler: Vec<Alias>,
}

/// What Babel's module resolver rewrites.
#[derive(Debug, Default)]
struct Rewrites {
    aliases: Vec<Alias>,
    /// The `root` directories, relative to the root.
    roots: Vec<PathBuf>,
}

/// One alias, in the order its config declares it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Alias {
    key: String,
    /// webpack's `key$`: only the key itself, no path below it.
    exact: bool,
    /// Relative to the root; `None` for a path above it, which the scan
    /// does not hold.
    target: Option<PathBuf>,
    /// The config that declares it, relative to the root.
    config: PathBuf,
}

/// An alias a specifier matches.
pub(crate) struct Rewritten<'a> {
    /// The path it leads to, relative to the root; `None` above it.
    pub(crate) path: Option<PathBuf>,
    pub(crate) key: &'a str,
    pub(crate) config: &'a Path,
}

impl BundlerAliases {
    pub(crate) fn read(ctx: &RepoContext) -> Self {
        let dir_of = |f: &Path| f.parent().unwrap_or(Path::new("")).to_path_buf();
        let manifests: Vec<&PathBuf> = ctx
            .files()
            .iter()
            .filter(|f| f.file_name().is_some_and(|n| n == "package.json"))
            .collect();
        let markers: BTreeSet<PathBuf> = manifests.iter().map(|f| dir_of(f)).collect();
        let named = |f: &&&PathBuf| {
            let text = ctx.read_to_string(f).ok();
            let value = text.as_deref().and_then(json_config);
            value.is_some_and(|v| v.get("name").is_some_and(|n| n.is_string()))
        };
        let mut packages: BTreeSet<PathBuf> =
            manifests.iter().filter(named).map(|f| dir_of(f)).collect();
        let mut configs: Vec<&PathBuf> = ctx
            .files()
            .iter()
            .filter(|f| bundler_of(f).is_some())
            .filter(|f| markers.contains(&dir_of(f)))
            .collect();
        // Vite before webpack in one package, each in path order
        configs.sort_by_key(|f| (bundler_of(f) != Some(VITE), (*f).clone()));
        let mut scopes: BTreeMap<PathBuf, Scope> = BTreeMap::new();
        let read = |config: &Path| ctx.read_to_string(config).ok();
        for dir in &markers {
            for (names, relative) in [(&BABELRC[..], true), (&BABEL_CONFIGS[..], false)] {
                for name in names {
                    let config = dir.join(name);
                    let Some((aliases, roots)) =
                        read(&config).map(|t| module_resolver(&config, &t))
                    else {
                        continue;
                    };
                    if aliases.is_empty() && roots.is_empty() {
                        continue;
                    }
                    let scope = scopes.entry(dir.clone()).or_default();
                    let rewrites = if relative {
                        &mut scope.babelrc
                    } else {
                        packages.insert(dir.clone());
                        &mut scope.babel
                    };
                    rewrites.aliases.extend(aliases);
                    rewrites.roots.extend(roots);
                }
            }
        }
        for config in configs {
            let Some(aliases) = read(config).map(|t| aliases_of(config, &t)) else {
                continue;
            };
            if !aliases.is_empty() {
                packages.insert(dir_of(config));
                scopes
                    .entry(dir_of(config))
                    .or_default()
                    .bundler
                    .extend(aliases);
            }
        }
        BundlerAliases {
            scopes,
            packages,
            markers,
        }
    }

    /// What the configs about `file` (relative to the root) rewrite, in the
    /// order they apply: Babel's file-relative config up to the nearest
    /// `package.json`, then those of the package that owns the file. A
    /// bundler's own config is outside them.
    fn scopes(&self, file: &Path) -> [Option<&Scope>; 2] {
        if self.scopes.is_empty() || bundler_of(file).is_some() {
            return [None, None];
        }
        let nearest = |dirs: &BTreeSet<PathBuf>| {
            file.ancestors()
                .skip(1)
                .find(|dir| dirs.contains(*dir))
                .and_then(|dir| self.scopes.get(dir))
        };
        [nearest(&self.markers), nearest(&self.packages)]
    }

    /// Babel's aliases and roots that apply to `file`, in order.
    fn babel(&self, file: &Path) -> impl Iterator<Item = &Rewrites> {
        let [relative, package] = self.scopes(file);
        relative
            .map(|s| &s.babelrc)
            .into_iter()
            .chain(package.map(|s| &s.babel))
    }

    /// The directories in which Babel looks for a bare name written in
    /// `file`, relative to the root.
    pub(crate) fn roots(&self, file: &Path) -> Vec<&PathBuf> {
        self.babel(file).flat_map(|r| &r.roots).collect()
    }

    /// Babel's alias that `specifier`, written in `file` (relative to the
    /// root), matches: the first whose key is the specifier or its first
    /// segments. Babel rewrites the source before any bundler sees it.
    pub(crate) fn babel_alias(&self, file: &Path, specifier: &str) -> Option<Rewritten<'_>> {
        let aliases = self.babel(file).flat_map(|r| &r.aliases);
        first_match(aliases, specifier)
    }

    /// A bundler's alias that `specifier`, written in `file`, matches.
    pub(crate) fn bundler_alias(&self, file: &Path, specifier: &str) -> Option<Rewritten<'_>> {
        let [_, package] = self.scopes(file);
        first_match(package.into_iter().flat_map(|s| &s.bundler), specifier)
    }

    /// The alias, Babel's or a bundler's, that `specifier` matches.
    pub(crate) fn matching(&self, file: &Path, specifier: &str) -> Option<Rewritten<'_>> {
        self.babel_alias(file, specifier)
            .or_else(|| self.bundler_alias(file, specifier))
    }
}

/// The first of `aliases` whose key is `specifier` or its first segments,
/// and where it leads.
fn first_match<'a>(
    aliases: impl IntoIterator<Item = &'a Alias>,
    specifier: &str,
) -> Option<Rewritten<'a>> {
    aliases.into_iter().find_map(|alias| {
        let rest = match specifier.strip_prefix(alias.key.as_str())? {
            "" => "",
            rest if !alias.exact => rest.strip_prefix('/')?,
            _ => return None,
        };
        let path = alias.target.as_ref().map(|target| match rest {
            "" => target.clone(),
            rest => target.join(rest),
        });
        Some(Rewritten {
            path,
            key: &alias.key,
            config: &alias.config,
        })
    })
}

/// The bundler whose config `file` is: `vite.config` or `webpack.config`.
fn bundler_of(file: &Path) -> Option<&'static str> {
    let name = file.file_name()?.to_str()?;
    let (stem, extension) = name.rsplit_once('.')?;
    let code = ["ts", "js", "mjs", "mts", "cjs", "cts"].contains(&extension);
    [VITE, WEBPACK].into_iter().find(|b| code && stem == *b)
}

/// The aliases the config at `config` (relative to the root) with `text`
/// declares, in order.
fn aliases_of(config: &Path, text: &str) -> Vec<Alias> {
    let Ok(source_type) = source_type(config) else {
        return Vec::new();
    };
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, text, source_type).parse();
    if parsed.fatal_error {
        return Vec::new();
    }
    let Some(dir) = components(config.parent().unwrap_or(Path::new(""))) else {
        return Vec::new();
    };
    let mut reader = Reader {
        vite: bundler_of(config) == Some(VITE),
        dir: padded(dir),
        modules: BTreeMap::new(),
        consts: BTreeMap::new(),
        root_set: false,
    };
    let body = &parsed.program.body;
    reader.bindings(body);
    let Some(object) = exported(body).and_then(|e| reader.object(e, 0)) else {
        return Vec::new();
    };
    reader.root_set = property(object, "root").is_some();
    let Some(alias) = property(object, "resolve")
        .and_then(|r| reader.object(r, 0))
        .and_then(|r| property(r, "alias"))
        .map(|a| reader.followed(a, 0))
    else {
        return Vec::new();
    };
    let declared: Vec<(String, &Expression)> = match alias {
        Expression::ObjectExpression(object) => object
            .properties
            .iter()
            .filter_map(|p| match p {
                ObjectPropertyKind::ObjectProperty(p) if !p.computed => {
                    Some((p.key.static_name()?.into_owned(), &p.value))
                }
                _ => None,
            })
            .collect(),
        // Vite's `[{ find, replacement }]`; a regex `find` is left out
        Expression::ArrayExpression(array) if reader.vite => array
            .elements
            .iter()
            .filter_map(|element| {
                let ArrayExpressionElement::ObjectExpression(entry) = element else {
                    return None;
                };
                let Expression::StringLiteral(find) = property(entry, "find")? else {
                    return None;
                };
                Some((find.value.to_string(), property(entry, "replacement")?))
            })
            .collect(),
        _ => Vec::new(),
    };
    declared
        .into_iter()
        .filter_map(|(key, value)| {
            // `key$` is webpack's exact match; to Vite it is a literal `$`
            let (key, exact) = match key.strip_suffix('$') {
                Some(_) if reader.vite => return None,
                Some(name) => (name.to_owned(), true),
                None => (key, false),
            };
            let target = landed(&reader.path(value, 0)?);
            (!key.is_empty()).then(|| Alias {
                key,
                exact,
                target,
                config: config.to_path_buf(),
            })
        })
        .collect()
}

/// The `alias` and `root` that Babel's config at `config` (relative to the
/// root) with `text` gives `babel-plugin-module-resolver`: the targets and
/// the roots relative to the root.
fn module_resolver(config: &Path, text: &str) -> (Vec<Alias>, Vec<PathBuf>) {
    let Some(dir) = components(config.parent().unwrap_or(Path::new(""))) else {
        return (Vec::new(), Vec::new());
    };
    let dir = padded(dir);
    // where an alias's value leads: `Some(None)` above the root
    let relative = |value: &str| -> Option<Option<PathBuf>> {
        // a package name, an absolute path or a glob is no directory here
        if !value.starts_with('.') || value.contains('*') {
            return None;
        }
        Some(landed(&join(dir.clone(), [value])?))
    };
    // a root is a directory, `./` written or not, inside the root
    let root_dir = |value: &str| -> Option<PathBuf> {
        if value.contains('*') {
            return None;
        }
        landed(&join(dir.clone(), [value])?)
    };
    // (key, target) pairs and roots, each as the config writes them
    let mut aliases: Vec<(String, Option<Option<PathBuf>>)> = Vec::new();
    let mut roots: Vec<PathBuf> = Vec::new();
    let is_plugin = |name: &str| matches!(name, "module-resolver" | "babel-plugin-module-resolver");
    match config.extension().and_then(|e| e.to_str()) {
        Some("js" | "cjs" | "mjs" | "cts") => {
            let Ok(source_type) = source_type(config) else {
                return (Vec::new(), Vec::new());
            };
            let allocator = Allocator::default();
            let parsed = Parser::new(&allocator, text, source_type).parse();
            let mut reader = Reader {
                vite: false,
                dir: dir.clone(),
                modules: BTreeMap::new(),
                consts: BTreeMap::new(),
                root_set: false,
            };
            reader.bindings(&parsed.program.body);
            let options = exported(&parsed.program.body)
                .and_then(|e| reader.object(e, 0))
                .and_then(|c| property(c, "plugins"))
                .and_then(|p| match reader.followed(p, 0) {
                    Expression::ArrayExpression(plugins) => Some(plugins),
                    _ => None,
                })
                .and_then(|plugins| {
                    plugins.elements.iter().find_map(|plugin| {
                        let ArrayExpressionElement::ArrayExpression(entry) = plugin else {
                            return None;
                        };
                        let [name, options, ..] = entry.elements.as_slice() else {
                            return None;
                        };
                        let ArrayExpressionElement::StringLiteral(name) = name else {
                            return None;
                        };
                        let ArrayExpressionElement::ObjectExpression(options) = options else {
                            return None;
                        };
                        is_plugin(name.value.as_str()).then_some(options)
                    })
                });
            let Some(options) = options else {
                return (Vec::new(), Vec::new());
            };
            // a string written out, or a path computed (`path.resolve(__dirname,
            // 'src')`)
            let root_of = |expr: &Expression| -> Option<PathBuf> {
                match reader.followed(expr, 0) {
                    Expression::StringLiteral(s) => root_dir(s.value.as_str()),
                    value => landed(&reader.path(value, 0)?),
                }
            };
            if let Some(root) = property(options, "root") {
                match reader.followed(root, 0) {
                    Expression::ArrayExpression(a) => roots.extend(
                        a.elements
                            .iter()
                            .filter_map(ArrayExpressionElement::as_expression)
                            .filter_map(root_of),
                    ),
                    value => roots.extend(root_of(value)),
                }
            }
            if let Some(Expression::ObjectExpression(alias)) =
                property(options, "alias").map(|a| reader.followed(a, 0))
            {
                for p in &alias.properties {
                    let ObjectPropertyKind::ObjectProperty(p) = p else {
                        continue;
                    };
                    let Some(key) = p.key.static_name().filter(|_| !p.computed) else {
                        continue;
                    };
                    let target = match reader.followed(&p.value, 0) {
                        Expression::StringLiteral(s) => relative(s.value.as_str()),
                        value => reader.path(value, 0).map(|c| landed(&c)),
                    };
                    aliases.push((key.into_owned(), target));
                }
            }
        }
        _ => {
            let Some(value) = json_config(text) else {
                return (Vec::new(), Vec::new());
            };
            let value = match config.file_name().and_then(|n| n.to_str()) {
                Some("package.json") => value.get("babel").cloned().unwrap_or_default(),
                _ => value,
            };
            let plugins = value.get("plugins").and_then(|p| p.as_array());
            let options = plugins.into_iter().flatten().find_map(|plugin| {
                let entry = plugin.as_array()?;
                is_plugin(entry.first()?.as_str()?)
                    .then(|| entry.get(1))
                    .flatten()
            });
            let Some(options) = options else {
                return (Vec::new(), Vec::new());
            };
            let root = options.get("root");
            let root = match root {
                Some(serde_json::Value::String(r)) => vec![r.as_str()],
                Some(serde_json::Value::Array(rs)) => {
                    rs.iter().filter_map(|r| r.as_str()).collect()
                }
                _ => Vec::new(),
            };
            roots.extend(root.into_iter().filter_map(root_dir));
            for (key, target) in options
                .get("alias")
                .and_then(|a| a.as_object())
                .into_iter()
                .flatten()
            {
                aliases.push((key.clone(), target.as_str().and_then(relative)));
            }
        }
    }
    let aliases = aliases
        .into_iter()
        .filter_map(|(key, target)| {
            // `^@(.+)`, `x$`: a regular expression
            let regex = key.starts_with('^') || key.ends_with('$');
            (!key.is_empty() && !regex).then_some(Alias {
                key,
                exact: false,
                target: target?,
                config: config.to_path_buf(),
            })
        })
        .collect();
    (aliases, roots)
}

/// What `export default` or `module.exports =` gives.
fn exported<'a, 'b>(body: &'b [Statement<'a>]) -> Option<&'b Expression<'a>> {
    body.iter().rev().find_map(|statement| match statement {
        Statement::ExportDefaultDeclaration(export) => export.declaration.as_expression(),
        Statement::ExpressionStatement(statement) => {
            let Expression::AssignmentExpression(assign) = &statement.expression else {
                return None;
            };
            let AssignmentTarget::StaticMemberExpression(target) = &assign.left else {
                return None;
            };
            let module = matches!(&target.object, Expression::Identifier(o) if o.name == "module");
            (module && target.property.name == "exports").then_some(&assign.right)
        }
        _ => None,
    })
}

/// The property `name` of `object`, written out.
fn property<'a, 'b>(object: &'b ObjectExpression<'a>, name: &str) -> Option<&'b Expression<'a>> {
    object.properties.iter().find_map(|p| match p {
        ObjectPropertyKind::ObjectProperty(p)
            if !p.computed && p.key.static_name().as_deref() == Some(name) =>
        {
            Some(&p.value)
        }
        _ => None,
    })
}

struct Reader<'a, 'b> {
    vite: bool,
    /// The config's directory, as components relative to the root.
    dir: Vec<String>,
    /// What module-level imports and requires bind: `path` to `path`,
    /// `resolve` to `path.resolve`, with `node:` dropped.
    modules: BTreeMap<String, String>,
    /// Module-level `const` bindings, by name.
    consts: BTreeMap<String, &'b Expression<'a>>,
    /// The config sets Vite's `root`, which `/src` is relative to.
    root_set: bool,
}

impl<'a, 'b> Reader<'a, 'b> {
    fn bindings(&mut self, body: &'b [Statement<'a>]) {
        for statement in body {
            match statement {
                Statement::ImportDeclaration(import) => {
                    let module = module_name(import.source.value.as_str());
                    for specifier in import.specifiers.iter().flatten() {
                        let (local, bound) = match specifier {
                            ImportDeclarationSpecifier::ImportSpecifier(s) => {
                                (&s.local.name, format!("{module}.{}", s.imported.name()))
                            }
                            ImportDeclarationSpecifier::ImportDefaultSpecifier(s) => {
                                (&s.local.name, module.to_owned())
                            }
                            ImportDeclarationSpecifier::ImportNamespaceSpecifier(s) => {
                                (&s.local.name, module.to_owned())
                            }
                        };
                        self.modules.insert(local.to_string(), bound);
                    }
                }
                Statement::VariableDeclaration(declaration) => {
                    for declarator in &declaration.declarations {
                        let Some(init) = &declarator.init else {
                            continue;
                        };
                        match (&declarator.id, required(init)) {
                            // `const path = require('path')`
                            (BindingPattern::BindingIdentifier(id), Some(module)) => {
                                self.modules.insert(id.name.to_string(), module.to_owned());
                            }
                            // `const { resolve } = require('path')`
                            (BindingPattern::ObjectPattern(pattern), Some(module)) => {
                                for p in &pattern.properties {
                                    let (Some(key), BindingPattern::BindingIdentifier(local)) =
                                        (p.key.static_name(), &p.value)
                                    else {
                                        continue;
                                    };
                                    self.modules
                                        .insert(local.name.to_string(), format!("{module}.{key}"));
                                }
                            }
                            (BindingPattern::BindingIdentifier(id), None)
                                if declaration.kind.is_const() =>
                            {
                                self.consts.insert(id.name.to_string(), init);
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// `expr` through module-level `const` bindings and what only wraps a
    /// value (`as`, `satisfies`, parentheses).
    fn followed(&self, expr: &'b Expression<'a>, depth: usize) -> &'b Expression<'a> {
        let expr = expr.get_inner_expression();
        match expr {
            Expression::Identifier(id) if depth < MAX_DEPTH => {
                match self.consts.get(id.name.as_str()) {
                    Some(init) => self.followed(init, depth + 1),
                    None => expr,
                }
            }
            _ => expr,
        }
    }

    /// The object a config value stands for: written out, through
    /// `defineConfig(..)`, or what a function returns when its body
    /// returns one object.
    fn object(&self, expr: &'b Expression<'a>, depth: usize) -> Option<&'b ObjectExpression<'a>> {
        if depth > MAX_DEPTH {
            return None;
        }
        match self.followed(expr, depth) {
            Expression::ObjectExpression(object) => Some(object),
            Expression::CallExpression(call) => {
                let callee = self.qualified(&call.callee)?;
                if !callee.ends_with(".defineConfig") {
                    return None;
                }
                let first = call.arguments.first()?.as_expression()?;
                self.object(first, depth + 1)
            }
            Expression::ArrowFunctionExpression(arrow) => match &arrow.body {
                ArrowFunctionBody::FunctionBody(body) => self.returned(body, depth),
                value => self.object(value.as_expression()?, depth + 1),
            },
            Expression::FunctionExpression(function) => {
                self.returned(function.body.as_ref()?, depth)
            }
            _ => None,
        }
    }

    /// The object a function body returns, when its only `return` at its
    /// top level gives one.
    fn returned(
        &self,
        body: &'b FunctionBody<'a>,
        depth: usize,
    ) -> Option<&'b ObjectExpression<'a>> {
        let mut returns = body.statements.iter().filter_map(|s| match s {
            Statement::ReturnStatement(r) => Some(r),
            _ => None,
        });
        let only = returns.next()?;
        if returns.next().is_some() {
            return None;
        }
        self.object(only.argument.as_ref()?, depth + 1)
    }

    /// The dotted name `expr` stands for through the module-level imports
    /// and requires (`path.resolve`), or a global's own name (`URL`).
    fn qualified(&self, expr: &Expression) -> Option<String> {
        match expr.get_inner_expression() {
            Expression::Identifier(id) => Some(
                self.modules
                    .get(id.name.as_str())
                    .cloned()
                    .unwrap_or_else(|| id.name.to_string()),
            ),
            Expression::StaticMemberExpression(m) => Some(format!(
                "{}.{}",
                self.qualified(&m.object)?,
                m.property.name
            )),
            _ => None,
        }
    }

    /// The path `expr` computes, as components relative to the root.
    fn path(&self, expr: &'b Expression<'a>, depth: usize) -> Option<Vec<String>> {
        if depth > MAX_DEPTH {
            return None;
        }
        match self.followed(expr, depth) {
            Expression::Identifier(id) if id.name == "__dirname" => Some(self.dir.clone()),
            // a path from the project root, which only Vite reads so
            Expression::StringLiteral(s) if self.vite && !self.root_set => {
                let rest = s.value.strip_prefix('/')?;
                join(self.dir.clone(), [rest])
            }
            Expression::CallExpression(call) => {
                let callee = self.qualified(&call.callee)?;
                let args: Vec<&Expression> = call
                    .arguments
                    .iter()
                    .map(|a| a.as_expression())
                    .collect::<Option<_>>()?;
                match (callee.as_str(), args.split_first()?) {
                    ("path.resolve" | "path.join", (first, rest)) => {
                        let parts: Vec<&str> =
                            rest.iter().map(|e| literal(e)).collect::<Option<_>>()?;
                        join(self.path(first, depth + 1)?, parts)
                    }
                    ("url.fileURLToPath", (url, [])) => self.url(url, depth + 1),
                    _ => None,
                }
            }
            // `new URL('./src', import.meta.url).pathname`
            Expression::StaticMemberExpression(m) if m.property.name == "pathname" => {
                self.url(&m.object, depth + 1)
            }
            _ => None,
        }
    }

    /// The path of `new URL('./src', import.meta.url)`.
    fn url(&self, expr: &'b Expression<'a>, depth: usize) -> Option<Vec<String>> {
        let Expression::NewExpression(new) = self.followed(expr, depth) else {
            return None;
        };
        if !matches!(self.qualified(&new.callee)?.as_str(), "URL" | "url.URL") {
            return None;
        }
        let [relative, base] = new.arguments.as_slice() else {
            return None;
        };
        let Expression::StaticMemberExpression(base) = base.as_expression()? else {
            return None;
        };
        let meta = matches!(&base.object, Expression::ImportMeta(_));
        if !meta || base.property.name != "url" {
            return None;
        }
        let relative = literal(relative.as_expression()?)?;
        if !relative.starts_with('.') {
            return None;
        }
        join(self.dir.clone(), [relative])
    }
}

/// `node:path` as `path`.
fn module_name(source: &str) -> &str {
    source.strip_prefix("node:").unwrap_or(source)
}

/// The module `require('m')` loads, `node:` dropped.
fn required<'s>(expr: &'s Expression) -> Option<&'s str> {
    let Expression::CallExpression(call) = expr.get_inner_expression() else {
        return None;
    };
    let callee = matches!(&call.callee, Expression::Identifier(c) if c.name == "require");
    let [argument] = call.arguments.as_slice() else {
        return None;
    };
    match argument.as_expression()? {
        Expression::StringLiteral(s) if callee => Some(module_name(s.value.as_str())),
        _ => None,
    }
}

fn literal<'s>(expr: &'s Expression) -> Option<&'s str> {
    match expr {
        Expression::StringLiteral(s) => Some(s.value.as_str()),
        _ => None,
    }
}

/// `path` joined with relative `parts`, `..` leaving a directory; `None`
/// when that leaves the root or a part is absolute.
fn join<'p>(
    mut path: Vec<String>,
    parts: impl IntoIterator<Item = &'p str>,
) -> Option<Vec<String>> {
    for part in parts {
        for component in Path::new(part).components() {
            match component {
                Component::Normal(name) => path.push(name.to_str()?.to_owned()),
                Component::CurDir => {}
                Component::ParentDir => {
                    path.pop()?;
                }
                Component::RootDir | Component::Prefix(_) => return None,
            }
        }
    }
    Some(path)
}

fn components(dir: &Path) -> Option<Vec<String>> {
    join(Vec::new(), [dir.to_str()?])
}

/// `dir` below [`PAD`] segments that stand for what is above the root.
fn padded(dir: Vec<String>) -> Vec<String> {
    let mut path = vec![ABOVE.to_owned(); PAD];
    path.extend(dir);
    path
}

/// Where a path computed from a [`padded`] directory lands, relative to
/// the root; `None` above the root, where it climbed into the padding.
fn landed(path: &[String]) -> Option<PathBuf> {
    let (above, inside) = path.split_at_checked(PAD)?;
    above
        .iter()
        .all(|segment| segment == ABOVE)
        .then(|| inside.iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The aliases of the config at `path` with `text`, as `key -> target`,
    /// `$` marking an exact one.
    fn aliases(path: &str, text: &str) -> Vec<String> {
        aliases_of(Path::new(path), text)
            .into_iter()
            .map(|a| {
                let exact = if a.exact { "$" } else { "" };
                let target = a
                    .target
                    .as_ref()
                    .map_or("(outside)".into(), |t| t.display().to_string());
                format!("{}{exact} -> {target}", a.key)
            })
            .collect()
    }

    #[test]
    fn vite_forms_that_wrap_the_config_are_read() {
        // a module-level config and alias table, `satisfies`, `node:path`
        // as a namespace
        let text = "import * as path from 'node:path';\n\
                    import type { UserConfig } from 'vite';\n\
                    const alias = { '@': path.join(__dirname, 'src') };\n\
                    const config = { resolve: { alias } } satisfies UserConfig;\n\
                    export default config;\n";
        assert_eq!(aliases("web/vite.config.ts", text), ["@ -> web/src"]);
        // a function whose block returns one object, the array form, a
        // regex `find` left out
        let text = "import { defineConfig } from 'vite';\n\
                    import { resolve } from 'path';\n\
                    export default defineConfig(function ({ command }) {\n\
                    \x20 const base = resolve(__dirname, 'lib');\n\
                    \x20 return { resolve: { alias: [\n\
                    \x20   { find: /^~(.*)$/, replacement: base },\n\
                    \x20   { find: '~lib', replacement: resolve(__dirname, 'lib') },\n\
                    \x20   { find: 'base', replacement: base },\n\
                    \x20 ] } };\n\
                    });\n";
        // `base` is a local of the function, not a module-level const
        assert_eq!(aliases("vite.config.mts", text), ["~lib -> lib"]);
    }

    #[test]
    fn what_cannot_be_computed_or_needs_the_importer_counts_for_nothing() {
        let text = "import path from 'path';\n\
                    import { mergeConfig } from 'vite';\n\
                    import base from './base';\n\
                    export default {\n\
                    \x20 root: 'app',\n\
                    \x20 resolve: { alias: {\n\
                    \x20   ...base.resolve.alias,\n\
                    \x20   '/abs': '/src',\n\
                    \x20   rel: './src',\n\
                    \x20   pkg: 'preact/compat',\n\
                    \x20   cwd: path.resolve('src'),\n\
                    \x20   proc: path.resolve(process.cwd(), 'src'),\n\
                    \x20   up: path.resolve(__dirname, '../..'),\n\
                    \x20   [computed]: path.resolve(__dirname, 'src'),\n\
                    \x20   ok: path.resolve(__dirname, 'src'),\n\
                    \x20 } },\n\
                    } as const;\n";
        // `root` is set, so `/src` is not from this directory; a path above
        // the root keeps its key, which then leads to no scanned file
        assert_eq!(
            aliases("app/vite.config.js", text),
            ["up -> (outside)", "ok -> app/src"]
        );
        // a merged config is not read
        let text = "import { mergeConfig } from 'vite';\n\
                    export default mergeConfig(base, { resolve: { alias: { a: '/a' } } });\n";
        assert!(aliases("vite.config.ts", text).is_empty());
    }

    #[test]
    fn webpack_configs_in_common_js_are_read() {
        let text = "const { resolve } = require('node:path');\n\
                    module.exports = (env, argv) => ({\n\
                    \x20 resolve: { alias: {\n\
                    \x20   Utilities: resolve(__dirname, 'src/utilities/'),\n\
                    \x20   Templates$: resolve(__dirname, 'src/templates/main.js'),\n\
                    \x20   Gone: false,\n\
                    \x20 } },\n\
                    });\n";
        assert_eq!(
            aliases("pack/webpack.config.js", text),
            [
                "Utilities -> pack/src/utilities",
                "Templates$ -> pack/src/templates/main.js"
            ]
        );
        // an array of configs is not read
        let text = "const path = require('path');\n\
                    module.exports = [{ resolve: { alias: { a: path.resolve(__dirname, 'a') } } }];\n";
        assert!(aliases("webpack.config.js", text).is_empty());
    }

    #[test]
    fn babel_s_module_resolver_gives_aliases_and_roots() {
        let read = |path: &str, text: &str| -> (Vec<String>, Vec<String>) {
            let (aliases, roots) = module_resolver(Path::new(path), text);
            let aliases = aliases
                .iter()
                .map(|a| {
                    let target = a
                        .target
                        .as_ref()
                        .map_or("(outside)".into(), |t| t.display().to_string());
                    format!("{} -> {target}", a.key)
                })
                .collect();
            let roots = roots.iter().map(|r| r.display().to_string()).collect();
            (aliases, roots)
        };
        let text = "const path = require('path');\n\
                    module.exports = {\n\
                    \x20 plugins: [\n\
                    \x20   'other-plugin',\n\
                    \x20   ['module-resolver', {\n\
                    \x20     root: ['./src', 'lib', path.resolve(__dirname, 'vendor'), './packages/*'],\n\
                    \x20     alias: {\n\
                    \x20       shared: path.resolve(__dirname, '../shared'),\n\
                    \x20       '^~(.+)': './src/\\\\1',\n\
                    \x20       lib$: './lib',\n\
                    \x20       vendor: 'vendor-pkg',\n\
                    \x20     },\n\
                    \x20   }],\n\
                    \x20 ],\n\
                    };\n";
        assert_eq!(
            read("mobile/babel.config.js", text),
            (
                vec!["shared -> shared".to_owned()],
                vec![
                    "mobile/src".to_owned(),
                    "mobile/lib".to_owned(),
                    "mobile/vendor".to_owned()
                ]
            )
        );
        // JSON with comments, roots as a list, a glob left out
        let text = "{ /* roots */ \"plugins\": [[\"module-resolver\", \
                    { \"root\": [\"./app\", \"./packages/*\"], \"alias\": { \"@\": \"./app\" } }]] }";
        assert_eq!(
            read("web/.babelrc.json", text),
            (vec!["@ -> web/app".to_owned()], vec!["web/app".to_owned()])
        );
    }

    #[test]
    fn a_vite_key_ending_in_a_dollar_is_left_out_and_order_is_kept() {
        let text = "import { fileURLToPath, URL } from 'node:url';\n\
                    export default {\n\
                    \x20 resolve: { alias: {\n\
                    \x20   'vue$': fileURLToPath(new URL('./src/vue.ts', import.meta.url)),\n\
                    \x20   '@/deep': new URL('./src/deep', import.meta.url).pathname,\n\
                    \x20   '@': fileURLToPath(new URL('./src', import.meta.url)),\n\
                    \x20 } },\n\
                    };\n";
        assert_eq!(
            aliases("vite.config.js", text),
            ["@/deep -> src/deep", "@ -> src"]
        );
    }
}
