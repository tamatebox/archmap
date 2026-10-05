---
name: archmap
description: Explains how to read archmap output, from its MCP tools or its CLI; archmap maps a repository's components and dependencies. Use when orienting in an unfamiliar Rust, Python or TypeScript/JavaScript repository, locating where code lives, tracing dependencies, judging what a change could affect, checking a finished change against the rules in an archmap.toml, or reading archmap output (summary, query, impact, check, .archmap/ files).
license: MIT
compatibility: Requires the archmap binary, installed separately (cargo install --git https://github.com/tamatebox/archmap archmap-cli); its MCP server runs as `archmap mcp`.
---

# archmap

archmap maps a repository into components (packages, modules, files, external dependencies) and the dependencies observed in manifests and imports, each with `file:line` evidence. It answers structural questions (where code lives, what imports what, what a change may reach): open the `file:line` it gives, and search normally for what it does not map, such as error messages, configuration values, dynamic loading and relations beyond imports. The code decides what is true; archmap decides where to look.

Its MCP tools (`summary`, `query`, `impact`, `check`) say what they take; a client that loads tools on demand may list them by name only until you search its tools for `archmap`. Without them, the `archmap` CLI has the same commands (`archmap summary .`, `archmap query <target>`, `archmap impact <target>`, `archmap check`), with `--path <root>` for another root and `--format json` for every piece of evidence. If neither exists, archmap is not installed: tell the user (`cargo install --git https://github.com/tamatebox/archmap archmap-cli`, again after the plugin updates) and continue without it; `components: 0 shown` means it cannot read the repository's languages, so continue without it too. Do not read `.archmap/graph.json`: it is an export.

## Reading the output

Answers say what their marks, headings and counts mean: a `Marks` line, their headings and, in `query` and `impact`, `Not traced`. What no answer can say:

- **Observed, not inferred.** Names are package, directory and module names, not responsibilities; label any role you infer as inference.
- **A missing edge is not a missing dependency.** `## Coverage`, `Not mapped` and `Not traced` say what the map misses, and runtime coupling (HTTP, databases, queues, subprocesses) is unseen. `Imported by: none` does not mean unused: framework entry files and scripts started from a command or a config have no importers by design. Search the code before calling anything unused.
- **Keep one depth.** Every tool rolls up to the same default depth; if you set `depth`, keep it across calls, since other depths name other components.
- **Check what a name resolved to.** Names repeat: read the `id:` line, and when a result is surprising retry with the id or `./<path>`, which always reads as a path. A target that names several things lists candidates.
- **impact is reachability, not a verdict.** `Direct dependents` are the firm part; transitive ones widen, most of all through a Python `__init__.py` that may use what it imports. Listed components may not use what changes; unlisted ones may still depend on it. `Tests to run again` leaves out a Rust crate's own unit tests.
- **Changed in the same commits is history, not dependency.** A file in most commits (a lockfile, a changelog) is a hub, not a partner.
- **`'#123'` (quoted in a shell) is work as GitHub records it**, read from the snapshot that `archmap fetch github` writes through the network and the user's gh login: leave fetching to the user. A commit without a local match is no proof the work was not merged, since squash and rebase merges rewrite SHAs.
- **Components cover everything under the root**, fixtures and vendored code included: check the path before treating one as product code.
