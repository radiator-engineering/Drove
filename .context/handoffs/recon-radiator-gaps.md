# Brief: recon-radiator-gaps (spawned worker, composer-2.5-fast, read-only)

You were spawned by the Drove controller. Do not edit any file except your report. Do not run `append-event.sh` or `git commit`. Do not start agents.

## Question
Drove v2 (see `~/Development/Drove/docs/superpowers/specs/2026-09-06-drove-v2-design.md`, sections 3 and 9) needs these from the Radiator hub in `~/Development/radiator-cli`. For each, answer **exists / partial / missing**, cite the file and line, and if missing propose the smallest hub-side addition (message name, fields) that would provide it:

1. Per-pane metadata tokens the hub stores and returns in a snapshot (Drove wants `drove_name`, `drove_profile`, `drove_digest`).
2. Reading recent pane output (for `ready=output("...")`).
3. Process info for a pane: foreground pid, argv, exited-or-running.
4. Closing a pane, renaming a pane, renaming a workspace.
5. Starting a coding agent in a chat pane with an initial prompt, and knowing when it is idle.
6. A stable pane identity across hub restarts (does `w1:p2` survive a reconnect?).
7. What `events.subscribe` emits on pane open/close/exit, with an example payload.

## Deliverable
Write `~/Development/Drove/.context/handoffs/recon-radiator-gaps-report.md`, under 150 lines, with a table for the seven items then the proposed additions as a short protocol sketch. Reply to the controller with exactly `REPORT READY` when done.
