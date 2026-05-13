#!/usr/bin/env python3
"""Self-test for `agent_consume_pack.py` (bd-17c65.10.8 / J8).

Exercises the `consume()` function against representative ee response
shapes - both the post-A4 `pack.text` happy path and the items[]
fallback - plus error envelopes. Pure Python 3.8+ stdlib (unittest);
zero third-party deps so it runs on any CI worker without pip install.

Run:
    python3 scripts/agent_consume_pack_test.py
or via the standard unittest runner:
    python3 -m unittest scripts/agent_consume_pack_test.py
"""
import importlib.util
import sys
import unittest
from pathlib import Path

_SPEC = importlib.util.spec_from_file_location(
    "agent_consume_pack",
    Path(__file__).resolve().parent / "agent_consume_pack.py",
)
assert _SPEC is not None and _SPEC.loader is not None
agent_consume_pack = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(agent_consume_pack)
consume = agent_consume_pack.consume


class ConsumeHappyPath(unittest.TestCase):
    """Post-A4 happy path: when `pack.text` is present we use it verbatim."""

    def test_pack_text_is_used_verbatim_when_present(self) -> None:
        resp = {
            "schema": "ee.response.v1",
            "success": True,
            "data": {
                "pack": {
                    "text": "# Context Pack\n\nverbatim markdown body\n",
                    "items": [{"rank": 1, "content": "ignored when text present"}],
                }
            },
        }
        out = consume(resp)
        self.assertEqual(out, "# Context Pack\n\nverbatim markdown body\n")

    def test_empty_pack_text_falls_back_to_items_render(self) -> None:
        # An empty string is falsy, so consume() must drop through to the
        # items[] renderer so an agent never sees a zero-byte fragment.
        resp = {
            "schema": "ee.response.v1",
            "success": True,
            "data": {"pack": {"text": "", "query": "q", "items": []}},
        }
        out = consume(resp)
        self.assertIn("# Context Pack: q", out)


class ConsumeItemsFallback(unittest.TestCase):
    """Pre-A4 / no-text pack: render minimally from items[]."""

    def test_items_render_includes_header_budget_and_each_memory(self) -> None:
        resp = {
            "success": True,
            "data": {
                "pack": {
                    "query": "prepare release",
                    "budget": {"usedTokens": 750, "maxTokens": 1000},
                    "items": [
                        {
                            "rank": 1,
                            "memoryId": "mem_01abc",
                            "section": "procedural_rules",
                            "content": "Run cargo fmt --check.",
                            "why": "matched release query",
                        },
                        {
                            "rank": 2,
                            "memoryId": "mem_02def",
                            "section": "failures",
                            "content": "Release failed when index stale.",
                        },
                    ],
                }
            },
        }
        out = consume(resp)
        self.assertIn("# Context Pack: prepare release", out)
        self.assertIn("**Budget:** 750/1000 tokens", out)
        self.assertIn("## procedural_rules 1. mem_01abc", out)
        self.assertIn("Run cargo fmt --check.", out)
        self.assertIn("**Why:** matched release query", out)
        self.assertIn("## failures 2. mem_02def", out)
        # No `**Why:**` line when the item has no why field.
        why_lines = [ln for ln in out.splitlines() if ln.startswith("**Why:**")]
        self.assertEqual(
            len(why_lines), 1, f"expected exactly one Why line, got {why_lines!r}"
        )

    def test_items_render_handles_missing_fields_gracefully(self) -> None:
        # A degenerate pack with only the bare minimum still renders
        # without raising; the script must never throw on a schema
        # that ships fewer fields than the consumer expects.
        resp = {"success": True, "data": {"pack": {"items": [{"content": "x"}]}}}
        out = consume(resp)
        self.assertIn("# Context Pack:", out)
        self.assertIn("x", out)

    def test_items_render_includes_budget_block_with_default_zeros(self) -> None:
        # When `budget` is absent the renderer surfaces the default
        # `0/0 tokens` line. Agents can detect this as "no budget
        # reported" and avoid confusing it with the success case where
        # `usedTokens` happens to equal zero (rare but valid).
        resp = {
            "success": True,
            "data": {
                "pack": {"query": "q", "items": [{"rank": 1, "content": "x"}]}
            },
        }
        out = consume(resp)
        self.assertIn("**Budget:**", out)
        self.assertIn("0/0 tokens", out)

    def test_items_render_omits_why_block_when_field_empty(self) -> None:
        # `why: ""` (empty string) should not produce a `**Why:**` line.
        resp = {
            "success": True,
            "data": {
                "pack": {
                    "query": "q",
                    "items": [
                        {"rank": 1, "memoryId": "m1", "content": "x", "why": ""}
                    ],
                }
            },
        }
        out = consume(resp)
        self.assertNotIn("**Why:**", out)


class ConsumeErrorEnvelope(unittest.TestCase):
    """`success: false` responses produce an HTML comment, not silent empty."""

    def test_error_envelope_renders_html_comment_with_code(self) -> None:
        resp = {
            "schema": "ee.error.v1",
            "success": False,
            "error": {"code": "search_index_stale", "message": "rebuild required"},
        }
        out = consume(resp)
        # The HTML-comment framing means a downstream Markdown renderer
        # silently swallows it; an agent that ignores comments still
        # sees zero noise, but an agent that inspects the raw fragment
        # can scrape the error code from it.
        self.assertIn("<!-- ee error:", out)
        self.assertIn("search_index_stale", out)
        self.assertTrue(out.startswith("<!--"))
        self.assertTrue(out.rstrip().endswith("-->"))

    def test_error_envelope_with_missing_error_object_uses_unknown(self) -> None:
        # An old/broken response that lacks the `error` field entirely
        # must not crash; the renderer falls back to the literal
        # "unknown" code so the agent can still distinguish error from
        # success.
        resp = {"success": False}
        out = consume(resp)
        self.assertIn("unknown", out)
        self.assertTrue(out.startswith("<!-- ee error:"))

    def test_missing_success_field_treated_as_error(self) -> None:
        # An old/broken response that lacks the `success` field must not
        # silently render an empty pack; it must produce the error
        # comment so the agent loop notices.
        resp = {"data": {"pack": {"text": "should not appear"}}}
        out = consume(resp)
        self.assertIn("<!-- ee error:", out)
        self.assertNotIn("should not appear", out)


class ConsumePromptFragmentDiscipline(unittest.TestCase):
    """Output discipline checks that future agent harnesses can rely on."""

    def test_output_is_pure_markdown_no_tool_call_wrapping(self) -> None:
        # The whole point of J8: an agent prepends this verbatim to its
        # LLM prompt. No JSON envelopes, no XML tag wrappers, no "tool
        # result" framing; pure Markdown only.
        resp = {"success": True, "data": {"pack": {"text": "# Pack\n\nbody\n"}}}
        out = consume(resp)
        self.assertFalse(out.startswith("{"))
        self.assertFalse(out.startswith("<tool_result"))
        self.assertFalse(out.startswith("<response>"))

    def test_items_render_terminates_with_newline(self) -> None:
        # An agent concatenating this fragment with a subsequent prompt
        # block must not have to add a trailing newline. The items[]
        # fallback always ends with one (\n on the final code fence).
        resp = {
            "success": True,
            "data": {"pack": {"items": [{"content": "x"}]}},
        }
        out = consume(resp)
        self.assertTrue(out.endswith("\n"), f"items render must end \\n; got {out!r}")


if __name__ == "__main__":
    runner = unittest.main(exit=False, verbosity=2, module="__main__")
    sys.exit(0 if runner.result.wasSuccessful() else 1)
