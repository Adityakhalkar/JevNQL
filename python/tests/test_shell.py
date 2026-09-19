import json
from pathlib import Path

import pytest

from jevnql.bindings import Core, CoreError
from jevnql.cli import main

REPO = Path(__file__).resolve().parents[2]
DATA = sorted(str(p) for p in (REPO / "examples" / "data" / "mini").glob("*.csv"))
PLAN_PATH = REPO / "examples" / "plans" / "high_value_unhappy.json"
PLAN = json.loads(PLAN_PATH.read_text())


@pytest.fixture
def core():
    try:
        with Core(DATA, backend="simulated") as c:
            yield c
    except CoreError as e:
        pytest.skip(str(e))


def test_run_returns_rows_and_metrics(core):
    out = core.run(PLAN)
    assert out["ok"], out
    assert out["columns"] == ["customer_id", "total_spend", "review_history", "dissatisfaction"]
    assert [r[0] for r in out["rows"]] == ["2", "1", "3"]
    m = out["metrics"]
    assert m["engine_path"] == ["DataFusion", "Jev", "DataFusion"]
    assert (m["rows_scanned"], m["semantic_rows"], m["requests"]) == (13, 3, 3)


def test_semantic_cache_survives_between_requests(core):
    core.run(PLAN)
    again = core.run(PLAN)["metrics"]
    assert (again["requests"], again["cache_hits"]) == (0, 3)


def test_explain_only_does_not_execute(core):
    out = core.run(PLAN, explain_only=True)
    assert out["ok"] and "PHYSICAL PLAN" in out["text"] and "RESULT" not in out["text"]


def test_engine_errors_are_reported_not_raised(core):
    out = core.run({"version": 1, "steps": [{"id": "x", "op": "scan", "table": "nope"}]})
    assert not out["ok"] and "unknown table `nope`" in out["error"]


def test_budget_errors_surface(tmp_path):
    with Core(DATA, backend="simulated", max_semantic_rows=2) as core:
        out = core.run(PLAN)
    assert not out["ok"] and "3 rows would be sent" in out["error"]


def test_missing_file_is_a_core_error():
    with pytest.raises(CoreError, match="does-not-exist"):
        Core(["does-not-exist.csv"]).catalog()


def test_cli_runs_a_plan_without_an_llm(capsys):
    assert main([*DATA, "--backend", "simulated", "--plan", str(PLAN_PATH)]) == 0
    out = capsys.readouterr().out
    for section in ("LOGICAL PLAN", "OPTIMIZER", "PHYSICAL PLAN", "RESULT", "METRICS"):
        assert section in out
    assert "DataFusion → Jev(simulated) → DataFusion" in out
