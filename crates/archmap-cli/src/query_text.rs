//! Compact text rendering of `archmap query`, for agents and people.
//!
//! `query` is a drill-down: the local structure of one component, enough to
//! decide what to read next. Lists are capped and the rest is summarized as
//! counts, so a busy component cannot flood an agent's context. `--verbose`
//! lifts the caps; `--format json` carries every piece of evidence.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use archmap_core::{
    ArchitectureGraph, ComponentId, ComponentKind, DynamicImport, Edge, EdgeKind, Evidence, Scope,
    Symbol, SymbolKind, UnmappedImport, UnmappedReason,
};

use crate::views::{ComponentView, FileView, QueryResult, UnmappedView};

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
        QueryResult::File(view) => file(&mut out, view, rolled, &caps),
        QueryResult::Symbols(symbols) => symbol_list(&mut out, symbols, target, rolled, &caps),
        QueryResult::NotMapped(view) => unmapped_name(&mut out, view, full, rolled, &caps),
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
    truncated |= neighbors(out, "Depends on", outgoing, rolled, true, caps);
    truncated |= neighbors(out, "Used by", incoming, rolled, true, caps);
    if c.kind != ComponentKind::External {
        truncated |= not_mapped(out, &view.not_mapped, &view.dynamic_imports, caps);
    }
    truncated
}

/// Imports that no edge shows, one line per module (or per function that
/// loads modules by name) with why and where: the places to read in the
/// source instead of trusting the edges alone. Returns whether anything was
/// left out.
fn not_mapped(
    out: &mut String,
    unmapped: &[&UnmappedImport],
    dynamic: &[&DynamicImport],
    caps: &Caps,
) -> bool {
    const DYNAMIC: &str = "dynamic";
    let mut groups: BTreeMap<(&str, &str), Vec<&Evidence>> = BTreeMap::new();
    for import in unmapped {
        groups
            .entry((import.module.as_str(), reason_label(import.reason)))
            .or_default()
            .push(&import.evidence);
    }
    for import in dynamic {
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
            .map(|e| import_location(e, 0, false))
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

/// Why an import has no edge, in the words every command uses.
pub(crate) fn reason_label(reason: UnmappedReason) -> &'static str {
    match reason {
        UnmappedReason::Undeclared => "undeclared",
        UnmappedReason::DeclaredNotRequired => "extra or dev dependency",
        UnmappedReason::LocalName => "local name",
    }
}

#[derive(Default)]
struct Neighbor<'a> {
    /// One entry per import statement, with how many more files it loads.
    imports: Vec<(&'a Evidence, usize)>,
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
    show_targets: bool,
    caps: &Caps,
) -> bool {
    let mut by_id: BTreeMap<&ComponentId, Neighbor> = BTreeMap::new();
    for (id, edge) in edges {
        let n = by_id.entry(id).or_default();
        match edge.kind {
            EdgeKind::Import => {
                // one entry per statement: a statement can point at several files
                for e in &edge.evidence {
                    match n
                        .imports
                        .iter_mut()
                        .find(|(x, _)| x.file == e.file && x.line == e.line)
                    {
                        Some((first, more)) if e.target != first.target => *more += 1,
                        Some(_) => {}
                        None => n.imports.push((e, 0)),
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
                .map(|(e, more)| import_location(e, *more, show_targets))
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

/// A file-level drill-down: the file's public symbols, what it imports, who
/// imports it (where evidence records that), and its imports without an edge.
fn file(out: &mut String, view: &FileView, rolled: &ArchitectureGraph, caps: &Caps) -> bool {
    let component = view.component.as_ref().and_then(|id| rolled.component(id));
    let mut head = format!("{} (file)", view.file);
    if let Some(c) = component {
        let _ = write!(head, " in {} ({}", c.name, component_kind(c.kind));
        if let Some(language) = &c.language {
            let _ = write!(head, ", {language}");
        }
        head.push(')');
    }
    let _ = writeln!(out, "{head}, depth {}", view.depth);
    if let Some(c) = component {
        let _ = writeln!(out, "id: {}", c.id);
    }

    let total = view.symbols.len();
    let shown = total.min(caps.symbols);
    let mut truncated = shown < total;
    let _ = writeln!(out, "\nPublic symbols: {}", count(total, shown));
    for symbol in view.symbols.iter().take(shown) {
        let _ = writeln!(out, "  {}", symbol_line(symbol));
    }

    let imports = view.imports.iter().map(|e| (&e.to, e));
    truncated |= neighbors(out, "Imports", imports, rolled, true, caps);
    match &view.importers {
        Some(edges) => {
            let importers = edges.iter().map(|e| (&e.from, e));
            truncated |= neighbors(out, "Imported by", importers, rolled, false, caps);
        }
        None => {
            let language = component
                .and_then(|c| c.language.as_deref())
                .unwrap_or("this language");
            let _ = writeln!(
                out,
                "\nImported by: unknown (no evidence names imported files for {language})"
            );
        }
    }
    truncated |= not_mapped(out, &view.not_mapped, &view.dynamic_imports, caps);
    truncated
}

/// Imports without an edge of one import name and the modules below it:
/// which components import it, where, why no edge shows it, and what the
/// evidence notes add (such as where an extra is declared).
fn unmapped_name(
    out: &mut String,
    view: &UnmappedView,
    full: &ArchitectureGraph,
    rolled: &ArchitectureGraph,
    caps: &Caps,
) -> bool {
    let _ = writeln!(
        out,
        "{}: imports without an edge, depth {}",
        view.requested, view.depth
    );
    // Statements, not modules: `from torch import nn, Tensor` is one import
    // of two modules.
    let folded = |i: &UnmappedImport| full.ancestor_at(&i.from, view.depth);
    let mut statements: BTreeMap<ComponentId, BTreeSet<(&str, Option<u32>)>> = BTreeMap::new();
    for import in &view.not_mapped {
        statements
            .entry(folded(import))
            .or_default()
            .insert((import.evidence.file.as_str(), import.evidence.line));
    }
    let mut components: Vec<(ComponentId, usize)> = statements
        .into_iter()
        .map(|(id, s)| (id, s.len()))
        .collect();
    components.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let shown = components.len().min(caps.neighbors);
    let mut truncated = shown < components.len();
    let names: Vec<String> = components
        .iter()
        .take(shown)
        .map(|(id, n)| format!("{} {n}", display(rolled, id)))
        .collect();
    let mut line = format!("in: {}", names.join(", "));
    if truncated {
        let _ = write!(line, ", +{} more", components.len() - shown);
    }
    let _ = writeln!(out, "{line}");

    // One location per component first, in the order of `in:`, so the cap
    // cannot hide a lone importer elsewhere; then the rest by file and line.
    let rank: BTreeMap<&ComponentId, usize> = components
        .iter()
        .enumerate()
        .map(|(i, (id, _))| (id, i))
        .collect();
    let mut by_place: Vec<(&UnmappedImport, ComponentId)> =
        view.not_mapped.iter().map(|i| (*i, folded(i))).collect();
    by_place.sort_by(|a, b| {
        (&a.0.evidence.file, a.0.evidence.line).cmp(&(&b.0.evidence.file, b.0.evidence.line))
    });
    let mut seen = BTreeSet::new();
    let (mut first, rest): (Vec<_>, Vec<_>) = by_place
        .into_iter()
        .partition(|(i, c)| seen.insert((i.module.as_str(), i.reason, c.clone())));
    first.sort_by_key(|(_, c)| rank[c]);
    let ordered: Vec<&UnmappedImport> = first.into_iter().chain(rest).map(|(i, _)| i).collect();
    truncated |= not_mapped(out, &ordered, &[], caps);

    // A one-word note (`import`, `use`) only names the statement.
    let notes: BTreeSet<&str> = view
        .not_mapped
        .iter()
        .filter_map(|i| i.evidence.note.as_deref())
        .filter(|n| n.contains(' '))
        .collect();
    if !notes.is_empty() {
        let shown = notes.len().min(caps.locations);
        truncated |= shown < notes.len();
        let _ = writeln!(out, "\nNotes: {}", count(notes.len(), shown));
        for note in notes.iter().take(shown) {
            let _ = writeln!(out, "  {note}");
        }
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

/// `src/a.py:3 -> src/b.py (local)`: where the statement is, the file it
/// loads when the evidence names one (and how many more), and `(local)` when
/// it sits inside a function body, so it runs only when the function is
/// called.
fn import_location(evidence: &Evidence, more_files: usize, show_target: bool) -> String {
    let mut out = location(evidence);
    if let Some(target) = evidence.target.as_ref().filter(|_| show_target) {
        let _ = write!(out, " -> {target}");
        if more_files > 0 {
            let _ = write!(out, " (+{})", plural(more_files, "file"));
        }
    }
    if evidence.scope == Some(Scope::Local) {
        out.push_str(" (local)");
    }
    out
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
