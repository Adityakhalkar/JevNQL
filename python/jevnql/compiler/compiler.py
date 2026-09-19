"""Natural language -> logical JevIR.

The model proposes a plan; the Rust core type-checks it. Validation errors
name the failing step and are fed back until the plan checks or attempts run
out. The compiler never produces SQL and never executes anything.
"""

from __future__ import annotations

import json
import re
from dataclasses import dataclass
from datetime import date

from jevnql.bindings.core import Core
from jevnql.compiler.llm import LLM
from jevnql.compiler.prompt import build_system


@dataclass
class Compiled:
    plan: dict
    assumptions: list[str]
    logical_plan: str
    attempts: int


class CompileError(RuntimeError):
    def __init__(self, question: str, problems: list[str]):
        self.question = question
        self.problems = problems
        super().__init__(f"could not compile {question!r}: " + " | ".join(problems))


_FENCE = re.compile(r"```(?:json)?\s*(.*?)```", re.S)


def extract_payload(text: str) -> tuple[dict, list[str]]:
    """Parses `{"assumptions": [...], "plan": {...}}` out of a reply."""
    fenced = _FENCE.search(text)
    body = fenced.group(1) if fenced else text[text.find("{") : text.rfind("}") + 1]
    try:
        payload = json.loads(body)
    except json.JSONDecodeError as e:
        raise ValueError(f"reply is not valid JSON ({e})") from None
    if not isinstance(payload, dict) or not isinstance(payload.get("plan"), dict):
        raise ValueError('reply must be an object with a "plan" object')
    assumptions = payload.get("assumptions", [])
    if not isinstance(assumptions, list) or not all(isinstance(a, str) for a in assumptions):
        raise ValueError('"assumptions" must be a list of strings')
    return payload["plan"], assumptions


class Compiler:
    """Compiles questions against one dataset; remembers accepted turns so
    follow-up questions can build on earlier plans."""

    def __init__(self, core: Core, llm: LLM, max_attempts: int = 3, today: date | None = None):
        self.core = core
        self.llm = llm
        self.max_attempts = max_attempts
        self.system = build_system(core.catalog(), today or date.today())
        self.history: list[dict] = []

    def compile(self, question: str) -> Compiled:
        messages = [*self.history, {"role": "user", "content": question}]
        problems: list[str] = []
        for attempt in range(1, self.max_attempts + 1):
            reply = self.llm.complete(self.system, messages)
            messages.append({"role": "assistant", "content": reply})
            try:
                plan, assumptions = extract_payload(reply)
            except ValueError as e:
                problem = f"Your reply was not the required JSON object: {e}."
            else:
                check = self.core.validate(plan)
                if check.ok:
                    self.history += [{"role": "user", "content": question}, {"role": "assistant", "content": reply}]
                    return Compiled(plan, assumptions, check.logical_plan or "", attempt)
                problem = f"The plan failed validation: {check.error}."
            problems.append(problem)
            messages.append({"role": "user", "content": f"{problem} Reply with the corrected, complete JSON object."})
        raise CompileError(question, problems)
