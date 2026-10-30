#!/usr/bin/env python3
"""A commit deleted a `fn NAME` and left NAME called somewhere else. (bd-1ks7e)

WHY THIS EXISTS. On 2026-10-29 18:57 `f9ecec213` removed
`fn incident_card_source_ids` from `src/core/context.rs` and -- correctly --
deleted one of its two call sites, in that same file. The second caller lives in
`src/core/context_why_not.rs`, which reaches the crate through

    src/core/context.rs:130   #[path = "context_why_not.rs"]

and whose line 7 is `use super::*`, so it names nothing a reader would recognise
as an import. `git grep 'mod context_why_not'` returns NOTHING. The deleting
diff showed one function removed and one call site removed and read as complete.

Main did not compile for the next 24 hours. Nothing noticed, because on this
host `verify.sh` exits before stage one, hosted Actions are off repo-wide, and
`cargo check` is a tripwired subcommand -- so the only instrument that can see
`error[E0425]: cannot find function` costs a ~30-minute remote dispatch that
nobody runs per commit.

THIS GATE IS THE CHEAP SUBSET OF THAT BUILD. It cannot find a type error, a
borrow error, or a trait mismatch. It finds exactly one class: a function
definition the commit removed whose name is still referenced in code and is
defined nowhere. That class is worth its own gate because it is the class
`#[path]` + `use super::*` makes invisible to review.

MEASURED BLAST RADIUS, 120 commits of history touching `src/` (2026-10-30):

    fires                                                    1
    f9ecec213  incident_card_source_ids  -> context_why_not.rs   TRUE (the P0)

A SECOND COMMIT FIRED BEFORE PROSE WAS EXCLUDED, and it is the reason this
script strips comments and string literals rather than running `git grep`:

    b5ecbfa7e  situation_compare_without_dry_run_is_policy_error
               src/cli/mod.rs:83317
               /// I restored `situation_compare_without_dry_run_is_policy_error` on

That is a doc comment deliberately naming a test that commit deleted. A `git
grep` for the symbol cannot tell it from a call, so the naive predicate reports
a healthy commit as broken -- the same mention-read-as-invocation defect that
put `scripts/e2e_invocation_audit.sh` into a stale-baseline red earlier the same
week. One true fire in 120 commits is a usable signal; two with one of them
prose is not.

WHAT THE STRIPPER'S FAILURE DIRECTIONS ARE, because they are not symmetric.
Under-stripping (leaving a comment in) produces a false FIRE: noise, loud, and
self-correcting when a human reads the quoted line. Over-stripping (swallowing
real code) produces a false PASS: silent, and indistinguishable from a healthy
commit. So every ambiguous case here resolves toward under-stripping:

  - an unterminated string or block comment emits the REMAINDER VERBATIM rather
    than discarding it, and is reported as a parse anomaly;
  - `'` is only consumed when it completes a char literal. Rust lifetimes
    (`'a`) are left alone, because treating `'a` as an opening quote swallows
    everything to the next apostrophe. Arm `char_literal_quote` is the control:
    `if c == '"' {` contains a double quote INSIDE a char literal, and a
    stripper that enters string mode there eats the following code.

WHAT IT DELIBERATELY DOES NOT DO.

It does not require that a removed function have had no callers -- it requires
that no call SURVIVE. A rename, a move between files, and a signature change
all leave a definition somewhere and pass.

It does not look at the module graph. The module graph is what hid the break:
a `#[path]` sibling is not reachable from any `mod` line you can grep. This gate
reads the SYMBOL across every tracked `.rs` file and asks only whether some
definition of it still exists.

It is not a substitute for a build and must not be cited as one. bd-g3obb tracks
the missing TEST-target build gate; this closes one named class inside it and
leaves the rest open.

WHY IT STAYS `fn`-ONLY -- MEASURED, AND DECLINED. Removing a `struct`, `enum`,
`trait`, `type`, `const` or `static` whose users survive is the same
name-resolution class (E0412/E0422), so widening looks free. It is not. Measured
over 60 commits with every kind enabled:

    fn 1 fire   struct 1 fire   enum/trait/type/const/static/macro 0

and the struct fire is a FALSE POSITIVE on the same commit. `f9ecec213` deleted
a function-LOCAL `struct Sources` declared inside the removed function body, and
`Sources` survives in `src/cli/mod.rs:1560` as

    Sources(AgentSourcesArgs),

an enum VARIANT of `AgentCommand`, unrelated in every way. Rust keeps types,
variants, fields, modules and methods in separate namespaces; a word-level grep
collides them, and no amount of comment stripping separates them because both
occurrences are real code. Distinguishing them needs actual name resolution,
which is a compiler. Function-local declarations make it worse: their scope is
one body, so "defined somewhere in the tree" is simply the wrong question for
them, and their names are generic (`Sources`, `Row`, `Args`) and collide often.

Snake_case free functions are the one population where the flat predicate is
accurate enough to be worth running. Do not widen this gate; dispatch a build.

THE KNOWN FAIL-OPEN, stated so it is not mistaken for coverage. Resolution here
is tree-wide, so a top-level `fn foo` removed from one file while an unrelated
function-LOCAL `fn foo` exists in another is reported clean. That is a false
PASS. It is accepted because the alternative is scope analysis, and because the
shape is rare: a nested helper and its caller live in the same body, so a commit
removing one normally removes both in the same diff.

EXIT CODES
    0  no orphan
    1  an orphan: a removed definition is still called
    2  usage error
    3  CANNOT ANSWER -- git did not answer, or the range has no parent commit.
       Fail-closed, and a distinct code so it is not read as a red. The model is
       `scripts/verification-drift-guard.sh:205`: check the subprocess's exit
       status BEFORE reading its output, so "the tool failed" never arrives
       looking like "the predicate found nothing".
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys

SCAN_DIRS = ("src", "tests", "benches")

# A function definition at any indentation, with any ordering of the qualifiers
# Rust permits before `fn`. Visibility may carry a restriction: `pub(crate)`,
# `pub(super)`, `pub(in crate::core)`.
DEF_RE = re.compile(
    r"^\s*(?:#\[[^\]]*\]\s*)*"
    r"(?:pub\s*(?:\(\s*[^)]*\))?\s+)?"
    r"(?:default\s+)?(?:const\s+)?(?:async\s+)?(?:unsafe\s+)?"
    r"(?:extern\s+(?:\"[^\"]*\"\s+)?)?"
    r"fn\s+([A-Za-z_][A-Za-z0-9_]*)"
)

RAW_RE = re.compile(r"(?:b)?r(?P<h>#*)\"")
CHAR_RE = re.compile(
    r"'(?:\\(?:[nrt0\\'\"]|x[0-9A-Fa-f]{2}|u\{[0-9A-Fa-f]{1,6}\})|[^\\'\n])'"
)
IDENT_CHAR = re.compile(r"[A-Za-z0-9_]")


def strip_rust_noise(text: str) -> tuple[str, list[str]]:
    """Blank out comments and string literals, preserving line structure.

    Returns the stripped text and a list of parse anomalies. Newlines inside a
    removed region are kept so a reported line number still matches the file.
    """
    out: list[str] = []
    anomalies: list[str] = []
    i, n = 0, len(text)
    while i < n:
        c = text[i]
        prev = text[i - 1] if i else ""
        if c == "/" and i + 1 < n and text[i + 1] == "/":
            end = text.find("\n", i)
            if end < 0:
                break
            out.append("\n")
            i = end + 1
            continue
        if c == "/" and i + 1 < n and text[i + 1] == "*":
            depth, start, i = 1, i, i + 2
            while i < n and depth:
                if text.startswith("/*", i):
                    depth += 1
                    i += 2
                elif text.startswith("*/", i):
                    depth -= 1
                    i += 2
                else:
                    if text[i] == "\n":
                        out.append("\n")
                    i += 1
            if depth:
                # Unterminated: emit verbatim rather than discard the tail.
                anomalies.append("unterminated block comment")
                out.append(text[start:])
                break
            continue
        if not IDENT_CHAR.match(prev):
            raw = RAW_RE.match(text, i)
            if raw:
                close = '"' + raw.group("h")
                end = text.find(close, raw.end())
                if end < 0:
                    anomalies.append("unterminated raw string")
                    out.append(text[i:])
                    break
                out.append("\n" * text.count("\n", i, end))
                i = end + len(close)
                continue
        if c == '"' or (c == "b" and text.startswith('b"', i) and not IDENT_CHAR.match(prev)):
            start = i
            i += 2 if c == "b" else 1
            closed = False
            while i < n:
                if text[i] == "\\":
                    i += 2
                    continue
                if text[i] == '"':
                    i += 1
                    closed = True
                    break
                if text[i] == "\n":
                    out.append("\n")
                i += 1
            if not closed:
                anomalies.append("unterminated string literal")
                out.append(text[start:])
                break
            continue
        if c == "'":
            # Only a COMPLETE char literal is consumed. `'a` is a lifetime, and
            # treating it as a quote swallows code to the next apostrophe.
            ch = CHAR_RE.match(text, i)
            if ch:
                i = ch.end()
                continue
        out.append(c)
        i += 1
    return "".join(out), anomalies


def defined_names(text: str) -> set[str]:
    return {
        m.group(1) for m in (DEF_RE.match(line) for line in text.splitlines()) if m
    }


def removed_definitions(diff: str) -> set[str]:
    """Names whose definition line the diff deletes."""
    gone: set[str] = set()
    for line in diff.splitlines():
        if line.startswith("-") and not line.startswith("---"):
            m = DEF_RE.match(line[1:])
            if m:
                gone.add(m.group(1))
    return gone


def find_orphans(
    removed: set[str], files: dict[str, str]
) -> list[tuple[str, list[tuple[str, int, str]]]]:
    """Removed names with no surviving definition but a surviving code reference."""
    stripped: dict[str, str] = {}
    anomalies: dict[str, list[str]] = {}
    defined: set[str] = set()
    for path, text in files.items():
        clean, notes = strip_rust_noise(text)
        stripped[path] = clean
        if notes:
            anomalies[path] = notes
        defined |= defined_names(clean)
    orphans = []
    for name in sorted(removed - defined):
        word = re.compile(rf"\b{re.escape(name)}\b")
        hits = []
        for path in sorted(stripped):
            for lineno, line in enumerate(stripped[path].splitlines(), 1):
                if word.search(line):
                    hits.append((path, lineno, line.strip()[:120]))
        if hits:
            orphans.append((name, hits))
    return orphans, anomalies


def git(*args: str) -> tuple[int, str]:
    proc = subprocess.run(
        ["git", *args], capture_output=True, text=True, errors="replace"
    )
    return proc.returncode, proc.stdout


def git_bytes(*args: str) -> tuple[int, bytes]:
    proc = subprocess.run(["git", *args], capture_output=True)
    return proc.returncode, proc.stdout


def tree_files(treeish: str | None) -> dict[str, str] | None:
    """Every tracked .rs file under SCAN_DIRS, at `treeish` or in the index."""
    if treeish is None:
        rc, out = git("ls-files", "--", *SCAN_DIRS)
    else:
        rc, out = git("ls-tree", "-r", "--name-only", treeish, "--", *SCAN_DIRS)
    if rc != 0:
        return None
    names = [p for p in out.splitlines() if p.endswith(".rs")]
    if not names:
        return {}
    prefix = f"{treeish}:" if treeish is not None else ":"
    spec = "".join(f"{prefix}{p}\n" for p in names).encode()
    proc = subprocess.run(
        ["git", "cat-file", "--batch"], input=spec, capture_output=True
    )
    if proc.returncode != 0:
        return None
    # The header's size is a BYTE count, so the stream must be walked as bytes:
    # decoding first desynchronizes every offset after the first non-ASCII
    # character anywhere in the tree.
    blob, files, pos = proc.stdout, {}, 0
    for path in names:
        nl = blob.find(b"\n", pos)
        if nl < 0:
            break
        header = blob[pos:nl].split()
        if len(header) < 3:
            pos = nl + 1
            continue
        size = int(header[2])
        files[path] = blob[nl + 1 : nl + 1 + size].decode("utf-8", "replace")
        pos = nl + 1 + size + 1
    return files


def run_check(mode: str, rng: str | None) -> tuple[int, dict]:
    if mode == "staged":
        rc, diff = git("diff", "--cached", "-U0", "--", *SCAN_DIRS)
        after: str | None = None
        label = "staged changes"
    else:
        base, _, head = (rng or "HEAD~1..HEAD").partition("..")
        head = head or "HEAD"
        for ref in (base, head):
            if git("rev-parse", "--verify", "--quiet", ref)[0] != 0:
                return 3, {
                    "verdict": "cannot_answer",
                    "reason": f"git cannot resolve {ref!r}",
                }
        rc, diff = git("diff", "-U0", base, head, "--", *SCAN_DIRS)
        after = head
        label = f"{base}..{head}"
    if rc != 0:
        return 3, {"verdict": "cannot_answer", "reason": "git diff failed"}

    removed = removed_definitions(diff)
    if not removed:
        # Nothing was deleted, so nothing can be orphaned. Skipping the tree
        # read takes the common case from ~10s to ~0.1s. Report the SKIP rather
        # than a file count this run never looked at -- a gate that claims to
        # have scanned 1563 files when it read none is the constant-green shape
        # this family exists to catch.
        return 0, {
            "verdict": "clean",
            "range": label,
            "rs_files_scanned": 0,
            "scan_skipped": "no function definition was removed in this range",
            "definitions_removed": [],
            "orphans": [],
            "parse_anomalies": {},
        }
    files = tree_files(after)
    if files is None:
        return 3, {"verdict": "cannot_answer", "reason": "could not read the tree"}

    orphans, anomalies = find_orphans(removed, files)
    report = {
        "verdict": "orphan" if orphans else "clean",
        "range": label,
        "rs_files_scanned": len(files),
        "definitions_removed": sorted(removed),
        "orphans": [
            {"name": name, "references": [
                {"file": p, "line": n, "text": t} for p, n, t in hits
            ]}
            for name, hits in orphans
        ],
        "parse_anomalies": anomalies,
    }
    return (1 if orphans else 0), report


def emit(report: dict) -> None:
    print(f"range                : {report['range']}")
    print(f".rs files scanned    : {report['rs_files_scanned']}")
    if report.get("scan_skipped"):
        print(f"scan skipped         : {report['scan_skipped']}")
    removed = report["definitions_removed"]
    print(f"definitions removed  : {len(removed)}")
    for name in removed:
        print(f"    {name}")
    if report["parse_anomalies"]:
        print("parse anomalies (reported, NOT suppressed -- these under-strip):")
        for path, notes in sorted(report["parse_anomalies"].items()):
            print(f"    {path}: {', '.join(notes)}")
    if not report["orphans"]:
        print("\nOK: every removed definition is either redefined or uncalled.")
        return
    print(f"\nORPHANED: {len(report['orphans'])} removed definition(s) still called.")
    for item in report["orphans"]:
        print(f"\n  {item['name']} -- defined nowhere, referenced in code:")
        for ref in item["references"]:
            print(f"    {ref['file']}:{ref['line']}  {ref['text']}")
    print(
        "\nThis is rustc E0425. Either restore a definition, or remove the call.\n"
        "A `#[path]` sibling with `use super::*` is invisible to a `mod` grep, so\n"
        "grep the SYMBOL across the tree before deleting a free function."
    )


# --------------------------------------------------------------------------
# Self-test. Arms run the pure predicates on synthetic inputs, so they need no
# git history. The stripper's own failure DIRECTION is under control here: the
# `char_literal_quote` and `unterminated_*` arms exist because over-stripping
# produces a silent false pass, which is the only failure this gate cannot
# survive.
# --------------------------------------------------------------------------
def self_test() -> int:
    arms: list[tuple[str, bool]] = []

    def arm(name: str, ok: bool) -> None:
        arms.append((name, ok))

    def orphan_names(removed, files):
        found, _ = find_orphans(set(removed), files)
        return {name for name, _ in found}

    # 1. The real break: a call survives in a #[path] sibling.
    arm(
        "positive_control_f9ecec213_shape",
        orphan_names(
            ["incident_card_source_ids"],
            {
                "src/core/context.rs": "fn other() {}\n",
                "src/core/context_why_not.rs": (
                    "use super::*;\n"
                    "fn probe() {\n"
                    "    if incident_card_source_ids(&card).contains(&id) {}\n"
                    "}\n"
                ),
            },
        )
        == {"incident_card_source_ids"},
    )

    # 2. The measured false positive: a doc comment naming a deleted test.
    arm(
        "doc_comment_reference_is_not_a_call",
        orphan_names(
            ["situation_compare_without_dry_run_is_policy_error"],
            {
                "src/cli/mod.rs": (
                    "/// I restored `situation_compare_without_dry_run_is_policy_error` on\n"
                    "fn unrelated() {}\n"
                )
            },
        )
        == set(),
    )

    # 3. Block comment, including a nested one.
    arm(
        "block_comment_reference_is_not_a_call",
        orphan_names(
            ["gone_fn"],
            {"src/a.rs": "/* gone_fn /* nested gone_fn */ still */\nfn k() {}\n"},
        )
        == set(),
    )

    # 4/5. String and raw-string literals are prose for this purpose.
    arm(
        "string_literal_reference_is_not_a_call",
        orphan_names(["gone_fn"], {"src/a.rs": 'fn k() { log("gone_fn"); }\n'})
        == set(),
    )
    arm(
        "raw_string_reference_is_not_a_call",
        orphan_names(
            ["gone_fn"], {"src/a.rs": 'fn k() { let s = r#"gone_fn("x")"#; }\n'}
        )
        == set(),
    )

    # 6. A rename or a move leaves a definition, so nothing is orphaned.
    arm(
        "moved_definition_is_not_an_orphan",
        orphan_names(
            ["moved_fn"],
            {
                "src/a.rs": "fn caller() { moved_fn(); }\n",
                "src/b.rs": "pub(crate) fn moved_fn() {}\n",
            },
        )
        == set(),
    )

    # 7. A clean deletion: the name is gone everywhere.
    arm(
        "clean_deletion_passes",
        orphan_names(["gone_fn"], {"src/a.rs": "fn k() {}\n"}) == set(),
    )

    # 8. OVER-STRIP CONTROL. A char literal holding a double quote must not put
    #    the stripper into string mode and eat the call that follows.
    arm(
        "char_literal_quote_does_not_swallow_code",
        orphan_names(
            ["gone_fn"],
            {"src/a.rs": "fn k(c: char) {\n    if c == '\"' { gone_fn(); }\n}\n"},
        )
        == {"gone_fn"},
    )

    # 9. OVER-STRIP CONTROL. A lifetime is not an opening quote.
    arm(
        "lifetime_is_not_a_char_literal",
        orphan_names(
            ["gone_fn"],
            {"src/a.rs": "fn k<'a>(x: &'a str) { gone_fn(x); }\n"},
        )
        == {"gone_fn"},
    )

    # 10. Re-added in the same file within the same commit.
    arm(
        "redefined_in_place_passes",
        orphan_names(
            ["gone_fn"], {"src/a.rs": "fn gone_fn() {}\nfn k() { gone_fn(); }\n"}
        )
        == set(),
    )

    # 11. FAIL-OPEN CONTROL. An unterminated literal must leave the tail
    #     VISIBLE. Discarding it would hide every call below the anomaly.
    found, notes = find_orphans(
        {"gone_fn"}, {"src/a.rs": 'fn k() { let s = "unterminated\n    gone_fn();\n}\n'}
    )
    arm(
        "unterminated_string_still_reports_the_tail",
        {n for n, _ in found} == {"gone_fn"} and bool(notes.get("src/a.rs")),
    )
    found, notes = find_orphans(
        {"gone_fn"}, {"src/a.rs": "fn k() { /* open\n    gone_fn();\n}\n"}
    )
    arm(
        "unterminated_block_comment_still_reports_the_tail",
        {n for n, _ in found} == {"gone_fn"} and bool(notes.get("src/a.rs")),
    )

    # 12. The definition regex must see every qualifier ordering, or a removal
    #     goes unnoticed and the gate reports clean.
    quals = [
        "fn plain(",
        "    fn indented(",
        "pub fn public(",
        "pub(crate) fn restricted(",
        "pub(super) fn parental(",
        "pub(in crate::core) fn pathed(",
        "    pub(crate) async fn async_fn(",
        "pub const fn const_fn(",
        "unsafe fn unsafe_fn(",
        "pub unsafe extern \"C\" fn extern_fn(",
        "    default fn defaulted(",
    ]
    seen = removed_definitions("\n".join(f"-{q}" for q in quals))
    expected = {
        "plain", "indented", "public", "restricted", "parental", "pathed",
        "async_fn", "const_fn", "unsafe_fn", "extern_fn", "defaulted",
    }
    arm("definition_regex_covers_qualifier_orderings", seen == expected)

    # 13. A `fn` inside a comment or string in the diff must not count as a
    #     removed definition -- that would invent names and red a clean commit.
    arm(
        "removed_definition_ignores_a_call_line",
        removed_definitions("-    gone_fn();\n-    let x = also_fn(1);\n") == set(),
    )

    # 14. Word boundary: a removed `fn get` must not match `get_evidence_span`.
    arm(
        "reference_match_is_word_bounded",
        orphan_names(["get"], {"src/a.rs": "fn k() { get_evidence_span(); }\n"})
        == set(),
    )

    # 15. A substring of a longer removed name must not credit the short one.
    arm(
        "prefix_is_not_containment",
        orphan_names(
            ["card_sources"], {"src/a.rs": "fn k() { verified_card_sources(); }\n"}
        )
        == set(),
    )

    width = max(len(name) for name, _ in arms)
    for name, ok in arms:
        print(f"  [{'PASS' if ok else 'FAIL'}] {name.ljust(width)}")
    passed = sum(1 for _, ok in arms if ok)
    print(f"\nself-test: {passed}/{len(arms)} passed")
    return 0 if passed == len(arms) else 1


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Find a function definition a commit deleted that is still called."
    )
    parser.add_argument(
        "--range",
        metavar="A..B",
        help="commit range to inspect (default HEAD~1..HEAD)",
    )
    parser.add_argument(
        "--staged",
        action="store_true",
        help="inspect staged changes against the index, for pre-commit use",
    )
    parser.add_argument("--json", action="store_true", help="emit the report as JSON")
    parser.add_argument("--self-test", action="store_true", help="run the arms")
    args = parser.parse_args()

    if args.self_test:
        return self_test()
    if args.staged and args.range:
        print("--staged and --range are mutually exclusive", file=sys.stderr)
        return 2

    code, report = run_check("staged" if args.staged else "range", args.range)
    if args.json:
        print(json.dumps(report, indent=2, sort_keys=True))
    elif report["verdict"] == "cannot_answer":
        print(f"CANNOT ANSWER: {report['reason']}", file=sys.stderr)
        print(
            "Exit 3 is not a red. Nothing was checked, so nothing is known.",
            file=sys.stderr,
        )
    else:
        emit(report)
    return code


if __name__ == "__main__":
    sys.exit(main())
