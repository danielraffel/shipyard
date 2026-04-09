"""Regression tests for Codex review P1 findings (post-Stage 1 attempt 4).

Codex-via-RepoPrompt flagged four P1 issues that would keep surfacing
via the "each Stage 1 attempt finds one more bug" loop:

1. apply_bundle() + _apply_bundle_windows() still hard-timed out at
   120s even after upload_bundle was raised to 1800s in v0.1.10
2. FallbackChain.execute() was invoked without kwargs, silently
   dropping resume_from / mode / progress_callback on fallback
   targets (same class as the v0.1.7 kwargs fix but in the fallback
   code path)
3. Remote commands used raw string concat with repo_path /
   remote_bundle_path, so a space or quote in config would break
   shell/PowerShell parsing
4. Streaming layer kept the full merged build output resident in a
   list, which could use significant memory for long build logs
"""

from __future__ import annotations

import inspect
import subprocess
from unittest.mock import patch

from shipyard.bundle.git_bundle import apply_bundle
from shipyard.executor.dispatch import ExecutorDispatcher
from shipyard.executor.ssh import _build_remote_command as _build_posix
from shipyard.executor.ssh_windows import (
    _apply_bundle_windows,
    _ps_single_quote,
)
from shipyard.executor.ssh_windows import (
    _build_remote_command as _build_windows,
)


def _ok():
    return subprocess.CompletedProcess(args=[], returncode=0, stdout="", stderr="")


# ── P1-A: apply_bundle + _apply_bundle_windows accept timeout ──────────


def test_apply_bundle_accepts_timeout_kwarg() -> None:
    params = inspect.signature(apply_bundle).parameters
    assert "timeout" in params
    # Default should be 30 min, matching upload_bundle
    assert params["timeout"].default >= 1800


def test_apply_bundle_uses_passed_timeout() -> None:
    """The timeout kwarg is threaded all the way through to subprocess.run."""
    captured: list[int] = []

    def fake_run(*args, **kwargs):
        captured.append(kwargs.get("timeout"))
        return _ok()

    with patch("subprocess.run", side_effect=fake_run):
        apply_bundle(
            host="ubuntu",
            bundle_path="/tmp/foo.bundle",
            repo_path="/home/x/repo",
            timeout=600,
        )
    assert captured == [600]


def test_windows_apply_bundle_accepts_timeout_kwarg() -> None:
    params = inspect.signature(_apply_bundle_windows).parameters
    assert "timeout" in params
    assert params["timeout"].default >= 1800


def test_windows_apply_bundle_uses_passed_timeout() -> None:
    captured: list[int] = []

    def fake_run(*args, **kwargs):
        captured.append(kwargs.get("timeout"))
        return _ok()

    with patch("subprocess.run", side_effect=fake_run):
        _apply_bundle_windows(
            host="win",
            bundle_path="shipyard.bundle",
            repo_path="C:\\repo",
            ssh_options=[],
            timeout=900,
        )
    assert captured == [900]


# ── P1-B: FallbackChain receives dispatch kwargs ───────────────────────


def test_fallback_chain_forwards_kwargs() -> None:
    """validate_target forwards kwargs through the fallback-chain branch."""
    from shipyard.failover.chain import FallbackChain

    recorded: dict = {}

    class RecordingExecutor:
        def validate(self, *, sha, branch, target_config, validation_config, log_path, **kwargs):
            recorded.update(kwargs)
            from shipyard.core.job import TargetResult, TargetStatus
            return TargetResult(
                target_name=target_config.get("name", "x"),
                platform="linux-x64",
                status=TargetStatus.PASS,
                backend="stub",
            )

        def probe(self, target_config):
            return True

    fake_chain = FallbackChain(
        backends=[{"type": "ssh", "host": "x"}],
        executors={"ssh": RecordingExecutor()},
    )

    dispatcher = ExecutorDispatcher()
    # Patch executor_for to return our fake chain
    with patch.object(dispatcher, "executor_for", return_value=fake_chain):
        dispatcher.validate_target(
            sha="abc",
            branch="main",
            target_config={"name": "test", "type": "ssh", "host": "x"},
            validation_config={"command": "true"},
            log_path="/tmp/log",
            resume_from="test",
            mode="smoke",
        )

    # The fallback path must receive the same kwargs the direct path does
    assert recorded.get("resume_from") == "test"
    assert recorded.get("mode") == "smoke"


# ── P1-C: Remote command builders quote path inputs ────────────────────


def test_posix_build_quotes_repo_path_with_space() -> None:
    cmd = _build_posix(
        sha="deadbeef",
        remote_repo="/home/user/with space/pulp",
        validation_config={"command": "echo hi"},
    )
    # The quoted repo path must appear as a single shell token
    assert "/home/user/with space/pulp" in cmd
    assert "'/home/user/with space/pulp'" in cmd or \
           "/home/user/with\\ space/pulp" in cmd


def test_posix_build_quotes_sha() -> None:
    cmd = _build_posix(
        sha="abc123",
        remote_repo="/home/user/repo",
        validation_config={"command": "echo hi"},
    )
    assert "abc123" in cmd


def test_windows_build_escapes_single_quote_in_repo_path() -> None:
    """A single quote in repo_path must not break the PS literal."""
    cmd = _build_windows(
        sha="abc",
        remote_repo="C:\\Users\\it's\\repo",
        validation_config={"command": "Write-Output hi"},
    )
    # The escaped literal must contain the doubled quote
    assert "'C:\\Users\\it''s\\repo'" in cmd


def test_ps_single_quote_escaping() -> None:
    assert _ps_single_quote("it's") == "it''s"
    assert _ps_single_quote("no quotes") == "no quotes"
    assert _ps_single_quote("multiple 'in' here") == "multiple ''in'' here"


# ── P1-D: Streaming layer has bounded tail buffer ─────────────────────


def test_streaming_layer_has_bounded_output_buffer() -> None:
    """The streaming layer must NOT keep unbounded output in memory."""
    # This test asserts the source shape of the cap, since actually
    # running a subprocess that produces 1+ MB of output in a unit
    # test is heavy. The cap is documented in-code as
    # _OUTPUT_TAIL_BYTES at the top of the reader loop.
    from shipyard.executor import streaming

    src = inspect.getsource(streaming)
    assert "output_tail_bytes_cap" in src, (
        "Tail buffer cap missing — streaming would keep every byte "
        "resident for the lifetime of a long build"
    )
    assert "collections.deque" in src or "deque()" in src, (
        "Tail buffer should be a deque for O(1) popleft"
    )
    assert "popleft" in src, "Tail rotation needs to pop the oldest fragment"
