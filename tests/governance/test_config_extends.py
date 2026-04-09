"""Regression tests for nested `extends` chain resolution.

Codex flagged that the original `_resolve_overrides_for` copied
only the direct parent's override map without recursing into the
parent's own `extends`, so multi-level inheritance was truncated.
Example: `release/** → develop/** → main` would drop `main` fields
when resolving `release/*`.
"""

from __future__ import annotations

from shipyard.core.config import Config
from shipyard.governance.config import (
    load_governance_config,
    resolve_branch_rules,
)


def _config(data: dict) -> Config:
    return Config(data=data)


def test_three_level_extends_chain_inherits_all_ancestors() -> None:
    """release/** extends develop/** which extends main — all three must flow."""
    cfg = _config({
        "project": {"profile": "solo"},
        "branch_protection": {
            "main": {
                "enforce_admins": True,                # grandparent field
            },
            "develop/**": {
                "extends": "main",
                "dismiss_stale_reviews": True,         # parent field
            },
            "release/**": {
                "extends": "develop/**",
                "require_review_count": 2,             # child field
            },
        },
    })
    gov = load_governance_config(cfg)
    rules = resolve_branch_rules(gov, "release/v1.0")
    # All three ancestors contribute
    assert rules.enforce_admins is True             # from main
    assert rules.dismiss_stale_reviews is True      # from develop/**
    assert rules.require_review_count == 2          # from release/**


def test_child_field_overrides_parent() -> None:
    """The child's own override wins over an ancestor's."""
    cfg = _config({
        "branch_protection": {
            "main": {"require_review_count": 1},
            "develop/**": {
                "extends": "main",
                "require_review_count": 3,  # wins
            },
        },
    })
    gov = load_governance_config(cfg)
    rules = resolve_branch_rules(gov, "develop/auth")
    assert rules.require_review_count == 3


def test_extends_cycle_is_broken_not_infinite() -> None:
    """A self-referential or cyclic extends must not infinite-loop."""
    cfg = _config({
        "branch_protection": {
            "a/**": {"extends": "b/**", "enforce_admins": True},
            "b/**": {"extends": "a/**", "dismiss_stale_reviews": True},
        },
    })
    gov = load_governance_config(cfg)
    # Should terminate and not raise. The exact result of a cycle is
    # not prescribed beyond "terminates and resolves as much as
    # possible", but these two assertions guarantee the child's own
    # field survives.
    rules = resolve_branch_rules(gov, "a/feature")
    assert rules.enforce_admins is True


def test_extends_pointing_at_unknown_glob_is_ignored() -> None:
    """`extends = "nonexistent"` is a no-op, not an error."""
    cfg = _config({
        "branch_protection": {
            "main": {"extends": "ghost", "enforce_admins": True},
        },
    })
    gov = load_governance_config(cfg)
    rules = resolve_branch_rules(gov, "main")
    assert rules.enforce_admins is True  # own field still applies
