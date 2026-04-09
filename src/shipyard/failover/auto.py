"""Auto-inject a cloud fallback for unreachable SSH targets.

Pulp's local_ci.py implements this via its `[failover] namespace_auto`
flag: when the default cloud provider is `namespace` and
`namespace_auto = true`, every SSH target that would otherwise be
marked unreachable gets an implicit failover to Namespace before the
run is rejected. This lets a solo developer keep their Windows VM
shut down most of the time and still ship PRs — Shipyard dispatches
the unreachable target to the cloud automatically instead of
erroring out.

Shipyard already has a `FallbackChain` that walks through a
per-target `fallback = [...]` list in order, probing each backend and
skipping the ones that fail. This module just materializes the
"implicit cloud fallback" that users would otherwise have to write
by hand on every SSH target.

Config schema::

    [failover.cloud_auto]
    enabled = true           # opt-in
    provider = "namespace"   # one of the cloud provider names
    workflow = "ci.yml"      # workflow file to dispatch
    repository = "owner/repo"  # optional; defaults to origin

When enabled, this helper walks `config.targets` and, for each SSH or
ssh-windows target that has no explicit `fallback` entry, appends a
synthetic cloud fallback. Explicit fallbacks are left alone — users
who know what they want stay in control.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from shipyard.core.config import Config


_SSH_BACKENDS = frozenset({"ssh", "ssh-windows", "ssh_windows"})


def auto_cloud_fallback_config(config: Config) -> dict[str, Any] | None:
    """Return the `[failover.cloud_auto]` section when enabled, else None.

    Tolerates missing sections and typos gracefully — if the config
    isn't opted in, this returns None and callers skip injection.
    """
    section = config.get("failover.cloud_auto")
    if not isinstance(section, dict):
        return None
    if not section.get("enabled", False):
        return None
    return section


def build_cloud_fallback_entry(
    auto_config: dict[str, Any],
    *,
    target_config: dict[str, Any],
) -> dict[str, Any]:
    """Materialize the cloud fallback dict to append to a target's chain.

    Inherits `platform`, `runner_provider`, and `runner_selector`
    from the auto-config, but lets the target override them by having
    `cloud_runner_provider` etc. already set in the target config.
    """
    provider = auto_config.get("provider", "namespace")
    workflow = auto_config.get("workflow", "ci.yml")
    repository = auto_config.get("repository")

    entry: dict[str, Any] = {
        "type": "cloud",
        "workflow": workflow,
        "runner_provider": target_config.get(
            "cloud_runner_provider", provider,
        ),
    }
    if repository:
        entry["repository"] = repository
    if "cloud_runner_selector" in target_config:
        entry["runner_selector"] = target_config["cloud_runner_selector"]
    elif "runner_selector" in auto_config:
        entry["runner_selector"] = auto_config["runner_selector"]
    return entry


def apply_auto_cloud_fallback(config: Config) -> list[str]:
    """Mutate the config in place, appending cloud fallbacks where missing.

    Returns the list of target names that got a fallback injected.
    Targets that already have a `fallback = [...]` are left alone —
    users who declare their own chain stay in control of the order.
    A target is considered SSH if its `type`/`backend` is `ssh` or
    `ssh-windows`.
    """
    auto_config = auto_cloud_fallback_config(config)
    if auto_config is None:
        return []

    targets = config.data.get("targets")
    if not isinstance(targets, dict):
        return []

    injected: list[str] = []
    for name, target_config in targets.items():
        if not isinstance(target_config, dict):
            continue
        backend = _normalize_backend(target_config)
        if backend not in _SSH_BACKENDS:
            continue
        if target_config.get("fallback"):
            continue
        entry = build_cloud_fallback_entry(
            auto_config, target_config=target_config,
        )
        target_config["fallback"] = [entry]
        injected.append(name)
    return injected


def _normalize_backend(target_config: dict[str, Any]) -> str:
    backend = str(
        target_config.get("type")
        or target_config.get("backend")
        or "local"
    ).strip().lower().replace("_", "-")
    if backend == "ssh" and str(
        target_config.get("platform", "")
    ).startswith("windows"):
        return "ssh-windows"
    return backend
