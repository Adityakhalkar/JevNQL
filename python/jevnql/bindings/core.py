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
    """`$JEVNQL_BIN`, else a cargo build in this repo, else `jevnql-engine` on PATH."""
    if env := os.environ.get("JEVNQL_BIN"):
        return env
    for profile in ("release", "debug"):
        candidate = REPO_ROOT / "target" / profile / "jevnql-engine"
        if candidate.exists():
            return str(candidate)
    if found := shutil.which("jevnql-engine"):
        return found
    raise CoreError("jevnql-engine not found; run `cargo build -p jevnql-cli` or set JEVNQL_BIN")


class Core:
    """The Rust engine over a fixed set of data files.

    Runs one `jevnql-engine serve` process for its lifetime, so data is
    loaded once and the semantic cache persists across questions.
    """

    def __init__(
        self,
        files: list[str | Path],
        backend: str = "auto",
        max_semantic_rows: int | None = None,
        binary: str | None = None,
    ):
        self.files = [str(Path(f)) for f in files]
        args = [binary or find_binary(), "--backend", backend]
        if max_semantic_rows is not None:
            args += ["--max-semantic-rows", str(max_semantic_rows)]
        self._args = [*args, "serve", *self.files]
        self._proc: subprocess.Popen[str] | None = None

    def _request(self, request: dict) -> dict:
        if self._proc is None:
            self._proc = subprocess.Popen(
                self._args, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True
            )
        assert self._proc.stdin and self._proc.stdout and self._proc.stderr
        try:
            self._proc.stdin.write(json.dumps(request) + "\n")
            self._proc.stdin.flush()
        except BrokenPipeError:
            pass  # the engine exited; its stderr says why
        line = self._proc.stdout.readline()
        if not line:
            err = self._proc.stderr.read().strip()
            self._proc = None
            raise CoreError(err or "jevnql-engine exited unexpectedly")
        return json.loads(line)

    def catalog(self) -> list[dict]:
        """Table profiles: name, rows, and columns with type, range and examples."""
        out = self._request({"cmd": "catalog"})
        if not out["ok"]:
            raise CoreError(out["error"])
        return out["tables"]

    def validate(self, plan: dict) -> Validation:
        """Type-checks a logical JevIR plan document against the data."""
        out = self._request({"cmd": "validate", "plan": plan})
        return Validation(
            ok=out["ok"], error=out.get("error"), schema=out.get("schema", []), logical_plan=out.get("logical_plan")
        )

    def run(self, plan: dict, explain_only: bool = False, optimize: bool = True) -> dict:
        """Optimizes and executes a plan. The reply has `ok`, and either
        `error` or `text` (EXPLAIN, results, metrics) plus `columns`, `rows`
        and `metrics` when executed."""
        return self._request({"cmd": "run", "plan": plan, "explain_only": explain_only, "optimize": optimize})

    def close(self) -> None:
        if self._proc is not None:
            if self._proc.stdin:
                self._proc.stdin.close()
            self._proc.wait(timeout=10)
            self._proc = None

    def __enter__(self) -> Core:
        return self

    def __exit__(self, *exc: object) -> None:
        self.close()
