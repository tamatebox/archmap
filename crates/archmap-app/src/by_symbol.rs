//! `query <file> --by-symbol`: every public symbol of one file with the
//! statements that take it and where it is used, in one answer.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use anyhow::{bail, Result};
use archmap_core::{Edge, Evidence, Symbol, SymbolUses};
use serde::Serialize;

use crate::not_traced::{with_uses, NotTraced};
use crate::query::{file_view, instance_method, uses_of};
use crate::query_text::{
    count, file_head, marks, not_traced, plural, with_more, MAX_LOCATIONS, NOT_TRACED,
};
use crate::resolve::{resolve, Resolved};
use crate::target::{component_file, reject_outside, unquote};
use crate::views::Importer;
use crate::{Answer, Format, Found, Workspace};

/// What `query --by-symbol` is asked.
#[derive(Debug, Clone, Copy)]
pub struct BySymbolRequest<'a> {
    pub target: &'a str,
    pub depth: usize,
    pub format: Format,
    /// Every symbol instead of a capped list (text only).
    pub verbose: bool,
}

/// Symbols listed in text; JSON lists all of them.
const MAX_SYMBOLS: usize = 30;

impl Workspace {
    /// Every public symbol of a file with who takes it and where it is used,
    /// as text or JSON, or the candidates when the target names several
    /// things.
    pub fn by_symbol(&self, request: &BySymbolRequest) -> Result<Answer> {
        by_symbol(self, request)
    }
}

/// One public symbol of the file.
#[derive(Debug, Serialize)]
struct Row<'a> {
    #[serde(flatten)]
    symbol: &'a Symbol,
    /// The statements that take its name.
    imported_by: Vec<Importer<'a>>,
    /// The statements that take its file whole.
    may_use: Vec<Importer<'a>>,
    /// Where it is used, its own file included; `None` for a language no
    /// uses pass reads.
    #[serde(skip_serializing_if = "Option::is_none")]
    used_at: Option<SymbolUses>,
    /// What the uses pass could not follow for it, as `query <symbol>`
    /// names it.
    #[serde(skip_serializing_if = "Option::is_none")]
    not_traced: Option<NotTraced>,
}

#[derive(Debug, Serialize)]
struct BySymbolView<'a> {
    requested: &'a str,
    depth: usize,
    file: String,
    symbols: Vec<Row<'a>>,
    /// What could reach the file unseen, as `query <file>` names it.
    #[serde(skip_serializing_if = "Option::is_none")]
    not_traced: Option<&'a NotTraced>,
}

fn by_symbol(ws: &Workspace, request: &BySymbolRequest) -> Result<Answer> {
    let BySymbolRequest {
        target,
        depth,
        format,
        verbose,
    } = *request;
    let target = unquote(target);
    let root = ws.root();
    reject_outside(root, target)?;
    let full = ws.graph();
    let rolled = full.rollup(depth);
    let file = match resolve(full, &rolled, &ws.report, target)? {
        Resolved::File(file) => file,
        Resolved::Component(c) => match component_file(full, root, c) {
            Some(file) => file,
            None => bail!(
                "`{target}` is a component with files of its own: by symbol takes one file, \
                 such as one `query {target}` lists"
            ),
        },
        Resolved::Candidates(candidates) => {
            return Ok(Answer {
                output: candidates.render(full, target, format, verbose)?,
                found: Found::Candidates,
            })
        }
        _ => bail!("by symbol takes a file, and `{target}` names none"),
    };
    let view = file_view(full, depth, target, &file);
    let mut symbols: Vec<&Symbol> = view.symbols.clone();
    symbols.sort_by_key(|s| s.location().map(|e| (e.file.clone(), e.line)));
    // a uses pass per symbol, which reads the same files: on every core
    let uses = in_parallel(&symbols, |symbol| uses_of(full, &ws.report, symbol));
    let rows: Vec<Row> = symbols
        .into_iter()
        .zip(uses)
        .map(|(symbol, used_at)| {
            let found = full.symbol_importers(symbol);
            let (imported_by, may_use) = match found {
                Some(found) => (importers(found.by_name), importers(found.may_use)),
                None => (Vec::new(), Vec::new()),
            };
            let not_traced = used_at
                .as_ref()
                .and_then(|uses| with_uses(None, uses, instance_method(symbol)));
            Row {
                symbol,
                imported_by,
                may_use,
                used_at,
                not_traced,
            }
        })
        .collect();
    let result = BySymbolView {
        requested: target,
        depth,
        file: file.clone(),
        symbols: rows,
        not_traced: view.not_traced.as_ref(),
    };
    let output = match format {
        Format::Json => crate::json(&result)?,
        Format::Text => {
            let mut out = String::new();
            let component = view.component.as_ref().and_then(|id| rolled.component(id));
            file_head(&mut out, &file, component, depth);
            let (cap, locations) = match verbose {
                true => (usize::MAX, usize::MAX),
                false => (MAX_SYMBOLS, MAX_LOCATIONS),
            };
            let mut truncated = text(&mut out, &result, &file, cap);
            // no line says who imports the file: a script says so here
            let mut tail = String::new();
            if let Some(found) = result.not_traced {
                truncated |= not_traced(&mut tail, found, locations, true, true, true);
            }
            let shown = result.symbols.len().min(cap);
            truncated |= symbol_gaps(&mut tail, &result.symbols[..shown], locations);
            marks(&mut out, &tail);
            out.push_str(&tail);
            if truncated {
                let _ = writeln!(
                    out,
                    "\nLists are capped; JSON lists every entry with all evidence."
                );
            }
            out
        }
    };
    Ok(Answer {
        output,
        found: Found::One,
    })
}

/// The most threads the uses passes take: archmap runs beside an editor
/// and other tools.
const MAX_THREADS: usize = 8;

/// Fewer items than this are worth no thread.
const MIN_PARALLEL: usize = 4;

/// `f` of each of `items`, in their order, run on up to [`MAX_THREADS`] of
/// the cores there are.
fn in_parallel<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    if items.len() < MIN_PARALLEL {
        return items.iter().map(f).collect();
    }
    let threads = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(MAX_THREADS);
    let chunk = items.len().div_ceil(threads).max(1);
    std::thread::scope(|scope| {
        let handles: Vec<_> = items
            .chunks(chunk)
            .map(|part| scope.spawn(|| part.iter().map(&f).collect::<Vec<R>>()))
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().expect("a uses pass panicked"))
            .collect()
    })
}

fn importers<'g>(list: Vec<(&'g Edge, &'g Evidence)>) -> Vec<Importer<'g>> {
    list.into_iter()
        .map(|(edge, evidence)| Importer {
            from: &edge.from,
            evidence,
            through: None,
        })
        .collect()
}

/// A line per symbol: how many statements take it, by name or its file
/// whole, and where it is used, in its own file too.
fn text(out: &mut String, view: &BySymbolView, file: &str, cap: usize) -> bool {
    let total = view.symbols.len();
    let shown = total.min(cap);
    let _ = writeln!(
        out,
        "\nPublic symbols and their uses: {}",
        count(total, shown)
    );
    let width = view
        .symbols
        .iter()
        .take(shown)
        .map(|r| r.symbol.name.chars().count())
        .max()
        .unwrap_or(0);
    for row in view.symbols.iter().take(shown) {
        let mut parts: Vec<String> = Vec::new();
        let tests = row.imported_by.iter().filter(|i| i.evidence.test).count();
        match (row.imported_by.len() - tests, tests) {
            (0, 0) => {}
            (0, tests) => parts.push(format!("imported by {tests} in tests")),
            (production, 0) => parts.push(format!("imported by {production}")),
            (production, tests) => parts.push(format!(
                "imported by {}, {tests} in tests",
                production + tests
            )),
        }
        if !row.may_use.is_empty() {
            parts.push(format!("may use {}", row.may_use.len()));
        }
        if let Some(uses) = &row.used_at {
            let files: BTreeSet<&str> =
                uses.uses.iter().map(|u| u.evidence.file.as_str()).collect();
            let own = uses.uses.iter().filter(|u| u.evidence.file == file).count();
            let tests = uses.uses.iter().filter(|u| u.evidence.test).count();
            if !uses.uses.is_empty() {
                let mut part = format!(
                    "used at {} in {}",
                    uses.uses.len(),
                    plural(files.len(), "file")
                );
                if tests > 0 {
                    let _ = write!(part, ", {tests} in tests");
                }
                if own > 0 {
                    let _ = write!(part, ", {own} in this file");
                }
                parts.push(part);
            } else if !parts.is_empty() && !instance_method(row.symbol) {
                parts.push("no use found".to_owned());
            }
            // what the uses pass reads of a method that takes a value
            if instance_method(row.symbol) {
                parts.push("calls through a value are not read".to_owned());
            }
        }
        let line = if parts.is_empty() {
            NONE.to_owned()
        } else {
            parts.join("; ")
        };
        let _ = writeln!(out, "  {:<width$}  {line}", row.symbol.name);
    }
    // what the cap leaves out that a reader looks for
    if shown < total {
        let none = view.symbols[shown..].iter().filter(|r| unused(r)).count();
        let _ = writeln!(out, "  {} more, {none} of them {NONE}", total - shown);
    }
    shown < total
}

/// What `query <symbol>` names under `Not traced` for each of `rows`, as
/// it writes it (`cap` locations a line): each line once, under the
/// symbols it holds for, since most hold for every symbol a statement
/// takes. Returns whether some were left out.
fn symbol_gaps(out: &mut String, rows: &[Row], cap: usize) -> bool {
    let mut truncated = false;
    // each line with the symbols it holds for, in source order
    let mut lines: Vec<(String, Vec<&str>)> = Vec::new();
    for row in rows {
        let Some(found) = &row.not_traced else {
            continue;
        };
        let mut text = String::new();
        truncated |= not_traced(&mut text, found, cap, false, false, false);
        for line in text.lines().filter(|l| l.starts_with("  ")) {
            let name = row.symbol.name.as_str();
            match lines.iter_mut().find(|(l, _)| l == line) {
                Some((_, names)) => names.push(name),
                None => lines.push((line.to_owned(), vec![name])),
            }
        }
    }
    let mut groups: Vec<(Vec<&str>, Vec<String>)> = Vec::new();
    for (line, names) in lines {
        match groups.iter_mut().find(|(n, _)| *n == names) {
            Some((_, group)) => group.push(line),
            None => groups.push((names, vec![line])),
        }
    }
    if groups.is_empty() {
        return truncated;
    }
    if !out.contains(NOT_TRACED) {
        let _ = writeln!(out, "\n{NOT_TRACED}");
    }
    for (names, group) in groups {
        let shown: Vec<String> = names.iter().take(cap).map(|n| (*n).to_owned()).collect();
        truncated |= shown.len() < names.len();
        let _ = writeln!(out, "  {}:", with_more(&shown, names.len()));
        for line in group {
            let _ = writeln!(out, "  {line}");
        }
    }
    truncated
}

const NONE: &str = "none found";

/// No statement takes the symbol and no use of it was read.
fn unused(row: &Row) -> bool {
    row.imported_by.is_empty()
        && row.may_use.is_empty()
        && row.used_at.as_ref().is_some_and(|u| u.uses.is_empty())
}
