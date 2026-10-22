#!/bin/bash
# Tests for the Claude Code plugin (plugin/): shellcheck, manifests, the
# install script (fixture release, fake curl and codesign, temp HOME) and
# both hooks (fake btp). Never touches the network, the real HOME or
# ~/.local/bin.
#
#   scripts/test-plugin.sh

# The bash -c snippets take their values as arguments, single quotes on purpose.
# shellcheck disable=SC2016

set -uo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
plugin="$repo/plugin"
work="$(mktemp -d "${TMPDIR:-/tmp}/btp-plugin-tests.XXXXXX")"
trap 'rm -rf "$work"' EXIT

pass=0
fail=0
ok() {
  pass=$((pass + 1))
  printf 'ok   %s\n' "$1"
}
ko() {
  fail=$((fail + 1))
  printf 'FAIL %s\n' "$1"
  [ $# -lt 2 ] || printf '     %s\n' "$2"
}
check() { # check NAME CMD...
  local name="$1"
  shift
  if "$@"; then ok "$name"; else ko "$name"; fi
}

# A PATH without any real btp (only the fakes of each test come first).
BASE_PATH="/usr/bin:/bin:/usr/sbin:/sbin"
unset CLAUDE_CONFIG_DIR CLAUDE_PLUGIN_DATA XDG_DATA_HOME BTP_PLUGIN_MIN_VERSION

# --- shellcheck -------------------------------------------------------------

if command -v shellcheck >/dev/null; then
  if out="$(shellcheck -x "$plugin"/hooks/*.sh "$plugin"/scripts/*.sh "$repo/scripts/test-plugin.sh" 2>&1)"; then
    ok "shellcheck"
  else
    ko "shellcheck" "$out"
  fi
else
  ko "shellcheck" "shellcheck is not installed"
fi

# --- manifests --------------------------------------------------------------

for f in "$plugin/.claude-plugin/plugin.json" "$repo/.claude-plugin/marketplace.json" "$plugin/hooks/hooks.json"; do
  check "valid JSON: ${f#"$repo"/}" jq -e . "$f" >/dev/null
done
check "marketplace lists the plugin at ./plugin" jq -e '.plugins[] | select(.name == "balance-ton-port" and .source == "./plugin")' \
  "$repo/.claude-plugin/marketplace.json" >/dev/null
check "hooks.json wires both hooks" jq -e '
  (.hooks.SessionStart[0].hooks[0].command == "\"${CLAUDE_PLUGIN_ROOT}/hooks/session-start.sh\"")
  and (.hooks.SessionEnd[0].hooks[0].command == "\"${CLAUDE_PLUGIN_ROOT}/hooks/session-end.sh\"")' \
  "$plugin/hooks/hooks.json" >/dev/null
for p in hooks/session-start.sh hooks/session-end.sh scripts/install-btp.sh; do
  check "executable: plugin/$p" test -x "$plugin/$p"
done
check "install command exists" test -f "$plugin/commands/install.md"
check "install command clears the decline" grep -q -- '--clear-decline' "$plugin/commands/install.md"
check "skill ships in the plugin" test -f "$plugin/skills/balance-ton-port/SKILL.md"
if command -v claude >/dev/null; then
  check "claude plugin validate (plugin)" claude plugin validate "$plugin" >/dev/null 2>&1
  check "claude plugin validate (marketplace)" claude plugin validate "$repo/.claude-plugin/marketplace.json" >/dev/null 2>&1
fi

# --- lib.sh -----------------------------------------------------------------

# shellcheck source=SCRIPTDIR/../plugin/scripts/lib.sh
. "$plugin/scripts/lib.sh"
v="$BTP_MIN_VERSION"

cargo_version="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$repo/crates/btp-cli/Cargo.toml")"
if btp_version_ge "$cargo_version" "$BTP_MIN_VERSION"; then
  ok "BTP_MIN_VERSION ($BTP_MIN_VERSION) <= the btp-cli version ($cargo_version)"
else
  # Expected between raising the minimum and the release bump; the release
  # workflow refuses a tag below it.
  printf 'note btp-cli is %s, below BTP_MIN_VERSION %s: bump it before the release\n' \
    "$cargo_version" "$BTP_MIN_VERSION"
fi
check "release workflow checks BTP_MIN_VERSION" grep -q BTP_MIN_VERSION "$repo/.github/workflows/release.yml"
check "version 0.2.0 >= 0.1.0" btp_version_ge 0.2.0 0.1.0
check "version 0.1.0 >= 0.1.0" btp_version_ge 0.1.0 0.1.0
check "version 0.10.0 >= 0.9.3" btp_version_ge 0.10.0 0.9.3
check "version 1.0.0-beta.1 >= 1.0.0" btp_version_ge 1.0.0-beta.1 1.0.0
check "version 0.0.9 < 0.1.0" bash -c '. "$1"; ! btp_version_ge 0.0.9 0.1.0' _ "$plugin/scripts/lib.sh"
check "garbage version is not >=" bash -c '. "$1"; ! btp_version_ge abc 0.1.0' _ "$plugin/scripts/lib.sh"
s=$'quote " backslash \\ tab \t newline\nend'
check "json escape round-trips through jq" test \
  "$(printf '"%s"' "$(btp_json_escape "$s")" | jq -r .)" = "$s"
check "json field: reason" test \
  "$(btp_json_field reason '{"session_id":"abc-1","reason": "prompt_input_exit","cwd":"/x"}')" = prompt_input_exit

# --- fakes ------------------------------------------------------------------

fakebin="$work/fakebin"
mkdir -p "$fakebin"
cat >"$fakebin/btp" <<'EOF'
#!/bin/bash
echo "$*" >>"$FAKE_BTP_LOG"
case "${1:-}" in
  --version) echo "btp ${FAKE_BTP_VERSION:-$FAKE_BTP_DEFAULT}" ;;
  stale)
    if [ "${2:-}" = --help ]; then
      [ -n "${FAKE_BTP_OLD:-}" ] && exit 1
      exit 0
    fi
    printf '%s\n' "${FAKE_STALE:-[]}"
    exit "${FAKE_STALE_EXIT:-0}"
    ;;
  session-end)
    ps -o pgid= -p $$ | tr -d ' ' >"$FAKE_BTP_LOG.pgid"
    sleep "${FAKE_SLEEP:-0}"
    echo "session-end output"
    echo "session-end error" >&2
    ;;
esac
EOF
chmod +x "$fakebin/btp"
export FAKE_BTP_LOG="$work/btp.log" FAKE_BTP_DEFAULT="$v"

fix="$work/fixture"
mkdir -p "$fix/bin"
cat >"$fix/bin/curl" <<'EOF'
#!/bin/bash
# Fake curl: serves files from $FAKE_RELEASE_DIR by URL.
out="" url=""
while [ $# -gt 0 ]; do
  case "$1" in
    -o) out="$2"; shift 2 ;;
    --proto | --retry | --max-time) shift 2 ;;
    -*) shift ;;
    *) url="$1"; shift ;;
  esac
done
echo "$url" >>"$FAKE_RELEASE_DIR/curl.log"
case "$url" in
  https://api.github.com/repos/matthieuh/balance-ton-port/releases/latest) src="$FAKE_RELEASE_DIR/latest.json" ;;
  https://github.com/matthieuh/balance-ton-port/releases/download/*) src="$FAKE_RELEASE_DIR/${url##*/}" ;;
  *) exit 6 ;;
esac
[ -f "$src" ] || exit 22
cp "$src" "$out"
EOF
cat >"$fix/bin/codesign" <<'EOF'
#!/bin/bash
# Fake codesign: only accepts the exact Developer ID requirement; fails it
# when FAKE_CS_BAD is set (a wrong team or a self-made certificate).
req='-R=anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists and certificate leaf[field.1.2.840.113635.100.6.1.13] exists and certificate leaf[subject.OU] = "47P2KJ2HF3"'
echo "$*" >>"$FAKE_RELEASE_DIR/codesign.log"
[ "$1" = --verify ] && [ "$2" = --strict ] && [ "$3" = "$req" ] && [ -f "$4" ] || exit 1
[ -z "${FAKE_CS_BAD:-}" ] || exit 3
EOF
chmod +x "$fix/bin/curl" "$fix/bin/codesign"

# make_release VERSION: a fixture release in a fresh dir; sets rel.
make_release() {
  local name="btp-$1-macos-universal.tar.gz"
  rel="$work/release-$1-$RANDOM"
  mkdir -p "$rel/stage"
  printf '#!/bin/bash\necho "btp %s"\n' "$1" >"$rel/stage/btp"
  chmod +x "$rel/stage/btp"
  echo MIT >"$rel/stage/LICENSE"
  pack_release "$1"
  printf '{"tag_name": "v%s", "name": "v%s"}\n' "$1" "$1" >"$rel/latest.json"
}
pack_release() { # after editing $rel/stage
  local name="btp-$1-macos-universal.tar.gz"
  tar -czf "$rel/$name" -C "$rel/stage" btp LICENSE
  (cd "$rel" && shasum -a 256 "$name" >"$name.sha256")
}

# run_hook HOOK PAYLOAD [with-btp]: runs a hook like Claude Code does, in
# $work/home, with or without the fake btp on PATH (the fake curl always).
# HOOK_ENV adds env assignments. Sets out, err, code, ms.
run_hook() {
  local hook="$1" payload="$2" path="$fix/bin:$BASE_PATH" t0 t1
  [ "${3:-}" = with-btp ] && path="$fakebin:$path"
  : >"$FAKE_BTP_LOG"
  t0="$(perl -MTime::HiRes=time -e 'printf "%d", time*1000')"
  # shellcheck disable=SC2086 # HOOK_ENV is a list of assignments
  out="$(printf '%s' "$payload" | env HOME="$work/home" PATH="$path" FAKE_RELEASE_DIR="${rel:-$work}" \
    CLAUDE_PLUGIN_DATA="$work/data" ${HOOK_ENV:-} "$plugin/hooks/$hook" 2>"$work/err")"
  code=$?
  t1="$(perl -MTime::HiRes=time -e 'printf "%d", time*1000')"
  ms=$((t1 - t0))
  err="$(cat "$work/err")"
}
mkdir -p "$work/home"
no_session_end() { ! grep -q session-end "$FAKE_BTP_LOG"; }

# --- session-end.sh ---------------------------------------------------------

end_payload='{"session_id":"conv-1","transcript_path":"/x.jsonl","cwd":"/x","hook_event_name":"SessionEnd","reason":"prompt_input_exit"}'
# The child runs detached: give it a moment before reading its log.
settle() { sleep 0.3; }

run_hook session-end.sh "$end_payload"
check "session-end: btp missing exits 0" test "$code" = 0
check "session-end: btp missing prints nothing" test -z "$out$err"

run_hook session-end.sh "$end_payload" with-btp
settle
check "session-end: exits 0 with btp" test "$code" = 0
check "session-end: prints nothing (btp output swallowed)" test -z "$out$err"
check "session-end: calls btp session-end with the parent pid, reason and id" \
  grep -Eq '^session-end --session-pid=[0-9]+ --reason=prompt_input_exit --session-id=conv-1$' "$FAKE_BTP_LOG"

run_hook session-end.sh "${end_payload/conv-1/-rf}" with-btp
settle
check "session-end: an id starting with - stays a value" grep -q -- ' --session-id=-rf$' "$FAKE_BTP_LOG"

for r in clear resume; do
  run_hook session-end.sh "${end_payload/prompt_input_exit/$r}" with-btp
  settle
  check "session-end: reason $r exits 0 silently" test "$code:$out$err" = "0:"
  check "session-end: reason $r never runs btp" test ! -s "$FAKE_BTP_LOG"
done
for r in other logout; do
  run_hook session-end.sh "${end_payload/prompt_input_exit/$r}" with-btp
  settle
  check "session-end: reason $r runs btp session-end" grep -q "^session-end .*--reason=$r" "$FAKE_BTP_LOG"
done

FAKE_BTP_VERSION=0.1.0 run_hook session-end.sh "$end_payload" with-btp
settle
check "session-end: too old btp (version) exits 0 silently" test "$code:$out$err" = "0:"
check "session-end: too old btp (version) is not called" no_session_end
FAKE_BTP_OLD=1 run_hook session-end.sh "$end_payload" with-btp
settle
check "session-end: btp without the session commands is not called" no_session_end
HOOK_ENV="BTP_PLUGIN_MIN_VERSION=0.1.0" FAKE_BTP_VERSION=0.1.0 run_hook session-end.sh "$end_payload" with-btp
settle
check "session-end: BTP_PLUGIN_MIN_VERSION lowers the minimum" grep -q '^session-end ' "$FAKE_BTP_LOG"

FAKE_SLEEP=3 run_hook session-end.sh "$end_payload" with-btp
check "session-end: a hung btp is not waited for (${ms} ms)" test "$code" = 0 -a "$ms" -lt 1500
settle
check "session-end: btp runs outside the hook's process group" \
  test -s "$FAKE_BTP_LOG.pgid" -a "$(cat "$FAKE_BTP_LOG.pgid")" != "$(ps -o pgid= -p $$ | tr -d ' ')"
check "session-end: the hung btp still runs after the hook returned" pgrep -f "$fakebin/btp session-end" >/dev/null
pkill -f "$fakebin/btp session-end"

FAKE_BTP_VERSION=x FAKE_SLEEP=0 run_hook session-end.sh "not json" with-btp
check "session-end: garbage payload exits 0 silently" test "$code:$out$err" = "0:"

# --- session-start.sh -------------------------------------------------------

start_payload='{"session_id":"conv-2","transcript_path":"/x.jsonl","cwd":"/x","hook_event_name":"SessionStart","source":"startup"}'
context() { printf '%s' "$out" | jq -r '.hookSpecificOutput.additionalContext'; }
asks_install() { grep -q "ask the user once whether to install" <<<"$(context)"; }

rm -rf "$work/data"
run_hook session-start.sh "$start_payload"
check "session-start: btp missing exits 0" test "$code" = 0
check "session-start: btp missing emits valid hook JSON" \
  bash -c 'printf "%s" "$1" | jq -e ".hookSpecificOutput.hookEventName == \"SessionStart\"" >/dev/null' _ "$out"
check "session-start: install context asks first and names the script" \
  bash -c 'grep -q "ask the user" <<<"$1" && grep -q "install-btp.sh\" --data-dir \"$2\"" <<<"$1"' _ "$(context)" "$work/data"
check "session-start: install context gives the opt-out marker" \
  grep -q "touch \"$work/data/install-declined\"" <<<"$(context)"

FAKE_BTP_VERSION=0.1.0 run_hook session-start.sh "$start_payload" with-btp
check "session-start: too old btp is offered an update" grep -q "is too old" <<<"$(context)"

for src in clear resume compact; do
  run_hook session-start.sh "${start_payload/startup/$src}"
  check "session-start: no install question on source $src" test "$code:$out" = "0:"
done

out="$(printf '%s' "$start_payload" | env -u CLAUDE_PLUGIN_DATA HOME="$work/home" PATH="$BASE_PATH" \
  "$plugin/hooks/session-start.sh")"
check "session-start: data dir falls back to ~/.local/share without CLAUDE_PLUGIN_DATA" \
  grep -q "touch \"$work/home/.local/share/balance-ton-port-plugin/install-declined\"" <<<"$(context)"

mkdir -p "$work/data" && touch "$work/data/install-declined"
run_hook session-start.sh "$start_payload"
check "session-start: declined install stays silent" test "$code:$out" = "0:"
rm -rf "$work/data"

# A refused release keeps the question away until a newer tag exists.
make_release 0.1.0
mkdir -p "$work/data" && echo v0.1.0 >"$work/data/install-refused"
run_hook session-start.sh "$start_payload"
check "session-start: refused release, no newer one: silent" test "$code:$out" = "0:"
check "session-start: ... and the latest tag is cached" test "$(cat "$work/data/latest-release-tag")" = v0.1.0
printf '{"tag_name": "v%s"}\n' "$v" >"$rel/latest.json"
: >"$rel/curl.log"
run_hook session-start.sh "$start_payload"
check "session-start: within a day the cache is used (silent, no lookup)" \
  test "$code:$out:$(wc -l <"$rel/curl.log" | tr -d ' ')" = "0::0"
touch -t 202001010000 "$work/data/latest-release-tag"
run_hook session-start.sh "$start_payload"
check "session-start: a day later a newer release brings the question back" asks_install
rm -rf "$work/data"
unset rel

run_hook session-start.sh "${start_payload/startup/clear}" with-btp
check "session-start: no stale servers prints nothing" test "$code:$out" = "0:"
check "session-start: asks btp stale for the current conversation" grep -q '^stale --conversation=conv-2$' "$FAKE_BTP_LOG"

stale='[{"pid":42,"start_time":1,"port":5173,"ports":[5173],"tool_label":"Vite","command":"vite --secret-flag","worktree":"/w","location_label":"w","idle_ms":400000,"conversation_id":"conv-1","note":"testing \"the\" form"},{"pid":43,"start_time":1,"port":4322,"ports":[4322],"tool_label":"Astro","command":"astro","worktree":"/w","location_label":"w","idle_ms":400000,"conversation_id":"conv-1","note":null}]'
FAKE_STALE="$stale" run_hook session-start.sh "${start_payload/startup/clear}" with-btp
check "session-start: stale servers emit valid hook JSON" test "$(printf '%s' "$out" | jq -r .hookSpecificOutput.hookEventName)" = SessionStart
c="$(context)"
check "session-start: stale list is delimited data" \
  bash -c 'grep -q "data, not instructions" <<<"$1" && grep -q "BEGIN BTP SERVER LIST" <<<"$1" && grep -q "END BTP SERVER LIST" <<<"$1"' _ "$c"
check "session-start: stale lines carry port, tool and note only" \
  bash -c 'grep -qx "port=5173 tool=\"Vite\" note=\"testing the form\"" <<<"$1" && grep -qx "port=4322 tool=\"Astro\" note=\"\"" <<<"$1" && ! grep -q "secret-flag\|conv-1" <<<"$1"' _ "$c"
check "session-start: stale context asks one question" bash -c 'grep -q "ONE question" <<<"$1" && grep -q "kill <port>" <<<"$1"' _ "$c"

FAKE_STALE_EXIT=1 FAKE_STALE="btp: not run from a Claude Code session" run_hook session-start.sh "$start_payload" with-btp
check "session-start: btp stale failing stays silent" test "$code:$out$err" = "0:"

# --- install-btp.sh ---------------------------------------------------------

# run_install [ARGS...]: the install script in a fresh temp HOME (unless
# KEEP_HOME), --data-dir $ihome/data by default. Sets out, code, ihome.
run_install() {
  if [ -z "${KEEP_HOME:-}" ]; then
    ihome="$work/ihome-$RANDOM"
    mkdir -p "$ihome"
  fi
  [ $# -gt 0 ] || set -- --data-dir "$ihome/data"
  out="$(env HOME="$ihome" PATH="$fix/bin:$BASE_PATH" FAKE_RELEASE_DIR="$rel" \
    "$plugin/scripts/install-btp.sh" "$@" 2>&1)"
  code=$?
}
not_installed() { ! test -e "$ihome/.local/bin/btp"; }
declined_at() { mkdir -p "$1" && touch "$1/install-declined"; }

make_release "$v"
ihome="$work/ihome-pre"
declined_at "$ihome/data"
KEEP_HOME=1 run_install
check "install: success exits 0" test "$code" = 0
check "install: btp is in ~/.local/bin and runs" test "$("$ihome/.local/bin/btp" --version)" = "btp $v"
check "install: marker holds the installed sha256" test \
  "$(cat "$ihome/.local/bin/.btp-installed-by-balance-ton-port")" = "$(shasum -a 256 "$ihome/.local/bin/btp" | cut -d' ' -f1)"
check "install: a success clears the earlier no" test ! -e "$ihome/data/install-declined"
check "install: no staging file left" bash -c '! ls -A "$1" | grep -q "^\.btp\.new"' _ "$ihome/.local/bin"
check "install: PATH hint when ~/.local/bin is not on PATH" grep -q 'export PATH="$HOME/.local/bin:$PATH"' <<<"$out"
check "install: codesign got the Developer ID requirement" test -s "$rel/codesign.log"
check "install: only GitHub URLs fetched" bash -c '! grep -qv "^https://api\.github\.com/repos/matthieuh/\|^https://github\.com/matthieuh/" "$1"' _ "$rel/curl.log"

make_release "$v"
KEEP_HOME=1 run_install
check "install: replaces a btp it installed (update)" test "$code" = 0

make_release "$v"
echo "0000000000000000000000000000000000000000000000000000000000000000  btp-$v-macos-universal.tar.gz" \
  >"$rel/btp-$v-macos-universal.tar.gz.sha256"
ihome="$work/ihome-badsha"
declined_at "$ihome/data"
KEEP_HOME=1 run_install
check "install: bad sha256 is refused" test "$code" != 0
check "install: bad sha256 says why" grep -q "sha256 mismatch" <<<"$out"
check "install: bad sha256 installs nothing" not_installed
check "install: a failed install keeps the user's no" test -e "$ihome/data/install-declined"
KEEP_HOME=1 run_install --data-dir "$ihome/data" --clear-decline
check "install: --clear-decline clears the no even when the install fails" \
  test "$code" != 0 -a ! -e "$ihome/data/install-declined"

make_release "$v"
(cd "$rel" && printf '%s  btp-%s-macos-universal.tar.gz' "$(shasum -a 256 "btp-$v-macos-universal.tar.gz" | cut -d' ' -f1)" "$v" \
  >"btp-$v-macos-universal.tar.gz.sha256")
run_install
check "install: a .sha256 without trailing newline is accepted" test "$code" = 0

make_release "$v"
(cd "$rel" && shasum -a 256 "btp-$v-macos-universal.tar.gz" | sed 's/  .*/  other.tar.gz/' \
  >"btp-$v-macos-universal.tar.gz.sha256")
run_install
check "install: sha256 file for another file is refused" test "$code" != 0 -a ! -e "$ihome/.local/bin/btp"

make_release "$v"
FAKE_CS_BAD=1 run_install
check "install: a signature failing the Developer ID requirement is refused" test "$code" != 0
check "install: ... says why" grep -q "not signed with the project's Developer ID" <<<"$out"
check "install: ... installs nothing" not_installed

make_release "$v"
ihome="$work/ihome-foreign"
mkdir -p "$ihome/.local/bin"
printf '#!/bin/bash\necho mine\n' >"$ihome/.local/bin/btp"
chmod +x "$ihome/.local/bin/btp"
KEEP_HOME=1 run_install
check "install: refuses to overwrite a btp it did not install" test "$code" != 0
check "install: the foreign btp is untouched" test "$("$ihome/.local/bin/btp")" = mine

make_release "$v"
ihome="$work/ihome-pre"
echo "# changed" >>"$ihome/.local/bin/btp"
before="$(shasum -a 256 "$ihome/.local/bin/btp")"
KEEP_HOME=1 run_install
check "install: refuses a btp it installed that changed since" test "$code" != 0
check "install: the changed btp is left untouched" test "$(shasum -a 256 "$ihome/.local/bin/btp")" = "$before"

make_release 0.1.0
run_install
check "install: a release older than the plugin needs is refused" test "$code" != 0 -a ! -e "$ihome/.local/bin/btp"
check "install: ... and its tag is recorded" test "$(cat "$ihome/data/install-refused")" = v0.1.0

make_release "$v"
printf '#!/bin/bash\necho "btp 9.9.9"\n' >"$rel/stage/btp"
pack_release "$v"
run_install
check "install: a binary reporting another version is refused" test "$code" != 0 -a ! -e "$ihome/.local/bin/btp"

make_release "$v"
printf '#!/bin/bash\n[ "$1" = stale ] && exit 2\necho "btp %s"\n' "$v" >"$rel/stage/btp"
pack_release "$v"
run_install
check "install: a btp without the session commands is refused" test "$code" != 0 -a ! -e "$ihome/.local/bin/btp"
check "install: ... and its tag is recorded" test "$(cat "$ihome/data/install-refused")" = "v$v"

make_release "$v"
ihome="$work/ihome-nodata"
mkdir -p "$ihome/.claude-perso/plugins/data/balance-ton-port-balance-ton-port"
declined_at "$ihome/.claude-perso/plugins/data/balance-ton-port-balance-ton-port"
declined_at "$ihome/.local/share/balance-ton-port-plugin"
KEEP_HOME=1 run_install --data-dir "" --clear-decline
check "install: no data dir given: clears the no in Claude Code's data dirs" \
  test "$code" = 0 -a ! -e "$ihome/.claude-perso/plugins/data/balance-ton-port-balance-ton-port/install-declined"
check "install: ... and in the hooks' fallback dir" test ! -e "$ihome/.local/share/balance-ton-port-plugin/install-declined"

# --- no em dash -------------------------------------------------------------

emdash="$(printf '\xe2\x80\x94')"
if grep -rl "$emdash" "$repo/.claude-plugin" "$plugin" "$repo/README.md" "$repo/RELEASING.md" "$repo/scripts/test-plugin.sh" >/dev/null; then
  ko "no em dash in the plugin files"
else
  ok "no em dash in the plugin files"
fi

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
