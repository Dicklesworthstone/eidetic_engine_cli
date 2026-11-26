#!/usr/bin/env python3
"""Real-corpus probe in REPORT mode (bd-reality-core-convergence-1azkt.63).

Drives an `ee` binary black-box through trust -> import -> retrieve -> pack ->
learn on the real-shape CASS corpus (.43) and computes the §16.5 outcome
metrics. Report mode never fails on a metric value; it exits non-zero only on
harness errors (corpus shape precheck, cass provisioning, import crash,
malformed JSON).

    probe.py --ee target/debug/ee [--target-records N] [--out DIR] [--queries all|evaluation]
    probe.py --ee /path/to/ee --real-cass /path/to/cass --cass-sha256 HEX --enforce --out /tmp/ee-report
    probe.py --self-test [--ee target/debug/ee]

Writes ``ee.real_corpus_probe.v2`` JSON to stdout (and DIR/summary.json), plus
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
import bisect
import hashlib
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


def read_to_string(rel_path):
    """Read a repo-relative file as UTF-8 text (tracked by check-golden-consumption.py)."""
    path = os.path.join(REPO_ROOT, rel_path)
    with open(path, encoding="utf-8") as handle:
        return handle.read()


def fail_closed_contract_snapshot():
    """Produce the canonical fail-closed contract pinned by tests/golden/probe-fail-closed.snap."""
    healthy = {
        "scenario_hit_at_5": {"value": 0.925},
        "recall_at_5": {"mean": 0.0375},
        "FAR": {"packed_items": 29},
        "UTR": {"value": 1.0},
    }
    return {
        "schema": "ee.real_corpus_probe.fail_closed_contract.v1",
        "bead": "bd-reality-core-convergence-1azkt.67",
        "registered_env_vars": {
            "model_dir": MODEL_DIR_ENV,
            "download": DOWNLOAD_ENV,
            "model_env_both": model_env("/m", "off"),
            "model_env_none": model_env(None),
        },
        "zero_guarded_metrics": [
            {"path": path, "label": label} for path, label in ZERO_GUARDED
        ],
        "zero_guard_cases": {
            "healthy": zero_guard(healthy, [], 4009, 40),
            "scale_recall_zero_allowed": zero_guard(
                dict(healthy, recall_at_5={"mean": 0.0}), [], 12009, 20
            ),
            "scenario_hit_zero_with_degraded": zero_guard(
                dict(healthy, scenario_hit_at_5={"value": 0.0}),
                [{"pack_degraded": ["index_incompatible"]}],
                4009,
                40,
            ),
            "packed_items_zero_without_degraded": zero_guard(
                dict(healthy, FAR={"packed_items": 0}), [], 4009, 40
            ),
            "utr_null_without_degraded": zero_guard(
                dict(healthy, UTR={"value": None}), [], 4009, 40
            ),
            "empty_corpus": zero_guard(healthy, [], 0, 40),
            "zero_judged_queries": zero_guard(healthy, [], 4009, 0),
        },
        "describe_envelope_cases": {
            "error_envelope_with_degraded": describe_envelope(
                {
                    "error": {
                        "code": "index_incompatible",
                        "message": "index generation does not match schema",
                        "repair": "run `ee index rebuild`",
                    },
                    "degraded": [{"code": "lexical_only_fallback"}],
                },
                "",
            ),
            "no_json_stdout": describe_envelope(None, ""),
            "no_json_stdout_with_stderr": describe_envelope(None, "fatal panic\n"),
        },
        "absent_surface_cases": {
            "present": surface_or_unavailable(
                {"data": {"evidenceAdmission": {"admitted": 988, "quarantined": 3021}}},
                "evidence_admission",
                "evidenceAdmission",
                surface="evidence admission",
            ),
            "absent": surface_or_unavailable(
                {"data": {"spansImported": 7}},
                "evidence_admission",
                "evidenceAdmission",
                surface="evidence admission",
            ),
            "zero_preserved": surface_or_unavailable(
                {"data": {"spansImported": 0}},
                "spans_imported",
                "spansImported",
                surface="imported-span count",
            ),
        },
        "provisioning_refusal_code": CASS_UNAVAILABLE,
    }


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


def provision_real_cass(binary, expected_digest, corpus, root, workspace, env, manifest):
    """Index a private copy and read the session inventory from that same CASS."""
    binary = os.path.abspath(binary)
    if (os.path.basename(binary) != "cass" or not os.path.isfile(binary)
            or not os.access(binary, os.X_OK)):
        raise Harness(f"--real-cass requires an executable named cass: {binary!r}",
                      code=CASS_UNAVAILABLE)
    assert_no_symlink_component(binary, "real cass binary")
    env["EE_CASS_BINARY"] = binary
    env["CASS_DATA_DIR"] = os.path.join(root, "cass-data")
    env["CASS_IGNORE_SOURCES_CONFIG"] = "1"
    try:
        digest = hashlib.sha256()
        with open(binary, "rb") as handle:
            for chunk in iter(lambda: handle.read(1024 * 1024), b""):
                digest.update(chunk)
        if digest.hexdigest() != expected_digest:
            raise Harness("real CASS executable does not match --cass-sha256", code=CASS_UNAVAILABLE)
        index = subprocess.run(
            [os.path.join(HERE, "index_with_real_cass.sh"), "--cass", binary,
             "--corpus", corpus, "--data-dir", root],
            cwd=workspace, env=env, capture_output=True, text=True, timeout=7200)
        if index.returncode:
            raise Harness(f"real CASS index failed ({index.returncode}): {index.stderr[-1500:]}",
                          code=CASS_UNAVAILABLE)
        receipt = json.loads(index.stdout)
        if (not isinstance(receipt, dict) or receipt.get("schema") != "ee.real_shape_cass.real_index.v1"
                or receipt.get("data_dir") != root or receipt.get("corpus") != corpus
                or receipt.get("conversations") != len(manifest["sessions"])
                or receipt.get("indexer") != "real"):
            raise Harness("real CASS index receipt does not match the generated corpus")
        listing = subprocess.run(
            [binary, "sessions", "--workspace", workspace, "--json",
             "--limit", str(len(manifest["sessions"]) + 1), "--data-dir", env["CASS_DATA_DIR"]],
            cwd=workspace, env=env, capture_output=True, text=True, timeout=900)
        if listing.returncode:
            raise Harness(f"real CASS session listing failed ({listing.returncode}): "
                          f"{listing.stderr[-1500:]}", code=CASS_UNAVAILABLE)
        counts = indexed_session_counts(json.loads(listing.stdout), root, manifest)
        receipt["binary_sha256"] = digest.hexdigest()
        return receipt, counts
    except (OSError, subprocess.TimeoutExpired, ValueError) as error:
        raise Harness(f"real CASS provisioning failed: {error}", code=CASS_UNAVAILABLE) from error


def indexed_session_counts(listing, root, manifest):
    # JSONL contains metadata and tool records. The number of messages CASS
    # discovers can differ from the number of physical lines used to grade spans.
    expected = {os.path.join(root, row["path"]): row["path"] for row in manifest["sessions"]}
    counts = {}
    sessions = listing.get("sessions") if isinstance(listing, dict) else None
    if not isinstance(sessions, list):
        identity_error("real CASS did not return a session inventory")
    for row in sessions:
        if not isinstance(row, dict):
            identity_error("invalid real CASS session row")
        path = row.get("path", row.get("source_path"))
        count = row.get("message_count")
        if (not isinstance(path, str) or path not in expected or expected[path] in counts
                or type(count) is not int or count <= 0):
            identity_error("unknown, duplicate or invalid real CASS session")
        counts[expected[path]] = count
    if len(counts) != len(expected):
        identity_error("real CASS did not index every generated session")
    return counts


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


CASS_LOCATOR_RE = re.compile(
    r"cass-session://(sess_[0-7][0-9A-HJKMNP-TV-Z]{25})#L([1-9][0-9]*)-([1-9][0-9]*)"
)
IDENTITY_ERROR = "real_corpus_identity_invalid"


def identity_error(message):
    raise Harness(message, code=IDENTITY_ERROR)


def inspection_rows(payload, table, count):
    """Read the public inspection contract, rejecting truncated or failed reads."""
    if not isinstance(payload, dict) or not isinstance(payload.get("data"), dict):
        identity_error(f"missing {table} inspection response")
    report = payload["data"].get("report", {})
    if not isinstance(report, dict):
        identity_error(f"invalid {table} inspection report")
    rows = report.get("rows")
    if (payload.get("success") is not True or report.get("table") != table
            or report.get("exists") is not True or report.get("error") is not None
            or not isinstance(rows, list) or len(rows) != count
            or type(report.get("tableRowCount")) is not int
            or type(report.get("returnedRowCount")) is not int
            or report["tableRowCount"] != count or report["returnedRowCount"] != count
            or any(not isinstance(row, dict) or not isinstance(row.get("values"), dict)
                   for row in rows)):
        identity_error(f"incomplete or invalid {table} inspection; expected exactly {count} rows")
    return [row["values"] for row in rows]


def join_imported_sessions(manifest, corpus, imported, inspected, workspace_id, indexed_counts=None):
    """Join exact persisted source identities, never response/discovery order.

    The import report deliberately redacts sourcePath. Read-only db inspection
    supplies the stored cass_session_id and source_path; both must name the same
    generated file. The report's session ID set is then cross-checked.
    """
    counts = (indexed_counts if indexed_counts is not None else
              {s["path"]: s["message_count"] for s in manifest["sessions"]})
    expected = {}
    for source in manifest["sessions"]:
        path = os.path.join(os.path.realpath(corpus), source["path"])
        if path in expected:
            identity_error("duplicate source path in corpus manifest")
        expected[path] = source
    rows = inspection_rows(inspected, "sessions", len(expected))
    path_by_session = {}
    mapped_paths = set()
    for row in rows:
        sid = row.get("id")
        source_path = row.get("cass_session_id")
        source = expected.get(source_path) if isinstance(source_path, str) else None
        if (not isinstance(sid, str) or not sid.startswith("sess_")
                or sid in path_by_session or source is None or source_path in mapped_paths
                or row.get("source_path") != source_path
                or row.get("workspace_id") != workspace_id
                or type(row.get("message_count")) is not int
                or row["message_count"] != counts[source["path"]]):
            identity_error("missing, duplicate or conflicting persisted CASS session identity")
        path_by_session[sid] = source["path"]
        mapped_paths.add(source_path)
    sessions_out = (imported or {}).get("data", {}).get("sessions")
    if not isinstance(sessions_out, list) or len(sessions_out) != len(expected):
        identity_error("import response does not cover every corpus session")
    seen = set()
    for row in sessions_out:
        if not isinstance(row, dict):
            identity_error("import session result is not an object")
        sid = row.get("sessionId")
        if (not isinstance(sid, str) or sid not in path_by_session or sid in seen
                or row.get("status") not in ("imported", "skipped")
                or type(row.get("messageCount")) is not int
                or row["messageCount"] != counts[path_by_session[sid]]):
            identity_error("import response conflicts with persisted session identities")
        seen.add(sid)
    if seen != set(path_by_session):
        identity_error("import response omitted a persisted session identity")
    return path_by_session


def locate(node, path_by_session, record_counts):
    """Resolve evidence using only typed session/line fields and canonical URIs.

    Query text, excerpts, notes and search-document URIs are not identity evidence.
    Ask's span.byteStart/byteEnd are content offsets, not transcript line numbers.
    All supplied witnesses must agree, including when one is incomplete.
    """
    if not isinstance(node, dict):
        identity_error("retrieval item is not an object")
    metadata = node.get("metadata") or {}
    if not isinstance(metadata, dict):
        identity_error("retrieval metadata is not an object")
    evidence = (node.get("entityKind") == "evidence_span"
                or metadata.get("entity_kind") == "evidence_span"
                or str(node.get("docId", "")).startswith("ev_")
                or node.get("evidenceSpanId") is not None
                or node.get("evidenceId") is not None)
    if not evidence:
        return None
    witnesses = {"session": set(), "start": set(), "end": set()}
    for fields in (node, metadata):
        for kind, names in (("session", ("sessionId", "session_id")),
                            ("start", ("startLine", "start_line")),
                            ("end", ("endLine", "end_line"))):
            for name in names:
                value = fields.get(name)
                if value is None:
                    continue
                if kind == "session":
                    if not isinstance(value, str) or not value:
                        identity_error("invalid typed evidence session ID")
                else:
                    if isinstance(value, str) and re.fullmatch(r"[1-9][0-9]*", value):
                        value = int(value)
                    if type(value) is not int or value < 1:
                        identity_error("invalid typed evidence line number")
                witnesses[kind].add(value)
    uris = [node.get("provenanceUri"), metadata.get("provenance_uri")]
    provenance = node.get("provenance") or []
    if not isinstance(provenance, list) or any(not isinstance(p, dict) for p in provenance):
        identity_error("invalid evidence provenance list")
    uris.extend(p.get("uri") for p in provenance)
    for uri in uris:
        if uri is None:
            continue
        if not isinstance(uri, str):
            identity_error("invalid evidence provenance URI")
        if not uri.startswith("cass-session:"):
            continue
        match = CASS_LOCATOR_RE.fullmatch(uri)
        if match is None:
            identity_error("malformed canonical CASS evidence URI")
        sid, start, end = match.groups()
        witnesses["session"].add(sid)
        witnesses["start"].add(int(start))
        witnesses["end"].add(int(end))
    if any(len(values) != 1 for values in witnesses.values()):
        identity_error("missing or conflicting evidence session/line provenance")
    sid = next(iter(witnesses["session"]))
    start = next(iter(witnesses["start"]))
    end = next(iter(witnesses["end"]))
    path = path_by_session.get(sid)
    if path is None or path not in record_counts or not 1 <= start <= end <= record_counts[path]:
        identity_error("evidence locator names an unknown session or invalid transcript range")
    return path, start, end


def label_index(label_instances):
    """Index exact concept locations once; range lookups need no corpus scan."""
    by_path = {}
    for label, instances in label_instances.items():
        for where in instances:
            by_path.setdefault(where["path"], []).append((where["line"], label))
    return {path: sorted(rows) for path, rows in by_path.items()}


def labels_hit(loc, indexed):
    if loc is None:
        return set()
    path, start, end = loc
    rows = indexed.get(path, [])
    first = bisect.bisect_left(rows, (start, ""))
    labels = set()
    for index in range(first, len(rows)):
        line, label = rows[index]
        if line > end:
            break
        labels.add(label)
    return labels


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


def apply_plausible_irrelevant_control(metrics):
    """Mutate a metric summary as if packs were flooded with readable, plausible
    but judged-irrelevant prose (bd-reality-core-convergence-1azkt.50).

    UTR stays high (>= 0.99, zero JSON scaffolding) and named-distractor FAR
    stays low (<= 0.05, because the injected prose is not one of the explicitly
    labelled distractors), yet exact relevance and query coverage collapse.
    The relevance gate in evaluate_enforced_metrics must reject this.
    """
    mutated = json.loads(json.dumps(metrics))
    far = dict(mutated.get("FAR") or {})
    far["packed_items"] = max(2000, far.get("packed_items") or 2000)
    far["distractor_items"] = min(far.get("distractor_items") or 0, int(far["packed_items"] * 0.01))
    far["value"] = round(far["distractor_items"] / far["packed_items"], 4)
    far["relevant_items"] = 0
    mutated["FAR"] = far
    utr = dict(mutated.get("UTR") or {})
    utr["value"] = max(0.995, utr.get("value") or 0.995)
    mutated["UTR"] = utr
    mutated["pack_scenario_precision"] = {"value": 0.0}
    mutated["pack_exact_precision"] = {"value": 0.0}
    coverage = mutated.get("pack_query_coverage") or {}
    mutated["pack_query_coverage"] = {"value": 0.0, "relevant_queries": 0,
                                      "queries": coverage.get("queries", 40)}
    mutated["pack_precision"] = {
        "value": 0.0,
        "definition": "packed items from a session holding a judged-relevant record / packed items",
        "items_per_pack": round(far["packed_items"] / 40.0, 2),
    }
    return mutated


def evaluate_enforced_metrics(metrics, total_records, query_count, enforce_latency=False):
    """Evaluate §16.5 outcome metrics against bd-reality-core-convergence-1azkt.50 thresholds.

    Returns a dict with per-check rows and a `failures` list for checks in
    `state == "enforce"`.
    """
    checks = []
    failures = []

    def record(name, state, observed, threshold, passed):
        row = {
            "check": name,
            "state": state,
            "observed": observed,
            "threshold": threshold,
            "passed": bool(passed),
        }
        checks.append(row)
        if state == "enforce" and not passed:
            failures.append(row)

    utr_val = (metrics.get("UTR") or {}).get("value")
    record("real_corpus_pack_usefulness_utr", "enforce", utr_val, ">= 0.95",
           isinstance(utr_val, (int, float)) and utr_val >= 0.95)

    far_obj = metrics.get("FAR") or {}
    far_val = far_obj.get("value")
    record("real_corpus_admission_far", "enforce", far_val, "<= 0.05",
           isinstance(far_val, (int, float)) and far_val <= 0.05)

    distractors_top5 = (metrics.get("equivalent_recall_at_5") or {}).get("distractors_in_top5")
    record("real_corpus_top5_no_distractors", "enforce", distractors_top5, "== 0",
           distractors_top5 == 0)

    scen_hit = (metrics.get("scenario_hit_at_5") or {}).get("value")
    record("real_corpus_scenario_hit_at_5", "enforce", scen_hit, ">= 0.85",
           isinstance(scen_hit, (int, float)) and scen_hit >= 0.85)

    exact_prec = (metrics.get("pack_exact_precision") or {}).get("value")
    coverage = metrics.get("pack_query_coverage") or {}
    relevant_queries = coverage.get("relevant_queries")
    # Preserve the half-query acceptance floor, counting each task only once.
    # Many copies of a correct answer to one task cannot compensate for misses.
    min_relevant_queries = max(1, (query_count + 1) // 2)
    record(
        "real_corpus_pack_relevance_survival",
        "enforce",
        {"pack_exact_precision": exact_prec, "relevant_queries": relevant_queries,
         "queries": coverage.get("queries")},
        f"pack_exact_precision >= 0.05 and relevant_queries >= {min_relevant_queries}/{query_count}",
        type(exact_prec) in (int, float)
        and 0.05 <= exact_prec <= 1.0
        and type(relevant_queries) is int
        and min_relevant_queries <= relevant_queries <= query_count
        and type(coverage.get("queries")) is int
        and coverage["queries"] == query_count,
    )

    pap = metrics.get("PAP") or {}
    pap_prec = pap.get("precision")
    pap_rec = pap.get("recall")
    pap_polluted = pap.get("polluted")
    record(
        "real_corpus_learn_quality",
        "enforce",
        {"precision": pap_prec, "recall": pap_rec, "polluted": pap_polluted},
        "precision >= 0.70, recall >= 0.70, polluted == 0",
        isinstance(pap_prec, (int, float))
        and pap_prec >= 0.70
        and isinstance(pap_rec, (int, float))
        and pap_rec >= 0.70
        and pap_polluted == 0,
    )

    ttfuc_p50 = (metrics.get("TTFUC") or {}).get("pack_p50_seconds")
    latency_state = "enforce" if enforce_latency else "report"
    record("real_corpus_latency", latency_state, ttfuc_p50, "<= 2.0",
           isinstance(ttfuc_p50, (int, float)) and ttfuc_p50 <= 2.0)

    wcs_slope = (metrics.get("WCS") or {}).get("slope")
    wcs_state = "enforce" if isinstance(wcs_slope, (int, float)) else "report"
    record("real_corpus_wcs_slope", wcs_state, wcs_slope, "<= 0.15",
           isinstance(wcs_slope, (int, float)) and wcs_slope <= 0.15)

    return {
        "checks": checks,
        "enforced_count": sum(1 for c in checks if c["state"] == "enforce"),
        "failed_count": len(failures),
        "failures": failures,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--ee")
    parser.add_argument("--real-cass", metavar="PATH",
                        help="index and measure with the pinned cass executable in a private home")
    parser.add_argument("--cass-sha256", metavar="HEX", help="trusted SHA-256 of the real cass executable")
    parser.add_argument("--target-records", type=int, default=0)
    parser.add_argument("--seed", type=int, default=43)
    parser.add_argument("--out")
    parser.add_argument("--queries", choices=["all", "evaluation"], default="all")
    parser.add_argument("--pack-tokens", type=int, default=3000)
    parser.add_argument("--keep", action="store_true")
    parser.add_argument("--mode", choices=["report", "enforce"], default="report",
                        help="report mode exits 0 on any valid measurement; enforce mode exits 1 "
                             "when any enforced §16.5 outcome metric misses its threshold (.50)")
    parser.add_argument("--enforce", action="store_true",
                        help="shorthand for --mode enforce")
    parser.add_argument("--enforce-latency", action="store_true",
                        help="enforce TTFUC <= 2.0s cold latency check (for release binaries)")
    parser.add_argument("--plausible-irrelevant-control", action="store_true",
                        help="negative control (.50): flood packs with readable, plausible but "
                             "judged-irrelevant prose while leaving UTR and named-distractor FAR "
                             "within target; enforce mode must reject it")
    parser.add_argument("--self-test", action="store_true",
                        help="validate the probe's own platform, diagnostic, env and fail-closed "
                             "behaviour; with --ee it also runs the end-to-end controls")
    parser.add_argument("--negative-control", action="store_true",
                        help="skip the import so every metric is a legitimate zero; the "
                             "fail-closed guard must then fire (exit 2), never publish 0.0")
    args = parser.parse_args()

    if bool(args.real_cass) != bool(args.cass_sha256):
        parser.error("--real-cass and --cass-sha256 must be supplied together")
    if args.cass_sha256 and not re.fullmatch("[a-fA-F0-9]{64}", args.cass_sha256):
        parser.error("--cass-sha256 must be a 64-digit hexadecimal SHA-256")
    if args.self_test:
        if args.real_cass:
            parser.error("--real-cass requires a probe run with --ee")
        return self_test(os.path.abspath(args.ee) if args.ee else None, keep=args.keep)
    if not args.ee:
        parser.error("--ee is required unless --self-test is given")

    mode = "enforce" if args.enforce else args.mode
    ee = os.path.abspath(args.ee)
    root = allocate_physical_root()
    out = args.out or os.path.join(root, "out")
    os.makedirs(out, exist_ok=True)
    workspace = os.path.join(root, "ws")
    corpus = os.path.join(root, "corpus")
    os.makedirs(workspace)
    summary = {"schema": "ee.real_corpus_probe.v2", "mode": mode, "ee": ee,
               "cass": os.path.abspath(args.real_cass) if args.real_cass else "stub:scripts/real_shape_corpus/cass_fixture.py",
               "cass_mode": "real" if args.real_cass else "emulator",
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
        originals = label_index({label: [where] for label, where in locators.items()})
        equivalents = label_index(manifest["label_instances"])
        record_counts = {s["path"]: s["message_count"] for s in manifest["sessions"]}
        summary["corpus"] = {
            "manifest_schema": manifest["schema"], "judgments_schema": judgments["schema"],
            "seed": args.seed, "total_records": manifest["total_records"],
            "digest": generate.tree_digest(corpus), "digest_algorithm": "blake2b-256",
        }
        summary["grading"] = {
            "schema": "ee.real_shape_cass.grading.v2", "label_scope": "exact_equivalent_records",
            "identity_join": "stored_session_source_path",
            "concepts": len(manifest["label_instances"]),
            "record_locations": sum(len(rows) for rows in manifest["label_instances"].values()),
            "note": "Original-only recall remains diagnostic. Seeded exact equivalents are graded; "
                    "adjacent unlabeled turns receive no exact credit. Pack survival requires "
                    "exact precision >= 0.05 and at least one relevant record for half the queries.",
        }
        event("corpus", sessions=len(manifest["sessions"]), records=manifest["total_records"])

        env = {k: v for k, v in os.environ.items() if not k.startswith("EE_")}
        if args.real_cass:
            # Isolate connector source roots, database overrides and discovery
            # configuration owned by the caller. Pass the same env to ee.
            env = {"PATH": os.environ.get("PATH", os.defpath)}
        env.update({
            "HOME": os.path.join(root, "home"), "XDG_CONFIG_HOME": os.path.join(root, "xdg", "config"),
            "XDG_DATA_HOME": os.path.join(root, "xdg", "data"), "XDG_CACHE_HOME": os.path.join(root, "xdg", "cache"),
            "XDG_STATE_HOME": os.path.join(root, "xdg", "state"),
            "NO_COLOR": "1",
        })
        for key in ("HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME", "XDG_STATE_HOME"):
            os.makedirs(env[key], exist_ok=True)
        indexed_root, indexed_counts = corpus, None
        if args.real_cass:
            indexed_root = root
            summary["real_index"], indexed_counts = provision_real_cass(
                args.real_cass, args.cass_sha256.lower(), corpus, root, workspace, env, manifest)
        else:
            env.update({"EE_CASS_BINARY": install_cass_fixture(root),
                        "CASS_FIXTURE_ROOT": corpus, "CASS_FIXTURE_LOG": os.path.join(out, "cass-fixture.log")})
        model_cache = os.environ.get("EE_PROBE_MODEL_DIR")
        download = os.environ.get("EE_PROBE_EMBED_DOWNLOAD")
        embedding = model_env(model_cache, download)
        env.update(embedding)
        summary["embedding"] = {"configured": embedding or "ee defaults (nothing set)"}

        v_code, v_payload, _, _ = run_ee(ee, ["version", "--json"], env, workspace)
        if v_code == 0 and isinstance(v_payload, dict):
            v_data = v_payload.get("data") or {}
            build = v_data.get("build") or {}
            summary["candidate_identity"] = {
                "version": v_data.get("version"),
                "targetTriple": build.get("targetTriple"),
                "source": v_data.get("source"),
                "frankenStack": build.get("frankenStack"),
            }

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
            inspected = {}
            for table, count in (("workspaces", 1), ("sessions", len(manifest["sessions"]))):
                inspect_code, inspected[table], _, inspect_err = run_ee(
                    ee, ["db", "inspect", table, "--workspace", workspace,
                         "--limit", str(count + 1), "--json"], env, workspace)
                if inspect_code != 0 or inspected[table] is None:
                    raise Harness(f"session identity inspection failed: "
                                  f"{describe_envelope(inspected[table], inspect_err)}",
                                  code=IDENTITY_ERROR)
            stored_workspace = inspection_rows(inspected["workspaces"], "workspaces", 1)[0]
            if (stored_workspace.get("path") != workspace
                    or not isinstance(stored_workspace.get("id"), str)):
                identity_error("stored workspace identity does not match the isolated corpus workspace")
            path_by_session = join_imported_sessions(
                manifest, indexed_root, payload, inspected["sessions"], stored_workspace["id"], indexed_counts)
            summary["grading"]["session_sources"] = path_by_session
            spans = surface_or_unavailable(payload, "spans_imported", "spansImported",
                                           surface="imported-span count")
            summary["import"] = {"seconds": round(elapsed, 3), "sessions": len(path_by_session),
                                 "spans": spans, "per_session_seconds": round(elapsed / max(1, len(path_by_session)), 3),
                                 "admission": surface_or_unavailable(
                                     payload, "evidence_admission", "evidenceAdmission",
                                     surface="evidence admission")}

        queries = [q for q in judgments["retrieval"] if args.queries == "all" or q["split"] == "evaluation"]
        recall5, distractor_hits, search_times = [], 0, []
        equivalent_recall5, equivalent_hit5, equivalent_distractors = [], [], 0
        scenario_hit5, on_scenario_items = [], 0
        scenario_of = {sess["path"]: sess["scenario"] for sess in manifest["sessions"]}
        pack_times, useful_chars, total_chars = [], 0, 0
        packed_items, packed_distractors, packed_relevant, on_topic_items = 0, 0, 0, 0
        pack_relevant_queries = 0
        per_query = []
        for q in queries:
            code, sp, elapsed, err = run_ee(ee, ["search", q["query"], "--workspace", workspace, "--limit", "10", "--json"], env, workspace)
            search_times.append(elapsed)
            if code != 0 or sp is None or sp.get("success") is not True:
                raise Harness(f"search failed ({code}): {describe_envelope(sp, err)}")
            hits = result_nodes(sp)
            hit_locs = [locate(h, path_by_session, record_counts) for h in hits]
            original_hit_labels = [labels_hit(loc, originals) for loc in hit_locs]
            hit_labels = [labels_hit(loc, equivalents) for loc in hit_locs]
            original_top5 = set().union(*original_hit_labels[:5]) if original_hit_labels else set()
            top5 = set().union(*hit_labels[:5]) if hit_labels else set()
            relevant = set(q["relevant"])
            distractors = set(q["distractors"]) | {"distract_espresso", "distract_lunch"}
            recall5.append(len(original_top5 & relevant) / len(relevant))
            equivalent_recall5.append(len(top5 & relevant) / len(relevant))
            equivalent_hit5.append(bool(top5 & relevant))
            # Scenario membership remains a separate, deliberately broad metric.
            relevant_scenarios = {locators[label]["slug"] for label in relevant}
            scenario_hit5.append(any(loc is not None and scenario_of.get(loc[0]) in relevant_scenarios
                                     for loc in hit_locs[:5]))
            distractor_hits += len(original_top5 & set(q["distractors"]))
            equivalent_distractors += len(top5 & distractors)

            code, pk, elapsed, err = run_ee(ee, ["pack", q["query"], "--workspace", workspace,
                                                 "--max-tokens", str(args.pack_tokens), "--json"], env, workspace)
            pack_times.append(elapsed)
            if code != 0 or pk is None or pk.get("success") is not True:
                raise Harness(f"pack failed ({code}): {describe_envelope(pk, err)}")
            items = pack_items(pk)
            relevant_paths = {locators[label]["path"] for label in relevant}
            q_items = []
            q_relevant = set()
            seen_locations = set()
            for item in items:
                content = item.get("content") or ""
                ratio = scaffold_ratio(content)
                total_chars += len(content)
                useful_chars += len(content) * (1 - ratio)
                loc = locate(item, path_by_session, record_counts)
                if loc is not None:
                    if loc in seen_locations:
                        identity_error("pack repeated the same exact evidence location")
                    seen_locations.add(loc)
                lab = labels_hit(loc, equivalents)
                relevant_labels = lab & relevant
                distractor_labels = lab & distractors
                packed_items += 1
                if loc is not None and loc[0] in relevant_paths:
                    on_topic_items += 1
                if loc is not None and scenario_of.get(loc[0]) in relevant_scenarios:
                    on_scenario_items += 1
                if distractor_labels:
                    packed_distractors += 1
                if relevant_labels:
                    packed_relevant += 1
                    q_relevant.update(relevant_labels)
                q_items.append({"labels": sorted(lab), "original_labels": sorted(labels_hit(loc, originals)),
                                "relevant_labels": sorted(relevant_labels), "locator": loc,
                                "classification": "distractor" if distractor_labels else "relevant" if relevant_labels
                                                  else "other_labeled" if lab else "unlabeled",
                                "scaffold": round(ratio, 3), "chars": len(content),
                                "preview": content[:120]})
            pack_relevant_queries += bool(q_relevant)
            per_query.append({"id": q["id"], "query": q["query"], "recall_at_5": recall5[-1],
                              "equivalent_recall_at_5": equivalent_recall5[-1],
                              "relevant_labels": sorted(relevant), "distractor_labels": sorted(distractors),
                              "relevant_label_count": len(relevant),
                              "retrieved_relevant_label_count": len(top5 & relevant),
                              "pack_relevant_labels": sorted(q_relevant),
                              "search_top_original_labels": [sorted(l) for l in original_hit_labels[:5]],
                              "search_top_labels": [sorted(l) for l in hit_labels[:5]],
                              "search_top_locators": hit_locs[:5],
                              "pack_items": q_items, "pack_degraded": sorted(degraded_codes(pk.get("degraded", [])))})
            event("query", id=q["id"], original_recall_at_5=recall5[-1],
                  equivalent_recall_at_5=equivalent_recall5[-1],
                  retrieved_relevant_labels=len(top5 & relevant), relevant_labels=len(relevant),
                  pack_relevant=bool(q_relevant), pack_items=len(items))

        # ask: answered with a citation from a judged scenario, or abstained.
        ask_answered, ask_cited_relevant, ask_negative_abstained = 0, 0, 0
        for q in queries:
            code, ap, elapsed, err = run_ee(ee, ["ask", q["query"], "--workspace", workspace, "--json"], env, workspace)
            data = (ap or {}).get("data", {})
            if data and not data.get("abstained", True):
                ask_answered += 1
                for citation in data.get("citations") or []:
                    loc = locate(citation, path_by_session, record_counts)
                    if labels_hit(loc, equivalents) & set(q["relevant"]):
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
                            "queries": len(recall5), "distractors_in_top5": distractor_hits,
                            "label_scope": "original_records_only", "status": "diagnostic"}
        m["equivalent_recall_at_5"] = {
            "mean": round(sum(equivalent_recall5) / len(equivalent_recall5), 4) if equivalent_recall5 else None,
            "queries": len(queries), "distractors_in_top5": equivalent_distractors,
            "definition": "mean fraction of distinct relevant concepts found in top five exact source spans",
        }
        m["equivalent_hit_at_5"] = {
            "value": round(sum(equivalent_hit5) / len(queries), 4) if queries else None,
            "definition": "queries with an exact original or seeded-equivalent relevant top-five span / queries",
        }
        m["FAR"] = {"value": round(packed_distractors / packed_items, 4) if packed_items else None,
                    "packed_items": packed_items, "distractor_items": packed_distractors,
                    "relevant_items": packed_relevant, "target": "<= 0.05"}
        m["pack_precision"] = {"value": round(on_topic_items / packed_items, 4) if packed_items else None,
                               "definition": "packed items from a session holding a judged-relevant record / packed items",
                               "items_per_pack": round(packed_items / max(1, len(queries)), 2)}
        m["scenario_hit_at_5"] = {"value": round(sum(scenario_hit5) / len(scenario_hit5), 4) if scenario_hit5 else None,
                                  "definition": "queries whose top 5 include a span from a judged scenario (or a seeded copy)"}
        m["pack_scenario_precision"] = {"value": round(on_scenario_items / packed_items, 4) if packed_items else None}
        m["pack_exact_precision"] = {
            "value": round(packed_relevant / packed_items, 4) if packed_items else None,
            "definition": "packed items containing exact judged original or seeded-equivalent records / packed items",
        }
        m["pack_query_coverage"] = {
            "value": round(pack_relevant_queries / len(queries), 4) if queries else None,
            "relevant_queries": pack_relevant_queries, "queries": len(queries),
        }
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
        if args.plausible_irrelevant_control:
            summary["metrics"] = apply_plausible_irrelevant_control(m)
            m = summary["metrics"]
            summary["plausible_irrelevant_control"] = True
        enforced = evaluate_enforced_metrics(
            m,
            manifest["total_records"],
            len(queries),
            enforce_latency=args.enforce_latency,
        )
        summary["enforced_checks"] = enforced
    except Harness as error:
        summary["harness_error"] = str(error)
        if error.code:
            summary["harness_error_code"] = error.code
        event("harness_error", error=str(error), code=error.code)
        sys.stderr.write("EE_REAL_CORPUS_RC=2\n")
        print(json.dumps(summary, indent=2))
        return 2
    finally:
        summary_path = os.path.join(out, "summary.json")
        with open(summary_path, "w") as fh:
            json.dump(summary, fh, indent=2)
        if not args.keep and not args.out:
            shutil.rmtree(root, ignore_errors=True)
    print(json.dumps(summary, indent=2))
    if mode == "enforce" and summary.get("enforced_checks", {}).get("failed_count", 0) > 0:
        for row in summary["enforced_checks"]["failures"]:
            sys.stderr.write(
                f"real-corpus-probe: FAIL check={row['check']} "
                f"observed={json.dumps(row['observed'])} "
                f"threshold={row['threshold']!r} artifact={summary_path}\n"
            )
        sys.stderr.write("EE_REAL_CORPUS_RC=1\n")
        return 1
    sys.stderr.write("EE_REAL_CORPUS_RC=0\n")
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



def exact_record_self_test(scratch, arm, healthy_metrics):
    """Exercise the production identity and grading helpers with adversarial twins."""
    import copy

    corpus = os.path.join(scratch, "exact-records")
    repeated = os.path.join(scratch, "exact-records-repeat")
    manifest = generate.generate(corpus, "/work/ledger", 43, 4000)
    generate.generate(repeated, "/work/ledger", 43, 4000)
    arm("seeded metadata and transcripts are reproducible",
        generate.tree_digest(corpus) == generate.tree_digest(repeated))
    arm("4,009-record scale corpus retains all 1,485 exact locations",
        manifest["total_records"] == 4009
        and sum(map(len, manifest["label_instances"].values())) == 1485)
    arm("scaled corpus passes strict shape and identity validation", generate.validate(corpus) == [])
    judgments = json.load(open(os.path.join(corpus, "judgments.json")))
    query = judgments["retrieval"][0]
    relevant = set(query["relevant"])
    label = sorted(relevant)[0]
    original = manifest["labels"][label]
    variant = next(loc for loc in manifest["label_instances"][label] if not loc["core"])
    originals = label_index({name: [loc] for name, loc in manifest["labels"].items()})
    equivalents = label_index(manifest["label_instances"])
    counts = {s["path"]: s["message_count"] for s in manifest["sessions"]}
    workspace_id = "wsp_00000000000000000000000001"
    session_paths = {f"sess_{index:026d}": s["path"]
                     for index, s in enumerate(manifest["sessions"], 1)}
    ids_by_path = {path: sid for sid, path in session_paths.items()}

    def inspection(rows):
        return {"success": True, "data": {"report": {
            "table": "sessions", "exists": True, "error": None,
            "tableRowCount": len(rows), "returnedRowCount": len(rows),
            "rows": [{"values": row} for row in rows],
        }}}

    rows = [{"id": sid, "workspace_id": workspace_id,
             "cass_session_id": os.path.join(corpus, path),
             "source_path": os.path.join(corpus, path), "message_count": counts[path]}
            for sid, path in session_paths.items()]
    imported = {"success": True, "data": {"sessions": [
        {"sessionId": sid, "sourcePath": "[REDACTED_PATH]", "status": "imported",
         "messageCount": counts[path]} for sid, path in reversed(list(session_paths.items()))
    ]}}
    inspected = inspection(list(reversed(rows)))
    arm("reordered import and stored rows join by exact source identity",
        join_imported_sessions(manifest, corpus, imported, inspected, workspace_id) == session_paths)

    def refuses(name, operation):
        try:
            operation()
        except Harness as error:
            arm(name, error.code == IDENTITY_ERROR)
        else:
            arm(name, False)

    sample_path = manifest["sessions"][0]["path"]
    indexed_counts = {path: max(1, count - 1) for path, count in counts.items()}
    listing = {"sessions": [{"path": os.path.join(corpus, path), "message_count": count}
                            for path, count in indexed_counts.items()]}
    arm("real CASS message counts are independent of JSONL record positions",
        indexed_session_counts(listing, corpus, manifest) == indexed_counts)
    indexed_rows = [dict(row, message_count=indexed_counts[session_paths[row["id"]]]) for row in rows]
    indexed_import = copy.deepcopy(imported)
    for row in indexed_import["data"]["sessions"]:
        row["messageCount"] = indexed_counts[session_paths[row["sessionId"]]]
    arm("real CASS source paths still identify the exact generated records after import",
        join_imported_sessions(manifest, corpus, indexed_import, inspection(indexed_rows),
                               workspace_id, indexed_counts) == session_paths)
    refuses("changed message count between real CASS discovery and EE import is rejected",
            lambda: join_imported_sessions(manifest, corpus, imported, inspection(indexed_rows),
                                           workspace_id, indexed_counts))
    for problem, entries in (("incomplete", listing["sessions"][1:]),
                           ("duplicate", [*listing["sessions"][1:], listing["sessions"][1]]),
                           ("foreign source", [*listing["sessions"][1:],
                                               {"path": os.path.join("/other", sample_path),
                                                "message_count": 1}])):
        refuses(f"{problem} real CASS index is rejected",
                lambda: indexed_session_counts({"sessions": entries}, corpus, manifest))

    for name, mutate in (
            ("duplicate imported ID", lambda p: p["data"]["sessions"].__setitem__(0, p["data"]["sessions"][1])),
            ("unknown imported ID", lambda p: p["data"]["sessions"][0].__setitem__("sessionId", "sess_unknown")),
            ("missing imported session", lambda p: p["data"]["sessions"].pop()),
            ("refused imported session", lambda p: p["data"]["sessions"][0].__setitem__("status", "refused")),
    ):
        bad = copy.deepcopy(imported)
        mutate(bad)
        refuses(name + " is rejected", lambda: join_imported_sessions(
            manifest, corpus, bad, inspected, workspace_id))
    for name, key, value in (
            ("conflicting source path", "source_path", rows[1]["source_path"]),
            ("unknown corpus source", "cass_session_id", "/elsewhere/session.jsonl"),
            ("wrong workspace", "workspace_id", "other-workspace"),
            ("wrong source length", "message_count", 0),
            ("duplicate persisted ID", "id", rows[1]["id"]),
    ):
        bad_rows = copy.deepcopy(rows)
        bad_rows[0][key] = value
        refuses(name + " is rejected", lambda: join_imported_sessions(
            manifest, corpus, imported, inspection(bad_rows), workspace_id))

    def item(where):
        sid = ids_by_path[where["path"]]
        line = where["line"]
        return {"entityKind": "evidence_span", "evidenceSpanId": "ev_example",
                "sessionId": sid, "startLine": line, "endLine": line,
                "provenance": [{"uri": f"cass-session://{sid}#L{line}-{line}"}]}

    original_item = item(original)
    variant_item = item(variant)
    original_loc = locate(original_item, session_paths, counts)
    variant_loc = locate(variant_item, session_paths, counts)
    arm("original exact record retains original and equivalent credit",
        label in labels_hit(original_loc, originals) and label in labels_hit(original_loc, equivalents))
    arm("seeded exact equivalent receives credit without changing original-only recall",
        label in labels_hit(variant_loc, equivalents) and not labels_hit(variant_loc, originals))
    occupied = {line for line, _ in equivalents[variant["path"]]}
    noise_line = next(line for line in range(1, counts[variant["path"]] + 1) if line not in occupied)
    noise = item(dict(variant, line=noise_line))
    noise["content"] = json.dumps(original_item)
    noise_loc = locate(noise, session_paths, counts)
    arm("adjacent unlabeled same-scenario noise earns no exact credit, even with a quoted source ID",
        noise_loc[0] == variant_loc[0] and labels_hit(noise_loc, equivalents) == set())
    distractor = manifest["label_instances"]["distract_espresso"][-1]
    distractor_labels = labels_hit(locate(item(distractor), session_paths, counts), equivalents)
    arm("seeded distractors remain graded negatives",
        "distract_espresso" in distractor_labels and not distractor_labels & relevant)
    arm("memory content cannot impersonate a typed evidence source",
        locate({"entityKind": "memory", "content": json.dumps(original_item)}, session_paths, counts) is None)

    sid = variant_item["sessionId"]
    line = variant["line"]
    search_item = {"docId": "ev_example", "metadata": {
        "session_id": sid, "start_line": str(line), "end_line": str(line),
        "provenance_uri": f"cass-session://{sid}#L{line}-{line}",
    }}
    arm("search string metadata resolves the same source as typed pack fields",
        locate(search_item, session_paths, counts) == variant_loc)
    citation = {"entityKind": "evidence_span", "evidenceId": "ev_example",
                "provenanceUri": f"cass-session://{sid}#L{line}-{line}",
                "span": {"byteStart": 0, "byteEnd": 700}}
    arm("ask citation byte offsets are never treated as transcript lines",
        locate(citation, session_paths, counts) == variant_loc)
    for name, bad in (
            ("conflicting URI and typed line", dict(variant_item, startLine=line + 1)),
            ("conflicting URI and session", dict(variant_item, sessionId=original_item["sessionId"])),
            ("missing provenance", {"entityKind": "evidence_span", "content": json.dumps(variant_item)}),
            ("boolean line number", dict(variant_item, startLine=True)),
            ("malformed canonical URI", dict(variant_item, provenance=[{"uri": f"cass-session://{sid}#L0-2"}])),
            ("unknown typed session", {"entityKind": "evidence_span", "sessionId": "sess_unknown",
                                       "startLine": 1, "endLine": 1}),
            ("range outside the transcript", {"entityKind": "evidence_span", "sessionId": sid,
                                               "startLine": 1, "endLine": counts[variant["path"]] + 1}),
            ("reversed transcript range", {"entityKind": "evidence_span", "sessionId": sid,
                                           "startLine": 2, "endLine": 1}),
    ):
        refuses(name + " is rejected", lambda: locate(bad, session_paths, counts))

    duplicated = copy.deepcopy(healthy_metrics)
    duplicated["FAR"]["relevant_items"] = 2000
    duplicated["pack_exact_precision"] = {"value": 1.0}
    duplicated["pack_query_coverage"] = {"value": 0.025, "relevant_queries": 1, "queries": 40}
    result = evaluate_enforced_metrics(duplicated, 4009, 40)
    arm("2,000 relevant copies from one task cannot compensate for 39 missed tasks",
        any(f["check"] == "real_corpus_pack_relevance_survival" for f in result["failures"]))
    adjacent = copy.deepcopy(healthy_metrics)
    adjacent["pack_scenario_precision"] = {"value": 1.0}
    adjacent["pack_exact_precision"] = {"value": 0.0}
    adjacent["pack_query_coverage"] = {"value": 0.0, "relevant_queries": 0, "queries": 40}
    result = evaluate_enforced_metrics(adjacent, 4009, 40)
    arm("perfect scenario membership cannot conceal exact-record retrieval misses",
        any(f["check"] == "real_corpus_pack_relevance_survival" for f in result["failures"]))
    bad_distractors = copy.deepcopy(healthy_metrics)
    bad_distractors["equivalent_recall_at_5"]["distractors_in_top5"] = 1
    result = evaluate_enforced_metrics(bad_distractors, 4009, 40)
    arm("a seeded distractor fails even when original-only top-five distractors are zero",
        any(f["check"] == "real_corpus_top5_no_distractors" for f in result["failures"]))


def self_test(ee=None, keep=False):
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

        print("GOLDEN-CONTRACT arms -- tests/golden/probe-fail-closed.snap is consumed and enforced:")
        golden_raw = read_to_string("tests/golden/probe-fail-closed.snap")
        golden_contract = json.loads(golden_raw)
        live_contract = fail_closed_contract_snapshot()
        arm("tests/golden/probe-fail-closed.snap parses as ee.real_corpus_probe.fail_closed_contract.v1",
            golden_contract.get("schema") == "ee.real_corpus_probe.fail_closed_contract.v1")
        arm("live fail-closed contract matches tests/golden/probe-fail-closed.snap",
            live_contract == golden_contract)
        mutated_contract = dict(live_contract, provisioning_refusal_code="mutated_code")
        arm("TWIN: a mutated fail-closed contract does NOT match tests/golden/probe-fail-closed.snap",
            mutated_contract != golden_contract)

        print("ENFORCE-GATE arms -- .50 per-metric enforce checks and plausible-irrelevant control:")
        sample_metrics = {
            "UTR": {"value": 0.9997, "packed_chars": 222753, "target": ">= 0.95"},
            "TTFUC": {"pack_p50_seconds": 1.637, "pack_max_seconds": 2.469, "target": "<= 2 s cold"},
            "recall_at_5": {"mean": 0.7583, "queries": 40, "distractors_in_top5": 0},
            "equivalent_recall_at_5": {"mean": 0.7583, "queries": 40, "distractors_in_top5": 0},
            "FAR": {"value": 0.0431, "packed_items": 1670, "distractor_items": 72, "relevant_items": 102, "target": "<= 0.05"},
            "scenario_hit_at_5": {"value": 0.975},
            "pack_scenario_precision": {"value": 0.0784},
            "pack_exact_precision": {"value": 0.0611},
            "pack_query_coverage": {"value": 0.5, "relevant_queries": 20, "queries": 40},
            "PAP": {"precision": 1.0, "recall": 0.8333, "proposals": 5, "polluted": 0},
            "WCS": {"remember_p50_seconds": 1.503, "corpus_records": 123},
        }
        enforced_ok = evaluate_enforced_metrics(sample_metrics, 123, 40, enforce_latency=True)
        arm("TWIN THAT MUST NOT FIRE: healthy release metrics pass all enforced checks",
            enforced_ok["failed_count"] == 0 and enforced_ok["enforced_count"] >= 7)
        bad_utr = evaluate_enforced_metrics(
            dict(sample_metrics, UTR={"value": 0.65}), 123, 40
        )
        arm("TWIN: v0.16.0-class UTR (0.65 < 0.95) is rejected by enforce gate",
            any(f["check"] == "real_corpus_pack_usefulness_utr" for f in bad_utr["failures"]))
        bad_pap = evaluate_enforced_metrics(
            dict(sample_metrics, PAP={"precision": 0.0, "recall": 0.0, "polluted": 2}), 123, 40
        )
        arm("TWIN: v0.16.0-class PAP (0/10 proposals) is rejected by enforce gate",
            any(f["check"] == "real_corpus_learn_quality" for f in bad_pap["failures"]))
        bad_lat = evaluate_enforced_metrics(
            dict(sample_metrics, TTFUC={"pack_p50_seconds": 8.7}), 123, 40, enforce_latency=True
        )
        arm("TWIN: v0.16.0-class cold latency (8.7s > 2.0s) is rejected when latency enforced",
            any(f["check"] == "real_corpus_latency" for f in bad_lat["failures"]))
        plausible_irrelevant = apply_plausible_irrelevant_control(sample_metrics)
        pi_eval = evaluate_enforced_metrics(plausible_irrelevant, 123, 40)
        arm("PLAUSIBLE-IRRELEVANT CONTROL: UTR and named-distractor FAR still pass their individual checks",
            plausible_irrelevant["UTR"]["value"] >= 0.95
            and plausible_irrelevant["FAR"]["value"] <= 0.05)
        arm("PLAUSIBLE-IRRELEVANT CONTROL: enforce gate rejects the run on relevance survival",
            any(f["check"] == "real_corpus_pack_relevance_survival" for f in pi_eval["failures"]))

        print("EXACT-RECORD arms -- variants, provenance, and query-level grading:")
        exact_record_self_test(scratch, arm, sample_metrics)

        if ee:
            print(f"END-TO-END controls against {ee}:")
            base = [sys.executable, "-I", __file__, "--ee", ee, "--target-records", "0"]
            # The control runs with downloads off so it is hermetic and offline:
            # a gate must not pull a 489 MB model, and the lexical embedder is a
            # coherent configuration that retrieves. A baseline run measuring the
            # neural path supplies EE_PROBE_MODEL_DIR instead.
            hermetic = dict(os.environ, EE_PROBE_EMBED_DOWNLOAD="off")
            hermetic.pop("EE_PROBE_MODEL_DIR", None)
            positive = subprocess.run(base + ["--enforce"], capture_output=True, text=True,
                                      timeout=7200, env=hermetic)
            summary = json.loads(positive.stdout or "{}")
            metrics = summary.get("metrics", {})
            recall = (metrics.get("recall_at_5") or {}).get("mean")
            utr = (metrics.get("UTR") or {}).get("value")
            enforced = summary.get("enforced_checks", {})
            arm(f"POSITIVE CONTROL (--enforce): the probe exits 0 (got {positive.returncode}; "
                f"harness_error={summary.get('harness_error')}; "
                f"failed_count={enforced.get('failed_count')})",
                positive.returncode == 0
                and summary.get("mode") == "enforce"
                and enforced.get("failed_count") == 0)
            arm(f"POSITIVE CONTROL: recall_at_5 mean > 0 (got {recall})",
                isinstance(recall, (int, float)) and recall > 0)
            arm(f"POSITIVE CONTROL: UTR is not null (got {utr})", utr is not None)
            arm("POSITIVE CONTROL: candidate_identity is recorded",
                isinstance((summary.get("candidate_identity") or {}).get("version"), str))
            live_pi = evaluate_enforced_metrics(
                apply_plausible_irrelevant_control(metrics), 123, 40
            )
            arm("NEGATIVE CONTROL (live metrics): plausible-irrelevant dilution is rejected by enforce gate",
                live_pi.get("failed_count", 0) > 0
                and any(f["check"] == "real_corpus_pack_relevance_survival"
                        for f in live_pi.get("failures", [])))
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
        if keep:
            print(f"Self-test artifacts retained: {scratch}")
        else:
            shutil.rmtree(scratch, ignore_errors=True)

    print(f"\n{checked - len(failures)}/{checked} arms ok")
    if failures:
        print("FAILED arms:")
        for label in failures:
            print(f"  - {label}")
        print("EE_REAL_CORPUS_SELF_TEST_RC=1")
        return 1
    print("EE_REAL_CORPUS_SELF_TEST_RC=0")
    return 0


if __name__ == "__main__":
    sys.exit(main())
