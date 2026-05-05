from __future__ import annotations

from pathlib import Path

import pytest

from shipyard_sandbox import (
    PythonShipyardSource,
    Sandbox,
    resolve_binary,
    resolve_python_shipyard_source,
)


@pytest.fixture(scope="session")
def repo_root() -> Path:
    return Path(__file__).resolve().parents[2]


@pytest.fixture(scope="session")
def shipyard_binary(repo_root: Path) -> Path:
    return resolve_binary(repo_root)


@pytest.fixture(scope="session")
def python_shipyard_source(repo_root: Path) -> PythonShipyardSource:
    source = resolve_python_shipyard_source(repo_root)
    if source is None:
        pytest.skip(
            "Python Shipyard source not found. Set SHIPYARD_PYTHON_REPO_FOR_TEST "
            "and SHIPYARD_PYTHON_FOR_TEST to enable dual-binary parity checks."
        )
    return source


@pytest.fixture
def sandbox(shipyard_binary: Path) -> Sandbox:
    with Sandbox() as active:
        active.stage_binary(shipyard_binary)
        yield active
