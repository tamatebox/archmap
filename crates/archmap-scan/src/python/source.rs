//! Line-based structural scan of a Python file.
//!
//! This is deliberately not a full parser. It recognizes the statements
//! that carry architectural facts (`import`, `from ... import`, top-level
//! `def` / `class`, public methods, `CONSTANT = ...`) and ignores everything
//! else. It can be swapped for a real parser behind the same functions.

use archmap_core::SymbolKind;

/// One `import` or `from ... import` statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PyImport {
    /// Dotted module path after `from` / `import`; empty for `from . import x`.
    pub module: String,
    /// Number of leading dots in a relative import (0 = absolute).
    pub level: usize,
    /// Names imported by a `from` statement (may be submodules).
    pub names: Vec<String>,
    pub line: u32,
    /// Inside a function body, so it runs only when the function is called.
    pub local: bool,
}

/// One public definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PyDef {
    /// `Foo` or `Foo.method`.
    pub name: String,
    pub kind: SymbolKind,
    pub signature: Option<String>,
    pub line: u32,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct PyFile {
    pub imports: Vec<PyImport>,
    pub defs: Vec<PyDef>,
}

struct ClassCtx {
    name: String,
    indent: usize,
    /// Indentation of the class body, learned from its first statement.
    body_indent: Option<usize>,
}

pub fn scan_source(text: &str) -> PyFile {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = PyFile::default();
    let mut class: Option<ClassCtx> = None;
    let mut in_string: Option<&str> = None;
    // Indentation of the enclosing `def` headers, innermost last.
    let mut functions: Vec<usize> = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        let raw = lines[i];
        let line_no = (i + 1) as u32;
        i += 1;

        // Track multi-line string literals so docstrings are not scanned.
        if let Some(delim) = in_string {
            if raw.contains(delim) {
                in_string = None;
            }
            continue;
        }

        let trimmed = raw.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indent = raw.len() - trimmed.len();

        if let Some(open) = opens_multiline_string(trimmed) {
            in_string = Some(open);
            // A statement may still start on this line (e.g. `X = """`), but
            // we only care about defs/imports, which never do.
            continue;
        }

        while functions.last().is_some_and(|&d| indent <= d) {
            functions.pop();
        }
        let local = !functions.is_empty();

        // Leaving a class body.
        if let Some(ctx) = &class {
            if indent <= ctx.indent {
                class = None;
            }
        }
        if let Some(ctx) = &mut class {
            if ctx.body_indent.is_none() {
                ctx.body_indent = Some(indent);
            }
        }

        if let Some(rest) = trimmed.strip_prefix("import ") {
            for module in rest.split(',') {
                let module = module.split_whitespace().next().unwrap_or("");
                if !module.is_empty() {
                    out.imports.push(PyImport {
                        module: module.to_owned(),
                        level: 0,
                        names: Vec::new(),
                        line: line_no,
                        local,
                    });
                }
            }
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("from ") {
            if let Some((target, names)) = rest.split_once(" import ") {
                let target = target.trim();
                let level = target.chars().take_while(|c| *c == '.').count();
                let module = target[level..].to_owned();
                let names = names
                    .trim()
                    .trim_start_matches('(')
                    .trim_end_matches(')')
                    .trim_end_matches('\\')
                    .split(',')
                    .filter_map(|n| n.split_whitespace().next())
                    .filter(|n| *n != "*" && !n.is_empty())
                    .map(str::to_owned)
                    .collect();
                out.imports.push(PyImport {
                    module,
                    level,
                    names,
                    line: line_no,
                    local,
                });
            }
            continue;
        }

        let is_def = trimmed.starts_with("def ") || trimmed.starts_with("async def ");
        let is_class = trimmed.starts_with("class ");
        if is_def || is_class {
            let (header, consumed) = collect_header(&lines, i - 1);
            i = (i - 1) + consumed;
            if is_def {
                functions.push(indent);
            }
            let Some(name) = def_name(&header) else {
                continue;
            };

            if indent == 0 {
                // Methods are only interface when their class is public.
                if is_class && is_public(&name) {
                    class = Some(ClassCtx {
                        name: name.clone(),
                        indent,
                        body_indent: None,
                    });
                }
                if is_public(&name) {
                    out.defs.push(PyDef {
                        name,
                        kind: if is_class {
                            SymbolKind::Struct
                        } else {
                            SymbolKind::Function
                        },
                        signature: Some(header),
                        line: line_no,
                    });
                }
            } else if let Some(ctx) = &class {
                if is_def && ctx.body_indent == Some(indent) && is_public(&name) {
                    out.defs.push(PyDef {
                        name: format!("{}.{name}", ctx.name),
                        kind: SymbolKind::Function,
                        signature: Some(header),
                        line: line_no,
                    });
                }
            }
            continue;
        }

        if indent == 0 {
            if let Some(constant) = constant_name(trimmed) {
                out.defs.push(PyDef {
                    name: constant.to_owned(),
                    kind: SymbolKind::Constant,
                    signature: None,
                    line: line_no,
                });
            }
        }
    }

    out
}

/// A line that opens a triple-quoted string without closing it.
fn opens_multiline_string(trimmed: &str) -> Option<&'static str> {
    for delim in ["\"\"\"", "'''"] {
        if let Some(pos) = trimmed.find(delim) {
            let after = &trimmed[pos + 3..];
            if !after.contains(delim) {
                return Some(delim);
            }
            return None;
        }
    }
    None
}

/// Join a `def` / `class` header that may span several lines. Returns the
/// header without the trailing colon and the number of lines consumed.
fn collect_header(lines: &[&str], start: usize) -> (String, usize) {
    let mut depth: i32 = 0;
    let mut parts = Vec::new();
    let mut consumed = 0;
    for line in lines.iter().skip(start).take(50) {
        consumed += 1;
        let code = strip_comment(line).trim();
        parts.push(code.to_owned());
        for c in code.chars() {
            match c {
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth -= 1,
                _ => {}
            }
        }
        if depth <= 0 && code.ends_with(':') {
            break;
        }
    }
    let joined = parts.join(" ");
    let header = joined.trim_end_matches(':').trim();
    let header = header.split_whitespace().collect::<Vec<_>>().join(" ");
    (
        header
            .replace("( ", "(")
            .replace(" )", ")")
            .replace(" ,", ","),
        consumed,
    )
}

fn strip_comment(line: &str) -> &str {
    // Good enough for headers: a `#` outside quotes ends the code.
    let mut in_quote: Option<char> = None;
    for (idx, c) in line.char_indices() {
        match (in_quote, c) {
            (Some(q), c) if c == q => in_quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => in_quote = Some(c),
            (None, '#') => return &line[..idx],
            _ => {}
        }
    }
    line
}

fn def_name(header: &str) -> Option<String> {
    let rest = header
        .strip_prefix("async def ")
        .or_else(|| header.strip_prefix("def "))
        .or_else(|| header.strip_prefix("class "))?;
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    (!name.is_empty()).then_some(name)
}

fn is_public(name: &str) -> bool {
    !name.starts_with('_')
}

/// `NAME = ...` or `NAME: type = ...` at module level, uppercase only.
fn constant_name(trimmed: &str) -> Option<&str> {
    let name_end = trimmed
        .find(|c: char| !(c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
        .unwrap_or(trimmed.len());
    let name = &trimmed[..name_end];
    if name.is_empty() || !name.chars().next()?.is_ascii_uppercase() {
        return None;
    }
    let rest = trimmed[name_end..].trim_start();
    let is_assignment = rest.starts_with(':') && rest.contains('=')
        || (rest.starts_with('=') && !rest.starts_with("=="));
    is_assignment.then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
"""Module docstring with a fake
def not_a_def():
"""
import os, sys
import requests as rq
from . import sibling
from ..users import User, Role
from shop.billing.charge import (
    Payment,
    refund,
)

MAX_RETRIES = 3
Config: dict = {}
lower = 1

def _private():
    pass

async def pay(
    user: User,
    amount: int,  # yen
) -> bool:
    import json
    def inner():
        pass
    return True

class Payment(Base):
    """Docstring."""
    RATE = 2

    def __init__(self, x):
        self.x = x

    def charge(self, amount: int) -> "Receipt":
        if amount:
            def helper(): pass
        return None

    @staticmethod
    def _hidden():
        pass

class _Internal:
    def visible_but_owner_private(self):
        pass

CURRENCY = "JPY"
"#;

    #[test]
    fn extracts_imports_with_levels_and_names() {
        let file = scan_source(SAMPLE);
        let modules: Vec<(String, usize, Vec<String>)> = file
            .imports
            .iter()
            .map(|i| (i.module.clone(), i.level, i.names.clone()))
            .collect();
        assert_eq!(
            modules,
            vec![
                ("os".into(), 0, vec![]),
                ("sys".into(), 0, vec![]),
                ("requests".into(), 0, vec![]),
                ("".into(), 1, vec!["sibling".into()]),
                ("users".into(), 2, vec!["User".into(), "Role".into()]),
                ("shop.billing.charge".into(), 0, vec![]),
                ("json".into(), 0, vec![]),
            ]
        );
        assert_eq!(file.imports[3].line, 7);
    }

    #[test]
    fn extracts_public_top_level_defs_and_methods() {
        let file = scan_source(SAMPLE);
        let names: Vec<(&str, SymbolKind)> = file
            .defs
            .iter()
            .map(|d| (d.name.as_str(), d.kind))
            .collect();
        assert_eq!(
            names,
            vec![
                ("MAX_RETRIES", SymbolKind::Constant),
                ("pay", SymbolKind::Function),
                ("Payment", SymbolKind::Struct),
                ("Payment.charge", SymbolKind::Function),
                ("CURRENCY", SymbolKind::Constant),
            ]
        );
        let pay = &file.defs[1];
        assert_eq!(
            pay.signature.as_deref(),
            Some("async def pay(user: User, amount: int,) -> bool")
        );
        assert_eq!(pay.line, 21);
        assert_eq!(
            file.defs[3].signature.as_deref(),
            Some("def charge(self, amount: int) -> \"Receipt\"")
        );
    }

    #[test]
    fn imports_inside_functions_are_local() {
        let text = "import a\nif flag:\n    import b\nclass C:\n    import c\n    def m(self):\n        import d\n    x = 1\ndef f():\n    import e\n    def g():\n        import f2\n    import h\nimport i\n";
        let file = scan_source(text);
        let local: Vec<(&str, bool)> = file
            .imports
            .iter()
            .map(|i| (i.module.as_str(), i.local))
            .collect();
        assert_eq!(
            local,
            vec![
                ("a", false),
                ("b", false), // module level, inside `if`: runs on load
                ("c", false), // class bodies run on load too
                ("d", true),
                ("e", true),
                ("f2", true),
                ("h", true),
                ("i", false),
            ]
        );
    }

    #[test]
    fn multiline_from_import_keeps_module_only() {
        let file = scan_source("from a.b import (\n    x,\n    y,\n)\n");
        assert_eq!(file.imports.len(), 1);
        assert_eq!(file.imports[0].module, "a.b");
    }
}
