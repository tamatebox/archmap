//! What one TS/JS file says, read from its `oxc` AST: the modules its
//! `import` and `export ... from` statements load and the names they take,
//! the declarations it exports, and its export table (see [`ExportTable`]).
//! Only top-level statements are read; function bodies are not walked.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use archmap_core::SymbolKind;
use oxc_allocator::Allocator;
use oxc_ast::ast::{
    Class, ClassElement, Declaration, ExportDefaultDeclarationKind, Expression, Function,
    ImportDeclarationSpecifier, MethodDefinitionKind, Statement, TSAccessibility,
    TSImportEqualsDeclaration, TSModuleReference,
};
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
    /// The exports a named or default import takes (`default` for a
    /// default import); empty for a namespace or side-effect import,
    /// `import x = require()` and `export ... from`.
    pub names: Vec<String>,
}

#[derive(Debug, Default)]
pub(crate) struct ParsedFile {
    pub imports: Vec<ImportStatement>,
    pub symbols: Vec<ExportedSymbol>,
    /// What the file exports, with indices into `imports`.
    pub exports: ExportTable,
}

/// Characters of a signature kept; a longer one ends in `...`.
const MAX_SIGNATURE: usize = 200;

/// Parse `text`, the file at `path`; its extension decides TypeScript or
/// JavaScript, JSX and `.d.ts`. Fails only when the parser gives up.
pub(crate) fn parse(path: &Path, text: &str) -> Result<ParsedFile, String> {
    let mut source_type = SourceType::from_path(path).map_err(|e| e.to_string())?;
    if source_type.is_javascript() {
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
    // the export still counts): the statement and the export taken, `None`
    // for a namespace.
    let mut bindings: BTreeMap<String, (usize, Option<String>)> = BTreeMap::new();
    // `export { a as b }` without a source and `export default a`, read
    // once every binding is known: (local name, exported name, line).
    let mut exported: Vec<(String, String, u32)> = Vec::new();
    for statement in &parsed.program.body {
        let start = statement.span().start;
        let line = lines.line(start);
        let index = file.imports.len();
        let load = |specifier: String, note: &'static str, names: Vec<String>| ImportStatement {
            specifier,
            line,
            note,
            names,
        };
        match statement {
            Statement::ImportDeclaration(d) => {
                let mut names = Vec::new();
                for specifier in d.specifiers.iter().flatten() {
                    let (local, taken) = match specifier {
                        ImportDeclarationSpecifier::ImportSpecifier(s) => (
                            s.local.name.to_string(),
                            Some(s.imported.name().to_string()),
                        ),
                        ImportDeclarationSpecifier::ImportDefaultSpecifier(s) => {
                            (s.local.name.to_string(), Some("default".to_owned()))
                        }
                        ImportDeclarationSpecifier::ImportNamespaceSpecifier(s) => {
                            (s.local.name.to_string(), None)
                        }
                    };
                    names.extend(taken.clone());
                    bindings.insert(local, (index, taken));
                }
                file.imports
                    .push(load(d.source.value.to_string(), "import", names));
            }
            Statement::ExportFromDeclaration(d) => {
                for s in &d.specifiers {
                    let export = Export::Reexport {
                        import: index,
                        name: s.local.name().to_string(),
                        line,
                    };
                    file.exports
                        .names
                        .entry(s.exported.name().to_string())
                        .or_insert(export);
                }
                file.imports
                    .push(load(d.source.value.to_string(), "export", Vec::new()));
            }
            Statement::ExportAllDeclaration(d) => {
                match &d.exported {
                    Some(exported) => {
                        file.exports
                            .names
                            .entry(exported.name().to_string())
                            .or_insert(Export::Namespace {
                                import: index,
                                line,
                            });
                    }
                    None => file.exports.stars.push((index, line)),
                }
                file.imports
                    .push(load(d.source.value.to_string(), "export", Vec::new()));
            }
            Statement::TSImportEqualsDeclaration(d) => {
                if let Some(specifier) = required_by(d) {
                    bindings.insert(d.id.name.to_string(), (index, None));
                    file.imports.push(load(specifier, "import", Vec::new()));
                }
            }
            Statement::ExportDeclaration(d) => {
                if let Declaration::TSImportEqualsDeclaration(i) = &d.declaration {
                    if let Some(specifier) = required_by(i) {
                        file.exports.names.entry(i.id.name.to_string()).or_insert(
                            Export::Namespace {
                                import: index,
                                line,
                            },
                        );
                        file.imports.push(load(specifier, "import", Vec::new()));
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
                    exported.push((local, name, line));
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
                // An expression or a declaration is the file's own.
                let local = match &d.declaration {
                    ExportDefaultDeclarationKind::Identifier(i) => i.name.to_string(),
                    _ => String::new(),
                };
                exported.push((local, "default".to_owned(), line));
            }
            _ => {}
        }
    }
    for (local, name, line) in exported {
        let export = match bindings.get(&local) {
            Some((import, Some(taken))) => Export::Reexport {
                import: *import,
                name: taken.clone(),
                line,
            },
            Some((import, None)) => Export::Namespace {
                import: *import,
                line,
            },
            None => Export::Local,
        };
        file.exports.names.entry(name).or_insert(export);
    }
    let mut seen = BTreeSet::new();
    file.symbols.retain(|s| seen.insert(s.name.clone()));
    Ok(file)
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
             import e = require('e');\nexport { f } from 'f';\n",
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
                ("n", vec![]),
                ("side", vec![]),
                ("e", vec![]),
                ("f", vec![]),
            ]
        );
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
        };
        let names: BTreeMap<String, Export> = [
            ("local", Export::Local),
            ("f", Export::Local),
            ("shown", Export::Local),
            ("b", reexport(0, "a", 5)),
            ("d", reexport(1, "default", 6)),
            ("ns", Export::Namespace { import: 3, line: 8 }),
            ("y", reexport(4, "x", 10)),
            ("def", reexport(5, "default", 12)),
            (
                "whole",
                Export::Namespace {
                    import: 6,
                    line: 14,
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
        assert_eq!(file.exports.stars, [(2, 7)]);
        // re-exported bindings are no symbols of this file
        let symbols: Vec<&str> = file.symbols.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(symbols, ["local", "f", "shown"]);
    }

    #[test]
    fn an_exported_import_equals_is_a_namespace() {
        let file = parse(Path::new("x.ts"), "export import fs = require('fs');\n").unwrap();
        assert_eq!(
            file.exports.names.get("fs"),
            Some(&Export::Namespace { import: 0, line: 1 })
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
