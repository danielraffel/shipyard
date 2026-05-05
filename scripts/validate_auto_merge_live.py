#!/usr/bin/env python3
"""Validate Shipyard auto-merge against a real sandbox pull request.

The script creates temporary base/head branches in this repo, opens a PR from
head to base, seeds an isolated ShipState root with passing evidence, runs the
Rust `auto-merge` command, verifies the PR merged, then deletes the temporary
branches. It never targets `main`.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[1]


@dataclass
class SandboxPr:
    tempdir: tempfile.TemporaryDirectory[str]
    worktree: Path
    base_branch: str
    head_branch: str
    number: int
    url: str
    head_sha: str


def run(args: list[str], *, cwd: Path = ROOT, check: bool = True) -> subprocess.CompletedProcess[str]:
    result = subprocess.run(
        args,
        cwd=cwd,
        check=False,
        capture_output=True,
        text=True,
    )
    if check and result.returncode != 0:
        sys.stderr.write(result.stderr)
        sys.stderr.write(result.stdout)
        raise SystemExit(result.returncode)
    return result


def load_json(result: subprocess.CompletedProcess[str]) -> Any:
    return json.loads(result.stdout)


def utc_now() -> str:
    return datetime.now(UTC).isoformat().replace("+00:00", "Z")


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
            run(
                ["gh", "run", "cancel", str(item["databaseId"]), "--repo", repo],
                check=False,
            )


def delete_remote_branch(branch: str) -> None:
    run(["git", "push", "origin", f":refs/heads/{branch}"], check=False)


def create_sandbox_pr(repo: str) -> SandboxPr:
    stamp = int(time.time())
    base_branch = f"shipyard-auto-merge-base-{stamp}"
    head_branch = f"shipyard-auto-merge-head-{stamp}"
    tempdir = tempfile.TemporaryDirectory(prefix="shipyard-auto-merge-pr-")
    worktree = Path(tempdir.name) / "repo"
    try:
        run(["git", "worktree", "add", "--detach", str(worktree), "HEAD"])
        run(["git", "checkout", "-b", base_branch], cwd=worktree)
        run(["git", "push", "origin", f"HEAD:refs/heads/{base_branch}"], cwd=worktree)
        run(["git", "checkout", "-b", head_branch], cwd=worktree)
        run(
            [
                "git",
                "commit",
                "--allow-empty",
                "-m",
                "test: shipyard rust auto-merge live smoke",
            ],
            cwd=worktree,
        )
        head_sha = run(["git", "rev-parse", "HEAD"], cwd=worktree).stdout.strip()
        run(["git", "push", "origin", f"HEAD:refs/heads/{head_branch}"], cwd=worktree)
        pr_url = run(
            [
                "gh",
                "pr",
                "create",
                "--repo",
                repo,
                "--base",
                base_branch,
                "--head",
                head_branch,
                "--title",
                "Shipyard Rust auto-merge live smoke",
                "--body",
                "Temporary PR created by scripts/validate_auto_merge_live.py.",
            ],
            cwd=worktree,
        ).stdout.strip()
        pr = load_json(
            run(
                [
                    "gh",
                    "pr",
                    "view",
                    pr_url,
                    "--repo",
                    repo,
                    "--json",
                    "number,url",
                ],
                cwd=worktree,
            )
        )
        return SandboxPr(
            tempdir=tempdir,
            worktree=worktree,
            base_branch=base_branch,
            head_branch=head_branch,
            number=int(pr["number"]),
            url=str(pr["url"]),
            head_sha=head_sha,
        )
    except BaseException:
        cleanup_sandbox_pr(
            SandboxPr(
                tempdir=tempdir,
                worktree=worktree,
                base_branch=base_branch,
                head_branch=head_branch,
                number=0,
                url="",
                head_sha="",
            ),
            repo,
        )
        raise


def cleanup_sandbox_pr(sandbox_pr: SandboxPr, repo: str) -> None:
    cancel_branch_runs(repo, sandbox_pr.head_branch)
    if sandbox_pr.number:
        result = run(
            [
                "gh",
                "pr",
                "view",
                str(sandbox_pr.number),
                "--repo",
                repo,
                "--json",
                "state",
            ],
            check=False,
        )
        if result.returncode == 0 and load_json(result).get("state") == "OPEN":
            run(
                [
                    "gh",
                    "pr",
                    "close",
                    str(sandbox_pr.number),
                    "--repo",
                    repo,
                    "--delete-branch",
                ],
                check=False,
            )
    delete_remote_branch(sandbox_pr.head_branch)
    delete_remote_branch(sandbox_pr.base_branch)
    run(["git", "worktree", "remove", "--force", str(sandbox_pr.worktree)], check=False)
    run(["git", "branch", "-D", sandbox_pr.head_branch], check=False)
    run(["git", "branch", "-D", sandbox_pr.base_branch], check=False)
    sandbox_pr.tempdir.cleanup()


def write_passing_ship_state(
    state_dir: Path,
    *,
    repo: str,
    pr: SandboxPr,
    target: str,
) -> None:
    ship_dir = state_dir / "ship"
    (ship_dir / "archive").mkdir(parents=True, exist_ok=True)
    now = utc_now()
    payload: dict[str, Any] = {
        "schema_version": 1,
        "pr": pr.number,
        "repo": repo,
        "branch": pr.head_branch,
        "base_branch": pr.base_branch,
        "head_sha": pr.head_sha,
        "policy_signature": "auto-merge-live-smoke",
        "pr_url": pr.url,
        "pr_title": "Shipyard Rust auto-merge live smoke",
        "commit_subject": "auto-merge live smoke",
        "dispatched_runs": [
            {
                "target": target,
                "provider": "github",
                "run_id": "live-auto-merge",
                "status": "pass",
                "started_at": now,
                "updated_at": now,
                "attempt": 1,
                "required": True,
            }
        ],
        "evidence_snapshot": {target: "pass"},
        "attempt": 1,
        "created_at": now,
        "updated_at": now,
    }
    (ship_dir / f"{pr.number}.json").write_text(
        json.dumps(payload, indent=2) + "\n",
        encoding="utf-8",
    )


def shipyard_json(rust_bin: Path, state_dir: Path, args: list[str]) -> dict[str, Any]:
    result = run([str(rust_bin), "--json", "--state-dir", str(state_dir), *args])
    return load_json(result)


def validate_auto_merge(args: argparse.Namespace) -> dict[str, Any]:
    sandbox_pr = create_sandbox_pr(args.repo)
    with tempfile.TemporaryDirectory(prefix="shipyard-auto-merge-state-") as tempdir:
        state_dir = Path(tempdir) / "state"
        try:
            write_passing_ship_state(
                state_dir,
                repo=args.repo,
                pr=sandbox_pr,
                target=args.target,
            )
            payload = shipyard_json(
                args.rust_bin,
                state_dir,
                [
                    "auto-merge",
                    str(sandbox_pr.number),
                    "--merge-method",
                    args.merge_method,
                    "--no-delete-branch",
                ],
            )
            if payload.get("event") != "merged":
                raise SystemExit(
                    f"Expected auto-merge event=merged; got {json.dumps(payload, indent=2)}"
                )
            pr = load_json(
                run(
                    [
                        "gh",
                        "pr",
                        "view",
                        str(sandbox_pr.number),
                        "--repo",
                        args.repo,
                        "--json",
                        "state,mergedAt,baseRefName,headRefName",
                    ]
                )
            )
            if pr.get("state") != "MERGED" or not pr.get("mergedAt"):
                raise SystemExit(f"PR did not merge: {json.dumps(pr, indent=2)}")
            active_state = state_dir / "ship" / f"{sandbox_pr.number}.json"
            archived_states = sorted(
                (state_dir / "ship" / "archive").glob(f"{sandbox_pr.number}-*.json")
            )
            if active_state.exists() or not archived_states:
                raise SystemExit(
                    "ShipState archive mismatch after auto-merge: "
                    + json.dumps(
                        {
                            "active_exists": active_state.exists(),
                            "archived_count": len(archived_states),
                        },
                        indent=2,
                    )
                )
            return {
                "auto_merge": payload,
                "pr": pr,
                "sandbox": {
                    "number": sandbox_pr.number,
                    "url": sandbox_pr.url,
                    "base_branch": sandbox_pr.base_branch,
                    "head_branch": sandbox_pr.head_branch,
                },
            }
        finally:
            cleanup_sandbox_pr(sandbox_pr, args.repo)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo", default="danielraffel/Shipyard")
    parser.add_argument("--target", default="live-auto-merge")
    parser.add_argument("--merge-method", choices=["merge", "squash", "rebase"], default="squash")
    parser.add_argument("--rust-bin", type=Path, default=ROOT / "target" / "release" / "shipyard")
    args = parser.parse_args()

    rust_bin = args.rust_bin if args.rust_bin.is_absolute() else ROOT / args.rust_bin
    if not rust_bin.exists():
        raise SystemExit(f"Shipyard binary not found: {rust_bin}. Run `cargo build --release` first.")
    args.rust_bin = rust_bin

    run(["gh", "auth", "status"])
    run(["gh", "repo", "view", args.repo, "--json", "nameWithOwner"])

    print(json.dumps(validate_auto_merge(args), indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
