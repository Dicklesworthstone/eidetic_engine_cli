#!/usr/bin/env python3
import argparse, json, subprocess, sys


def consume(response):
    if not response.get("success"):
        error = response.get("error", {})
        return f"<!-- ee error: {error.get('code', 'unknown')} -->"

    pack = response["data"]["pack"]
    if pack.get("text"):
        return pack["text"]

    budget = pack.get("budget", {})
    lines = [
        f"# Context Pack: {pack.get('query', '')}\n\n",
        f"**Budget:** {budget.get('usedTokens', 0)}/{budget.get('maxTokens', 0)} tokens\n",
    ]
    for item in pack.get("items", []):
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
    if args.from_stdin or not args.query:
        return json.load(sys.stdin)

    command = [
        args.ee,
        "context",
        args.query,
        "--workspace",
        args.workspace,
        "--max-tokens",
        str(args.max_tokens),
        "--json",
    ]
    output = subprocess.check_output(command, text=True)
    return json.loads(output)


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
