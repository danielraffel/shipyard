"""Preflight runs BEFORE `gh pr create` — regression for shipyard#157.

The old order was push → create_pr → preflight. If the ssh probe
failed (flaky Tailscale, missing host config in a worktree), the PR
was already open on GitHub with no ship-state, no dispatched runs,
and no recovery path short of manually closing the PR. The new
order runs preflight first: a probe failure exits cleanly and the
user never sees a stranded PR.

Drives `shipyard ship` through Click's CliRunner with every
network operation mocked, then asserts that the preflight probe
ran before any gh command would have.
"""

from __future__ import annotations

from typing import Any
from unittest.mock import MagicMock, patch

import pytest
from click.testing import CliRunner

from shipyard.cli import main
from shipyard.preflight import BackendUnreachableError


@pytest.fixture
def mock_ship_env(monkeypatch: pytest.MonkeyPatch, tmp_path):
    """Seed just enough of the shipyard environment to reach the
    preflight call: fake git branch/sha, a project-dir config, and
    a state_dir. Every subprocess and HTTP call we don't want to
    run is patched off below."""
    from shipyard.core.config import Config
    from shipyard.core.ship_state import ShipStateStore

    proj = tmp_path / ".shipyard"
    proj.mkdir()
    (proj / "config.toml").write_text(
        '[project]\nname = "t"\n\n'
        '[targets.ubuntu]\nbackend = "ssh"\nhost = "<no host>"\n'
    )
    monkeypatch.chdir(tmp_path)
    monkeypatch.setattr("shipyard.cli._git_branch", lambda: "feat/x")
    monkeypatch.setattr("shipyard.cli._git_sha", lambda: "deadbeefcafe")
    monkeypatch.setattr(
        "shipyard.cli._git_commit_subject", lambda _sha: "subject"
    )
    monkeypatch.setattr(
        "shipyard.cli._detect_repo_slug_or_empty", lambda: "owner/repo"
    )
    monkeypatch.setattr(
        "shipyard.cli._pr_url", lambda _repo, _n: "https://example/pr/1"
    )
    monkeypatch.setattr(
        "shipyard.core.config.Config.load_from_cwd",
        lambda cwd=None: Config(
            data={
                "project": {"name": "t"},
                "targets": {
                    "ubuntu": {
                        "backend": "ssh",
                        "host": "<no host>",
                    },
                },
            },
            global_dir=tmp_path / "_global",
            project_dir=proj,
        ),
    )
    # Fresh ship-state store so we can observe whether a state
    # file got written (it shouldn't on preflight failure).
    store = ShipStateStore(tmp_path / "ship")
    return store, tmp_path


def test_preflight_failure_does_not_create_pr(
    mock_ship_env, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The critical #157 regression: a preflight failure must NOT
    reach `create_pr` or `git push`. The user should see the
    backend-unreachable error, exit code 3, and no leftover PR."""
    store, tmp_path = mock_ship_env

    # Preflight throws — simulates ssh probe failure / no host.
    def fake_preflight(*args: Any, **kw: Any):
        raise BackendUnreachableError(
            "Target 'ubuntu' (ssh) is unreachable. "
            "SSH backend unreachable at <no host>."
        )

    monkeypatch.setattr("shipyard.cli.run_submission_preflight", fake_preflight)

    # Track whether GitHub would have been touched. These should
    # NEVER fire if preflight runs first.
    create_pr = MagicMock()
    find_pr = MagicMock(return_value=None)
    monkeypatch.setattr("shipyard.ship.pr.create_pr", create_pr)
    monkeypatch.setattr("shipyard.ship.pr.find_pr_for_branch", find_pr)

    # Track git-push invocations — also shouldn't fire.
    push_calls = []

    def fake_subprocess_run(*args: Any, **kw: Any):
        cmd = args[0] if args else kw.get("args", [])
        if isinstance(cmd, list) and len(cmd) >= 3 and cmd[:2] == ["git", "push"]:
            push_calls.append(cmd)
        return MagicMock(returncode=0, stdout="", stderr="")

    monkeypatch.setattr("subprocess.run", fake_subprocess_run)

    runner = CliRunner()
    result = runner.invoke(main, ["ship"])
    assert result.exit_code == 3, (
        f"expected BackendUnreachable exit code 3, got {result.exit_code}\n"
        f"output:\n{result.output}"
    )
    assert "unreachable" in result.output.lower()
    # The real regression check: neither create_pr nor git push
    # should have been reached.
    create_pr.assert_not_called()
    assert push_calls == [], (
        "git push must not run before preflight passes — would leave "
        f"a stranded PR. Got: {push_calls}"
    )
