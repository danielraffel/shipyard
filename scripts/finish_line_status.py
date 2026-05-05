#!/usr/bin/env python3
"""Report credential-backed blockers for Shipyard release sign-off.

This helper is intentionally non-mutating. It checks only whether the
release-bot repository secret is configured and whether the local shell has
the signing/notarization environment needed for the final macOS release
validation.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Mapping

import release_env

RELEASE_BOT_SECRET = release_env.RELEASE_BOT_SECRET
SIGNING_ENV = release_env.SHIPYARD_SIGNING_ENV
DEFAULT_REPO = "danielraffel/Shipyard"


@dataclass(frozen=True)
class SecretProbe:
    present: bool
    updated_at: str | None = None
    error: str | None = None


def env_presence(names: tuple[str, ...], environ: Mapping[str, str]) -> dict[str, bool]:
    return {name: bool(environ.get(name)) for name in names}


def missing_env(names: tuple[str, ...], environ: Mapping[str, str]) -> list[str]:
    return [name for name in names if not environ.get(name)]


def parse_secret_list(raw: str, name: str) -> SecretProbe:
    try:
        items = json.loads(raw)
    except json.JSONDecodeError as error:
        return SecretProbe(False, error=f"could not parse gh secret list JSON: {error}")
    if not isinstance(items, list):
        return SecretProbe(False, error="gh secret list JSON was not a list")
    for item in items:
        if isinstance(item, dict) and item.get("name") == name:
            updated_at = item.get("updatedAt")
            return SecretProbe(
                True,
                updated_at if isinstance(updated_at, str) else None,
            )
    return SecretProbe(False)


def probe_repo_secret(repo: str, name: str) -> SecretProbe:
    try:
        result = subprocess.run(
            ["gh", "secret", "list", "--repo", repo, "--json", "name,updatedAt"],
            check=False,
            capture_output=True,
            text=True,
            timeout=30,
        )
    except (FileNotFoundError, subprocess.SubprocessError) as error:
        return SecretProbe(False, error=f"could not run gh secret list: {error}")
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or "no gh output"
        return SecretProbe(False, error=f"gh secret list failed: {detail}")
    return parse_secret_list(result.stdout, name)


def build_report(
    *,
    repo: str,
    environ: Mapping[str, str],
    secret_probe: SecretProbe,
    signing_sources: Mapping[str, str] | None = None,
    env_file: dict[str, object] | None = None,
) -> dict[str, object]:
    signing_missing = missing_env(SIGNING_ENV, environ)
    release_bot_env = bool(environ.get(RELEASE_BOT_SECRET))
    blockers: list[str] = []
    if not secret_probe.present:
        blockers.append(f"configure repo secret {RELEASE_BOT_SECRET} in {repo}")
    if signing_missing:
        blockers.append(
            "export signing/notarization env vars: " + ", ".join(signing_missing)
        )
    return {
        "ready": not blockers,
        "repo": repo,
        "release_bot": {
            "repo_secret_present": secret_probe.present,
            "repo_secret_updated_at": secret_probe.updated_at,
            "repo_secret_error": secret_probe.error,
            "local_env_present": release_bot_env,
        },
        "signing": {
            "env": env_presence(SIGNING_ENV, environ),
            "missing": signing_missing,
            "sources": dict(signing_sources or {}),
        },
        "env_file": env_file,
        "blockers": blockers,
        "next_commands": [
            "target/release/shipyard --json doctor --release-chain",
            "scripts/release-macos-local.sh --tag vX.Y.Z --skip-build --binary target/release/shipyard",
            "scripts/release-macos-local.sh --tag vX.Y.Z --upload --rollback-tag vPREVIOUS",
        ],
    }


def render_text(report: dict[str, object]) -> str:
    lines = [
        f"repo: {report['repo']}",
        f"ready: {str(report['ready']).lower()}",
    ]
    release_bot = report["release_bot"]
    signing = report["signing"]
    assert isinstance(release_bot, dict)
    assert isinstance(signing, dict)
    lines.append(
        "release-bot secret: "
        + ("present" if release_bot["repo_secret_present"] else "missing")
    )
    if release_bot.get("repo_secret_updated_at"):
        lines.append(f"release-bot secret updated_at: {release_bot['repo_secret_updated_at']}")
    if release_bot.get("repo_secret_error"):
        lines.append(f"release-bot secret error: {release_bot['repo_secret_error']}")
    missing = signing["missing"]
    assert isinstance(missing, list)
    if missing:
        lines.append("missing signing env: " + ", ".join(str(item) for item in missing))
    else:
        lines.append("missing signing env: none")
    blockers = report["blockers"]
    assert isinstance(blockers, list)
    if blockers:
        lines.append("blockers:")
        lines.extend(f"- {blocker}" for blocker in blockers)
    return "\n".join(lines)


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", default=DEFAULT_REPO)
    parser.add_argument("--json", action="store_true", help="Emit machine-readable JSON")
    parser.add_argument(
        "--skip-gh",
        action="store_true",
        help="Do not query GitHub; report the release-bot repo secret as unknown/missing.",
    )
    parser.add_argument(
        "--env-file",
        type=Path,
        help=(
            "Optional dotenv file to inspect for local signing credentials. "
            "Only Shipyard-prefixed names and the documented macOS aliases "
            "APPLE_ID, TEAM_ID, APP_SPECIFIC_PASSWORD/APP_PASSWORD, and APP_CERT "
            "are considered."
        ),
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv or sys.argv[1:])
    env_file_report: dict[str, object] | None = None
    environ: Mapping[str, str] = os.environ
    signing_sources: Mapping[str, str] = {}
    if args.env_file:
        try:
            dotenv = release_env.parse_dotenv(args.env_file)
        except OSError as error:
            raise SystemExit(f"could not read --env-file {args.env_file}: {error}") from error
        environ, signing_sources = release_env.apply_dotenv_aliases(os.environ, dotenv)
        env_file_report = {
            "path": str(args.env_file),
            "loaded": True,
            "keys_loaded": len(dotenv),
        }
    secret_probe = (
        SecretProbe(False, error="skipped gh secret probe")
        if args.skip_gh
        else probe_repo_secret(args.repo, RELEASE_BOT_SECRET)
    )
    report = build_report(
        repo=args.repo,
        environ=environ,
        secret_probe=secret_probe,
        signing_sources=signing_sources,
        env_file=env_file_report,
    )
    if args.json:
        print(json.dumps(report, indent=2, sort_keys=True))
    else:
        print(render_text(report))
    return 0 if report["ready"] else 2


if __name__ == "__main__":
    raise SystemExit(main())
