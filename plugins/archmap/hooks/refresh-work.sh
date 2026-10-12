#!/bin/sh
# The archmap plugin's SessionStart hook, run in the background. When the
# user turned auto_refresh on, it refreshes the project's work snapshot with
# `archmap fetch github --if-older-than`; otherwise, and when archmap, its
# flag or gh is missing, it starts no fetch and writes nothing. It checks
# only what the fetch needs: archmap decides the rest (the repository and
# its host, gh's login for it, freshness, the lock, waiting after failures)
# before it writes anything, and records a failed fetch beside the
# snapshot. It prints nothing, so nothing reaches the session.

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
# archmap fetches through gh; which host, and whether gh holds a login for
# it, archmap decides before it writes anything
command -v gh >/dev/null 2>&1 || exit 0
archmap fetch github \
  --if-older-than "${CLAUDE_PLUGIN_OPTION_AUTO_REFRESH_INTERVAL:-30m}" \
  --path "$root" </dev/null >/dev/null 2>&1
exit 0
