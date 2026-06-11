#!/usr/bin/env python3
import argparse
import json
import subprocess
import sys
from collections.abc import Mapping


MALFORMED_RESPONSE_CODE = "malformed_pack_response"


def error_comment(code):
    return f"<!-- ee error: {code or 'unknown'} -->"


def error_code(response):
    if not isinstance(response, Mapping):
        return MALFORMED_RESPONSE_CODE
    error = response.get("error", {})
    if isinstance(error, Mapping):
        return error.get("code", "unknown")
    return "unknown"


def consume(response):
    if not isinstance(response, Mapping):
        return error_comment(MALFORMED_RESPONSE_CODE)
    if response.get("success") is not True:
        return error_comment(error_code(response))

    data = response.get("data")
    if not isinstance(data, Mapping):
        return error_comment(MALFORMED_RESPONSE_CODE)
    pack = data.get("pack")
    if not isinstance(pack, Mapping):
        return error_comment(MALFORMED_RESPONSE_CODE)

    text = pack.get("text")
    if text:
        if not isinstance(text, str):
            return error_comment(MALFORMED_RESPONSE_CODE)
        return text

    budget = pack.get("budget", {})
    if not isinstance(budget, Mapping):
        return error_comment(MALFORMED_RESPONSE_CODE)
    items = pack.get("items", [])
    if not isinstance(items, list):
        return error_comment(MALFORMED_RESPONSE_CODE)
    lines = [
        f"# Context Pack: {pack.get('query', '')}\n\n",
        f"**Budget:** {budget.get('usedTokens', 0)}/{budget.get('maxTokens', 0)} tokens\n",
    ]
    for item in items:
        if not isinstance(item, Mapping):
            return error_comment(MALFORMED_RESPONSE_CODE)
        why = item.get("why", "")
        lines.append(
            f"\n## {item.get('section', 'memory')} {item.get('rank', '?')}. "
            f"{item.get('memoryId', '')}\n\n"
        )
        lines.append(f"```\n{item.get('content', '')}\n```\n")
        if why:
            lines.append(f"\n**Why:** {why}\n")
    return "".join(lines)


def load_response(args):
    try:
        if args.from_stdin or not args.query:
            return json.load(sys.stdin)

        command = [args.ee, "pack", args.query, "--workspace", args.workspace, "--max-tokens", str(args.max_tokens), "--json"]
        output = subprocess.check_output(command, text=True)
        return json.loads(output)
    except json.JSONDecodeError as e:
        print(f"Error decoding JSON response: {e}", file=sys.stderr)
        sys.exit(1)
    except subprocess.CalledProcessError as e:
        print(f"Error executing command: {e}", file=sys.stderr)
        sys.exit(1)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--from-stdin", action="store_true")
    parser.add_argument("--workspace", default=".")
    parser.add_argument("--query")
    parser.add_argument("--max-tokens", type=int, default=1000)
    parser.add_argument("--ee", default="ee")
    args = parser.parse_args()
    print(consume(load_response(args)))


if __name__ == "__main__":
    main()
