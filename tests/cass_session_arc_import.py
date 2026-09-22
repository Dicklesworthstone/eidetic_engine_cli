#!/usr/bin/env python3
"""Exercise the real EE importer and public learning lifecycle with fixture CASS.

The CASS subprocess supplies deterministic upstream responses; EE itself is the
built production binary, not a mock. No private database seeding is used. The
original policy failure/fix wording intentionally contains no invented technical
commands, so successful mining alone cannot hide a rejected application.
"""
from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

FAILURE = "Failure arc: storing silently would violate the no-loop-takeover policy."
REPAIR = "Fix: require accept/reject commands and audit every accepted capture."
ORDINARY_FAILURE = "The build failed in src/cache.rs because the key used display labels."
ORDINARY_REPAIR = "Fixed src/cache.rs by using stable identity bytes."


def exercise(binary: Path) -> None:
    with tempfile.TemporaryDirectory(prefix="ee-cass-learning-") as temporary:
        workspace = Path(temporary).resolve()
        transcript = workspace / "session.jsonl"
        bodies = [FAILURE, REPAIR, ORDINARY_FAILURE, ORDINARY_REPAIR]
        records = [json.dumps({"role": "user" if index == 1 else "assistant", "content": body})
                   for index, body in enumerate(bodies)]
        original = "\n".join(records) + "\n"
        transcript.write_text(original, encoding="utf-8")
        sessions = {"sessions": [{"path": str(transcript), "workspace": str(workspace),
            "agent": "codex", "started_at": "2026-06-17T13:00:00Z",
            "ended_at": "2026-06-17T13:20:00Z", "message_count": len(records),
            "token_count": 300, "content_hash": "blake3:session-learning-import-fixture"}]}
        view = {"path": str(transcript), "target_line": 1, "context": len(records),
            "lines": [{"line": index + 1, "content": record, "highlighted": index == 1}
                      for index, record in enumerate(records)], "total_lines": len(records)}
        cass = workspace / "cass"
        cass.write_text(
            f"#!{sys.executable}\nimport json, sys\n"
            f"responses = {dict(sessions=sessions, view=view)!r}\n"
            "command = sys.argv[1] if len(sys.argv) > 1 else ''\n"
            "if command not in responses:\n"
            "    print('unexpected fixture CASS command: ' + command, file=sys.stderr)\n"
            "    sys.exit(64)\n"
            "print(json.dumps(responses[command]))\n", encoding="utf-8")
        cass.chmod(0o755)
        env = os.environ.copy()
        for name in ("EE_WORKSPACE", "EE_WORKSPACE_REGISTRY", "EE_DB", "EE_DATABASE_PATH"):
            env.pop(name, None)
        env.update(EE_CASS_BINARY=str(cass), EE_EMBED_DOWNLOAD="off", NO_COLOR="1",
            XDG_DATA_HOME=str(workspace / "xdg-data"),
            XDG_CONFIG_HOME=str(workspace / "xdg-config"),
            XDG_CACHE_HOME=str(workspace / "xdg-cache"))

        def run(*args: str) -> dict:
            result = subprocess.run([str(binary), "--workspace", str(workspace), "--json", *args],
                cwd=workspace, env=env, capture_output=True, text=True, timeout=180, check=False)
            event = {"schema": "ee.test_event.v1", "testId": "cass_session_arc_import",
                "kind": "command_finish", "args": args, "exitCode": result.returncode,
                "stdout": result.stdout, "stderr": result.stderr}
            print(json.dumps(event), flush=True)
            assert result.returncode == 0, event
            envelope = json.loads(result.stdout)
            assert envelope["schema"] == "ee.response.v2" and envelope["success"] is True, envelope
            assert isinstance(envelope["data"], dict), envelope
            return envelope["data"]

        def memories() -> list:
            data = run("memory", "list")
            rows = data["memories"]
            assert isinstance(rows, list), data
            return rows

        run("init")
        assert not memories(), "fixture must start without durable lessons"
        imported = run("import", "cass", "--limit", "1")
        assert imported["schema"] == "ee.import.cass.v1", imported
        assert imported["sessionsImported"] == 1 and imported["spansImported"] == 4, imported
        session_id = imported["sessions"][0]["sessionId"]
        assert not memories(), "import may create evidence, not implicit lessons"
        preview = run("review", "session", session_id, "--dry-run", "--limit", "8", "--min-confidence", "0.8")
        assert preview["durableMutation"] is False, preview
        proposed = run("review", "session", session_id, "--propose", "--limit", "8", "--min-confidence", "0.8")
        arcs = [row for row in proposed["candidates"] if row["candidateKind"].startswith("session_arc_")]
        assert len(arcs) == 4, proposed
        assert {row["candidateId"] for row in arcs} == {
            row["candidateId"] for row in preview["candidates"] if row["candidateKind"].startswith("session_arc_")}, (preview, proposed)
        assert not memories(), "proposing must not implicitly accept either episode"
        groups: dict[str, list[dict]] = {}
        for row in arcs:
            groups.setdefault(row["sessionArc"]["arcId"], []).append(row)
        assert len(groups) == 2 and all(len(pair) == 2 for pair in groups.values()), groups
        # The generic policy pair must keep the exact observed policy wording;
        # adding a fake cargo command to satisfy specificity is not a repair.
        policy = [pair for pair in groups.values() if any(FAILURE in row["proposedContent"] for row in pair)]
        assert len(policy) == 1, groups
        assert all(FAILURE in row["proposedContent"] and REPAIR in row["proposedContent"]
                   for row in policy[0]), policy
        created: set[str] = set()
        for pair in sorted(groups.values(), key=lambda pair: pair[0]["sessionArc"]["failureSpan"]["startLine"], reverse=True):
            rule = next(row for row in pair if row["candidateKind"] == "session_arc_rule")
            anti = next(row for row in pair if row["candidateKind"] == "session_arc_anti_pattern")
            assert rule["sessionArc"]["linkedCandidateId"] == anti["candidateId"], pair
            assert anti["sessionArc"]["linkedCandidateId"] == rule["candidateId"], pair
            pair_memories: list[str] = []
            for row in (rule, anti):
                candidate_id = row["candidateId"]
                validated = run("curate", "validate", candidate_id, "--actor", "CassImportLifecycle")
                assert validated["validation"]["decision"] == "approved", validated
                applied = run("curate", "apply", candidate_id, "--actor", "CassImportLifecycle")
                assert applied["application"]["status"] == "applied", applied
                memory_id = applied["application"]["createdMemoryId"]
                assert memory_id not in created, applied
                created.add(memory_id)
                pair_memories.append(memory_id)
                replay = run("curate", "apply", candidate_id, "--actor", "CassImportLifecycle")
                assert replay["application"]["status"] == "already_applied", replay
            links = run("memory", "link", pair_memories[0], "--relation", "related")["links"]
            assert len(links) == 1, links
            edge = links[0]
            assert edge["source_memory_id"] == pair_memories[0] and edge["target_memory_id"] == pair_memories[1], edge
            assert edge["relation"] == "related" and edge["directed"] is False, edge
            assert len(memories()) == len(created), "only explicitly applied episodes become memories"
        assert len(created) == 4
        assert transcript.read_text(encoding="utf-8") == original, "learning must not rewrite upstream transcript"
        print("PASS: real CASS import -> four proposals -> validation -> four memories -> two links -> idempotent replay", flush=True)


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("usage: cass_session_arc_import.py /absolute/path/to/ee")
    executable = Path(sys.argv[1]).resolve(strict=True)
    if not executable.is_file() or not os.access(executable, os.X_OK):
        raise SystemExit(f"not an executable: {executable}")
    exercise(executable)
