# Runner watchdog

`shipyard runner` detects and (optionally) auto-recovers a self-hosted
GitHub Actions runner that has gotten itself stuck.

## Why

On 2026-05-12 a Pulp self-hosted runner sat busy on a UBSan job from a
closed branch for >75 minutes while 17 stale queued runs piled up behind
it. A critical-path PR was blocked for hours before a human noticed. The
watchdog is the structural fix: detect the symptoms automatically, report
them, and offer a guarded recovery path.

## Symptoms it catches

| Symptom | Detection | Default action |
|---|---|---|
| `orphaned_busy` | API reports `busy=true` but no `Runner.Worker` process is visible locally | Report only (clears in 1-5 min) |
| `hung_worker` | A `Runner.Worker` has been running longer than `max_job_min` | Report only (auto-kill is intentionally opt-in twice) |
| `stale_queued_runs` | Queued runs older than `max_queue_age_hours` | Report on `status`; cancel on `cleanup --fix` |

## Subcommands

### `shipyard runner status`

One-shot health check. Exit codes:

- `0` runner healthy, no symptoms
- `1` runner online but at least one symptom detected
- `2` runner offline / API unreachable

```bash
shipyard runner status                              # uses config defaults
shipyard runner status --runner-id 1763             # explicit override
shipyard runner status --max-queue-age-hours 4      # widen the queue cutoff
shipyard runner status --json                       # structured output
```

### `shipyard runner cleanup`

Lists stale queued runs. Default is `--dry-run`; pass `--fix` to actually
cancel them via `POST /actions/runs/<id>/cancel`. Exits non-zero when
stale runs are found in dry-run mode (matches the prototype script's
contract, so cron consumers see drift).

```bash
shipyard runner cleanup                             # dry-run, prints stale ids
shipyard runner cleanup --fix                       # cancel them
shipyard runner cleanup --stale-hours 4 --fix
shipyard runner cleanup --json --fix                # structured cancel report
```

`--force-kill` is reserved for terminating a hung `Runner.Worker`
process. It requires `--fix` and two confirmation prompts (`y` then the
literal word `KILL`). On non-TTY stdin it is ignored unless `--yes` is
also passed. The current implementation **does not** actually terminate
the process — it prints diagnostic guidance so the operator can decide
how to kill it. This is deliberate: silent worker-kill can corrupt
in-flight build artifacts.

### `shipyard runner watch`

Polling daemon. Defaults to the `runner.watchdog.watch_interval_seconds`
config value (300 s). Logs one line per tick. With `--fix`, cancels stale
queued runs every tick.

```bash
shipyard runner watch
shipyard runner watch --interval 60 --fix
shipyard runner watch --json   # NDJSON-style structured ticks
```

The loop never exits on its own; press Ctrl-C or run it under
`launchd` / `systemd` for unattended operation. A hidden
`--max-iterations N` flag exists for tests.

## Configuration

Defaults live in `.shipyard/config.toml`:

```toml
[runner.watchdog]
runner_id = 1763
runner_dir = "/Users/runner/actions-runner"
max_job_min = 90
max_queue_age_hours = 2
watch_interval_seconds = 300
auto_fix = false
```

Per-machine overrides go in `.shipyard.local/config.toml` and follow the
standard Shipyard layered-config rules.

Every command-line flag wins over config; config wins over the built-in
defaults (`max_job_min=90`, `max_queue_age_hours=2`,
`watch_interval_seconds=300`).

## Lessons learned (from the prototype)

- Stale queued runs from 5+ hours ago can sit forever and monopolize the
  runner when they eventually get FIFO'd in.
- A worker PID staying alive 1-5 min after `gh run cancel` is normal —
  the runner takes time to honour graceful shutdown. Don't treat that as
  a symptom on its own.
- `concurrency: cancel-in-progress: true` on a workflow *should*
  auto-cancel duplicate runs on force-push, but doesn't always (see Pulp
  issue #1884).
- Auto-killing the Worker process is too risky to wire silently;
  `--force-kill` deliberately stops short of `kill -9` and prints
  guidance instead.

## Implementation notes

- Pure detection logic lives in `src/runner_watchdog.rs` and has no I/O.
- The CLI shell-out is contained in `src/app/runner_cmd.rs`. It uses the
  existing `gh` invocation pattern from `src/cloud.rs`; no new HTTP
  client dependency.
- `crate::cloud::QueuedRun` and `GitHubActions::{list_queued_runs,
  cancel_workflow_run}` are reused unchanged from the cloud-handoff
  subcommand.
- This subcommand intentionally does not touch any other Shipyard
  subcommand.
