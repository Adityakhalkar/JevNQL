import json
import os
from datetime import date
from pathlib import Path

import pytest

from jevnql.bindings import Core, CoreError
from jevnql.compiler import AnthropicLLM, CompileError, Compiler, extract_payload

REPO = Path(__file__).resolve().parents[2]
DATA = sorted(str(p) for p in (REPO / "examples" / "data" / "mini").glob("*.csv"))
EXAMPLE_PLAN = json.loads((REPO / "examples" / "plans" / "high_value_unhappy.json").read_text())


@pytest.fixture(scope="module")
def core():
    try:
        return Core(DATA)
    except CoreError as e:
        pytest.skip(str(e))


class ScriptedLLM:
    """Replays canned replies and records what it was asked."""

    def __init__(self, *replies: str):
        self.replies = list(replies)
        self.calls: list[tuple[str, list[dict]]] = []

    def complete(self, system, messages):
        self.calls.append((system, [dict(m) for m in messages]))
        return self.replies.pop(0)


def reply(plan, assumptions=()):
    return json.dumps({"assumptions": list(assumptions), "plan": plan})


BAD_PLAN = {
    "version": 1,
    "steps": [
        {"id": "o", "op": "scan", "table": "orders"},
        {"id": "f", "op": "filter", "input": "o", "predicate": {"kind": "column", "name": "revenue"}},
    ],
}

COUNT_PLAN = {
    "version": 1,
    "steps": [
        {"id": "r", "op": "scan", "table": "reviews"},
        {"id": "n", "op": "aggregate", "input": "r", "group_by": ["customer_id"],
         "aggregates": [{"func": "count", "output": "reviews"}]},
        {"id": "top", "op": "top_k", "input": "n", "k": 1,
         "keys": [{"expr": {"kind": "column", "name": "reviews"}, "descending": True}]},
    ],
}


def test_prompt_carries_catalog_and_date(core):
    compiler = Compiler(core, ScriptedLLM(), today=date(2026, 9, 19))
    assert "table orders (7 rows)" in compiler.system
    assert "order_date: date, range 2025-06-01 .. 2026-05-01" in compiler.system
    assert "segment: utf8, e.g." in compiler.system and "'enterprise'" in compiler.system
    assert "Today is 2026-09-19." in compiler.system


def test_valid_plan_compiles_first_try(core):
    llm = ScriptedLLM(reply(EXAMPLE_PLAN, ["'this year' means 2026"]))
    compiled = Compiler(core, llm).compile("Top spenders this year who seem unhappy with pricing?")
    assert compiled.attempts == 1
    assert compiled.assumptions == ["'this year' means 2026"]
    assert compiled.logical_plan.startswith("TopK[20: dissatisfaction DESC]")


def test_validation_errors_are_fed_back(core):
    llm = ScriptedLLM(reply(BAD_PLAN), reply(COUNT_PLAN))
    compiled = Compiler(core, llm).compile("Who wrote the most reviews?")
    assert compiled.attempts == 2
    feedback = llm.calls[1][1][-1]["content"]
    assert "step `f`: unknown column `revenue`" in feedback
    # the rejected attempt stays in the transcript (append-only)
    assert llm.calls[1][1][-2]["role"] == "assistant"


def test_non_json_reply_is_repaired(core):
    llm = ScriptedLLM("Sure! Here is the plan you asked for.", f"```json\n{reply(COUNT_PLAN)}\n```")
    compiled = Compiler(core, llm).compile("Who wrote the most reviews?")
    assert compiled.attempts == 2
    assert "not the required JSON object" in llm.calls[1][1][-1]["content"]


def test_gives_up_after_max_attempts(core):
    llm = ScriptedLLM(reply(BAD_PLAN), reply(BAD_PLAN))
    with pytest.raises(CompileError) as err:
        Compiler(core, llm, max_attempts=2).compile("Revenue?")
    assert len(err.value.problems) == 2


def test_follow_up_sees_previous_accepted_turn(core):
    llm = ScriptedLLM(reply(BAD_PLAN), reply(COUNT_PLAN), reply(COUNT_PLAN))
    compiler = Compiler(core, llm)
    compiler.compile("Who wrote the most reviews?")
    compiler.compile("And what did they say?")
    history = llm.calls[-1][1]
    # only the question and the accepted answer are kept, not failed attempts
    assert [m["role"] for m in history] == ["user", "assistant", "user"]
    assert history[0]["content"] == "Who wrote the most reviews?"
    assert json.loads(history[1]["content"])["plan"] == COUNT_PLAN


def test_extract_payload_rejects_wrong_shapes():
    with pytest.raises(ValueError):
        extract_payload('{"plan": [1, 2]}')
    with pytest.raises(ValueError):
        extract_payload('{"plan": {}, "assumptions": "none"}')
    plan, assumptions = extract_payload('Here:\n{"plan": {"steps": []}}')
    assert plan == {"steps": []} and assumptions == []


@pytest.mark.skipif(not os.environ.get("ANTHROPIC_API_KEY"), reason="ANTHROPIC_API_KEY not set")
def test_live_compiles_deterministic_and_semantic_questions(core):
    compiler = Compiler(core, AnthropicLLM())
    counted = compiler.compile("Which customer has submitted the most reviews?")
    assert "Semantic" not in counted.logical_plan
    judged = compiler.compile("Which customers seem unhappy about our prices?")
    assert "Semantic" in judged.logical_plan
