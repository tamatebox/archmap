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

use ruff_python_ast::{Expr, Number, Operator, PySourceType, Stmt};
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
                let value = self.path(&assign.value);
                for target in &assign.targets {
                    self.bind(target, value.clone());
                }
            }
            Stmt::AnnAssign(assign) => {
                let value = assign.value.as_deref().and_then(|v| self.path(v));
                self.bind(&assign.target, value);
            }
            Stmt::Expr(expr) => self.call(&expr.value),
            Stmt::If(stmt) => {
                self.body(&stmt.body);
                for clause in &stmt.elif_else_clauses {
                    self.body(&clause.body);
                }
            }
            Stmt::Try(stmt) => {
                self.body(&stmt.body);
                for handler in &stmt.handlers {
                    let ruff_python_ast::ExceptHandler::ExceptHandler(handler) = handler;
                    self.body(&handler.body);
                }
                self.body(&stmt.orelse);
                self.body(&stmt.finalbody);
            }
            Stmt::With(stmt) => self.body(&stmt.body),
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
            _ => {}
        }
    }

    fn unbind(&mut self, name: &str) {
        self.modules.remove(name);
        self.names.insert(name.to_owned(), None);
    }

    fn bind(&mut self, target: &Expr, value: Option<Vec<String>>) {
        if let Expr::Name(name) = target {
            self.modules.remove(name.id.as_str());
            self.names.insert(name.id.to_string(), value);
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
