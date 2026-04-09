"""Regression test: VS toolchain cache must key on (host, ssh_options)."""

from __future__ import annotations

from unittest.mock import patch

from shipyard.executor.ssh_windows import SSHWindowsExecutor
from shipyard.executor.windows_toolchain import VsToolchain


def _arm_toolchain() -> VsToolchain:
    return VsToolchain(
        cmake_platform="ARM64",
        cmake_generator_instance="C:/VS/ARM64",
    )


def _x64_toolchain() -> VsToolchain:
    return VsToolchain(
        cmake_platform="x64",
        cmake_generator_instance="C:/VS/x64",
    )


def test_same_host_different_ssh_options_are_cached_separately() -> None:
    """Two targets on the same hostname with different SSH options don't share cache.

    Codex flagged that keying by host alone lets the first detected
    toolchain pollute subsequent runs against a completely different
    endpoint that happens to share the hostname (e.g., different
    users, ports, or jump hosts).
    """
    executor = SSHWindowsExecutor()

    calls = []

    def fake_detect(host, ssh_options, *, timeout=60):
        calls.append((host, tuple(ssh_options)))
        if "-p2222" in ssh_options:
            return _x64_toolchain()
        return _arm_toolchain()

    with patch(
        "shipyard.executor.ssh_windows.detect_vs_toolchain",
        side_effect=fake_detect,
    ):
        # Two targets with the same hostname but different port
        t1 = executor._get_vs_toolchain(
            host="build.example.com",
            ssh_options=["-p1111"],
            target_config={"windows_vs_detect": True},
        )
        t2 = executor._get_vs_toolchain(
            host="build.example.com",
            ssh_options=["-p2222"],
            target_config={"windows_vs_detect": True},
        )

    assert t1 is not None and t1.cmake_platform == "ARM64"
    assert t2 is not None and t2.cmake_platform == "x64"
    assert len(calls) == 2  # Both probed; no cache collision


def test_same_host_same_ssh_options_is_cached() -> None:
    """Within the same (host, options) tuple, the cache still deduplicates."""
    executor = SSHWindowsExecutor()

    calls = []

    def fake_detect(host, ssh_options, *, timeout=60):
        calls.append(host)
        return _arm_toolchain()

    with patch(
        "shipyard.executor.ssh_windows.detect_vs_toolchain",
        side_effect=fake_detect,
    ):
        for _ in range(3):
            executor._get_vs_toolchain(
                host="build.example.com",
                ssh_options=["-p1111"],
                target_config={"windows_vs_detect": True},
            )
    assert len(calls) == 1
