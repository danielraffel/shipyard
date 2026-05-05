# Shipyard sandbox E2E harness

Isolated end-to-end tests for the `shipyard` binary. The harness is
based on the upstream Shipyard `#248` / PR `#260` pattern and the Pulp
sibling sandbox gate: every test runs with a fresh tempdir, a shadowed
`PATH`, and an isolated `HOME`.

## What It Proves

- The staged binary launches and reports help/version text.
- Unknown top-level and nested commands fail loudly instead of silently
  exiting zero.
- Every command path advertised by `shipyard --help` and nested
  subcommand help responds non-silently, including Clap-generated
  `help` pseudo-commands.
- Runtime paths from `--json paths` resolve under the sandbox `HOME`.
- Read-only state surfaces such as `ship-state list`, `daemon status`,
  and `pin show` do not silently succeed or touch the user's real state.
- `daemon refresh` can spawn a real detached child, advertise explicit
  repos, refresh again with prior-repo reuse, and stop cleanly from an
  isolated short state root.
- `install.sh` resolves production names (`shipyard` / `sy`) and can
  validate a skip-download install inside the sandbox without touching
  the real install directory.
- `install.sh` refuses latest macOS x64 installs with exit `2`, while
  preserving the older pinned Intel-capable release escape hatch.
- `shipyard pin bump` can run a consumer repo's
  `tools/install-shipyard.sh` wrapper and verify a temp `shipyard
  --version` on an isolated `PATH`.
- `shipyard pin bump` can also exercise the PR path against an
  isolated bare `origin` and fake `gh`, proving branch push, PR URL
  rendering, and JSON stdout cleanliness without opening a real PR.
- When the previous Python Shipyard repo is available, safe JSON
  contracts can be checked against both implementations in the same
  isolated sandbox.
- The test harness refuses destructive/live commands before execution.
- The contamination audit fails if a test creates new files under real
  Shipyard or Shipyard Rust install/state paths.

## Running

```bash
cargo build --release
python3 -m pytest tools/sandbox-e2e/
```

To test a specific binary:

```bash
SHIPYARD_BINARY_FOR_TEST=/path/to/shipyard python3 -m pytest tools/sandbox-e2e/
```

To enable Python-vs-Rust parity checks explicitly:

```bash
SHIPYARD_BINARY_FOR_TEST=/path/to/shipyard \
SHIPYARD_PYTHON_REPO_FOR_TEST=/path/to/shipyard \
SHIPYARD_PYTHON_FOR_TEST=/path/to/shipyard/.venv/bin/python \
python3 -m pytest tools/sandbox-e2e/
```

If the Python repo is not present, those cross-binary checks are skipped
so CI can still validate the Shipyard binary in isolation.

## Deliberately Excluded

The sandbox refuses commands that can mutate repositories, GitHub, a
daemon, or a real install:

- `shipyard ship`
- `shipyard run`
- `shipyard auto-merge`
- `shipyard cloud add-lane`
- `shipyard cloud retarget`
- `shipyard cloud handoff run`
- `shipyard daemon start|run|refresh|stop`
- `shipyard wait ...`
- `shipyard watch --follow`

Those flows need dedicated live validation, not a generic binary smoke
test.
