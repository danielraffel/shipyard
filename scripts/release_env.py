#!/usr/bin/env python3
"""Shared release credential environment helpers.

The release scripts require Shipyard-prefixed environment variables.  This
module can optionally inspect a dotenv file and map a narrow set of existing
macOS release aliases into those names for local validation/release runs.
Values are never rendered by these helpers.
"""

from __future__ import annotations

from pathlib import Path
from typing import Mapping


RELEASE_BOT_SECRET = "RELEASE_BOT_TOKEN"
SHIPYARD_SIGNING_ENV = (
    "SHIPYARD_SIGNING_IDENTITY",
    "SHIPYARD_NOTARIZE_APPLE_ID",
    "SHIPYARD_NOTARIZE_TEAM_ID",
    "SHIPYARD_NOTARIZE_APP_PASSWORD",
)
DOTENV_SIGNING_ALIASES: dict[str, tuple[str, ...]] = {
    "SHIPYARD_SIGNING_IDENTITY": ("APP_CERT",),
    "SHIPYARD_NOTARIZE_APPLE_ID": ("APPLE_ID",),
    "SHIPYARD_NOTARIZE_TEAM_ID": ("TEAM_ID",),
    "SHIPYARD_NOTARIZE_APP_PASSWORD": ("APP_SPECIFIC_PASSWORD", "APP_PASSWORD"),
}


def parse_dotenv(path: Path) -> dict[str, str]:
    values: dict[str, str] = {}
    for raw in path.read_text(encoding="utf-8", errors="replace").splitlines():
        line = raw.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        if line.startswith("export "):
            line = line[7:].strip()
        key, value = line.split("=", 1)
        key = key.strip()
        if not key:
            continue
        values[key] = _clean_dotenv_value(value.strip())
    return values


def _clean_dotenv_value(value: str) -> str:
    if len(value) >= 2 and value[0] == value[-1] and value[0] in {"'", '"'}:
        return value[1:-1]
    return value


def apply_dotenv_aliases(
    environ: Mapping[str, str],
    dotenv: Mapping[str, str],
) -> tuple[dict[str, str], dict[str, str]]:
    merged = dict(environ)
    sources = {
        name: "environment"
        for name in SHIPYARD_SIGNING_ENV
        if merged.get(name)
    }

    for name in (*SHIPYARD_SIGNING_ENV, RELEASE_BOT_SECRET):
        if not merged.get(name) and dotenv.get(name):
            merged[name] = dotenv[name]
            if name in SHIPYARD_SIGNING_ENV:
                sources[name] = f"dotenv:{name}"

    for target, aliases in DOTENV_SIGNING_ALIASES.items():
        if merged.get(target):
            continue
        for alias in aliases:
            if dotenv.get(alias):
                merged[target] = dotenv[alias]
                sources[target] = f"dotenv:{alias}"
                break

    return merged, sources
