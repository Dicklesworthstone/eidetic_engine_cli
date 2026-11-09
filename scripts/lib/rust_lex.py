"""Rust comment-blanking and string-literal masking, shared by the Python gates.

WHY THIS IS A MODULE AND NOT COPIED INTO EACH GATE. Every gate that scans Rust
source needs the same two things, and getting either wrong is silent:

  * comments must be BLANKED, not deleted, so reported line numbers stay real;
  * string literals must be MASKED TO SENTINELS rather than preserved, because
    a gate that searches the stripped text finds its own pattern written INSIDE
    a string and grades it as code.

The grammar has four traps that each produced a wrong answer that looked fine
(see `reference_rust_source_scanning_gate_traps`), and the fixes are recorded
inline below. A COPY of this lexer would not have received the byte-raw fix
made on 2026-11-09 -- `scripts/check-freshness-vocabulary.py` had shipped one
day earlier with that bug -- which is the whole argument for one definition.

Two consumers today:
  scripts/check-freshness-vocabulary.py   (bd-6zxuu; 31 self-test arms)
  scripts/check-e2e-execution-coverage.py (bd-e2d1q)

The arms that prove this lexer live in those gates' --self-test, because an
arm is only worth anything if something runs it, and `scripts/check-*.py` is
the glob this repository actually runs (CLAUDE.md). A module under scripts/lib/
is imported, never executed, so arms placed HERE would run nowhere -- which is
the exact defect class bd-e2d1q is about. Keep them in the gates.

Both consumers must be re-run after any change here. `lex` is a pure function
of its input, so "byte-identical output on the real tree, before and after" is
a cheap and complete check, and it is what the extraction of this module was
verified with.
"""

import re

# A masked literal becomes \x00<index>\x00 and its value is kept in a side
# table, so code that merely MENTIONS a pattern inside a string vanishes with
# the literal while the literals a gate needs to read are recoverable by index.
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
    """Enough of Rust's escape grammar for the short ASCII literals these gates
    compare. `\\u{..}` and byte escapes are left verbatim: mangling them would
    be worse than showing them."""
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
# walking every character in Python. A per-character scan of the 97 files the
# freshness gate reads cost 9.1 s; this form is the same grammar driven by one
# compiled alternation. Ordered longest-first so `r#"` and `//` win over `"`
# and `/`.
#
# The `[bc]?` is load-bearing. Rust's byte-raw literal is `br"..."`, and a
# lookbehind that only guards a bare `r` rejects the raw branch because `r`
# follows `b` -- so `br#"..."#` lexed as an ORDINARY string starting at the
# quote, which mis-terminates on any `"` inside and can blank real code.
# Measured 2026-11-09: 78 real `br"..."` literals in src/. (Zero `cr"`, but the
# prefix costs nothing and Rust has the form.) `b"..."` and `c"..."` are not
# raw and need no branch: the plain `"` alternative handles them, and the prefix
# letter stays as code.
#
# NOTE on measuring this yourself: grepping `br#*"` reports ~177 hits and most
# are the CONTENTS of the ordinary string "br" (the beads CLI name), where `br`
# sits just before the closing quote. Require a token boundary before the
# prefix -- not an identifier character and not a quote -- or the count is your
# own instrument talking.
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
            # `_unescape` over all 187k literals in the freshness gate's files
            # cost 0.9 s, and the overwhelming majority hold no backslash.
            atom(_unescape(inner) if "\\" in inner else inner, text[start:end])
            i = end
            continue

        if token == "'":
            # A char literal, or a lifetime (`'static`). Only a char literal
            # closes with `'`; a lifetime is left as code so `&'static str`
            # survives for a struct parser downstream.
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
