#!/usr/bin/env python3
"""Refuse a comparison of `SwarmBriefSourceFreshness::state` against a value
nothing in the tree can produce -- the defect of bd-6zxuu, which was dead for
as long as it existed.

## THE DEFECT THIS GUARDS

`src/core/swarm_next_action.rs::actionable_queue_evidence_from_brief_fallback`
compared the RAW freshness vocabulary against the NORMALISED one:

    Some(source)
        if source.status == SwarmBriefSourceStatus::Ready
            && source.freshness.state == "fresh" =>      <- could never be true

`SwarmBriefSourceFreshness::state` is a `&'static str` whose value domain is
exactly {"current", "unknown", "stale"}. Nothing produces `"fresh"`, so the arm
was unreachable and a Ready AND current Beads source fell through to
`stale_fallback` -- which sets `stale_source_kind`, maps to
`external_state_required`, and withholds `authoritative`. A healthy brief asked
for external state. The compiler cannot see it: every one of these vocabularies
is `&str`, so a wrong literal is a dead branch, not a type error.

## WHY A GATE AND NOT A UNIT TEST

bd-6zxuu's acceptance offers an enum, a value-domain unit test, or a comment.
A unit test is the right answer and is still owed, but it cannot run on this
host -- no cargo, verify.sh cannot reach a stage, Actions is disabled repo-wide
-- so the arm that guards this would itself be unreachable here, which is the
condition that let the original bug live. This gate answers the same question
from source in milliseconds with no toolchain, and `scripts/check-*.py` (the
family CLAUDE.md documents) picks it up with no wiring.

It is NOT a second copy of a rule. It DERIVES the value domain from the Rust on
every run -- from the constructors and from every struct literal -- and
compares the comparisons against that. Add a producer and the domain grows by
itself; there is no list here to go stale.

## WHY ONLY THIS ONE FIELD, AND WHY THAT IS NOT TIMIDITY

Measured 2026-11-09, this tree carries FIVE string freshness vocabularies, not
the three bd-6zxuu names:

  1. `SwarmBriefSourceFreshness::state`              {current, unknown, stale}
  2. `SwarmSourceAuthorityFreshness::freshness_state`  {fresh, stale, unknown}
  3. `SwarmWorkPacketSourceProvenance::status`
                        {fresh, stale, degraded, unavailable, skipped}
  4. `SwarmWorkPacketSourceProvenance::freshness: Option<String>`
                        read by the bridge as {current, fresh, stale}
  5. `MemoryAnchorFreshnessState::surface_label()`
                        {fresh, symbol_changed, symbol_missing, unknown}

Only #1 is gated, because only #1 has an UNAMBIGUOUS access path. `.freshness.state`
resolves to exactly one type: of the 17 types used as a field named `freshness`,
`SwarmBriefSourceFreshness` is the only one declaring a field `state`. The gate
RE-DERIVES that fact every run and refuses (exit 3) if a second type gains a
`state` field, so the scope cannot silently become wrong.

The others are deliberately NOT gated, and the reason is the hazard itself:
`freshness_state` is a field name shared by at least four structs carrying at
least TWO different vocabularies -- `swarm_next_action.rs` normalises to
{fresh, stale, unknown} while `recall.rs` stores the raw {current, ...}. A gate
keyed on that NAME would check one struct's literals against another struct's
domain, which is a mechanised version of the very confusion it was meant to
catch. Those four are documented at their struct definitions instead.

## WHAT IS DELIBERATELY UNCHECKED

`src/core/swarm_next_action.rs` aliases the carrier into a local:

    let freshness_state = source.freshness.state;

That local is named after vocabulary #2's FIELD while holding vocabulary #1's
VALUE. The comparisons downstream are in a different function, against a
parameter of the same name. Tracking that soundly needs type resolution, and
tracking it by name would reintroduce the collision above, so the gate reports
these sites and does not grade them. The original bug was a direct-access
comparison, which is what this covers.

Non-literal comparisons (`assert_eq!(f.state, expected_state)`) are also
reported rather than graded: there is no literal to check.

The comparisons themselves are matched against the whole file, so a comparison
split across lines is still graded. The tripwire for an UNPARSED comparing
shape, though, reads one line at a time, so a shape this gate cannot parse AND
that puts its operator on a different line from the field access would be
missed rather than refused. Widening that to a line window was rejected: it
would invent exit-3 refusals out of unrelated neighbouring code, and every
comparison in the tree today is on one line.

## EXIT CODES

  0  every direct-access comparison names a producible value
  1  RED: a comparison can never be true (or can never be false)
  2  usage / internal error
  3  CANNOT ANSWER -- the scope assumptions no longer hold. Never a pass.

`--self-test` drives every verdict path and prints its own arm count rather
than naming one here, since a hardcoded count is one more thing to go stale.
Several arms are controls on this file's own lexer, because over-blanking
would be a silent false PASS; four of them were each OBSERVED failing against
the broken version of the code they guard.
"""

from __future__ import annotations

import argparse
import os
import pathlib
import re
import sys
import tempfile

CARRIER_TYPE = "SwarmBriefSourceFreshness"
CARRIER_FIELD = "state"
OWNER_FIELD = "freshness"
ACCESS_PATH = f".{OWNER_FIELD}.{CARRIER_FIELD}"

EXIT_OK = 0
EXIT_RED = 1
EXIT_USAGE = 2
EXIT_CANNOT = 3


class CannotAnswer(Exception):
    """A scope assumption failed. Exit 3, never 0 -- an unanswerable check is
    not a passing check."""


# --------------------------------------------------------------------------
# Lexing: blank comments, and reduce every literal to an ATOM
# --------------------------------------------------------------------------
# Two separate hazards, and the first draft of this file got both wrong.
#
# COMMENTS. Direction matters. Leaving a comment in produces a FALSE RED, which
# is loud and gets looked at. Blanking real code produces a FALSE PASS, which is
# silent and is the failure this whole file exists to prevent. The real tree
# exercises both directions for free: `swarm_next_action.rs` happens to hold
# `freshness.state == "stale"` inside a comment AND a live comparison, so the
# self-test grades this scanner against both. It locates them by content, never
# by line number -- an earlier draft named the lines here and in the arms, and
# an edit to that same file silently turned the negative arm into a no-op.
#
# STRING INTERIORS. Preserving a string literal verbatim -- which is what a
# comment stripper must do -- then searching the result finds `.freshness.state`
# written INSIDE a string and grades it as code. That is a false positive, and
# a `r#"..."#` one even produced a false RED. So a literal is replaced by a
# SENTINEL, `\x00<index>\x00`, with its value kept in a side table. Code that
# mentions the access path inside a string vanishes with the rest of the
# literal, while the literals the comparisons are checked against are still
# recoverable by index.
#
# Both replacements preserve newline COUNT, so every reported line number is
# the real one.

SENTINEL_RE = re.compile(r"\x00(\d+)\x00")
CHAR_ATOM = "\x01"

_ESCAPES = {
    "n": "\n",
    "t": "\t",
    "r": "\r",
    "0": "\0",
    "\\": "\\",
    '"': '"',
    "'": "'",
}


def _unescape(raw: str) -> str:
    """Enough of Rust's escape grammar for the short ASCII literals this gate
    compares. `\\u{..}` and byte escapes are left verbatim: they cannot name a
    freshness state, and mangling them would be worse than showing them."""
    out: list[str] = []
    i = 0
    while i < len(raw):
        if raw[i] == "\\" and i + 1 < len(raw):
            nxt = raw[i + 1]
            if nxt in _ESCAPES:
                out.append(_ESCAPES[nxt])
                i += 2
                continue
            if nxt == "\n":  # Rust line continuation
                i += 2
                while i < len(raw) and raw[i] in " \t":
                    i += 1
                continue
        out.append(raw[i])
        i += 1
    return "".join(out)


# Jump straight to the next construct that can change lexer state instead of
# walking every character in Python. A per-character scan of the 97 relevant
# files cost 9.1 s; this form is the same grammar driven by one compiled
# alternation. Ordered longest-first so `r#"` and `//` win over `"` and `/`.
#
# The `[bc]?` is load-bearing. Rust's byte-raw literal is `br"..."`, and a
# lookbehind that only guards a bare `r` rejects the raw branch because `r`
# follows `b` -- so `br#"..."#` lexed as an ORDINARY string starting at the
# quote, which mis-terminates on any `\"` inside and can blank real code.
# Measured 2026-11-09: 78 real `br"..."` literals in src/, five of them in
# files this gate lexes. (Zero `cr"`, but the prefix costs nothing and Rust
# has the form.) `b"..."` and `c"..."` are not raw and need no branch: the
# plain `"` alternative handles them, and the prefix letter stays as code.
#
# NOTE on measuring this yourself: grepping `br#*"` reports ~177 hits and most
# are the CONTENTS of the ordinary string "br" (the beads CLI name), where `br`
# sits just before the closing quote. Require a token boundary before the
# prefix -- not an identifier character and not a quote -- or the count is
# your own instrument talking.
_TOKEN = re.compile(r'(?<![A-Za-z0-9_])[bc]?r(#*)"|//|/\*|"|\'')
_STRING_BODY = re.compile(r'(?:[^"\\]|\\.)*"', re.DOTALL)
_NOT_NEWLINE = re.compile(r"[^\n]")


def _blank(segment: str) -> str:
    """Same length, same newlines, no content -- so line numbers survive and
    two identifiers either side of a comment never merge."""
    return _NOT_NEWLINE.sub(" ", segment)


def lex(text: str) -> tuple[str, list[str]]:
    """Returns (masked text, literal values indexed by sentinel)."""
    out: list[str] = []
    values: list[str] = []
    i = 0
    n = len(text)

    def atom(value: str, span: str) -> None:
        # NOTE: `value` arrives already unescaped where that applies. Do NOT
        # unescape here -- a raw string's backslashes are literal, and
        # unescaping `r"a\nb"` would silently corrupt it.
        out.append(f"\x00{len(values)}\x00")
        values.append(value)
        out.append("\n" * span.count("\n"))

    while i < n:
        match = _TOKEN.search(text, i)
        if match is None:
            out.append(text[i:])
            break
        out.append(text[i : match.start()])
        token = match.group(0)
        start = match.start()

        # group(1) is the hash run, and it exists ONLY on the raw alternative --
        # `""` for `r"`, None for a plain `"`. Testing `token.startswith("r")`
        # was wrong the moment the `[bc]?` prefix was allowed, since the token
        # can now start with `b`.
        if match.group(1) is not None:
            terminator = '"' + "#" * len(match.group(1))
            close = text.find(terminator, match.end())
            if close == -1:
                atom(text[match.end() :], text[start:])
                i = n
            else:
                atom(text[match.end() : close], text[start : close + len(terminator)])
                i = close + len(terminator)
            continue

        if token == '"':
            body = _STRING_BODY.match(text, match.end())
            end = body.end() if body else n
            inner = text[match.end() : end - 1] if body else text[match.end() :]
            # `_unescape` over all 187k literals in the relevant files cost
            # 0.9 s, and the overwhelming majority hold no backslash at all.
            atom(_unescape(inner) if "\\" in inner else inner, text[start:end])
            i = end
            continue

        if token == "'":
            # A char literal, or a lifetime (`'static`). Only a char literal
            # closes with `'`; a lifetime is left as code so `&'static str`
            # survives for the struct parser.
            if start + 1 < n and text[start + 1] == "\\":
                close = text.find("'", start + 2)
                if close != -1:
                    out.append(CHAR_ATOM)
                    i = close + 1
                    continue
            elif start + 2 < n and text[start + 2] == "'":
                out.append(CHAR_ATOM)
                i = start + 3
                continue
            out.append("'")
            i = start + 1
            continue

        if token == "//":
            end = text.find("\n", start)
            end = n if end == -1 else end
            out.append(_blank(text[start:end]))
            i = end
            continue

        # token == "/*": block comments NEST in Rust.
        depth = 1
        scan = match.end()
        while depth > 0 and scan < n:
            opened = text.find("/*", scan)
            closed = text.find("*/", scan)
            if closed == -1:
                scan = n
                break
            if opened != -1 and opened < closed:
                depth += 1
                scan = opened + 2
            else:
                depth -= 1
                scan = closed + 2
        out.append(_blank(text[start:scan]))
        i = scan
    return "".join(out), values


# --------------------------------------------------------------------------
# Struct parsing -- one parser serves both the ambiguity probe and the domain
# --------------------------------------------------------------------------

STRUCT_RE = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?struct\s+([A-Za-z_]\w*)")
FIELD_RE = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?([a-z_]\w*)\s*:\s*(.+?),?\s*$")


def _block_end(text: str, open_brace: int) -> int:
    """Index just past the brace matching the one at `open_brace`."""
    depth = 0
    for i in range(open_brace, len(text)):
        if text[i] == "{":
            depth += 1
        elif text[i] == "}":
            depth -= 1
            if depth == 0:
                return i + 1
    return len(text)


def parse_structs(stripped: str) -> dict[str, dict[str, str]]:
    """type name -> {field name: declared type}. Tuple and unit structs have
    no named fields and are skipped."""
    structs: dict[str, dict[str, str]] = {}
    lines = stripped.splitlines(keepends=True)
    offset = 0
    for line in lines:
        found = STRUCT_RE.match(line)
        if found:
            # Search from just past the struct NAME, not from the start of the
            # line. `pub(crate) struct Foo {` carries a `(` in its visibility
            # modifier, so a line-start origin saw paren < brace and skipped
            # the struct -- all 159 `pub(crate) struct` declarations in this
            # tree, any one of which could have held the `freshness` field that
            # makes the access path ambiguous. Missing those is the SILENT
            # direction: the gate would report no ambiguity and grade on.
            origin = offset + found.end()
            brace = stripped.find("{", origin)
            semi = stripped.find(";", origin)
            paren = stripped.find("(", origin)
            # Only a brace struct has named fields, and the brace must come
            # before any `;` or `(` that would end the item first.
            if brace != -1 and (semi == -1 or brace < semi) and (paren == -1 or brace < paren):
                body = stripped[brace + 1 : _block_end(stripped, brace) - 1]
                fields: dict[str, str] = {}
                for bline in body.splitlines():
                    bare = bline.strip()
                    if not bare or bare.startswith("#["):
                        continue
                    fm = FIELD_RE.match(bline)
                    if fm:
                        fields[fm.group(1)] = fm.group(2).strip()
                structs[found.group(1)] = fields
        offset += len(line)
    return structs


def rust_files(root: pathlib.Path, subdir: str) -> list[pathlib.Path]:
    base = root / subdir
    if not base.is_dir():
        return []
    return sorted(p for p in base.rglob("*.rs") if p.is_file())


Unit = tuple[pathlib.Path, str, list[str]]

# Lexing is a per-character Python scan, so lexing all 1531 `*.rs` files cost
# 9.1 s -- too slow for a `check-*.py` family CLAUDE.md describes as running in
# seconds. Reading is cheap and lexing is not, so the tree is READ once and only
# the files that mention freshness at all are LEXED. Every construct this gate
# looks for contains the substring, case-insensitively:
#   a struct with a `freshness` field          -> "freshness"
#   a `SwarmBriefSourceFreshness` construction -> "Freshness"
#   a `.freshness.state` comparison            -> "freshness"
# The one shape that could escape it -- a CARRIER CANDIDATE type whose own
# definition never says "freshness" -- is picked up by a second stage in
# `resolve_access_path`, because missing it would under-report ambiguity, which
# is the silent direction.
RELEVANT = "freshness"


def load(files: list[pathlib.Path]) -> list[Unit]:
    out: list[Unit] = []
    for f in files:
        masked, values = lex(f.read_text(errors="replace"))
        out.append((f, masked, values))
    return out


def read_tree(root: pathlib.Path, subdir: str) -> dict[pathlib.Path, str]:
    return {p: p.read_text(errors="replace") for p in rust_files(root, subdir)}


def lex_relevant(raw: dict[pathlib.Path, str]) -> list[Unit]:
    out: list[Unit] = []
    for path, text in raw.items():
        if RELEVANT not in text.lower():
            continue
        masked, values = lex(text)
        out.append((path, masked, values))
    return out


# --------------------------------------------------------------------------
# Check A: is the access path still unambiguous?
# --------------------------------------------------------------------------


def _bare_type(declared: str) -> str:
    bare = declared.strip()
    if bare.startswith("Option<") and bare.endswith(">"):
        bare = bare[len("Option<") : -1].strip()
    return bare.split("<")[0].strip().rsplit("::", 1)[-1]


def resolve_access_path(
    loaded: list[Unit], raw: dict[pathlib.Path, str] | None = None
) -> list[str]:
    """Every type reachable as `<x>.freshness.state`: every type used as the
    declared type of a field named `freshness` that itself declares a field
    named `state`.

    `raw` is the whole tree, unlexed. It is consulted ONLY to find the
    definition of a carrier candidate that the relevance filter skipped --
    a struct whose own file never mentions freshness. Without that second
    stage the gate would report such a type as having no `state` field and so
    miss a real ambiguity, which is the silent direction.
    """
    structs: dict[str, dict[str, str]] = {}
    for _path, text, _values in loaded:
        structs.update(parse_structs(text))

    owner_types = {
        _bare_type(fields[OWNER_FIELD])
        for fields in structs.values()
        if fields.get(OWNER_FIELD)
    }

    missing = sorted(t for t in owner_types if t not in structs)
    if missing and raw:
        for name in missing:
            needle = f"struct {name}"
            for path, text in raw.items():
                if needle in text and RELEVANT not in text.lower():
                    masked, _values = lex(text)
                    for found, fields in parse_structs(masked).items():
                        structs.setdefault(found, fields)

    return sorted(t for t in owner_types if CARRIER_FIELD in structs.get(t, {}))


# --------------------------------------------------------------------------
# Check B: derive the value domain from the Rust
# --------------------------------------------------------------------------

STATE_ASSIGN_RE = re.compile(rf"\b{CARRIER_FIELD}\s*:\s*([^,\n]+)")
# A constructing occurrence of the type: NOT its own `struct` definition and
# NOT the `impl` header (handled separately, since the impl body holds the
# constructors' `Self { .. }` literals).
CONSTRUCT_RE = re.compile(rf"(?<!struct )(?<!impl ){CARRIER_TYPE}\s*\{{")
IMPL_RE = re.compile(rf"\bimpl\s+{CARRIER_TYPE}\s*\{{")


def _line_of(text: str, index: int) -> int:
    return text.count("\n", 0, index) + 1


def derive_domain(
    loaded: list[Unit], root: pathlib.Path
) -> tuple[dict[str, list[str]], list[str]]:
    """Returns (value -> producer sites, unresolved producer sites).

    An unresolved producer is a `state:` assignment whose right-hand side holds
    no string literal. The domain is then INCOMPLETE, so grading would be
    unreliable and the caller must refuse rather than pass.
    """
    domain: dict[str, list[str]] = {}
    unresolved: list[str] = []

    for path, text, values in loaded:
        rel = path.relative_to(root).as_posix()
        blocks: list[tuple[int, int]] = []
        for match in IMPL_RE.finditer(text):
            brace = text.index("{", match.start())
            blocks.append((brace, _block_end(text, brace)))
        for match in CONSTRUCT_RE.finditer(text):
            brace = text.index("{", match.start())
            blocks.append((brace, _block_end(text, brace)))

        for start, end in blocks:
            body = text[start:end]
            for assign in STATE_ASSIGN_RE.finditer(body):
                expr = assign.group(1)
                site = f"{rel}:{_line_of(text, start + assign.start())}"
                literals = [values[int(i)] for i in SENTINEL_RE.findall(expr)]
                if not literals:
                    # `state: v` where `v` is a binding, a call, or a constant.
                    # The domain is then incomplete; the caller must refuse.
                    shown = SENTINEL_RE.sub("<literal>", expr).strip()
                    unresolved.append(f"{site}  {CARRIER_FIELD}: {shown}")
                    continue
                for value in literals:
                    domain.setdefault(value, []).append(site)

    return domain, unresolved


# --------------------------------------------------------------------------
# Checks C/D: find comparisons
# --------------------------------------------------------------------------

ESC = re.escape(ACCESS_PATH)
LIT = r"\x00(\d+)\x00"
# `<expr>.freshness.state == <lit>` / `!=`, either operand order.
CMP_RE = re.compile(rf"[A-Za-z_][\w.()\[\]]*{ESC}\s*(==|!=)\s*{LIT}")
CMP_REV_RE = re.compile(rf"{LIT}\s*(==|!=)\s*[A-Za-z_][\w.()\[\]]*{ESC}")
# A bare local or parameter of the carrier type: `freshness.state == <lit>`.
BARE_RE = re.compile(rf"(?<![\w.]){OWNER_FIELD}\.{CARRIER_FIELD}\s*(==|!=)\s*{LIT}")
ASSERT_RE = re.compile(rf"assert_(eq|ne)!\(\s*[A-Za-z_][\w.()\[\]]*{ESC}\s*,\s*{LIT}")
ASSERT_BARE_RE = re.compile(
    rf"assert_(eq|ne)!\(\s*{OWNER_FIELD}\.{CARRIER_FIELD}\s*,\s*{LIT}"
)
# Any occurrence at all, for the unclassified-shape guard below.
OCCURRENCE_RE = re.compile(rf"(?<![\w]){OWNER_FIELD}\.{CARRIER_FIELD}\b")
ALIAS_RE = re.compile(rf"\blet\s+(\w+)\s*(?::[^=]+)?=\s*[A-Za-z_][\w.()\[\]]*{ESC}\s*;")
# A shape that compares but that none of the patterns above parsed.
SUSPICIOUS = ("==", "!=", "assert_eq!", "assert_ne!", "matches!")


def find_comparisons(
    loaded: list[Unit], root: pathlib.Path
) -> tuple[list[tuple[str, str, str]], list[str], list[str], list[str]]:
    """Returns (graded, aliases, non_literal, unclassified).

    `graded` is (site, operator, literal) for every direct-access comparison
    against a string literal.
    """
    graded: list[tuple[str, str, str]] = []
    aliases: list[str] = []
    non_literal: list[str] = []
    unclassified: list[str] = []

    for path, text, values in loaded:
        # Of the files that mention freshness at all, only a handful mention
        # this access path. Without this guard the per-line loop below ran over
        # every line of all 163 files -- a million regex calls for 1.4 s.
        if not OCCURRENCE_RE.search(text):
            continue
        rel = path.relative_to(root).as_posix()
        lines = text.splitlines()
        matched_lines: set[int] = set()

        def record(lineno: int, op: str, index: str) -> None:
            graded.append(
                (
                    f"{rel}:{lineno}",
                    {"eq": "==", "ne": "!="}.get(op, op),
                    values[int(index)],
                )
            )
            matched_lines.add(lineno)

        for pattern in (CMP_RE, BARE_RE, ASSERT_RE, ASSERT_BARE_RE):
            for match in pattern.finditer(text):
                record(_line_of(text, match.start()), match.group(1), match.group(2))
        for match in CMP_REV_RE.finditer(text):
            record(_line_of(text, match.start()), match.group(2), match.group(1))

        for match in ALIAS_RE.finditer(text):
            lineno = _line_of(text, match.start())
            aliases.append(f"{rel}:{lineno}  let {match.group(1)} = ...{ACCESS_PATH}")
            matched_lines.add(lineno)

        for index, line in enumerate(lines, start=1):
            if not OCCURRENCE_RE.search(line) or index in matched_lines:
                continue
            if any(token in line for token in SUSPICIOUS):
                shown = SENTINEL_RE.sub(
                    lambda m: f'"{values[int(m.group(1))]}"', line
                ).strip()
                if SENTINEL_RE.search(line):
                    unclassified.append(f"{rel}:{index}  {shown}")
                else:
                    non_literal.append(f"{rel}:{index}  {shown}")

    return graded, sorted(aliases), sorted(non_literal), sorted(unclassified)


# --------------------------------------------------------------------------
# Driver
# --------------------------------------------------------------------------


def repo_root() -> pathlib.Path:
    env = os.environ.get("EE_REPO_ROOT")
    if env:
        root = pathlib.Path(env)
        if not root.is_dir():
            raise CannotAnswer(f"EE_REPO_ROOT is not a directory: {env}")
        return root
    return pathlib.Path(__file__).resolve().parent.parent


def run(root: pathlib.Path, verbose: bool = True) -> int:
    src_raw = read_tree(root, "src")
    if not src_raw:
        raise CannotAnswer(
            f"no *.rs under {root}/src -- an empty tree reads identically to a clean one"
        )
    tests_raw = read_tree(root, "tests")
    src = lex_relevant(src_raw)
    tests = lex_relevant(tests_raw)

    def say(message: str = "") -> None:
        if verbose:
            print(message)

    # --- A. the access path must still resolve to one type -----------------
    reachable = resolve_access_path(src + tests, raw={**src_raw, **tests_raw})
    if reachable != [CARRIER_TYPE]:
        raise CannotAnswer(
            f"`{ACCESS_PATH}` no longer resolves to exactly one type.\n"
            f"  reachable now: {reachable or '(none)'}\n"
            f"  expected:      ['{CARRIER_TYPE}']\n"
            "  A second type with a `state` field makes every comparison below\n"
            "  ambiguous, which is the confusion this gate exists to catch. Give\n"
            "  the new type a distinct field name, or teach this gate to resolve\n"
            "  the owner. Do NOT widen the domain to satisfy both."
        )
    say(f"ok   `{ACCESS_PATH}` resolves to exactly one type: {CARRIER_TYPE}")

    # --- B. derive the value domain ---------------------------------------
    domain, unresolved = derive_domain(src, root)
    if unresolved:
        raise CannotAnswer(
            "a producer of `"
            + CARRIER_FIELD
            + "` assigns a non-literal, so the value domain is incomplete and a\n"
            "  verdict here would be unreliable:\n    "
            + "\n    ".join(unresolved)
        )
    if not domain:
        raise CannotAnswer(
            f"derived an EMPTY value domain for {CARRIER_TYPE}::{CARRIER_FIELD}.\n"
            "  Every comparison would read as dead. That is this gate's own\n"
            "  instrument failing, not a finding."
        )
    say(
        f"ok   derived domain from {len(src)} of {len(src_raw)} src file(s) "
        f"(the rest never mention freshness): {sorted(domain)}"
    )
    for value in sorted(domain):
        say(f"       {value!r:12} <- {', '.join(sorted(set(domain[value])))}")

    # --- C. the comparison scope must still be src/ only -------------------
    test_graded, _, _, _ = find_comparisons(tests, root)
    if test_graded:
        raise CannotAnswer(
            "a literal comparison on this carrier now exists under tests/, which\n"
            "  this gate does not grade (its domain is derived from src/ only):\n    "
            + "\n    ".join(f"{site} {op} {value!r}" for site, op, value in test_graded)
            + "\n  Widen the derivation to tests/ as well, or move the assertion."
        )
    say(
        f"ok   no literal comparison under tests/ "
        f"({len(tests)} of {len(tests_raw)} file(s) examined) -- scope holds"
    )

    # --- D. grade every direct-access comparison ---------------------------
    graded, aliases, non_literal, unclassified = find_comparisons(src, root)
    if unclassified:
        raise CannotAnswer(
            "a comparing shape on this carrier was not parsed, so it is UNGRADED:\n    "
            + "\n    ".join(unclassified)
            + "\n  Teach the gate this shape rather than leaving it unchecked."
        )

    dead = [(site, op, value) for site, op, value in graded if value not in domain]

    say(f"ok   graded {len(graded)} direct-access comparison(s)")
    if aliases:
        say(f"note {len(aliases)} alias site(s), reported but NOT graded (see docstring):")
        for item in aliases:
            say(f"       {item}")
    if non_literal:
        say(f"note {len(non_literal)} non-literal comparison(s), nothing to grade:")
        for item in non_literal:
            say(f"       {item}")

    if dead:
        print()
        print(f"FAIL: {len(dead)} comparison(s) can never be true.")
        print(f"  {CARRIER_TYPE}::{CARRIER_FIELD} can only ever be: {sorted(domain)}")
        for site, op, value in dead:
            verdict = "never true" if op == "==" else "always true"
            print(f"    {site}  {op} {value!r}  -> {verdict}")
        print()
        print("  This is bd-6zxuu's defect. The vocabularies are NOT interchangeable:")
        print(f"    raw        {CARRIER_TYPE}::{CARRIER_FIELD}  {sorted(domain)}")
        print("    normalised SwarmSourceAuthorityFreshness::freshness_state")
        print("                 {'fresh', 'stale', 'unknown'}")
        print("  Bridged in swarm_next_action.rs::source_authority_freshness_from_provenance.")
        print("  Fix the literal to one the field can hold. Do NOT add a producer")
        print("  for the wrong value to make the comparison reachable.")
        return EXIT_RED

    print(
        f"OK: {len(graded)} comparison(s) on {CARRIER_TYPE}::{CARRIER_FIELD}, "
        f"all against producible values {sorted(domain)}."
    )
    return EXIT_OK


# --------------------------------------------------------------------------
# Self-test
# --------------------------------------------------------------------------

FIXTURE_CARRIER = """
pub struct SwarmBriefSourceFreshness {
    pub observed_at: Option<String>,
    pub state: &'static str,
}

// The owner. Without a struct declaring a field named `freshness` of the
// carrier type, `.freshness.state` resolves to NOTHING and check A refuses --
// which is correct, and is what the first draft of this fixture got wrong.
pub struct Snap {
    pub freshness: SwarmBriefSourceFreshness,
}

impl SwarmBriefSourceFreshness {
    pub const fn current() -> Self {
        Self { observed_at: None, state: "current" }
    }
    pub const fn unknown() -> Self {
        Self { observed_at: None, state: "unknown" }
    }
}

fn stale_one(observed: String, stale: bool) -> SwarmBriefSourceFreshness {
    SwarmBriefSourceFreshness {
        observed_at: Some(observed),
        state: if stale { "stale" } else { "current" },
    }
}
"""


def _write(root: pathlib.Path, rel: str, body: str) -> None:
    path = root / rel
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(body)


_ARMS_RUN = [0]


def _arm(label: str, root: pathlib.Path, want: int, expect: str | None = None) -> bool:
    import contextlib
    import io

    _ARMS_RUN[0] += 1

    buffer = io.StringIO()
    try:
        with contextlib.redirect_stdout(buffer):
            got = run(root, verbose=True)
        detail = buffer.getvalue()
    except CannotAnswer as exc:
        got = EXIT_CANNOT
        detail = buffer.getvalue() + str(exc)

    ok = got == want and (expect is None or expect in detail)
    print(f"  {'ok  ' if ok else 'FAIL'} {label:62} (want {want}, got {got})")
    if not ok:
        for line in detail.splitlines():
            print(f"         {line}")
        if expect is not None and expect not in detail:
            print(f"         expected to find: {expect!r}")
    return ok


def self_test() -> int:
    real = repo_root()
    failures = 0

    print("=== the real tree ===")
    if not _arm("the tree this gate ships in is green", real, EXIT_OK):
        failures += 1

    # Both stripper directions, graded against the REAL tree rather than a
    # fixture, and through the real `find_comparisons` rather than a restated
    # copy of its parse -- an arm that reimplements the code it checks agrees
    # with that code's bugs.
    #
    # src/core/swarm_next_action.rs carries one of each by luck of history:
    #   :12429  `freshness.state == "stale"` inside a `//` comment  -> must NOT be graded
    #   :2107   `source.freshness.state == "current"` as live code  -> MUST be graded
    # Both anchors are located by CONTENT, never by line number. An earlier
    # draft hardcoded the two lines; a doc-comment edit to this same file then
    # shifted them, and the commented arm PASSED VACUOUSLY -- it asserted that
    # a line which no longer held a comment was not graded. An arm that can
    # pass because its target moved is decoration.
    target = real / "src/core/swarm_next_action.rs"
    if target.is_file():
        graded, _, _, _ = find_comparisons(load([target]), target.parent)
        graded_lines = {int(site.rsplit(":", 1)[1]) for site, _op, _v in graded}
        raw_lines = target.read_text(errors="replace").splitlines()

        # No leading dot: the in-tree comment writes it as `` `freshness.state ``
        # inside backticks, so requiring `.freshness.state` found nothing and
        # the arm reported NONE FOUND rather than passing on an empty set.
        commented = [
            n
            for n, line in enumerate(raw_lines, start=1)
            if line.lstrip().startswith("//")
            and f"{OWNER_FIELD}.{CARRIER_FIELD} ==" in line
        ]
        live = [
            n
            for n, line in enumerate(raw_lines, start=1)
            if not line.lstrip().startswith("//") and f"source{ACCESS_PATH} ==" in line
        ]

        for found, want_graded, why in (
            (commented, False, "every in-tree COMMENTED comparison is ungraded"),
            (live, True, "every in-tree LIVE comparison is graded"),
        ):
            # An empty anchor set means the arm proved nothing.
            _ARMS_RUN[0] += 1
            ok = bool(found) and all((n in graded_lines) == want_graded for n in found)
            print(f"  {'ok  ' if ok else 'FAIL'} {why:62} (lines {found or 'NONE FOUND'})")
            if not ok:
                print(f"         graded lines in that file: {sorted(graded_lines)}")
                failures += 1

    with tempfile.TemporaryDirectory(prefix="freshness_vocab_") as tmp:
        base = pathlib.Path(tmp)

        def fixture(name: str, extra_src: str, extra_tests: str = "") -> pathlib.Path:
            """A root carrying the real carrier shape plus the code under test."""
            root = base / name
            _write(root, "src/carrier.rs", FIXTURE_CARRIER)
            _write(root, "src/use.rs", extra_src)
            if extra_tests:
                _write(root, "tests/t.rs", extra_tests)
            return root

        def bare(name: str, files: dict[str, str]) -> pathlib.Path:
            """A root WITHOUT the carrier, for arms about a missing domain."""
            root = base / name
            (root / "src").mkdir(parents=True, exist_ok=True)
            for rel, body in files.items():
                _write(root, rel, body)
            return root

        print()
        print("=== GREEN ===")
        green = fixture(
            "green",
            'fn f(s: &Snap) -> bool { s.freshness.state == "current" }\n'
            'fn g(s: &Snap) -> bool { s.freshness.state != "stale" }\n'
            'fn h(f: &SwarmBriefSourceFreshness) -> bool { freshness.state == "unknown" }\n',
        )
        failures += not _arm("comparisons against producible values pass", green, EXIT_OK)
        failures += not _arm(
            "the derived domain is reported, not assumed", green, EXIT_OK, "'current'"
        )

        print()
        print("=== RED: the historical defect ===")
        failures += not _arm(
            "bd-6zxuu's own literal is refused",
            fixture("bug", 'fn f(s: &Snap) -> bool { s.freshness.state == "fresh" }\n'),
            EXIT_RED,
            'src/use.rs:1  == \'fresh\'',
        )
        failures += not _arm(
            "the RED names the producible set",
            fixture("bug2", 'fn f(s: &Snap) -> bool { s.freshness.state == "fresh" }\n'),
            EXIT_RED,
            "can only ever be: ['current', 'stale', 'unknown']",
        )
        failures += not _arm(
            "`!=` against a dead value is ALWAYS TRUE and also refused",
            fixture("ne", 'fn f(s: &Snap) -> bool { s.freshness.state != "fresh" }\n'),
            EXIT_RED,
            "always true",
        )
        failures += not _arm(
            "a reversed-operand comparison is refused",
            fixture("rev", 'fn f(s: &Snap) -> bool { "fresh" == s.freshness.state }\n'),
            EXIT_RED,
        )
        failures += not _arm(
            "assert_eq! against a dead value is refused",
            fixture("ae", 'fn t(s: &Snap) { assert_eq!(s.freshness.state, "fresh"); }\n'),
            EXIT_RED,
        )
        failures += not _arm(
            "a bare local comparison is refused",
            fixture("bare", 'fn f(f: &F) -> bool { freshness.state == "degraded" }\n'),
            EXIT_RED,
        )
        failures += not _arm(
            "the remedy forbids adding a producer for the wrong value",
            fixture("remedy", 'fn f(s: &Snap) -> bool { s.freshness.state == "fresh" }\n'),
            EXIT_RED,
            "Do NOT add a producer",
        )

        print()
        print("=== CONTROLS on the comment stripper (over-stripping = silent false PASS) ===")
        failures += not _arm(
            "a dead comparison in a `//` comment is NOT a finding",
            fixture("c1", 'fn f() {}\n// s.freshness.state == "fresh"\n'),
            EXIT_OK,
        )
        failures += not _arm(
            "a dead comparison in a `/* */` comment is NOT a finding",
            fixture("c2", 'fn f() {}\n/* s.freshness.state == "fresh" */\n'),
            EXIT_OK,
        )
        failures += not _arm(
            "a dead comparison in a NESTED block comment is NOT a finding",
            fixture("c3", 'fn f() {}\n/* a /* b */ s.freshness.state == "fresh" */\n'),
            EXIT_OK,
        )
        failures += not _arm(
            "a dead comparison inside a STRING is NOT a finding",
            fixture("c4", 'fn f() -> &str { "s.freshness.state == \\"fresh\\"" }\n'),
            EXIT_OK,
        )
        failures += not _arm(
            "a dead comparison inside a RAW string is NOT a finding",
            fixture("c5", 'fn f() -> &str { r#"s.freshness.state == "fresh""# }\n'),
            EXIT_OK,
        )
        # `br"` defeats a lookbehind that guards only a bare `r`, so the literal
        # lexes as an ordinary string, mis-terminates on the inner `\"`, and can
        # blank real code. Five of the files this gate lexes contain one.
        failures += not _arm(
            "a dead comparison inside a BYTE-raw string is NOT a finding",
            fixture("c5b", 'fn f() -> &[u8] { br#"s.freshness.state == "fresh""# }\n'),
            EXIT_OK,
        )
        failures += not _arm(
            "a byte-raw literal does not swallow the code after it",
            fixture(
                "c5c",
                'fn u() -> &[u8] { br#"a "quoted" tail"# }\n'
                'fn f(s: &Snap) -> bool { s.freshness.state == "fresh" }\n',
            ),
            EXIT_RED,
        )
        # THE arm that actually catches the `br"` bug. The two above pass even
        # with the broken lexer: mis-lexing a byte-raw string with an EVEN
        # number of inner quotes happens to re-synchronise. An ODD number does
        # not -- the bogus "string" state runs past the closing `"#` and
        # swallows the next line, masking the dead comparison and producing a
        # SILENT exit 0. Observed failing (0 instead of 1) against the
        # pre-`[bc]?` lexer.
        failures += not _arm(
            "an ODD inner quote in a byte-raw literal still cannot mask code",
            fixture(
                "c5d",
                'fn u() -> &[u8] { br#"has one " quote"# }\n'
                'fn f(s: &Snap) -> bool { s.freshness.state == "fresh" }\n',
            ),
            EXIT_RED,
        )
        # The direction that matters: the stripper must not eat live code.
        failures += not _arm(
            "a comment on the PRECEDING line does not hide the next line",
            fixture("c6", '// harmless\nfn f(s: &Snap) -> bool { s.freshness.state == "fresh" }\n'),
            EXIT_RED,
        )
        failures += not _arm(
            "a `//` inside a string does not comment out the rest of the file",
            fixture(
                "c7",
                'fn u() -> &str { "http://example.invalid" }\n'
                'fn f(s: &Snap) -> bool { s.freshness.state == "fresh" }\n',
            ),
            EXIT_RED,
        )
        failures += not _arm(
            "a lifetime `'static` does not open a char literal and swallow code",
            fixture(
                "c8",
                "fn u() -> &'static str { \"x\" }\n"
                'fn f(s: &Snap) -> bool { s.freshness.state == "fresh" }\n',
            ),
            EXIT_RED,
        )

        print()
        print("=== CANNOT ANSWER (3), never a pass ===")
        failures += not _arm(
            "an empty value domain refuses rather than calling everything dead",
            bare(
                "nodomain",
                {
                    # The type EXISTS, so check A passes; it simply has no
                    # constructor and no struct literal, so nothing produces a
                    # value. Every comparison would read as dead.
                    "src/carrier.rs": (
                        "pub struct SwarmBriefSourceFreshness {\n"
                        "    pub state: &'static str,\n"
                        "}\n"
                        "pub struct Holder {\n"
                        "    pub freshness: SwarmBriefSourceFreshness,\n"
                        "}\n"
                    ),
                    "src/u.rs": 'fn f(s: &Holder) -> bool { s.freshness.state == "fresh" }\n',
                },
            ),
            EXIT_CANNOT,
            "EMPTY value domain",
        )
        failures += not _arm(
            "a second type with a `state` field makes the path ambiguous",
            fixture(
                "ambig",
                "pub struct OtherFreshness {\n    pub state: &'static str,\n}\n"
                "pub struct OtherHolder {\n    pub freshness: OtherFreshness,\n}\n"
                'fn f(s: &Snap) -> bool { s.freshness.state == "current" }\n',
            ),
            EXIT_CANNOT,
            "no longer resolves to exactly one type",
        )
        failures += not _arm(
            "a `pub(crate)` second type is seen too (its own paren hid it)",
            fixture(
                "ambig_pubcrate",
                "pub struct OtherFreshness {\n    pub state: &'static str,\n}\n"
                "pub(crate) struct OtherHolder {\n    pub freshness: OtherFreshness,\n}\n"
                'fn f(s: &Snap) -> bool { s.freshness.state == "current" }\n',
            ),
            EXIT_CANNOT,
            "no longer resolves to exactly one type",
        )
        failures += not _arm(
            "a non-literal producer refuses -- the domain would be incomplete",
            fixture(
                "indirect",
                "fn mk(v: &'static str) -> SwarmBriefSourceFreshness {\n"
                "    SwarmBriefSourceFreshness { observed_at: None, state: v }\n"
                "}\n",
            ),
            EXIT_CANNOT,
            "non-literal",
        )
        failures += not _arm(
            "a graded comparison appearing under tests/ refuses -- scope moved",
            fixture(
                "testscope",
                'fn f(s: &Snap) -> bool { s.freshness.state == "current" }\n',
                'fn t(s: &Snap) { assert_eq!(s.freshness.state, "current"); }\n',
            ),
            EXIT_CANNOT,
            "under tests/",
        )
        failures += not _arm(
            "an unparsed comparing shape refuses rather than passing silently",
            fixture(
                "shape",
                'fn f(s: &Snap) -> bool { matches!(s.freshness.state, "fresh" | "current") }\n',
            ),
            EXIT_CANNOT,
            "UNGRADED",
        )
        failures += not _arm(
            "an empty src/ tree refuses -- it reads identically to a clean one",
            bare("bare_root", {}),
            EXIT_CANNOT,
            "empty tree reads identically",
        )

        print()
        print("=== the domain GROWS with its producers ===")
        failures += not _arm(
            "adding a real producer for a value makes its comparison live",
            fixture(
                "grow",
                "fn mk() -> SwarmBriefSourceFreshness {\n"
                '    SwarmBriefSourceFreshness { observed_at: None, state: "fresh" }\n'
                "}\n"
                'fn f(s: &Snap) -> bool { s.freshness.state == "fresh" }\n',
            ),
            EXIT_OK,
        )

    print()
    if failures:
        print(f"*** {failures} of {_ARMS_RUN[0]} SELF-TEST ARM(S) FAILED ***")
        return EXIT_RED
    print(f"self-test: all {_ARMS_RUN[0]} arms passed.")
    return EXIT_OK


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(
        description=(
            "Refuse a comparison of SwarmBriefSourceFreshness::state against a value "
            "no producer in the tree can emit (bd-6zxuu)."
        )
    )
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="drive every verdict path, including controls on the comment stripper",
    )
    parser.add_argument(
        "--quiet", action="store_true", help="print only the final verdict line"
    )
    args = parser.parse_args(argv)

    try:
        if args.self_test:
            return self_test()
        return run(repo_root(), verbose=not args.quiet)
    except CannotAnswer as exc:
        print(f"CANNOT ANSWER: {exc}", file=sys.stderr)
        return EXIT_CANNOT


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
