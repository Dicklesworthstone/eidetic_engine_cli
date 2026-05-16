#!/usr/bin/env python3
"""Lint RCH documentation command examples for local-Cargo bypasses.

The checker treats fenced shell blocks in the RCH docs as executable contract
material. RCH-specific blocks in AGENTS.md and README.md are also scanned, while
general installation/build prose in those files is ignored.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable


SCHEMA = "ee.rch_doc_examples.v1"
SMOKE_SCHEMA = "ee.rch_doc_examples.smoke_command.v1"
DEFAULT_FILES = [
    "docs/rch_runbook.md",
    "docs/rch_verification.md",
    "AGENTS.md",
    "README.md",
]
SHELL_LANGS = {"", "bash", "sh", "shell", "zsh", "console"}
RCH_BLOCK_RE = re.compile(
    r"(\bRCH\b|\brch\b|RCH_[A-Z0-9_]+|scripts/rch_verify\.sh|check-local-cargo-tripwire|"
    r"check-rch-portability|CARGO_TARGET_DIR)"
)
FORBIDDEN_CARGO_RE = re.compile(
    r"(^|[;\s])(?:[A-Z_][A-Z0-9_]*=[^\s]+\s+)*cargo\s+"
    r"(build|check|test|bench|clippy|run|install|rustc|fix)(\s|$)"
)
RCH_EXEC_RE = re.compile(r"(^|[;\s/])rch(\s+--json)?\s+exec(\s|--|$)")
RCH_VERIFY_RE = re.compile(r"(^|[;\s])(?:bash\s+)?(?:\./)?scripts/rch_verify\.sh(\s|$)")
TRIPWIRE_RE = re.compile(r"(^|[;\s])(?:sh\s+|bash\s+)?(?:\./)?scripts/check-local-cargo-tripwire\.sh(\s|$)")
ROUTER_RE = re.compile(r"(^|[;\s])(?:python3\s+)?(?:\./)?scripts/rch_compile_blocker_router\.py(\s|$)")


@dataclass(frozen=True)
class CodeBlock:
    index: int
    language: str
    start_line: int
    body: str


@dataclass(frozen=True)
class CommandExample:
    file: str
    block_index: int
    block_start_line: int
    line: int
    command: str
    language: str


def command_hash(command: str) -> str:
    return hashlib.sha256(command.encode("utf-8")).hexdigest()


def parse_fenced_blocks(text: str) -> list[CodeBlock]:
    blocks: list[CodeBlock] = []
    in_block = False
    language = ""
    start_line = 0
    body: list[str] = []
    block_index = 0

    for line_number, line in enumerate(text.splitlines(), start=1):
        stripped = line.strip()
        if not in_block:
            if stripped.startswith("```"):
                in_block = True
                fence_info = stripped[3:].strip().split(None, 1)
                language = fence_info[0].lower() if fence_info else ""
                start_line = line_number
                body = []
            continue

        if stripped.startswith("```"):
            blocks.append(
                CodeBlock(
                    index=block_index,
                    language=language,
                    start_line=start_line,
                    body="\n".join(body),
                )
            )
            block_index += 1
            in_block = False
            language = ""
            start_line = 0
            body = []
            continue

        body.append(line)

    return blocks


def strip_prompt(line: str) -> str:
    return re.sub(r"^\s*(?:\$|>)\s+", "", line).strip()


def commands_from_block(path: str, block: CodeBlock) -> list[CommandExample]:
    commands: list[CommandExample] = []
    continuation = ""
    continuation_line = block.start_line + 1

    def push(command: str, line_number: int) -> None:
        normalized = " ".join(command.split())
        if normalized:
            commands.append(
                CommandExample(
                    file=path,
                    block_index=block.index,
                    block_start_line=block.start_line,
                    line=line_number,
                    command=normalized,
                    language=block.language,
                )
            )

    for offset, raw in enumerate(block.body.splitlines(), start=1):
        line_number = block.start_line + offset
        line = strip_prompt(raw)
        if not line or line.startswith("#"):
            if continuation:
                push(continuation, continuation_line)
                continuation = ""
            continue

        if line.endswith("\\"):
            fragment = line[:-1].strip()
            if not continuation:
                continuation_line = line_number
                continuation = fragment
            else:
                continuation = f"{continuation} {fragment}".strip()
            continue

        if continuation:
            push(f"{continuation} {line}", continuation_line)
            continuation = ""
        else:
            push(line, line_number)

    if continuation:
        push(continuation, continuation_line)

    return commands


def is_rch_specific(path: str, block: CodeBlock) -> bool:
    if path in {"docs/rch_runbook.md", "docs/rch_verification.md"}:
        return True
    return RCH_BLOCK_RE.search(block.body) is not None


def wrapper_before_cargo(command: str, pattern: re.Pattern[str], cargo_start: int) -> bool:
    match = pattern.search(command)
    return match is not None and match.start() < cargo_start


def classify_command(command: str) -> tuple[str, str]:
    cargo_match = FORBIDDEN_CARGO_RE.search(command)
    if not cargo_match:
        return ("shell_or_non_compile", "command does not run a forbidden Cargo compilation subcommand")

    cargo_start = cargo_match.start()
    if wrapper_before_cargo(command, RCH_EXEC_RE, cargo_start):
        return ("rch_exec_wrapper", "Cargo command is wrapped through rch exec")
    if wrapper_before_cargo(command, RCH_VERIFY_RE, cargo_start):
        return ("rch_verify_wrapper", "Cargo command is wrapped through scripts/rch_verify.sh")
    if wrapper_before_cargo(command, TRIPWIRE_RE, cargo_start):
        return ("tripwire_detector_example", "Cargo text is an input to the local-Cargo tripwire")
    if wrapper_before_cargo(command, ROUTER_RE, cargo_start):
        return ("compile_blocker_router_example", "Cargo text is metadata for the compile-blocker router")

    return (
        "denied_bare_cargo",
        "direct Cargo compilation command is not wrapped by rch exec or scripts/rch_verify.sh",
    )


def scan_file(repo_root: Path, relative_path: str) -> dict:
    path = repo_root / relative_path
    text = path.read_text(encoding="utf-8")
    result = {
        "path": relative_path,
        "mode": "scan_all" if relative_path.startswith("docs/rch_") else "scan_rch_blocks_only",
        "fenced_blocks": 0,
        "command_blocks": 0,
        "commands": 0,
        "allowed": 0,
        "denied": 0,
        "skipped_blocks": 0,
        "allowlist_hits": {},
        "denials": [],
        "smoke_candidates": [],
    }

    for block in parse_fenced_blocks(text):
        result["fenced_blocks"] += 1
        if block.language not in SHELL_LANGS:
            result["skipped_blocks"] += 1
            continue
        if not is_rch_specific(relative_path, block):
            result["skipped_blocks"] += 1
            continue

        examples = commands_from_block(relative_path, block)
        if not examples:
            result["skipped_blocks"] += 1
            continue

        result["command_blocks"] += 1
        for example in examples:
            result["commands"] += 1
            kind, reason = classify_command(example.command)
            if "scripts/rch_verify.sh" in example.command and "--dry-run" in example.command:
                result["smoke_candidates"].append(
                    {
                        "source_file": example.file,
                        "block_index": example.block_index,
                        "line": example.line,
                        "command_hash": command_hash(example.command),
                        "command": example.command,
                    }
                )
            if kind == "denied_bare_cargo":
                result["denied"] += 1
                result["denials"].append(
                    {
                        "file": example.file,
                        "line": example.line,
                        "block_index": example.block_index,
                        "command_hash": command_hash(example.command),
                        "command_excerpt": example.command[:240],
                        "classifier_reason": reason,
                        "suggested_rch_wrapper": "scripts/rch_verify.sh -- <cargo command>",
                    }
                )
            else:
                result["allowed"] += 1
                hits = result["allowlist_hits"]
                hits[kind] = hits.get(kind, 0) + 1

    return result


def build_report(repo_root: Path, files: Iterable[str]) -> dict:
    checked_files = [scan_file(repo_root, path) for path in files]
    denials = [denial for file_report in checked_files for denial in file_report["denials"]]
    allowlist_hits: dict[str, int] = {}
    for file_report in checked_files:
        for key, value in file_report["allowlist_hits"].items():
            allowlist_hits[key] = allowlist_hits.get(key, 0) + value

    report = {
        "schema": SCHEMA,
        "status": "denied" if denials else "ok",
        "checked_files": checked_files,
        "checked_file_count": len(checked_files),
        "command_count": sum(item["commands"] for item in checked_files),
        "allowed_count": sum(item["allowed"] for item in checked_files),
        "denied_count": len(denials),
        "allowlist_hits": dict(sorted(allowlist_hits.items())),
        "first_failure": denials[0] if denials else None,
        "denials": denials,
    }
    return report


def extract_smoke_command(report: dict) -> dict:
    candidates = [
        candidate
        for file_report in report["checked_files"]
        for candidate in file_report["smoke_candidates"]
    ]
    if not candidates:
        raise SystemExit("no smoke command found in scanned docs")
    candidate = candidates[0]
    return {
        "schema": SMOKE_SCHEMA,
        **candidate,
    }


def run_self_test() -> None:
    cases = {
        "docs/rch_runbook.md": """```bash
scripts/rch_verify.sh -- cargo test --lib good -- --nocapture
```
```bash
RCH_REQUIRE_REMOTE=1 cargo test --lib bad
```
```text
cargo test transcript output is ignored here
```
""",
        "AGENTS.md": """```bash
cargo build --release
```
```bash
RCH_REQUIRE_REMOTE=1 cargo test --lib bad
```
```bash
rch exec -- env TMPDIR=/tmp cargo clippy --all-targets -- -D warnings
```
""",
        "README.md": """```bash
cargo install eidetic-engine
```
```bash
scripts/check-local-cargo-tripwire.sh --cmd 'cargo test --lib bad' --json
```
""",
        "docs/rch_verification.md": """```bash
scripts/rch_verify.sh --dry-run -- cargo test --lib smoke -- --nocapture
```
""",
    }

    reports = []
    for path, text in cases.items():
        blocks = []
        for block in parse_fenced_blocks(text):
            fake_path = Path(path)
            del fake_path
            result = {
                "path": path,
                "mode": "scan_all" if path.startswith("docs/rch_") else "scan_rch_blocks_only",
                "fenced_blocks": 0,
                "command_blocks": 0,
                "commands": 0,
                "allowed": 0,
                "denied": 0,
                "skipped_blocks": 0,
                "allowlist_hits": {},
                "denials": [],
                "smoke_candidates": [],
            }
            # Reuse the normal command classifier while avoiding temp files.
            result["fenced_blocks"] += 1
            if block.language in SHELL_LANGS and is_rch_specific(path, block):
                examples = commands_from_block(path, block)
                if examples:
                    result["command_blocks"] += 1
                for example in examples:
                    result["commands"] += 1
                    kind, reason = classify_command(example.command)
                    if "scripts/rch_verify.sh" in example.command and "--dry-run" in example.command:
                        result["smoke_candidates"].append(
                            {
                                "source_file": example.file,
                                "block_index": example.block_index,
                                "line": example.line,
                                "command_hash": command_hash(example.command),
                                "command": example.command,
                            }
                        )
                    if kind == "denied_bare_cargo":
                        result["denied"] += 1
                        result["denials"].append(
                            {
                                "file": example.file,
                                "line": example.line,
                                "block_index": example.block_index,
                                "command_hash": command_hash(example.command),
                                "command_excerpt": example.command[:240],
                                "classifier_reason": reason,
                                "suggested_rch_wrapper": "scripts/rch_verify.sh -- <cargo command>",
                            }
                        )
                    else:
                        result["allowed"] += 1
                        hits = result["allowlist_hits"]
                        hits[kind] = hits.get(kind, 0) + 1
            else:
                result["skipped_blocks"] += 1
            blocks.append(result)
        merged = {
            "path": path,
            "mode": "scan_all" if path.startswith("docs/rch_") else "scan_rch_blocks_only",
            "fenced_blocks": sum(item["fenced_blocks"] for item in blocks),
            "command_blocks": sum(item["command_blocks"] for item in blocks),
            "commands": sum(item["commands"] for item in blocks),
            "allowed": sum(item["allowed"] for item in blocks),
            "denied": sum(item["denied"] for item in blocks),
            "skipped_blocks": sum(item["skipped_blocks"] for item in blocks),
            "allowlist_hits": {},
            "denials": [denial for item in blocks for denial in item["denials"]],
            "smoke_candidates": [
                candidate for item in blocks for candidate in item["smoke_candidates"]
            ],
        }
        for item in blocks:
            for key, value in item["allowlist_hits"].items():
                merged["allowlist_hits"][key] = merged["allowlist_hits"].get(key, 0) + value
        reports.append(merged)

    denials = [denial for report in reports for denial in report["denials"]]
    if len(denials) != 2:
        raise SystemExit(f"self-test expected exactly 2 denials, got {len(denials)}: {denials}")
    if not any("bad" in denial["command_excerpt"] for denial in denials):
        raise SystemExit("self-test denials did not include the synthetic bad cargo commands")
    if not any(report["skipped_blocks"] for report in reports if report["path"] == "README.md"):
        raise SystemExit("self-test expected README non-RCH cargo block to be skipped")
    if not any(report["smoke_candidates"] for report in reports):
        raise SystemExit("self-test expected a dry-run smoke candidate")
    print("self-test passed")


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo-root", default=".", help="repository root")
    parser.add_argument("--json", action="store_true", help="emit JSON report")
    parser.add_argument("--self-test", action="store_true", help="run built-in classifier tests")
    parser.add_argument(
        "--extract-smoke-command",
        action="store_true",
        help="emit the first dry-run scripts/rch_verify.sh command extracted from real docs",
    )
    parser.add_argument("files", nargs="*", help="markdown files to scan")
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    if args.self_test:
        run_self_test()
        return 0

    repo_root = Path(args.repo_root).resolve()
    files = args.files or DEFAULT_FILES
    report = build_report(repo_root, files)
    payload = extract_smoke_command(report) if args.extract_smoke_command else report

    if args.json or args.extract_smoke_command:
        print(json.dumps(payload, sort_keys=True, separators=(",", ":")))
    else:
        print(
            f"RCH doc example lint: {report['status']} "
            f"({report['allowed_count']} allowed, {report['denied_count']} denied)"
        )
        if report["first_failure"]:
            failure = report["first_failure"]
            print(
                f"{failure['file']}:{failure['line']}: {failure['classifier_reason']}\n"
                f"  {failure['command_excerpt']}\n"
                f"  fix: {failure['suggested_rch_wrapper']}"
            )

    return 1 if report["denied_count"] else 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
