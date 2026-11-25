#!/usr/bin/env python3
"""Fail when a declared surface function is reachable ONLY from test code.

Asked for, in the same words, by two beads:
  bd-6oqlx  "a gate that fails if a scoring surface has only test callers"
  bd-223vl  the eight doctor fixers "reachable only from their own unit tests"

A function existing is not a capability. Both beads record the same way of
hiding: the fn is written, its unit tests pass, every reader assumes it runs,
and nothing calls it outside `#[cfg(test)]`.

WHY THIS NEEDS A REAL PARSE AND NOT A GREP
    Two mistakes are easy here and I made both before writing this.

    1. EXCLUDING THE DEFINING FILE. Looking for callers "somewhere other than
       the module that defines them" reports a wired feature as dead, because
       dispatch tables live NEXT TO the functions they dispatch.
       src/core/doctor_fixers.rs has exactly that: `fix_dispatch_for_finding`
       routes ten fixers from inside the same file. A sweep that skips the file
       sees zero callers for all ten.
    2. SPLITTING ON THE FIRST `#[cfg(test)]`. A file may have several test
       modules with PRODUCTION CODE BETWEEN THEM. src/core/doctor_fixers.rs
       carries `#[cfg(test)]` at two places, so
       `source.split("#[cfg(test)]").next()` -- which an existing test in that
       very file uses -- treats everything after the first as test and cannot
       see production code that follows it.

    So: brace-match every `#[cfg(test)] mod ... { }` region, treat ONLY those
    line ranges as test, and count references everywhere else INCLUDING the
    defining file.

SHRINK-ONLY BASELINE, FAILING IN BOTH DIRECTIONS
    The known violations are baselined rather than fixed here, because wiring
    eight doctor fixers at once is a decision with its own tests and belongs to
    bd-223vl, not to a gate that merely adds visibility. Following the house
    pattern (scripts/action-pins-baseline.txt,
    tests/fixtures/e2e_invocation/orphan_baseline.txt):

      * a NEW test-only surface is an error;
      * a baselined entry that is now reachable is ALSO an error, telling you to
        delete the line.

    The second direction is what stops a baseline decaying into a permanent
    ignore-list.

USAGE
    scripts/check-production-reachability.py
    scripts/check-production-reachability.py --json
    scripts/check-production-reachability.py --self-test
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
import tempfile
from pathlib import Path

BASELINE = "scripts/production-reachability-baseline.txt"

# Surfaces to audit: (file, regex matching the function names that are surface).
# Deliberately explicit. A gate that guesses which functions matter produces
# noise, and noise is how a gate gets disabled.
SURFACES: list[tuple[str, str]] = [
    # bd-223vl: `ee doctor --fix` fixers.
    ("src/core/doctor_fixers.rs", r"fix_[a-z0-9_]+"),
    # bd-6oqlx: the documented scoring multiplier stack.
    ("src/search/scoring.rs", r"(?:anchor_match_score|bead_affinity_score|final_score|stale_anchor_floor|freshness_drift_multiplier)"),
]

# `[ \t]*`, NOT `\s*`, after the `^`. Under re.M, `\s` matches newlines, so
# `^\s*fn` could anchor on a BLANK LINE ABOVE the definition and consume the
# newline -- making `m.start()` land one line early and `def_line` one too
# small. The consequence was a silent FALSE NEGATIVE, in the direction that
# hides findings: the `idx == def_line` guard then failed to skip the real
# definition line, so the fn counted as referencing itself from production and
# could never be reported as test-only. Any surface fn preceded by a blank
# line was exempt from this gate. It stayed invisible because Rust style puts
# a doc comment immediately above most fns, and `\s*` cannot cross a `///`
# line -- so the bug only fires on the undocumented ones.
DEF_RE_TEMPLATE = r"^[ \t]*(?:pub(?:\s*\([^)]*\))?\s+)?(?:async\s+)?(?:unsafe\s+)?fn\s+({name})\s*[(<]"
CFG_TEST_RE = re.compile(r"#\[cfg\(test\)\]")


def strip_line_comment(line: str) -> str:
    """Drop a Rust `//` line comment so a MENTION cannot pass as a CALL.

    A name written in a comment is not a caller. Without this, documenting why
    a fn is NOT wired marks it as wired, and the gate reports the opposite of
    the truth. That is not hypothetical: commit 5b09b76f9 added a comment to
    the dispatch table in src/core/doctor_fixers.rs naming
    `fix_graph_snapshot_stale` and `fix_policy_safety_inconsistent` -- to
    record that nothing dispatches them -- and this gate immediately reported
    both as "now reachable" and failed, demanding their baseline lines be
    deleted. Deleting them would have retired two true findings on the
    strength of a comment that says they are true.

    scripts/e2e_invocation_audit.sh reached the same conclusion for shell and
    states it the same way: "Invoked is NOT the same as mentioned."

    KNOWN LIMITATION, stated rather than hidden: this splits on the first `//`
    without tracking string literals, so `let u = "http://x"; fix_y();` loses
    its real call. That direction is the safe one -- the fn is then reported as
    test-only, which is a visible false finding rather than a silent miss.
    Block comments are not handled; every current surface uses `//`.
    """
    return line.split("//", 1)[0]
MOD_RE = re.compile(r"\bmod\s+[A-Za-z_][A-Za-z0-9_]*\s*\{")
MOD_FAST_RE = re.compile(r"\bmod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;")
MOD_DECL_RE = re.compile(
    r"^[ \t]*(?:pub(?:\s*\([^)]*\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;",
    re.M,
)
PATH_ATTR_RE = re.compile(r'#\[path\s*=\s*"([^"]+)"\]')
OWN_SURFACE_RE = re.compile(
    r"^pub(?:\s*\([^)]*\))?\s+(?:struct|enum|trait|type|fn)\s+",
    re.M,
)
IMPL_BLOCK_RE = re.compile(r"^impl\b", re.M)
IDENT_TOKEN_RE = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
BRACE_RE = re.compile(r"[{}]")
NON_CODE_BRACE_MASK_RE = re.compile(
    r"//[^\n]*"
    r"|/\*[\s\S]*?\*/"
    r'|b?r(#*)"[\s\S]*?"\1'
    # `[\s\S]`, not `.`: a backslash before a newline continues a Rust string, and `.` does not match
    # the newline, so with it every later quote in the file paired with the wrong partner.
    r'|b?"(?:\\[\s\S]|[^"\\])*"'
    r"|b?'(?:\\.|[^'\\])'"
)


def _mask_match(m: re.Match[str]) -> str:
    s = m.group(0)
    if "{" not in s and "}" not in s and not s.startswith("//") and not s.startswith("/*"):
        return s
    if "\n" not in s:
        return " " * len(s)
    return "".join("\n" if ch == "\n" else " " for ch in s)


def mask_non_code_braces(text: str) -> str:
    """Replace comments, string literals, and char literals with spaces while
    preserving newlines so brace matching in `test_line_ranges` is never thrown
    off by `'{'` char literals or comments between `#[cfg(test)]` and `mod`.
    """
    return NON_CODE_BRACE_MASK_RE.sub(_mask_match, text)


def test_line_ranges(text: str) -> list[tuple[int, int]]:
    """1-based inclusive line ranges of every `#[cfg(test)] mod ... { .. }`.

    Brace-matched over masked code, so several test modules in one file are all
    found, `'{'` char literals inside tests do not unbalance braces, and
    production code BETWEEN test modules is not swallowed.
    """
    if "#[cfg(test)]" not in text:
        return []
    attrs = list(CFG_TEST_RE.finditer(text))
    if not attrs:
        return []
    ranges: list[tuple[int, int]] = []
    masked: str | None = None
    mask_offset = 0
    for attr in attrs:
        # Fast pre-check: `mod <name> {` must appear within 500 chars of `#[cfg(test)]`.
        m_raw = MOD_RE.search(text, attr.end(), attr.end() + 500)
        if not m_raw:
            continue
        between_slice = text[attr.end() : m_raw.start()]
        if "//" in between_slice or "/*" in between_slice:
            between_slice = mask_non_code_braces(between_slice)
        if between_slice.strip().replace("\n", "") and not re.fullmatch(
            r"[\s]*(?:#\[[^\]]*\]\s*)*", between_slice
        ):
            continue
        if masked is None:
            masked = mask_non_code_braces(text[attr.start() :])
            mask_offset = attr.start()
        depth = 0
        i = len(masked)
        for bm in BRACE_RE.finditer(masked, m_raw.end() - 1 - mask_offset):
            if bm.group(0) == "{":
                depth += 1
            else:
                depth -= 1
                if depth == 0:
                    i = bm.start()
                    break
        end_pos = min(i + mask_offset, len(text) - 1)
        start_line = text.count("\n", 0, attr.start()) + 1
        end_line = text.count("\n", 0, end_pos) + 1
        ranges.append((start_line, end_line))
    return ranges


def in_ranges(line_no: int, ranges: list[tuple[int, int]]) -> bool:
    return any(lo <= line_no <= hi for lo, hi in ranges)


def rust_files(root: Path) -> list[Path]:
    out = []
    for base in ("src", "tests", "benches"):
        start = root / base
        if not start.is_dir():
            continue
        for dirpath, dirnames, filenames in os.walk(start):
            dirnames[:] = [d for d in dirnames if not d.startswith(".") and d != "target"]
            for name in filenames:
                if name.endswith(".rs"):
                    out.append(Path(dirpath) / name)
    return sorted(out)


def read(path: Path) -> str:
    try:
        return path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return ""


def audit(root: Path, surfaces: list[tuple[str, str]]) -> dict:
    files = rust_files(root)
    text_cache: dict[Path, str] = {}
    lines_cache: dict[Path, list[str]] = {}
    tranges_cache: dict[Path, list[tuple[int, int]]] = {}
    for f in files:
        text_cache[f] = read(f)

    def get_lines(f: Path) -> list[str]:
        res = lines_cache.get(f)
        if res is None:
            res = text_cache[f].splitlines()
            lines_cache[f] = res
        return res

    def get_tranges(f: Path) -> list[tuple[int, int]]:
        res = tranges_cache.get(f)
        if res is None:
            res = test_line_ranges(text_cache[f])
            tranges_cache[f] = res
        return res

    first_cfg_cache: dict[Path, int] = {}

    def is_pos_in_test_range(f: Path, pos: int, idx: int) -> bool:
        first_cfg = first_cfg_cache.get(f)
        if first_cfg is None:
            first_cfg = text_cache[f].find("#[cfg(test)]")
            first_cfg_cache[f] = first_cfg
        if first_cfg == -1 or pos < first_cfg:
            return False
        return in_ranges(idx, get_tranges(f))

    findings = []
    examined = 0
    resolved = 0
    for rel, name_pat in surfaces:
        path = root / rel
        if path not in text_cache:
            findings.append({"surface": rel, "name": None, "issue": "surface file not found"})
            continue
        resolved += 1
        text = text_cache[path]
        def_re = re.compile(DEF_RE_TEMPLATE.format(name=name_pat), re.M)
        for m in def_re.finditer(text):
            fname = m.group(1)
            def_line = text.count("\n", 0, m.start()) + 1
            # A fn DEFINED inside a test module is a test helper, not a surface.
            if is_pos_in_test_range(path, m.start(), def_line):
                continue
            examined += 1
            has_prod_ref = False
            word = re.compile(r"\b" + re.escape(fname) + r"\b")
            # Check defining file first (dispatch tables live next to fixers).
            ordered_files = [path] + [f for f in files if f != path]
            for f in ordered_files:
                ftext = text_cache[f]
                if fname not in ftext:
                    continue
                flines = get_lines(f)
                seen_lines: set[int] = set()
                for wm in word.finditer(ftext):
                    idx = ftext.count("\n", 0, wm.start()) + 1
                    if idx in seen_lines or (f == path and idx == def_line):
                        continue
                    seen_lines.add(idx)
                    if not word.search(strip_line_comment(flines[idx - 1])):
                        continue
                    if is_pos_in_test_range(f, wm.start(), idx):
                        continue
                    has_prod_ref = True
                    break
                if has_prod_ref:
                    break
            if not has_prod_ref:
                test_refs = 0
                for f, ftext in text_cache.items():
                    if fname not in ftext:
                        continue
                    flines = get_lines(f)
                    seen_lines = set()
                    for wm in word.finditer(ftext):
                        idx = ftext.count("\n", 0, wm.start()) + 1
                        if idx in seen_lines or (f == path and idx == def_line):
                            continue
                        seen_lines.add(idx)
                        if not word.search(strip_line_comment(flines[idx - 1])):
                            continue
                        if is_pos_in_test_range(f, wm.start(), idx):
                            test_refs += 1
                findings.append(
                    {
                        "surface": rel,
                        "name": fname,
                        "defined_at": f"{rel}:{def_line}",
                        "issue": "reachable only from test code",
                        "test_references": test_refs,
                    }
                )
    # Tree-wide module reachability across src/**.rs (bd-6czxg Clause 2).
    src_files = [f for f in files if f.is_relative_to(root / "src")]
    mod_decls: list[tuple[Path, int, str, Path, bool]] = []
    test_mod_files: set[Path] = set()
    for f in src_files:
        ftext = text_cache[f]
        if "mod " not in ftext:
            continue
        flines = get_lines(f)
        for m in MOD_FAST_RE.finditer(ftext):
            idx = ftext.count("\n", 0, m.start()) + 1
            if not MOD_DECL_RE.match(strip_line_comment(flines[idx - 1])):
                continue
            mod_name = m.group(1)
            j = idx - 2
            is_cfg_test = False
            path_override = None
            while j >= 0:
                prev = strip_line_comment(flines[j]).strip()
                if not prev:
                    j -= 1
                    continue
                if prev.startswith("#["):
                    if "cfg(" in prev and "test" in prev:
                        is_cfg_test = True
                    pm = PATH_ATTR_RE.search(prev)
                    if pm:
                        path_override = pm.group(1)
                    j -= 1
                else:
                    break
            if not is_cfg_test:
                is_cfg_test = is_pos_in_test_range(f, m.start(), idx)
            parent_dir = (
                f.parent
                if f.name in ("mod.rs", "lib.rs", "main.rs")
                else f.parent / f.stem
            )
            if path_override:
                target = Path(os.path.normpath(f.parent / path_override))
                if target not in text_cache:
                    target = Path(os.path.normpath(parent_dir / path_override))
            else:
                c1 = Path(os.path.normpath(parent_dir / f"{mod_name}.rs"))
                c2 = Path(os.path.normpath(parent_dir / mod_name / "mod.rs"))
                target = c1 if c1 in text_cache else c2
            mod_decls.append((f, idx, mod_name, target, is_cfg_test))

    changed = True
    while changed:
        changed = False
        for decl_file, _, _, target, is_cfg_test in mod_decls:
            if (
                is_cfg_test
                or decl_file in test_mod_files
                or target.name.endswith("_tests.rs")
                or target.name.endswith("_test_module.rs")
            ):
                if target not in test_mod_files:
                    test_mod_files.add(target)
                    changed = True

    prod_mod_decls = [
        d
        for d in mod_decls
        if not d[4] and d[0] not in test_mod_files and d[3] not in test_mod_files
    ]
    decl_files_set = {d[0] for d in prod_mod_decls}
    non_test_src_files = sorted(
        (f for f in src_files if f not in test_mod_files),
        key=lambda p: (0 if p in decl_files_set else 1, len(text_cache[p])),
    )
    decl_locations: set[tuple[Path, int, str]] = {
        (d[0], d[1], d[2]) for d in prod_mod_decls
    }
    # Track which declarations still need a production reference outside their target.
    pending_decls: list[tuple[Path, int, str, Path]] = [
        (d[0], d[1], d[2], d[3]) for d in prod_mod_decls
    ]
    modules_examined = len(pending_decls)
    unresolved_by_idx: dict[int, tuple[Path, int, str, Path]] = dict(
        enumerate(pending_decls)
    )
    word_re_cache: dict[str, re.Pattern[str]] = {
        d[2]: re.compile(r"\b" + re.escape(d[2]) + r"\b") for d in pending_decls
    }
    # Scan non_test_src_files (smallest first) and retire each declaration as
    # soon as its first production reference is found, using a single combined
    # regex pass per file rather than one regex pass per candidate module.
    for f in non_test_src_files:
        if not unresolved_by_idx:
            break
        ftext = text_cache.get(f)
        if not ftext:
            continue
        by_name: dict[str, list[int]] = {}
        for i, (_, _, mod_name, target) in unresolved_by_idx.items():
            if f != target and mod_name in ftext:
                by_name.setdefault(mod_name, []).append(i)
        if not by_name:
            continue
        flines = get_lines(f)
        combined_re = re.compile(
            r"\b("
            + "|".join(re.escape(m) for m in sorted(by_name, key=len, reverse=True))
            + r")\b"
        )
        for m in combined_re.finditer(ftext):
            mod_name = m.group(1)
            indices = by_name.get(mod_name)
            if not indices:
                continue
            idx = ftext.count("\n", 0, m.start()) + 1
            if (f, idx, mod_name) in decl_locations:
                continue
            if not word_re_cache[mod_name].search(strip_line_comment(flines[idx - 1])):
                continue
            if is_pos_in_test_range(f, m.start(), idx):
                continue
            for i in indices:
                unresolved_by_idx.pop(i, None)
            del by_name[mod_name]
            if not by_name:
                break

    for _, (decl_file, decl_idx, mod_name, target) in sorted(
        unresolved_by_idx.items()
    ):
        if True:
            target_text = text_cache.get(target)
            if target_text is not None:
                t_masked = mask_non_code_braces(target_text)
                # Exclude impl-only extension modules (e.g. daemon_search_diagnostics.rs)
                # that attach inherent methods to a parent type without declaring
                # their own top-level public types/functions.
                if not OWN_SURFACE_RE.search(t_masked) and IMPL_BLOCK_RE.search(t_masked):
                    modules_examined -= 1
                    continue
            word = re.compile(r"\b" + re.escape(mod_name) + r"\b")
            test_refs = 0
            for f in src_files:
                if f == target:
                    continue
                ftext = text_cache[f]
                if mod_name not in ftext:
                    continue
                flines = get_lines(f)
                seen_lines: set[int] = set()
                for wm in word.finditer(ftext):
                    idx = ftext.count("\n", 0, wm.start()) + 1
                    if idx in seen_lines or (f == decl_file and idx == decl_idx):
                        continue
                    seen_lines.add(idx)
                    if not word.search(strip_line_comment(flines[idx - 1])):
                        continue
                    if f not in test_mod_files and not is_pos_in_test_range(
                        f, wm.start(), idx
                    ):
                        continue
                    test_refs += 1
            rel_decl = os.path.relpath(decl_file, root).replace(os.sep, "/")
            rel_target = (
                os.path.relpath(target, root).replace(os.sep, "/")
                if target.exists()
                else mod_name
            )
            findings.append(
                {
                    "surface": rel_target,
                    "name": f"module:{mod_name}",
                    "defined_at": f"{rel_decl}:{decl_idx}",
                    "issue": "module unreferenced in production src/",
                    "test_references": test_refs,
                }
            )

    return {
        "schema": "ee.production_reachability.v1",
        "surfacesExamined": examined,
        "modulesExamined": modules_examined,
        # SCOPE, reported because this gate's function pass is scoped to SURFACES
        # while `modulesExamined` covers all non-test module declarations in src/.
        "surfaceFilesDeclared": len(surfaces),
        "surfaceFilesResolved": resolved,
        "rustFilesScanned": len(files),
        "testOnly": findings,
    }


def load_baseline(root: Path) -> set[str]:
    path = root / BASELINE
    if not path.is_file():
        return set()
    out = set()
    for line in read(path).splitlines():
        line = line.strip()
        if line and not line.startswith("#"):
            out.add(line)
    return out


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", default=".")
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()

    if args.self_test:
        return self_test()

    root = Path(args.root).resolve()
    report = audit(root, SURFACES)
    baseline = load_baseline(root)
    found = {f["name"] for f in report["testOnly"] if f.get("name")}

    new = sorted(found - baseline)
    fixed = sorted(baseline - found)
    report["baselineSize"] = len(baseline)
    report["newViolations"] = new
    report["baselinedButNowReachable"] = fixed

    if args.json:
        print(json.dumps(report, indent=2, sort_keys=True))
    else:
        print(
            "[reachability] predicate: a declared surface fn with NO reference outside "
            "a brace-matched #[cfg(test)] module"
        )
        print(
            f"[reachability] surfaces examined: {report['surfacesExamined']}; "
            f"test-only: {len(found)}; baseline: {len(baseline)}"
        )
        print(
            f"[reachability] SCOPE: those surfaces come from "
            f"{report['surfaceFilesResolved']} of {report['surfaceFilesDeclared']} "
            f"declared surface file(s), against {report['rustFilesScanned']} .rs "
            f"file(s) scanned for references. Every other file in the tree is "
            f"OUTSIDE this gate's population, so a pass is NOT a tree-wide "
            f"reachability result."
        )
        for f in report["testOnly"]:
            mark = "NEW" if f.get("name") in new else "baselined"
            print(f"   [{mark}] {f.get('name')}  ({f.get('defined_at')}) "
                  f"test refs={f.get('test_references')}")

    status = 0
    if new:
        print(f"\n[reachability] FAIL: {len(new)} surface(s) reachable only from tests "
              f"and not in {BASELINE}:", file=sys.stderr)
        for n in new:
            print(f"    {n}", file=sys.stderr)
        print("  Wire it, or add it to the baseline with a reason if it is "
              "deliberately not dispatched.", file=sys.stderr)
        status = 1
    if fixed:
        print(f"\n[reachability] FAIL: {len(fixed)} baselined entr(ies) are now "
              f"reachable; delete their line(s) from {BASELINE}:", file=sys.stderr)
        for n in fixed:
            print(f"    {n}", file=sys.stderr)
        print("  A baseline that keeps wired entries is an ignore-list.", file=sys.stderr)
        status = 1
    if status == 0:
        print(
            "[reachability] OK -- no test-only surface beyond the accepted baseline, "
            f"WITHIN the {report['surfaceFilesResolved']} declared surface file(s). "
            "Unlisted files were not examined; add a (path, pattern) pair to "
            "SURFACES to bring one into scope."
        )
    return status


# ---------------------------------------------------------------------------
# Self-test. The arms that matter are the REFUSALS and the two traps in the
# docstring: a dispatch table in the defining file must count as production, and
# production code AFTER a test module must not be swallowed.
# ---------------------------------------------------------------------------


def self_test() -> int:
    arms: list[tuple[str, bool]] = []

    def arm(label: str, ok: bool) -> None:
        arms.append((label, ok))
        print(f"  [{'ok  ' if ok else 'FAIL'}] {label}")

    with tempfile.TemporaryDirectory(prefix="reach-selftest-") as tmp:
        root = Path(tmp)
        (root / "src").mkdir()
        (root / "src" / "surface.rs").write_text(
            # A: called only from the test module -> VIOLATION
            "fn fix_only_tested() -> u32 { 1 }\n"
            # B: called by a dispatch table in THIS SAME FILE -> production
            "fn fix_in_table() -> u32 { 2 }\n"
            "pub fn dispatch(which: &str) -> u32 {\n"
            "    match which { \"t\" => fix_in_table(), _ => 0 }\n"
            "}\n"
            "\n"
            "#[cfg(test)]\n"
            "mod tests {\n"
            "    use super::*;\n"
            "    #[test]\n"
            "    fn t1() { assert_eq!(fix_only_tested(), 1); }\n"
            "    #[test]\n"
            "    fn t2() { assert_eq!(fix_in_table(), 2); }\n"
            "    #[test]\n"
            "    fn t3() { assert_eq!(fix_after_tests(), 3); }\n"
            "}\n"
            "\n"
            # C: defined AFTER a test module and called from production after it.
            # The split-on-first-cfg(test) bug would call this test-only.
            "fn fix_after_tests() -> u32 { 3 }\n"
            "pub fn later_production() -> u32 { fix_after_tests() }\n"
            "\n"
            "#[cfg(test)]\n"
            "mod more_tests {\n"
            "    #[test]\n"
            "    fn t4() { assert!(true); }\n"
            "}\n"
            "\n"
            # D: called only from tests, but NAMED in a production comment.
            # The comment is the shape that broke this gate for real: a note
            # explaining that the fn is not dispatched. A mention must not
            # count as a call, or documenting a hole closes the report of it.
            "fn fix_mentioned_in_comment() -> u32 { 4 }\n"
            "pub fn table_v2(which: &str) -> u32 {\n"
            "    // fix_mentioned_in_comment would fit here but nothing emits\n"
            "    // its finding, so an arm would be dead code.\n"
            "    match which { \"x\" => 0, _ => 1 }\n"
            "}\n"
            "\n"
            "#[cfg(test)]\n"
            "mod comment_tests {\n"
            "    use super::*;\n"
            "    #[test]\n"
            "    fn t5() { assert_eq!(fix_mentioned_in_comment(), 4); }\n"
            "}\n"
            "\n"
            # E: a real call that happens to carry a trailing comment. The
            # comment strip must not eat the call sitting before it.
            "fn fix_called_with_trailing_comment() -> u32 { 5 }\n"
            "pub fn table_v3() -> u32 { fix_called_with_trailing_comment() } // see above\n",
            encoding="utf-8",
        )
        # F: an UNLISTED file carrying the same shape of defect. This is the
        # positive control for the scope report: the gate must not flag it
        # (it is outside SURFACES), and the scope numbers must say so. Without
        # this arm the scope line is an unchecked claim.
        (root / "src" / "unlisted.rs").write_text(
            "fn fix_unlisted_and_test_only() -> u32 { 9 }\n"
            "#[cfg(test)]\n"
            "mod tests {\n"
            "    #[test]\n"
            "    fn t() { assert_eq!(super::fix_unlisted_and_test_only(), 9); }\n"
            "}\n",
            encoding="utf-8",
        )
        surfaces = [("src/surface.rs", r"fix_[a-z0-9_]+")]
        rep = audit(root, surfaces)
        names = {f["name"] for f in rep["testOnly"]}

        arm("a fn called only from #[cfg(test)] is flagged", "fix_only_tested" in names)
        arm(
            "a fn dispatched from a table IN THE SAME FILE is NOT flagged",
            "fix_in_table" not in names,
        )
        arm(
            "production code AFTER a test module is seen (no split-on-first bug)",
            "fix_after_tests" not in names,
        )
        arm(
            "a fn only MENTIONED in a production comment is still flagged",
            "fix_mentioned_in_comment" in names,
        )
        arm(
            "a real call with a trailing comment is NOT flagged",
            "fix_called_with_trailing_comment" not in names,
        )
        arm("all five surface fns were examined", rep["surfacesExamined"] == 5)

        # SCOPE ARMS. The three together are what make the pass line's new
        # wording true rather than decorative: a defect of exactly the flagged
        # shape, in a file nobody listed, is invisible to this gate -- and the
        # denominator has to admit that the file was there to be missed.
        arm(
            "a test-only fn in an UNLISTED file is NOT flagged (the scope limit is real)",
            "fix_unlisted_and_test_only" not in names,
        )
        arm(
            "the scope report names the declared surface count, not the tree",
            rep["surfaceFilesDeclared"] == 1 and rep["surfaceFilesResolved"] == 1,
        )
        arm(
            "the reference denominator counts the unlisted file it did not examine",
            rep["rustFilesScanned"] >= 2,
        )

        ranges = test_line_ranges(read(root / "src" / "surface.rs"))
        arm("every test module is found, not just the first", len(ranges) == 3)
        arm(
            "the ranges do not overlap the production tail",
            all(not (lo <= 1 <= hi) for lo, hi in ranges),
        )

        # BASELINE DIRECTION 2: an entry that is now reachable must be reported.
        (root / "scripts").mkdir()
        (root / BASELINE).write_text(
            "# planted\nfix_only_tested\nfix_in_table\n", encoding="utf-8"
        )
        base = load_baseline(root)
        found = names
        arm("a baselined-but-now-reachable entry is detected",
            "fix_in_table" in (base - found))
        arm("a genuinely test-only baselined entry is NOT reported as fixed",
            "fix_only_tested" not in (base - found))

        # A surface file that does not exist must be reported, not silently skipped.
        rep2 = audit(root, [("src/nope.rs", r"fix_[a-z0-9_]+")])
        arm(
            "a missing surface file is reported, not silently passed",
            any(f.get("issue") == "surface file not found" for f in rep2["testOnly"]),
        )

        # MODULE REACHABILITY ARMS (bd-6czxg Clause 2):
        # 1. A `'{'` char literal inside a #[cfg(test)] mod must not unbalance
        #    brace matching and swallow production code after the test module.
        # 2. A `pub mod unwired_mod;` with no production consumer is flagged as
        #    `module:unwired_mod`.
        # 3. A `mod wired_immediately;` followed on the very next line by
        #    `use wired_immediately::Thing;` is NOT flagged (no line-window bug).
        # 4. A `#[cfg(test)] mod test_only_mod;` is NOT flagged.
        (root / "src" / "lib.rs").write_text(
            "pub mod unwired_mod;\n"
            "mod wired_immediately;\n"
            "use wired_immediately::Thing;\n"
            "#[cfg(test)]\n"
            "mod test_only_mod;\n"
            "#[cfg(test)]\n"
            "mod char_brace_tests {\n"
            "    #[test]\n"
            "    fn char_literal_brace() {\n"
            "        let s = \"{hello}\";\n"
            "        assert!(s.starts_with('{'));\n"
            "    }\n"
            "}\n"
            "pub fn use_thing() -> u32 { Thing::val() }\n"
            "mod impl_ext;\n",
            encoding="utf-8",
        )
        # 5. An impl-only extension module (inherent methods on a parent type,
        #    reached through method calls that never name the module) is exempt.
        #    Its string literal continues across a line with a trailing `\`,
        #    which the masking must treat as one literal: when it did not, the
        #    quotes paired up wrongly from there on and blanked the `impl` line,
        #    so src/db/memory_seal_authority.rs (about 20 production callers)
        #    was reported as unwired.
        (root / "src" / "impl_ext.rs").write_text(
            "const SQL: &str = \"SELECT \\\n"
            "    a FROM t\";\n"
            "impl Thing {\n"
            "    pub fn ext() -> &'static str { \"ok\" }\n"
            "}\n",
            encoding="utf-8",
        )
        (root / "src" / "unwired_mod.rs").write_text(
            "pub fn never_called() -> u32 { 42 }\n",
            encoding="utf-8",
        )
        (root / "src" / "wired_immediately.rs").write_text(
            "pub struct Thing;\n"
            "impl Thing { pub fn val() -> u32 { 7 } }\n",
            encoding="utf-8",
        )
        (root / "src" / "test_only_mod.rs").write_text(
            "pub fn helper() -> u32 { 1 }\n",
            encoding="utf-8",
        )
        rep3 = audit(root, surfaces)
        names3 = {f["name"] for f in rep3["testOnly"]}
        arm(
            "an unwired production module in src/ is flagged as module:<name>",
            "module:unwired_mod" in names3,
        )
        arm(
            "a module used on the line immediately after its declaration is NOT flagged",
            "module:wired_immediately" not in names3,
        )
        arm(
            "a #[cfg(test)]-gated module declaration is NOT flagged",
            "module:test_only_mod" not in names3,
        )
        arm(
            "an impl-only module whose string continues past a trailing backslash is NOT flagged",
            "module:impl_ext" not in names3,
        )
        lib_ranges = test_line_ranges(read(root / "src" / "lib.rs"))
        arm(
            "a '{' char literal in a test module does not swallow trailing production code",
            len(lib_ranges) == 1 and lib_ranges[0][1] < 14,
        )

    passed = sum(1 for _, ok in arms if ok)
    print(f"\n[reachability] self-test: {passed}/{len(arms)} arms passed")
    return 0 if passed == len(arms) else 1


if __name__ == "__main__":
    raise SystemExit(main())
