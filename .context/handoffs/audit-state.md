# Brief: audit-state (read-only auditor, Sonnet)

You were spawned by the Drove controller as a read-only auditor of `~/Development/Drove` (branch `main`, commit ca1a3e2). Do NOT edit, create, commit, push, or run anything that writes to the repo, the log, or any Herdr session; you may read files and run `cargo test`. Do not start subagents. Do not append to the log. Recent bugs #20, #22, #24, #25, #29 all had the shape "Drove trusted a recorded id or an assumption about the live Herdr session that was false at that moment, and aborted instead of reconciling". The user is tired of patching these one at a time and wants every remaining hole of that kind found in one pass. Prefer fewer confirmed findings over many guesses; read whole code paths, not grep hits. Write your report to the file named below, then end your turn with the single line `AUDIT DONE: <path>`.

Report file: `.context/reports/audit-state.md` (create the directory; this is the one write you may make).

## Dimension: recorded state trusted without checking the live session

Local state (`src/state.rs`, saved under `~/.local/state/drove/projects/<sha256>.json`) records backend ids (`w1`, `w1:t2`, `w1:p8`) per declared resource. D48 added `state::prune_missing` but only `up`/`plan`/`status` call it. Find every place recorded state, a cached id, or an assumption about the live session is trusted, and every place a mismatch is handled by aborting instead of reconciling. Cover `up`, `plan`, `status`, `down`, `run`, focus/attach (D43/D44), `caller_pane`, hooks, tasks and approval digests, content-addressed identity and adoption by label (D21-D24), the Radiator backend if present, and `prune_missing` itself (tabs whose workspace still exists, panes moved between tabs, ids reused by Herdr after close, labels renamed by the user, the D49 root tab). Also: state file for a different profile/session than the resolved target, two repos sharing one session, corrupt or older-schema state file.

## Report shape

Ranked list, most severe first, at most 12 items. Each: title; file:line anchors; exact trigger in the user's terms; what Drove does wrong and leaves behind; smallest fix that reconciles instead of aborting (can it reuse `prune_missing`?); confidence (confirmed by reading a complete path / plausible). Then "Systemic cause" (two or three sentences) and "Already fine" (paths checked that need nothing).
