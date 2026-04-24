"""End-to-end tests for LocalExecutor tree-drift detection (#249).

Concrete races we're guarding against (from the #249 brief):

  2026-04-24, Spectr PR #20 — 30+ min burned on a mac build that
  failed at 90%+ because a parallel edit on a different branch of
  the same tree leaked a new SDK method call into the build step.
  The compile error surfaced in the middle of build, ~20 minutes
  into an otherwise-healthy run.

The guard's job is to catch that drift at the first stage boundary
after the edit and fail fast with a clear error, instead of letting
the run burn another 15 minutes before surfacing an unrelated-
looking compile error.

Tests here drive real `shipyard.executor.local.LocalExecutor.validate`
calls against real git repos under `tmp_path` with multi-stage
validation configs, which is the integration shape #249 cares about.
"""

from __future__ import annotations

import subprocess
import tempfile
from pathlib import Path

import pytest

from shipyard.core.classify import FailureClass
from shipyard.core.job import TargetStatus
from shipyard.executor.local import LocalExecutor


@pytest.fixture
def _drift_workspace() -> tuple[Path, Path]:
    # Two sibling dirs: a git repo under repo/ and a log dir under
    # logs/. In production the log path lives under state_dir
    # (typically ~/.shipyard/logs/...), outside the working tree, so
    # the log file's own writes can never be confused with drift.
    # Keep that invariant in tests.
    with tempfile.TemporaryDirectory() as tmp:
        tmp_root = Path(tmp)
        root = tmp_root / "repo"
        root.mkdir()
        logs = tmp_root / "logs"
        logs.mkdir()
        subprocess.run(["git", "init", "-q"], cwd=root, check=True)
        subprocess.run(
            ["git", "config", "user.email", "t@example.com"],
            cwd=root, check=True,
        )
        subprocess.run(
            ["git", "config", "user.name", "Test"], cwd=root, check=True,
        )
        (root / "src.txt").write_text("v1\n")
        subprocess.run(["git", "add", "src.txt"], cwd=root, check=True)
        subprocess.run(
            ["git", "commit", "-q", "-m", "initial"], cwd=root, check=True,
        )
        yield root, logs


@pytest.fixture
def repo_dir(_drift_workspace: tuple[Path, Path]) -> Path:
    return _drift_workspace[0]


@pytest.fixture
def log_path(_drift_workspace: tuple[Path, Path]) -> str:
    return str(_drift_workspace[1] / "run.log")


def _run(
    executor: LocalExecutor,
    repo_dir: Path,
    log_path: str,
    *,
    stages: dict[str, str],
    allow_tree_drift: bool = False,
):
    validation_config: dict = dict(stages)
    validation_config["_allow_tree_drift"] = allow_tree_drift
    return executor.validate(
        sha="abc1234",
        branch="test",
        target_config={
            "name": "test",
            "platform": "macos-arm64",
            "cwd": str(repo_dir),
        },
        validation_config=validation_config,
        log_path=log_path,
    )


def test_clean_tree_passes(repo_dir: Path, log_path: str) -> None:
    # Acceptance criterion: clean tree → no change from today.
    # Every stage succeeds, no drift, PASS result.
    executor = LocalExecutor()
    result = _run(
        executor, repo_dir, log_path,
        stages={"setup": "true", "build": "true", "test": "true"},
    )
    assert result.status == TargetStatus.PASS, (
        f"expected PASS, got {result.status} / {result.error_message!r}"
    )
    assert result.failure_class is None


def test_edit_between_stages_aborts_with_tree_drift(
    repo_dir: Path, log_path: str,
) -> None:
    # Acceptance criterion: edit a tracked file mid-run, next stage
    # boundary detects drift and aborts with TREE_DRIFT failure class.
    # The `setup` stage mutates a tracked file; the next stage
    # (`build`) runs its drift check first, sees the new signature,
    # and the run terminates before `build`'s command ever executes.
    executor = LocalExecutor()
    # Note: the setup command edits `src.txt` — that's the "parallel
    # edit" in the real scenario, modeled here as an in-stage mutation
    # so the test is deterministic.
    edit_cmd = f"echo edit >> {repo_dir / 'src.txt'}"
    result = _run(
        executor, repo_dir, log_path,
        stages={
            "setup": edit_cmd,
            "build": "echo build-would-have-run",
            "test": "echo test-would-have-run",
        },
    )
    assert result.status == TargetStatus.ERROR
    assert result.failure_class == FailureClass.TREE_DRIFT.value
    assert "working tree changed" in (result.error_message or "")
    # The error must name the stage that was about to start.
    assert "stage=build" in (result.error_message or "")
    # Build's actual command must NOT have been run — the log should
    # not mention `build-would-have-run`.
    log_text = Path(log_path).read_text()
    assert "build-would-have-run" not in log_text
    assert "TREE_DRIFT" in log_text


def test_allow_tree_drift_suppresses_guard(
    repo_dir: Path, log_path: str,
) -> None:
    # Acceptance criterion: --allow-tree-drift tolerates mid-run edits.
    # Same scenario as above, but with the escape hatch engaged — the
    # build + test stages proceed normally and the run reaches PASS.
    executor = LocalExecutor()
    edit_cmd = f"echo edit >> {repo_dir / 'src.txt'}"
    result = _run(
        executor, repo_dir, log_path,
        stages={
            "setup": edit_cmd,
            "build": "echo build-ran",
            "test": "echo test-ran",
        },
        allow_tree_drift=True,
    )
    assert result.status == TargetStatus.PASS, (
        f"expected PASS with --allow-tree-drift, got "
        f"{result.status} / {result.error_message!r}"
    )
    log_text = Path(log_path).read_text()
    assert "build-ran" in log_text
    assert "test-ran" in log_text


def test_edit_during_first_stage_not_caught(
    repo_dir: Path, log_path: str,
) -> None:
    # Documented behavior: drift detection is stage-boundary-grained,
    # not in-stage. An edit during the last stage won't be caught —
    # there's no next boundary to check at. This test pins that
    # behavior so a future refactor that claims "detects drift in
    # every stage" gets called out for the missing check.
    executor = LocalExecutor()
    edit_cmd = f"echo edit >> {repo_dir / 'src.txt'}"
    result = _run(
        executor, repo_dir, log_path,
        stages={"test": edit_cmd},
    )
    # Single-stage run → no boundary check fires → PASS despite
    # mid-stage drift. This is the "acceptable granularity" trade-off
    # the issue explicitly calls out.
    assert result.status == TargetStatus.PASS


def test_untracked_file_creation_between_stages_is_drift(
    repo_dir: Path, log_path: str,
) -> None:
    # Creating an untracked non-ignored file mid-run counts as drift.
    # This is the shape of a parallel agent that writes a new source
    # file into the project root (e.g. a code generator or refactor
    # agent). Should fail fast just like the tracked-file edit case.
    executor = LocalExecutor()
    create_cmd = f"echo new-src > {repo_dir / 'new_src.cpp'}"
    result = _run(
        executor, repo_dir, log_path,
        stages={
            "setup": create_cmd,
            "build": "echo build",
        },
    )
    assert result.status == TargetStatus.ERROR
    assert result.failure_class == FailureClass.TREE_DRIFT.value


def test_gitignored_file_not_drift(
    repo_dir: Path, log_path: str,
) -> None:
    # Build outputs write to `build/` and similar. Those paths are
    # gitignored in every real project and must NOT be flagged as
    # drift — otherwise the guard false-positives on every real
    # build and becomes useless.
    (repo_dir / ".gitignore").write_text("build/\n")
    subprocess.run(["git", "add", ".gitignore"], cwd=repo_dir, check=True)
    subprocess.run(
        ["git", "commit", "-q", "-m", "ignore build"],
        cwd=repo_dir, check=True,
    )

    executor = LocalExecutor()
    # The setup stage creates build/ and writes output into it. By
    # the next stage boundary that's present on disk — but because
    # it's gitignored the signature shouldn't change.
    build_dir = repo_dir / "build"
    write_cmd = (
        f"mkdir -p {build_dir} && echo artifact > {build_dir / 'out.o'}"
    )
    result = _run(
        executor, repo_dir, log_path,
        stages={
            "setup": write_cmd,
            "build": "true",
            "test": "true",
        },
    )
    assert result.status == TargetStatus.PASS, (
        f"gitignored writes must not false-positive; got "
        f"{result.status} / {result.error_message!r}"
    )
    assert result.failure_class is None
