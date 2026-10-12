#!/bin/sh
# Tests of refresh-work.sh, run with `sh plugins/archmap/hooks/refresh-work.test.sh`
# from anywhere. Stand-ins for archmap and gh record their calls; the hook
# runs with a PATH that holds them and the system's tools only. With a built
# archmap (debug/archmap in $CARGO_TARGET_DIR or target/, or
# $ARCHMAP_TEST_BIN), one case runs the real binary against a root whose
# origin is not on github.com.

here=$(cd "$(dirname "$0")" && pwd)
hook="$here/refresh-work.sh"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
failed=0
root="$work/project dir"
mkdir -p "$root"

fail() {
  echo "FAIL: $1"
  failed=1
}

# A bin directory with the stand-ins named, each recording its calls in
# $work/log: `archmap` answers `fetch github --help` with $HELP and prints on
# both streams otherwise; `gh auth token` succeeds when $TOKEN is set.
bin() {
  dir="$work/bin-$1"
  rm -rf "$dir"
  mkdir -p "$dir"
  shift
  for tool in "$@"; do
    case $tool in
      archmap)
        cat >"$dir/archmap" <<'STAND_IN'
#!/bin/sh
echo "archmap $*" >>"$LOG"
if [ "$1 $2 $3" = "fetch github --help" ]; then
  printf '%s\n' "$HELP"
  exit 0
fi
echo "fetched"
echo "a failure" >&2
exit "${FETCH_EXIT:-0}"
STAND_IN
        ;;
      gh)
        cat >"$dir/gh" <<'STAND_IN'
#!/bin/sh
echo "gh $*" >>"$LOG"
if [ "$1 $2" = "auth token" ] && [ -n "${TOKEN:-}" ]; then
  echo "a token"
  exit 0
fi
echo "no oauth token" >&2
exit 1
STAND_IN
        ;;
    esac
    chmod +x "$dir/$tool"
  done
  echo "$dir"
}

HELP_NEW="      --if-older-than <IF_OLDER_THAN>  An automatic refresh"
HELP_OLD="      --update  Update the snapshot"
HELP_MENTION="      --max-items <MAX_ITEMS>  with --if-older-than, the snapshot's own"

# Run the hook: $1 the bin directory, then NAME=value settings. Leaves its
# exit code, output and the calls in $code, $out and $calls.
run() {
  path="$1:/usr/bin:/bin"
  shift
  : >"$work/log"
  out=$(env -i PATH="$path" HOME="$work" LOG="$work/log" HELP="$HELP_NEW" \
    CLAUDE_PROJECT_DIR="$root" "$@" sh "$hook" </dev/null 2>&1)
  code=$?
  calls=$(cat "$work/log")
}

# The hook succeeded silently, asked no fetch, and wrote nothing.
skipped() {
  [ "$code" = 0 ] || fail "$1: exit $code"
  [ -z "$out" ] || fail "$1: printed: $out"
  case $calls in *"fetch github --if-older-than"*) fail "$1: fetched: $calls" ;; esac
  [ -z "$(ls -A "$root")" ] || fail "$1: wrote into the root: $(ls -A "$root")"
}

all=$(bin all archmap gh)

run "$all" TOKEN=1
skipped "off unless turned on"
[ -z "$calls" ] || fail "off: asked: $calls"

run "$all" TOKEN=1 CLAUDE_PLUGIN_OPTION_AUTO_REFRESH=false
skipped "turned off"
[ -z "$calls" ] || fail "turned off: asked: $calls"

run "$(bin gh-only gh)" TOKEN=1 CLAUDE_PLUGIN_OPTION_AUTO_REFRESH=true
skipped "no archmap"
[ -z "$calls" ] || fail "no archmap: asked: $calls"

run "$all" TOKEN=1 CLAUDE_PLUGIN_OPTION_AUTO_REFRESH=true HELP="$HELP_OLD"
skipped "an archmap without --if-older-than"
[ "$calls" = "archmap fetch github --help" ] || fail "an old archmap: asked: $calls"

run "$all" TOKEN=1 CLAUDE_PLUGIN_OPTION_AUTO_REFRESH=true HELP="$HELP_MENTION"
skipped "an archmap whose help only mentions --if-older-than"
[ "$calls" = "archmap fetch github --help" ] || fail "a mention: asked: $calls"

run "$(bin archmap-only archmap)" TOKEN=1 CLAUDE_PLUGIN_OPTION_AUTO_REFRESH=true
skipped "no gh"

run "$all" CLAUDE_PLUGIN_OPTION_AUTO_REFRESH=true
skipped "no github.com login"
case $calls in *"gh auth token --hostname github.com"*) ;; *) fail "no login: never asked gh: $calls" ;; esac

run "$all" TOKEN=1 CLAUDE_PLUGIN_OPTION_AUTO_REFRESH=true CLAUDE_PROJECT_DIR="$work/missing"
skipped "no project directory"

run "$all" TOKEN=1 CLAUDE_PLUGIN_OPTION_AUTO_REFRESH=true
[ "$code" = 0 ] || fail "on: exit $code"
[ -z "$out" ] || fail "on: printed: $out"
last=$(printf '%s\n' "$calls" | tail -n 1)
[ "$last" = "archmap fetch github --if-older-than 30m --path $root" ] ||
  fail "on: fetched as: $last"

run "$all" TOKEN=1 CLAUDE_PLUGIN_OPTION_AUTO_REFRESH=1 CLAUDE_PLUGIN_OPTION_AUTO_REFRESH_INTERVAL=2h
last=$(printf '%s\n' "$calls" | tail -n 1)
[ "$last" = "archmap fetch github --if-older-than 2h --path $root" ] ||
  fail "an interval: fetched as: $last"

# a fetch that fails says nothing to the session, and the hook succeeds
run "$all" TOKEN=1 CLAUDE_PLUGIN_OPTION_AUTO_REFRESH=true FETCH_EXIT=1
[ "$code" = 0 ] || fail "a failed fetch: exit $code"
[ -z "$out" ] || fail "a failed fetch: printed: $out"

# with the real archmap, which the hook gets past its checks to run, a root
# whose origin is not on github.com gets nothing: archmap names no
# repository from it and stops before writing
real=${ARCHMAP_TEST_BIN:-${CARGO_TARGET_DIR:-$here/../../../target}/debug/archmap}
if [ -x "$real" ]; then
  with_real=$(bin real gh)
  # records each call, then runs the real binary
  cat >"$with_real/archmap" <<'STAND_IN'
#!/bin/sh
echo "archmap $*" >>"$LOG"
exec "$REAL" "$@"
STAND_IN
  chmod +x "$with_real/archmap"
  git -C "$root" init -q
  git -C "$root" remote add origin https://git.example.com/acme/shop.git
  run "$with_real" TOKEN=1 CLAUDE_PLUGIN_OPTION_AUTO_REFRESH=true \
    REAL="$(cd "$(dirname "$real")" && pwd)/$(basename "$real")"
  [ "$code" = 0 ] || fail "another host: exit $code"
  [ -z "$out" ] || fail "another host: printed: $out"
  last=$(printf '%s\n' "$calls" | tail -n 1)
  [ "$last" = "archmap fetch github --if-older-than 30m --path $root" ] ||
    fail "another host: the real archmap was not asked to fetch: $calls"
  [ ! -e "$root/.archmap" ] || fail "another host: wrote $(ls -A "$root/.archmap")"
  rm -rf "$root/.git"
else
  echo "note: no built archmap at $real; the case with the real binary did not run"
fi

# what Claude Code is told: in the background, on every start but compact
hooks="$here/hooks.json"
grep -q '"async": true' "$hooks" || fail "hooks.json: not async"
grep -q '"matcher": "startup|resume|clear|fork"' "$hooks" || fail "hooks.json: matcher"
grep -q 'refresh-work.sh' "$hooks" || fail "hooks.json: another command"

if [ "$failed" = 0 ]; then
  echo "refresh-work.sh: all cases passed"
fi
exit "$failed"
