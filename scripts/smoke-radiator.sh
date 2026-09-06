#!/usr/bin/env bash
# Builds the real `radiator-cli` hub, starts it on an isolated hub, and runs
# tests/radiator_smoke.rs against it. Skips (exit 0) instead of failing when
# radiator-cli isn't checked out next to this repo, or its build fails — a
# missing sibling checkout is expected outside the author's machine, not a
# Drove regression.
#
# Isolation: `radiator hub`'s state file is keyed by `--hub-name` alone
# (`radiator_hub::paths::state_path`), independent of `--socket` — passing
# `--socket` without also pinning `--hub-name` still persists layout to the
# *default* hub's state file (`hub-main.layout.json`) and can leak a
# throwaway workspace into a live hub sharing that name. This script points
# `XDG_RUNTIME_DIR` at a scratch directory for the hub subprocess and uses a
# unique `--hub-name`, so both its socket and its state file live under the
# scratch dir and never touch a real hub, matching how
# `scripts/smoke-herdr.sh` uses a unique `--session` for the same reason.
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

hub_name="ci$$"
# A Unix socket path has a short OS limit (SUN_LEN, ~104 bytes on macOS).
# `$TMPDIR` on macOS is already a long per-process path, so `mktemp -d`
# there plus `radiator/hub-${hub_name}.sock` can blow the limit — root the
# scratch dir at `/tmp` directly instead.
tmp="$(mktemp -d /tmp/drove-radiator-XXXXXX)"
runtime_dir="$tmp/rt"
mkdir -p "$runtime_dir"
socket="$runtime_dir/radiator/hub-${hub_name}.sock"
server_pid=""

cleanup() {
  if [[ -n "$server_pid" ]]; then
    kill "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
  fi
  rm -rf "$tmp"
}
trap cleanup EXIT

# Unset any ambient socket override so `--hub-name`'s derived path (under our
# scratch `XDG_RUNTIME_DIR`) is what actually gets bound.
env -u RADIATOR_HUB_SOCKET XDG_RUNTIME_DIR="$runtime_dir" \
  "$radiator_cli/target/debug/radiator" --hub-name "$hub_name" hub >"$tmp/hub.log" 2>&1 &
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
