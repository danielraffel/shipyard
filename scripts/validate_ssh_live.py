#!/usr/bin/env python3
"""Live validation for Rust POSIX SSH and SSH-Windows executors.

The harness creates throwaway remote git repositories, runs
`shipyard run` against them from an isolated temp clone/state root, and
then removes the remote temp directories. It intentionally avoids consumer
project checkouts such as Pulp's real validation directories.
"""

from __future__ import annotations

import argparse
import base64
import json
import os
import shlex
import shutil
import subprocess
import sys
import tempfile
import uuid
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SSH_OPTIONS = ["-o", "ConnectTimeout=10", "-o", "BatchMode=yes"]


def run(
    args: list[str],
    *,
    cwd: Path | None = None,
    env: dict[str, str] | None = None,
    timeout: int = 60,
    check: bool = True,
) -> subprocess.CompletedProcess[str]:
    result = subprocess.run(
        args,
        cwd=cwd,
        env=env,
        check=False,
        capture_output=True,
        text=True,
        timeout=timeout,
    )
    if check and result.returncode != 0:
        raise SystemExit(
            f"command failed ({result.returncode}): {' '.join(args)}\n"
            f"stdout:\n{result.stdout}\n"
            f"stderr:\n{result.stderr}"
        )
    return result


def toml(value: str) -> str:
    return json.dumps(value)


def ps_quote(value: str) -> str:
    return "'" + value.replace("'", "''") + "'"


def ssh(host: str, remote_command: str, *, timeout: int = 60) -> subprocess.CompletedProcess[str]:
    return run(["ssh", *SSH_OPTIONS, host, remote_command], timeout=timeout)


def powershell(
    host: str,
    script: str,
    *,
    timeout: int = 60,
) -> subprocess.CompletedProcess[str]:
    encoded = base64.b64encode(script.encode("utf-16le")).decode("ascii")
    return run(
        [
            "ssh",
            *SSH_OPTIONS,
            host,
            "powershell",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-EncodedCommand",
            encoded,
        ],
        timeout=timeout,
    )


def prepare_posix(host: str, remote_root: str) -> None:
    repo = f"{remote_root}/repo"
    command = (
        f"rm -rf {shlex.quote(remote_root)} && "
        f"mkdir -p {shlex.quote(repo)} && "
        f"git init -q {shlex.quote(repo)}"
    )
    ssh(host, f"sh -lc {shlex.quote(command)}", timeout=60)


def cleanup_posix(host: str, remote_root: str) -> None:
    command = f"rm -rf {shlex.quote(remote_root)}"
    run(["ssh", *SSH_OPTIONS, host, f"sh -lc {shlex.quote(command)}"], timeout=30, check=False)


def windows_temp_root(host: str) -> str:
    script = (
        "$ErrorActionPreference = 'Stop'; "
        "[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false); "
        "Write-Output $env:TEMP"
    )
    result = powershell(host, script, timeout=90)
    root = result.stdout.strip().splitlines()[-1].strip()
    if not root:
        raise SystemExit(f"could not resolve Windows TEMP for {host}")
    return root.rstrip("\\/")


def prepare_windows(host: str, remote_root: str) -> None:
    repo = f"{remote_root}\\repo"
    script = (
        "$ErrorActionPreference = 'Stop'; "
        f"$root = {ps_quote(remote_root)}; "
        f"$repo = {ps_quote(repo)}; "
        "Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue; "
        "New-Item -ItemType Directory -Force -Path $repo | Out-Null; "
        "git init -q $repo"
    )
    powershell(host, script, timeout=120)


def cleanup_windows(host: str, remote_root: str) -> None:
    script = (
        f"$root = {ps_quote(remote_root)}; "
        "Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue"
    )
    try:
        powershell(host, script, timeout=60)
    except SystemExit:
        pass


def target_config(
    *,
    posix_host: str | None,
    posix_root: str,
    windows_host: str | None,
    windows_root: str | None,
) -> tuple[str, list[str], dict[str, str]]:
    targets: list[str] = []
    remote_roots: dict[str, str] = {}
    chunks = [
        """
[validation.default]
command = "git rev-parse --is-inside-work-tree"
""".lstrip()
    ]
    ssh_options_toml = ", ".join(toml(option) for option in SSH_OPTIONS)
    if posix_host:
        targets.append("live_ssh")
        remote_roots["live_ssh"] = posix_root
        chunks.append(
            f"""
[targets.live_ssh]
backend = "ssh"
platform = "linux-x64"
host = {toml(posix_host)}
repo_path = {toml(posix_root + "/repo")}
remote_bundle_path = {toml(posix_root + "/shipyard.bundle")}
ssh_options = [{ssh_options_toml}]
timeout_secs = 120
bundle_upload_timeout_secs = 60
bundle_apply_timeout_secs = 60
""".lstrip()
        )
    if windows_host and windows_root:
        targets.append("live_windows")
        remote_roots["live_windows"] = windows_root
        chunks.append(
            f"""
[targets.live_windows]
backend = "ssh-windows"
platform = "windows-x64"
host = {toml(windows_host)}
repo_path = {toml(windows_root + "\\repo")}
remote_bundle_path = {toml(windows_root + "\\shipyard.bundle")}
ssh_options = [{ssh_options_toml}]
timeout_secs = 180
bundle_upload_timeout_secs = 120
bundle_apply_timeout_secs = 120
""".lstrip()
        )
    return "\n".join(chunks), targets, remote_roots


def write_config(clone: Path, content: str) -> None:
    config_dir = clone / ".shipyard"
    config_dir.mkdir()
    (config_dir / "config.toml").write_text(content, encoding="utf-8")


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--binary", default=str(ROOT / "target" / "release" / "shipyard"))
    parser.add_argument("--posix-host", default=os.environ.get("SHIPYARD_LIVE_SSH_HOST"))
    parser.add_argument(
        "--windows-host",
        default=os.environ.get("SHIPYARD_LIVE_SSH_WINDOWS_HOST"),
    )
    parser.add_argument("--posix-root")
    parser.add_argument("--windows-root")
    parser.add_argument("--keep", action="store_true")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    binary = Path(args.binary).expanduser().resolve()
    if not binary.exists():
        raise SystemExit(f"binary not found: {binary}")
    if shutil.which("ssh") is None:
        raise SystemExit("ssh is required for live SSH validation")
    if not args.posix_host and not args.windows_host:
        raise SystemExit("provide --posix-host and/or --windows-host")

    run_id = uuid.uuid4().hex[:12]
    posix_root = args.posix_root or f"/tmp/shipyard-live-{run_id}"
    windows_root = args.windows_root
    if args.windows_host and not windows_root:
        windows_root = f"{windows_temp_root(args.windows_host)}\\shipyard-live-{run_id}"

    prepared: list[tuple[str, str, str]] = []
    try:
        if args.posix_host:
            prepare_posix(args.posix_host, posix_root)
            prepared.append(("posix", args.posix_host, posix_root))
        if args.windows_host and windows_root:
            prepare_windows(args.windows_host, windows_root)
            prepared.append(("windows", args.windows_host, windows_root))

        config, targets, remote_roots = target_config(
            posix_host=args.posix_host,
            posix_root=posix_root,
            windows_host=args.windows_host,
            windows_root=windows_root,
        )

        root = Path(tempfile.mkdtemp(prefix="shipyard-ssh-live-"))
        try:
            clone = root / "repo"
            state = root / "state"
            state.mkdir()
            run(["git", "clone", "--quiet", "--branch", "main", str(ROOT), str(clone)], timeout=120)
            write_config(clone, config)
            result = run(
                [
                    str(binary),
                    "--mode",
                    "isolated",
                    "--state-dir",
                    str(state),
                    "--cwd",
                    str(clone),
                    "--json",
                    "run",
                    "--targets",
                    ",".join(targets),
                    "--no-warm",
                    "--allow-tree-drift",
                ],
                cwd=clone,
                timeout=360,
                check=False,
            )
            if result.returncode != 0:
                raise SystemExit(
                    f"shipyard live SSH smoke failed ({result.returncode})\n"
                    f"stdout:\n{result.stdout}\n"
                    f"stderr:\n{result.stderr}"
                )
            payload = json.loads(result.stdout)
            job = payload["run"]
            if job["overall"] != "pass":
                raise SystemExit(f"unexpected SSH job result: {json.dumps(job, indent=2)}")
            for target in targets:
                result_row = job["results"][target]
                if result_row["status"] != "pass":
                    raise SystemExit(
                        f"unexpected {target} result: {json.dumps(result_row, indent=2)}"
                    )
            print(
                json.dumps(
                    {
                        "ok": True,
                        "targets": {target: job["results"][target] for target in targets},
                        "remote_roots": remote_roots,
                    },
                    indent=2,
                )
            )
            return 0
        finally:
            if args.keep:
                print(f"kept tempdir: {root}", file=sys.stderr)
            else:
                shutil.rmtree(root, ignore_errors=True)
    finally:
        if not args.keep:
            for kind, host, remote_root in prepared:
                if kind == "posix":
                    cleanup_posix(host, remote_root)
                else:
                    cleanup_windows(host, remote_root)


if __name__ == "__main__":
    sys.exit(main())
