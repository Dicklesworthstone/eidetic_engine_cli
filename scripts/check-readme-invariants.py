#!/usr/bin/env python3
"""Fast local precheck: does tests/readme_invariants/manifest.toml still pin README.md?

THIS IS A PRECHECK, NOT THE AUTHORITY. The authoritative assertions are
`readme_invariant_manifest_schema_is_pinned` in
tests/readme_invariant_manifest_schema.rs and
`readme_invariant_harness_covers_all_candidates` in
tests/readme_invariant_harness.rs. Both run only on the dispatch lane, inside
the integration_n_r and readme_invariant_harness targets. This script replays
them in Python so a README edit learns its verdict before it is committed.

WHY IT EXISTS. Measured 2026-11-24 by replaying both assertions over history:
green at 599112199, then RED from e66a56dc6 (2026-11-22) to HEAD. That commit
added a README paragraph whose lines shifted 21 anchors and whose claim line
(now L742) had no entry, and its message reports "documentation invariants
passed". Later README commits moved more lines, to 28 and then all 31 anchors,
and left two more claim lines unpinned (now L253 and L783). None of their
authors could see it. The gate was correct and unrunnable here, which is the
same thing as absent.

WHAT IT CHECKS, mirroring the Rust:

    validate_anchor_hash          every entry's readme_line_anchor line,
                                  whitespace-collapsed, BLAKE3-hashes to its
                                  sentence_hash
    validate_id / _classification stable slug, unique id, known classification,
                                  non-empty readme_section
    validate_verify               a `test` path exists under tests/ or scripts/;
                                  a `defer_bead` id is in .beads/issues.jsonl
                                  and not closed, and sets defer_until
    the harness loop              every README prose line with a trigger word or
                                  a quantity, outside code fences, tables,
                                  headings and HTML, has its hash in the
                                  manifest or matches a scrubber regex

NOT MIRRORED: the Rust also requires `verify` to be an INLINE table. tomllib
parses `verify = {...}` and a `[invariant.verify]` sub-table identically, so a
sub-table passes here and fails there.

When an anchor fails, the report names the line where that hash lives now, so
a pure line shift is a one-number fix. A changed sentence has no such line: its
claim needs re-reading against its verify test before the hash is updated.

DRIFT IS THE RISK of a second implementation. The self-test pins BLAKE3 against
reference digests, and gives each predicate a mutant arm that must fail. If this
script and the Rust disagree, the RUST IS RIGHT and this file is the bug.

Exit 0 green, 1 red, 2 usage, 3 cannot answer: an input is unreadable, or a
scrubber pattern is one Python's re rejects, so this mirror cannot say what the
Rust's regex_lite would do. No toolchain, network or third-party module: the
BLAKE3 below is the reference algorithm, so the gate runs wherever python3 does.
"""

import json
import pathlib
import re
import struct
import sys
import tempfile
import tomllib

REPO = pathlib.Path(__file__).resolve().parent.parent
EXIT_OK, EXIT_RED, EXIT_USAGE = 0, 1, 2
EXIT_CANNOT = 3
MANIFEST_PATH = "tests/readme_invariants/manifest.toml"

# tests/readme_invariant_harness.rs. regex_lite is ASCII-only for \b and (?i), hence re.ASCII.
TRIGGER = re.compile(
    r"(?i)\b(always|never|every|must|cannot|deterministic|deterministically|byte-stable|byte-identical|reproducible|surfaces?|enforces?|prevents?|detects?|quarantines?|decays?|audits?|redacts?)\b",
    re.ASCII,
)
QUANT = re.compile(r"(?i)\b\d+(\.\d+)?\s*(ms|millisecond|seconds?|minutes?|hours?|kB|MB|GB|TB|tokens?)\b", re.ASCII)
CLASSIFICATIONS = ("quantitative", "invariant", "promise", "constraint")

# Rust's char::is_whitespace is the Unicode White_Space property. Python's str.split() also splits on
# \x1c-\x1f, which Rust does not, so canonicalization uses this class instead.
_WS = "\t\n\x0b\x0c\r \x85\xa0                　"
_WS_RUN = re.compile(f"[{_WS}]+")


def canonical(line: str) -> str:
    """`line.split_whitespace().collect::<Vec<_>>().join(" ")`."""
    return " ".join(part for part in _WS_RUN.split(line) if part)


def rust_trim(line: str) -> str:
    return line.strip(_WS)


def rust_lines(text: str) -> list[str]:
    """`str::lines()`: split on \\n, drop one trailing \\r per line, no empty line after a final \\n."""
    lines = text.split("\n")
    if lines and lines[-1] == "":
        lines.pop()
    return [line[:-1] if line.endswith("\r") else line for line in lines]


# ---- BLAKE3, hash mode only (the reference implementation's structure) ----------------------------------
_IV = (0x6A09E667, 0xBB67AE85, 0x3C6EF372, 0xA54FF53A, 0x510E527F, 0x9B05688C, 0x1F83D9AB, 0x5BE0CD19)
_PERM = (2, 6, 3, 10, 7, 0, 4, 13, 1, 11, 12, 5, 9, 14, 15, 8)
_CHUNK_START, _CHUNK_END, _PARENT, _ROOT = 1, 2, 4, 8
_BLOCK, _CHUNK = 64, 1024
_M = 0xFFFFFFFF


def _g(s, a, b, c, d, x, y):
    s[a] = (s[a] + s[b] + x) & _M
    v = s[d] ^ s[a]
    s[d] = ((v >> 16) | (v << 16)) & _M
    s[c] = (s[c] + s[d]) & _M
    v = s[b] ^ s[c]
    s[b] = ((v >> 12) | (v << 20)) & _M
    s[a] = (s[a] + s[b] + y) & _M
    v = s[d] ^ s[a]
    s[d] = ((v >> 8) | (v << 24)) & _M
    s[c] = (s[c] + s[d]) & _M
    v = s[b] ^ s[c]
    s[b] = ((v >> 7) | (v << 25)) & _M


def _compress(cv, words, counter, block_len, flags):
    s = list(cv) + list(_IV[:4]) + [counter & _M, (counter >> 32) & _M, block_len, flags]
    m = list(words)
    for round_index in range(7):
        _g(s, 0, 4, 8, 12, m[0], m[1])
        _g(s, 1, 5, 9, 13, m[2], m[3])
        _g(s, 2, 6, 10, 14, m[4], m[5])
        _g(s, 3, 7, 11, 15, m[6], m[7])
        _g(s, 0, 5, 10, 15, m[8], m[9])
        _g(s, 1, 6, 11, 12, m[10], m[11])
        _g(s, 2, 7, 8, 13, m[12], m[13])
        _g(s, 3, 4, 9, 14, m[14], m[15])
        if round_index < 6:
            m = [m[i] for i in _PERM]
    return [s[i] ^ s[i + 8] for i in range(8)] + [s[i + 8] ^ cv[i] for i in range(8)]


def _words(block: bytes):
    return struct.unpack("<16I", block.ljust(_BLOCK, b"\0"))


def _chunk_output(data: bytes, chunk_index: int):
    """(cv, words, counter, block_len, flags) of a chunk's last block, before finalization."""
    cv = _IV
    blocks = [data[i : i + _BLOCK] for i in range(0, len(data), _BLOCK)] or [b""]
    for number, block in enumerate(blocks[:-1]):
        flags = _CHUNK_START if number == 0 else 0
        cv = _compress(cv, _words(block), chunk_index, _BLOCK, flags)[:8]
    flags = (_CHUNK_START if len(blocks) == 1 else 0) | _CHUNK_END
    return (cv, _words(blocks[-1]), chunk_index, len(blocks[-1]), flags)


def _parent_output(left, right):
    return (_IV, tuple(left) + tuple(right), 0, _BLOCK, _PARENT)


def _cv(output):
    cv, words, counter, block_len, flags = output
    return _compress(cv, words, counter, block_len, flags)[:8]


def blake3_hex(data: bytes) -> str:
    """32-byte BLAKE3 digest of `data`, as lowercase hex."""
    chunks = [data[i : i + _CHUNK] for i in range(0, len(data), _CHUNK)] or [b""]
    stack = []
    for index, chunk in enumerate(chunks[:-1]):
        cv = _cv(_chunk_output(chunk, index))
        total = index + 1
        while total & 1 == 0:
            cv = _cv(_parent_output(stack.pop(), cv))
            total >>= 1
        stack.append(cv)
    output = _chunk_output(chunks[-1], len(chunks) - 1)
    while stack:
        output = _parent_output(stack.pop(), _cv(output))
    cv, words, counter, block_len, flags = output
    return struct.pack("<8I", *_compress(cv, words, 0, block_len, flags | _ROOT)[:8]).hex()


def sentence_hash(text: str) -> str:
    return "blake3:" + blake3_hex(text.encode("utf-8"))


# ---- the checks -----------------------------------------------------------------------------------------
def schema_failures(manifest: dict, readme: str, bead_status, path_exists) -> list[str]:
    """tests/readme_invariant_manifest_schema.rs, one message per failure."""
    failures = []
    if manifest.get("schema") != "ee.readme_invariants.v1":
        failures.append(f"schema is {manifest.get('schema')!r}, expected 'ee.readme_invariants.v1'")
    if not manifest.get("scrubber", {}).get("denylist_regexes"):
        failures.append("scrubber denylist must be explicit")
    entries = manifest.get("invariant") or []
    if not entries:
        failures.append("manifest must seed at least one invariant")
    lines = rust_lines(readme)
    where = None
    seen = set()
    for entry in entries:
        entry_id = entry.get("id", "")
        if not re.fullmatch(r"[a-z][a-z0-9-]*", entry_id):
            failures.append(f"{entry_id} has an invalid stable slug")
        if entry_id in seen:
            failures.append(f"{entry_id} is duplicated")
        seen.add(entry_id)
        if entry.get("classification") not in CLASSIFICATIONS:
            failures.append(f"{entry_id} has unsupported classification {entry.get('classification')}")
        if not str(entry.get("readme_section", "")).strip():
            failures.append(f"{entry_id} has an empty README section")
        anchor = entry.get("readme_line_anchor")
        expected = entry.get("sentence_hash")
        if not isinstance(anchor, int) or not 1 <= anchor <= len(lines):
            failures.append(f"{entry_id} points at missing README line {anchor}")
        else:
            text = canonical(lines[anchor - 1])
            if not text:
                failures.append(f"{entry_id} points at an empty README line {anchor}")
            if sentence_hash(text) != expected:
                if where is None:
                    where = {}
                    for number, line in enumerate(lines, 1):
                        if canonical(line):
                            where.setdefault(sentence_hash(canonical(line)), []).append(number)
                moved = where.get(expected)
                hint = (
                    f"the pinned text is now at line {', '.join(map(str, moved))}: set readme_line_anchor"
                    if moved
                    else "the pinned text is gone: re-read the claim against its verify test, then re-pin it"
                )
                failures.append(f"{entry_id} anchor {anchor} hash mismatch ({hint}); line reads {text[:100]!r}")
        verify = entry.get("verify") or {}
        kind = verify.get("type")
        if kind == "test":
            path = verify.get("path")
            if not path:
                failures.append(f"{entry_id} test verifier must set path")
            elif not (path.startswith("tests/") or path.startswith("scripts/")):
                failures.append(f"{entry_id} test verifier path must live under tests/ or scripts/: {path}")
            elif not path_exists(path.split("::")[0]):
                failures.append(f"{entry_id} test verifier path does not exist on disk: {path}")
        elif kind == "defer_bead":
            bead = verify.get("id")
            if not bead:
                failures.append(f"{entry_id} defer_bead verifier must set id")
            elif not bead.startswith("bd-"):
                failures.append(f"{entry_id} defer_bead verifier has invalid id {bead}")
            elif bead_status(bead) is None:
                failures.append(f"{entry_id} defer_bead {bead} not found in .beads/issues.jsonl")
            elif bead_status(bead) == "closed":
                failures.append(f"{entry_id} defer_bead {bead} is closed")
            if not verify.get("defer_until"):
                failures.append(f"{entry_id} defer_bead verifier must set defer_until")
        else:
            failures.append(f"{entry_id} has unsupported verifier type {kind}")
    return failures


def harness_failures(manifest: dict, readme: str) -> list[str]:
    """tests/readme_invariant_harness.rs: README claim lines the manifest does not account for."""
    scrubber = [re.compile(pattern, re.ASCII) for pattern in manifest.get("scrubber", {}).get("denylist_regexes", [])]
    hashes = {entry.get("sentence_hash") for entry in manifest.get("invariant") or []}
    failures = []
    fence = False
    for number, raw in enumerate(rust_lines(readme), 1):
        stripped = rust_trim(raw)
        if stripped.startswith("```") or stripped.startswith("~~~"):
            fence = not fence
            continue
        if fence or not stripped or stripped == "---":
            continue
        if stripped.startswith(("#", "<!--", "<", "|")) or stripped.endswith("-->"):
            continue
        text = canonical(raw)
        if any(rx.search(text) for rx in scrubber):
            continue
        if not TRIGGER.search(text) and not QUANT.search(text):
            continue
        if sentence_hash(text) not in hashes:
            failures.append(
                f"L{number} unaccounted README invariant candidate: {text[:120]!r}\n"
                f"    add an [[invariant]] with readme_line_anchor = {number} and sentence_hash = \"{sentence_hash(text)}\","
                " or scrub the line if it is not load-bearing"
            )
    return failures


class CannotAnswer(Exception):
    """An input is unreadable, or outside what this mirror can parse: no verdict either way."""


def live_inputs(root: pathlib.Path):
    try:
        manifest = tomllib.loads((root / MANIFEST_PATH).read_text(encoding="utf-8"))
        readme = (root / "README.md").read_text(encoding="utf-8")
        statuses = {}
        for line in (root / ".beads/issues.jsonl").read_text(encoding="utf-8").splitlines():
            if line.strip():
                record = json.loads(line)
                if "id" in record and "status" in record:
                    statuses[record["id"]] = record["status"]
    except (OSError, UnicodeDecodeError, tomllib.TOMLDecodeError, json.JSONDecodeError) as exc:
        raise CannotAnswer(f"cannot read the gate's inputs under {root}: {exc}") from exc
    return manifest, readme, statuses.get, lambda path: (root / path).exists()


def evaluate(root: pathlib.Path) -> tuple[int, list[str]]:
    """(manifest entry count, failures) for the tree at `root`; raises CannotAnswer."""
    manifest, readme, bead_status, path_exists = live_inputs(root)
    try:
        unaccounted = harness_failures(manifest, readme)
    except re.error as exc:
        raise CannotAnswer(
            f"a scrubber pattern does not compile under Python's re, so this mirror cannot tell what "
            f"regex_lite makes of it: {exc}"
        ) from exc
    return len(manifest.get("invariant") or []), schema_failures(manifest, readme, bead_status, path_exists) + unaccounted


# ---- self-test --------------------------------------------------------------------------------------------
def self_test() -> int:
    failed = []

    def arm(name, ok):
        print(f"  {'ok  ' if ok else 'FAIL'} {name}")
        if not ok:
            failed.append(name)

    # Reference digests from b3sum 1.x over the official test-vector input (byte i = i mod 251). They cover
    # one block, several blocks, one chunk, and two- to nine-chunk trees, including uneven ones.
    vectors = {
        0: "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262",
        1: "2d3adedff11b61f14c886e35afa036736dcd87a74d27b5c1510225d0f592e213",
        63: "e9bc37a594daad83be9470df7f7b3798297c3d834ce80ba85d6e207627b7db7b",
        64: "4eed7141ea4a5cd4b788606bd23f46e212af9cacebacdc7d1f4c6dc7f2511b98",
        65: "de1e5fa0be70df6d2be8fffd0e99ceaa8eb6e8c93a63f2d8d1c30ecb6b263dee",
        1023: "10108970eeda3eb932baac1428c7a2163b0e924c9a9e25b35bba72b28f70bd11",
        1024: "42214739f095a406f3fc83deb889744ac00df831c10daa55189b5d121c855af7",
        1025: "d00278ae47eb27b34faecf67b4fe263f82d5412916c1ffd97c8cb7fb814b8444",
        2048: "e776b6028c7cd22a4d0ba182a8bf62205d2ef576467e838ed6f2529b85fba24a",
        2049: "5f4d72f40d7a5f82b15ca2b2e44b1de3c2ef86c426c95c1af0b6879522563030",
        3072: "b98cb0ff3623be03326b373de6b9095218513e64f1ee2edd2525c7ad1e5cffd2",
        3073: "7124b49501012f81cc7f11ca069ec9226cecb8a2c850cfe644e327d22d3e1cd3",
        5121: "628bd2cb2004694adaab7bbd778a25df25c47b9d4155a55f8fbd79f2fe154cff",
        8193: "bab6c09cb8ce8cf459261398d2e7aef35700bf488116ceb94a36d0f5f1b7bc3b",
    }
    for length, digest in vectors.items():
        arm(f"blake3 reference vector, {length} bytes", blake3_hex(bytes(i % 251 for i in range(length))) == digest)

    arm("canonical collapses tabs, runs and NBSP", canonical("\t a  b  c \n") == "a b c")
    arm("canonical keeps \\x1c, as Rust's split_whitespace does", canonical("a\x1cb") == "a\x1cb")
    arm("lines() drops \\r and the empty tail", rust_lines("a\r\nb\n") == ["a", "b"])
    arm("trigger is word-bounded", not TRIGGER.search("everyone agrees") and bool(TRIGGER.search("EVERY run")))
    arm("quantity needs a unit", bool(QUANT.search("in 40 ms")) and not QUANT.search("40 apples"))

    claim = "Every pack names its sources."
    readme = "\n".join(["# Title", "", claim, "", "```", "Every line in a fence is ignored.", "```",
                        "| Every | table row is ignored |", "Scrubbed: every word here is fine.", "Plain prose."]) + "\n"

    def manifest_for(anchor=3, digest=None, verify=None, extra=None):
        entry = {"id": "rfm-001-sources", "readme_section": "Title", "readme_line_anchor": anchor,
                 "sentence_hash": digest or sentence_hash(claim), "classification": "invariant",
                 "verify": verify or {"type": "test", "path": "tests/pack.rs::names_sources"}}
        return {"schema": "ee.readme_invariants.v1", "scrubber": {"denylist_regexes": ["^Scrubbed:"]},
                "invariant": [entry] + (extra or [])}

    status = {"bd-open": "open", "bd-shut": "closed"}.get
    exists = lambda path: path == "tests/pack.rs"
    arm("clean fixture passes the schema", schema_failures(manifest_for(), readme, status, exists) == [])
    arm("clean fixture passes the harness", harness_failures(manifest_for(), readme) == [])

    # Two lines of drift so the stale anchor lands on prose rather than a blank: a blank adds the Rust's
    # separate "empty README line" failure, which is the next arm's subject.
    shifted = "intro line\nsecond intro line\n" + readme
    drift = schema_failures(manifest_for(), shifted, status, exists)
    arm("MUTANT anchor drift is red and names the new line",
        len(drift) == 1 and "now at line 5" in drift[0])
    blank = schema_failures(manifest_for(), "intro line\n" + readme, status, exists)
    arm("MUTANT anchor drifted onto a blank line reports both failures, as the Rust does",
        len(blank) == 2 and "empty README line 3" in blank[0] and "now at line 4" in blank[1])
    edited = readme.replace(claim, "Every pack names most of its sources.")
    changed = schema_failures(manifest_for(), edited, status, exists)
    arm("MUTANT edited sentence is red with no line to move to",
        len(changed) == 1 and "the pinned text is gone" in changed[0])
    arm("MUTANT edited sentence leaves the harness red too", len(harness_failures(manifest_for(), edited)) == 1)
    arm("MUTANT anchor past the end is red",
        any("missing README line" in f for f in schema_failures(manifest_for(anchor=99), readme, status, exists)))
    uncovered = readme + "Imports never block the pack.\n"
    arm("MUTANT uncovered claim line is red", len(harness_failures(manifest_for(), uncovered)) == 1)
    arm("MUTANT missing verifier file is red",
        len(schema_failures(manifest_for(verify={"type": "test", "path": "tests/gone.rs"}), readme, status, exists)) == 1)
    arm("MUTANT verifier outside tests/ and scripts/ is red",
        len(schema_failures(manifest_for(verify={"type": "test", "path": "src/pack.rs"}), readme, status, lambda _: True)) == 1)
    arm("defer_bead to an open bead passes",
        schema_failures(manifest_for(verify={"type": "defer_bead", "id": "bd-open", "defer_until": "2027-01-01"}),
                        readme, status, exists) == [])
    arm("MUTANT defer_bead to a closed bead is red",
        len(schema_failures(manifest_for(verify={"type": "defer_bead", "id": "bd-shut", "defer_until": "2027-01-01"}),
                            readme, status, exists)) == 1)
    duplicate = manifest_for(extra=[dict(manifest_for()["invariant"][0])])
    arm("MUTANT duplicated id is red", any("is duplicated" in f for f in schema_failures(duplicate, readme, status, exists)))

    # End to end through the file reads, and the third state: no verdict is not a red.
    def tree(directory, scrubber="^Scrubbed:", beads=True):
        root = pathlib.Path(directory)
        (root / "tests/readme_invariants").mkdir(parents=True)
        (root / "README.md").write_text(readme, encoding="utf-8")
        (root / "tests/pack.rs").write_text("", encoding="utf-8")
        (root / MANIFEST_PATH).write_text(
            'schema = "ee.readme_invariants.v1"\n'
            f"[scrubber]\ndenylist_regexes = ['{scrubber}']\n"
            '[[invariant]]\nid = "rfm-001-sources"\nreadme_section = "Title"\nreadme_line_anchor = 3\n'
            f'sentence_hash = "{sentence_hash(claim)}"\nclassification = "invariant"\n'
            'verify = { type = "test", path = "tests/pack.rs" }\n',
            encoding="utf-8",
        )
        if beads:
            (root / ".beads").mkdir()
            (root / ".beads/issues.jsonl").write_text('{"id":"bd-open","status":"open"}\n', encoding="utf-8")
        return root

    with tempfile.TemporaryDirectory() as directory:
        arm("fixture tree is green end to end", run(tree(directory), quiet=True) == EXIT_OK)
    with tempfile.TemporaryDirectory() as directory:
        arm("MUTANT missing issues.jsonl cannot answer (3), not red",
            run(tree(directory, beads=False), quiet=True) == EXIT_CANNOT)
    with tempfile.TemporaryDirectory() as directory:
        arm("MUTANT scrubber pattern Python cannot compile cannot answer (3), not red",
            run(tree(directory, scrubber="(unclosed"), quiet=True) == EXIT_CANNOT)

    print(f"self-test: {len(failed)} of the arms failed" if failed else "self-test: all arms passed")
    return EXIT_RED if failed else EXIT_OK


def run(root: pathlib.Path, quiet: bool = False) -> int:
    say = (lambda *_: None) if quiet else print
    try:
        entries, failures = evaluate(root)
    except CannotAnswer as exc:
        say(f"check-readme-invariants: CANNOT ANSWER, no verdict either way: {exc}")
        return EXIT_CANNOT
    if failures:
        say(f"check-readme-invariants: {len(failures)} failure(s) against {MANIFEST_PATH}")
        for failure in failures:
            say(f"  {failure}")
        return EXIT_RED
    say(f"check-readme-invariants: {entries} manifest entries pin README.md, and no claim line is unaccounted")
    return EXIT_OK


def main(argv: list[str]) -> int:
    if "--self-test" in argv:
        return self_test()
    if argv:
        print("usage: check-readme-invariants.py [--self-test]", file=sys.stderr)
        return EXIT_USAGE
    return run(REPO)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
