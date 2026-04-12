"""Tests for the cloud executor (GitHub Actions dispatch)."""

from __future__ import annotations

import json
import subprocess
from unittest.mock import MagicMock, patch

import pytest

from shipyard.core.job import TargetStatus
from shipyard.executor.cloud import CloudExecutor


@pytest.fixture
def executor() -> CloudExecutor:
    return CloudExecutor(
        workflow="build.yml",
        repo="owner/repo",
        poll_interval=0.01,
        dispatch_settle_secs=0.5,
    )


@pytest.fixture
def target_config() -> dict:
    return {
        "name": "ubuntu",
        "platform": "linux-x64",
        "runner_provider": "namespace",
        "runner_selector": "namespace-profile-default",
    }


@pytest.fixture
def validation_config() -> dict:
    return {"command": "cmake --build build && ctest --test-dir build"}


class TestCloudExecutorProbe:
    def test_probe_succeeds_when_gh_authenticated(self, executor: CloudExecutor) -> None:
        with patch("subprocess.run") as mock_run:
            mock_run.return_value = MagicMock(returncode=0)
            assert executor.probe({}) is True

        mock_run.assert_called_once()
        args = mock_run.call_args
        assert args[0][0] == ["gh", "auth", "status"]

    def test_probe_fails_when_gh_not_authenticated(self, executor: CloudExecutor) -> None:
        with patch("subprocess.run") as mock_run:
            mock_run.return_value = MagicMock(returncode=1)
            assert executor.probe({}) is False

    def test_probe_fails_when_gh_not_installed(self, executor: CloudExecutor) -> None:
        with patch("subprocess.run", side_effect=FileNotFoundError):
            assert executor.probe({}) is False

    def test_probe_fails_on_timeout(self, executor: CloudExecutor) -> None:
        with patch("subprocess.run", side_effect=subprocess.TimeoutExpired("gh", 10)):
            assert executor.probe({}) is False


class TestCloudExecutorValidate:
    def test_successful_workflow_run(
        self,
        executor: CloudExecutor,
        target_config: dict,
        validation_config: dict,
        tmp_path,
    ) -> None:
        log_path = str(tmp_path / "cloud.log")

        with patch("shipyard.executor.cloud.subprocess.run") as mock_run:
            # Dispatch succeeds
            dispatch_result = MagicMock(returncode=0)
            # List returns a run
            list_result = MagicMock(
                returncode=0,
                stdout=json.dumps([{"databaseId": 12345, "status": "in_progress"}]),
            )
            # View returns completed+success
            view_result = MagicMock(
                returncode=0,
                stdout=json.dumps({"status": "completed", "conclusion": "success"}),
            )
            mock_run.side_effect = [dispatch_result, list_result, view_result]

            result = executor.validate(
                sha="abc123",
                branch="feature/test",
                target_config=target_config,
                validation_config=validation_config,
                log_path=log_path,
            )

        assert result.status == TargetStatus.PASS
        assert result.backend == "cloud"
        assert result.provider == "namespace"
        assert result.runner_profile == "namespace-profile-default"
        assert result.duration_secs is not None
        assert result.duration_secs >= 0

    def test_workflow_run_failure(
        self,
        executor: CloudExecutor,
        target_config: dict,
        validation_config: dict,
        tmp_path,
    ) -> None:
        log_path = str(tmp_path / "cloud.log")

        with patch("shipyard.executor.cloud.subprocess.run") as mock_run:
            dispatch_result = MagicMock(returncode=0)
            list_result = MagicMock(
                returncode=0,
                stdout=json.dumps([{"databaseId": 99, "status": "in_progress"}]),
            )
            view_result = MagicMock(
                returncode=0,
                stdout=json.dumps({"status": "completed", "conclusion": "failure"}),
            )
            mock_run.side_effect = [dispatch_result, list_result, view_result]

            result = executor.validate(
                sha="abc123",
                branch="main",
                target_config=target_config,
                validation_config=validation_config,
                log_path=log_path,
            )

        assert result.status == TargetStatus.FAIL
        assert result.backend == "cloud"

    def test_dispatch_failure_returns_error(
        self,
        executor: CloudExecutor,
        target_config: dict,
        validation_config: dict,
        tmp_path,
    ) -> None:
        log_path = str(tmp_path / "cloud.log")

        with patch("shipyard.executor.cloud.subprocess.run") as mock_run:
            mock_run.side_effect = subprocess.CalledProcessError(1, "gh")

            result = executor.validate(
                sha="abc123",
                branch="main",
                target_config=target_config,
                validation_config=validation_config,
                log_path=log_path,
            )

        assert result.status == TargetStatus.ERROR
        assert "Failed to dispatch" in (result.error_message or "")

    def test_run_not_found_returns_error(
        self,
        target_config: dict,
        validation_config: dict,
        tmp_path,
    ) -> None:
        log_path = str(tmp_path / "cloud.log")
        executor = CloudExecutor(
            workflow="build.yml",
            repo="owner/repo",
            poll_interval=0.01,
            dispatch_settle_secs=10,
        )

        # time.monotonic() call sequence:
        # 1: start_time in validate()
        # 2: deadline = monotonic() + settle in _wait_for_run
        # 3: while monotonic() < deadline (first check, within deadline)
        # 4: while monotonic() < deadline (second check, past deadline -> exit)
        # 5: monotonic() - start_time in validate() error path
        clock = iter([100.0, 100.0, 105.0, 111.0, 112.0, 113.0, 114.0])

        with patch("shipyard.executor.cloud.subprocess.run") as mock_run, \
             patch("shipyard.executor.cloud.time.sleep"), \
             patch("shipyard.executor.cloud.time.monotonic", side_effect=clock):
            dispatch_result = MagicMock(returncode=0)
            empty_list = MagicMock(returncode=0, stdout="[]")
            mock_run.side_effect = [dispatch_result, empty_list, empty_list, empty_list]

            result = executor.validate(
                sha="abc123",
                branch="main",
                target_config=target_config,
                validation_config=validation_config,
                log_path=log_path,
            )

        assert result.status == TargetStatus.ERROR
        assert "did not appear" in (result.error_message or "")

    def test_runner_overrides_passed_as_input(
        self,
        executor: CloudExecutor,
        validation_config: dict,
        tmp_path,
    ) -> None:
        log_path = str(tmp_path / "cloud.log")
        config_with_overrides = {
            "name": "ubuntu",
            "platform": "linux-x64",
            "runner_provider": "namespace",
            "runner_overrides": {"linux-x64": "nscloud-ubuntu-22.04-amd64-4x16"},
        }

        with patch("shipyard.executor.cloud.subprocess.run") as mock_run:
            dispatch_result = MagicMock(returncode=0)
            list_result = MagicMock(
                returncode=0,
                stdout=json.dumps([{"databaseId": 42, "status": "queued"}]),
            )
            view_result = MagicMock(
                returncode=0,
                stdout=json.dumps({"status": "completed", "conclusion": "success"}),
            )
            mock_run.side_effect = [dispatch_result, list_result, view_result]

            result = executor.validate(
                sha="abc123",
                branch="main",
                target_config=config_with_overrides,
                validation_config=validation_config,
                log_path=log_path,
            )

        assert result.status == TargetStatus.PASS
        # Verify dispatch call included runner_overrides as JSON
        dispatch_call = mock_run.call_args_list[0]
        cmd = dispatch_call[0][0]
        # Find the -f runner_overrides=... argument
        found_override = False
        for i, arg in enumerate(cmd):
            if arg == "-f" and i + 1 < len(cmd) and cmd[i + 1].startswith("runner_overrides="):
                found_override = True
                payload = json.loads(cmd[i + 1].split("=", 1)[1])
                assert payload["linux-x64"] == "nscloud-ubuntu-22.04-amd64-4x16"
        assert found_override, "runner_overrides input not found in dispatch command"


class TestCloudPollTimeout:
    """Tests for the max_poll_secs deadline that prevents indefinite hangs."""

    def test_poll_run_raises_timeout_when_never_completes(self) -> None:
        """A run that stays in_progress past the deadline raises TimeoutError."""
        executor = CloudExecutor(
            workflow="build.yml",
            repo="owner/repo",
            poll_interval=0.01,
            max_poll_secs=0.05,  # 50ms deadline
        )

        # Always return in_progress — never completes
        in_progress = MagicMock(
            returncode=0,
            stdout=json.dumps({"status": "in_progress", "conclusion": None}),
        )
        with patch("subprocess.run", return_value=in_progress):
            with pytest.raises(TimeoutError, match="did not complete within"):
                executor._poll_run("12345")

    def test_poll_run_completes_before_deadline(self) -> None:
        """A run that completes before the deadline returns the conclusion."""
        executor = CloudExecutor(
            workflow="build.yml",
            repo="owner/repo",
            poll_interval=0.01,
            max_poll_secs=10,  # generous
        )
        completed = MagicMock(
            returncode=0,
            stdout=json.dumps({"status": "completed", "conclusion": "success"}),
        )
        with patch("subprocess.run", return_value=completed):
            assert executor._poll_run("12345") == "success"

    def test_validate_returns_error_on_timeout(
        self, target_config: dict, validation_config: dict, tmp_path,
    ) -> None:
        """When _poll_run times out, validate() returns ERROR with the timeout message."""
        executor = CloudExecutor(
            workflow="build.yml",
            repo="owner/repo",
            poll_interval=0.01,
            dispatch_settle_secs=0.5,
            max_poll_secs=0.05,
        )
        log_path = str(tmp_path / "log.txt")

        # Patch dispatch + run lookup to succeed, then make _poll_run hang
        with patch.object(
            executor, "_dispatch_workflow", return_value=None,
        ), patch.object(
            executor, "_wait_for_run", return_value="12345",
        ), patch.object(
            executor, "_poll_run",
            side_effect=TimeoutError("Cloud workflow run 12345 did not complete within 60s"),
        ):
            result = executor.validate(
                sha="abc123",
                branch="main",
                target_config=target_config,
                validation_config=validation_config,
                log_path=log_path,
            )

        assert result.status == TargetStatus.ERROR
        assert "did not complete" in (result.error_message or "")

    def test_per_target_max_poll_overrides_executor_default(
        self, target_config: dict, validation_config: dict, tmp_path,
    ) -> None:
        """target_config['cloud_max_poll_secs'] overrides the executor default."""
        executor = CloudExecutor(
            workflow="build.yml",
            repo="owner/repo",
            poll_interval=0.01,
            max_poll_secs=10,  # default
        )
        log_path = str(tmp_path / "log.txt")
        target = {**target_config, "cloud_max_poll_secs": 999}

        captured = {}

        def fake_poll(run_id, *, progress_callback=None, max_poll_secs=None):
            captured["max_poll_secs"] = max_poll_secs
            return "success"

        with patch.object(
            executor, "_dispatch_workflow", return_value=None,
        ), patch.object(
            executor, "_wait_for_run", return_value="12345",
        ), patch.object(executor, "_poll_run", side_effect=fake_poll):
            executor.validate(
                sha="abc123",
                branch="main",
                target_config=target,
                validation_config=validation_config,
                log_path=log_path,
            )

        assert captured["max_poll_secs"] == 999.0
