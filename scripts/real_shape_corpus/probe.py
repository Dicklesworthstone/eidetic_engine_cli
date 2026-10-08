#!/usr/bin/env python3
"""Real-corpus probe in REPORT mode (bd-reality-core-convergence-1azkt.63).

Drives an `ee` binary black-box through trust -> import -> retrieve -> pack ->
learn on the real-shape CASS corpus (.43) and computes the §16.5 outcome
metrics. Report mode never fails on a metric value; it exits non-zero only on
harness errors (corpus shape precheck, cass provisioning, import crash,
malformed JSON).

    probe.py --ee target/debug/ee [--target-records N] [--out DIR] [--queries all|evaluation]

Writes ``ee.real_corpus_probe.v1`` JSON to stdout (and DIR/summary.json), plus
one ``ee.test_event.v1`` line per step on stderr. A metric whose surface is
missing reports {"status": "unavailable", "reason": ...}; never zero.
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
sys.path.insert(0, HERE)
import generate  # noqa: E402

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
    pass


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
    nobody else can write, so the emulator is installed as one."""
    bin_dir = os.path.join(root, "bin")
    os.makedirs(bin_dir, exist_ok=True)
    os.chmod(bin_dir, 0o755)
    target = os.path.join(bin_dir, "cass")
    shutil.copyfile(os.path.join(HERE, "cass_fixture.py"), target)
    os.chmod(target, 0o555)
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
    parser.add_argument("--ee", required=True)
    parser.add_argument("--target-records", type=int, default=0)
    parser.add_argument("--seed", type=int, default=43)
    parser.add_argument("--out")
    parser.add_argument("--queries", choices=["all", "evaluation"], default="all")
    parser.add_argument("--pack-tokens", type=int, default=3000)
    parser.add_argument("--keep", action="store_true")
    args = parser.parse_args()

    ee = os.path.abspath(args.ee)
    root = tempfile.mkdtemp(prefix="ee-real-corpus-")
    out = args.out or os.path.join(root, "out")
    os.makedirs(out, exist_ok=True)
    workspace = os.path.join(root, "ws")
    corpus = os.path.join(root, "corpus")
    os.makedirs(workspace)
    summary = {"schema": "ee.real_corpus_probe.v1", "mode": "report", "ee": ee,
               "cass": "stub:scripts/real_shape_corpus/cass_fixture.py", "metrics": {}}
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
        if model_cache:
            env["EE_MODEL_DIR"] = model_cache

        code, payload, elapsed, err = run_ee(ee, ["init", "--workspace", workspace, "--json"], env, workspace)
        event("init", exit=code, seconds=round(elapsed, 3))
        if code != 0:
            raise Harness(f"ee init failed ({code}): {err[-400:]}")

        code, payload, elapsed, err = run_ee(
            ee, ["import", "cass", "--workspace", workspace, "--limit", "100000", "--json"], env, workspace, timeout=7200)
        json.dump(payload, open(os.path.join(out, "import.json"), "w"), indent=1)
        event("import", exit=code, seconds=round(elapsed, 3))
        if code != 0 or payload is None:
            raise Harness(f"ee import cass failed ({code}): {err[-800:]}")
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
        spans = find_key(payload, "spans_imported") or find_key(payload, "spansImported")
        summary["import"] = {"seconds": round(elapsed, 3), "sessions": len(sessions_out),
                             "spans": spans, "per_session_seconds": round(elapsed / max(1, len(sessions_out)), 3),
                             "admission": find_key(payload, "evidence_admission") or find_key(payload, "evidenceAdmission")}

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
                raise Harness(f"search returned no JSON ({code}): {err[-400:]}")
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
                raise Harness(f"pack returned no JSON ({code}): {err[-400:]}")
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
                raise Harness(f"ee remember failed ({code}): {err[-400:]}")
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
        m["PAP"] = learn.get("PAP", {"status": "unavailable", "reason": "no proposals"})
        m["WCS"] = {"remember_p50_seconds": med(write_times), "corpus_records": manifest["total_records"],
                    "note": "slope = d log(remember_p50) / d log(corpus_records) across runs at several --target-records"}
        summary["per_query"] = per_query
    except Harness as error:
        summary["harness_error"] = str(error)
        event("harness_error", error=str(error))
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


if __name__ == "__main__":
    sys.exit(main())
