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

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    ArrayExpressionElement, ArrowFunctionBody, AssignmentTarget, BindingPattern, Expression,
    FunctionBody, ImportDeclarationSpecifier, ObjectExpression, ObjectPropertyKind, Statement,
};
use oxc_parser::Parser;

use super::source::source_type;
use crate::RepoContext;

/// Configs read, by their name before the extension.
const VITE: &str = "vite.config";
const WEBPACK: &str = "webpack.config";

/// Module-level `const` bindings a value is followed through, at most.
const MAX_DEPTH: usize = 8;

/// The aliases of every package whose bundler config declares some.
#[derive(Debug, Default)]
pub(crate) struct BundlerAliases {
    /// By the package's directory, relative to the root.
    scopes: BTreeMap<PathBuf, Vec<Alias>>,
    /// The directories that hold a `package.json`, relative to the root.
    packages: BTreeSet<PathBuf>,
}

/// One alias, in the order its config declares it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Alias {
    key: String,
    /// webpack's `key$`: only the key itself, no path below it.
    exact: bool,
    /// Relative to the root.
    target: PathBuf,
    /// The config that declares it, relative to the root.
    config: PathBuf,
}

/// An alias a specifier matches.
pub(crate) struct Rewritten<'a> {
    /// The path it leads to, relative to the root.
    pub(crate) path: PathBuf,
    pub(crate) key: &'a str,
    pub(crate) config: &'a Path,
}

impl BundlerAliases {
    pub(crate) fn read(ctx: &RepoContext) -> Self {
        let packages: BTreeSet<PathBuf> = ctx
            .files()
            .iter()
            .filter(|f| f.file_name().is_some_and(|n| n == "package.json"))
            .map(|f| f.parent().unwrap_or(Path::new("")).to_path_buf())
            .collect();
        let mut configs: Vec<&PathBuf> = ctx
            .files()
            .iter()
            .filter(|f| bundler_of(f).is_some())
            .filter(|f| packages.contains(f.parent().unwrap_or(Path::new(""))))
            .collect();
        // Vite before webpack in one package, each in path order
        configs.sort_by_key(|f| (bundler_of(f) != Some(VITE), (*f).clone()));
        let mut scopes: BTreeMap<PathBuf, Vec<Alias>> = BTreeMap::new();
        for config in configs {
            let Ok(text) = ctx.read_to_string(config) else {
                continue;
            };
            let aliases = aliases_of(config, &text);
            if !aliases.is_empty() {
                let dir = config.parent().unwrap_or(Path::new("")).to_path_buf();
                scopes.entry(dir).or_default().extend(aliases);
            }
        }
        BundlerAliases { scopes, packages }
    }

    /// The alias that `specifier`, written in `file` (relative to the
    /// root), matches: the first its package's configs declare whose key is
    /// the specifier or the specifier's first segments.
    pub(crate) fn matching(&self, file: &Path, specifier: &str) -> Option<Rewritten<'_>> {
        if self.scopes.is_empty() || bundler_of(file).is_some() {
            return None;
        }
        let package = file
            .ancestors()
            .skip(1)
            .find(|dir| self.packages.contains(*dir))?;
        self.scopes.get(package)?.iter().find_map(|alias| {
            let rest = match specifier.strip_prefix(alias.key.as_str())? {
                "" => "",
                rest if !alias.exact => rest.strip_prefix('/')?,
                _ => return None,
            };
            let path = match rest {
                "" => alias.target.clone(),
                rest => alias.target.join(rest),
            };
            Some(Rewritten {
                path,
                key: &alias.key,
                config: &alias.config,
            })
        })
    }
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
        dir,
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
            let target: PathBuf = reader.path(value, 0)?.iter().collect();
            (!key.is_empty()).then(|| Alias {
                key,
                exact,
                target,
                config: config.to_path_buf(),
            })
        })
        .collect()
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
                format!("{}{exact} -> {}", a.key, a.target.display())
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
        // `root` is set, so `/src` is not from this directory
        assert_eq!(aliases("app/vite.config.js", text), ["ok -> app/src"]);
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
