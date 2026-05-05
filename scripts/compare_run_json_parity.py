#!/usr/bin/env python3
"""Compare Python and Rust `shipyard run` JSON/error parity.

The harness creates disposable git repos and points HOME/state at temp
directories so it does not touch the user's daily Shipyard install,
config, daemon, or state.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_PYTHON_BIN = Path("/Users/danielraffel/Code/shipyard/.venv/bin/shipyard")
DEFAULT_RUST_BIN = ROOT / "target" / "release" / "shipyard"


@dataclass(frozen=True)
class Scenario:
    name: str
    config: str
    args: tuple[str, ...] = ()
    expect_json: bool = True


SCENARIOS = (
    Scenario(
        name="pass_staged",
        config="""
[validation.default]
setup = "true"
build = "true"

[targets.mac]
backend = "local"
platform = "macos-arm64"
cwd = "{repo}"
""",
    ),
    Scenario(
        name="tree_drift",
        config="""
[validation.default]
setup = "printf changed > source.txt"
build = "echo SHOULD_NOT_RUN"

[targets.mac]
backend = "local"
platform = "macos-arm64"
cwd = "{repo}"
""",
    ),
    Scenario(
        name="allow_tree_drift",
        config="""
[validation.default]
setup = "printf changed > source.txt"
build = "true"

[targets.mac]
backend = "local"
platform = "macos-arm64"
cwd = "{repo}"
""",
        args=("--allow-tree-drift",),
    ),
    Scenario(
        name="skip_target_report",
        config="""
[validation.default]
command = "true"

[targets.mac]
backend = "local"
platform = "macos-arm64"
cwd = "{repo}"

[targets.linux]
backend = "local"
platform = "linux-x64"
cwd = "{repo}"
""",
        args=("--skip-target", "linux"),
    ),
    Scenario(
        name="skip_outside_requested",
        config="""
[validation.default]
command = "true"

[targets.mac]
backend = "local"
platform = "macos-arm64"
cwd = "{repo}"

[targets.linux]
backend = "local"
platform = "linux-x64"
cwd = "{repo}"
""",
        args=("--targets", "mac", "--skip-target", "linux"),
        expect_json=False,
    ),
)


def run(args: list[str], *, cwd: Path, env: dict[str, str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        args,
        cwd=cwd,
        env=env,
        text=True,
        capture_output=True,
        check=False,
    )


def git(args: list[str], cwd: Path) -> None:
    env = os.environ.copy()
    env.update(
        {
            "GIT_AUTHOR_NAME": "Shipyard Test",
            "GIT_AUTHOR_EMAIL": "shipyard-test@example.invalid",
            "GIT_COMMITTER_NAME": "Shipyard Test",
            "GIT_COMMITTER_EMAIL": "shipyard-test@example.invalid",
        }
    )
    subprocess.run(
        ["git", *args],
        cwd=cwd,
        env=env,
        check=True,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )


def seed_repo(root: Path, config_template: str) -> None:
    root.mkdir(parents=True)
    git(["init", "-q", "--initial-branch=main"], root)
    (root / "source.txt").write_text("initial\n", encoding="utf-8")
    (root / ".shipyard").mkdir()
    (root / ".shipyard" / "config.toml").write_text(
        config_template.format(repo=root),
        encoding="utf-8",
    )
    git(["add", "."], root)
    git(["commit", "-qm", "initial"], root)
    git(["checkout", "-qb", "feature"], root)


def command_summary(proc: subprocess.CompletedProcess[str], expect_json: bool) -> dict[str, Any]:
    if not expect_json:
        output = "\n".join(
            part.strip() for part in (proc.stdout, proc.stderr) if part.strip()
        )
        if output.startswith("error: "):
            output = output.removeprefix("error: ")
        return {
            "code": proc.returncode,
            "message": output,
        }
    data = json.loads(proc.stdout)
    results = data["run"].get("results", {})
    normalized_results = {
        name: {
            "status": result.get("status"),
            "phase": result.get("phase"),
            "failure_class": result.get("failure_class"),
            "error_prefix": (result.get("error_message") or "").splitlines()[:4],
            "keys": sorted(result.keys()),
        }
        for name, result in sorted(results.items())
    }
    preflight = data.get("preflight", {})
    return {
        "code": proc.returncode,
        "command": data.get("command"),
        "overall": data["run"].get("overall"),
        "targets": data["run"].get("targets"),
        "results": normalized_results,
        "preflight_keys": sorted(preflight.keys()),
        "preflight_skipped_targets": preflight.get("skipped_targets"),
        "preflight_warning_prefixes": [
            str(warning).splitlines()[0] for warning in preflight.get("warnings", [])
        ],
        "preflight_target_keys": {
            name: sorted(target.keys())
            for name, target in sorted((preflight.get("targets") or {}).items())
        },
    }


def compare_scenario(
    scenario: Scenario,
    *,
    python_bin: Path,
    rust_bin: Path,
    temp_root: Path,
) -> dict[str, Any]:
    home = temp_root / "home"
    home.mkdir(exist_ok=True)
    env = os.environ.copy()
    env["HOME"] = str(home)
    env.pop("SHIPYARD_NO_WARM_POOL", None)

    py_repo = temp_root / scenario.name / "python"
    rust_repo = temp_root / scenario.name / "rust"
    seed_repo(py_repo, scenario.config)
    seed_repo(rust_repo, scenario.config)

    py = run(
        [str(python_bin), "--json", "run", "--no-warm", *scenario.args],
        cwd=py_repo,
        env=env,
    )
    rust = run(
        [
            str(rust_bin),
            "--json",
            "--state-dir",
            str(temp_root / scenario.name / "rust-state"),
            "--global-dir",
            str(temp_root / scenario.name / "rust-global"),
            "--cwd",
            str(rust_repo),
            "run",
            "--no-warm",
            *scenario.args,
        ],
        cwd=rust_repo,
        env=env,
    )
    python_summary = command_summary(py, scenario.expect_json)
    rust_summary = command_summary(rust, scenario.expect_json)
    return {
        "scenario": scenario.name,
        "match": python_summary == rust_summary,
        "python": python_summary,
        "rust": rust_summary,
    }


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--python-bin", type=Path, default=DEFAULT_PYTHON_BIN)
    parser.add_argument("--rust-bin", type=Path, default=DEFAULT_RUST_BIN)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    python_bin = args.python_bin
    rust_bin = args.rust_bin
    if not python_bin.is_absolute():
        python_bin = Path.cwd() / python_bin
    if not rust_bin.is_absolute():
        rust_bin = Path.cwd() / rust_bin
    with tempfile.TemporaryDirectory(prefix="shipyard-run-parity-") as temp:
        results = [
            compare_scenario(
                scenario,
                python_bin=python_bin,
                rust_bin=rust_bin,
                temp_root=Path(temp),
            )
            for scenario in SCENARIOS
        ]
    print(json.dumps({"results": results}, indent=2))
    return 0 if all(result["match"] for result in results) else 1


if __name__ == "__main__":
    sys.exit(main())
