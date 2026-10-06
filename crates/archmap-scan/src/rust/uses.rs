//! Where a Rust symbol is used, read on demand for one symbol: the files
//! whose statements `query` lists and every file of the symbol's crate (unit
//! tests import nothing from their own crate), parsed again for positions.
//! Each path is resolved where it is written, in the module that encloses
//! it, with the analyzer's own resolver against the index the scan kept, so
//! inline modules, block-level `use`, globs and re-exports resolve as the
//! scan resolves them.

use std::collections::BTreeSet;
use std::path::Path;

use archmap_core::{
    Evidence, ImportPlace, Symbol, SymbolKind, SymbolUse, SymbolUses, Unread, UnreadReason, UseRole,
};
use syn::visit::{self, Visit};

use super::source::{cfg_test, name, use_decls, visit_arguments, UseDecl, NOT_CODE};
use super::tree::{Ns, Resolved, Resolver};
use super::{text_hash, Index};
use crate::context::display_path;

/// An item a path reaches: the file that defines it, the module of that
/// file (its index in `RustFile::modules`), and its name there.
type Item = (usize, usize, String);

/// What a use must resolve to.
struct Target {
    /// The file that defines the item (for a method, its type).
    file: usize,
    /// The module of that file that defines it, so that an item of the
    /// same name in an inline module is another; `None` when the scan
    /// cannot place it, and any module of the file counts.
    module: Option<usize>,
    /// The item's name there (for a method, its type's).
    name: String,
    /// A method's name, for `Type::method` symbols.
    member: Option<String>,
    /// A method that takes `self`: values of its type may call it unseen.
    instance: bool,
    /// A constant or a struct: a plain name in a pattern that reaches it
    /// is a path pattern, no binding.
    pattern: bool,
}

/// Read the uses of the Rust `symbol` into `out`. `statements` are those
/// `query` lists for it.
pub(crate) fn read(
    index: &Index,
    root: &Path,
    symbol: &Symbol,
    statements: &[&Evidence],
    out: &mut SymbolUses,
) {
    let resolver = Resolver::new(&index.forest, &index.files, &index.packages);
    let Some(target) = target(index, &resolver, symbol) else {
        return;
    };
    let names = names(index, &target);
    // the statements' files, and every file of the crate that defines it
    let mut files: BTreeSet<usize> = statements
        .iter()
        .filter_map(|e| file_index(index, &e.file))
        .collect();
    let roots: BTreeSet<Option<usize>> = index
        .forest
        .nodes
        .iter()
        .filter(|n| n.file == target.file)
        .map(|n| n.root)
        .collect();
    files.extend(
        index
            .forest
            .nodes
            .iter()
            .filter(|n| roots.contains(&n.root))
            .map(|n| n.file),
    );
    let mut uses = Vec::new();
    for file in files {
        let rel = display_path(&index.files[file].rel);
        let Ok(text) = std::fs::read_to_string(root.join(&index.files[file].rel)) else {
            out.unread.push(unread(&rel, UnreadReason::FileGone));
            continue;
        };
        // a file that names none of them uses none of them
        if !names.iter().any(|n| text.contains(n.as_str())) {
            continue;
        }
        if text_hash(&text) != index.files[file].hash {
            out.unread.push(unread(&rel, UnreadReason::Changed));
            continue;
        }
        let Ok(parsed) = syn::parse_file(&text) else {
            out.unread.push(unread(&rel, UnreadReason::ParseError));
            continue;
        };
        let mut walker = Walker {
            index,
            resolver: &resolver,
            target: &target,
            file,
            rel: &rel,
            module: 0,
            nodes: nodes_of(index, file, 0),
            tests: vec![false],
            frames: vec![Frame::default()],
            binding: None,
            in_impl: false,
            uses: Vec::new(),
        };
        walker.visit_file(&parsed);
        uses.extend(walker.uses);
    }
    end_statements(index, &target, statements, &uses, out);
    out.uses.extend(uses);
    for found in &mut out.uses {
        if found.binding.as_deref() == Some(symbol.name.as_str()) {
            found.binding = None;
        }
    }
}

/// The namespace a path's last name is read in: a value for an
/// expression's or a pattern's path, else a type.
fn namespace(value: bool) -> Ns {
    if value {
        Ns::Value
    } else {
        Ns::Type
    }
}

fn unread(file: &str, reason: UnreadReason) -> Unread {
    Unread {
        file: file.to_owned(),
        line: None,
        reason,
    }
}

fn file_index(index: &Index, rel: &str) -> Option<usize> {
    index.files.iter().position(|f| display_path(&f.rel) == rel)
}

/// The item a use resolves to: the symbol's own, or for a method
/// (`Type::method`) its type's, in the file and module that define the
/// type.
fn target(index: &Index, resolver: &Resolver, symbol: &Symbol) -> Option<Target> {
    if symbol.kind == SymbolKind::Module {
        return None;
    }
    let location = symbol.location()?;
    let (name, member) = match symbol.name.rsplit_once("::") {
        Some((owner, member)) => {
            let owner = owner.split('<').next().unwrap_or(owner);
            let owner = owner.rsplit("::").next().unwrap_or(owner).trim();
            (owner.to_owned(), Some(member.to_owned()))
        }
        None => (symbol.name.clone(), None),
    };
    // a method whose type another file defines is reached through that file
    let file = symbol
        .evidence
        .iter()
        .find(|e| e.note.as_deref() == Some("impl"))
        .and_then(|e| e.target.as_deref())
        .unwrap_or(&location.file);
    let file = file_index(index, file)?;
    // the declaration, in the module of its file that holds it
    let at = file_index(index, &location.file)?;
    let declared = index.files[at]
        .parsed
        .modules
        .iter()
        .enumerate()
        .find_map(|(module, facts)| {
            let decl = facts
                .symbols
                .iter()
                .find(|s| s.name == symbol.name && Some(s.line) == location.line)?;
            Some((module, decl))
        });
    // an item is defined where it is declared; a method's type where the
    // path of its `impl` reaches it, as the scan resolved it
    let (module, name) = match (&member, declared) {
        (None, Some((module, _))) => (Some(module), name),
        (Some(_), Some((module, decl))) => {
            let reached = decl.owner.as_ref().and_then(|ty| {
                nodes_of(index, at, module).into_iter().find_map(|node| {
                    reach(
                        index,
                        resolver,
                        node,
                        &ty.segments,
                        ty.leading_colon,
                        Ns::Type,
                    )
                })
            });
            match reached {
                Some((f, module, item)) if f == file => (Some(module), item),
                _ => (None, name),
            }
        }
        (_, None) => (None, name),
    };
    let instance = member.is_some() && symbol.signature.as_deref().is_some_and(takes_self);
    let pattern =
        member.is_none() && matches!(symbol.kind, SymbolKind::Constant | SymbolKind::Struct);
    Some(Target {
        file,
        module,
        name,
        member,
        instance,
        pattern,
    })
}

/// The item `segments`, written in module node `node`, reach, as the scan
/// resolves a `use` of that path, the last name looked up in `ns`; `None`
/// when they reach no item of the repository.
fn reach(
    index: &Index,
    resolver: &Resolver,
    node: usize,
    segments: &[String],
    leading_colon: bool,
    ns: Ns,
) -> Option<Item> {
    let decl = UseDecl {
        path: segments.to_vec(),
        binds: None,
        glob: false,
        leading_colon,
        reexport: false,
        line: 0,
        scope: archmap_core::Scope::Module,
        test: false,
        note: "use",
    };
    match resolver.resolve_in(node, &decl, ns) {
        Resolved::Module {
            node,
            name: Some(name),
            ..
        } => {
            let module = &index.forest.nodes[node];
            Some((module.file, module.module, name))
        }
        _ => None,
    }
}

/// Whether a Rust function's signature takes `self` first (`&self`,
/// `&'a mut self`, `self: Box<Self>`): a method that values of its type
/// call.
pub fn takes_self(signature: &str) -> bool {
    // the parameters open at the first `(` after the name outside the
    // generics, whose bounds may hold one (`<F: Fn(u32) -> u32>`)
    let after_fn = signature
        .find("fn ")
        .map_or(signature, |at| &signature[at + 3..]);
    let mut depth = 0usize;
    let mut previous = ' ';
    let mut open = None;
    for (at, c) in after_fn.char_indices() {
        match c {
            '<' => depth += 1,
            // the arrow of `Fn(u32) -> u32` closes nothing
            '>' if previous != '-' => depth = depth.saturating_sub(1),
            '(' if depth == 0 => {
                open = Some(at);
                break;
            }
            _ => {}
        }
        previous = c;
    }
    let Some(after) = open.map(|at| &after_fn[at + 1..]) else {
        return false;
    };
    let first = after.split([',', ')']).next().unwrap_or("").trim();
    let receiver = first.split(':').next().unwrap_or(first).trim();
    receiver
        .split_whitespace()
        .last()
        .map(|w| w.trim_start_matches('&'))
        == Some("self")
}

/// The names a file must hold to use the target: its own, its member's,
/// and every name a re-export gives it (`pub use graph::build as make`).
fn names(index: &Index, target: &Target) -> BTreeSet<String> {
    let mut names: BTreeSet<String> = BTreeSet::from([target.name.clone()]);
    names.extend(target.member.clone());
    for file in &index.files {
        for module in &file.parsed.modules {
            for decl in &module.uses {
                if decl.path.last() == Some(&target.name) {
                    names.extend(decl.binds.clone());
                }
            }
        }
    }
    names
}

/// The nodes of module `module` of file `file`: one per crate that loads
/// the file.
fn nodes_of(index: &Index, file: usize, module: usize) -> Vec<usize> {
    index
        .forest
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.file == file && n.module == module)
        .map(|(i, _)| i)
        .collect()
}

/// Every statement `query` listed ends in one list: a use through it, a
/// re-export that only passes the symbol on, a value of a method's type,
/// or never used.
fn end_statements(
    index: &Index,
    target: &Target,
    statements: &[&Evidence],
    uses: &[SymbolUse],
    out: &mut SymbolUses,
) {
    let used_files: BTreeSet<&str> = uses.iter().map(|u| u.evidence.file.as_str()).collect();
    for statement in statements {
        let Some(line) = statement.line else {
            continue;
        };
        let used = uses.iter().any(|u| {
            u.statement
                .as_ref()
                .is_some_and(|s| s.file == statement.file && s.line == line)
        });
        let word = statement
            .note
            .as_deref()
            .and_then(|n| n.split_whitespace().next())
            .unwrap_or("");
        // a path in code is its own use: the file's uses end it
        let ended = used || (word == "path" && used_files.contains(statement.file.as_str()));
        if ended {
            continue;
        }
        let reexport = file_index(index, &statement.file).is_some_and(|f| {
            index.files[f]
                .parsed
                .modules
                .iter()
                .flat_map(|m| &m.uses)
                .any(|d| d.line == line && d.reexport)
        });
        if reexport {
            out.passed_on.push((*statement).clone());
        } else if target.instance {
            out.values.push((*statement).clone());
        } else {
            out.unused.push((*statement).clone());
        }
    }
}

/// What the code around the walk binds in one scope: a block, an item, a
/// closure, or the branch that an `if let`, a `while let`, a match arm or a
/// `for` binds its pattern for.
#[derive(Default)]
struct Frame {
    /// The bindings of parameters and patterns. An item nested inside
    /// cannot use them (the compiler rejects it), so they hide its names
    /// too.
    locals: BTreeSet<String>,
    /// A block's items and an item's generic parameters, by namespace: they
    /// hide the module's names in the whole scope.
    values: BTreeSet<String>,
    types: BTreeSet<String>,
    /// What a block's `use` declarations bind.
    uses: Vec<BlockUse>,
    /// The module nodes that a block's glob `use` declarations import, with
    /// their lines.
    globs: Vec<(usize, u32)>,
}

/// A name that a block's `use` declaration binds.
struct BlockUse {
    name: String,
    /// The module node it reaches and the item there (`None`: the module
    /// itself); `None` when the scan cannot place it.
    reached: Option<(usize, Option<String>)>,
    line: u32,
}

/// Where the name a path starts with comes from, where the path is written.
enum Origin {
    /// A binding or an item of a block, or a block's `use` of what the scan
    /// cannot place: no item of the repository.
    Hidden,
    /// A block's `use` or glob at `line`, and what the path reaches through
    /// it.
    Block { reached: Vec<Item>, line: u32 },
    /// The module's own names, as the resolver reads them.
    Module,
}

struct Walker<'a> {
    index: &'a Index,
    resolver: &'a Resolver<'a>,
    target: &'a Target,
    file: usize,
    rel: &'a str,
    /// The module of the file the walk is in, as `collect` numbers it.
    module: usize,
    /// Its nodes: one per crate that loads the file.
    nodes: Vec<usize>,
    /// Whether the enclosing items are test code (`#[test]`,
    /// `#[cfg(test)]`), innermost last.
    tests: Vec<bool>,
    /// The scopes around the walk, innermost last.
    frames: Vec<Frame>,
    /// The names the pattern being read binds, while one is read.
    binding: Option<BTreeSet<String>>,
    /// Inside an `impl` whose type is the target's: its own or a trait's.
    in_impl: bool,
    uses: Vec<SymbolUse>,
}

impl Walker<'_> {
    fn test(&self) -> bool {
        self.tests.last().copied().unwrap_or(false)
            || self
                .nodes
                .iter()
                .any(|&n| self.index.forest.nodes[n].test || self.index.forest.in_test_target(n))
    }

    /// Where `segments`, written here, start: in the value namespace when
    /// `value` (an expression's or a pattern's path) and the path has one
    /// segment, else in the type namespace. A block's names come before the
    /// module's; a glob counts when the path reaches an item through it.
    fn origin(&self, segments: &[String], value: bool) -> Origin {
        let Some((first, rest)) = segments.split_first() else {
            return Origin::Hidden;
        };
        for frame in self.frames.iter().rev() {
            let hidden = match value && rest.is_empty() {
                true => frame.locals.contains(first) || frame.values.contains(first),
                false => frame.types.contains(first),
            };
            if hidden {
                return Origin::Hidden;
            }
            if let Some(bound) = frame.uses.iter().find(|u| &u.name == first) {
                let reached = match &bound.reached {
                    // an item, which later segments stay at
                    Some((module, Some(item))) => {
                        let at = &self.index.forest.nodes[*module];
                        vec![(at.file, at.module, item.clone())]
                    }
                    Some((module, None)) if !rest.is_empty() => self
                        .reach(*module, rest, false, value)
                        .into_iter()
                        .collect(),
                    _ => Vec::new(),
                };
                return Origin::Block {
                    reached,
                    line: bound.line,
                };
            }
            for &(module, line) in &frame.globs {
                if let Some(found) = self.reach(module, segments, false, value) {
                    return Origin::Block {
                        reached: vec![found],
                        line,
                    };
                }
            }
        }
        Origin::Module
    }

    /// The files and items `segments`, written here, reach.
    fn resolves(&self, segments: &[String], leading_colon: bool, value: bool) -> Vec<Item> {
        let origin = match leading_colon {
            true => Origin::Module,
            false => self.origin(segments, value),
        };
        match origin {
            Origin::Hidden => Vec::new(),
            Origin::Block { reached, .. } => reached,
            Origin::Module => self
                .nodes
                .iter()
                .filter_map(|&node| self.reach(node, segments, leading_colon, value))
                .collect(),
        }
    }

    fn reach(
        &self,
        node: usize,
        segments: &[String],
        leading_colon: bool,
        value: bool,
    ) -> Option<Item> {
        let ns = namespace(value);
        reach(self.index, self.resolver, node, segments, leading_colon, ns)
    }

    fn is_target(&self, (file, module, name): &Item) -> bool {
        *file == self.target.file
            && *name == self.target.name
            && self.target.module.is_none_or(|m| m == *module)
    }

    /// The segments of a path written here that name the target (for a
    /// method, its type's, which the method follows), with the namespace
    /// they are read in; `None` when the path does not name it.
    fn naming<'s>(
        &self,
        segments: &'s [String],
        leading_colon: bool,
        value: bool,
    ) -> Option<(&'s [String], bool)> {
        let (named, value) = match &self.target.member {
            Some(member) => match segments.split_last() {
                Some((last, prefix)) if last == member && !prefix.is_empty() => (prefix, false),
                _ => return None,
            },
            None => (segments, value),
        };
        self.resolves(named, leading_colon, value)
            .iter()
            .any(|item| self.is_target(item))
            .then_some((named, value))
    }

    /// The `use` declaration that a path naming the target with `named`
    /// goes through: a block's, else the module's that binds the first
    /// segment, else a glob of the module through which the path reaches
    /// the target.
    fn statement(&self, named: &[String], leading_colon: bool, value: bool) -> Option<ImportPlace> {
        let place = |line| ImportPlace {
            file: self.rel.to_owned(),
            line,
        };
        match (leading_colon, self.origin(named, value)) {
            (false, Origin::Block { line, .. }) => return Some(place(line)),
            (false, Origin::Module) => {}
            _ => return None,
        }
        let first = named.first()?;
        let module = &self.index.files[self.file].parsed.modules[self.module];
        let decls = || {
            module
                .uses
                .iter()
                .filter(|d| d.scope == archmap_core::Scope::Module)
        };
        if let Some(decl) = decls().find(|d| d.binds.as_deref() == Some(first)) {
            return Some(place(decl.line));
        }
        decls()
            .filter(|d| d.glob)
            .find(|d| {
                let path: Vec<String> = d.path.iter().chain(named).cloned().collect();
                self.nodes.iter().any(|&node| {
                    self.reach(node, &path, d.leading_colon, value)
                        .is_some_and(|item| self.is_target(&item))
                })
            })
            .map(|d| place(d.line))
    }

    /// `Self` in an `impl` of the target's type: `Self::m` for its method,
    /// and for the type itself `Self` built or matched (`Self { .. }`,
    /// `Self(..)`), never as a type, which every signature writes.
    fn through_self(&self, segments: &[String], role: UseRole) -> bool {
        if !self.in_impl {
            return false;
        }
        match &self.target.member {
            Some(member) => segments.len() == 2 && &segments[1] == member,
            None => segments.len() == 1 && role != UseRole::Type,
        }
    }

    /// A path written here with `role`; `value`: an expression's or a
    /// pattern's path, which a binding of its one segment hides. Whether
    /// it names the target.
    fn path(&mut self, path: &syn::Path, role: UseRole, value: bool) -> bool {
        let segments: Vec<String> = path.segments.iter().map(|s| name(&s.ident)).collect();
        let Some(first) = segments.first() else {
            return false;
        };
        let leading_colon = path.leading_colon.is_some();
        let named = match first == "Self" {
            true => self
                .through_self(&segments, role)
                .then_some((&segments[..], value)),
            false => self.naming(&segments, leading_colon, value),
        };
        let Some((named, value)) = named else {
            return false;
        };
        // the segment that names it: the method, else the item
        let wanted = self.target.member.as_ref().unwrap_or(&self.target.name);
        let at = path
            .segments
            .iter()
            .find(|s| &name(&s.ident) == wanted)
            .map_or_else(|| path.segments[0].ident.span(), |s| s.ident.span());
        // the item as a qualifier of something else (`Edge::new` for `Edge`):
        // the path without its last segment reaches it too
        let qualifier = self.target.member.is_none()
            && first != "Self"
            && segments.len() > 1
            && self
                .naming(&segments[..segments.len() - 1], leading_colon, false)
                .is_some();
        let role = match qualifier {
            true => UseRole::Read,
            false => role,
        };
        let statement = match first == "Self" {
            true => None,
            false => self.statement(named, leading_colon, value),
        };
        self.push(at, role, Some(segments.join("::")), statement);
        true
    }

    /// The generic arguments of a path's segments, which hold types of
    /// their own.
    fn arguments(&mut self, path: &syn::Path) {
        for segment in &path.segments {
            self.visit_path_arguments(&segment.arguments);
        }
    }

    /// A path in an expression: `<T>::m` is read as `T::m`, and the type of
    /// `<T as Trait>::m` as a type.
    fn expr_path(&mut self, p: &syn::ExprPath, role: UseRole) {
        let Some(q) = &p.qself else {
            self.path(&p.path, role, true);
            self.arguments(&p.path);
            return;
        };
        match (&*q.ty, q.position) {
            (syn::Type::Path(t), 0) if t.qself.is_none() => {
                let mut full = t.path.clone();
                full.segments.extend(p.path.segments.iter().cloned());
                self.path(&full, role, true);
                self.arguments(&full);
            }
            _ => {
                self.visit_type(&q.ty);
                if q.position > 0 {
                    self.path(&p.path, role, true);
                }
                self.arguments(&p.path);
            }
        }
    }

    fn push(
        &mut self,
        at: proc_macro2::Span,
        role: UseRole,
        binding: Option<String>,
        statement: Option<ImportPlace>,
    ) {
        let start = at.start();
        let evidence = Evidence::new(self.rel)
            .at_line(start.line as u32)
            .in_test(self.test())
            .type_only(role == UseRole::Type);
        self.uses.push(SymbolUse {
            evidence,
            column: start.column as u32 + 1,
            role,
            binding,
            statement,
        });
    }

    fn scoped(&mut self, frame: Frame, walk: impl FnOnce(&mut Self)) {
        self.frames.push(frame);
        walk(self);
        self.frames.pop();
    }

    /// A scope that `locals` are bound in.
    fn binding_in(&mut self, locals: BTreeSet<String>, walk: impl FnOnce(&mut Self)) {
        let frame = Frame {
            locals,
            ..Frame::default()
        };
        self.scoped(frame, walk);
    }

    /// An item: its generic parameters hide names inside it.
    fn item(&mut self, generics: &syn::Generics, walk: impl FnOnce(&mut Self)) {
        let mut frame = Frame::default();
        for param in &generics.params {
            match param {
                syn::GenericParam::Type(t) => {
                    frame.types.insert(name(&t.ident));
                }
                syn::GenericParam::Const(c) => {
                    frame.values.insert(name(&c.ident));
                }
                syn::GenericParam::Lifetime(_) => {}
            }
        }
        self.scoped(frame, walk);
    }

    /// A function: its parameters are bound in its body.
    fn function(&mut self, sig: &syn::Signature, block: Option<&syn::Block>) {
        self.item(&sig.generics, |w| {
            w.visit_generics(&sig.generics);
            let mut params = BTreeSet::new();
            for input in &sig.inputs {
                match input {
                    syn::FnArg::Typed(t) => {
                        params.extend(w.pattern(&t.pat));
                        w.visit_type(&t.ty);
                    }
                    syn::FnArg::Receiver(r) => w.visit_type(&r.ty),
                }
            }
            w.visit_return_type(&sig.output);
            if let Some(block) = block {
                w.binding_in(params, |w| w.visit_block(block));
            }
        });
    }

    /// Read a pattern: the paths it names, and the names it binds.
    fn pattern(&mut self, pat: &syn::Pat) -> BTreeSet<String> {
        let outer = self.binding.replace(BTreeSet::new());
        self.visit_pat(pat);
        std::mem::replace(&mut self.binding, outer).unwrap_or_default()
    }

    fn tested(&mut self, attrs: &[syn::Attribute], walk: impl FnOnce(&mut Self)) {
        let test = self.tests.last().copied().unwrap_or(false) || cfg_test(attrs);
        self.tests.push(test);
        walk(self);
        self.tests.pop();
    }

    /// What a block's items and `use` declarations make visible in it.
    fn block_frame(&self, block: &syn::Block) -> Frame {
        let mut frame = Frame::default();
        for statement in &block.stmts {
            let syn::Stmt::Item(item) = statement else {
                continue;
            };
            let (values, types) = (&mut frame.values, &mut frame.types);
            match item {
                syn::Item::Use(u) => {
                    let line = u.use_token.span.start().line as u32;
                    for decl in use_decls(u, line, archmap_core::Scope::Local, false) {
                        let reached = self.nodes.iter().find_map(|&node| {
                            match self.resolver.resolve(node, &decl) {
                                Resolved::Module { node, name, .. } => Some((node, name)),
                                _ => None,
                            }
                        });
                        match (decl.binds, decl.glob, reached) {
                            (_, true, Some((module, None))) => frame.globs.push((module, line)),
                            (Some(name), false, reached) => frame.uses.push(BlockUse {
                                name,
                                reached,
                                line,
                            }),
                            _ => {}
                        }
                    }
                }
                syn::Item::Fn(f) => {
                    values.insert(name(&f.sig.ident));
                }
                syn::Item::Const(c) => {
                    values.insert(name(&c.ident));
                }
                syn::Item::Static(s) => {
                    values.insert(name(&s.ident));
                }
                // a tuple or unit struct is its constructor's name too
                syn::Item::Struct(s) => {
                    types.insert(name(&s.ident));
                    if !matches!(s.fields, syn::Fields::Named(_)) {
                        values.insert(name(&s.ident));
                    }
                }
                syn::Item::Enum(e) => {
                    types.insert(name(&e.ident));
                }
                syn::Item::Union(u) => {
                    types.insert(name(&u.ident));
                }
                syn::Item::Type(t) => {
                    types.insert(name(&t.ident));
                }
                syn::Item::Trait(t) => {
                    types.insert(name(&t.ident));
                }
                syn::Item::Mod(m) => {
                    types.insert(name(&m.ident));
                }
                _ => {}
            }
        }
        frame
    }
}

impl<'ast> Visit<'ast> for Walker<'_> {
    fn visit_item_use(&mut self, _: &'ast syn::ItemUse) {}

    fn visit_item_mod(&mut self, m: &'ast syn::ItemMod) {
        let Some((_, items)) = &m.content else {
            return;
        };
        let facts = &self.index.files[self.file].parsed.modules[self.module];
        let Some(&inner) = facts.inline.get(&name(&m.ident)) else {
            return;
        };
        let (module, nodes) = (self.module, std::mem::take(&mut self.nodes));
        self.module = inner;
        self.nodes = nodes_of(self.index, self.file, inner);
        let frames = std::mem::replace(&mut self.frames, vec![Frame::default()]);
        let in_impl = std::mem::replace(&mut self.in_impl, false);
        self.tested(&m.attrs, |w| items.iter().for_each(|i| w.visit_item(i)));
        self.in_impl = in_impl;
        self.frames = frames;
        self.module = module;
        self.nodes = nodes;
    }

    fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
        self.tested(&f.attrs, |w| w.function(&f.sig, Some(&f.block)));
    }

    fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
        self.tested(&f.attrs, |w| w.function(&f.sig, Some(&f.block)));
    }

    fn visit_trait_item_fn(&mut self, f: &'ast syn::TraitItemFn) {
        self.tested(&f.attrs, |w| w.function(&f.sig, f.default.as_ref()));
    }

    fn visit_item_impl(&mut self, imp: &'ast syn::ItemImpl) {
        self.tested(&imp.attrs, |w| {
            w.item(&imp.generics, |w| {
                let of_target = match &*imp.self_ty {
                    syn::Type::Path(p) if p.qself.is_none() => {
                        let segments: Vec<String> =
                            p.path.segments.iter().map(|s| name(&s.ident)).collect();
                        let leading = p.path.leading_colon.is_some();
                        w.resolves(&segments, leading, false)
                            .iter()
                            .any(|item| w.is_target(item))
                    }
                    _ => false,
                };
                // `Self` is the type in an impl of a trait for it too
                let outer = std::mem::replace(&mut w.in_impl, of_target);
                visit::visit_item_impl(w, imp);
                w.in_impl = outer;
            })
        });
    }

    fn visit_item_trait(&mut self, t: &'ast syn::ItemTrait) {
        self.tested(&t.attrs, |w| {
            w.item(&t.generics, |w| {
                let outer = std::mem::replace(&mut w.in_impl, false);
                visit::visit_item_trait(w, t);
                w.in_impl = outer;
            })
        });
    }

    fn visit_block(&mut self, block: &'ast syn::Block) {
        let frame = self.block_frame(block);
        self.scoped(frame, |w| visit::visit_block(w, block));
    }

    fn visit_local(&mut self, local: &'ast syn::Local) {
        if let Some(init) = &local.init {
            self.visit_expr(&init.expr);
            if let Some((_, diverge)) = &init.diverge {
                self.visit_expr(diverge);
            }
        }
        // bound after the statement, `let ... else` too
        let names = self.pattern(&local.pat);
        if let Some(frame) = self.frames.last_mut() {
            frame.locals.extend(names);
        }
    }

    fn visit_expr_closure(&mut self, c: &'ast syn::ExprClosure) {
        let mut params = BTreeSet::new();
        for input in &c.inputs {
            params.extend(self.pattern(input));
        }
        self.visit_return_type(&c.output);
        self.binding_in(params, |w| w.visit_expr(&c.body));
    }

    fn visit_arm(&mut self, arm: &'ast syn::Arm) {
        let names = self.pattern(&arm.pat);
        self.binding_in(names, |w| {
            if let Some((_, guard)) = &arm.guard {
                w.visit_expr(guard);
            }
            w.visit_expr(&arm.body);
        });
    }

    fn visit_expr_for_loop(&mut self, f: &'ast syn::ExprForLoop) {
        self.visit_expr(&f.expr);
        let names = self.pattern(&f.pat);
        self.binding_in(names, |w| w.visit_block(&f.body));
    }

    // the bindings of `if let` and `while let` live in the rest of the
    // condition and in the branch, never after it
    fn visit_expr_if(&mut self, e: &'ast syn::ExprIf) {
        self.binding_in(BTreeSet::new(), |w| {
            w.visit_expr(&e.cond);
            w.visit_block(&e.then_branch);
        });
        if let Some((_, other)) = &e.else_branch {
            self.visit_expr(other);
        }
    }

    fn visit_expr_while(&mut self, e: &'ast syn::ExprWhile) {
        self.binding_in(BTreeSet::new(), |w| {
            w.visit_expr(&e.cond);
            w.visit_block(&e.body);
        });
    }

    fn visit_expr_let(&mut self, l: &'ast syn::ExprLet) {
        self.visit_expr(&l.expr);
        let names = self.pattern(&l.pat);
        if let Some(frame) = self.frames.last_mut() {
            frame.locals.extend(names);
        }
    }

    fn visit_pat_ident(&mut self, p: &'ast syn::PatIdent) {
        // a constant or a unit struct makes a plain name a path pattern
        let plain = p.by_ref.is_none() && p.mutability.is_none() && p.subpat.is_none();
        if plain && self.target.pattern {
            let path = syn::Path::from(p.ident.clone());
            if self.path(&path, UseRole::Read, true) {
                return;
            }
        }
        if let Some(names) = &mut self.binding {
            names.insert(name(&p.ident));
        }
        if let Some((_, sub)) = &p.subpat {
            self.visit_pat(sub);
        }
    }

    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        match &*call.func {
            syn::Expr::Path(p) => self.expr_path(p, UseRole::Call),
            other => self.visit_expr(other),
        }
        call.args.iter().for_each(|a| self.visit_expr(a));
    }

    fn visit_expr_method_call(&mut self, m: &'ast syn::ExprMethodCall) {
        let on_self = matches!(&*m.receiver, syn::Expr::Path(p) if p.path.is_ident("self"));
        if on_self
            && self.in_impl
            && self.target.member.as_deref() == Some(name(&m.method).as_str())
        {
            let binding = Some(format!("self.{}", m.method));
            self.push(m.method.span(), UseRole::Call, binding, None);
        }
        visit::visit_expr_method_call(self, m);
    }

    fn visit_expr_path(&mut self, p: &'ast syn::ExprPath) {
        self.expr_path(p, UseRole::Read);
    }

    fn visit_expr_struct(&mut self, s: &'ast syn::ExprStruct) {
        match &s.qself {
            None => {
                self.path(&s.path, UseRole::New, false);
            }
            Some(q) => self.visit_type(&q.ty),
        }
        self.arguments(&s.path);
        s.fields.iter().for_each(|f| self.visit_expr(&f.expr));
        if let Some(rest) = &s.rest {
            self.visit_expr(rest);
        }
    }

    fn visit_type_path(&mut self, t: &'ast syn::TypePath) {
        match &t.qself {
            None => {
                self.path(&t.path, UseRole::Type, false);
            }
            Some(q) => {
                self.visit_type(&q.ty);
                if q.position > 0 {
                    self.path(&t.path, UseRole::Type, false);
                }
            }
        }
        self.arguments(&t.path);
    }

    fn visit_pat_tuple_struct(&mut self, p: &'ast syn::PatTupleStruct) {
        match &p.qself {
            None => {
                self.path(&p.path, UseRole::Read, false);
            }
            Some(q) => self.visit_type(&q.ty),
        }
        self.arguments(&p.path);
        p.elems.iter().for_each(|e| self.visit_pat(e));
    }

    fn visit_pat_struct(&mut self, p: &'ast syn::PatStruct) {
        match &p.qself {
            None => {
                self.path(&p.path, UseRole::Read, false);
            }
            Some(q) => self.visit_type(&q.ty),
        }
        self.arguments(&p.path);
        p.fields.iter().for_each(|f| self.visit_pat(&f.pat));
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        let last = mac.path.segments.last();
        let code = last.is_none_or(|s| !NOT_CODE.contains(&s.ident.to_string().as_str()));
        if code {
            visit_arguments(&mac.tokens, self);
        }
    }

    fn visit_attribute(&mut self, _: &'ast syn::Attribute) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_receiver_is_self_first() {
        for (signature, yes) in [
            ("pub fn weight(&self) -> u32", true),
            ("pub fn take(self) -> u32", true),
            ("pub fn set(&mut self, n: u32)", true),
            ("pub fn get<'a>(&'a self) -> &'a str", true),
            ("pub fn boxed(self: Box<Self>)", true),
            // a bound in the generics holds parentheses of its own
            ("pub fn map<F: Fn(u32) -> u32>(&self, f: F) -> u32", true),
            ("pub fn with<F: FnOnce(&Self) -> u32>(f: F) -> u32", false),
            ("pub fn new(from: u32) -> Self", false),
            ("pub fn parse(text: &'a str) -> Self", false),
            ("pub fn none() -> Self", false),
        ] {
            assert_eq!(takes_self(signature), yes, "{signature}");
        }
    }
}
