# Shipyard Hardening Plan

This document covers issues #31 and #34, additional improvement opportunities,
Pulp impact analysis, and a README/docs restructure plan.

---

## 1. Issue #31 — Incremental Bundle Delivery

### Problem

`create_bundle()` always builds a full bundle (`git bundle create sha --all`),
producing ~443 MB for Pulp's 372 MB repo. Even when the remote is only 1-2
commits behind, the entire repo history is re-bundled and uploaded. This means
10-15 min of upload time per Windows ship cycle.

The existing `_remote_has_sha()` optimization skips bundling entirely when
the remote already has the exact SHA — but when it doesn't (the common case
during active development), there's no incremental path.

### Approach: Remote HEAD Negotiation

Query the remote for its current HEAD SHA, then create a delta bundle:

```
git bundle create shipyard.bundle <target_sha> ^<remote_head_sha>
```

This produces a thin bundle containing only the commits between what the
remote has and what it needs. For a typical 1-2 commit delta, the bundle
drops from ~443 MB to a few KB.

### Implementation

**Files to modify:**

| File | Change |
|------|--------|
| `src/shipyard/bundle/git_bundle.py` | Add `create_incremental_bundle()` that accepts `exclude_shas` parameter; modify `create_bundle()` to accept optional `basis_shas` for `^sha` exclusion |
| `src/shipyard/executor/ssh.py` | Before `create_bundle()`, query remote HEAD via SSH (`git rev-parse HEAD`); pass result to incremental bundle; fall back to full bundle on failure |
| `src/shipyard/executor/ssh_windows.py` | Same changes as `ssh.py` but via PowerShell/EncodedCommand transport |

**Steps:**

1. Add `_remote_head_sha(host, repo_path, ssh_options)` helper to `ssh.py`
   (similar to existing `_remote_has_sha()` but returns the SHA string)
2. Add equivalent `_remote_head_sha_windows()` to `ssh_windows.py`
3. Modify `create_bundle()` to accept optional `basis_shas: list[str]`
   parameter — when provided, appends `^<sha>` for each to the git command
4. In both SSH executors' `_validate_once()`:
   - If `_remote_has_sha()` → skip bundle (existing behavior)
   - Else: query `_remote_head_sha()` → if valid, create incremental bundle
   - If incremental bundle fails (e.g., no common ancestor), fall back to
     full bundle
5. Add tests for incremental bundle creation and fallback

**Fallback safety:** If the remote HEAD is on a completely divergent branch
or the incremental bundle fails for any reason, fall back to a full bundle.
The user never sees this — it's transparent.

**Expected impact:** Pulp Windows cycle drops from ~15 min (443 MB upload)
to seconds (KB-sized delta) for typical iterations.

### Pulp Impact

**None (additive).** The change is internal to `create_bundle()` and the
SSH executors. Pulp calls `shipyard run` / `shipyard ship` — the CLI
interface doesn't change. The bundle format is standard git bundles; only
the content size changes. If anything, Pulp benefits the most since it was
the project that surfaced this issue.

---

## 2. Issue #34 — `--resume-from` for SSH Executors

### Problem

`--resume-from` is accepted by the CLI but silently ignored by SSH and
SSH-Windows executors. The flag is only implemented in `LocalExecutor`.
Every SSH validation run re-runs all 4 stages from scratch, even when
only the test stage is relevant. For Pulp on Windows: ~12 min of
configure+build before 1 min of tests.

### Approach: Probe Remote State + Skip Stages

1. When `--resume-from <stage>` is passed, SSH into the remote and check
   for build artifacts that prove earlier stages completed
2. If artifacts exist, skip stages before the resume point
3. If artifacts don't exist, warn and run from the beginning

### Implementation

**Files to modify:**

| File | Change |
|------|--------|
| `src/shipyard/executor/ssh.py` | Stop deleting `resume_from`; add `_probe_remote_build_state()` helper; modify `_build_remote_command()` to accept `resume_from` and skip stages |
| `src/shipyard/executor/ssh_windows.py` | Same changes via PowerShell |
| `src/shipyard/executor/base.py` | Document `resume_from` in the Protocol (if not already) |

**Remote state probing:**

The probe checks for indicators that earlier stages completed:

| Stage to skip | What to check on remote |
|---------------|------------------------|
| setup | Build directory exists |
| configure | `CMakeCache.txt` exists (CMake), `build.ninja` (Ninja), `Makefile` (make) — or a generic marker file written by the setup stage |
| build | Binary artifacts exist in build dir, or a shipyard marker file |

For generality, we'll use a **Shipyard marker file** approach:
- After each stage succeeds on the remote, the executor writes
  `.shipyard-stage-<name>-<sha>` to the build directory
- Resume probing checks for these markers
- Markers are SHA-specific so stale build artifacts from a different
  commit don't cause false skips

**Steps:**

1. Modify `_build_remote_command()` in both SSH executors to support
   `resume_from` — filter stages the same way `_get_stages()` does in
   `local.py`
2. After each stage succeeds, append a marker-write command:
   `touch .shipyard-stage-<name>-<sha>` (POSIX) or
   `New-Item .shipyard-stage-<name>-<sha>` (Windows)
3. Add `_probe_remote_build_state()` that checks for markers via SSH
4. In `validate()`, when `resume_from` is set:
   - Probe remote for the marker of the stage before resume_from
   - If marker exists for the current SHA → proceed with resume
   - If marker doesn't exist → log a warning, run all stages
5. Add tests

**Expected impact:** Pulp Windows test-only iterations drop from ~15 min
to ~2 min.

### Pulp Impact

**None (additive).** Pulp's `.shipyard/config.toml` uses stage-aware
validation. The `--resume-from` flag is a CLI option that Pulp doesn't
currently use (because it didn't work). Once implemented, Pulp's CI
skill can recommend it for test-only reruns.

The marker files (`.shipyard-stage-*`) are written to the remote build
directory, not committed to git. No config changes needed.

---

## 3. Additional Improvement Opportunities

These are opportunities beyond #31 and #34 to make Shipyard a better
reusable abstraction.

### 3a. Implement `shipyard targets` Subcommand

**Priority: Medium**

The README references `shipyard targets add ubuntu` but this doesn't
exist. The skill file explicitly notes: "There is no `shipyard config`
or `shipyard targets` subcommand yet."

**Scope:**
- `shipyard targets` — list configured targets with reachability status
- `shipyard targets add <name>` — interactive add (detect backend, probe
  host, write to config)
- `shipyard targets remove <name>` — remove from config
- `shipyard targets test <name>` — probe + run a health check

This makes the "add a new platform" workflow zero-config-editing, which
is important for the "obviously useful" bar.

### 3b. Implement `shipyard config` Subcommand

**Priority: Medium**

Currently referenced in README but not implemented.

**Scope:**
- `shipyard config profiles` — list defined profiles
- `shipyard config use <profile>` — switch active profile
- `shipyard config show` — dump effective merged config (global + project + local)

The profile switching is already partially implemented in `cli.py`
(`_rewrite_profile_in_config()`) but the subcommand routing doesn't exist.

### 3c. Cloud Executor Polling Timeout

**Priority: High (reliability)**

`cloud.py:_poll_run()` loops `while True` with no deadline. A hanging
GitHub Actions workflow blocks the drain lock indefinitely, blocking
every other queued job on the machine. Add a configurable timeout
(default 60 min) after which the cloud run is marked ERROR.

### 3d. Queue `_jobs` Direct Access Cleanup

**Priority: Low (code quality)**

`cli.py:337` accesses `queue._jobs` directly. Add a public
`queue.all_jobs()` method and use it.

### 3e. `ship/merge.py:ship()` Vestigial Code

**Priority: Low (code quality)**

The `ship()` function in `merge.py` exists alongside a much more
capable `ship` command in `cli.py`. Determine if `merge.py:ship()` is
still called anywhere; if not, remove it to reduce confusion.

### 3f. Config Field Naming Consistency

**Priority: Low**

Target config uses `backend = "local"` but fallback entries use
`type = "cloud"`. Normalize to one field name (preferably `backend`)
with backward compat for `type`.

---

## 4. Pulp Impact Summary

| Change | Pulp Impact | Action Needed |
|--------|------------|---------------|
| Incremental bundles (#31) | Beneficial, no breaking changes | None |
| SSH resume-from (#34) | Beneficial, no breaking changes | Update CI skill to recommend `--resume-from` |
| `shipyard targets` subcommand | No impact (new feature) | None |
| `shipyard config` subcommand | No impact (new feature) | None |
| Cloud polling timeout | Beneficial (prevents hangs) | None |
| Queue cleanup | No impact (internal) | None |
| `merge.py` cleanup | No impact (internal) | None |
| Config field normalization | **Low risk** — Pulp uses `backend` not `type` | Verify `.shipyard/config.toml` fallback entries |

**Critical note:** Pulp's `tools/shipyard.toml` pins **v0.1.3** but the
migration docs show dogfooding through v0.1.13+. The pin file is stale
and should be bumped when these changes ship. This isn't caused by our
changes but is an existing risk — a fresh `install-shipyard.sh` on a
new machine installs v0.1.3 which has 10 known bugs fixed in later
versions.

---

## 5. README & Docs Restructure

### Goal

Make the README simple, focused, and obviously useful — inspired by
[uv's README](https://github.com/astral-sh/uv):

- **Who it's for** and **why it's useful** up top
- **Installation** immediately after
- **Everything else** linked to separate pages

Current README: ~870 lines. Target: ~200-250 lines.

### Proposed README Structure

```
# Shipyard

One-line: Cross-platform CI for AI agents. Validates exact commits
across your machines, merges only when everything is green.

## Highlights
- 4-5 bullet points (the differentiators, distilled)

## Install

### Claude Code (recommended)
[3 steps, concise]

### CLI
[curl one-liner]

## Quick Start
shipyard init → shipyard run → shipyard ship

## How It Works
[5-8 lines: local/remote/cloud, exact-SHA, evidence gate]

## Documentation
- [Examples & Scenarios](docs/examples.md)
- [Targets & Fallback Chains](docs/targets.md)
- [Security & Governance](docs/governance.md)
- [Agent Integration](docs/agent-integration.md)
- [Profiles & Configuration](docs/profiles.md)
- [CLI Reference](docs/cli-reference.md)
- [Workflow Scenarios](docs/workflows.md)

## Requirements
[table, same as current]
```

### Content to Extract to `docs/`

| Current Section | New File | ~Lines Moved |
|----------------|----------|-------------|
| Security & Governance Profiles | `docs/governance.md` | ~100 |
| Examples (4 scenarios) | `docs/examples.md` | ~130 |
| How Targets Work | `docs/targets.md` | ~90 |
| Agent Integration | `docs/agent-integration.md` | ~85 |
| Workflow Scenarios | `docs/workflows.md` | ~100 |
| Profiles | `docs/profiles.md` | ~95 |
| Quick Reference (expanded) | `docs/cli-reference.md` | ~30 (+ expansion) |

### What Stays

- Title + one-liner + highlights
- Install (both paths, concise)
- Quick start (3 commands)
- How it works (brief)
- Documentation links
- Requirements table
- "This Repo Uses Shipyard" (trimmed to 3 lines + link)

### Additional Cleanup

- Remove references to `shipyard targets add` and `shipyard config use`
  from the README until those commands exist (or implement them first —
  see 3a/3b above)
- Move blog post link from line 1 to the docs or a "Learn More" section
- Plugin manifest version (`0.1.1`) should be updated to match the
  actual release

---

## 6. Implementation Order

Recommended sequence, balancing impact and risk:

| Phase | Work | Why This Order |
|-------|------|---------------|
| **1** | Incremental bundles (#31) | Highest user impact; unblocks fast Windows iteration for Pulp |
| **2** | SSH `--resume-from` (#34) | Second-highest impact; compounds with #31 for fast test loops |
| **3** | Cloud polling timeout (3c) | Reliability fix; small scope, high value |
| **4** | README restructure (5) | Non-code; can be done in parallel with anything |
| **5** | `shipyard targets` subcommand (3a) | Fills a documented gap; makes README honest |
| **6** | `shipyard config` subcommand (3b) | Fills the other documented gap |
| **7** | Code quality items (3d, 3e, 3f) | Low priority, do opportunistically |

Phases 1-3 are the "make it great" work. Phase 4 is the "make it
obviously useful" work. Phases 5-6 fill gaps between docs and reality.
Phase 7 is housekeeping.
