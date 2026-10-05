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
    Argument, ArrowFunctionBody, AssignmentExpression, AssignmentPattern, AssignmentTarget,
    BinaryOperator, BindingPattern, CallExpression, Class, ClassElement, Declaration, Decorator,
    ExportDefaultDeclarationKind, ExportNamedDeclaration, ExportSpecifier, Expression,
    FormalParameter, FormalParameters, Function, IdentifierReference, ImportDeclarationSpecifier,
    ImportExpression, JSXFragment, JSXOpeningElement, MethodDefinitionKind, NewExpression,
    ObjectExpression, ObjectProperty, ObjectPropertyKind, Statement, StaticMemberExpression,
    TSAccessibility, TSClassImplements, TSImportEqualsDeclaration, TSImportType,
    TSImportTypeQualifier, TSInterfaceDeclaration, TSMethodSignature, TSModuleReference,
    TSPropertySignature, TSType, VariableDeclarator,
};
use oxc_ast::AstKind;
use oxc_ast_visit::{walk, Visit};
use oxc_parser::Parser;
use oxc_span::{GetSpan, SourceType, Span};

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
    /// A mock outside any function (`vi.mock`, `jest.mock`, which run before
    /// the file's imports) whose factory never loads the real module: the
    /// file gets the factory's stand-in for it.
    pub replaces: bool,
    /// The names among `names` whose bindings the file names, and only
    /// where TypeScript reads a type (see [`Positions`]): a compiler drops
    /// them as it drops what `type` marks, unless the tsconfig keeps values;
    /// each with the local names that bind it.
    pub type_uses: BTreeMap<String, BTreeSet<String>>,
    /// `type` marks the whole statement (`import type`, `export type ..
    /// from`, an `import()` type), which every compiler erases; one whose
    /// names `type` marks one by one still loads its module where the
    /// tsconfig keeps loads.
    pub type_statement: bool,
}

/// A call that loads a module by a name computed at runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DynamicCall {
    /// `require` or `import()`.
    pub call: &'static str,
    pub line: u32,
    pub local: bool,
    /// The static start of the specifier it computes (`./pages/`).
    pub prefix: Option<String>,
}

/// Calls that load a module named by their first argument; `vi.mock` and
/// `jest.mock` with a factory never load the real one, which their note
/// keeps apart.
const MODULE_CALLS: [&str; 12] = [
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
    "jest.createMockFromModule",
    "jest.genMockFromModule",
];

/// Whether a statement's note is a test runner's call that puts a mock in
/// place of its module (`vi.mock`, `jest.requireMock`), as the scan notes
/// such calls; not one that loads or keeps the real module.
pub fn is_mock_call(note: &str) -> bool {
    matches!(
        note,
        "vi.mock"
            | "vi.doMock"
            | "vi.importMock"
            | "jest.mock"
            | "jest.doMock"
            | "jest.requireMock"
            | "jest.createMockFromModule"
            | "jest.genMockFromModule"
    )
}

/// Module calls through which a file runs a module's real code, or keeps
/// it, whatever a mock of the module in the file replaces.
pub(crate) const LOADS_REAL: [&str; 10] = [
    "vi.doMock",
    "vi.unmock",
    "vi.importActual",
    "vi.importMock",
    "jest.doMock",
    "jest.unmock",
    "jest.requireActual",
    "jest.requireMock",
    "jest.createMockFromModule",
    "jest.genMockFromModule",
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
    /// The declarations directly inside a top-level `declare global { .. }`,
    /// which a module adds to the global scope, the first of a name and none
    /// that the file also exports.
    pub declared_global: Vec<ExportedSymbol>,
    /// The file's React directive in its prologue, `use client` or `use
    /// server`, with its line: where the file runs, or that it exports
    /// server functions.
    pub directive: Option<(&'static str, u32)>,
}

/// The directives React reads in a file's prologue.
const DIRECTIVES: [&str; 2] = ["use client", "use server"];

/// Characters of a signature kept; a longer one ends in `...`.
const MAX_SIGNATURE: usize = 200;

/// How the parser reads the file at `path`: its extension decides
/// TypeScript or JavaScript, JSX and `.d.ts`.
pub(crate) fn source_type(path: &Path) -> Result<SourceType, String> {
    let source_type = SourceType::from_path(path).map_err(|e| e.to_string())?;
    // Plain `.js` files carry JSX as often as `.jsx` files do.
    Ok(match source_type.is_javascript() {
        true => source_type.with_jsx(true),
        false => source_type,
    })
}

/// Parse `text`, the file at `path`; its extension decides TypeScript or
/// JavaScript, JSX and `.d.ts`. Fails only when the parser gives up.
pub(crate) fn parse(path: &Path, text: &str) -> Result<ParsedFile, String> {
    let source_type = source_type(path)?;
    let javascript = source_type.is_javascript();
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
    let mut hidden = Hidden::default();
    hidden.visit_program(&parsed.program);
    hidden.values.sort_unstable();
    hidden.decorators.sort_unstable();
    let source = Source {
        text,
        lines: &lines,
        comments: &comments,
        values: &hidden.values,
        decorators: &hidden.decorators,
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

    let mut file = ParsedFile {
        directive: parsed.program.directives.iter().find_map(|d| {
            let value = d.expression.value.as_str();
            let known = DIRECTIVES.iter().find(|known| **known == value)?;
            Some((*known, lines.line(d.span.start)))
        }),
        ..ParsedFile::default()
    };
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
                    types: BTreeSet<String>,
                    type_statement: bool| ImportStatement {
            specifier,
            line,
            note,
            names,
            types,
            local: false,
            replaces: false,
            type_uses: BTreeMap::new(),
            type_statement,
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
                let whole_type = d.import_kind.is_type();
                file.imports.push(load(
                    d.source.value.to_string(),
                    "import",
                    names,
                    types,
                    whole_type,
                ));
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
                let whole_type = d.export_kind.is_type();
                file.imports.push(load(
                    d.source.value.to_string(),
                    "export",
                    names,
                    types,
                    whole_type,
                ));
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
                    type_only,
                ));
            }
            Statement::TSImportEqualsDeclaration(d) => {
                if let Some(specifier) = required_by(d) {
                    file.module_syntax = true;
                    let type_only = d.import_kind.is_type();
                    bindings.insert(d.id.name.to_string(), (index, None, type_only));
                    let types = whole_if(type_only);
                    file.imports
                        .push(load(specifier, "import", whole(), types, type_only));
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
                        let types = whole_if(type_only);
                        file.imports
                            .push(load(specifier, "import", whole(), types, type_only));
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
            // `export = Engine`: the module is the declaration, which an
            // import takes as its default
            Statement::TSExportAssignment(a) => {
                if let Expression::Identifier(id) = &a.expression {
                    let local = id.name.to_string();
                    file.symbols
                        .extend(locals.get(&local).cloned().unwrap_or_default());
                    exported.push((local, "default".to_owned(), line, false));
                }
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
            // a declaration exported under another name; a default keeps
            // its declared name apart
            None if !local.is_empty() && local != name && name != "default" => Export::Alias {
                local: local.clone(),
                line,
            },
            None => Export::Local,
        };
        if let Entry::Vacant(slot) = file.exports.names.entry(name) {
            // the first default export lends its declared name
            if slot.key() == "default" && export == Export::Local && !local.is_empty() {
                file.exports.default_name = Some(local);
            }
            let own = matches!(export, Export::Local | Export::Alias { .. });
            if own && export_type {
                file.exports.type_exports.insert(slot.key().clone());
            }
            slot.insert(export);
        }
    }
    let mut seen = BTreeSet::new();
    file.symbols.retain(|s| seen.insert(s.name.clone()));
    file.exports.types = declared_types(&parsed.program.body);
    // the bindings the file names only where a type is read; JavaScript
    // has no types, and a declaration file emits nothing whatever it names
    let mut positions = Positions {
        wanted: bindings
            .iter()
            .filter(|(_, (_, _, type_only))| !type_only)
            .map(|(local, _)| local.clone())
            .collect(),
        ..Positions::default()
    };
    let typescript = source_type.is_typescript() && !source_type.is_typescript_definition();
    if typescript && !positions.wanted.is_empty() {
        positions.visit_program(&parsed.program);
        // classic JSX calls its factory, which no identifier names: React,
        // or what a `@jsx` / `@jsxFrag` pragma says
        if positions.jsx {
            let factories = comments
                .iter()
                .flat_map(|&(start, end)| jsx_pragmas(&text[start as usize..end as usize]))
                .chain(["React".to_owned()]);
            positions.values.extend(factories);
        }
    }
    let mut kept: BTreeSet<(usize, String)> = BTreeSet::new();
    let mut typed: BTreeMap<(usize, String), BTreeSet<String>> = BTreeMap::new();
    for (local, (index, taken, type_only)) in &bindings {
        let name = taken.clone().unwrap_or_else(|| WHOLE_MODULE.to_owned());
        let only_types = positions.types.contains(local) && !positions.values.contains(local);
        match (*type_only, only_types) {
            (true, _) => {}
            (false, true) => {
                typed
                    .entry((*index, name))
                    .or_default()
                    .insert(local.clone());
            }
            (false, false) => {
                kept.insert((*index, name));
            }
        }
    }
    for ((index, name), locals) in typed {
        if kept.contains(&(index, name.clone())) {
            continue;
        }
        if let Some(import) = file.imports.get_mut(index) {
            import.type_uses.insert(name, locals);
        }
    }
    // after the statements, so the indices in the export table stay valid
    let mut calls = Calls {
        lines: &lines,
        javascript,
        functions: Vec::new(),
        imports: Vec::new(),
        dynamic: Vec::new(),
        taken: BTreeMap::new(),
        module_syntax: false,
        has_jsx: false,
    };
    calls.visit_program(&parsed.program);
    file.imports.extend(calls.imports);
    file.dynamic = calls.dynamic;
    file.module_syntax |= calls.module_syntax;
    file.has_jsx = calls.has_jsx;
    if !file.module_syntax {
        for statement in &parsed.program.body {
            if let Some(declaration) = statement.as_declaration() {
                for symbols in source.declared(declaration, statement.span().start) {
                    file.globals.extend(symbols);
                }
            }
        }
        // a name declared twice (overloads, a method's included) is its
        // first declaration, as in a module
        let mut seen = BTreeSet::new();
        file.globals.retain(|s| seen.insert(s.name.clone()));
    }
    // what `declare global` adds to the global scope; a name the file also
    // exports keeps the export
    for statement in &parsed.program.body {
        let Some(Declaration::TSGlobalDeclaration(global)) = statement.as_declaration() else {
            continue;
        };
        for inner in &global.body.body {
            // `export` is allowed there and changes nothing, so the
            // signature leaves it out
            let declaration = match inner {
                Statement::ExportDeclaration(export) => Some(&export.declaration),
                _ => inner.as_declaration(),
            };
            for symbols in declaration
                .map(|d| source.declared(d, d.span().start))
                .unwrap_or_default()
            {
                file.declared_global.extend(symbols);
            }
        }
    }
    let mut seen: BTreeSet<String> = file.symbols.iter().map(|s| s.name.clone()).collect();
    file.declared_global.retain(|s| seen.insert(s.name.clone()));
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
    /// The names that a `require` or `import()` call, by its start, takes
    /// at once: those its result is destructured into (`const { a } =
    /// require('m')`) or the property read from it (`require('m').a`).
    taken: BTreeMap<u32, Vec<String>>,
    /// `import.meta`, or in JavaScript `require` or `module.exports`.
    module_syntax: bool,
    has_jsx: bool,
}

/// The start of the `require` or `import()` call whose module
/// `expression` is, through parentheses: `require('m')`, or an
/// `import('m')` awaited. An `import()` not awaited is a promise, whose
/// `then` is no name of the module.
fn loading_call(expression: &Expression) -> Option<u32> {
    match expression {
        Expression::ParenthesizedExpression(p) => loading_call(&p.expression),
        Expression::AwaitExpression(a) => match a.argument.without_parentheses() {
            Expression::ImportExpression(i) => Some(i.span.start),
            other => loading_call(other),
        },
        Expression::CallExpression(c) if matches!(&c.callee, Expression::Identifier(callee) if callee.name == "require") => {
            Some(c.span.start)
        }
        _ => None,
    }
}

/// The keys an object pattern destructures, when every one is written out
/// and nothing collects the rest.
fn destructured(pattern: &BindingPattern) -> Option<Vec<String>> {
    let BindingPattern::ObjectPattern(object) = pattern else {
        return None;
    };
    if object.rest.is_some() {
        return None;
    }
    object
        .properties
        .iter()
        .map(|p| p.key.static_name().map(|name| name.into_owned()))
        .collect()
}

impl Calls<'_> {
    /// The names the loading call at `start` takes: those read at once, or
    /// the whole module.
    fn names_of(&mut self, start: u32) -> Vec<String> {
        self.taken
            .remove(&start)
            .unwrap_or_else(|| vec![WHOLE_MODULE.to_owned()])
    }

    /// A statement that takes `name`, as a type only when `type_only`.
    /// Calls on one line that load one module the same way are one
    /// statement, with the names of all (`import('./m').A | import('./m').B`).
    fn import(
        &mut self,
        specifier: String,
        start: u32,
        note: &'static str,
        name: String,
        type_only: bool,
    ) {
        let line = self.lines.line(start);
        let local = !self.functions.is_empty();
        let same = self
            .imports
            .iter_mut()
            .rev()
            .take_while(|i| i.line == line)
            .find(|i| i.specifier == specifier && i.note == note && i.local == local);
        if let Some(same) = same {
            // a value wins over a type of the same name
            if !same.names.contains(&name) {
                same.names.push(name.clone());
                if type_only {
                    same.types.insert(name);
                }
            } else if !type_only {
                same.types.remove(&name);
            }
            return;
        }
        let types = match type_only {
            true => BTreeSet::from([name.clone()]),
            false => BTreeSet::new(),
        };
        self.imports.push(ImportStatement {
            specifier,
            line,
            note,
            names: vec![name],
            types,
            local,
            replaces: false,
            type_uses: BTreeMap::new(),
            type_statement: type_only,
        });
    }

    fn dynamic(&mut self, call: &'static str, start: u32, specifier: Option<&Expression>) {
        self.dynamic.push(DynamicCall {
            call,
            line: self.lines.line(start),
            local: !self.functions.is_empty(),
            prefix: specifier.and_then(computed_prefix),
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
                Some(specifier) => {
                    // a mock that runs before the imports and stands in for
                    // the whole module, which takes the names it gives it
                    let hoisted =
                        matches!(note, "vi.mock" | "jest.mock") && self.functions.is_empty();
                    let replaces = hoisted && stands_in(it);
                    let names = match note {
                        "require" => self.names_of(it.span.start),
                        _ if replaces => {
                            factory_keys(it).unwrap_or_else(|| vec![WHOLE_MODULE.to_owned()])
                        }
                        _ => vec![WHOLE_MODULE.to_owned()],
                    };
                    let before = self.imports.len();
                    for name in names.iter().cloned() {
                        self.import(specifier.clone(), it.span.start, note, name, false);
                    }
                    if names.is_empty() {
                        // a factory that gives the module no name
                        self.import(specifier.clone(), it.span.start, note, String::new(), false);
                        if let Some(mock) = self.imports.last_mut() {
                            mock.names.clear();
                        }
                    }
                    if replaces && self.imports.len() > before {
                        if let Some(mock) = self.imports.last_mut() {
                            mock.replaces = true;
                        }
                    }
                }
                // a mock of a computed name loads nothing to point at
                None if note == "require" => {
                    self.dynamic(note, it.span.start, first.as_expression())
                }
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
            Some(specifier) => {
                for name in self.names_of(it.span.start) {
                    self.import(specifier.clone(), it.span.start, "import()", name, false);
                }
            }
            None => self.dynamic("import()", it.span.start, Some(&it.source)),
        }
        walk::walk_import_expression(self, it);
    }

    fn visit_variable_declarator(&mut self, it: &VariableDeclarator<'a>) {
        if let (Some(start), Some(names)) = (
            it.init.as_ref().and_then(loading_call),
            destructured(&it.id),
        ) {
            self.taken.insert(start, names);
        }
        walk::walk_variable_declarator(self, it);
    }

    fn visit_static_member_expression(&mut self, it: &StaticMemberExpression<'a>) {
        if let Some(start) = loading_call(&it.object) {
            self.taken
                .entry(start)
                .or_insert_with(|| vec![it.property.name.to_string()]);
        }
        walk::walk_static_member_expression(self, it);
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

/// Whether a mock call's factory, its second argument, stands in for the
/// module without loading the real one: a function written in place that
/// never names its first parameter (`importOriginal`), loads no module and
/// calls nothing bound outside it but Vitest's and Jest's own methods and
/// the language's globals, since such a function may load the real module.
fn stands_in(call: &CallExpression) -> bool {
    let Some(factory) = call.arguments.get(1).and_then(|a| a.as_expression()) else {
        return false;
    };
    let params = match factory.without_parentheses() {
        Expression::ArrowFunctionExpression(f) => &f.params,
        Expression::FunctionExpression(f) => &f.params,
        _ => return false,
    };
    let mut bound = Bindings::default();
    bound.visit_expression(factory);
    let mut original = LoadsOriginal {
        parameter: params
            .items
            .first()
            .and_then(|p| p.pattern.get_identifier_name())
            .map(|name| name.to_string()),
        own: bound.0,
        found: false,
    };
    original.visit_expression(factory);
    !original.found
}

/// The names a mock's factory gives the module, when it returns an object
/// written out (`() => ({ placeOrder: vi.fn() })`, or such an object after
/// `return`); none for a spread, a computed key or any other value.
fn factory_keys(call: &CallExpression) -> Option<Vec<String>> {
    let (keys, all) = written_keys(factory_object(call)?);
    all.then(|| keys.into_iter().map(|(key, _)| key).collect())
}

/// The object a mock's factory returns when it is written out: an arrow
/// function's value, or the value of the one `return` at the top of the
/// factory's body.
pub(super) fn factory_object<'b, 'a>(
    call: &'b CallExpression<'a>,
) -> Option<&'b ObjectExpression<'a>> {
    let factory = call.arguments.get(1)?.as_expression()?;
    let value = match factory.without_parentheses() {
        Expression::ArrowFunctionExpression(f) => match &f.body {
            ArrowFunctionBody::FunctionBody(body) => returned(&body.statements)?,
            value => value.as_expression()?,
        },
        Expression::FunctionExpression(f) => returned(&f.body.as_ref()?.statements)?,
        _ => return None,
    };
    match value.without_parentheses() {
        Expression::ObjectExpression(object) => Some(object),
        _ => None,
    }
}

/// The value of the one `return` with a value at the top of a body.
fn returned<'b, 'a>(statements: &'b [Statement<'a>]) -> Option<&'b Expression<'a>> {
    let mut values = statements.iter().filter_map(|s| match s {
        Statement::ReturnStatement(r) => r.argument.as_ref(),
        _ => None,
    });
    match (values.next(), values.next()) {
        (Some(value), None) => Some(value),
        _ => None,
    }
}

/// The keys an object writes out, apart from `__esModule`, with their
/// places, and whether they are all of its keys: nothing is spread into it
/// and no key is computed.
pub(super) fn written_keys(object: &ObjectExpression) -> (Vec<(String, Span)>, bool) {
    let mut keys = Vec::new();
    let mut all = true;
    for property in &object.properties {
        match property {
            ObjectPropertyKind::ObjectProperty(p) if !p.computed => match p.key.static_name() {
                Some(key) if key == "__esModule" => {}
                Some(key) => keys.push((key.into_owned(), p.key.span())),
                None => all = false,
            },
            _ => all = false,
        }
    }
    (keys, all)
}

/// Calls that load a module's real code, or build a mock from it.
const LOADS_ORIGINAL: [&str; 6] = [
    "vi.importActual",
    "vi.importMock",
    "jest.requireActual",
    "jest.requireMock",
    "jest.createMockFromModule",
    "jest.genMockFromModule",
];

/// What a mock's factory may call without loading a module: the test
/// frameworks' objects and the language's and runtime's globals.
const CALLABLE: [&str; 27] = [
    "vi",
    "jest",
    "expect",
    "Array",
    "BigInt",
    "Boolean",
    "Buffer",
    "Date",
    "Error",
    "Headers",
    "JSON",
    "Map",
    "Math",
    "Number",
    "Object",
    "Promise",
    "Reflect",
    "RegExp",
    "Request",
    "Response",
    "Set",
    "String",
    "Symbol",
    "TypeError",
    "URL",
    "WeakMap",
    "console",
];

/// The names a factory binds itself: its parameters and declarations.
#[derive(Default)]
struct Bindings(BTreeSet<String>);

impl<'a> Visit<'a> for Bindings {
    fn visit_binding_identifier(&mut self, it: &oxc_ast::ast::BindingIdentifier<'a>) {
        self.0.insert(it.name.to_string());
    }
}

/// Finds, in a mock's factory, what may load the real module: its
/// `importOriginal` parameter named, a module loaded, or a call of
/// something bound outside it.
struct LoadsOriginal {
    parameter: Option<String>,
    own: BTreeSet<String>,
    found: bool,
}

impl LoadsOriginal {
    /// A call of `callee`, through members and calls (`vi.fn().mockReturnValue`).
    fn calls(&mut self, callee: &Expression) {
        if let Some(name) = callee_root(callee) {
            let safe = CALLABLE.contains(&name) || self.own.contains(name);
            self.found |= !safe || self.parameter.as_deref() == Some(name);
        }
    }
}

impl<'a> Visit<'a> for LoadsOriginal {
    fn visit_identifier_reference(&mut self, it: &IdentifierReference<'a>) {
        self.found |= self.parameter.as_deref() == Some(it.name.as_str());
    }

    fn visit_static_member_expression(&mut self, it: &StaticMemberExpression<'a>) {
        if let Expression::Identifier(object) = &it.object {
            let call = format!("{}.{}", object.name, it.property.name);
            self.found |= LOADS_ORIGINAL.contains(&call.as_str());
        }
        walk::walk_static_member_expression(self, it);
    }

    fn visit_call_expression(&mut self, it: &CallExpression<'a>) {
        self.calls(&it.callee);
        walk::walk_call_expression(self, it);
    }

    fn visit_new_expression(&mut self, it: &NewExpression<'a>) {
        self.calls(&it.callee);
        walk::walk_new_expression(self, it);
    }

    fn visit_import_expression(&mut self, it: &ImportExpression<'a>) {
        self.found = true;
        walk::walk_import_expression(self, it);
    }
}

/// The name a callee starts from: `vi` for `vi.fn().mockReturnValue`,
/// `make` for `make()`; none for a function written in place.
fn callee_root<'e>(callee: &'e Expression) -> Option<&'e str> {
    match callee.without_parentheses() {
        Expression::Identifier(name) => Some(name.name.as_str()),
        Expression::StaticMemberExpression(member) => callee_root(&member.object),
        Expression::ComputedMemberExpression(member) => callee_root(&member.object),
        Expression::CallExpression(call) => callee_root(&call.callee),
        _ => None,
    }
}

fn takes_require(params: &FormalParameters) -> bool {
    params.items.iter().any(|p| {
        p.pattern
            .get_identifier_name()
            .is_some_and(|n| n == "require")
    })
}

/// The spans that lie between `start` and `end`, of spans in source order.
fn within(spans: &[(u32, u32)], start: u32, end: u32) -> &[(u32, u32)] {
    let first = spans.partition_point(|&(s, _)| s < start);
    let inside = spans[first..]
        .iter()
        .take_while(|&&(s, e)| s < end && e <= end)
        .count();
    &spans[first..first + inside]
}

/// What a signature leaves out, since it can hold a secret as a constant
/// can: the values a declaration is written with (parameter defaults,
/// destructured ones included, and the arguments of calls in a class's
/// `extends`) and its decorators, with what they are called with.
#[derive(Default)]
struct Hidden {
    values: Vec<(u32, u32)>,
    decorators: Vec<(u32, u32)>,
}

impl<'a> Visit<'a> for Hidden {
    fn visit_formal_parameter(&mut self, it: &FormalParameter<'a>) {
        if let Some(value) = &it.initializer {
            self.values.push((value.span().start, value.span().end));
        }
        walk::walk_formal_parameter(self, it);
    }

    fn visit_assignment_pattern(&mut self, it: &AssignmentPattern<'a>) {
        self.values
            .push((it.right.span().start, it.right.span().end));
        walk::walk_assignment_pattern(self, it);
    }

    fn visit_decorator(&mut self, it: &Decorator<'a>) {
        self.decorators.push((it.span.start, it.span.end));
    }

    fn visit_class(&mut self, it: &Class<'a>) {
        if let Some(heritage) = &it.heritage {
            let mut arguments = Arguments(Vec::new());
            arguments.visit_expression(&heritage.expression);
            self.values.extend(arguments.0);
        }
        walk::walk_class(self, it);
    }
}

/// The arguments of every call in an expression (`mixin(Base('x'))('y')`
/// reads `mixin(…)(…)`).
struct Arguments(Vec<(u32, u32)>);

impl Arguments {
    fn push(&mut self, arguments: &[Argument]) {
        if let (Some(first), Some(last)) = (arguments.first(), arguments.last()) {
            self.0.push((first.span().start, last.span().end));
        }
    }
}

impl<'a> Visit<'a> for Arguments {
    fn visit_call_expression(&mut self, it: &CallExpression<'a>) {
        self.push(&it.arguments);
        self.visit_expression(&it.callee);
    }

    fn visit_new_expression(&mut self, it: &NewExpression<'a>) {
        self.push(&it.arguments);
        self.visit_expression(&it.callee);
    }
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
    /// Those of every value a signature hides (see [`Hidden`]).
    values: &'a [(u32, u32)],
    /// Those of every decorator.
    decorators: &'a [(u32, u32)],
}

impl Source<'_> {
    fn slice(&self, start: u32, end: u32) -> &str {
        self.text.get(start as usize..end as usize).unwrap_or("")
    }

    /// The text between two offsets with each comment and decorator in it
    /// replaced by a space, since once whitespace is collapsed a `//`
    /// comment would read as part of the code after it, and each value a
    /// signature hides by `…`.
    fn code(&self, start: u32, end: u32) -> String {
        let mut holes: Vec<(u32, u32, &str)> = within(self.comments, start, end)
            .iter()
            .chain(within(self.decorators, start, end))
            .map(|&(s, e)| (s, e, " "))
            .chain(
                within(self.values, start, end)
                    .iter()
                    .map(|&(s, e)| (s, e, "…")),
            )
            .collect();
        holes.sort_unstable();
        let mut out = String::new();
        let mut at = start;
        for (s, e, with) in holes {
            // a comment inside a default goes with it
            if s < at {
                continue;
            }
            out.push_str(self.slice(at, s));
            out.push_str(with);
            at = e;
        }
        out.push_str(self.slice(at, end));
        out
    }

    fn symbol(&self, name: String, kind: SymbolKind, start: u32, end: u32) -> ExportedSymbol {
        ExportedSymbol {
            name,
            kind,
            line: self.lines.line(self.past_decorators(start)),
            signature: Some(signature(&self.code(start, end))),
        }
    }

    /// Where the code at `start` begins once the decorators, comments and
    /// whitespace before it are passed: a decorator is no part of the line
    /// of what it decorates.
    fn past_decorators(&self, start: u32) -> u32 {
        let mut at = start;
        loop {
            let rest = self.text.get(at as usize..).unwrap_or("");
            at += (rest.len() - rest.trim_start().len()) as u32;
            let skipped = [self.decorators, self.comments]
                .iter()
                .find_map(|spans| spans.iter().find(|&&(s, _)| s == at));
            match skipped {
                Some(&(_, end)) => at = end,
                None => return at,
            }
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
    /// is no function or class reads as `exports.name` does.
    fn property(
        &self,
        name: &str,
        property: &ObjectProperty,
        locals: &BTreeMap<String, Vec<ExportedSymbol>>,
    ) -> Vec<ExportedSymbol> {
        let start = property.span.start;
        // a name that a top-level declaration gives keeps that declaration
        let declared = matches!(&property.value,
            Expression::Identifier(id) if locals.contains_key(id.name.as_str()));
        match value_kind(&property.value) {
            (SymbolKind::Constant, _) if !declared => {
                vec![ExportedSymbol {
                    name: name.to_owned(),
                    kind: SymbolKind::Constant,
                    line: self.lines.line(start),
                    signature: Some(format!("module.exports.{name}")),
                }]
            }
            _ => self.assigned(name, &property.value, start, locals),
        }
    }

    /// What a value is without what it holds, after its name: a literal by
    /// its type (`: string`), anything else by its form (` = z.object(…)`).
    /// No value reaches the output; a constant may hold a secret.
    fn shape(&self, value: &Expression) -> String {
        match value {
            Expression::StringLiteral(_) | Expression::TemplateLiteral(_) => ": string".into(),
            _ if numeric(value) => ": number".into(),
            Expression::BigIntLiteral(_) => ": bigint".into(),
            Expression::BooleanLiteral(_) => ": boolean".into(),
            Expression::NullLiteral(_) => ": null".into(),
            Expression::RegExpLiteral(_) => ": RegExp".into(),
            _ => format!(" = {}", self.form(value)),
        }
    }

    /// `z.object(…).strict(…)`, `new Map(…)`, `[…] as const`,
    /// `process.env.KEY`: calls by what they call, objects and arrays
    /// elided, references by name, anything else `…`.
    fn form(&self, value: &Expression) -> String {
        let text = |span: oxc_span::Span| self.code(span.start, span.end);
        match value {
            Expression::Identifier(id) => id.name.to_string(),
            Expression::ThisExpression(_) => "this".into(),
            Expression::StaticMemberExpression(m) => {
                format!("{}.{}", self.form(&m.object), m.property.name)
            }
            Expression::CallExpression(c) => format!("{}(…)", self.form(&c.callee)),
            Expression::NewExpression(n) => format!("new {}(…)", self.form(&n.callee)),
            Expression::ObjectExpression(_) => "{…}".into(),
            // how many items, never which
            Expression::ArrayExpression(a) => match a.elements.len() {
                0 => "[]".into(),
                1 => "[… 1 item]".into(),
                n => format!("[… {n} items]"),
            },
            Expression::ParenthesizedExpression(p) => self.form(&p.expression),
            Expression::AwaitExpression(a) => format!("await {}", self.form(&a.argument)),
            Expression::TSAsExpression(a) => format!(
                "{} as {}",
                self.form(&a.expression),
                text(a.type_annotation.span())
            ),
            Expression::TSSatisfiesExpression(a) => format!(
                "{} satisfies {}",
                self.form(&a.expression),
                text(a.type_annotation.span())
            ),
            _ => "…".into(),
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
                        let declarator = match (&d.init, &d.type_annotation) {
                            // a value without a declared type: its shape
                            (Some(init), None) if kind != SymbolKind::Function => format!(
                                "{}{}",
                                self.code(d.span.start, d.id.span().end),
                                self.shape(init)
                            ),
                            _ => self.code(d.span.start, end),
                        };
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

/// The top-level names declared only as types: interfaces and type aliases,
/// a default interface included, that no value of the same name merges with
/// (a variable, a function, a class, an enum, a namespace, `import x =`).
fn declared_types(body: &[Statement]) -> BTreeSet<String> {
    let (mut types, mut values) = (BTreeSet::new(), BTreeSet::new());
    for statement in body {
        let declaration = match statement {
            Statement::ExportDeclaration(export) => Some(&export.declaration),
            Statement::ExportDefaultDeclaration(export) => {
                match &export.declaration {
                    ExportDefaultDeclarationKind::TSInterfaceDeclaration(i) => {
                        types.insert(i.id.name.to_string());
                    }
                    ExportDefaultDeclarationKind::FunctionDeclaration(f) => {
                        values.extend(f.id.as_ref().map(|id| id.name.to_string()));
                    }
                    ExportDefaultDeclarationKind::ClassDeclaration(c) => {
                        values.extend(c.id.as_ref().map(|id| id.name.to_string()));
                    }
                    _ => {}
                }
                None
            }
            _ => statement.as_declaration(),
        };
        match declaration {
            Some(Declaration::TSInterfaceDeclaration(i)) => {
                types.insert(i.id.name.to_string());
            }
            Some(Declaration::TSTypeAliasDeclaration(t)) => {
                types.insert(t.id.name.to_string());
            }
            Some(Declaration::VariableDeclaration(v)) => {
                for declarator in &v.declarations {
                    let ids = declarator.id.get_binding_identifiers();
                    values.extend(ids.iter().map(|id| id.name.to_string()));
                }
            }
            Some(Declaration::FunctionDeclaration(f)) => {
                values.extend(f.id.as_ref().map(|id| id.name.to_string()));
            }
            Some(Declaration::ClassDeclaration(c)) => {
                values.extend(c.id.as_ref().map(|id| id.name.to_string()));
            }
            Some(Declaration::TSEnumDeclaration(e)) => {
                values.insert(e.id.name.to_string());
            }
            Some(Declaration::TSNamespaceDeclaration(n)) => {
                values.insert(n.id.name.to_string());
            }
            Some(Declaration::TSImportEqualsDeclaration(i)) => {
                values.insert(i.id.name.to_string());
            }
            _ => {}
        }
    }
    types.retain(|name| !values.contains(name));
    types
}

/// Where a file names each identifier: in a type, which TypeScript reads
/// and erases (an annotation, a type argument, `typeof x` in a type, an
/// interface, `implements`, `export type`), or anywhere else, which runs
/// (an expression, JSX, `export { x }`, a decorator, `import a = b.c`). By
/// name, not by scope: a local of an imported name used as a value counts
/// for the import, which then runs. Inside a class with a decorator every
/// name counts as a value, as `emitDecoratorMetadata` may turn its
/// annotations into references that run.
#[derive(Default)]
struct Positions {
    /// The names whose positions count: those imports bind.
    wanted: BTreeSet<String>,
    in_type: usize,
    decorated: usize,
    types: BTreeSet<String>,
    values: BTreeSet<String>,
    /// The file holds JSX, which may call a factory it never names.
    jsx: bool,
}

impl Positions {
    fn in_type(&mut self, walk: impl FnOnce(&mut Self)) {
        self.in_type += 1;
        walk(self);
        self.in_type -= 1;
    }

    fn as_value(&mut self, walk: impl FnOnce(&mut Self)) {
        let in_type = std::mem::take(&mut self.in_type);
        walk(self);
        self.in_type = in_type;
    }
}

/// The names a comment's `@jsx h` and `@jsxFrag Fragment` pragmas give,
/// by their first segment (`h` of `h.f`).
fn jsx_pragmas(comment: &str) -> Vec<String> {
    let mut names = Vec::new();
    for pragma in ["@jsx ", "@jsxFrag "] {
        for (at, _) in comment.match_indices(pragma) {
            let rest = comment[at + pragma.len()..].trim_start();
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
                .collect();
            if !name.is_empty() {
                names.push(name);
            }
        }
    }
    names
}

impl<'a> Visit<'a> for Positions {
    fn visit_identifier_reference(&mut self, it: &IdentifierReference<'a>) {
        let name = it.name.as_str();
        if !self.wanted.contains(name) {
            return;
        }
        if self.in_type > 0 && self.decorated == 0 {
            self.types.insert(name.to_owned());
        } else {
            self.values.insert(name.to_owned());
        }
    }

    fn visit_ts_type(&mut self, it: &TSType<'a>) {
        self.in_type(|v| walk::walk_ts_type(v, it));
    }

    fn visit_ts_interface_declaration(&mut self, it: &TSInterfaceDeclaration<'a>) {
        self.in_type(|v| walk::walk_ts_interface_declaration(v, it));
    }

    fn visit_ts_class_implements(&mut self, it: &TSClassImplements<'a>) {
        self.in_type(|v| walk::walk_ts_class_implements(v, it));
    }

    // a computed key in a type (`[KEY]: string`) is a value the compiler
    // keeps
    fn visit_ts_property_signature(&mut self, it: &TSPropertySignature<'a>) {
        if it.computed {
            self.as_value(|v| v.visit_property_key(&it.key));
        }
        walk::walk_ts_property_signature(self, it);
    }

    fn visit_ts_method_signature(&mut self, it: &TSMethodSignature<'a>) {
        if it.computed {
            self.as_value(|v| v.visit_property_key(&it.key));
        }
        walk::walk_ts_method_signature(self, it);
    }

    fn visit_jsx_opening_element(&mut self, it: &JSXOpeningElement<'a>) {
        self.jsx = true;
        walk::walk_jsx_opening_element(self, it);
    }

    fn visit_jsx_fragment(&mut self, it: &JSXFragment<'a>) {
        self.jsx = true;
        walk::walk_jsx_fragment(self, it);
    }

    fn visit_export_named_declaration(&mut self, it: &ExportNamedDeclaration<'a>) {
        match it.export_kind.is_type() {
            true => self.in_type(|v| walk::walk_export_named_declaration(v, it)),
            false => walk::walk_export_named_declaration(self, it),
        }
    }

    fn visit_export_specifier(&mut self, it: &ExportSpecifier<'a>) {
        match it.export_kind.is_type() {
            true => self.in_type(|v| walk::walk_export_specifier(v, it)),
            false => walk::walk_export_specifier(self, it),
        }
    }

    fn visit_class(&mut self, it: &Class<'a>) {
        let decorated = !it.decorators.is_empty()
            || it.body.body.iter().any(|element| match element {
                ClassElement::MethodDefinition(m) => {
                    !m.decorators.is_empty()
                        || m.value
                            .params
                            .items
                            .iter()
                            .any(|p| !p.decorators.is_empty())
                }
                ClassElement::PropertyDefinition(p) => !p.decorators.is_empty(),
                ClassElement::AccessorProperty(p) => !p.decorators.is_empty(),
                _ => false,
            });
        self.decorated += usize::from(decorated);
        walk::walk_class(self, it);
        self.decorated -= usize::from(decorated);
    }
}

/// The static start of a computed specifier: a template's text before its
/// first substitution (`` `./pages/${name}` ``), the string a `+` starts
/// with (`'./pages/' + name`), or the segments `path.join(__dirname, ..)`
/// or `path.resolve` writes before a computed one, from the file's
/// directory (`./handlers/`). `None` when it starts computed, or when text
/// after a computed part climbs out with `..`, which may leave the prefix.
fn computed_prefix(specifier: &Expression) -> Option<String> {
    let climbs = |text: &str| text.split('/').any(|segment| segment == "..");
    let prefix = match specifier.get_inner_expression() {
        Expression::TemplateLiteral(t) if !t.expressions.is_empty() => {
            let later = t.quasis.iter().skip(1);
            if later
                .filter_map(|q| q.value.cooked.as_ref())
                .any(|q| climbs(q))
            {
                return None;
            }
            t.quasis.first()?.value.cooked.as_ref()?.to_string()
        }
        Expression::BinaryExpression(b) if b.operator == BinaryOperator::Addition => {
            let right = match b.right.get_inner_expression() {
                Expression::StringLiteral(s) => Some(s.value.as_str()),
                Expression::TemplateLiteral(t) => t
                    .quasis
                    .iter()
                    .filter_map(|q| q.value.cooked.as_ref())
                    .map(|q| q.as_str())
                    .find(|q| climbs(q)),
                _ => None,
            };
            if right.is_some_and(climbs) {
                return None;
            }
            match b.left.get_inner_expression() {
                Expression::StringLiteral(s) => s.value.to_string(),
                left => computed_prefix(left)?,
            }
        }
        Expression::CallExpression(call) => {
            let Expression::StaticMemberExpression(callee) = &call.callee else {
                return None;
            };
            let on_path = matches!(&callee.object, Expression::Identifier(o) if o.name == "path");
            if !on_path || !matches!(callee.property.name.as_str(), "join" | "resolve") {
                return None;
            }
            let mut args = call.arguments.iter().map(|a| a.as_expression());
            let first = args.next()??;
            if !matches!(first, Expression::Identifier(d) if d.name == "__dirname") {
                return None;
            }
            let mut prefix = String::from("./");
            let mut computed = false;
            for arg in args {
                match arg? {
                    // a later segment that climbs may leave the prefix
                    Expression::StringLiteral(s) if computed && climbs(&s.value) => {
                        return None;
                    }
                    Expression::StringLiteral(s) if !computed => {
                        prefix.push_str(s.value.trim_matches('/'));
                        prefix.push('/');
                    }
                    Expression::StringLiteral(_) => {}
                    // a segment computed: the path so far is the prefix
                    _ => computed = true,
                }
            }
            // every segment written out: no name is computed
            return computed.then_some(prefix);
        }
        _ => return None,
    };
    (!prefix.is_empty()).then_some(prefix)
}

/// A number, or arithmetic of numbers (`20 * 1024 * 1024`).
fn numeric(value: &Expression) -> bool {
    match value {
        Expression::NumericLiteral(_) => true,
        Expression::UnaryExpression(u) => u.operator.is_arithmetic() && numeric(&u.argument),
        Expression::BinaryExpression(b) => {
            b.operator.is_arithmetic() && numeric(&b.left) && numeric(&b.right)
        }
        Expression::ParenthesizedExpression(p) => numeric(&p.expression),
        _ => false,
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
        // cut between words, never inside a token such as `=>`
        let cut: String = s.chars().take(MAX_SIGNATURE).collect();
        let cut = match cut.rfind(' ') {
            Some(space) => &cut[..space],
            None => &cut,
        };
        s = format!("{cut} ...");
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
            // a value shows its shape, never what it holds
            ("RATES", "const rates = {…}"),
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
            (
                "shown",
                Export::Alias {
                    local: "hidden".into(),
                    line: 4,
                },
            ),
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
    fn calls_on_one_line_that_load_one_module_are_one_statement() {
        // as a compiler writes declaration files
        let file = parse(
            Path::new("x.d.ts"),
            "export type T = import('./m').A | import('./m').B | typeof import('./m');\n\
             export declare const load: () => [typeof import('./m'), import('./n').C];\n",
        )
        .unwrap();
        let imports: Vec<(&str, u32, Vec<&str>, usize)> = file
            .imports
            .iter()
            .map(|i| {
                (
                    i.specifier.as_str(),
                    i.line,
                    i.names.iter().map(String::as_str).collect(),
                    i.types.len(),
                )
            })
            .collect();
        assert_eq!(
            imports,
            [
                ("./m", 1, vec!["A", "B", "*"], 3),
                ("./m", 2, vec!["*"], 1),
                ("./n", 2, vec!["C"], 1),
            ]
        );
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
                // a factory that gives the module no name
                ("d", 7, "vi.mock", false, vec![], vec![]),
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
    fn a_hoisted_mock_whose_factory_loads_nothing_replaces_the_module() {
        let file = parse(
            Path::new("x.test.ts"),
            "const mocks = vi.hoisted(() => ({ f: vi.fn() }));\n\
             function local() { return 1; }\n\
             vi.mock('a', () => ({ f: vi.fn().mockReturnValue(Buffer.from('x')), g: mocks.f }));\n\
             jest.mock('b', function () { const own = { f: jest.fn() }; return own; });\n\
             vi.mock('c', async (importOriginal) => ({ ...(await importOriginal()) }));\n\
             vi.mock('d', (original) => wrap(original));\n\
             vi.mock('e', () => ({ ...vi.importActual('e') }));\n\
             jest.mock('f', () => jest.createMockFromModule('f'));\n\
             vi.mock('g', () => local());\n\
             vi.mock('h', () => ({ f: helpers.make() }));\n\
             vi.mock('i', () => new Fake());\n\
             vi.mock('j', async () => ({ ...(await import('./k')) }));\n\
             vi.mock('l', factory);\n\
             vi.mock('m', { spy: true });\n\
             vi.mock('n');\n\
             vi.doMock('o', () => ({}));\n\
             describe('p', () => { vi.mock('p', () => ({})); });\n",
        )
        .unwrap();
        let replaced: Vec<&str> = file
            .imports
            .iter()
            .filter(|i| i.replaces)
            .map(|i| i.specifier.as_str())
            .collect();
        // a factory of the frameworks' calls, globals and its own bindings;
        // not one that names `importOriginal`, loads a module, calls what is
        // bound outside it, is no function written in place, runs later
        // or inside a function
        assert_eq!(replaced, ["a", "b"]);
    }

    #[test]
    fn a_destructured_require_or_import_takes_its_names() {
        let file = parse(
            Path::new("x.js"),
            "const { pad, trim: t } = require('./a');\n\
             async function f() { const { default: run } = await import('./b'); }\n\
             const { x, ...rest } = require('./c');\n\
             const whole = require('./d');\n\
             const one = require('./e').one;\n\
             require('./f').go();\n\
             const { [key]: k } = require('./g');\n\
             const { h } = require('./h').inner;\n\
             import('./i').then((m) => m.i);\n\
             async function g() { return (await import('./j')).j; }\n",
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
                ("./a", vec!["pad", "trim"]),
                ("./b", vec!["default"]),
                // a rest element, a computed key: the whole module
                ("./c", vec!["*"]),
                ("./d", vec!["*"]),
                // a property read at once
                ("./e", vec!["one"]),
                ("./f", vec!["go"]),
                ("./g", vec!["*"]),
                // what is destructured is `inner`'s, which the file exports
                ("./h", vec!["inner"]),
                // a promise's `then` is none of the module's names
                ("./i", vec!["*"]),
                ("./j", vec!["j"]),
            ]
        );
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
    fn export_assignment_exports_its_declaration_as_the_default() {
        // a declaration file in the CommonJS style
        let file = parse(
            Path::new("engine.d.ts"),
            "declare class Engine {\n  start(): void;\n}\ndeclare namespace Engine {\n  const x: number;\n}\n\
             export = Engine;\n",
        )
        .unwrap();
        let symbols: Vec<(&str, u32)> = file
            .symbols
            .iter()
            .map(|s| (s.name.as_str(), s.line))
            .collect();
        assert_eq!(symbols, [("Engine", 1), ("Engine.start", 2)]);
        assert_eq!(file.exports.default_name.as_deref(), Some("Engine"));
    }

    #[test]
    fn overloads_keep_their_first_signature_in_scripts_too() {
        let file = parse(
            Path::new("legacy.ts"),
            "function g(a: string): void;\nfunction g(a: any) {}\n\
             class L {\n  m(a: string): void;\n  m(a: any) {}\n}\n",
        )
        .unwrap();
        let globals: Vec<(&str, u32, &str)> = file
            .globals
            .iter()
            .map(|s| {
                (
                    s.name.as_str(),
                    s.line,
                    s.signature.as_deref().unwrap_or_default(),
                )
            })
            .collect();
        assert_eq!(
            globals,
            [
                ("g", 1, "function g(a: string): void"),
                ("L", 3, "class L"),
                ("L.m", 4, "m(a: string): void"),
            ]
        );
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
    fn bindings_written_only_in_types_are_type_uses() {
        let file = parse(
            Path::new("uses.ts"),
            "import { A, B, C, D, E, F, G, H, I } from './m';\n\
             import J from './j';\n\
             import type { K } from './k';\n\
             let a: A = x as B;\n\
             interface Z extends C {}\n\
             class Y implements D {}\n\
             f<E>(y satisfies F);\n\
             export type { G };\n\
             export { type H };\n\
             type T = typeof I;\n\
             const j = J;\n\
             let k: K;\n",
        )
        .unwrap();
        let uses = |i: usize| -> Vec<&str> {
            file.imports[i]
                .type_uses
                .keys()
                .map(String::as_str)
                .collect()
        };
        assert_eq!(uses(0), ["A", "B", "C", "D", "E", "F", "G", "H", "I"]);
        // a value, and what `type` already marks
        assert!(uses(1).is_empty());
        assert!(uses(2).is_empty());
        // JavaScript has no types to read
        let file = parse(Path::new("uses.js"), "import { A } from './m';\n").unwrap();
        assert!(file.imports[0].type_uses.is_empty());
    }

    #[test]
    fn a_computed_specifier_s_static_start_is_its_prefix() {
        let file = parse(
            Path::new("loader.js"),
            "const path = require('path');\n\
             import(`./pages/${name}`);\n\
             require('./handlers/' + name + '.js');\n\
             require(path.join(__dirname, 'plugins', kind, 'index.js'));\n\
             import(`${base}/x`);\n\
             require(path.join(__dirname, 'all'));\n\
             require(name);\n\
             import(`./app/${a}/../../lib/${b}`);\n\
             require('./app/' + a + '/../lib');\n\
             require(path.join(__dirname, 'app', a, '..', 'lib'));\n",
        )
        .unwrap();
        let prefixes: Vec<(u32, Option<&str>)> = file
            .dynamic
            .iter()
            .map(|d| (d.line, d.prefix.as_deref()))
            .collect();
        assert_eq!(
            prefixes,
            [
                (2, Some("./pages/")),
                (3, Some("./handlers/")),
                (4, Some("./plugins/")),
                (5, None),
                (6, None),
                (7, None),
                // text after a computed part that climbs out
                (8, None),
                (9, None),
                (10, None),
            ]
        );
    }

    #[test]
    fn a_modules_declare_global_declares_globals() {
        let file = parse(
            Path::new("env.ts"),
            "export const mode = 'a';\n\
             declare global {\n  interface Window { shop: string }\n  var mode: string;\n  \
             function track(e: string): void;\n}\n\
             declare module 'other' {\n  global {\n    const hidden: string;\n  }\n}\n",
        )
        .unwrap();
        let global: Vec<(&str, SymbolKind, u32)> = file
            .declared_global
            .iter()
            .map(|s| (s.name.as_str(), s.kind, s.line))
            .collect();
        // `mode` names the module's own export, which keeps it
        assert_eq!(
            global,
            [
                ("Window", SymbolKind::Trait, 3),
                ("track", SymbolKind::Function, 5),
            ]
        );
        assert!(file.globals.is_empty());
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
                row("size", SymbolKind::Constant, 10, "module.exports.size"),
                row("run", SymbolKind::Function, 11, "run()"),
                row("go", SymbolKind::Function, 12, "go: function ()"),
            ]
        );
        // a name that no top-level declaration gives, such as one taken
        // from another module, reads as a property
        assert_eq!(
            symbols(
                "i.js",
                "const { pad } = require('./format');\nconst run = require('./fn').default;\n\
                 module.exports = { pad, run, other: pad };\n",
            ),
            [
                row("pad", SymbolKind::Constant, 3, "module.exports.pad"),
                row(
                    "run",
                    SymbolKind::Constant,
                    2,
                    "const run = require(…).default"
                ),
                row("other", SymbolKind::Constant, 3, "module.exports.other"),
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
    fn a_constant_shows_its_type_or_the_shape_of_its_value() {
        let file = parse(
            Path::new("x.ts"),
            "export const API_KEY = 'sk-123';\nexport const revalidate = 60;\n\
             export const DEBUG = false;\nexport const schema = z.object({ a: z.string() }).strict();\n\
             export const store = new Map<string, number>();\nexport const UNITS = ['g', 'kg'] as const;\n\
             export const config = { a: 1 } satisfies Config;\nexport const typed: Limits = { max: 1 };\n\
             export const alias = other;\nexport const sum = 1 + 2;\nexport const ref = process.env.KEY;\n\
             export let count = 0;\nexport const MAX_UPLOAD = 20 * 1024 * 1024;\nexport const EMPTY = [];\n",
        )
        .unwrap();
        let signatures: Vec<&str> = file
            .symbols
            .iter()
            .map(|s| s.signature.as_deref().unwrap_or_default())
            .collect();
        assert_eq!(
            signatures,
            [
                "export const API_KEY: string",
                "export const revalidate: number",
                "export const DEBUG: boolean",
                "export const schema = z.object(…).strict(…)",
                "export const store = new Map(…)",
                "export const UNITS = [… 2 items] as const",
                "export const config = {…} satisfies Config",
                "export const typed: Limits",
                "export const alias = other",
                "export const sum: number",
                "export const ref = process.env.KEY",
                "export let count: number",
                // arithmetic of numbers is a number too
                "export const MAX_UPLOAD: number",
                "export const EMPTY = []",
            ]
        );
    }

    #[test]
    fn signatures_leave_out_default_values() {
        let file = parse(
            Path::new("x.ts"),
            "export function connect(url = 'postgres://u:p@h', retries = 3 /* tries */) {}\n\
             export const sign = (key: string = process.env.KEY ?? 'sk-1', { scope = 'all' } = {}) => key;\n\
             export class Client {\n  open(token = 'abc') {}\n}\n",
        )
        .unwrap();
        let signatures: Vec<&str> = file
            .symbols
            .iter()
            .map(|s| s.signature.as_deref().unwrap_or_default())
            .collect();
        assert_eq!(
            signatures,
            [
                "export function connect(url = …, retries = …)",
                "export const sign = (key: string = …, { scope = … } = …) =>",
                "export class Client",
                "open(token = …)",
            ]
        );
    }

    #[test]
    fn signatures_leave_out_decorators_and_what_extends_calls_with() {
        let file = parse(
            Path::new("x.ts"),
            "export class Users {\n  @Get(':token')\n  static find(@Param('id') id: string): string {}\n}\n\
             export @Tag('prod') class Later {}\n\
             export class Mixed extends mixin(Base('hidden'))('more') {}\n\
             @Injectable({ key: 'sk-1' })\nclass Svc {}\nexport { Svc };\n",
        )
        .unwrap();
        let found: Vec<(&str, u32, &str)> = file
            .symbols
            .iter()
            .map(|s| {
                (
                    s.name.as_str(),
                    s.line,
                    s.signature.as_deref().unwrap_or_default(),
                )
            })
            .collect();
        assert_eq!(
            found,
            [
                ("Users", 1, "export class Users"),
                // a decorator is no part of the line either
                ("Users.find", 3, "static find(id: string): string"),
                ("Later", 5, "export class Later"),
                ("Mixed", 6, "export class Mixed extends mixin(…)(…)"),
                ("Svc", 8, "class Svc"),
            ]
        );
    }

    #[test]
    fn a_long_signature_is_cut_between_words() {
        let long = format!("export const handler = ({}: number) =>", "a".repeat(196));
        let cut = signature(&long);
        // never inside a token such as `=>`
        assert!(cut.ends_with(" ..."), "{cut}");
        assert!(!cut.contains("=..."), "{cut}");
        assert!(cut.chars().count() <= MAX_SIGNATURE + 4, "{cut}");
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
