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

/// A call that loads a module by a name computed at runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PyDynamicImport {
    /// The function called: `import_module`, `__import__` or
    /// `spec_from_file_location`.
    pub call: &'static str,
    pub line: u32,
    /// Inside a function body.
    pub local: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct PyFile {
    pub imports: Vec<PyImport>,
    pub defs: Vec<PyDef>,
    pub dynamic_imports: Vec<PyDynamicImport>,
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
    // Delimiter of the open multi-line string, and whether the statement
    // that opened it is inside a function.
    let mut in_string: Option<(&str, bool)> = None;
    // Indentation of the enclosing `def` headers, innermost last.
    let mut functions: Vec<usize> = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        let raw = lines[i];
        let line_no = (i + 1) as u32;
        i += 1;

        // Track multi-line string literals so docstrings are not scanned.
        if let Some((delim, local)) = in_string {
            if let Some(end) = raw.find(delim) {
                // The statement may go on after the string (`"""; f(x)`),
                // even into another one (`""" + """`).
                let rest = &raw[end + delim.len()..];
                let reopened = opens_multiline_string(rest);
                let code = reopened.map_or(rest, |(_, at)| &rest[..at]);
                if let Some(call) = dynamic_call(code) {
                    out.dynamic_imports.push(PyDynamicImport {
                        call,
                        line: line_no,
                        local,
                    });
                }
                in_string = reopened.map(|(open, _)| (open, local));
            }
            continue;
        }

        let trimmed = raw.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indent = raw.len() - trimmed.len();

        if let Some((open, at)) = opens_multiline_string(trimmed) {
            // A statement may still start on this line (e.g. `X = """`), but
            // only a call before the string matters: defs and imports never
            // open one. The line may continue a call at any indentation, so
            // it does not close functions.
            let local = functions.first().is_some_and(|&d| indent > d);
            in_string = Some((open, local));
            if let Some(call) = dynamic_call(&trimmed[..at]) {
                out.dynamic_imports.push(PyDynamicImport {
                    call,
                    line: line_no,
                    local,
                });
            }
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

        if let Some(call) = dynamic_call(trimmed) {
            out.dynamic_imports.push(PyDynamicImport {
                call,
                line: line_no,
                local,
            });
        }

        let code = strip_comment(trimmed);
        if let Some(rest) = code.strip_prefix("import ") {
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

        if let Some(rest) = code.strip_prefix("from ") {
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

/// Functions that load a module by a name computed at runtime.
const DYNAMIC_CALLS: &[&str] = &["__import__", "import_module", "spec_from_file_location"];

/// A call to one of [`DYNAMIC_CALLS`] in this code. Definitions of functions
/// with those names, longer names ending in them (`my_import_module`) and
/// names inside string literals or comments do not count.
fn dynamic_call(code: &str) -> Option<&'static str> {
    let code = code.trim_start();
    if code.starts_with("def ") || code.starts_with("async def ") {
        return None;
    }
    DYNAMIC_CALLS.iter().copied().find(|name| {
        code.match_indices(name).any(|(at, _)| {
            let before = code[..at].chars().next_back();
            let is_call = code[at + name.len()..].trim_start().starts_with('(');
            is_call && !before.is_some_and(|c| c.is_alphanumeric() || c == '_') && is_code(code, at)
        })
    })
}

/// Whether byte `at` of `line` is code: outside string literals (single,
/// double and triple quotes, with backslash escapes) and before a comment.
fn is_code(line: &str, at: usize) -> bool {
    let bytes = line.as_bytes();
    let mut quote: Option<&[u8]> = None;
    let mut i = 0;
    while i < at {
        match quote {
            Some(_) if bytes[i] == b'\\' => i += 2,
            Some(q) if bytes[i..].starts_with(q) => {
                i += q.len();
                quote = None;
            }
            Some(_) => i += 1,
            None => match bytes[i] {
                b'#' => return false,
                c @ (b'"' | b'\'') => {
                    let len = if bytes[i..].starts_with(&[c; 3]) {
                        3
                    } else {
                        1
                    };
                    quote = Some(&bytes[i..i + len]);
                    i += len;
                }
                _ => i += 1,
            },
        }
    }
    quote.is_none()
}

/// A line that opens a triple-quoted string without closing it: the
/// delimiter and where it starts.
fn opens_multiline_string(trimmed: &str) -> Option<(&'static str, usize)> {
    for delim in ["\"\"\"", "'''"] {
        if let Some(pos) = trimmed.find(delim) {
            let after = &trimmed[pos + 3..];
            if !after.contains(delim) {
                return Some((delim, pos));
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

/// `line` up to its comment: the first `#` outside string literals.
fn strip_comment(line: &str) -> &str {
    line.match_indices('#')
        .find(|(at, _)| is_code(line, *at))
        .map_or(line, |(at, _)| &line[..at])
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
    fn calls_that_load_modules_by_name_are_dynamic_imports() {
        let text = "\
import importlib
from importlib import import_module
# importlib.import_module(\"commented out\")
\"\"\"
__import__(\"inside a docstring\")
\"\"\"
PLUGIN = importlib.import_module(NAME)
def import_module(name):
    return __import__(name)
def load(path):
    spec = importlib.util.spec_from_file_location(\"m\", path)
    return my_import_module(path)
";
        let file = scan_source(text);
        let calls: Vec<(&str, u32, bool)> = file
            .dynamic_imports
            .iter()
            .map(|d| (d.call, d.line, d.local))
            .collect();
        assert_eq!(
            calls,
            vec![
                ("import_module", 7, false),
                ("__import__", 9, true),
                ("spec_from_file_location", 11, true),
            ]
        );
    }

    #[test]
    fn calls_written_inside_strings_or_comments_are_not_dynamic_imports() {
        let text = r#"guard('__import__("os").system("ls")')
check("import_module(x)")
text = "escaped \"__import__(x)\" is still text"
x = 1  # import_module(y)
mod = importlib.import_module(name)  # a real call before a comment
s = 'it\'s'; plugin = import_module(name)
"#;
        let file = scan_source(text);
        let calls: Vec<(&str, u32)> = file
            .dynamic_imports
            .iter()
            .map(|d| (d.call, d.line))
            .collect();
        assert_eq!(calls, vec![("import_module", 5), ("import_module", 6)]);
    }

    #[test]
    fn calls_beside_a_multiline_string_are_dynamic_imports() {
        let text = r#"doc = """
__import__("inside the string")
"""; plugin = import_module(name)
mod = import_module(name); note = """
import_module("inside the string")
"""
"#;
        let file = scan_source(text);
        let calls: Vec<(&str, u32)> = file
            .dynamic_imports
            .iter()
            .map(|d| (d.call, d.line))
            .collect();
        assert_eq!(calls, vec![("import_module", 3), ("import_module", 4)]);

        // a string opened at column 0 inside a function does not end it
        let text = "def f():\n    sql = dedent(\n\"\"\"\nSELECT 1\n\"\"\")\n    import os\n";
        let file = scan_source(text);
        assert_eq!(
            file.imports.iter().map(|i| i.local).collect::<Vec<_>>(),
            vec![true]
        );

        // a call after the string belongs to the statement that opened it
        let text = "def f():\n    pass\nX = \"\"\"\n\"\"\"; m = importlib.import_module(n)\n";
        let file = scan_source(text);
        let calls: Vec<(u32, bool)> = file
            .dynamic_imports
            .iter()
            .map(|d| (d.line, d.local))
            .collect();
        assert_eq!(calls, vec![(4, false)]);
    }

    #[test]
    fn a_multiline_string_can_reopen_where_it_closes() {
        let text = "s = \"\"\"\ntext\n\"\"\" + \"\"\"\nimport not_an_import\n\"\"\"\nimport real\n";
        let file = scan_source(text);
        let modules: Vec<&str> = file.imports.iter().map(|i| i.module.as_str()).collect();
        assert_eq!(modules, vec!["real"]);
    }

    #[test]
    fn an_escaped_quote_does_not_end_a_string_in_a_header() {
        let file = scan_source("def f(x=\"\\\"#\"):\n    pass\ndef g():\n    pass\n");
        let defs: Vec<(&str, Option<&str>)> = file
            .defs
            .iter()
            .map(|d| (d.name.as_str(), d.signature.as_deref()))
            .collect();
        assert_eq!(
            defs,
            vec![("f", Some("def f(x=\"\\\"#\")")), ("g", Some("def g()"))]
        );
    }

    #[test]
    fn comments_after_imports_name_nothing() {
        let file = scan_source("import os  # os, sys\nfrom shop import users  # users, billing\n");
        let imports: Vec<(&str, Vec<&str>)> = file
            .imports
            .iter()
            .map(|i| {
                (
                    i.module.as_str(),
                    i.names.iter().map(String::as_str).collect(),
                )
            })
            .collect();
        assert_eq!(imports, vec![("os", vec![]), ("shop", vec!["users"])]);
    }

    #[test]
    fn multiline_from_import_keeps_module_only() {
        let file = scan_source("from a.b import (\n    x,\n    y,\n)\n");
        assert_eq!(file.imports.len(), 1);
        assert_eq!(file.imports[0].module, "a.b");
    }
}
