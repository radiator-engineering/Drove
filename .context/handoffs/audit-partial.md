# Brief: audit-partial (read-only auditor, Sonnet)

You were spawned by the Drove controller as a read-only auditor of `~/Development/Drove` (branch `main`, commit ca1a3e2). Do NOT edit, create, commit, push, or run anything that writes to the repo, the log, or any Herdr session; you may read files and run `cargo test`. Do not start subagents. Do not append to the log. Recent bugs #20, #22, #24, #25, #29 all had the shape "Drove trusted a recorded id or an assumption about the live Herdr session that was false at that moment, and aborted instead of reconciling". The user is tired of patching these one at a time and wants every remaining hole of that kind found in one pass. Prefer fewer confirmed findings over many guesses; read whole code paths, not grep hits. Write your report to the file named below, then end your turn with the single line `AUDIT DONE: <path>`.

Report file: `.context/reports/audit-partial.md` (create the directory; this is the one write you may make).

## Dimension: partial failure, ordering, and recovery

For every multi-step path (`up` apply in `src/executor.rs`, `down`, `run`, `ensure_session`/`start_session_server` in `src/backend/herdr.rs`, tab/pane creation with splits and ratios, the D49 root-tab reuse, hook and task execution with approval, D47 session stop): if step k fails, what has been saved to state, what exists in the session, is a rerun idempotent, does the rerun reconcile or error again, and is the message enough for the user to know what to do? Also: state save granularity (before or after the backend call; process killed in between), `?` that should be collect-and-continue, silently swallowed errors (`let _ =`, `.ok()`, `unwrap_or_default` on things that matter), timeouts and retries on the socket and CLI shell-outs, and exit codes that lie (0 on partial success, 1 when state is actually fine).

## Report shape

Ranked list, most severe first, at most 12 items. Each: title; file:line anchors; failing step and realistic cause; what is left behind and what the next `drove up`/`down` does; smallest fix (reorder, collect-and-continue, save-after, idempotent rerun, better message); confidence (confirmed / plausible). Then "Systemic cause" and "Already fine".
