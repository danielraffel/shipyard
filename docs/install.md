# Installation

## Quick install

```bash
curl -fsSL https://generouscorp.com/Shipyard/install.sh | sh
```

Downloads the right binary for your platform and installs it on `$PATH`.

## Platform binaries

| OS | Architecture | Binary |
|----|-------------|--------|
| macOS | Apple Silicon (ARM64) | `shipyard-macos-arm64` |
| macOS | Intel (x64) | `shipyard-macos-x64` |
| Windows | x64 | `shipyard-windows-x64.exe` |
| Linux | x64 | `shipyard-linux-x64` |
| Linux | ARM64 | `shipyard-linux-arm64` |

## Build from source (contributors)

```bash
git clone https://github.com/danielraffel/Shipyard.git
cd Shipyard
python3 -m venv .venv && source .venv/bin/activate
pip install -e ".[dev]"
pytest                         # verify everything works
```

Or with `uv`:

```bash
uv sync --extra dev
uv run pytest
```

## Optional dependencies

You don't need everything — just what matches your setup. See the
[main README requirements table](../README.md#requirements) for details.
