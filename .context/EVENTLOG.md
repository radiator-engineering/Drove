# Coordination event log

`events.jsonl` is the append-only, ordered truth for this task's multi-agent
work. One JSON object per line. It is **not** a transcript — it holds small
coordination facts, and points at bigger artifacts by path.

## Rules
- **Single writer.** Only the controller/driver appends, and only via
  `append-event.sh`. Workers report back; the controller records.
- **Append-only.** Lines are never edited or deleted. "The log up to event N"
  is exactly what the system knew at event N — that is what makes it replayable
  and auditable.
- **Small events.** No model output, no file contents. Reference artifacts by
  path: `ref=.context/handoffs/foo.md`.

## Event shape
Every line has `seq` (monotonic), `ts` (UTC), `type`, plus type-specific fields:

| type       | fields (besides seq/ts/type)                    | meaning                          |
|------------|--------------------------------------------------|----------------------------------|
| `spawn`    | agent, model, tab, role                          | a worker was launched            |
| `prompt`   | agent, ref                                        | a delegation packet was sent     |
| `message`  | from, to, subject, ref                            | inter-agent routing              |
| `drain`    | agent                                             | inbox/turn processed             |
| `result`   | agent, ref, verdict                               | worker reported; artifact at ref |
| `decision` | key, value, ref                                   | a choice others must follow      |
| `escalate` | subject, ref                                      | queued for human approval        |
| `approval` | subject, by, decision                             | human decided                    |
| `retire`   | agent, disposition                                | worker tab closed                |

Parallel-work events (append these when workers run side by side):

| type        | fields (besides seq/ts/type)                     | meaning                                        |
|-------------|--------------------------------------------------|------------------------------------------------|
| `claim`     | agent, paths (comma-separated globs)             | the files this worker owns; nobody else edits  |
| `progress`  | agent, msg, ref                                  | controller polled a working agent; short note  |
| `seam`      | agents, subject, ref                             | a cross-worker dependency found mid-work       |
| `violation` | agent, paths                                     | worker changed files outside its claim         |
| `ack`       | by, seq_done, outcome, ref                       | a reactor acted on event seq_done              |

Any line the controller did not write carries `by=<agent>`; only writers named
in a `decision key=log-writers` may set it. A reactor (a process that acts on
events, e.g. a committer) resumes from its own `ack` lines, never a side file.

Extend with new `type`s freely; keep fields flat and small.

## Read it
    eventlog-view.sh -f                           # live, colored, aligned (for a pane)
    tail -f .context/events.jsonl                 # raw
    jq -c 'select(.type=="decision")' .context/events.jsonl
    jq -c 'select(.agent=="reviewer")' .context/events.jsonl
    jq -r 'select(.type=="claim") | "\(.agent)\t\(.paths)"' .context/events.jsonl   # who owns what
    check-claims.sh reviewer feature/base            # did the worker stay inside its claim?
