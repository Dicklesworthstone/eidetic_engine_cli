#!/usr/bin/env python3
"""Refuse an e2e script that is NAMED by a root but EXECUTED by nothing.

bd-e2d1q. An e2e script can be "covered" by a Rust test that reads its SOURCE
and asserts on its TEXT, and never runs it. That test is not useless -- a text
scan is the right tool for a cross-cutting logging contract -- but it OCCUPIES
THE SLOT a runner would occupy and reports green either way:

  * `scripts/e2e_invocation_audit.sh` asks "is this script REFERENCED", so a
    const table of script paths in a test counts it wired and it never appears
    in orphan_baseline.txt;
  * a reader who greps a script's name finds `tests/..._e2e_contract.rs` and
    reasonably concludes it is covered -- the filename says e2e_contract;
  * if the script's behaviour regresses, every one of those tests still passes.

So this gate answers the OTHER question. For every tracked e2e script it asks
whether anything EXECUTES it, and requires each script that nothing executes to
be declared, with a reason, in

    tests/fixtures/e2e_invocation/text_conformance_only.txt

That registry is not a softer ignore-list. It is RE-EARNED on every run, the
way the invocation audit's retired.txt is: a declared script must still be
unexecuted (so the file can only shrink), its named covering test must exist
and must really reference it, and it must not also sit in orphan_baseline.txt.

WHY A PYTHON GATE. The three rails that could host this all refuse on this
host: verify.sh exits before stage one (`ee_require_current_binary`, 0.14.2
against a 0.18.0 tree), hosted Actions is off repo-wide, and a dispatch costs a
pinned build. `scripts/check-*.py` needs no toolchain, no binary and no lane,
and the documented family glob picks this file up with no wiring at all.

## THE PREDICATES, AND WHY THEY ARE SHAPED THIS WAY

EXECUTION IN RUST IS A PROCESS API AND NOTHING ELSE. bd-e2d1q records an
earlier detector that included the bare words `bash` and `source`, which
matched

    assert!(first.contains("bash"), "doctor_undo_replay.sh shebang must ...")

-- the stem sits in the assertion MESSAGE and "bash" is the string being
asserted ABOUT. Two of eighteen scripts were reported fixed when nothing had
changed. Shell verbs in a .rs file are string data.

AND THE LITERALS MUST BE MASKED, not merely have comments stripped. Measured
2026-11-09: `Command::new` appears INSIDE string literals 20 times across
tests/unsafe_claim_planner_conformance.rs, tests/ee_spawn_isolation_contract.rs
and tests/contracts/spawn_deadline_coverage.rs -- the last of which is a gate
whose fixtures are Rust source as text. A predicate run over raw text credits
all three files with executing whatever they name. This gate reads script paths
out of the literal SIDE TABLE and looks for process APIs only in the masked
text, so neither can be confused for the other. See scripts/lib/rust_lex.py.

THE UNIT IS THE CARRIER, NOT THE FILE. bd-e2d1q's own census was explicit that
its EXECUTES/INSPECTS split was a FILE-level proxy -- "a file that spawns
something else while only reading this script is miscounted as EXECUTES, so 18
is a FLOOR" -- and a single test file genuinely does both. So a reference is
graded by its enclosing item (`fn`, `const` or `static`), and a `const` table
is resolved one step further to the functions that CONSUME the const, because
the dominant idiom puts the path and the execution site far apart:

    const GRAPH_E2E_SCRIPTS: &[(&str, &str)] = &[("scripts/.../g1_ppr.sh", ...)];
    ...
    let contents = read_script(path);        // <- 40 lines away, another fn

THE REFERENCE SOURCES ARE A TRACKED SET, which excludes four vendored full-tree
copies for free. bd-e2d1q's instrument once counted a September snapshot of the
whole repository alongside HEAD -- its positive control fired on a line inside
the snapshot -- and the bead names ONE directory to exclude. There are FOUR
(.ntm/fmt2/tree, .ntm/fmt3/tree, .ntm/swarm/evidence/rc-20260924/tree and
beads_compliance_audit/passes/...), every one of them UNTRACKED, so enumerating
from `git ls-files` excludes all of them and any future one, with no list to
maintain. Measured 2026-11-09.

## ONE LIMIT WORTH STATING, BECAUSE IT IS NOT VISIBLE IN THE OUTPUT

"Executed" here means some root runs the script. It does NOT ask whether that
root is itself run. scripts/e2e_overhaul/swarm_scale.sh, for instance, is
executed only by scripts/e2e_overhaul/failure_modes.sh:2448 -- which is in
turn registered in scripts/e2e_overhaul.sh:74, so the chain does reach a
driver, but this gate did not check that and would have credited it either
way. The same limit applies to `scripts/e2e_invocation_audit.sh`, whose
question is one hop deep as well. A script whose only executor is itself
unexecuted is a real hole and neither gate sees it.

## EXIT CODES

  0  every unexecuted script is declared, and every declaration is still earned
  1  RED -- a new unexecuted script, or a stale/unearned declaration
  2  usage
  3  CANNOT ANSWER -- a predicate is blind, so no verdict exists. Never a pass.

Exit 3 carries real weight here. Both predicates have a POSITIVE CONTROL built
into the run: if the Rust predicate finds no executed script anywhere, or the
shell predicate finds none, it is blind, and a blind predicate would report the
entire population as unexecuted while looking exactly like a confirmation.

`--self-test` drives every verdict path and prints its own arm count.
"""

from __future__ import annotations

import argparse
import pathlib
import re
import subprocess
import sys
import tempfile

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent / "lib"))

from rust_lex import SENTINEL_RE, lex  # noqa: E402  (path insert precedes it)

EXIT_OK = 0
EXIT_RED = 1
EXIT_USAGE = 2
EXIT_CANNOT = 3

REGISTRY_REL = "tests/fixtures/e2e_invocation/text_conformance_only.txt"
ORPHAN_BASELINE_REL = "tests/fixtures/e2e_invocation/orphan_baseline.txt"
RETIRED_REL = "tests/fixtures/e2e_invocation/retired.txt"


class CannotAnswer(Exception):
    """A scope assumption failed. Exit 3, never 0 -- an unanswerable check is
    not a passing check."""


# --------------------------------------------------------------------------
# Population -- mirrors e2e_invocation_audit.sh's e2e_audit_paths()
# --------------------------------------------------------------------------
# Two directories, not one (the audit's bd-1cn5o lesson): scripts/e2e_*.sh is
# 93 files while scripts/e2e_overhaul/ holds another ~115 that an earlier glob
# never looked at. Kept deliberately in step with that function so the two
# gates cannot disagree about what the population IS; if it changes there,
# this must change too, and the self-test asserts the shapes it expects.
POPULATION_RES = (
    re.compile(r"scripts/e2e_[^/]*\.sh\Z"),
    re.compile(r"scripts/e2e_overhaul/(?:lib/)?[^/]*\.sh\Z"),
)

# The audit excludes itself -- a tool that matches its own glob.
POPULATION_SKIP = ("scripts/e2e_invocation_audit.sh",)


def git_tracked(root: pathlib.Path) -> list[str]:
    try:
        done = subprocess.run(
            ["git", "ls-files", "-z"],
            cwd=root,
            capture_output=True,
            text=True,
            check=True,
        )
    except (OSError, subprocess.CalledProcessError) as exc:
        raise CannotAnswer(f"git ls-files failed in {root}: {exc}") from exc
    return [p for p in done.stdout.split("\0") if p]


def population(tracked: list[str]) -> list[str]:
    found = {
        p
        for p in tracked
        if any(rx.fullmatch(p) for rx in POPULATION_RES) and p not in POPULATION_SKIP
    }
    return sorted(found)


# --------------------------------------------------------------------------
# Which tracked files are allowed to count as a ROOT
# --------------------------------------------------------------------------
# Every exclusion is a thing that CANNOT execute a script, and most of them
# were learned by the invocation audit the hard way.
#
#   *.md, docs/**        Markdown cannot invoke anything. Twelve scripts once
#                        passed the audit on prose alone (its bd-q2nq9).
#   tests/fixtures/**    Fixture data cannot invoke anything, and the audit
#                        already plants a negative for a script named only in
#                        a fixtures JSON.
#   .beads/**            THE TRACKER. A bead description naming a script is
#                        filing, not wiring -- and it is load-bearing here:
#                        .beads/issues.jsonl references all nineteen of the
#                        currently-undeclared scripts, so without this
#                        exclusion every one of them looks referenced by a
#                        root. Same class as the docs exclusion.
#   scripts/check-*.py   Gates that READ scripts in order to lint them. The
#   scripts/audit-*.py   invocation audit excludes both globs for this reason;
#                        scripts/audit-set-e-dead-handlers.py reads
#                        doctor_undo_replay.sh and executes nothing.
#   the registry files   Bookkeeping that NAMES scripts. Without this a
#                        declared script is "referenced" by its own
#                        declaration -- the audit records this exact trap
#                        making a gate vacuous through its own baseline.
#   .gitignore etc.      Cannot invoke anything.
#   the script itself    A script naming itself is not a root for itself.
EXCLUDED_SOURCE_RES = (
    re.compile(r".*\.md\Z"),
    re.compile(r"docs/.*"),
    re.compile(r"tests/fixtures/.*"),
    re.compile(r"\.beads/.*"),
    re.compile(r"scripts/check-[^/]*\.py\Z"),
    re.compile(r"scripts/audit-[^/]*\.py\Z"),
    re.compile(r"\.gitignore\Z"),
    re.compile(r"\.gitattributes\Z"),
    # HOSTED ACTIONS ARE DISABLED REPO-WIDE (bd-actions-disabled-repo-wide-33mfm),
    # so a workflow naming a script does not run it -- it is a mention with a
    # YAML shape. Counting it would reproduce the exact hole CLAUDE.md
    # documents: .github/workflows/ci-static.yml invokes 21 scripts and eight
    # of them appear nowhere in verify.sh, so they execute NOWHERE while
    # looking wired. Measured 2026-11-09: ZERO e2e scripts draw their only
    # executing root from .github/, so this exclusion changes no verdict today
    # and exists to keep a future one honest. Excluding can only move a script
    # from EXEC to a finding, which is the loud direction.
    re.compile(r"\.github/.*"),
)


def eligible_sources(tracked: list[str]) -> list[str]:
    return [p for p in tracked if not any(rx.fullmatch(p) for rx in EXCLUDED_SOURCE_RES)]


# --------------------------------------------------------------------------
# Rust: a process API, and nothing that merely looks like one
# --------------------------------------------------------------------------
# `.output(` and `.status(` are included because the idiom in this repo is
# `Command::new(x).arg(y).output()`, and a helper may return a configured
# `Command` that a caller then drives -- so the spawn verb and `Command::new`
# are not always in the same item.
EXEC_RE = re.compile(
    r"(?:std::process::)?Command::(?:new|from)\b"
    r"|\.spawn\s*\("
    r"|\.output\s*\("
    r"|\.status\s*\("
    r"|\bassert_cmd\b"
    r"|\bescargot\b"
)

# Declarations that can CARRY a path literal. `static` is included for
# completeness; the dominant shape in this repo is `const`.
ITEM_RE = re.compile(
    r"^[ \t]*(?:pub(?:\([^)]*\))?[ \t]+)?"
    r"(?:(?:async[ \t]+)?(?:unsafe[ \t]+)?(?:extern[ \t]+\"[^\"]*\"[ \t]+)?fn|const|static)"
    r"[ \t]+(?:mut[ \t]+)?([A-Za-z_]\w*)"
)


IDENT_RE = re.compile(r"[A-Za-z_]\w*")


def rust_items(stripped: str) -> list[tuple[int, str, str]]:
    """(line, kind, name) for every fn/const/static declaration, in order."""
    items: list[tuple[int, str, str]] = []
    for lineno, line in enumerate(stripped.splitlines(), start=1):
        match = ITEM_RE.match(line)
        if not match:
            continue
        head = line[: match.start(1)]
        kind = "fn" if re.search(r"\bfn\b", head) else (
            "const" if re.search(r"\bconst\b", head) else "static"
        )
        items.append((lineno, kind, match.group(1)))
    return items


def carrier_of(items: list[tuple[int, str, str]], lineno: int):
    """The nearest declaration at or above `lineno`, or None above the first."""
    found = None
    for item in items:
        if item[0] <= lineno:
            found = item
        else:
            break
    return found


def _line_starts(text: str) -> list[int]:
    starts = [0]
    for match in re.finditer(r"\n", text):
        starts.append(match.end())
    return starts


def _line_of(starts: list[int], index: int) -> int:
    low, high = 0, len(starts) - 1
    while low < high:
        mid = (low + high + 1) // 2
        if starts[mid] <= index:
            low = mid
        else:
            high = mid - 1
    return low + 1


def classify_rust(text: str, needles: dict[str, re.Pattern[str]]) -> dict[str, list[dict]]:
    """Grade every script reference in one Rust file.

    Returns script -> list of {line, verdict, carrier, why}.
    """
    stripped, literals = lex(text)
    starts = _line_starts(stripped)
    items = rust_items(stripped)

    exec_lines = {_line_of(starts, m.start()) for m in EXEC_RE.finditer(stripped)}
    exec_carriers = {carrier_of(items, n) for n in exec_lines}
    exec_carriers.discard(None)

    # Items that CONSUME the carrier, so a path sitting in a table OR returned
    # by a helper can be followed to the code that uses it. Keyed by the
    # consumed name, valued by the consumer that spawns, so the verdict can
    # name its own evidence.
    #
    # THE CARRIER MAY BE A `fn`, NOT ONLY A `const`, and an earlier version of
    # this gate resolved only const/static. That produced FOUR false findings
    # on the real tree in one run, all of this exact shape:
    #
    #     fn e2e_script() -> PathBuf {
    #         repo_root().join("scripts").join("e2e_overhaul")
    #             .join("rch_runbook_docs_smoke.sh")      // <- the carrier
    #     }
    #     #[test]
    #     fn shell_e2e_driver_emits_phase_logs_and_final_summary() {
    #         let output = Command::new("bash").arg(e2e_script())...   // <- runs it
    #     }
    #
    # which is `feedback_helpers_hide_the_population` applied to a path instead
    # of a call: keying on the enclosing item misses a single-purpose helper
    # whose whole job is to hand the path to its caller. So the rule is uniform
    # over kinds: a carrier is executed when an item that NAMES it spawns.
    #
    # DELIBERATELY NOT TRANSITIVE. A two-hop chain (helper -> helper -> spawn)
    # reads as NAMED_ONLY here, which is the LOUD direction: it demands a
    # registry line rather than silently crediting execution. Widening it adds
    # false EXEC, and a false EXEC is invisible -- it is the exact failure this
    # gate exists to prevent. No two-hop case exists in the tree today.
    # Tokenise each body ONCE and test set membership, rather than running a
    # boundary-anchored regex per (carrier, name) pair. The two are equivalent
    # -- IDENT_RE takes maximal identifier runs, so membership in that set is
    # exactly what `(?<![A-Za-z0-9_])name(?![A-Za-z0-9_])` asks -- and the
    # per-pair form built a fresh pattern string for every pair, which
    # overflowed re's compile cache: cProfile showed 39,703 compiles and 6.8 s
    # of the gate's 8.7 s inside `re.Pattern.search`. Verified by byte-identical
    # output on the real tree.
    exec_consumed: dict[str, str] = {}
    for carrier in exec_carriers:
        mentioned = set(IDENT_RE.findall(_item_body(stripped, items, carrier)))
        for _, _, name in items:
            if name == carrier[2] or name in exec_consumed:
                continue
            if name in mentioned:
                exec_consumed[name] = f"{carrier[1]} {carrier[2]}"

    sentinel_line = {
        int(m.group(1)): _line_of(starts, m.start())
        for m in SENTINEL_RE.finditer(stripped)
    }

    out: dict[str, list[dict]] = {}
    for index, value in enumerate(literals):
        if ".sh" not in value:
            continue
        lineno = sentinel_line.get(index)
        if lineno is None:
            # A literal the masker produced but that no sentinel survives for
            # cannot be placed. Counting it would invent a line number.
            continue
        for script, needle in needles.items():
            if not needle.search(value):
                continue
            carrier = carrier_of(items, lineno)
            if lineno in exec_lines:
                verdict, why = "EXEC_RUST", "process API on the same line"
            elif carrier is not None and carrier in exec_carriers:
                verdict, why = "EXEC_RUST", f"process API in {carrier[1]} {carrier[2]}"
            elif carrier is not None and carrier[2] in exec_consumed:
                verdict = "EXEC_RUST"
                why = (
                    f"{carrier[1]} {carrier[2]} is consumed by "
                    f"{exec_consumed[carrier[2]]}, which spawns"
                )
            else:
                verdict = "NAMED_RUST"
                why = (
                    f"in {carrier[1]} {carrier[2]}"
                    if carrier is not None
                    else "at module level"
                )
            out.setdefault(script, []).append(
                {"line": lineno, "verdict": verdict, "why": why}
            )
    return out


def _item_body(stripped: str, items: list[tuple[int, str, str]], carrier) -> str:
    """Source text from a declaration to the next one. Crude but adequate: the
    question is only which identifiers appear inside an item, and a nested fn
    merely widens the window by its own body."""
    lines = stripped.splitlines()
    start = carrier[0] - 1
    end = len(lines)
    for item in items:
        if item[0] > carrier[0]:
            end = item[0] - 1
            break
    return "\n".join(lines[start:end])


# --------------------------------------------------------------------------
# Shell / YAML / Python: a non-comment reference that is not a MESSAGE
# --------------------------------------------------------------------------
# In shell, naming a script on a command line IS running it, so the mention
# problem has a different shape than in Rust: the mentions are comments and
# message strings.
#
# MESSAGE STRINGS ARE WHY THIS BEAD UNDERCOUNTED ITSELF. bd-e2d1q's population
# was "e2e scripts reachable ONLY from tests/**", and
# scripts/e2e_overhaul/auto_enroll_perf_gate.sh was excluded from it on the
# strength of scripts/bench.sh:1403 --
#
#     echo "[+] Candidate comparison belongs to scripts/.../auto_enroll_perf_gate.sh" >&2
#
# -- plus one README line. Neither invokes anything, and the script sits in the
# very same const array as four scripts the bead DOES list (the array has five
# entries; the bead names four). So the bead's own population filter made the
# mention-versus-invocation error the bead is about. Measured 2026-11-09.
MESSAGE_COMMANDS = ("echo", "printf", "print", "warn", "note", "log", "die", "fail")

# Splitting on these approximates "start of a command" well enough to find the
# verb that owns a reference.
SEGMENT_SPLIT_RE = re.compile(r"\|\||&&|[;|]|\$\(|\(|`")


def strip_shell_comment(line: str) -> str:
    """Drop a trailing `#` comment, respecting quotes.

    A reference inside a comment is a mention, and the audit's own history says
    so: it found a verify.sh note explaining why a script was deliberately NOT
    wired, and counted the script invoked.
    """
    single = double = False
    for i, ch in enumerate(line):
        if ch == "'" and not double:
            single = not single
        elif ch == '"' and not single:
            double = not double
        elif ch == "#" and not single and not double:
            if i == 0 or line[i - 1] in " \t":
                return line[:i]
    return line


def is_message_context(line: str, position: int) -> bool:
    """True when the reference at `position` is an argument to a command that
    cannot execute anything.

    The unit is the enclosing command SEGMENT, so a reference after `&& echo`
    is caught as well as one on a bare `echo` line. The residual limit is
    stated in the gate's output: a message command reached in a way this does
    not split on reads as an invocation, and that direction is a FALSE EXEC --
    which is why the registry, not this function, is the thing that makes a
    verdict reviewable.
    """
    cut = 0
    for match in SEGMENT_SPLIT_RE.finditer(line):
        if match.end() <= position:
            cut = match.end()
        else:
            break
    segment = line[cut:position]
    words = re.findall(r"[A-Za-z_][\w.-]*", segment)
    # Skip leading VAR=value assignments, which precede the real verb.
    for word in words:
        if re.match(rf"^{re.escape(word)}\s*=", segment.strip()):
            continue
        return word in MESSAGE_COMMANDS
    return False


def classify_shell(text: str, needles: dict[str, re.Pattern[str]]) -> dict[str, list[dict]]:
    out: dict[str, list[dict]] = {}
    for lineno, raw in enumerate(text.splitlines(), start=1):
        if ".sh" not in raw:
            continue
        line = strip_shell_comment(raw)
        if not line.strip():
            continue
        for script, needle in needles.items():
            for match in needle.finditer(line):
                if is_message_context(line, match.start()):
                    verdict, why = "NAMED_SHELL", "argument to a message command"
                else:
                    verdict, why = "EXEC_SHELL", "named on a command line"
                out.setdefault(script, []).append(
                    {"line": lineno, "verdict": verdict, "why": why}
                )
                break
    return out


def needle_for(script: str) -> re.Pattern[str]:
    """Match the script by any spelling that identifies it.

    The preceding character may be `/` -- a reference is overwhelmingly written
    as `"$SCRIPT_DIR/g1_ppr.sh"` or `scripts/e2e_overhaul/g1_ppr.sh`. An earlier
    version of this measurement excluded `/` from the allowed preceding class
    in order to reject `my_g1_ppr.sh`, and thereby rejected every path-prefixed
    reference: it reported scripts/e2e_overhaul/swarm_scale.sh as executed by
    nothing when scripts/e2e_overhaul/failure_modes.sh:2448 runs it, and
    scripts/e2e_event_contract_radar.sh likewise against verify.sh:1770. Two
    false findings in one run, both in the over-reporting direction.
    """
    base = script.rsplit("/", 1)[-1]
    return re.compile(rf"(?<![A-Za-z0-9_]){re.escape(base)}(?![A-Za-z0-9_])")


# --------------------------------------------------------------------------
# Registry
# --------------------------------------------------------------------------


def read_registry(path: pathlib.Path) -> list[tuple[int, str, str, str]]:
    """(lineno, script, covering test, what is unproven) for each declaration."""
    if not path.exists():
        return []
    rows = []
    for lineno, raw in enumerate(path.read_text().splitlines(), start=1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        parts = [p.strip() for p in line.split("|")]
        if len(parts) != 3:
            raise CannotAnswer(
                f"{REGISTRY_REL}:{lineno}: expected "
                f"`<script> | <covering test> | <what is unproven>`, got {raw!r}"
            )
        rows.append((lineno, parts[0], parts[1], parts[2]))
    return rows


def read_simple_list(path: pathlib.Path) -> set[str]:
    if not path.exists():
        return set()
    out = set()
    for raw in path.read_text().splitlines():
        line = raw.strip()
        if line and not line.startswith("#"):
            out.add(line.split("|")[0].strip())
    return out


# --------------------------------------------------------------------------
# Run
# --------------------------------------------------------------------------


def repo_root() -> pathlib.Path:
    here = pathlib.Path(__file__).resolve().parent.parent
    if (here / ".git").exists() or (here / "Cargo.toml").exists():
        return here
    raise CannotAnswer(f"cannot locate the repository root from {__file__}")


def evaluate(root: pathlib.Path) -> dict:
    tracked = git_tracked(root)
    pop = population(tracked)
    if not pop:
        raise CannotAnswer(
            "no tracked e2e scripts matched the population patterns; either the "
            "layout moved or git ls-files returned nothing, and an empty "
            "population would make every check below vacuously green"
        )

    # A needle is a BASENAME, so two population entries sharing one would be
    # indistinguishable and every reference would be credited to both. There
    # are no collisions today (measured 2026-11-09), and
    # e2e_invocation_audit.sh names this as precisely the near-miss it exists
    # to catch -- "scripts/e2e_overhaul/tiered_recall.sh sitting next to a
    # registered tiered_recall_e2e.sh" -- so it refuses rather than guesses.
    collisions: dict[str, list[str]] = {}
    for script in pop:
        collisions.setdefault(script.rsplit("/", 1)[-1], []).append(script)
    clashing = {base: paths for base, paths in collisions.items() if len(paths) > 1}
    if clashing:
        detail = "; ".join(
            f"{base}: {' and '.join(paths)}" for base, paths in sorted(clashing.items())
        )
        raise CannotAnswer(
            "two e2e scripts share a basename, which this gate matches on, so "
            f"a reference cannot be attributed to one of them -- {detail}"
        )

    needles = {script: needle_for(script) for script in pop}
    # One cheap pass decides which files are worth lexing at all. It carries
    # the same identifier lookarounds as `needle_for`, and that is a measured
    # choice rather than a copy: dropping them makes the pattern cheaper per
    # position but admits 6 more files to the lexer, and the run got SLOWER
    # (6.6 s -> 8.0 s). Verdicts were unchanged either way -- the per-script
    # needles grade whatever this lets through -- so the only thing the
    # looser form bought was work.
    any_needle = re.compile(
        r"(?<![A-Za-z0-9_])(?:"
        + "|".join(sorted({re.escape(s.rsplit("/", 1)[-1]) for s in pop}))
        + r")(?![A-Za-z0-9_])"
    )

    sites: dict[str, list[dict]] = {script: [] for script in pop}
    sources = eligible_sources(tracked)
    scanned_rust = scanned_other = 0

    for rel in sources:
        path = root / rel
        try:
            text = path.read_text(errors="replace")
        except (OSError, UnicodeError):
            continue
        if ".sh" not in text or not any_needle.search(text):
            continue
        local = {s: n for s, n in needles.items() if s != rel}
        if rel.endswith(".rs"):
            scanned_rust += 1
            found = classify_rust(text, local)
        else:
            scanned_other += 1
            found = classify_shell(text, local)
        for script, rows in found.items():
            for row in rows:
                row["file"] = rel
                sites[script].append(row)

    verdicts: dict[str, str] = {}
    for script in pop:
        rows = sites[script]
        if not rows:
            verdicts[script] = "UNREFERENCED"
        elif any(r["verdict"] == "EXEC_SHELL" for r in rows):
            verdicts[script] = "EXEC_SHELL"
        elif any(r["verdict"] == "EXEC_RUST" for r in rows):
            verdicts[script] = "EXEC_RUST"
        else:
            verdicts[script] = "NAMED_ONLY"

    return {
        "root": root,
        "population": pop,
        "sites": sites,
        "verdicts": verdicts,
        "scanned_rust": scanned_rust,
        "scanned_other": scanned_other,
    }


def run(root: pathlib.Path, verbose: bool = True) -> int:
    data = evaluate(root)
    pop = data["population"]
    verdicts = data["verdicts"]
    sites = data["sites"]

    def say(*args):
        if verbose:
            print(*args)

    counts = {k: 0 for k in ("EXEC_SHELL", "EXEC_RUST", "NAMED_ONLY", "UNREFERENCED")}
    for verdict in verdicts.values():
        counts[verdict] += 1

    say("e2e execution coverage (bd-e2d1q)")
    say(f"  tracked e2e scripts            {len(pop)}")
    say(f"  rust files scanned             {data['scanned_rust']}")
    say(f"  other root files scanned       {data['scanned_other']}")
    say(f"  executed by a shell/CI root    {counts['EXEC_SHELL']}")
    say(f"  executed by a rust root        {counts['EXEC_RUST']}")
    say(f"  NAMED but executed by nothing  {counts['NAMED_ONLY']}")
    say(f"  unreferenced (orphan audit)    {counts['UNREFERENCED']}")
    say("")

    # POSITIVE CONTROLS. A blind predicate reports the whole population as
    # unexecuted and is indistinguishable from a clean confirmation, so each
    # predicate must be shown to fire at all before any verdict is published.
    if counts["EXEC_RUST"] == 0:
        raise CannotAnswer(
            "the Rust execution predicate matched NOTHING across "
            f"{data['scanned_rust']} files. It is blind, so 'nothing is "
            "executed from Rust' is not a finding -- it is the absence of one"
        )
    if counts["EXEC_SHELL"] == 0:
        raise CannotAnswer(
            "the shell execution predicate matched NOTHING across "
            f"{data['scanned_other']} files. It is blind; see above"
        )

    registry_path = data["root"] / REGISTRY_REL
    declared = read_registry(registry_path)
    orphan_baseline = read_simple_list(data["root"] / ORPHAN_BASELINE_REL)
    retired = read_simple_list(data["root"] / RETIRED_REL)

    declared_scripts = {}
    failures: list[str] = []

    for lineno, script, covering, unproven in declared:
        if script in declared_scripts:
            failures.append(
                f"{REGISTRY_REL}:{lineno}: {script} is declared twice "
                f"(also line {declared_scripts[script][0]})"
            )
            continue
        declared_scripts[script] = (lineno, covering, unproven)

    for script, (lineno, covering, unproven) in sorted(declared_scripts.items()):
        where = f"{REGISTRY_REL}:{lineno}"
        if script not in verdicts:
            failures.append(
                f"{where}: {script} is not a tracked e2e script. Either it was "
                "renamed or removed, or the path is wrong -- a declaration "
                "about a file that does not exist exempts nothing"
            )
            continue
        verdict = verdicts[script]
        if verdict in ("EXEC_RUST", "EXEC_SHELL"):
            proof = next(
                r for r in sites[script] if r["verdict"].startswith("EXEC")
            )
            failures.append(
                f"{where}: {script} is now EXECUTED "
                f"({proof['file']}:{proof['line']}, {proof['why']}). The "
                "declaration is stale -- delete this line. This registry can "
                "only shrink; that is what stops it decaying into an "
                "ignore-list"
            )
            continue
        if verdict == "UNREFERENCED":
            failures.append(
                f"{where}: {script} is referenced by NOTHING, so it is an "
                "orphan rather than text-conformance-covered. It belongs to "
                "scripts/e2e_invocation_audit.sh, not here"
            )
            continue
        if script in orphan_baseline:
            failures.append(
                f"{where}: {script} is also in {ORPHAN_BASELINE_REL}. A script "
                "cannot be both un-referenced and covered by a named test; one "
                "of the two records is wrong"
            )
        if script in retired:
            failures.append(
                f"{where}: {script} is also in {RETIRED_REL}, which asserts its "
                "assertions are covered elsewhere. Those two claims contradict"
            )
        covering_path = data["root"] / covering
        if not covering_path.exists():
            failures.append(
                f"{where}: the covering test {covering} does not exist. This is "
                "the check that stops a declaration naming a test that was "
                "deleted or renamed"
            )
        elif not any(r["file"] == covering for r in sites[script]):
            named = sorted({r["file"] for r in sites[script]})
            failures.append(
                f"{where}: {covering} does not reference {script}. "
                f"Referencing files are: {', '.join(named) if named else '(none)'}"
            )
        if not unproven:
            failures.append(
                f"{where}: the third field is empty. It must say what execution "
                "coverage is ABSENT, which is the whole point of the entry"
            )

    undeclared = [
        s for s in pop if verdicts[s] == "NAMED_ONLY" and s not in declared_scripts
    ]
    for script in undeclared:
        rows = sites[script]
        where = ", ".join(f"{r['file']}:{r['line']}" for r in rows[:3])
        failures.append(
            f"{script}: NAMED by a root but EXECUTED by nothing ({where}). "
            f"Either give it an executing runner, or declare it in "
            f"{REGISTRY_REL} with the test that covers its text and what that "
            "leaves unproven"
        )

    if verbose and declared_scripts:
        say(f"declared text-conformance-only ({len(declared_scripts)}):")
        for script, (_, covering, unproven) in sorted(declared_scripts.items()):
            say(f"  {script}")
            say(f"      covered by {covering} -- {unproven}")
        say("")

    if verbose:
        say("NOTE on the unit: a Rust reference is graded by its enclosing")
        say("  fn/const/static, and that carrier by the items which CONSUME it")
        say("  (one hop, not transitive) -- not by its file, since a file that")
        say("  spawns one script while only reading another is the normal")
        say("  case. A shell reference is graded by its")
        say("  enclosing command segment, so an `echo` naming a script is a")
        say("  message, not a run; a message command reached by a construct")
        say("  this does not split on would read as an invocation.")
        say("")

    if failures:
        print(f"RED: {len(failures)} problem(s)", file=sys.stderr)
        for failure in failures:
            print(f"  - {failure}", file=sys.stderr)
        return EXIT_RED

    say(
        f"OK: {counts['NAMED_ONLY']} text-conformance-only script(s), all declared "
        f"and all still earned."
    )
    return EXIT_OK


# --------------------------------------------------------------------------
# Self-test
# --------------------------------------------------------------------------

_ARMS = [0]
# A separate counter from _ARMS. Naming fixture directories after the arm count
# is only unique while every `_fixture` call is followed by exactly one `_arm`,
# and a second fixture built before the next arm would silently REUSE the
# previous directory -- leaving its files in place and grading a fixture nobody
# wrote. No such pair exists below; this removes the ordering requirement.
_FIXTURES = [0]


def _write(root: pathlib.Path, rel: str, body: str) -> None:
    path = root / rel
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(body)


def _fixture(tmp: pathlib.Path, files: dict[str, str]) -> pathlib.Path:
    _FIXTURES[0] += 1
    root = tmp / f"case{_FIXTURES[0]}"
    root.mkdir(parents=True)
    _write(root, "Cargo.toml", "[package]\nname = \"fixture\"\n")
    for rel, body in files.items():
        _write(root, rel, body)
    subprocess.run(["git", "init", "-q"], cwd=root, check=True)
    subprocess.run(["git", "add", "-A"], cwd=root, check=True)
    return root


def _arm(label: str, root: pathlib.Path, want: int) -> bool:
    _ARMS[0] += 1
    try:
        got = run(root, verbose=False)
    except CannotAnswer:
        got = EXIT_CANNOT
    ok = got == want
    print(f"  {'ok  ' if ok else 'FAIL'} {label}   (want {want}, got {got})")
    return ok


# A referencing test that only READS -- the shape bd-e2d1q is about.
READER = """\
const SCRIPTS: &[&str] = &[
    "scripts/e2e_sample.sh",
];

fn read_script(path: &str) -> String {
    std::fs::read_to_string(path).unwrap()
}

#[test]
fn sample_script_is_shaped_like_a_driver() {
    for path in SCRIPTS {
        let body = read_script(path);
        assert!(body.starts_with("#!/usr/bin/env bash"));
    }
}
"""

# A referencing test that RUNS it, via the same const-table indirection.
RUNNER = """\
const SCRIPTS: &[&str] = &[
    "scripts/e2e_sample.sh",
];

#[test]
fn sample_script_passes() {
    for path in SCRIPTS {
        let out = std::process::Command::new("bash").arg(path).output().unwrap();
        assert!(out.status.success());
    }
}
"""

# A referencing test that RUNS it, with the path returned by a HELPER FN. This
# is the shape that defeated an earlier version of this gate on the real tree
# -- four false findings in one run -- because the consumed-name rule resolved
# `const` but not `fn`. The literal's enclosing item is `script_path`, and the
# process API is in a different function entirely.
FN_RUNNER = """\
fn script_path() -> std::path::PathBuf {
    repo_root().join("scripts").join("e2e_sample.sh")
}

#[test]
fn sample_script_passes() {
    let out = std::process::Command::new("bash")
        .arg(script_path())
        .output()
        .unwrap();
    assert!(out.status.success());
}
"""

# The negative twin: the SAME helper-fn shape, consumed only by a reader. If
# the arm above passed because the rule credits any file with a process API,
# this one would pass too and neither would be testing the rule.
FN_READER = """\
fn script_path() -> std::path::PathBuf {
    repo_root().join("scripts").join("e2e_sample.sh")
}

#[test]
fn sample_script_is_shaped_like_a_driver() {
    let body = std::fs::read_to_string(script_path()).unwrap();
    assert!(body.starts_with("#!/usr/bin/env bash"));
}
"""

# The control that keeps both predicates honest: the fixture ALWAYS carries a
# script executed from Rust and one executed from shell, so the positive
# controls inside run() can fire. Without these every arm below would exit 3.
CONTROL_FILES = {
    "scripts/e2e_control_rust.sh": "#!/usr/bin/env bash\necho rust\n",
    "scripts/e2e_control_shell.sh": "#!/usr/bin/env bash\necho shell\n",
    "tests/control_runner.rs": (
        '#[test]\nfn control() {\n'
        '    std::process::Command::new("bash")\n'
        '        .arg("scripts/e2e_control_rust.sh")\n'
        "        .output()\n"
        "        .unwrap();\n}\n"
    ),
    "scripts/verify.sh": (
        "#!/usr/bin/env bash\n"
        'run_stage "Control" "./scripts/e2e_control_shell.sh"\n'
    ),
}

SCRIPT = "#!/usr/bin/env bash\nset -euo pipefail\necho sample\n"
DECL = "scripts/e2e_sample.sh | tests/reader.rs | behaviour never executed\n"


def self_test() -> int:
    failed = 0
    with tempfile.TemporaryDirectory() as raw:
        tmp = pathlib.Path(raw)

        def case(extra: dict[str, str]) -> pathlib.Path:
            files = dict(CONTROL_FILES)
            files.update(extra)
            return _fixture(tmp, files)

        # ---- the finding this gate exists to produce ----------------------
        root = case({"scripts/e2e_sample.sh": SCRIPT, "tests/reader.rs": READER})
        failed += not _arm(
            "a script only READ by a test, undeclared, is RED", root, EXIT_RED
        )

        root = case(
            {
                "scripts/e2e_sample.sh": SCRIPT,
                "tests/reader.rs": READER,
                REGISTRY_REL: DECL,
            }
        )
        failed += not _arm("...and is OK once declared", root, EXIT_OK)

        root = case({"scripts/e2e_sample.sh": SCRIPT, "tests/runner.rs": RUNNER})
        failed += not _arm(
            "a script RUN through a const table is EXEC, not a finding",
            root,
            EXIT_OK,
        )

        # The bug this gate shipped with for one iteration, and its twin.
        root = case({"scripts/e2e_sample.sh": SCRIPT, "tests/fnrunner.rs": FN_RUNNER})
        failed += not _arm(
            "a script RUN via a path returned by a HELPER FN is EXEC",
            root,
            EXIT_OK,
        )

        root = case(
            {
                "scripts/e2e_sample.sh": SCRIPT,
                "tests/fnreader.rs": FN_READER,
                REGISTRY_REL: (
                    "scripts/e2e_sample.sh | tests/fnreader.rs | text only\n"
                ),
            }
        )
        failed += not _arm(
            "...but the SAME helper-fn shape consumed only by a reader is not",
            root,
            EXIT_OK,
        )

        # The stale direction: the registry can only shrink.
        root = case(
            {
                "scripts/e2e_sample.sh": SCRIPT,
                "tests/runner.rs": RUNNER,
                REGISTRY_REL: "scripts/e2e_sample.sh | tests/runner.rs | x\n",
            }
        )
        failed += not _arm(
            "a declaration for a script that NOW runs is RED (stale)",
            root,
            EXIT_RED,
        )

        # ---- the mention traps bd-e2d1q records -------------------------------
        root = case(
            {
                "scripts/e2e_sample.sh": SCRIPT,
                "tests/reader.rs": READER,
                "scripts/bench.sh": (
                    "#!/usr/bin/env bash\n"
                    'echo "[+] belongs to scripts/e2e_sample.sh" >&2\n'
                ),
                REGISTRY_REL: DECL,
            }
        )
        failed += not _arm(
            "an `echo` naming a script is a MESSAGE, not an invocation",
            root,
            EXIT_OK,
        )

        root = case(
            {
                "scripts/e2e_sample.sh": SCRIPT,
                "tests/reader.rs": READER,
                "scripts/driver.sh": (
                    "#!/usr/bin/env bash\n"
                    'true && echo "see scripts/e2e_sample.sh for detail"\n'
                ),
                REGISTRY_REL: DECL,
            }
        )
        failed += not _arm(
            "an `echo` after `&&` is still a message (segment, not line)",
            root,
            EXIT_OK,
        )

        root = case(
            {
                "scripts/e2e_sample.sh": SCRIPT,
                "tests/reader.rs": READER,
                "scripts/driver.sh": (
                    "#!/usr/bin/env bash\n"
                    'json=$("${REPO_ROOT}/scripts/e2e_sample.sh" --json)\n'
                ),
            }
        )
        failed += not _arm(
            "a path-prefixed reference in $(...) IS an invocation",
            root,
            EXIT_OK,
        )

        root = case(
            {
                "scripts/e2e_sample.sh": SCRIPT,
                "tests/reader.rs": READER,
                "scripts/driver.sh": (
                    "#!/usr/bin/env bash\n"
                    '"$SCRIPT_DIR/e2e_sample.sh" 2>/dev/null || true\n'
                ),
            }
        )
        failed += not _arm(
            '"$SCRIPT_DIR/x.sh" IS an invocation (the `/` lookbehind bug)',
            root,
            EXIT_OK,
        )

        root = case(
            {
                "scripts/e2e_sample.sh": SCRIPT,
                "tests/reader.rs": READER,
                "scripts/driver.sh": (
                    "#!/usr/bin/env bash\n"
                    "# scripts/e2e_sample.sh is deliberately NOT wired here\n"
                ),
                REGISTRY_REL: DECL,
            }
        )
        failed += not _arm(
            "a reference in a shell COMMENT is not an invocation", root, EXIT_OK
        )

        root = case(
            {
                "scripts/e2e_sample.sh": SCRIPT,
                "tests/reader.rs": READER,
                "scripts/driver.sh": (
                    "#!/usr/bin/env bash\n"
                    'true   # see scripts/e2e_sample.sh\n'
                ),
                REGISTRY_REL: DECL,
            }
        )
        failed += not _arm(
            "a reference in a TRAILING shell comment is not an invocation",
            root,
            EXIT_OK,
        )

        root = case(
            {
                "scripts/e2e_sample.sh": SCRIPT,
                "tests/reader.rs": READER,
                "scripts/driver.sh": (
                    "#!/usr/bin/env bash\n"
                    'grep "#" scripts/e2e_sample.sh\n'
                ),
            }
        )
        failed += not _arm(
            "a `#` inside a quoted string does not start a comment",
            root,
            EXIT_OK,
        )

        # Rust: a process API written INSIDE a string literal is data.
        root = case(
            {
                "scripts/e2e_sample.sh": SCRIPT,
                "tests/reader.rs": READER,
                "tests/fixture_text.rs": (
                    "const SAMPLE: &str = "
                    '"Command::new(\\"bash\\").arg(\\"scripts/e2e_sample.sh\\")";\n'
                ),
                REGISTRY_REL: DECL,
            }
        )
        failed += not _arm(
            "Command::new INSIDE a string literal is not an execution",
            root,
            EXIT_OK,
        )

        root = case(
            {
                "scripts/e2e_sample.sh": SCRIPT,
                "tests/reader.rs": READER,
                "tests/shebang.rs": (
                    "#[test]\nfn shebang() {\n"
                    '    let first = std::fs::read_to_string("scripts/e2e_sample.sh")'
                    ".unwrap();\n"
                    '    assert!(first.contains("bash"), '
                    '"e2e_sample.sh shebang must select bash");\n}\n'
                ),
                REGISTRY_REL: DECL,
            }
        )
        failed += not _arm(
            "the shell verb `bash` in an assertion message is not an execution",
            root,
            EXIT_OK,
        )

        root = case(
            {
                "scripts/e2e_sample.sh": SCRIPT,
                "tests/reader.rs": READER,
                "tests/commented.rs": (
                    "#[test]\nfn x() {\n"
                    '    // std::process::Command::new("scripts/e2e_sample.sh");\n'
                    "}\n"
                ),
                REGISTRY_REL: DECL,
            }
        )
        failed += not _arm(
            "a commented-out Command::new is not an execution", root, EXIT_OK
        )

        # A byte-raw literal with an ODD inner quote count. On the pre-fix
        # lexer the bogus string state ran past the terminator and swallowed
        # following lines, masking the real Command::new below it.
        root = case(
            {
                "scripts/e2e_sample.sh": SCRIPT,
                "tests/byteraw.rs": (
                    "#[test]\nfn x() {\n"
                    '    let _ = br#"has one " quote"#;\n'
                    '    std::process::Command::new("bash")\n'
                    '        .arg("scripts/e2e_sample.sh")\n'
                    "        .output()\n        .unwrap();\n}\n"
                ),
            }
        )
        failed += not _arm(
            "a byte-raw string does not swallow the Command::new after it",
            root,
            EXIT_OK,
        )

        # ---- source exclusions ------------------------------------------
        for rel, label in (
            ("docs/guide.md", "a Markdown mention is not a root"),
            ("README.md", "a README mention is not a root"),
            (
                "tests/fixtures/cases/sample.json",
                "a fixture JSON mention is not a root",
            ),
            (".beads/issues.jsonl", "a bead description is not a root"),
            (
                "scripts/check-something.py",
                "a check-*.py gate reading a script is not a root",
            ),
            (
                "scripts/audit-something.py",
                "an audit-*.py gate reading a script is not a root",
            ),
        ):
            root = case(
                {
                    "scripts/e2e_sample.sh": SCRIPT,
                    "tests/reader.rs": READER,
                    rel: "run scripts/e2e_sample.sh to check this\n",
                    REGISTRY_REL: DECL,
                }
            )
            failed += not _arm(label, root, EXIT_OK)

        # The registry must not make itself vacuous.
        root = case(
            {
                "scripts/e2e_sample.sh": SCRIPT,
                "tests/reader.rs": READER,
                REGISTRY_REL: DECL,
                RETIRED_REL: "scripts/e2e_sample.sh | verify.sh | none\n",
            }
        )
        failed += not _arm(
            "declared AND retired is a contradiction, so RED", root, EXIT_RED
        )

        root = case(
            {
                "scripts/e2e_sample.sh": SCRIPT,
                "tests/reader.rs": READER,
                REGISTRY_REL: DECL,
                ORPHAN_BASELINE_REL: "scripts/e2e_sample.sh\n",
            }
        )
        failed += not _arm(
            "declared AND orphan-baselined is a contradiction, so RED",
            root,
            EXIT_RED,
        )

        # ---- registry hygiene --------------------------------------------
        root = case(
            {
                "scripts/e2e_sample.sh": SCRIPT,
                "tests/reader.rs": READER,
                REGISTRY_REL: "scripts/e2e_sample.sh | tests/gone.rs | x\n",
            }
        )
        failed += not _arm(
            "a declaration naming a covering test that does not exist is RED",
            root,
            EXIT_RED,
        )

        root = case(
            {
                "scripts/e2e_sample.sh": SCRIPT,
                "tests/reader.rs": READER,
                "tests/unrelated.rs": "#[test]\nfn x() {}\n",
                REGISTRY_REL: "scripts/e2e_sample.sh | tests/unrelated.rs | x\n",
            }
        )
        failed += not _arm(
            "a covering test that exists but does NOT reference the script is RED",
            root,
            EXIT_RED,
        )

        root = case(
            {
                "scripts/e2e_sample.sh": SCRIPT,
                "tests/reader.rs": READER,
                REGISTRY_REL: "scripts/e2e_sample.sh | tests/reader.rs | \n",
            }
        )
        failed += not _arm(
            "a declaration with an empty reason field is RED", root, EXIT_RED
        )

        root = case(
            {
                "scripts/e2e_sample.sh": SCRIPT,
                "tests/reader.rs": READER,
                REGISTRY_REL: DECL + DECL,
            }
        )
        failed += not _arm("a duplicate declaration is RED", root, EXIT_RED)

        root = case(
            {
                "scripts/e2e_sample.sh": SCRIPT,
                "tests/reader.rs": READER,
                REGISTRY_REL: "scripts/e2e_gone.sh | tests/reader.rs | x\n" + DECL,
            }
        )
        failed += not _arm(
            "a declaration for a script that is not in the population is RED",
            root,
            EXIT_RED,
        )

        root = case(
            {
                "scripts/e2e_sample.sh": SCRIPT,
                "tests/reader.rs": READER,
                REGISTRY_REL: "scripts/e2e_sample.sh tests/reader.rs x\n",
            }
        )
        failed += not _arm(
            "a malformed registry line is CANNOT ANSWER, not a pass",
            root,
            EXIT_CANNOT,
        )

        # An unreferenced script belongs to the orphan audit, and a
        # declaration claiming text coverage for it is wrong.
        root = case(
            {
                "scripts/e2e_sample.sh": SCRIPT,
                REGISTRY_REL: DECL,
            }
        )
        failed += not _arm(
            "a declaration for an UNREFERENCED script is RED (orphan, not covered)",
            root,
            EXIT_RED,
        )

        root = case({"scripts/e2e_sample.sh": SCRIPT})
        failed += not _arm(
            "an unreferenced script alone is not this gate's finding",
            root,
            EXIT_OK,
        )

        # ---- the blind-predicate controls --------------------------------
        root = _fixture(
            tmp,
            {
                "scripts/e2e_sample.sh": SCRIPT,
                "tests/reader.rs": READER,
                "scripts/verify.sh": (
                    "#!/usr/bin/env bash\n"
                    'run_stage "Control" "./scripts/e2e_control_shell.sh"\n'
                ),
                "scripts/e2e_control_shell.sh": "#!/usr/bin/env bash\n",
            },
        )
        failed += not _arm(
            "no Rust execution anywhere means the predicate is BLIND (exit 3)",
            root,
            EXIT_CANNOT,
        )

        root = _fixture(
            tmp,
            {
                "scripts/e2e_sample.sh": SCRIPT,
                "tests/reader.rs": READER,
                "scripts/e2e_control_rust.sh": "#!/usr/bin/env bash\n",
                "tests/control_runner.rs": CONTROL_FILES["tests/control_runner.rs"],
            },
        )
        failed += not _arm(
            "no shell execution anywhere means that predicate is BLIND (exit 3)",
            root,
            EXIT_CANNOT,
        )

        # Two scripts sharing a basename cannot be told apart by a needle.
        root = case(
            {
                "scripts/e2e_sample.sh": SCRIPT,
                "scripts/e2e_overhaul/e2e_sample.sh": SCRIPT,
                "tests/reader.rs": READER,
            }
        )
        failed += not _arm(
            "a basename collision is CANNOT ANSWER, not a double credit",
            root,
            EXIT_CANNOT,
        )

        # A .github workflow cannot execute anything while Actions is off.
        root = case(
            {
                "scripts/e2e_sample.sh": SCRIPT,
                "tests/reader.rs": READER,
                ".github/workflows/ci.yml": (
                    "jobs:\n  x:\n    steps:\n"
                    "      - run: ./scripts/e2e_sample.sh\n"
                ),
                REGISTRY_REL: DECL,
            }
        )
        failed += not _arm(
            "a .github workflow is not an executing root (Actions is off)",
            root,
            EXIT_OK,
        )

        # An empty population cannot be graded.
        root = _fixture(tmp, {"README.md": "nothing here\n"})
        failed += not _arm(
            "an empty population is CANNOT ANSWER, not a vacuous pass",
            root,
            EXIT_CANNOT,
        )

    print()
    if failed:
        print(f"self-test: {failed} of {_ARMS[0]} arms FAILED.")
        return EXIT_RED
    print(f"self-test: all {_ARMS[0]} arms passed.")
    return EXIT_OK


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(
        description="Refuse an e2e script that is named by a root but executed "
        "by nothing (bd-e2d1q).",
        allow_abbrev=False,
    )
    parser.add_argument(
        "--self-test", action="store_true", help="prove every verdict path"
    )
    parser.add_argument("--quiet", action="store_true", help="only report failures")
    try:
        args = parser.parse_args(argv)
    except SystemExit:
        return EXIT_USAGE

    if args.self_test:
        return self_test()
    try:
        return run(repo_root(), verbose=not args.quiet)
    except CannotAnswer as exc:
        print(f"CANNOT ANSWER: {exc}", file=sys.stderr)
        return EXIT_CANNOT


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
