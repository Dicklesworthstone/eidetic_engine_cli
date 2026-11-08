#!/usr/bin/env python3
"""Pin the lexical analyzer's source to the corpus revision that invalidates indexes.

WHY THIS EXISTS (bd-b5j51)

Index reuse is gated by `expected_index_corpus_revision()` in src/core/index.rs.
`LEXICAL_ANALYZER_REVISION` participates in that hash so a generation built under
literal-token analysis is rebuilt instead of reused. Its docstring states the
contract: "Index and query analysis must advance together."

Nothing enforced it. The constant had exactly two occurrences in the tree -- its
definition and the one hashing site -- while the analyzer is DEFINED in two other
places. Either could be edited without the constant moving, and then every
existing index is deemed current and reused: documents tokenized under the old
analysis, queries tokenized under the new one. Silent recall loss on populated
workspaces, with no error and no degraded code. That is the same failure shape as
the North Star inflection miss, reintroduced by a correct-looking edit.

The three engine regressions added with the stemmer each build a FRESH index in a
tempdir, so none of them can observe a reuse bug. They assert the analyzer is
correct today, which is a different property from "a change in analysis forces a
rebuild".

HOW IT ENFORCES IT

The constant must carry a fingerprint of the analyzer source it describes:

    "ee.lexical_analyzer.english_stem.v1+fp<10 hex>"

The fingerprint covers the two regions that define analysis, with comments
stripped and whitespace normalized, so prose edits and reformatting do not churn
it while a changed filter, tokenizer, or Language does. Because the expected
value lives IN the constant rather than in a committed fixture, there is no
expectation file to refresh -- an analysis change cannot be waved through by
editing one field, which is the flaw in the fixture-based design this replaces.

WHAT IT DOES NOT CATCH, stated plainly rather than overclaimed: analysis can also
change with no diff in this repository at all, when an upstream frankensearch or
tantivy tokenizer changes under a dependency bump. No source fingerprint can see
that. Bumping the constant by hand remains correct and necessary there, and this
gate accepts it -- a changed `v1` segment with an unchanged fingerprint passes,
because that is exactly the shape of a deliberate upstream-driven invalidation.

Exit codes follow the local family: 0 ok, 1 drift, 2 self-test failure,
3 cannot answer (usage, or a region this gate could not locate -- never a pass).
"""

from __future__ import annotations

import hashlib
import pathlib
import re
import sys

REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent

CONST_FILE = REPO_ROOT / "src" / "search" / "mod.rs"
MAINTENANCE_FILE = REPO_ROOT / "src" / "core" / "index_delta_lexical.rs"
HASH_SITE_FILE = REPO_ROOT / "src" / "core" / "index.rs"

CONST_NAME = "LEXICAL_ANALYZER_REVISION"
ANALYZER_FN = "configure_lexical_analyzer"
MAINTENANCE_FN = "maintenance_writer"
CORPUS_FN = "expected_index_corpus_revision"

FINGERPRINT_MARKER = "+fp"
FINGERPRINT_HEX_LEN = 10

CONST_DECL = re.compile(
    r"const\s+" + CONST_NAME + r"\s*:\s*&\s*(?:'static\s+)?str\s*=\s*\"(?P<value>[^\"]*)\"\s*;"
)

# A declaration may carry visibility and qualifiers before `fn`. Anchoring on a
# bare `fn` at line start silently misses `pub fn`, which is how the corpus
# revision function -- the one this gate's liveness check reads -- would have
# come back as "cannot answer" on a perfectly healthy tree.
def _fn_decl(name: str) -> re.Pattern[str]:
    return re.compile(
        r"^[ \t]*(?:pub(?:\s*\([^)]*\))?[ \t]+)?"
        r"(?:(?:default|const|async|unsafe)[ \t]+)*"
        r"(?:extern[ \t]+\"[^\"]*\"[ \t]+)?"
        r"fn[ \t]+" + re.escape(name) + r"\b",
        re.M,
    )


class CannotAnswer(Exception):
    """A required region could not be located. Never degrade this to a pass."""


def strip_rust_comments(source: str) -> str:
    """Remove comments without touching string or char literals.

    Over-stripping is the dangerous direction: it would make the fingerprint
    blind to a real analysis change, which is a silent false pass. A `//` inside
    a string literal must therefore survive, and self-test arms assert that
    direction explicitly rather than only asserting that comments are removed.
    """
    out: list[str] = []
    i = 0
    n = len(source)
    while i < n:
        ch = source[i]
        nxt = source[i + 1] if i + 1 < n else ""

        # Raw string literal: r"..." or r#"..."#  (no escape processing inside)
        if ch == "r" and (nxt == '"' or nxt == "#"):
            j = i + 1
            hashes = 0
            while j < n and source[j] == "#":
                hashes += 1
                j += 1
            if j < n and source[j] == '"':
                terminator = '"' + "#" * hashes
                end = source.find(terminator, j + 1)
                if end == -1:
                    raise CannotAnswer("unterminated raw string literal")
                out.append(source[i : end + len(terminator)])
                i = end + len(terminator)
                continue

        if ch == '"':
            j = i + 1
            while j < n:
                if source[j] == "\\":
                    j += 2
                    continue
                if source[j] == '"':
                    break
                j += 1
            if j >= n:
                raise CannotAnswer("unterminated string literal")
            out.append(source[i : j + 1])
            i = j + 1
            continue

        # Char literal. Lifetimes ('a) look similar, so only consume a genuine
        # closing quote; otherwise fall through and treat it as ordinary text.
        if ch == "'":
            if i + 2 < n and source[i + 1] == "\\":
                end = source.find("'", i + 2)
                if end != -1:
                    out.append(source[i : end + 1])
                    i = end + 1
                    continue
            elif i + 2 < n and source[i + 2] == "'":
                out.append(source[i : i + 3])
                i += 3
                continue

        if ch == "/" and nxt == "/":
            end = source.find("\n", i)
            i = n if end == -1 else end
            continue

        if ch == "/" and nxt == "*":
            depth = 1
            j = i + 2
            while j < n and depth:
                if source.startswith("/*", j):
                    depth += 1
                    j += 2
                elif source.startswith("*/", j):
                    depth -= 1
                    j += 2
                else:
                    j += 1
            if depth:
                raise CannotAnswer("unterminated block comment")
            out.append(" ")
            i = j
            continue

        out.append(ch)
        i += 1
    return "".join(out)


def extract_fn(source: str, name: str, path_label: str) -> str:
    """Return a function's full text, located by brace balance from its signature.

    Fails closed. A renamed or deleted function is "cannot answer", not "no
    analysis found, therefore nothing changed" -- the second reading is how a
    gate reports green for a tree it never inspected.

    EVERY step runs on comment-stripped text, and that is load-bearing rather
    than tidy. Searching the raw source for the body's opening brace found a
    `{` inside a comment in the SIGNATURE -- measured with
    `// registers {content,title}`, a comment a maintainer of this very function
    would plausibly write -- and then balanced from there, returning a "body"
    that was a fragment of the signature and contained no analysis at all. The
    gate reported a mismatch, and the moment anyone pasted that fingerprint in
    to clear it, every later change to the real body became invisible. A silent
    false pass, reached by fixing the red it printed.
    """
    cleaned = strip_rust_comments(source)
    matches = list(_fn_decl(name).finditer(cleaned))
    if not matches:
        raise CannotAnswer(f"{path_label}: no `fn {name}` found")
    if len(matches) > 1:
        # Taking the first of several is the same silent-wrong-region failure as
        # the two defects above, one layer out: a cfg-gated or test-module
        # namesake added ABOVE the live analyzer would be fingerprinted in its
        # place, and the gate would pass while watching the wrong code. There is
        # exactly one of each today, so this is loud rather than restrictive.
        raise CannotAnswer(
            f"{path_label}: {len(matches)} declarations of `fn {name}`; this gate "
            "pins one region and cannot tell which one analysis flows through"
        )
    match = matches[0]
    open_brace = cleaned.find("{", match.end())
    if open_brace == -1:
        raise CannotAnswer(f"{path_label}: `fn {name}` has no body")

    depth = 0
    for offset in range(open_brace, len(cleaned)):
        ch = cleaned[offset]
        if ch == "{":
            depth += 1
        elif ch == "}":
            depth -= 1
            if depth == 0:
                return cleaned[match.start() : offset + 1]
    raise CannotAnswer(f"{path_label}: `fn {name}` body is unbalanced")


def normalize(text: str) -> str:
    return " ".join(strip_rust_comments(text).split())


def fingerprint(regions: list[str]) -> str:
    digest = hashlib.sha256()
    for region in regions:
        digest.update(normalize(region).encode("utf-8"))
        digest.update(b"\x00")
    return digest.hexdigest()[:FINGERPRINT_HEX_LEN]


def read_const_value(source: str) -> str:
    """Read the LIVE constant, never a commented-out one.

    Stripping first matters because the natural way to bump this constant is to
    leave the previous line commented out above the new one. Searching raw text
    returns that corpse: measured, a commented `...OLD.v0+fp0000000000` above
    the live declaration was what the gate read. Both directions are wrong --
    a stale comment reds a healthy tree, and a comment carrying the CORRECT
    fingerprint greens a stale constant.
    """
    matches = list(CONST_DECL.finditer(strip_rust_comments(source)))
    if not matches:
        raise CannotAnswer(f"{CONST_FILE.name}: no `{CONST_NAME}` string constant found")
    if len(matches) > 1:
        raise CannotAnswer(
            f"{CONST_FILE.name}: {len(matches)} live declarations of {CONST_NAME}; "
            "the one the corpus revision hashes is not determinable from text"
        )
    return matches[0].group("value")


def corpus_revision_references_const(source: str) -> bool:
    """Is the constant still hashed into the corpus revision?

    Without this the gate has a blind spot big enough to swallow its own
    purpose: deleting the `hash_index_corpus_component` call removes the
    interlock entirely while leaving a perfectly well-formed constant behind,
    and a gate that only compares a fingerprint to a suffix would report green.
    """
    body = extract_fn(source, CORPUS_FN, HASH_SITE_FILE.name)
    return CONST_NAME in body


def evaluate(
    const_source: str, maintenance_source: str, hash_site_source: str
) -> tuple[int, list[str]]:
    lines: list[str] = []
    value = read_const_value(const_source)
    analyzer_region = extract_fn(const_source, ANALYZER_FN, CONST_FILE.name)
    maintenance_region = extract_fn(maintenance_source, MAINTENANCE_FN, MAINTENANCE_FILE.name)

    # Print the denominators. A zero-length region and a region with no analysis
    # change read identically in the verdict, so the sizes are what let a reader
    # tell "inspected and unchanged" from "never inspected".
    lines.append(
        f"[lexical-analyzer-revision] regions: {ANALYZER_FN} "
        f"{len(normalize(analyzer_region))} normalized chars, "
        f"{MAINTENANCE_FN} {len(normalize(maintenance_region))} normalized chars"
    )
    if not normalize(analyzer_region) or not normalize(maintenance_region):
        raise CannotAnswer("an analyzer region normalized to nothing")

    expected = fingerprint([analyzer_region, maintenance_region])
    lines.append(f"[lexical-analyzer-revision] {CONST_NAME} = {value!r}")
    lines.append(f"[lexical-analyzer-revision] computed analyzer fingerprint = {expected}")

    if not corpus_revision_references_const(hash_site_source):
        lines.append(
            f"[lexical-analyzer-revision] FAIL -- {CONST_NAME} is no longer hashed into "
            f"{CORPUS_FN}(). The interlock is dead: changing analysis would not "
            f"invalidate a single existing index."
        )
        return 1, lines

    marker_at = value.rfind(FINGERPRINT_MARKER)
    if marker_at == -1:
        lines.append(
            f"[lexical-analyzer-revision] FAIL -- {CONST_NAME} carries no "
            f"{FINGERPRINT_MARKER}<fingerprint> segment. Set it to "
            f"{value + FINGERPRINT_MARKER + expected!r}"
        )
        return 1, lines

    declared = value[marker_at + len(FINGERPRINT_MARKER) :]
    base = value[:marker_at]
    if declared == expected:
        lines.append(
            "[lexical-analyzer-revision] OK -- the analyzer source and the corpus "
            "revision agree, and the revision is still hashed into "
            f"{CORPUS_FN}()"
        )
        return 0, lines

    lines.append(
        f"[lexical-analyzer-revision] FAIL -- analyzer source changed without "
        f"advancing {CONST_NAME}. Declared fingerprint {declared}, computed "
        f"{expected}."
    )
    lines.append(
        "[lexical-analyzer-revision]   Existing indexes would be REUSED under the "
        "old analysis while queries use the new one."
    )
    lines.append(f"[lexical-analyzer-revision]   Set {CONST_NAME} to {base + FINGERPRINT_MARKER + expected!r}")
    lines.append(
        "[lexical-analyzer-revision]   Bump the version segment too if consumers "
        "should see a named change rather than only a new fingerprint."
    )
    return 1, lines


# ---------------------------------------------------------------------------
# Self-test
# ---------------------------------------------------------------------------

SYNTH_ANALYZER = """
pub(crate) const LEXICAL_ANALYZER_REVISION: &str = "ee.lexical_analyzer.english_stem.v1+fp%s";

/// Doc comment that must not participate in the fingerprint.
fn configure_lexical_analyzer(index: TantivyIndex) -> SearchResult<TantivyIndex> {
    // A line comment, also excluded.
    for field_name in ["content", "title"] {
        let analyzer = TextAnalyzer::builder(ExistingAnalyzer(existing))
            .filter(Stemmer::new(Language::English))
            .build();
        handle.tokenizers().register(&name, analyzer);
    }
    Ok(index)
}
"""

SYNTH_MAINTENANCE = """
fn maintenance_writer(index: &Index) -> Result<IndexWriter, IncrementalFallback> {
    index.tokenizers().register(
        "frankensearch_default",
        TextAnalyzer::builder(SimpleTokenizer::default())
            .filter(LowerCaser)
            .filter(Stemmer::new(Language::English))
            .build(),
    );
    Ok(writer)
}
"""

SYNTH_HASH_SITE = """
pub fn expected_index_corpus_revision() -> &'static CorpusRevision {
    INDEX_CORPUS_REVISION.get_or_init(|| {
        hash_index_corpus_component(&mut hasher, "lexical_analyzer", crate::search::LEXICAL_ANALYZER_REVISION);
        CorpusRevision::new(hasher.finalize())
    })
}
"""


def _synth(analyzer: str | None = None, maintenance: str | None = None) -> tuple[str, str]:
    """Return synthetic sources whose constant already carries the right fingerprint.

    The fingerprint covers the two function regions only, never the constant's
    own line, so substituting the placeholder cannot feed back into the value.
    """
    analyzer = SYNTH_ANALYZER if analyzer is None else analyzer
    maintenance = SYNTH_MAINTENANCE if maintenance is None else maintenance
    placeholder = analyzer.replace("%s", "0" * FINGERPRINT_HEX_LEN)
    fp = fingerprint(
        [
            extract_fn(placeholder, ANALYZER_FN, "synth"),
            extract_fn(maintenance, MAINTENANCE_FN, "synth"),
        ]
    )
    return analyzer.replace("%s", fp), maintenance


def self_test() -> int:
    arms: list[tuple[str, bool]] = []

    def check(label: str, condition: bool) -> None:
        arms.append((label, condition))
        print(f"{'ok  ' if condition else 'FAIL'} {label}")

    def rc(analyzer: str, maintenance: str, hash_site: str = SYNTH_HASH_SITE) -> int:
        try:
            code, _ = evaluate(analyzer, maintenance, hash_site)
            return code
        except CannotAnswer:
            return 3

    baseline_analyzer, baseline_maintenance = _synth()

    check(
        "arm01 a matching fingerprint passes",
        rc(baseline_analyzer, baseline_maintenance) == 0,
    )

    # The real bug: analysis changed, constant did not.
    no_stemmer = baseline_maintenance.replace(
        "            .filter(Stemmer::new(Language::English))\n", ""
    )
    check(
        "arm02 dropping the Stemmer filter is caught",
        rc(baseline_analyzer, no_stemmer) == 1,
    )

    dutch = baseline_maintenance.replace("Language::English", "Language::Dutch")
    check("arm03 changing the Language is caught", rc(baseline_analyzer, dutch) == 1)

    tokenizer_swap = baseline_maintenance.replace(
        "SimpleTokenizer::default()", "WhitespaceTokenizer::default()"
    )
    check("arm04 changing the tokenizer is caught", rc(baseline_analyzer, tokenizer_swap) == 1)

    lowercaser_dropped = baseline_maintenance.replace("            .filter(LowerCaser)\n", "")
    check(
        "arm05 dropping LowerCaser is caught",
        rc(baseline_analyzer, lowercaser_dropped) == 1,
    )

    analyzer_side = baseline_analyzer.replace(
        ".filter(Stemmer::new(Language::English))", ".filter(AsciiFoldingFilter)"
    )
    check(
        "arm06 a change in the OTHER analyzer region is caught",
        rc(analyzer_side, baseline_maintenance) == 1,
    )

    # Insensitivity, the other direction. These must NOT churn the fingerprint.
    comment_only = baseline_maintenance.replace(
        "    index.tokenizers().register(",
        "    // freshly reworded explanation\n    index.tokenizers().register(",
    )
    check(
        "arm07 a line-comment edit does not churn the fingerprint",
        rc(baseline_analyzer, comment_only) == 0,
    )

    block_comment = baseline_maintenance.replace(
        "    Ok(writer)", "    /* a block comment\n       over two lines */\n    Ok(writer)"
    )
    check(
        "arm08 a block-comment edit does not churn the fingerprint",
        rc(baseline_analyzer, block_comment) == 0,
    )

    reindented = baseline_maintenance.replace("            .filter(LowerCaser)", "        .filter(LowerCaser)")
    check(
        "arm09 reindentation does not churn the fingerprint",
        rc(baseline_analyzer, reindented) == 0,
    )

    doc_comment = baseline_analyzer.replace(
        "/// Doc comment that must not participate in the fingerprint.",
        "/// Completely rewritten documentation.",
    )
    check(
        "arm10 a doc-comment rewrite does not churn the fingerprint",
        rc(doc_comment, baseline_maintenance) == 0,
    )

    # Over-stripping control. If the comment scanner ate string literals, a change
    # INSIDE a literal containing `//` would become invisible -- a silent false
    # pass. This arm fails if the scanner is too aggressive.
    literal_base_analyzer, literal_base_maintenance = _synth(
        maintenance=SYNTH_MAINTENANCE.replace(
            '"frankensearch_default"', '"frankensearch_default//keep"'
        )
    )
    literal_changed = literal_base_maintenance.replace(
        '"frankensearch_default//keep"', '"frankensearch_default//CHANGED"'
    )
    check(
        "arm11 a change inside a string literal containing // is still caught",
        rc(literal_base_analyzer, literal_changed) == 1,
    )
    check(
        "arm12 that same literal baseline passes unchanged",
        rc(literal_base_analyzer, literal_base_maintenance) == 0,
    )

    # Liveness: the constant can be perfectly well-formed while the interlock is gone.
    dead_hash_site = SYNTH_HASH_SITE.replace(
        'hash_index_corpus_component(&mut hasher, "lexical_analyzer", crate::search::LEXICAL_ANALYZER_REVISION);',
        "",
    )
    check(
        "arm13 a constant no longer hashed into the corpus revision is caught",
        rc(baseline_analyzer, baseline_maintenance, dead_hash_site) == 1,
    )

    check(
        "arm14 a missing fingerprint segment is caught",
        rc(SYNTH_ANALYZER.replace("+fp%s", ""), baseline_maintenance) == 1,
    )

    check(
        "arm15 a wrong fingerprint is caught",
        rc(SYNTH_ANALYZER.replace("%s", "deadbeefff"), baseline_maintenance) == 1,
    )

    # Cannot-answer arms. None of these may report a pass.
    check(
        "arm16 a renamed analyzer fn is cannot-answer, not a pass",
        rc(baseline_analyzer.replace(ANALYZER_FN, "configure_something_else"), baseline_maintenance)
        == 3,
    )
    check(
        "arm17 a renamed maintenance fn is cannot-answer, not a pass",
        rc(baseline_analyzer, baseline_maintenance.replace(MAINTENANCE_FN, "other_writer")) == 3,
    )
    check(
        "arm18 a missing constant is cannot-answer, not a pass",
        rc(baseline_analyzer.replace(CONST_NAME, "SOMETHING_ELSE"), baseline_maintenance) == 3,
    )

    # Regression arms for two defects found reading this file back with fresh
    # eyes. Both were silent-false-pass shaped, and neither synthetic arm above
    # could see them, because both live in the step BEFORE the fingerprint: the
    # located region and the read constant.
    brace_in_signature_comment = baseline_analyzer.replace(
        "fn configure_lexical_analyzer(index: TantivyIndex) -> SearchResult<TantivyIndex> {",
        "fn configure_lexical_analyzer(index: TantivyIndex) "
        "/* registers {content,title} */ -> SearchResult<TantivyIndex> {",
    )
    check(
        "arm19 a brace inside a signature comment does not derail the region",
        rc(brace_in_signature_comment, baseline_maintenance) == 0,
    )
    check(
        "arm20 and with that comment present, a real body change is still caught",
        rc(
            brace_in_signature_comment.replace(
                ".filter(Stemmer::new(Language::English))", ".filter(AsciiFoldingFilter)"
            ),
            baseline_maintenance,
        )
        == 1,
    )

    commented_out_decl = baseline_analyzer.replace(
        "pub(crate) const",
        f'// pub(crate) const {CONST_NAME}: &str = '
        f'"ee.lexical_analyzer.OLD.v0+fp0000000000";\npub(crate) const',
        1,
    )
    check(
        "arm21 a commented-out stale declaration does not red a healthy tree",
        rc(commented_out_decl, baseline_maintenance) == 0,
    )
    commented_out_correct = SYNTH_ANALYZER.replace("%s", "deadbeefff").replace(
        "pub(crate) const",
        f'// pub(crate) const {CONST_NAME}: &str = '
        f'"{read_const_value(baseline_analyzer)}";\npub(crate) const',
        1,
    )
    check(
        "arm22 a commented-out CORRECT declaration cannot green a stale constant",
        rc(commented_out_correct, baseline_maintenance) == 1,
    )

    # Control on the real tree. Without this the whole self-test could pass
    # because the extractor returns nothing on real input -- the twin hole of
    # every synthetic suite.
    try:
        real_analyzer = extract_fn(
            CONST_FILE.read_text(encoding="utf-8"), ANALYZER_FN, CONST_FILE.name
        )
        real_maintenance = extract_fn(
            MAINTENANCE_FILE.read_text(encoding="utf-8"), MAINTENANCE_FN, MAINTENANCE_FILE.name
        )
        real_ok = (
            len(normalize(real_analyzer)) > 200
            and len(normalize(real_maintenance)) > 100
            and "Stemmer" in normalize(real_analyzer)
            and "Stemmer" in normalize(real_maintenance)
        )
    except (CannotAnswer, OSError):
        real_ok = False
    check(
        "arm23 control: both regions extract non-trivially from the real tree",
        real_ok,
    )

    # Ambiguity must be loud. Before these guards both arms returned 0 -- a PASS
    # on a tree where the gate could not tell which declaration analysis flows
    # through, which is the silent-wrong-region class one layer out.
    duplicate_fn = (
        baseline_analyzer
        + "\nfn configure_lexical_analyzer(index: TantivyIndex) "
        "-> SearchResult<TantivyIndex> {\n    Ok(index)\n}\n"
    )
    check(
        "arm24 two declarations of the analyzer fn is cannot-answer, not a pass",
        rc(duplicate_fn, baseline_maintenance) == 3,
    )
    duplicate_const = baseline_analyzer.replace(
        "pub(crate) const",
        f'pub(crate) const {CONST_NAME}: &str = '
        f'"ee.lexical_analyzer.other.v1+fp0000000000";\npub(crate) const',
        1,
    )
    check(
        "arm25 two live declarations of the constant is cannot-answer, not a pass",
        rc(duplicate_const, baseline_maintenance) == 3,
    )

    passed = sum(1 for _, ok in arms if ok)
    print(f"check-lexical-analyzer-revision self-test: {passed}/{len(arms)} arms passed")
    return 0 if passed == len(arms) else 2


def main(argv: list[str]) -> int:
    if argv[1:] == ["--self-test"]:
        return self_test()
    if argv[1:]:
        print(f"usage: {argv[0]} [--self-test]", file=sys.stderr)
        return 3
    try:
        code, lines = evaluate(
            CONST_FILE.read_text(encoding="utf-8"),
            MAINTENANCE_FILE.read_text(encoding="utf-8"),
            HASH_SITE_FILE.read_text(encoding="utf-8"),
        )
    except CannotAnswer as error:
        print(f"[lexical-analyzer-revision] CANNOT ANSWER -- {error}", file=sys.stderr)
        print(
            "[lexical-analyzer-revision] This is not a pass. Repoint the gate at the "
            "renamed surface rather than deleting the check.",
            file=sys.stderr,
        )
        return 3
    except OSError as error:
        print(f"[lexical-analyzer-revision] CANNOT ANSWER -- {error}", file=sys.stderr)
        return 3
    for line in lines:
        print(line)
    return code


if __name__ == "__main__":
    sys.exit(main(sys.argv))
