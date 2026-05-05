#!/usr/bin/env python3
"""Validate Shipyard cloud dispatch against real GitHub workflows.

The script uses a temporary ShipState root plus the dedicated
`cloud-live-smoke.yml` workflow. It validates the cloud command paths
without reading or mutating the user's daily Shipyard config, daemon, or state.
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
    branch: str
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


def write_ship_state(
    state_dir: Path,
    *,
    pr: int,
    repo: str,
    branch: str,
    head_sha: str,
    dispatched_runs: list[dict[str, Any]] | None = None,
) -> None:
    ship_dir = state_dir / "ship"
    archive_dir = ship_dir / "archive"
    archive_dir.mkdir(parents=True, exist_ok=True)
    now = utc_now()
    payload: dict[str, Any] = {
        "schema_version": 1,
        "pr": pr,
        "repo": repo,
        "branch": branch,
        "base_branch": "main",
        "head_sha": head_sha,
        "policy_signature": "cloud-live-smoke",
        "pr_url": f"https://github.com/{repo}/pull/{pr}",
        "pr_title": "Shipyard cloud live smoke",
        "commit_subject": "cloud live smoke",
        "dispatched_runs": dispatched_runs or [],
        "evidence_snapshot": {},
        "attempt": 1,
        "created_at": now,
        "updated_at": now,
    }
    (ship_dir / f"{pr}.json").write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")


def workflow_file(workflow: str) -> str:
    return workflow if workflow.endswith((".yml", ".yaml")) else f"{workflow}.yml"


def gh_run_view(repo: str, run_id: str | int) -> dict[str, Any]:
    result = run(
        [
            "gh",
            "run",
            "view",
            str(run_id),
            "--repo",
            repo,
            "--json",
            "status,conclusion,url,workflowName",
        ],
        check=False,
    )
    if result.returncode != 0:
        return {}
    return load_json(result)


def run_is_completed(repo: str, run_id: str | int) -> bool:
    return gh_run_view(repo, run_id).get("status") == "completed"


def wait_for_run(repo: str, run_id: str | int, timeout: float, poll_interval: float) -> dict[str, Any]:
    deadline = time.monotonic() + timeout
    last: dict[str, Any] | None = None
    while time.monotonic() < deadline:
        last = gh_run_view(repo, run_id)
        if last.get("status") == "completed":
            return last
        time.sleep(poll_interval)
    raise TimeoutError(f"workflow run {run_id} did not complete in {timeout}s; last={last}")


def workflow_run_ids(repo: str, workflow: str, branch: str) -> set[int]:
    result = run(
        [
            "gh",
            "run",
            "list",
            "--repo",
            repo,
            "--workflow",
            workflow_file(workflow),
            "--branch",
            branch,
            "--limit",
            "20",
            "--json",
            "databaseId",
        ]
    )
    return {int(item["databaseId"]) for item in load_json(result)}


def wait_for_new_workflow_run(
    repo: str,
    workflow: str,
    branch: str,
    *,
    previous_ids: set[int],
    timeout: float,
    poll_interval: float,
) -> dict[str, Any]:
    deadline = time.monotonic() + timeout
    last: list[dict[str, Any]] = []
    while time.monotonic() < deadline:
        result = run(
            [
                "gh",
                "run",
                "list",
                "--repo",
                repo,
                "--workflow",
                workflow_file(workflow),
                "--branch",
                branch,
                "--limit",
                "10",
                "--json",
                "databaseId,status,conclusion,url,workflowName,createdAt",
            ]
        )
        last = load_json(result)
        candidates = [
            item for item in last if int(item.get("databaseId") or 0) not in previous_ids
        ]
        if candidates:
            return max(candidates, key=lambda item: int(item["databaseId"]))
        time.sleep(poll_interval)
    raise TimeoutError(
        f"new workflow run for {workflow} on {branch} did not appear in {timeout}s; last={last}"
    )


def cancel_run(repo: str, run_id: str | int) -> None:
    run(
        [
            "gh",
            "api",
            "-X",
            "POST",
            f"repos/{repo}/actions/runs/{run_id}/cancel",
        ],
        check=False,
    )


def matching_active_jobs(repo: str, run_id: str | int, target: str) -> list[dict[str, Any]]:
    result = run(
        [
            "gh",
            "run",
            "view",
            str(run_id),
            "--repo",
            repo,
            "--json",
            "jobs",
        ],
        check=False,
    )
    if result.returncode != 0:
        return []
    jobs = load_json(result).get("jobs", [])
    needle = target.lower()
    return [
        job
        for job in jobs
        if needle in str(job.get("name") or "").lower()
        and job.get("status") in {"queued", "in_progress"}
    ]


def wait_for_matching_active_jobs(
    repo: str,
    run_id: str | int,
    target: str,
    *,
    timeout: float,
    poll_interval: float,
) -> list[dict[str, Any]]:
    deadline = time.monotonic() + timeout
    last: list[dict[str, Any]] = []
    while time.monotonic() < deadline:
        last = matching_active_jobs(repo, run_id, target)
        if last:
            return last
        if run_is_completed(repo, run_id):
            break
        time.sleep(poll_interval)
    raise TimeoutError(f"no active jobs matching {target!r} in run {run_id}; last={last}")


def create_sandbox_pr(repo: str, base: str) -> SandboxPr:
    tempdir = tempfile.TemporaryDirectory(prefix="shipyard-retarget-pr-")
    worktree = Path(tempdir.name) / "repo"
    branch = f"shipyard-retarget-smoke-{int(time.time())}"
    try:
        run(["git", "worktree", "add", "--detach", str(worktree), "HEAD"])
        run(["git", "checkout", "-b", branch], cwd=worktree)
        run(
            [
                "git",
                "commit",
                "--allow-empty",
                "-m",
                "test: shipyard rust retarget live smoke",
            ],
            cwd=worktree,
        )
        head_sha = run(["git", "rev-parse", "HEAD"], cwd=worktree).stdout.strip()
        run(["git", "push", "origin", f"HEAD:refs/heads/{branch}"], cwd=worktree)
        pr_url = run(
            [
                "gh",
                "pr",
                "create",
                "--repo",
                repo,
                "--base",
                base,
                "--head",
                branch,
                "--draft",
                "--title",
                "Shipyard Rust retarget live smoke",
                "--body",
                "Temporary PR created by scripts/validate_cloud_live.py; safe to close.",
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
            branch=branch,
            number=int(pr["number"]),
            url=str(pr["url"]),
            head_sha=head_sha,
        )
    except BaseException:
        cleanup_sandbox_pr(
            SandboxPr(
                tempdir=tempdir,
                worktree=worktree,
                branch=branch,
                number=0,
                url="",
                head_sha="",
            ),
            repo,
        )
        raise


def cleanup_sandbox_pr(sandbox_pr: SandboxPr, repo: str) -> None:
    cancel_branch_runs(repo, sandbox_pr.branch)
    if sandbox_pr.number:
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
    else:
        run(
            ["git", "push", "origin", f":refs/heads/{sandbox_pr.branch}"],
            check=False,
        )
    run(["git", "worktree", "remove", "--force", str(sandbox_pr.worktree)], check=False)
    sandbox_pr.tempdir.cleanup()


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


def shipyard_json(rust_bin: Path, state_dir: Path, args: list[str]) -> dict[str, Any]:
    result = run([str(rust_bin), "--json", "--state-dir", str(state_dir), *args])
    return load_json(result)


def validate_add_lane(args: argparse.Namespace, state_dir: Path) -> dict[str, Any]:
    previous_ids = workflow_run_ids(args.repo, args.workflow, args.branch)
    write_ship_state(
        state_dir,
        pr=args.pr,
        repo=args.repo,
        branch=args.branch,
        head_sha=args.head_sha,
    )

    payload = shipyard_json(
        args.rust_bin,
        state_dir,
        [
            "cloud",
            "add-lane",
            "--pr",
            str(args.pr),
            "--target",
            args.target,
            "--provider",
            args.provider,
            "--workflow",
            args.workflow,
            "--apply",
        ],
    )
    run_id = str(payload.get("run_id") or "")
    if not run_id.isdecimal():
        raise SystemExit(f"Expected numeric live run_id; got payload: {json.dumps(payload, indent=2)}")
    if int(run_id) in previous_ids:
        raise SystemExit(
            "Rust add-lane recorded a stale pre-existing run: "
            + json.dumps({"run_id": run_id, "payload": payload}, indent=2)
        )

    run_state = wait_for_run(args.repo, run_id, args.timeout, args.poll_interval)
    if run_state.get("conclusion") != "success":
        raise SystemExit(
            "Cloud add-lane live smoke failed: "
            + json.dumps({"dispatch": payload, "run": run_state}, indent=2)
        )
    return {"dispatch": payload, "run": run_state}


def validate_handoff(args: argparse.Namespace, state_dir: Path) -> dict[str, Any]:
    before_source = workflow_run_ids(args.repo, args.workflow, args.branch)
    run(
        [
            "gh",
            "workflow",
            "run",
            workflow_file(args.workflow),
            "--repo",
            args.repo,
            "--ref",
            args.branch,
            "-f",
            f"runner_provider={args.provider}",
            "-f",
            "note=shipyard-handoff-source",
            "-f",
            f"sleep_seconds={args.source_sleep_seconds}",
        ]
    )
    source = wait_for_new_workflow_run(
        args.repo,
        args.workflow,
        args.branch,
        previous_ids=before_source,
        timeout=args.timeout,
        poll_interval=args.poll_interval,
    )
    source_id = int(source["databaseId"])
    before_handoff = workflow_run_ids(args.repo, args.workflow, args.branch)
    try:
        payload = shipyard_json(
            args.rust_bin,
            state_dir,
            [
                "cloud",
                "handoff",
                "run",
                str(source_id),
                "--repo",
                args.repo,
                "--to",
                args.provider,
                "--apply",
            ],
        )
        if str(payload.get("cancelled_run_id")) != str(source_id):
            raise SystemExit(
                f"Expected handoff to cancel {source_id}; got {json.dumps(payload, indent=2)}"
            )

        cancelled = wait_for_run(args.repo, source_id, args.timeout, args.poll_interval)
        if cancelled.get("conclusion") != "cancelled":
            raise SystemExit(
                "Cloud handoff source run was not cancelled: "
                + json.dumps({"handoff": payload, "source": cancelled}, indent=2)
            )

        redispatched = wait_for_new_workflow_run(
            args.repo,
            args.workflow,
            args.branch,
            previous_ids=before_handoff,
            timeout=args.timeout,
            poll_interval=args.poll_interval,
        )
        redispatched = wait_for_run(
            args.repo,
            redispatched["databaseId"],
            args.timeout,
            args.poll_interval,
        )
        if redispatched.get("conclusion") != "success":
            raise SystemExit(
                "Cloud handoff redispatched run failed: "
                + json.dumps({"handoff": payload, "redispatched": redispatched}, indent=2)
            )
        return {"handoff": payload, "source": cancelled, "redispatched": redispatched}
    except BaseException:
        if gh_run_view(args.repo, source_id).get("status") != "completed":
            cancel_run(args.repo, source_id)
        raise


def validate_list_stuck(args: argparse.Namespace, state_dir: Path) -> dict[str, Any]:
    payload = shipyard_json(
        args.rust_bin,
        state_dir,
        [
            "cloud",
            "handoff",
            "list-stuck",
            "--threshold",
            "1s",
            "--repo",
            args.repo,
        ],
    )
    if payload.get("event") != "list-stuck":
        raise SystemExit(f"Unexpected list-stuck payload: {json.dumps(payload, indent=2)}")
    return payload


def validate_cloud_run(args: argparse.Namespace, state_dir: Path) -> dict[str, Any]:
    previous_ids = workflow_run_ids(args.repo, args.workflow, args.branch)
    payload = shipyard_json(
        args.rust_bin,
        state_dir,
        [
            "cloud",
            "run",
            args.workflow,
            args.branch,
            "--provider",
            args.provider,
            "--no-wait",
        ],
    )
    if payload.get("command") != "cloud.run":
        raise SystemExit(f"Unexpected cloud.run payload: {json.dumps(payload, indent=2)}")

    record = payload.get("record") or {}
    plan = payload.get("plan") or {}
    run_id_raw = str(record.get("run_id") or "")
    if not run_id_raw.isdecimal():
        raise SystemExit(f"Expected numeric cloud.run run_id; got {json.dumps(payload, indent=2)}")
    run_id = int(run_id_raw)
    if run_id in previous_ids:
        raise SystemExit(
            "cloud run recorded a stale pre-existing run: "
            + json.dumps({"run_id": run_id, "payload": payload}, indent=2)
        )
    if plan.get("ref") != args.branch:
        raise SystemExit(
            "cloud run planned the wrong ref: "
            + json.dumps({"expected": args.branch, "plan": plan}, indent=2)
        )
    if record.get("provider") != args.provider:
        raise SystemExit(
            "cloud run recorded the wrong provider: "
            + json.dumps({"expected": args.provider, "record": record}, indent=2)
        )

    run_state = wait_for_run(args.repo, run_id, args.timeout, args.poll_interval)
    if run_state.get("conclusion") != "success":
        raise SystemExit(
            "Cloud run live smoke failed: "
            + json.dumps({"dispatch": payload, "run": run_state}, indent=2)
        )

    status = shipyard_json(
        args.rust_bin,
        state_dir,
        [
            "cloud",
            "status",
            "latest",
            "--no-refresh",
        ],
    )
    records = status.get("records") or []
    latest = records[0] if records else {}
    if str(latest.get("run_id") or "") != str(run_id):
        raise SystemExit(
            "Cloud run durable record did not match dispatched run: "
            + json.dumps({"expected_run_id": run_id, "status": status}, indent=2)
        )
    return {"dispatch": payload, "run": run_state, "status": status}


def validate_retarget(args: argparse.Namespace, state_dir: Path) -> dict[str, Any]:
    sandbox_pr = create_sandbox_pr(args.repo, args.base)
    source_id: int | None = None
    new_run_id: int | None = None
    try:
        before_source = workflow_run_ids(args.repo, args.workflow, sandbox_pr.branch)
        run(
            [
                "gh",
                "workflow",
                "run",
                workflow_file(args.workflow),
                "--repo",
                args.repo,
                "--ref",
                sandbox_pr.branch,
                "-f",
                "runner_provider=github-hosted",
                "-f",
                "note=shipyard-retarget-source",
                "-f",
                f"sleep_seconds={args.source_sleep_seconds}",
            ]
        )
        source = wait_for_new_workflow_run(
            args.repo,
            args.workflow,
            sandbox_pr.branch,
            previous_ids=before_source,
            timeout=args.timeout,
            poll_interval=args.poll_interval,
        )
        source_id = int(source["databaseId"])
        active_jobs = wait_for_matching_active_jobs(
            args.repo,
            source_id,
            args.retarget_target,
            timeout=args.timeout,
            poll_interval=args.poll_interval,
        )
        before_retarget = workflow_run_ids(args.repo, args.workflow, sandbox_pr.branch)
        now = utc_now()
        write_ship_state(
            state_dir,
            pr=sandbox_pr.number,
            repo=args.repo,
            branch=sandbox_pr.branch,
            head_sha=sandbox_pr.head_sha,
            dispatched_runs=[
                {
                    "target": args.retarget_target,
                    "provider": "github-hosted",
                    "run_id": str(source_id),
                    "status": "in_progress",
                    "started_at": now,
                    "updated_at": now,
                    "attempt": 1,
                    "required": True,
                }
            ],
        )
        payload = shipyard_json(
            args.rust_bin,
            state_dir,
            [
                "cloud",
                "retarget",
                "--pr",
                str(sandbox_pr.number),
                "--target",
                args.retarget_target,
                "--provider",
                args.provider,
                "--workflow",
                args.workflow,
                "--apply",
            ],
        )
        new_run_id_raw = str(payload.get("new_run_id") or "")
        if not new_run_id_raw.isdecimal():
            raise SystemExit(f"Expected numeric new_run_id; got {json.dumps(payload, indent=2)}")
        new_run_id = int(new_run_id_raw)
        if new_run_id in before_retarget:
            raise SystemExit(
                "Rust retarget recorded a stale pre-existing run: "
                + json.dumps({"new_run_id": new_run_id, "payload": payload}, indent=2)
            )
        cancelled_ids = {int(job_id) for job_id in payload.get("cancelled_job_ids", [])}
        expected_ids = {int(job["databaseId"]) for job in active_jobs}
        if not expected_ids.issubset(cancelled_ids):
            raise SystemExit(
                "Rust retarget did not cancel the expected active job(s): "
                + json.dumps(
                    {
                        "expected_job_ids": sorted(expected_ids),
                        "cancelled_job_ids": sorted(cancelled_ids),
                        "payload": payload,
                    },
                    indent=2,
                )
            )
        cancelled = wait_for_run(args.repo, source_id, args.timeout, args.poll_interval)
        if cancelled.get("conclusion") != "cancelled":
            raise SystemExit(
                "Cloud retarget source run was not cancelled: "
                + json.dumps({"retarget": payload, "source": cancelled}, indent=2)
            )
        redispatched = wait_for_run(args.repo, new_run_id, args.timeout, args.poll_interval)
        if redispatched.get("conclusion") != "success":
            raise SystemExit(
                "Cloud retarget redispatched run failed: "
                + json.dumps({"retarget": payload, "redispatched": redispatched}, indent=2)
            )
        return {
            "pr": {
                "number": sandbox_pr.number,
                "url": sandbox_pr.url,
                "branch": sandbox_pr.branch,
            },
            "retarget": payload,
            "source": cancelled,
            "redispatched": redispatched,
        }
    except BaseException:
        for run_id in [source_id, new_run_id]:
            if run_id is not None and not run_is_completed(args.repo, run_id):
                cancel_run(args.repo, run_id)
        raise
    finally:
        cleanup_sandbox_pr(sandbox_pr, args.repo)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo", default="danielraffel/Shipyard")
    parser.add_argument("--branch", default="main")
    parser.add_argument("--base", default="main")
    parser.add_argument("--pr", type=int, default=900001)
    parser.add_argument("--target", default="cloud-live-smoke")
    parser.add_argument("--retarget-target", default="Cloud Live Smoke")
    parser.add_argument("--workflow", default="cloud-live-smoke")
    parser.add_argument("--provider", default="namespace")
    parser.add_argument("--rust-bin", type=Path, default=ROOT / "target" / "release" / "shipyard")
    parser.add_argument("--timeout", type=float, default=180.0)
    parser.add_argument("--poll-interval", type=float, default=5.0)
    parser.add_argument("--source-sleep-seconds", type=int, default=120)
    parser.add_argument(
        "--only",
        choices=["all", "add-lane", "cloud-run", "handoff", "list-stuck", "retarget"],
        default="all",
        help="Limit validation to one live cloud surface.",
    )
    args = parser.parse_args()

    rust_bin = args.rust_bin if args.rust_bin.is_absolute() else ROOT / args.rust_bin
    if not rust_bin.exists():
        raise SystemExit(f"Shipyard binary not found: {rust_bin}. Run `cargo build --release` first.")
    args.rust_bin = rust_bin

    run(["gh", "auth", "status"])
    run(["gh", "repo", "view", args.repo, "--json", "nameWithOwner"])
    args.head_sha = run(["git", "rev-parse", args.branch]).stdout.strip()

    with tempfile.TemporaryDirectory(prefix="shipyard-cloud-live-") as tempdir:
        state_dir = Path(tempdir) / "state"
        results: dict[str, Any] = {}
        if args.only in {"all", "add-lane"}:
            results["add_lane"] = validate_add_lane(args, state_dir)
        if args.only in {"all", "cloud-run"}:
            results["cloud_run"] = validate_cloud_run(args, state_dir)
        if args.only in {"all", "list-stuck"}:
            results["list_stuck"] = validate_list_stuck(args, state_dir)
        if args.only in {"all", "retarget"}:
            results["retarget"] = validate_retarget(args, state_dir)
        if args.only in {"all", "handoff"}:
            results["handoff"] = validate_handoff(args, state_dir)

    print(json.dumps(results, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
