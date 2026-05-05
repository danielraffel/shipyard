#!/usr/bin/env python3
"""Benchmark Python Shipyard vs the previous Python implementation for read-heavy CLI paths.

This intentionally avoids third-party benchmark tools so the harness works
on a fresh macOS machine. It records elapsed wall time for each command and
prints p50/p95 summaries. Use release-built Rust binaries for meaningful
numbers:

    cargo build --release
    scripts/benchmark_read_paths.py --rust-bin target/release/shipyard
"""

from __future__ import annotations

import argparse
import json
import shutil
import statistics
import subprocess
import tempfile
import time
from pathlib import Path


def percentile(values: list[float], pct: float) -> float:
    if not values:
        return 0.0
    ordered = sorted(values)
    index = min(len(ordered) - 1, round((pct / 100.0) * (len(ordered) - 1)))
    return ordered[index]


def timed_run(argv: list[str], cwd: Path) -> tuple[float, int]:
    start = time.perf_counter()
    result = subprocess.run(
        argv,
        cwd=cwd,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        text=True,
        check=False,
    )
    return time.perf_counter() - start, result.returncode


def bench(label: str, argv: list[str], cwd: Path, iterations: int) -> dict[str, object]:
    samples: list[float] = []
    codes: dict[int, int] = {}
    for _ in range(iterations):
        elapsed, code = timed_run(argv, cwd)
        samples.append(elapsed)
        codes[code] = codes.get(code, 0) + 1
    return {
        "label": label,
        "argv": argv,
        "iterations": iterations,
        "return_codes": codes,
        "p50_ms": round(statistics.median(samples) * 1_000, 2),
        "p95_ms": round(percentile(samples, 95) * 1_000, 2),
        "min_ms": round(min(samples) * 1_000, 2),
        "max_ms": round(max(samples) * 1_000, 2),
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--python-bin", default=shutil.which("shipyard") or "shipyard")
    parser.add_argument("--rust-bin", default="target/release/shipyard")
    parser.add_argument("--iterations", type=int, default=10)
    parser.add_argument("--cwd", type=Path, default=Path.cwd())
    args = parser.parse_args()

    rust_bin = Path(args.rust_bin)
    if not rust_bin.is_absolute():
        rust_bin = Path.cwd() / rust_bin
    python_bin = Path(args.python_bin)
    python_argv0 = str(python_bin) if python_bin.exists() else args.python_bin

    with tempfile.TemporaryDirectory(prefix="shipyard-bench-") as state_dir:
        commands = [
            ("python:version", [python_argv0, "--version"]),
            ("rust:version", [str(rust_bin), "--version"]),
            ("python:doctor-json", [python_argv0, "--json", "doctor"]),
            (
                "rust:doctor-json",
                [str(rust_bin), "--json", "--state-dir", state_dir, "doctor"],
            ),
            ("python:ship-state-list", [python_argv0, "--json", "ship-state", "list"]),
            (
                "rust:ship-state-list",
                [str(rust_bin), "--json", "--state-dir", state_dir, "ship-state", "list"],
            ),
        ]
        results = [
            bench(label, argv, args.cwd, args.iterations)
            for label, argv in commands
        ]

    print(json.dumps({"results": results}, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
