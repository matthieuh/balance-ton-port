# shellcheck shell=bash
# Shared by the plugin's hooks and install script. Sourced, never run.

# The oldest btp that has the commands the hooks call (`session-end`,
# `stale`, `note`, `recent`). Kept equal to the btp-cli crate version
# (plugin/tests/run.sh checks it), so a release bumps both together.
BTP_MIN_VERSION="0.1.0"

# The marker install-btp.sh leaves next to the btp it installed. It holds
# that file's sha256: a btp is replaced only while it still matches.
# shellcheck disable=SC2034 # used by install-btp.sh
BTP_MARKER_NAME=".btp-installed-by-balance-ton-port"

# Where the plugin keeps its own state (the "no" to the install). Claude
# Code sets CLAUDE_PLUGIN_DATA for hooks; the install command passes it.
btp_data_dir() {
  printf '%s\n' "${CLAUDE_PLUGIN_DATA:-${XDG_DATA_HOME:-$HOME/.local/share}/balance-ton-port-plugin}"
}

# btp_version_ge A B: succeeds when version A >= version B (X.Y.Z, a
# pre-release suffix is ignored).
btp_version_ge() {
  local a="${1%%-*}" b="${2%%-*}" i x y
  local -a av bv
  IFS=. read -r -a av <<<"$a"
  IFS=. read -r -a bv <<<"$b"
  for i in 0 1 2; do
    x="${av[$i]:-0}"
    y="${bv[$i]:-0}"
    [[ "$x" =~ ^[0-9]+$ && "$y" =~ ^[0-9]+$ ]] || return 1
    if ((10#$x > 10#$y)); then return 0; fi
    if ((10#$x < 10#$y)); then return 1; fi
  done
  return 0
}

# btp_version PATH: prints the version that binary reports, or fails.
btp_version() {
  local out
  out="$("$1" --version 2>/dev/null)" || return 1
  out="${out#btp }"
  [[ "$out" =~ ^[0-9]+\.[0-9]+\.[0-9]+ ]] || return 1
  printf '%s\n' "$out"
}

# btp_usable PATH: the binary is recent enough for the hooks. The version
# alone is not enough while the minimum equals the last release's number,
# so the `stale` command must exist too.
btp_usable() {
  local v
  [ -x "$1" ] || return 1
  v="$(btp_version "$1")" || return 1
  btp_version_ge "$v" "$BTP_MIN_VERSION" || return 1
  "$1" stale --help >/dev/null 2>&1
}

# The btp binaries worth looking at: the one on PATH, then ~/.local/bin
# (where install-btp.sh puts it, often not on PATH).
btp_candidates() {
  local p
  p="$(command -v btp 2>/dev/null)" && [ -n "$p" ] && printf '%s\n' "$p"
  [ -x "$HOME/.local/bin/btp" ] && printf '%s\n' "$HOME/.local/bin/btp"
  return 0
}

# Prints the first usable btp, or fails.
btp_find() {
  local p
  while IFS= read -r p; do
    if btp_usable "$p"; then
      printf '%s\n' "$p"
      return 0
    fi
  done < <(btp_candidates)
  return 1
}

# Prints the first btp found at all (usable or not), or fails.
btp_find_any() {
  local p
  p="$(btp_candidates | head -n 1)"
  [ -n "$p" ] || return 1
  printf '%s\n' "$p"
}

# btp_json_field KEY JSON: the string value of a top-level-looking key.
# Enough for Claude Code's hook payload (ids and reason words, no escapes).
btp_json_field() {
  local re="\"$1\"[[:space:]]*:[[:space:]]*\"([^\"\\\\]*)\""
  if [[ "$2" =~ $re ]]; then
    printf '%s\n' "${BASH_REMATCH[1]}"
  fi
}

# btp_json_escape TEXT: TEXT as the inside of a JSON string.
btp_json_escape() {
  local s="$1"
  s="${s//\\/\\\\}"
  s="${s//\"/\\\"}"
  s="${s//$'\n'/\\n}"
  s="${s//$'\r'/\\r}"
  s="${s//$'\t'/\\t}"
  printf '%s' "$s"
}
