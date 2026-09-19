"""Bridge to the Rust core through the `jevnql` CLI (JSON over stdout).

The Rust engine owns the IR, validation, optimization and execution; this
module only ships plan documents across the process boundary.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
from dataclasses import dataclass, field
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[3]


class CoreError(RuntimeError):
    """The core could not run (missing binary, unreadable data, ...)."""


@dataclass
class Validation:
    ok: bool
    error: str | None = None
    schema: list[dict] = field(default_factory=list)
    logical_plan: str | None = None


def find_binary() -> str:
    """`$JEVNQL_BIN`, else a cargo build in this repo, else `jevnql` on PATH."""
    if env := os.environ.get("JEVNQL_BIN"):
        return env
    for profile in ("release", "debug"):
        candidate = REPO_ROOT / "target" / profile / "jevnql"
        if candidate.exists():
            return str(candidate)
    if found := shutil.which("jevnql"):
        return found
    raise CoreError("jevnql binary not found; run `cargo build -p jevnql-cli` or set JEVNQL_BIN")


class Core:
    """The Rust engine over a fixed set of data files."""

    def __init__(self, files: list[str | Path], binary: str | None = None):
        self.files = [str(Path(f)) for f in files]
        self.binary = binary or find_binary()

    def _run(self, args: list[str], stdin: str | None = None) -> subprocess.CompletedProcess[str]:
        proc = subprocess.run(
            [self.binary, *args, *self.files], input=stdin, capture_output=True, text=True, check=False
        )
        if proc.returncode not in (0, 1):
            raise CoreError(proc.stderr.strip() or f"jevnql exited with {proc.returncode}")
        return proc

    def catalog(self) -> list[dict]:
        """Table profiles: name, rows, and columns with type, range and examples."""
        return json.loads(self._run(["catalog"]).stdout)["tables"]

    def validate(self, plan: dict) -> Validation:
        """Type-checks a logical JevIR plan document against the data."""
        out = json.loads(self._run(["validate", "--plan", "-"], stdin=json.dumps(plan)).stdout)
        return Validation(
            ok=out["ok"], error=out.get("error"), schema=out.get("schema", []), logical_plan=out.get("logical_plan")
        )
