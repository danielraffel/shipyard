---
name: shipyard
description: Shipyard operations guardrails. Use when working in /Users/danielraffel/Code/shipyard, /Users/danielraffel/Code/shipyard-rust, or /Users/danielraffel/Code/shipyard-macos-gui on parity checks, drift checks, sandbox validation, live Tailscale/GitHub webhook validation, release signing, GUI validation, Pulp/consumer pin cutover, or any go/no-go migration work.
---

# Shipyard

## Core Rule

Preserve the user's active Shipyard install and rollback path. Rust Shipyard is
the daily implementation as of `v0.51.0` / `v0.51.1`, but do not replace
`/Users/danielraffel/.local/bin/shipyard`, remove preserved backups, change
Pulp pins, reset Tailscale Funnel, or merge GUI cutover support without a clear
go/no-go for that operation.

## First Steps

1. Confirm the active repo and dirty state with `git status --short`.
2. Use RepoPrompt for code analysis across Shipyard, historical shipyard-rust,
   and the macOS GUI before declaring parity or implementation gaps.
3. Read the current planning packet before making release/cutover claims:
   `planning/post-cutover-status.md`, `planning/go-no-go-completion-audit.md`,
   `planning/upstream-drift.md`, `planning/documentation-backlog.md`, and
   `docs/plan/README.md`.
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
scripts/validate_webhook_tunnel_live.py \
  --repo danielraffel/Shipyard \
  --binary "$(command -v shipyard)" \
  --apply \
  --allow-funnel-reset \
  --json
```

Run that only in an approved window where briefly taking over the
machine-global Tailscale Serve/Funnel route is acceptable. The validator knows
about the App Store Tailscale binary at
`/Applications/Tailscale.app/Contents/MacOS/Tailscale`; do not assume a
`tailscale` PATH shim exists.

## macOS GUI

The GUI lives at `/Users/danielraffel/Code/shipyard-macos-gui`. Validate it
against a sandboxed or signed rehearsal artifact before replacing the active
production `shipyard`. Update GUI docs during migration/release work, not
after the fact.

## Platform Notes

Read `references/platforms.md` when work touches Tailscale, live mode,
signing, packaging, Namespace/GitHub Actions runners, Windows SSH/PowerShell,
or cross-platform sandbox E2E behavior.

Namespace is optional and account-dependent. When Namespace is unavailable,
Shipyard should default to GitHub-hosted Linux/macOS/Windows runners or explicit
self-hosted GitHub Actions labels. Do not assume `nsc` access, and do not route
new Shipyard CI to Namespace unless the user explicitly confirms active access.

For local capacity, keep GitHub Actions as the dispatch layer and use SSH only
to manage the runner hosts. Stable labels such as `shipyard-macos-arm64`,
`shipyard-linux-arm64`, and `shipyard-windows-x64` are preferable to raw host
names in workflow `runs-on` selectors.

## Cloud Retargeting

`shipyard cloud retarget --apply` is intentionally fail-closed. It cancels
matching GitHub Actions jobs first, uses whole-run cancellation only when every
active job in the run matches the target, and does not dispatch a replacement
if cancellation cannot be proven complete. When handling `event=cancel_failed`,
preserve the classification (`auth`, `scope`, `not_found`, `unsupported`,
`transient`, `unknown`), run/job URLs, manual recovery steps, and
branch-protection warning; do not collapse HTTP 404/not-found into an
`actions:write` scope hint unless the raw error also indicates auth or
permission trouble.

## Cutover Discipline

Release/cutover is a human decision, not an implementation side effect. Before
asking for go/no-go, ensure:

- Drift tracker has no untriaged upstream changes.
- CLI surface comparison is clean.
- CI, coverage, sandbox E2E, and GUI validation are green on the current Rust
  commit.
- Tailscale/GitHub live delivery is either passed in an approved reset window
  or explicitly risk-accepted.
- Signing/notarization and rollback paths are validated.
- Documentation changes for Shipyard, GUI, and Pulp/consumer pins are tracked.
