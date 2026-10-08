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

    turns = [fix(t) for t in s["turns"]]
    # Drop labels on copies: judgments refer to the authored core only.
    stripped = []
    for t in turns:
        if t[0] in ("user", "assistant") and len(t) > 2:
            t = t[:2]
        if t[0] == "tool" and len(t) > 5:
            t = t[:5]
        stripped.append(t)
    noise_keys = sorted(S.NOISE)
    for _ in range(rng.randint(0, 4)):
        stripped.insert(rng.randint(1, len(stripped)), ("noise", rng.choice(noise_keys)))
    s["turns"] = stripped
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
            "message_count": len(records),
        })
        if core:
            for label, line in labels.items():
                if label in locators:
                    raise ValueError(f"duplicate label {label}")
                locators[label] = {"path": rel, "line": line, "slug": session["slug"]}
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
        "schema": "ee.real_shape_cass.manifest.v1",
        "seed": seed,
        "workspace": ws,
        "target_records": target_records,
        "total_records": total_records,
        "claude_code_version": CLAUDE_VERSION,
        "codex_cli_version": CODEX_CLI_VERSION,
        "sessions": manifest_sessions,
        "labels": locators,
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
        "schema": "ee.real_shape_cass.judgments.v1",
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
    """Return shape problems for every record in a generated corpus."""
    problems = []
    with open(os.path.join(out, "manifest.json"), encoding="utf-8") as fh:
        manifest = json.load(fh)
    for session in manifest["sessions"]:
        path = os.path.join(out, session["path"])
        with open(path, encoding="utf-8") as fh:
            for number, line in enumerate(fh, 1):
                where = f"{session['path']}:{number}"
                try:
                    rec = json.loads(line)
                except json.JSONDecodeError:
                    problems.append(f"{where}: not JSON")
                    continue
                table = CLAUDE_REQUIRED if session["agent"] == "claude_code" else CODEX_REQUIRED
                kind = rec.get("type")
                if kind not in table:
                    problems.append(f"{where}: unknown record type {kind!r}")
                    continue
                missing = [k for k in table[kind] if k not in rec]
                if missing:
                    problems.append(f"{where}: missing envelope fields {missing}")
                if session["agent"] == "claude_code" and kind in ("user", "assistant"):
                    content = rec.get("message", {}).get("content")
                    if kind == "assistant" and not isinstance(content, list):
                        problems.append(f"{where}: assistant content must be a block list")
                    if isinstance(content, list):
                        for block in content:
                            need = CLAUDE_BLOCKS.get(block.get("type"))
                            if need is None or any(k not in block for k in need):
                                problems.append(f"{where}: malformed {block.get('type')!r} block")
    for label, where in manifest["labels"].items():
        if not os.path.exists(os.path.join(out, where["path"])):
            problems.append(f"label {label}: missing session file")
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
