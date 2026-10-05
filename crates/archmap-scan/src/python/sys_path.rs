//! The directories a `conftest.py` adds to `sys.path`, which pytest loads
//! before the files below its directory: `sys.path.insert(0, X)` and
//! `sys.path.append(X)` where `X` is a path the scan can compute from
//! `__file__` (`Path(__file__).resolve().parents[1] / "src"`,
//! `os.path.join(os.path.dirname(__file__), "..")`), through module-level
//! names too. A function counts only by the module it comes from as the
//! file imports it (`os.path.join`, `join` from `os.path`), never by its
//! name alone.
//!
//! The file is parsed with Ruff's parser. Only statements that run when the
//! module loads are read: those at module level and inside `if`, `try` and
//! `with` there, not function or class bodies. A path that leaves the root,
//! or that the scan cannot compute, adds nothing.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use ruff_python_ast::visitor::{walk_expr, walk_stmt, Visitor};
use ruff_python_ast::{Expr, ExprContext, Number, Operator, PySourceType, Stmt};
use ruff_python_parser::parse_unchecked_source;
use ruff_text_size::Ranged;

use crate::lines::Lines;

/// A directory on `sys.path`, relative to the root (empty for the root),
/// with the line of the call that adds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SearchPath {
    pub(super) dir: PathBuf,
    pub(super) line: u32,
}

/// What one `conftest.py` adds to `sys.path`, each list in the order
/// `sys.path` ends up holding it.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Added {
    /// Inserted before the entries already there.
    pub(super) front: Vec<SearchPath>,
    /// Appended after them.
    pub(super) back: Vec<SearchPath>,
}

/// What the `conftest.py` at `file` (relative to the root) with `text`
/// adds to `sys.path`.
pub(super) fn added_by(file: &Path, text: &str) -> Added {
    let parsed = parse_unchecked_source(text, PySourceType::Python);
    let mut reader = Reader {
        file: components(file),
        names: BTreeMap::new(),
        modules: BTreeMap::new(),
        lines: Lines::new(text),
        added: Added::default(),
    };
    reader.body(&parsed.syntax().body);
    // each insert goes before the ones made earlier
    reader.added.front.reverse();
    reader.added
}

struct Reader {
    file: Option<Vec<String>>,
    /// Module-level names bound to a path the scan computed; `None` for a
    /// name bound to anything else since.
    names: BTreeMap<String, Option<Vec<String>>>,
    /// What module-level imports bind: `osp` to `os.path` for `import
    /// os.path as osp`, `join` to `os.path.join` for `from os.path import
    /// join`.
    modules: BTreeMap<String, String>,
    lines: Lines,
    added: Added,
}

impl Reader {
    fn body(&mut self, body: &[Stmt]) {
        for stmt in body {
            self.stmt(stmt);
        }
    }

    fn stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Assign(assign) => {
                self.unbind_stores(&assign.value);
                let value = self.path(&assign.value);
                for target in &assign.targets {
                    self.bind(target, value.clone());
                }
            }
            Stmt::AnnAssign(assign) => {
                let value = assign.value.as_deref().and_then(|v| self.path(v));
                self.bind(&assign.target, value);
            }
            Stmt::Expr(expr) => {
                self.unbind_stores(&expr.value);
                self.call(&expr.value);
            }
            // each arm from the same names; a name the arms bind apart is
            // known only where they agree
            Stmt::If(stmt) => {
                self.unbind_stores(&stmt.test);
                let mut arms: Vec<Vec<&Stmt>> = vec![stmt.body.iter().collect()];
                arms.extend(
                    stmt.elif_else_clauses
                        .iter()
                        .map(|c| c.body.iter().collect()),
                );
                let skipped = stmt.elif_else_clauses.iter().all(|c| c.test.is_some());
                self.arms(&arms, skipped);
            }
            Stmt::Try(stmt) => {
                let mut arms: Vec<Vec<&Stmt>> =
                    vec![stmt.body.iter().chain(&stmt.orelse).collect()];
                for handler in &stmt.handlers {
                    let ruff_python_ast::ExceptHandler::ExceptHandler(handler) = handler;
                    if let Some(name) = &handler.name {
                        self.unbind(name.as_str());
                    }
                    arms.push(handler.body.iter().collect());
                }
                self.arms(&arms, false);
                self.body(&stmt.finalbody);
            }
            Stmt::With(stmt) => {
                for item in &stmt.items {
                    self.unbind_stores(&item.context_expr);
                    if let Some(target) = &item.optional_vars {
                        self.unbind_stores(target);
                    }
                }
                self.body(&stmt.body);
            }
            // a definition binds its name to something other than a path
            Stmt::FunctionDef(def) => self.unbind(def.name.as_str()),
            Stmt::ClassDef(def) => self.unbind(def.name.as_str()),
            Stmt::Import(import) => {
                for alias in &import.names {
                    let name = alias.name.as_str();
                    let (local, module) = match &alias.asname {
                        Some(local) => (local.as_str(), name),
                        None => {
                            let top = name.split('.').next().unwrap_or(name);
                            (top, top)
                        }
                    };
                    self.names.remove(local);
                    self.modules.insert(local.to_owned(), module.to_owned());
                }
            }
            Stmt::ImportFrom(import) if import.level == 0 => {
                let Some(module) = &import.module else {
                    return;
                };
                for alias in &import.names {
                    let local = alias.asname.as_ref().unwrap_or(&alias.name);
                    self.names.remove(local.as_str());
                    self.modules
                        .insert(local.to_string(), format!("{module}.{}", alias.name));
                }
            }
            // a loop, an augmented assignment, `del`: what they store is no
            // path the scan computes
            other => {
                let mut stores = Stores::default();
                stores.visit_stmt(other);
                for name in stores.names {
                    self.unbind(&name);
                }
            }
        }
    }

    /// Run each of `arms` from the names as they are, `skipped` when none
    /// may run, then keep a name only where every way agrees.
    fn arms(&mut self, arms: &[Vec<&Stmt>], skipped: bool) {
        let (names, modules) = (self.names.clone(), self.modules.clone());
        let mut ways = Vec::new();
        for arm in arms {
            self.names = names.clone();
            self.modules = modules.clone();
            for stmt in arm {
                self.stmt(stmt);
            }
            ways.push((self.names.clone(), self.modules.clone()));
        }
        if skipped {
            ways.push((names, modules));
        }
        let Some(((first_names, first_modules), rest)) = ways.split_first() else {
            return;
        };
        self.names = first_names
            .iter()
            .map(|(name, path)| {
                let same = rest.iter().all(|(n, _)| n.get(name) == Some(path));
                (name.clone(), path.clone().filter(|_| same))
            })
            .collect();
        for (n, _) in rest {
            for name in n.keys() {
                if !first_names.contains_key(name) {
                    self.names.insert(name.clone(), None);
                }
            }
        }
        self.modules = first_modules
            .iter()
            .filter(|(name, module)| rest.iter().all(|(_, m)| m.get(*name) == Some(module)))
            .map(|(name, module)| (name.clone(), module.clone()))
            .collect();
    }

    /// Unbind what `expr` stores: a walrus, or the targets of a `with`.
    fn unbind_stores(&mut self, expr: &Expr) {
        let mut stores = Stores::default();
        stores.visit_expr(expr);
        for name in stores.names {
            self.unbind(&name);
        }
    }

    fn unbind(&mut self, name: &str) {
        self.modules.remove(name);
        self.names.insert(name.to_owned(), None);
    }

    fn bind(&mut self, target: &Expr, value: Option<Vec<String>>) {
        match target {
            Expr::Name(name) => {
                self.modules.remove(name.id.as_str());
                self.names.insert(name.id.to_string(), value);
            }
            // a tuple or a list unpacks something other than a path
            other => self.unbind_stores(other),
        }
    }

    /// `sys.path.insert(i, X)` or `sys.path.append(X)`.
    fn call(&mut self, expr: &Expr) {
        let Expr::Call(call) = expr else {
            return;
        };
        let Expr::Attribute(method) = call.func.as_ref() else {
            return;
        };
        if self.qualified(&method.value).as_deref() != Some("sys.path")
            || !call.arguments.keywords.is_empty()
        {
            return;
        }
        let args = &call.arguments.args;
        let (added, front) = match (method.attr.as_str(), args.len()) {
            ("insert", 2) => (&args[1], true),
            ("append", 1) => (&args[0], false),
            _ => return,
        };
        let Some(dir) = self.path(added) else {
            return;
        };
        let entry = SearchPath {
            dir: dir.iter().collect(),
            line: self.lines.of(call.range().start().to_usize()),
        };
        if front {
            self.added.front.push(entry);
        } else {
            self.added.back.push(entry);
        }
    }

    /// The path `expr` computes, as components relative to the root.
    fn path(&self, expr: &Expr) -> Option<Vec<String>> {
        match expr {
            Expr::Name(name) if name.id.as_str() == "__file__" => self.file.clone(),
            Expr::Name(name) => self.names.get(name.id.as_str()).cloned().flatten(),
            Expr::Attribute(attr) => {
                let mut path = self.path(&attr.value)?;
                if attr.attr.as_str() != "parent" {
                    return None;
                }
                path.pop()?;
                Some(path)
            }
            Expr::Subscript(subscript) => {
                // `.parents[N]`
                let Expr::Attribute(attr) = subscript.value.as_ref() else {
                    return None;
                };
                let Expr::NumberLiteral(n) = subscript.slice.as_ref() else {
                    return None;
                };
                let Number::Int(n) = &n.value else {
                    return None;
                };
                if attr.attr.as_str() != "parents" {
                    return None;
                }
                let mut path = self.path(&attr.value)?;
                for _ in 0..=n.as_usize()? {
                    path.pop()?;
                }
                Some(path)
            }
            Expr::BinOp(op) if op.op == Operator::Div => {
                join(self.path(&op.left)?, [literal(&op.right)?])
            }
            Expr::Call(call) => {
                if !call.arguments.keywords.is_empty() {
                    return None;
                }
                let args = &call.arguments.args;
                // a method of a path: `.resolve()`, `.joinpath("src")`
                if let Expr::Attribute(method) = call.func.as_ref() {
                    if let Some(path) = self.path(&method.value) {
                        return match method.attr.as_str() {
                            "resolve" | "absolute" if args.is_empty() => Some(path),
                            "joinpath" => join(path, literals(args)?),
                            _ => None,
                        };
                    }
                }
                // a function of `os`, `os.path` or `pathlib`, or `str`
                let function = self.qualified(&call.func)?;
                let (first, rest) = args.split_first()?;
                let path = self.path(first)?;
                match (function.as_str(), rest.is_empty()) {
                    (
                        "pathlib.Path" | "pathlib.PurePath" | "str" | "os.fspath"
                        | "os.path.abspath" | "os.path.realpath" | "os.path.normpath",
                        true,
                    ) => Some(path),
                    ("os.path.dirname", true) => {
                        let mut path = path;
                        path.pop()?;
                        Some(path)
                    }
                    ("os.path.join", _) => join(path, literals(rest)?),
                    _ => None,
                }
            }
            _ => None,
        }
    }
}

impl Reader {
    /// The dotted name `expr` stands for through the module-level imports
    /// (`os.path.join`), `str` for the builtin.
    fn qualified(&self, expr: &Expr) -> Option<String> {
        match expr {
            Expr::Name(name) => match self.modules.get(name.id.as_str()) {
                Some(module) => Some(module.clone()),
                None => (name.id.as_str() == "str").then(|| "str".to_owned()),
            },
            Expr::Attribute(attr) => {
                Some(format!("{}.{}", self.qualified(&attr.value)?, attr.attr))
            }
            _ => None,
        }
    }
}

fn literals(exprs: &[Expr]) -> Option<Vec<&str>> {
    exprs.iter().map(literal).collect()
}

fn literal(expr: &Expr) -> Option<&str> {
    match expr {
        Expr::StringLiteral(s) => Some(s.value.to_str()),
        _ => None,
    }
}

/// The names a statement or an expression stores into or deletes, apart
/// from those of a function or class body, which bind there.
#[derive(Default)]
struct Stores {
    names: Vec<String>,
}

impl<'a> Visitor<'a> for Stores {
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        match stmt {
            Stmt::FunctionDef(def) => self.names.push(def.name.to_string()),
            Stmt::ClassDef(def) => self.names.push(def.name.to_string()),
            _ => walk_stmt(self, stmt),
        }
    }

    fn visit_expr(&mut self, expr: &'a Expr) {
        match expr {
            Expr::Name(name) if name.ctx != ExprContext::Load => {
                self.names.push(name.id.to_string());
            }
            // a lambda's parameters bind in it
            Expr::Lambda(_) => {}
            _ => walk_expr(self, expr),
        }
    }
}

/// `path` joined with relative `parts`, `..` leaving a directory; `None`
/// when that leaves the root or a part is absolute.
fn join<'a>(
    mut path: Vec<String>,
    parts: impl IntoIterator<Item = &'a str>,
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

fn components(file: &Path) -> Option<Vec<String>> {
    join(Vec::new(), [file.to_str()?])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs(entries: &[SearchPath]) -> Vec<(&str, u32)> {
        entries
            .iter()
            .map(|e| (e.dir.to_str().unwrap(), e.line))
            .collect()
    }

    #[test]
    fn paths_computed_from_the_file_are_read() {
        let added = added_by(
            Path::new("tests/conftest.py"),
            "import os\n\
             import sys\n\
             from os.path import dirname, join\n\
             from pathlib import Path\n\
             ROOT = Path(__file__).resolve().parents[1]\n\
             sys.path.insert(0, str(ROOT / \"src\" / \"lib\"))\n\
             if str(ROOT / \"scripts\") not in sys.path:\n\
             \x20   sys.path.insert(0, str(ROOT / \"scripts\"))\n\
             sys.path.append(os.path.abspath(os.path.join(os.path.dirname(__file__), \"..\", \"tools\")))\n\
             sys.path.append(join(dirname(__file__), \"helpers\"))\n\
             sys.path.append(Path(__file__).parent.parent.joinpath(\"vendor\"))\n",
        );
        assert_eq!(dirs(&added.front), vec![("scripts", 8), ("src/lib", 6)]);
        assert_eq!(
            dirs(&added.back),
            vec![("tools", 9), ("tests/helpers", 10), ("vendor", 11)]
        );
    }

    #[test]
    fn a_name_stored_another_way_is_no_path_and_arms_must_agree() {
        let added = |body: &str| {
            let text = format!(
                "import sys\nfrom pathlib import Path\nROOT = Path(__file__).parent.parent\n{body}"
            );
            let added = added_by(Path::new("tests/conftest.py"), &text);
            dirs(&added.front)
                .into_iter()
                .map(|(dir, _)| dir.to_owned())
                .collect::<Vec<_>>()
        };
        // an augmented assignment, a tuple, a loop, a walrus, `del`
        for body in [
            "ROOT /= \"lib\"\nsys.path.insert(0, str(ROOT))\n",
            "ROOT, _ = ROOT / \"lib\", None\nsys.path.insert(0, str(ROOT))\n",
            "for ROOT in [ROOT / \"lib\"]:\n    pass\nsys.path.insert(0, str(ROOT))\n",
            "if (ROOT := Path(\"x\")):\n    pass\nsys.path.insert(0, str(ROOT))\n",
            "del ROOT\nsys.path.insert(0, str(ROOT))\n",
        ] {
            assert!(added(body).is_empty(), "{body}");
        }
        // arms that bind it apart, or one that may not run
        for body in [
            "if X:\n    ROOT = ROOT / \"a\"\nelse:\n    ROOT = ROOT / \"b\"\nsys.path.insert(0, str(ROOT))\n",
            "if X:\n    ROOT = ROOT / \"a\"\nsys.path.insert(0, str(ROOT))\n",
            "try:\n    ROOT = ROOT / \"a\"\nexcept ImportError:\n    pass\nsys.path.insert(0, str(ROOT))\n",
        ] {
            assert!(added(body).is_empty(), "{body}");
        }
        // arms that agree, and a call inside one arm
        assert_eq!(
            added("if X:\n    ROOT = ROOT / \"a\"\nelse:\n    ROOT = ROOT / \"a\"\nsys.path.insert(0, str(ROOT))\n"),
            ["a"]
        );
        assert_eq!(
            added("if X:\n    sys.path.insert(0, str(ROOT / \"b\"))\n"),
            ["b"]
        );
    }

    #[test]
    fn what_the_scan_cannot_compute_adds_nothing() {
        let added = added_by(
            Path::new("conftest.py"),
            "import os, sys\n\
             from pathlib import Path\n\
             sys.path.insert(0, os.environ[\"SRC\"])\n\
             sys.path.insert(0, \"src\")\n\
             sys.path.append(os.path.join(os.path.dirname(__file__), \"..\"))\n\
             sys.path.append(Path(__file__).parents[1])\n\
             HERE = Path(__file__).parent\n\
             HERE = Path.cwd()\n\
             sys.path.append(str(HERE))\n\
             sys.path.append(str(Path(__file__).parent / f\"{os.sep}x\"))\n\
             def join(*parts):\n\
             \x20   return \"/\".join(parts)\n\
             sys.path.append(join(os.path.dirname(__file__), \"helpers\"))\n\
             sys.path.append(\", \".join([str(Path(__file__).parent)]))\n\
             def pytest_configure(config):\n\
             \x20   sys.path.insert(0, str(Path(__file__).parent / \"later\"))\n",
        );
        assert_eq!(added, Added::default());
    }
}
