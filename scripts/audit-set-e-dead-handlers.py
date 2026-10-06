#!/usr/bin/env python3
"""READ-ONLY census of `set -e` dead failure-handlers (bd-q726t).

WHAT THE DEFECT IS. Under `set -e`, a SIMPLE assignment from a failing command
substitution adopts that command's status and aborts the script, so any `$?`
handler written after it is dead code. The author wrote failure handling; the
shell guarantees it never runs. The fix shape is

    if out="$(...)"; then rc=0; else rc=$?; fi

because a command in an `if` condition is exempt from `-e`.

THIS REPORTS CANDIDATES, NEVER FINDINGS, and it is deliberately NOT wired into
any gate. Confirming a site needs a planted failure executed AT that site. It is
an audit tool precisely because baselining candidates as accepted debt would
assert they are defects, which this census cannot establish.

THE PREDICATE IS FIVE CLAUSES, and the three subtractive ones each exist because
a control fired on real repository code. Do not simplify them away:

  1. assignment from `$(` but NOT `$((`. `$(` is a PREFIX of `$((`, so arithmetic
     expansion otherwise counts as command substitution. Two false rows
     (check-format.sh, e2e_overhaul/concurrency.sh) came from omitting this.
  2. the file's `set -e` must be SHELL, not heredoc text. check-format.sh has NO
     `set -e`; the `set -eu` at its :114 is a line inside a PYTHON heredoc that
     builds a generated git hook. Six false rows came from omitting this, and the
     error was only caught by RUNNING `check-format.sh --self-test` and finding
     its drift arm passed, which a static read had predicted was impossible.
  3. the ENCLOSING FUNCTION'S CALL SITE decides reachability. bash ignores `-e`
     for a command whose status is tested, and that suppression is inherited by
     the whole function body. So a handler inside a function called `f || ...`,
     `if f`, or `! f` DOES run. Six refutations came from this clause, including
     check-forbidden-deps.rs's audit_manifest (called `|| exit $?` at both
     sites). Dispatchers count too: verify.sh:1360 runs every stage function as
     `{ set +e; eval "$cmd"; }`, so stage functions are all exempt.

Two further shapes are KNOWN AND NOT YET IMPLEMENTED here. Treat the output as a
floor, not a population:

  4. `<cmd>` then `$?` with no assignment -- e.g. scripts/lib/e2e_logger.sh:617
     runs `"$@" >out 2>err` then `local rc=$?`.
  5. a later guard on the ASSIGNED VARIABLE instead of on `$?` -- e.g.
     e2e_overhaul/determinism.sh:353 assigns from a pipeline and the emptiness
     check that exists to catch no-output sits at :362, past the abort. This
     shape is plausibly MORE common, since testing the captured value reads as
     more natural than testing `$?`.

SCREEN NEGATIVE TESTS FIRST. Where a positive test's substitution fails only when
something is broken, a negative test's substitution is DESIGNED to fail on every
healthy run, so its handler is dead every time rather than rarely -- and the
script exits with the product's own failure code, so the harness bug wears the
product's clothes. handoff_hmac.sh:69 is that case: the assertion that a tampered
HMAC capsule is rejected cannot run.

USAGE
    scripts/audit-set-e-dead-handlers.py              census this repo
    scripts/audit-set-e-dead-handlers.py --self-test  8 arms, one per clause above
"""
import re, glob, os, sys

# `$(` is a PREFIX of `$((`, so a naive pattern counts arithmetic expansion as
# command substitution. `n=$((n + 1))` cannot fail the way a subshell can, and it
# put two false rows in the first census (check-format.sh:283,
# e2e_overhaul/concurrency.sh:160). The negative lookahead is the whole fix.
ASSIGN = re.compile(r"^\s*(?:local\s+|declare\s+|export\s+)?([A-Za-z_][A-Za-z0-9_]*)=\"?\$\((?!\()")
READS_Q = re.compile(r"\$\?")
SET_E = re.compile(r"^\s*set\s+-[a-zA-Z]*e[a-zA-Z]*\b|^\s*set\s+-o\s+errexit")
SET_PLUS_E = re.compile(r"^\s*set\s+\+[a-zA-Z]*e")
IF_COND = re.compile(r"^\s*(?:if|while|until|elif)\s")
GUARDED = re.compile(r"\|\|\s*(?:true|:|rc=|status=|echo)|\|\|\s*\{")


FUNC_DEF = re.compile(r"^([A-Za-z_][A-Za-z0-9_:-]*)\s*\(\)\s*\{")
# Contexts where bash IGNORES `set -e` for the tested command. Per bash(1), the
# suppression is inherited by the WHOLE function body, which is why the enclosing
# function's CALL SITE decides whether a `$?` handler inside it is reachable.
# Proven by execution 2026-10-05: a bare call aborted at the assignment and the
# handler never ran; the same function called `f || ...` ran its handler.
TEST_CTX = re.compile(r"^\s*(?:if|while|until|elif)\s+(?:!\s*)?(\S+)|^\s*!\s*(\S+)|^\s*(\S+)[^|&]*(?:\|\||&&)")


def function_ranges(lines):
    """[(name, start, end)] for `name() {` ... `}` at column 0 (the house style)."""
    out, cur, start = [], None, None
    for i, l in enumerate(lines):
        m = FUNC_DEF.match(l)
        if m and cur is None:
            cur, start = m.group(1), i
        elif cur is not None and l.rstrip() == "}":
            out.append((cur, start, i))
            cur = None
    return out


def call_contexts(name, all_lines):
    """How is `name` invoked across the tree? -> set of 'bare' / 'tested'."""
    kinds = set()
    call = re.compile(r"(?:^|\s|\(|;|&&|\|\|)" + re.escape(name) + r"(?:\s|$|;|\))")
    for lines in all_lines:
        for l in lines:
            s = l.strip()
            if not s or s.startswith("#") or FUNC_DEF.match(l):
                continue
            if not call.search(s):
                continue
            m = TEST_CTX.match(l)
            tested = bool(m and name in (m.group(1) or m.group(2) or m.group(3) or ""))
            # `f || x` / `f && x` / `if f` / `! f` all suppress -e in f's body.
            if tested or re.match(r"^\s*" + re.escape(name) + r"\b[^|&]*(\|\||&&)", l):
                kinds.add("tested")
            else:
                kinds.add("bare")
    return kinds


HEREDOC = re.compile(r"<<-?\s*'?\"?([A-Za-z_][A-Za-z0-9_]*)'?\"?\s*(?:$|[|&;])")


def shell_lines(lines):
    """Mask out heredoc bodies. A `python3 - <<'PY' ... PY` block is NOT shell, and
    counting its text as shell is how check-format.sh was wrongly admitted: the
    `set -eu` my first pass found at :114 is a line inside a Python string that
    builds a generated git hook. The file itself has only `set -uo pipefail`.
    Returns a parallel list where heredoc-body lines are None."""
    out, tag = [], None
    for l in lines:
        if tag is not None:
            out.append(None)
            if l.strip() == tag:
                tag = None
            continue
        out.append(l)
        m = HEREDOC.search(l)
        if m:
            tag = m.group(1)
    return out


def census(root):
    rows = []
    for path in sorted(set(glob.glob(os.path.join(root, "scripts", "**", "*.sh"), recursive=True))):
        try:
            lines = open(path, encoding="utf-8", errors="replace").read().splitlines()
        except OSError:
            continue
        shell = shell_lines(lines)
        if not any(l is not None and SET_E.search(l) for l in shell):
            continue
        active, on = [], False
        for l in shell:
            if l is not None and SET_E.search(l):
                on = True
            elif l is not None and SET_PLUS_E.search(l):
                on = False
            active.append(on)
        for i, line in enumerate(lines):
            if shell[i] is None:
                continue
            m = ASSIGN.match(line)
            if not m or IF_COND.match(line) or GUARDED.search(line):
                continue
            if not any(READS_Q.search(w) for w in lines[i + 1:i + 4]):
                continue
            rows.append({"file": os.path.relpath(path, root), "line": i + 1,
                         "var": m.group(1), "live": active[i], "_lines": lines,
                         "snippet": line.strip()[:78]})
    # Second pass: attribute each candidate to its enclosing function and decide
    # whether that function is ever invoked in a context that suppresses -e.
    corpus = []
    for p in sorted(set(glob.glob(os.path.join(root, "scripts", "**", "*.sh"), recursive=True))):
        try:
            corpus.append(open(p, encoding="utf-8", errors="replace").read().splitlines())
        except OSError:
            pass
    for r in rows:
        fns = function_ranges(r["_lines"])
        enclosing = next((n for n, s, e in fns if s < r["line"] - 1 <= e), None)
        r["fn"] = enclosing
        if enclosing is None:
            r["verdict"] = "TOP-LEVEL: -e applies, handler DEAD"
        else:
            kinds = call_contexts(enclosing, corpus)
            if "tested" in kinds:
                r["verdict"] = f"EXEMPT: {enclosing}() is called in a tested context"
            elif kinds:
                r["verdict"] = f"DEAD: {enclosing}() only ever called bare"
            else:
                r["verdict"] = f"UNKNOWN: no call site found for {enclosing}()"
        del r["_lines"]
    return rows


def self_test():
    import tempfile
    arms = []

    def arm(label, ok):
        arms.append(ok)
        print(f"  [{'ok  ' if ok else 'FAIL'}] {label}")

    with tempfile.TemporaryDirectory() as tmp:
        d = os.path.join(tmp, "scripts")
        os.makedirs(d)
        # POSITIVE: the known-broken shape.
        open(os.path.join(d, "bad.sh"), "w").write(
            'set -euo pipefail\nout="$(git rev-parse --show-toplevel)"\n'
            'case "$?" in\n  1) echo real ;;\nesac\n')
        # NEGATIVE 1: the fix shape -- assignment inside an `if` condition.
        open(os.path.join(d, "fixed.sh"), "w").write(
            'set -euo pipefail\nif out="$(git rev-parse --show-toplevel)"; then rc=0; else rc=$?; fi\n'
            'echo "$rc"\n')
        # NEGATIVE 2: no set -e at all -- out of scope for THIS half.
        open(os.path.join(d, "nosete.sh"), "w").write(
            'out="$(false)"\nrc=$?\necho "$rc"\n')
        # NEGATIVE 3: guarded with `|| true`, so set -e never fires.
        open(os.path.join(d, "guarded.sh"), "w").write(
            'set -e\nout="$(false)" || true\nrc=$?\necho "$rc"\n')
        # NEGATIVE 4: assignment present, but nothing reads $? nearby.
        open(os.path.join(d, "noq.sh"), "w").write(
            'set -e\nout="$(date)"\necho "$out"\necho done\n')
        # NEGATIVE 5: arithmetic expansion, not command substitution. Without the
        # lookahead this was flagged, because `$(` is a prefix of `$((`.
        open(os.path.join(d, "arith.sh"), "w").write(
            'set -e\nn=$((n + 1))\nrc=$?\necho "$rc"\n')
        # NEGATIVE 6: `set -e` and the whole shape appear only INSIDE a heredoc
        # that feeds another language. This is the real check-format.sh case.
        open(os.path.join(d, "heredoc.sh"), "w").write(
            'set -uo pipefail\npython3 - <<\'PY\'\nbody = """\nset -eu\nout=$(false)\n'
            'rc=$?\n"""\nPY\necho done\n')
        rows = census(tmp)
        got = {r["file"] for r in rows}
        arm("arithmetic `n=$((n + 1))` is NOT mistaken for a subshell",
            "scripts/arith.sh" not in got)
        arm("`set -e` inside a heredoc body does NOT admit the file",
            "scripts/heredoc.sh" not in got)
        arm("the known-broken shape is flagged", "scripts/bad.sh" in got)
        arm("the `if out=$(...)` fix shape is NOT flagged", "scripts/fixed.sh" not in got)
        arm("a file without set -e is NOT flagged", "scripts/nosete.sh" not in got)
        arm("`|| true` guarded assignment is NOT flagged", "scripts/guarded.sh" not in got)
        arm("an assignment with no nearby $? is NOT flagged", "scripts/noq.sh" not in got)
        arm("exactly one candidate across five fixtures", len(rows) == 1)
    print(f"\n[set-e-census] self-test: {sum(arms)}/{len(arms)} arms passed")
    return 0 if all(arms) else 1


if __name__ == "__main__":
    if "--self-test" in sys.argv:
        sys.exit(self_test())
    rows = census(".")
    live = [r for r in rows if r["live"]]
    print(f"candidate sites: {len(rows)}  (set -e active at the line: {len(live)})")
    import collections
    tally = collections.Counter(r["verdict"].split(":")[0] for r in live)
    for k, v in tally.most_common():
        print(f"    {k:<10} {v}")
    print()
    w = max((len(r["file"]) for r in rows), default=10)
    for r in sorted(live, key=lambda r: (r["verdict"], r["file"], r["line"])):
        print(f"  {r['file']:<{w}} :{r['line']:<5} {r['var']:<13} {r['verdict']}")
    dormant = [r for r in rows if not r["live"]]
    if dormant:
        print("\n  -- set +e in scope at the line, so the handler CAN run --")
        for r in sorted(dormant, key=lambda r: (r["file"], r["line"])):
            print(f"  {r['file']:<{w}} :{r['line']:<5} {r['var']}")
