//! Compact text rendering of `archmap query`, for agents and people.
//!
//! `query` is a drill-down: the local structure of one component, enough to
//! decide what to read next. Lists are capped and the rest is summarized as
//! counts, so a busy component cannot flood an agent's context. `--verbose`
//! lifts the caps; `--format json` carries every piece of evidence.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use archmap_core::{
    ArchitectureGraph, ComponentId, ComponentKind, Edge, EdgeKind, Evidence, Symbol, SymbolKind,
    UnmappedReason,
};

use crate::commands::{ComponentView, QueryResult};

/// Default caps, lifted by `--verbose`.
const MAX_SYMBOLS: usize = 30;
const MAX_NEIGHBORS: usize = 30;
const MAX_LOCATIONS: usize = 3;

struct Caps {
    symbols: usize,
    neighbors: usize,
    locations: usize,
}

impl Caps {
    fn new(verbose: bool) -> Self {
        if verbose {
            Caps {
                symbols: usize::MAX,
                neighbors: usize::MAX,
                locations: usize::MAX,
            }
        } else {
            Caps {
                symbols: MAX_SYMBOLS,
                neighbors: MAX_NEIGHBORS,
                locations: MAX_LOCATIONS,
            }
        }
    }
}

pub fn render(
    result: &QueryResult,
    target: &str,
    full: &ArchitectureGraph,
    rolled: &ArchitectureGraph,
    verbose: bool,
) -> String {
    let caps = Caps::new(verbose);
    let mut out = String::new();
    let truncated = match result {
        QueryResult::Component(view) => component(&mut out, view, full, rolled, &caps),
        QueryResult::Symbols(symbols) => symbol_list(&mut out, symbols, target, rolled, &caps),
    };
    if truncated {
        let _ = writeln!(
            out,
            "\nLists are capped. --verbose shows every entry; --format json adds all evidence."
        );
    }
    out
}

fn component(
    out: &mut String,
    view: &ComponentView,
    full: &ArchitectureGraph,
    rolled: &ArchitectureGraph,
    caps: &Caps,
) -> bool {
    let c = view.component;
    let mut head = format!("{} ({}", c.name, component_kind(c.kind));
    if let Some(language) = &c.language {
        let _ = write!(head, ", {language}");
    }
    head.push(')');
    if let Some(path) = &c.path {
        let _ = write!(head, " at {path}");
    }
    let _ = writeln!(out, "{head}, depth {}", view.depth);
    let _ = writeln!(out, "id: {}", c.id);
    if let Some(from) = &view.folded_from {
        let _ = writeln!(out, "folded from: {}", display(full, from));
    }

    let mut truncated = false;
    if !view.children.is_empty() {
        let total = view.children.len();
        let shown = total.min(caps.neighbors);
        truncated |= shown < total;
        let hint = if full.depth_of(&c.id) >= view.depth {
            format!(", folded at this depth: use --depth {}", view.depth + 1)
        } else {
            String::new()
        };
        let names: Vec<String> = view
            .children
            .iter()
            .take(shown)
            .map(|id| display(full, id))
            .collect();
        let _ = writeln!(out, "\nChildren: {}{hint}", count(total, shown));
        let _ = writeln!(out, "  {}", names.join(", "));
    }

    let total = view.symbols.len();
    let shown = total.min(caps.symbols);
    truncated |= shown < total;
    let _ = writeln!(out, "\nPublic symbols: {}", count(total, shown));
    for symbol in view.symbols.iter().take(shown) {
        let _ = writeln!(out, "  {}", symbol_line(symbol));
    }

    let outgoing = view.outgoing.iter().map(|e| (&e.to, *e));
    let incoming = view.incoming.iter().map(|e| (&e.from, *e));
    truncated |= neighbors(out, "Depends on", outgoing, rolled, caps);
    truncated |= neighbors(out, "Used by", incoming, rolled, caps);
    if c.kind != ComponentKind::External {
        truncated |= not_mapped(out, view, caps);
    }
    truncated
}

/// Imports that no edge shows, one line per module (or per function that
/// loads modules by name) with why and where: the places to read in the
/// source instead of trusting the edges alone. Returns whether anything was
/// left out.
fn not_mapped(out: &mut String, view: &ComponentView, caps: &Caps) -> bool {
    const DYNAMIC: &str = "dynamic";
    let mut groups: BTreeMap<(&str, &str), Vec<&Evidence>> = BTreeMap::new();
    for import in &view.not_mapped {
        let why = match import.reason {
            UnmappedReason::Undeclared => "undeclared",
            UnmappedReason::DeclaredNotRequired => "extra or dev dependency",
            UnmappedReason::LocalName => "local name",
        };
        groups
            .entry((import.module.as_str(), why))
            .or_default()
            .push(&import.evidence);
    }
    for import in &view.dynamic_imports {
        groups
            .entry((import.call.as_str(), DYNAMIC))
            .or_default()
            .push(&import.evidence);
    }
    if groups.is_empty() {
        let _ = writeln!(out, "\nNot mapped: none");
        return false;
    }

    let mut list: Vec<((&str, &str), Vec<&Evidence>)> = groups.into_iter().collect();
    list.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0)));
    let total = list.len();
    let shown = total.min(caps.neighbors);
    let mut truncated = shown < total;
    let _ = writeln!(out, "\nNot mapped: {}", count(total, shown));

    let list = &list[..shown];
    let name_width = list.iter().map(|((n, _), _)| n.chars().count()).max();
    let why_width = list.iter().map(|((_, w), _)| w.chars().count()).max();
    let (name_width, why_width) = (name_width.unwrap_or(0), why_width.unwrap_or(0));
    for ((name, why), evidence) in list {
        let noun = if *why == DYNAMIC { "call" } else { "import" };
        let locations: Vec<String> = evidence
            .iter()
            .take(caps.locations)
            .map(|e| location(e))
            .collect();
        let more = evidence.len().saturating_sub(caps.locations);
        truncated |= more > 0;
        let mut line = format!(
            "  {name:<name_width$}  {why:<why_width$}  {}: {}",
            plural(evidence.len(), noun),
            locations.join(", ")
        );
        if more > 0 {
            let _ = write!(line, ", +{more} more");
        }
        let _ = writeln!(out, "{line}");
    }
    truncated
}

#[derive(Default)]
struct Neighbor<'a> {
    imports: Vec<&'a Evidence>,
    declared: BTreeSet<&'a str>,
    other: BTreeMap<&'static str, usize>,
}

/// One line per neighboring component: import statements (count and a few
/// locations), manifest declarations, other edge kinds. Returns whether
/// anything was left out.
fn neighbors<'a>(
    out: &mut String,
    title: &str,
    edges: impl Iterator<Item = (&'a ComponentId, &'a Edge)>,
    rolled: &ArchitectureGraph,
    caps: &Caps,
) -> bool {
    let mut by_id: BTreeMap<&ComponentId, Neighbor> = BTreeMap::new();
    for (id, edge) in edges {
        let n = by_id.entry(id).or_default();
        match edge.kind {
            EdgeKind::Import => {
                // one entry per statement: a statement can point at several files
                for e in &edge.evidence {
                    if !n
                        .imports
                        .iter()
                        .any(|x| x.file == e.file && x.line == e.line)
                    {
                        n.imports.push(e);
                    }
                }
            }
            EdgeKind::Dependency => n
                .declared
                .extend(edge.evidence.iter().map(|e| e.file.as_str())),
            kind => *n.other.entry(kind.as_str()).or_default() += edge.evidence.len().max(1),
        }
    }
    if by_id.is_empty() {
        let _ = writeln!(out, "\n{title}: none");
        return false;
    }

    let mut list: Vec<(&ComponentId, Neighbor)> = by_id.into_iter().collect();
    list.sort_by(|a, b| {
        b.1.imports
            .len()
            .cmp(&a.1.imports.len())
            .then_with(|| a.0.cmp(b.0))
    });
    let total = list.len();
    let shown = total.min(caps.neighbors);
    let mut truncated = shown < total;
    let _ = writeln!(out, "\n{title}: {}", count(total, shown));

    let names: Vec<String> = list
        .iter()
        .take(shown)
        .map(|(id, _)| display(rolled, id))
        .collect();
    let width = names.iter().map(|n| n.chars().count()).max().unwrap_or(0);
    for ((_, n), name) in list.iter().take(shown).zip(&names) {
        let mut parts = Vec::new();
        if !n.imports.is_empty() {
            let locations: Vec<String> = n
                .imports
                .iter()
                .take(caps.locations)
                .map(|e| location(e))
                .collect();
            let more = n.imports.len().saturating_sub(caps.locations);
            truncated |= more > 0;
            let mut part = format!(
                "{}: {}",
                plural(n.imports.len(), "import"),
                locations.join(", ")
            );
            if more > 0 {
                let _ = write!(part, ", +{more} more");
            }
            parts.push(part);
        }
        if !n.declared.is_empty() {
            let files: Vec<&str> = n.declared.iter().copied().collect();
            parts.push(format!("declared in {}", files.join(", ")));
        }
        for (kind, n) in &n.other {
            parts.push(format!("{n} {kind}"));
        }
        let _ = writeln!(out, "  {name:<width$}  {}", parts.join("; "));
    }
    truncated
}

fn symbol_list(
    out: &mut String,
    symbols: &[&Symbol],
    target: &str,
    rolled: &ArchitectureGraph,
    caps: &Caps,
) -> bool {
    let total = symbols.len();
    let shown = total.min(caps.symbols);
    let _ = writeln!(out, "Symbols matching `{target}`: {}", count(total, shown));
    for symbol in symbols.iter().take(shown) {
        let _ = writeln!(
            out,
            "  {}  in {}",
            symbol_line(symbol),
            display(rolled, &symbol.component)
        );
    }
    shown < total
}

/// `def pay(user: User) -> Payment  src/shop/billing/charge.py:25`. The
/// signature stands alone when it already names the symbol.
fn symbol_line(symbol: &Symbol) -> String {
    let qualified = symbol.name.contains('.') || symbol.name.contains("::");
    let what = match &symbol.signature {
        Some(signature) if !qualified => signature.clone(),
        Some(signature) => format!("{}: {signature}", symbol.name),
        None => format!("{} {}", symbol_kind(symbol.kind), symbol.name),
    };
    match symbol.evidence.first() {
        Some(evidence) => format!("{what}  {}", location(evidence)),
        None => what,
    }
}

/// Internal components by name, external ones by id (`ext:requests`).
fn display(graph: &ArchitectureGraph, id: &ComponentId) -> String {
    match graph.component(id) {
        Some(c) if c.kind != ComponentKind::External => c.name.clone(),
        _ => id.as_str().to_owned(),
    }
}

fn location(evidence: &Evidence) -> String {
    match evidence.line {
        Some(line) => format!("{}:{line}", evidence.file),
        None => evidence.file.clone(),
    }
}

fn count(total: usize, shown: usize) -> String {
    if shown < total {
        format!("{total}, showing {shown}")
    } else {
        total.to_string()
    }
}

fn plural(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

fn component_kind(kind: ComponentKind) -> &'static str {
    match kind {
        ComponentKind::Package => "package",
        ComponentKind::Module => "module",
        ComponentKind::External => "external",
    }
}

fn symbol_kind(kind: SymbolKind) -> &'static str {
    match kind {
        SymbolKind::Function => "function",
        SymbolKind::Struct => "struct",
        SymbolKind::Enum => "enum",
        SymbolKind::Trait => "trait",
        SymbolKind::TypeAlias => "type",
        SymbolKind::Constant => "constant",
        SymbolKind::Module => "module",
        SymbolKind::Other => "symbol",
    }
}
