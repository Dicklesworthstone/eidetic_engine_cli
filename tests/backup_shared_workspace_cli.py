#!/usr/bin/env python3
"""Real CLI shared-store recovery: create, verify, restore, and query both scopes.

No mock database, raw SQL seeding, or external service is used. Each source is
registered and populated through public EE commands. Restore keeps the existing
foreign-store trust cap; this scenario tests ownership, typed content, evidence,
and source immutability rather than claiming foreign trust is locally attested.
"""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile


def exercise(binary: Path) -> None:
    # Retain the unpredictable, owner-only directory on failure for inspection.
    root = Path(tempfile.mkdtemp(prefix="ee-shared-backup-cli-")).resolve()
    env = {key: value for key, value in os.environ.items() if not key.startswith("EE_")}
    env.update(EE_EMBED_DOWNLOAD="off", NO_COLOR="1", XDG_DATA_HOME=str(root / "data"),
               XDG_CONFIG_HOME=str(root / "config"), XDG_CACHE_HOME=str(root / "cache"))

    def run(workspace: Path, *args: str) -> dict:
        result = subprocess.run([str(binary), "--workspace", str(workspace), "--json", *args],
                                cwd=root, env=env, capture_output=True, text=True,
                                timeout=180, check=False)
        event = {"schema": "ee.test_event.v1", "testId": "backup_shared_workspace_cli",
                 "kind": "command_finish", "args": args, "exitCode": result.returncode,
                 "stdout": result.stdout, "stderr": result.stderr}
        print(json.dumps(event), flush=True)
        assert result.returncode == 0, event
        envelope = json.loads(result.stdout)
        assert envelope["schema"] == "ee.response.v2" and envelope["success"] is True, envelope
        assert isinstance(envelope["data"], dict), envelope
        return envelope["data"]

    workspaces = [root / "CopperKestrel", root / "VioletHeron"]
    for workspace in workspaces:
        workspace.mkdir()
        run(workspace, "init")
    shared = workspaces[0] / ".ee" / "ee.db"
    ids: list[list[str]] = []
    rules: list[str] = []
    for number, workspace in enumerate(workspaces):
        selected = []
        for slot in range(number + 2):
            chosen = f"{workspace.name}Store{slot}"
            memory = run(workspace, "remember", f"The durable database choice is {chosen}.",
                         "--database", str(shared), "--level", "semantic", "--kind", "decision",
                         "--field", f"chosen={chosen}", "--field", "options=OtherStore",
                         "--field", f"options={chosen}", "--field", "rationale=Keep workspace ownership exact.",
                         "--tags", f"{workspace.name},storage-{slot}", "--valid-from", "2026-01-01T00:00:00Z",
                         "--source", "manual://shared-backup-cli", "--no-auto-link", "--no-propose-candidates")
            selected.append(memory["memory_id"])
        ids.append(selected)
        sources = [part for memory_id in selected for part in ("--source-memory", memory_id)]
        rule = run(workspace, "rule", "add", f"Run cargo check before publishing {workspace.name} storage changes.",
                   "--database", str(shared), "--maturity", "validated", "--tag", workspace.name,
                   "--protect", *sources)
        rules.append(rule["ruleId"])

    def memories(workspace: Path, database: Path | None = None) -> list:
        arguments = ("--database", str(database)) if database else ()
        rows = run(workspace, "memory", "list", *arguments)["memories"]
        assert isinstance(rows, list)
        return rows

    for number, workspace in enumerate(workspaces):
        assert {row["id"] for row in memories(workspace, shared)} == set(ids[number])
    # Read-only backup planning must not initialize a key or touch DB bytes.
    before = hashlib.sha256(shared.read_bytes()).hexdigest()
    for number, workspace in enumerate(workspaces):
        output = root / f"dry-{number}"
        dry = run(workspace, "backup", "create", "--database", str(shared), "--output-dir", str(output),
                  "--redaction", "none", "--include-graph-cache=false", "--dry-run")
        assert dry["recoveryInventory"]["snapshotCoverageComplete"] is True, dry
        assert not output.exists()
    assert hashlib.sha256(shared.read_bytes()).hexdigest() == before

    for number, workspace in enumerate(workspaces):
        source_rules = run(workspace, "rule", "list", "--database", str(shared))["rules"]
        backup = run(workspace, "backup", "create", "--database", str(shared), "--output-dir", str(root / f"backups-{number}"),
                     "--redaction", "none", "--include-graph-cache=false")
        assert backup["status"] == "completed", backup
        inventory = backup["recoveryInventory"]
        assert inventory["snapshotCoverageComplete"] is True, inventory
        assert inventory["requiredRowScope"] == "selected_workspace_and_shared_history", inventory
        counts = {row["table"]: row["rowCount"] for row in inventory["tables"]}
        assert counts["workspaces"] == 1 and counts["memories"] == len(ids[number]), counts
        assert counts["rule_source_memories"] == len(ids[number]), counts
        archive = Path(backup["backupPath"])
        records = [json.loads(line) for line in (archive / "records.jsonl").read_text().splitlines()]
        exported = [row for row in records if row["schema"] == "ee.export.memory.v1"]
        assert {row["memory_id"] for row in exported} == set(ids[number]), exported
        assert all(row["typed_fields"]["fields"]["chosen"].startswith(workspace.name) for row in exported)
        assert run(workspace, "backup", "verify", str(archive))["status"] == "verified"
        side = root / f"restored-{number}"
        restored = run(workspace, "backup", "restore", str(archive), "--side-path", str(side), "--skip-graph-cache")
        assert restored["status"] == "completed", restored
        rows = memories(side)
        assert {row["id"] for row in rows} == set(ids[number]), rows
        assert all(workspace.name in row["content"] for row in rows), rows
        restored_rules = run(side, "rule", "list")["rules"]
        assert {row["id"] for row in restored_rules} == {rules[number]}, restored_rules
        assert restored_rules == source_rules, (source_rules, restored_rules)
        assert not set(ids[1 - number]) & {row["id"] for row in rows}
    # Backup and isolated restore cannot mutate the selected shared source.
    assert hashlib.sha256(shared.read_bytes()).hexdigest() == before
    print("PASS: shared CLI store -> scoped backups -> verified independent restores -> exact memories and rule evidence", flush=True)


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("usage: backup_shared_workspace_cli.py /absolute/path/to/ee")
    executable = Path(sys.argv[1]).resolve(strict=True)
    if not executable.is_file() or not os.access(executable, os.X_OK):
        raise SystemExit("EE binary is not executable")
    exercise(executable)
