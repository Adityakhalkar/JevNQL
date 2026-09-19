"""Language-model clients used by the compiler."""

from __future__ import annotations

from typing import Protocol

import anthropic

DEFAULT_MODEL = "claude-opus-5"


class LLMError(RuntimeError):
    pass


class LLM(Protocol):
    def complete(self, system: str, messages: list[dict]) -> str:
        """Returns the assistant's text reply."""
        ...


class AnthropicLLM:
    """Claude via the Anthropic SDK.

    Uses server-side refusal fallbacks and automatic prompt caching: the
    system prompt (instructions + catalog) is identical for every question in
    a session.
    """

    def __init__(self, model: str = DEFAULT_MODEL, effort: str = "high", client: anthropic.Anthropic | None = None):
        self.model = model
        self.effort = effort
        self.client = client or anthropic.Anthropic()

    def complete(self, system: str, messages: list[dict]) -> str:
        response = self.client.beta.messages.create(
            model=self.model,
            max_tokens=16000,
            system=system,
            messages=messages,
            output_config={"effort": self.effort},
            cache_control={"type": "ephemeral"},
            betas=["server-side-fallback-2026-07-01"],
            fallbacks="default",
        )
        if response.stop_reason == "refusal":
            raise LLMError(f"model declined to compile this question ({response.stop_details})")
        if response.stop_reason == "max_tokens":
            raise LLMError("reply exceeded max_tokens before the plan was complete")
        return "".join(block.text for block in response.content if block.type == "text")
