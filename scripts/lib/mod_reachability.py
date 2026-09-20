#!/usr/bin/env python3
"""Every tracked .rs under src/ and tests/ must be reachable from a cargo target.

bd-l6h3g. A .rs file that no cargo target reaches is invisible to BOTH
`cargo fmt` and rustc: the formatter reports it clean because it never examines
it, and the compiler never type-checks it. Four such files were found in this
repo carrying 2350 lines and 17 test functions, one of them a 1151-line
bypass-token module undeclared since 2026-05-15.

READ-ONLY. This script parses and reports; it never edits a file. AGENTS.md
forbids scripts that modify code files in this repo.

HOW REACHABILITY IS COMPUTED
  Roots come from `cargo fmt --verbose --check`, which prints every target root
  cargo itself walks. From each root we follow `mod x;` and
  `#[path = "..."] mod y;` transitively. That is the same traversal rustfmt and
  rustc perform, so "reachable" here means what it means to them.

  Taking the roots from cargo rather than reconstructing them from Cargo.toml is
  deliberate: this repo sets autotests = false and declares 42 explicit [[test]]
  targets, and a hand-rolled target list got the answer wrong twice before this.

SELF-VALIDATION, which is the part these checks usually skip
  The resolver is checked against a KNOWN POSITIVE and a KNOWN NEGATIVE taken
  from observed cargo behaviour, not from reading:
    tests/contracts/ask_native.rs   cargo fmt REPORTED it -> must be reachable
    src/core/preflight_token.rs     its `pub mod` line was removed by 0f68778a0
                                    -> must be unreachable
  If either control disagrees the script REFUSES to emit a verdict. A resolver
  that cannot find a file cargo just formatted has no business declaring other
  files unreachable.

EXIT CODES
  0  every in-scope file is reachable or allowlisted, and the allowlist is clean
  1  a real finding: an unreachable file that is not allowlisted, an allowlist
     entry that has rotted, or a section-2 budget that no longer matches
  2  ENVIRONMENTALLY inconclusive: cargo missing or timed out. The tree was not
     examined. Callers may treat this as "not blocking".
  3  THE INSTRUMENT IS BROKEN: a self-validation control disagreed with observed
     cargo behaviour. This is NOT interchangeable with 2 and callers must NOT
     fail open on it.

  2 AND 3 WERE ONE CODE UNTIL THIS WAS FIXED, and the caller mapped that single
  code to "not blocking". So a resolver whose own controls had failed reported
  warned-but-green -- a check that cannot tell you it is broken, which is the
  exact defect this gate exists to find. A distinct code, returned before the
  excuser, is the only form that survives a caller written to fail open.
"""

from __future__ import annotations

import os
import pathlib
import re
import subprocess
import sys

REPO = pathlib.Path(__file__).resolve().parents[2]
# Overridable so the gate's own arms can be exercised against a scratch
# allowlist without editing the committed one. A check whose failure modes can
# only be tested by damaging the real file does not get tested.
ALLOWLIST = pathlib.Path(
    os.environ.get("MOD_REACHABILITY_ALLOWLIST", REPO / "scripts" / "mod-reachability-allowlist.txt")
)
# THE POPULATION, DECLARED BEFORE IT IS MEASURED (bd-hvlm2 follow-up).
#
# This gate answers ONE predicate: is a tracked .rs file reachable from a cargo
# target root, as reported by `cargo fmt --verbose`. Everything below is scoped
# to that predicate. A gate that reports on some surfaces and is silent about
# the rest does not read as partial, it reads as clean -- which is why the
# denominator is written down here rather than left implicit in the tuple.
#
# TRACKED RUST SURFACES IN THE ROOT WORKSPACE -- 4, this gate's whole reach:
#     src/       444 files   IN SCOPE
#     tests/     771 files   IN SCOPE
#     benches/    40 files   IN SCOPE as of this change
#     build.rs     1 file    IN SCOPE as of this change
#
# TRACKED RUST SURFACES THIS GATE STRUCTURALLY CANNOT SEE -- 2:
#     fuzz/                27 files, its own Cargo.toml, `exclude`d from the
#                          root workspace, so this gate's single cargo
#                          invocation never walks it.
#     crates/determinism/   1 file, likewise a separate manifest.
#   Covering these needs a cargo invocation PER MANIFEST, not a wider tuple.
#   Tracked as bd-ik4wg. That bead records what is MEASURED (28 tracked .rs that
#   no root-workspace target root reaches) and what is NOT (whether any of them
#   is unreachable inside its own manifest -- nobody has run that pass). Do not
#   file a second bead off this comment; the first draft of it said "no bead
#   exists", which stopped being true minutes later and nearly caused exactly
#   that duplicate.
#
# NOT A SURFACE FOR THIS PREDICATE AT ALL:
#     scripts/**.sh -- shell scripts have no module graph. Their reachability
#     is "named by a gate root", a different predicate measured separately by
#     bd-unreachable-e2e-scripts-u14sr. Adding them here would be a category
#     error, not a widening.
#
# WHAT WIDENING TO benches/ DOES AND DOES NOT FIX. It does NOT discharge
# bd-unreachable-bench-tests-k0le8. That bead is about 74 `#[test]` fns under
# benches/ that never EXECUTE because every [[bench]] declares
# `harness = false`, so libtest never runs them. Those files are perfectly
# reachable as modules; compilation and execution are different predicates and
# this gate only answers the first. Expecting this change to clear k0le8 would
# be the same mistake as reading a green here as "everything is covered".
IN_SCOPE = ("src/", "tests/", "benches/", "build.rs")

# Controls, from observed cargo behaviour. See the module docstring.
#
# PER-SURFACE CONTROLS. A surface added without its own control is
# unfalsifiable: an EMPTY scan over it passes exactly like a clean one, and
# this gate cannot tell the difference from the outside.
CONTROL_REACHABLE = "tests/contracts/ask_native.rs"
CONTROL_UNREACHABLE = "src/core/preflight_token.rs"
# benches/: the positive arm is a real declared target.
CONTROL_REACHABLE_BENCH = "benches/remember.rs"
# benches/: THE NEGATIVE ARM IS UNREPRESENTABLE TODAY, and is recorded as
# absent rather than quietly skipped. All 40 benches/*.rs are themselves
# declared [[bench]] targets (by `name`, via autodiscovery) and benches/ holds
# ZERO nested .rs, so no tracked-but-unreachable bench file exists to name. The
# invariant that stands in for it is asserted at runtime below: tracked
# benches/ files must equal benches/ roots cargo walks. The day someone adds
# benches/helpers/foo.rs declared by nothing, that equality breaks and the main
# check fires -- which is the negative arm arriving the moment it is possible.
CONTROL_UNREACHABLE_BENCH = None

PATH_MOD = re.compile(
    r'#\[path\s*=\s*"([^"]+)"\]\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+(\w+)\s*;', re.S
)
PLAIN_MOD = re.compile(r"^[ \t]*(?:pub(?:\([^)]*\))?\s+)?mod\s+(\w+)\s*;", re.M)


def target_roots() -> list[pathlib.Path] | None:
    """Every target root cargo walks, straight from cargo itself."""
    try:
        proc = subprocess.run(
            ["cargo", "fmt", "--verbose", "--check"],
            cwd=REPO,
            capture_output=True,
            text=True,
            timeout=180,
        )
    except (FileNotFoundError, subprocess.TimeoutExpired):
        return None
    roots = [pathlib.Path(m) for m in re.findall(r'"([^"]+\.rs)"', proc.stdout)]
    return [r for r in roots if r.is_file()] or None


CFG_ATTR = re.compile(r"^\s*#\[cfg\((?P<expr>.+)\)\]\s*$")
PATH_ATTR = re.compile(r'^\s*#\[path\s*=\s*"(?P<target>[^"]+)"\]\s*$')
OTHER_ATTR = re.compile(r"^\s*#\[")
MOD_LINE = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+(?P<name>\w+)\s*;")
# bd-l6h3g. `include!("x.rs")` textually inlines a file, so rustc DOES compile
# it -- but rustfmt does NOT follow `include!`, only `mod`/`#[path]`. A file
# reached this way is therefore COMPILED AND UNFORMATTED, which is half of what
# this gate looks for, not none of it. Following it here stops the gate calling
# such a file "compiled by nothing", which is simply false; the formatter half
# is reported separately by include_only_paths() so the fact is not lost.
INCLUDE_LINE = re.compile(r'^\s*include!\s*\(\s*"(?P<target>[^"]+)"\s*\)\s*;')
# Files reached ONLY via include!: compiled, but never seen by `cargo fmt`.
INCLUDE_ONLY: set[pathlib.Path] = set()


def children(path: pathlib.Path) -> list[tuple[pathlib.Path, str | None]]:
    """(child, cfg_expression_or_None) for every module this file declares.

    bd-waksx. The cfg matters. `src/core/index.rs:61` reads

        #[cfg(unix)]
        #[path = "index_read_lease.rs"]
        mod read_lease;

    so that file is reachable on unix and NOT reachable on a windows target.
    A resolver that follows the `#[path]` and ignores the `#[cfg]` above it
    answers for one platform and reports a number that reads as universal.
    This repo ships six platforms through the cross-compile flow, so that is
    not a hypothetical.

    Scanning line by line rather than with a multi-line regex, because the cfg
    attribute, the path attribute and the mod line are three separate lines and
    only their ADJACENCY binds them.
    """
    try:
        lines = path.read_text(errors="replace").splitlines()
    except OSError:
        return []
    here = path.parent
    found: list[tuple[pathlib.Path, str | None]] = []
    pending_cfg: str | None = None
    pending_path: str | None = None

    for raw in lines:
        cfg = CFG_ATTR.match(raw)
        if cfg:
            # Nested cfgs on one declaration are rare; join rather than drop one.
            pending_cfg = (
                cfg.group("expr")
                if pending_cfg is None
                else f"{pending_cfg} + {cfg.group('expr')}"
            )
            continue
        redirect = PATH_ATTR.match(raw)
        if redirect:
            pending_path = redirect.group("target")
            continue
        included = INCLUDE_LINE.match(raw)
        if included:
            candidate = (here / included.group("target")).resolve()
            if candidate.is_file():
                found.append((candidate, pending_cfg))
                INCLUDE_ONLY.add(candidate)
            pending_cfg = None
            pending_path = None
            continue
        declaration = MOD_LINE.match(raw)
        if declaration:
            if pending_path is not None:
                candidate = (here / pending_path).resolve()
                if candidate.is_file():
                    found.append((candidate, pending_cfg))
            else:
                name = declaration.group("name")
                for candidate in (here / f"{name}.rs", here / name / "mod.rs"):
                    if candidate.is_file():
                        found.append((candidate.resolve(), pending_cfg))
                        break
            pending_cfg = None
            pending_path = None
            continue
        if OTHER_ATTR.match(raw) or not raw.strip():
            # Another attribute or a blank line does not break the run of
            # attributes attached to the declaration below.
            continue
        # Any other code ends the attribute run.
        pending_cfg = None
        pending_path = None
    return found


def reachable_set(
    roots: list[pathlib.Path],
) -> tuple[set[pathlib.Path], dict[pathlib.Path, set[str]]]:
    """(reachable under this host's cfg, {file: cfgs} for cfg-ONLY reachability).

    A file reached by at least one unconditional chain is unconditionally
    reachable and is absent from the second map. A file every chain to which
    passes through a cfg is CONDITIONALLY reachable, and the gates are recorded
    so the report can name them.
    """
    unconditional: set[pathlib.Path] = {r.resolve() for r in roots}
    conditional: dict[pathlib.Path, set[str]] = {}
    queue: list[tuple[pathlib.Path, bool, tuple[str, ...]]] = [
        (r, False, ()) for r in unconditional
    ]

    while queue:
        node, node_is_conditional, gates = queue.pop()
        for child, cfg in children(node):
            child_gates = gates + ((cfg,) if cfg else ())
            child_is_conditional = node_is_conditional or cfg is not None
            if not child_is_conditional:
                if child in unconditional:
                    continue
                unconditional.add(child)
                conditional.pop(child, None)
                queue.append((child, False, child_gates))
            else:
                if child in unconditional:
                    continue
                known = conditional.get(child)
                if known is not None and set(child_gates) <= known:
                    continue
                conditional.setdefault(child, set()).update(child_gates)
                queue.append((child, True, child_gates))

    return unconditional | set(conditional), conditional


def host_cfg_label() -> str:
    """The cfg set this evaluation actually answers for, named in the output."""
    import platform

    system = platform.system().lower()
    family = "unix" if system in {"darwin", "linux", "freebsd"} else system
    return f"target_family={family}, target_os={system}"


def tracked_in_scope() -> list[str]:
    """Population = files GIT TRACKS. An UNTRACKED .rs is invisible to this gate.

    bd-l6h3g. Found by planting a probe: an undeclared file that had not been
    `git add`ed did NOT trip the gate, and every self-test arm still passed.
    This is correct in CI, where the checkout contains only committed files, and
    it is a real blind spot locally -- a brand-new file is unchecked until it is
    staged. Do not "fix" it by globbing the filesystem: that would pull in
    target/, scratch files and editor droppings, and the resulting noise is what
    makes a gate get switched off. Stage the file, then run the gate.
    """
    proc = subprocess.run(
        ["git", "ls-files", "*.rs"], cwd=REPO, capture_output=True, text=True
    )
    return sorted(
        line
        for line in proc.stdout.split()
        if line.startswith(IN_SCOPE)
    )


SECTION2_MARK = "# @SECTION-2-BEGIN"
SECTION2_BUDGET = "# @SECTION-2-BUDGET:"


def read_allowlist() -> tuple[list[tuple[str, str, bool]], int | None]:
    """((path, reason, in_section_2) triples, declared section-2 budget).

    The budget is what makes "MAY ONLY SHRINK" mechanical instead of hortatory.
    A prose promise in a comment is not a ratchet; a declared count that must be
    edited in the same commit as the entry is.
    """
    if not ALLOWLIST.is_file():
        return [], None
    entries: list[tuple[str, str, bool]] = []
    budget: int | None = None
    in_section_2 = False
    for raw in ALLOWLIST.read_text().splitlines():
        line = raw.strip()
        if line.startswith(SECTION2_BUDGET):
            try:
                budget = int(line[len(SECTION2_BUDGET):].strip())
            except ValueError:
                budget = None
            continue
        if line.startswith(SECTION2_MARK):
            in_section_2 = True
            continue
        if not line or line.startswith("#"):
            continue
        path, _, reason = line.partition("\t")
        entries.append((path.strip(), reason.strip(), in_section_2))
    return entries, budget


def main() -> int:
    roots = target_roots()
    if roots is None:
        print("[mod-reachability] cargo unavailable or timed out — inconclusive, not blocking", file=sys.stderr)
        return 2

    reachable, cfg_only = reachable_set(roots)
    tracked = tracked_in_scope()

    def is_reachable(rel: str) -> bool:
        return (REPO / rel).resolve() in reachable

    # --- controls, before any verdict -------------------------------------
    if not is_reachable(CONTROL_REACHABLE):
        print(
            f"[mod-reachability] CONTROL FAILED: {CONTROL_REACHABLE} should be reachable "
            "(cargo fmt reports it) but the resolver says otherwise — refusing to emit",
            file=sys.stderr,
        )
        return 3
    if is_reachable(CONTROL_UNREACHABLE):
        print(
            f"[mod-reachability] CONTROL FAILED: {CONTROL_UNREACHABLE} should be unreachable "
            "(0f68778a0 removed its `pub mod` line) but the resolver says otherwise — refusing to emit",
            file=sys.stderr,
        )
        return 3
    # benches/ positive arm. Without it, widening IN_SCOPE to benches/ would be
    # indistinguishable from a tuple entry that matches nothing at all.
    if not is_reachable(CONTROL_REACHABLE_BENCH):
        print(
            f"[mod-reachability] CONTROL FAILED: {CONTROL_REACHABLE_BENCH} should be reachable "
            "(it is a declared [[bench]] target) but the resolver says otherwise — refusing to emit",
            file=sys.stderr,
        )
        return 3
    # benches/ negative arm, as an invariant because no fixture can express it
    # (see CONTROL_UNREACHABLE_BENCH). Every tracked benches/*.rs must be a root
    # cargo walks. This is not decoration: it is the assertion that would have
    # caught a nested, undeclared bench file, and it fails loudly if one appears
    # while the allowlist says nothing about it.
    bench_tracked = {t for t in tracked if t.startswith("benches/")}
    bench_roots = {
        str(r.relative_to(REPO))
        for r in roots
        if str(r.relative_to(REPO)).startswith("benches/")
    }
    bench_orphans = sorted(bench_tracked - bench_roots)
    if bench_orphans and CONTROL_UNREACHABLE_BENCH is None:
        print(
            "[mod-reachability] benches/ now contains tracked .rs that are not target "
            f"roots: {', '.join(bench_orphans)}. The negative control for this surface "
            "was recorded as unrepresentable because no such file existed; one exists "
            "now, so name it as CONTROL_UNREACHABLE_BENCH and let the main check judge it.",
            file=sys.stderr,
        )

    allow, budget = read_allowlist()
    allow_paths = {p for p, _, _ in allow}
    section2 = [p for p, _, s2 in allow if s2]
    findings = 0

    # --- 0. the section-2 ratchet, in BOTH directions ----------------------
    # Growing is new undeclared-code debt. Shrinking without lowering the
    # declared budget leaves an allowance nobody is using, which is how a
    # ratchet stops ratcheting -- the same both-directions argument made for the
    # anchor budget in bd-d8trk.
    if budget is None:
        findings += 1
        print(
            f"ALLOWLIST IS MISSING ITS `{SECTION2_BUDGET} <n>` DIRECTIVE.\n"
            "  Without it, section 2 'may only shrink' is a comment and nothing enforces it."
        )
    elif len(section2) != budget:
        findings += 1
        direction = "GREW" if len(section2) > budget else "SHRANK"
        print(f"SECTION-2 RATCHET: declared budget {budget}, actual {len(section2)} — it {direction}.")
        for p in sorted(section2):
            print(f"    {p}")
        if len(section2) > budget:
            print(
                "  A new entry in section 2 is new undeclared-code debt. Declare the module\n"
                "  or fix the file; do NOT raise the budget to make this pass."
            )
        else:
            print(
                f"  Good news, but finish it: lower `{SECTION2_BUDGET} {len(section2)}` in this\n"
                "  same commit, or the freed allowance silently absorbs the next new file."
            )

    # --- 1. unreachable and not allowlisted -------------------------------
    unreachable = [f for f in tracked if not is_reachable(f)]
    new_unreachable = [f for f in unreachable if f not in allow_paths]
    if new_unreachable:
        findings += len(new_unreachable)
        print("UNREACHABLE FROM ANY CARGO TARGET, and not on the allowlist:")
        for f in new_unreachable:
            print(f"    {f}")
        print(
            "  Nothing compiles or formats these. Either declare the module "
            "(`mod x;` or `#[path = \"...\"] mod x;` from a reachable parent), "
            "or add an allowlist entry WITH A REASON if the file is deliberate "
            "test data."
        )

    # --- 1b. compiled via include!, therefore NEVER FORMATTED --------------
    # Not a finding: rustc does compile these, so the "compiled by nothing"
    # verdict would be false. But `cargo fmt --check` cannot reach them, so a
    # green Format step says nothing about them. Printed by name every run so
    # the gap stays visible instead of being silently absorbed by the fact that
    # the file is reachable.
    include_only = sorted(
        str(p.relative_to(REPO)) for p in INCLUDE_ONLY if p.is_file()
    )
    if include_only:
        print(
            "COMPILED VIA include!, BUT INVISIBLE TO `cargo fmt` "
            "(rustfmt follows mod/#[path], never include!):"
        )
        for p in include_only:
            print(f"    {p}")
        print(
            "  These DO compile, so they are not unreachable-code debt and do not "
            "fail this gate. They are unformatted-code debt: the Format step is "
            "green over a population that excludes them."
        )

    # --- 2. allowlist rot: entry names a file that is gone -----------------
    missing = [p for p in allow_paths if not (REPO / p).is_file()]
    if missing:
        findings += len(missing)
        print("ALLOWLIST ENTRIES NAMING FILES THAT NO LONGER EXIST:")
        for p in sorted(missing):
            print(f"    {p}")
        print("  Remove them. An allowlist that outlives its files decays into fiction.")

    # --- 3. allowlist rot: entry is now reachable and unnecessary ----------
    now_reachable = [p for p in allow_paths if (REPO / p).is_file() and is_reachable(p)]
    if now_reachable:
        findings += len(now_reachable)
        print("ALLOWLIST ENTRIES THAT ARE NOW REACHABLE (the exemption is obsolete):")
        for p in sorted(now_reachable):
            print(f"    {p}")
        print("  Remove them, so the allowlist only ever names real exemptions.")

    # --- 4. an entry with no reason is not an exemption, it is a hole ------
    unreasoned = [p for p, reason, _ in allow if not reason]
    if unreasoned:
        findings += len(unreasoned)
        print("ALLOWLIST ENTRIES WITH NO REASON:")
        for p in sorted(unreasoned):
            print(f"    {p}")
        print("  Every exemption states why, or it cannot be reviewed.")

    if findings:
        print(f"\n[mod-reachability] {findings} finding(s) above. Each is printed by name.")
        return 1

    # Print the population, not just a verdict: a green whose denominator is
    # invisible is the failure this gate exists to remove.
    # bd-waksx: the count MUST carry its scope. A bare "0 unaccounted" reads as
    # universal and is not -- it answers for the cfg set this host evaluates.
    # The scope is put inside the same sentence as the number precisely so the
    # number cannot be quoted without it.
    print(
        f"[mod-reachability] {len(tracked)} tracked .rs under {', '.join(IN_SCOPE)}: "
        f"{len(tracked) - len(unreachable)} reachable, {len(unreachable)} allowlisted, "
        f"0 unaccounted UNDER cfg({host_cfg_label()}) -- this is a per-target "
        f"answer, not a universal one."
    )
    # PER SURFACE, because a total hides a zero. A surface that matched nothing
    # -- a renamed directory, a tuple entry that never applies on this host --
    # contributes 0 to every column and is indistinguishable from a clean one
    # inside an aggregate. Printing each surface on its own line makes an empty
    # scan visible as an empty scan. This is the same reason the count above
    # carries its cfg scope (bd-waksx): a number whose population is invisible
    # cannot be audited.
    for surface in IN_SCOPE:
        s_tracked = [t for t in tracked if t.startswith(surface)]
        s_unreachable = [u for u in unreachable if str(u).startswith(surface)]
        note = "  <-- EMPTY SCAN: this surface matched no tracked file" if not s_tracked else ""
        print(
            f"[mod-reachability]   {surface:<10} {len(s_tracked):>4} tracked, "
            f"{len(s_tracked) - len(s_unreachable):>4} reachable, "
            f"{len(s_unreachable):>3} allowlisted{note}"
        )
    print(
        "[mod-reachability]   NOT COVERED by this gate: fuzz/ (27) and "
        "crates/determinism/ (1) are separate manifests excluded from the root "
        "workspace, so one cargo invocation cannot reach them; scripts/**.sh "
        "have no module graph and are a different predicate (bd-unreachable-"
        "e2e-scripts-u14sr). 4 of 6 tracked Rust surfaces are in scope here."
    )
    in_scope_cfg_only = sorted(
        (path, gates)
        for path, gates in cfg_only.items()
        if str(path.relative_to(REPO)).startswith(IN_SCOPE)
    )
    if in_scope_cfg_only:
        print(
            f"  {len(in_scope_cfg_only)} of those are reached ONLY through a cfg-gated "
            "declaration, so another target may not compile them at all:"
        )
        for path, gates in in_scope_cfg_only:
            print(f"    {path.relative_to(REPO)}   via #[cfg({' , '.join(sorted(gates))})]")
        print(
            "  These are NOT findings here -- they are reachable on this host. They are "
            "the part of the answer that does not generalise, named so nobody quotes the "
            "zero as if it did."
        )
    else:
        print("  No file depends on a cfg gate to be reachable, so the zero does generalise.")
    return 0


def self_test() -> int:
    """Plant files whose correct classification is known, and assert it.

    bd-l6h3g. A reachability gate reports a ZERO on a healthy tree, and a zero
    is exactly what a silently broken resolver also reports. The controls in
    main() pin two real files, which dies the moment either is declared or
    retired. This plants its own inputs instead, so the arms cannot rot, and it
    includes the cases that have actually produced wrong answers here:
    a declaration inside a comment (a `cargo test` living in a comment is how
    bd-p54ks's guard first miscounted), a `#[path]` redirect, an `include!`,
    and a `mod` naming a file that does not exist.
    """
    import tempfile

    failures: list[str] = []

    def arm(name: str, actual: object, expected: object) -> None:
        ok = actual == expected
        print(f"  [{'ok  ' if ok else 'FAIL'}] {name}")
        if not ok:
            failures.append(f"{name}: expected {expected!r}, got {actual!r}")

    with tempfile.TemporaryDirectory() as raw:
        root = pathlib.Path(raw)
        for leaf in ("plain.rs", "redirected_file.rs", "included.rs", "cfgd.rs"):
            (root / leaf).write_text("// planted\n")
        parent = root / "parent.rs"
        parent.write_text(
            "//! planted parent\n"
            "mod plain;\n"
            '#[path = "redirected_file.rs"]\n'
            "mod redirected;\n"
            "#[cfg(test)]\n"
            "mod cfgd;\n"
            "mod tests {\n"
            '    include!("included.rs");\n'
            "}\n"
            "// mod commented_out;\n"
            '// include!("commented_include.rs");\n'
            "mod does_not_exist;\n"
        )
        INCLUDE_ONLY.clear()
        found = children(parent)
        names = sorted(p.name for p, _ in found)

        arm(
            "plain `mod x;`, `#[path]`, cfg-gated and include! are all followed",
            names,
            ["cfgd.rs", "included.rs", "plain.rs", "redirected_file.rs"],
        )
        arm(
            "a declaration inside a comment is NOT followed",
            [n for n in names if "commented" in n],
            [],
        )
        arm(
            "a `mod` naming a file that does not exist yields no child",
            [n for n in names if "does_not_exist" in n],
            [],
        )
        arm(
            "the cfg on a gated declaration is captured, not dropped",
            sorted(c for _, c in found if c),
            ["test"],
        )
        arm(
            "an include!-only file is recorded as compiled-but-unformatted",
            sorted(p.name for p in INCLUDE_ONLY),
            ["included.rs"],
        )

        # The gate must be able to FAIL. An allowlist entry with no reason is
        # the cheapest provable failure path that needs no cargo.
        bad = root / "allow.txt"
        bad.write_text("some/unreachable.rs\n")
        parsed = [
            line.split("\t", 1)
            for line in bad.read_text().splitlines()
            if line.strip() and not line.startswith("#")
        ]
        arm(
            "an allowlist line with no TAB reason parses as reasonless (a finding)",
            [len(p) for p in parsed],
            [1],
        )

    INCLUDE_ONLY.clear()
    if failures:
        print(f"\n[mod-reachability] self-test: {len(failures)} arm(s) FAILED:")
        for f in failures:
            print(f"    {f}")
        return 1
    print("\n[mod-reachability] self-test: 6/6 arms passed.")
    return 0


if __name__ == "__main__":
    if "--self-test" in sys.argv[1:]:
        sys.exit(self_test())
    sys.exit(main())
