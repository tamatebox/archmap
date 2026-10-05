//! Where an environment variable is read and written, read on demand for one
//! name: from the TS/JS files of the scan whose text names it or
//! `process.env`, parsed, so that strings and comments that name it count
//! for nothing.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use archmap_core::{EnvUses, Evidence, Unread, UnreadReason};
use oxc_allocator::Allocator;
use oxc_ast::ast::{
    BindingPattern, Expression, IdentifierReference, ImportDeclarationSpecifier, UnaryOperator,
};
use oxc_ast::AstKind;
use oxc_parser::Parser;
use oxc_semantic::{AstNodes, NodeId, Scoping, SemanticBuilder, SymbolId};
use oxc_span::{GetSpan, Span};

use super::source::source_type;
use crate::lines::Lines;

/// Read where `name` is read and written in `files`, relative to `root`,
/// `test` telling which are test code.
pub(crate) fn read(
    root: &Path,
    files: &[String],
    name: &str,
    test: impl Fn(&str) -> bool,
) -> EnvUses {
    let mut found = EnvUses::default();
    for file in files {
        let path = root.join(file);
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        // what may read it: its name, or the environment by a computed key or
        // whole, through `process` imported from Node too
        let may_read = [
            name,
            "process.env",
            "import.meta.env",
            "process'",
            "process\"",
        ];
        if !may_read.iter().any(|m| text.contains(m)) {
            continue;
        }
        let Ok(source_type) = source_type(&path) else {
            continue;
        };
        let allocator = Allocator::default();
        let parsed = Parser::new(&allocator, &text, source_type).parse();
        if parsed.fatal_error {
            found.unread.push(Unread {
                file: file.clone(),
                line: None,
                reason: UnreadReason::ParseError,
            });
            continue;
        }
        let program = allocator.alloc(parsed.program);
        let semantic = SemanticBuilder::new()
            .with_build_nodes(true)
            .build(program)
            .semantic;
        found.files_read += 1;
        let read = File {
            file,
            lines: Lines::new(&text),
            nodes: semantic.nodes(),
            scoping: semantic.scoping(),
            test: test(file),
        };
        read.find(name, &mut found);
    }
    found.normalize();
    found
}

/// What a file binds `process` and its environment to.
#[derive(Default)]
struct Bound {
    process: BTreeSet<SymbolId>,
    env: BTreeMap<SymbolId, &'static str>,
}

struct File<'a> {
    file: &'a str,
    lines: Lines,
    nodes: &'a AstNodes<'a>,
    scoping: &'a Scoping,
    test: bool,
}

impl File<'_> {
    fn at(&self, span: Span, note: &str) -> Evidence {
        Evidence::new(self.file)
            .at_line(self.lines.of(span.start as usize))
            .with_note(note)
            .in_test(self.test)
    }

    /// The binding a reference resolves to, `None` for a global.
    fn symbol(&self, id: &IdentifierReference) -> Option<SymbolId> {
        let reference = id.reference_id.get()?;
        self.scoping.get_reference(reference).symbol_id()
    }

    /// Whether `expression` is Node's `process`: the global, or a binding
    /// of the module imported whole (`import process from 'node:process'`).
    fn process(&self, expression: &Expression, bound: &Bound) -> bool {
        let Expression::Identifier(id) = expression else {
            return false;
        };
        match self.symbol(id) {
            None => id.name == "process",
            Some(symbol) => bound.process.contains(&symbol),
        }
    }

    /// The environment object `expression` is, by what it reads it from:
    /// `process.env`, `import.meta.env`, or a binding of either
    /// (`import { env } from 'node:process'`, `const { env } = process`).
    fn environment(&self, expression: &Expression, bound: &Bound) -> Option<&'static str> {
        match expression {
            Expression::StaticMemberExpression(m) if m.property.name == "env" => match &m.object {
                Expression::ImportMeta(_) => Some("import.meta.env"),
                object if self.process(object, bound) => Some("process.env"),
                _ => None,
            },
            Expression::Identifier(id) => self
                .symbol(id)
                .and_then(|symbol| bound.env.get(&symbol).copied()),
            _ => None,
        }
    }

    /// The bindings of `process` and of the environment the file makes.
    fn bound(&self) -> Bound {
        let mut bound = Bound::default();
        for node in self.nodes.iter() {
            match node.kind() {
                AstKind::ImportDeclaration(d)
                    if matches!(d.source.value.as_str(), "process" | "node:process") =>
                {
                    for specifier in d.specifiers.iter().flatten() {
                        match specifier {
                            ImportDeclarationSpecifier::ImportSpecifier(s)
                                if s.imported.name() == "env" =>
                            {
                                if let Some(symbol) = s.local.symbol_id.get() {
                                    bound.env.insert(symbol, "process.env");
                                }
                            }
                            ImportDeclarationSpecifier::ImportDefaultSpecifier(s) => {
                                bound.process.extend(s.local.symbol_id.get());
                            }
                            ImportDeclarationSpecifier::ImportNamespaceSpecifier(s) => {
                                bound.process.extend(s.local.symbol_id.get());
                            }
                            ImportDeclarationSpecifier::ImportSpecifier(_) => {}
                        }
                    }
                }
                AstKind::VariableDeclarator(d) => {
                    let Some(init) = &d.init else {
                        continue;
                    };
                    match &d.id {
                        // `const env = process.env`: the environment again
                        BindingPattern::BindingIdentifier(b) => {
                            if let (Some(object), Some(symbol)) =
                                (self.environment(init, &bound), b.symbol_id.get())
                            {
                                bound.env.insert(symbol, object);
                            }
                        }
                        // `const { env } = process`
                        BindingPattern::ObjectPattern(p) if self.process(init, &bound) => {
                            for property in &p.properties {
                                let env = property.key.static_name().as_deref() == Some("env");
                                if let (true, BindingPattern::BindingIdentifier(b)) =
                                    (env, &property.value)
                                {
                                    if let Some(symbol) = b.symbol_id.get() {
                                        bound.env.insert(symbol, "process.env");
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        bound
    }

    /// The reads and writes of `name` in the file, and the places that
    /// may read it unseen.
    fn find(&self, name: &str, found: &mut EnvUses) {
        let bound = self.bound();
        for node in self.nodes.iter() {
            match node.kind() {
                AstKind::StaticMemberExpression(m) if m.property.name == "env" => {
                    let object = match &m.object {
                        Expression::ImportMeta(_) => "import.meta.env",
                        object if self.process(object, &bound) => "process.env",
                        _ => continue,
                    };
                    self.env(node.id(), m.span, object, name, found);
                }
                // a binding of the environment, read where it is used
                AstKind::IdentifierReference(id) => {
                    if let Some(object) = self.symbol(id).and_then(|s| bound.env.get(&s)) {
                        self.env(node.id(), id.span, object, name, found);
                    }
                }
                // a test sets it for its run
                AstKind::CallExpression(c) => {
                    let Expression::StaticMemberExpression(callee) = &c.callee else {
                        continue;
                    };
                    let stubs = matches!(&callee.object, Expression::Identifier(id) if id.name == "vi")
                        && callee.property.name == "stubEnv";
                    let named = c
                        .arguments
                        .first()
                        .and_then(|a| a.as_expression())
                        .is_some_and(
                            |a| matches!(a, Expression::StringLiteral(s) if s.value == name),
                        );
                    if stubs && named {
                        found.writes.push(self.at(c.span, "vi.stubEnv"));
                    }
                }
                _ => {}
            }
        }
    }

    /// What reads `object` (`process.env`) at `span`, the node `id`, does
    /// with it.
    fn env(&self, id: NodeId, span: Span, object: &str, name: &str, found: &mut EnvUses) {
        let parent = self.nodes.parent_node(id);
        match parent.kind() {
            AstKind::StaticMemberExpression(m) if m.object.span() == span => {
                if m.property.name == name {
                    self.member(parent.id(), m.span, object, found);
                }
            }
            AstKind::ComputedMemberExpression(m) if m.object.span() == span => {
                match &m.expression {
                    Expression::StringLiteral(s) if s.value == name => {
                        self.member(parent.id(), m.span, object, found);
                    }
                    Expression::StringLiteral(_) => {}
                    Expression::TemplateLiteral(t) if t.expressions.is_empty() => {
                        if t.quasis.iter().any(|q| q.value.raw == name) {
                            self.member(parent.id(), m.span, object, found);
                        }
                    }
                    _ => found.computed.push(self.at(m.span, object)),
                }
            }
            AstKind::VariableDeclarator(d)
                if d.init.as_ref().is_some_and(|init| init.span() == span) =>
            {
                let pattern = match &d.id {
                    BindingPattern::ObjectPattern(pattern) => pattern,
                    // a binding of the environment, whose uses are read
                    BindingPattern::BindingIdentifier(_) => return,
                    _ => return found.whole.push(self.at(span, object)),
                };
                for property in &pattern.properties {
                    if property.key.static_name().as_deref() == Some(name) {
                        found.reads.push(self.at(property.span, object));
                    }
                }
                if pattern.rest.is_some() {
                    found.whole.push(self.at(span, object));
                }
            }
            _ => found.whole.push(self.at(span, object)),
        }
    }

    /// A member of the object that is the variable: written where it is
    /// assigned or deleted, read elsewhere.
    fn member(&self, id: NodeId, span: Span, object: &str, found: &mut EnvUses) {
        let parent = self.nodes.parent_node(id);
        let written = match parent.kind() {
            AstKind::AssignmentExpression(a) => a.left.span() == span,
            AstKind::UpdateExpression(_) => true,
            AstKind::UnaryExpression(u) => u.operator == UnaryOperator::Delete,
            _ => false,
        };
        match written {
            true => found.writes.push(self.at(span, object)),
            false => found.reads.push(self.at(span, object)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uses_in(tag: &str, text: &str, name: &str) -> EnvUses {
        let dir = std::env::temp_dir().join(format!("archmap-env-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.ts"), text).unwrap();
        let found = read(&dir, &["a.ts".to_owned()], name, |_| false);
        std::fs::remove_dir_all(&dir).unwrap();
        found
    }

    fn lines(list: &[Evidence]) -> Vec<u32> {
        list.iter().filter_map(|e| e.line).collect()
    }

    #[test]
    fn reads_writes_and_what_may_read_it_unseen() {
        let found = uses_in(
            "forms",
            "const a = process.env.APP_REGION;\n\
             const b = process.env['APP_REGION'] ?? process.env.OTHER;\n\
             const { APP_REGION, OTHER } = process.env;\n\
             const c = import.meta.env.APP_REGION;\n\
             process.env.APP_REGION = 'x';\n\
             delete process.env.APP_REGION;\n\
             vi.stubEnv('APP_REGION', 'y');\n\
             const key = 'APP_' + 'REGION';\n\
             const d = process.env[key];\n\
             const all = { ...process.env };\n\
             // process.env.APP_REGION in a comment\n\
             const s = 'process.env.APP_REGION';\n",
            "APP_REGION",
        );
        assert_eq!(lines(&found.reads), [1, 2, 3, 4]);
        assert_eq!(lines(&found.writes), [5, 6, 7]);
        assert_eq!(lines(&found.computed), [9]);
        assert_eq!(lines(&found.whole), [10]);
        assert_eq!(found.reads[3].note.as_deref(), Some("import.meta.env"));
    }

    #[test]
    fn process_imported_from_node_and_bindings_of_the_environment_count() {
        let found = uses_in(
            "node",
            "import proc from 'node:process';\n\
             import { env } from 'process';\n\
             const a = proc.env.APP_REGION;\n\
             const b = env.APP_REGION;\n\
             const { env: e } = proc;\n\
             const c = e['APP_REGION'];\n\
             const meta = import.meta.env;\n\
             const d = meta[key];\n\
             const kept = process.env;\n\
             send(kept);\n",
            "APP_REGION",
        );
        assert_eq!(lines(&found.reads), [3, 4, 6]);
        assert_eq!(lines(&found.computed), [8]);
        // an alias passed on passes the environment whole
        assert_eq!(lines(&found.whole), [10]);
    }

    #[test]
    fn a_local_named_process_is_no_environment() {
        let found = uses_in(
            "local",
            "const process = { env: { APP_REGION: 1 } };\nexport const a = process.env.APP_REGION;\n",
            "APP_REGION",
        );
        assert!(found.reads.is_empty(), "{found:?}");
    }
}
