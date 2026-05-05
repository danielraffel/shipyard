#!/usr/bin/env python3
"""Validate Rust `ship` PR setup against a real sandbox pull request.

The harness creates a temporary worktree, lets Shipyard create a missing
`develop/*` base branch, pushes a temporary head branch, opens a PR, runs a
local validation target, and merges into the temporary base. Cleanup removes
both temporary branches. It never targets `main` and uses an isolated state
root.
"""

from __future__ import annotations

import argparse
import json
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[1]


def run(
    args: list[str],
    *,
    cwd: Path = ROOT,
    check: bool = True,
    timeout: int = 120,
) -> subprocess.CompletedProcess[str]:
    result = subprocess.run(
        args,
        cwd=cwd,
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


def load_json(result: subprocess.CompletedProcess[str]) -> Any:
    return json.loads(result.stdout)


def delete_remote_branch(repo: str, branch: str) -> None:
    run(["gh", "api", "-X", "DELETE", f"repos/{repo}/branches/{branch}/protection"], check=False)
    run(["git", "push", "origin", f":refs/heads/{branch}"], check=False)
    run(["gh", "api", "-X", "DELETE", f"repos/{repo}/git/refs/heads/{branch}"], check=False)


def cancel_branch_runs(repo: str, branch: str) -> None:
    result = run(
        [
            "gh",
            "run",
            "list",
            "--repo",
            repo,
            "--branch",
            branch,
            "--limit",
            "20",
            "--json",
            "databaseId,status",
        ],
        check=False,
    )
    if result.returncode != 0:
        return
    for item in load_json(result):
        if item.get("status") != "completed":
            run(["gh", "run", "cancel", str(item["databaseId"]), "--repo", repo], check=False)


def remote_branch_exists(branch: str) -> bool:
    return (
        run(
            ["git", "ls-remote", "--exit-code", "--heads", "origin", branch],
            check=False,
        ).returncode
        == 0
    )


def branch_protection_exists(repo: str, branch: str) -> bool:
    return (
        run(
            ["gh", "api", f"repos/{repo}/branches/{branch}/protection"],
            check=False,
        ).returncode
        == 0
    )


def write_shipyard_config(worktree: Path) -> None:
    config_dir = worktree / ".shipyard"
    config_dir.mkdir(exist_ok=True)
    (config_dir / "config.toml").write_text(
        """
[validation.default]
command = "git rev-parse --is-inside-work-tree"

[targets.local]
backend = "local"
platform = "local"
advisory = true
""".lstrip(),
        encoding="utf-8",
    )


def create_worktree(root: Path, head_branch: str) -> Path:
    worktree = root / "repo"
    run(["git", "worktree", "add", "--detach", str(worktree), "HEAD"])
    run(["git", "checkout", "-b", head_branch], cwd=worktree)
    write_shipyard_config(worktree)
    run(["git", "add", ".shipyard/config.toml"], cwd=worktree)
    run(
        [
            "git",
            "commit",
            "-m",
            "test: shipyard rust ship live smoke",
            "-m",
            "Temporary PR created by scripts/validate_ship_live.py.\n\nLane-Policy: local=advisory",
        ],
        cwd=worktree,
    )
    return worktree


def cleanup(repo: str, worktree: Path, head_branch: str, base_branch: str, pr: int | None) -> None:
    cancel_branch_runs(repo, head_branch)
    cancel_branch_runs(repo, base_branch)
    if pr is not None:
        state = run(
            ["gh", "pr", "view", str(pr), "--repo", repo, "--json", "state"],
            check=False,
        )
        if state.returncode == 0 and load_json(state).get("state") == "OPEN":
            run(["gh", "pr", "close", str(pr), "--repo", repo], check=False)
    delete_remote_branch(repo, head_branch)
    delete_remote_branch(repo, base_branch)
    run(["git", "worktree", "remove", "--force", str(worktree)], check=False)
    run(["git", "branch", "-D", head_branch], check=False)


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--binary", default=str(ROOT / "target" / "release" / "shipyard"))
    parser.add_argument("--repo", default="danielraffel/Shipyard")
    parser.add_argument("--keep", action="store_true")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    binary = Path(args.binary).expanduser().resolve()
    if not binary.exists():
        raise SystemExit(f"binary not found: {binary}")
    if shutil.which("gh") is None:
        raise SystemExit("gh is required for live ship validation")
    run(["gh", "auth", "status"], check=True)

    stamp = int(time.time())
    head_branch = f"shipyard-ship-head-{stamp}"
    base_branch = f"develop/shipyard-ship-base-{stamp}"
    root = Path(tempfile.mkdtemp(prefix="shipyard-ship-live-"))
    worktree = root / "repo"
    pr_number: int | None = None
    try:
        worktree = create_worktree(root, head_branch)
        state_dir = root / "state"
        result = run(
            [
                str(binary),
                "--mode",
                "isolated",
                "--state-dir",
                str(state_dir),
                "--cwd",
                str(worktree),
                "--json",
                "ship",
                "--base",
                base_branch,
                "--no-warm",
            ],
            cwd=worktree,
            timeout=360,
        )
        payload = load_json(result)
        pr_number = int(payload["pr"])
        pr = load_json(
            run(
                [
                    "gh",
                    "pr",
                    "view",
                    str(pr_number),
                    "--repo",
                    args.repo,
                    "--json",
                    "state,baseRefName,headRefName,title,body,url",
                ],
                cwd=worktree,
            )
        )
        if payload.get("command") != "ship":
            raise SystemExit(f"unexpected command payload: {payload}")
        if payload.get("merged") is not True:
            raise SystemExit(f"ship did not merge sandbox PR: {json.dumps(payload, indent=2)}")
        if payload["ship_state"]["base_branch"] != base_branch:
            raise SystemExit(f"unexpected base branch in ship state: {payload['ship_state']}")
        if not remote_branch_exists(base_branch):
            raise SystemExit(f"ship did not create remote base branch {base_branch}")
        if not branch_protection_exists(args.repo, base_branch):
            raise SystemExit(f"ship did not apply branch protection to {base_branch}")
        runs = payload["ship_state"].get("dispatched_runs") or []
        if not runs or runs[0].get("required") is not False:
            raise SystemExit(f"advisory local lane not recorded as non-required: {runs}")
        if pr.get("state") != "MERGED":
            raise SystemExit(f"expected merged PR, got {pr}")
        body = pr.get("body") or ""
        if "## Advisory lanes" not in body or "- `local`" not in body:
            raise SystemExit(f"auto-created PR body is missing advisory lane section: {body}")
        print(
            json.dumps(
                {
                    "ok": True,
                    "pr": pr_number,
                    "url": pr["url"],
                    "base_branch": base_branch,
                    "head_branch": head_branch,
                    "merged": payload["merged"],
                    "run": payload["run"],
                },
                indent=2,
            )
        )
        return 0
    finally:
        if args.keep:
            print(f"kept tempdir: {root}", file=sys.stderr)
        else:
            cleanup(args.repo, worktree, head_branch, base_branch, pr_number)
            shutil.rmtree(root, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
