#!/usr/bin/env python3
"""Minimal `cass` emulator over a generated real-shape corpus.

bd-reality-core-convergence-1azkt.43. Serves the subset of the cass robot
contract `ee import cass` uses (`sessions --json`, `view -n -C --json`,
`index`, `health --json`) straight from ``$CASS_FIXTURE_ROOT/manifest.json``.
Every response is logged as "stub" in ``$CASS_FIXTURE_LOG`` when set, so a run
through this emulator can never be mistaken for one through real cass.
"""

import json
import os
import sys


def root():
    value = os.environ.get("CASS_FIXTURE_ROOT")
    if not value:
        sys.stderr.write("CASS_FIXTURE_ROOT is not set\n")
        sys.exit(70)
    return value


def log(args):
    path = os.environ.get("CASS_FIXTURE_LOG")
    if path:
        with open(path, "a", encoding="utf-8") as fh:
            fh.write("stub " + " ".join(args) + "\n")


def manifest():
    with open(os.path.join(root(), "manifest.json"), encoding="utf-8") as fh:
        return json.load(fh)


def opt(args, name, default=None):
    if name in args:
        i = args.index(name)
        if i + 1 < len(args):
            return args[i + 1]
    return default


def cmd_sessions(args):
    m = manifest()
    limit = int(opt(args, "--limit", "1000000"))
    workspace = opt(args, "--workspace", m["workspace"])
    rows = []
    # Most recent first, as cass lists them.
    for s in sorted(m["sessions"], key=lambda s: s["started_at"], reverse=True)[:limit]:
        rows.append({
            "path": os.path.join(root(), s["path"]),
            "agent": s["agent"],
            "workspace": workspace,
            "title": s["title"],
            "started_at": s["started_at"],
            "ended_at": s["ended_at"],
            "message_count": s["message_count"],
            "token_count": s["message_count"] * 180,
        })
    print(json.dumps({"sessions": rows}))


def cmd_view(args):
    target = int(opt(args, "-n", "1"))
    context = int(opt(args, "-C", "4"))
    path = args[args.index("--") + 1] if "--" in args else args[-1]
    with open(path, encoding="utf-8") as fh:
        lines = fh.read().split("\n")
    if lines and lines[-1] == "":
        lines.pop()
    lo = max(1, target - context)
    hi = min(len(lines), target + context)
    out = [{"line": n, "content": lines[n - 1]} for n in range(lo, hi + 1)]
    print(json.dumps({"lines": out, "total_lines": len(lines)}))


def main():
    args = sys.argv[1:]
    log(args)
    command = args[0] if args else ""
    if command == "sessions":
        cmd_sessions(args)
    elif command == "view":
        cmd_view(args)
    elif command == "index":
        print(json.dumps({"success": True, "conversations": len(manifest()["sessions"])}))
    elif command == "health":
        print(json.dumps({"status": "ok", "healthy": True}))
    else:
        sys.stderr.write(f"unexpected cass fixture command: {' '.join(args)}\n")
        sys.exit(64)


if __name__ == "__main__":
    main()
