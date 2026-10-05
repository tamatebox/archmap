//! Where a Python symbol is used, read on demand for one symbol: the file
//! that defines it and the files whose statements import it, parsed with
//! Ruff's parser. Each name is resolved with Python's scope rules: a name
//! bound anywhere in a function is local to all of it unless declared
//! `global` or `nonlocal`; a class body's names are not seen by the
//! functions in it; comprehensions, lambdas and type parameters have scopes
//! of their own; defaults, annotations, decorators and a comprehension's
//! first iterable belong to the scope around them.
//!
//! A statement binds a name that holds the symbol, its class (for a method)
//! or a module on the way, and the attributes from that name to the symbol
//! are its path: `[]` for `from charge import pay`, `["pay"]` for `import
//! charge`, `["billing", "charge", "pay"]` for `import store.billing.charge`.
//! A binding that nothing uses is a negative fact only where nothing may
//! reach it unseen: a name bound again in its scope, a syntax error in the
//! file or a name reached by a computed one end its statement as unread.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use archmap_core::{
    Evidence, ImportPlace, Symbol, SymbolKind, SymbolUse, SymbolUses, Unread, UnreadReason,
    UseRole, WHOLE_MODULE,
};
use ruff_python_ast::visitor::{self, Visitor};
use ruff_python_ast::{self as ast, Expr, ExprContext, ModModule, PySourceType, Stmt};
use ruff_python_parser::{parse_string_annotation, parse_unchecked_source, Parsed};
use ruff_text_size::{Ranged, TextRange, TextSize};

use super::source::{scan_source, DunderAll};
use crate::lines::Lines;

/// A file larger than this is not parsed: recovering from errors in a huge
/// generated file can be slow.
const MAX_BYTES: usize = 4 * 1024 * 1024;

/// What the pass reads for one symbol.
pub(crate) struct Request<'g> {
    pub root: &'g Path,
    pub symbol: &'g Symbol,
    /// The statements `query` lists for it, and the others on their lines
    /// that load the same file.
    pub statements: Vec<&'g Evidence>,
    /// The files some statement of the scan imports from another file,
    /// with the names those statements take from each (`*` for the
    /// module whole).
    pub imported: BTreeMap<&'g str, BTreeSet<&'g str>>,
    /// The names under which each file offers the symbol, through the
    /// barrels that pass it on.
    pub offered: BTreeMap<String, BTreeSet<String>>,
}

/// What a use must reach.
struct Target {
    /// The file that defines the symbol.
    file: String,
    /// Its name, or a method's class and name: `["Wallet", "open"]`.
    tail: Vec<String>,
    /// The symbol is a class: calling it makes one.
    class: bool,
    /// How a dotted string names it from its module: `charge.pay`.
    dotted: Option<String>,
    /// The line of its definition.
    line: Option<u32>,
}

/// A name that leads to the symbol.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Binding {
    /// The line of the statement that binds it; none for the defining
    /// file's own name and a method's `self`.
    line: Option<u32>,
    name: String,
    /// Where it is bound: `None` for the module, else where the function or
    /// class whose body binds it starts.
    scope: Option<u32>,
    /// The attributes from the bound value to the symbol.
    path: Vec<String>,
    /// How many values along the path are modules, the bound one first: a
    /// chain that stops at one of them uses the module as a value.
    modules: usize,
    /// Bound by a star import, which writes no name.
    star: bool,
    /// The first parameter of a method of the symbol's class.
    instance: bool,
}

impl Binding {
    fn new(line: u32, name: &str, scope: Option<u32>, path: Vec<String>, modules: usize) -> Self {
        Binding {
            line: Some(line),
            name: name.to_owned(),
            scope,
            path,
            modules,
            star: false,
            instance: false,
        }
    }
}

/// What one statement binds that leads to the symbol.
#[derive(Default)]
struct Bound {
    bindings: Vec<Binding>,
    /// A star of a module whose `__all__` code builds: it may bind the
    /// name.
    may_bind: bool,
    /// The calls that load the module by a literal name.
    calls: Vec<LoadCall>,
    /// It binds a name its scope declares `nonlocal`, which a function
    /// around binds too.
    rebound: bool,
}

/// A call that loads a module by a literal name (`import_module("a.b")`),
/// whose value leads to the symbol as `binding` says.
#[derive(Debug, Clone)]
struct LoadCall {
    range: TextRange,
    binding: Binding,
    /// A statement binds its value to a name or drops it, so the value goes
    /// nowhere else.
    held: bool,
}

/// Read the uses of the Python symbol `request` names into `out`.
pub(crate) fn read(request: &Request, out: &mut SymbolUses) {
    let symbol = request.symbol;
    let Some(location) = symbol.location() else {
        return;
    };
    let tail: Vec<String> = match symbol.name.split_once('.') {
        Some((class, member)) => vec![class.to_owned(), member.to_owned()],
        None => vec![symbol.name.clone()],
    };
    let target = Target {
        file: location.file.clone(),
        class: symbol.kind == SymbolKind::Struct,
        dotted: module_name(&location.file).map(|module| format!("{module}.{}", tail.join("."))),
        tail,
        line: location.line,
    };
    // the statements by file and line, and the defining file
    let mut files: BTreeMap<&str, BTreeMap<u32, Vec<&Evidence>>> = BTreeMap::new();
    files.entry(location.file.as_str()).or_default();
    for evidence in &request.statements {
        if let Some(line) = evidence.line {
            files
                .entry(evidence.file.as_str())
                .or_default()
                .entry(line)
                .or_default()
                .push(evidence);
        }
    }
    let pass = Pass {
        root: request.root,
        target: &target,
        imported: &request.imported,
        offered: &request.offered,
    };
    for (&file, statements) in &files {
        let defining = file == target.file;
        let test = match defining {
            true => location.test,
            false => statements.values().flatten().any(|e| e.test),
        };
        match File::read(request.root, file, test) {
            Ok(read) => pass.file(&read, defining, statements, out),
            Err(reason) => out.unread.push(Unread {
                file: file.to_owned(),
                line: None,
                reason,
            }),
        }
    }
    // a name written as the symbol's own is no other name
    for found in &mut out.uses {
        if found.binding.as_deref() == Some(symbol.name.as_str()) {
            found.binding = None;
        }
    }
}

struct Pass<'t> {
    root: &'t Path,
    target: &'t Target,
    imported: &'t BTreeMap<&'t str, BTreeSet<&'t str>>,
    offered: &'t BTreeMap<String, BTreeSet<String>>,
}

impl Pass<'_> {
    /// One file: what its statements bind, the uses through them (and in
    /// the defining file through its own name), and how each statement
    /// ends.
    fn file(
        &self,
        read: &File,
        defining: bool,
        statements: &BTreeMap<u32, Vec<&Evidence>>,
        out: &mut SymbolUses,
    ) {
        let target = self.target;
        let mut bindings = Vec::new();
        let mut calls = Vec::new();
        let mut ended: BTreeSet<u32> = BTreeSet::new();
        let mut may_bind: BTreeSet<u32> = BTreeSet::new();
        let mut nonlocal: BTreeSet<u32> = BTreeSet::new();
        for (&line, evidence) in statements {
            match self.bindings(read, line, evidence) {
                Ok(bound) => {
                    if bound.may_bind {
                        may_bind.insert(line);
                    }
                    if bound.rebound {
                        nonlocal.insert(line);
                    }
                    bindings.extend(bound.bindings);
                    calls.extend(bound.calls);
                }
                Err(reason) => {
                    ended.insert(line);
                    out.unread.push(Unread {
                        file: read.path.clone(),
                        line: Some(line),
                        reason,
                    });
                }
            }
        }
        if defining {
            bindings.push(Binding {
                line: None,
                name: target.tail[0].clone(),
                scope: None,
                path: target.tail[1..].to_vec(),
                modules: 0,
                star: false,
                instance: false,
            });
            bindings.extend(instance_bindings(read.module(), target));
        }
        let mut walker = Walker::new(read, target, bindings, calls);
        walker.module(read.module());
        walker.rebound.extend(nonlocal);
        if walker.own_rebound {
            // its own code may read another binding of the name
            out.unread.push(Unread {
                file: read.path.clone(),
                line: target.line,
                reason: UnreadReason::Rebound,
            });
        }
        // every statement ends in one list
        for (&line, evidence) in statements {
            if ended.contains(&line)
                || walker.used.contains(&line)
                || walker.escaped.contains(&line)
                || walker.strings
            {
                continue;
            }
            let first = evidence[0];
            let unread = |reason| Unread {
                file: read.path.clone(),
                line: Some(line),
                reason,
            };
            // a module-level name its `__all__` lists, or that a package's
            // `__init__.py` binds, is offered to whoever imports the module,
            // one a star import binds, to a module that imports it, and any
            // to a statement of another file that takes it from the module,
            // by name or whole (a star, or the module as a value)
            let package = read.path == "__init__.py" || read.path.ends_with("/__init__.py");
            let taken = self.imported.get(read.path.as_str());
            let takes = |name: &str| taken.is_some_and(|names| names.contains(name));
            let offers = walker.bindings.iter().any(|b| {
                b.line == Some(line)
                    && b.scope.is_none()
                    && (package
                        || read.lists(&b.name)
                        || takes(&b.name)
                        || takes(WHOLE_MODULE)
                        || b.star && taken.is_some() && read.exports(&b.name))
            });
            if offers || evidence.iter().any(|e| e.passes_on()) {
                out.passed_on.push(first.clone());
            } else if walker.rebound.contains(&line) {
                out.unread.push(unread(UnreadReason::Rebound));
            } else if read.syntax_error {
                out.unread.push(unread(UnreadReason::ParseError));
            } else if walker.dynamic || may_bind.contains(&line) {
                out.unread.push(unread(UnreadReason::DynamicAccess));
            } else if target.tail.len() == 2 {
                // values or subclasses of the class may reach the method
                out.values.push(first.clone());
            } else {
                out.unused.push(first.clone());
            }
        }
        out.uses.extend(walker.uses);
        out.escapes.extend(walker.escapes);
        out.subclasses.extend(walker.subclasses);
    }

    /// What the import statements starting on `line` bind that leads to
    /// the symbol, as `evidence`, the statement's evidence for the files
    /// on the way, says it does.
    fn bindings(
        &self,
        read: &File,
        line: u32,
        evidence: &[&Evidence],
    ) -> Result<Bound, UnreadReason> {
        let target = self.target;
        let mut statements = Vec::new();
        find_imports(
            &read.module().body,
            &Scope::default(),
            &read.lines,
            line,
            &mut statements,
        );
        let mut finder = CallFinder::new(&read.lines, line);
        finder.visit_body(&read.module().body);
        if statements.is_empty() && finder.found.is_empty() {
            return Err(UnreadReason::StatementNotFound);
        }
        let rest = &target.tail[1..];
        // the evidence for the symbol's name, apart from the other names a
        // statement takes from the same file
        let evidence: Vec<&Evidence> = evidence
            .iter()
            .copied()
            .filter(|e| e.names.contains(&target.tail[0]) || e.names.contains(WHOLE_MODULE))
            .collect();
        let mut bound = Bound::default();
        let mut star = false;
        for (statement, scope) in statements {
            // a name binds where its scope's declarations say
            let bind = |bound: &mut Bound, name: &str, path: Vec<String>, modules: usize| {
                bound.rebound |= scope.nonlocals.contains(name);
                let binding = Binding::new(line, name, scope.binds(name), path, modules);
                bound.bindings.push(binding);
            };
            match statement {
                Stmt::ImportFrom(from) => {
                    let named = from.names.iter().filter(|a| &a.name != "*").count();
                    for alias in &from.names {
                        let taken = alias.name.as_str();
                        let name = alias.asname.as_ref().unwrap_or(&alias.name).as_str();
                        if taken == "*" {
                            star = true;
                            self.star(line, &evidence, &mut bound);
                            continue;
                        }
                        for &e in &evidence {
                            let offered = self.offered(e);
                            if offered.as_deref() == Some(taken) {
                                bind(&mut bound, name, rest.to_vec(), 0);
                            } else if module_name(loaded(e)).as_deref() == Some(taken) {
                                // a submodule
                                if let Some(offered) = offered {
                                    bind(&mut bound, name, [&[offered], rest].concat(), 1);
                                }
                            } else if offered.is_none() && named == 1 {
                                // the one name it takes leads to the symbol
                                // under a name given on the way
                                bind(&mut bound, name, rest.to_vec(), 0);
                            }
                        }
                    }
                }
                Stmt::Import(import) => {
                    for alias in &import.names {
                        let parts: Vec<&str> = alias.name.split('.').collect();
                        for &e in &evidence {
                            if module_name(loaded(e)).as_deref() != parts.last().copied() {
                                continue;
                            }
                            let Some(offered) = self.offered(e) else {
                                continue;
                            };
                            let (name, mut path): (&str, Vec<String>) = match &alias.asname {
                                Some(asname) => (asname.as_str(), Vec::new()),
                                None => (
                                    parts[0],
                                    parts[1..].iter().map(|p| (*p).to_owned()).collect(),
                                ),
                            };
                            let modules = path.len() + 1;
                            path.push(offered);
                            path.extend(rest.iter().cloned());
                            bind(&mut bound, name, path, modules);
                        }
                    }
                }
                _ => {}
            }
        }
        // a call returns the module it names (`import_module`), or the
        // package its name starts with, as `import a.b` binds `a`
        // (`__import__`)
        for found in &finder.found {
            let parts: Vec<&str> = found.module.split('.').collect();
            for &e in &evidence {
                if module_name(loaded(e)).as_deref() != parts.last().copied() {
                    continue;
                }
                let Some(offered) = self.offered(e) else {
                    continue;
                };
                let mut path: Vec<String> = match found.top {
                    true => parts[1..].iter().map(|p| (*p).to_owned()).collect(),
                    false => Vec::new(),
                };
                let modules = path.len() + 1;
                path.push(offered);
                path.extend(rest.iter().cloned());
                let name = match found.holder {
                    Holder::Name(name) => name,
                    Holder::Dropped | Holder::Nothing => "",
                };
                let binding = Binding::new(line, name, found.scope, path, modules);
                bound.rebound |= found.nonlocal;
                if !name.is_empty() {
                    bound.bindings.push(binding.clone());
                }
                bound.calls.push(LoadCall {
                    range: found.range,
                    binding,
                    held: found.holder != Holder::Nothing,
                });
            }
        }
        bound.bindings.sort();
        bound.bindings.dedup();
        if bound.bindings.is_empty() && bound.calls.is_empty() && !star {
            return Err(UnreadReason::NoPath);
        }
        Ok(bound)
    }

    /// The binding a star import makes of the symbol's name (or its
    /// class's): from the file it loads whole, when that file's star takes
    /// the name, or may.
    fn star(&self, line: u32, evidence: &[&Evidence], bound: &mut Bound) {
        let name = &self.target.tail[0];
        for e in evidence {
            if e.via().is_some() || !e.names.contains(WHOLE_MODULE) {
                continue;
            }
            let Some(loaded) = e.target.as_deref() else {
                continue;
            };
            let source = std::fs::read_to_string(self.root.join(loaded))
                .ok()
                .map(|text| scan_source(&text));
            // a barrel offers it under the names it passes it on as
            // (`from .impl import amount as total`)
            let offered: Vec<&String> = match self.offered.get(loaded) {
                Some(names) if loaded != self.target.file => names.iter().collect(),
                _ => vec![name],
            };
            let all = source.map(|source| source.all);
            for offered in offered {
                let takes = match &all {
                    Some(Some(DunderAll::Listed(names))) => Some(names.contains(offered)),
                    Some(None) => Some(!offered.starts_with('_')),
                    Some(Some(DunderAll::Built)) | None => None,
                };
                if takes == Some(false) {
                    continue;
                }
                bound.may_bind |= takes.is_none();
                bound.bindings.push(Binding {
                    star: true,
                    ..Binding::new(line, offered, None, self.target.tail[1..].to_vec(), 0)
                });
            }
        }
    }

    /// The name under which the file a statement's evidence loads offers
    /// the symbol (or its class): its own, unless that file binds it from
    /// another under a name of its own (`from .charge import pay as
    /// settle`); `None` when its statement cannot tell.
    fn offered(&self, evidence: &Evidence) -> Option<String> {
        let name = &self.target.tail[0];
        let Some(place) = evidence.via() else {
            return Some(name.clone());
        };
        let (file, line) = place.rsplit_once(':')?;
        let line: u32 = line.parse().ok()?;
        let text = std::fs::read_to_string(self.root.join(file)).ok()?;
        let (mut offered, mut bound) = (BTreeSet::new(), BTreeSet::new());
        for import in scan_source(&text).imports.iter().filter(|i| i.line == line) {
            if import.names.iter().any(|n| n == WHOLE_MODULE) {
                offered.insert(name.clone());
            }
            for (taken, binds) in import.names.iter().zip(&import.bound) {
                bound.insert(binds.clone());
                if taken == name {
                    offered.insert(binds.clone());
                }
            }
        }
        let one = |set: BTreeSet<String>| match set.len() {
            1 => set.into_iter().next(),
            _ => None,
        };
        match offered.is_empty() {
            true => one(bound),
            false => one(offered),
        }
    }
}

/// The file whose names a statement's evidence takes: the first file a
/// walk through re-exports passed, else the file it loads.
fn loaded(evidence: &Evidence) -> &str {
    evidence
        .via()
        .and_then(|place| place.rsplit_once(':'))
        .map(|(file, _)| file)
        .or(evidence.target.as_deref())
        .unwrap_or("")
}

/// The name an import gives a module's file: its stem, or for a package's
/// `__init__.py` its directory's name.
fn module_name(file: &str) -> Option<String> {
    let path = Path::new(file);
    let stem = path.file_stem()?.to_str()?;
    match stem {
        "__init__" => path.parent()?.file_name()?.to_str().map(str::to_owned),
        _ => Some(stem.to_owned()),
    }
}

/// `self` and `cls` in the methods of the class that declares a method:
/// `self.open()` calls it. Not in a static method, whose first parameter
/// is no instance.
fn instance_bindings(module: &ModModule, target: &Target) -> Vec<Binding> {
    let [class, member] = target.tail.as_slice() else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for statement in &module.body {
        let Stmt::ClassDef(c) = statement else {
            continue;
        };
        if c.name.as_str() != class {
            continue;
        }
        for item in &c.body {
            let Stmt::FunctionDef(f) = item else {
                continue;
            };
            let is_static = f
                .decorator_list
                .iter()
                .any(|d| matches!(&d.expression, Expr::Name(n) if n.id.as_str() == "staticmethod"));
            let parameters = &f.parameters;
            let first = parameters.posonlyargs.first().or(parameters.args.first());
            if let (false, Some(first)) = (is_static, first) {
                found.push(Binding {
                    line: None,
                    name: first.parameter.name.to_string(),
                    scope: Some(f.range.start().to_u32()),
                    path: vec![member.clone()],
                    modules: 0,
                    star: false,
                    instance: true,
                });
            }
        }
    }
    found
}

/// A Python file parsed for its uses.
struct File {
    path: String,
    text: String,
    lines: Lines,
    parsed: Parsed<ModModule>,
    test: bool,
    /// The parser recovered from an error: uses in the broken part are lost.
    syntax_error: bool,
    /// Its `__all__`, as the scan reads it.
    all: Option<DunderAll>,
}

impl File {
    fn read(root: &Path, file: &str, test: bool) -> Result<File, UnreadReason> {
        let text = std::fs::read_to_string(root.join(file)).map_err(|_| UnreadReason::FileGone)?;
        if text.len() > MAX_BYTES {
            return Err(UnreadReason::TooLarge);
        }
        let kind = match file.ends_with(".pyi") {
            true => PySourceType::Stub,
            false => PySourceType::Python,
        };
        let parsed = parse_unchecked_source(&text, kind);
        Ok(File {
            path: file.to_owned(),
            lines: Lines::new(&text),
            syntax_error: !parsed.errors().is_empty(),
            all: scan_source(&text).all,
            parsed,
            text,
            test,
        })
    }

    fn module(&self) -> &ModModule {
        self.parsed.syntax()
    }

    /// Whether its `__all__` lists `name`, which it then passes on.
    fn lists(&self, name: &str) -> bool {
        matches!(&self.all, Some(DunderAll::Listed(names)) if names.iter().any(|n| n == name))
    }

    /// Whether a star import of the module binds `name`: its `__all__`
    /// lists it, or without a literal one, it is public.
    fn exports(&self, name: &str) -> bool {
        match &self.all {
            Some(DunderAll::Listed(_)) => self.lists(name),
            Some(DunderAll::Built) | None => !name.starts_with('_'),
        }
    }
}

/// The scope a statement is in, which its names bind in unless it declares
/// them `global` or `nonlocal`.
#[derive(Debug, Clone, Default)]
struct Scope {
    /// `None` for the module, else where the function or class whose body
    /// it is starts.
    id: Option<u32>,
    globals: BTreeSet<String>,
    /// Names a function around binds, which Python requires of them.
    nonlocals: BTreeSet<String>,
}

impl Scope {
    /// The scope of the body of a function or class that starts at `id`.
    fn of(id: TextSize, body: &[Stmt]) -> Scope {
        let mut binder = Binder::default();
        binder.visit_body(body);
        Scope {
            id: Some(id.to_u32()),
            globals: binder.globals,
            nonlocals: binder.nonlocals,
        }
    }

    /// Where a statement in it binds `name`: the module for a name it
    /// declares `global`.
    fn binds(&self, name: &str) -> Option<u32> {
        match self.globals.contains(name) {
            true => None,
            false => self.id,
        }
    }
}

/// The import statements in `body` that start on `line`, each with the
/// scope it is in.
fn find_imports<'a>(
    body: &'a [Stmt],
    scope: &Scope,
    lines: &Lines,
    line: u32,
    out: &mut Vec<(&'a Stmt, Scope)>,
) {
    for statement in body {
        let mut inner =
            |body: &'a [Stmt], scope: &Scope| find_imports(body, scope, lines, line, out);
        match statement {
            Stmt::Import(_) | Stmt::ImportFrom(_) => {
                if lines.of(statement.start().to_usize()) == line {
                    out.push((statement, scope.clone()));
                }
            }
            Stmt::FunctionDef(f) => inner(&f.body, &Scope::of(f.range.start(), &f.body)),
            Stmt::ClassDef(c) => inner(&c.body, &Scope::of(c.range.start(), &c.body)),
            Stmt::If(s) => {
                inner(&s.body, scope);
                for clause in &s.elif_else_clauses {
                    inner(&clause.body, scope);
                }
            }
            Stmt::Try(s) => {
                inner(&s.body, scope);
                for ast::ExceptHandler::ExceptHandler(handler) in &s.handlers {
                    inner(&handler.body, scope);
                }
                inner(&s.orelse, scope);
                inner(&s.finalbody, scope);
            }
            Stmt::With(s) => inner(&s.body, scope),
            Stmt::For(s) => {
                inner(&s.body, scope);
                inner(&s.orelse, scope);
            }
            Stmt::While(s) => {
                inner(&s.body, scope);
                inner(&s.orelse, scope);
            }
            Stmt::Match(s) => {
                for case in &s.cases {
                    inner(&case.body, scope);
                }
            }
            _ => {}
        }
    }
}

/// What holds the value of a call that loads a module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Holder<'a> {
    /// A statement binds it to one name (`m = import_module("a")`).
    Name(&'a str),
    /// A statement drops it: the call stands alone.
    Dropped,
    /// Code uses it as a value: an attribute, an argument, a return.
    Nothing,
}

/// A call that loads a module by a literal name, as the scan reads one.
struct FoundCall<'a> {
    range: TextRange,
    /// The module's absolute dotted name.
    module: String,
    /// It returns the package the name starts with (`__import__`).
    top: bool,
    /// Where the name that holds it binds, as for an import.
    scope: Option<u32>,
    /// That name is declared `nonlocal`.
    nonlocal: bool,
    holder: Holder<'a>,
}

/// The calls whose function name (`import_module`, `__import__`) starts on
/// `line` and that name a module by one string literal.
struct CallFinder<'a, 'l> {
    lines: &'l Lines,
    line: u32,
    scope: Scope,
    held: Vec<(TextRange, Holder<'a>)>,
    found: Vec<FoundCall<'a>>,
}

impl<'l> CallFinder<'_, 'l> {
    fn new(lines: &'l Lines, line: u32) -> Self {
        CallFinder {
            lines,
            line,
            scope: Scope::default(),
            held: Vec::new(),
            found: Vec::new(),
        }
    }
}

impl<'a> Visitor<'a> for CallFinder<'a, '_> {
    fn visit_stmt(&mut self, statement: &'a Stmt) {
        match statement {
            Stmt::Assign(a) => {
                if let ([Expr::Name(name)], Expr::Call(call)) = (a.targets.as_slice(), &*a.value) {
                    self.held
                        .push((call.range(), Holder::Name(name.id.as_str())));
                }
            }
            Stmt::AnnAssign(a) => {
                if let (Expr::Name(name), Some(Expr::Call(call))) = (&*a.target, a.value.as_deref())
                {
                    self.held
                        .push((call.range(), Holder::Name(name.id.as_str())));
                }
            }
            Stmt::Expr(e) => {
                if let Expr::Call(call) = &*e.value {
                    self.held.push((call.range(), Holder::Dropped));
                }
            }
            _ => {}
        }
        let inner = match statement {
            Stmt::FunctionDef(f) => Some(Scope::of(f.range.start(), &f.body)),
            Stmt::ClassDef(c) => Some(Scope::of(c.range.start(), &c.body)),
            _ => None,
        };
        match inner {
            Some(inner) => {
                let outer = std::mem::replace(&mut self.scope, inner);
                visitor::walk_stmt(self, statement);
                self.scope = outer;
            }
            None => visitor::walk_stmt(self, statement),
        }
    }

    fn visit_expr(&mut self, expr: &'a Expr) {
        if let Expr::Call(call) = expr {
            if let Some((module, top, at)) = loads_by_name(call) {
                if self.lines.of(at) == self.line {
                    let holder = self
                        .held
                        .iter()
                        .find(|(range, _)| *range == call.range())
                        .map_or(Holder::Nothing, |(_, holder)| *holder);
                    let name = match holder {
                        Holder::Name(name) => name,
                        Holder::Dropped | Holder::Nothing => "",
                    };
                    self.found.push(FoundCall {
                        range: call.range(),
                        module,
                        top,
                        scope: self.scope.binds(name),
                        nonlocal: self.scope.nonlocals.contains(name),
                        holder,
                    });
                }
            }
        }
        visitor::walk_expr(self, expr);
    }
}

/// The module a call loads by one string literal, as the scan reads it:
/// `import_module("a.b")`, `import_module(".b", package="a")` or
/// `import_module(".b", "a")`, and `__import__("a.b")` with no other
/// argument, which returns `a`; with where its function's name starts.
fn loads_by_name(call: &ast::ExprCall) -> Option<(String, bool, usize)> {
    let (callee, at) = match &*call.func {
        Expr::Name(name) => (name.id.as_str(), name.range.start()),
        Expr::Attribute(attribute) => (attribute.attr.as_str(), attribute.attr.range.start()),
        _ => return None,
    };
    let literal = |expr: &Expr| match expr {
        Expr::StringLiteral(string) => Some(string.value.to_str().to_owned()),
        _ => None,
    };
    let arguments = &call.arguments;
    let module = literal(arguments.args.first()?)?;
    let level = module.chars().take_while(|c| *c == '.').count();
    let package = match (callee, arguments.args.len(), &*arguments.keywords) {
        ("__import__", 1, []) if level == 0 => return Some((module, true, at.to_usize())),
        ("import_module", 1, []) => None,
        ("import_module", 2, []) => Some(literal(&arguments.args[1])?),
        ("import_module", 1, [keyword])
            if keyword.arg.as_ref().map(|a| a.as_str()) == Some("package") =>
        {
            Some(literal(&keyword.value)?)
        }
        _ => return None,
    };
    if level == 0 {
        return Some((module, false, at.to_usize()));
    }
    // a relative name, from the package: one dot is the package itself
    let package = package?;
    let mut parts: Vec<&str> = package.split('.').collect();
    for _ in 1..level {
        parts.pop()?;
    }
    if level < module.len() {
        parts.push(&module[level..]);
    }
    (!parts.is_empty()).then(|| (parts.join("."), false, at.to_usize()))
}

/// The names one scope binds, with how many places bind each, and its
/// `global` and `nonlocal` names. The bodies of the functions, classes,
/// lambdas and comprehensions inside it are scopes of their own, apart
/// from a walrus in a comprehension, which binds here.
#[derive(Default)]
struct Binder {
    sites: BTreeMap<String, usize>,
    /// Names that `import a.b` binds without `as`: every such statement
    /// binds the same package `a`, so they are one place.
    packages: BTreeSet<String>,
    /// Names of `@overload` stubs, which the definition after them binds
    /// again before code reads them: one place when nothing else binds
    /// them (a stub file).
    overloads: BTreeSet<String>,
    globals: BTreeSet<String>,
    nonlocals: BTreeSet<String>,
}

impl Binder {
    /// How many places bind each name.
    fn sites(mut self) -> BTreeMap<String, usize> {
        for name in self.overloads {
            self.sites.entry(name).or_insert(1);
        }
        self.sites
    }

    fn bind(&mut self, name: &str) {
        *self.sites.entry(name.to_owned()).or_default() += 1;
    }

    /// A comprehension's iterables and parts, without its own targets.
    fn comprehension(&mut self, generators: &[ast::Comprehension], parts: &[&Expr]) {
        for generator in generators {
            self.visit_expr(&generator.iter);
            for condition in &generator.ifs {
                self.visit_expr(condition);
            }
        }
        for part in parts {
            self.visit_expr(part);
        }
    }
}

impl<'a> Visitor<'a> for Binder {
    fn visit_stmt(&mut self, statement: &'a Stmt) {
        match statement {
            Stmt::FunctionDef(f) if is_overload(f) => {
                self.overloads.insert(f.name.to_string());
            }
            Stmt::FunctionDef(f) => self.bind(&f.name),
            Stmt::ClassDef(c) => self.bind(&c.name),
            Stmt::Import(import) => {
                for alias in &import.names {
                    match &alias.asname {
                        Some(name) => self.bind(name),
                        None => {
                            let package = alias.name.split('.').next().unwrap_or_default();
                            if self.packages.insert(package.to_owned()) {
                                self.bind(package);
                            }
                        }
                    }
                }
            }
            Stmt::ImportFrom(from) => {
                for alias in from.names.iter().filter(|a| &a.name != "*") {
                    self.bind(alias.asname.as_ref().unwrap_or(&alias.name));
                }
            }
            Stmt::Global(g) => self.globals.extend(g.names.iter().map(|n| n.to_string())),
            Stmt::Nonlocal(n) => self.nonlocals.extend(n.names.iter().map(|n| n.to_string())),
            _ => visitor::walk_stmt(self, statement),
        }
    }

    fn visit_expr(&mut self, expr: &'a Expr) {
        match expr {
            Expr::Name(name) if matches!(name.ctx, ExprContext::Store | ExprContext::Del) => {
                self.bind(&name.id)
            }
            Expr::Lambda(_) => {}
            Expr::ListComp(c) => self.comprehension(&c.generators, &[&c.elt]),
            Expr::SetComp(c) => self.comprehension(&c.generators, &[&c.elt]),
            Expr::Generator(c) => self.comprehension(&c.generators, &[&c.elt]),
            Expr::DictComp(c) => {
                let parts: Vec<&Expr> = c.key.iter().map(|k| &**k).chain([&*c.value]).collect();
                self.comprehension(&c.generators, &parts);
            }
            _ => visitor::walk_expr(self, expr),
        }
    }

    fn visit_except_handler(&mut self, handler: &'a ast::ExceptHandler) {
        let ast::ExceptHandler::ExceptHandler(h) = handler;
        if let Some(name) = &h.name {
            self.bind(name);
        }
        visitor::walk_except_handler(self, handler);
    }

    fn visit_pattern(&mut self, pattern: &'a ast::Pattern) {
        let name = match pattern {
            ast::Pattern::MatchAs(p) => p.name.as_ref(),
            ast::Pattern::MatchStar(p) => p.name.as_ref(),
            ast::Pattern::MatchMapping(p) => p.rest.as_ref(),
            _ => None,
        };
        if let Some(name) = name {
            self.bind(name);
        }
        visitor::walk_pattern(self, pattern);
    }
}

/// Module-level names that functions bind through `global`.
#[derive(Default)]
struct GlobalAssignments {
    sites: BTreeMap<String, usize>,
}

impl<'a> Visitor<'a> for GlobalAssignments {
    fn visit_stmt(&mut self, statement: &'a Stmt) {
        if let Stmt::FunctionDef(f) = statement {
            let mut binder = Binder::default();
            binder.visit_body(&f.body);
            for name in &binder.globals {
                if let Some(&n) = binder.sites.get(name) {
                    *self.sites.entry(name.clone()).or_default() += n;
                }
            }
        }
        visitor::walk_stmt(self, statement);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Module,
    Function,
    Class,
    /// A lambda, a comprehension or type parameters.
    Inner,
}

/// A scope of the walk.
struct Frame {
    kind: Kind,
    /// `None` for the module, else where its node starts.
    id: Option<u32>,
    sites: BTreeMap<String, usize>,
    globals: BTreeSet<String>,
    nonlocals: BTreeSet<String>,
}

struct Walker<'r> {
    read: &'r File,
    target: &'r Target,
    bindings: Vec<Binding>,
    /// The calls that load a module on the way to the symbol.
    calls: Vec<LoadCall>,
    frames: Vec<Frame>,
    in_annotation: bool,
    /// The callee being walked: a use that is all of it is a call.
    callee: Option<TextRange>,
    uses: Vec<SymbolUse>,
    /// The statements a use went through.
    used: BTreeSet<u32>,
    escapes: Vec<Evidence>,
    /// The statements whose module binding escaped.
    escaped: BTreeSet<u32>,
    /// A string names the symbol by its dotted path.
    strings: bool,
    subclasses: Vec<Evidence>,
    /// The statements whose binding its scope binds again.
    rebound: BTreeSet<u32>,
    /// The defining file binds the symbol's name more than once (`pay =
    /// traced(pay)`), so what its own code reads by it depends on run
    /// order.
    own_rebound: bool,
    /// Code may reach the file's names by a computed one: a call of the
    /// builtin `globals()`, `locals()`, `vars()` without arguments, `eval`
    /// or `exec`, or `sys.modules`.
    dynamic: bool,
}

impl<'r> Walker<'r> {
    fn new(
        read: &'r File,
        target: &'r Target,
        bindings: Vec<Binding>,
        calls: Vec<LoadCall>,
    ) -> Self {
        Walker {
            read,
            target,
            bindings,
            calls,
            frames: Vec::new(),
            in_annotation: false,
            callee: None,
            uses: Vec::new(),
            used: BTreeSet::new(),
            escapes: Vec::new(),
            escaped: BTreeSet::new(),
            strings: false,
            subclasses: Vec::new(),
            rebound: BTreeSet::new(),
            own_rebound: false,
            dynamic: false,
        }
    }

    fn module(&mut self, module: &ModModule) {
        let mut binder = Binder::default();
        binder.visit_body(&module.body);
        let mut globals = GlobalAssignments::default();
        globals.visit_body(&module.body);
        for (name, n) in globals.sites {
            *binder.sites.entry(name).or_default() += n;
        }
        // a star binds what it takes without writing it
        for binding in self.bindings.iter().filter(|b| b.star) {
            *binder.sites.entry(binding.name.clone()).or_default() += 1;
        }
        self.push(Kind::Module, None, binder);
        self.visit_body(&module.body);
        self.frames.pop();
    }

    fn push(&mut self, kind: Kind, id: Option<u32>, mut binder: Binder) {
        let globals = std::mem::take(&mut binder.globals);
        let nonlocals = std::mem::take(&mut binder.nonlocals);
        let sites = binder.sites();
        // a binding its scope binds again is no fact of what code uses
        for binding in &self.bindings {
            let again = sites.get(&binding.name).is_some_and(|&n| n > 1);
            if binding.scope != id || !again {
                continue;
            }
            match binding.line {
                Some(line) => {
                    self.rebound.insert(line);
                }
                None if !binding.instance => self.own_rebound = true,
                None => {}
            }
        }
        self.frames.push(Frame {
            kind,
            id,
            sites,
            globals,
            nonlocals,
        });
    }

    /// Where a name read here is bound, with how many places bind it
    /// there; `None` when no scope of the file binds it (a builtin).
    fn resolve(&self, name: &str) -> Option<(Option<u32>, usize)> {
        let last = self.frames.len() - 1;
        for (i, frame) in self.frames.iter().enumerate().rev() {
            match frame.kind {
                Kind::Module => break,
                // a class body's names are its own code's only
                Kind::Class if i != last => continue,
                _ => {}
            }
            if frame.globals.contains(name) {
                break;
            }
            if frame.nonlocals.contains(name) {
                continue;
            }
            if let Some(&sites) = frame.sites.get(name) {
                return Some((frame.id, sites));
            }
        }
        self.frames[0].sites.get(name).map(|&sites| (None, sites))
    }

    /// The bindings a name read here refers to, when they lead to the
    /// symbol and nothing else binds the name in their scope.
    fn bindings_of(&self, name: &str) -> Vec<Binding> {
        match self.resolve(name) {
            Some((scope, 1)) => self
                .bindings
                .iter()
                .filter(|b| b.name == name && b.scope == scope)
                .cloned()
                .collect(),
            _ => Vec::new(),
        }
    }

    fn role(&self, whole: TextRange) -> UseRole {
        if self.in_annotation {
            UseRole::Type
        } else if self.callee == Some(whole) {
            match self.target.class {
                true => UseRole::New,
                false => UseRole::Call,
            }
        } else {
            UseRole::Read
        }
    }

    fn evidence(&self, range: TextRange) -> Evidence {
        Evidence::new(self.read.path.as_str())
            .at_line(self.read.lines.of(range.start().to_usize()))
            .in_test(self.read.test)
    }

    /// A use shown at `at`, of the expression `whole`, written `written`.
    fn push_use(&mut self, at: TextRange, whole: TextRange, written: String, line: Option<u32>) {
        let role = self.role(whole);
        self.uses.push(SymbolUse {
            evidence: self.evidence(at).type_only(role == UseRole::Type),
            column: self
                .read
                .lines
                .column(&self.read.text, at.start().to_usize()),
            role,
            binding: Some(written),
            statement: line.map(|line| ImportPlace {
                file: self.read.path.clone(),
                line,
            }),
        });
        if let Some(line) = line {
            self.used.insert(line);
        }
    }

    fn escape(&mut self, range: TextRange, line: Option<u32>) {
        self.escapes.push(self.evidence(range));
        if let Some(line) = line {
            self.escaped.insert(line);
        }
    }

    /// A name read on its own: a use when it holds the symbol; a module on
    /// the way used as a value lets code reach it unseen.
    fn name(&mut self, name: &ast::ExprName) {
        if name.ctx != ExprContext::Load {
            return;
        }
        let bindings = self.bindings_of(&name.id);
        if let Some(binding) = bindings.iter().find(|b| b.path.is_empty()) {
            self.push_use(name.range, name.range, name.id.to_string(), binding.line);
            return;
        }
        for binding in bindings.iter().filter(|b| b.modules > 0) {
            self.escape(name.range, binding.line);
        }
    }

    /// The call at `range` that loads a module on the way to the symbol.
    fn load_call(&self, range: TextRange) -> Option<&LoadCall> {
        self.calls.iter().find(|c| c.range == range)
    }

    /// An attribute chain from a name, or from a call that loads a module:
    /// a use when it follows a binding's path to the symbol; an escape when
    /// it stops at, or reads a dunder of, a module on the way. Returns
    /// whether the chain was read.
    fn attribute(&mut self, attribute: &ast::ExprAttribute) -> bool {
        let mut chain = vec![attribute];
        let mut base = &*attribute.value;
        while let Expr::Attribute(inner) = base {
            chain.push(inner);
            base = &inner.value;
        }
        chain.reverse();
        let read = self.read;
        let (bindings, base_range, written_base) = match base {
            Expr::Name(name) => (self.bindings_of(&name.id), name.range, name.id.as_str()),
            Expr::Call(call) => match self.load_call(call.range()) {
                Some(found) => (
                    vec![found.binding.clone()],
                    call.range(),
                    &read.text[call.range().start().to_usize()..call.range().end().to_usize()],
                ),
                None => return false,
            },
            _ => return false,
        };
        if bindings.is_empty() {
            return false;
        }
        let attrs: Vec<&str> = chain.iter().map(|a| a.attr.as_str()).collect();
        let along = |path: &[String]| {
            attrs
                .iter()
                .zip(path)
                .take_while(|(a, p)| **a == p.as_str())
                .count()
        };
        if let Some(binding) = bindings.iter().find(|b| along(&b.path) == b.path.len()) {
            // shown where the chain names the symbol
            let n = binding.path.len();
            let (at, whole) = match n {
                0 => (base_range, base_range),
                n => (chain[n - 1].attr.range, chain[n - 1].range),
            };
            let written = std::iter::once(written_base)
                .chain(attrs[..n].iter().copied())
                .collect::<Vec<_>>()
                .join(".");
            self.push_use(at, whole, written, binding.line);
            return true;
        }
        for binding in &bindings {
            let n = along(&binding.path);
            let stops = n == attrs.len() || attrs[n].starts_with("__");
            if n < binding.modules && stops {
                self.escape(attribute.range, binding.line);
            }
        }
        true
    }

    /// A string: in an annotation, the type it holds; elsewhere, a dotted
    /// path that names the symbol (`mock.patch("store.charge.pay")`) lets
    /// code reach it unseen.
    fn string(&mut self, string: &ast::ExprStringLiteral) {
        if self.in_annotation {
            if let Some(single) = string.as_single_part_string() {
                if let Ok(parsed) = parse_string_annotation(&self.read.text, single) {
                    self.visit_expr(&parsed.syntax().body);
                }
            }
            return;
        }
        let Some(dotted) = &self.target.dotted else {
            return;
        };
        let value = string.value.to_str();
        let names = value
            .strip_suffix(dotted.as_str())
            .is_some_and(|before| before.is_empty() || before.ends_with('.'));
        if names {
            let evidence = self.evidence(string.range).with_note("string");
            self.escapes.push(evidence);
            self.strings = true;
        }
    }

    /// Whether `call` calls a builtin that reads names by computed ones:
    /// `globals()`, `locals()`, `vars()` without arguments, `eval` or
    /// `exec`, by a name the file binds nowhere (not `model.eval()`).
    fn reads_namespace(&self, call: &ast::ExprCall) -> bool {
        let Expr::Name(name) = &*call.func else {
            return false;
        };
        let builtin = match name.id.as_str() {
            "globals" | "locals" | "eval" | "exec" => true,
            "vars" => call.arguments.is_empty(),
            _ => false,
        };
        builtin && self.resolve(&name.id).is_none()
    }

    /// For a method, a base that names its class: the subclass's code may
    /// reach the method unseen.
    fn bases(&mut self, arguments: &ast::Arguments) {
        if self.target.tail.len() != 2 {
            return;
        }
        for base in &arguments.args {
            let mut attrs = Vec::new();
            let mut at = base;
            while let Expr::Attribute(a) = at {
                attrs.push(a.attr.as_str());
                at = &a.value;
            }
            attrs.reverse();
            let Expr::Name(name) = at else {
                continue;
            };
            let class = self.bindings_of(&name.id).iter().any(|b| {
                !b.instance
                    && b.path.len() == attrs.len() + 1
                    && attrs.iter().zip(&b.path).all(|(a, p)| *a == p.as_str())
            });
            if class {
                self.subclasses.push(self.evidence(base.range()));
            }
        }
    }

    /// Type parameters, a scope of their own around what they apply to;
    /// returns whether one was pushed.
    fn type_params(&mut self, params: Option<&ast::TypeParams>) -> bool {
        let Some(params) = params else {
            return false;
        };
        let mut binder = Binder::default();
        for param in params.iter() {
            binder.bind(param.name());
        }
        self.push(Kind::Inner, Some(params.range.start().to_u32()), binder);
        let outer = std::mem::replace(&mut self.in_annotation, true);
        for param in params.iter() {
            visitor::walk_type_param(self, param);
        }
        self.in_annotation = outer;
        true
    }

    fn function(&mut self, f: &ast::StmtFunctionDef) {
        // decorators and defaults run where the function is defined
        for decorator in &f.decorator_list {
            self.visit_decorator(decorator);
        }
        for parameter in f.parameters.iter_non_variadic_params() {
            if let Some(default) = &parameter.default {
                self.visit_expr(default);
            }
        }
        let typed = self.type_params(f.type_params.as_deref());
        for parameter in f.parameters.iter() {
            if let Some(annotation) = parameter.annotation() {
                self.visit_annotation(annotation);
            }
        }
        if let Some(returns) = &f.returns {
            self.visit_annotation(returns);
        }
        let mut binder = Binder::default();
        for parameter in f.parameters.iter() {
            binder.bind(parameter.name());
        }
        binder.visit_body(&f.body);
        self.push(Kind::Function, Some(f.range.start().to_u32()), binder);
        self.visit_body(&f.body);
        self.frames.pop();
        if typed {
            self.frames.pop();
        }
    }

    fn class(&mut self, c: &ast::StmtClassDef) {
        for decorator in &c.decorator_list {
            self.visit_decorator(decorator);
        }
        let typed = self.type_params(c.type_params.as_deref());
        if let Some(arguments) = &c.arguments {
            self.bases(arguments);
            self.visit_arguments(arguments);
        }
        let mut binder = Binder::default();
        binder.visit_body(&c.body);
        self.push(Kind::Class, Some(c.range.start().to_u32()), binder);
        self.visit_body(&c.body);
        self.frames.pop();
        if typed {
            self.frames.pop();
        }
    }

    fn lambda(&mut self, lambda: &ast::ExprLambda) {
        let mut binder = Binder::default();
        if let Some(parameters) = &lambda.parameters {
            for parameter in parameters.iter_non_variadic_params() {
                if let Some(default) = &parameter.default {
                    self.visit_expr(default);
                }
            }
            for parameter in parameters.iter() {
                binder.bind(parameter.name());
            }
        }
        binder.visit_expr(&lambda.body);
        self.push(Kind::Inner, Some(lambda.range.start().to_u32()), binder);
        self.visit_expr(&lambda.body);
        self.frames.pop();
    }

    /// A comprehension: its first iterable runs in the scope around it, its
    /// targets bind in its own.
    fn comprehension<'a>(
        &mut self,
        range: TextRange,
        generators: &'a [ast::Comprehension],
        parts: &[&'a Expr],
    ) {
        let Some((first, rest)) = generators.split_first() else {
            return;
        };
        self.visit_expr(&first.iter);
        let mut binder = Binder::default();
        for generator in generators {
            binder.visit_expr(&generator.target);
        }
        self.push(Kind::Inner, Some(range.start().to_u32()), binder);
        for condition in &first.ifs {
            self.visit_expr(condition);
        }
        for generator in rest {
            self.visit_expr(&generator.iter);
            for condition in &generator.ifs {
                self.visit_expr(condition);
            }
        }
        for part in parts {
            self.visit_expr(part);
        }
        self.frames.pop();
    }
}

/// A stub decorated `@overload` or `@typing.overload`.
fn is_overload(f: &ast::StmtFunctionDef) -> bool {
    f.decorator_list.iter().any(|d| match &d.expression {
        Expr::Name(name) => name.id.as_str() == "overload",
        Expr::Attribute(attribute) => attribute.attr.as_str() == "overload",
        _ => false,
    })
}

/// `Literal` or `typing.Literal`, whose strings are values, not types.
fn is_literal(expr: &Expr) -> bool {
    match expr {
        Expr::Name(name) => name.id.as_str() == "Literal",
        Expr::Attribute(attribute) => attribute.attr.as_str() == "Literal",
        _ => false,
    }
}

impl<'a> Visitor<'a> for Walker<'_> {
    fn visit_stmt(&mut self, statement: &'a Stmt) {
        match statement {
            Stmt::FunctionDef(f) => self.function(f),
            Stmt::ClassDef(c) => self.class(c),
            // an import binds and uses nothing
            Stmt::Import(_) | Stmt::ImportFrom(_) | Stmt::Global(_) | Stmt::Nonlocal(_) => {}
            // `type X = ...` holds a type
            Stmt::TypeAlias(alias) => {
                let typed = self.type_params(alias.type_params.as_deref());
                self.visit_annotation(&alias.value);
                if typed {
                    self.frames.pop();
                }
            }
            _ => visitor::walk_stmt(self, statement),
        }
    }

    fn visit_annotation(&mut self, expr: &'a Expr) {
        let outer = std::mem::replace(&mut self.in_annotation, true);
        self.visit_expr(expr);
        self.in_annotation = outer;
    }

    fn visit_expr(&mut self, expr: &'a Expr) {
        match expr {
            Expr::Name(name) => self.name(name),
            Expr::Attribute(attribute) => {
                let sys = matches!(&*attribute.value, Expr::Name(n) if n.id.as_str() == "sys");
                self.dynamic |= sys && attribute.attr.as_str() == "modules";
                if !self.attribute(attribute) {
                    visitor::walk_expr(self, expr);
                }
            }
            Expr::Call(call) => {
                self.dynamic |= self.reads_namespace(call);
                // a module that a call loads goes on as a value
                if let Some(found) = self.load_call(call.range()).filter(|c| !c.held) {
                    let line = found.binding.line;
                    self.escape(call.range(), line);
                }
                let outer = self.callee.replace(call.func.range());
                self.visit_expr(&call.func);
                self.callee = outer;
                // arguments are values, in an annotation too
                let outer = std::mem::replace(&mut self.in_annotation, false);
                self.visit_arguments(&call.arguments);
                self.in_annotation = outer;
            }
            Expr::StringLiteral(string) => self.string(string),
            Expr::Subscript(s) if self.in_annotation && is_literal(&s.value) => {
                self.visit_expr(&s.value);
            }
            Expr::Lambda(lambda) => self.lambda(lambda),
            Expr::ListComp(c) => self.comprehension(c.range, &c.generators, &[&c.elt]),
            Expr::SetComp(c) => self.comprehension(c.range, &c.generators, &[&c.elt]),
            Expr::Generator(c) => self.comprehension(c.range, &c.generators, &[&c.elt]),
            Expr::DictComp(c) => {
                let parts: Vec<&Expr> = c.key.iter().map(|k| &**k).chain([&*c.value]).collect();
                self.comprehension(c.range, &c.generators, &parts);
            }
            _ => visitor::walk_expr(self, expr),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ruff_python_parser::parse_expression;

    #[test]
    fn a_call_names_its_module_as_the_scan_reads_it() {
        let named = |code: &str| {
            let parsed = parse_expression(code).unwrap();
            match parsed.expr() {
                Expr::Call(call) => loads_by_name(call).map(|(module, top, _)| (module, top)),
                _ => None,
            }
        };
        let module = |name: &str, top: bool| Some((name.to_owned(), top));
        assert_eq!(
            named("importlib.import_module('a.b')"),
            module("a.b", false)
        );
        assert_eq!(
            named("import_module('.b', package='a')"),
            module("a.b", false)
        );
        assert_eq!(named("import_module('..c', 'a.b')"), module("a.c", false));
        assert_eq!(named("import_module('.', 'a.b')"), module("a.b", false));
        assert_eq!(named("__import__('a.b')"), module("a.b", true));
        // a computed name, a relative one without its package, and
        // `__import__` with a `fromlist`, which returns another module
        assert_eq!(named("import_module(name)"), None);
        assert_eq!(named("import_module(f'a.{name}')"), None);
        assert_eq!(named("import_module('.b')"), None);
        assert_eq!(named("__import__('a.b', fromlist=['c'])"), None);
        assert_eq!(named("load('a.b')"), None);
    }
}
