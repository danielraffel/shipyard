#!/usr/bin/env python3
"""Live validation for the Rust GitHub Actions cloud executor.

This is intentionally separate from normal CI. It dispatches the lightweight
`cloud-live-smoke.yml` workflow through `shipyard run` using a temporary
clone and isolated HOME/state roots, then verifies the Rust run JSON reports a
passing cloud target.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def run(
    args: list[str],
    *,
    cwd: Path | None = None,
    env: dict[str, str] | None = None,
    timeout: int = 60,
    check: bool = True,
) -> subprocess.CompletedProcess[str]:
    result = subprocess.run(
        args,
        cwd=cwd,
        env=env,
        check=False,
        capture_output=True,
        text=True,
        timeout=timeout,
    )
    if check and result.returncode != 0:
        raise SystemExit(
            f"command failed ({result.returncode}): {' '.join(args)}\n"
            f"stdout:\n{result.stdout}\n"
            f"stderr:\n{result.stderr}"
        )
    return result


def write_cloud_config(
    clone: Path,
    *,
    repo: str,
    workflow: str,
    poll_interval: int,
    dispatch_settle: int,
    timeout: int,
) -> None:
    config_dir = clone / ".shipyard"
    config_dir.mkdir()
    (config_dir / "config.toml").write_text(
        f"""
[cloud]
provider = "namespace"
repository = "{repo}"
poll_interval_secs = {poll_interval}
dispatch_settle_secs = {dispatch_settle}
max_poll_secs = {timeout}

[targets.cloud_smoke]
backend = "cloud"
platform = "linux-x64"
workflow = "{workflow}"
""".lstrip(),
        encoding="utf-8",
    )


def latest_workflow_run(repo: str, workflow: str) -> dict[str, object] | None:
    result = run(
        [
            "gh",
            "run",
            "list",
            "--repo",
            repo,
            "--workflow",
            workflow,
            "--branch",
            "main",
            "--limit",
            "1",
            "--json",
            "databaseId,url,status,conclusion,headBranch",
        ],
        check=False,
    )
    if result.returncode != 0 or not result.stdout.strip():
        return None
    runs = json.loads(result.stdout)
    return runs[0] if runs else None


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--binary", default=str(ROOT / "target" / "release" / "shipyard"))
    parser.add_argument("--repo", default="danielraffel/Shipyard")
    parser.add_argument("--workflow", default="cloud-live-smoke.yml")
    parser.add_argument("--poll-interval", type=int, default=5)
    parser.add_argument("--dispatch-settle", type=int, default=90)
    parser.add_argument("--timeout", type=int, default=360)
    parser.add_argument("--keep", action="store_true")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    binary = Path(args.binary).expanduser().resolve()
    if not binary.exists():
        raise SystemExit(f"binary not found: {binary}")
    if shutil.which("gh") is None:
        raise SystemExit("gh is required for live cloud executor validation")
    run(["gh", "auth", "status"], check=True)

    root = Path(tempfile.mkdtemp(prefix="shipyard-cloud-executor-"))
    try:
        clone = root / "repo"
        home = root / "home"
        state = root / "state"
        home.mkdir()
        state.mkdir()
        run(["git", "clone", "--quiet", "--branch", "main", str(ROOT), str(clone)], timeout=120)
        write_cloud_config(
            clone,
            repo=args.repo,
            workflow=args.workflow,
            poll_interval=args.poll_interval,
            dispatch_settle=args.dispatch_settle,
            timeout=args.timeout,
        )

        env = os.environ.copy()
        gh_config_dir = os.environ.get("GH_CONFIG_DIR") or str(Path.home() / ".config" / "gh")
        gh_token = env.get("GH_TOKEN")
        if not gh_token:
            token_result = run(["gh", "auth", "token"], check=False)
            gh_token = token_result.stdout.strip() if token_result.returncode == 0 else ""
        env.update(
            {
                "HOME": str(home),
                "USERPROFILE": str(home),
                "XDG_CONFIG_HOME": str(home / ".config"),
                "XDG_STATE_HOME": str(home / ".local" / "state"),
                "XDG_CACHE_HOME": str(home / ".cache"),
                "RUST_BACKTRACE": "1",
            }
        )
        if gh_token:
            env["GH_TOKEN"] = gh_token
            env.pop("GH_CONFIG_DIR", None)
        else:
            env["GH_CONFIG_DIR"] = gh_config_dir
        result = run(
            [
                str(binary),
                "--mode",
                "isolated",
                "--state-dir",
                str(state),
                "--cwd",
                str(clone),
                "--json",
                "run",
                "--targets",
                "cloud_smoke",
                "--no-warm",
                "--allow-tree-drift",
            ],
            cwd=clone,
            env=env,
            timeout=args.timeout + args.dispatch_settle + 120,
            check=False,
        )
        if result.returncode != 0:
            raise SystemExit(
                f"shipyard cloud executor smoke failed ({result.returncode})\n"
                f"stdout:\n{result.stdout}\n"
                f"stderr:\n{result.stderr}"
            )
        payload = json.loads(result.stdout)
        job = payload["run"]
        target = job["results"]["cloud_smoke"]
        if job["overall"] != "pass":
            raise SystemExit(f"unexpected cloud job result: {json.dumps(job, indent=2)}")
        if target["status"] != "pass" or target["backend"] != "cloud":
            raise SystemExit(f"unexpected cloud target result: {json.dumps(target, indent=2)}")
        latest = latest_workflow_run(args.repo, args.workflow)
        print(json.dumps({"ok": True, "target": target, "latest_run": latest}, indent=2))
        return 0
    finally:
        if args.keep:
            print(f"kept tempdir: {root}", file=sys.stderr)
        else:
            shutil.rmtree(root, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
