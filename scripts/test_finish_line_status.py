#!/usr/bin/env python3
from __future__ import annotations

import json
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import finish_line_status


class FinishLineStatusTests(unittest.TestCase):
    def test_parse_secret_list_detects_release_bot_token(self) -> None:
        probe = finish_line_status.parse_secret_list(
            json.dumps(
                [
                    {
                        "name": "RELEASE_BOT_TOKEN",
                        "updatedAt": "2026-04-25T21:00:00Z",
                    }
                ]
            ),
            "RELEASE_BOT_TOKEN",
        )

        self.assertTrue(probe.present)
        self.assertEqual(probe.updated_at, "2026-04-25T21:00:00Z")
        self.assertIsNone(probe.error)

    def test_parse_secret_list_reports_missing_without_error(self) -> None:
        probe = finish_line_status.parse_secret_list("[]", "RELEASE_BOT_TOKEN")

        self.assertFalse(probe.present)
        self.assertIsNone(probe.error)

    def test_build_report_lists_credential_blockers(self) -> None:
        report = finish_line_status.build_report(
            repo="owner/repo",
            environ={},
            secret_probe=finish_line_status.SecretProbe(False),
        )

        self.assertFalse(report["ready"])
        self.assertIn("configure repo secret RELEASE_BOT_TOKEN", report["blockers"][0])
        self.assertEqual(
            report["signing"]["missing"],
            list(finish_line_status.SIGNING_ENV),
        )

    def test_build_report_ready_when_secret_and_env_are_present(self) -> None:
        environ = {name: "set" for name in finish_line_status.SIGNING_ENV}
        environ["RELEASE_BOT_TOKEN"] = "set"

        report = finish_line_status.build_report(
            repo="owner/repo",
            environ=environ,
            secret_probe=finish_line_status.SecretProbe(
                True, updated_at="2026-04-25T21:00:00Z"
            ),
        )

        self.assertTrue(report["ready"])
        self.assertEqual(report["blockers"], [])
        self.assertTrue(report["release_bot"]["repo_secret_present"])

    def test_build_report_can_include_safe_signing_sources(self) -> None:
        environ = {name: "set" for name in finish_line_status.SIGNING_ENV}
        sources = {"SHIPYARD_NOTARIZE_APPLE_ID": "dotenv:APPLE_ID"}

        report = finish_line_status.build_report(
            repo="owner/repo",
            environ=environ,
            secret_probe=finish_line_status.SecretProbe(True),
            signing_sources=sources,
            env_file={"path": "/tmp/.env", "loaded": True, "keys_loaded": 4},
        )

        self.assertTrue(report["ready"])
        self.assertEqual(report["signing"]["sources"], sources)
        self.assertEqual(report["env_file"]["path"], "/tmp/.env")


if __name__ == "__main__":
    unittest.main()
