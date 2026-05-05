---
name: shipyard
description: Shipyard operations guardrails. Use when working in /Users/danielraffel/Code/shipyard, /Users/danielraffel/Code/shipyard-rust, or /Users/danielraffel/Code/shipyard-macos-gui on parity checks, drift checks, sandbox validation, live Tailscale/GitHub webhook validation, release signing, GUI validation, Pulp/consumer pin cutover, or any go/no-go migration work.
---

# Shipyard

## Core Rule

Preserve the user's active Shipyard install until they explicitly approve a
replacement or release install. Do not replace
`/Users/danielraffel/.local/bin/shipyard`, change Pulp pins, reset Tailscale
Funnel, or merge GUI cutover support without a clear go/no-go.

## First Steps

1. Confirm the active repo and dirty state with `git status --short`.
2. Use RepoPrompt for code analysis across Shipyard, historical shipyard-rust,
   and the macOS GUI before declaring parity or implementation gaps.
3. Read the current planning packet before making cutover claims:
   `planning/cutover-go-no-go.md`, `planning/parity-matrix.md`,
   `planning/feature-audit.md`, `planning/quality-gates.md`,
   `planning/mainline-migration-plan.md`, and `docs/plan/README.md`.
4. Use `--mode isolated`, temporary install directories, and sandbox HOME/PATH
   roots for rehearsals that must not touch the active production state.

## Drift And Parity

Run drift checks whenever Python Shipyard may have changed:

```sh
python3 scripts/update_drift_tracker.py
```

Only advance the baseline with `--mark-reviewed` after the new upstream changes
have been audited and reflected in Rust or explicitly risk-accepted.

Compare command surfaces safely:

```sh
python3 scripts/compare_cli_surface.py \
  --python-bin /Users/danielraffel/Code/shipyard/.venv/bin/shipyard \
  --rust-bin target/release/shipyard \
  --allow-rust-only paths
```

Run the finish-line credential gate before signing or release claims:

```sh
python3 scripts/finish_line_status.py \
  --env-file /Users/danielraffel/Code/PlunderTube/.env \
  --json
```

## Validation Gates

Prefer non-mutating checks first:

```sh
cargo test --all-targets --locked
python3 -m unittest discover -s scripts -p 'test_*.py'
python3 scripts/update_drift_tracker.py
python3 scripts/compare_cli_surface.py --allow-rust-only paths
scripts/validate_webhook_tunnel_live.py --json
```

The live webhook gate is intentionally dangerous because it resets the local
Funnel config:

```sh
scripts/validate_webhook_tunnel_live.py --apply --allow-funnel-reset --json
```

Run that only in an approved window where interrupting the current Python
Shipyard live mode is acceptable.

## macOS GUI

The GUI lives at `/Users/danielraffel/Code/shipyard-macos-gui`. Validate it
against a sandboxed or signed rehearsal artifact before replacing the active
production `shipyard`. Update GUI docs during migration/release work, not
after the fact.

## Platform Notes

Read `references/platforms.md` when work touches Tailscale, live mode,
signing, packaging, Namespace/GitHub Actions runners, Windows SSH/PowerShell,
or cross-platform sandbox E2E behavior.

## Cutover Discipline

Cutover is a human decision, not an implementation side effect. Before asking
for go/no-go, ensure:

- Drift tracker has no untriaged upstream changes.
- CLI surface comparison is clean.
- CI, coverage, sandbox E2E, and GUI validation are green on the current Rust
  commit.
- Tailscale/GitHub live delivery is either passed in an approved reset window
  or explicitly risk-accepted.
- Signing/notarization and rollback paths are validated.
- Documentation changes for Shipyard, GUI, and Pulp/consumer pins are tracked.
