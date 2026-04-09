---
name: ci
description: Cross-platform CI coordination with Shipyard — validates, ships, manages queue, switches profiles
---

# CI Operations with Shipyard

Shipyard coordinates validation across local, SSH, and cloud targets.

## Quick reference

| Task | Command |
|------|---------|
| Validate current branch | `shipyard run --json` |
| Validate specific targets | `shipyard run --targets mac,ubuntu --json` |
| Fast smoke check | `shipyard run --smoke --json` |
| Full ship (PR + validate + merge) | `shipyard ship --json` |
| Ship to develop instead of main | `shipyard ship --base develop --json` |
| Show queue and status | `shipyard status --json` |
| Show all queued jobs | `shipyard queue --json` |
| Show targets and reachability | `shipyard targets --json` |
| Show run logs | `shipyard logs <job_id> --json` |
| Show logs for one target | `shipyard logs <job_id> --target windows` |
| Check merge readiness | `shipyard evidence --json` |
| Bump job priority | `shipyard bump <job_id> high` |
| Cancel a job | `shipyard cancel <job_id>` |
| Switch profile | `shipyard config use <profile>` |
| List profiles | `shipyard config profiles` |
| Show config | `shipyard config show` |
| Environment check | `shipyard doctor --json` |
| Clean up artifacts | `shipyard cleanup --apply` |

## Ship workflow (the main flow)

1. Work on a feature branch. Commit your changes.
2. Run `shipyard ship --json` — this pushes, creates a PR, validates on all
   platforms, and merges when green.
3. If a target fails, read the logs with `shipyard logs <id> --target <name>`,
   fix the issue, and run `shipyard ship --json` again.

Shipyard refuses to merge unless every required platform has passing evidence
for the exact HEAD SHA.

## Profiles

Profiles control which targets are active. Switch without editing config:

- `shipyard config use local` — Mac only, no VMs or cloud
- `shipyard config use normal` — Mac + cloud (Namespace)
- `shipyard config use full` — Mac + VMs + cloud fallback

Check which profile is active: `shipyard config profiles`
See where things will run: `shipyard targets --json`

## Queue management

When multiple jobs are queued (common with parallel worktrees):

- `shipyard queue --json` — see what's running and pending
- `shipyard bump <id> high` — make a job run next
- `shipyard bump <id> low` — deprioritize a job
- `shipyard cancel <id>` — cancel a pending or running job

## Target configuration

Targets are defined in `.shipyard/config.toml`:

```toml
[targets.mac]
backend = "local"
platform = "macos-arm64"

[targets.ubuntu]
backend = "ssh"
host = "ubuntu"
platform = "linux-x64"

# Optional fallback chain
fallback = [
    { type = "vm", vm_name = "Ubuntu 24.04" },
    { type = "cloud", provider = "namespace" },
]
```

## Troubleshooting

- `shipyard doctor --json` — checks git, ssh, gh, nsc are installed
- `shipyard targets --json` — shows which targets are reachable
- `shipyard logs <id> --target <name>` — full log for a failed target
- If a target is unreachable with no fallback, it reports unreachable
