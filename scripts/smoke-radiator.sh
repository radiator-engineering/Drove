#!/usr/bin/env bash
# Builds the real `radiator-cli` hub, starts it on a temp socket, and runs
# tests/radiator_smoke.rs against it. Skips (exit 0) instead of failing when
# radiator-cli isn't checked out next to this repo, or its build fails — a
# missing sibling checkout is expected outside the author's machine, not a
# Drove regression.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
radiator_cli="${RADIATOR_CLI_ROOT:-$root/../radiator-cli}"

if [[ ! -d "$radiator_cli" ]]; then
  echo "skip: no radiator-cli checkout at $radiator_cli (set RADIATOR_CLI_ROOT to override)"
  exit 0
fi

if ! (cd "$radiator_cli" && cargo build --quiet --bin radiator) 2>"$root/target/smoke-radiator-build.log"; then
  echo "skip: radiator-cli did not build; see target/smoke-radiator-build.log" >&2
  tail -n 40 "$root/target/smoke-radiator-build.log" >&2 || true
  exit 0
fi

tmp="$(mktemp -d)"
socket="$tmp/hub.sock"
server_pid=""

cleanup() {
  if [[ -n "$server_pid" ]]; then
    kill "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
  fi
  rm -rf "$tmp"
}
trap cleanup EXIT

"$radiator_cli/target/debug/radiator" --socket "$socket" hub >"$tmp/hub.log" 2>&1 &
server_pid="$!"

for _ in {1..100}; do
  [[ -S "$socket" ]] && break
  sleep 0.05
done
[[ -S "$socket" ]] || {
  cat "$tmp/hub.log"
  echo "Radiator hub smoke daemon did not start" >&2
  exit 1
}

RADIATOR_SMOKE_SOCKET="$socket" cargo test --manifest-path "$root/Cargo.toml" --test radiator_smoke -- --nocapture
