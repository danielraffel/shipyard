"""Unit tests for `shipyard.core.tree_drift` (#249).

Exercises the signature + dirty-path helpers directly against real
git repos under `tmp_path`. Keeps executor-integration out of scope —
that's covered separately in tests/executor/test_249_tree_drift.py.
"""

from __future__ import annotations

import subprocess
from pathlib import Path  # noqa: TC003 — tmp_path fixture type hint

from shipyard.core.tree_drift import (
    compute_signature,
    format_drift_error,
    list_dirty_paths,
)


def _init_repo(path: Path, *, seed_file: str = "seed.txt") -> None:
    subprocess.run(["git", "init", "-q"], cwd=path, check=True)
    subprocess.run(
        ["git", "config", "user.email", "t@example.com"],
        cwd=path, check=True,
    )
    subprocess.run(
        ["git", "config", "user.name", "Test"], cwd=path, check=True,
    )
    (path / seed_file).write_text("seed\n")
    subprocess.run(["git", "add", seed_file], cwd=path, check=True)
    subprocess.run(
        ["git", "commit", "-q", "-m", "initial"], cwd=path, check=True,
    )


def test_signature_stable_when_tree_unchanged(tmp_path: Path) -> None:
    _init_repo(tmp_path)
    sig_a = compute_signature(tmp_path)
    sig_b = compute_signature(tmp_path)
    assert sig_a is not None
    assert sig_a == sig_b


def test_signature_changes_on_tracked_file_edit(tmp_path: Path) -> None:
    _init_repo(tmp_path)
    baseline = compute_signature(tmp_path)
    (tmp_path / "seed.txt").write_text("seed\nedit\n")
    after = compute_signature(tmp_path)
    assert baseline != after


def test_signature_changes_on_untracked_file_create(tmp_path: Path) -> None:
    _init_repo(tmp_path)
    baseline = compute_signature(tmp_path)
    (tmp_path / "new.txt").write_text("hello\n")
    after = compute_signature(tmp_path)
    assert baseline != after


def test_signature_changes_on_untracked_file_content(
    tmp_path: Path,
) -> None:
    _init_repo(tmp_path)
    (tmp_path / "new.txt").write_text("v1\n")
    baseline = compute_signature(tmp_path)
    (tmp_path / "new.txt").write_text("v2\n")
    after = compute_signature(tmp_path)
    assert baseline != after


def test_signature_ignores_gitignored_file(tmp_path: Path) -> None:
    # Ignored paths shouldn't count — build caches / outputs are the
    # classic false-positive source. This is load-bearing for real
    # projects where cmake / ninja write to build/ mid-run.
    _init_repo(tmp_path)
    (tmp_path / ".gitignore").write_text("build/\n")
    subprocess.run(
        ["git", "add", ".gitignore"], cwd=tmp_path, check=True,
    )
    subprocess.run(
        ["git", "commit", "-q", "-m", "ignore build"],
        cwd=tmp_path, check=True,
    )
    baseline = compute_signature(tmp_path)
    (tmp_path / "build").mkdir()
    (tmp_path / "build" / "artifact.o").write_text("x")
    after = compute_signature(tmp_path)
    assert baseline == after


def test_signature_changes_when_head_moves(tmp_path: Path) -> None:
    _init_repo(tmp_path)
    baseline = compute_signature(tmp_path)
    (tmp_path / "seed.txt").write_text("seed\ncommit2\n")
    subprocess.run(
        ["git", "commit", "-aq", "-m", "second"], cwd=tmp_path, check=True,
    )
    after = compute_signature(tmp_path)
    assert baseline != after


def test_signature_returns_none_outside_git_repo(tmp_path: Path) -> None:
    # Callers treat None as "can't check, skip the guard" so non-git
    # projects aren't penalized. Regression guard against a refactor
    # that raises instead.
    assert compute_signature(tmp_path) is None


def test_list_dirty_paths_clean_tree(tmp_path: Path) -> None:
    _init_repo(tmp_path)
    assert list_dirty_paths(tmp_path) == []


def test_list_dirty_paths_reports_modifications(tmp_path: Path) -> None:
    _init_repo(tmp_path)
    (tmp_path / "seed.txt").write_text("seed\nedit\n")
    (tmp_path / "new.txt").write_text("new\n")
    paths = list_dirty_paths(tmp_path)
    # Shape matches `git status --short` — status prefix + path.
    assert any("seed.txt" in line for line in paths)
    assert any("new.txt" in line for line in paths)


def test_format_drift_error_shape() -> None:
    # Exact wording is load-bearing for the #249 acceptance criteria:
    # the stage name, the "what changed" header, and the escape-hatch
    # hint must all be present so agents / operators see a useful
    # diagnostic without parsing logs.
    msg = format_drift_error(
        stage="build",
        initial_paths=[" M initial-dirty.txt"],
        current_paths=[" M initial-dirty.txt", " M new-change.cpp"],
    )
    assert "stage=build" in msg
    assert "what changed:" in msg
    assert "new-change.cpp" in msg
    # The before-set file must NOT be reported as changed.
    assert "+ initial-dirty.txt" not in msg
    assert "--allow-tree-drift" in msg


def test_format_drift_error_no_paths_still_useful() -> None:
    # Edge case: signature changed but list_dirty_paths returned empty
    # (e.g. a HEAD move with matching tree state). Message must still
    # be clear about what to do.
    msg = format_drift_error(stage="configure", initial_paths=[], current_paths=[])
    assert "stage=configure" in msg
    assert "#238" in msg
    assert "--allow-tree-drift" in msg
