# Installation

## Canonical install location

Every supported install path writes the `shipyard` binary to the same
place by default:

```
~/.local/bin/shipyard
```

| Path | Lands at |
|---|---|
| `curl … install.sh` | `~/.local/bin/shipyard` |
| Claude Code plugin (auto-installs on first session if missing) | `~/.local/bin/shipyard` |
| Codex one-liner (same `install.sh`) | `~/.local/bin/shipyard` |
| Project pinners (see "Pin a specific version" below) | `~/.local/bin/shipyard` (recommended) |

Make sure `~/.local/bin` is on your `PATH` and every install method
reaches the same binary. No PATH juggling, no "which shipyard am I
running" confusion.

## Quick install

```bash
curl -fsSL https://generouscorp.com/Shipyard/install.sh | sh
```

Downloads the right binary for your platform and installs it at
`~/.local/bin/shipyard`.

## First-run auth

A few Shipyard commands (`cloud retarget`, `cloud handoff`, anything
that cancels + re-dispatches a GitHub Actions workflow run) need a
`gh` token with the **`workflow` scope** — GitHub's short name for
`actions:write` on a classic PAT, or **Actions: Read and write** on a
fine-grained token. Without it you'll hit:

```
error: Couldn't cancel the matching job(s). Your gh token may lack
`actions:write` scope.
```

`shipyard doctor` probes for this; fix it at install time so the
first retarget attempt doesn't surprise you.

### Interactive gh login (most common)

```bash
gh auth refresh -h github.com -s workflow
```

Follow the browser prompt. You don't have to log out first —
`refresh` adds the scope to your existing session.

### Fine-grained personal access token

github.com → Settings → Developer settings → Personal access tokens →
**Fine-grained tokens** → edit the token that's stored in `gh auth` →
**Actions: Read and write**. Save. `gh auth status` should now show
the scope in its `Token scopes:` line.

### GitHub App / bot identity

If Shipyard is running under an App install (CI, `RELEASE_BOT_TOKEN`,
a bot like `pulp-release-bot`), the scope lives on the **App's
permissions**, not the invoking user's token. github.com →
organizations/<org> → Settings → GitHub Apps → your app →
**Permissions & events** → **Actions: Read and write**. Accept the
install prompt on each repo after saving.

## Pin a specific version

Pass `SHIPYARD_VERSION` to install an exact release instead of the
latest. Useful for project-pinning so every teammate + agent runs
the same shipyard build.

```bash
SHIPYARD_VERSION="v0.22.1" curl -fsSL https://generouscorp.com/Shipyard/install.sh | bash
# or if you've already fetched the script:
SHIPYARD_VERSION="v0.22.1" bash install.sh
```

Accepts `"v0.22.1"`, `"0.22.1"`, or `"latest"` (default).

Project-level pinning pattern: keep the desired version in a small
pin file (e.g. `tools/shipyard.toml` with `version = "0.22.1"`), read
it in a wrapper script, and call `install.sh` with
`SHIPYARD_VERSION="$(read-version)"`. Nothing more complicated is
needed — every teammate ends up with the same binary at
`~/.local/bin/shipyard`.

## Install to a different directory

Pass `SHIPYARD_INSTALL_DIR`. Only override when you have a specific
reason; the default keeps every install path aligned.

```bash
SHIPYARD_INSTALL_DIR="${HOME}/mytools/bin" bash install.sh
```

## Platform binaries

| OS | Architecture | Binary |
|----|-------------|--------|
| macOS | Apple Silicon (ARM64) | `shipyard-macos-arm64.dmg` |
| Windows | x64 | `shipyard-windows-x64.exe` |
| Linux | x64 | `shipyard-linux-x64` |
| Linux | ARM64 | `shipyard-linux-arm64` |

Intel Macs (x86_64) are not supported from v0.50.0 onward. Apple Silicon only. Older releases (v0.44.0–v0.49.0) that shipped Intel dmgs remain installable by pinning `SHIPYARD_VERSION`; `install.sh` on an Intel Mac surfaces a clear "unsupported" message instead of a 404 on v0.50.0+.

## Build from source

### Isolated dev build

Your dev build lives in the checkout under `target/`; the system
`shipyard` at `~/.local/bin/shipyard` is unaffected unless you copy or
install it there.

```bash
git clone https://github.com/danielraffel/Shipyard.git
cd Shipyard
cargo build --release --locked
target/release/shipyard --version
```

Run the main local gates before relying on a source build:

```bash
cargo test --all-targets --locked
cargo clippy --all-targets --locked -- -D warnings
python3 -m unittest discover -s scripts -p 'test_*.py'
```

### Make a source build your system `shipyard`

If you want your local checkout to take over at
`~/.local/bin/shipyard` (same location `install.sh` uses), copy the
release binary and refresh the `sy` symlink:

```bash
mkdir -p ~/.local/bin
cp target/release/shipyard ~/.local/bin/shipyard
ln -sf ~/.local/bin/shipyard ~/.local/bin/sy
```

Only do this intentionally: Claude Code, Codex, the macOS GUI, and
project pinners all treat `~/.local/bin/shipyard` as canonical.

## Optional dependencies

You don't need everything — just what matches your setup. See the
[main README requirements table](../README.md#requirements) for
details.
