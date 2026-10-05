//! The files a framework loads for a URL: the route files of Next.js
//! packages, which the scan keeps for `impact` to ask about on demand.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::test_code::is_test_code;

/// The directories of a Next.js package whose subdirectories are URL
/// segments.
pub(crate) const NEXT_ROUTES: [&str; 4] = ["app", "pages", "src/app", "src/pages"];

/// The files of an app router segment that Next.js loads for a URL: pages,
/// layouts and their states, route handlers and metadata routes
/// (<https://nextjs.org/docs/app/api-reference/file-conventions>).
const APP_ROUTE_FILES: [&str; 19] = [
    "page",
    "layout",
    "template",
    "loading",
    "error",
    "global-error",
    "not-found",
    "global-not-found",
    "forbidden",
    "unauthorized",
    "default",
    "route",
    "opengraph-image",
    "twitter-image",
    "icon",
    "apple-icon",
    "sitemap",
    "robots",
    "manifest",
];

const EXTENSIONS: [&str; 6] = ["js", "jsx", "ts", "tsx", "mjs", "cjs"];

/// The files Next.js runs before the requests of the URLs they match, in a
/// package's directory or its `src/`: `middleware`, and from Next.js 16 on,
/// which renamed it, `proxy`.
const MIDDLEWARE: &str = "middleware";
const PROXY: &str = "proxy";

/// The route directories of the packages whose manifest declares `next`,
/// and those packages' directories, relative to the root with `/`
/// separators (`app`, `web/src/pages`; `` for the root, `web/`), each with
/// whether the version it declares admits Next.js 16.
#[derive(Debug, Default)]
pub(crate) struct Routes {
    dirs: BTreeSet<String>,
    packages: BTreeMap<String, bool>,
}

impl Routes {
    /// The route directories of the packages at `packages`, relative
    /// directories, empty for the root, each with whether it may run
    /// Next.js 16 or later.
    pub(crate) fn of<'a>(packages: impl IntoIterator<Item = (&'a Path, bool)>) -> Routes {
        let mut found = Routes::default();
        for (package, sixteen) in packages {
            let slashed = |p: &Path| p.to_string_lossy().replace('\\', "/");
            for routes in NEXT_ROUTES {
                found.dirs.insert(slashed(&package.join(routes)));
            }
            let dir = slashed(package);
            let dir = match dir.is_empty() {
                true => dir,
                false => format!("{dir}/"),
            };
            found.packages.insert(dir, sixteen);
        }
        found
    }

    /// The files among `paths` that Next.js runs before every request
    /// their matcher covers (`middleware.ts`, `proxy.ts`).
    pub(crate) fn before<'a>(&self, paths: impl IntoIterator<Item = &'a str>) -> BTreeSet<&'a str> {
        paths
            .into_iter()
            .filter(|path| {
                self.packages.iter().any(|(dir, sixteen)| {
                    let Some(below) = path.strip_prefix(dir.as_str()) else {
                        return false;
                    };
                    let name = below.strip_prefix("src/").unwrap_or(below);
                    name.rsplit_once('.').is_some_and(|(stem, extension)| {
                        (stem == MIDDLEWARE || (stem == PROXY && *sixteen))
                            && EXTENSIONS.contains(&extension)
                    })
                })
            })
            .collect()
    }
}

impl Routes {
    /// The files among `paths` that Next.js loads for a URL (see
    /// [`crate::route_files`]).
    pub(crate) fn files<'a>(&self, paths: impl IntoIterator<Item = &'a str>) -> BTreeSet<&'a str> {
        if self.dirs.is_empty() {
            return BTreeSet::new();
        }
        paths
            .into_iter()
            .filter(|path| {
                self.dirs.iter().any(|dir| {
                    let Some(below) = path
                        .strip_prefix(dir.as_str())
                        .and_then(|b| b.strip_prefix('/'))
                    else {
                        return false;
                    };
                    is_route(dir.ends_with("pages"), below)
                        && !test_code_below(Path::new(dir), Path::new(path))
                })
            })
            .collect()
    }
}

/// Whether `below`, a file below a route directory, is a route file: of the
/// pages router any code file, of the app router one of its file
/// conventions outside private folders.
fn is_route(pages: bool, below: &str) -> bool {
    let (dirs, name) = below.rsplit_once('/').unwrap_or(("", below));
    let Some((stem, extension)) = name.rsplit_once('.') else {
        return false;
    };
    if !EXTENSIONS.contains(&extension) {
        return false;
    }
    pages || (!dirs.split('/').any(|d| d.starts_with('_')) && APP_ROUTE_FILES.contains(&stem))
}

/// Whether `file`, below `routes`, a route directory of a package that
/// declares `next`, is test code: by the rule every analyzer shares, except
/// that a directory named `test` or `tests` there is the URL `/test`
/// (`app/test/page.tsx`), while test file names, `__tests__` and
/// `__mocks__` keep their meaning.
pub(crate) fn test_code_below(routes: &Path, file: &Path) -> bool {
    // the directories below the routes, as segments the rule never reads
    let mut segments = routes.to_path_buf();
    let below = file.strip_prefix(routes).unwrap_or(file).to_path_buf();
    let mut parts = below.components().peekable();
    while let Some(part) = parts.next() {
        let name = part.as_os_str();
        let is_dir = parts.peek().is_some();
        segments.push(if is_dir && (name == "test" || name == "tests") {
            std::ffi::OsStr::new("route")
        } else {
            name
        });
    }
    is_test_code(&segments)
}

#[cfg(test)]
mod tests {
    use super::is_route;

    #[test]
    fn route_files_are_told_by_their_router_and_name() {
        for (pages, below) in [
            (false, "page.tsx"),
            (false, "(shop)/items/[id]/page.tsx"),
            (false, "blog/[...slug]/layout.ts"),
            (false, "api/items/route.ts"),
            (false, "sitemap.ts"),
            (false, "@modal/(.)photo/page.tsx"),
            (true, "index.tsx"),
            (true, "api/hello.ts"),
            (true, "_app.tsx"),
        ] {
            assert!(is_route(pages, below), "{below}");
        }
        for below in [
            "items/card.tsx",
            "_parts/page.tsx",
            "page.module.css",
            "page",
        ] {
            assert!(!is_route(false, below), "{below}");
        }
    }
}
