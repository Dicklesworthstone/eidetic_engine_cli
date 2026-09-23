#!/usr/bin/env python3
"""Exercise public task-scoped packs and their durable replay, without SQL edits.

Every mutation goes through ee. Command logs and isolated stores are retained on
failure. Optional model downloads and ambient EE configuration are disabled.
"""
from __future__ import annotations
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile


def main() -> None:
    if len(sys.argv) != 2:
        raise SystemExit('usage: pack_task_paths_cli.py /absolute/path/to/ee')
    binary = Path(sys.argv[1]).resolve(strict=True)
    root = Path(tempfile.mkdtemp(prefix='ee-task-pack-')).resolve()
    workspace = root / 'workspace'
    workspace.mkdir()
    env = {k: v for k, v in os.environ.items() if not k.startswith('EE_')}
    env.update(EE_EMBED_DOWNLOAD='off', NO_COLOR='1', XDG_DATA_HOME=str(root/'data'),
               XDG_CONFIG_HOME=str(root/'config'), XDG_CACHE_HOME=str(root/'cache'))
    logfile = root/'commands.jsonl'
    print(f'Task pack command log: {logfile}', flush=True)

    def execute(*args: str, ok: bool = True, frames: bool = False):
        cmd = [str(binary), '--workspace', str(workspace), '--json', *args]
        out = subprocess.run(cmd, cwd=root, env=env, text=True, capture_output=True,
                             timeout=240, check=False)
        event = dict(command=cmd, exitCode=out.returncode, stdout=out.stdout, stderr=out.stderr)
        with logfile.open('a') as log:
            log.write(json.dumps(event)+'\n')
        assert (out.returncode == 0) == ok, event
        if not ok:
            return event
        values = [json.loads(line) for line in out.stdout.splitlines() if line.strip()]
        if frames:
            return values
        assert len(values) == 1 and values[0].get('success') is True, event
        return values[0]['data']

    execute('init')
    rules = [
        ('directory', 'src/payments', 'outboxpathcanary DirGuidance: preserve the transactional outbox during invoice delivery.'),
        ('file_pattern', 'tests/*.rs', 'outboxpathcanary TestGuidance: use a deterministic test clock for expiry checks.'),
        ('workspace', None, 'outboxpathcanary GeneralGuidance: retain source provenance when publishing changes.'),
    ]
    sources = []
    for i, (scope, pattern, body) in enumerate(rules):
        saved = execute('remember', f'Independent source observation number {i} about engineering decisions.',
                        '--kind', 'fact', '--level', 'semantic', '--source', f'manual://scope-source-{i}')
        source = saved.get('memoryId') or saved.get('memory_id')
        assert source, saved
        sources.append(source)
        flags = ['--scope-pattern', pattern] if pattern else []
        execute('rule', 'add', body, '--scope', scope, *flags, '--maturity', 'validated',
                '--source-memory', source, '--confidence', '0.95', '--utility', '0.9')
    execute('index', 'rebuild')
    query = 'outboxpathcanary'
    flags = ['--source-mode', 'lexical_only', '--strict-source-mode', '--speed', 'instant',
             '--max-tokens', '6000', '--candidate-pool', '32', '--relevance-floor', '0',
             '--pack-profile', 'verbose', '--no-lod', '--as-of', '2030-01-01T00:00:00Z']

    def pack(paths: list[str], persist: bool = False):
        targets = [part for path in paths for part in ['--task-path', path]]
        return execute('pack', query, *flags, *targets, *([] if persist else ['--read-only']))

    def selected(data):
        return json.dumps(data['pack']['items'])

    none = pack([])
    assert 'taskPaths' not in none['request'], none['request']
    assert 'DirGuidance' not in selected(none) and 'TestGuidance' not in selected(none), none
    assert 'GeneralGuidance' in selected(none), none
    directory = pack(['src/payments/invoice.rs'])
    assert directory['request']['taskPaths'] == ['src/payments/invoice.rs'], directory
    assert 'DirGuidance' in selected(directory) and 'TestGuidance' not in selected(directory), directory
    other = pack(['src/payments-other/invoice.rs'])
    assert 'DirGuidance' not in selected(other), other
    tests = pack(['tests/expiry.rs'])
    assert 'TestGuidance' in selected(tests) and 'DirGuidance' not in selected(tests), tests
    paths = ['src/payments/invoice.rs', 'tests/expiry.rs']
    both = pack(paths)
    assert 'DirGuidance' in selected(both) and 'TestGuidance' in selected(both), both
    reordered = pack(['tests/expiry.rs', './src/payments/invoice.rs', 'tests/expiry.rs'])
    assert reordered['request']['taskPaths'] == paths, reordered
    assert both['pack']['hash'] == reordered['pack']['hash'], (both, reordered)
    equivalent_selection = pack(['src/payments/other.rs'])
    assert 'DirGuidance' in selected(equivalent_selection), equivalent_selection
    assert directory['pack']['hash'] != equivalent_selection['pack']['hash'], 'Task identity must bind even identical selected guidance'

    for bad in ['../private-root/file.rs', '/private-root/file.rs', 'src/*.rs', 'src/../outside.rs']:
        failure = execute('pack', query, *flags, '--task-path', bad, '--read-only', ok=False)
        assert 'private-root' not in failure['stdout'], failure
    outside = root/'outside'
    outside.mkdir()
    (workspace/'escape').symlink_to(outside, target_is_directory=True)
    execute('pack', query, *flags, '--task-path', 'escape/file.rs', '--read-only', ok=False)

    persisted = pack(paths, persist=True)
    assert persisted['request']['taskPaths'] == paths
    why = execute('why', sources[0])
    selection = why['selection']['latestPackSelection']
    pack_id = selection['packId']
    replay = execute('pack', 'replay', pack_id)
    assert replay['replay']['status'] == 'available', replay
    records = replay['replay']['ledger']['request']['taskPaths']
    assert [record['text'] for record in records] == paths, replay
    assert all(not record['redacted'] and record['hash'].startswith('blake3:') for record in records)
    shown = execute('context', 'show', pack_id)
    assert shown['pack']['taskPaths'] == records, shown

    stream = execute('pack', query, *flags, '--task-path', paths[0], '--stream', '--read-only', frames=True)
    headers = [frame for frame in stream if frame.get('kind') == 'header']
    assert len(headers) == 1 and headers[0]['taskPaths'] == [paths[0]], stream
    assert any(frame.get('kind') in ['trailer', 'complete', 'footer'] for frame in stream), stream
    print('PASS: literal task scopes, canonical hashes, rejection, durable replay and stream targets', flush=True)


if __name__ == '__main__':
    main()
