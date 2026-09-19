"""Naive vs optimized execution of the benchmark suite (standard library only).

    python3 benchmarks/run.py                                # demo data, simulated backend
    python3 benchmarks/run.py --data examples/data/large
    python3 benchmarks/run.py --backend jev                  # real Jev (costs money)

Talks to `jevnql serve` (JSON lines); build it first with
`cargo build --release -p jevnql-cli` or point $JEVNQL_BIN at a binary.

Each case runs twice in fresh engines (no shared cache): exactly as written
("naive": no rewrites, no batch fusion) and optimized. Results must match.
Where the suite names a ground-truth persona, the optimized result is scored
against <data>-truth/personas.csv from examples/generate.py.
"""

from __future__ import annotations

import argparse
import csv
import json
import os
import subprocess
from collections import Counter
from pathlib import Path

HERE = Path(__file__).resolve().parent
UNLIMITED = 10**9


def engine_binary() -> str:
    if env := os.environ.get("JEVNQL_BIN"):
        return env
    for profile in ("release", "debug"):
        candidate = HERE.parent / "target" / profile / "jevnql"
        if candidate.exists():
            return str(candidate)
    raise SystemExit("jevnql not found; run `cargo build --release -p jevnql-cli` or set JEVNQL_BIN")


def run_once(plan: dict, files: list[str], backend: str, optimize: bool) -> dict:
    """One plan in a fresh engine process (no shared semantic cache)."""
    args = [engine_binary(), "--backend", backend, "--max-semantic-rows", str(UNLIMITED), "serve", *files]
    request = json.dumps({"cmd": "run", "plan": plan, "optimize": optimize}) + "\n"
    proc = subprocess.run(args, input=request, capture_output=True, text=True, check=False)
    if proc.returncode != 0 or not proc.stdout.strip():
        raise SystemExit(proc.stderr.strip() or f"jevnql exited with {proc.returncode}")
    return json.loads(proc.stdout.splitlines()[0])


def load_truth(data: Path) -> tuple[dict[str, str], Counter]:
    truth_dir = data.parent / f"{data.name}-truth"
    personas = {r["customer_id"]: r["persona"] for r in csv.DictReader(open(truth_dir / "personas.csv"))}
    reviews = Counter(r["customer_id"] for r in csv.DictReader(open(data / "reviews.csv")))
    return personas, reviews


def accuracy(spec: dict, out: dict, personas: dict[str, str], reviews: Counter) -> str:
    rows = [dict(zip(out["columns"], r)) for r in out["rows"]]
    if "score" in spec:
        rows = [r for r in rows if float(r[spec["score"]]) >= spec["threshold"]]
        label = f"{spec['score']} >= {spec['threshold']}"
    else:
        label = "returned rows"
    found = {r[spec["key"]] for r in rows}
    hits = sum(personas[k] == spec["persona"] for k in found)
    precision = hits / len(found) if found else 0.0
    text = f"precision {precision:.0%} ({hits}/{len(found)} {label} are `{spec['persona']}`)"
    if "min_reviews" in spec:
        eligible = {k for k, p in personas.items() if p == spec["persona"] and reviews[k] >= spec["min_reviews"]}
        text += f", recall {len(found & eligible) / len(eligible):.0%} of {len(eligible)}"
    return text


def normalize(rows: list[list[str]]) -> list[tuple[str, ...]]:
    """Numbers to 9 significant digits: parallel float aggregation may differ
    in the last bits between plans (addition order is not fixed)."""

    def cell(v: str) -> str:
        try:
            return f"{float(v):.9g}"
        except ValueError:
            return v

    return [tuple(cell(v) for v in r) for r in rows]


def compare(naive: dict, optimized: dict) -> str:
    """Identical, or how the results differ. Rows are identified by their
    first column; live models can return slightly different scores for the
    same input, so numeric differences are reported, not hidden."""
    a, b = normalize(naive["rows"]), normalize(optimized["rows"])
    if a == b:
        return "identical"
    if sorted(a) == sorted(b):
        return "same rows, different order"
    keys_a, keys_b = [r[0] for r in a], [r[0] for r in b]
    if sorted(keys_a) == sorted(keys_b):
        by_key = {r[0]: r for r in a}
        delta = 0.0
        for row in b:
            for x, y in zip(by_key[row[0]], row):
                try:
                    delta = max(delta, abs(float(x) - float(y)))
                except ValueError:
                    pass
        return f"same rows; values differ by up to {delta:.3g} (semantic backend noise)"
    moved = len(set(keys_a) ^ set(keys_b)) // 2
    return f"**{moved} of {len(b)} rows differ**"


def run_case(case: dict, files: list[str], backend: str) -> tuple[dict, dict]:
    plan = json.loads((HERE / case["plan"]).read_text())
    outs = []
    for optimize in (False, True):
        out = run_once(plan, files, backend, optimize)
        if not out["ok"]:
            raise SystemExit(f"{case['name']}: {out['error']}")
        outs.append(out)
    return outs[0], outs[1]


def fmt(m: dict) -> list[str]:
    return [
        f"{m['rows_scanned']:,}",
        f"{m['semantic_rows']:,}",
        f"{m['distinct_states']:,}",
        f"{m['semantic_batches']} / {m['requests']:,}",
        f"{m['input_tokens']:,}",
        f"${m['estimated_cost_usd']:.4f}",
        f"{m['total_ms']:,.0f} ms",
    ]


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--data", type=Path, default=HERE.parent / "examples" / "data" / "demo")
    parser.add_argument("--backend", choices=["simulated", "jev"], default="simulated")
    parser.add_argument("--out", type=Path, help="also write the report here (markdown)")
    args = parser.parse_args()

    data = args.data.resolve()
    files = sorted(str(p) for p in data.glob("*.csv"))
    personas, reviews = load_truth(data)
    suite = json.loads((HERE / "suite.json").read_text())

    header = ["plan", "rows scanned", "rows → Jev", "distinct states", "batches / requests",
              "input tokens", "est. cost", "time"]
    lines = [
        f"# JevNQL benchmark: naive vs optimized\n",
        f"Data: `{data.name}` ({len(personas):,} customers). Semantic backend: `{args.backend}`"
        + (" (keyword stand-in, not Jev; accuracy reflects the simulator)" if args.backend == "simulated" else "")
        + ".\n",
    ]
    for case in suite:
        naive, optimized = run_case(case, files, args.backend)
        n, o = naive["metrics"], optimized["metrics"]
        lines += [
            f"## {case['name']}\n",
            f"> {case['question']}\n",
            "| " + " | ".join(header) + " |",
            "|" + "---|" * len(header),
            "| naive | " + " | ".join(fmt(n)) + " |",
            "| optimized | " + " | ".join(fmt(o)) + " |",
            "",
            f"- Rows sent to the semantic backend: **{n['semantic_rows']:,} → {o['semantic_rows']:,}**"
            + (f" ({n['semantic_rows'] / max(o['semantic_rows'], 1):.1f}× fewer)" if o["semantic_rows"] else ""),
            f"- Estimated semantic cost: ${n['estimated_cost_usd']:.4f} → ${o['estimated_cost_usd']:.4f}",
            f"- Results: {compare(naive, optimized)} ({len(optimized['rows'])} rows)",
        ]
        if "truth" in case:
            lines.append(f"- Accuracy vs planted personas: {accuracy(case['truth'], optimized, personas, reviews)}")
        lines.append("")
    report = "\n".join(lines)
    print(report)
    if args.out:
        args.out.write_text(report)


if __name__ == "__main__":
    main()
