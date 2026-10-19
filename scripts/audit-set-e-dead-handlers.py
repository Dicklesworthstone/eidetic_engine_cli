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
# A line that CONSUMES the previous command's status, as opposed to one that
# runs something and then reads its own. `rc=$?` and `case "$?" in` consume;
# `env ... || exit_code=$?` does not -- its `$?` is its own.
HANDLER = re.compile(
    r"^(?:local\s+|declare\s+|export\s+)?[A-Za-z_][A-Za-z0-9_]*=\"?\$\?"
    r"|^(?:case|if|while|until|elif|test|\[\[|\[)\b[^|&]*\$\?"
)
# The capture form alone. Shape 4 requires this rather than the full HANDLER:
# an `if`/`case` line merely CONTAINING `$?` may be consuming its own
# condition's status, which is not evidence about the line above it.
PURE_HANDLER = re.compile(
    r"^(?:local\s+|declare\s+|export\s+)?[A-Za-z_][A-Za-z0-9_]*=\"?\$\?"
)
# `n=$((...))` is arithmetic, not a spawned command.
ARITH = re.compile(r"^(?:local\s+|declare\s+|export\s+)?[A-Za-z_][A-Za-z0-9_]*=\$\(\(")
SET_E = re.compile(r"^\s*set\s+-[a-zA-Z]*e[a-zA-Z]*\b|^\s*set\s+-o\s+errexit")
SET_PLUS_E = re.compile(r"^\s*set\s+\+[a-zA-Z]*e")
IF_COND = re.compile(r"^\s*(?:if|while|until|elif)\s")
# `|| <anything>=` rather than a vocabulary of variable names. The old form
# listed `rc=` and `status=` literally and so did not recognise
# `|| TAMPERED_RC=$?` -- the exact repair this census asks callers to make,
# spelled in the UPPERCASE convention the e2e scripts use. A detector that
# flags its own fix shape trains people to revert correct code.
GUARDED = re.compile(r"\|\|\s*(?:true|:|echo)|\|\|\s*[A-Za-z_][A-Za-z0-9_]*=|\|\|\s*\{")


FUNC_DEF = re.compile(r"^([A-Za-z_][A-Za-z0-9_:-]*)\s*\(\)\s*\{")
# Contexts where bash IGNORES `set -e` for the tested command. Per bash(1), the
# suppression is inherited by the WHOLE function body, which is why the enclosing
# function's CALL SITE decides whether a `$?` handler inside it is reachable.
# Proven by execution 2026-10-05: a bare call aborted at the assignment and the
# handler never ran; the same function called `f || ...` ran its handler.
TEST_CTX = re.compile(r"^\s*(?:if|while|until|elif)\s+(?:!\s*)?(\S+)|^\s*!\s*(\S+)|^\s*(\S+)[^|&]*(?:\|\||&&)")


def _decomment(line):
    """Drop a shell comment, respecting quotes.

    Quote tracking is not pedantry here: `RC=$?  # capture` must KEEP its
    handler, and a '#' inside "...#..." must not truncate real code.
    """
    in_s = None
    esc = False
    for i, c in enumerate(line):
        if esc:
            esc = False
        elif c == "\\":
            esc = True
        elif in_s:
            if c == in_s:
                in_s = None
        elif c in "\"'":
            in_s = c
        elif c == "#":
            return line[:i]
    return line


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
        # -e STATE AT THE CALL SITE, NOT AT THE DEFINITION. A function's
        # runtime errexit state comes from wherever it is invoked, so a call
        # bracketed by `set +e` / `set -e` suppresses -e through the whole body
        # exactly as `f || x` does. This file's header already accepts the
        # principle for dispatchers (verify.sh runs stages as
        # `{ set +e; eval "$cmd"; }`); this makes it general.
        #
        # Without it, rch_verify.sh's run_rch_invocation_once and
        # run_rch_invocation_retry were reported DEAD. Both are called at
        # :7524 and :7571, each between a `set +e` and a `set -e`, so their
        # `local status=$?` runs correctly. Those were the census's last two
        # DEAD rows and both were wrong -- in the dispatch wrapper, where
        # "repairing" working error handling would have been the worst place
        # to be wrong.
        errexit = False
        for l in lines:
            bare = _decomment(l)
            if SET_E.search(bare):
                errexit = True
            elif SET_PLUS_E.search(bare):
                errexit = False
            s = l.strip()
            if not s or s.startswith("#") or FUNC_DEF.match(l):
                continue
            if not call.search(s):
                continue
            if not errexit:
                kinds.add("tested")
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
            # SHAPE 4, previously unimplemented and documented as such at the
            # top of this file: `<cmd>` then `$?` with NO assignment. `$?`
            # refers to whatever command preceded it, so requiring `VAR=$(` was
            # an arbitrary narrowing of the same defect. The canonical specimen
            # is scripts/lib/e2e_logger.sh, which runs `"$@"` BARE and then
            # `local rc=$?` -- measured 2026-10-18, that rc plumbing is dead
            # whenever -e is live through the call, which is what made four
            # handoff assertions unreachable (2552809ee).
            #
            # The rule is therefore "the command before a handler", and the
            # handler test below (next effective line, must CONSUME `$?`) is
            # what keeps that from matching everything.
            # A CONTINUATION LINE IS NOT A COMMAND. Shape 1 never hit this
            # because `VAR=$(` only matches a command's FIRST line, but shape 4
            # accepts bare commands, so every `\`-continued argument line
            # started its own candidate and they all joined forward to the same
            # handler. Measured: rch_verify.sh:4969-4984 produced sixteen
            # consecutive rows for ONE `run_process_with_timeout` invocation,
            # and the census jumped from 17 candidates to 316.
            if i > 0:
                prev = lines[i - 1]
                if shell[i - 1] is not None and prev.rstrip().endswith("\\"):
                    continue
            m = ASSIGN.match(line)
            stripped = _decomment(line).strip()
            if not stripped or stripped in {"fi", "done", "esac", "}", "{", "else", "do", "then"}:
                continue
            # SHAPE 4 IS DELIBERATELY NARROWER THAN SHAPE 1, because "any
            # command before a `$?`" over-matches in two measured ways:
            #   `set -euo pipefail` followed by `if ...; else rc=$?; fi` -- the
            #       `$?` belongs to the `if`'s own condition, not to `set`.
            #   `n=$((n + 1))` -- arithmetic, not a spawned command, and this
            #       file already carries an arm asserting it is not flagged.
            # So a shape-4 candidate must be a real command AND its handler
            # must be the pure `rc=$?` capture, not a test that merely mentions
            # `$?` somewhere in a compound line.
            if m is None:
                if ARITH.match(stripped) or stripped.startswith("set "):
                    continue
                # A FUNCTION DEFINITION IS NOT A COMMAND EXECUTION, and the
                # `$?` on the line after it is the TRAP-HANDLER idiom:
                #     cleanup() { local exit_code=$?; ...; exit "$exit_code"; }
                # There `$?` holds the status of whatever triggered the trap,
                # which is correct and deliberate. Flagging it inverts the
                # finding -- it would report the one place that handles exit
                # status properly as the place that does not.
                if FUNC_DEF.match(stripped):
                    continue
                require_pure_handler = True
            else:
                require_pure_handler = False
            # JOIN CONTINUATIONS BEFORE JUDGING THE GUARD. A command that wraps
            # carries its `|| RC=$?` on the NEXT physical line, so testing
            # `line` alone declared a guarded assignment unguarded -- the shape
            # produced by wrapping a long `ee` invocation, which is most of
            # them.
            logical, last = line, i
            while logical.rstrip().endswith("\\") and last + 1 < len(lines):
                last += 1
                logical = logical.rstrip()[:-1] + " " + lines[last]
            if IF_COND.match(logical) or GUARDED.search(logical):
                continue
            # AND LOOK FOR THE HANDLER IN CODE, NOT PROSE. This window read raw
            # lines, so a COMMENT mentioning `$?` -- including one explaining a
            # repair -- counted as the handler and manufactured a candidate out
            # of the assignment above it. Same substitution this repo keeps
            # finding elsewhere: a sentence about the mechanism read as the
            # mechanism.
            # `$?` HOLDS THE LAST COMMAND'S STATUS, so a read three lines down
            # with a command in between does NOT belong to this assignment. The
            # old 3-line any() window ignored that and attributed other
            # commands' handlers to whatever assignment preceded them --
            # e2e_output_budget.sh:126 was reported DEAD because
            # `env ... || exit_code=$?` two lines later reads `$?` for ITS OWN
            # command, and :127 for the same reason.
            #
            # So: the handler must be the NEXT effective line (blank and
            # comment lines skipped, heredoc bodies already excluded), AND it
            # must be a line that READS `$?` rather than one that runs
            # something and then reads its own status.
            nxt = None
            for j in range(last + 1, len(lines)):
                if shell[j] is None:
                    continue
                body = _decomment(lines[j]).strip()
                if not body:
                    continue
                nxt = body
                break
            if nxt is None:
                continue
            if require_pure_handler:
                if not PURE_HANDLER.match(nxt):
                    continue
            elif not HANDLER.match(nxt):
                continue
            # A shape-4 row has no assigned variable; name the shape instead so
            # the two are distinguishable in the output rather than silently
            # merged.
            rows.append({"file": os.path.relpath(path, root), "line": i + 1,
                         "var": m.group(1) if m else "<bare-cmd>",
                         "live": active[i], "_lines": lines,
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
        # NEGATIVE 7: the guard names an UPPERCASE variable. The old GUARDED
        # listed `rc=` and `status=` literally, so the e2e scripts' own
        # convention (`|| TAMPERED_RC=$?`) read as unguarded.
        # The trailing line MUST also read `$?`, or this arm proves nothing: with
        # the handler only on the assignment line, the window finds no `$?` and
        # the fixture passes whatever GUARDED says. Measured -- the first
        # version of this arm survived a full reversion of GUARDED to the old
        # vocabulary, i.e. it was decoration.
        open(os.path.join(d, "upper.sh"), "w").write(
            'set -e\nOUT="$(false)" || TAMPERED_RC=$?\necho "also reads $? later"\n')
        # NEGATIVE 8: the guard sits on a CONTINUATION line, which is what a
        # wrapped command produces. Judging the first physical line alone called
        # this unguarded.
        open(os.path.join(d, "cont.sh"), "w").write(
            'set -e\nOUT="$(some_cmd --flag \\\n    --other)" || RC=$?\necho "$RC"\n')
        # NEGATIVE 9: the only nearby `$?` is inside a COMMENT. Prose about a
        # handler is not a handler -- and a comment EXPLAINING this repair was
        # what manufactured a real false positive.
        open(os.path.join(d, "comment.sh"), "w").write(
            'set -e\nout="$(date)"\n# the rc=$? handler belongs here one day\necho done\n')
        # POSITIVE 2 -- THE TWIN HOLE of NEGATIVE 9. Stripping comments must not
        # eat a REAL handler that carries a trailing comment. Without this arm,
        # `_decomment` could be written to blank the whole line and every
        # negative above would still pass while the census went blind.
        open(os.path.join(d, "trailcomment.sh"), "w").write(
            'set -e\nout="$(false)"\nrc=$?  # keep the status\necho "$rc"\n')
        # NEGATIVE 10: the next line RUNS something and reads ITS OWN status.
        # `$?` holds only the last command's result, so that read has nothing to
        # do with the assignment above it. The old 3-line any() window counted
        # it and reported 6 correct sites as DEAD -- three quarters of the
        # census's headline findings.
        open(os.path.join(d, "intervening.sh"), "w").write(
            'set -e\nout="$(date)"\nrun_thing --flag || other=$?\necho done\n')
        # EXEMPT-BY-CALL-SITE: the dead-handler SHAPE is present, but the only
        # call is bracketed by `set +e`, so -e is off through the body and the
        # handler runs. This must be reported EXEMPT, not DEAD -- the two
        # rch_verify.sh rows it models were the census's last DEAD rows and
        # both were wrong. Asserted on the VERDICT, not on presence, because
        # the row legitimately still exists.
        open(os.path.join(d, "setpluse.sh"), "w").write(
            'set -e\nwrapped() {\n    out="$(false)"\n    rc=$?\n    echo "$rc"\n}\n'
            'set +e\nwrapped\nset -e\necho done\n')
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
        arm("`|| UPPERCASE_RC=$?` is recognised as a guard",
            "scripts/upper.sh" not in got)
        arm("a guard on a CONTINUATION line is recognised",
            "scripts/cont.sh" not in got)
        arm("a $? inside a COMMENT is not counted as a handler",
            "scripts/comment.sh" not in got)
        arm("a real handler with a TRAILING comment is still flagged",
            "scripts/trailcomment.sh" in got)
        arm("a $? belonging to an INTERVENING command is not attributed here",
            "scripts/intervening.sh" not in got)
        setpluse_rows = [r for r in rows if r["file"] == "scripts/setpluse.sh"]
        arm("a handler whose only call site is under `set +e` is EXEMPT, not DEAD",
            bool(setpluse_rows)
            and all("EXEMPT" in r["verdict"] for r in setpluse_rows))
        arm("exactly three candidates across the fixtures", len(rows) == 3)

        # --- GATE ARMS (bd-w6849) ------------------------------------------
        # The census had no exit statement at all: it fell off the end of
        # __main__ and ALWAYS exited 0. So wiring it to a root as "assert exit
        # 0" would have been unfalsifiable -- a check that cannot red is worse
        # than an unwired tool, because it reads as coverage. `--gate` is what
        # makes the wiring mean something, and these arms are what make the
        # gate mean something.
        #
        # The first is the positive control and it is the only one that could
        # have caught a gate wired to the wrong verdict set.
        arm(
            "the gate BLOCKS on a DEAD verdict",
            gate_blocking_rows(
                [{"live": True, "verdict": "DEAD: f() only ever called bare"}]
            )
            != [],
        )
        arm(
            "the gate BLOCKS on a TOP-LEVEL dead handler",
            gate_blocking_rows(
                [{"live": True, "verdict": "TOP-LEVEL: -e applies, handler DEAD"}]
            )
            != [],
        )
        arm(
            "the gate does NOT block on EXEMPT, the verdict the real tree has",
            gate_blocking_rows(
                [{"live": True, "verdict": "EXEMPT: f() is called in a tested context"}]
            )
            == [],
        )
        arm(
            "the gate does NOT block on UNKNOWN (no call site found is not a defect)",
            gate_blocking_rows(
                [{"live": True, "verdict": "UNKNOWN: no call site found for f()"}]
            )
            == [],
        )
        arm(
            "a DORMANT row is never blocking, whatever its verdict",
            gate_blocking_rows(
                [{"live": False, "verdict": "DEAD: f() only ever called bare"}]
            )
            == [],
        )
        arm(
            "the gate is clean on the live tree as measured today",
            gate_blocking_rows(census(".")) == [],
        )
    print(f"\n[set-e-census] self-test: {sum(arms)}/{len(arms)} arms passed")
    return 0 if all(arms) else 1


#: Verdict prefixes that mean "this handler cannot run and that is a defect".
#:
#: UNKNOWN is DELIBERATELY not here. It means no call site was found for the
#: enclosing function, which is as likely to be a limitation of the call-site
#: search as a real dead handler -- and this census has already shipped four
#: false-positive classes (b9727a932, 8c24ce49c), three quarters of its
#: headline DEAD rows at one point. A gate that reds on UNKNOWN would red on
#: the instrument's own uncertainty. They are still PRINTED by the census.
GATE_BLOCKING_VERDICTS = ("DEAD", "TOP-LEVEL")


def gate_blocking_rows(rows):
    """Rows that must fail `--gate`: `set -e` live at the line AND a verdict
    saying the handler cannot run.

    Pure, so the self-test can assert both polarities without a repository.
    """
    return [
        r
        for r in rows
        if r.get("live") and r["verdict"].startswith(GATE_BLOCKING_VERDICTS)
    ]


if __name__ == "__main__":
    if "--self-test" in sys.argv:
        sys.exit(self_test())
    gate = "--gate" in sys.argv
    # `--root DIR` exists so the gate's RED direction can be demonstrated
    # end-to-end against a fixture tree. Without it the only proof that
    # `--gate` fails is the self-test's synthetic verdict dicts, which cannot
    # catch a gate wired correctly to the wrong census.
    root = "."
    if "--root" in sys.argv:
        root = sys.argv[sys.argv.index("--root") + 1]
    rows = census(root)
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

    # Default stays a CENSUS and still exits 0 whatever it found: a red census
    # is indistinguishable from a broken one. `--gate` is the opt-in verdict.
    if not gate:
        sys.exit(0)
    blocking = gate_blocking_rows(rows)
    if not blocking:
        print(
            f"\n[set-e-census] GATE PASS -- 0 dead handlers among "
            f"{len(live)} site(s) where `set -e` is live, of {len(rows)} "
            f"candidate(s). Verdicts that block: {'/'.join(GATE_BLOCKING_VERDICTS)}; "
            f"UNKNOWN is reported but does not block."
        )
        sys.exit(0)
    print(
        f"\n[set-e-census] GATE FAIL -- {len(blocking)} handler(s) cannot run "
        f"because `set -e` aborts first:",
        file=sys.stderr,
    )
    for r in sorted(blocking, key=lambda r: (r["file"], r["line"])):
        print(f"    {r['file']}:{r['line']}  {r['var']}  {r['verdict']}", file=sys.stderr)
    print(
        "  Put the assignment to the LEFT of `||` so -e is suspended through "
        "the call, e.g. `out=$(cmd) || rc=$?`.",
        file=sys.stderr,
    )
    sys.exit(1)
