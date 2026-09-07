# Brief: audit-herdr (read-only auditor, Sonnet)

You were spawned by the Drove controller as a read-only auditor of `~/Development/Drove` (branch `main`, commit ca1a3e2). Do NOT edit, create, commit, push, or run anything that writes to the repo, the log, or any Herdr session; you may read files and run `cargo test`. Do not start subagents. Do not append to the log. Recent bugs #20, #22, #24, #25, #29 all had the shape "Drove trusted a recorded id or an assumption about the live Herdr session that was false at that moment, and aborted instead of reconciling". The user is tired of patching these one at a time and wants every remaining hole of that kind found in one pass. Prefer fewer confirmed findings over many guesses; read whole code paths, not grep hits. Write your report to the file named below, then end your turn with the single line `AUDIT DONE: <path>`.

You may also run read-only `herdr` commands: `herdr --help`, `herdr <verb> --help`, `herdr session list`, `herdr --session drove api snapshot`. Herdr 0.8.2 is installed; sessions `default`, `drove`, `agentmon` are running. Never stop, delete, create, or modify anything in them.

Report file: `.context/reports/audit-herdr.md` (create the directory; this is the one write you may make).

## Dimension: the Herdr boundary

Drove drives Herdr through a JSON socket API (`src/backend/herdr.rs` `HerdrClient`; fixtures in `tests/herdr_contract.rs`) and shell-outs to the `herdr` binary via `herdr_bin_path()` / `HERDR_BIN_PATH` (`ensure_session`, `start_session_server`, D47 `stop_session`). Target resolution follows D46: flag > DROVE_* env > profile > file > ambient HERDR_SESSION/RADIATOR_HUB > built-in. For every socket call and shell-out check: which Herdr error codes can come back, which are handled, which become a generic abort; response fields assumed present (root_pane, tab ids, labels, cwd) and what happens when absent; socket path and session-directory assumptions (`~/.config/herdr/sessions/NAME/herdr.sock`); connecting to a session listed but stopped, or with a stale socket file; `ensure_session` races (socket not yet accepting; two `drove up` at once); `HERDR_BIN_PATH` pointing at an old binary, version skew; JSON parsing of CLI output (`--json` or not, stderr vs stdout, exit codes); D46 precedence in practice, including running from inside a pane of a different session and from outside any session; `default` session special-casing. Then the fake Herdr test harness: which real-Herdr behaviours does it not model, so bugs pass CI?

## Report shape

Ranked list, most severe first, at most 12 items. Each: title; file:line anchors; exact trigger; what Drove does wrong; smallest fix; confidence (confirmed / plausible). Then "Harness gaps" ranked by how likely each hides a bug, and "Already fine".
