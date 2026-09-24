#!/usr/bin/env python3
"""GH #49: exercise persisted candidate pools through the public CLI.

All stores and user configuration are isolated. Failures retain command logs.
No model downloads, SQL edits, process-wide environment mutations, or sleeps.
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
        raise SystemExit("usage: pack_candidate_pool_cli.py /path/to/ee")
    binary = Path(sys.argv[1]).resolve(strict=True)
    root = Path(tempfile.mkdtemp(prefix="ee-candidate-pool-")).resolve()
    workspace = root / "workspace"
    workspace.mkdir()
    home = root / "home"
    home.mkdir()
    env = {k: v for k, v in os.environ.items() if not k.startswith("EE_")}
    env.update(HOME=str(home), USERPROFILE=str(home), EE_EMBED_DOWNLOAD="off",
               XDG_DATA_HOME=str(root / "data"), XDG_CONFIG_HOME=str(home / ".config"),
               XDG_CACHE_HOME=str(root / "cache"), NO_COLOR="1")
    logfile = root / "commands.jsonl"
    print(f"Candidate-pool command log: {logfile}", flush=True)

    def execute(*args: str, ok: bool = True):
        command = [str(binary), "--workspace", str(workspace), "--json", *args]
        result = subprocess.run(command, cwd=root, env=env, text=True,
                                capture_output=True, check=False, timeout=240)
        record = dict(command=command, exitCode=result.returncode,
                      stdout=result.stdout, stderr=result.stderr)
        with logfile.open("a") as log:
            log.write(json.dumps(record) + "\n")
        assert (result.returncode == 0) == ok, record
        if not ok:
            return record
        output = json.loads(result.stdout)
        assert output.get("success") is True, record
        return output["data"]

    key = "pack.candidate_pool"
    default = execute("config", "get", key)
    assert (default["value"], default["source"]) == ("100", "default"), default
    planned = execute("config", "set", key, "7", "--dry-run")
    assert planned["wouldWrite"] and not planned["applied"], planned
    assert not (workspace / ".ee" / "config.toml").exists()
    user_config = home / ".config" / "ee" / "config.toml"
    execute("config", "set", key, "13", "--config", str(user_config))
    user = execute("config", "get", key)
    assert (user["value"], user["source"]) == ("13", "user"), user
    execute("init")
    execute("remember", "poolconfigurationcanary: verify persisted retrieval settings",
            "--kind", "fact", "--level", "semantic")
    execute("index", "rebuild")
    common = ["--source-mode", "lexical_only", "--strict-source-mode", "--speed", "instant",
              "--read-only", "--max-tokens", "1000", "--relevance-floor", "0",
              "--pack-profile", "verbose"]
    query_file = root / "pool.eeq.json"
    query = {"version": "ee.query.v1", "query": {"text": "poolconfigurationcanary"}}
    query_file.write_text(json.dumps(query))

    def pool(*command: str) -> int:
        response = execute(*command, *common)
        return response["request"]["candidatePool"]

    assert pool("pack", "poolconfigurationcanary") == 13
    assert pool("context", "poolconfigurationcanary") == 13
    execute("config", "set", key, "7")
    project = execute("config", "get", key)
    assert (project["value"], project["source"]) == ("7", "project"), project
    assert pool("pack", "poolconfigurationcanary") == 7
    assert pool("context", "poolconfigurationcanary") == 7
    assert pool("pack", "--query-file", str(query_file)) == 7
    assert pool("pack", "build", "--query-file", str(query_file)) == 7
    assert pool("pack", "poolconfigurationcanary", "--candidate-pool", "11") == 11
    query["budget"] = {"candidatePool": 9}
    query_file.write_text(json.dumps(query))
    assert pool("pack", "build", "--query-file", str(query_file)) == 9
    assert pool("pack", "build", "--query-file", str(query_file), "--candidate-pool", "11") == 11
    execute("config", "set", key, "5")
    assert pool("pack", "poolconfigurationcanary") == 5
    config = workspace / ".ee" / "config.toml"
    before = config.read_bytes()
    for invalid in ["0", "4294967296", "1.5", "true", "not-a-number"]:
        execute("config", "set", key, invalid, ok=False)
        assert config.read_bytes() == before, invalid
    print("PASS: candidate-pool defaults, persistence, precedence, public pack paths and rejected writes", flush=True)


if __name__ == "__main__":
    main()
