#!/bin/bash
# Check if the shipyard CLI binary is installed.
# If not, download and install it automatically.
# Runs as a SessionStart hook from the Claude Code plugin.

if command -v shipyard &>/dev/null; then
  exit 0
fi

echo ""
echo "[Shipyard] CLI binary not found. Installing..."
echo ""

# Download and run the install script
curl -fsSL https://raw.githubusercontent.com/danielraffel/Shipyard/main/install.sh | sh

# Verify it worked
if command -v shipyard &>/dev/null; then
  echo ""
  echo "[Shipyard] Installed successfully: $(shipyard --version)"
elif [ -f "$HOME/.local/bin/shipyard" ]; then
  echo ""
  echo "[Shipyard] Installed to ~/.local/bin/shipyard"
  echo "[Shipyard] Add to PATH: export PATH=\"\$HOME/.local/bin:\$PATH\""
else
  echo ""
  echo "[Shipyard] Installation may have failed. Try manually:"
  echo "  curl -fsSL https://raw.githubusercontent.com/danielraffel/Shipyard/main/install.sh | sh"
fi

exit 0
