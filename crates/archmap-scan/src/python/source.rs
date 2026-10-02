//! Line-based structural scan of a Python file.
//!
//! This is deliberately not a full parser. It recognizes the statements
//! that carry architectural facts (`import`, `from ... import`, top-level
//! `def` / `class`, public methods, `CONSTANT = ...`) and ignores everything
//! else. It can be swapped for a real parser behind the same functions.

use std::collections::BTreeSet;

use archmap_core::{SymbolKind, WHOLE_MODULE};

/// One `import` or `from ... import` statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PyImport {
    /// Dotted module path after `from` / `import`; empty for `from . import x`.
    pub module: String,
    /// Number of leading dots in a relative import (0 = absolute).
    pub level: usize,
    /// Names imported by a `from` statement (may be submodules); `*` for a
    /// star import or a list that could not be read whole.
    pub names: Vec<String>,
    /// What the statement binds in the importing file: for `from`, the
    /// name each of `names` is bound to (its own, or the one after `as`);
    /// for `import`, the module's top-level name, or the one after `as`.
    pub bound: Vec<String>,
    pub line: u32,
    /// Inside a function body, so it runs only when the function is called.
    pub local: bool,
    /// Under `if TYPE_CHECKING:`, which only type checkers enter.
    pub type_only: bool,
    /// Inside a class body, where it binds names of the class.
    pub in_class: bool,
    /// The name list could not be read whole: `*` in `names` stands for
    /// what else it may take, and what it binds is unknown.
    pub unread: bool,
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

/// The module-level `__all__`: the names a star import of the file takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DunderAll {
    /// A list or tuple of string literals.
    Listed(Vec<String>),
    /// Built or changed by code the scan does not run.
    Built,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct PyFile {
    pub imports: Vec<PyImport>,
    pub defs: Vec<PyDef>,
    pub dynamic_imports: Vec<PyDynamicImport>,
    pub all: Option<DunderAll>,
    /// The names that module-level statements other than imports bind when
    /// the module runs, as the scan reads them: targets of `x = …` and
    /// `x: T = …`, and `def` and `class` names, in `if`, `try` and `else`
    /// blocks too and private ones included, but not under `if
    /// TYPE_CHECKING:`.
    pub module_names: BTreeSet<String>,
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
    // Indentation of the enclosing `if TYPE_CHECKING:` headers.
    let mut type_checking: Vec<usize> = Vec::new();
    // Indentation of the enclosing `class` headers, of any class.
    let mut classes: Vec<usize> = Vec::new();
    // Brackets that a statement other than an import or a header left
    // open: the lines inside them go on with it, as a call's arguments.
    let mut brackets: i32 = 0;
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
                brackets = (brackets + code_brackets(code)).max(0);
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
            let before = &trimmed[..at];
            if let Some(call) = dynamic_call(before) {
                out.dynamic_imports.push(PyDynamicImport {
                    call,
                    line: line_no,
                    local,
                });
            }
            // `DOC = """`, at module level
            let inside = |headers: &[usize]| headers.first().is_some_and(|&d| indent > d);
            if brackets == 0 && !local && !inside(&classes) && !inside(&type_checking) {
                out.module_names
                    .extend(assigned_names(before).into_iter().map(str::to_owned));
            }
            brackets = (brackets + code_brackets(before)).max(0);
            continue;
        }

        while functions.last().is_some_and(|&d| indent <= d) {
            functions.pop();
        }
        let local = !functions.is_empty();
        // a line at a header's indentation, its `else:` included, ends the block
        while type_checking.last().is_some_and(|&d| indent <= d) {
            type_checking.pop();
        }
        let type_only = !type_checking.is_empty();
        while classes.last().is_some_and(|&d| indent <= d) {
            classes.pop();
        }
        let in_class = !classes.is_empty();

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
        let continuing = brackets > 0;
        if is_type_checking(code) {
            brackets = 0;
            type_checking.push(indent);
            continue;
        }
        let is_import = code.starts_with("import ") || code.starts_with("from ");
        if is_import {
            brackets = 0;
        }
        // An import continued over lines (in brackets, or after a
        // backslash) is read whole, and its other lines are no statements.
        let (joined, extra, open) = if is_import {
            continued(&lines, i, code)
        } else {
            (String::new(), 0, false)
        };
        i += extra;
        let code = if is_import { joined.as_str() } else { code };
        if let Some(rest) = code.strip_prefix("import ") {
            // a backslash on the last line of the file is left at the end
            for item in rest.trim_end_matches('\\').split(',') {
                let mut words = item.split_whitespace();
                let module = words.next().unwrap_or("");
                if !module.is_empty() {
                    let bound = match (words.next(), words.next()) {
                        (Some("as"), Some(alias)) => alias,
                        _ => module.split('.').next().unwrap_or(module),
                    };
                    out.imports.push(PyImport {
                        module: module.to_owned(),
                        level: 0,
                        names: Vec::new(),
                        bound: vec![bound.to_owned()],
                        line: line_no,
                        local,
                        type_only,
                        in_class,
                        unread: false,
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
                let (mut names, mut bound): (Vec<String>, Vec<String>) = names
                    .trim()
                    .trim_start_matches('(')
                    .trim_end_matches(')')
                    .trim_end_matches('\\')
                    .split(',')
                    .filter_map(|item| {
                        let mut words = item.split_whitespace();
                        let name = words.next()?;
                        let alias = match (words.next(), words.next()) {
                            (Some("as"), Some(alias)) => alias,
                            _ => name,
                        };
                        Some((name.to_owned(), alias.to_owned()))
                    })
                    .unzip();
                // a list that could not be read whole may take anything
                if open && !names.iter().any(|n| n == WHOLE_MODULE) {
                    names.push(WHOLE_MODULE.to_owned());
                    bound.push(WHOLE_MODULE.to_owned());
                }
                out.imports.push(PyImport {
                    module,
                    level,
                    names,
                    bound,
                    line: line_no,
                    local,
                    type_only,
                    in_class,
                    unread: open,
                });
            }
            continue;
        }

        if !local && !in_class && is_dunder_all(code) {
            brackets = 0;
            let (joined, extra, _) = continued(&lines, i, code);
            i += extra;
            let listed = (indent == 0).then(|| listed_names(&joined)).flatten();
            out.all = Some(match (&out.all, listed) {
                (Some(DunderAll::Built), _) | (_, None) => DunderAll::Built,
                (_, Some(names)) => DunderAll::Listed(names),
            });
            continue;
        }

        let is_def = trimmed.starts_with("def ") || trimmed.starts_with("async def ");
        let is_class = trimmed.starts_with("class ");
        if is_def || is_class {
            brackets = 0;
            let (header, consumed) = collect_header(&lines, i - 1);
            let header = if is_def {
                without_defaults(&header)
            } else {
                header
            };
            i = (i - 1) + consumed;
            if is_def {
                functions.push(indent);
            }
            if is_class {
                classes.push(indent);
            }
            let Some(name) = def_name(&header) else {
                continue;
            };
            if !local && !in_class && !type_only {
                out.module_names.insert(name.clone());
            }

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

        if !continuing && !local && !in_class && !type_only {
            out.module_names
                .extend(assigned_names(code).into_iter().map(str::to_owned));
        }
        brackets = (brackets + code_brackets(code)).max(0);
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

/// `header`, a `def`, with each parameter's default value as `…`: a
/// default can hold a secret (`url="postgres://user:pass@host"`), as a
/// constant can. A default runs from an `=` in the parameter list to the
/// next `,` or `)` there, past strings and brackets.
fn without_defaults(header: &str) -> String {
    let chars: Vec<char> = header.chars().collect();
    let mut out = String::with_capacity(header.len());
    let (mut depth, mut skipping, mut quote) = (0usize, false, None);
    // inside the parameters of a lambda default, whose commas are its own
    let mut lambda = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        // what the text keeps: everything but a default
        let mut kept = vec![c];
        if let Some(q) = quote {
            if c == '\\' {
                kept.extend(chars.get(i + 1));
                i += 1;
            } else if c == q {
                quote = None;
            }
        } else {
            match c {
                '\'' | '"' => quote = Some(c),
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => {
                    if depth == 1 {
                        skipping = false;
                    }
                    depth = depth.saturating_sub(1);
                }
                ':' if depth == 1 && lambda => lambda = false,
                ',' if depth == 1 && !lambda => skipping = false,
                '=' if depth == 1
                    && !skipping
                    && chars.get(i + 1) != Some(&'=')
                    && !matches!(out.chars().last(), Some('=' | '!' | '<' | '>')) =>
                {
                    if chars.get(i + 1) == Some(&' ') {
                        kept.push(' ');
                    }
                    kept.push('…');
                    out.extend(kept);
                    skipping = true;
                    let value: String = chars[i + 1..].iter().collect();
                    let value = value.trim_start();
                    lambda = value.starts_with("lambda")
                        && !value[6..].starts_with(|c: char| c.is_alphanumeric() || c == '_');
                    i += 1;
                    continue;
                }
                _ => {}
            }
        }
        if !skipping {
            out.extend(kept);
        }
        i += 1;
    }
    out
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

/// The statement that `first` starts, joined with the lines that continue
/// it, at most 50 as for headers: while a bracket is open, or after a
/// trailing backslash, up to a `;`, which an import never holds. Returns
/// it, how many lines after the first it took (`next` is the index of the
/// line after the first), and whether it was still open where the reading
/// stopped.
fn continued(lines: &[&str], next: usize, first: &str) -> (String, usize, bool) {
    let (first, mut ended) = before_semicolon(first);
    let mut joined = first.trim_end().to_owned();
    let mut depth = bracket_depth(&joined);
    let mut extra = 0;
    while !ended && extra < 50 && next + extra < lines.len() {
        let backslash = joined.ends_with('\\');
        if depth <= 0 && !backslash {
            break;
        }
        if backslash {
            joined.pop();
        }
        let (line, end) = before_semicolon(strip_comment(lines[next + extra]).trim());
        ended = end;
        extra += 1;
        depth += bracket_depth(line);
        joined.push(' ');
        joined.push_str(line.trim_end());
    }
    let open = depth > 0 || joined.ends_with('\\');
    (joined, extra, open)
}

/// `code` up to its first `;`, and whether it had one.
fn before_semicolon(code: &str) -> (&str, bool) {
    match code.split_once(';') {
        Some((before, _)) => (before, true),
        None => (code, false),
    }
}

/// Brackets opened minus brackets closed in `code`.
fn bracket_depth(code: &str) -> i32 {
    code.chars()
        .map(|c| match c {
            '(' | '[' | '{' => 1,
            ')' | ']' | '}' => -1,
            _ => 0,
        })
        .sum()
}

/// `line` up to its comment: the first `#` outside string literals.
fn strip_comment(line: &str) -> &str {
    line.match_indices('#')
        .find(|(at, _)| is_code(line, *at))
        .map_or(line, |(at, _)| &line[..at])
}

/// `if TYPE_CHECKING:` or `if <module>.TYPE_CHECKING:` (`typing.`, `t.`),
/// whose body only type checkers enter.
fn is_type_checking(code: &str) -> bool {
    let Some(condition) = code
        .trim_end()
        .strip_prefix("if ")
        .and_then(|c| c.strip_suffix(':'))
    else {
        return false;
    };
    match condition.trim().strip_suffix("TYPE_CHECKING") {
        Some("") => true,
        Some(prefix) => prefix
            .strip_suffix('.')
            .is_some_and(|m| !m.is_empty() && m.chars().all(|c| c.is_alphanumeric() || c == '_')),
        None => false,
    }
}

/// A statement that sets or changes `__all__`.
fn is_dunder_all(code: &str) -> bool {
    code.strip_prefix("__all__")
        .is_some_and(|rest| !rest.starts_with(|c: char| c.is_alphanumeric() || c == '_'))
}

/// The names of `__all__ = [...]` (or a tuple, or annotated), when every
/// item is a string literal.
fn listed_names(statement: &str) -> Option<Vec<String>> {
    let rest = statement.strip_prefix("__all__")?.trim_start();
    let rest = match rest.strip_prefix(':') {
        Some(annotated) => annotated.split_once('=')?.1,
        None => rest.strip_prefix('=')?,
    };
    let value = rest.trim();
    let inner = value
        .strip_prefix('[')
        .and_then(|v| v.strip_suffix(']'))
        .or_else(|| value.strip_prefix('(').and_then(|v| v.strip_suffix(')')))?;
    inner
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(|item| {
            let quote = item.chars().next().filter(|c| *c == '"' || *c == '\'')?;
            let name = item.strip_prefix(quote)?.strip_suffix(quote)?;
            (!name.contains(['"', '\''])).then(|| name.to_owned())
        })
        .collect()
}

/// The names a simple assignment binds: `x = …`, `x: T = …` and each
/// target of `x = y = …`; not an annotation alone, a tuple, an attribute
/// or an item.
fn assigned_names(code: &str) -> Vec<&str> {
    let mut names = Vec::new();
    let mut rest = code;
    while let Some((name, value)) = assignment(rest) {
        names.push(name);
        rest = value;
    }
    names
}

/// The target of the assignment `code` starts with, and the code after its
/// `=`.
fn assignment(code: &str) -> Option<(&str, &str)> {
    let end = code
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(code.len());
    let name = &code[..end];
    if name.is_empty() || name.starts_with(|c: char| c.is_ascii_digit()) || is_keyword(name) {
        return None;
    }
    let rest = code[end..].trim_start();
    let value = match rest.strip_prefix(':') {
        Some(annotated) => annotated.split_once('=')?.1,
        None => rest.strip_prefix('=')?,
    };
    (!value.starts_with('=')).then(|| (name, value.trim_start()))
}

/// The keywords that start a statement and may be followed by `:` or `=`
/// in what the scan reads.
fn is_keyword(word: &str) -> bool {
    matches!(
        word,
        "if" | "elif"
            | "else"
            | "try"
            | "except"
            | "finally"
            | "for"
            | "while"
            | "with"
            | "lambda"
            | "async"
            | "class"
            | "def"
    )
}

/// Brackets opened minus brackets closed in `code`, outside string
/// literals.
fn code_brackets(code: &str) -> i32 {
    code.char_indices()
        .filter(|(at, _)| is_code(code, *at))
        .map(|(_, c)| match c {
            '(' | '[' | '{' => 1,
            ')' | ']' | '}' => -1,
            _ => 0,
        })
        .sum()
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
                (
                    "shop.billing.charge".into(),
                    0,
                    vec!["Payment".into(), "refund".into()],
                ),
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
    fn headers_leave_out_default_values() {
        // a default can hold a secret, as a constant can
        let file = scan_source(
            "def connect(url=\"postgres://u:p@h\", retries: int = 3, *,\n            \
             key=os.environ.get('K', 'x'), mode='a,b', flag=(1 == 2)) -> Conn:\n    pass\n\
             class Repo(Base, metaclass=Meta):\n    pass\n\
             def sort(items, key=lambda a, b: a < b, reverse=False):\n    pass\n",
        );
        let signatures: Vec<&str> = file
            .defs
            .iter()
            .filter_map(|d| d.signature.as_deref())
            .collect();
        assert_eq!(
            signatures,
            [
                "def connect(url=…, retries: int = …, *, key=…, mode=…, flag=…) -> Conn",
                "class Repo(Base, metaclass=Meta)",
                // a lambda's parameters are part of the default
                "def sort(items, key=…, reverse=…)",
            ]
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
    fn imports_under_type_checking_take_types_only() {
        let text = "\
from typing import TYPE_CHECKING
if TYPE_CHECKING:
    from shop.orders import Order
    import shop.users
    # a comment at any indentation ends nothing
# here either
    if flag:
        import nested
    else:
        import nested_else
else:
    import fallback
if typing.TYPE_CHECKING:  # pragma: no cover
    from shop import (
Payment,
    )
elif other:
    import runtime_elif
import after
if TYPE_CHECKING:
    NOTE = \"\"\"
a string at column 0 ends nothing
\"\"\"
    import after_string
if TYPE_CHECKING:
    import second_block
def f():
    if TYPE_CHECKING:
        import in_function
    import local_runtime
class C:
    if t.TYPE_CHECKING:
        import in_class
if not TYPE_CHECKING:
    import runtime_not
if TYPE_CHECKING_EXTRA:
    import other_name
";
        let file = scan_source(text);
        let marks: Vec<(&str, bool)> = file
            .imports
            .iter()
            .map(|i| (i.module.as_str(), i.type_only))
            .collect();
        assert_eq!(
            marks,
            vec![
                ("typing", false),
                ("shop.orders", true),
                ("shop.users", true),
                ("nested", true),
                ("nested_else", true),
                // the other branch runs
                ("fallback", false),
                ("shop", true),
                ("runtime_elif", false),
                ("after", false),
                ("after_string", true),
                ("second_block", true),
                ("in_function", true),
                ("local_runtime", false),
                ("in_class", true),
                ("runtime_not", false),
                ("other_name", false),
            ]
        );
    }

    #[test]
    fn imports_in_class_bodies_bind_no_module_name() {
        let text = "class C:\n    from x import y\nclass _P:\n    import z\nif flag:\n    \
                    class D:\n        from a import b\n    from c import d\nfrom m import n\n";
        let file = scan_source(text);
        let marks: Vec<(&str, bool)> = file
            .imports
            .iter()
            .map(|i| (i.module.as_str(), i.in_class))
            .collect();
        assert_eq!(
            marks,
            [
                ("x", true),
                ("z", true),
                ("a", true),
                ("c", false),
                ("m", false)
            ]
        );
    }

    #[test]
    fn a_literal_dunder_all_lists_what_a_star_import_takes() {
        let listed = |names: &[&str]| {
            Some(DunderAll::Listed(
                names.iter().map(|n| n.to_string()).collect(),
            ))
        };
        assert_eq!(
            scan_source("__all__ = ['pay', \"Refund\"]\n").all,
            listed(&["pay", "Refund"])
        );
        // over lines, annotated, as a tuple; the lines in it are no statements
        let file = scan_source("__all__: list[str] = (\n    'pay',\n    'post',\n)\nPOST = 1\n");
        assert_eq!(file.all, listed(&["pay", "post"]));
        assert_eq!(file.defs.len(), 1);
        // built at runtime
        for text in [
            "__all__ = ['pay']\n__all__ += other.__all__\n",
            "__all__ = base + ['pay']\n",
            "__all__ = ['pay']\n__all__.extend(['post'])\n",
            "__all__ = [name for name in dir() if name.isupper()]\n",
        ] {
            assert_eq!(scan_source(text).all, Some(DunderAll::Built), "{text}");
        }
        assert_eq!(scan_source("x = 1\n").all, None);
    }

    #[test]
    fn module_level_assignments_and_definitions_name_the_module() {
        let text = "\
pay = make_pay()
_cache: dict = {}
count: int
if flag:
    def helper():
        inner = 1
    refund = 2
else:
    Foo = Any
try:
    from .fast import speed
except ImportError:
    speed = None
class _Private:
    attr = 1
    def method(self):
        pass
if TYPE_CHECKING:
    Lazy = int
x.y = 1
a, b = 1, 2
if x == 1:
    pass
";
        let file = scan_source(text);
        let names: Vec<&str> = file.module_names.iter().map(String::as_str).collect();
        // an annotation alone, a function's, a class's and a type checker's
        // names are none of the module's when it runs
        assert_eq!(
            names,
            ["Foo", "_Private", "_cache", "helper", "pay", "refund", "speed"]
        );

        // the lines inside a call's brackets are its arguments, a string's
        // brackets open none, and a string can be the value
        let text = "\
registry.register(
    pay=pay,
    level=1,
)
app = FastAPI(
    title=\"x\",
)
paren = \"(\"
after = 1
DOC = \"\"\"
text = 1
\"\"\"
first = second = 0
match = None
";
        let file = scan_source(text);
        let names: Vec<&str> = file.module_names.iter().map(String::as_str).collect();
        assert_eq!(
            names,
            ["DOC", "after", "app", "first", "match", "paren", "second"]
        );
    }

    #[test]
    fn a_string_that_closes_a_call_closes_its_brackets() {
        let file = scan_source(
            "parser.add_argument(\"--x\", help=\"\"\"\nSome help.\n\"\"\")\n\nMAX_RETRIES = 3\nlimit = 2\n",
        );
        let defs: Vec<&str> = file.defs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(defs, ["MAX_RETRIES"]);
        assert!(
            file.module_names.contains("limit"),
            "{:?}",
            file.module_names
        );
    }

    #[test]
    fn from_imports_record_the_names_they_bind() {
        let file = scan_source(
            "from .charge import charge as pay, refund\nfrom shop import (\n    Payment as P,\n)\n\
             import os as system\nfrom x import *\n",
        );
        let bound: Vec<(Vec<&str>, Vec<&str>)> = file
            .imports
            .iter()
            .map(|i| {
                (
                    i.names.iter().map(String::as_str).collect(),
                    i.bound.iter().map(String::as_str).collect(),
                )
            })
            .collect();
        assert_eq!(
            bound,
            [
                (vec!["charge", "refund"], vec!["pay", "refund"]),
                (vec!["Payment"], vec!["P"]),
                // `import` binds the module itself
                (vec![], vec!["system"]),
                (vec!["*"], vec!["*"]),
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
            vec![("f", Some("def f(x=…)")), ("g", Some("def g()"))]
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
    fn imports_continued_over_lines_keep_their_names() {
        let file = scan_source(
            "from a.b import (\n    x,\n    y,  # why\n)\nfrom c import d, \\\n    e\n\
             import f, \\\n    g\n\ndef h():\n    pass\n",
        );
        let imports: Vec<(&str, Vec<&str>, u32)> = file
            .imports
            .iter()
            .map(|i| {
                (
                    i.module.as_str(),
                    i.names.iter().map(String::as_str).collect(),
                    i.line,
                )
            })
            .collect();
        assert_eq!(
            imports,
            [
                ("a.b", vec!["x", "y"], 1),
                ("c", vec!["d", "e"], 5),
                ("f", vec![], 7),
                ("g", vec![], 7),
            ]
        );
        // the lines after them are read as before
        let defs: Vec<(&str, u32)> = file
            .defs
            .iter()
            .map(|d| (d.name.as_str(), d.line))
            .collect();
        assert_eq!(defs, [("h", 10)]);
    }

    #[test]
    fn an_import_ends_at_a_semicolon() {
        let file = scan_source(
            "import a; x = (\n    b,\n)\nfrom c import d; values = (\n    1,\n    e,\n)\n\
             import os; y = (\n    importlib.import_module(n)\n)\n",
        );
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
        assert_eq!(imports, [("a", vec![]), ("c", vec!["d"]), ("os", vec![])]);
        // the lines after a semicolon are read as before
        let calls: Vec<u32> = file.dynamic_imports.iter().map(|d| d.line).collect();
        assert_eq!(calls, [9]);
    }

    #[test]
    fn a_name_list_that_cannot_be_read_takes_the_whole_module() {
        // open at the end of the file
        let file = scan_source("from a import (\n    b,\n");
        assert_eq!(file.imports.len(), 1);
        assert_eq!(file.imports[0].names, ["b", "*"]);
        // what else it binds is unknown, unlike a star import
        assert!(file.imports[0].unread);
        assert!(!scan_source("from a import *\n").imports[0].unread);
        // still open after 50 lines
        let names: String = (0..60).map(|i| format!("    n{i},\n")).collect();
        let file = scan_source(&format!("from a import (\n{names})\n"));
        assert_eq!(file.imports[0].names.len(), 51);
        assert_eq!(file.imports[0].names.last().map(String::as_str), Some("*"));
        let file = scan_source("import a, \\");
        let modules: Vec<&str> = file.imports.iter().map(|i| i.module.as_str()).collect();
        assert_eq!(modules, ["a"]);
        // `import` after a backslash on the line before
        let file = scan_source("from a.b \\\n    import c\n");
        assert_eq!(file.imports[0].module, "a.b");
        assert_eq!(file.imports[0].names, ["c"]);
        // a star import takes the whole module
        let file = scan_source("from a import *\n");
        assert_eq!(file.imports[0].names, ["*"]);
    }
}
