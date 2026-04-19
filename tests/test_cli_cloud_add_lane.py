"""Tests for `shipyard cloud add-lane`.

Symmetric to `test_cli_cloud_retarget.py`: pure helpers are
unit-tested; the CLI end-to-end flow is mocked out — no real gh
calls. Covers the acceptance criteria from issue #86:

- Dry-run by default, --apply executes.
- Refuses if the ship is already past dispatch phase.
- Idempotent: re-running with the same target is a no-op.
- Appends a DispatchedRun on apply; state persists.
- Refuses when no ShipState exists.
"""

from __future__ import annotations

import json as _json
from datetime import datetime, timezone
from typing import TYPE_CHECKING, Any

from click.testing import CliRunner

from shipyard.cli import main
from shipyard.core.ship_state import (
    DispatchedRun,
    ShipState,
    ShipStateStore,
)

if TYPE_CHECKING:
    from pathlib import Path

    import pytest


def _state(
    pr: int = 42,
    evidence: dict[str, str] | None = None,
    dispatched: list[DispatchedRun] | None = None,
) -> ShipState:
    now = datetime.now(timezone.utc)
    return ShipState(
        pr=pr,
        repo="owner/repo",
        branch="feature/x",
        base_branch="main",
        head_sha="a" * 40,
        policy_signature="p1",
        evidence_snapshot=evidence or {},
        dispatched_runs=dispatched or [],
        created_at=now,
        updated_at=now,
    )


def _runner_with_store(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> tuple[CliRunner, ShipStateStore]:
    store = ShipStateStore(path=tmp_path / "ship")
    monkeypatch.setattr(
        "shipyard.cli.Context.ship_state",
        property(lambda self: store),
    )
    return CliRunner(), store


def _patch_dispatch_flow(
    monkeypatch: pytest.MonkeyPatch,
    *,
    dispatch_repo: str = "owner/repo",
    provider: str = "namespace",
) -> dict[str, Any]:
    """Common monkeypatches for the dispatch side.

    Returns a dict the caller can inspect after the CLI invocation
    to assert on what would have been dispatched.
    """
    from types import SimpleNamespace

    captured: dict[str, Any] = {"dispatched_with": None, "discovered": 0}

    monkeypatch.setattr(
        "shipyard.cli.discover_workflows",
        lambda: {"build": SimpleNamespace(file="build.yml", key="build")},
    )
    monkeypatch.setattr(
        "shipyard.cli.default_workflow_key",
        lambda cfg, workflows: "build",
    )

    fake_plan = SimpleNamespace(
        repository=dispatch_repo,
        ref="feature/x",
        workflow=SimpleNamespace(
            key="build", file="build.yml", name="Build"
        ),
        provider=provider,
        dispatch_fields={"runner_provider": provider},
        to_dict=lambda: {"provider": provider},
    )
    monkeypatch.setattr(
        "shipyard.cli.resolve_cloud_dispatch_plan",
        lambda **kw: fake_plan,
    )

    def fake_dispatch(**kw: Any) -> None:
        captured["dispatched_with"] = kw

    monkeypatch.setattr("shipyard.cli.workflow_dispatch", fake_dispatch)

    def fake_discover(**kw: Any) -> dict[str, Any]:
        captured["discovered"] += 1
        return {"databaseId": 987654, "url": "https://gh/run/987654"}

    monkeypatch.setattr("shipyard.cli.find_dispatched_run", fake_discover)
    return captured


class TestAddLaneMissingState:
    def test_no_state_exits_1(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        runner, _ = _runner_with_store(tmp_path, monkeypatch)
        result = runner.invoke(
            main,
            [
                "cloud", "add-lane",
                "--pr", "999",
                "--target", "windows",
            ],
        )
        assert result.exit_code == 1
        assert "no in-flight ship state" in result.output.lower()


class TestAddLanePastDispatch:
    def test_terminal_pass_refused(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        runner, store = _runner_with_store(tmp_path, monkeypatch)
        # All targets terminal → ship is past dispatch.
        store.save(_state(
            pr=10,
            evidence={"macos": "pass", "linux": "pass"},
        ))
        result = runner.invoke(
            main,
            [
                "cloud", "add-lane",
                "--pr", "10",
                "--target", "windows",
            ],
        )
        assert result.exit_code == 1
        assert "past dispatch phase" in result.output.lower()

    def test_terminal_fail_refused(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        runner, store = _runner_with_store(tmp_path, monkeypatch)
        store.save(_state(
            pr=10,
            evidence={"macos": "pass", "linux": "fail"},
        ))
        result = runner.invoke(
            main,
            [
                "cloud", "add-lane",
                "--pr", "10",
                "--target", "windows",
            ],
        )
        assert result.exit_code == 1
        assert "past dispatch phase" in result.output.lower()

    def test_in_flight_proceeds(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        runner, store = _runner_with_store(tmp_path, monkeypatch)
        # Mixed terminal + pending → still in flight. Should plan.
        store.save(_state(
            pr=10,
            evidence={"macos": "pass", "linux": "pending"},
        ))
        _patch_dispatch_flow(monkeypatch)
        result = runner.invoke(
            main,
            [
                "cloud", "add-lane",
                "--pr", "10",
                "--target", "windows",
            ],
        )
        assert result.exit_code == 0, result.output
        assert "Dry-run" in result.output


class TestAddLaneIdempotent:
    def _prepopulated_state(self, pr: int = 10) -> ShipState:
        now = datetime.now(timezone.utc)
        return _state(
            pr=pr,
            evidence={"macos": "pending"},
            dispatched=[
                DispatchedRun(
                    target="windows",
                    provider="github-hosted",
                    run_id="111",
                    status="in_progress",
                    started_at=now,
                    updated_at=now,
                )
            ],
        )

    def test_existing_target_noop_human(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        runner, store = _runner_with_store(tmp_path, monkeypatch)
        store.save(self._prepopulated_state())
        captured = _patch_dispatch_flow(monkeypatch)
        result = runner.invoke(
            main,
            [
                "cloud", "add-lane",
                "--pr", "10",
                "--target", "windows",
                "--apply",
            ],
        )
        # Exit 0, no dispatch, state unchanged.
        assert result.exit_code == 0, result.output
        assert captured["dispatched_with"] is None
        assert "already tracked" in result.output.lower()
        reloaded = store.get(10)
        assert reloaded is not None
        assert len(reloaded.dispatched_runs) == 1

    def test_existing_target_noop_json(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        runner, store = _runner_with_store(tmp_path, monkeypatch)
        store.save(self._prepopulated_state())
        _patch_dispatch_flow(monkeypatch)
        result = runner.invoke(
            main,
            [
                "--json", "cloud", "add-lane",
                "--pr", "10",
                "--target", "windows",
            ],
        )
        assert result.exit_code == 0, result.output
        parsed = _json.loads(result.output)
        assert parsed["command"] == "cloud.add-lane"
        assert parsed["event"] == "noop"
        assert parsed["already_tracked"] is True


class TestAddLaneDryRun:
    def test_dry_run_default_no_dispatch(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        runner, store = _runner_with_store(tmp_path, monkeypatch)
        store.save(_state(pr=10, evidence={"macos": "pending"}))
        captured = _patch_dispatch_flow(monkeypatch)
        result = runner.invoke(
            main,
            [
                "cloud", "add-lane",
                "--pr", "10",
                "--target", "windows",
                "--provider", "namespace",
            ],
        )
        assert result.exit_code == 0, result.output
        assert "Dry-run" in result.output
        assert captured["dispatched_with"] is None
        # State unchanged.
        reloaded = store.get(10)
        assert reloaded is not None
        assert reloaded.dispatched_runs == []

    def test_dry_run_json_envelope(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        runner, store = _runner_with_store(tmp_path, monkeypatch)
        store.save(_state(pr=10, evidence={"macos": "pending"}))
        _patch_dispatch_flow(monkeypatch)
        result = runner.invoke(
            main,
            [
                "--json", "cloud", "add-lane",
                "--pr", "10",
                "--target", "windows",
            ],
        )
        assert result.exit_code == 0, result.output
        parsed = _json.loads(result.output)
        assert parsed["command"] == "cloud.add-lane"
        assert parsed["event"] == "plan"
        assert parsed["target"] == "windows"
        assert parsed["dry_run"] is True


class TestAddLaneApply:
    def test_apply_dispatches_and_appends(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        runner, store = _runner_with_store(tmp_path, monkeypatch)
        store.save(_state(pr=10, evidence={"macos": "pending"}))
        captured = _patch_dispatch_flow(monkeypatch)
        result = runner.invoke(
            main,
            [
                "cloud", "add-lane",
                "--pr", "10",
                "--target", "windows",
                "--provider", "namespace",
                "--apply",
            ],
        )
        assert result.exit_code == 0, result.output
        assert captured["dispatched_with"] is not None
        assert captured["dispatched_with"]["workflow_file"] == "build.yml"
        assert captured["dispatched_with"]["ref"] == "feature/x"
        # State updated with a new DispatchedRun.
        reloaded = store.get(10)
        assert reloaded is not None
        assert len(reloaded.dispatched_runs) == 1
        added = reloaded.dispatched_runs[0]
        assert added.target == "windows"
        assert added.provider == "namespace"
        assert added.run_id == "987654"
        assert added.status == "queued"

    def test_apply_json_envelope(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        runner, store = _runner_with_store(tmp_path, monkeypatch)
        store.save(_state(pr=10, evidence={"macos": "pending"}))
        _patch_dispatch_flow(monkeypatch)
        result = runner.invoke(
            main,
            [
                "--json", "cloud", "add-lane",
                "--pr", "10",
                "--target", "windows",
                "--apply",
            ],
        )
        assert result.exit_code == 0, result.output
        parsed = _json.loads(result.output)
        assert parsed["command"] == "cloud.add-lane"
        assert parsed["event"] == "applied"
        assert parsed["target"] == "windows"
        assert parsed["run_id"] == "987654"

    def test_apply_survives_find_dispatched_timeout(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        # If find_dispatched_run times out, the lane should still
        # be recorded with a placeholder run_id — the watch loop
        # can backfill it later.
        runner, store = _runner_with_store(tmp_path, monkeypatch)
        store.save(_state(pr=10, evidence={"macos": "pending"}))
        _patch_dispatch_flow(monkeypatch)

        def boom(**kw: Any) -> dict[str, Any]:
            raise TimeoutError("gh took too long")

        monkeypatch.setattr("shipyard.cli.find_dispatched_run", boom)
        result = runner.invoke(
            main,
            [
                "cloud", "add-lane",
                "--pr", "10",
                "--target", "windows",
                "--apply",
            ],
        )
        assert result.exit_code == 0, result.output
        reloaded = store.get(10)
        assert reloaded is not None
        assert len(reloaded.dispatched_runs) == 1
        assert reloaded.dispatched_runs[0].run_id.startswith("pending-")


class TestShipStateHelpers:
    """Direct unit tests for the ShipState helpers we added."""

    def test_has_target_true_for_present(self) -> None:
        now = datetime.now(timezone.utc)
        state = _state(dispatched=[
            DispatchedRun(
                target="macos", provider="namespace", run_id="1",
                status="in_progress", started_at=now, updated_at=now,
            )
        ])
        assert state.has_target("macos") is True

    def test_has_target_false_for_absent(self) -> None:
        state = _state()
        assert state.has_target("windows") is False

    def test_append_run_appends_and_touches(self) -> None:
        state = _state()
        before = state.updated_at
        now = datetime.now(timezone.utc)
        state.append_run(DispatchedRun(
            target="windows", provider="github-hosted", run_id="9",
            status="queued", started_at=now, updated_at=now,
        ))
        assert len(state.dispatched_runs) == 1
        assert state.dispatched_runs[0].target == "windows"
        assert state.updated_at >= before
