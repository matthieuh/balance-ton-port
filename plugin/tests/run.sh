#!/bin/bash
# Tests for the Claude Code plugin: shellcheck, manifests, the install
# script (fixture release, fake curl and codesign, temp HOME) and both hooks
# (fake btp). Never touches the network, the real HOME or ~/.local/bin.
#
#   plugin/tests/run.sh

# The bash -c snippets take their values as arguments, single quotes on purpose.
# shellcheck disable=SC2016

set -uo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
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

# --- shellcheck -------------------------------------------------------------

if command -v shellcheck >/dev/null; then
  if out="$(shellcheck -x "$plugin"/hooks/*.sh "$plugin"/scripts/*.sh "$plugin"/tests/*.sh 2>&1)"; then
    ok "shellcheck"
  else
    ko "shellcheck" "$out"
  fi
else
  ko "shellcheck" "shellcheck is not installed"
fi

# --- manifests --------------------------------------------------------------

for f in "$repo/.claude-plugin/plugin.json" "$repo/.claude-plugin/marketplace.json" "$plugin/hooks/hooks.json"; do
  check "valid JSON: ${f#"$repo"/}" jq -e . "$f" >/dev/null
done
check "marketplace lists the plugin" jq -e '.plugins[] | select(.name == "balance-ton-port" and .source == "./")' \
  "$repo/.claude-plugin/marketplace.json" >/dev/null
check "hooks.json wires both hooks" jq -e '
  (.hooks.SessionStart[0].hooks[0].command | test("plugin/hooks/session-start.sh"))
  and (.hooks.SessionEnd[0].hooks[0].command | test("plugin/hooks/session-end.sh"))' \
  "$plugin/hooks/hooks.json" >/dev/null
for p in $(jq -r '.commands, .hooks' "$repo/.claude-plugin/plugin.json"); do
  check "plugin.json path exists: $p" test -e "$repo/$p"
done
check "skill ships at the plugin's default path" test -f "$repo/skills/balance-ton-port/SKILL.md"
if command -v claude >/dev/null; then
  check "claude plugin validate (plugin)" claude plugin validate "$repo/.claude-plugin/plugin.json" >/dev/null 2>&1
  check "claude plugin validate (marketplace)" claude plugin validate "$repo/.claude-plugin/marketplace.json" >/dev/null 2>&1
fi

# --- lib.sh -----------------------------------------------------------------

# shellcheck source=SCRIPTDIR/../scripts/lib.sh
. "$plugin/scripts/lib.sh"

cargo_version="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$repo/crates/btp-cli/Cargo.toml")"
check "BTP_MIN_VERSION ($BTP_MIN_VERSION) equals the btp-cli version ($cargo_version)" \
  test "$BTP_MIN_VERSION" = "$cargo_version"
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

# --- fake btp ---------------------------------------------------------------

fakebin="$work/fakebin"
mkdir -p "$fakebin"
cat >"$fakebin/btp" <<'EOF'
#!/bin/bash
echo "$*" >>"$FAKE_BTP_LOG"
case "${1:-}" in
  --version) echo "btp ${FAKE_BTP_VERSION:-0.1.0}" ;;
  stale)
    if [ "${2:-}" = --help ]; then
      [ -n "${FAKE_BTP_OLD:-}" ] && exit 1
      exit 0
    fi
    printf '%s\n' "${FAKE_STALE:-[]}"
    exit "${FAKE_STALE_EXIT:-0}"
    ;;
  session-end)
    sleep "${FAKE_SLEEP:-0}"
    echo "session-end output"
    echo "session-end error" >&2
    ;;
esac
EOF
chmod +x "$fakebin/btp"
export FAKE_BTP_LOG="$work/btp.log"

# run_hook HOOK PAYLOAD [with-btp]: runs a hook like Claude Code does, in a
# temp HOME, with or without the fake btp on PATH. Sets out, err, code, ms.
run_hook() {
  local hook="$1" payload="$2" path="$BASE_PATH" t0 t1
  [ "${3:-}" = with-btp ] && path="$fakebin:$BASE_PATH"
  : >"$FAKE_BTP_LOG"
  t0="$(perl -MTime::HiRes=time -e 'printf "%d", time*1000')"
  out="$(printf '%s' "$payload" | env -u CLAUDE_CONFIG_DIR HOME="$work/home" PATH="$path" \
    CLAUDE_PLUGIN_DATA="$work/data" "$plugin/hooks/$hook" 2>"$work/err")"
  code=$?
  t1="$(perl -MTime::HiRes=time -e 'printf "%d", time*1000')"
  ms=$((t1 - t0))
  err="$(cat "$work/err")"
}
mkdir -p "$work/home"

end_payload='{"session_id":"conv-1","transcript_path":"/x.jsonl","cwd":"/x","hook_event_name":"SessionEnd","reason":"prompt_input_exit"}'

# --- session-end.sh ---------------------------------------------------------

run_hook session-end.sh "$end_payload"
check "session-end: btp missing exits 0" test "$code" = 0
check "session-end: btp missing prints nothing" test -z "$out$err"

run_hook session-end.sh "$end_payload" with-btp
check "session-end: exits 0 with btp" test "$code" = 0
check "session-end: prints nothing (btp output swallowed)" test -z "$out$err"
check "session-end: calls btp session-end with the parent pid, reason and id" \
  grep -Eq '^session-end --session-pid [0-9]+ --reason prompt_input_exit --session-id conv-1$' "$FAKE_BTP_LOG"

for r in clear resume; do
  run_hook session-end.sh "${end_payload/prompt_input_exit/$r}" with-btp
  check "session-end: reason $r exits 0 silently" test "$code:$out$err" = "0:"
  check "session-end: reason $r never runs btp session-end" bash -c '! grep -q session-end "$1"' _ "$FAKE_BTP_LOG"
done
for r in other logout; do
  run_hook session-end.sh "${end_payload/prompt_input_exit/$r}" with-btp
  check "session-end: reason $r runs btp session-end" grep -q "^session-end .*--reason $r" "$FAKE_BTP_LOG"
done

FAKE_BTP_VERSION=0.0.9 run_hook session-end.sh "$end_payload" with-btp
check "session-end: too old btp (version) exits 0 silently" test "$code:$out$err" = "0:"
check "session-end: too old btp (version) is not called" bash -c '! grep -q session-end "$1"' _ "$FAKE_BTP_LOG"
FAKE_BTP_OLD=1 run_hook session-end.sh "$end_payload" with-btp
check "session-end: btp without the session commands is not called" bash -c '! grep -q session-end "$1"' _ "$FAKE_BTP_LOG"

FAKE_SLEEP=5 run_hook session-end.sh "$end_payload" with-btp
check "session-end: a hung btp is not waited for (${ms} ms)" test "$code" = 0 -a "$ms" -lt 1500
pkill -f "$fakebin/btp session-end" 2>/dev/null

run_hook session-end.sh "not json" with-btp
check "session-end: garbage payload exits 0 silently" test "$code:$out$err" = "0:"

# --- session-start.sh -------------------------------------------------------

start_payload='{"session_id":"conv-2","transcript_path":"/x.jsonl","cwd":"/x","hook_event_name":"SessionStart","source":"startup"}'
context() { printf '%s' "$out" | jq -r '.hookSpecificOutput.additionalContext'; }

rm -rf "$work/data"
run_hook session-start.sh "$start_payload"
check "session-start: btp missing exits 0" test "$code" = 0
check "session-start: btp missing emits valid hook JSON" \
  bash -c 'printf "%s" "$1" | jq -e ".hookSpecificOutput.hookEventName == \"SessionStart\"" >/dev/null' _ "$out"
check "session-start: install context asks first and names the script" \
  bash -c 'grep -q "ask the user" <<<"$1" && grep -q "install-btp.sh\" --data-dir \"$2\"" <<<"$1"' _ "$(context)" "$work/data"
check "session-start: install context gives the opt-out marker" \
  grep -q "touch \"$work/data/install-declined\"" <<<"$(context)"

FAKE_BTP_VERSION=0.0.9 run_hook session-start.sh "$start_payload" with-btp
check "session-start: too old btp is offered an update" grep -q "is too old" <<<"$(context)"

run_hook session-start.sh "${start_payload/startup/clear}"
check "session-start: no install question after /clear" test "$code:$out" = "0:"

mkdir -p "$work/data" && touch "$work/data/install-declined"
run_hook session-start.sh "$start_payload"
check "session-start: declined install stays silent" test "$code:$out" = "0:"
rm -rf "$work/data"

run_hook session-start.sh "${start_payload/startup/clear}" with-btp
check "session-start: no stale servers prints nothing" test "$code:$out" = "0:"
check "session-start: asks btp stale for the current conversation" grep -q '^stale --conversation conv-2$' "$FAKE_BTP_LOG"

stale='[{"pid":42,"start_time":1,"port":5173,"ports":[5173],"tool_label":"Vite","command":"vite","worktree":"/w","location_label":"w","idle_ms":400000,"conversation_id":"conv-1","note":"testing the form"}]'
FAKE_STALE="$stale" run_hook session-start.sh "${start_payload/startup/clear}" with-btp
check "session-start: stale servers emit valid hook JSON" test "$(printf '%s' "$out" | jq -r .hookSpecificOutput.hookEventName)" = SessionStart
check "session-start: stale context carries the list and one question" \
  bash -c 'grep -q "\"port\": *5173" <<<"$1" && grep -q "ONE question" <<<"$1" && grep -q "kill <port>" <<<"$1"' _ "$(context)"

FAKE_STALE_EXIT=1 FAKE_STALE="btp: not run from a Claude Code session" run_hook session-start.sh "$start_payload" with-btp
check "session-start: btp stale failing stays silent" test "$code:$out$err" = "0:"

# --- install-btp.sh ---------------------------------------------------------

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
# Fake codesign: --verify passes unless FAKE_CS_BAD; -d reports FAKE_TEAM.
case "$1" in
  --verify) [ -z "${FAKE_CS_BAD:-}" ] ;;
  -d) echo "Identifier=btp" >&2; echo "TeamIdentifier=${FAKE_TEAM:-47P2KJ2HF3}" >&2 ;;
  *) exit 1 ;;
esac
EOF
chmod +x "$fix/bin/curl" "$fix/bin/codesign"

# make_release VERSION: a fixture release in a fresh dir; sets rel.
make_release() {
  local v="$1" name="btp-$1-macos-universal.tar.gz"
  rel="$work/release-$v-$RANDOM"
  mkdir -p "$rel/stage"
  printf '#!/bin/bash\necho "btp %s"\n' "$v" >"$rel/stage/btp"
  chmod +x "$rel/stage/btp"
  echo MIT >"$rel/stage/LICENSE"
  tar -czf "$rel/$name" -C "$rel/stage" btp LICENSE
  (cd "$rel" && shasum -a 256 "$name" >"$name.sha256")
  printf '{"tag_name": "v%s", "name": "v%s"}\n' "$v" "$v" >"$rel/latest.json"
}

# run_install: the install script in a fresh temp HOME (unless KEEP_HOME).
# Sets out, code, ihome.
run_install() {
  if [ -z "${KEEP_HOME:-}" ]; then
    ihome="$work/ihome-$RANDOM"
    mkdir -p "$ihome"
  fi
  out="$(env -u CLAUDE_CONFIG_DIR HOME="$ihome" PATH="$fix/bin:$BASE_PATH" FAKE_RELEASE_DIR="$rel" \
    "$plugin/scripts/install-btp.sh" --data-dir "$ihome/data" 2>&1)"
  code=$?
}
installed() { test -x "$ihome/.local/bin/btp"; }
not_installed() { ! test -e "$ihome/.local/bin/btp"; }

v="$BTP_MIN_VERSION"

make_release "$v"
mkdir -p "$work/ihome-pre/data" && touch "$work/ihome-pre/data/install-declined"
ihome="$work/ihome-pre"
KEEP_HOME=1 run_install
check "install: success exits 0" test "$code" = 0
check "install: btp is in ~/.local/bin and runs" test "$("$ihome/.local/bin/btp" --version)" = "btp $v"
check "install: marker holds the installed sha256" test \
  "$(cat "$ihome/.local/bin/.btp-installed-by-balance-ton-port")" = "$(shasum -a 256 "$ihome/.local/bin/btp" | cut -d' ' -f1)"
check "install: clears the earlier no" test ! -e "$ihome/data/install-declined"
check "install: PATH hint when ~/.local/bin is not on PATH" grep -q 'export PATH="$HOME/.local/bin:$PATH"' <<<"$out"
check "install: only GitHub URLs fetched" bash -c '! grep -qv "^https://\(api\.\)\?github.com/repos/matthieuh/\|^https://github.com/matthieuh/" "$1"' _ "$rel/curl.log"

make_release "$v"
KEEP_HOME=1 run_install
check "install: replaces a btp it installed (update)" test "$code" = 0

make_release "$v"
echo "0000000000000000000000000000000000000000000000000000000000000000  btp-$v-macos-universal.tar.gz" \
  >"$rel/btp-$v-macos-universal.tar.gz.sha256"
run_install
check "install: bad sha256 is refused" test "$code" != 0
check "install: bad sha256 says why" grep -q "sha256 mismatch" <<<"$out"
check "install: bad sha256 installs nothing" not_installed

make_release "$v"
(cd "$rel" && shasum -a 256 "btp-$v-macos-universal.tar.gz" | sed 's/  .*/  other.tar.gz/' \
  >"btp-$v-macos-universal.tar.gz.sha256")
run_install
check "install: sha256 file for another file is refused" test "$code" != 0 -a ! -e "$ihome/.local/bin/btp"

make_release "$v"
FAKE_TEAM=AAAAAAAAAA run_install
check "install: wrong team is refused" test "$code" != 0
check "install: wrong team says why" grep -q "signed by team AAAAAAAAAA, expected 47P2KJ2HF3" <<<"$out"
check "install: wrong team installs nothing" not_installed

make_release "$v"
FAKE_CS_BAD=1 run_install
check "install: broken signature is refused" test "$code" != 0 -a ! -e "$ihome/.local/bin/btp"

make_release "$v"
ihome="$work/ihome-foreign"
mkdir -p "$ihome/.local/bin"
printf '#!/bin/bash\necho mine\n' >"$ihome/.local/bin/btp"
chmod +x "$ihome/.local/bin/btp"
KEEP_HOME=1 run_install
check "install: refuses to overwrite a btp it did not install" test "$code" != 0
check "install: the foreign btp is untouched" test "$("$ihome/.local/bin/btp")" = mine

make_release "$v"
KEEP_HOME=1 run_install # same HOME: still foreign
echo "changed" >>"$work/ihome-pre/.local/bin/btp"
ihome="$work/ihome-pre"
KEEP_HOME=1 run_install
check "install: refuses a btp it installed that changed since" test "$code" != 0

make_release 0.0.1
run_install
check "install: a release older than the plugin needs is refused" test "$code" != 0 -a ! -e "$ihome/.local/bin/btp"

make_release "$v"
printf '#!/bin/bash\necho "btp 9.9.9"\n' >"$rel/stage/btp"
tar -czf "$rel/btp-$v-macos-universal.tar.gz" -C "$rel/stage" btp LICENSE
(cd "$rel" && shasum -a 256 "btp-$v-macos-universal.tar.gz" >"btp-$v-macos-universal.tar.gz.sha256")
run_install
check "install: a binary reporting another version is refused" test "$code" != 0 -a ! -e "$ihome/.local/bin/btp"

make_release "$v"
printf '#!/bin/bash\n[ "$1" = stale ] && exit 2\necho "btp %s"\n' "$v" >"$rel/stage/btp"
tar -czf "$rel/btp-$v-macos-universal.tar.gz" -C "$rel/stage" btp LICENSE
(cd "$rel" && shasum -a 256 "btp-$v-macos-universal.tar.gz" >"btp-$v-macos-universal.tar.gz.sha256")
run_install
check "install: a btp without the session commands is refused" test "$code" != 0 -a ! -e "$ihome/.local/bin/btp"
check "install: ... and says so" grep -q "does not have the session commands" <<<"$out"

# --- no em dash -------------------------------------------------------------

emdash="$(printf '\xe2\x80\x94')"
if grep -rl "$emdash" "$repo/.claude-plugin" "$plugin" "$repo/skills" "$repo/README.md" >/dev/null; then
  ko "no em dash in the plugin files"
else
  ok "no em dash in the plugin files"
fi

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
