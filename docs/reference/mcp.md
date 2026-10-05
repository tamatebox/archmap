# MCP server

`archmap mcp` serves `summary`, `query`, `impact` and `check` as MCP tools
over stdio. It is the other interface beside the CLI, with the same
capabilities: both get every answer from the same shared layer
(`archmap-app`), so a tool returns exactly what the command prints. What the
answers say is in [commands.md](commands.md).

```bash
archmap mcp                       # tools read the working directory
archmap mcp --path ../some-repo   # tools read another repository by default
```

stdout carries JSON-RPC only. The server exits when the client closes its
stdin, and exits 2 at once when `--path` is not a directory.

## Registering it

The [plugin](../../plugins/archmap/) declares the server in
`plugins/archmap/.mcp.json`, started as
`archmap mcp --path ${CLAUDE_PROJECT_DIR:-.}`, with `alwaysLoad: true` so
that Claude Code keeps the four tools' descriptions in context instead of
deferring them behind its tool search: a deferred tool shows by name only,
and an agent that sees only names tends to search the code by hand instead.
The cost is the four definitions in every session. The plugin ships no binary:
install archmap first (see [README](../../README.md#install)). Without the
binary, Claude Code shows the server as failed in `/mcp` and the plugin's
skill still loads; the skill tells the agent how to install archmap. An
`archmap` installed before the server existed fails with "unrecognized
subcommand 'mcp'": install again.

Elsewhere, register the command with your client, such as
`claude mcp add archmap -- archmap mcp --path /path/to/repo`.

## Roots

A tool reads the server's root (`--path`, else the working directory)
unless the call gives `path`: another repository root, absolute or relative
to the server's root. A `path` that is no directory is a tool error. MCP
roots are not used; the 2026-07-28 MCP specification deprecates them in
favor of tool parameters and server configuration. Targets are paths
(absolute or relative to the root) or names, as for the CLI.

## Tools

| tool | parameters |
|---|---|
| `summary` | `path?`, `depth?` |
| `query` | `target`, `path?`, `depth?`, `format?` (`text` or `json`) |
| `impact` | `target`, `path?`, `depth?`, `format?`, `verbose?` |
| `check` | `path?`, `config?`, `depth?`, `format?` |

The parameters mirror the CLI's flags, and their defaults are the CLI's:
`depth` is 2 for every tool, as `DEFAULT_DEPTH`, so an agent that never
sets it always reads the same components (one that sets it should keep the
value across calls); text answers are capped and `format: json` gives every
entry with all its evidence, which for a busy target runs large, and
`impact`'s `verbose` lists every entry in text.
`summary --verbose` and `query --verbose` have no parameter: JSON carries
everything, and summary's `omitted:` lines name the query for the rest.
`check`'s `config` is a rules file inside the root, relative to it; the
rules are read on every call.

Every tool is read-only. A target that names several things answers with
its candidates, an ordinary result, as are `check`'s findings; a tool error
(`isError`) carries the CLI's error message: nothing has that name, a path
is outside the root, a root cannot be read. Scan warnings, which the CLI
prints to stderr, follow an answer in a content block of their own, the
first 5 shown, so a JSON answer stays JSON.

The server's instructions and each tool's description say what the tool
gives, when it helps and what it cannot see, including that runtime
coupling (HTTP, databases, queues, dynamic loading) is not seen. They live
in `crates/archmap-mcp/src/text.rs`. A client loads them in every session,
so they stay short (a test caps what a client receives) and leave what a
mark, heading or count means to the answers, which say it themselves.

## The graph it keeps

The server keeps one scanned graph per root in memory, up to 8 roots; the
least recently used goes first. Before each call it stamps the root: the
files a scan would walk, with their size, modification time and (on Unix)
change time, and the `site-packages` directories of the virtualenvs the
Python analyzer reads, the git HEAD with whether the clone is shallow, and
the size and modification time of the work snapshot (`.archmap/github.json`,
which the walk skips).
When the stamp matches the one taken before the kept
graph's scan, the graph answers; otherwise the root is scanned again, so an
edit, a new file, a deleted or renamed file, or a package installed into
`.venv`, a commit, a checkout, `git fetch --unshallow` or a new work
snapshot shows in the next answer. The stamp goes by sizes and times, so an
edit that keeps a file's size and lands within the file system's timestamp
resolution of the previous stamp is seen only with the next change. A
failed scan keeps nothing, and the next call tries again; a target outside
the root fails before any scan. Calls run one at a time, so two calls on one root scan it
once, and a call on a very large root holds the others until its scan ends.
Nothing is written to disk; `.archmap/graph.json` is never read, while
`.archmap/github.json` is, by `'#N'` targets and `summary`'s Coverage; the
server never reaches the network and fetches nothing. The uses
of a symbol that `query` lists are read from the files when it asks, after
the stamp, so they read the same files as the graph; for Rust, the module
trees the scan built are kept beside the graph to resolve them, which adds
a few MB to a kept root (about 3 MB for rust-lang/cargo's 1,374 files).
The committed git history that `impact` reads for the files changed in the
same commits is read once per kept graph, on the first `impact`.

On a synthetic tree of 20,000 Python files, the first call takes about
1.1 s and a call that reuses the graph about 0.1 s, almost all of it the
stamp's walk; the CLI, which scans on every command, takes about 1 s each.
