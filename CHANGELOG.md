# Changelog

All notable user-facing changes to the Shipyard CLI are recorded here.
Plugin changes track in `.claude-plugin/plugin.json`; the two surfaces
are independently versioned.

## [0.8.0] — 2026-04-17

### Added

- **`shipyard release-bot setup | status`** — guided fine-grained PAT
  provisioning for `RELEASE_BOT_TOKEN` across multi-project setups.
  Opens a pre-filled PAT creation URL, stores the secret via
  stdin-piped `gh secret set` (never argv, never logged, never
  persisted), dispatches a real workflow run to verify
  `actions/checkout` accepts the token before exiting. Per-project
  PAT name default (`<repo>-release-bot`); `--shared-name` flag
  opts into a single PAT across all consumer repos.
- **`shipyard doctor --release-chain`** — live probe of the release
  chain via workflow_dispatch. Catches PAT-scope and secret-drift
  failure modes before a real release attempt hits them.
- **`shipyard cloud run --require-sha`** — refuses to dispatch
  unless the remote ref currently points at the specified SHA.
  Guards against force-push races.
- **`shipyard pr --skip-bump / --skip-skill-update`** — shorthand
  trailer flags that auto-append the exact `Version-Bump:` or
  `Skill-Update:` format to the tip commit.
- **`shipyard watch`** — live stream of an in-flight ship; reads
  Phase 1 state; NDJSON events under `--json`; exit 0/1/2/3/130
  contract for scripts.
- **`shipyard auto-merge <pr>`** — cron-friendly one-shot merge
  daemon. Exits 0 merged, 1 fail, 2 not-found, 3 in-flight.
  Decouples dispatch from merge; obsoletes the always-on
  conductor pattern and supersedes #41.
- **`shipyard cloud retarget`** — mid-flight per-target provider
  switching on an open PR. Cancels the matching in-progress job,
  dispatches a fresh run with the new provider.

### Fixed / Hardened

- release-bot setup: run-ID correlation (not timestamps) for
  verification; three-state secret probe (present/missing/unknown).
- doctor release-chain: exact (key, target) matching for trailer
  conflict stripping (substring would have stripped
  `skill=ci-tools` when target was `skill=ci`).
- trailer shortcuts: refuse to amend when index has staged changes;
  replace stale per-surface/per-skill trailers rather than stack.
- auto-merge: catches `GhError` for structured failure output;
  preserves idempotent success on re-runs after prior merge.
- cloud retarget: routes all lookups through dispatch plan repo;
  single JSON envelope per invocation; re-resolves plan with
  authoritative dispatch-repo head ref.

## [0.7.0]

## [0.6.0]

## [0.5.0]

## [0.4.0]

## [0.3.0] — 2026-04-15

### Added

- **`shipyard ship --resume` / `--no-resume`.** An interrupted ship (laptop
  closed, OS restart, agent crash) now persists durable state to
  `<state_dir>/ship/<pr>.json` on every material event — dispatched run
  IDs, evidence snapshot, merge-policy signature, PR context. A fresh
  session automatically picks up from the same run IDs without
  re-dispatching. Refuses to resume on SHA or merge-policy drift with a
  clear message. Works across agents — the state file is the interop
  point, so a ship you started in Claude Code can resume in Codex (or
  any shell) and vice versa.
- **`shipyard ship-state list | show <pr> | discard <pr>`.** Inspect and
  manage in-flight ship state. `list` is a one-line-per-PR summary with
  title and GitHub URL; `show` dumps every field including dispatched
  runs and evidence snapshot; `discard` archives a stale entry.
- **`shipyard cleanup --ship-state`.** Opt-in pruning of ship state:
  archived files older than 30 days are deleted; active files older
  than 14 days whose PR is closed/merged on GitHub are deleted. Open
  PRs are always preserved. Dry-run by default; `--apply` to delete.
- **PR context in ship state.** `pr_url`, `pr_title`, and `commit_subject`
  are recorded so `ship-state list/show` is self-describing — come back
  to a week-old state file and immediately see what you were shipping.
- Full documentation: [`docs/ship-resume.md`](docs/ship-resume.md).

### Fixed

- `RepoRef` objects are now serialized via their `.slug` property when
  populating the ship-state `repo` field. Prior to this, a ship could
  raise `TypeError: Object of type RepoRef is not JSON serializable`.
- Import ordering in `_preview_ship_state_prune` fixed so ruff passes.

### Phase 2 (deferred)

Cloud-hosted hand-off via Claude Code Routines remains deferred to
issue #41. Phase 1 covers the close-and-reopen-same-day case
completely with zero external dependencies; Phase 2 is only uniquely
valuable when the user is genuinely away for hours (overnight, plane,
weekend). Revisit after two weeks of real-world Phase 1 usage.
