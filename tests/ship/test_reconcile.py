"""Unit tests for the ship-state reconcile logic.

Does not exercise `gh` or any subprocess — the CLI layer is
responsible for shelling out. These tests feed in a statusCheckRollup
dict shape (as `gh pr view --json statusCheckRollup` returns) and
assert the pure reconcile function heals drifted state correctly.
"""

from __future__ import annotations

from datetime import datetime, timezone

from shipyard.core.ship_state import DispatchedRun, ShipState
from shipyard.ship.reconcile import reconcile_ship_state


def _base_state() -> ShipState:
    ts = datetime(2026, 4, 21, 22, 0, tzinfo=timezone.utc)
    return ShipState(
        pr=618,
        repo="danielraffel/pulp",
        branch="chore/pin-bump",
        base_branch="main",
        head_sha="fe1bf4f",
        policy_signature="fc9b712b",
        pr_url="https://github.com/danielraffel/pulp/pull/618",
        pr_title="Bump shipyard pin",
        commit_subject="chore: bump",
        dispatched_runs=[
            DispatchedRun(
                target="mac", provider="local", run_id="sy-1",
                status="completed", started_at=ts, updated_at=ts,
            ),
            DispatchedRun(
                target="ubuntu", provider="ssh", run_id="sy-1",
                status="completed", started_at=ts, updated_at=ts,
            ),
            DispatchedRun(
                target="windows", provider="ssh-windows", run_id="sy-1",
                # stale: actually passed on GH but webhook was missed
                status="failed", started_at=ts, updated_at=ts,
            ),
        ],
    )


def test_stale_failed_target_heals_to_completed_on_success() -> None:
    """The #618 pulp drift bug: windows locally shows failed, GH shows
    pass. Reconcile flips it."""
    state = _base_state()
    rollup = [
        {"name": "Build and Test / mac (pull_request)",
         "state": "COMPLETED", "conclusion": "SUCCESS"},
        {"name": "Build and Test / ubuntu (pull_request)",
         "state": "COMPLETED", "conclusion": "SUCCESS"},
        {"name": "Build and Test / windows (pull_request)",
         "state": "COMPLETED", "conclusion": "SUCCESS"},
    ]
    new_state, changes = reconcile_ship_state(state, rollup)
    statuses = {r.target: r.status for r in new_state.dispatched_runs}
    assert statuses == {
        "mac": "completed",
        "ubuntu": "completed",
        "windows": "completed",
    }
    # The only change should be windows — mac + ubuntu were already correct.
    assert len(changes) == 1
    assert "windows" in changes[0]
    assert "failed" in changes[0]
    assert "completed" in changes[0]


def test_no_matching_check_preserves_old_status() -> None:
    """If GitHub has no check we can match to a target, we DON'T
    overwrite with uncertainty. The target keeps its old status."""
    state = _base_state()
    rollup = [
        {"name": "Totally Unrelated Linter",
         "state": "COMPLETED", "conclusion": "SUCCESS"},
    ]
    new_state, changes = reconcile_ship_state(state, rollup)
    assert changes == []
    assert [r.status for r in new_state.dispatched_runs] == [
        "completed", "completed", "failed",
    ]


def test_in_progress_check_maps_to_in_progress() -> None:
    state = _base_state()
    rollup = [
        {"name": "windows", "state": "IN_PROGRESS", "conclusion": None},
    ]
    new_state, _ = reconcile_ship_state(state, rollup)
    windows = [r for r in new_state.dispatched_runs if r.target == "windows"][0]
    assert windows.status == "in_progress"


def test_failure_conclusion_maps_to_failed() -> None:
    state = _base_state()
    # Flip mac from "completed" to "failed" via reconcile.
    rollup = [
        {"name": "Build and Test / mac (pull_request)",
         "state": "COMPLETED", "conclusion": "FAILURE"},
    ]
    new_state, changes = reconcile_ship_state(state, rollup)
    mac = [r for r in new_state.dispatched_runs if r.target == "mac"][0]
    assert mac.status == "failed"
    assert any("mac" in c for c in changes)


def test_word_boundary_match_not_substring() -> None:
    """The target 'mac' must not spuriously match 'macOS (ARM64)' if
    there's an exact 'mac' check available — but it should still match
    'macOS' as a fallback when nothing more specific exists."""
    state = _base_state()
    rollup = [
        # Both present. Prefer the exact 'mac'.
        {"name": "macOS (ARM64)", "state": "COMPLETED", "conclusion": "FAILURE"},
        {"name": "mac", "state": "COMPLETED", "conclusion": "SUCCESS"},
    ]
    new_state, _ = reconcile_ship_state(state, rollup)
    mac = [r for r in new_state.dispatched_runs if r.target == "mac"][0]
    # The exact-name check wins over the substring one.
    assert mac.status == "completed"


def test_status_unchanged_emits_no_change() -> None:
    state = _base_state()
    rollup = [
        {"name": "mac", "state": "COMPLETED", "conclusion": "SUCCESS"},
    ]
    _, changes = reconcile_ship_state(state, rollup)
    # mac was already 'completed' locally — no change emitted.
    assert changes == []


def test_null_state_with_conclusion_maps_correctly() -> None:
    """GH's statusCheckRollup returns `state: null` for legacy commit
    statuses once they complete. Regression for #618 drift: the
    reconcile ignored these entirely and kept showing stale failures."""
    state = _base_state()
    rollup = [
        # No state field — just conclusion. This is what GH returns
        # for certain check types after completion.
        {"name": "windows", "state": None, "conclusion": "SUCCESS"},
    ]
    new_state, changes = reconcile_ship_state(state, rollup)
    windows = [r for r in new_state.dispatched_runs if r.target == "windows"][0]
    assert windows.status == "completed"
    assert any("windows" in c for c in changes)
