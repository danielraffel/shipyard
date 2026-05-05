# Rust Cutover And Rollback

This page records the operational checklist for the first Rust-backed
Shipyard release. It is intentionally conservative: do not replace a
working Python install, publish a release, or update consumer pins until
the final go/no-go has been approved.

## Current Cutover Candidate

- Migration worktree: `/Users/danielraffel/Code/shipyard-mainline-rust-cutover`
- Branch: `rust-mainline-cutover`
- Prepared commit: `f368a22` (`Migrate Shipyard CLI to Rust`)
- Python parity baseline: `d1999c69085f5b8c8c8672cb3943be3ccc59ed66`
- Rust version: `shipyard 0.51.0`

The daily fallback remains the installed Python binary until cutover:

```sh
/Users/danielraffel/.local/bin/shipyard --version
```

Expected pre-cutover output:

```text
shipyard, version 0.46.0
```

## Required Go/No-Go Gates

Run these before opening or merging the migration PR:

```sh
python3 scripts/compare_cli_surface.py \
  --python-bin /Users/danielraffel/Code/shipyard/.venv/bin/shipyard \
  --rust-bin target/release/shipyard \
  --allow-rust-only paths

python3 -m unittest discover -s scripts -p 'test_*.py'
cargo fmt -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --all-targets --locked
cargo llvm-cov --locked --summary-only --fail-under-lines 75

SHIPYARD_BINARY_FOR_TEST="$PWD/target/release/shipyard" \
SHIPYARD_PYTHON_REPO_FOR_TEST=/Users/danielraffel/Code/shipyard \
SHIPYARD_PYTHON_FOR_TEST=/Users/danielraffel/Code/shipyard/.venv/bin/python \
  /Users/danielraffel/Code/shipyard/.venv/bin/python -m pytest tools/sandbox-e2e/ -q
```

Expected result from the latest rehearsal:

- CLI surface compare: `missing_from_rust: []`
- Rust tests: `583` passing
- Coverage: `82.44%` line coverage
- Script tests: `63` passing
- Sandbox E2E: `38` passing with Python parity enabled

## Release Rehearsal

Build and sign a no-upload macOS artifact:

```sh
cargo build --release --locked --bin shipyard

scripts/release-macos-local.sh \
  --tag v0.51.0-rust-rehearsal \
  --skip-build \
  --binary target/release/shipyard \
  --dist-dir target/release-rehearsal \
  --env-file /Users/danielraffel/Code/PlunderTube/.env
```

The latest rehearsal produced:

```text
target/release-rehearsal/v0.51.0-rust-rehearsal/shipyard-macos-arm64.dmg
```

The DMG was signed, notarized, stapled, validated, mounted, and
launch-smoked as `shipyard 0.51.0`. No upload was performed.

## GUI Validation

Validate the macOS GUI against the exact Rust artifact selected for
cutover:

```sh
SHIPYARD_GUI_TEST_RUST_BINARY=/Users/danielraffel/Code/shipyard-mainline-rust-cutover/target/release/shipyard \
  xcodebuildmcp macos test \
  --project-path ShipyardMenuBar.xcodeproj \
  --scheme ShipyardMenuBar
```

Latest result: `19` passed, `1` skipped, `0` failed on My Mac
macOS `26.4.1`.

If Xcode stalls before test execution in `SWBBuildService` or `clang`,
record that as an Xcode build-system caveat. Do not classify it as a
Rust runtime failure unless the Rust binary is actually executed and
fails.

## Webhook And Funnel Gate

Non-mutating preflight is safe anytime:

```sh
python3 scripts/validate_webhook_tunnel_live.py \
  --binary target/release/shipyard \
  --json
```

Full live delivery is a machine-level Tailscale Funnel takeover. Run it
only in an approved window:

```sh
python3 scripts/validate_webhook_tunnel_live.py \
  --binary target/release/shipyard \
  --apply \
  --allow-funnel-reset \
  --json
```

The validator resets Serve/Funnel only for an owned validation run and
cleans up transient GitHub hooks. If the daily Python daemon currently
owns Funnel, stop it deliberately, run the Rust gate, then either restore
Python or proceed with the approved cutover.

## Release And Install

Do not use `--upload` until the release tag, rollback tag, and monitoring
window are agreed:

```sh
scripts/release-macos-local.sh \
  --tag vX.Y.Z \
  --upload \
  --rollback-tag vPREVIOUS \
  --env-file /Users/danielraffel/Code/PlunderTube/.env
```

The release script keeps GitHub releases draft until expected assets are
present and install E2E succeeds. A failed install E2E returns a just-
published release to draft.

## Rollback

Rollback if Rust doctor, GUI selected-CLI resolution, daemon IPC,
webhook/Funnel, installer E2E, or a consumer gate fails.

1. Stop the Rust daemon:

   ```sh
   shipyard daemon stop
   ```

2. Reinstall the recorded previous Python Shipyard release.
3. Confirm the fallback binary:

   ```sh
   /Users/danielraffel/.local/bin/shipyard --version
   shipyard --json doctor
   ```

4. Point the GUI selected CLI path back to the Python binary if needed.
5. Revert or supersede consumer pin PRs targeting the Rust release.
6. If release upload already happened and install E2E failed, keep the
   release in draft until the failure is understood.

## Consumer Pin Updates

Do not update Pulp or other consumers until the Rust release is published
and stable.

When approved, update consumers through the CLI rather than hand-editing:

```sh
shipyard pin show
shipyard pin bump --to vX.Y.Z
```

Keep those PRs isolated and validate each consumer with its normal
Shipyard gate.

## Deferred Features

Issue `#265` and issue `#266` are intentionally post-cutover unless they
land in Python before merge. The cutover goal is parity and stability,
not new feature scope.
