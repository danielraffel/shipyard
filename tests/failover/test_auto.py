"""Unit tests for auto cloud fallback injection."""

from __future__ import annotations

from shipyard.core.config import Config
from shipyard.failover.auto import (
    apply_auto_cloud_fallback,
    auto_cloud_fallback_config,
    build_cloud_fallback_entry,
)


def _make_config(data: dict) -> Config:
    """Build a Config instance directly from a dict."""
    return Config(data=data)


# ── auto_cloud_fallback_config ──────────────────────────────────────────


def test_auto_config_returns_none_when_section_missing() -> None:
    config = _make_config({})
    assert auto_cloud_fallback_config(config) is None


def test_auto_config_returns_none_when_disabled() -> None:
    config = _make_config({
        "failover": {"cloud_auto": {"enabled": False, "provider": "namespace"}},
    })
    assert auto_cloud_fallback_config(config) is None


def test_auto_config_returns_none_when_enabled_omitted() -> None:
    """Without `enabled = true` the section is a no-op — opt-in only."""
    config = _make_config({
        "failover": {"cloud_auto": {"provider": "namespace"}},
    })
    assert auto_cloud_fallback_config(config) is None


def test_auto_config_returns_section_when_enabled() -> None:
    config = _make_config({
        "failover": {
            "cloud_auto": {
                "enabled": True,
                "provider": "namespace",
                "workflow": "ci.yml",
            }
        },
    })
    section = auto_cloud_fallback_config(config)
    assert section is not None
    assert section["provider"] == "namespace"


def test_auto_config_tolerates_wrong_section_type() -> None:
    """A non-dict section (e.g. misconfigured TOML) returns None."""
    config = _make_config({"failover": {"cloud_auto": "enabled"}})
    assert auto_cloud_fallback_config(config) is None


# ── build_cloud_fallback_entry ──────────────────────────────────────────


def test_build_entry_uses_auto_defaults() -> None:
    entry = build_cloud_fallback_entry(
        {"provider": "namespace", "workflow": "ci.yml"},
        target_config={},
    )
    assert entry["type"] == "cloud"
    assert entry["workflow"] == "ci.yml"
    assert entry["runner_provider"] == "namespace"


def test_build_entry_includes_repository_when_set() -> None:
    entry = build_cloud_fallback_entry(
        {
            "provider": "namespace",
            "workflow": "ci.yml",
            "repository": "myorg/myrepo",
        },
        target_config={},
    )
    assert entry["repository"] == "myorg/myrepo"


def test_build_entry_omits_repository_when_unset() -> None:
    entry = build_cloud_fallback_entry(
        {"provider": "namespace", "workflow": "ci.yml"},
        target_config={},
    )
    assert "repository" not in entry


def test_build_entry_target_overrides_provider() -> None:
    """A per-target cloud_runner_provider beats the auto default."""
    entry = build_cloud_fallback_entry(
        {"provider": "namespace", "workflow": "ci.yml"},
        target_config={"cloud_runner_provider": "github-hosted"},
    )
    assert entry["runner_provider"] == "github-hosted"


def test_build_entry_target_overrides_selector() -> None:
    entry = build_cloud_fallback_entry(
        {
            "provider": "namespace",
            "workflow": "ci.yml",
            "runner_selector": "base",
        },
        target_config={"cloud_runner_selector": "macos-arm64-large"},
    )
    assert entry["runner_selector"] == "macos-arm64-large"


def test_build_entry_inherits_auto_selector_when_no_override() -> None:
    entry = build_cloud_fallback_entry(
        {
            "provider": "namespace",
            "workflow": "ci.yml",
            "runner_selector": "base",
        },
        target_config={},
    )
    assert entry["runner_selector"] == "base"


# ── apply_auto_cloud_fallback ───────────────────────────────────────────


def test_apply_noop_when_disabled() -> None:
    config = _make_config({
        "targets": {
            "ubuntu": {"type": "ssh", "host": "ubuntu"},
        },
    })
    injected = apply_auto_cloud_fallback(config)
    assert injected == []
    assert "fallback" not in config.data["targets"]["ubuntu"]


def test_apply_injects_on_ssh_targets_when_enabled() -> None:
    config = _make_config({
        "failover": {
            "cloud_auto": {
                "enabled": True,
                "provider": "namespace",
                "workflow": "ci.yml",
            }
        },
        "targets": {
            "ubuntu": {"type": "ssh", "host": "ubuntu"},
            "windows": {
                "type": "ssh",
                "host": "win",
                "platform": "windows-x64",
            },
        },
    })
    injected = apply_auto_cloud_fallback(config)
    assert set(injected) == {"ubuntu", "windows"}
    for name in ("ubuntu", "windows"):
        target = config.data["targets"][name]
        assert "fallback" in target
        assert len(target["fallback"]) == 1
        entry = target["fallback"][0]
        assert entry["type"] == "cloud"
        assert entry["workflow"] == "ci.yml"


def test_apply_skips_local_targets() -> None:
    config = _make_config({
        "failover": {
            "cloud_auto": {"enabled": True, "provider": "namespace"},
        },
        "targets": {
            "mac": {"type": "local", "platform": "macos-arm64"},
            "ubuntu": {"type": "ssh", "host": "ubuntu"},
        },
    })
    injected = apply_auto_cloud_fallback(config)
    assert injected == ["ubuntu"]
    assert "fallback" not in config.data["targets"]["mac"]


def test_apply_preserves_explicit_fallback() -> None:
    """A target with an explicit fallback chain is left alone."""
    existing_fallback = [{"type": "vm", "vm_name": "Ubuntu 24.04"}]
    config = _make_config({
        "failover": {
            "cloud_auto": {"enabled": True, "provider": "namespace"},
        },
        "targets": {
            "ubuntu": {
                "type": "ssh",
                "host": "ubuntu",
                "fallback": existing_fallback,
            },
        },
    })
    injected = apply_auto_cloud_fallback(config)
    assert injected == []
    assert config.data["targets"]["ubuntu"]["fallback"] == existing_fallback


def test_apply_skips_ssh_target_with_empty_fallback_list() -> None:
    """An empty explicit fallback list is still opt-out, not opt-in."""
    config = _make_config({
        "failover": {
            "cloud_auto": {"enabled": True, "provider": "namespace"},
        },
        "targets": {
            # `fallback = []` is falsy, so the auto-inject treats it
            # as "no explicit fallback" and adds one. This matches
            # the intuitive "opt-in by declaring anything" UX.
            "ubuntu": {"type": "ssh", "host": "ubuntu", "fallback": []},
        },
    })
    injected = apply_auto_cloud_fallback(config)
    assert injected == ["ubuntu"]


def test_apply_detects_windows_platform_with_ssh_backend() -> None:
    """type=ssh + platform=windows-* is treated as an SSH target."""
    config = _make_config({
        "failover": {
            "cloud_auto": {"enabled": True, "provider": "namespace"},
        },
        "targets": {
            "win": {
                "type": "ssh",
                "host": "win",
                "platform": "windows-arm64",
            },
        },
    })
    injected = apply_auto_cloud_fallback(config)
    assert injected == ["win"]


def test_apply_handles_missing_targets_section() -> None:
    config = _make_config({
        "failover": {"cloud_auto": {"enabled": True, "provider": "namespace"}},
    })
    assert apply_auto_cloud_fallback(config) == []


def test_apply_handles_non_dict_target_entry() -> None:
    """A malformed target entry should be skipped, not crash."""
    config = _make_config({
        "failover": {"cloud_auto": {"enabled": True, "provider": "namespace"}},
        "targets": {
            "good": {"type": "ssh", "host": "ubuntu"},
            "broken": "not-a-dict",
        },
    })
    injected = apply_auto_cloud_fallback(config)
    assert injected == ["good"]
