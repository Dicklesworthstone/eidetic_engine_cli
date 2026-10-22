#!/usr/bin/env python3
"""Census: which absence assertions in e2e scripts are GUARDED? (bd-tnfge)

An absence assertion is satisfied by a world in which the mechanism it tests
never ran. `contains($SECRET) | not` passes on a response that never carried
diff evidence at all. bd-tue5y and bd-5tluk were each one instance, both found
by hand. This answers the population question those two raised.

A site is GUARDED when another assertion on the SAME payload variable, in the
same enclosing scope, establishes positively that the population is non-empty.
The predicate is keyed on CALLER DATA (the payload variable), never on prose.

THIS IS NOT A GATE AND MUST NOT BE WIRED AS ONE. Read the limits first.

VALIDATED AGAINST BOTH POLARITIES OF TWO CONTROLS, and it passes only one:

  bd-tue5y, scripts/e2e_reach.sh at 234c97ecc -- PASSES both ways.
      before: $pack_no_global :309 UNGUARDED   (the real defect)
      after : 0 unguarded                      (the real fix)
    The flip is on exactly the site that commit repaired.

  bd-5tluk, scripts/e2e_capture.sh at 58a8bd22f -- CANNOT BE MADE TO FLIP.
    Its guard asserts CONTENT PRESENCE, not length: "the capture rendered a
    diff excerpt AND the redactor fired naming openai_api_key". With only
    length-based markers the guard is invisible (under-credit). Admit
    contains()/test() and the PRE-FIX vacuous assertion reads as guarded too
    (over-credit), because whether a content-presence peer is the RIGHT
    precondition is semantic and no regex decides it.

So run it BOTH ways and read the band, never a single number:

    --strict     length-based guards only -> UNGUARDED is an UPPER bound
    (default)    contains()/test() too    -> UNGUARDED is a LOWER bound

OTHER LIMITS, measured 2026-10-22 and stated so the output is not over-read:

  - COVERAGE. It parses `assert_jq "$VAR"` only: 692 of 1139 assert_jq calls
    in scripts/e2e_*.sh, i.e. 61%. Inline `$(...)` payloads are invisible, as
    are absence idioms in other assertion styles (about 90 lines) and
    everything in scripts/e2e_overhaul/, scripts/lib/ and tests/*.rs.
  - It reports CANDIDATES, not defects. Confirming one needs execution on a
    planted empty world, not reading.
  - TWO SUB-CLASSES are conflated in the output and want different guards:
    collection-absence (`length == 0`, `index(..) == null`, `all(..| not)`)
    is vacuous when the collection is missing or empty; scalar-null
    (`.data.someField == null`) is vacuous when the enclosing object is
    absent. `.success == true` guards NEITHER, which is why it is excluded
    from the positive markers -- a command can succeed and return nothing,
    and that is precisely bd-tue5y's defect.
"""
import re
import sys

# assert_jq "$payload" '<predicate>' "label"   (predicate may span lines)
CALL = re.compile(
    r"""assert_jq\s+"(?P<payload>\$\{?[A-Za-z_][A-Za-z0-9_]*\}?)"\s+'(?P<pred>[^']*)'""",
    re.DOTALL,
)
# Any assert_jq whose payload is not a simple variable (inline $(...), etc.)
CALL_ANY = re.compile(r"assert_jq\s+(?P<arg>\S)", re.DOTALL)

ABSENCE = [
    re.compile(r"\|\s*not\b"),
    re.compile(r"length\s*\)?\s*==\s*0\b"),
    re.compile(r"==\s*\[\]"),
    re.compile(r"==\s*null\b"),
    re.compile(r"length\s*\)?\s*<\s*1\b"),
]
# Establishes the POPULATION IS NON-EMPTY. Deliberately NOT `success == true`:
# a command can succeed and return an empty collection, which is precisely the
# vacuous world an absence assertion must be guarded against (bd-tue5y). Keeping
# it here made the classifier report its own negative control as GUARDED.
# `contains(` / `test(` are here because bd-5tluk's real guard asserts CONTENT
# PRESENCE, not length ("the capture rendered a diff excerpt AND the redactor
# fired"). Without them the classifier could not see that fix at all. They are
# only ever consulted on a peer that is NOT itself an absence assertion, since
# `contains($SECRET) | not` is the negative side, not a guard for it.
POSITIVE = [
    re.compile(r"length\s*\)?\s*>=\s*[1-9]"),
    re.compile(r"length\s*\)?\s*>\s*0\b"),
    re.compile(r"length\s*\)?\s*==\s*[1-9]"),
    re.compile(r"\|\s*any\b"),
    re.compile(r"contains\s*\("),
    re.compile(r"\btest\s*\("),
]
FUNC = re.compile(r"^\s*(?:function\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*\(\)\s*\{")


def function_spans(lines):
    """[(name, start_idx, end_idx)] by brace matching, `${` stripped first.

    Backward-scanning for the nearest preceding `name() {` is WRONG in shell:
    functions are defined at top level, so a call site in the main body finds
    the LAST-DEFINED function and every unrelated site collapses into one
    scope. That merged peers across the whole file and manufactured guards.
    """
    spans = []
    i = 0
    while i < len(lines):
        m = FUNC.match(lines[i])
        if not m:
            i += 1
            continue
        name = m.group(1)
        depth = 0
        j = i
        while j < len(lines):
            bare = re.sub(r"\$\{", "", lines[j].split("#")[0])
            depth += bare.count("{") - bare.count("}")
            if depth <= 0 and j > i:
                break
            if depth <= 0 and j == i and bare.count("}") >= bare.count("{"):
                break
            j += 1
        spans.append((name, i, j))
        i = j + 1
    return spans


def enclosing(lines, idx, spans=None):
    if spans is None:
        spans = function_spans(lines)
    best = None
    for name, s, e in spans:
        if s <= idx <= e and (best is None or (e - s) < (best[2] - best[1])):
            best = (name, s, e)
    return best[0] if best else "<toplevel>"


def classify(text):
    lines = text.splitlines()
    spans = function_spans(lines)
    calls = []
    for m in CALL.finditer(text):
        ln = text.count("\n", 0, m.start())
        calls.append(
            {
                "line": ln + 1,
                "payload": m.group("payload").strip("${}"),
                "pred": m.group("pred"),
                "fn": enclosing(lines, ln, spans),
            }
        )
    for c in calls:
        c["absence"] = any(p.search(c["pred"]) for p in ABSENCE)
        c["positive"] = any(p.search(c["pred"]) for p in POSITIVE)
    rows = []
    for c in calls:
        if not c["absence"]:
            continue
        peers = [
            o
            for o in calls
            if o is not c and o["payload"] == c["payload"] and o["fn"] == c["fn"]
        ]
        # A guard must be POSITIVE and must not itself be an absence assertion:
        # two negatives on one payload guard nothing.
        guards = [o for o in peers if o["positive"] and not o["absence"]]
        if guards:
            rows.append(
                (
                    c,
                    "GUARDED",
                    f"peer at line {guards[0]['line']} on ${c['payload']} in {c['fn']}()",
                )
            )
        elif peers:
            rows.append((c, "UNGUARDED", f"{len(peers)} peer(s) on ${c['payload']}, none positive"))
        else:
            rows.append((c, "UNGUARDED", f"no other assertion on ${c['payload']} in {c['fn']}()"))
    return calls, rows


def main(paths, verbose=False):
    tot_calls = tot_abs = g = u = 0
    per_file = []
    unguarded = []
    for p in paths:
        try:
            text = open(p, encoding="utf-8", errors="replace").read()
        except Exception:
            continue
        calls, rows = classify(text)
        tot_calls += len(calls)
        tot_abs += len(rows)
        fg = sum(1 for _, v, _ in rows if v == "GUARDED")
        fu = len(rows) - fg
        g += fg
        u += fu
        if rows:
            per_file.append((p, len(rows), fg, fu))
        for c, v, why in rows:
            if v == "UNGUARDED":
                unguarded.append((p, c["line"], c["fn"], c["payload"], why, c["pred"][:70]))
    print(f"assert_jq calls parsed (simple-var payload): {tot_calls}")
    print(f"ABSENCE assertions found: {tot_abs}   GUARDED {g}   UNGUARDED {u}")
    if tot_abs:
        print(f"guarded fraction: {g}/{tot_abs} = {100.0*g/tot_abs:.1f}%")
    print()
    print("per-file (file, absence, guarded, unguarded):")
    for p, n, fg, fu in sorted(per_file, key=lambda r: -r[3]):
        print(f"  {fu:3d} unguarded / {n:3d} absence  {p}")
    if verbose:
        print()
        print("UNGUARDED sites:")
        for p, ln, fn, pay, why, pred in unguarded:
            print(f"  {p}:{ln}  fn={fn}  ${pay}")
            print(f"      why: {why}")
            print(f"      pred: {pred}")
    return g, u, tot_abs


if __name__ == "__main__":
    # --strict drops the content-presence markers, keeping only length-based
    # non-emptiness. Strict UNDER-credits guards (it cannot see bd-5tluk's
    # content-presence precondition) so its UNGUARDED count is an UPPER bound.
    # Permissive OVER-credits (any contains()/test() peer counts) so its
    # UNGUARDED count is a LOWER bound. Neither is the answer; the band is.
    if "--strict" in sys.argv:
        POSITIVE[:] = [
            p for p in POSITIVE if "contains" not in p.pattern and "test" not in p.pattern
        ]
    args = [a for a in sys.argv[1:] if not a.startswith("-")]
    main(args, verbose="-v" in sys.argv)
