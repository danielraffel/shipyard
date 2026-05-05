#!/usr/bin/env python3
"""Compare Python Shipyard and Shipyard CLI command surfaces.

The helper intentionally walks only ``--help`` output. It is safe to run
against daily binaries because it never invokes mutating command paths.
"""

from __future__ import annotations

import argparse
import json
import shutil
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path


CommandPath = tuple[str, ...]


@dataclass(frozen=True)
class HelpResult:
    path: CommandPath
    returncode: int
    stdout: str
    stderr: str

    @property
    def text(self) -> str:
        return self.stdout + self.stderr


def parse_command_names(help_text: str) -> set[str]:
    commands: set[str] = set()
    in_commands = False
    for line in help_text.splitlines():
        stripped = line.strip()
        if stripped == "Commands:":
            in_commands = True
            continue
        if not in_commands:
            continue
        if not stripped:
            break
        if stripped.startswith("-"):
            break
        name = stripped.split()[0]
        if name == "help":
            continue
        commands.add(name)
    return commands


def run_help(binary: str, path: CommandPath, cwd: Path) -> HelpResult:
    result = subprocess.run(
        [binary, *path, "--help"],
        cwd=cwd,
        check=False,
        capture_output=True,
        text=True,
        timeout=20,
    )
    return HelpResult(
        path=path,
        returncode=result.returncode,
        stdout=result.stdout,
        stderr=result.stderr,
    )


def walk_help_tree(
    binary: str,
    *,
    cwd: Path,
    max_depth: int,
) -> dict[CommandPath, set[str]]:
    tree: dict[CommandPath, set[str]] = {}

    def visit(path: CommandPath) -> None:
        result = run_help(binary, path, cwd)
        if result.returncode != 0:
            tree[path] = set()
            return
        subcommands = parse_command_names(result.text)
        tree[path] = subcommands
        if len(path) >= max_depth:
            return
        for name in sorted(subcommands):
            visit((*path, name))

    visit(())
    return tree


def flatten(tree: dict[CommandPath, set[str]]) -> set[CommandPath]:
    paths = set(tree.keys())
    for parent, children in tree.items():
        paths.update((*parent, child) for child in children)
    return paths


def format_path(path: CommandPath) -> str:
    return " ".join(path) if path else "<root>"


def compare_trees(
    python_tree: dict[CommandPath, set[str]],
    rust_tree: dict[CommandPath, set[str]],
    *,
    allowed_rust_only: set[CommandPath],
) -> dict[str, object]:
    python_paths = flatten(python_tree)
    rust_paths = flatten(rust_tree)
    missing = sorted(python_paths - rust_paths)
    rust_only = sorted(rust_paths - python_paths - allowed_rust_only)
    return {
        "missing_from_rust": [format_path(path) for path in missing],
        "rust_only": [format_path(path) for path in rust_only],
        "python_commands": [format_path(path) for path in sorted(python_paths)],
        "rust_commands": [format_path(path) for path in sorted(rust_paths)],
    }


def parse_allowed(values: list[str]) -> set[CommandPath]:
    return {tuple(value.split()) for value in values}


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--python-bin",
        default=shutil.which("shipyard") or "shipyard",
        help="Python Shipyard executable to compare",
    )
    parser.add_argument(
        "--rust-bin",
        default="target/release/shipyard",
        help="Shipyard executable to compare",
    )
    parser.add_argument("--cwd", type=Path, default=Path.cwd())
    parser.add_argument("--max-depth", type=int, default=2)
    parser.add_argument(
        "--allow-rust-only",
        action="append",
        default=["paths", "help"],
        help="Command path allowed to exist only in Rust, e.g. 'paths'",
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv or sys.argv[1:])
    rust_bin = (
        str(Path(args.rust_bin).resolve())
        if Path(args.rust_bin).exists()
        else args.rust_bin
    )
    python_bin = (
        str(Path(args.python_bin).resolve())
        if Path(args.python_bin).exists()
        else args.python_bin
    )
    python_tree = walk_help_tree(python_bin, cwd=args.cwd, max_depth=args.max_depth)
    rust_tree = walk_help_tree(rust_bin, cwd=args.cwd, max_depth=args.max_depth)
    report = compare_trees(
        python_tree,
        rust_tree,
        allowed_rust_only=parse_allowed(args.allow_rust_only),
    )
    print(json.dumps(report, indent=2, sort_keys=True))
    return 1 if report["missing_from_rust"] or report["rust_only"] else 0


if __name__ == "__main__":
    raise SystemExit(main())
