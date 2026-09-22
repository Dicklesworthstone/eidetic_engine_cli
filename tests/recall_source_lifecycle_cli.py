#!/usr/bin/env python3
"""Public capture/revise/expire/seal -> code recall without direct database edits."""
from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from typing import Any


def main() -> None:
    if len(sys.argv) != 2:
        raise SystemExit('usage: recall_source_lifecycle_cli.py /path/to/ee')
    binary = Path(sys.argv[1]).resolve(strict=True)
    root = Path(tempfile.mkdtemp(prefix='ee-recall-source-')).resolve()
    workspace = root / 'workspace'
    workspace.mkdir()
    env = {key: value for key, value in os.environ.items() if not key.startswith('EE_')}
    env.update(EE_EMBED_DOWNLOAD='off', NO_COLOR='1', XDG_DATA_HOME=str(root / 'data'),
               XDG_CONFIG_HOME=str(root / 'config'), XDG_CACHE_HOME=str(root / 'cache'))
    log = root / 'commands.jsonl'
    print(f'Recall lifecycle command log: {log}', flush=True)

    def run(*args: str) -> dict[str, Any]:
        command = [str(binary), '--workspace', str(workspace), '--json', *args]
        output = subprocess.run(command, cwd=root, env=env, text=True, capture_output=True,
                                timeout=180, check=False)
        event = {'schema': 'ee.test_event.v1', 'testId': 'recall_source_lifecycle_cli',
                 'command': command, 'exitCode': output.returncode,
                 'stdout': output.stdout, 'stderr': output.stderr}
        with log.open('a') as stream:
            stream.write(json.dumps(event) + '\n')
        assert output.returncode == 0, event
        response = json.loads(output.stdout)
        assert response['schema'] == 'ee.response.v2' and response['success'] is True, event
        return response

    def remember(content: str, *extra: str) -> str:
        return run('remember', content, '--level', 'procedural', '--kind', 'rule',
                   '--source', 'manual://recall-lifecycle', '--valid-from',
                   '2020-01-01T00:00:00Z', *extra)['data']['memoryId']

    def recall(*selectors: str) -> dict[str, Any]:
        response = run('recall', *selectors)
        result = response['data']['recall']
        assert result['schema'] == 'ee.recall.v1', response
        assert not any(d['code'] == 'embed_model_unavailable' for d in response.get('degraded', [])), response
        return result

    def ids(result: dict[str, Any]) -> set[str]:
        rows = result['items']
        result_ids = {row['memoryId'] for row in rows}
        assert len(result_ids) == len(rows), result
        return result_ids

    run('init')
    old_body = 'Use the original release routine. anchor:path:src/old_release.rs anchor:symbol:OldRelease::run'
    original = remember(old_body)
    for selectors in [('--path', 'src/old_release.rs'), ('--path', 'src/old*'),
                      ('--symbol', 'OldRelease::run')]:
        assert ids(recall(*selectors)) == {original}

    before = run('memory', 'show', original)['data']['memory']
    first = recall('--path', 'src/*')
    assert recall('--path', 'src/*') == first, 'Identical read-only recall must be stable'
    assert run('memory', 'show', original)['data']['memory'] == before, 'Recall changed its source'
    preview = run('memory', 'revise', original, '--content',
                  'Use the replacement release routine. anchor:path:src/new_release.rs anchor:symbol:NewRelease::run',
                  '--dry-run')['data']
    assert preview['dry_run'] and preview['new_id'] is None and not preview['persisted'], preview
    assert ids(recall('--path', 'src/old_release.rs')) == {original}
    changed = run('memory', 'revise', original, '--content',
                  'Use the replacement release routine. anchor:path:src/new_release.rs anchor:symbol:NewRelease::run')['data']
    latest = changed['new_id']
    assert latest != original and changed['persisted'], changed
    assert ids(recall('--path', 'src/new_release.rs')) == {latest}
    assert ids(recall('--symbol', 'NewRelease::run')) == {latest}
    for selectors in [('--path', 'src/old_release.rs'), ('--symbol', 'OldRelease::run')]:
        result = recall(*selectors)
        assert not ids(result) and result['totalMatched'] == 0, result
        assert old_body not in json.dumps(result), result
    combined = recall('--path', 'src/*', '--symbol', 'NewRelease::run')
    assert ids(combined) == {latest}, combined

    expired = remember('Retired release route. anchor:path:src/expired.rs', '--valid-to', '2021-01-01T00:00:00.123456789Z')
    future = run('remember', 'Future release route. anchor:path:src/future.rs', '--level', 'procedural',
                 '--kind', 'rule', '--source', 'manual://recall-lifecycle',
                 '--valid-from', '2099-01-01T00:00:00.123456789Z')['data']['memoryId']
    assert expired not in ids(recall('--path', 'src/expired.rs'))
    assert future not in ids(recall('--path', 'src/future.rs'))
    assert ids(recall('--path', 'src/*')) == {latest}

    sealed_body = 'Registered release procedure. anchor:path:src/sealed.rs anchor:symbol:SealedRelease::run'
    sealed = remember(sealed_body, '--seal')
    hidden = recall('--path', 'src/sealed.rs', '--symbol', 'SealedRelease::run')
    assert not ids(hidden) and sealed_body not in json.dumps(hidden), hidden
    supplied = root / 'sealed-content.txt'
    supplied.write_text(sealed_body)
    revealed = run('memory', 'reveal', sealed, '--content-file', str(supplied))['data']
    revealed_id = revealed['revealedMemoryId']
    assert revealed['revealVerified'] is True and revealed_id != sealed, revealed
    assert ids(recall('--path', 'src/sealed.rs', '--symbol', 'SealedRelease::run')) == {revealed_id}
    assert ids(recall('--path', 'src/*')) == {latest, revealed_id}
    print('PASS: public anchored recall respects revision, validity and seal lifecycle without models', flush=True)


if __name__ == '__main__':
    main()
