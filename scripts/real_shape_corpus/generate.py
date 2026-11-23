#!/usr/bin/env python3
"""Deterministic real-shape CASS corpus generator (bd-reality-core-convergence-1azkt.43).

Renders the authored scenarios in ``sessions.py`` into on-disk transcripts that
use the exact record envelopes Claude Code and Codex write, then optionally
expands them by seeded recombination to a target span count.

    generate.py --out DIR [--workspace PATH] [--seed N] [--target-records N]

Outputs under DIR:
  home/.claude/projects/<slug-of-workspace>/<uuid>.jsonl   Claude Code sessions
  home/.codex/sessions/2026/09/<dd>/rollout-*.jsonl          Codex sessions
  manifest.json   sessions (path, agent, timestamps, counts) + label locators
  judgments.json  retrieval / learn / admission ground truth with locators

The same arguments always produce byte-identical files: ids come from a
seeded RNG and timestamps from a fixed epoch, never from the clock.
"""

from __future__ import annotations

import argparse
import copy
import hashlib
import json
import os
import random
import sys
import uuid
from datetime import datetime, timedelta, timezone

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import sessions as S  # noqa: E402

EPOCH = datetime(2026, 9, 1, 9, 0, 0, tzinfo=timezone.utc)
CLAUDE_VERSION = "2.0.14"
CODEX_CLI_VERSION = "0.46.0"
MODEL = "claude-sonnet-4-5-20250929"

# Identifier substitutions used by seeded expansion so recombined sessions are
# distinct documents rather than byte duplicates.
CRATES = ["ledger", "relay", "vault", "quarry", "beacon", "harbor", "tally", "sprocket"]
VERSIONS = ["0.9.0", "1.2.0", "0.4.3", "2.0.1", "0.11.0", "1.0.0-rc.1"]


class Ids:
    def __init__(self, rng: random.Random):
        self.rng = rng

    def uuid(self) -> str:
        return str(uuid.UUID(int=self.rng.getrandbits(128), version=4))

    def token(self, prefix: str, n: int = 24) -> str:
        alphabet = "ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz0123456789"
        return prefix + "".join(self.rng.choice(alphabet) for _ in range(n))


def ts(t: datetime) -> str:
    return t.strftime("%Y-%m-%dT%H:%M:%S.") + f"{t.microsecond // 1000:03d}Z"


def subst(value, ws: str):
    if isinstance(value, str):
        return value.replace("{ws}", ws)
    if isinstance(value, dict):
        return {k: subst(v, ws) for k, v in value.items()}
    if isinstance(value, list):
        return [subst(v, ws) for v in value]
    return value


def expand_noise(turns):
    out = []
    for turn in turns:
        if turn[0] == "noise":
            name, inp, output, is_error = S.NOISE[turn[1]]
            out.append(("tool", name, inp, output, is_error))
        else:
            out.append(turn)
    return out


def claude_project_dir(ws: str) -> str:
    return ws.replace("/", "-").replace(".", "-")


def render_claude(session, ws, ids: Ids, start: datetime, session_id: str):
    """Return (records, labels) where labels maps label -> 1-based line."""
    records = []
    labels = {}
    parent = None
    t = start
    sidechain_from = session.get("sidechain_from")
    common = lambda: {  # noqa: E731
        "isSidechain": False,
        "userType": "external",
        "cwd": ws,
        "sessionId": session_id,
        "version": CLAUDE_VERSION,
        "gitBranch": "main",
    }
    turns = expand_noise(session["turns"])
    for index, turn in enumerate(turns):
        kind = turn[0]
        t += timedelta(seconds=7 + (index * 13) % 29)
        sidechain = sidechain_from is not None and index >= sidechain_from
        if kind == "user":
            rec = {"parentUuid": parent, **common(), "type": "user",
                   "message": {"role": "user", "content": subst(turn[1], ws)},
                   "uuid": ids.uuid(), "timestamp": ts(t)}
            if len(records) == 0:
                rec["promptId"] = ids.uuid()
        elif kind in ("assistant", "thinking"):
            block = ({"type": "text", "text": subst(turn[1], ws)} if kind == "assistant"
                     else {"type": "thinking", "thinking": subst(turn[1], ws),
                           "signature": ids.token("Eq", 64)})
            rec = {"parentUuid": parent, **common(),
                   "message": {"id": ids.token("msg_01"), "type": "message", "role": "assistant",
                               "model": MODEL, "content": [block], "stop_reason": None,
                               "stop_sequence": None,
                               "usage": {"input_tokens": 4, "cache_creation_input_tokens": 312,
                                         "cache_read_input_tokens": 18044, "output_tokens": 96,
                                         "service_tier": "standard"}},
                   "requestId": ids.token("req_011C"), "type": "assistant",
                   "uuid": ids.uuid(), "timestamp": ts(t)}
        elif kind == "tool":
            _, name, inp, output, is_error = turn[:5]
            label = turn[5] if len(turn) > 5 else None
            tool_id = ids.token("toolu_01")
            call = {"parentUuid": parent, **common(),
                    "message": {"id": ids.token("msg_01"), "type": "message", "role": "assistant",
                                "model": MODEL,
                                "content": [{"type": "tool_use", "id": tool_id, "name": name,
                                             "input": subst(inp, ws)}],
                                "stop_reason": "tool_use", "stop_sequence": None,
                                "usage": {"input_tokens": 4, "cache_creation_input_tokens": 120,
                                          "cache_read_input_tokens": 18356, "output_tokens": 61,
                                          "service_tier": "standard"}},
                    "requestId": ids.token("req_011C"), "type": "assistant",
                    "uuid": ids.uuid(), "timestamp": ts(t)}
            if sidechain:
                call["isSidechain"] = True
            records.append(call)
            if label:
                labels[label + ":call"] = len(records)
            parent = call["uuid"]
            t += timedelta(seconds=2)
            out = subst(output, ws)
            result = {"parentUuid": parent, **common(), "type": "user",
                      "message": {"role": "user", "content": [
                          {"tool_use_id": tool_id, "type": "tool_result", "content": out,
                           **({"is_error": True} if is_error else {})}]},
                      "uuid": ids.uuid(), "timestamp": ts(t),
                      "toolUseResult": ({"stdout": out if not is_error else "",
                                         "stderr": out if is_error else "",
                                         "interrupted": False, "isImage": False}
                                        if name == "Bash" else {"type": "text", "file": {"filePath": subst(inp.get("file_path", ""), ws)}}
                                        if name == "Read" else {"filePath": subst(inp.get("file_path", ""), ws)})}
            if sidechain:
                result["isSidechain"] = True
            records.append(result)
            if label:
                labels[label] = len(records)
            parent = result["uuid"]
            continue
        elif kind == "summary":
            rec = {"type": "summary", "summary": turn[1], "leafUuid": parent}
            records.append(rec)
            continue
        elif kind == "meta":
            rec = {"parentUuid": parent, **common(), "type": "user", "isMeta": True,
                   "message": {"role": "user", "content": turn[1]},
                   "uuid": ids.uuid(), "timestamp": ts(t)}
        else:
            raise ValueError(f"unknown turn kind {kind}")
        if sidechain:
            rec["isSidechain"] = True
        records.append(rec)
        label = turn[2] if kind in ("user", "assistant") and len(turn) > 2 else None
        if label:
            labels[label] = len(records)
        parent = rec["uuid"]
    return records, labels, t


def render_codex(session, ws, ids: Ids, start: datetime, session_id: str):
    records = [{"timestamp": ts(start), "type": "session_meta",
                "payload": {"id": session_id, "timestamp": ts(start), "cwd": ws,
                            "originator": "codex_cli_rs", "cli_version": CODEX_CLI_VERSION,
                            "instructions": None,
                            "git": {"commit_hash": ids.token("", 40).lower(), "branch": "main"}}}]
    labels = {}
    t = start
    records.append({"timestamp": ts(t), "type": "turn_context",
                    "payload": {"cwd": ws, "approval_policy": "on-request",
                                "sandbox_policy": {"mode": "workspace-write"},
                                "model": "gpt-5-codex", "summary": "auto"}})
    for index, turn in enumerate(expand_noise(session["turns"])):
        kind = turn[0]
        t += timedelta(seconds=5 + (index * 11) % 23)
        if kind == "user":
            records.append({"timestamp": ts(t), "type": "response_item",
                            "payload": {"type": "message", "role": "user",
                                        "content": [{"type": "input_text", "text": subst(turn[1], ws)}]}})
        elif kind == "assistant":
            records.append({"timestamp": ts(t), "type": "response_item",
                            "payload": {"type": "message", "role": "assistant",
                                        "content": [{"type": "output_text", "text": subst(turn[1], ws)}]}})
        elif kind == "thinking":
            records.append({"timestamp": ts(t), "type": "response_item",
                            "payload": {"type": "reasoning", "summary": [
                                {"type": "summary_text", "text": subst(turn[1], ws)}],
                                "encrypted_content": ids.token("gAAAA", 80)}})
            continue
        elif kind == "tool":
            _, name, inp, output, is_error = turn[:5]
            label = turn[5] if len(turn) > 5 else None
            call_id = ids.token("call_")
            records.append({"timestamp": ts(t), "type": "response_item",
                            "payload": {"type": "function_call", "name": name,
                                        "arguments": json.dumps(subst(inp, ws)), "call_id": call_id}})
            if label:
                labels[label + ":call"] = len(records)
            t += timedelta(seconds=1)
            records.append({"timestamp": ts(t), "type": "response_item",
                            "payload": {"type": "function_call_output", "call_id": call_id,
                                        "output": json.dumps({"output": subst(output, ws),
                                                              "metadata": {"exit_code": 1 if is_error else 0,
                                                                           "duration_seconds": 0.4}})}})
            if label:
                labels[label] = len(records)
            continue
        else:
            continue
        label = turn[2] if len(turn) > 2 else None
        if label:
            labels[label] = len(records)
    return records, labels, t


def vary(session, rng: random.Random, generation: int):
    """Seeded recombination for scale variants: rename the crate/version and
    reorder/insert noise so each copy is a distinct, still-coherent session."""
    s = copy.deepcopy(session)
    crate = rng.choice(CRATES)
    version = rng.choice(VERSIONS)

    def fix(v):
        if isinstance(v, str):
            return v.replace("ledger", crate).replace("0.9.0", version).replace("0.8.4", "0.0.1")
        if isinstance(v, dict):
            return {k: fix(x) for k, x in v.items()}
        if isinstance(v, (list, tuple)):
            return type(v)(fix(x) for x in v)
        return v

    turns = []
    for turn in s["turns"]:
        # Labels identify concepts, not crate names. Transform only the payload
        # so substitutions cannot silently rename a concept on a scale variant.
        payload_end = 2 if turn[0] in ("user", "assistant") else 5 if turn[0] == "tool" else len(turn)
        turns.append(fix(turn[:payload_end]) + turn[payload_end:])
    noise_keys = sorted(S.NOISE)
    for _ in range(rng.randint(0, 4)):
        turns.insert(rng.randint(1, len(turns)), ("noise", rng.choice(noise_keys)))
    s["turns"] = turns
    s["scenario"] = session["slug"]
    s["slug"] = f"{s['slug']}-g{generation:05d}"
    return s


def write_jsonl(path, records):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8", newline="\n") as fh:
        for rec in records:
            fh.write(json.dumps(rec, ensure_ascii=False, separators=(",", ":")))
            fh.write("\n")


def generate(out: str, ws: str, seed: int, target_records: int):
    rng = random.Random(seed)
    ids = Ids(rng)
    home = os.path.join(out, "home")
    manifest_sessions = []
    locators = {}
    label_instances = {}
    total_records = 0
    start = EPOCH

    def emit(session, agent, core):
        nonlocal total_records, start
        session_id = ids.uuid()
        if agent == "claude_code":
            records, labels, end = render_claude(session, ws, ids, start, session_id)
            rel = os.path.join("home", ".claude", "projects", claude_project_dir(ws), f"{session_id}.jsonl")
        else:
            records, labels, end = render_codex(session, ws, ids, start, session_id)
            day = 1 + (len(manifest_sessions) % 28)
            rel = os.path.join("home", ".codex", "sessions", "2026", "09", f"{day:02d}",
                               f"rollout-{ts(start)[:19].replace(':', '-')}-{session_id}.jsonl")
        write_jsonl(os.path.join(out, rel), records)
        manifest_sessions.append({
            "path": rel, "agent": agent, "slug": session["slug"], "title": session.get("title", ""),
            "core": core, "session_id": session_id, "started_at": ts(start), "ended_at": ts(end),
            "scenario": session.get("scenario", session["slug"]),
            "message_count": len(records),
            "labeled_record_count": len(set(labels.values())),
            "unlabeled_record_count": len(records) - len(set(labels.values())),
        })
        for label, line in labels.items():
            locator = {
                "path": rel, "line": line, "slug": session["slug"],
                "session_id": session_id, "record_id": f"{session_id}:{line}", "core": core,
            }
            label_instances.setdefault(label, []).append(locator)
            if core:
                if label in locators:
                    raise ValueError(f"duplicate label {label}")
                locators[label] = dict(locator)
        total_records += len(records)
        start = end + timedelta(minutes=37)

    for session in S.CLAUDE_SESSIONS:
        emit(session, "claude_code", True)
    for session in S.CODEX_SESSIONS:
        emit(session, "codex", True)

    pool = [(s, "claude_code") for s in S.CLAUDE_SESSIONS] + [(s, "codex") for s in S.CODEX_SESSIONS]
    generation = 0
    while total_records < target_records:
        generation += 1
        base, agent = pool[rng.randrange(len(pool))]
        emit(vary(base, rng, generation), agent, False)

    manifest = {
        "schema": "ee.real_shape_cass.manifest.v2",
        "seed": seed,
        "workspace": ws,
        "target_records": target_records,
        "total_records": total_records,
        "claude_code_version": CLAUDE_VERSION,
        "codex_cli_version": CODEX_CLI_VERSION,
        "sessions": manifest_sessions,
        "labels": locators,
        "label_instances": label_instances,
    }
    judgments = build_judgments(locators)
    for name, doc in (("manifest.json", manifest), ("judgments.json", judgments)):
        with open(os.path.join(out, name), "w", encoding="utf-8", newline="\n") as fh:
            json.dump(doc, fh, indent=2, sort_keys=True, ensure_ascii=False)
            fh.write("\n")
    return manifest


def build_judgments(locators):
    def need(label):
        if label not in locators:
            raise ValueError(f"judgment references unknown label {label}")
        return label

    queries = []
    for index, (query, relevant, distractors) in enumerate(S.RETRIEVAL_QUERIES):
        # Fixed split: even index calibrates, odd index evaluates (never tune on it).
        queries.append({
            "id": f"q{index:02d}", "query": query, "split": "calibration" if index % 2 == 0 else "evaluation",
            "relevant": {need(k): v for k, v in relevant.items()},
            "distractors": [need(k) for k in distractors],
        })
    return {
        "schema": "ee.real_shape_cass.judgments.v2",
        "retrieval": queries,
        "negative_queries": S.NEGATIVE_QUERIES,
        "learn": S.LEARN_JUDGMENTS,
        "must_not_propose": S.MUST_NOT_PROPOSE,
        "admission": {
            "risky": [need(k) for k in S.ADMISSION_RISKY],
            "benign": [need(k) for k in S.ADMISSION_BENIGN],
            "secrets": {need(k): v for k, v in S.SECRET_LABELS.items()},
        },
    }


# Field table from SHAPES.md. A corpus whose records lose these envelopes is
# "clean" and validates nothing, so the probe refuses it.
CLAUDE_REQUIRED = {
    "user": ["parentUuid", "isSidechain", "userType", "cwd", "sessionId", "version", "gitBranch",
             "type", "message", "uuid", "timestamp"],
    "assistant": ["parentUuid", "isSidechain", "userType", "cwd", "sessionId", "version", "gitBranch",
                  "message", "requestId", "type", "uuid", "timestamp"],
    "summary": ["type", "summary", "leafUuid"],
}
CODEX_REQUIRED = {"session_meta": ["timestamp", "type", "payload"],
                  "turn_context": ["timestamp", "type", "payload"],
                  "response_item": ["timestamp", "type", "payload"]}
CLAUDE_BLOCKS = {"text": ["text"], "thinking": ["thinking", "signature"],
                 "tool_use": ["id", "name", "input"], "tool_result": ["tool_use_id", "content"]}


def validate(out: str) -> list:
    """Return envelope, exact-identity and judgment problems in a v2 corpus."""
    problems = []
    documents = {}
    for name in ("manifest.json", "judgments.json"):
        try:
            with open(os.path.join(out, name), encoding="utf-8") as fh:
                documents[name] = json.load(fh)
        except (OSError, ValueError) as error:
            problems.append(f"{name}: cannot read JSON: {error}")
    if len(documents) != 2:
        return problems
    manifest = documents["manifest.json"]
    judgments = documents["judgments.json"]
    if not isinstance(manifest, dict) or not isinstance(judgments, dict):
        return problems + ["manifest.json and judgments.json must be objects"]
    if manifest.get("schema") != "ee.real_shape_cass.manifest.v2":
        problems.append("manifest.json: expected ee.real_shape_cass.manifest.v2")
    if judgments.get("schema") != "ee.real_shape_cass.judgments.v2":
        problems.append("judgments.json: expected ee.real_shape_cass.judgments.v2")
    sessions = manifest.get("sessions")
    labels = manifest.get("labels")
    instances = manifest.get("label_instances")
    if not isinstance(sessions, list) or not sessions:
        problems.append("manifest.json: sessions must be a nonempty list")
        sessions = []
    if not isinstance(labels, dict) or not labels:
        problems.append("manifest.json: labels must be a nonempty object")
        labels = {}
    if not isinstance(instances, dict) or not instances:
        problems.append("manifest.json: label_instances must be a nonempty object")
        instances = {}
    if set(labels) != set(instances):
        problems.append("manifest.json: labels and label_instances must name the same concepts")

    by_path = {}
    session_ids = set()
    core_scenarios = {}
    record_counts = {}
    total_records = 0
    root = os.path.realpath(out)
    for index, session in enumerate(sessions):
        if not isinstance(session, dict):
            problems.append(f"session {index}: must be an object")
            continue
        fields = ("path", "session_id", "slug", "scenario")
        if any(not isinstance(session.get(key), str) or not session[key] for key in fields):
            problems.append(f"session {index}: missing path/session_id/slug/scenario")
            continue
        rel = session["path"]
        if type(session.get("core")) is not bool:
            problems.append(f"{rel}: core must be a boolean")
            continue
        if session.get("agent") not in ("claude_code", "codex"):
            problems.append(f"{rel}: unknown agent")
            continue
        path = os.path.realpath(os.path.join(out, rel))
        if os.path.isabs(rel) or os.path.normpath(rel) != rel or os.path.commonpath([root, path]) != root:
            problems.append(f"{rel}: session path escapes the corpus or is not normalized")
            continue
        if rel in by_path:
            problems.append(f"{rel}: duplicate session path")
            continue
        by_path[rel] = session
        if session["session_id"] in session_ids:
            problems.append(f"{rel}: duplicate source session_id")
        session_ids.add(session["session_id"])
        if session["core"]:
            if session["scenario"] != session["slug"]:
                problems.append(f"{rel}: core scenario must equal its slug")
            if session["scenario"] in core_scenarios:
                problems.append(f"{rel}: conflicting core scenario")
            else:
                core_scenarios[session["scenario"]] = session
        try:
            with open(path, encoding="utf-8") as fh:
                lines = list(fh)
        except (OSError, UnicodeError) as error:
            problems.append(f"{rel}: cannot read session: {error}")
            lines = []
        record_counts[rel] = len(lines)
        total_records += len(lines)
        if type(session.get("message_count")) is not int or session["message_count"] != len(lines):
            problems.append(f"{rel}: message_count does not match its records")
        for number, line in enumerate(lines, 1):
            where = f"{rel}:{number}"
            try:
                rec = json.loads(line)
            except json.JSONDecodeError:
                problems.append(f"{where}: not JSON")
                continue
            if not isinstance(rec, dict):
                problems.append(f"{where}: record must be an object")
                continue
            table = CLAUDE_REQUIRED if session["agent"] == "claude_code" else CODEX_REQUIRED
            kind = rec.get("type")
            if not isinstance(kind, str) or kind not in table:
                problems.append(f"{where}: unknown record type {kind!r}")
                continue
            missing = [key for key in table[kind] if key not in rec]
            if missing:
                problems.append(f"{where}: missing envelope fields {missing}")
            if session["agent"] == "claude_code" and kind in ("user", "assistant"):
                if rec.get("sessionId") != session["session_id"]:
                    problems.append(f"{where}: source sessionId conflicts with manifest")
                message = rec.get("message")
                content = message.get("content") if isinstance(message, dict) else None
                if kind == "assistant" and not isinstance(content, list):
                    problems.append(f"{where}: assistant content must be a block list")
                if isinstance(content, list):
                    for block in content:
                        block_type = block.get("type") if isinstance(block, dict) else None
                        need = CLAUDE_BLOCKS.get(block_type) if isinstance(block_type, str) else None
                        if need is None or any(key not in block for key in need):
                            problems.append(f"{where}: malformed {block_type!r} block")
            if session["agent"] == "codex" and number == 1:
                payload = rec.get("payload")
                if kind != "session_meta" or not isinstance(payload, dict) or payload.get("id") != session["session_id"]:
                    problems.append(f"{where}: initial session_meta conflicts with manifest")
    if type(manifest.get("total_records")) is not int or manifest["total_records"] != total_records:
        problems.append("manifest.json: total_records does not match session files")

    def locate(label, locator, source):
        where = f"{source} {label}"
        if not isinstance(locator, dict):
            problems.append(f"{where}: locator must be an object")
            return None
        rel = locator.get("path")
        session = by_path.get(rel) if isinstance(rel, str) else None
        if session is None:
            problems.append(f"{where}: missing session locator")
            return None
        line = locator.get("line")
        if type(line) is not int or not 1 <= line <= record_counts[rel]:
            problems.append(f"{where}: line is outside the session")
            return None
        if locator.get("slug") != session["slug"] or locator.get("session_id") != session["session_id"]:
            problems.append(f"{where}: conflicting slug or source session_id")
        if type(locator.get("core")) is not bool or locator["core"] != session["core"]:
            problems.append(f"{where}: conflicting core flag")
        if locator.get("record_id") != f"{session['session_id']}:{line}":
            problems.append(f"{where}: conflicting exact record_id")
        return rel, line

    original_sessions = {}
    for label, locator in labels.items():
        key = locate(label, locator, "label")
        if key is not None:
            session = by_path[key[0]]
            if not session["core"]:
                problems.append(f"label {label}: original locator must belong to the authored core")
            original_sessions[label] = session
    seen_records = {}
    seen_concept_sessions = set()
    concepts_by_path = {rel: set() for rel in by_path}
    lines_by_path = {rel: set() for rel in by_path}
    for label, locators in instances.items():
        if not isinstance(locators, list) or not locators:
            problems.append(f"label_instances {label}: must be a nonempty list")
            continue
        original = labels.get(label)
        if sum(locator == original for locator in locators) != 1:
            problems.append(f"label_instances {label}: must contain the original locator exactly once")
        for locator in locators:
            key = locate(label, locator, "label_instances")
            if key is None:
                continue
            rel, line = key
            if key in seen_records:
                problems.append(f"label_instances {label}: duplicate or conflicting exact record with {seen_records[key]}")
            else:
                seen_records[key] = label
            concept_session = label, rel
            if concept_session in seen_concept_sessions:
                problems.append(f"label_instances {label}: duplicate concept in one source session")
            seen_concept_sessions.add(concept_session)
            session = by_path[rel]
            origin = original_sessions.get(label)
            if origin is None or session["scenario"] != origin["scenario"]:
                problems.append(f"label_instances {label}: instance belongs to a different scenario")
            if session["core"] and locator != original:
                problems.append(f"label_instances {label}: conflicting authored-core locator")
            concepts_by_path[rel].add(label)
            lines_by_path[rel].add(line)
    for rel, session in by_path.items():
        origin = core_scenarios.get(session["scenario"])
        if origin is None:
            problems.append(f"{rel}: missing authored core for scenario")
        elif session["agent"] != origin["agent"]:
            problems.append(f"{rel}: variant agent conflicts with its core scenario")
        elif concepts_by_path[rel] != concepts_by_path[origin["path"]]:
            problems.append(f"{rel}: missing or extra concept locators for its scenario")
        labeled_count = len(lines_by_path[rel])
        if type(session.get("labeled_record_count")) is not int or session["labeled_record_count"] != labeled_count:
            problems.append(f"{rel}: labeled_record_count does not match exact locators")
        if type(session.get("unlabeled_record_count")) is not int or session["unlabeled_record_count"] != record_counts[rel] - labeled_count:
            problems.append(f"{rel}: unlabeled_record_count does not match exact locators")

    queries = judgments.get("retrieval")
    if not isinstance(queries, list) or not queries:
        problems.append("judgments.json: retrieval must be a nonempty list")
        queries = []
    query_ids = set()
    for index, query in enumerate(queries):
        if not isinstance(query, dict):
            problems.append(f"retrieval {index}: query must be an object")
            continue
        query_id = query.get("id")
        where = f"retrieval {index}"
        if not isinstance(query_id, str) or not query_id:
            problems.append(f"{where}: missing query id")
        elif query_id in query_ids:
            problems.append(f"{where}: duplicate query id {query_id}")
        else:
            query_ids.add(query_id)
        relevant = query.get("relevant")
        distractors = query.get("distractors")
        if not isinstance(relevant, dict) or not relevant:
            problems.append(f"{where}: relevant must be a nonempty object")
            relevant = {}
        if not isinstance(distractors, list):
            problems.append(f"{where}: distractors must be a list")
            distractors = []
        for label, grade in relevant.items():
            if label not in labels:
                problems.append(f"{where}: missing relevant concept {label}")
            if type(grade) is not int or grade <= 0:
                problems.append(f"{where}: relevance grade for {label} must be a positive integer")
        seen_distractors = set()
        for label in distractors:
            if not isinstance(label, str) or label not in labels:
                problems.append(f"{where}: missing distractor concept {label!r}")
                continue
            if label in seen_distractors:
                problems.append(f"{where}: duplicate distractor concept {label}")
            seen_distractors.add(label)
            if label in relevant:
                problems.append(f"{where}: concept {label} is both relevant and a distractor")
    return problems


def tree_digest(out: str) -> str:
    h = hashlib.blake2b(digest_size=32)
    for root, _dirs, files in sorted(os.walk(out)):
        for name in sorted(files):
            path = os.path.join(root, name)
            h.update(os.path.relpath(path, out).encode())
            with open(path, "rb") as fh:
                h.update(fh.read())
    return h.hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--out", required=True)
    parser.add_argument("--workspace", default="/work/ledger")
    parser.add_argument("--seed", type=int, default=43)
    parser.add_argument("--target-records", type=int, default=0,
                        help="expand by seeded recombination until at least this many records exist")
    parser.add_argument("--digest", action="store_true", help="print a digest of the output tree")
    args = parser.parse_args()
    manifest = generate(args.out, args.workspace, args.seed, args.target_records)
    summary = {"sessions": len(manifest["sessions"]), "records": manifest["total_records"],
               "labels": len(manifest["labels"])}
    if args.digest:
        summary["digest"] = tree_digest(args.out)
    print(json.dumps(summary))


if __name__ == "__main__":
    main()
