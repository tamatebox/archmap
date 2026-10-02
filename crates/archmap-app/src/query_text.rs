//! Compact text rendering of `archmap query`, for agents and people.
//!
//! `query` is a drill-down: the local structure of one component, enough to
//! decide what to read next. Lists are capped and the rest is summarized as
//! counts, so a busy component cannot flood an agent's context. `--verbose`
//! lifts the caps; `--format json` carries every piece of evidence.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use archmap_core::{
    ArchitectureGraph, Component, ComponentId, ComponentKind, DynamicImport, Edge, EdgeKind,
    Evidence, Scope, Symbol, SymbolKind, SymbolUse, SymbolUses, UnmappedImport, UnmappedReason,
    UseRole, WHOLE_MODULE,
};

use crate::not_traced::NotTraced;
use crate::pairs::{Counted, Pairs};

use crate::views::{ComponentView, FileView, Importer, QueryResult, SymbolView, UnmappedView};

/// Default caps, lifted by `--verbose`.
const MAX_SYMBOLS: usize = 30;
const MAX_NEIGHBORS: usize = 30;
const MAX_LOCATIONS: usize = 3;
const MAX_IMPORTERS: usize = 5;
const MAX_USE_FILES: usize = 10;

struct Caps {
    symbols: usize,
    neighbors: usize,
    locations: usize,
    importers: usize,
    use_files: usize,
}

impl Caps {
    fn new(verbose: bool) -> Self {
        if verbose {
            Caps {
                symbols: usize::MAX,
                neighbors: usize::MAX,
                locations: usize::MAX,
                importers: usize::MAX,
                use_files: usize::MAX,
            }
        } else {
            Caps {
                symbols: MAX_SYMBOLS,
                neighbors: MAX_NEIGHBORS,
                locations: MAX_LOCATIONS,
                importers: MAX_IMPORTERS,
                use_files: MAX_USE_FILES,
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
    let pairs = Pairs::new(full);
    let mut out = String::new();
    let mut truncated = match result {
        QueryResult::Component(view) => component(&mut out, view, full, rolled, &pairs, &caps),
        QueryResult::File(view) => file(&mut out, view, rolled, &pairs, &caps),
        QueryResult::Symbols(symbols) => {
            symbol_list(&mut out, symbols, target, full, rolled, &caps)
        }
        QueryResult::NotMapped(view) => unmapped_name(&mut out, view, full, rolled, &caps),
    };
    // a file's text says "Imported by: none" without why; a symbol's says it
    let (not_traced, no_importers) = match result {
        QueryResult::Component(view) => (view.not_traced.as_ref(), false),
        QueryResult::File(view) => (view.not_traced.as_ref(), true),
        QueryResult::Symbols(symbols) => {
            (symbols.first().and_then(|v| v.not_traced.as_ref()), false)
        }
        QueryResult::NotMapped(_) => (None, false),
    };
    if let Some(not_traced) = not_traced {
        truncated |= self::not_traced(&mut out, not_traced, caps.locations, no_importers, false);
    }
    if truncated {
        let _ = writeln!(
            out,
            "\nLists are capped; JSON lists every entry with all evidence."
        );
    }
    out
}

fn component(
    out: &mut String,
    view: &ComponentView,
    full: &ArchitectureGraph,
    rolled: &ArchitectureGraph,
    pairs: &Pairs,
    caps: &Caps,
) -> bool {
    let c = view.component;
    component_head(
        out,
        c,
        view.depth,
        view.folded_from.as_ref(),
        view.subpath.as_deref(),
        full,
    );
    let mut truncated = namesakes(out, &view.also_named, &view.also_at_path, caps.neighbors);

    if !view.children.is_empty() {
        let total = view.children.len();
        let shown = total.min(caps.neighbors);
        truncated |= shown < total;
        let hint = if full.depth_of(&c.id) >= view.depth {
            format!(", folded at this depth: query at depth {}", view.depth + 1)
        } else {
            String::new()
        };
        let names: Vec<&str> = view
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
    for symbol in in_source_order(view.symbols.iter().copied())
        .into_iter()
        .take(shown)
    {
        let _ = writeln!(out, "  {}", symbol_line(symbol));
    }

    let outgoing = view.outgoing.iter().map(|e| (&e.to, *e));
    let incoming = view.incoming.iter().map(|e| (&e.from, *e));
    truncated |= neighbors(out, "Depends on", outgoing, rolled, pairs, true, caps);
    truncated |= neighbors(out, "Used by", incoming, rolled, pairs, true, caps);
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
        UnmappedReason::Unresolved => "unresolved",
    }
}

#[derive(Default)]
struct Neighbor<'a> {
    /// One entry per import statement, with the other files it loads and
    /// how it counts for the pair, as `summary` counts it.
    imports: Vec<(&'a Evidence, BTreeSet<Option<&'a str>>, Counted)>,
    /// Where manifests declare the dependency: each file, with the line
    /// when the declaration has one, in file and line order.
    declared: BTreeSet<(&'a str, Option<u32>)>,
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
    pairs: &Pairs,
    show_targets: bool,
    caps: &Caps,
) -> bool {
    let mut by_id: BTreeMap<&ComponentId, Neighbor> = BTreeMap::new();
    for (id, edge) in edges {
        let n = by_id.entry(id).or_default();
        match edge.kind {
            EdgeKind::Import => {
                let pair = pairs.pair(rolled, &edge.from, &edge.to);
                // one entry per statement: a statement can point at several
                // files, and at one file through several re-exports
                for e in &edge.evidence {
                    let counted = pair.counted(e);
                    match n
                        .imports
                        .iter_mut()
                        .find(|(x, ..)| x.file == e.file && x.line == e.line)
                    {
                        Some((first, more, c)) => {
                            *c = (*c).min(counted);
                            if e.target != first.target {
                                more.insert(e.target.as_deref());
                            } else if first.type_only && !e.type_only {
                                // a statement that takes values and types
                                // from one file shows as what runs
                                *first = e;
                            }
                        }
                        None => n.imports.push((e, BTreeSet::new(), counted)),
                    }
                }
            }
            EdgeKind::Dependency => n
                .declared
                .extend(edge.evidence.iter().map(|e| (e.file.as_str(), e.line))),
            kind => *n.other.entry(kind.as_str()).or_default() += edge.evidence.len().max(1),
        }
    }
    if by_id.is_empty() {
        let _ = writeln!(out, "\n{title}: none");
        return false;
    }

    let mut list: Vec<(&ComponentId, Neighbor)> = by_id.into_iter().collect();
    for (_, n) in &mut list {
        // production code first, then tests, then what counts for no pair
        n.imports.sort_by_key(|(.., c)| *c);
    }
    let of_kind =
        |n: &Neighbor, kind: Counted| n.imports.iter().filter(|(.., c)| *c == kind).count();
    list.sort_by(|a, b| {
        of_kind(&b.1, Counted::Production)
            .cmp(&of_kind(&a.1, Counted::Production))
            .then_with(|| of_kind(&b.1, Counted::Test).cmp(&of_kind(&a.1, Counted::Test)))
            .then_with(|| a.0.cmp(b.0))
    });
    let total = list.len();
    let shown = total.min(caps.neighbors);
    let mut truncated = shown < total;
    let _ = writeln!(out, "\n{title}: {}", count(total, shown));

    let names: Vec<&str> = list
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
                .map(|(e, more, c)| {
                    let mut at = import_location(e, more.len(), show_targets);
                    if *c == Counted::Through {
                        at.push_str(" (through)");
                    }
                    at
                })
                .collect();
            let more = n.imports.len().saturating_sub(caps.locations);
            truncated |= more > 0;
            let mut counts = Vec::new();
            let production = of_kind(n, Counted::Production);
            if production > 0 {
                counts.push(plural(production, "import"));
            }
            match of_kind(n, Counted::Test) {
                0 => {}
                tests if counts.is_empty() => {
                    counts.push(format!("{} in tests", plural(tests, "import")))
                }
                tests => counts.push(format!("{tests} in tests")),
            }
            for (kind, what) in [
                (Counted::Through, "through re-exports"),
                (Counted::Entry, "of its entry file"),
            ] {
                let k = of_kind(n, kind);
                if k > 0 {
                    counts.push(format!("{k} {what}"));
                }
            }
            let mut part = format!("{}: {}", counts.join(", "), locations.join(", "));
            if more > 0 {
                let _ = write!(part, ", +{more} more");
            }
            parts.push(part);
        }
        if !n.declared.is_empty() {
            let places: Vec<String> = n
                .declared
                .iter()
                .map(|(file, line)| match line {
                    Some(line) => format!("{file}:{line}"),
                    None => (*file).to_owned(),
                })
                .collect();
            parts.push(format!("declared in {}", places.join(", ")));
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
fn file(
    out: &mut String,
    view: &FileView,
    rolled: &ArchitectureGraph,
    pairs: &Pairs,
    caps: &Caps,
) -> bool {
    let component = view.component.as_ref().and_then(|id| rolled.component(id));
    file_head(out, &view.file, component, view.depth);
    let mut truncated = namesakes(out, &view.also_named, &view.also_at_path, caps.neighbors);

    let total = view.symbols.len();
    let shown = total.min(caps.symbols);
    truncated |= shown < total;
    let _ = writeln!(out, "\nPublic symbols: {}", count(total, shown));
    for symbol in in_source_order(view.symbols.iter().copied())
        .into_iter()
        .take(shown)
    {
        let _ = writeln!(out, "  {}", symbol_line(symbol));
    }

    let imports = view.imports.iter().map(|e| (&e.to, e));
    truncated |= neighbors(out, "Imports", imports, rolled, pairs, true, caps);
    match &view.importers {
        // a side-effect import can still load a script
        Some(edges) if view.script && edges.is_empty() => {
            let _ = writeln!(
                out,
                "\nImported by: none (a script: its declarations are global, so what uses them \
                 is not traced)"
            );
        }
        Some(edges) => {
            let importers = edges.iter().map(|e| (&e.from, e));
            truncated |= neighbors(out, "Imported by", importers, rolled, pairs, false, caps);
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
    if !view.imports_below.is_empty() {
        let statements: BTreeSet<(&str, Option<u32>)> = view
            .imports_below
            .iter()
            .flat_map(|e| &e.evidence)
            .map(|e| (e.file.as_str(), e.line))
            .collect();
        let _ = writeln!(
            out,
            "\nImports below: {} (they run it first): `impact {}` lists them",
            statements.len(),
            shell_word(&view.file)
        );
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
    symbols: &[SymbolView],
    target: &str,
    full: &ArchitectureGraph,
    rolled: &ArchitectureGraph,
    caps: &Caps,
) -> bool {
    let total = symbols.len();
    let shown = total.min(caps.symbols);
    let _ = writeln!(out, "Symbols matching `{target}`: {}", count(total, shown));
    for view in symbols.iter().take(shown) {
        let mut line = format!(
            "  {}  in {}",
            symbol_line(view.symbol),
            display(rolled, &view.symbol.component)
        );
        if let ([_, _, ..], Some(by_name), Some(may_use)) =
            (symbols, &view.imported_by, &view.may_use)
        {
            let _ = write!(
                line,
                "  imported by {}, may use {}",
                by_name.len(),
                may_use.len()
            );
        }
        let _ = writeln!(out, "{line}");
    }
    let mut truncated = shown < total;
    match symbols {
        [one] => {
            truncated |= importers(out, one, full, rolled, caps);
            if let Some(uses) = &one.used_at {
                truncated |= used_at(out, uses, one.instance_method, caps);
            }
        }
        [] => {}
        [first, ..] => {
            let _ = writeln!(
                out,
                "\nQuery one by its id, such as `{}`, for the statements that import it.",
                shell_word(first.symbol.id.as_str())
            );
        }
    }
    truncated
}

/// The statements that import one symbol: those that take its name, then
/// those that take its file whole.
fn importers(
    out: &mut String,
    view: &SymbolView,
    full: &ArchitectureGraph,
    rolled: &ArchitectureGraph,
    caps: &Caps,
) -> bool {
    // a Rust module is a symbol of the file that declares it and a
    // component of its own; imports name its own file
    let id = ComponentId::new(view.symbol.id.as_str());
    if full.component(&id).is_some() {
        let _ = writeln!(
            out,
            "\n`{}` is a module: `query {id}` lists what imports it",
            view.symbol.name
        );
        return false;
    }
    let (Some(by_name), Some(may_use)) = (&view.imported_by, &view.may_use) else {
        let language = rolled
            .component(&view.symbol.component)
            .and_then(|c| c.language.as_deref())
            .unwrap_or("this language");
        let _ = writeln!(
            out,
            "\nImported by: unknown (no evidence names imported files for {language})"
        );
        return false;
    };
    let mut truncated = false;
    // the component that declares it, before roll-up folds it away
    let script = full
        .symbol(&view.symbol.id)
        .and_then(|s| full.component(&s.component))
        .is_some_and(|c| c.kind == ComponentKind::Script);
    if by_name.is_empty() && script {
        let _ = writeln!(
            out,
            "\nImported by: none (a script declares it globally: what uses it is not traced)"
        );
    } else if by_name.is_empty() {
        let _ = writeln!(
            out,
            "\nImported by: none resolved\n  (only import statements are read; code that a \
             framework or runtime loads by name or path is not seen)"
        );
    } else {
        truncated |= sites(out, "Imported by", None, "from", by_name, caps);
    }
    if !may_use.is_empty() {
        truncated |= sites(
            out,
            "May use",
            Some("imports the whole module"),
            "whole",
            may_use,
            caps,
        );
    }
    truncated
}

/// Where one symbol is used: a line per file, production code first, then
/// the files with the most uses, then by path, and the imports whose
/// binding is never used. A method that is not static says that the list
/// holds only the uses through its class and `this`, so that an empty list
/// never reads as unused.
fn used_at(out: &mut String, uses: &SymbolUses, instance_method: bool, caps: &Caps) -> bool {
    // the calls through values and subclasses are under `Not traced`
    let partial = instance_method || !uses.subclasses.is_empty();
    let lead = match partial {
        true => "through the class and this only: ",
        false => "",
    };
    let mut truncated = false;
    if uses.uses.is_empty() {
        let _ = match partial {
            true => writeln!(out, "\nUsed at: {lead}none"),
            false => writeln!(
                out,
                "\nUsed at: none found\n  (only code that names it is read: a framework that \
                 loads it by path, a string or a value of a type is not seen)"
            ),
        };
    } else {
        let mut by_file: BTreeMap<&str, Vec<&SymbolUse>> = BTreeMap::new();
        for found in &uses.uses {
            by_file.entry(&found.evidence.file).or_default().push(found);
        }
        let mut files: Vec<(&str, Vec<&SymbolUse>)> = by_file.into_iter().collect();
        files.sort_by_key(|(file, list)| {
            (
                list.iter().all(|u| u.evidence.test),
                std::cmp::Reverse(list.len()),
                *file,
            )
        });
        let shown = files.len().min(caps.use_files);
        truncated |= shown < files.len();
        let mut roles: BTreeMap<UseRole, usize> = BTreeMap::new();
        for found in &uses.uses {
            *roles.entry(found.role).or_default() += 1;
        }
        let roles: Vec<String> = roles
            .into_iter()
            .map(|(role, n)| role_count(role, n))
            .collect();
        let showing = match shown < files.len() {
            true => format!(", showing {shown}"),
            false => String::new(),
        };
        let _ = writeln!(
            out,
            "\nUsed at: {lead}{} in {}{showing} ({})",
            uses.uses.len(),
            plural(files.len(), "file"),
            roles.join(", ")
        );
        for (_, list) in files.iter().take(shown) {
            // two uses that would read alike say their columns
            let mut alike: BTreeMap<String, usize> = BTreeMap::new();
            for found in list {
                *alike.entry(use_location(found, false)).or_default() += 1;
            }
            let locations: Vec<String> = list
                .iter()
                .take(caps.locations)
                .map(|u| use_location(u, alike[&use_location(u, false)] > 1))
                .collect();
            truncated |= locations.len() < list.len();
            let mut line = locations.join(", ");
            if list.len() > locations.len() {
                let _ = write!(
                    line,
                    ", +{} more in this file",
                    list.len() - locations.len()
                );
            }
            let _ = writeln!(out, "  {line}");
        }
    }
    // an import of its name that nothing uses, apart from an import of the
    // module whole that never names it
    let (mut whole, mut named): (Vec<&Evidence>, Vec<&Evidence>) = uses
        .unused
        .iter()
        .partition(|e| e.names.contains(WHOLE_MODULE));
    // two statements on one line are one place
    for list in [&mut whole, &mut named] {
        list.dedup_by(|a, b| a.file == b.file && a.line == b.line);
    }
    for (list, what) in [
        (named, "never used ({})"),
        (whole, "never named ({} of the whole module)"),
    ] {
        if list.is_empty() {
            continue;
        }
        let places: Vec<String> = list
            .iter()
            .take(caps.locations)
            .map(|e| location(e))
            .collect();
        truncated |= places.len() < list.len();
        let count = what.replace("{}", &plural(list.len(), "import"));
        let _ = writeln!(out, "  {count}: {}", with_more(&places, list.len()));
    }
    truncated
}

/// A use as `file:line` (`file:line:column` with `column`), its role and
/// test marks, and the name it is used by when that is not the symbol's own
/// (`as fp`, `as m.formatPrice`).
fn use_location(found: &SymbolUse, column: bool) -> String {
    let mut out = location(&found.evidence);
    if column {
        let _ = write!(out, ":{}", found.column);
    }
    let _ = write!(out, " ({})", found.role.as_str());
    if found.evidence.test {
        out.push_str(" (test)");
    }
    if let Some(binding) = &found.binding {
        let _ = write!(out, " as {binding}");
    }
    out
}

/// `3 calls`, `1 JSX element`, `2 new`, `1 read`: how many uses have a
/// role.
fn role_count(role: UseRole, n: usize) -> String {
    match role {
        UseRole::Call => plural(n, "call"),
        UseRole::New => format!("{n} new"),
        UseRole::Jsx => plural(n, "JSX element"),
        UseRole::Type => plural(n, "type"),
        UseRole::Read => format!("{n} read"),
    }
}

/// The statements of one list; one that reaches the symbol through a
/// barrel names it, `taken` saying how it takes the barrel (`from` for the
/// name, `whole`).
fn sites(
    out: &mut String,
    title: &str,
    note: Option<&str>,
    taken: &str,
    list: &[Importer],
    caps: &Caps,
) -> bool {
    let shown = list.len().min(caps.importers);
    let exports = list
        .iter()
        .filter(|i| i.evidence.note.as_deref() == Some("export"))
        .count();
    let _ = writeln!(
        out,
        "\n{}",
        statements_title(title, note, list.len(), shown, exports)
    );
    for importer in list.iter().take(shown) {
        let mut line = import_location(importer.evidence, 0, false);
        if let Some(barrel) = importer.through {
            let _ = write!(line, " ({taken} {barrel}, which passes it on)");
        }
        let _ = writeln!(out, "  {line}");
    }
    shown < list.len()
}

/// `Imported by: 6, showing 5 (1 re-export)`: the heading of a list of
/// import statements, with what it notes about them and how many of them
/// are re-exports.
pub(crate) fn statements_title(
    title: &str,
    note: Option<&str>,
    total: usize,
    shown: usize,
    exports: usize,
) -> String {
    let mut notes: Vec<String> = note.map(str::to_owned).into_iter().collect();
    if exports > 0 {
        notes.push(plural(exports, "re-export"));
    }
    let notes = match notes.is_empty() {
        true => String::new(),
        false => format!(" ({})", notes.join("; ")),
    };
    format!("{title}: {}{notes}", count(total, shown))
}

/// Symbols as the source orders them, by file and then line; JSON keeps
/// them by id, a stable order to diff.
fn in_source_order<'a>(symbols: impl IntoIterator<Item = &'a Symbol>) -> Vec<&'a Symbol> {
    let place = |s: &Symbol| s.location().map(|e| (e.file.clone(), e.line));
    let mut sorted: Vec<&Symbol> = symbols.into_iter().collect();
    sorted.sort_by(|a, b| place(a).cmp(&place(b)).then_with(|| a.id.cmp(&b.id)));
    sorted
}

/// `def pay(user: User) -> Payment  src/shop/billing/charge.py:25`. The
/// signature stands alone when it already names the symbol.
pub(crate) fn symbol_line(symbol: &Symbol) -> String {
    let qualified = symbol.name.contains('.') || symbol.name.contains("::");
    let what = match &symbol.signature {
        Some(signature) if !qualified => signature.clone(),
        Some(signature) => format!("{}: {signature}", symbol.name),
        None => format!("{} {}", symbol_kind(symbol.kind), symbol.name),
    };
    match symbol.location() {
        Some(evidence) => format!("{what}  {}", location(evidence)),
        None => what,
    }
}

/// The first lines of an answer about a component: `name (kind, language)
/// at path, depth N`, its id, and the component asked for when it folds
/// into this one, and the package subpath asked for.
pub(crate) fn component_head(
    out: &mut String,
    c: &Component,
    depth: usize,
    folded_from: Option<&ComponentId>,
    subpath: Option<&str>,
    full: &ArchitectureGraph,
) {
    let mut head = format!("{} ({}", c.name, component_kind(c.kind));
    if let Some(language) = &c.language {
        let _ = write!(head, ", {language}");
    }
    head.push(')');
    if let Some(path) = &c.path {
        let _ = write!(head, " at {path}");
    }
    let _ = writeln!(out, "{head}, depth {depth}");
    let _ = writeln!(out, "id: {}", c.id);
    if let Some(from) = folded_from {
        let _ = writeln!(out, "folded from: {}", display(full, from));
    }
    if let Some(subpath) = subpath {
        let _ = writeln!(out, "subpath: {subpath}");
    }
}

/// The first lines of an answer about a file: `file (file) in name (kind,
/// language), depth N`, and the id of the component that holds it.
pub(crate) fn file_head(out: &mut String, file: &str, component: Option<&Component>, depth: usize) {
    let mut head = format!("{file} (file)");
    if let Some(c) = component {
        let _ = write!(head, " in {} ({}", c.name, component_kind(c.kind));
        if let Some(language) = &c.language {
            let _ = write!(head, ", {language}");
        }
        head.push(')');
    }
    let _ = writeln!(out, "{head}, depth {depth}");
    if let Some(c) = component {
        let _ = writeln!(out, "id: {}", c.id);
    }
}

/// The `also named:` and `also at this path:` lines: the other components
/// that share the name or the path of the one answered for, `cap` of each.
/// Whether a list was cut.
pub(crate) fn namesakes(
    out: &mut String,
    also_named: &[&ComponentId],
    also_at_path: &[&ComponentId],
    cap: usize,
) -> bool {
    let mut truncated = false;
    for (label, ids) in [
        ("also named", also_named),
        ("also at this path", also_at_path),
    ] {
        if ids.is_empty() {
            continue;
        }
        let shown: Vec<String> = ids
            .iter()
            .take(cap)
            .map(|id| shell_word(id.as_str()))
            .collect();
        let mut line = format!("{label}: {}", shown.join(", "));
        if ids.len() > shown.len() {
            truncated = true;
            let _ = write!(line, ", +{} more", ids.len() - shown.len());
        }
        let _ = writeln!(out, "{line}");
    }
    truncated
}

/// How a list names a component, in `query` and `summary` alike: an
/// internal one by its name, or by its id when several components share the
/// name (`types.ts` in each package of a monorepo); an external one by its
/// id (`ext:pypi:requests`).
pub(crate) fn display<'a>(graph: &'a ArchitectureGraph, id: &'a ComponentId) -> &'a str {
    match graph.component(id) {
        Some(c)
            if c.kind != ComponentKind::External
                && graph.components_named(&c.name).nth(1).is_none() =>
        {
            c.name.as_str()
        }
        _ => id.as_str(),
    }
}

/// `src/a.py:3 -> src/b.py (via src/c.py:4) (local)`: where the statement
/// is, the file it loads when the evidence names one (and how many more),
/// the re-export it went through when its note says so, and `(local)` when
/// it sits inside a function body, so it runs only when the function is
/// called.
pub(crate) fn import_location(evidence: &Evidence, more_files: usize, show_target: bool) -> String {
    let mut out = location(evidence);
    if let Some(target) = evidence.target.as_ref().filter(|_| show_target) {
        let _ = write!(out, " -> {target}");
        if more_files > 0 {
            let _ = write!(out, " (+{})", plural(more_files, "file"));
        }
    }
    if let Some(place) = evidence.note.as_deref().and_then(crate::pairs::via_place) {
        let _ = write!(out, " (via {place})");
    }
    // a re-export statement passes names on: not a use of them
    if evidence.note.as_deref() == Some("export") {
        out.push_str(" (export)");
    }
    // a mock that replaces the module for its file's whole run
    if evidence.replaces {
        out.push_str(" (mock)");
    }
    // types only: erased before the program runs
    if evidence.type_only {
        out.push_str(" (type)");
    }
    if evidence.test {
        out.push_str(" (test)");
    }
    if evidence.scope == Some(Scope::Local) {
        out.push_str(" (local)");
    }
    out
}

/// `word` as a shell reads it back: in single quotes when it holds a
/// character the shell would expand or split on (TS/JS paths such as
/// `app/(public)/[slug]/page.tsx`).
pub(crate) fn shell_word(word: &str) -> String {
    let plain = word
        .chars()
        .all(|c| c.is_alphanumeric() || "_-./:@+,%".contains(c));
    if plain {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
    }
}

fn location(evidence: &Evidence) -> String {
    match evidence.line {
        Some(line) => format!("{}:{line}", evidence.file),
        None => evidence.file.clone(),
    }
}

pub(crate) fn count(total: usize, shown: usize) -> String {
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

pub(crate) fn component_kind(kind: ComponentKind) -> &'static str {
    match kind {
        ComponentKind::Package => "package",
        ComponentKind::Module => "module",
        ComponentKind::Script => "script",
        ComponentKind::External => "external",
    }
}

pub(crate) fn symbol_kind(kind: SymbolKind) -> &'static str {
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

/// The `Not traced` section at the end of `query` and `impact`. `query`
/// says a script where its text shows importers, and a symbol nothing
/// imports, so not again here: `script` and `no_importers` say them when
/// asked to. Shows `cap` locations per kind; returns whether some were left
/// out.
pub(crate) fn not_traced(
    out: &mut String,
    found: &NotTraced,
    cap: usize,
    no_importers: bool,
    script: bool,
) -> bool {
    let mut lines = Vec::new();
    if let Some(note) = found.script.filter(|_| script) {
        lines.push(format!("  {note}"));
    }
    if let Some(why) = found.no_importers.filter(|_| no_importers) {
        lines.push(format!("  no importers: {why}"));
    }
    let mut truncated = false;
    if let Some(v) = &found.values {
        let mut line = format!("  values: {}", v.note);
        if v.total > 0 {
            let places: Vec<String> = v
                .shown
                .iter()
                .take(cap)
                .map(|s| {
                    let at = place(&s.file, s.line);
                    if s.test {
                        format!("{at} (test)")
                    } else {
                        at
                    }
                })
                .collect();
            truncated |= places.len() < v.total;
            let _ = write!(
                line,
                "; {} of the class or its module may make them: {}",
                plural(v.total, "import"),
                with_more(&places, v.total)
            );
        }
        lines.push(line);
    }
    if let Some(sub) = &found.subclasses {
        let places: Vec<String> = sub
            .shown
            .iter()
            .take(cap)
            .map(|s| {
                let at = place(&s.file, s.line);
                if s.test {
                    format!("{at} (test)")
                } else {
                    at
                }
            })
            .collect();
        truncated |= places.len() < sub.total;
        lines.push(format!(
            "  subclasses: calls through a subclass (Sub.m(), super.m()) are not read; \
             extended at {}",
            with_more(&places, sub.total)
        ));
    }
    if let Some(w) = &found.whole_module {
        let what = match w.total {
            1 => "1 place uses the module as a value".to_owned(),
            n => format!("{n} places use the module as a value"),
        };
        let places: Vec<String> = w
            .shown
            .iter()
            .take(cap)
            .map(|s| {
                let at = place(&s.file, s.line);
                if s.test {
                    format!("{at} (test)")
                } else {
                    at
                }
            })
            .collect();
        truncated |= places.len() < w.total;
        lines.push(format!(
            "  whole module: {what}, which may use this: {}",
            with_more(&places, w.total)
        ));
    }
    if !found.renamed.is_empty() {
        let names: Vec<String> = found
            .renamed
            .iter()
            .take(cap)
            .map(|r| format!("as `{}` by {}", r.name, place(&r.file, r.line)))
            .collect();
        truncated |= names.len() < found.renamed.len();
        lines.push(format!(
            "  renamed: passed on {}; what takes that name is not followed",
            with_more(&names, found.renamed.len())
        ));
    }
    if let Some(d) = &found.dynamic {
        let what = if d.total == 1 {
            "1 call loads a module by a computed name".to_owned()
        } else {
            format!("{} calls load modules by computed names", d.total)
        };
        let places: Vec<String> = d
            .shown
            .iter()
            .take(cap)
            .map(|c| {
                let at = place(&c.file, c.line);
                if c.test {
                    format!("{at} (test)")
                } else {
                    at
                }
            })
            .collect();
        truncated |= places.len() < d.total;
        lines.push(format!(
            "  dynamic: {what}, which may be this: {}",
            with_more(&places, d.total)
        ));
    }
    if let Some(n) = &found.named_like {
        let what = if n.total == 1 {
            format!("1 import of `{}` maps to no file", n.name)
        } else {
            format!("{} imports of `{}` map to no file", n.total, n.name)
        };
        let places: Vec<String> = n
            .shown
            .iter()
            .take(cap)
            .map(|i| format!("{} ({})", place(&i.file, i.line), reason_label(i.reason)))
            .collect();
        truncated |= places.len() < n.total;
        lines.push(format!(
            "  named like it: {what}: {}",
            with_more(&places, n.total)
        ));
    }
    if let Some(m) = &found.macros {
        let what = if m.total == 1 {
            format!(
                "1 macro call whose arguments are not read names `{}`",
                m.name
            )
        } else {
            format!(
                "{} macro calls whose arguments are not read name `{}`",
                m.total, m.name
            )
        };
        let places: Vec<String> = m
            .shown
            .iter()
            .take(cap)
            .map(|c| {
                let at = format!("{} ({}!)", place(&c.file, c.line), c.name);
                if c.test {
                    format!("{at} (test)")
                } else {
                    at
                }
            })
            .collect();
        truncated |= places.len() < m.total;
        lines.push(format!("  macros: {what}: {}", with_more(&places, m.total)));
    }
    if let Some(u) = &found.uses {
        let places: Vec<String> = u
            .shown
            .iter()
            .take(cap)
            .map(|s| format!("{} ({})", place(&s.file, s.line), s.reason.as_str()))
            .collect();
        truncated |= places.len() < u.total;
        lines.push(format!(
            "  uses: not read in {}: {}",
            plural(u.total, "place"),
            with_more(&places, u.total)
        ));
    }
    if let Some(r) = &found.not_read {
        let mut line = format!(
            "  not read: {} of {} {} files",
            r.files - r.read,
            r.files,
            r.languages.join(" and ")
        );
        if let Some(note) = r.note {
            let _ = write!(line, ": {note}");
        }
        lines.push(line);
    }
    if !lines.is_empty() {
        let _ = writeln!(out, "\nNot traced:");
        for line in lines {
            let _ = writeln!(out, "{line}");
        }
    }
    truncated
}

pub(crate) fn place(file: &str, line: Option<u32>) -> String {
    match line {
        Some(line) => format!("{file}:{line}"),
        None => file.to_owned(),
    }
}

/// `a, b, +N more`: the shown entries, and how many of `total` were left out.
pub(crate) fn with_more(shown: &[String], total: usize) -> String {
    let mut out = shown.join(", ");
    if total > shown.len() {
        let _ = write!(out, ", +{} more", total - shown.len());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_the_shell_would_expand_are_quoted() {
        for plain in [
            "a::b",
            "ts-shop::src/lib/money.ts::formatPrice",
            "a@b+c,d%e",
        ] {
            assert_eq!(shell_word(plain), plain);
        }
        // zsh expands these: globs, `~user`, `^` and `~` with
        // EXTENDED_GLOB, `=cmd`
        for (word, quoted) in [
            (
                "x::src/app/(public)/page.tsx::A",
                "'x::src/app/(public)/page.tsx::A'",
            ),
            ("x::src/[slug]/page.tsx", "'x::src/[slug]/page.tsx'"),
            ("~a", "'~a'"),
            ("a^b", "'a^b'"),
            ("=a", "'=a'"),
            ("it's", "'it'\\''s'"),
        ] {
            assert_eq!(shell_word(word), quoted, "{word}");
        }
    }

    #[test]
    fn every_reason_has_a_label() {
        assert_eq!(reason_label(UnmappedReason::Unresolved), "unresolved");
        assert_eq!(reason_label(UnmappedReason::LocalName), "local name");
    }
}
