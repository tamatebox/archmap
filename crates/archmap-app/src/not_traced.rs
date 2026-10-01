//! What `query` and `impact` could not follow for their target, counted
//! only from what the analyzers record and said only when something
//! applies. A target's own imports without an edge stay under `Not mapped`;
//! this is what could reach the target unseen.

use archmap_core::{ArchitectureGraph, ComponentId, UnmappedReason};
use serde::Serialize;

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
    /// The target is a script: no import names its globals.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) script: bool,
    /// Importers of the target are recorded, and none exists: only import
    /// statements are read, so code that a framework or runtime loads by
    /// name or path is not seen.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) unreached: bool,
}

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
    pub(crate) language: String,
    pub(crate) files: usize,
    pub(crate) read: usize,
}

/// The target as far as what could reach it unseen is concerned.
pub(crate) struct Subject<'a> {
    /// The language of the target's analyzer.
    pub(crate) language: Option<&'a str>,
    /// The name an import that missed the target would carry: a file's
    /// stem, a module's last segment. `None` for a symbol.
    pub(crate) name: Option<String>,
    /// What belongs to the target itself, whose own calls `Not mapped`
    /// already lists.
    pub(crate) own: Own<'a>,
    pub(crate) script: bool,
    pub(crate) unreached: bool,
}

pub(crate) enum Own<'a> {
    File(&'a str),
    /// A component as folded at `depth`.
    Component(&'a ComponentId, usize),
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
        })
        .collect();
    calls.sort_by(|a, b| (&a.file, a.line).cmp(&(&b.file, b.line)));
    let dynamic = (!calls.is_empty()).then(|| Dynamic {
        total: calls.len(),
        shown: calls.into_iter().take(cap).collect(),
    });

    let named_like = subject.name.as_deref().and_then(|name| {
        let mut imports: Vec<NamedImport> = full
            .unmapped_imports
            .iter()
            .filter(|i| {
                matches!(
                    i.reason,
                    UnmappedReason::LocalName | UnmappedReason::Unresolved
                ) && in_family(&i.from)
                    && last_name(&i.module) == name
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
            name: name.to_owned(),
            total: imports.len(),
            shown: imports.into_iter().take(cap).collect(),
        })
    });

    let not_read = subject.language.and_then(|language| {
        let coverage = full.meta.coverage.get(language)?;
        let read = coverage.read?;
        (read < coverage.files).then(|| NotRead {
            language: language.to_owned(),
            files: coverage.files,
            read,
        })
    });

    let found = NotTraced {
        dynamic,
        named_like,
        not_read,
        script: subject.script,
        unreached: subject.unreached,
    };
    let empty = found.dynamic.is_none()
        && found.named_like.is_none()
        && found.not_read.is_none()
        && !found.script
        && !found.unreached;
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

/// The name an import ends in: `helpers` for `helpers` and `scripts.helpers`,
/// `button` for `components/button` and `./button.js`.
fn last_name(module: &str) -> &str {
    match module.rsplit_once('/') {
        Some((_, last)) => last.split('.').next().unwrap_or(last),
        None => module.rsplit('.').next().unwrap_or(module),
    }
}

/// The name an import would give `file`: its stem, or its directory's name
/// for an entry file (`__init__.py`, `index.ts`, `mod.rs`).
pub(crate) fn file_name(file: &str) -> String {
    let (dir, name) = file.rsplit_once('/').unwrap_or(("", file));
    let stem = name.split('.').next().unwrap_or(name);
    match stem {
        "__init__" | "index" | "mod" if !dir.is_empty() => {
            dir.rsplit('/').next().unwrap_or(dir).to_owned()
        }
        _ => stem.to_owned(),
    }
}

/// The last segment of a component name (`billing` for `shop.billing`,
/// `graph` for `archmap_core::graph`).
pub(crate) fn component_name(name: &str) -> String {
    name.rsplit(['.', '/', ':'])
        .find(|s| !s.is_empty())
        .unwrap_or(name)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_import_ends_in_the_name_of_what_it_loads() {
        assert_eq!(last_name("helpers"), "helpers");
        assert_eq!(last_name("scripts.helpers"), "helpers");
        assert_eq!(last_name("components/button"), "button");
        assert_eq!(last_name("./button.js"), "button");
        assert_eq!(last_name("@/lib/missing"), "missing");
    }

    #[test]
    fn an_entry_file_goes_by_its_directory() {
        assert_eq!(file_name("scripts/helpers.py"), "helpers");
        assert_eq!(file_name("src/shop/billing/__init__.py"), "billing");
        assert_eq!(file_name("src/components/index.ts"), "components");
        assert_eq!(file_name("src/global.d.ts"), "global");
        assert_eq!(component_name("shop.billing"), "billing");
        assert_eq!(component_name("archmap_core::graph"), "graph");
    }
}
