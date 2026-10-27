#!/usr/bin/env python3
"""Real-corpus probe in REPORT mode (bd-reality-core-convergence-1azkt.63).

Drives an `ee` binary black-box through trust -> import -> retrieve -> pack ->
learn on the real-shape CASS corpus (.43) and computes the §16.5 outcome
metrics. Report mode never fails on a metric value; it exits non-zero only on
harness errors (corpus shape precheck, cass provisioning, import crash,
malformed JSON).

    probe.py --ee target/debug/ee [--target-records N] [--out DIR] [--queries all|evaluation]
    probe.py --self-test [--ee target/debug/ee]

Writes ``ee.real_corpus_probe.v1`` JSON to stdout (and DIR/summary.json), plus
one ``ee.test_event.v1`` line per step on stderr. A metric whose surface is
missing reports {"status": "unavailable", "reason": ...}; never zero.

FAIL-CLOSED (bd-reality-core-convergence-1azkt.67). Report mode never fails on a
metric VALUE, but a value indistinguishable from "the harness never ran" is not a
measurement. Three defects made this probe constant-red on macOS while reporting
numbers that read exactly like product failure -- recall 0.0 and UTR null against
a binary that, measured by hand on the same corpus, returned 10 results per query
and UTR 1.0000:

  (a) tempfile.mkdtemp() yields /var/folders/... on macOS, and /var is a symlink,
      so ee's CASS path safety correctly refuses the emulator and the probe dies
      at the import step. Fixed by allocate_physical_root(), which transliterates
      the trusted-target-root resolution in tests/no_mocks_e2e.rs rather than
      deriving a second symlink policy.
  (b) harness errors reported proc.stderr, but ee writes ee.error.v2 envelopes to
      STDOUT, so every message was "... failed (5): " with no reason. Fixed by
      describe_envelope(), which is now the only way this file reports a failure.
  (c) the model passthrough exported EE_MODEL_DIR, which is not in
      src/config/env_registry.rs. The registered name is EE_EMBED_MODEL_DIR; it is
      MODEL_DIR_ENV below and --self-test cross-checks it against the registry.

So a measured zero on a corpus that must not be empty now exits non-zero as a
harness error (zero_guard) carrying the degraded codes that caused it, and
--self-test proves both directions of that guard plus (a), (b) and (c).
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
REPO_ROOT = os.path.dirname(os.path.dirname(HERE))
sys.path.insert(0, HERE)
import generate  # noqa: E402

# The registered name in src/config/env_registry.rs. EE_MODEL_DIR -- what this
# probe used to export -- appears zero times there and zero times in
# docs/env_vars.md, so it configured nothing. --self-test asserts this constant
# against the registry so a rename cannot re-orphan it.
MODEL_DIR_ENV = "EE_EMBED_MODEL_DIR"
DOWNLOAD_ENV = "EE_EMBED_DOWNLOAD"
ENV_REGISTRY = os.path.join(REPO_ROOT, "src", "config", "env_registry.rs")

SCAFFOLD_PATTERNS = [
    re.compile(r'"[A-Za-z_][A-Za-z0-9_]*"\s*:'),  # JSON keys
    re.compile(r"\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b"),  # uuids
    re.compile(r"\b(?:msg|req|toolu)_[0-9A-Za-z]{10,}\b"),  # message / request / tool ids
    re.compile(r"\\[nt\"\\]"),  # JSON escapes
    re.compile(r"[{}\[\]]"),
]


def event(step, **fields):
    sys.stderr.write(json.dumps({"schema": "ee.test_event.v1", "suite": "real_corpus_probe",
                                 "step": step, **fields}) + "\n")


class Harness(Exception):
    """A harness failure, optionally carrying a stable machine-readable code.

    `.63` names a code for one specific provisioning failure, so the code
    travels on the exception and lands in the summary as `harness_error_code`.
    It is a SEPARATE field from the prose in `harness_error`: a consumer that
    substring-matches a message is reading a channel that changes whenever the
    wording is improved.
    """

    def __init__(self, message, code=None):
        super().__init__(message)
        self.code = code


# `.63` acceptance: "a missing cass binary yields real_corpus_cass_unavailable".
CASS_UNAVAILABLE = "real_corpus_cass_unavailable"


def assert_no_symlink_component(path, label):
    """Raise if any component of `path` is a symlink.

    ee's CASS path safety refuses an import binary whose path holds a symlink
    component, so the probe must live on a physical path. This mirrors
    `unique_log_dir_at` in tests/no_mocks_e2e.rs, which resolves the trusted
    target root with `canonicalize()` for the same reason (plan Part III sec A,
    2026-09-01); the policy is transliterated here, not re-derived, because the
    repo already carries symlink-workspace policy in several vocabularies
    (bd-68yht) and must not grow another.
    """
    current = os.sep
    for part in [component for component in os.path.abspath(path).split(os.sep) if component]:
        current = os.path.join(current, part)
        if os.path.islink(current):
            raise Harness(
                f"{label} {path!r} contains symlink component {current!r}; ee's CASS path "
                "safety refuses it. Resolve the root to its physical path before use."
            )


def allocate_physical_root(prefix="ee-real-corpus-"):
    """Allocate a scratch root guaranteed free of symlink components.

    On macOS `tempfile.mkdtemp()` returns /var/folders/..., and /var is a symlink
    to /private/var -- which is exactly the refusal that killed every run of this
    probe on the developer's own platform.
    """
    root = os.path.realpath(tempfile.mkdtemp(prefix=prefix))
    assert_no_symlink_component(root, "probe root")
    return root


def describe_envelope(payload, err):
    """Render an ee failure the way ee actually reports one.

    ee writes ee.error.v2 envelopes to STDOUT, so a harness that quotes stderr
    quotes the empty string. Every failure message in this file goes through
    here so that defect cannot silently return.
    """
    parts = []
    if payload is None:
        parts.append("no JSON on stdout")
    elif isinstance(payload, dict):
        error = payload.get("error")
        if isinstance(error, dict):
            for field in ("code", "message", "repair"):
                value = error.get(field)
                if value:
                    parts.append(f"{field}={value}")
        codes = sorted(degraded_codes(payload.get("degraded") or []))
        if codes:
            parts.append("degraded=" + ",".join(codes))
        if not parts:
            parts.append("stdout envelope carried no error block: " + json.dumps(payload)[:300])
    else:
        parts.append("stdout envelope was not an object: " + json.dumps(payload)[:200])
    tail = (err or "").strip()
    if tail:
        parts.append("stderr=" + tail[-300:])
    return "; ".join(parts)


# Metrics whose zero is indistinguishable from a dead harness. Each entry is
# (dotted path into summary["metrics"], human label).
#
# recall_at_5 is DELIBERATELY NOT HERE, and the reason is measured. The oracle
# reaches scale by seeding copies of each judged scenario while judgments label
# only the authored core, so at scale a correct answer is usually a seeded copy
# rather than the judged span itself: label-exact recall@5 fell 0.4583 at 502
# records to 0.0375 at 4009 and to exactly 0.0 at 12009, on a product that was
# retrieving correctly the whole time (scenario_hit_at_5 stayed 1.0 / 0.925).
# Guarding on it made this guard fire on legitimate scale behaviour -- the
# "constant-red is the same defect as constant-green" failure it exists to
# prevent, reintroduced one level up.
#
# scenario_hit_at_5 is the scale-invariant form of the same question (did the
# top 5 include a span from a judged scenario, copy or original), so it is the
# correct sentinel: it can only be zero if retrieval genuinely returned nothing
# relevant, which is the condition worth failing closed on.
ZERO_GUARDED = [
    ("scenario_hit_at_5.value", "scenario_hit_at_5"),
    ("FAR.packed_items", "packed items"),
    ("UTR.value", "UTR"),
]


def zero_guard(metrics, per_query, total_records, query_count):
    """Return a harness-error string when a measured value cannot be a measurement.

    Report mode must not publish a 0.0 that is indistinguishable from "the probe
    never retrieved anything", because that is precisely what made this gate
    constant-red for three weeks while reading as a product failure. The returned
    message names the degraded codes observed across the run, which is the
    diagnostic that was missing: `index_incompatible` in every per-query entry is
    a product finding, and an empty corpus is a harness finding, and before this
    guard existed they produced byte-identical output.
    """
    if not total_records or not query_count:
        return (
            f"empty world: corpus holds {total_records} records and {query_count} judged "
            "queries, so no metric computed from it is a measurement"
        )
    observed = sorted({code for row in per_query for code in row.get("pack_degraded", [])})
    for path, label in ZERO_GUARDED:
        key, field = path.split(".")
        value = (metrics.get(key) or {}).get(field)
        if value is None or value == 0:
            return (
                f"{label} measured {value!r} on a corpus of {total_records} records and "
                f"{query_count} judged queries, which must not be empty; publishing it as a "
                "metric would make a harness failure indistinguishable from a product failure. "
                + (f"degraded codes observed: {', '.join(observed)}" if observed
                   else "no degraded codes were reported, so look at the harness first")
            )
    return None


def surface_or_unavailable(payload, *names, surface):
    """First present value, or an `unavailable` record that says why.

    `.63` acceptance: "a metric whose prerequisite surface does not exist yet
    reports `unavailable` with a reason; it never reports zero." A bare `null`
    satisfies the never-zero half and fails the other one -- it cannot be told
    apart from "the surface answered and the answer was empty". The baseline
    recorded on that bead published `admission: null` for v0.16.0, a release
    where the surface genuinely did not exist, and nothing in the output said so.

    `zero_guard` cannot cover these: it walks `metrics`, and these fields live in
    `summary["import"]`.

    The presence test is `is not None`, not truthiness. The chain this replaced
    used `or`, so a genuine zero -- zero spans imported, zero admitted -- fell
    through to the next spelling and then to None, turning a real measurement
    into an absent surface. That is the inverse of the defect this file exists
    to prevent, so --self-test asserts a zero survives.
    """
    for name in names:
        value = find_key(payload, name)
        if value is not None:
            return value
    return {"status": "unavailable",
            "reason": f"{surface} absent from this ee's import response; looked for "
                      + ", ".join(names)}


def model_env(model_cache, download=None):
    """The embedding passthrough, as a dict so --self-test can assert the exported
    NAMES behaviourally instead of grepping this file for a string.

    The probe strips every EE_* variable from its child environment on purpose, so
    these two are the only route for an operator to say which embedder to measure.
    Both names are registered in src/config/env_registry.rs and --self-test asserts
    that; the probe's own knobs stay EE_PROBE_*-prefixed so they cannot be confused
    with product configuration.

    Nothing is defaulted here. The probe measures whatever ee does with no
    embedding configuration unless told otherwise, because the DEFAULT policy is
    the one users get and the one that was silently broken.
    """
    env = {}
    if model_cache:
        env[MODEL_DIR_ENV] = model_cache
    if download:
        env[DOWNLOAD_ENV] = download
    return env


def registered_env_names(path=None):
    """Every `"EE_*"` string literal in src/config/env_registry.rs."""
    source = open(path or ENV_REGISTRY).read()
    return set(re.findall(r'"(EE_[A-Z0-9_]+)"', source))


def run_ee(ee, args, env, cwd, timeout=900):
    started = time.monotonic()
    proc = subprocess.run([ee, *args], env=env, cwd=cwd, capture_output=True, text=True, timeout=timeout)
    elapsed = time.monotonic() - started
    payload = None
    if proc.stdout.strip():
        try:
            payload = json.loads(proc.stdout)
        except json.JSONDecodeError as error:
            raise Harness(f"malformed JSON from `ee {' '.join(args[:3])}`: {error}: {proc.stdout[:400]}")
    return proc.returncode, payload, elapsed, proc.stderr


def install_cass_fixture(root):
    """ee only runs an import binary literally named `cass` from a directory
    nobody else can write, so the emulator is installed as one.

    A missing emulator source is a PROVISIONING failure, not a defect in this
    file, and it gets the code `.63` names for it. Without the pre-check,
    `shutil.copyfile` raises a bare FileNotFoundError traceback, which reads as
    a probe bug and sends the reader to the wrong place entirely.
    """
    source = os.path.join(HERE, "cass_fixture.py")
    if not os.path.isfile(source):
        raise Harness(
            f"cass emulator source {source!r} is missing, so no import binary can be "
            "provisioned and nothing that needs cass is measurable. This is a "
            "provisioning gap, not a product result.",
            code=CASS_UNAVAILABLE,
        )
    bin_dir = os.path.join(root, "bin")
    os.makedirs(bin_dir, exist_ok=True)
    os.chmod(bin_dir, 0o755)
    target = os.path.join(bin_dir, "cass")
    shutil.copyfile(source, target)
    os.chmod(target, 0o555)
    # The exact path ee's trust check inspects, so assert the policy here too
    # rather than trusting that the root was allocated correctly upstream.
    assert_no_symlink_component(target, "cass emulator path")
    if not os.access(target, os.X_OK):
        raise Harness(
            f"cass emulator {target!r} installed but is not executable, so ee's import "
            "would fail later with an error that names ee rather than this harness.",
            code=CASS_UNAVAILABLE,
        )
    return target


def walk(value):
    yield value
    if isinstance(value, dict):
        for v in value.values():
            yield from walk(v)
    elif isinstance(value, list):
        for v in value:
            yield from walk(v)


def find_key(value, key):
    for node in walk(value):
        if isinstance(node, dict) and key in node:
            return node[key]
    return None


LINE_RE = re.compile(r"#L(\d+)(?:-(\d+))?")


def locate(node, path_by_session):
    """Map a hit/item to (corpus-relative path, start, end) when it is evidence."""
    text = json.dumps(node)
    session = None
    for sid, path in path_by_session.items():
        if sid and sid in text:
            session = path
            break
    if session is None:
        return None
    start = find_key(node, "start_line") or find_key(node, "startLine")
    end = find_key(node, "end_line") or find_key(node, "endLine")
    if start is None:
        match = LINE_RE.search(text)
        if not match:
            return None
        start = int(match.group(1))
        end = int(match.group(2) or match.group(1))
    return session, int(start), int(end if end is not None else start)


def labels_hit(loc, locators):
    if loc is None:
        return set()
    path, start, end = loc
    return {label for label, where in locators.items()
            if where["path"] == path and start <= where["line"] <= end}


def scaffold_ratio(text):
    if not text:
        return 0.0
    masked = text
    scaffold = 0
    for pattern in SCAFFOLD_PATTERNS:
        for match in pattern.finditer(masked):
            scaffold += len(match.group(0))
        masked = pattern.sub(" ", masked)
    return min(1.0, scaffold / len(text))


def result_nodes(payload):
    """Search hits: dicts that carry a doc id and a score."""
    data = payload.get("data", payload) if isinstance(payload, dict) else {}
    for key in ("results", "hits"):
        rows = data.get(key) if isinstance(data, dict) else None
        if isinstance(rows, list):
            return rows
    return []


def pack_items(payload):
    data = payload.get("data", payload) if isinstance(payload, dict) else {}
    pack = data.get("pack", data) if isinstance(data, dict) else {}
    for key in ("items", "selected", "selectedItems"):
        rows = pack.get(key) if isinstance(pack, dict) else None
        if isinstance(rows, list):
            return rows
    return []


def degraded_codes(payload):
    codes = set()
    for node in walk(payload):
        if isinstance(node, dict) and isinstance(node.get("code"), str):
            codes.add(node["code"])
    return codes


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--ee")
    parser.add_argument("--target-records", type=int, default=0)
    parser.add_argument("--seed", type=int, default=43)
    parser.add_argument("--out")
    parser.add_argument("--queries", choices=["all", "evaluation"], default="all")
    parser.add_argument("--pack-tokens", type=int, default=3000)
    parser.add_argument("--keep", action="store_true")
    parser.add_argument("--self-test", action="store_true",
                        help="validate the probe's own platform, diagnostic, env and fail-closed "
                             "behaviour; with --ee it also runs the end-to-end controls")
    parser.add_argument("--negative-control", action="store_true",
                        help="skip the import so every metric is a legitimate zero; the "
                             "fail-closed guard must then fire (exit 2), never publish 0.0")
    args = parser.parse_args()

    if args.self_test:
        return self_test(os.path.abspath(args.ee) if args.ee else None)
    if not args.ee:
        parser.error("--ee is required unless --self-test is given")

    ee = os.path.abspath(args.ee)
    root = allocate_physical_root()
    out = args.out or os.path.join(root, "out")
    os.makedirs(out, exist_ok=True)
    workspace = os.path.join(root, "ws")
    corpus = os.path.join(root, "corpus")
    os.makedirs(workspace)
    summary = {"schema": "ee.real_corpus_probe.v1", "mode": "report", "ee": ee,
               "cass": "stub:scripts/real_shape_corpus/cass_fixture.py",
               # Recorded so a failed run is inspectable: with --out or --keep the
               # root survives, and the workspace under it is the store whose
               # readiness surfaces can then be queried by hand.
               "root": root, "workspace": workspace, "metrics": {}}
    try:
        manifest = generate.generate(corpus, workspace, args.seed, args.target_records)
        problems = generate.validate(corpus)
        if problems:
            raise Harness("corpus shape precheck failed: " + "; ".join(problems[:5]))
        judgments = json.load(open(os.path.join(corpus, "judgments.json")))
        locators = manifest["labels"]
        event("corpus", sessions=len(manifest["sessions"]), records=manifest["total_records"])

        env = {k: v for k, v in os.environ.items() if not k.startswith("EE_")}
        env.update({
            "HOME": os.path.join(root, "home"), "XDG_CONFIG_HOME": os.path.join(root, "xdg", "config"),
            "XDG_DATA_HOME": os.path.join(root, "xdg", "data"), "XDG_CACHE_HOME": os.path.join(root, "xdg", "cache"),
            "XDG_STATE_HOME": os.path.join(root, "xdg", "state"),
            "EE_CASS_BINARY": install_cass_fixture(root),
            "CASS_FIXTURE_ROOT": corpus, "CASS_FIXTURE_LOG": os.path.join(out, "cass-fixture.log"),
            "NO_COLOR": "1",
        })
        for key in ("HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME", "XDG_STATE_HOME"):
            os.makedirs(env[key], exist_ok=True)
        model_cache = os.environ.get("EE_PROBE_MODEL_DIR")
        download = os.environ.get("EE_PROBE_EMBED_DOWNLOAD")
        embedding = model_env(model_cache, download)
        env.update(embedding)
        summary["embedding"] = {"configured": embedding or "ee defaults (nothing set)"}

        code, payload, elapsed, err = run_ee(ee, ["init", "--workspace", workspace, "--json"], env, workspace)
        event("init", exit=code, seconds=round(elapsed, 3))
        if code != 0:
            raise Harness(f"ee init failed ({code}): {describe_envelope(payload, err)}")

        if args.negative_control:
            # Deliberately leave the store empty while the corpus on disk stays
            # non-empty. Every downstream metric is then a legitimate zero, which
            # is exactly the reading this probe used to publish as a product
            # failure -- so zero_guard MUST fire, and --self-test asserts it does.
            path_by_session = {}
            summary["import"] = {"status": "skipped",
                                 "reason": "negative control: the fail-closed guard must fire"}
            event("import", skipped="negative_control")
        else:
            code, payload, elapsed, err = run_ee(
                ee, ["import", "cass", "--workspace", workspace, "--limit", "100000", "--json"], env, workspace, timeout=7200)
            json.dump(payload, open(os.path.join(out, "import.json"), "w"), indent=1)
            event("import", exit=code, seconds=round(elapsed, 3))
            if code != 0 or payload is None:
                raise Harness(f"ee import cass failed ({code}): {describe_envelope(payload, err)}")
            sessions_out = find_key(payload, "sessions") or []
            # Output redaction masks sourcePath, so sessions are matched by
            # discovery order (the emulator lists newest first) and checked
            # against their record counts.
            discovered = sorted(manifest["sessions"], key=lambda s: s["started_at"], reverse=True)
            path_by_session = {}
            if len(sessions_out) == len(discovered):
                for row, source in zip(sessions_out, discovered):
                    sid = row.get("session_id") or row.get("sessionId")
                    count = row.get("messageCount") or row.get("message_count")
                    if sid and count in (None, source["message_count"]):
                        path_by_session[sid] = source["path"]
            if len(path_by_session) != len(discovered):
                raise Harness(f"could not map imported sessions to corpus files ({len(path_by_session)}/{len(discovered)})")
            spans = surface_or_unavailable(payload, "spans_imported", "spansImported",
                                           surface="imported-span count")
            summary["import"] = {"seconds": round(elapsed, 3), "sessions": len(sessions_out),
                                 "spans": spans, "per_session_seconds": round(elapsed / max(1, len(sessions_out)), 3),
                                 "admission": surface_or_unavailable(
                                     payload, "evidence_admission", "evidenceAdmission",
                                     surface="evidence admission")}

        queries = [q for q in judgments["retrieval"] if args.queries == "all" or q["split"] == "evaluation"]
        recall5, distractor_hits, search_times = [], 0, []
        scenario_hit5, on_scenario_items = [], 0
        scenario_of = {sess["path"]: sess["slug"].rsplit("-g", 1)[0] if not sess["core"] else sess["slug"]
                       for sess in manifest["sessions"]}
        pack_times, useful_chars, total_chars = [], 0, 0
        packed_items, packed_distractors, packed_relevant, on_topic_items = 0, 0, 0, 0
        per_query = []
        for q in queries:
            code, sp, elapsed, err = run_ee(ee, ["search", q["query"], "--workspace", workspace, "--limit", "10", "--json"], env, workspace)
            search_times.append(elapsed)
            if sp is None:
                raise Harness(f"search returned no JSON ({code}): {describe_envelope(sp, err)}")
            hits = result_nodes(sp)
            hit_locs = [locate(h, path_by_session) for h in hits]
            hit_labels = [labels_hit(loc, locators) for loc in hit_locs]
            top5 = set().union(*hit_labels[:5]) if hit_labels else set()
            relevant = set(q["relevant"])
            recall5.append(len(top5 & relevant) / len(relevant))
            # At scale, seeded copies of a judged scenario are equally right.
            relevant_scenarios = {locators[label]["slug"] for label in relevant}
            scenario_hit5.append(any(loc is not None and scenario_of.get(loc[0]) in relevant_scenarios
                                     for loc in hit_locs[:5]))
            distractor_hits += len(top5 & set(q["distractors"]))

            code, pk, elapsed, err = run_ee(ee, ["pack", q["query"], "--workspace", workspace,
                                                 "--max-tokens", str(args.pack_tokens), "--json"], env, workspace)
            pack_times.append(elapsed)
            if pk is None:
                raise Harness(f"pack returned no JSON ({code}): {describe_envelope(pk, err)}")
            items = pack_items(pk)
            relevant_paths = {locators[label]["path"] for label in relevant}
            q_items = []
            for item in items:
                content = item.get("content") or ""
                ratio = scaffold_ratio(content)
                total_chars += len(content)
                useful_chars += len(content) * (1 - ratio)
                loc = locate(item, path_by_session)
                lab = labels_hit(loc, locators)
                packed_items += 1
                if loc is not None and loc[0] in relevant_paths:
                    on_topic_items += 1
                if loc is not None and scenario_of.get(loc[0]) in relevant_scenarios:
                    on_scenario_items += 1
                if lab & set(q["distractors"]) or lab & {"distract_espresso", "distract_lunch"}:
                    packed_distractors += 1
                if lab & relevant:
                    packed_relevant += 1
                q_items.append({"labels": sorted(lab), "scaffold": round(ratio, 3), "chars": len(content),
                                "preview": content[:120]})
            per_query.append({"id": q["id"], "query": q["query"], "recall_at_5": recall5[-1],
                              "search_top_labels": [sorted(l) for l in hit_labels[:5]],
                              "pack_items": q_items, "pack_degraded": sorted(degraded_codes(pk.get("degraded", [])))})
            event("query", id=q["id"], recall_at_5=recall5[-1], pack_items=len(items))

        # ask: answered with a citation from a judged scenario, or abstained.
        ask_answered, ask_cited_relevant, ask_negative_abstained = 0, 0, 0
        for q in queries:
            code, ap, elapsed, err = run_ee(ee, ["ask", q["query"], "--workspace", workspace, "--json"], env, workspace)
            data = (ap or {}).get("data", {})
            if data and not data.get("abstained", True):
                ask_answered += 1
                relevant_scenarios = {locators[label]["slug"] for label in q["relevant"]}
                for citation in data.get("citations") or []:
                    loc = locate(citation, path_by_session)
                    if loc is not None and scenario_of.get(loc[0]) in relevant_scenarios:
                        ask_cited_relevant += 1
                        break
        for query in judgments["negative_queries"]:
            code, ap, elapsed, err = run_ee(ee, ["ask", query, "--workspace", workspace, "--json"], env, workspace)
            if ((ap or {}).get("data") or {}).get("abstained", False):
                ask_negative_abstained += 1
        event("ask", answered=ask_answered, cited_relevant=ask_cited_relevant,
              negative_abstained=ask_negative_abstained)

        neg_flagged = 0
        for query in judgments["negative_queries"]:
            code, sp, elapsed, err = run_ee(ee, ["search", query, "--workspace", workspace, "--json"], env, workspace)
            codes = degraded_codes(sp or {})
            if codes & {"weak_query_recall", "no_relevant_results", "weak_semantic_evidence", "low_recall_after_floor"} or not result_nodes(sp or {}):
                neg_flagged += 1

        write_times = []
        for index in range(3):
            code, wp, elapsed, err = run_ee(ee, ["remember", f"Probe write-cost sample {index}: rotate the staging bucket credentials monthly.",
                                                 "--workspace", workspace, "--json"], env, workspace)
            if code != 0:
                raise Harness(f"ee remember failed ({code}): {describe_envelope(wp, err)}")
            write_times.append(elapsed)
        event("write_cost", seconds=[round(t, 3) for t in write_times])

        learn = learn_metrics(ee, env, workspace, manifest, path_by_session, judgments, corpus)
        summary["learn"] = learn

        def med(xs):
            xs = sorted(xs)
            return round(xs[len(xs) // 2], 3) if xs else None

        m = summary["metrics"]
        m["UTR"] = {"value": round(useful_chars / total_chars, 4) if total_chars else None,
                    "packed_chars": total_chars, "target": ">= 0.95"}
        m["TTFUC"] = {"pack_p50_seconds": med(pack_times), "pack_max_seconds": round(max(pack_times), 3) if pack_times else None,
                      "target": "<= 2 s cold"}
        m["search_latency"] = {"p50_seconds": med(search_times)}
        m["recall_at_5"] = {"mean": round(sum(recall5) / len(recall5), 4) if recall5 else None,
                            "queries": len(recall5), "distractors_in_top5": distractor_hits}
        m["FAR"] = {"value": round(packed_distractors / packed_items, 4) if packed_items else None,
                    "packed_items": packed_items, "distractor_items": packed_distractors,
                    "relevant_items": packed_relevant, "target": "<= 0.05"}
        m["pack_precision"] = {"value": round(on_topic_items / packed_items, 4) if packed_items else None,
                               "definition": "packed items from a session holding a judged-relevant record / packed items",
                               "items_per_pack": round(packed_items / max(1, len(queries)), 2)}
        m["scenario_hit_at_5"] = {"value": round(sum(scenario_hit5) / len(scenario_hit5), 4) if scenario_hit5 else None,
                                  "definition": "queries whose top 5 include a span from a judged scenario (or a seeded copy)"}
        m["pack_scenario_precision"] = {"value": round(on_scenario_items / packed_items, 4) if packed_items else None}
        m["negative_query_flag_rate"] = {"value": neg_flagged / len(judgments["negative_queries"])}
        m["ask"] = {"answered_rate": round(ask_answered / len(queries), 4) if queries else None,
                    "cited_relevant_rate": round(ask_cited_relevant / len(queries), 4) if queries else None,
                    "negative_abstain_rate": round(ask_negative_abstained / len(judgments["negative_queries"]), 4)}
        m["PAP"] = learn.get("PAP", {"status": "unavailable", "reason": "no proposals"})
        m["WCS"] = {"remember_p50_seconds": med(write_times), "corpus_records": manifest["total_records"],
                    "note": "slope = d log(remember_p50) / d log(corpus_records) across runs at several --target-records"}
        summary["per_query"] = per_query
        problem = zero_guard(m, per_query, manifest["total_records"], len(queries))
        if problem:
            raise Harness(problem)
    except Harness as error:
        summary["harness_error"] = str(error)
        if error.code:
            summary["harness_error_code"] = error.code
        event("harness_error", error=str(error), code=error.code)
        print(json.dumps(summary, indent=2))
        return 2
    finally:
        with open(os.path.join(out, "summary.json"), "w") as fh:
            json.dump(summary, fh, indent=2)
        if not args.keep and not args.out:
            shutil.rmtree(root, ignore_errors=True)
    print(json.dumps(summary, indent=2))
    return 0


def learn_metrics(ee, env, workspace, manifest, path_by_session, judgments, corpus):
    session_by_slug = {}
    for sid, path in path_by_session.items():
        for s in manifest["sessions"]:
            if s["path"] == path and s["core"]:
                session_by_slug[s["slug"]] = sid
    accepted, total, polluted, arcs_with_accept = 0, 0, 0, 0
    detail = []
    for judgment in judgments["learn"]:
        sid = session_by_slug.get(judgment["arc"])
        if not sid:
            detail.append({"arc": judgment["arc"], "status": "session_not_imported"})
            continue
        code, payload, elapsed, err = run_ee(ee, ["review", "session", sid, "--propose", "--workspace", workspace, "--json"], env, workspace)
        if payload is None:
            detail.append({"arc": judgment["arc"], "status": "no_json", "exit": code, "stderr": err[-300:]})
            continue
        texts = [c.get("proposedContent") or c.get("content") or ""
                 for c in (find_key(payload, "candidates") or []) if isinstance(c, dict)]
        arc_accept = False
        for text in texts:
            total += 1
            low = text.lower()
            bad = any(marker.lower() in low for marker in judgments["must_not_propose"])
            ok = (not bad) and any(all(p in low for p in phrases) for phrases in judgment["accept_any"])
            polluted += bad
            accepted += ok
            arc_accept |= ok
        arcs_with_accept += arc_accept
        detail.append({"arc": judgment["arc"], "proposals": len(texts), "accepted": arc_accept,
                       "samples": [t[:200] for t in texts[:4]]})
    result = {"detail": detail}
    if total:
        result["PAP"] = {"precision": round(accepted / total, 4), "recall": round(arcs_with_accept / len(judgments["learn"]), 4),
                         "proposals": total, "polluted": polluted, "target": "precision >= 0.7, recall >= 0.7"}
    else:
        result["PAP"] = {"status": "unavailable", "reason": "review session --propose produced no candidates"}
    return result


ERROR_ENVELOPE_STUB = """#!/usr/bin/env python3
import json, sys
sys.stdout.write(json.dumps({"schema": "ee.error.v2", "status": "error", "error": {
    "code": "index_incompatible",
    "message": "index producer does not match the configured embedder",
    "repair": "ee index rebuild --workspace ."}}))
sys.exit(5)
"""

SILENT_STUB = """#!/usr/bin/env python3
import sys
sys.exit(7)
"""


def _write_stub(directory, name, body):
    path = os.path.join(directory, name)
    with open(path, "w") as handle:
        handle.write(body)
    os.chmod(path, 0o755)
    return path


def self_test(ee=None):
    """Validate the probe against the three defects it shipped with, both ways.

    Every arm has a twin that must FAIL, because a control nobody saw fail is
    decoration: the symlink assertion is checked against a planted symlink, the
    diagnostic against a binary that prints nothing, and the fail-closed guard
    against healthy metrics. Every arm above the END-TO-END block needs no `ee`
    (no count is named here: one went stale the first time an arm was added);
    pass --ee to add the end-to-end positive and negative controls.
    """
    failures = []
    checked = 0

    def arm(label, ok):
        nonlocal checked
        checked += 1
        print(f"  {'ok  ' if ok else 'FAIL'}  {label}")
        if not ok:
            failures.append(label)

    scratch = allocate_physical_root(prefix="ee-probe-selftest-")
    try:
        print("PLATFORM arms -- the macOS /var symlink refusal must be impossible:")
        alias_base = os.path.join(scratch, "alias-base")
        physical = os.path.join(scratch, "physical")
        os.makedirs(physical)
        os.symlink(physical, alias_base)
        # tempfile caches its directory at first use, so setting TMPDIR here does
        # nothing once allocate_physical_root has already run -- the first draft of
        # this arm passed through the real TMPDIR and proved nothing.
        previous_tmpdir = tempfile.tempdir
        tempfile.tempdir = alias_base
        try:
            allocated = allocate_physical_root(prefix="selftest-")
        finally:
            tempfile.tempdir = previous_tmpdir
        arm(f"a root allocated under a symlinked TMPDIR resolves physical: {allocated}",
            allocated.startswith(os.path.realpath(physical)))
        arm("the allocated root does not retain the symlink alias",
            not allocated.startswith(alias_base + os.sep))
        try:
            assert_no_symlink_component(os.path.join(alias_base, "bin", "cass"), "planted path")
            arm("TWIN: the symlink assertion fires on a planted symlink component", False)
        except Harness as error:
            arm("TWIN: the symlink assertion fires, naming the offending component",
                "alias-base" in str(error))
        try:
            install_cass_fixture(alias_base)
            arm("TWIN: install_cass_fixture refuses a root with a symlink component", False)
        except Harness:
            arm("TWIN: install_cass_fixture refuses a root with a symlink component", True)

        print(f"ENV arms -- the passthrough must use the REGISTERED name ({MODEL_DIR_ENV}):")
        registered = registered_env_names()
        arm(f"src/config/env_registry.rs parsed: {len(registered)} EE_* names "
            f"(an empty read would make the next two arms vacuous)", len(registered) > 20)
        arm(f"{MODEL_DIR_ENV} is registered", MODEL_DIR_ENV in registered)
        arm(f"{DOWNLOAD_ENV} is registered", DOWNLOAD_ENV in registered)
        arm("TWIN: the dead name EE_MODEL_DIR is NOT registered, so it configured nothing",
            "EE_MODEL_DIR" not in registered)
        arm(f"model_env('/m') exports exactly {MODEL_DIR_ENV}",
            model_env("/m") == {MODEL_DIR_ENV: "/m"})
        arm(f"model_env(None, 'off') exports exactly {DOWNLOAD_ENV}",
            model_env(None, "off") == {DOWNLOAD_ENV: "off"})
        arm("model_env(None) exports nothing, so the DEFAULT policy is what gets measured",
            model_env(None) == {})

        print("DIAGNOSTIC arms -- ee writes error envelopes to STDOUT, not stderr:")
        stub = _write_stub(scratch, "ee-error-stub", ERROR_ENVELOPE_STUB)
        code, payload, _, err = run_ee(stub, ["index", "status"], dict(os.environ), scratch)
        described = describe_envelope(payload, err)
        arm(f"the stub failed as expected (exit {code}, empty stderr {not err.strip()})",
            code == 5 and not err.strip())
        arm(f"the harness message carries the code: {described[:70]}",
            "index_incompatible" in described)
        arm("the harness message carries the message", "does not match" in described)
        arm("the harness message carries the repair", "ee index rebuild" in described)
        silent = _write_stub(scratch, "ee-silent-stub", SILENT_STUB)
        code, payload, _, err = run_ee(silent, ["index", "status"], dict(os.environ), scratch)
        silent_described = describe_envelope(payload, err)
        arm(f"TWIN: a binary that prints nothing reports why, not the empty string "
            f"({silent_described!r})", silent_described == "no JSON on stdout")

        print("FAIL-CLOSED arms -- a measured zero on a non-empty corpus is a harness error:")
        healthy = {"scenario_hit_at_5": {"value": 0.925}, "recall_at_5": {"mean": 0.0375},
                   "FAR": {"packed_items": 29}, "UTR": {"value": 1.0}}
        arm("healthy metrics pass the guard",
            zero_guard(healthy, [], 4009, 40) is None)
        arm("TWIN THAT MUST NOT FIRE: label-exact recall_at_5 0.0 at scale is NOT refused, "
            "because the oracle's seeded copies deflate it by design",
            zero_guard(dict(healthy, recall_at_5={"mean": 0.0}), [], 12009, 20) is None)
        problem = zero_guard(dict(healthy, scenario_hit_at_5={"value": 0.0}),
                             [{"pack_degraded": ["index_incompatible"]}], 4009, 40)
        arm(f"TWIN: scenario_hit_at_5 0.0 on 4009 records is refused: {str(problem)[:60]}",
            problem is not None and "must not be empty" in problem)
        arm("the refusal names the degraded code that caused it",
            problem is not None and "index_incompatible" in problem)
        arm("TWIN: zero packed items is refused",
            zero_guard(dict(healthy, FAR={"packed_items": 0}), [], 4009, 40) is not None)
        arm("TWIN: a null UTR is refused",
            zero_guard(dict(healthy, UTR={"value": None}), [], 4009, 40) is not None)
        empty = zero_guard(healthy, [], 0, 40)
        arm(f"an EMPTY corpus is refused distinctly, not as a product zero: {str(empty)[:50]}",
            empty is not None and "empty world" in empty)
        arm("zero judged queries is refused too", zero_guard(healthy, [], 4009, 0) is not None)

        print("ABSENT-SURFACE arms -- an absent surface says so; it is not a null:")
        present = {"data": {"evidenceAdmission": {"admitted": 988, "quarantined": 3021}}}
        arm("TWIN THAT MUST NOT FIRE: a present surface returns its own value",
            surface_or_unavailable(present, "evidence_admission", "evidenceAdmission",
                                   surface="evidence admission")
            == {"admitted": 988, "quarantined": 3021})
        absent = surface_or_unavailable({"data": {"spansImported": 7}},
                                        "evidence_admission", "evidenceAdmission",
                                        surface="evidence admission")
        arm(f"an absent surface reports status=unavailable, not null "
            f"({absent.get('status')!r})", absent.get("status") == "unavailable")
        arm("the unavailable record carries a reason naming the surface and the spellings "
            "it looked for",
            "evidence admission" in absent.get("reason", "")
            and "evidenceAdmission" in absent.get("reason", ""))
        # The inverse defect: the `or` chain this replaced dropped a genuine zero,
        # which would publish an absent surface for a real measurement of none.
        arm("TWIN: a genuine zero survives and is NOT reported as unavailable",
            surface_or_unavailable({"data": {"spansImported": 0}}, "spans_imported",
                                   "spansImported", surface="imported-span count") == 0)

        print("PROVISIONING arms -- a missing cass emulator must name itself:")
        try:
            install_cass_fixture(os.path.join(scratch, "provision-present"))
            arm("TWIN THAT MUST NOT FIRE: a present emulator source installs cleanly", True)
        except Harness as error:
            arm(f"TWIN THAT MUST NOT FIRE: a present emulator source installs cleanly "
                f"(got {error})", False)
        # Simulated by rebinding HERE, not by deleting the fixture: HERE is read
        # only by install_cass_fixture after import, so the swap is exactly this
        # scoped, and no arm of this self-test may remove a file from the repo.
        previous_here = globals()["HERE"]
        globals()["HERE"] = os.path.join(scratch, "no-fixture-here")
        os.makedirs(globals()["HERE"], exist_ok=True)
        try:
            install_cass_fixture(os.path.join(scratch, "provision-missing"))
            arm(f"a missing cass emulator raises {CASS_UNAVAILABLE}", False)
        except Harness as error:
            arm(f"a missing cass emulator raises {CASS_UNAVAILABLE} (got {error.code!r})",
                error.code == CASS_UNAVAILABLE)
            arm("the refusal names the absent source instead of reporting a metric zero",
                "cass_fixture.py" in str(error))
        finally:
            globals()["HERE"] = previous_here

        print("SHAPE-PRECHECK arms -- an envelope-stripped corpus must be refused:")
        shape_root = os.path.join(scratch, "shape")
        shape_corpus = os.path.join(shape_root, "corpus")
        shape_workspace = os.path.join(shape_root, "ws")
        os.makedirs(shape_workspace)
        shape_manifest = generate.generate(shape_corpus, shape_workspace, 43, 0)
        arm(f"TWIN THAT MUST NOT FIRE: the generated corpus passes its own precheck "
            f"({len(shape_manifest['sessions'])} sessions, "
            f"{shape_manifest['total_records']} records)",
            generate.validate(shape_corpus) == [])
        victim = os.path.join(shape_corpus, shape_manifest["sessions"][0]["path"])
        with open(victim, encoding="utf-8") as handle:
            lines = handle.read().splitlines()
        stripped = json.loads(lines[0])
        # Envelope fields, not body content: the precheck's job is to refuse a
        # corpus whose records no longer carry the shape ee's importer reads.
        removed = sorted(k for k in ("sessionId", "uuid", "timestamp") if k in stripped)
        for key in removed:
            del stripped[key]
        lines[0] = json.dumps(stripped)
        with open(victim, "w", encoding="utf-8") as handle:
            handle.write("\n".join(lines) + "\n")
        problems = generate.validate(shape_corpus)
        arm(f"an envelope-stripped record is refused by the shape precheck "
            f"({len(problems)} problem(s) after removing {removed})", bool(problems))
        arm("the refusal names the missing envelope fields, not a metric value",
            any("missing envelope fields" in problem for problem in problems))

        if ee:
            print(f"END-TO-END controls against {ee}:")
            base = [sys.executable, "-I", __file__, "--ee", ee, "--target-records", "0"]
            # The control runs with downloads off so it is hermetic and offline:
            # a gate must not pull a 489 MB model, and the lexical embedder is a
            # coherent configuration that retrieves. A baseline run measuring the
            # neural path supplies EE_PROBE_MODEL_DIR instead.
            hermetic = dict(os.environ, EE_PROBE_EMBED_DOWNLOAD="off")
            hermetic.pop("EE_PROBE_MODEL_DIR", None)
            positive = subprocess.run(base, capture_output=True, text=True, timeout=7200,
                                      env=hermetic)
            summary = json.loads(positive.stdout or "{}")
            metrics = summary.get("metrics", {})
            recall = (metrics.get("recall_at_5") or {}).get("mean")
            utr = (metrics.get("UTR") or {}).get("value")
            arm(f"POSITIVE CONTROL: the probe exits 0 (got {positive.returncode}; "
                f"harness_error={summary.get('harness_error')})", positive.returncode == 0)
            arm(f"POSITIVE CONTROL: recall_at_5 mean > 0 (got {recall})",
                isinstance(recall, (int, float)) and recall > 0)
            arm(f"POSITIVE CONTROL: UTR is not null (got {utr})", utr is not None)
            negative = subprocess.run(base + ["--negative-control"], env=hermetic,
                                      capture_output=True, text=True, timeout=7200)
            neg_summary = json.loads(negative.stdout or "{}")
            arm(f"NEGATIVE CONTROL: an un-imported store exits non-zero "
                f"(got {negative.returncode})", negative.returncode != 0)
            arm(f"NEGATIVE CONTROL: it reports a harness error, not recall 0.0: "
                f"{str(neg_summary.get('harness_error'))[:60]}",
                "must not be empty" in str(neg_summary.get("harness_error")))
        else:
            print("END-TO-END controls SKIPPED (no --ee); arms above are binary-free.")
    finally:
        shutil.rmtree(scratch, ignore_errors=True)

    print(f"\n{checked - len(failures)}/{checked} arms ok")
    if failures:
        print("FAILED arms:")
        for label in failures:
            print(f"  - {label}")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
