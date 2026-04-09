# Releasing Shipyard

## When to release

Release a new version when `src/shipyard/` changes (new commands, bug fixes,
behavior changes). You don't need a release for:

- README edits
- Plugin file changes (commands/, skills/, agents/, hooks/)
- Config file changes (.shipyard/)
- Planning docs

Plugin files are delivered from the Git repo, not from the binary.

## How to release

One command:

```bash
./scripts/release.sh patch    # 0.1.0 → 0.1.1 (bug fixes)
./scripts/release.sh minor    # 0.1.0 → 0.2.0 (new features)
./scripts/release.sh major    # 0.1.0 → 1.0.0 (breaking changes)
./scripts/release.sh 0.3.0    # explicit version
```

The script:

1. Bumps the version in `pyproject.toml`, `__init__.py`, and plugin manifests
2. Commits the version bump
3. Tags the commit (`v0.1.1`)
4. Pushes the tag
5. The tag push triggers `.github/workflows/release.yml` which:
   - Builds binaries on 5 platforms (macOS ARM64/x64, Windows x64, Linux x64/ARM64)
   - Creates a GitHub Release with binaries + SHA256 checksums
   - Uses GitHub-hosted runners by default; Namespace via manual dispatch

## Monitoring a release

```bash
# Watch the build
gh run list --repo danielraffel/Shipyard --limit 3

# Check the release page
gh release view v0.1.1 --repo danielraffel/Shipyard
```

## Using Namespace runners for a release build

```bash
gh workflow run release.yml --repo danielraffel/Shipyard -f runner_provider=namespace
```

## Version locations

The version lives in 4 places (all updated by `release.sh`):

| File | Field |
|------|-------|
| `pyproject.toml` | `version = "X.Y.Z"` |
| `src/shipyard/__init__.py` | `__version__ = "X.Y.Z"` |
| `.claude-plugin/plugin.json` | `"version": "X.Y.Z"` |
| `.claude-plugin/marketplace.json` | `"version": "X.Y.Z"` |

## Versioning convention

- **Patch** (0.1.x): bug fixes, small improvements
- **Minor** (0.x.0): new features, new commands, new ecosystem detectors
- **Major** (x.0.0): breaking changes to config format, CLI output schema, or behavior

The `--json` output schema has its own version (`schema_version` field) that
increments independently when the output format changes.
