#!/usr/bin/env python3
"""Real ee binary: exact validity, typed revisions, export, restore, re-backup.

No database edits or mock engine. Every mutation goes through public commands.
The temporary workspace and JSONL command log are retained for diagnosis.
"""
from __future__ import annotations

import datetime as dt
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
from typing import Any


def instant_ns(raw: str | None) -> int | None:
    if raw is None:
        return None
    match = re.fullmatch(r"(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2})(?:\.(\d{1,9}))?(Z|[+-]\d{2}:\d{2})", raw)
    assert match, f"Not an exact supported RFC3339 timestamp: {raw}"
    clock = dt.datetime.fromisoformat(match[1] + match[3].replace('Z', '+00:00'))
    delta = clock.astimezone(dt.timezone.utc) - dt.datetime(1970, 1, 1, tzinfo=dt.timezone.utc)
    return (delta.days * 86400 + delta.seconds) * 1_000_000_000 + int((match[2] or '').ljust(9, '0'))


def main() -> None:
    if len(sys.argv) != 2:
        raise SystemExit('usage: memory_lifecycle_cli.py /absolute/path/to/ee')
    binary = Path(sys.argv[1]).resolve(strict=True)
    root = Path(tempfile.mkdtemp(prefix='ee-lifecycle-cli-')).resolve()
    workspace = root / 'workspace'
    workspace.mkdir()
    env = {key: value for key, value in os.environ.items() if not key.startswith('EE_')}
    env.update(EE_EMBED_DOWNLOAD='off', NO_COLOR='1', XDG_DATA_HOME=str(root / 'data'),
               XDG_CONFIG_HOME=str(root / 'config'), XDG_CACHE_HOME=str(root / 'cache'))
    logfile = root / 'commands.jsonl'
    print(f'Lifecycle command log: {logfile}', flush=True)

    def run(*args: str, ws: Path = workspace, success: bool = True) -> dict[str, Any]:
        command = [str(binary), '--workspace', str(ws), '--json', *args]
        output = subprocess.run(command, cwd=root, env=env, text=True,
                                capture_output=True, timeout=180, check=False)
        event = {'schema': 'ee.test_event.v1', 'testId': 'memory_lifecycle_cli',
                 'command': command, 'exitCode': output.returncode,
                 'stdout': output.stdout, 'stderr': output.stderr}
        with logfile.open('a') as stream:
            stream.write(json.dumps(event) + '\n')
        assert (output.returncode == 0) == success, event
        value = json.loads(output.stdout)
        if success:
            assert value['schema'] == 'ee.response.v2' and value['success'] is True, event
            return value['data']
        assert value.get('success') is False or value['schema'] == 'ee.error.v2', event
        return value

    def show(memory_id: str, ws: Path = workspace, database: str | None = None) -> dict[str, Any]:
        extra = ['--database', database] if database else []
        return run('memory', 'show', memory_id, *extra, ws=ws)['memory']

    def export(ws: Path, directory: Path, database: str | None = None) -> dict[str, dict[str, Any]]:
        extra = ['--database', database] if database else []
        exported = run('export', '--output-dir', str(directory), '--redaction', 'none', *extra, ws=ws)
        records = [json.loads(line) for line in Path(exported['recordsPath']).read_text().splitlines() if line.strip()]
        memories = [row for row in records if row.get('schema') == 'ee.export.memory.v1']
        result = {row['memory_id']: row for row in memories}
        assert len(result) == len(memories) and result, records
        return result

    def preserved(before: dict[str, dict[str, Any]], after: dict[str, dict[str, Any]]) -> None:
        assert set(before) == set(after), (before.keys(), after.keys())
        for identity, source in before.items():
            restored = after[identity]
            for field in ['content', 'kind', 'level', 'logical_id', 'typed_fields']:
                assert source.get(field) == restored.get(field), (identity, field, source, restored)
            for field in ['created_at', 'updated_at', 'valid_from', 'valid_to', 'superseded_at']:
                assert instant_ns(source.get(field)) == instant_ns(restored.get(field)), (identity, field, source, restored)

    run('init')
    start = '2020-01-02T03:04:05.123456789+05:30'
    end = '2037-01-02T03:04:05.987654321+05:30'
    revisit = '2027-01-01T00:00:00.123456789Z'
    captured = run('remember', 'The release choice remains structured during editorial revisions.',
                   '--kind', 'decision', '--level', 'semantic', '--source', 'manual://lifecycle-fixture',
                   '--valid-from', start, '--valid-to', end,
                   '--field', 'chosen=SQLite', '--field', 'options=SQLite', '--field', 'options=Postgres',
                   '--field', 'rationale=Offline operation', '--field', f'revisit-by={revisit}')
    original = captured['memoryId']
    source = show(original)
    assert instant_ns(source['valid_from']) == instant_ns(start), source
    assert instant_ns(source['valid_to']) == instant_ns(end), source
    expected_fields = {'chosen': 'SQLite', 'options': ['SQLite', 'Postgres'],
                       'rationale': 'Offline operation', 'revisit_by': revisit}
    assert source['typedFields'] == expected_fields, source

    preview = run('memory', 'revise', original, '--tags', 'reviewed', '--dry-run')
    assert preview['dry_run'] and preview['new_id'] is None and not preview['persisted'], preview
    assert show(original) == source, 'Dry-run changed original memory'
    revised = run('memory', 'revise', original, '--tags', 'reviewed')
    middle = revised['new_id']
    assert middle != original and revised['persisted'], revised
    assert show(middle)['typedFields'] == expected_fields
    assert show(original)['typedFields'] == expected_fields

    body = 'Chosen: Postgres\nRationale: Shared writer coordination'
    changed = run('memory', 'revise', middle, '--content', body)
    latest = changed['new_id']
    assert latest not in [original, middle] and 'typed_fields' in changed['changed_fields'], changed
    expected_fields.update(chosen='Postgres', rationale='Shared writer coordination')
    assert show(latest)['typedFields'] == expected_fields
    found = run('search', 'Shared writer coordination', '--field', 'chosen=Postgres', '--speed', 'instant')
    assert any(row.get('memoryId') == latest or row.get('docId') == latest for row in found['results']), found
    before = export(workspace, root / 'export-before')
    assert len(before) == 3, before
    for dry in [True, False]:
        run('memory', 'revise', latest, '--content', 'Chosen: SQLite\nRevisit by: invalid-date',
            *(['--dry-run'] if dry else []), success=False)
    assert export(workspace, root / 'export-after-invalid') == before, 'Rejected revision changed durable memory data'

    backup = run('backup', 'create', '--redaction', 'none', '--include-graph-cache=false')
    assert backup['recoveryInventory']['snapshotCoverageComplete'], backup
    assert run('backup', 'verify', backup['backupPath'])['status'] == 'verified'
    side = root / 'restored'
    restored = run('backup', 'restore', backup['backupPath'], '--side-path', str(side), '--skip-graph-cache')
    database = restored['restoredDatabasePath']
    assert Path(database).is_file(), restored
    after = export(side, root / 'export-restored', database)
    preserved(before, after)
    assert show(latest, side, database)['typedFields'] == expected_fields
    # A second recovery must not round precision or erase typed data either.
    again = run('backup', 'create', '--database', database, '--redaction', 'none',
                '--include-graph-cache=false', ws=side)
    assert run('backup', 'verify', again['backupPath'], ws=side)['status'] == 'verified'
    second_side = root / 'restored-again'
    second = run('backup', 'restore', again['backupPath'], '--side-path', str(second_side),
                 '--skip-graph-cache', ws=side)
    preserved(before, export(second_side, root / 'export-restored-again', second['restoredDatabasePath']))
    print('PASS: exact lifecycle timestamps and typed immutable revisions survive public recovery twice', flush=True)


if __name__ == '__main__':
    main()
