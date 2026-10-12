#!/bin/sh
# Tests of refresh-work.sh, run with `sh plugins/archmap/hooks/refresh-work.test.sh`
# from anywhere. Stand-ins for archmap, gh and ssh record their calls; the
# hook runs with a PATH that holds them and the system's tools only. With a
# built archmap (debug/archmap in $CARGO_TARGET_DIR or target/, or
# $ARCHMAP_TEST_BIN), the last cases run the real binary, which the hook
# reaches: an origin on another host with no snapshot, an SSH alias of
# github.com with a fresh snapshot, an alias ssh ties to no host, and an
# alias with no login for its host.

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
# both streams otherwise; `gh auth token --hostname H` succeeds when $TOKEN
# lists H; `ssh
# -G` takes the alias github-work to github.com and names nothing else.
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
if [ "$1 $2" = "auth token" ]; then
  host=github.com
  [ "$3" = "--hostname" ] && host=$4
  case " ${TOKEN:-} " in
    *" $host "*) echo "a token"; exit 0 ;;
  esac
fi
echo "no oauth token" >&2
exit 1
STAND_IN
        ;;
      ssh)
        cat >"$dir/ssh" <<'STAND_IN'
#!/bin/sh
echo "ssh $*" >>"$LOG"
[ "$1" = "-G" ] || exit 255
case $2 in
  github-work) echo "hostname github.com" ;;
  *) echo "hostname $2" ;;
esac
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

run "$all" TOKEN=github.com
skipped "off unless turned on"
[ -z "$calls" ] || fail "off: asked: $calls"

run "$all" TOKEN=github.com CLAUDE_PLUGIN_OPTION_AUTO_REFRESH=false
skipped "turned off"
[ -z "$calls" ] || fail "turned off: asked: $calls"

run "$(bin gh-only gh)" TOKEN=github.com CLAUDE_PLUGIN_OPTION_AUTO_REFRESH=true
skipped "no archmap"
[ -z "$calls" ] || fail "no archmap: asked: $calls"

run "$all" TOKEN=github.com CLAUDE_PLUGIN_OPTION_AUTO_REFRESH=true HELP="$HELP_OLD"
skipped "an archmap without --if-older-than"
[ "$calls" = "archmap fetch github --help" ] || fail "an old archmap: asked: $calls"

run "$all" TOKEN=github.com CLAUDE_PLUGIN_OPTION_AUTO_REFRESH=true HELP="$HELP_MENTION"
skipped "an archmap whose help only mentions --if-older-than"
[ "$calls" = "archmap fetch github --help" ] || fail "a mention: asked: $calls"

run "$(bin archmap-only archmap)" TOKEN=github.com CLAUDE_PLUGIN_OPTION_AUTO_REFRESH=true
skipped "no gh"

# which host, and gh's login for it, are archmap's to tell
run "$all" CLAUDE_PLUGIN_OPTION_AUTO_REFRESH=true
case $calls in *"gh auth"*) fail "a login: the hook asked gh: $calls" ;; esac
last=$(printf '%s\n' "$calls" | tail -n 1)
[ "$last" = "archmap fetch github --if-older-than 30m --path $root" ] ||
  fail "a login: never left to archmap: $calls"

run "$all" TOKEN=github.com CLAUDE_PLUGIN_OPTION_AUTO_REFRESH=true CLAUDE_PROJECT_DIR="$work/missing"
skipped "no project directory"

run "$all" TOKEN=github.com CLAUDE_PLUGIN_OPTION_AUTO_REFRESH=true
[ "$code" = 0 ] || fail "on: exit $code"
[ -z "$out" ] || fail "on: printed: $out"
last=$(printf '%s\n' "$calls" | tail -n 1)
[ "$last" = "archmap fetch github --if-older-than 30m --path $root" ] ||
  fail "on: fetched as: $last"

run "$all" TOKEN=github.com CLAUDE_PLUGIN_OPTION_AUTO_REFRESH=1 CLAUDE_PLUGIN_OPTION_AUTO_REFRESH_INTERVAL=2h
last=$(printf '%s\n' "$calls" | tail -n 1)
[ "$last" = "archmap fetch github --if-older-than 2h --path $root" ] ||
  fail "an interval: fetched as: $last"

# a fetch that fails says nothing to the session, and the hook succeeds
run "$all" TOKEN=github.com CLAUDE_PLUGIN_OPTION_AUTO_REFRESH=true FETCH_EXIT=1
[ "$code" = 0 ] || fail "a failed fetch: exit $code"
[ -z "$out" ] || fail "a failed fetch: printed: $out"

# with the real archmap, which the hook gets past its checks to run, a root
# whose origin is not on github.com gets nothing: archmap names no
# repository from it and stops before writing
real=${ARCHMAP_TEST_BIN:-${CARGO_TARGET_DIR:-$here/../../../target}/debug/archmap}
if [ -x "$real" ]; then
  with_real=$(bin real gh ssh)
  # records each call, then runs the real binary
  cat >"$with_real/archmap" <<'STAND_IN'
#!/bin/sh
echo "archmap $*" >>"$LOG"
exec "$REAL" "$@"
STAND_IN
  chmod +x "$with_real/archmap"
  git -C "$root" init -q
  git -C "$root" remote add origin https://git.example.com/acme/shop.git
  run "$with_real" TOKEN=github.com CLAUDE_PLUGIN_OPTION_AUTO_REFRESH=true \
    REAL="$(cd "$(dirname "$real")" && pwd)/$(basename "$real")"
  [ "$code" = 0 ] || fail "another host: exit $code"
  [ -z "$out" ] || fail "another host: printed: $out"
  last=$(printf '%s\n' "$calls" | tail -n 1)
  [ "$last" = "archmap fetch github --if-older-than 30m --path $root" ] ||
    fail "another host: the real archmap was not asked to fetch: $calls"
  [ ! -e "$root/.archmap" ] || fail "another host: wrote $(ls -A "$root/.archmap")"
  # an SSH alias: the real archmap takes the repository from the snapshot a
  # fetch with --repo wrote, finds it fresh, and asks GitHub nothing
  git -C "$root" remote set-url origin git@github-work:acme/shop.git
  mkdir -p "$root/.archmap"
  snapshot="{\"schema\": 1, \"source\": \"github\", \"host\": \"github.com\",
    \"repository\": \"acme/shop\", \"fetched_at\": \"$(date -u +%Y-%m-%dT%H:%M:%SZ)\",
    \"range\": {\"since_rule\": \"all\", \"bound\": 5000, \"issues\": 0, \"pull_requests\": 0},
    \"relation_types\": [], \"items\": [], \"relations\": []}"
  printf '%s\n' "$snapshot" >"$root/.archmap/github.json"
  run "$with_real" TOKEN=github.com CLAUDE_PLUGIN_OPTION_AUTO_REFRESH=true \
    REAL="$(cd "$(dirname "$real")" && pwd)/$(basename "$real")"
  [ "$code" = 0 ] || fail "an alias: exit $code"
  [ -z "$out" ] || fail "an alias: printed: $out"
  case $calls in *"gh api"*) fail "an alias: asked GitHub: $calls" ;; esac
  # the fetch ran as far as its lock, and left the snapshot as it was
  [ -e "$root/.archmap/github.json.lock" ] || fail "an alias: the fetch never ran: $calls"
  [ "$(cat "$root/.archmap/github.json")" = "$snapshot" ] || fail "an alias: the snapshot changed"
  rm "$root/.archmap/github.json.lock"
  # an alias SSH ties to no host, and a host gh holds no login for: archmap
  # stops before its lock, and nothing is written
  for case in "git@github-other:acme/shop.git TOKEN=github.com ssh -G github-other" \
    "git@github-work:acme/shop.git TOKEN= gh auth token --hostname github.com"; do
    origin=${case%% *}
    rest=${case#* }
    token=${rest%% *}
    step=${rest#* }
    git -C "$root" remote set-url origin "$origin"
    run "$with_real" "$token" CLAUDE_PLUGIN_OPTION_AUTO_REFRESH=true \
      REAL="$(cd "$(dirname "$real")" && pwd)/$(basename "$real")"
    [ "$code" = 0 ] || fail "$origin: exit $code"
    [ -z "$out" ] || fail "$origin: printed: $out"
    # archmap got as far as the step that refused it
    last=$(printf '%s\n' "$calls" | tail -n 1)
    [ "$last" = "$step" ] || fail "$origin: stopped elsewhere: $calls"
    case $calls in *"gh api"*) fail "$origin: asked GitHub: $calls" ;; esac
    [ "$(ls -A "$root/.archmap")" = "github.json" ] ||
      fail "$origin: wrote $(ls -A "$root/.archmap")"
  done
  rm -rf "$root/.git" "$root/.archmap"
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
