#!/usr/bin/env python3
from __future__ import annotations

import json
import subprocess
import sys
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))

import update_drift_tracker


def gh_failure() -> subprocess.CalledProcessError:
    error = subprocess.CalledProcessError(1, ["gh"])
    error.stderr = "GraphQL: API rate limit already exceeded"
    return error


class UpdateDriftTrackerTests(unittest.TestCase):
    def test_merged_prs_since_falls_back_to_rest_when_graphql_is_limited(self) -> None:
        rest_payload = json.dumps(
            [
                {
                    "number": 12,
                    "title": "new behavior",
                    "merged_at": "2026-05-02T10:00:00Z",
                    "merge_commit_sha": "abc123",
                    "html_url": "https://github.com/o/r/pull/12",
                },
                {
                    "number": 11,
                    "title": "unrelated",
                    "merged_at": "2026-05-02T09:00:00Z",
                    "merge_commit_sha": "zzz999",
                    "html_url": "https://github.com/o/r/pull/11",
                },
            ]
        )

        with patch.object(
            update_drift_tracker,
            "run",
            side_effect=[gh_failure(), rest_payload],
        ):
            prs = update_drift_tracker.merged_prs_since(
                "o/r",
                "2026-05-01T00:00:00+00:00",
                {"abc123"},
            )

        self.assertEqual(
            prs,
            [
                {
                    "number": "12",
                    "title": "new behavior",
                    "merged_at": "2026-05-02T10:00:00Z",
                    "merge_sha": "abc123",
                    "url": "https://github.com/o/r/pull/12",
                }
            ],
        )

    def test_issue_watch_falls_back_to_rest_issue_and_search(self) -> None:
        issue_payload = json.dumps(
            {
                "number": 77,
                "title": "cloud handoff",
                "state": "open",
                "html_url": "https://github.com/o/r/issues/77",
                "updated_at": "2026-05-02T10:00:00Z",
                "comments": 2,
            }
        )
        search_payload = json.dumps(
            {
                "items": [
                    {
                        "number": 88,
                        "title": "follow-up",
                        "state": "open",
                        "html_url": "https://github.com/o/r/pull/88",
                        "updated_at": "2026-05-02T11:00:00Z",
                    }
                ]
            }
        )

        with patch.object(
            update_drift_tracker,
            "run",
            side_effect=[gh_failure(), issue_payload, gh_failure(), search_payload],
        ):
            data = update_drift_tracker.issue_view("o/r", "77")
            snapshot = update_drift_tracker.issue_watch_snapshot("o/r", data)

        self.assertEqual(data["state"], "OPEN")
        self.assertIn("updated at 2026-05-02T10:00:00Z", snapshot)
        self.assertIn("2 comments", snapshot)
        self.assertIn("#88 (checks unavailable via REST fallback)", snapshot)

    def test_pr_view_falls_back_to_rest(self) -> None:
        pr_payload = json.dumps(
            {
                "number": 736,
                "title": "sandbox e2e",
                "state": "closed",
                "html_url": "https://github.com/o/r/pull/736",
                "merged_at": "2026-05-02T12:00:00Z",
            }
        )

        with patch.object(
            update_drift_tracker,
            "run",
            side_effect=[gh_failure(), pr_payload],
        ):
            data = update_drift_tracker.pr_view("o/r", "736")

        self.assertEqual(data["state"], "MERGED")
        self.assertEqual(data["mergedAt"], "2026-05-02T12:00:00Z")
        self.assertEqual(data["status_summary"], "checks unavailable via REST fallback")


if __name__ == "__main__":
    unittest.main()
