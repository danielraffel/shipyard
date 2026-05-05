#!/usr/bin/env python3
from __future__ import annotations

import json
import socket
import subprocess
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import validate_webhook_tunnel_live as live


class ValidateWebhookTunnelLiveTests(unittest.TestCase):
    def test_parse_tailscale_status_accepts_full_funnel_cap(self) -> None:
        status = live.parse_tailscale_status(
            json.dumps(
                {
                    "BackendState": "Running",
                    "Self": {
                        "DNSName": "node.tailnet.ts.net.",
                        "CapMap": {"https://tailscale.com/cap/funnel": []},
                    },
                }
            ),
            "/usr/local/bin/tailscale",
        )

        self.assertTrue(status["ready"])
        self.assertEqual(status["dns_name"], "node.tailnet.ts.net.")
        self.assertTrue(status["funnel_permitted"])

    def test_parse_tailscale_status_accepts_short_funnel_cap(self) -> None:
        status = live.parse_tailscale_status(
            json.dumps(
                {
                    "BackendState": "Running",
                    "Self": {
                        "DNSName": "node.tailnet.ts.net",
                        "CapMap": {"funnel": []},
                    },
                }
            ),
            "/usr/local/bin/tailscale",
        )

        self.assertTrue(status["ready"])

    def test_parse_tailscale_status_requires_running_dns_and_cap(self) -> None:
        status = live.parse_tailscale_status(
            json.dumps(
                {
                    "BackendState": "NeedsLogin",
                    "Self": {
                        "DNSName": "",
                        "CapMap": {},
                    },
                }
            ),
            "/usr/local/bin/tailscale",
        )

        self.assertFalse(status["ready"])
        self.assertEqual(status["backend_state"], "NeedsLogin")
        self.assertEqual(status["dns_name"], "")
        self.assertFalse(status["funnel_permitted"])

    def test_hook_url_reads_config_url(self) -> None:
        self.assertEqual(
            live.hook_url({"config": {"url": "https://node.tailnet.ts.net/webhook"}}),
            "https://node.tailnet.ts.net/webhook",
        )
        self.assertIsNone(live.hook_url({"config": {}}))

    def test_tailscale_variant_hint_mentions_app_binary(self) -> None:
        detail = live.with_tailscale_variant_hint(
            "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
            "failed",
        )

        self.assertIn("Tailscale.app", detail)
        self.assertIn("symlink", detail)

    def test_probe_tailscale_candidates_uses_first_ready_binary(self) -> None:
        original = live.probe_tailscale
        calls: list[str] = []

        def fake_probe(candidate: str) -> dict[str, object]:
            calls.append(candidate)
            if candidate == "broken":
                raise live.ValidationError("crashed")
            return {
                "binary": candidate,
                "ready": True,
                "backend_state": "Running",
                "dns_name": "node.tailnet.ts.net",
                "funnel_permitted": True,
            }

        live.probe_tailscale = fake_probe
        try:
            status = live.probe_tailscale_candidates(["broken", "working"])
        finally:
            live.probe_tailscale = original

        self.assertEqual(calls, ["broken", "working"])
        self.assertEqual(status["binary"], "working")

    def test_report_json_shape(self) -> None:
        report = live.ValidationReport()
        report.add("one", True, "ok")
        report.add("two", False, "blocked")

        self.assertFalse(report.ok)
        self.assertEqual(
            report.to_json()["steps"],
            [
                {"name": "one", "ok": True, "detail": "ok"},
                {"name": "two", "ok": False, "detail": "blocked"},
            ],
        )

    def test_wait_for_ipc_event_reads_line_delimited_event(self) -> None:
        left, right = socket.socketpair()
        try:
            right.sendall(
                b'{"type":"status"}\n{"type":"event","kind":"unhandled"}\n'
            )
            event = live.wait_for_ipc_event(left, 1.0)
        finally:
            left.close()
            right.close()

        self.assertEqual(event["kind"], "unhandled")

    def test_wait_for_ipc_event_times_out_cleanly(self) -> None:
        left, right = socket.socketpair()
        try:
            with self.assertRaisesRegex(
                live.ValidationError,
                "no daemon IPC webhook event observed",
            ):
                live.wait_for_ipc_event(left, 0.01)
        finally:
            left.close()
            right.close()

    def test_wait_for_ping_event_sends_ping_and_reads_event(self) -> None:
        left, right = socket.socketpair()
        original = live.ping_hook
        calls: list[tuple[str, int]] = []

        def fake_ping(repo: str, hook_id: int) -> None:
            calls.append((repo, hook_id))
            right.sendall(b'{"type":"event","kind":"unhandled"}\n')

        live.ping_hook = fake_ping
        try:
            event = live.wait_for_ping_event(
                "owner/repo",
                123,
                left,
                1.0,
                ping_interval=0.1,
            )
        finally:
            live.ping_hook = original
            left.close()
            right.close()

        self.assertEqual(calls, [("owner/repo", 123)])
        self.assertEqual(event["kind"], "unhandled")

    def test_wait_for_ping_event_reports_ping_attempts_on_timeout(self) -> None:
        left, right = socket.socketpair()
        original = live.ping_hook
        calls: list[tuple[str, int]] = []

        def fake_ping(repo: str, hook_id: int) -> None:
            calls.append((repo, hook_id))

        live.ping_hook = fake_ping
        try:
            with self.assertRaisesRegex(
                live.ValidationError,
                r"after \d+ ping attempt",
            ):
                live.wait_for_ping_event(
                    "owner/repo",
                    123,
                    left,
                    0.05,
                    ping_interval=0.01,
                )
        finally:
            live.ping_hook = original
            left.close()
            right.close()

        self.assertGreaterEqual(len(calls), 1)

    def test_probe_public_webhook_accepts_method_not_allowed(self) -> None:
        original = live.run

        def fake_run(
            cmd: list[str],
            *,
            cwd: Path = live.ROOT,
            env: object | None = None,
            input_text: str | None = None,
            timeout: float = 30.0,
        ) -> subprocess.CompletedProcess[str]:
            return subprocess.CompletedProcess(cmd, 0, stdout="405 0.012345", stderr="")

        live.run = fake_run
        try:
            detail = live.probe_public_webhook("https://node.tailnet.ts.net/webhook")
        finally:
            live.run = original

        self.assertEqual(detail, "http=405 duration=0.012345s")

    def test_wait_for_public_webhook_retries_until_ready(self) -> None:
        original = live.probe_public_webhook
        calls = 0

        def fake_probe(url: str) -> str:
            nonlocal calls
            calls += 1
            if calls == 1:
                raise live.ValidationError("not ready")
            return "http=405 duration=0.1s"

        live.probe_public_webhook = fake_probe
        try:
            detail = live.wait_for_public_webhook(
                "https://node.tailnet.ts.net/webhook",
                3.0,
            )
        finally:
            live.probe_public_webhook = original

        self.assertEqual(detail, "http=405 duration=0.1s")
        self.assertEqual(calls, 2)

    def test_summarize_hook_deliveries_includes_delivery_status(self) -> None:
        summary = live.summarize_hook_deliveries(
            [
                {
                    "guid": "delivery-1",
                    "event": "ping",
                    "status": "OK",
                    "status_code": 200,
                    "duration": 0.2,
                    "delivered_at": "2026-05-05T00:00:00Z",
                }
            ]
        )

        self.assertIn("delivery-1", summary)
        self.assertIn("event=ping", summary)
        self.assertIn("code=200", summary)

    def test_summarize_hook_delivery_details_handles_empty_ids(self) -> None:
        summary = live.summarize_hook_delivery_details(
            "owner/repo",
            123,
            [{"guid": "missing-id"}],
        )

        self.assertEqual(summary, "no delivery details available")

    def test_funnel_status_helpers_detect_clear_and_active_configs(self) -> None:
        self.assertTrue(live.funnel_status_is_clear("No serve config\n"))
        self.assertFalse(
            live.funnel_status_is_clear(
                "\n# Funnel on:\nhttps://node.ts.net\n|-- / proxy http://127.0.0.1:1234"
            )
        )
        self.assertEqual(
            live.summarize_funnel_status("\n# Funnel on:\nhttps://node.ts.net\n"),
            "# Funnel on:",
        )

    def test_reset_funnel_falls_back_to_serve_reset_for_app_store_state(self) -> None:
        original = live.run
        calls: list[list[str]] = []
        status_calls = 0

        def fake_run(
            cmd: list[str],
            *,
            env: object | None = None,
            timeout: float | None = None,
            check: bool = False,
        ) -> subprocess.CompletedProcess[str]:
            nonlocal status_calls
            calls.append(cmd)
            if cmd[1:] == ["funnel", "status"]:
                status_calls += 1
                stdout = "still serving" if status_calls == 1 else "No serve config"
                return subprocess.CompletedProcess(cmd, 0, stdout=stdout, stderr="")
            return subprocess.CompletedProcess(cmd, 0, stdout="", stderr="")

        live.run = fake_run
        try:
            status = live.reset_funnel("/Applications/Tailscale.app/Contents/MacOS/Tailscale")
        finally:
            live.run = original

        self.assertEqual(status, "No serve config")
        self.assertEqual(
            calls,
            [
                ["/Applications/Tailscale.app/Contents/MacOS/Tailscale", "funnel", "reset"],
                ["/Applications/Tailscale.app/Contents/MacOS/Tailscale", "funnel", "status"],
                ["/Applications/Tailscale.app/Contents/MacOS/Tailscale", "serve", "reset"],
                ["/Applications/Tailscale.app/Contents/MacOS/Tailscale", "funnel", "status"],
            ],
        )


if __name__ == "__main__":
    unittest.main()
