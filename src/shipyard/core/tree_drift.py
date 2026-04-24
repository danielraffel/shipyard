"""Working-tree drift detection for `shipyard run` (#249).

Context: `shipyard run` reads the live working tree through every
validation stage (configure → build → test). Edits that land mid-run
— whether from a parallel agent or a human — corrupt the run
silently, then surface as an unrelated-looking compile error 20+
minutes in. #238 shipped a doc-fix + one-liner warning (P3 scope);
this module is the P2 follow-up that fails fast with a specific
error instead.

How it works: compute a content-addressed signature of the working
tree at run start, re-compute at each stage boundary, abort with
``FailureClass.TREE_DRIFT`` when the signature changes. ~milliseconds
per check on a typical repo.

What counts as the tree: git-tracked files with unstaged/staged
modifications, plus all untracked files that aren't gitignored. That's
the set `git ls-files -m -o --exclude-standard` reports. Ignored files
don't count — build outputs and caches routinely change mid-run and
aren't user edits.

Kept in a dedicated module so LocalExecutor's `_run_stages` integration
stays small and both surfaces (executor + future CLI probes) share the
same signature function.
"""

from __future__ import annotations

import hashlib
import subprocess
from pathlib import Path


def compute_signature(cwd: str | Path | None = None) -> str | None:
    """Return a short opaque signature of the working-tree state.

    The signature changes iff any tracked file's on-disk content
    differs, any untracked non-ignored file appears/disappears/changes,
    or HEAD moves. Returns ``None`` when git is unavailable or the
    path isn't a git repo — callers treat that as "can't check, skip
    the guard" so non-git projects aren't penalized.
    """
    cwd_str = str(cwd) if cwd else None

    head = _git_output(["git", "rev-parse", "HEAD"], cwd_str, timeout=5)
    if head is None:
        return None

    # `-m` = tracked files with unstaged modifications.
    # `-o --exclude-standard` = untracked files that aren't gitignored.
    # Staged-but-unmodified files don't show up here but still contribute
    # via the diff hash below — so a `git add` between stages is caught.
    listing = _git_output(
        ["git", "ls-files", "-m", "-o", "--exclude-standard"],
        cwd_str, timeout=30,
    )
    if listing is None:
        return None

    # Also capture the full diff against HEAD so staged changes (which
    # `-m` doesn't list) and content-level edits show up in the hash.
    # `--no-ext-diff` keeps user-configured external diff tools from
    # making the signature non-deterministic.
    diff = _git_output(
        ["git", "diff", "--no-ext-diff", "HEAD"],
        cwd_str, timeout=30,
    )
    if diff is None:
        return None

    h = hashlib.sha256()
    h.update(head.encode())
    h.update(b"\0LS\0")
    h.update(listing.encode())
    h.update(b"\0DIFF\0")
    h.update(diff.encode())

    # Untracked file contents aren't in `git diff HEAD`, so hash them
    # directly. Sort for determinism.
    base = Path(cwd_str) if cwd_str else Path.cwd()
    for path in sorted(p for p in listing.splitlines() if p):
        full = base / path
        h.update(b"\0U\0")
        h.update(path.encode())
        try:
            h.update(full.read_bytes())
        except OSError:
            # Path vanished between the listing and the read (race
            # with a rename/delete). That *is* drift — fold a sentinel
            # into the hash so a subsequent compute sees a different
            # value.
            h.update(b"<missing>")

    return h.hexdigest()[:32]


def list_dirty_paths(cwd: str | Path | None = None) -> list[str]:
    """List paths currently dirty relative to HEAD.

    Shape matches `git status --short` (status prefix + path) so the
    error message surfaces the same view the user would see by running
    git themselves. Empty list when clean or git is unavailable.
    """
    cwd_str = str(cwd) if cwd else None
    output = _git_output(
        ["git", "status", "--short", "--untracked-files=all"],
        cwd_str, timeout=30,
    )
    if output is None:
        return []
    return [line for line in output.splitlines() if line.strip()]


def format_drift_error(
    stage: str,
    initial_paths: list[str],
    current_paths: list[str],
) -> str:
    """Format the user-facing drift error message (#249 acceptance).

    Shows the stage that was about to run and what changed since the
    run started — the initial dirty set vs the current one — so the
    operator / agent knows exactly which files drifted without diffing
    manually.
    """
    lines = [
        f"working tree changed during `shipyard run` (stage={stage}).",
        "mid-run edits produce non-deterministic failures (#238).",
        "re-run after your other edits settle, or use separate "
        "worktrees for parallel work.",
    ]
    initial_set = set(initial_paths)
    current_set = set(current_paths)
    added = sorted(current_set - initial_set)
    removed = sorted(initial_set - current_set)
    if added or removed:
        lines.append("")
        lines.append("what changed:")
        for entry in added:
            lines.append(f"  + {entry}")
        for entry in removed:
            lines.append(f"  - {entry}")
    # Pass the `--allow-tree-drift` hint last so it's visible but not
    # the first thing a reviewer sees — we don't want to advertise the
    # escape hatch over the fix (rebase / separate worktree).
    lines.append("")
    lines.append(
        "(pass --allow-tree-drift to suppress this guard when you "
        "know a build step mutates the tree on purpose.)"
    )
    return "\n".join(lines)


def _git_output(
    cmd: list[str], cwd: str | None, *, timeout: float,
) -> str | None:
    """Run a git command, return stdout on success, None on any failure."""
    try:
        result = subprocess.run(
            cmd, cwd=cwd, capture_output=True, text=True, timeout=timeout,
        )
    except (FileNotFoundError, subprocess.SubprocessError):
        return None
    if result.returncode != 0:
        return None
    return result.stdout
