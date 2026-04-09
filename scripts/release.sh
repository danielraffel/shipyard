#!/bin/bash
set -euo pipefail

# Release a new version of Shipyard.
#
# Usage:
#   ./scripts/release.sh patch    # 0.1.0 → 0.1.1
#   ./scripts/release.sh minor    # 0.1.0 → 0.2.0
#   ./scripts/release.sh major    # 0.1.0 → 1.0.0
#   ./scripts/release.sh 0.3.0    # explicit version
#
# This script:
#   1. Bumps the version in pyproject.toml and __init__.py
#   2. Commits the version bump
#   3. Tags the commit
#   4. Pushes the tag (which triggers the release workflow)
#
# The release workflow builds binaries on 5 platforms and publishes
# a GitHub Release automatically.

BUMP="${1:-}"

if [ -z "$BUMP" ]; then
  echo "Usage: ./scripts/release.sh <patch|minor|major|X.Y.Z>"
  exit 1
fi

# Get current version from pyproject.toml
CURRENT=$(grep '^version = ' pyproject.toml | head -1 | sed 's/version = "\(.*\)"/\1/')
echo "Current version: $CURRENT"

# Calculate new version
IFS='.' read -r MAJOR MINOR PATCH <<< "$CURRENT"
case "$BUMP" in
  patch) NEW="$MAJOR.$MINOR.$((PATCH + 1))" ;;
  minor) NEW="$MAJOR.$((MINOR + 1)).0" ;;
  major) NEW="$((MAJOR + 1)).0.0" ;;
  [0-9]*) NEW="$BUMP" ;;
  *) echo "Invalid bump: $BUMP (use patch, minor, major, or X.Y.Z)"; exit 1 ;;
esac

echo "New version: $NEW"
echo ""

# Confirm
read -p "Release v${NEW}? [y/N] " -n 1 -r
echo ""
if [[ ! $REPLY =~ ^[Yy]$ ]]; then
  echo "Cancelled."
  exit 0
fi

# Update version in pyproject.toml
sed -i '' "s/^version = \"$CURRENT\"/version = \"$NEW\"/" pyproject.toml

# Update version in __init__.py
sed -i '' "s/__version__ = \"$CURRENT\"/__version__ = \"$NEW\"/" src/shipyard/__init__.py

# Update version in plugin files
sed -i '' "s/\"version\": \"$CURRENT\"/\"version\": \"$NEW\"/g" .claude-plugin/plugin.json
sed -i '' "s/\"version\": \"$CURRENT\"/\"version\": \"$NEW\"/g" .claude-plugin/marketplace.json

# Commit and tag
git add pyproject.toml src/shipyard/__init__.py .claude-plugin/plugin.json .claude-plugin/marketplace.json
git commit -m "Release v${NEW}"
git tag -a "v${NEW}" -m "Shipyard v${NEW}"

# Push
echo ""
echo "Pushing tag v${NEW}..."
git push origin main
git push origin "v${NEW}"

echo ""
echo "Done. Release workflow will build binaries and publish to:"
echo "  https://github.com/danielraffel/Shipyard/releases/tag/v${NEW}"
echo ""
echo "Monitor with:"
echo "  gh run list --repo danielraffel/Shipyard --limit 3"
