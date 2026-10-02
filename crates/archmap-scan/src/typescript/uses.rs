//! Where a TS/JS symbol is used, read on demand for one symbol: the
//! identifiers that oxc's semantic analysis resolves to the symbol, in the
//! file that defines it and in the files whose statements import it, through
//! the bindings those statements make.
//!
//! Which file a statement loads comes from the evidence the scan recorded;
//! nothing is resolved again. What a module offers on the way to the symbol
//! (a barrel's names, a namespace it passes on) comes from the export tables
//! the analyzer reads, so a binding reaches the symbol along a list of member
//! names, its path: `[]` for the symbol itself, `["formatPrice"]` for a
//! namespace of the defining file, `["money", "formatPrice"]` for a barrel
//! that passes that namespace on.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use archmap_core::{
    ArchitectureGraph, Evidence, ImportPlace, Renamed, SymbolUse, SymbolUses, Unread, UnreadReason,
    UseRole,
};
use oxc_allocator::Allocator;
use oxc_ast::ast::{
    BindingIdentifier, BindingPattern, ClassElement, Expression, ImportDeclarationSpecifier,
    ModuleExportName, Program, Statement, TSImportTypeQualifier, TSModuleReference,
};
use oxc_ast::AstKind;
use oxc_parser::Parser;
use oxc_semantic::{AstNode, AstNodes, NodeId, Semantic, SemanticBuilder, SymbolId};
use oxc_span::{GetSpan, Span};

use super::exports::{Export, MAX_HOPS};
use super::source::{parse, source_type, ParsedFile};
use crate::lines::Lines;

/// Destructurings followed from one binding: `const { money } = all`, then
/// `const { formatPrice } = money`.
const MAX_DESTRUCTURINGS: usize = 2;

/// Test helpers that return the module they load, as `require` does.
const RETURNING_HELPERS: [&str; 4] = [
    "vi.importActual",
    "vi.importMock",
    "jest.requireActual",
    "jest.requireMock",
];

/// What the pass reads for one symbol.
pub(crate) struct Request<'g> {
    pub root: &'g Path,
    pub graph: &'g ArchitectureGraph,
    /// The file that defines the symbol, relative to the root.
    pub defining: &'g str,
    /// The symbol's name: `formatPrice`, or `Wallet.pay` for a member.
    pub name: &'g str,
    /// The defining file is test code.
    pub test: bool,
    /// The statements that import the symbol, with their evidence.
    pub statements: Vec<&'g Evidence>,
}

/// Read the uses of the symbol `request` names into `out`.
pub(crate) fn read(request: &Request, out: &mut SymbolUses) {
    let tail: Vec<String> = match request.name.split_once('.') {
        Some((class, member)) => vec![class.to_owned(), member.to_owned()],
        None => vec![request.name.to_owned()],
    };
    let mut pass = Pass {
        root: request.root,
        defining: request.defining,
        tail,
        loads: loads(request.graph),
        parsed: BTreeMap::new(),
        paths: BTreeMap::new(),
        out: &mut *out,
    };
    pass.defining_file(request.test);
    let mut by_file: BTreeMap<&str, BTreeMap<u32, &Evidence>> = BTreeMap::new();
    for evidence in &request.statements {
        if evidence.file == request.defining {
            continue;
        }
        if let Some(line) = evidence.line {
            by_file
                .entry(evidence.file.as_str())
                .or_default()
                .entry(line)
                .or_insert(evidence);
        }
    }
    for (file, statements) in by_file {
        pass.importing_file(file, &statements);
    }
    // a name written as the symbol's own is no other name
    for found in &mut out.uses {
        if found.binding.as_deref() == Some(request.name) {
            found.binding = None;
        }
    }
}

/// The files each statement loads, by its file and line, from evidence the
/// scan recorded without walking re-exports. One line can hold statements
/// that load different files.
fn loads(graph: &ArchitectureGraph) -> BTreeMap<(&str, u32), BTreeSet<&str>> {
    let mut loads: BTreeMap<(&str, u32), BTreeSet<&str>> = BTreeMap::new();
    for edge in &graph.edges {
        for evidence in &edge.evidence {
            let (Some(line), Some(target)) = (evidence.line, evidence.target.as_deref()) else {
                continue;
            };
            if evidence.via().is_none() {
                loads
                    .entry((evidence.file.as_str(), line))
                    .or_default()
                    .insert(target);
            }
        }
    }
    loads
}

struct Pass<'g, 'o> {
    root: &'g Path,
    defining: &'g str,
    /// The path from the defining file's namespace to the symbol: its
    /// exported name, or a class's and a member's.
    tail: Vec<String>,
    loads: BTreeMap<(&'g str, u32), BTreeSet<&'g str>>,
    /// Files read for their export tables.
    parsed: BTreeMap<String, Option<ParsedFile>>,
    /// The paths from each module's namespace to the symbol.
    paths: BTreeMap<String, BTreeSet<Vec<String>>>,
    out: &'o mut SymbolUses,
}

/// A file read for its uses: its text, lines and semantic model.
struct Read<'a> {
    file: &'a str,
    text: &'a str,
    lines: Lines,
    program: &'a Program<'a>,
    semantic: Semantic<'a>,
    /// Uses here are test code.
    test: bool,
    /// The statement whose bindings are followed, if any.
    statement: Option<ImportPlace>,
    /// The length of the path from the defining file's namespace to the
    /// symbol: a path at least that long starts at a module whole.
    tail: usize,
}

impl Read<'_> {
    /// Whether a node that leads to the symbol along `rests` holds a module
    /// whole, so that a use other than by a static name may reach the
    /// symbol: neither the symbol itself nor the class of a member.
    fn holds_module(&self, rests: &[Vec<String>]) -> bool {
        rests.iter().any(|r| r.len() >= self.tail)
    }
}

impl<'g> Pass<'g, '_> {
    fn unread(&mut self, file: &str, line: Option<u32>, reason: UnreadReason) {
        self.out.unread.push(Unread {
            file: file.to_owned(),
            line,
            reason,
        });
    }

    /// The one file a statement at `line` of `file` loads.
    fn loaded(&self, file: &str, line: u32) -> Option<&'g str> {
        let targets = self.loads.get(&(file, line))?;
        match targets.len() {
            1 => targets.iter().next().copied(),
            _ => None,
        }
    }

    /// A file's parse for its export table, read once.
    fn parsed(&mut self, file: &str) -> Option<&ParsedFile> {
        if !self.parsed.contains_key(file) {
            let path = self.root.join(file);
            let parsed = std::fs::read_to_string(&path)
                .ok()
                .and_then(|text| parse(&path, &text).ok());
            self.parsed.insert(file.to_owned(), parsed);
        }
        self.parsed.get(file).and_then(Option::as_ref)
    }

    /// The paths from `module`'s namespace to the symbol, through the names
    /// its export table passes on unchanged (`export { a } from`,
    /// `export *`) and the namespaces it passes on (`export * as ns from`).
    /// A name passed on under another name is no path: renames are not
    /// followed.
    fn paths_of(&mut self, module: &str, hops: usize) -> BTreeSet<Vec<String>> {
        if let Some(known) = self.paths.get(module) {
            return known.clone();
        }
        let mut found = BTreeSet::new();
        if module == self.defining {
            found.insert(self.tail.clone());
            let default = self
                .parsed(module)
                .and_then(|p| p.exports.default_name.clone());
            if default.as_deref() == Some(self.tail[0].as_str()) {
                let mut path = self.tail.clone();
                path[0] = "default".to_owned();
                found.insert(path);
            }
        } else if hops < MAX_HOPS {
            // a cycle through barrels ends where it started
            self.paths.insert(module.to_owned(), BTreeSet::new());
            found = self.barrel_paths(module, hops);
        }
        self.paths.insert(module.to_owned(), found.clone());
        found
    }

    fn barrel_paths(&mut self, module: &str, hops: usize) -> BTreeSet<Vec<String>> {
        let Some(parsed) = self.parsed(module) else {
            return BTreeSet::new();
        };
        let lines: Vec<u32> = parsed.imports.iter().map(|i| i.line).collect();
        let names: Vec<(String, Export)> = parsed
            .exports
            .names
            .iter()
            .map(|(name, export)| (name.clone(), export.clone()))
            .collect();
        let stars: Vec<usize> = parsed.exports.stars.iter().map(|s| s.0).collect();
        let mut found = BTreeSet::new();
        for (exported, export) in &names {
            match export {
                Export::Reexport { import, name, .. } if name == exported => {
                    if let Some(target) = self.loaded(module, lines[*import]) {
                        let paths = self.paths_of(target, hops + 1);
                        found.extend(paths.into_iter().filter(|p| &p[0] == name));
                    }
                }
                Export::Namespace { import, .. } => {
                    if let Some(target) = self.loaded(module, lines[*import]) {
                        for path in self.paths_of(target, hops + 1) {
                            found.insert(std::iter::once(exported.clone()).chain(path).collect());
                        }
                    }
                }
                _ => {}
            }
        }
        for import in stars {
            if let Some(target) = self.loaded(module, lines[import]) {
                let paths = self.paths_of(target, hops + 1);
                // a name the barrel exports itself wins over `export *`
                found.extend(
                    paths.into_iter().filter(|p| {
                        p[0] != "default" && !names.iter().any(|(name, _)| name == &p[0])
                    }),
                );
            }
        }
        found
    }

    fn defining_file(&mut self, test: bool) {
        let file = self.defining;
        let path = self.root.join(file);
        let Ok(text) = std::fs::read_to_string(&path) else {
            self.unread(file, None, UnreadReason::FileGone);
            return;
        };
        let allocator = Allocator::default();
        let Some(read) = read_file(&allocator, &path, file, &text, test, self.tail.len()) else {
            self.unread(file, None, UnreadReason::ParseError);
            return;
        };
        // the local name the symbol, or its class, is declared by
        let exported = self.tail[0].clone();
        let local = local_name(&read.program.body, &exported).unwrap_or(exported);
        let scoping = read.semantic.scoping();
        let Some(symbol) = scoping.get_root_binding(local.as_str().into()) else {
            return;
        };
        let rests = vec![self.tail[1..].to_vec()];
        let mut uses = Vec::new();
        follow_binding(&read, symbol, &local, &rests, 0, &mut uses, self.out);
        if let [_, member] = self.tail.as_slice() {
            this_member(&read, symbol, member, &mut uses);
        }
        self.out.uses.extend(uses);
    }

    fn importing_file(&mut self, file: &'g str, statements: &BTreeMap<u32, &Evidence>) {
        let path = self.root.join(file);
        let Ok(text) = std::fs::read_to_string(&path) else {
            self.unread(file, None, UnreadReason::FileGone);
            return;
        };
        let allocator = Allocator::default();
        let tail = self.tail.len();
        let Some(mut read) = read_file(&allocator, &path, file, &text, false, tail) else {
            self.unread(file, None, UnreadReason::ParseError);
            return;
        };
        let by_line = loading_nodes(&read);
        for (&line, evidence) in statements {
            read.test = evidence.test;
            read.statement = Some(ImportPlace {
                file: file.to_owned(),
                line,
            });
            self.statement(&read, &by_line, evidence, line);
        }
    }

    /// The bindings one statement makes for the symbol, followed.
    fn statement(
        &mut self,
        read: &Read,
        by_line: &BTreeMap<u32, Vec<NodeId>>,
        evidence: &Evidence,
        line: u32,
    ) {
        let word = evidence
            .note
            .as_deref()
            .and_then(|n| n.split_whitespace().next())
            .unwrap_or("");
        if word.contains('.') && !RETURNING_HELPERS.contains(&word) {
            // a mock call returns nothing that names the symbol
            return;
        }
        let nodes = read.semantic.nodes();
        let candidates: Vec<&AstNode> = by_line
            .get(&line)
            .into_iter()
            .flatten()
            .map(|&id| nodes.get_node(id))
            .filter(|node| matches_note(node.kind(), word))
            .collect();
        if candidates.is_empty() {
            self.unread(read.file, Some(line), UnreadReason::StatementNotFound);
            return;
        }
        let sources: BTreeSet<Option<String>> =
            candidates.iter().map(|n| source_of(n.kind())).collect();
        let Some(module) = self.loaded(read.file, line).filter(|_| sources.len() == 1) else {
            self.unread(read.file, Some(line), UnreadReason::AmbiguousStatement);
            return;
        };
        let paths = self.paths_of(module, 0);
        if paths.is_empty() {
            return;
        }
        let all: Vec<Vec<String>> = paths.iter().cloned().collect();
        let mut uses = Vec::new();
        for node in candidates {
            match node.kind() {
                AstKind::ImportDeclaration(d) => {
                    for specifier in d.specifiers.iter().flatten() {
                        let (local, rests) = match specifier {
                            ImportDeclarationSpecifier::ImportSpecifier(s) => {
                                (&s.local, starting(&paths, &s.imported.name()))
                            }
                            ImportDeclarationSpecifier::ImportDefaultSpecifier(s) => {
                                (&s.local, starting(&paths, "default"))
                            }
                            ImportDeclarationSpecifier::ImportNamespaceSpecifier(s) => {
                                (&s.local, all.clone())
                            }
                        };
                        if rests.is_empty() {
                            continue;
                        }
                        let Some(symbol) = local.symbol_id.get() else {
                            continue;
                        };
                        let referenced = follow_binding(
                            read,
                            symbol,
                            &local.name,
                            &rests,
                            0,
                            &mut uses,
                            self.out,
                        );
                        if !referenced && rests.iter().any(Vec::is_empty) {
                            self.out.unused.push(evidence.clone());
                        }
                    }
                }
                AstKind::TSImportEqualsDeclaration(d) => {
                    if let Some(symbol) = d.id.symbol_id.get() {
                        follow_binding(read, symbol, &d.id.name, &all, 0, &mut uses, self.out);
                    }
                }
                AstKind::TSImportType(t) => {
                    let mut names = Vec::new();
                    if let Some(qualifier) = &t.qualifier {
                        qualifier_names(qualifier, &mut names);
                    }
                    if paths.contains(&names) {
                        let shown = match &t.qualifier {
                            Some(TSImportTypeQualifier::Identifier(i)) => i.span,
                            Some(TSImportTypeQualifier::QualifiedName(q)) => q.right.span,
                            None => t.span,
                        };
                        uses.push(make_use(read, shown, UseRole::Type, None));
                    }
                }
                AstKind::ExportFromDeclaration(d) => {
                    for specifier in &d.specifiers {
                        let local = specifier.local.name();
                        let exported = specifier.exported.name();
                        if local != exported && paths.contains(&vec![local.to_string()]) {
                            self.out.renamed.push(Renamed {
                                evidence: evidence.clone(),
                                name: exported.to_string(),
                            });
                        }
                    }
                }
                AstKind::CallExpression(_) | AstKind::ImportExpression(_) => {
                    match loaded_value(nodes, node) {
                        Some((id, span)) => {
                            follow_node(read, id, span, "", &all, 0, &mut uses, self.out);
                        }
                        // a promise of the module, whatever is done with it
                        None => self.out.escapes.push(evidence_at(read, node.kind().span())),
                    }
                }
                _ => {}
            }
        }
        self.out.uses.extend(uses);
    }
}

/// Parse and analyze one file; `None` when the parser gives up.
fn read_file<'a>(
    allocator: &'a Allocator,
    path: &Path,
    file: &'a str,
    text: &'a str,
    test: bool,
    tail: usize,
) -> Option<Read<'a>> {
    let source_type = source_type(path).ok()?;
    let parsed = Parser::new(allocator, text, source_type).parse();
    if parsed.fatal_error {
        return None;
    }
    let program: &'a Program<'a> = allocator.alloc(parsed.program);
    let semantic = SemanticBuilder::new()
        .with_build_nodes(true)
        .build(program)
        .semantic;
    Some(Read {
        file,
        text,
        lines: Lines::new(text),
        program,
        semantic,
        test,
        statement: None,
        tail,
    })
}

/// The nodes that may load a module, by the line they start on: import and
/// re-export declarations, `import x = require()`, `import()` types, and
/// calls of `require`, `import()` and the test helpers that load a module.
fn loading_nodes(read: &Read) -> BTreeMap<u32, Vec<NodeId>> {
    let mut by_line: BTreeMap<u32, Vec<NodeId>> = BTreeMap::new();
    for node in read.semantic.nodes().iter() {
        let loads = match node.kind() {
            AstKind::ImportDeclaration(_)
            | AstKind::TSImportEqualsDeclaration(_)
            | AstKind::TSImportType(_)
            | AstKind::ExportFromDeclaration(_)
            | AstKind::ExportAllDeclaration(_)
            | AstKind::ImportExpression(_) => true,
            AstKind::CallExpression(c) => loading_callee(&c.callee).is_some(),
            _ => false,
        };
        if loads {
            let line = read.lines.of(node.kind().span().start as usize);
            by_line.entry(line).or_default().push(node.id());
        }
    }
    by_line
}

/// The name of a call that loads a module by its first argument:
/// `require`, `vi.importActual`, `jest.requireActual` and the like.
fn loading_callee(callee: &Expression) -> Option<String> {
    match callee {
        Expression::Identifier(i) if i.name == "require" => Some("require".to_owned()),
        Expression::StaticMemberExpression(m) => match &m.object {
            Expression::Identifier(o) if o.name == "vi" || o.name == "jest" => {
                Some(format!("{}.{}", o.name, m.property.name))
            }
            _ => None,
        },
        _ => None,
    }
}

/// Whether a node is a statement of the kind its evidence's note names.
fn matches_note(kind: AstKind, word: &str) -> bool {
    match kind {
        AstKind::ImportDeclaration(_)
        | AstKind::TSImportEqualsDeclaration(_)
        | AstKind::TSImportType(_) => word == "import",
        AstKind::ExportFromDeclaration(_) | AstKind::ExportAllDeclaration(_) => word == "export",
        AstKind::ImportExpression(_) => word == "import()",
        AstKind::CallExpression(c) => loading_callee(&c.callee).as_deref() == Some(word),
        _ => false,
    }
}

/// The module specifier a loading node writes, when it is a literal.
fn source_of(kind: AstKind) -> Option<String> {
    let literal = |e: &Expression| match e {
        Expression::StringLiteral(s) => Some(s.value.to_string()),
        Expression::TemplateLiteral(t) if t.expressions.is_empty() => {
            t.quasis.first().map(|q| q.value.raw.to_string())
        }
        _ => None,
    };
    match kind {
        AstKind::ImportDeclaration(d) => Some(d.source.value.to_string()),
        AstKind::ExportFromDeclaration(d) => Some(d.source.value.to_string()),
        AstKind::ExportAllDeclaration(d) => Some(d.source.value.to_string()),
        AstKind::TSImportType(t) => Some(t.source.value.to_string()),
        AstKind::ImportExpression(i) => literal(&i.source),
        AstKind::CallExpression(c) => c
            .arguments
            .first()
            .and_then(|a| a.as_expression())
            .and_then(literal),
        AstKind::TSImportEqualsDeclaration(d) => match &d.module_reference {
            TSModuleReference::ExternalModuleReference(r) => Some(r.expression.value.to_string()),
            _ => None,
        },
        _ => None,
    }
}

/// The paths that start with `name`, without it.
fn starting(paths: &BTreeSet<Vec<String>>, name: &str) -> Vec<Vec<String>> {
    paths
        .iter()
        .filter(|p| p.first().map(String::as_str) == Some(name))
        .map(|p| p[1..].to_vec())
        .collect()
}

/// The names a qualifier writes: `A.B` of `import('m').A.B`.
fn qualifier_names(qualifier: &TSImportTypeQualifier, names: &mut Vec<String>) {
    match qualifier {
        TSImportTypeQualifier::Identifier(i) => names.push(i.name.to_string()),
        TSImportTypeQualifier::QualifiedName(q) => {
            qualifier_names(&q.left, names);
            names.push(q.right.name.to_string());
        }
    }
}

/// The node that holds a loaded module: a `require` call or a test
/// helper's call holds it itself, an `import()` once awaited. `None` for an
/// `import()` not awaited, which holds a promise of it.
fn loaded_value(nodes: &AstNodes, node: &AstNode) -> Option<(NodeId, Span)> {
    if !matches!(node.kind(), AstKind::ImportExpression(_)) {
        return Some((node.id(), node.kind().span()));
    }
    let parent = nodes.parent_node(node.id());
    match parent.kind() {
        AstKind::AwaitExpression(a) => Some((parent.id(), a.span)),
        _ => None,
    }
}

/// The local name the defining file declares its export `exported` by:
/// `rates` for `export { rates as RATES }`; `None` when it is declared by
/// that name.
fn local_name(body: &[Statement], exported: &str) -> Option<String> {
    for statement in body {
        let Statement::ExportNamedDeclaration(d) = statement else {
            continue;
        };
        for specifier in &d.specifiers {
            if specifier.exported.name() == exported {
                if let ModuleExportName::IdentifierReference(r) = &specifier.local {
                    return Some(r.name.to_string());
                }
            }
        }
    }
    None
}

/// Follow every reference to a binding along `rests`, the paths from it to
/// the symbol (`[]`: the binding is the symbol). Returns whether the binding
/// is referenced at all, uses or not.
fn follow_binding(
    read: &Read,
    symbol: SymbolId,
    name: &str,
    rests: &[Vec<String>],
    destructured: usize,
    uses: &mut Vec<SymbolUse>,
    out: &mut SymbolUses,
) -> bool {
    let mut referenced = false;
    for reference in read.semantic.scoping().get_resolved_references(symbol) {
        referenced = true;
        let id = reference.node_id();
        let span = read.semantic.nodes().get_node(id).kind().span();
        follow_node(read, id, span, name, rests, destructured, uses, out);
    }
    referenced
}

/// From a node that leads to the symbol along `rests`: a static member name
/// takes one step, a destructuring binds names one step on, and anything
/// else is a use where a path ends, an escape where the node holds a module
/// whole, and nothing otherwise. `written` is the code's name for the node.
#[allow(clippy::too_many_arguments)]
fn follow_node(
    read: &Read,
    id: NodeId,
    span: Span,
    written: &str,
    rests: &[Vec<String>],
    destructured: usize,
    uses: &mut Vec<SymbolUse>,
    out: &mut SymbolUses,
) {
    if rests.iter().any(Vec::is_empty) {
        if let Some(role) = role_at(read, id, span) {
            let binding = (!written.is_empty()).then(|| written.to_owned());
            uses.push(make_use(read, shown_span(read, id, span), role, binding));
        }
        return;
    }
    let module = read.holds_module(rests);
    let escape = |out: &mut SymbolUses| {
        if module {
            out.escapes.push(evidence_at(read, span));
        }
    };
    let nodes = read.semantic.nodes();
    let parent = nodes.parent_node(id);
    let member = |name: &str, at: Span, uses: &mut Vec<SymbolUse>, out: &mut SymbolUses| {
        let next: Vec<Vec<String>> = rests
            .iter()
            .filter(|r| r[0] == name)
            .map(|r| r[1..].to_vec())
            .collect();
        if !next.is_empty() {
            let written = match written {
                "" => name.to_owned(),
                written => format!("{written}.{name}"),
            };
            follow_node(
                read,
                parent.id(),
                at,
                &written,
                &next,
                destructured,
                uses,
                out,
            );
        }
    };
    match parent.kind() {
        AstKind::ParenthesizedExpression(p) => {
            follow_node(
                read,
                parent.id(),
                p.span,
                written,
                rests,
                destructured,
                uses,
                out,
            );
        }
        AstKind::TSNonNullExpression(n) => {
            follow_node(
                read,
                parent.id(),
                n.span,
                written,
                rests,
                destructured,
                uses,
                out,
            );
        }
        AstKind::StaticMemberExpression(m) if m.object.span() == span => {
            member(&m.property.name, m.span, uses, out);
        }
        AstKind::ComputedMemberExpression(m) if m.object.span() == span => match &m.expression {
            Expression::StringLiteral(s) => member(&s.value, m.span, uses, out),
            _ => escape(out),
        },
        AstKind::JSXMemberExpression(j) if j.object.span() == span => {
            member(&j.property.name, j.span, uses, out);
        }
        AstKind::TSQualifiedName(q) if q.left.span() == span => {
            member(&q.right.name, q.span, uses, out);
        }
        AstKind::VariableDeclarator(d)
            if d.init.as_ref().is_some_and(|init| init.span() == span) =>
        {
            let pattern = match &d.id {
                BindingPattern::ObjectPattern(pattern) => pattern,
                // `const m = require('m')` binds the module as an import does;
                // `const n = m` is an alias, which is data flow
                BindingPattern::BindingIdentifier(binding) if written.is_empty() => {
                    if let Some(symbol) = binding.symbol_id.get() {
                        follow_binding(read, symbol, &binding.name, rests, destructured, uses, out);
                    }
                    return;
                }
                _ => return escape(out),
            };
            if destructured >= MAX_DESTRUCTURINGS || pattern.rest.is_some() {
                escape(out);
            }
            if destructured >= MAX_DESTRUCTURINGS {
                return;
            }
            for property in &pattern.properties {
                let Some(key) = property.key.static_name() else {
                    escape(out);
                    continue;
                };
                let next: Vec<Vec<String>> = rests
                    .iter()
                    .filter(|r| r[0] == key.as_ref())
                    .map(|r| r[1..].to_vec())
                    .collect();
                if next.is_empty() {
                    continue;
                }
                let Some(binding) = bound_identifier(&property.value) else {
                    escape(out);
                    continue;
                };
                if let Some(symbol) = binding.symbol_id.get() {
                    follow_binding(
                        read,
                        symbol,
                        &binding.name,
                        &next,
                        destructured + 1,
                        uses,
                        out,
                    );
                }
            }
        }
        // passed on or named in a type, not used here
        AstKind::ExportSpecifier(_)
        | AstKind::ExportDefaultDeclaration(_)
        | AstKind::JSXClosingElement(_)
        | AstKind::TSTypeQuery(_) => {}
        _ => escape(out),
    }
}

/// The identifier a binding pattern binds, through a default value
/// (`{ a = 1 }`); `None` for a nested pattern.
fn bound_identifier<'b>(pattern: &'b BindingPattern<'b>) -> Option<&'b BindingIdentifier<'b>> {
    match pattern {
        BindingPattern::BindingIdentifier(i) => Some(i),
        BindingPattern::AssignmentPattern(a) => bound_identifier(&a.left),
        _ => None,
    }
}

/// What the code at node `id` does with the symbol, from its parents;
/// `None` where it only passes the name on (an export) or closes a JSX
/// element whose opening name counted.
fn role_at(read: &Read, id: NodeId, span: Span) -> Option<UseRole> {
    let nodes = read.semantic.nodes();
    let (mut id, mut span) = (id, span);
    loop {
        let parent = nodes.parent_node(id);
        if parent.id() == id {
            return Some(UseRole::Read);
        }
        let role = match parent.kind() {
            AstKind::ParenthesizedExpression(_)
            | AstKind::TSNonNullExpression(_)
            | AstKind::ChainExpression(_) => {
                id = parent.id();
                span = parent.kind().span();
                continue;
            }
            AstKind::CallExpression(c) if c.callee.span() == span => UseRole::Call,
            AstKind::NewExpression(n) if n.callee.span() == span => UseRole::New,
            AstKind::TaggedTemplateExpression(t) if t.tag.span() == span => UseRole::Call,
            AstKind::JSXOpeningElement(_) => UseRole::Jsx,
            AstKind::TSTypeReference(_)
            | AstKind::TSQualifiedName(_)
            | AstKind::TSClassImplements(_)
            | AstKind::TSInterfaceHeritage(_)
            | AstKind::TSTypeQuery(_) => UseRole::Type,
            AstKind::ExportSpecifier(_)
            | AstKind::ExportDefaultDeclaration(_)
            | AstKind::JSXClosingElement(_) => return None,
            _ => UseRole::Read,
        };
        return Some(role);
    }
}

/// Where a use is shown: the last name a member chain writes
/// (`formatPrice` of `m.formatPrice`), else the node itself.
fn shown_span(read: &Read, id: NodeId, span: Span) -> Span {
    match read.semantic.nodes().get_node(id).kind() {
        AstKind::StaticMemberExpression(m) => m.property.span,
        AstKind::ComputedMemberExpression(m) => m.expression.span(),
        AstKind::JSXMemberExpression(j) => j.property.span,
        AstKind::TSQualifiedName(q) => q.right.span,
        _ => span,
    }
}

fn make_use(read: &Read, span: Span, role: UseRole, binding: Option<String>) -> SymbolUse {
    let mut evidence = evidence_at(read, span);
    evidence.type_only = role == UseRole::Type;
    SymbolUse {
        evidence,
        column: read.lines.column(read.text, span.start as usize),
        role,
        binding,
        statement: read.statement.clone(),
    }
}

fn evidence_at(read: &Read, span: Span) -> Evidence {
    Evidence::new(read.file)
        .at_line(read.lines.of(span.start as usize))
        .in_test(read.test)
}

/// `this.member` inside the class `class` declares, where `this` is an
/// instance of it (or the class itself, for a static member): in its own
/// members of the same kind and the arrow functions in them, never in a
/// nested function or class.
fn this_member(read: &Read, class: SymbolId, member: &str, uses: &mut Vec<SymbolUse>) {
    let nodes = read.semantic.nodes();
    let declaration = nodes.get_node(read.semantic.scoping().symbol_declaration(class));
    let AstKind::Class(declared) = declaration.kind() else {
        return;
    };
    let is_static = declared.body.body.iter().find_map(|element| match element {
        ClassElement::MethodDefinition(m) if m.key.static_name().as_deref() == Some(member) => {
            Some(m.r#static)
        }
        ClassElement::PropertyDefinition(p) if p.key.static_name().as_deref() == Some(member) => {
            Some(p.r#static)
        }
        _ => None,
    });
    let Some(is_static) = is_static else {
        return;
    };
    for node in nodes.iter() {
        let AstKind::StaticMemberExpression(m) = node.kind() else {
            continue;
        };
        if m.property.name != member || !matches!(m.object, Expression::ThisExpression(_)) {
            continue;
        }
        if this_of(nodes, node.id()) != Some((declaration.id(), is_static)) {
            continue;
        }
        if let Some(role) = role_at(read, node.id(), m.span) {
            let binding = Some(format!("this.{member}"));
            uses.push(make_use(read, m.property.span, role, binding));
        }
    }
}

/// The class whose own member a `this` at `id` sits in, with whether that
/// member is static: arrow functions keep `this`, a nested function or
/// class does not.
fn this_of(nodes: &AstNodes, id: NodeId) -> Option<(NodeId, bool)> {
    for ancestor in nodes.ancestors(id) {
        match ancestor.kind() {
            AstKind::Function(_) => {
                let method = nodes.parent_node(ancestor.id());
                let AstKind::MethodDefinition(m) = method.kind() else {
                    return None;
                };
                return Some((class_of(nodes, method.id())?, m.r#static));
            }
            AstKind::PropertyDefinition(p) => {
                return Some((class_of(nodes, ancestor.id())?, p.r#static));
            }
            AstKind::StaticBlock(_) => return Some((class_of(nodes, ancestor.id())?, true)),
            AstKind::Class(_) => return None,
            _ => {}
        }
    }
    None
}

/// The class a class element at `id` belongs to.
fn class_of(nodes: &AstNodes, id: NodeId) -> Option<NodeId> {
    nodes
        .ancestors(id)
        .find(|a| matches!(a.kind(), AstKind::Class(_)))
        .map(AstNode::id)
}
