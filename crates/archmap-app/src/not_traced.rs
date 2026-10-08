//! What `query` and `impact` could not follow for their target, counted
//! only from what the analyzers record and said only when something
//! applies. A target's own imports without an edge stay under `Not mapped`;
//! this is what could reach the target unseen.

use std::collections::{BTreeMap, BTreeSet};

use archmap_core::{
    ArchitectureGraph, Component, ComponentId, ComponentKind, DynamicImport, EdgeKind, EnvUses,
    Evidence, ImportPlace, Symbol, SymbolUses, UnmappedImport, UnmappedReason, UnreadMacro,
    UnreadReason,
};
use serde::Serialize;

use archmap_scan::ScanReport;

use crate::target::test_files;

/// What could not be traced to a target.
#[derive(Debug, Default, Serialize)]
pub struct NotTraced {
    /// Calls elsewhere in the target's language that load modules by
    /// computed names: any of them may load the target.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) dynamic: Option<Dynamic>,
    /// Imports without an edge named like the target, which no file
    /// resolved: they may be the target, unresolved. A name match, not an
    /// import of it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) named_like: Option<NamedLike>,
    /// Macro calls whose arguments were not read and whose paths name the
    /// target: they may use it unseen. A name match, not a use of it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) macros: Option<Macros>,
    /// Files of the target's language that its analyzer did not read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) not_read: Option<NotRead>,
    /// The target is a script, whose globals no import names; the value
    /// says so in words.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) script: Option<&'static str>,
    /// The target is, or its file holds, a declaration in a module's
    /// `declare global`, whose uses no import names; the value says so in
    /// words.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) global: Option<&'static str>,
    /// Importers of the target are recorded and none exists; the value says
    /// why that is no proof of no use. Never for a test file, which its
    /// runner loads.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) no_importers: Option<&'static str>,
    /// Places where a binding of a symbol's module whole is used other than
    /// by a static name (passed as a value, `ns[key]`): that code may use
    /// the symbol unseen.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) whole_module: Option<Spots>,
    /// Places where the class of a static member is used as a value
    /// (passed, kept in a variable, `C[key]`): that code may call the
    /// member unseen.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) class_values: Option<Spots>,
    /// Strings that name a symbol by its dotted path
    /// (`mock.patch("shop.charge.pay")`): code that looks the name up there
    /// may use the symbol unseen.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) strings: Option<Spots>,
    /// Statements and files whose uses of a symbol were not read, with why.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) uses: Option<UsesNotRead>,
    /// A method that is not static, which code calls through a value of its
    /// type unseen: what reading those calls needs, and the statements that
    /// bind its class without another use the pass reads.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) values: Option<Values>,
    /// The places that extend a member's class: calls through a subclass
    /// (`Rich.open()`, `super.open()`) are not read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) subclasses: Option<Spots>,
    /// For `impact`: the re-exports that pass the target's names on, past
    /// which it follows only what takes those names. A rename, a removal or
    /// an error on load breaks whatever else loads them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) barrels: Option<Barrels>,
    /// For `impact`: what in the history read may hide files changed in
    /// the same commits as the target.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) history: Option<HistoryGaps>,
    /// For an environment variable: what may read it unseen.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) env: Option<EnvGaps>,
    /// For a name taken from a package: the statements that pass it on
    /// (`export { x } from 'pkg'`), whose files' importers are not read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) relays: Option<Spots>,
    /// For `impact`: the files changed or reached that a framework loads
    /// for a URL: tests that reach them through it are not listed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) routes: Option<Routes>,
    /// For `impact`: the files changed or reached that a framework runs
    /// before the requests of every URL they match, which tests of any URL
    /// may reach.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) middleware: Option<Routes>,
}

/// What an environment variable's answer may miss: reads of the
/// environment by a computed key or whole, where it is set, and the
/// languages whose reads of it are not read.
#[derive(Debug, Serialize)]
pub(crate) struct EnvGaps {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) computed: Option<Spots>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) whole: Option<Spots>,
    pub(crate) set: &'static str,
    pub(crate) forms: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) languages: Vec<String>,
}

pub(crate) const ENV_FORMS: &str = "`globalThis.process.env`, Bun's `Bun.env` and Deno's \
     `Deno.env.get()` are not read, nor a helper that wraps the environment (`createEnv`)";

pub(crate) const ENV_SET: &str = "where its value is set is not read: `.env` files, deployment \
     settings, a framework's config (`next.config` `env`)";

impl EnvGaps {
    /// The gaps of `uses`, in a scan whose other languages are those of
    /// `full`'s Coverage.
    pub(crate) fn of(full: &ArchitectureGraph, uses: &EnvUses) -> EnvGaps {
        let spots = |list: &[Evidence]| {
            (!list.is_empty()).then(|| Spots {
                total: list.len(),
                shown: list
                    .iter()
                    .map(|e| Spot {
                        file: e.file.clone(),
                        line: e.line,
                        test: e.test,
                    })
                    .collect(),
            })
        };
        EnvGaps {
            computed: spots(&uses.computed),
            whole: spots(&uses.whole),
            set: ENV_SET,
            forms: ENV_FORMS,
            languages: full
                .meta
                .coverage
                .iter()
                .filter(|(language, c)| {
                    c.files > 0 && !matches!(language.as_str(), "typescript" | "javascript")
                })
                .map(|(language, _)| language.clone())
                .collect(),
        }
    }
}

/// Files a framework loads by their path, by path.
#[derive(Debug, Serialize)]
pub(crate) struct Routes {
    pub(crate) total: usize,
    #[serde(rename = "files")]
    pub(crate) shown: Vec<String>,
}

/// The route files among `files`, those the change starts from or reaches,
/// and the files run before every request, when there are any.
pub(crate) fn routes(
    report: &ScanReport,
    files: &BTreeSet<String>,
) -> (Option<Routes>, Option<Routes>) {
    let listed = |found: BTreeSet<&str>| {
        let total = found.len();
        (total > 0).then(|| Routes {
            total,
            shown: found.into_iter().map(str::to_owned).collect(),
        })
    };
    let paths = || files.iter().map(String::as_str);
    (
        listed(archmap_scan::route_files(report, paths())),
        listed(archmap_scan::before_routes(report, paths())),
    )
}

/// Each in words: `the clone is shallow, so the history ends at its depth`.
#[derive(Debug, Serialize)]
pub(crate) struct HistoryGaps {
    pub(crate) gaps: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Barrels {
    pub(crate) total: usize,
    #[serde(rename = "locations")]
    pub(crate) shown: Vec<Barrel>,
}

/// A file that passes the target's names on, at its re-export on the
/// nearest way: of the target, or for a package entry the reach went on
/// from only through its re-exports, of the nearest file it came from.
#[derive(Debug, Serialize)]
pub(crate) struct Barrel {
    pub(crate) file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) line: Option<u32>,
    /// Every re-export of it on a way, by line.
    pub(crate) lines: Vec<u32>,
    /// Its file is a package's entry file, which runs before any module
    /// below the package is loaded.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) runs_first: bool,
    /// Test files that load its file, or a module below it when it runs
    /// first, and are not among the tests to run again.
    #[serde(skip_serializing_if = "is_zero")]
    pub(crate) tests_not_listed: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// What `impact` narrowed at barrels for: a file, or a symbol.
pub(crate) enum Narrowed<'a> {
    File(&'a str),
    /// A symbol, with the statements that make their file a barrel for it
    /// though they are no re-export (see `FirstStep::passing`).
    Symbol(&'a Symbol, &'a BTreeSet<ImportPlace>),
}

/// The re-exports of the files in `barrels`, which the reach went on from
/// only through the names they pass on: in each, the statements that
/// re-export the target (for a file, from it; for a symbol, its name or its
/// whole module) come nearest, then those that re-export the files it came
/// from, nearest first, and with none of those, all of its re-exports. Each
/// counts the test files that load its file (or, for a package's entry
/// file, a module below it) that `listed` does not hold, apart from those
/// that load it for types only or replace it with a mock.
/// Whether `relays` holds the statement `e` is evidence of.
fn relayed(relays: &BTreeSet<ImportPlace>, e: &Evidence) -> bool {
    e.line.is_some_and(|line| {
        relays.contains(&ImportPlace {
            file: e.file.clone(),
            line,
        })
    })
}

pub(crate) fn barrels(
    full: &ArchitectureGraph,
    target: Option<Narrowed>,
    barrels: &BTreeMap<String, Vec<String>>,
    listed: &BTreeSet<String>,
    cap: usize,
) -> Option<Barrels> {
    let imports = || {
        full.edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Import)
            .flat_map(|e| &e.evidence)
    };
    // the statements that re-export the target itself
    let own: BTreeSet<(&str, Option<u32>)> = match target {
        Some(Narrowed::File(file)) => imports()
            .filter(|e| e.passes_on() && e.target.as_deref() == Some(file) && e.file != file)
            .map(|e| (e.file.as_str(), e.line))
            .collect(),
        Some(Narrowed::Symbol(symbol, relays)) => full
            .symbol_importers(symbol)
            .into_iter()
            .flat_map(|found| found.by_name.into_iter().chain(found.may_use))
            .map(|(_, e)| e)
            .filter(|e| e.passes_on() || relayed(relays, e))
            .map(|e| (e.file.as_str(), e.line))
            .collect(),
        None => BTreeSet::new(),
    };
    // how near the way a statement is on: what re-exports the target is
    // nearest, then a statement by the file it re-exports
    let mut rank: BTreeMap<(&str, Option<u32>), usize> = BTreeMap::new();
    let mut statements: Vec<&Evidence> = Vec::new();
    let relays = match target {
        Some(Narrowed::Symbol(_, relays)) => Some(relays),
        _ => None,
    };
    for (barrel, from) in barrels {
        let re_exports: Vec<&Evidence> = imports()
            .filter(|e| {
                (e.passes_on() || relays.is_some_and(|r| relayed(r, e)))
                    && e.file == *barrel
                    && e.target.as_deref() != Some(barrel.as_str())
            })
            .collect();
        let near = |e: &Evidence| {
            if own.contains(&(e.file.as_str(), e.line)) {
                return Some(0);
            }
            let at = from.iter().position(|f| e.target.as_deref() == Some(f))?;
            Some(at + 1)
        };
        let on_way: Vec<&Evidence> = re_exports
            .iter()
            .copied()
            .filter(|e| near(e).is_some())
            .collect();
        // no file it came from is known: all of its re-exports
        let shown = if on_way.is_empty() {
            re_exports
        } else {
            on_way
        };
        for e in shown {
            rank.entry((e.file.as_str(), e.line))
                .or_insert(near(e).unwrap_or(usize::MAX));
            statements.push(e);
        }
    }
    let rank_of = |e: &Evidence| rank.get(&(e.file.as_str(), e.line)).copied().unwrap_or(0);
    statements.sort_by(|a, b| (&a.file, a.line).cmp(&(&b.file, b.line)));
    statements.dedup_by(|a, b| a.file == b.file && a.line == b.line);
    // each file once, at its statement on the nearest way, with all of them
    let mut files: Vec<(&Evidence, Vec<u32>)> = Vec::new();
    for e in statements {
        match files.last_mut() {
            Some((shown, lines)) if shown.file == e.file => {
                lines.extend(e.line);
                if (rank_of(e), e.line) < (rank_of(shown), shown.line) {
                    *shown = e;
                }
            }
            _ => files.push((e, e.line.into_iter().collect())),
        }
    }
    if files.is_empty() {
        return None;
    }
    let entries: BTreeSet<&str> = full
        .components
        .values()
        .flat_map(|c| &c.evidence)
        .filter(|e| e.runs_first())
        .map(|e| e.file.as_str())
        .collect();
    let shown = files
        .iter()
        .take(cap)
        .map(|(e, lines)| {
            let barrel = e.file.as_str();
            let runs_first = entries.contains(barrel);
            let below = if runs_first {
                full.imports_below(barrel)
            } else {
                Vec::new()
            };
            // a test whose mock replaces the barrel runs none of it
            let mocking: BTreeSet<&str> = imports()
                .filter(|i| i.replaces && i.target.as_deref() == Some(barrel))
                .map(|i| i.file.as_str())
                .collect();
            let loading: BTreeSet<&str> = imports()
                .filter(|i| i.target.as_deref() == Some(barrel) && i.via().is_none())
                .chain(below.iter().map(|(_, i)| *i))
                .filter(|i| i.test && !i.type_only && !listed.contains(&i.file))
                .map(|i| i.file.as_str())
                .filter(|file| !mocking.contains(file))
                .collect();
            Barrel {
                file: e.file.clone(),
                line: e.line,
                lines: lines.clone(),
                runs_first,
                tests_not_listed: loading.len(),
            }
        })
        .collect();
    Some(Barrels {
        total: files.len(),
        shown,
    })
}

pub(crate) const VALUES: &str =
    "calls through a value of the type (x.m()) need its type, which is not read";

/// What a `values` line says of a Rust trait, whose methods values of the
/// types that implement it call where a `use` brings it into scope.
pub(crate) const TRAIT_VALUES: &str =
    "calls of its methods through values of the types that implement it are not read";

#[derive(Debug, Serialize)]
pub(crate) struct Values {
    pub(crate) note: &'static str,
    /// Statements that bind the class (or its module) and may call it
    /// through values.
    pub(crate) total: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[serde(rename = "locations")]
    pub(crate) shown: Vec<Spot>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Spots {
    pub(crate) total: usize,
    #[serde(rename = "locations")]
    pub(crate) shown: Vec<Spot>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Spot {
    pub(crate) file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) line: Option<u32>,
    /// The place is test code.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) test: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct UsesNotRead {
    pub(crate) total: usize,
    #[serde(rename = "locations")]
    pub(crate) shown: Vec<UnreadSpot>,
}

#[derive(Debug, Serialize)]
pub(crate) struct UnreadSpot {
    pub(crate) file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) line: Option<u32>,
    pub(crate) reason: UnreadReason,
}

/// `found` with what the uses pass for a symbol could not follow: the
/// places its module escapes, the names it is passed on as, the files not
/// read, and the calls through values that `values` names (see
/// [`crate::query::values_note`]): of a method that is not static, or of a
/// trait's methods where statements bring it into scope.
pub(crate) fn with_uses(
    found: Option<NotTraced>,
    uses: &SymbolUses,
    values: Option<&'static str>,
) -> Option<NotTraced> {
    let mut found = found.unwrap_or_default();
    // a string that names the symbol, or a static member's class used as
    // a value, apart from a module used as a value
    let noted = |note: &str| -> Vec<&Evidence> {
        uses.escapes
            .iter()
            .filter(|e| match e.note.as_deref() {
                Some(n @ ("class" | "string")) => n == note,
                _ => note.is_empty(),
            })
            .collect()
    };
    for (list, into) in [
        (noted(""), &mut found.whole_module),
        (noted("class"), &mut found.class_values),
        (noted("string"), &mut found.strings),
    ] {
        if list.is_empty() {
            continue;
        }
        // production code first
        let mut spots: Vec<Spot> = list
            .iter()
            .map(|e| Spot {
                file: e.file.clone(),
                line: e.line,
                test: e.test,
            })
            .collect();
        spots.sort_by(|a, b| (a.test, &a.file, a.line).cmp(&(b.test, &b.file, b.line)));
        *into = Some(Spots {
            total: spots.len(),
            shown: spots,
        });
    }
    if !uses.unread.is_empty() {
        found.uses = Some(UsesNotRead {
            total: uses.unread.len(),
            shown: uses
                .unread
                .iter()
                .map(|u| UnreadSpot {
                    file: u.file.clone(),
                    line: u.line,
                    reason: u.reason,
                })
                .collect(),
        });
    }
    // a method that takes a value always says so, a trait only where
    // statements bring it into scope
    if let Some(note) = values.filter(|&note| note == VALUES || !uses.values.is_empty()) {
        let mut spots: Vec<Spot> = uses
            .values
            .iter()
            .map(|e| Spot {
                file: e.file.clone(),
                line: e.line,
                test: e.test,
            })
            .collect();
        spots.sort_by(|a, b| (a.test, &a.file, a.line).cmp(&(b.test, &b.file, b.line)));
        // two statements on one line are one place
        spots.dedup_by(|a, b| a.file == b.file && a.line == b.line);
        found.values = Some(Values {
            note,
            total: spots.len(),
            shown: spots,
        });
    }
    if !uses.subclasses.is_empty() {
        let mut spots: Vec<Spot> = uses
            .subclasses
            .iter()
            .map(|e| Spot {
                file: e.file.clone(),
                line: e.line,
                test: e.test,
            })
            .collect();
        spots.sort_by(|a, b| (a.test, &a.file, a.line).cmp(&(b.test, &b.file, b.line)));
        spots.dedup_by(|a, b| a.file == b.file && a.line == b.line);
        found.subclasses = Some(Spots {
            total: spots.len(),
            shown: spots,
        });
    }
    let empty = found.dynamic.is_none()
        && found.named_like.is_none()
        && found.macros.is_none()
        && found.not_read.is_none()
        && found.script.is_none()
        && found.global.is_none()
        && found.no_importers.is_none()
        && found.whole_module.is_none()
        && found.class_values.is_none()
        && found.strings.is_none()
        && found.uses.is_none()
        && found.values.is_none()
        && found.subclasses.is_none()
        && found.barrels.is_none()
        && found.relays.is_none();
    (!empty).then_some(found)
}

pub(crate) const SCRIPT: &str =
    "a script: its declarations are global, so no import names what uses them";
pub(crate) const GLOBAL: &str =
    "declarations in `declare global` are global, so no import names what uses them";
pub(crate) const NO_IMPORTERS: &str = "no import of it was found: only import statements are \
     read, so a file that a framework, a test runner or a command loads by name or path has none";

#[derive(Debug, Serialize)]
pub(crate) struct Dynamic {
    pub(crate) total: usize,
    #[serde(rename = "locations")]
    pub(crate) shown: Vec<DynamicCall>,
}

#[derive(Debug, Serialize)]
pub(crate) struct DynamicCall {
    pub(crate) file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) line: Option<u32>,
    pub(crate) call: String,
    /// The paths it can load start so, relative to the root, where the
    /// analyzer knows the static start of the name it computes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) below: Option<String>,
    /// The target is no file below that path but one that loading any of
    /// them runs first (a Python package's `__init__.py` above it).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) runs_first: bool,
    /// The call is test code.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) test: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct NamedLike {
    pub(crate) name: String,
    pub(crate) total: usize,
    #[serde(rename = "locations")]
    pub(crate) shown: Vec<NamedImport>,
}

#[derive(Debug, Serialize)]
pub(crate) struct NamedImport {
    pub(crate) file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) line: Option<u32>,
    pub(crate) module: String,
    pub(crate) reason: UnmappedReason,
}

#[derive(Debug, Serialize)]
pub(crate) struct Macros {
    /// The name their paths write.
    pub(crate) name: String,
    pub(crate) total: usize,
    #[serde(rename = "locations")]
    pub(crate) shown: Vec<MacroCall>,
}

#[derive(Debug, Serialize)]
pub(crate) struct MacroCall {
    pub(crate) file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) line: Option<u32>,
    /// The macro called (`json`).
    #[serde(rename = "macro")]
    pub(crate) name: String,
    /// The call is test code.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) test: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct NotRead {
    /// The languages counted: the target's, with TypeScript and JavaScript
    /// together.
    pub(crate) languages: Vec<String>,
    pub(crate) files: usize,
    pub(crate) read: usize,
    /// Which files those are, where the analyzer skips some by design.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) note: Option<&'static str>,
}

/// What the Rust analyzer leaves unread.
pub(crate) const RUST_NOT_READ: &str = "the Rust analyzer reads src/ and what the other Cargo \
     targets load, so files outside src/ that no target loads, such as test data, are among them, \
     as is any file that failed to parse";

/// The target as far as what could reach it unseen is concerned.
pub(crate) struct Subject<'a> {
    /// The language of the target's analyzer.
    pub(crate) language: Option<&'a str>,
    /// Where the target is, for imports without an edge that may name it:
    /// for a symbol, its file.
    pub(crate) place: Option<Place<'a>>,
    /// What belongs to the target itself, whose own calls `Not mapped`
    /// already lists.
    pub(crate) own: Own<'a>,
    pub(crate) script: bool,
    /// The target is, or its file holds, a declaration in a module's
    /// `declare global` (see [`declares_global`]).
    pub(crate) global: bool,
    /// Importers are recorded and none exists.
    pub(crate) unreached: bool,
}

pub(crate) enum Own<'a> {
    File(&'a str),
    /// A component as folded at `depth`.
    Component(&'a ComponentId, usize),
}

/// A target's place: a file, or a component's directory.
pub(crate) enum Place<'a> {
    File(&'a str),
    Directory(&'a str),
}

/// What could not be traced to `subject`, with at most `cap` locations per
/// kind; `None` when nothing applies.
pub(crate) fn not_traced(
    full: &ArchitectureGraph,
    subject: &Subject,
    cap: usize,
) -> Option<NotTraced> {
    let family = subject.language.map(family);
    let in_family = |component: &ComponentId| {
        family.is_some()
            && full
                .component(component)
                .and_then(|c| c.language.as_deref())
                .map(self::family)
                == family
    };
    let own = |component: &ComponentId, file: &str| match subject.own {
        Own::File(own) => file == own,
        Own::Component(id, depth) => full.ancestor_at(component, depth) == *id,
    };

    // a call whose name starts with a path the target is not under loads
    // nothing of it
    let may_load = |d: &DynamicImport| match &subject.place {
        Some(Place::File(place) | Place::Directory(place)) => d.may_load(place),
        None => true,
    };
    let mut calls: Vec<DynamicCall> = full
        .dynamic_imports
        .iter()
        .filter(|d| in_family(&d.from) && !own(&d.from, &d.evidence.file) && may_load(d))
        .map(|d| DynamicCall {
            file: d.evidence.file.clone(),
            line: d.evidence.line,
            call: d.call.clone(),
            below: d.prefix.as_ref().and_then(|p| p.path.clone()),
            // the target is a file that loading below the path runs first
            runs_first: match &subject.place {
                Some(Place::File(place)) => d
                    .prefix
                    .as_ref()
                    .is_some_and(|p| p.first.iter().any(|f| f == place)),
                _ => false,
            },
            test: d.evidence.test,
        })
        .collect();
    // production code first
    calls.sort_by(|a, b| (a.test, &a.file, a.line).cmp(&(b.test, &b.file, b.line)));
    let dynamic = (!calls.is_empty()).then(|| Dynamic {
        total: calls.len(),
        shown: calls.into_iter().take(cap).collect(),
    });

    let named_like = subject.place.as_ref().and_then(|place| {
        let target = segments_of(place);
        let name = target.last()?.clone();
        let package = match place {
            Place::File(file) => full.component_for_path(file).map(|c| &c.id),
            Place::Directory(dir) => full.component_for_path(dir).map(|c| &c.id),
        }
        .and_then(|c| package_of(full, c));
        let mut imports: Vec<NamedImport> = full
            .unmapped_imports
            .iter()
            .filter(|i| {
                matches!(
                    i.reason,
                    UnmappedReason::LocalName | UnmappedReason::Unresolved
                ) && in_family(&i.from)
                    && may_be(full, i, &target, package)
            })
            .map(|i| NamedImport {
                file: i.evidence.file.clone(),
                line: i.evidence.line,
                module: i.module.clone(),
                reason: i.reason,
            })
            .collect();
        imports.sort_by(|a, b| (&a.file, a.line).cmp(&(&b.file, b.line)));
        (!imports.is_empty()).then(|| NamedLike {
            name,
            total: imports.len(),
            shown: imports.into_iter().take(cap).collect(),
        })
    });

    let macros = subject.place.as_ref().and_then(|place| {
        let name = macro_name(full, place)?;
        let mut calls: Vec<MacroCall> = full
            .unread_macros
            .iter()
            .filter(|m| in_family(&m.from) && !own(&m.from, &m.evidence.file))
            .filter(|m| m.names.contains(&name))
            .map(|m: &UnreadMacro| MacroCall {
                file: m.evidence.file.clone(),
                line: m.evidence.line,
                name: m.name.clone(),
                test: m.evidence.test,
            })
            .collect();
        calls.sort_by(|a, b| (a.test, &a.file, a.line).cmp(&(b.test, &b.file, b.line)));
        (!calls.is_empty()).then(|| Macros {
            name,
            total: calls.len(),
            shown: calls.into_iter().take(cap).collect(),
        })
    });

    let not_read = family.and_then(|family| {
        let counted: Vec<(&String, usize, usize)> = full
            .meta
            .coverage
            .iter()
            .filter(|(language, _)| self::family(language) == family)
            .filter_map(|(language, c)| Some((language, c.files, c.read?)))
            .collect();
        let files: usize = counted.iter().map(|c| c.1).sum();
        let read: usize = counted.iter().map(|c| c.2).sum();
        (read < files).then(|| NotRead {
            languages: counted.iter().map(|c| c.0.clone()).collect(),
            files,
            read,
            note: (family == "rust").then_some(RUST_NOT_READ),
        })
    });

    // a test runner loads a test file: that nothing imports it is no news
    let test_file = match subject.own {
        Own::File(file) => test_files(full, [file]).contains(file),
        Own::Component(..) => false,
    };
    let found = NotTraced {
        dynamic,
        named_like,
        macros,
        not_read,
        script: subject.script.then_some(SCRIPT),
        global: subject.global.then_some(GLOBAL),
        // a script's own note, or a global one, already says why nothing
        // imports it
        no_importers: (subject.unreached && !test_file && !subject.script && !subject.global)
            .then_some(NO_IMPORTERS),
        ..Default::default()
    };
    let empty = found.dynamic.is_none()
        && found.named_like.is_none()
        && found.macros.is_none()
        && found.not_read.is_none()
        && found.script.is_none()
        && found.global.is_none()
        && found.no_importers.is_none();
    (!empty).then_some(found)
}

/// One analyzer reads TypeScript and JavaScript, and either can load the
/// other.
/// Whether a file at or below `component` holds a declaration in a
/// module's `declare global`.
pub(crate) fn holds_global(full: &ArchitectureGraph, component: &Component) -> bool {
    let below = |file: &str| match component.path.as_deref() {
        None => false,
        Some("" | ".") => true,
        Some(dir) => file.starts_with(&format!("{}/", dir.trim_end_matches('/'))) || file == dir,
    };
    full.symbols
        .values()
        .filter_map(Symbol::location)
        .any(|e| e.declares_global() && below(&e.file))
}

/// Whether `file` holds a declaration in a module's `declare global`.
pub(crate) fn declares_global(full: &ArchitectureGraph, file: &str) -> bool {
    full.symbols
        .values()
        .filter_map(Symbol::location)
        .any(|e| e.file == file && e.declares_global())
}

/// Whether code uses `symbol` without importing its file: a script's
/// declaration, or one in a module's `declare global`.
pub(crate) fn is_global(full: &ArchitectureGraph, symbol: &Symbol) -> bool {
    symbol.location().is_some_and(Evidence::declares_global)
        || full
            .component(&symbol.component)
            .is_some_and(|c| c.kind == ComponentKind::Script)
}

fn family(language: &str) -> &str {
    match language {
        "javascript" => "typescript",
        other => other,
    }
}

/// Entry files that an import names by their directory.
const ENTRIES: &[&str] = &["index", "__init__", "mod"];

/// A target's path as an import would name it: segments without the
/// extension, an entry file by its directory (`src/lib/utils.ts` ->
/// `src lib utils`, `shop/billing/__init__.py` -> `shop billing`).
fn segments_of(place: &Place) -> Vec<String> {
    let path = match place {
        Place::File(file) | Place::Directory(file) => file,
    };
    tail(path.split('/').filter(|s| !s.is_empty()).collect())
}

/// Parts of a path or import, without the last part's extension and
/// without an entry file's name at the end.
fn tail(mut parts: Vec<&str>) -> Vec<String> {
    if let Some(last) = parts.last_mut() {
        *last = last.split('.').next().unwrap_or(last);
    }
    if parts.len() > 1 && parts.last().is_some_and(|last| ENTRIES.contains(last)) {
        parts.pop();
    }
    parts.into_iter().map(str::to_owned).collect()
}

/// Whether an import without an edge may be the `target` it failed to
/// resolve to: a relative specifier that, resolved against the importer's
/// directory, lands on the target; an alias or path that the target's path
/// ends in (an alias only within the target's own package); a dotted name
/// the same way; a bare name that is the target's name.
fn may_be(
    full: &ArchitectureGraph,
    import: &UnmappedImport,
    target: &[String],
    package: Option<&ComponentId>,
) -> bool {
    let module = import.module.as_str();
    if module == "." || module == ".." || module.starts_with("./") || module.starts_with("../") {
        let dir = import.evidence.file.rsplit_once('/').map_or("", |(d, _)| d);
        let mut parts: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
        for part in module.split('/') {
            match part {
                "" | "." => {}
                ".." => {
                    if parts.pop().is_none() {
                        return false;
                    }
                }
                part => parts.push(part),
            }
        }
        return tail(parts) == target;
    }
    if module.contains('/') {
        let mut parts: Vec<&str> = module.split('/').filter(|s| !s.is_empty()).collect();
        // `@/x`, `~/x`, `#/x`: relative to the importer's own package
        let aliased = matches!(parts.first(), Some(&("@" | "~" | "#" | "$")));
        if aliased {
            parts.remove(0);
        } else if let Some(first) = parts.first_mut() {
            *first = first.trim_start_matches(['@', '~', '#']);
        }
        let parts = tail(parts);
        if parts.is_empty() || !target.ends_with(&parts) {
            return false;
        }
        return !aliased || package.is_some_and(|p| package_of(full, &import.from) == Some(p));
    }
    if module.contains('.') {
        let parts: Vec<String> = module.split('.').map(str::to_owned).collect();
        return target.ends_with(&parts);
    }
    target.last().map(String::as_str) == Some(module)
}

/// The name a path inside a macro call writes for the target at `place`:
/// its module's name (a file's stem, a `mod.rs` directory's name), or, for
/// a package's library root or its directory, its crate's name, which the
/// entry names (`archmap_scan`). The other files a package owns directly are
/// crate roots that no path names.
fn macro_name(full: &ArchitectureGraph, place: &Place) -> Option<String> {
    let path = match place {
        Place::File(file) | Place::Directory(file) => *file,
    };
    match full.component_for_path(path) {
        Some(owner) if owner.kind == ComponentKind::Package => {
            let whole = owner.path.as_deref() == Some(path);
            let entry = owner
                .evidence
                .iter()
                .find(|e| e.is_entry() && (whole || e.file == path))?;
            Some(
                entry
                    .names
                    .first()
                    .cloned()
                    .unwrap_or_else(|| owner.name.replace('-', "_")),
            )
        }
        _ => segments_of(place).last().cloned(),
    }
}

/// The nearest package that holds `component`.
fn package_of<'g>(full: &'g ArchitectureGraph, component: &ComponentId) -> Option<&'g ComponentId> {
    full.containment_path(component)
        .iter()
        .rev()
        .filter_map(|id| full.component(id))
        .find(|c| c.kind == ComponentKind::Package)
        .map(|c| &c.id)
}

#[cfg(test)]
mod tests {
    use archmap_core::{Component, DynamicImport, Edge, EdgeKind, Evidence};

    use super::*;

    #[test]
    fn a_target_goes_by_its_path_without_extension_and_entry_name() {
        assert_eq!(
            segments_of(&Place::File("src/lib/utils.ts")),
            ["src", "lib", "utils"]
        );
        assert_eq!(
            segments_of(&Place::File("src/shop/billing/__init__.py")),
            ["src", "shop", "billing"]
        );
        assert_eq!(
            segments_of(&Place::File("src/components/index.ts")),
            ["src", "components"]
        );
        assert_eq!(
            segments_of(&Place::File("src/global.d.ts")),
            ["src", "global"]
        );
        assert_eq!(
            segments_of(&Place::Directory("src/shop/billing")),
            ["src", "shop", "billing"]
        );
    }

    /// One TypeScript package, `web`, with the given statements.
    fn web(edges: Vec<Edge>, dynamic_imports: Vec<DynamicImport>) -> ArchitectureGraph {
        let mut full = ArchitectureGraph {
            edges,
            dynamic_imports,
            ..Default::default()
        };
        let mut web = Component::new("web", "web", ComponentKind::Package);
        web.language = Some("typescript".into());
        full.add_component(web);
        full
    }

    #[test]
    fn a_dynamic_call_is_test_code_only_when_its_analyzer_marked_it() {
        let call = |file: &str, test: bool| DynamicImport {
            from: ComponentId::new("web"),
            call: "import".into(),
            prefix: None,
            evidence: Evidence::new(file).at_line(1).in_test(test),
        };
        // a Next.js route below app/test/, which the path rule alone calls test code
        let full = web(
            vec![],
            vec![
                call("app/test/page.tsx", false),
                call("src/load.test.ts", true),
            ],
        );
        let subject = Subject {
            language: Some("typescript"),
            place: None,
            own: Own::File("src/target.ts"),
            script: false,
            global: false,
            unreached: false,
        };
        let dynamic = not_traced(&full, &subject, 10)
            .and_then(|found| found.dynamic)
            .unwrap();
        let marks: Vec<(&str, bool)> = dynamic
            .shown
            .iter()
            .map(|c| (c.file.as_str(), c.test))
            .collect();
        assert_eq!(
            marks,
            [("app/test/page.tsx", false), ("src/load.test.ts", true)]
        );
    }

    #[test]
    fn the_statements_recorded_in_a_file_decide_whether_it_is_test_code() {
        let full = web(
            vec![Edge::new("web", "ext:npm:react", EdgeKind::Import)
                .with_evidence(Evidence::new("app/test/page.tsx").at_line(1))],
            vec![],
        );
        let no_importers = |file: &'static str| {
            let subject = Subject {
                language: Some("typescript"),
                place: Some(Place::File(file)),
                own: Own::File(file),
                script: false,
                global: false,
                unreached: true,
            };
            not_traced(&full, &subject, 10).and_then(|found| found.no_importers)
        };
        // its analyzer read the route's import as production code
        assert_eq!(no_importers("app/test/page.tsx"), Some(NO_IMPORTERS));
        // nothing recorded in it: its path decides
        assert_eq!(no_importers("tests/helper.ts"), None);
    }

    #[test]
    fn a_barrel_is_named_at_the_re_export_of_the_nearest_file_it_came_from() {
        let re_export = |line: u32, target: &str| {
            Edge::new("pkg", "pkg", EdgeKind::Import).with_evidence(
                Evidence::new("pkg/__init__.py")
                    .at_line(line)
                    .pointing_at(target)
                    .with_note("export"),
            )
        };
        let full = ArchitectureGraph {
            edges: vec![
                re_export(1, "pkg/a.py"),
                re_export(2, "pkg/b.py"),
                re_export(3, "pkg/c.py"),
            ],
            ..Default::default()
        };
        let named = |from: &[&str]| {
            let reached = BTreeMap::from([(
                "pkg/__init__.py".to_owned(),
                from.iter().map(|f| (*f).to_owned()).collect(),
            )]);
            let found = barrels(
                &full,
                Some(Narrowed::File("pkg/z.py")),
                &reached,
                &BTreeSet::new(),
                usize::MAX,
            )
            .unwrap();
            let barrel = &found.shown[0];
            (found.total, barrel.line, barrel.lines.clone())
        };
        // the nearest way first, every re-export on a way listed
        assert_eq!(named(&["pkg/c.py", "pkg/b.py"]), (1, Some(3), vec![2, 3]));
        // where no file it came from is known, its first, not dropped
        assert_eq!(named(&[]), (1, Some(1), vec![1, 2, 3]));
        // the target's own re-export stays first, though the walk reaches
        // the target's file again further on (files that import each other)
        let reached = BTreeMap::from([(
            "pkg/__init__.py".to_owned(),
            vec!["pkg/a.py".to_owned(), "pkg/c.py".to_owned()],
        )]);
        let found = barrels(
            &full,
            Some(Narrowed::File("pkg/c.py")),
            &reached,
            &BTreeSet::new(),
            usize::MAX,
        )
        .unwrap();
        assert_eq!(
            (found.shown[0].line, found.shown[0].lines.clone()),
            (Some(3), vec![1, 3])
        );
    }
}
