#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tmp="$(mktemp -d)"
session="drove-ci-$$"
server_pid=""

cleanup() {
  if [[ -n "$server_pid" ]]; then
    kill "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
  fi
  rm -rf "$tmp"
}
trap cleanup EXIT

herdr --session "$session" server >"$tmp/herdr.log" 2>&1 &
server_pid="$!"

socket="${HOME}/.config/herdr/sessions/${session}/herdr.sock"
for _ in {1..100}; do
  [[ -S "$socket" ]] && break
  sleep 0.05
done
[[ -S "$socket" ]] || {
  cat "$tmp/herdr.log"
  echo "Herdr smoke server did not start" >&2
  exit 1
}

export DROVE_STATE_HOME="$tmp/state"
"$root/target/debug/drove" --file "$root/examples/basic/Drovefile" --session "$session" up
"$root/target/debug/drove" --file "$root/examples/basic/Drovefile" --session "$session" up
"$root/target/debug/drove" --file "$root/examples/basic/Drovefile" --session "$session" status
