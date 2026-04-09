"""Regression tests for Codex review findings on PRs #3, #5, #8, #10.

Each test pins a specific bug the Codex connector flagged during
review so the fix doesn't silently regress.
"""

from __future__ import annotations

from unittest.mock import patch

from shipyard.cli import _check_governance_drift, _resolve_validation
from shipyard.core.config import Config
from shipyard.core.job import ValidationMode

# ── PR #3: contract config must be read from top-level validation ─────


def test_resolve_validation_lifts_contract_from_top_level() -> None:
    """`[validation.contract]` must survive into the resolved mode dict.

    Codex flagged that the executor reads `validation_config.get("contract")`
    on the already-resolved `default`/`smoke` subtable, so a project
    using the documented `[validation.contract]` layout would have
    contract enforcement silently skipped.
    """
    config = Config(data={
        "validation": {
            "default": {"command": "pytest"},
            "contract": {"markers": ["__PULP_VALIDATION__:smoke"]},
        },
    })
    resolved = _resolve_validation(config, ValidationMode.FULL)
    assert resolved["command"] == "pytest"
    assert resolved["contract"] == {"markers": ["__PULP_VALIDATION__:smoke"]}


def test_resolve_validation_lifts_prepared_state_from_top_level() -> None:
    """Same fix covers `[validation.prepared_state]`."""
    config = Config(data={
        "validation": {
            "default": {"command": "pytest"},
            "prepared_state": {"enabled": True},
        },
    })
    resolved = _resolve_validation(config, ValidationMode.FULL)
    assert resolved["prepared_state"] == {"enabled": True}


def test_resolve_validation_mode_override_beats_top_level() -> None:
    """An explicit contract inside `default` overrides the top-level one."""
    config = Config(data={
        "validation": {
            "default": {
                "command": "pytest",
                "contract": {"markers": ["inner"]},
            },
            "contract": {"markers": ["outer"]},
        },
    })
    resolved = _resolve_validation(config, ValidationMode.FULL)
    assert resolved["contract"] == {"markers": ["inner"]}


def test_resolve_validation_smoke_mode_inherits_top_level() -> None:
    """Smoke mode also lifts top-level peers."""
    config = Config(data={
        "validation": {
            "smoke": {"command": "pytest -k smoke"},
            "contract": {"markers": ["smoke"]},
            "prepared_state": {"enabled": True},
        },
    })
    resolved = _resolve_validation(config, ValidationMode.SMOKE)
    assert resolved["command"] == "pytest -k smoke"
    assert resolved["contract"] == {"markers": ["smoke"]}
    assert resolved["prepared_state"] == {"enabled": True}


# ── PR #8: doctor must NOT report "aligned" on fetch errors ────────────


def test_doctor_governance_reports_fetch_error_not_aligned() -> None:
    """When build_status returns errors but no drift, doctor must flag it.

    Codex flagged that `has_drift` is False when every fetch failed,
    so doctor currently emits "aligned with <profile>" even though
    the live state was never actually read. Must treat
    `status.has_errors` as a not-aligned path.
    """
    config = Config(data={
        "project": {"profile": "solo"},
        "governance": {"required_status_checks": ["mac"]},
    })

    from shipyard.governance.github import RepoRef
    from shipyard.governance.status import GovernanceStatus

    fake_status = GovernanceStatus(
        repo=RepoRef("me", "r"),
        profile_name="solo",
        reports=(),
        errors=("main: permission denied",),
    )

    with patch(
        "shipyard.governance.detect_repo_from_remote",
        return_value=RepoRef("me", "r"),
    ), patch(
        "shipyard.governance.build_status",
        return_value=fake_status,
    ):
        section = _check_governance_drift(config)

    assert section is not None
    assert section["main"]["ok"] is False
    assert "could not read live state" in section["main"]["detail"]
    assert "permission denied" in section["main"]["detail"]


def test_doctor_governance_reports_aligned_on_clean_no_drift() -> None:
    """Sanity check — when there are no errors and no drift, aligned is OK."""
    config = Config(data={
        "project": {"profile": "solo"},
        "governance": {"required_status_checks": ["mac"]},
    })

    from shipyard.governance.github import RepoRef
    from shipyard.governance.status import GovernanceStatus

    fake_status = GovernanceStatus(
        repo=RepoRef("me", "r"),
        profile_name="solo",
        reports=(),
        errors=(),
    )

    with patch(
        "shipyard.governance.detect_repo_from_remote",
        return_value=RepoRef("me", "r"),
    ), patch(
        "shipyard.governance.build_status",
        return_value=fake_status,
    ):
        section = _check_governance_drift(config)

    assert section is not None
    assert section["main"]["ok"] is True
    assert "aligned with solo profile" in section["main"]["detail"]
