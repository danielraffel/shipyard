#!/usr/bin/env python3
"""Refresh the upstream Shipyard drift tracker report.

This keeps the previous Python implementation honest while Python Shipyard keeps moving.
It compares the last reviewed upstream mainline commit to the current
`origin/main`, resolves merged PRs in that commit range, and records any
explicit watchlist items that should be monitored before they land.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from collections import Counter
from datetime import UTC, datetime
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_STATE_PATH = ROOT / "planning" / "drift-tracker.json"
DEFAULT_REPORT_PATH = ROOT / "planning" / "upstream-drift.md"


def run(
    args: list[str],
    *,
    cwd: Path | None = None,
) -> str:
    """Run a subprocess and return trimmed stdout."""

    result = subprocess.run(
        args,
        cwd=cwd,
        check=True,
        capture_output=True,
        text=True,
    )
    return result.stdout.strip()


def warn(message: str) -> None:
    """Write a non-fatal diagnostic to stderr."""

    print(f"warning: {message}", file=sys.stderr)


def read_json(path: Path) -> dict[str, Any]:
    """Load a JSON file."""

    return json.loads(path.read_text(encoding="utf-8"))


def write_json(path: Path, payload: dict[str, Any]) -> None:
    """Write a JSON file with stable formatting."""

    path.write_text(
        json.dumps(payload, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )


def git_ref_name(state: dict[str, Any]) -> str:
    """Return the fully-qualified upstream ref to inspect."""

    upstream = state["upstream"]
    return f'{upstream["remote"]}/{upstream["branch"]}'


def fetch_upstream(state: dict[str, Any]) -> None:
    """Fetch the configured upstream remote."""

    upstream = state["upstream"]
    repo_path = Path(upstream["repo_path"])
    run(["git", "fetch", upstream["remote"]], cwd=repo_path)


def git_head(repo_path: Path, ref_name: str) -> dict[str, str]:
    """Return commit metadata for a git ref."""

    return {
        "sha": run(["git", "rev-parse", ref_name], cwd=repo_path),
        "committed_at": run(
            ["git", "show", "-s", "--format=%cI", ref_name],
            cwd=repo_path,
        ),
        "subject": run(
            ["git", "show", "-s", "--format=%s", ref_name],
            cwd=repo_path,
        ),
    }


def git_commits_since(
    repo_path: Path,
    baseline_sha: str,
    ref_name: str,
) -> list[dict[str, str]]:
    """List commits in `baseline_sha..ref_name`, oldest first."""

    output = run(
        [
            "git",
            "log",
            "--reverse",
            "--format=%H%x09%cI%x09%s",
            f"{baseline_sha}..{ref_name}",
        ],
        cwd=repo_path,
    )
    commits: list[dict[str, str]] = []
    if not output:
        return commits
    for line in output.splitlines():
        sha, committed_at, subject = line.split("\t", 2)
        commits.append(
            {
                "sha": sha,
                "committed_at": committed_at,
                "subject": subject,
            }
        )
    return commits


def merged_prs_since(
    github_repo: str,
    baseline_reviewed_at: str,
    commit_shas: set[str],
) -> list[dict[str, str]]:
    """Resolve merged PRs whose merge commits are in the git commit set."""

    if not commit_shas:
        return []

    merged_since = baseline_reviewed_at.split("T", maxsplit=1)[0]
    try:
        payload = run(
            [
                "gh",
                "pr",
                "list",
                "--repo",
                github_repo,
                "--state",
                "merged",
                "--limit",
                "200",
                "--search",
                f"merged:>={merged_since}",
                "--json",
                "number,title,mergedAt,mergeCommit,url",
            ]
        )
        candidates = json.loads(payload)
    except subprocess.CalledProcessError as error:
        warn(
            "gh pr list failed; falling back to REST pulls API "
            f"({format_subprocess_error(error)})"
        )
        return merged_prs_since_rest(github_repo, baseline_reviewed_at, commit_shas)

    prs: list[dict[str, str]] = []
    for item in candidates:
        merge_commit = item.get("mergeCommit") or {}
        sha = merge_commit.get("oid")
        if sha not in commit_shas:
            continue
        prs.append(
            {
                "number": str(item["number"]),
                "title": item["title"],
                "merged_at": item["mergedAt"],
                "merge_sha": sha,
                "url": item["url"],
            }
        )
    prs.sort(key=lambda item: item["merged_at"])
    return prs


def format_subprocess_error(error: subprocess.CalledProcessError) -> str:
    """Return a compact subprocess failure summary."""

    stderr = (error.stderr or "").strip()
    stdout = (error.stdout or "").strip()
    detail = stderr or stdout or f"exit {error.returncode}"
    return " ".join(detail.split())


def merged_prs_since_rest(
    github_repo: str,
    baseline_reviewed_at: str,
    commit_shas: set[str],
) -> list[dict[str, str]]:
    """Resolve merged PRs through GitHub's REST API.

    `gh pr list --json` uses GitHub GraphQL, which can be exhausted even
    when the REST/core bucket is healthy. The drift tracker should still
    work in that state because cutover parity checks are operationally
    important and read-only.
    """

    reviewed_at = parse_datetime(baseline_reviewed_at)
    prs: list[dict[str, str]] = []
    for page in range(1, 6):
        payload = run(
            [
                "gh",
                "api",
                "-X",
                "GET",
                f"repos/{github_repo}/pulls",
                "-f",
                "state=closed",
                "-f",
                "sort=updated",
                "-f",
                "direction=desc",
                "-F",
                "per_page=100",
                "-F",
                f"page={page}",
            ]
        )
        items = json.loads(payload)
        if not items:
            break
        for item in items:
            merged_at = item.get("merged_at")
            if not merged_at:
                continue
            if parse_datetime(merged_at) < reviewed_at:
                continue
            sha = item.get("merge_commit_sha")
            if sha not in commit_shas:
                continue
            prs.append(
                {
                    "number": str(item["number"]),
                    "title": item["title"],
                    "merged_at": merged_at,
                    "merge_sha": sha,
                    "url": item["html_url"],
                }
            )
        if len(items) < 100:
            break
    prs.sort(key=lambda item: item["merged_at"])
    return prs


def parse_datetime(value: str) -> datetime:
    """Parse GitHub/Git ISO datetimes into aware UTC datetimes."""

    normalized = value.replace("Z", "+00:00")
    parsed = datetime.fromisoformat(normalized)
    if parsed.tzinfo is None:
        return parsed.replace(tzinfo=UTC)
    return parsed.astimezone(UTC)


def watch_status_rollup(summary: list[dict[str, Any]]) -> str:
    """Compress a GitHub status rollup into a single human line."""

    if not summary:
        return "no checks reported"

    counts = Counter()
    for item in summary:
        status = (item.get("status") or "").lower()
        conclusion = (item.get("conclusion") or "").lower()
        if status in {"queued", "in_progress"}:
            counts[status] += 1
        elif conclusion:
            counts[conclusion] += 1
        else:
            counts["unknown"] += 1

    ordered_keys = [
        "failure",
        "cancelled",
        "timed_out",
        "action_required",
        "startup_failure",
        "queued",
        "in_progress",
        "success",
        "skipped",
        "neutral",
        "unknown",
    ]
    parts = [f"{counts[key]} {key}" for key in ordered_keys if counts[key]]
    return ", ".join(parts)


def issue_watch_snapshot(
    github_repo: str,
    issue: dict[str, Any],
) -> str:
    """Summarize issue activity plus any open PRs that reference it."""

    comments = issue.get("comments") or []
    parts = [
        f'updated at {issue["updatedAt"]}',
        f"{len(comments)} comment{'s' if len(comments) != 1 else ''}",
    ]
    related_prs = open_prs_referencing_issue(github_repo, str(issue["number"]))
    if related_prs:
        labels = []
        for pr in related_prs:
            checks = pr.get("status_summary") or watch_status_rollup(
                pr.get("statusCheckRollup") or []
            )
            labels.append(f'#{pr["number"]} ({checks})')
        parts.append("open PRs: " + ", ".join(labels))
    return "; ".join(parts)


def open_prs_referencing_issue(
    github_repo: str,
    issue_number: str,
) -> list[dict[str, Any]]:
    """Find open PRs whose title/body references a watched issue."""

    try:
        payload = run(
            [
                "gh",
                "pr",
                "list",
                "--repo",
                github_repo,
                "--state",
                "open",
                "--limit",
                "50",
                "--search",
                f"{issue_number} in:title,body",
                "--json",
                "number,title,state,url,updatedAt,statusCheckRollup",
            ]
        )
        return json.loads(payload)
    except subprocess.CalledProcessError as error:
        warn(
            "gh pr list failed for issue watch; falling back to REST search "
            f"({format_subprocess_error(error)})"
        )
        return open_prs_referencing_issue_rest(github_repo, issue_number)


def open_prs_referencing_issue_rest(
    github_repo: str,
    issue_number: str,
) -> list[dict[str, Any]]:
    """Find open PRs referencing an issue through REST search."""

    payload = run(
        [
            "gh",
            "api",
            "-X",
            "GET",
            "search/issues",
            "-f",
            f"q=repo:{github_repo} is:pr is:open {issue_number} in:title,body",
            "-F",
            "per_page=50",
        ]
    )
    data = json.loads(payload)
    prs: list[dict[str, Any]] = []
    for item in data.get("items", []):
        prs.append(
            {
                "number": item["number"],
                "title": item["title"],
                "state": item["state"].upper(),
                "url": item["html_url"],
                "updatedAt": item["updated_at"],
                "status_summary": "checks unavailable via REST fallback",
            }
        )
    return prs


def watch_item_repo(state: dict[str, Any], item: dict[str, Any]) -> str:
    """Return the GitHub repo for a watch item.

    Watch items default to the upstream Shipyard repo, but cross-repo
    follow-ups such as Pulp consumer validation can opt into their own
    `github_repo` without changing the primary drift window.
    """

    return item.get("github_repo") or state["upstream"]["github_repo"]


def watch_items(state: dict[str, Any]) -> list[dict[str, str]]:
    """Resolve configured upstream watch items."""

    items: list[dict[str, str]] = []

    for item in state.get("watch_items", []):
        kind = item["kind"]
        number = str(item["number"])
        github_repo = watch_item_repo(state, item)
        if kind == "pr":
            data = pr_view(github_repo, number)
            snapshot = data.get("status_summary") or watch_status_rollup(
                data.get("statusCheckRollup") or []
            )
            if data.get("mergedAt"):
                snapshot = f'merged at {data["mergedAt"]}; {snapshot}'
            items.append(
                {
                    "item": f'{github_repo} PR #{data["number"]}',
                    "title": data["title"],
                    "state": data["state"],
                    "snapshot": snapshot,
                    "why": item["why"],
                    "url": data["url"],
                }
            )
        elif kind == "issue":
            data = issue_view(github_repo, number)
            items.append(
                {
                    "item": f'{github_repo} Issue #{data["number"]}',
                    "title": data["title"],
                    "state": data["state"],
                    "snapshot": issue_watch_snapshot(github_repo, data),
                    "why": item["why"],
                    "url": data["url"],
                }
            )
        else:
            raise ValueError(f"unsupported watch item kind: {kind}")

    return items


def pr_view(github_repo: str, number: str) -> dict[str, Any]:
    """Read one PR, falling back to REST when GraphQL is unavailable."""

    try:
        payload = run(
            [
                "gh",
                "pr",
                "view",
                number,
                "--repo",
                github_repo,
                "--json",
                "number,title,state,url,mergedAt,statusCheckRollup",
            ]
        )
        return json.loads(payload)
    except subprocess.CalledProcessError as error:
        warn(
            f"gh pr view failed for {github_repo}#{number}; falling back to REST "
            f"({format_subprocess_error(error)})"
        )
        payload = run(["gh", "api", f"repos/{github_repo}/pulls/{number}"])
        data = json.loads(payload)
        return {
            "number": data["number"],
            "title": data["title"],
            "state": "MERGED" if data.get("merged_at") else data["state"].upper(),
            "url": data["html_url"],
            "mergedAt": data.get("merged_at"),
            "status_summary": "checks unavailable via REST fallback",
        }


def issue_view(github_repo: str, number: str) -> dict[str, Any]:
    """Read one issue, falling back to REST when GraphQL is unavailable."""

    try:
        payload = run(
            [
                "gh",
                "issue",
                "view",
                number,
                "--repo",
                github_repo,
                "--json",
                "number,title,state,url,updatedAt,comments",
            ]
        )
        return json.loads(payload)
    except subprocess.CalledProcessError as error:
        warn(
            f"gh issue view failed for {github_repo}#{number}; falling back to REST "
            f"({format_subprocess_error(error)})"
        )
        payload = run(["gh", "api", f"repos/{github_repo}/issues/{number}"])
        data = json.loads(payload)
        return {
            "number": data["number"],
            "title": data["title"],
            "state": data["state"].upper(),
            "url": data["html_url"],
            "updatedAt": data["updated_at"],
            "comments": [None] * int(data.get("comments") or 0),
        }


def render_report(
    state: dict[str, Any],
    head: dict[str, str],
    commits: list[dict[str, str]],
    prs: list[dict[str, str]],
    watchers: list[dict[str, str]],
) -> str:
    """Render the markdown report."""

    generated_at = datetime.now(UTC).replace(microsecond=0).isoformat()
    baseline = state["baseline"]
    pr_shas = {item["merge_sha"] for item in prs}
    direct_commits = [commit for commit in commits if commit["sha"] not in pr_shas]

    lines = [
        "# Upstream Drift Tracker",
        "",
        "_Generated by `scripts/update_drift_tracker.py`. Do not edit this file by hand._",
        "",
        f"- Generated at: `{generated_at}`",
        f'- Source repo: `{state["upstream"]["repo_path"]}`',
        f'- Tracking ref: `{git_ref_name(state)}`',
        f'- Last reviewed commit: `{baseline["commit"]}` ({baseline["reviewed_at"]})',
        f'- Current upstream head: `{head["sha"]}` ({head["committed_at"]})',
        "",
        "## Summary",
        "",
        f"- `{len(prs)}` merged PRs landed since the last reviewed commit",
        f"- `{len(direct_commits)}` direct commits in the range without a merged-PR match",
        f"- `{len(watchers)}` explicit upstream watch items are being monitored",
        "",
        "## Landed Since Last Review",
        "",
    ]

    if prs:
        lines.extend(
            [
                "| Merged At | PR | Title | Merge Commit |",
                "| --- | --- | --- | --- |",
            ]
        )
        for item in prs:
            lines.append(
                f'| {item["merged_at"]} | [#{item["number"]}]({item["url"]}) | '
                f'{item["title"]} | `{item["merge_sha"][:8]}` |'
            )
    else:
        lines.append("No merged PR drift since the last reviewed commit.")

    lines.extend(["", "## Direct Commits Without PR Match", ""])
    if direct_commits:
        lines.extend(
            [
                "| Commit | Date | Subject |",
                "| --- | --- | --- |",
            ]
        )
        for commit in direct_commits:
            lines.append(
                f'| `{commit["sha"][:8]}` | {commit["committed_at"]} | {commit["subject"]} |'
            )
    else:
        lines.append("No unmatched direct commits in the current drift window.")

    lines.extend(["", "## Watchlist", ""])
    if watchers:
        lines.extend(
            [
                "| Item | State | Snapshot | Why It Matters |",
                "| --- | --- | --- | --- |",
            ]
        )
        for item in watchers:
            label = f'[{item["item"]}]({item["url"]}) {item["title"]}'
            snapshot = item["snapshot"] or "-"
            lines.append(
                f'| {label} | {item["state"]} | {snapshot} | {item["why"]} |'
            )
    else:
        lines.append("No explicit watchlist items configured.")

    lines.extend(
        [
            "",
            "## Review Loop",
            "",
            "1. Run `scripts/update_drift_tracker.py` to refresh this report.",
            "2. Port or defer anything relevant in `planning/feature-audit.md` and `planning/parity-matrix.md`.",
            "3. After the report is fully triaged, run `scripts/update_drift_tracker.py --mark-reviewed` to advance the baseline.",
        ]
    )

    return "\n".join(lines) + "\n"


def refresh(state_path: Path, report_path: Path) -> None:
    """Refresh the report from the current upstream state."""

    state = read_json(state_path)
    repo_path = Path(state["upstream"]["repo_path"])
    fetch_upstream(state)
    ref_name = git_ref_name(state)
    head = git_head(repo_path, ref_name)
    commits = git_commits_since(repo_path, state["baseline"]["commit"], ref_name)
    prs = merged_prs_since(
        state["upstream"]["github_repo"],
        state["baseline"]["reviewed_at"],
        {commit["sha"] for commit in commits},
    )
    watchers = watch_items(state)
    report_path.write_text(
        render_report(state, head, commits, prs, watchers),
        encoding="utf-8",
    )


def mark_reviewed(state_path: Path) -> None:
    """Advance the baseline to the current upstream head."""

    state = read_json(state_path)
    repo_path = Path(state["upstream"]["repo_path"])
    fetch_upstream(state)
    ref_name = git_ref_name(state)
    head = git_head(repo_path, ref_name)
    state["baseline"]["commit"] = head["sha"]
    state["baseline"]["reviewed_at"] = datetime.now(UTC).replace(
        microsecond=0
    ).isoformat()
    write_json(state_path, state)


def parse_args() -> argparse.Namespace:
    """Parse CLI arguments."""

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--state",
        type=Path,
        default=DEFAULT_STATE_PATH,
        help="path to the drift tracker state JSON",
    )
    parser.add_argument(
        "--report",
        type=Path,
        default=DEFAULT_REPORT_PATH,
        help="path to the generated markdown report",
    )
    parser.add_argument(
        "--mark-reviewed",
        action="store_true",
        help="advance the baseline to the current upstream head after refresh",
    )
    return parser.parse_args()


def main() -> int:
    """Program entrypoint."""

    args = parse_args()
    refresh(args.state, args.report)
    if args.mark_reviewed:
        mark_reviewed(args.state)
        refresh(args.state, args.report)
    return 0


if __name__ == "__main__":
    sys.exit(main())
