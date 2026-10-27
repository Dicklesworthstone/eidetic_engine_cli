#!/usr/bin/env python3
"""Fleet-identifier disclosure gate, on a rail that actually runs here.

`tests/fleet_identifier_guard.rs` is the authority on what a private fleet
identifier is, and it is RED at HEAD on the tracker export. It is also
unreachable on this host: cargo is tripwired for `test`, `~/.rustup/toolchains`
is a dangling symlink, hosted Actions is off repo-wide and `verify.sh` refuses
every stage for want of a current native binary. So the one disclosure control
this public repository has cannot be executed by the person about to publish.

This mirror closes that gap WITHOUT becoming a second source of truth:

  * every token, allowlist and skip rule is PARSED OUT of the Rust guard at
    run time, so this file contains no fleet identifier and cannot drift from
    the gate it mirrors. Add a host word there and this picks it up.
  * the self-test arms are parsed out of the guard's own fixture tests, so the
    controls are the gate's controls, not a second opinion about them.
  * it refuses to report a census if any of that extraction comes back thin --
    an empty token list would scan clean over a carrying tree and report a
    green, which is the exact vacuous-pass this repository keeps paying for.

It is a RATCHET, not an equality. The tracker export carries a four-figure
backlog that only an operator-scheduled `br` migration can clear (bd-42puv:
the identifiers live in SQLite, so editing the JSONL is undone by the next
flush). Failing on that backlog every run would make this gate noise and it
would be switched off within a day. So:

    a carrying file NOT in the baseline        -> exit 1  (the real signal:
                                                  something was just published)
    a baseline file OVER its ceiling           -> exit 1
    a baseline file at ZERO                    -> exit 3  (clean: delete the
                                                  entry, the ratchet tightens)
    otherwise                                  -> exit 0

Ceilings carry headroom on purpose: `.beads/issues.jsonl` is regenerated from
SQLite on every flush, so its count moves with ordinary tracker churn. The
ratchet point is "this file reached zero", which is the migration's completion
signal, not "this file got one line smaller".
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys

GUARD = "tests/fleet_identifier_guard.rs"
BASELINE = "tests/fixtures/fleet_identifier_baseline.json"
EXCERPT_CAP = 200

# ---------------------------------------------------------------- extraction


def _const_block(source: str, name: str) -> str:
    """The body of `const <name>: ... = &[ ... ];`."""
    match = re.search(rf"const {name}\s*:[^=]*=\s*&\[", source)
    if not match:
        raise LookupError(f"{name} not found in {GUARD}")
    index = match.end()
    depth = 1
    while index < len(source) and depth:
        if source[index] == "[":
            depth += 1
        elif source[index] == "]":
            depth -= 1
        index += 1
    return source[match.end() : index - 1]


def _strings(block: str) -> list[str]:
    return re.findall(r'"((?:[^"\\]|\\.)*)"', block)


def _unescape(text: str) -> str:
    return text.encode().decode("unicode_escape")


def load_rules(source: str) -> dict:
    """Every rule the predicate needs, read out of the Rust guard."""
    host_words = [
        (word, flag == "true")
        for word, flag in re.findall(
            r'\(\s*"([^"]+)"\s*,\s*(true|false)\s*\)',
            _const_block(source, "BANNED_HOST_WORDS"),
        )
    ]
    rules = {
        "host_words": host_words,
        "substrings": _strings(_const_block(source, "BANNED_SUBSTRINGS")),
        "account_words": _strings(_const_block(source, "BANNED_ACCOUNT_WORDS")),
        "allowed_ips": re.findall(
            r'\(\s*"([^"]+)"\s*,', _const_block(source, "ALLOWLISTED_PUBLIC_IPS")
        ),
        "allowlisted_files": re.findall(
            r'\(\s*"([^"]+)"\s*,', _const_block(source, "ALLOWLISTED_FILES")
        ),
        "skipped_prefixes": _strings(_const_block(source, "SKIPPED_PREFIXES")),
        "skipped_directories": _strings(_const_block(source, "SKIPPED_DIRECTORIES")),
    }
    # VACUITY GUARD. A silently-empty rule set scans a carrying tree clean and
    # reports a green, which is worse than not running at all. These floors are
    # the populations measured when this mirror was written; the guard may grow
    # but it cannot shrink past them without someone editing this line.
    floors = {
        "host_words": 14,
        "substrings": 4,
        "account_words": 1,
        "allowed_ips": 1,
        "allowlisted_files": 1,
        "skipped_prefixes": 1,
        "skipped_directories": 6,
    }
    thin = {
        key: (len(rules[key]), floor)
        for key, floor in floors.items()
        if len(rules[key]) < floor
    }
    if thin:
        raise LookupError(
            "rule extraction came back thin, refusing to scan: "
            + ", ".join(f"{k} {got} < {want}" for k, (got, want) in thin.items())
        )
    rules["prefilter"] = build_prefilter(rules)
    return rules


def load_control_arms(source: str) -> tuple[list[str], list[str]]:
    """The guard's OWN fixture arms, so the controls share its mechanism."""
    positive: list[str] = []
    negative: list[str] = []
    for literal, verdict in re.findall(
        r'banned_token_in_line\(\s*"((?:[^"\\]|\\.)*)"\s*\)\.is_(some|none)\(\)', source
    ):
        (positive if verdict == "some" else negative).append(_unescape(literal))
    return positive, negative


# ---------------------------------------------------------------- predicate


# Per-reason supersets of the two O(line length) char loops below, each a
# strict relaxation of the predicate it guards: the VPS shape needs the prefix
# plus at least six digits, and a routable address needs four 1-3 digit octets,
# so neither loop can fire on a line its own pattern does not match. They
# bound the worst case on a pathologically long line.
#
# They did NOT make the run faster, and neither did the whole-file filter in
# `scan_lines`. See the PERFORMANCE note in that function before optimising:
# three plausible wins measured flat, and the reason is worth knowing.
VPS_PREFILTER = re.compile(r"vmi\d{6}", re.IGNORECASE)
IP_PREFILTER = re.compile(r"\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}")


def is_word_byte(char: str) -> bool:
    return char.isascii() and (char.isalnum() or char == "_")


def contains_word(haystack: str, needle: str, ignore_case: bool) -> bool:
    if ignore_case:
        haystack = haystack.lower()
    start = 0
    while True:
        begin = haystack.find(needle, start)
        if begin < 0:
            return False
        end = begin + len(needle)
        before_ok = begin == 0 or not is_word_byte(haystack[begin - 1])
        after_ok = end == len(haystack) or not is_word_byte(haystack[end])
        if before_ok and after_ok:
            return True
        start = begin + 1


def contains_vps_hostname(line: str) -> bool:
    folded = line.lower()
    for index in range(len(folded)):
        if not folded.startswith("vmi", index):
            continue
        if index and is_word_byte(folded[index - 1]):
            continue
        digits = 0
        while index + 3 + digits < len(folded) and folded[index + 3 + digits].isdigit():
            digits += 1
        if digits >= 6:
            return True
    return False


def is_non_routable(octets: list[int]) -> bool:
    first, second, third, _fourth = octets
    if first in (0, 10, 127):
        return True
    if first == 100 and 64 <= second <= 127:
        return True
    if (first, second) in ((169, 254), (192, 168)):
        return True
    if first == 172 and 16 <= second <= 31:
        return True
    if first == 192 and second == 0 and third in (0, 2):
        return True
    if (first, second, third) == (192, 88, 99):
        return True
    if first == 198 and second in (18, 19):
        return True
    if (first, second, third) in ((198, 51, 100), (203, 0, 113)):
        return True
    return first >= 224


def parse_dotted_quad(text: str) -> list[int] | None:
    """Reject empty octets, octets over three digits and octets above 255, so
    that version strings such as `1.2.3.4444` do not parse as addresses."""
    parts = text.split(".")
    if len(parts) != 4:
        return None
    octets = []
    for part in parts:
        if not part or len(part) > 3 or not (part.isdigit() and part.isascii()):
            return None
        value = int(part)
        if value > 255:
            return None
        octets.append(value)
    return octets


def contains_public_ip(line: str, allowed_ips: list[str]) -> str | None:
    index = 0
    while index < len(line):
        preceded_ok = index == 0 or not (
            is_word_byte(line[index - 1]) or line[index - 1] == "."
        )
        if not preceded_ok or not (line[index].isdigit() and line[index].isascii()):
            index += 1
            continue
        span_end = index
        while span_end < len(line) and (
            (line[span_end].isdigit() and line[span_end].isascii())
            or line[span_end] == "."
        ):
            span_end += 1
        # Sentence punctuation: `... on 203.0.113.5.` must still parse.
        end = span_end
        while end > index and line[end - 1] == ".":
            end -= 1
        followed_ok = end == len(line) or not is_word_byte(line[end])
        candidate = line[index:end]
        if followed_ok:
            octets = parse_dotted_quad(candidate)
            if octets and not is_non_routable(octets) and candidate not in allowed_ips:
                return candidate
        index = max(span_end, index + 1)
    return None


def build_prefilter(rules: dict) -> re.Pattern[str]:
    """A cheap SUPERSET of `banned_token_in_line`, to skip lines that cannot
    carry anything.

    Superset by construction: it drops every narrowing check the real
    predicate applies -- word boundaries, the one host word matched
    case-SENSITIVELY so that prose about stylesheets stays legal, the
    six-digit minimum after the VPS prefix, address range and octet
    validation -- so it can only over-match, and the precise predicate still
    decides every hit. A line this rejects provably carries nothing, which is
    what makes the shortcut safe. The equivalence is not argued, it is
    measured: the self-test runs all 25 of the guard's arms through BOTH paths
    and requires the same verdict, and the census is identical with and
    without this filter (4951 files, 1159 lines, 1 file).

    Note for anyone editing the prose in this file: the guard scans it like any
    other tracked file, and writing a banned host word out in lower case makes
    this script itself a carrying file. That happened once while it was being
    written and this gate is what caught it. Describe the tokens; do not spell
    them. `tests/fleet_identifier_guard.rs` is allowlisted because it must
    spell them to ban them -- that exemption is for the guard, not for its
    mirror, and it should stay that way.
    """
    alternatives = [re.escape(text) for text in rules["substrings"]]
    alternatives += [re.escape(word) for word, _ in rules["host_words"]]
    alternatives += [re.escape(word) for word in rules["account_words"]]
    alternatives.append(VPS_PREFILTER.pattern)
    alternatives.append(IP_PREFILTER.pattern)
    return re.compile("|".join(alternatives), re.IGNORECASE)


def banned_token_in_line(line: str, rules: dict) -> str | None:
    prefilter = rules.get("prefilter")
    if prefilter is not None and not prefilter.search(line):
        return None
    if VPS_PREFILTER.search(line) and contains_vps_hostname(line):
        return "vmi<digits> host name"
    if IP_PREFILTER.search(line):
        address = contains_public_ip(line, rules["allowed_ips"])
        if address:
            return f"routable address {address}"
    for needle in rules["substrings"]:
        if needle in line:
            return needle
    for word, ignore_case in rules["host_words"]:
        if contains_word(line, word, ignore_case):
            return word
    for word in rules["account_words"]:
        if contains_word(line, word, True):
            return word
    return None


# ---------------------------------------------------------------- the scan


def scan_lines(population, rules):
    """Count everything, cap only the printed excerpts, name every affected
    file. This is the shape bd-vejqk fixed in the Rust guard, and the
    self-test below replays its positive control against this function.

    `population` is a sequence of `(path, text)`. The prefilter runs once over
    each whole file before any line splitting: no alternative in it can match
    across a newline (its only `.` is escaped), so a line-level match is always
    a substring match of the whole text, and a file the filter rejects provably
    carries nothing.

    PERFORMANCE, measured 2026-10-27, so the next reader does not re-run these
    three experiments. The whole scan costs ~14 s over 4951 files and 2.26M
    lines. THREE correctness-preserving optimisations each measured FLAT:

      per-line prefilter inside the predicate   16.2 s -> 12.7 s
      + whole-file prefilter (this function)    12.7 s -> 14.1 s
      + per-reason gates on the two char loops  14.1 s -> 13.8 s

    all with a byte-identical census (4951 files, 1159 lines, 1 file), which is
    the only reason they were kept. The whole-file filter does skip 4673 of
    4951 files and 72.6% of all lines -- it just does not SAVE anything,
    because the regex engine then scans the same bytes in one pass instead of
    per line. The cost is proportional to bytes scanned, and no filter reduces
    that; it only moves it. Something that actually reduces bytes, or drops the
    `re.IGNORECASE` alternation, is the only thing left worth trying.

    What must NOT be "optimised" is the two char-loop predicates below. They
    are deliberately literal transliterations of the Rust rather than regexes:
    this file's job is to say what the guard would say, and a clever rewrite of
    `contains_public_ip`'s trailing-dot and octet-width rules is exactly where
    a port silently stops agreeing with the gate it mirrors.
    """
    total = 0
    per_file = []
    excerpts = []
    prefilter = rules.get("prefilter")
    for path, text in population:
        this_file = 0
        # Names count too: a fixture directory called after a worker leaks the
        # same information as a line of prose, and content-only scrubbing
        # leaves the code pointing at a path that no longer exists.
        token = banned_token_in_line(path, rules)
        if token:
            total += 1
            this_file += 1
            if len(excerpts) < EXCERPT_CAP:
                excerpts.append((path, 0, token))

        if prefilter is not None and not prefilter.search(text):
            if this_file:
                per_file.append((path, this_file))
            continue
        for index, line in enumerate(text.splitlines(), start=1):
            token = banned_token_in_line(line, rules)
            if token:
                total += 1
                this_file += 1
                # NO EARLY RETURN: reaching the cap stops us collecting
                # excerpts, never walking.
                if len(excerpts) < EXCERPT_CAP:
                    excerpts.append((path, index, token))
        if this_file:
            per_file.append((path, this_file))
    return total, per_file, excerpts


# ---------------------------------------------------------------- self-test


def self_test(rules: dict, positive: list[str], negative: list[str]) -> bool:
    failures = []
    # Floors, not equalities: the guard may add arms, but a regex that stops
    # matching its fixtures would otherwise leave this "passing" on nothing.
    # Measured against the guard as of this file's commit:
    # `guard_detects_each_banned_identifier_shape` carries 10 arms and
    # `guard_does_not_flag_neutral_placeholders` carries 15.
    if len(positive) < 10 or len(negative) < 15:
        failures.append(
            f"control arms came back thin: {len(positive)} positive, "
            f"{len(negative)} negative"
        )
    # Every arm is run through BOTH paths: with the prefilter (as the scan
    # runs) and without it (the full predicate). A disagreement means the
    # shortcut has stopped being a superset, which would show up as a silent
    # under-count over the real tree and nowhere else.
    unfiltered = {key: value for key, value in rules.items() if key != "prefilter"}
    for line in positive:
        fast, full = banned_token_in_line(line, rules), banned_token_in_line(
            line, unfiltered
        )
        if full is None:
            failures.append(f"MISSED a carrying line the guard catches: {line!r}")
        elif fast != full:
            failures.append(f"prefilter is not a superset, it dropped: {line!r}")
    for line in negative:
        fast, full = banned_token_in_line(line, rules), banned_token_in_line(
            line, unfiltered
        )
        if full is not None:
            failures.append(f"FLAGGED a legal line as {full!r}: {line!r}")
        elif fast != full:
            failures.append(f"prefilter disagreed on a legal line: {line!r}")

    # bd-vejqk's positive control, replayed: the count and the file list must
    # survive a file that exceeds the cap on its own. A carrying shape is
    # needed to drive it, so it is taken from the guard's own positive arms
    # rather than spelled here.
    driver = next((line for line in positive if contains_vps_hostname(line)), None)
    if driver is None:
        failures.append("no vmi-shaped positive arm to drive the cap control")
    else:
        synthetic = [
            ("aaa_low_sorting.txt", "\n".join([driver] * (EXCERPT_CAP + 50))),
            ("zzz_high_sorting.txt", driver),
        ]
        total, per_file, excerpts = scan_lines(synthetic, rules)
        if total != EXCERPT_CAP + 51:
            failures.append(f"cap control: total {total}, want {EXCERPT_CAP + 51}")
        if len(excerpts) != EXCERPT_CAP:
            failures.append(
                f"cap control: excerpts {len(excerpts)}, want {EXCERPT_CAP}"
            )
        named = [path for path, _ in per_file]
        if named != ["aaa_low_sorting.txt", "zzz_high_sorting.txt"]:
            failures.append(f"cap control: a file went unnamed: {named}")

    for line in failures:
        print(f"  SELF-TEST FAIL: {line}")
    print(
        f"  self-test: {len(positive)} positive + {len(negative)} negative arms "
        f"read from {GUARD}, plus bd-vejqk's cap control: {len(failures)} failure(s)"
    )
    return not failures


def tracked_files(rules: dict) -> list[str]:
    out = subprocess.run(
        ["git", "ls-files", "-z"], capture_output=True, text=True, check=True
    ).stdout
    skipped_dirs = set(rules["skipped_directories"])
    keep = []
    for path in (p for p in out.split("\0") if p):
        if path in rules["allowlisted_files"]:
            continue
        if any(path.startswith(prefix) for prefix in rules["skipped_prefixes"]):
            continue
        parts = path.split("/")[:-1]
        if any(part in skipped_dirs or part.startswith(".rch-target") for part in parts):
            continue
        keep.append(path)
    return sorted(keep)


def read_text(path: str) -> str | None:
    """None for anything that is not valid UTF-8: binary blobs are not prose.
    A binary file can still carry a violation IN ITS PATH, so the caller keeps
    it in the population with an empty body rather than dropping it."""
    try:
        with open(path, encoding="utf-8") as handle:
            return handle.read()
    except (UnicodeDecodeError, OSError):
        return None


def main() -> int:
    root = os.environ.get("EE_REPO_ROOT") or subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()
    os.chdir(root)

    try:
        with open(GUARD, encoding="utf-8") as handle:
            source = handle.read()
        rules = load_rules(source)
    except (OSError, LookupError) as error:
        print(f"  REFUSING TO SCAN: {error}")
        return 2
    positive, negative = load_control_arms(source)
    if not self_test(rules, positive, negative):
        print("  refusing to report a census from an instrument failing its own arms")
        return 2

    population = []
    unreadable = 0
    for path in tracked_files(rules):
        text = read_text(path)
        if text is None:
            unreadable += 1
            text = ""
        population.append((path, text))
    total, per_file, excerpts = scan_lines(population, rules)

    print(f"\n  tracked files scanned          {len(population)} ({unreadable} non-UTF-8)")
    print(f"  carrying lines (UNCAPPED)      {total}")
    print(f"  affected files (UNCAPPED)      {len(per_file)}")
    if total > len(excerpts):
        print(f"  excerpts withheld              {total - len(excerpts)} (cap {EXCERPT_CAP})")
    print("\n  every affected file:")
    for path, count in per_file:
        print(f"    {path}: {count} carrying line(s)")

    try:
        with open(BASELINE, encoding="utf-8") as handle:
            baseline = json.load(handle)
    except OSError:
        print(f"\n  no baseline at {BASELINE}")
        return 1 if total else 0
    ceilings = {entry["path"]: entry["ceiling"] for entry in baseline["files"]}
    observed = dict(per_file)
    new_files = sorted(set(observed) - set(ceilings))
    over = sorted(p for p, n in observed.items() if p in ceilings and n > ceilings[p])
    cleared = sorted(p for p in ceilings if observed.get(p, 0) == 0)

    print()
    if new_files:
        print("  FAIL: carrying file(s) absent from the baseline -- newly published:")
        for path in new_files:
            print(f"    {path}: {observed[path]} carrying line(s)")
    if over:
        print("  FAIL: baseline ceiling exceeded:")
        for path in over:
            print(f"    {path}: {observed[path]} > ceiling {ceilings[path]}")
    if new_files or over:
        print("  Replace them with the neutral placeholders the guard recommends:")
        print("  worker-01 / worker-a / windows-host-1 for hosts, RFC 5737")
        print("  198.51.100.x for public addresses, CGNAT 100.64.0.x for tailnet peers.")
        return 1
    if cleared:
        print("  RATCHET: baseline entr(ies) now carry nothing. Delete them from")
        print(f"  {BASELINE} so the gate tightens and cannot slacken again:")
        for path in cleared:
            print(f"    {path}")
        return 3
    print(f"  OK: {len(observed)} carrying file(s), all within baseline ceilings.")
    print(f"      {total} line(s) of known debt remain; see {baseline.get('bead', 'bd-42puv')}.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
