#!/usr/bin/env python3
r"""Flag shell sites where `set -e` aborts BEFORE the code written to read `$?` (bd-q726t).

THE DEFECT
    Under `set -e`, a SIMPLE ASSIGNMENT from a failing command substitution
    adopts that command's exit status and aborts the script. So

        output="$(some_cmd ...)"       # set -e takes the failure HERE
        rc=$?                          # never reached
        case "$rc" in
            1) ...real failure... ;;   # dead
            *) ...environment error... ;;
        esac

    is dead code. The author wrote failure handling; the shell guarantees it
    never runs. The fix shape is `out="$(...)" || rc=$?` or
    `if out="$(...)"; then rc=0; else rc=$?; fi` -- a command whose status is
    being tested is exempt from `-e`, so the handler survives.

    NEGATIVE TESTS ARE THE WORST CASE. A positive test's substitution fails
    only when something is broken. A negative test's substitution is DESIGNED
    to fail on every healthy run, so its handler is unreachable every time --
    and the script then exits with the product's own designed failure code,
    which reads as a product bug rather than a harness bug. The handoff-tamper
    arms were exactly this: the assertion that HMAC tamper detection works
    could not run, and the script exited 6 as though the product had misbehaved.

THE KNOWN POSITIVE, WHICH IS ALSO THIS FILE'S CONTROL
    scripts/e2e_repo_hygiene.sh carried this shape from 7ebb6d557 until
    4c3678daf. Executed then:

        no .git, before   exit 128, NO output, diagnosis printed 0 times
        no .git, after    exit 3 with the diagnosis
        has .git, after   exit 0, all 20 patterns matched

    `--control` replays that commit pair out of git and REQUIRES 1 hit at
    7ebb6d557 and 0 at 7ebb6d557^. A detector that returns 0 on the commit
    which introduced the bug is blind, and a clean scan from it means nothing.

FIVE INSTRUMENT FAULTS, EACH FOUND BY A CONTROL FIRING
    1. `$(` is a PREFIX of `$((`, so `n=$((n + 1))` counted as a command
       substitution. Negative lookahead; arm `arith_not_flagged`.
    2. The file's `set -e` must be SHELL, not heredoc TEXT. check-format.sh
       looked like a `set -e` file and is not: its `set -eu` is a line inside a
       python `body = '''#!/usr/bin/env bash ...'''` string that GENERATES a
       pre-push hook. Six false rows. Arm `heredoc_set_e_not_counted`.
    3. `set -e` suppression for a tested command is INHERITED BY THE WHOLE
       FUNCTION BODY, so the enclosing function's CALL SITE decides
       reachability, not the file's `set -e`. Six refutations. Executed:
       a bare call aborted at the assignment and the handler never ran;
       `f || ...`, `if f`, and `! f` all ran it and continued.
       Arm `tested_call_site_exempt`.
    4. A function dispatched as a QUOTED STRING has no syntactic call site.
       verify.sh does `run_stage "Unit..." "unit_contract_golden_tests"` and
       `run_stage` runs it as `{ set +e; eval "$cmd"; }`, so every verify.sh
       stage function has `-e` explicitly off. A call-site pass that finds no
       caller must say UNKNOWN, not DEAD. Arm `string_dispatch_is_unknown`.
    5. A backslash continuation hides the guard. The repaired sites read
           output=$("$EE_BINARY" "$@" 2>"$err") \
               || command_status=$?
       and a per-line matcher sees only the first line and flags the fix.
       Statements are joined on trailing `\` and on unbalanced `$(` before
       matching. Arm `continuation_guard_seen`.

WHAT THIS CHECK DOES NOT CLAIM
    A finding is a CANDIDATE, not a confirmed defect: proving one needs a
    planted failure at the real site. What the three clauses buy is that the
    list is not half false. The `set +e`/`set -e` scope is tracked
    LEXICALLY, and `set` is process-global in bash -- a `set +e` executed in a
    function persists after it returns -- so a site whose `-e` state depends on
    run-time order can be misread in either direction.

TWO CLASSES THAT ARE NOT IN THIS REPO, MEASURED RATHER THAN ASSUMED
    `local rc=$(cmd)` does NOT abort under `set -e`: the status becomes the
    `local` BUILTIN's, which is 0. So `local out=$(false); local rc=$?` leaves
    rc=0 -- the handler runs and can never see the failure. That is worse than
    a dead handler, because it reports PASS. Same for `declare`, `export`,
    `readonly`, and for `out=$(cmd) || true; rc=$?`.

    Measured over scripts/**/*.sh: ZERO declaration-builtin command
    substitutions exist (every grep hit for the shape is `$((` arithmetic), so
    the class is empty here. The house idiom already separates declaration from
    assignment -- `local out; out="$(...)"` -- which aborts honestly instead.
    `--json` reports `always_zero` so the zero stays visible and a new one is
    caught the day it lands.

SHRINK-ONLY BASELINE, FAILING IN BOTH DIRECTIONS
    Per the house pattern (scripts/action-pins-baseline.txt,
    scripts/production-reachability-baseline.txt):
      * a NEW candidate site is an error;
      * a baselined site that has since been repaired is ALSO an error, telling
        you to delete the line. That is what stops the baseline decaying into a
        permanent ignore-list.

USAGE
    scripts/check-dead-exit-code-handlers.py
    scripts/check-dead-exit-code-handlers.py --json
    scripts/check-dead-exit-code-handlers.py --self-test
    scripts/check-dead-exit-code-handlers.py --control    # replays 7ebb6d557
"""

from __future__ import annotations

import argparse
import json
import pathlib
import re
import subprocess
import sys

BASELINE = "scripts/dead-exit-code-handler-baseline.txt"

# A zero census over zero files is not a measurement. scripts/ has held
# hundreds of shell files for the life of this repo; if the walk returns fewer
# than this, the walk is the bug.
MINIMUM_PLAUSIBLE_SHELL_FILES = 100

# ---------------------------------------------------------------------------
# lexing: heredocs, comments, quotes
# ---------------------------------------------------------------------------

HEREDOC_OPEN = re.compile(r"<<-?\s*(?:'([^']+)'|\"([^\"]+)\"|([A-Za-z_][A-Za-z0-9_]*))")


def heredoc_body_lines(lines: list[str]) -> set[int]:
    """0-based indices of lines that are heredoc BODY, not shell.

    Fault 2 lives here: a `set -e` inside a python heredoc is text, and so is
    an assignment inside one. Both must be invisible to every later pass.
    """
    body: set[int] = set()
    i = 0
    while i < len(lines):
        line = lines[i]
        if not lines[i].lstrip().startswith("#"):
            m = HEREDOC_OPEN.search(line)
            if m:
                delim = m.group(1) or m.group(2) or m.group(3)
                j = i + 1
                while j < len(lines) and lines[j].strip() != delim:
                    body.add(j)
                    j += 1
                if j < len(lines):
                    body.add(j)  # the terminator line itself is not shell
                i = j + 1
                continue
        i += 1
    return body


def strip_quoted(line: str) -> str:
    """Blank out single- and double-quoted spans, keeping the line's length.

    Brace and paren counting on raw lines desyncs on an unbalanced brace inside
    a string literal. Lengths are preserved so column offsets stay usable.
    """
    out = []
    quote = None
    esc = False
    for ch in line:
        if esc:
            out.append(" ")
            esc = False
            continue
        if ch == "\\":
            out.append(ch if quote is None else " ")
            esc = quote != "'"
            continue
        if quote:
            if ch == quote:
                quote = None
                out.append(ch)
            else:
                out.append(" ")
            continue
        if ch in ("'", '"'):
            quote = ch
            out.append(ch)
            continue
        out.append(ch)
    return "".join(out)


def code_line(line: str, is_heredoc: bool) -> str:
    """The shell-significant text of a line: '' for heredoc body and comments."""
    if is_heredoc:
        return ""
    if line.lstrip().startswith("#"):
        return ""
    return line


# ---------------------------------------------------------------------------
# statements: join continuations so a guard on the next line is visible
# ---------------------------------------------------------------------------


def logical_statements(lines: list[str], hd: set[int]) -> list[tuple[int, str]]:
    """[(first 0-based line index, joined text)] for each logical statement.

    Joins while the line ends with a backslash or while `$(` is unbalanced
    (command substitution may span newlines with no continuation marker).
    Fault 5 is what this exists for.
    """
    stmts: list[tuple[int, str]] = []
    i = 0
    while i < len(lines):
        raw = code_line(lines[i], i in hd)
        if not raw.strip():
            i += 1
            continue
        start = i
        parts = [raw]
        while True:
            joined = " ".join(p.rstrip("\\").rstrip() for p in parts)
            scrubbed = strip_quoted(joined)
            depth = scrubbed.count("$(") + scrubbed.count("`") % 2
            closes = scrubbed.count(")")
            opens = scrubbed.count("(")
            unbalanced = opens > closes
            continued = parts[-1].rstrip().endswith("\\")
            if not (continued or (depth and unbalanced)):
                break
            i += 1
            if i >= len(lines):
                break
            parts.append(code_line(lines[i], i in hd))
        stmts.append((start, " ".join(p.rstrip("\\").rstrip() for p in parts)))
        i += 1
    return stmts


# ---------------------------------------------------------------------------
# predicates
# ---------------------------------------------------------------------------

# Clause 1: `$(` but never `$((`.
ASSIGN_SUBST = re.compile(r"^\s*([A-Za-z_][A-Za-z0-9_]*)=\s*\"?\$\((?!\()")
DECL_SUBST = re.compile(
    r"^\s*(?:local|declare|typeset|export|readonly)\s+(?:-\w+\s+)*"
    r"([A-Za-z_][A-Za-z0-9_]*)=\s*\"?\$\((?!\()"
)
# `$?` is the status of the MOST RECENTLY EXECUTED pipeline, so a read three
# statements later belongs to whatever ran in between -- not to the capture.
# Fault 6: the filed predicate's "within 3 statements" window is where its own
# overcount came from. Two clauses replace it:
#   * the reader must be the IMMEDIATELY NEXT logical statement, and
#   * `$?` must be read BEFORE anything else in that statement runs, or the
#     status being read is that statement's own.
# `env cmd ... || exit_code=$?` satisfies neither: it is both the command and
# the read, and it is guarded.
PURE_READ = re.compile(
    r"""^\s*(?:
          (?:local|declare|typeset|export|readonly)\s+(?:-\w+\s+)*)?
        [A-Za-z_][A-Za-z0-9_]*=\s*["']?\$\?
      | ^\s*(?:if|elif|while|until)\s+(?:!\s+)?(?:\[\[?|test)\s[^&|;]*\$\?
      | ^\s*case\s+["']?\$\?
      | ^\s*\(\(\s*\$?\??\s*\$\?
      | ^\s*return\s+["']?\$\?
      | ^\s*exit\s+["']?\$\?
    """,
    re.VERBOSE,
)


def pure_exit_status_read(stmt: str) -> bool:
    return bool(PURE_READ.match(stmt))
FUNC_DEF = re.compile(r"^\s*(?:function\s+)?([A-Za-z_][A-Za-z0-9_:.-]*)\s*\(\)\s*(\{)?")
SET_E_ON = re.compile(r"^\s*set\s+-[A-Za-z]*e")
SET_E_OFF = re.compile(r"^\s*set\s+\+[A-Za-z]*e")
TESTED_PREFIX = re.compile(r"^\s*(if|while|until|elif|!)\s")


def statement_is_guarded(stmt: str) -> bool:
    """True when the statement's own status is tested, so `-e` is suppressed."""
    if TESTED_PREFIX.match(stmt):
        return True
    scrubbed = strip_quoted(stmt)
    # A top-level `||` or `&&` -- outside any $( ) -- tests the left side.
    depth = 0
    k = 0
    while k < len(scrubbed) - 1:
        two = scrubbed[k : k + 2]
        if two == "$(":
            depth += 1
            k += 2
            continue
        if scrubbed[k] == ")" and depth:
            depth -= 1
            k += 1
            continue
        if depth == 0 and two in ("||", "&&"):
            return True
        k += 1
    return False


def set_e_state(code: list[str], upto: int) -> bool:
    """Lexical `-e` state at line index `upto`.

    Bounded, and the bound is stated in the module docstring: `set` is
    process-global, so run-time order can differ from lexical order.
    """
    state = False
    for i in range(upto):
        line = code[i]
        if SET_E_OFF.match(line):
            state = False
        elif SET_E_ON.match(line):
            state = True
    return state


def function_spans(code: list[str]) -> list[tuple[str, int, int]]:
    """[(name, first body line, last body line)] using literal-safe braces."""
    spans: list[tuple[str, int, int]] = []
    i = 0
    while i < len(code):
        m = FUNC_DEF.match(code[i])
        if m and ("{" in strip_quoted(code[i]) or (i + 1 < len(code) and strip_quoted(code[i + 1]).strip() == "{")):
            name = m.group(1)
            j = i if "{" in strip_quoted(code[i]) else i + 1
            depth = 0
            started = False
            while j < len(code):
                s = strip_quoted(code[j])
                depth += s.count("{") - s.count("}")
                if "{" in s:
                    started = True
                if started and depth <= 0:
                    break
                j += 1
            spans.append((name, i, min(j, len(code) - 1)))
            i = j + 1
            continue
        i += 1
    return spans


def enclosing_function(spans: list[tuple[str, int, int]], idx: int) -> str | None:
    best = None
    for name, a, b in spans:
        if a <= idx <= b and (best is None or (b - a) < (best[2] - best[1])):
            best = (name, a, b)
    return best[0] if best else None


# ---------------------------------------------------------------------------
# call-context classification (clause 3 + fault 4)
# ---------------------------------------------------------------------------


def classify_call_sites(name: str, corpus: dict[str, list[str]]) -> str:
    """'BARE' / 'TESTED' / 'UNKNOWN' for every invocation of `name`.

    BARE    at least one call site runs it with its status untested, so `-e`
            applies inside the body and the handler is dead there.
    TESTED  every call site tests it (`if f`, `f ||`, `f &&`, `! f`), so `-e`
            is suppressed throughout the body and the handler is live.
    UNKNOWN no syntactic call site at all -- fault 4. Most often a function
            dispatched by NAME as a string, which verify.sh runs under an
            injected `set +e`. Never guessed as BARE.
    """
    word = re.compile(r"(?<![A-Za-z0-9_./-])" + re.escape(name) + r"(?![A-Za-z0-9_-])")
    saw_any = False
    saw_bare = False
    for path, lines in corpus.items():
        hd = heredoc_body_lines(lines)
        code = [code_line(l, i in hd) for i, l in enumerate(lines)]
        spans = function_spans(code)
        for i, line in enumerate(code):
            if not line.strip() or not word.search(strip_quoted(line) if '"' not in line else line):
                continue
            # a definition is not a call
            m = FUNC_DEF.match(line)
            if m and m.group(1) == name:
                continue
            # self-recursion inside the function body is not an external caller
            if path and enclosing_function(spans, i) == name:
                continue
            quoted = re.search(r"[\"']" + re.escape(name) + r"[\"']", line)
            bare_word = word.search(re.sub(r"[\"'][^\"']*[\"']", "", line))
            if quoted and not bare_word:
                continue  # string dispatch: no syntactic call site here
            saw_any = True
            if not statement_is_guarded(line):
                saw_bare = True
    if not saw_any:
        return "UNKNOWN"
    return "BARE" if saw_bare else "TESTED"


# ---------------------------------------------------------------------------
# the scan
# ---------------------------------------------------------------------------


def scan_file(path: str, lines: list[str], corpus: dict[str, list[str]]) -> tuple[list[dict], list[dict]]:
    hd = heredoc_body_lines(lines)
    code = [code_line(l, i in hd) for i, l in enumerate(lines)]
    if not any(SET_E_ON.match(l) for l in code):
        # Clause 2: no SHELL `set -e` anywhere -> the abort class cannot occur.
        # The always-zero class still can, so keep scanning for it.
        set_e_file = False
    else:
        set_e_file = True
    spans = function_spans(code)
    stmts = logical_statements(lines, hd)
    dead: list[dict] = []
    always_zero: list[dict] = []

    for n, (idx, stmt) in enumerate(stmts):
        decl = DECL_SUBST.match(stmt)
        assign = ASSIGN_SUBST.match(stmt)
        if not (decl or assign):
            continue
        if n + 1 >= len(stmts):
            continue
        reader = stmts[n + 1]
        if not pure_exit_status_read(reader[1]):
            continue

        record = {
            "file": path,
            "line": idx + 1,
            "statement": re.sub(r"\s+", " ", stmt.strip())[:160],
            "reader_line": reader[0] + 1,
            "reader": re.sub(r"\s+", " ", reader[1].strip())[:120],
            "function": enclosing_function(spans, idx),
        }

        if decl:
            # The declaration builtin swallows the status: the handler is live
            # but can only ever see 0. Reported whether or not `set -e` is set.
            record["class"] = "always_zero"
            record["reason"] = "declaration builtin returns its own status (0)"
            always_zero.append(record)
            continue

        if statement_is_guarded(stmt):
            continue
        if not set_e_file or not set_e_state(code, idx):
            continue

        fn = record["function"]
        if fn is None:
            record["call_context"] = "TOP_LEVEL"
        else:
            record["call_context"] = classify_call_sites(fn, corpus)
        if record["call_context"] in ("TESTED", "UNKNOWN"):
            continue
        record["class"] = "dead_handler"
        dead.append(record)

    return dead, always_zero


def shell_files(root: str = "scripts") -> dict[str, list[str]]:
    corpus: dict[str, list[str]] = {}
    for p in sorted(pathlib.Path(root).rglob("*.sh")):
        try:
            corpus[str(p)] = p.read_text(errors="replace").splitlines()
        except OSError:
            continue
    return corpus


def audit(corpus: dict[str, list[str]], minimum: int = MINIMUM_PLAUSIBLE_SHELL_FILES):
    if len(corpus) < minimum:
        raise LookupError(
            f"resolved {len(corpus)} shell file(s), fewer than the "
            f"{minimum} this repository has always had. A zero census over a "
            f"collapsed file walk is not a measurement; fix the walk."
        )
    dead: list[dict] = []
    zero: list[dict] = []
    for path, lines in corpus.items():
        d, z = scan_file(path, lines, corpus)
        dead.extend(d)
        zero.extend(z)
    dead.sort(key=lambda r: (r["file"], r["line"]))
    zero.sort(key=lambda r: (r["file"], r["line"]))
    return dead, zero


def read_baseline() -> set[str]:
    p = pathlib.Path(BASELINE)
    if not p.exists():
        return set()
    out = set()
    for line in p.read_text().splitlines():
        line = line.split("#", 1)[0].strip()
        if line:
            out.add(line)
    return out


# ---------------------------------------------------------------------------
# self-test: one arm per instrument fault, each a mutant that must flip
# ---------------------------------------------------------------------------

ARMS: list[tuple[str, str, int]] = [
    (
        "known_positive_shape",
        """#!/bin/bash
set -euo pipefail
output="$(git check-ignore -v -- "$path")"
status=$?
if [ "$status" -ne 0 ]; then
    echo "no git"
fi
""",
        1,
    ),
    (
        "or_guard_not_flagged",
        """#!/bin/bash
set -euo pipefail
rc=0
output="$(some_cmd)" || rc=$?
echo "$rc"
""",
        0,
    ),
    (
        "continuation_guard_seen",
        """#!/bin/bash
set -euo pipefail
command_status=0
output=$("$EE_BINARY" run --workspace "$WS" 2>"$err") \\
    || command_status=$?
echo "$command_status"
""",
        0,
    ),
    (
        "if_guard_not_flagged",
        """#!/bin/bash
set -euo pipefail
if output="$(some_cmd)"; then rc=0; else rc=$?; fi
echo "$rc"
""",
        0,
    ),
    (
        "arith_not_flagged",
        """#!/bin/bash
set -euo pipefail
failures=$((failures + 1))
echo "$?"
""",
        0,
    ),
    (
        "heredoc_set_e_not_counted",
        """#!/bin/bash
set -uo pipefail
python3 - <<'PY'
body = '''#!/usr/bin/env bash
set -eu
echo hook
'''
PY
output="$(some_cmd)"
status=$?
echo "$status"
""",
        0,
    ),
    (
        "heredoc_body_site_not_flagged",
        """#!/bin/bash
set -euo pipefail
cat <<'SH' > /dev/null
output="$(some_cmd)"
status=$?
SH
echo done
""",
        0,
    ),
    (
        "set_plus_e_in_scope_not_flagged",
        """#!/bin/bash
set -euo pipefail
set +e
output="$(some_cmd)"
status=$?
set -e
echo "$status"
""",
        0,
    ),
    (
        "bare_call_site_is_dead",
        """#!/bin/bash
set -euo pipefail
run_case() {
    output="$(some_cmd)"
    status=$?
    echo "$status"
}
run_case
""",
        1,
    ),
    (
        "tested_call_site_exempt",
        """#!/bin/bash
set -euo pipefail
run_case() {
    output="$(some_cmd)"
    status=$?
    echo "$status"
}
run_case || echo "rc=$?"
""",
        0,
    ),
    (
        "bang_call_site_exempt",
        """#!/bin/bash
set -euo pipefail
run_case() {
    output="$(some_cmd)"
    status=$?
    echo "$status"
}
! run_case
""",
        0,
    ),
    (
        "string_dispatch_is_unknown",
        """#!/bin/bash
set -euo pipefail
unit_contract_golden_tests() {
    output="$(some_cmd)"
    status=$?
    echo "$status"
}
run_stage "Unit, Contract, and Golden Tests" "unit_contract_golden_tests"
""",
        0,
    ),
    (
        "no_set_e_not_flagged",
        """#!/bin/bash
set -uo pipefail
output="$(some_cmd)"
status=$?
echo "$status"
""",
        0,
    ),
    (
        "reader_too_far_not_flagged",
        """#!/bin/bash
set -euo pipefail
output="$(some_cmd)"
a=1
b=2
c=3
d=4
status=$?
""",
        0,
    ),
    # Fault 6, both halves. Each of these flagged a real file before the
    # window was narrowed to the immediately-next statement.
    (
        "intervening_statement_owns_the_status",
        """#!/bin/bash
set -euo pipefail
run_step() {
    start_ns="$(now_ns)"
    set +e
    "$EE_BINARY" "$@" >"$out" 2>"$err"
    status=$?
    set -e
}
run_step
""",
        0,
    ),
    (
        "reader_is_its_own_guarded_capture",
        """#!/bin/bash
set -euo pipefail
run_case() {
    start="$(now_ms)"
    env NO_COLOR=1 "${EE_BINARY}" "$@" >"$out" 2>"$err" || exit_code=$?
}
run_case
""",
        0,
    ),
    (
        "conditional_read_still_flagged",
        """#!/bin/bash
set -euo pipefail
output="$(some_cmd)"
if [ "$?" -ne 0 ]; then echo bad; fi
""",
        1,
    ),
]

ZERO_ARMS: list[tuple[str, str, int]] = [
    (
        "declaration_builtin_always_zero",
        """#!/bin/bash
set -euo pipefail
f() {
    local output="$(some_cmd)"
    local status=$?
    echo "$status"
}
f
""",
        1,
    ),
    (
        "split_declaration_is_not_always_zero",
        """#!/bin/bash
set -euo pipefail
f() {
    local output
    output="$(some_cmd)" || return $?
    echo ok
}
f
""",
        0,
    ),
]


def self_test() -> int:
    failures = 0
    print("check-dead-exit-code-handlers self-test")
    print("\n  dead-handler arms")
    for name, body, want in ARMS:
        lines = body.splitlines()
        corpus = {"arm.sh": lines}
        dead, _ = audit(corpus, minimum=1)
        got = len(dead)
        ok = got == want
        failures += 0 if ok else 1
        print(f"    {'ok  ' if ok else 'FAIL'}  {name:36} want={want} got={got}")
        if not ok:
            for d in dead:
                print(f"            {d['file']}:{d['line']} {d['statement'][:70]}")

    print("\n  always-zero arms")
    for name, body, want in ZERO_ARMS:
        corpus = {"arm.sh": body.splitlines()}
        _, zero = audit(corpus, minimum=1)
        got = len(zero)
        ok = got == want
        failures += 0 if ok else 1
        print(f"    {'ok  ' if ok else 'FAIL'}  {name:36} want={want} got={got}")

    print("\n  refusal arm")
    try:
        audit({}, minimum=MINIMUM_PLAUSIBLE_SHELL_FILES)
        print("    FAIL  empty_corpus_refused                 no LookupError raised")
        failures += 1
    except LookupError:
        print("    ok    empty_corpus_refused                 refused a zero-file census")

    total = len(ARMS) + len(ZERO_ARMS) + 1
    print()
    if failures:
        print(f"self-test: {failures} of {total} arm(s) FAILED")
        return 1
    print(f"self-test: {total}/{total} arms passed")
    return 0


def control() -> int:
    """Replay the known positive out of git: 1 hit at 7ebb6d557, 0 at its parent.

    A detector that reports 0 on the commit which INTRODUCED the bug is blind,
    and a clean scan from it would mean nothing.
    """
    path = "scripts/e2e_repo_hygiene.sh"
    expect = [("7ebb6d557^", 0), ("7ebb6d557", 1)]
    failures = 0
    print("check-dead-exit-code-handlers control (bd-13y74 / 7ebb6d557)")
    for treeish, want in expect:
        try:
            blob = subprocess.run(
                ["git", "show", f"{treeish}:{path}"],
                capture_output=True,
                text=True,
                check=True,
            ).stdout
        except (subprocess.CalledProcessError, FileNotFoundError) as exc:
            print(f"  SKIP  {treeish}: cannot read the blob ({exc})")
            print("  REFUSING: a control that did not run is not a control.")
            return 2
        dead, _ = audit({path: blob.splitlines()}, minimum=1)
        got = len(dead)
        ok = got == want
        failures += 0 if ok else 1
        print(f"  {'ok  ' if ok else 'FAIL'}  {treeish:12} want={want} got={got}")
        for d in dead:
            print(f"          :{d['line']} {d['statement'][:84]}")
    print()
    if failures:
        print("control: FAILED -- do not trust this detector's census")
        return 1
    print("control: both polarities as expected")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--json", action="store_true", help="machine-readable census")
    ap.add_argument("--self-test", action="store_true", help="run the planted arms")
    ap.add_argument("--control", action="store_true", help="replay the 7ebb6d557 pair")
    args = ap.parse_args()

    if args.self_test:
        return self_test()
    if args.control:
        return control()

    corpus = shell_files()
    try:
        dead, zero = audit(corpus)
    except LookupError as exc:
        print(f"[dead-handlers] REFUSING: {exc}", file=sys.stderr)
        return 2

    keys = {f"{d['file']}:{d['line']}" for d in dead}
    baseline = read_baseline()
    new = sorted(keys - baseline)
    repaired = sorted(baseline - keys)

    if args.json:
        print(
            json.dumps(
                {
                    "schema": "ee.check.dead_exit_code_handlers.v1",
                    "shell_files": len(corpus),
                    "dead_handler": dead,
                    "always_zero": zero,
                    "baseline": sorted(baseline),
                    "new": new,
                    "repaired": repaired,
                },
                indent=2,
            )
        )
        return 1 if (new or repaired) else 0

    print(
        f"[dead-handlers] {len(dead)} dead-handler candidate(s), "
        f"{len(zero)} always-zero site(s) across {len(corpus)} shell file(s); "
        f"baseline holds {len(baseline)}"
    )
    for d in dead:
        mark = "NEW " if f"{d['file']}:{d['line']}" in new else "    "
        print(f"  {mark}{d['file']}:{d['line']}  fn={d['function']} ctx={d['call_context']}")
        print(f"        {d['statement'][:110]}")
        print(f"        reads $? at :{d['reader_line']}  {d['reader'][:80]}")
    for z in zero:
        print(f"  ZERO {z['file']}:{z['line']}  {z['reason']}")
        print(f"        {z['statement'][:110]}")

    rc = 0
    if new:
        print(
            "\n[dead-handlers] FAIL: new candidate site(s). Under `set -e` the "
            "assignment aborts before `$?` is read, so the handler below it is "
            "dead. Use `out=\"$(...)\" || rc=$?`."
        )
        for k in new:
            print(f"    + {k}")
        rc = 1
    if repaired:
        print(
            f"\n[dead-handlers] FAIL: baselined site(s) no longer flagged. "
            f"Delete them from {BASELINE} so the baseline cannot decay into a "
            f"permanent ignore-list."
        )
        for k in repaired:
            print(f"    - {k}")
        rc = 1
    if rc == 0:
        print("[dead-handlers] ok: census matches the baseline exactly")
    return rc


if __name__ == "__main__":
    sys.exit(main())
