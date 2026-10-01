//! What `query` and `impact` could not follow for their target, counted
//! only from what the analyzers record and said only when something
//! applies. A target's own imports without an edge stay under `Not mapped`;
//! this is what could reach the target unseen.

use archmap_core::{ArchitectureGraph, ComponentId, ComponentKind, UnmappedImport, UnmappedReason};
use serde::Serialize;

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
    /// Files of the target's language that its analyzer did not read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) not_read: Option<NotRead>,
    /// The target is a script, whose globals no import names; the value
    /// says so in words.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) script: Option<&'static str>,
    /// Importers of the target are recorded and none exists; the value says
    /// why that is no proof of no use. Never for a test file, which its
    /// runner loads.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) no_importers: Option<&'static str>,
}

pub(crate) const SCRIPT: &str =
    "a script: its declarations are global, so no import names what uses them";
pub(crate) const NO_IMPORTERS: &str = "no import of it was found: only import statements are \
     read, so a file that a framework, a test runner or a command loads by name or path has none";

#[derive(Debug, Serialize)]
pub(crate) struct Dynamic {
    pub(crate) total: usize,
    pub(crate) shown: Vec<DynamicCall>,
}

#[derive(Debug, Serialize)]
pub(crate) struct DynamicCall {
    pub(crate) file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) line: Option<u32>,
    pub(crate) call: String,
    /// The call is test code.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) test: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct NamedLike {
    pub(crate) name: String,
    pub(crate) total: usize,
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
pub(crate) const RUST_NOT_READ: &str = "the Rust analyzer reads only src/, so tests/, benches/, \
     examples/ and build.rs are among them, as is any file that failed to parse";

/// The target as far as what could reach it unseen is concerned.
pub(crate) struct Subject<'a> {
    /// The language of the target's analyzer.
    pub(crate) language: Option<&'a str>,
    /// Where the target is, for imports without an edge that may name it.
    /// `None` for a symbol.
    pub(crate) place: Option<Place<'a>>,
    /// What belongs to the target itself, whose own calls `Not mapped`
    /// already lists.
    pub(crate) own: Own<'a>,
    pub(crate) script: bool,
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

    let mut calls: Vec<DynamicCall> = full
        .dynamic_imports
        .iter()
        .filter(|d| in_family(&d.from) && !own(&d.from, &d.evidence.file))
        .map(|d| DynamicCall {
            file: d.evidence.file.clone(),
            line: d.evidence.line,
            call: d.call.clone(),
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
        not_read,
        script: subject.script.then_some(SCRIPT),
        // a script's own note already says why nothing imports it
        no_importers: (subject.unreached && !test_file && !subject.script).then_some(NO_IMPORTERS),
    };
    let empty = found.dynamic.is_none()
        && found.named_like.is_none()
        && found.not_read.is_none()
        && found.script.is_none()
        && found.no_importers.is_none();
    (!empty).then_some(found)
}

/// One analyzer reads TypeScript and JavaScript, and either can load the
/// other.
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
                unreached: true,
            };
            not_traced(&full, &subject, 10).and_then(|found| found.no_importers)
        };
        // its analyzer read the route's import as production code
        assert_eq!(no_importers("app/test/page.tsx"), Some(NO_IMPORTERS));
        // nothing recorded in it: its path decides
        assert_eq!(no_importers("tests/helper.ts"), None);
    }
}
