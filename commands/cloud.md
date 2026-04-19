---
name: cloud
description: Dispatch and inspect GitHub Actions workflows from Shipyard
---

Use Shipyard's cloud subcommands to discover workflows, inspect dispatch defaults,
trigger workflow_dispatch runs, and inspect tracked cloud runs.

```bash
shipyard cloud workflows --json
shipyard cloud defaults --json
shipyard cloud run build --json
shipyard cloud status --json
```

When dispatching a workflow:
- Prefer the configured default workflow when the user does not specify one.
- Pass `--provider <name>` when the user wants to override the default runner provider.
- Pass `--wait` when the user wants the command to block until the GitHub run completes.

When summarizing JSON output, report:
- Workflow key/file/name
- Resolved provider and dispatch fields
- Tracked run ID, status, conclusion, and URL when available

## Mid-flight lane edits

```bash
# Swap one lane on an in-flight PR to a new provider:
shipyard cloud retarget --pr <n> --target macos --provider namespace [--apply]

# Add a brand-new lane to an in-flight PR without re-dispatching the matrix:
shipyard cloud add-lane --pr <n> --target windows [--provider namespace] [--apply]
```

Both are dry-run by default. `add-lane` is idempotent — if the target is
already tracked in ShipState.dispatched_runs, the command reports a no-op
and makes no changes. Use when the user says "I realized I should also
run windows" after a ship is already in flight.
