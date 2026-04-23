"""Daemon never dies from tunnel trouble.

These tests lock in the post-refactor contract:

1. IPC server comes up even when the tunnel probe fails at startup.
2. Tunnel supervisor retries forever with capped backoff until stop.
3. Mid-session tunnel loss (verify() → False) triggers re-establish
   without touching the IPC server.

Driven through the `Daemon` class directly with a fake tunnel
backend — no real tailscaled / webhook server / gh involved.
"""

from __future__ import annotations

import asyncio
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path
from unittest.mock import patch

import pytest

from shipyard.daemon.controller import Daemon, DaemonConfig
from shipyard.daemon.tunnels.base import (
    TunnelInfo,
    TunnelNotReadyError,
)

pytestmark = pytest.mark.skipif(
    sys.platform == "win32",
    reason="daemon uses AF_UNIX sockets; macOS/Linux only",
)


@dataclass
class _FakeTunnelState:
    """Mutable script driving the fake tunnel's behavior per-call.

    Tests set the `plan` list ahead of time: each entry is either
    "fail" (raise TunnelNotReadyError), "ok" (return a TunnelInfo),
    and subsequent `verify` calls consult `verify_plan` similarly.
    """

    plan: list[str]
    start_calls: int = 0
    verify_plan: list[bool] | None = None
    verify_calls: int = 0


class _FakeTunnel:
    name = "tailscale"

    def __init__(self, state: _FakeTunnelState) -> None:
        self.state = state

    async def start(self, local_port: int) -> TunnelInfo:
        self.state.start_calls += 1
        i = self.state.start_calls - 1
        # After the plan is exhausted, repeat the last step so tests
        # that want "forever failing" can pass plan=["fail"] and tests
        # that want "always healthy" can pass plan=["ok"].
        step = self.state.plan[i] if i < len(self.state.plan) else self.state.plan[-1]
        if step == "fail":
            raise TunnelNotReadyError("simulated: not ready")
        return TunnelInfo(public_url="https://fake.ts.net", backend=self.name)

    async def stop(self) -> None:
        return None

    async def verify(self, local_port: int) -> bool:
        self.state.verify_calls += 1
        if self.state.verify_plan is None:
            return True
        i = self.state.verify_calls - 1
        if i >= len(self.state.verify_plan):
            return True
        return self.state.verify_plan[i]


class _FakeWebhookServer:
    def __init__(self, _handler):
        pass

    def start(self) -> int:
        return 12345  # arbitrary port

    def stop(self) -> None:
        return None


class _FakeRegistrar:
    def __init__(self, _state_dir):
        self.calls: list[tuple[str, str]] = []

    def all(self) -> dict[str, int]:
        return {}

    async def ensure_registered(
        self, repo: str, url: str, secret: str, **kw
    ) -> int:
        self.calls.append((repo, url))
        return 1

    async def unregister_all(self, **kw) -> None:
        return None


def _make_daemon(tmp: Path, tunnel_state: _FakeTunnelState) -> Daemon:
    cfg = DaemonConfig(state_dir=tmp, repos=["owner/repo"])
    daemon = Daemon(cfg)
    daemon._tunnel = _FakeTunnel(tunnel_state)  # type: ignore[assignment]
    daemon._registrar = _FakeRegistrar(tmp)  # type: ignore[assignment]
    # Shorten backoffs to keep tests fast. The first attempt is
    # immediate regardless; we just need the retry sleeps to be 0.
    daemon._TUNNEL_RETRY_BACKOFFS = (0.0, 0.0, 0.0)  # type: ignore[assignment]
    daemon._TUNNEL_VERIFY_INTERVAL_SECS = 0.05  # type: ignore[assignment]
    return daemon


def _patch_webhook_server():
    """Patch WebhookServer in the controller module so start() doesn't
    bind a real TCP listener."""
    return patch(
        "shipyard.daemon.controller.WebhookServer",
        _FakeWebhookServer,
    )


def test_ipc_comes_up_even_when_tunnel_probe_keeps_failing() -> None:
    """Startup invariant. Tunnel probe fails on the first call — daemon
    must still have its IPC socket listening so subscribers can
    connect, rather than exiting like the old behavior."""

    async def run() -> None:
        with tempfile.TemporaryDirectory(prefix="sy-supv-") as tmp:
            # Infinite "fail" via single-entry plan (last step
            # repeats forever once exhausted).
            daemon = _make_daemon(
                Path(tmp),
                _FakeTunnelState(plan=["fail"]),
            )
            # Non-zero backoff to ensure the supervisor doesn't keep
            # sending retries while our assertions run.
            daemon._TUNNEL_RETRY_BACKOFFS = (0.5,)  # type: ignore[assignment]
            with _patch_webhook_server():
                await daemon.start()
                try:
                    # Give the supervisor a few ticks to attempt retries.
                    await asyncio.sleep(0.1)
                    # IPC socket is bound + accepting, despite tunnel
                    # not being up.
                    sock_path = Path(tmp) / "daemon" / "daemon.sock"
                    assert sock_path.exists(), (
                        "IPC socket must be listening even without tunnel"
                    )
                    # Status reflects tunnel-not-ready truth.
                    state = daemon._build_status_snapshot()
                    assert state.tunnel_url is None
                    assert state.tunnel_verified_at is None
                finally:
                    await daemon.stop()

    asyncio.run(run())


def test_transient_tunnel_bring_up_eventually_succeeds() -> None:
    """Classic recovery: N failures, then success. Webhooks register
    exactly once after the tunnel lands."""

    async def run() -> None:
        with tempfile.TemporaryDirectory(prefix="sy-supv-") as tmp:
            ts = _FakeTunnelState(plan=["fail", "fail", "ok"])
            daemon = _make_daemon(Path(tmp), ts)
            with _patch_webhook_server():
                await daemon.start()
                try:
                    # Poll for the tunnel state to settle. With 0s
                    # backoff this should happen within a handful of
                    # event-loop ticks.
                    for _ in range(200):
                        if daemon._state.tunnel is not None:
                            break
                        await asyncio.sleep(0.01)
                    assert daemon._state.tunnel is not None
                    assert daemon._state.tunnel.public_url == "https://fake.ts.net"
                    assert ts.start_calls == 3
                    # Registrar fired once for the configured repo.
                    reg = daemon._registrar  # type: ignore[attr-defined]
                    assert reg.calls == [
                        ("owner/repo", "https://fake.ts.net/webhook"),
                    ]
                finally:
                    await daemon.stop()

    asyncio.run(run())


def test_tunnel_loss_mid_session_re_establishes() -> None:
    """Up → verify fails → down → re-establish. The daemon must
    not exit during the transition."""

    async def run() -> None:
        with tempfile.TemporaryDirectory(prefix="sy-supv-") as tmp:
            ts = _FakeTunnelState(
                plan=["ok", "ok"],
                verify_plan=[True, False],  # healthy once, then lost
            )
            daemon = _make_daemon(Path(tmp), ts)
            with _patch_webhook_server():
                await daemon.start()
                try:
                    # Wait for first bring-up.
                    for _ in range(200):
                        if daemon._state.tunnel is not None:
                            break
                        await asyncio.sleep(0.01)
                    assert daemon._state.tunnel is not None
                    first_url = daemon._state.tunnel.public_url
                    # Wait for re-establish after verify False.
                    for _ in range(400):
                        if ts.start_calls >= 2:
                            break
                        await asyncio.sleep(0.01)
                    assert ts.start_calls >= 2, (
                        "supervisor should re-establish after verify()=False"
                    )
                    # Tunnel state is back up.
                    assert daemon._state.tunnel is not None
                    assert daemon._state.tunnel.public_url == first_url
                finally:
                    await daemon.stop()

    asyncio.run(run())
