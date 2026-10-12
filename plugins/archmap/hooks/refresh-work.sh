#!/bin/sh
# The archmap plugin's SessionStart hook, run in the background. When the
# user turned auto_refresh on, it refreshes the project's work snapshot with
# `archmap fetch github --if-older-than`; otherwise, and when archmap, its
# flag, gh or a github.com login is missing, it starts no fetch and writes
# nothing. It checks only what the fetch needs: archmap decides the rest
# (the repository from `origin`, freshness, the lock, waiting after
# failures) and records a failure beside the snapshot. It prints nothing,
# so nothing reaches the session.

case ${CLAUDE_PLUGIN_OPTION_AUTO_REFRESH:-} in
  true | 1) ;;
  *) exit 0 ;;
esac
root=${CLAUDE_PROJECT_DIR:-}
[ -n "$root" ] && [ -d "$root" ] || exit 0
command -v archmap >/dev/null 2>&1 || exit 0
# an archmap installed before automatic refreshes: the option's own line,
# not a mention in another option's help
archmap fetch github --help 2>/dev/null | grep -q -e '--if-older-than <' || exit 0
# without --repo, archmap fetches an origin on github.com only, through gh
command -v gh >/dev/null 2>&1 || exit 0
gh auth token --hostname github.com >/dev/null 2>&1 || exit 0
archmap fetch github \
  --if-older-than "${CLAUDE_PLUGIN_OPTION_AUTO_REFRESH_INTERVAL:-30m}" \
  --path "$root" </dev/null >/dev/null 2>&1
exit 0
