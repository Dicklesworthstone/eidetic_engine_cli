#!/usr/bin/env python3
"""Fail when a degraded code the PRODUCT declares is absent from the taxonomy
(bd-nnheo).

WHAT THE DEFECT IS. docs/degraded_code_taxonomy.md assigns every degraded code
a severity and an owner. Several gates derive their REQUIRED code set from that
document -- tests/auto_enroll_documentation_consistency.rs calls
`taxonomy_codes_with_prefixes(&["tailscale_", "hello_responder_",
"discovery_policy_", "mesh_peer_"])` and demands each result be documented. So a
code missing from the taxonomy is invisible to every gate derived downstream of
it: the hole is in the registry, the derived gate reads a shorter list, and it
PASSES.

That is not hypothetical. `mesh_peer_randomness_unavailable`
(src/mesh/peer.rs, emitted when OS randomness is unavailable during peer
enrollment) shipped absent from the taxonomy, and the onboarding-doc gate
consequently never demanded it be documented. A code can ship, be emitted to
users, and never acquire a severity or an owner.

tests/degraded_code_taxonomy_consistency_test.rs checks the FIXTURE axis in
both directions -- `every_j6_fixture_code_is_classified_in_taxonomy` and
`every_taxonomy_code_has_a_j6_fixture_or_pending_marker`. Neither asks whether
a code the product can actually emit is classified at all. Enumerated across
all ten files that read the taxonomy document on 2026-10-18: nothing checks
this direction.

PREDICATE. A `pub const <NAME>CODE<NAME>: &str = "<value>"` in src/ whose
VALUE does not appear as a backtick-delimited token in the taxonomy.

THIS IS A LOWER BOUND, deliberately and unavoidably. Codes built by an enum's
`as_str()` or written as inline literals are not declared this way and are not
counted. A clean run here means "no NEWLY DECLARED constant is unclassified",
never "every emittable code is classified".

A SHRINK-ONLY BASELINE, not an ignore-list. 32 codes are unclassified today;
reding on all of them would make this gate unlandable and it would be disabled
within a day. They are baselined so the gate is GREEN at HEAD and fails only
on a NEW unclassified constant -- or on a baselined entry that has since been
classified, which must also fail, or the baseline silently becomes a list of
things nobody will ever revisit.

USAGE
    scripts/check-degraded-code-classified.py              gate; exit 1 on new
    scripts/check-degraded-code-classified.py --json       machine-readable
    scripts/check-degraded-code-classified.py --self-test  11 arms
"""
import argparse
import glob
import json
import os
import re
import sys
import tempfile

DECL = re.compile(r'^pub const ([A-Z0-9_]*CODE[A-Z0-9_]*): &str = "([^"]+)"', re.M)
TAXONOMY = "docs/degraded_code_taxonomy.md"
BASELINE = "scripts/degraded-code-classification-baseline.txt"

#: Constants whose name contains CODE but which are not degraded codes.
#:
#: `TEAM_INVITE_CODE_PREFIX` is an invite-code PREFIX (`eeteam1-`), not a
#: degraded code, and it is the reason this list exists rather than being
#: notional: the predicate keys on the NAME containing "CODE", which an invite
#: code satisfies. Without the exclusion the gate demands a taxonomy entry for
#: a string that could never be emitted as a degradation.
NOT_DEGRADED_CODES = ("TEAM_INVITE_CODE_PREFIX",)

#: A code written INLINE at the push site rather than declared as a constant.
#:
#: This closes half of the blind spot this gate's own SCOPE line admits to:
#: "codes built by an enum as_str() or written inline are not declared this way
#: and are NOT counted". Measured 2026-10-27, that admission was hiding ten
#: codes in two coherent families -- four `regression_evidence_*` from
#: src/models/regression_causality.rs and six `swarm_replay_*` from
#: src/core/lab.rs -- none of them in the taxonomy, all of them reaching a
#: user-visible `degradedCodes` channel (the families appear in src/cli/mod.rs,
#: src/output/mod.rs and src/obs/volatile_fields.rs, the last of which is the
#: registry governing what gets scrubbed from goldens). The gate reported OK
#: over all ten because none is a `pub const`.
#:
#: The pattern is deliberately TIGHT: the literal must be pushed into a
#: receiver named `degraded` or `degraded_codes`. A looser rule that took any
#: code-shaped literal near the word "degraded" was tried first and returned
#: 135 "absent" codes, most of them field names -- `command`, `details`,
#: `degraded_codes` itself. That is an instrument failure, not a finding, so
#: the narrow form is the one that ships.
#:
#: The enum `as_str()` half of the admission is still NOT covered, and the
#: SCOPE line below still says so.
INLINE_PUSH = re.compile(
    r"\bdegraded(?:_codes)?\s*\.push\(\s*"
    r'"([a-z][a-z0-9_]*)"'
    r"(?:\s*\.to_(?:string|owned)\(\))?\s*\)"
)


def read(path):
    with open(path, encoding="utf-8", errors="replace") as handle:
        return handle.read()


def declared_codes(root):
    """value -> sorted [(relative path, const name)] for every declaration."""
    out = {}
    for path in sorted(glob.glob(os.path.join(root, "src", "**", "*.rs"), recursive=True)):
        for name, value in DECL.findall(read(path)):
            if name in NOT_DEGRADED_CODES:
                continue
            rel = os.path.relpath(path, root).replace("\\", "/")
            out.setdefault(value, []).append((rel, name))
    return {k: sorted(v) for k, v in out.items()}


def inline_pushed_codes(root):
    """value -> sorted [(relative path, "inline push")] for every inline literal
    pushed into a degraded channel."""
    out = {}
    for path in sorted(glob.glob(os.path.join(root, "src", "**", "*.rs"), recursive=True)):
        for value in INLINE_PUSH.findall(read(path)):
            rel = os.path.relpath(path, root).replace("\\", "/")
            entry = (rel, "inline push")
            if entry not in out.setdefault(value, []):
                out[value].append(entry)
    return {k: sorted(v) for k, v in out.items()}


def taxonomy_codes(text):
    """Codes classified in the taxonomy: the first backtick-delimited span per
    line, when it looks like a code.

    Same extraction the auto_enroll consistency test's helper performs, so this
    gate and the gates downstream of the taxonomy agree on what "classified"
    means.
    """
    codes = set()
    for line in text.split("\n"):
        match = re.search(r"`([^`]+)`", line)
        if match and re.fullmatch(r"[a-z0-9_.]+", match.group(1)):
            codes.add(match.group(1))
    return codes


def unclassified(declared, classified):
    """Declared values absent from the taxonomy.

    EMPTY-WORLD GUARD, in both directions. An empty `declared` means the source
    scan failed; an empty `classified` means the taxonomy extraction failed and
    would report EVERY code unclassified. Either is an instrument failure, and
    the second fails in the direction that manufactures a catastrophe.
    """
    if not declared:
        raise ValueError(
            "no degraded-code constants found under src/: the source scan "
            "produced nothing, so no classification gap can be inferred "
            "(instrument failure, not a clean tree)"
        )
    if not classified:
        raise ValueError(
            "taxonomy extraction produced no codes, so every declared code "
            "would read as unclassified (instrument failure, not total "
            "classification loss)"
        )
    return sorted(value for value in declared if value not in classified)


def load_baseline(root):
    path = os.path.join(root, BASELINE)
    if not os.path.isfile(path):
        return set()
    out = set()
    for line in read(path).split("\n"):
        line = line.strip()
        if line and not line.startswith("#"):
            out.add(line)
    return out


def audit(root):
    declared = declared_codes(root)
    inline = inline_pushed_codes(root)
    classified = taxonomy_codes(read(os.path.join(root, TAXONOMY)))
    missing = unclassified(declared, classified)
    # The two populations are kept SEPARATE in the report and merged only for
    # the verdict. Folding inline codes into `declaredDistinctValues` would
    # silently move every number this gate has ever printed, so a reader
    # comparing against an older run could not tell a new code from a widened
    # predicate.
    inline_missing = sorted(value for value in inline if value not in classified)
    sites = {value: declared[value] for value in missing}
    for value in inline_missing:
        sites.setdefault(value, []).extend(inline[value])
    return {
        "schema": "ee.degraded_code_classification.v1",
        "declaredConstants": sum(len(v) for v in declared.values()),
        "declaredDistinctValues": len(declared),
        "taxonomyCodes": len(classified),
        # The denominator that makes the number above readable, and the control
        # for the extraction: if this were 0 the predicate would be inverted and
        # every code would look unclassified.
        "classifiedDeclaredValues": len(declared) - len(missing),
        "inlinePushedValues": len(inline),
        "inlineUnclassified": inline_missing,
        "unclassified": sorted(set(missing) | set(inline_missing)),
        "sites": sites,
    }


def self_test():
    arms = []

    def arm(label, ok):
        arms.append(ok)
        print(f"  [{'ok  ' if ok else 'FAIL'}] {label}")

    # 1-2: extraction basics
    decl = DECL.findall('pub const FOO_CODE: &str = "foo_bar";\n')
    arm("a declaration is extracted with its name and value",
        decl == [("FOO_CODE", "foo_bar")])
    arm("an indented (non-top-level) declaration is NOT extracted",
        DECL.findall('    pub const INNER_CODE: &str = "x";\n') == [])

    # 3-4: taxonomy extraction
    tax = taxonomy_codes("| `alpha_beta` | low | owner |\n| `Gamma Delta` | low |\n")
    arm("a backtick code is classified", "alpha_beta" in tax)
    arm("a backtick span that is not code-shaped is ignored",
        "Gamma Delta" not in tax)

    # 5-6: the predicate, both polarities
    arm("a declared code absent from the taxonomy is reported",
        unclassified({"a": [], "b": []}, {"a"}) == ["b"])
    arm("a declared code present in the taxonomy is NOT reported",
        unclassified({"a": []}, {"a", "b"}) == [])

    # 7-8: EMPTY-WORLD GUARDS. The second is the one that matters: an empty
    # taxonomy would otherwise report every code in the product unclassified.
    try:
        unclassified({}, {"a"})
        empty_src = False
    except ValueError:
        empty_src = True
    arm("an empty source scan raises instead of reporting a clean tree", empty_src)
    try:
        unclassified({"a": []}, set())
        empty_tax = False
    except ValueError:
        empty_tax = True
    arm("an empty taxonomy raises instead of reporting total loss", empty_tax)

    # 9: the non-degraded exclusion must actually exclude.
    with tempfile.TemporaryDirectory(prefix="degraded-selftest-") as tmp:
        os.makedirs(os.path.join(tmp, "src"))
        with open(os.path.join(tmp, "src", "x.rs"), "w", encoding="utf-8") as handle:
            handle.write(
                'pub const TEAM_INVITE_CODE_PREFIX: &str = "eeteam1-";\n'
                'pub const REAL_THING_CODE: &str = "real_thing";\n'
            )
        found = declared_codes(tmp)
        arm("an invite-code prefix is excluded, a real code is kept",
            "eeteam1-" not in found and "real_thing" in found)

    # INLINE PUSHES, both polarities plus the receiver negative. The receiver
    # arm is the one that matters: a predicate keyed on "a code-shaped literal
    # near the word degraded" returned 135 hits, nearly all field names, so the
    # tightness is load-bearing rather than incidental.
    with tempfile.TemporaryDirectory(prefix="degraded-inline-") as tmp:
        os.makedirs(os.path.join(tmp, "src"))
        with open(os.path.join(tmp, "src", "y.rs"), "w", encoding="utf-8") as handle:
            handle.write(
                'degraded.push("plain_form");\n'
                'degraded_codes.push("owned_form".to_owned());\n'
                'degraded_codes.push("string_form".to_string());\n'
                'other_vec.push("not_a_degraded_channel");\n'
                'degraded.push(SOME_CONST);\n'
            )
        inline = inline_pushed_codes(tmp)
        arm("a bare inline push is caught", "plain_form" in inline)
        arm("a .to_owned() inline push is caught", "owned_form" in inline)
        arm("a .to_string() inline push is caught", "string_form" in inline)
        arm("a push into another receiver is NOT caught",
            "not_a_degraded_channel" not in inline)
        arm("a non-literal push contributes nothing", len(inline) == 3)

    # 10-11: END-TO-END on the live tree. The gate must be green at HEAD and
    # its classified count must be non-zero, or "unclassified" means nothing.
    report = audit(".")
    # The inline population must be NON-EMPTY on the live tree, or the new
    # predicate could have stopped matching and the gate would report a clean
    # inline axis having scanned nothing -- the same empty-world failure the
    # arms above guard for the declared axis.
    arm("the live tree has a non-zero inline-pushed count",
        report["inlinePushedValues"] > 0)
    arm("the live tree has a non-zero classified count (predicate not inverted)",
        report["classifiedDeclaredValues"] > 0)
    baseline = load_baseline(".")
    arm("every live unclassified code is in the baseline, so the gate is green",
        set(report["unclassified"]) <= baseline)

    print(f"\n[degraded-classified] self-test: {sum(arms)}/{len(arms)} arms passed")
    return 0 if all(arms) else 1


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", default=".")
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()

    if args.self_test:
        return self_test()

    try:
        report = audit(args.root)
    except ValueError as error:
        print(f"[degraded-classified] BLOCKED: {error}", file=sys.stderr)
        return 2

    baseline = load_baseline(args.root)
    found = set(report["unclassified"])
    new = sorted(found - baseline)
    fixed = sorted(baseline - found)
    report["baselineSize"] = len(baseline)
    report["newUnclassified"] = new
    report["baselinedButNowClassified"] = fixed

    if args.json:
        print(json.dumps(report, indent=2, sort_keys=True))
    else:
        print(
            "[degraded-classified] predicate: a `pub const *CODE*: &str` in src/, "
            "OR a literal pushed into a `degraded`/`degraded_codes` channel, "
            "whose value is absent from the taxonomy"
        )
        print(
            f"[degraded-classified] {report['declaredConstants']} constant(s), "
            f"{report['declaredDistinctValues']} distinct value(s), "
            f"{report['classifiedDeclaredValues']} classified, "
            f"{len(report['unclassified']) - len(report['inlineUnclassified'])} not; "
            f"taxonomy carries {report['taxonomyCodes']} code(s); "
            f"baseline {len(baseline)}"
        )
        print(
            f"[degraded-classified] inline-pushed: "
            f"{report['inlinePushedValues']} distinct value(s), "
            f"{report['inlinePushedValues'] - len(report['inlineUnclassified'])} "
            f"classified, {len(report['inlineUnclassified'])} not"
        )
        print(
            "[degraded-classified] SCOPE: an inline literal counts only when "
            "pushed into a receiver named `degraded`/`degraded_codes`. Codes "
            "built by an enum as_str(), or pushed through any other receiver, "
            "are still NOT counted -- so a pass means no newly declared constant "
            "and no newly inlined push is unclassified, never that every "
            "emittable code is classified."
        )

    status = 0
    if new:
        print(
            f"\n[degraded-classified] FAIL: {len(new)} degraded code(s) emitted "
            f"from src/ but absent from {TAXONOMY}:",
            file=sys.stderr,
        )
        for value in new:
            for rel, name in report["sites"].get(value, []):
                print(f"    {value}  ({rel}  {name})", file=sys.stderr)
        print(
            "  Give each a severity and an owner in the taxonomy. A code absent "
            "from it is also invisible to every gate that derives its required "
            "set from it, so this is not only a documentation gap.",
            file=sys.stderr,
        )
        status = 1
    if fixed:
        print(
            f"\n[degraded-classified] FAIL: {len(fixed)} baselined code(s) are now "
            f"classified; delete their line(s) from {BASELINE}:",
            file=sys.stderr,
        )
        for value in fixed:
            print(f"    {value}", file=sys.stderr)
        print(
            "  A baseline that keeps classified entries is an ignore-list.",
            file=sys.stderr,
        )
        status = 1
    if status == 0:
        print(
            f"[degraded-classified] OK -- no unclassified degraded code "
            f"beyond the {len(baseline)} baselined"
        )
    return status


if __name__ == "__main__":
    sys.exit(main())
