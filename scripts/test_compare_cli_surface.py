#!/usr/bin/env python3
from __future__ import annotations

import unittest

import compare_cli_surface


class CompareCliSurfaceTests(unittest.TestCase):
    def test_parse_command_names_handles_click_and_clap_blocks(self) -> None:
        click_help = """
Usage: shipyard [OPTIONS] COMMAND [ARGS]...

Commands:
  doctor  Check environment.
  cloud   Dispatch workflows.

Options:
  --help  Show this message and exit.
"""
        clap_help = """
Usage: shipyard [OPTIONS] <COMMAND>

Commands:
  doctor  Check environment
  paths   Print paths
  help    Print this message
"""

        self.assertEqual(
            compare_cli_surface.parse_command_names(click_help),
            {"doctor", "cloud"},
        )
        self.assertEqual(
            compare_cli_surface.parse_command_names(clap_help),
            {"doctor", "paths"},
        )

    def test_compare_trees_reports_missing_and_allowed_rust_only(self) -> None:
        python_tree = {
            (): {"doctor", "release-bot"},
            ("doctor",): set(),
            ("release-bot",): {"status"},
        }
        rust_tree = {
            (): {"doctor", "paths", "help"},
            ("doctor",): set(),
            ("paths",): set(),
            ("help",): set(),
        }

        report = compare_cli_surface.compare_trees(
            python_tree,
            rust_tree,
            allowed_rust_only={("paths",), ("help",)},
        )

        self.assertEqual(
            report["missing_from_rust"],
            ["release-bot", "release-bot status"],
        )
        self.assertEqual(report["rust_only"], [])


if __name__ == "__main__":
    unittest.main()
