//! What one TS/JS file says, read from its `oxc` AST: the modules its
//! `import` and `export ... from` statements load and the names they take,
//! the calls that load modules anywhere in the file (`require`, `import()`,
//! test mocks, `import()` types), the declarations it exports, and its export
//! table (see [`ExportTable`]). Declarations are read at the top level only.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use archmap_core::{SymbolKind, WHOLE_MODULE};
use oxc_allocator::Allocator;
use oxc_ast::ast::{
    AssignmentExpression, AssignmentTarget, CallExpression, Class, ClassElement, Declaration,
    ExportDefaultDeclarationKind, Expression, FormalParameters, Function,
    ImportDeclarationSpecifier, ImportExpression, MethodDefinitionKind, ObjectProperty,
    ObjectPropertyKind, Statement, TSAccessibility, TSImportEqualsDeclaration, TSImportType,
    TSImportTypeQualifier, TSModuleReference,
};
use oxc_ast::AstKind;
use oxc_ast_visit::{walk, Visit};
use oxc_parser::Parser;
use oxc_span::{GetSpan, SourceType};

use super::exports::{Export, ExportTable};

/// A declaration that other files can import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExportedSymbol {
    pub name: String,
    pub kind: SymbolKind,
    pub line: u32,
    pub signature: Option<String>,
}

/// A statement that loads another module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImportStatement {
    pub specifier: String,
    pub line: u32,
    /// `import`, or `export` for `export ... from`.
    pub note: &'static str,
    /// The names the statement takes, as the loaded module exports them:
    /// `default` for a default import; [`WHOLE_MODULE`] for a namespace
    /// import, `import x = require()` and `export *`; the specifiers'
    /// local names for `export { .. } from`; empty for a side-effect import.
    pub names: Vec<String>,
    /// The names among `names` taken as types only (`import type`,
    /// `{ type A }`, `export type ... from`), which the compiler erases.
    pub types: BTreeSet<String>,
    /// Inside a function body, so it runs only when the function is called.
    pub local: bool,
}

/// A call that loads a module by a name computed at runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DynamicCall {
    /// `require` or `import()`.
    pub call: &'static str,
    pub line: u32,
    pub local: bool,
}

/// Calls that load a module named by their first argument; `vi.mock` and
/// `jest.mock` with a factory never load the real one, which their note
/// keeps apart.
const MODULE_CALLS: [&str; 10] = [
    "vi.mock",
    "vi.doMock",
    "vi.unmock",
    "vi.importActual",
    "vi.importMock",
    "jest.mock",
    "jest.doMock",
    "jest.unmock",
    "jest.requireActual",
    "jest.requireMock",
];

#[derive(Debug, Default)]
pub(crate) struct ParsedFile {
    /// Statements first, then calls, each in file order.
    pub imports: Vec<ImportStatement>,
    pub dynamic: Vec<DynamicCall>,
    pub symbols: Vec<ExportedSymbol>,
    /// What the file exports, with indices into `imports`.
    pub exports: ExportTable,
    /// What makes TypeScript read the file as a module rather than a
    /// script: an `import` or `export` declaration, `import x = require()`,
    /// `import.meta`, and in JavaScript a `require` call or an assignment to
    /// `module.exports` or `exports`.
    pub module_syntax: bool,
    /// The file holds JSX, which makes it a module where the tsconfig's
    /// `jsx` imports a runtime.
    pub has_jsx: bool,
    /// Without module syntax: every top-level declaration, the first of a
    /// name, which a script declares globally.
    pub globals: Vec<ExportedSymbol>,
}

/// Characters of a signature kept; a longer one ends in `...`.
const MAX_SIGNATURE: usize = 200;

/// Parse `text`, the file at `path`; its extension decides TypeScript or
/// JavaScript, JSX and `.d.ts`. Fails only when the parser gives up.
pub(crate) fn parse(path: &Path, text: &str) -> Result<ParsedFile, String> {
    let mut source_type = SourceType::from_path(path).map_err(|e| e.to_string())?;
    let javascript = source_type.is_javascript();
    if javascript {
        // Plain `.js` files carry JSX as often as `.jsx` files do.
        source_type = source_type.with_jsx(true);
    }
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, text, source_type).parse();
    if parsed.fatal_error {
        return Err(parsed
            .diagnostics
            .first()
            .map_or_else(|| "the parser gave up".to_owned(), ToString::to_string));
    }
    let lines = LineIndex::new(text);
    let comments: Vec<(u32, u32)> = parsed
        .program
        .comments
        .iter()
        .map(|c| (c.span.start, c.span.end))
        .collect();
    let source = Source {
        text,
        lines: &lines,
        comments: &comments,
    };

    // Top-level declarations by name, for `export { a }` and
    // `export default a`. A name declared twice (a value and its type,
    // overloads) is its first declaration.
    let mut locals: BTreeMap<String, Vec<ExportedSymbol>> = BTreeMap::new();
    for statement in &parsed.program.body {
        if let Some(declaration) = statement.as_declaration() {
            for symbols in source.declared(declaration, statement.span().start) {
                locals.entry(symbols[0].name.clone()).or_insert(symbols);
            }
        }
    }

    let mut file = ParsedFile::default();
    // Names that import declarations bind (imports are hoisted, so one after
    // the export still counts): the statement, the export taken (`None` for
    // a namespace), and whether it is taken as a type only.
    let mut bindings: BTreeMap<String, (usize, Option<String>, bool)> = BTreeMap::new();
    // `export { a as b }` without a source and `export default a`, read
    // once every binding is known: (local name, exported name, line, type
    // only).
    let mut exported: Vec<(String, String, u32, bool)> = Vec::new();
    for statement in &parsed.program.body {
        let start = statement.span().start;
        let line = lines.line(start);
        let index = file.imports.len();
        let load = |specifier: String,
                    note: &'static str,
                    names: Vec<String>,
                    types: BTreeSet<String>| ImportStatement {
            specifier,
            line,
            note,
            names,
            types,
            local: false,
        };
        let whole = || vec![WHOLE_MODULE.to_owned()];
        let whole_if = |types: bool| match types {
            true => BTreeSet::from([WHOLE_MODULE.to_owned()]),
            false => BTreeSet::new(),
        };
        if statement.is_module_declaration() {
            file.module_syntax = true;
        }
        match statement {
            Statement::ImportDeclaration(d) => {
                let mut names = Vec::new();
                let (mut types, mut values) = (BTreeSet::new(), BTreeSet::new());
                for specifier in d.specifiers.iter().flatten() {
                    let (local, taken, inline_type) = match specifier {
                        ImportDeclarationSpecifier::ImportSpecifier(s) => (
                            s.local.name.to_string(),
                            Some(s.imported.name().to_string()),
                            s.import_kind.is_type(),
                        ),
                        ImportDeclarationSpecifier::ImportDefaultSpecifier(s) => {
                            (s.local.name.to_string(), Some("default".to_owned()), false)
                        }
                        ImportDeclarationSpecifier::ImportNamespaceSpecifier(s) => {
                            (s.local.name.to_string(), None, false)
                        }
                    };
                    let type_only = d.import_kind.is_type() || inline_type;
                    let name = taken.clone().unwrap_or_else(|| WHOLE_MODULE.to_owned());
                    if type_only {
                        types.insert(name.clone());
                    } else {
                        values.insert(name.clone());
                    }
                    names.push(name);
                    bindings.insert(local, (index, taken, type_only));
                }
                // a name taken as a value and as a type is loaded
                types.retain(|name| !values.contains(name));
                file.imports
                    .push(load(d.source.value.to_string(), "import", names, types));
            }
            Statement::ExportFromDeclaration(d) => {
                let mut names = Vec::new();
                let (mut types, mut values) = (BTreeSet::new(), BTreeSet::new());
                for s in &d.specifiers {
                    let local = s.local.name().to_string();
                    let type_only = d.export_kind.is_type() || s.export_kind.is_type();
                    let export = Export::Reexport {
                        import: index,
                        name: local.clone(),
                        line,
                        type_only,
                    };
                    file.exports
                        .names
                        .entry(s.exported.name().to_string())
                        .or_insert(export);
                    if type_only {
                        types.insert(local.clone());
                    } else {
                        values.insert(local.clone());
                    }
                    names.push(local);
                }
                types.retain(|name| !values.contains(name));
                file.imports
                    .push(load(d.source.value.to_string(), "export", names, types));
            }
            Statement::ExportAllDeclaration(d) => {
                let type_only = d.export_kind.is_type();
                match &d.exported {
                    Some(exported) => {
                        file.exports
                            .names
                            .entry(exported.name().to_string())
                            .or_insert(Export::Namespace {
                                import: index,
                                line,
                                type_only,
                            });
                    }
                    None => file.exports.stars.push((index, line, type_only)),
                }
                file.imports.push(load(
                    d.source.value.to_string(),
                    "export",
                    whole(),
                    whole_if(type_only),
                ));
            }
            Statement::TSImportEqualsDeclaration(d) => {
                if let Some(specifier) = required_by(d) {
                    file.module_syntax = true;
                    let type_only = d.import_kind.is_type();
                    bindings.insert(d.id.name.to_string(), (index, None, type_only));
                    file.imports
                        .push(load(specifier, "import", whole(), whole_if(type_only)));
                }
            }
            Statement::ExportDeclaration(d) => {
                if let Declaration::TSImportEqualsDeclaration(i) = &d.declaration {
                    if let Some(specifier) = required_by(i) {
                        let type_only = i.import_kind.is_type();
                        file.exports.names.entry(i.id.name.to_string()).or_insert(
                            Export::Namespace {
                                import: index,
                                line,
                                type_only,
                            },
                        );
                        file.imports
                            .push(load(specifier, "import", whole(), whole_if(type_only)));
                    }
                }
                for symbols in source.declared(&d.declaration, start) {
                    file.exports
                        .names
                        .entry(symbols[0].name.clone())
                        .or_insert(Export::Local);
                    file.symbols.extend(symbols);
                }
                // `export const { a, b } = o` binds names that make no symbol,
                // and without them a star elsewhere would answer for them.
                if let Declaration::VariableDeclaration(v) = &d.declaration {
                    for declarator in &v.declarations {
                        for id in declarator.id.get_binding_identifiers() {
                            file.exports
                                .names
                                .entry(id.name.to_string())
                                .or_insert(Export::Local);
                        }
                    }
                }
            }
            Statement::ExportNamedDeclaration(d) => {
                for specifier in &d.specifiers {
                    let local = specifier.local.name().to_string();
                    let name = specifier.exported.name().to_string();
                    if let Some(symbols) = locals.get(&local) {
                        // `export { a as default }` keeps `a`, as
                        // `export default a` does.
                        if name == "default" {
                            file.symbols.extend(symbols.iter().cloned());
                        } else {
                            file.symbols.extend(exported_as(symbols, &name));
                        }
                    }
                    let type_only = d.export_kind.is_type() || specifier.export_kind.is_type();
                    exported.push((local, name, line, type_only));
                }
            }
            Statement::ExportDefaultDeclaration(d) => {
                let symbols = match &d.declaration {
                    ExportDefaultDeclarationKind::FunctionDeclaration(f) => {
                        source.function(f, start).into_iter().collect()
                    }
                    ExportDefaultDeclarationKind::ClassDeclaration(c) => source.class(c, start),
                    ExportDefaultDeclarationKind::TSInterfaceDeclaration(i) => vec![source.symbol(
                        i.id.name.to_string(),
                        SymbolKind::Trait,
                        start,
                        i.body.span.start,
                    )],
                    ExportDefaultDeclarationKind::Identifier(i) => {
                        locals.get(&i.name.to_string()).cloned().unwrap_or_default()
                    }
                    _ => Vec::new(),
                };
                file.symbols.extend(symbols);
                // An expression is the file's own and anonymous; a named
                // declaration lends the export its name.
                let local = match &d.declaration {
                    ExportDefaultDeclarationKind::Identifier(i) => i.name.to_string(),
                    ExportDefaultDeclarationKind::FunctionDeclaration(f) => {
                        f.id.as_ref()
                            .map(|id| id.name.to_string())
                            .unwrap_or_default()
                    }
                    ExportDefaultDeclarationKind::ClassDeclaration(c) => {
                        c.id.as_ref()
                            .map(|id| id.name.to_string())
                            .unwrap_or_default()
                    }
                    ExportDefaultDeclarationKind::TSInterfaceDeclaration(i) => {
                        i.id.name.to_string()
                    }
                    _ => String::new(),
                };
                exported.push((local, "default".to_owned(), line, false));
            }
            Statement::ExpressionStatement(e) if javascript => {
                commonjs_exports(&source, &e.expression, &locals, &mut file);
            }
            _ => {}
        }
    }
    for (local, name, line, export_type) in exported {
        let export = match bindings.get(&local) {
            Some((import, Some(taken), binding_type)) => Export::Reexport {
                import: *import,
                name: taken.clone(),
                line,
                type_only: export_type || *binding_type,
            },
            Some((import, None, binding_type)) => Export::Namespace {
                import: *import,
                line,
                type_only: export_type || *binding_type,
            },
            None => Export::Local,
        };
        if let Entry::Vacant(slot) = file.exports.names.entry(name) {
            // the first default export lends its declared name
            if slot.key() == "default" && export == Export::Local && !local.is_empty() {
                file.exports.default_name = Some(local);
            }
            slot.insert(export);
        }
    }
    let mut seen = BTreeSet::new();
    file.symbols.retain(|s| seen.insert(s.name.clone()));
    // after the statements, so the indices in the export table stay valid
    let mut calls = Calls {
        lines: &lines,
        javascript,
        functions: Vec::new(),
        imports: Vec::new(),
        dynamic: Vec::new(),
        module_syntax: false,
        has_jsx: false,
    };
    calls.visit_program(&parsed.program);
    file.imports.extend(calls.imports);
    file.dynamic = calls.dynamic;
    file.module_syntax |= calls.module_syntax;
    file.has_jsx = calls.has_jsx;
    if !file.module_syntax {
        let mut seen = BTreeSet::new();
        for statement in &parsed.program.body {
            if let Some(declaration) = statement.as_declaration() {
                for symbols in source.declared(declaration, statement.span().start) {
                    if seen.insert(symbols[0].name.clone()) {
                        file.globals.extend(symbols);
                    }
                }
            }
        }
    }
    Ok(file)
}

/// The symbols and export-table entries of a top-level CommonJS export:
/// `exports.a = ..`, `module.exports.a = ..` and `module.exports = ..`, the
/// last also as the default export. The `exports.a = exports.b = void 0`
/// that compilers write before the real assignments gives nothing.
fn commonjs_exports(
    source: &Source,
    expression: &Expression,
    locals: &BTreeMap<String, Vec<ExportedSymbol>>,
    file: &mut ParsedFile,
) {
    let mut targets = Vec::new();
    let mut value = expression;
    while let Expression::AssignmentExpression(a) = value {
        let Some(target) = commonjs_target(&a.left) else {
            break;
        };
        targets.push((target, a.span.start));
        value = &a.right;
    }
    if value.is_void_0() {
        return;
    }
    for (target, start) in targets {
        match target {
            Some(name) if name != "default" => {
                file.symbols
                    .extend(source.assigned(name, value, start, locals));
                file.exports
                    .names
                    .entry(name.to_owned())
                    .or_insert(Export::Local);
            }
            // the module itself, or its default export
            _ => {
                let declared = match value {
                    Expression::Identifier(id) => locals.get(id.name.as_str()).cloned(),
                    Expression::FunctionExpression(f) => {
                        f.id.as_ref()
                            .and_then(|_| source.function(f, start).map(|s| vec![s]))
                    }
                    Expression::ClassExpression(c) => {
                        Some(source.class(c, start)).filter(|s| !s.is_empty())
                    }
                    _ => None,
                };
                if let Some(symbols) = declared {
                    if file.exports.default_name.is_none() {
                        file.exports.default_name = Some(symbols[0].name.clone());
                    }
                    file.symbols.extend(symbols);
                }
                if let (None, Expression::ObjectExpression(object)) = (target, value) {
                    for property in &object.properties {
                        let ObjectPropertyKind::ObjectProperty(p) = property else {
                            continue;
                        };
                        let Some(name) = p.key.static_name() else {
                            continue;
                        };
                        file.symbols.extend(source.property(&name, p, locals));
                        file.exports
                            .names
                            .entry(name.into_owned())
                            .or_insert(Export::Local);
                    }
                }
                file.exports
                    .names
                    .entry("default".to_owned())
                    .or_insert(Export::Local);
            }
        }
    }
}

/// What an assignment exports: `Some(None)` for `module.exports` itself,
/// `Some(Some(name))` for `exports.name` and `module.exports.name`.
fn commonjs_target<'a>(target: &'a AssignmentTarget) -> Option<Option<&'a str>> {
    let AssignmentTarget::StaticMemberExpression(member) = target else {
        return None;
    };
    let property = member.property.name.as_str();
    match &member.object {
        Expression::Identifier(object) if object.name == "module" => {
            (property == "exports").then_some(None)
        }
        Expression::Identifier(object) if object.name == "exports" => Some(Some(property)),
        Expression::StaticMemberExpression(inner)
            if inner.property.name == "exports"
                && matches!(&inner.object, Expression::Identifier(o) if o.name == "module") =>
        {
            Some(Some(property))
        }
        _ => None,
    }
}

/// The calls of a file that load modules, wherever they sit, and the
/// `import()` types (`typeof import('m')`, `import('m').A`).
struct Calls<'s> {
    lines: &'s LineIndex,
    /// JavaScript, where `require` and `module.exports` make a module.
    javascript: bool,
    /// The function bodies the walk is in, each with whether it takes a
    /// parameter named `require`, as a bundle's module wrapper does.
    functions: Vec<bool>,
    imports: Vec<ImportStatement>,
    dynamic: Vec<DynamicCall>,
    /// `import.meta`, or in JavaScript `require` or `module.exports`.
    module_syntax: bool,
    has_jsx: bool,
}

impl Calls<'_> {
    /// A statement that takes `name`, as a type only when `type_only`.
    fn import(
        &mut self,
        specifier: String,
        start: u32,
        note: &'static str,
        name: String,
        type_only: bool,
    ) {
        let types = match type_only {
            true => BTreeSet::from([name.clone()]),
            false => BTreeSet::new(),
        };
        self.imports.push(ImportStatement {
            specifier,
            line: self.lines.line(start),
            note,
            names: vec![name],
            types,
            local: !self.functions.is_empty(),
        });
    }

    fn dynamic(&mut self, call: &'static str, start: u32) {
        self.dynamic.push(DynamicCall {
            call,
            line: self.lines.line(start),
            local: !self.functions.is_empty(),
        });
    }
}

impl<'a> Visit<'a> for Calls<'_> {
    fn enter_node(&mut self, kind: AstKind<'a>) {
        match kind {
            AstKind::Function(f) => self.functions.push(takes_require(&f.params)),
            AstKind::ArrowFunctionExpression(f) => self.functions.push(takes_require(&f.params)),
            AstKind::ImportMeta(_) => self.module_syntax = true,
            AstKind::JSXElement(_) | AstKind::JSXFragment(_) => self.has_jsx = true,
            _ => {}
        }
    }

    fn leave_node(&mut self, kind: AstKind<'a>) {
        if matches!(
            kind,
            AstKind::Function(_) | AstKind::ArrowFunctionExpression(_)
        ) {
            self.functions.pop();
        }
    }

    fn visit_call_expression(&mut self, it: &CallExpression<'a>) {
        let note = match &it.callee {
            // a `require` that a function takes is the caller's, not Node's
            Expression::Identifier(callee)
                if callee.name == "require" && !self.functions.contains(&true) =>
            {
                Some("require")
            }
            Expression::StaticMemberExpression(member) => match &member.object {
                Expression::Identifier(object) => MODULE_CALLS.into_iter().find(|call| {
                    call.split_once('.')
                        == Some((object.name.as_str(), member.property.name.as_str()))
                }),
                _ => None,
            },
            _ => None,
        };
        if note == Some("require") && self.javascript {
            self.module_syntax = true;
        }
        if let (Some(note), Some(first)) = (note, it.arguments.first()) {
            match first.as_expression().and_then(literal) {
                Some(specifier) => self.import(
                    specifier,
                    it.span.start,
                    note,
                    WHOLE_MODULE.to_owned(),
                    false,
                ),
                // a mock of a computed name loads nothing to point at
                None if note == "require" => self.dynamic(note, it.span.start),
                None => {}
            }
        }
        walk::walk_call_expression(self, it);
    }

    fn visit_assignment_expression(&mut self, it: &AssignmentExpression<'a>) {
        if self.javascript && commonjs_target(&it.left).is_some() {
            self.module_syntax = true;
        }
        walk::walk_assignment_expression(self, it);
    }

    fn visit_import_expression(&mut self, it: &ImportExpression<'a>) {
        match literal(&it.source) {
            Some(specifier) => self.import(
                specifier,
                it.span.start,
                "import()",
                WHOLE_MODULE.to_owned(),
                false,
            ),
            None => self.dynamic("import()", it.span.start),
        }
        walk::walk_import_expression(self, it);
    }

    fn visit_ts_import_type(&mut self, it: &TSImportType<'a>) {
        // `import('m').A.B` takes `A`; `typeof import('m')` the whole module
        let name = it
            .qualifier
            .as_ref()
            .map_or_else(|| WHOLE_MODULE.to_owned(), |q| first_segment(q).to_owned());
        self.import(
            it.source.value.to_string(),
            it.span.start,
            "import",
            name,
            true,
        );
        walk::walk_ts_import_type(self, it);
    }
}

fn takes_require(params: &FormalParameters) -> bool {
    params.items.iter().any(|p| {
        p.pattern
            .get_identifier_name()
            .is_some_and(|n| n == "require")
    })
}

/// A specifier written out: a string, or a template without substitutions.
fn literal(expression: &Expression) -> Option<String> {
    match expression {
        Expression::StringLiteral(s) => Some(s.value.to_string()),
        Expression::TemplateLiteral(t) => t.single_quasi().map(|q| q.to_string()),
        _ => None,
    }
}

fn first_segment<'a>(qualifier: &'a TSImportTypeQualifier<'a>) -> &'a str {
    match qualifier {
        TSImportTypeQualifier::Identifier(name) => name.name.as_str(),
        TSImportTypeQualifier::QualifiedName(name) => first_segment(&name.left),
    }
}

/// The module that `import x = require('m')` loads; `None` for an alias of
/// a namespace (`import x = NS.inner`).
fn required_by(declaration: &TSImportEqualsDeclaration) -> Option<String> {
    match &declaration.module_reference {
        TSModuleReference::ExternalModuleReference(r) => Some(r.expression.value.to_string()),
        _ => None,
    }
}

/// The text of one file, its line index and its comments.
struct Source<'a> {
    text: &'a str,
    lines: &'a LineIndex,
    /// Start and end offsets of every comment, in source order.
    comments: &'a [(u32, u32)],
}

impl Source<'_> {
    fn slice(&self, start: u32, end: u32) -> &str {
        self.text.get(start as usize..end as usize).unwrap_or("")
    }

    /// The text between two offsets with each comment in it replaced by a
    /// space: once whitespace is collapsed, a `//` comment would read as
    /// part of the code after it.
    fn code(&self, start: u32, end: u32) -> String {
        let first = self.comments.partition_point(|&(s, _)| s < start);
        let mut out = String::new();
        let mut at = start;
        for &(s, e) in &self.comments[first..] {
            if s >= end || e > end {
                break;
            }
            out.push_str(self.slice(at, s));
            out.push(' ');
            at = e;
        }
        out.push_str(self.slice(at, end));
        out
    }

    fn symbol(&self, name: String, kind: SymbolKind, start: u32, end: u32) -> ExportedSymbol {
        ExportedSymbol {
            name,
            kind,
            line: self.lines.line(start),
            signature: Some(signature(&self.code(start, end))),
        }
    }

    /// The symbols of `name` assigned `value` by a statement at `start`
    /// (`exports.pad = (text) => ..`): a local declaration's own, under
    /// `name` at its line, else one by the kind of the value.
    fn assigned(
        &self,
        name: &str,
        value: &Expression,
        start: u32,
        locals: &BTreeMap<String, Vec<ExportedSymbol>>,
    ) -> Vec<ExportedSymbol> {
        if let Expression::Identifier(id) = value {
            if let Some(symbols) = locals.get(id.name.as_str()) {
                return exported_as(symbols, name);
            }
        }
        let (kind, end) = value_kind(value);
        vec![self.symbol(name.to_owned(), kind, start, end)]
    }

    /// The symbols of a property of `module.exports = { .. }`; a value that
    /// is no function or class goes by its key alone.
    fn property(
        &self,
        name: &str,
        property: &ObjectProperty,
        locals: &BTreeMap<String, Vec<ExportedSymbol>>,
    ) -> Vec<ExportedSymbol> {
        let start = property.span.start;
        match value_kind(&property.value) {
            (SymbolKind::Constant, _) if !matches!(property.value, Expression::Identifier(_)) => {
                vec![self.symbol(
                    name.to_owned(),
                    SymbolKind::Constant,
                    start,
                    property.key.span().end,
                )]
            }
            _ => self.assigned(name, &property.value, start, locals),
        }
    }

    fn function(&self, f: &Function, start: u32) -> Option<ExportedSymbol> {
        let name = f.id.as_ref()?.name.to_string();
        let end = f.body.as_ref().map_or(f.span.end, |b| b.span.start);
        Some(self.symbol(name, SymbolKind::Function, start, end))
    }

    /// A named class and its public methods, as `Class.method`.
    fn class(&self, c: &Class, start: u32) -> Vec<ExportedSymbol> {
        let Some(id) = &c.id else {
            return Vec::new();
        };
        let name = id.name.to_string();
        let mut symbols =
            vec![self.symbol(name.clone(), SymbolKind::Struct, start, c.body.span.start)];
        for element in &c.body.body {
            let ClassElement::MethodDefinition(m) = element else {
                continue;
            };
            let hidden = matches!(
                m.accessibility,
                Some(TSAccessibility::Private | TSAccessibility::Protected)
            ) || m.key.is_private_identifier();
            if hidden || !matches!(m.kind, MethodDefinitionKind::Method) {
                continue;
            }
            let Some(method) = m.key.static_name() else {
                continue;
            };
            let end = m.value.body.as_ref().map_or(m.span.end, |b| b.span.start);
            symbols.push(self.symbol(
                format!("{name}.{method}"),
                SymbolKind::Function,
                m.span.start,
                end,
            ));
        }
        symbols
    }

    /// The symbols of a declaration that starts at `start` (its `export`,
    /// when it has one), one group per declared name: the declaration first,
    /// then a class's methods.
    fn declared(&self, declaration: &Declaration, start: u32) -> Vec<Vec<ExportedSymbol>> {
        match declaration {
            Declaration::FunctionDeclaration(f) => self
                .function(f, start)
                .into_iter()
                .map(|s| vec![s])
                .collect(),
            Declaration::ClassDeclaration(c) => {
                let symbols = self.class(c, start);
                if symbols.is_empty() {
                    Vec::new()
                } else {
                    vec![symbols]
                }
            }
            Declaration::VariableDeclaration(v) => {
                let prefix_end = v.declarations.first().map_or(start, |d| d.span.start);
                let prefix = self.code(start, prefix_end);
                let value_kind = if v.kind.is_const() {
                    SymbolKind::Constant
                } else {
                    SymbolKind::Other
                };
                v.declarations
                    .iter()
                    .filter_map(|d| {
                        let name = d.id.get_identifier_name()?.to_string();
                        let (kind, end) = match &d.init {
                            Some(Expression::ArrowFunctionExpression(a)) => {
                                (SymbolKind::Function, a.body.span().start)
                            }
                            Some(Expression::FunctionExpression(f)) => (
                                SymbolKind::Function,
                                f.body.as_ref().map_or(f.span.end, |b| b.span.start),
                            ),
                            Some(init) => (value_kind, init.span().start),
                            None => (value_kind, d.span.end),
                        };
                        let declarator = self.code(d.span.start, end);
                        Some(vec![ExportedSymbol {
                            name,
                            kind,
                            line: self.lines.line(d.span.start),
                            signature: Some(signature(&format!("{prefix}{declarator}"))),
                        }])
                    })
                    .collect()
            }
            Declaration::TSTypeAliasDeclaration(t) => vec![vec![self.symbol(
                t.id.name.to_string(),
                SymbolKind::TypeAlias,
                start,
                t.span.end,
            )]],
            Declaration::TSInterfaceDeclaration(i) => vec![vec![self.symbol(
                i.id.name.to_string(),
                SymbolKind::Trait,
                start,
                i.body.span.start,
            )]],
            Declaration::TSEnumDeclaration(e) => vec![vec![self.symbol(
                e.id.name.to_string(),
                SymbolKind::Enum,
                start,
                e.body.span.start,
            )]],
            Declaration::TSNamespaceDeclaration(n) => vec![vec![self.symbol(
                n.id.name.to_string(),
                SymbolKind::Module,
                start,
                n.body.span().start,
            )]],
            _ => Vec::new(),
        }
    }
}

/// The kind of a value assigned to an export, and where its signature
/// ends: before a function's body, else before the value.
fn value_kind(value: &Expression) -> (SymbolKind, u32) {
    match value {
        Expression::ArrowFunctionExpression(a) => (SymbolKind::Function, a.body.span().start),
        Expression::FunctionExpression(f) => (
            SymbolKind::Function,
            f.body.as_ref().map_or(f.span.end, |b| b.span.start),
        ),
        Expression::ClassExpression(c) => (SymbolKind::Struct, c.body.span.start),
        _ => (SymbolKind::Constant, value.span().start),
    }
}

/// `symbols` (a declaration and its methods) exported under `name`.
fn exported_as(symbols: &[ExportedSymbol], name: &str) -> Vec<ExportedSymbol> {
    let declared = symbols[0].name.clone();
    symbols
        .iter()
        .map(|s| {
            let mut s = s.clone();
            if let Some(rest) = s.name.strip_prefix(declared.as_str()) {
                s.name = format!("{name}{rest}");
            }
            s
        })
        .collect()
}

/// `raw` with whitespace collapsed, none just inside parentheses and
/// brackets and no comma before a closing one, without a trailing `{`, `;`
/// or `=`, cut to [`MAX_SIGNATURE`] characters.
fn signature(raw: &str) -> String {
    let mut s = raw
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace("( ", "(")
        .replace(" )", ")")
        .replace(",)", ")")
        .replace("[ ", "[")
        .replace(" ]", "]")
        .replace(",]", "]")
        .replace(", }", " }");
    while let Some(last) = s.chars().last() {
        let dangling = matches!(last, '{' | ';' | ' ') || (last == '=' && !s.ends_with("=>"));
        if !dangling {
            break;
        }
        s.pop();
    }
    if s.chars().count() > MAX_SIGNATURE {
        s = s.chars().take(MAX_SIGNATURE).collect::<String>() + "...";
    }
    s
}

/// 1-based line numbers of byte offsets.
struct LineIndex {
    starts: Vec<u32>,
}

impl LineIndex {
    fn new(text: &str) -> Self {
        let mut starts = vec![0];
        starts.extend(text.match_indices('\n').map(|(i, _)| i as u32 + 1));
        Self { starts }
    }

    fn line(&self, offset: u32) -> u32 {
        self.starts.partition_point(|&s| s <= offset) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(file: &ParsedFile) -> Vec<(&str, SymbolKind, u32)> {
        file.symbols
            .iter()
            .map(|s| (s.name.as_str(), s.kind, s.line))
            .collect()
    }

    fn signature_of<'a>(file: &'a ParsedFile, name: &str) -> &'a str {
        file.symbols
            .iter()
            .find(|s| s.name == name)
            .and_then(|s| s.signature.as_deref())
            .unwrap_or_else(|| panic!("no signature for {name}"))
    }

    #[test]
    fn import_and_export_from_statements_with_their_lines() {
        let file = parse(
            Path::new("x.ts"),
            "import a from 'a';\nimport {\n  b,\n} from './b';\nexport { c } from './c';\n\
             export * from './d';\nconst s = `import x from 'tpl'`;\n// import y from 'comment';\n\
             const q = \"import z from 'str'\";\n",
        )
        .unwrap();
        let found: Vec<(&str, u32, &str)> = file
            .imports
            .iter()
            .map(|i| (i.specifier.as_str(), i.line, i.note))
            .collect();
        assert_eq!(
            found,
            [
                ("a", 1, "import"),
                ("./b", 2, "import"),
                ("./c", 5, "export"),
                ("./d", 6, "export"),
            ]
        );
    }

    const SAMPLE: &str = r#"export const CURRENCY = 'JPY';
export function formatPrice(price: Money): string {
  return '';
}
export class Wallet {
  pay(amount: number): void {}
  private audit(): void {}
  protected check(): void {}
  #hidden(): void {}
  static open(): Wallet { return new Wallet(); }
  get balance(): number { return 0; }
  constructor() {}
}
export const read = () => 1, other: string = 'a';
export let counter = 0;
export type Format = "LP" | "CD";
export interface Priced { price: number }
export enum Unit { A }
export namespace NS { export const x = 1; }
const rates = { JPY: 1 };
function limitOf(): number { return 1; }
export { rates as RATES };
export default limitOf;
"#;

    #[test]
    fn exported_declarations_become_symbols() {
        let file = parse(Path::new("x.ts"), SAMPLE).unwrap();
        assert_eq!(
            kinds(&file),
            [
                ("CURRENCY", SymbolKind::Constant, 1),
                ("formatPrice", SymbolKind::Function, 2),
                ("Wallet", SymbolKind::Struct, 5),
                ("Wallet.pay", SymbolKind::Function, 6),
                ("Wallet.open", SymbolKind::Function, 10),
                ("read", SymbolKind::Function, 14),
                ("other", SymbolKind::Constant, 14),
                ("counter", SymbolKind::Other, 15),
                ("Format", SymbolKind::TypeAlias, 16),
                ("Priced", SymbolKind::Trait, 17),
                ("Unit", SymbolKind::Enum, 18),
                ("NS", SymbolKind::Module, 19),
                ("RATES", SymbolKind::Constant, 20),
                ("limitOf", SymbolKind::Function, 21),
            ]
        );
    }

    #[test]
    fn signatures_stop_before_the_body() {
        let file = parse(Path::new("x.ts"), SAMPLE).unwrap();
        for (name, signature) in [
            (
                "formatPrice",
                "export function formatPrice(price: Money): string",
            ),
            ("Wallet", "export class Wallet"),
            ("Wallet.open", "static open(): Wallet"),
            ("read", "export const read = () =>"),
            ("other", "export const other: string"),
            ("Format", r#"export type Format = "LP" | "CD""#),
            ("Priced", "export interface Priced"),
            ("RATES", "const rates"),
            ("limitOf", "function limitOf(): number"),
        ] {
            assert_eq!(signature_of(&file, name), signature, "{name}");
        }
    }

    #[test]
    fn import_equals_require_is_an_import() {
        let file = parse(
            Path::new("x.ts"),
            "import fs = require('fs');\nimport inner = NS.inner;\n\
             export import other = require('./other');\n",
        )
        .unwrap();
        let found: Vec<(&str, u32, &str)> = file
            .imports
            .iter()
            .map(|i| (i.specifier.as_str(), i.line, i.note))
            .collect();
        assert_eq!(found, [("fs", 1, "import"), ("./other", 3, "import")]);
    }

    #[test]
    fn a_name_declared_twice_keeps_its_first_declaration() {
        // a zod schema and its type, and overloads before the implementation
        let file = parse(
            Path::new("x.ts"),
            "export const User = z.object({});\nexport type User = z.infer<typeof User>;\n\
             export function f(a: string): void;\nexport function f(a: number): void;\n\
             export function f(a: unknown) {}\nconst Item = 1;\ntype Item = number;\n\
             export { Item };\n",
        )
        .unwrap();
        assert_eq!(
            kinds(&file),
            [
                ("User", SymbolKind::Constant, 1),
                ("f", SymbolKind::Function, 3),
                ("Item", SymbolKind::Constant, 6),
            ]
        );
        assert_eq!(
            signature_of(&file, "f"),
            "export function f(a: string): void"
        );
    }

    #[test]
    fn a_default_export_by_specifier_keeps_the_declared_name() {
        let file = parse(
            Path::new("x.ts"),
            "function limitOf(): number { return 1; }\nexport { limitOf as default, limitOf as cap };\n",
        )
        .unwrap();
        let names: Vec<&str> = file.symbols.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["limitOf", "cap"]);
    }

    #[test]
    fn jsx_text_does_not_hide_later_exports() {
        let file = parse(
            Path::new("x.tsx"),
            "export function A() { return <p>Don't stop</p>; }\nexport function B() {}\n",
        )
        .unwrap();
        let names: Vec<&str> = file.symbols.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["A", "B"]);
    }

    #[test]
    fn javascript_files_may_hold_jsx() {
        let file = parse(Path::new("x.js"), "export const A = () => <p>it's</p>;\n").unwrap();
        assert_eq!(kinds(&file), [("A", SymbolKind::Function, 1)]);
    }

    #[test]
    fn an_unparsable_file_is_an_error() {
        let err = parse(Path::new("x.ts"), "export const = ;").unwrap_err();
        assert!(err.contains("Unexpected token"), "{err}");
    }

    #[test]
    fn signatures_leave_out_comments() {
        let file = parse(
            Path::new("x.ts"),
            "export type Profile = {\n  avatar: string | null; // relative path\n  \
             /** shown in the header */\n  status: string;\n};\nexport function make(\n  \
             id: string, // the id\n  /* options */ opts?: { a: number },\n): void {}\n",
        )
        .unwrap();
        assert_eq!(
            signature_of(&file, "Profile"),
            "export type Profile = { avatar: string | null; status: string; }"
        );
        assert_eq!(
            signature_of(&file, "make"),
            "export function make(id: string, opts?: { a: number }): void"
        );
    }

    #[test]
    fn signatures_are_collapsed_and_capped() {
        assert_eq!(
            signature("export  function f(\n  a: string,\n): void {"),
            "export function f(a: string): void"
        );
        assert_eq!(signature("function g( a, b )"), "function g(a, b)");
        assert_eq!(
            signature("export function h({\n  status,\n  page = 1,\n}: Options) {"),
            "export function h({ status, page = 1 }: Options)"
        );
        assert_eq!(
            signature("export type Pair = [\n  string,\n  number,\n];"),
            "export type Pair = [string, number]"
        );
        let long = format!("export type T = {}", "'x' | ".repeat(60));
        assert!(signature(&long).ends_with("..."));
        assert_eq!(signature(&long).chars().count(), MAX_SIGNATURE + 3);
    }

    #[test]
    fn import_statements_name_the_exports_they_take() {
        let file = parse(
            Path::new("x.ts"),
            "import a, { b, c as d } from 'm';\nimport * as ns from 'n';\nimport 'side';\n\
             import e = require('e');\nexport { f } from 'f';\nexport * from 'g';\n\
             export * as h from 'h';\nexport { default as i, j as k } from 'i';\n\
             export import l = require('l');\n",
        )
        .unwrap();
        let names: Vec<(&str, Vec<&str>)> = file
            .imports
            .iter()
            .map(|i| {
                (
                    i.specifier.as_str(),
                    i.names.iter().map(String::as_str).collect(),
                )
            })
            .collect();
        assert_eq!(
            names,
            [
                ("m", vec!["default", "b", "c"]),
                ("n", vec!["*"]),
                ("side", vec![]),
                ("e", vec!["*"]),
                ("f", vec!["f"]),
                ("g", vec!["*"]),
                ("h", vec!["*"]),
                ("i", vec!["default", "j"]),
                ("l", vec!["*"]),
            ]
        );
    }

    #[test]
    fn a_default_export_is_named_by_its_declaration() {
        let default_name =
            |text: &str| parse(Path::new("x.ts"), text).unwrap().exports.default_name;
        assert_eq!(
            default_name("export default function limitOf() {}\n").as_deref(),
            Some("limitOf")
        );
        assert_eq!(
            default_name("export default class Cart {}\n").as_deref(),
            Some("Cart")
        );
        assert_eq!(
            default_name("export default interface Shape {}\n").as_deref(),
            Some("Shape")
        );
        assert_eq!(
            default_name("function f() {}\nexport { f as default };\n").as_deref(),
            Some("f")
        );
        // the first default export wins, as in the export table
        assert_eq!(
            default_name(
                "export default function a() {}\nexport { b as default };\nfunction b() {}\n"
            )
            .as_deref(),
            Some("a")
        );
        assert_eq!(default_name("export default function () {}\n"), None);
        assert_eq!(default_name("export default 42;\n"), None);
        // a re-exported default is declared elsewhere
        assert_eq!(
            default_name("import x from './x';\nexport default x;\n"),
            None
        );
        assert_eq!(default_name("export { default } from './x';\n"), None);
    }

    const EXPORTS: &str = "export const local = 1;
export function f() {}
const hidden = 2;
export { hidden as shown };
export { a as b } from './a';
export { default as d } from './d';
export * from './star';
export * as ns from './ns';
import { x } from './x';
export { x as y };
import def from './def';
export { def };
import * as whole from './whole';
export { whole, late };
import { late } from './late';
export default local;
";

    #[test]
    fn export_tables_say_where_each_name_comes_from() {
        let file = parse(Path::new("x.ts"), EXPORTS).unwrap();
        let reexport = |import, name: &str, line| Export::Reexport {
            import,
            name: name.to_owned(),
            line,
            type_only: false,
        };
        let names: BTreeMap<String, Export> = [
            ("local", Export::Local),
            ("f", Export::Local),
            ("shown", Export::Local),
            ("b", reexport(0, "a", 5)),
            ("d", reexport(1, "default", 6)),
            (
                "ns",
                Export::Namespace {
                    import: 3,
                    line: 8,
                    type_only: false,
                },
            ),
            ("y", reexport(4, "x", 10)),
            ("def", reexport(5, "default", 12)),
            (
                "whole",
                Export::Namespace {
                    import: 6,
                    line: 14,
                    type_only: false,
                },
            ),
            // imports are hoisted: `late` is imported after its export
            ("late", reexport(7, "late", 14)),
            ("default", Export::Local),
        ]
        .into_iter()
        .map(|(name, export)| (name.to_owned(), export))
        .collect();
        assert_eq!(file.exports.names, names);
        assert_eq!(file.exports.stars, [(2, 7, false)]);
        // re-exported bindings are no symbols of this file
        let symbols: Vec<&str> = file.symbols.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(symbols, ["local", "f", "shown"]);
        // `export default local` lends the export its declared name
        assert_eq!(file.exports.default_name.as_deref(), Some("local"));
    }

    #[test]
    fn statements_of_types_only_say_which_names_are_types() {
        let file = parse(
            Path::new("x.ts"),
            "import type a from 'm';\nimport type { b } from 'm2';\nimport { c, type d } from 'n';\n\
             import type * as ns from 'o';\nimport type e = require('e');\n\
             export type { f } from 'f';\nexport { g, type h } from 'g';\n\
             export type * from 'i';\nexport type * as j from 'j';\n\
             import type { k } from 'k';\nexport { k };\nimport { l } from 'l';\nexport type { l };\n",
        )
        .unwrap();
        let types: Vec<(&str, Vec<&str>)> = file
            .imports
            .iter()
            .map(|i| {
                (
                    i.specifier.as_str(),
                    i.types.iter().map(String::as_str).collect(),
                )
            })
            .collect();
        assert_eq!(
            types,
            [
                ("m", vec!["default"]),
                ("m2", vec!["b"]),
                ("n", vec!["d"]),
                ("o", vec!["*"]),
                ("e", vec!["*"]),
                ("f", vec!["f"]),
                ("g", vec!["h"]),
                ("i", vec!["*"]),
                ("j", vec!["*"]),
                ("k", vec!["k"]),
                ("l", vec![]),
            ]
        );
        let reexport = |import, name: &str, line, type_only| Export::Reexport {
            import,
            name: name.to_owned(),
            line,
            type_only,
        };
        let table = &file.exports.names;
        assert_eq!(table.get("f"), Some(&reexport(5, "f", 6, true)));
        assert_eq!(table.get("g"), Some(&reexport(6, "g", 7, false)));
        assert_eq!(table.get("h"), Some(&reexport(6, "h", 7, true)));
        assert_eq!(
            table.get("j"),
            Some(&Export::Namespace {
                import: 8,
                line: 9,
                type_only: true
            })
        );
        // a binding imported as a type, or exported as one, passes on a type
        assert_eq!(table.get("k"), Some(&reexport(9, "k", 11, true)));
        assert_eq!(table.get("l"), Some(&reexport(10, "l", 13, true)));
        assert_eq!(file.exports.stars, [(7, 8, true)]);
    }

    #[test]
    fn a_name_taken_as_a_value_and_as_a_type_is_a_value() {
        let file = parse(
            Path::new("x.ts"),
            "import { A, type A as B } from 'm';\nexport { c, type c as d } from 'n';\n",
        )
        .unwrap();
        for import in &file.imports {
            assert!(import.types.is_empty(), "{import:?}");
        }
    }

    #[test]
    fn calls_that_load_modules_are_imports() {
        let file = parse(
            Path::new("x.ts"),
            "const a = require('a');\n\
             function f(name: string) {\n\
             \x20 require(`./plugins/${name}`);\n\
             \x20 return import('b');\n\
             }\n\
             const g = () => import(`c`);\n\
             vi.mock('d', () => ({}));\n\
             jest.requireActual('e');\n\
             type T = typeof import('t');\n\
             let w: import('u').Wallet.Inner;\n\
             import(a);\n\
             require.resolve('r');\n\
             vi.mock(a);\n",
        )
        .unwrap();
        // specifier, line, note, local, names, types
        type Row<'a> = (&'a str, u32, &'a str, bool, Vec<&'a str>, Vec<&'a str>);
        let imports: Vec<Row> = file
            .imports
            .iter()
            .map(|i| {
                (
                    i.specifier.as_str(),
                    i.line,
                    i.note,
                    i.local,
                    i.names.iter().map(String::as_str).collect(),
                    i.types.iter().map(String::as_str).collect(),
                )
            })
            .collect();
        assert_eq!(
            imports,
            [
                ("a", 1, "require", false, vec!["*"], vec![]),
                ("b", 4, "import()", true, vec!["*"], vec![]),
                ("c", 6, "import()", true, vec!["*"], vec![]),
                ("d", 7, "vi.mock", false, vec!["*"], vec![]),
                ("e", 8, "jest.requireActual", false, vec!["*"], vec![]),
                // in a type position: erased
                ("t", 9, "import", false, vec!["*"], vec!["*"]),
                ("u", 10, "import", false, vec!["Wallet"], vec!["Wallet"]),
            ]
        );
        // computed specifiers; `require.resolve` and a computed mock load nothing
        let dynamic: Vec<(&str, u32, bool)> = file
            .dynamic
            .iter()
            .map(|d| (d.call, d.line, d.local))
            .collect();
        assert_eq!(dynamic, [("require", 3, true), ("import()", 11, false)]);
    }

    #[test]
    fn a_require_that_a_function_binds_itself_is_no_import() {
        // bundles wrap each module in a function that takes `require`
        let file = parse(
            Path::new("x.js"),
            "(function (require, module, exports) {\n\
             \x20 require('./inner');\n\
             \x20 const f = () => require('./deep');\n\
             \x20 require(name);\n\
             })();\n\
             define(['require'], function (require) { require('./amd'); });\n\
             require('./outer');\n",
        )
        .unwrap();
        let specifiers: Vec<&str> = file.imports.iter().map(|i| i.specifier.as_str()).collect();
        assert_eq!(specifiers, ["./outer"]);
        assert!(file.dynamic.is_empty(), "{:?}", file.dynamic);
    }

    #[test]
    fn a_file_without_imports_or_exports_has_no_module_syntax() {
        let syntax = |path: &str, text: &str| parse(Path::new(path), text).unwrap().module_syntax;
        assert!(!syntax(
            "x.ts",
            "declare const VERSION: string;\ninterface Window { shop: string }\nfunction track() {}\n"
        ));
        // an alias of a namespace, a wrapper's own `require` and `import()`
        assert!(!syntax(
            "x.ts",
            "namespace NS { export const y = 1; }\nimport x = NS.y;\n\
             (function (require) { require('a'); })();\nconst p = import('b');\n"
        ));
        // CommonJS makes only JavaScript a module, as TypeScript reads it
        assert!(!syntax(
            "x.ts",
            "const fs = require('fs');\nmodule.exports = fs;\n"
        ));
        for (path, text) in [
            ("x.ts", "import 'a';"),
            ("x.ts", "export {};"),
            ("x.ts", "export const a = 1;"),
            ("x.ts", "export default 1;"),
            ("x.ts", "export * from 'a';"),
            ("x.ts", "import x = require('a');"),
            ("x.ts", "export = 1;"),
            ("x.d.ts", "export as namespace Lib;"),
            ("x.ts", "const u = import.meta.url;"),
            ("x.js", "function f() { return require('a'); }"),
            ("x.js", "module.exports = {};"),
            ("x.js", "exports.a = 1;"),
            ("x.js", "if (x) { module.exports.a = 1; }"),
        ] {
            assert!(syntax(path, text), "{text}");
        }
    }

    #[test]
    fn jsx_is_noticed() {
        let jsx = |text: &str| parse(Path::new("x.tsx"), text).unwrap().has_jsx;
        assert!(jsx("const a = <div />;"));
        assert!(jsx("function f() { return <></>; }"));
        assert!(!jsx("const a = 1 < 2;"));
    }

    #[test]
    fn every_top_level_declaration_is_a_global() {
        let file = parse(
            Path::new("global.d.ts"),
            "declare const VERSION: string;\ninterface Window {\n  shop: { version: string };\n}\n\
             declare function track(event: string): void;\n\
             declare module '*.svg' {\n  const src: string;\n  export default src;\n}\n",
        )
        .unwrap();
        let globals: Vec<(&str, SymbolKind, u32)> = file
            .globals
            .iter()
            .map(|s| (s.name.as_str(), s.kind, s.line))
            .collect();
        assert_eq!(
            globals,
            [
                ("VERSION", SymbolKind::Constant, 1),
                ("Window", SymbolKind::Trait, 2),
                ("track", SymbolKind::Function, 5),
            ]
        );
        assert!(file.symbols.is_empty());
    }

    #[test]
    fn commonjs_exports_are_symbols() {
        let symbols = |path: &str, text: &str| -> Vec<(String, SymbolKind, u32, String)> {
            parse(Path::new(path), text)
                .unwrap()
                .symbols
                .into_iter()
                .map(|s| (s.name, s.kind, s.line, s.signature.unwrap_or_default()))
                .collect()
        };
        let row = |name: &str, kind, line, signature: &str| {
            (name.to_owned(), kind, line, signature.to_owned())
        };
        assert_eq!(
            symbols(
                "x.cjs",
                "function helper(a) {\n  return a;\n}\nexports.pad = (text) => text;\n\
                 module.exports.limit = 10;\nexports.help = helper;\n",
            ),
            [
                row("pad", SymbolKind::Function, 4, "exports.pad = (text) =>"),
                row("limit", SymbolKind::Constant, 5, "module.exports.limit"),
                // a local declaration, under the exported name at its line
                row("help", SymbolKind::Function, 1, "function helper(a)"),
            ]
        );
        assert_eq!(
            symbols(
                "y.js",
                "function helper(a) {\n  return a;\n}\nclass Store {\n  get() {}\n}\n\
                 module.exports = {\n  helper,\n  Store,\n  size: 3,\n  run() {},\n  go: function () {},\n};\n",
            ),
            [
                row("helper", SymbolKind::Function, 1, "function helper(a)"),
                row("Store", SymbolKind::Struct, 4, "class Store"),
                row("Store.get", SymbolKind::Function, 5, "get()"),
                row("size", SymbolKind::Constant, 10, "size"),
                row("run", SymbolKind::Function, 11, "run()"),
                row("go", SymbolKind::Function, 12, "go: function ()"),
            ]
        );
        // the module itself: its declared name, as a default export
        let file = parse(
            Path::new("z.js"),
            "module.exports = function limitOf(n) {\n  return n;\n};\n",
        )
        .unwrap();
        assert_eq!(file.symbols[0].name, "limitOf");
        assert_eq!(file.exports.default_name.as_deref(), Some("limitOf"));
        // `exports.default` is the default export, under its declared name
        let file = parse(
            Path::new("d.js"),
            "function helper() {}\nexports.default = helper;\n",
        )
        .unwrap();
        assert_eq!(file.symbols[0].name, "helper");
        assert_eq!(file.exports.default_name.as_deref(), Some("helper"));
        // the placeholders compilers write before the real assignments
        let file = parse(
            Path::new("v.js"),
            "exports.a = exports.b = void 0;\nexports.a = () => 1;\n",
        )
        .unwrap();
        let placed: Vec<(&str, u32)> = file
            .symbols
            .iter()
            .map(|s| (s.name.as_str(), s.line))
            .collect();
        assert_eq!(placed, [("a", 2)]);
        // names enter the export table, so `export *` of the file finds them
        let file = parse(Path::new("w.js"), "exports.a = 1;\nmodule.exports.b = 2;\n").unwrap();
        for name in ["a", "b"] {
            assert_eq!(file.exports.names.get(name), Some(&Export::Local), "{name}");
        }
    }

    #[test]
    fn an_exported_import_equals_is_a_namespace() {
        let file = parse(Path::new("x.ts"), "export import fs = require('fs');\n").unwrap();
        assert_eq!(
            file.exports.names.get("fs"),
            Some(&Export::Namespace {
                import: 0,
                line: 1,
                type_only: false
            })
        );
    }

    #[test]
    fn destructured_exports_are_in_the_table() {
        // Redux Toolkit and Auth.js export this way; a missing entry lets a
        // star elsewhere answer for the name.
        let file = parse(
            Path::new("x.ts"),
            "export const { auth, signIn: login, ...rest } = make();\n\
             export const [first, , third] = list;\n",
        )
        .unwrap();
        for name in ["auth", "login", "rest", "first", "third"] {
            assert_eq!(file.exports.names.get(name), Some(&Export::Local), "{name}");
        }
    }
}
