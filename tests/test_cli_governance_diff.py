"""Regression tests for `governance diff` error handling.

Codex flagged that `governance diff` iterates only `status.reports`
and never checks `status.errors`, so a permission failure on every
branch would still print a clean diff and exit 0. Fix: surface
errors and exit non-zero.
"""

from __future__ import annotations

from unittest.mock import patch

from click.testing import CliRunner

from shipyard.cli import main
from shipyard.governance.github import RepoRef
from shipyard.governance.status import GovernanceStatus


def _fake_status_with_errors() -> GovernanceStatus:
    return GovernanceStatus(
        repo=RepoRef("me", "r"),
        profile_name="solo",
        reports=(),
        errors=("main: gh api timeout/missing",),
    )


def test_governance_diff_exits_nonzero_on_fetch_errors(tmp_path) -> None:
    """A diff that can't read live state must NOT exit 0 with a clean report."""
    # Minimal config in a temp .shipyard dir
    (tmp_path / ".shipyard").mkdir()
    (tmp_path / ".shipyard" / "config.toml").write_text(
        '[project]\nprofile = "solo"\n'
        '[governance]\nrequired_status_checks = ["mac"]\n'
    )

    runner = CliRunner()
    with patch(
        "shipyard.governance.detect_repo_from_remote",
        return_value=RepoRef("me", "r"),
    ), patch(
        "shipyard.governance.build_status",
        return_value=_fake_status_with_errors(),
    ), runner.isolated_filesystem(temp_dir=str(tmp_path)):
        # Inside isolated_filesystem, cwd has no .shipyard — copy it in.
        import shutil as _shutil
        _shutil.copytree(tmp_path / ".shipyard", ".shipyard")
        result = runner.invoke(main, ["governance", "diff"])

    assert result.exit_code == 1, result.output
    assert "could not be read" in result.output.lower() or "timeout" in result.output.lower()
