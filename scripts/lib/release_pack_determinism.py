"""Capture and replay the release probe's cross-process pack regression."""

import argparse
from collections import Counter, defaultdict
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timedelta, timezone
from hashlib import sha256
import json
from pathlib import Path
import subprocess
import sys
import threading
import time


STAGE = "pack_near_tie_determinism"
QUERY = "release artifact checksum"
EXPIRY_QUERY = "Lifecycle check for the sentinel compass."
PHRASES = (
    "review audit ledger", "validate quarantined evidence", "inspect native packaging",
    "compare build manifests", "archive compiler provenance", "check signing attestations",
    "review dependency changes", "verify metadata inventory", "reconcile published filenames",
    "approve prepared installers", "check artifact schemas", "confirm platform coverage",
    "analyze installed components", "retain migration history", "record test provenance",
    "confirm snapshot ownership", "record benchmark output", "capture rollback instructions",
    "check connectivity baselines", "verify inventory completeness", "review reproducibility notes",
)
MODES = {
    "instant": ("--speed", "instant"),
    "hybrid": ("--speed", "default", "--source-mode", "hybrid", "--strict-source-mode"),
}
SERIAL = 8
PARALLEL = 4
RUNS = SERIAL + PARALLEL


def file_hash(path):
    digest = sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def utc_now():
    return datetime.now(timezone.utc).replace(microsecond=0)


def require(condition, message):
    if not condition:
        raise ValueError(message)


def write_json(path, value):
    with path.open("x", encoding="utf-8") as out:
        json.dump(value, out, sort_keys=True)
        out.write("\n")


class Capture:
    def __init__(self, binary, work, captures):
        self.binary = binary.resolve(strict=True)
        self.captures = captures.resolve()
        self.workspace = work.resolve() / "workspace"
        self.workspace.mkdir(parents=True)
        self.index = work.resolve() / "index"
        self.index.mkdir()
        self.captures.mkdir()
        self.event_lock = threading.Lock()

    def event(self, kind, fields, **details):
        row = {
            "schema": "ee.test_event.v1",
            "ts": datetime.now(timezone.utc).isoformat(),
            "test_id": STAGE,
            "kind": kind,
            "fields": fields,
            **details,
        }
        with self.event_lock, (self.captures / "events.jsonl").open("a", encoding="utf-8") as out:
            out.write(json.dumps(row, sort_keys=True) + "\n")

    def run(self, label, *args, timeout=180):
        command = [str(self.binary), "--json", "--workspace", str(self.workspace), *args]
        out = self.captures / f"{label}.json"
        err = self.captures / f"{label}.stderr"
        start = time.monotonic()
        self.event("command_start", {"label": label}, command="ee", args=args)
        with out.open("xb") as stdout, err.open("xb") as stderr:
            result = subprocess.run(command, stdout=stdout, stderr=stderr, timeout=timeout)
        self.event("command_end", {"label": label}, exit_code=result.returncode,
                   elapsed_ms=round((time.monotonic() - start) * 1000),
                   stdout_hash="sha256:" + file_hash(out),
                   stderr_hash="sha256:" + file_hash(err))
        return response(out, result.returncode)

    def pack(self, label, mode, query=QUERY, as_of=None, performance=False):
        args = ["pack", query, "--index-dir", str(self.index), "--read-only",
                "--no-lens", "--max-tokens", "420", "--candidate-pool", "40", *MODES[mode]]
        if as_of is not None:
            args.extend(("--as-of", as_of.isoformat()))
        if performance:
            args.append("--explain-performance")
        return self.run(label, *args)

    def state(self, label):
        status = self.run(label, "index", "status", "--index-dir", str(self.index))
        require(status["health"] == "ready" and status["dbGeneration"] == status["indexGeneration"],
                f"{label} requires the published index to match the store")
        source = self.workspace / ".ee" / "ee.db"
        require(source.is_file(), "pack workspace database is missing")
        files = [source, source.with_name("ee.db-wal"), *sorted(self.index.rglob("*"))]
        self.record(f"{label}-files",
                    {str(path.relative_to(self.workspace.parent)): file_hash(path)
                     for path in files if path.is_file()})

    def record(self, label, value):
        target = self.captures / f"{label}.json"
        write_json(target, value)
        self.event("note", {"label": label, "sha256": file_hash(target)})

    def execute(self):
        self.run("version", "version")
        self.run("init", "init")
        self.run("model", "model", "fetch", "embedding-default", timeout=420)
        expiry = utc_now() + timedelta(minutes=20)

        def seed(item):
            n, phrase = item
            return self.run(f"seed-{n}", "remember",
                            f"Verify the release artifact checksum, then {phrase} before handoff.",
                            "--level", "procedural", "--kind", "rule", "--defer-index",
                            "--no-auto-link")

        seeds = list(enumerate(PHRASES))
        for item in seeds[:-3]:
            seed(item)
        with ThreadPoolExecutor(max_workers=3) as writers:
            list(writers.map(seed, seeds[-3:]))
        self.run("seed-expiry", "remember", EXPIRY_QUERY, "--valid-to", expiry.isoformat(),
                 "--level", "procedural", "--kind", "rule", "--defer-index", "--no-auto-link")
        self.run("rebuild", "index", "rebuild", "--index-dir", str(self.index), timeout=420)
        for mode, options in MODES.items():
            self.run(f"{mode}-search", "search", QUERY, "--index-dir", str(self.index),
                     "--limit", "40", *options)
            self.pack(f"{mode}-route", mode, performance=True)
        self.state("before")
        reference = utc_now()
        self.record("input", {
            "binarySha256": file_hash(self.binary), "reference": reference.isoformat(),
            "expiry": expiry.isoformat(), "query": QUERY, "maxTokens": 420, "candidatePool": 40,
        })
        for mode in MODES:
            for n in range(SERIAL):
                self.pack(f"{mode}-live-{n}", mode)
                self.pack(f"{mode}-historical-{n}", mode, as_of=reference)
        # Force the concurrent live cohort into a later reference minute.
        time.sleep(60 - int(time.time()) % 60)
        with ThreadPoolExecutor(max_workers=PARALLEL) as readers:
            for mode in MODES:
                for form, as_of in (("live", None), ("historical", reference)):
                    list(readers.map(
                        lambda n: self.pack(f"{mode}-{form}-{n}", mode, as_of=as_of),
                        range(SERIAL, RUNS),
                    ))
        for mode in MODES:
            self.pack(f"{mode}-before-expiry", mode, EXPIRY_QUERY, expiry - timedelta(seconds=1))
            self.pack(f"{mode}-after-expiry", mode, EXPIRY_QUERY, expiry + timedelta(seconds=1))
        self.state("after")


def response(path, code):
    envelope = json.loads(path.read_bytes())
    schema = "ee.explain.performance.v1" if path.stem.endswith("-route") else "ee.response.v2"
    require(code == 0 and envelope.get("schema") == schema
            and envelope.get("success") is True, f"{path.stem} failed (exit {code})")
    return envelope["data"]


def selection(report, scores=True):
    fields = ("memoryId", "section", "why", "provenance")
    if scores:
        fields += ("scores",)
    return {
        "items": [tuple(row[field] for field in fields) for row in report["items"]],
        "hash": report["hash"], "omissions": report["skipped"], "text": report["text"],
    }


def distinct(values):
    return len({json.dumps(value, sort_keys=True) for value in values})


def grade(captures):
    events = {}
    activity = []
    snapshots = {}
    for encoded in (captures / "events.jsonl").read_text().splitlines():
        event = json.loads(encoded)
        if event["schema"] == "ee.test_event.v1" and event["test_id"] == STAGE:
            row = event["fields"]
            if event["kind"] in ("command_start", "command_end"):
                activity.append((row["label"], event["kind"]))
            if event["kind"] == "command_end":
                require(row["label"] not in events, f"duplicate capture {row['label']}")
                events[row["label"]] = event
            if event["kind"] == "note":
                require(row["label"] not in snapshots, f"duplicate snapshot {row['label']}")
                snapshots[row["label"]] = row["sha256"]

    def read(label):
        event = events[label]
        path = captures / f"{label}.json"
        require("sha256:" + file_hash(path) == event["stdout_hash"],
                f"stdout digest mismatch for {label}")
        require("sha256:" + file_hash(captures / f"{label}.stderr") == event["stderr_hash"],
                f"stderr digest mismatch for {label}")
        return response(path, event["exit_code"])

    def snapshot(label):
        path = captures / f"{label}.json"
        require(file_hash(path) == snapshots[label], f"snapshot digest mismatch for {label}")
        return json.loads(path.read_bytes())

    def concurrent_reads(mode, form):
        parallel = {f"{mode}-{form}-{n}" for n in range(SERIAL, RUNS)}
        active = set()
        max_active = 0
        for label, kind in activity:
            if label in parallel:
                if kind == "command_start":
                    require(label not in active, f"duplicate start for {label}")
                    active.add(label)
                    max_active = max(max_active, len(active))
                else:
                    require(label in active, f"missing start for {label}")
                    active.remove(label)
        require(not active, "concurrent pack did not report completion")
        return max_active

    inputs = snapshot("input")
    require((inputs["query"], inputs["maxTokens"], inputs["candidatePool"]) == (QUERY, 420, 40),
            "pack query or budget changed")
    source_commit = read("version")["source"]["gitCommit"]
    for label in ("init", "model", "rebuild"):
        read(label)
    corpus = {read(f"seed-{n}")["memoryId"] for n in range(len(PHRASES))}
    require(len(corpus) == len(PHRASES), "near-tie corpus did not persist distinct IDs")
    expiry = read("seed-expiry")["memoryId"]
    before = read("before")
    after = read("after")
    files_before = snapshot("before-files")
    files_after = snapshot("after-files")
    durable_source = (before["health"] == after["health"] == "ready"
                      and before["dbGeneration"] == before["indexGeneration"]
                      == after["dbGeneration"] == after["indexGeneration"]
                      and bool(files_before) and files_before == files_after)
    results = {}
    for mode in MODES:
        search = read(f"{mode}-search")
        route = read(f"{mode}-route")["search"]
        lexical = Counter(item["lexicalScore"] for item in search["results"]
                          if item["docId"] in corpus and item.get("lexicalScore") is not None)
        source_counts = route["metrics"]["sourceCounts"]
        if mode == "instant":
            route_valid = not route["usesEmbeddings"] and source_counts.get("lexical", 0) >= 10
        else:
            route_valid = (route["usesEmbeddings"] and route["embedBackend"] == "neural_local"
                           and source_counts.get("hybrid", 0) >= 10
                           and search["metrics"]["sourceModeApplied"] == "hybrid")
        live = [read(f"{mode}-live-{n}") for n in range(RUNS)]
        historical = [read(f"{mode}-historical-{n}") for n in range(RUNS)]
        require(all((row["request"]["query"], row["request"]["maxTokens"],
                     row["request"]["candidatePool"]) == (QUERY, 420, 40)
                    for row in live + historical), f"{mode} pack request changed")
        cohorts = defaultdict(list)
        for result in live:
            cohorts[result["request"].get("recencyReferenceUnixMinute")].append(result["pack"])
        fixed_minutes = {result["request"].get("recencyReferenceUnixMinute") for result in historical}
        fixed_vectors = distinct(selection(result["pack"]) for result in historical)
        same_minute_vectors = max(distinct(selection(pack) for pack in cohort)
                                  for cohort in cohorts.values())
        live_vectors = distinct(selection(result["pack"], scores=False) for result in live)
        solid_fixture = all(
            len(pack["items"]) > 1 and bool(pack["skipped"]) and bool(pack["text"])
            and all(item["memoryId"] in corpus and item["section"] == "procedural_rules"
                    for item in pack["items"])
            for pack in (row["pack"] for row in live + historical)
        )
        unexpired = read(f"{mode}-before-expiry")["pack"]
        expired = read(f"{mode}-after-expiry")["pack"]
        expiry_changed = (expiry in {item["memoryId"] for item in unexpired["items"]}
                          and expiry not in {item["memoryId"] for item in expired["items"]}
                          and unexpired["hash"] != expired["hash"])
        max_tie = max(lexical.values(), default=0)
        parallel_live = concurrent_reads(mode, "live")
        parallel_historical = concurrent_reads(mode, "historical")
        valid_clocks = (None not in cohorts and len(cohorts) >= 2
                        and max(map(len, cohorts.values())) >= 2
                        and fixed_minutes == {
                            int(datetime.fromisoformat(inputs["reference"]).timestamp()) // 60
                        })
        results[mode] = {
            "pass": bool(route_valid and solid_fixture and max_tie >= 10 and valid_clocks
                         and 2 <= parallel_live <= PARALLEL and 2 <= parallel_historical <= PARALLEL
                         and expiry_changed and fixed_vectors == same_minute_vectors == live_vectors == 1),
            "routeVerified": bool(route_valid), "fixtureVerified": solid_fixture,
            "maxLexicalTie": max_tie, "expiryChangedSelection": expiry_changed,
            "clockControlsPassed": valid_clocks, "liveCohorts": sorted(map(len, cohorts.values())),
            "liveProcesses": RUNS, "historicalProcesses": RUNS, "parallelLimit": PARALLEL,
            "maxConcurrentLive": parallel_live, "maxConcurrentHistorical": parallel_historical,
            "durableLiveVectors": live_vectors, "maxVectorsWithinLiveCohort": same_minute_vectors,
            "historicalVectors": fixed_vectors,
        }
    return {
        "schema": "ee.release_pack_determinism.v1", "stage": STAGE,
        "pass": durable_source and all(mode["pass"] for mode in results.values()),
        "sourceCommit": source_commit, "binarySha256": inputs["binarySha256"],
        "durableSourceUnchanged": durable_source, "corpusCount": len(corpus), "modes": results,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    modes = parser.add_subparsers(dest="action", required=True)
    run = modes.add_parser("capture")
    run.add_argument("--binary", type=Path, required=True)
    run.add_argument("--work", type=Path, required=True)
    run.add_argument("--captures", type=Path, required=True)
    replay = modes.add_parser("grade")
    replay.add_argument("--captures", type=Path, required=True)
    args = parser.parse_args()
    capture = None
    try:
        if args.action == "capture":
            capture = Capture(args.binary, args.work, args.captures)
            capture.execute()
        result = grade(args.captures)
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
        result = {"schema": "ee.release_pack_determinism.v1", "stage": STAGE,
                  "pass": False, "error": str(error)}
    if capture is not None:
        capture.event("assert_ok" if result["pass"] else "assert_fail",
                      {"label": STAGE, "expected": "pass", "actual": result})
        print(f"EE_PACK_DETERMINISM_RC={0 if result['pass'] else 1}", file=sys.stderr)
    print(json.dumps(result, sort_keys=True))
    return 0 if result["pass"] else 1


if __name__ == "__main__":
    sys.exit(main())
