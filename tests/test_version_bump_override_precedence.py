"""Tests for `scripts/version_bump_check.py::assess_surfaces` override
precedence.

Before this fix, the override policy was "can raise, never lower" —
an explicit `Version-Bump: cli=patch` trailer would be silently
dropped when the heuristic chose minor (because the public-API path
cli.py was touched). That defeated the entire point of the trailer:
the author had taken explicit accountability with a reason string,
but their stated intent lost to pattern-matching.

New contract: the trailer is authoritative. If an author writes
`Version-Bump: cli=<level> reason="..."`, that `<level>` wins
against both the heuristic and the conventional-commit ceiling.
Two escape hatches remain:

  * ``skip`` still zeroes out the level (unchanged).
  * If the surface wasn't touched at all, the override is ignored
    (prevents rubber-stamping a bump on an unrelated surface).

Regression fixture: shipyard PR #151 should have been v0.23.1
(patch bug fix) but shipped as v0.24.0 because the heuristic beat
the trailer.
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

import pytest

SCRIPTS_DIR = Path(__file__).resolve().parent.parent / "scripts"


def _load_module():
    if str(SCRIPTS_DIR) not in sys.path:
        sys.path.insert(0, str(SCRIPTS_DIR))
    import version_bump_check  # type: ignore[import-not-found]

    return version_bump_check


def _git(*args: str, cwd: Path) -> None:
    subprocess.check_call(["git", *args], cwd=cwd)


def _init_repo(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv("GIT_AUTHOR_NAME", "T")
    monkeypatch.setenv("GIT_AUTHOR_EMAIL", "t@t")
    monkeypatch.setenv("GIT_COMMITTER_NAME", "T")
    monkeypatch.setenv("GIT_COMMITTER_EMAIL", "t@t")
    _git("init", "--quiet", "--initial-branch=main", cwd=tmp_path)
    (tmp_path / "src").mkdir()
    (tmp_path / "src" / "cli.py").write_text("# v1\n")
    (tmp_path / "pyproject.toml").write_text('version = "0.1.0"\n')
    _git("add", ".", cwd=tmp_path)
    _git("commit", "-q", "-m", "seed", cwd=tmp_path)
    return tmp_path


def _make_cli_surface(mod):
    return mod.Surface(
        name="cli",
        label="Test CLI",
        version_files=[mod.VersionFile(path="pyproject.toml", kind="pyproject_version")],
        trigger_paths=["src/**"],
        public_api_paths=["src/cli.py"],  # touching cli.py → heuristic=minor
        internal_only_paths=[],
        changelog=None,
        auto_apply_patch=True,
    )


def _make_config(mod, surface):
    return mod.Config(
        surfaces=[surface],
        generated_globs=[],
        trailer_version_bump="version-bump",
    )


def _change_cli_and_commit(repo: Path, *, commit_msg: str) -> None:
    (repo / "src" / "cli.py").write_text("# v2\nnew_func = 1\n")
    _git("add", ".", cwd=repo)
    _git("commit", "-q", "-m", commit_msg, cwd=repo)


def test_explicit_patch_override_beats_minor_heuristic(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Regression for shipyard #151 behavior:
    `Version-Bump: cli=patch` must produce final=patch even when the
    heuristic says minor (public-API path touched)."""
    mod = _load_module()
    repo = _init_repo(tmp_path, monkeypatch)
    surface = _make_cli_surface(mod)
    cfg = _make_config(mod, surface)

    _change_cli_and_commit(
        repo,
        commit_msg='bug: real fix\n\n'
                   'Version-Bump: cli=patch reason="bug fix"',
    )

    changed = ["src/cli.py"]
    verdicts = mod.assess_surfaces(cfg, changed, "HEAD~1", "HEAD", repo)

    assert len(verdicts) == 1
    v = verdicts[0]
    assert v.heuristic == "minor"
    assert v.trailer_override == "patch"
    assert v.final_level == "patch", (
        f"Expected final=patch (author override), got final={v.final_level}. "
        "The heuristic won over an explicit trailer — the trailer is meant "
        "to be authoritative when the author accepts accountability via "
        "the reason string."
    )


def test_explicit_minor_override_still_works_when_heuristic_is_minor(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """No-op case: override=minor + heuristic=minor → final=minor."""
    mod = _load_module()
    repo = _init_repo(tmp_path, monkeypatch)
    surface = _make_cli_surface(mod)
    cfg = _make_config(mod, surface)

    _change_cli_and_commit(
        repo,
        commit_msg='feat: x\n\nVersion-Bump: cli=minor reason="feature"',
    )

    changed = ["src/cli.py"]
    verdicts = mod.assess_surfaces(cfg, changed, "HEAD~1", "HEAD", repo)
    assert verdicts[0].final_level == "minor"


def test_explicit_major_override_raises_from_heuristic_minor(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Raising case still works: override=major, heuristic=minor →
    final=major (the author wants a bigger bump than the heuristic
    chose)."""
    mod = _load_module()
    repo = _init_repo(tmp_path, monkeypatch)
    surface = _make_cli_surface(mod)
    cfg = _make_config(mod, surface)

    _change_cli_and_commit(
        repo,
        commit_msg='feat!: breaking\n\n'
                   'Version-Bump: cli=major reason="breaking change"',
    )

    changed = ["src/cli.py"]
    verdicts = mod.assess_surfaces(cfg, changed, "HEAD~1", "HEAD", repo)
    assert verdicts[0].final_level == "major"


def test_skip_override_zeros_out_final(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """`skip` remains authoritative: final=none."""
    mod = _load_module()
    repo = _init_repo(tmp_path, monkeypatch)
    surface = _make_cli_surface(mod)
    cfg = _make_config(mod, surface)

    _change_cli_and_commit(
        repo,
        commit_msg='chore: x\n\nVersion-Bump: cli=skip reason="trivial"',
    )

    changed = ["src/cli.py"]
    verdicts = mod.assess_surfaces(cfg, changed, "HEAD~1", "HEAD", repo)
    assert verdicts[0].final_level == "none"


def test_override_ignored_when_surface_not_touched(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Rubber-stamp protection: if paths in this surface weren't
    touched at all, the override is ignored so a user can't bump an
    unrelated surface."""
    mod = _load_module()
    repo = _init_repo(tmp_path, monkeypatch)
    surface = _make_cli_surface(mod)
    cfg = _make_config(mod, surface)

    # Commit that touches a path OUTSIDE trigger_paths.
    (repo / "docs.md").write_text("hello\n")
    _git("add", ".", cwd=repo)
    _git(
        "commit", "-q", "-m",
        'docs: notes\n\nVersion-Bump: cli=minor reason="noop"',
        cwd=repo,
    )

    changed = ["docs.md"]
    verdicts = mod.assess_surfaces(cfg, changed, "HEAD~1", "HEAD", repo)
    assert verdicts[0].final_level == "none"


def test_conv_commit_feat_does_not_raise_over_explicit_patch_override(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Conv-commit-subject ceiling (``feat:`` → minor) must not
    silently raise an author-declared ``=patch`` back up to minor.

    Previously the code applied conv-commit promotion whenever the
    heuristic was non-none — bypassing the override entirely. That
    reduced the trailer to "can raise, never lower" even though
    skip-vs-level was meant to be a meaningful choice."""
    mod = _load_module()
    repo = _init_repo(tmp_path, monkeypatch)
    surface = _make_cli_surface(mod)
    cfg = _make_config(mod, surface)

    _change_cli_and_commit(
        repo,
        commit_msg='feat: looks-like-a-feature\n\n'
                   'Version-Bump: cli=patch reason="really a patch"',
    )

    changed = ["src/cli.py"]
    verdicts = mod.assess_surfaces(cfg, changed, "HEAD~1", "HEAD", repo)
    assert verdicts[0].final_level == "patch", (
        "Conv-commit-subject-based promotion ignored the explicit "
        "trailer and raised patch → minor. The trailer must win."
    )
