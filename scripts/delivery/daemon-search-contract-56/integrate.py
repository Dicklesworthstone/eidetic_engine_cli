#!/usr/bin/env python3
"""Complete GH #56 integration, including normal-build diagnostic wiring.

Reuse the reviewed producer/validator transforms without their incomplete CLI
rewrite. Compute every edit before writing; refuse unexpected or partial input.
"""
from __future__ import annotations

import argparse
import importlib.util
import os
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[3]


def require(condition: bool, message: str) -> None:
    if not condition:
        raise RuntimeError(message)


def replace_once(text: str, old: str, new: str, label: str) -> str:
    require(text.count(old) == 1, f"{label}: expected exactly one reviewed anchor")
    return text.replace(old, new, 1)


def normalized(text: str) -> str:
    return re.sub(r"\s+", "", text)


def updated_cli(text: str) -> str:
    link = "mod daemon_search_diagnostics;\n"
    lossy = r'\.map_err\(\|_\|\s*DaemonSearchFallbackReason::SearchResponseDrift\s*\)'
    if link in text:
        require(text.count(link) == 1 and
                "SearchResponseValidationError(String)" in text and
                "Self::SearchResponseValidationError(_)" in text and
                "const fn as_str(&self)" in text and
                "SearchDegradation::daemon_fallback(&reason.to_string())" in normalized(text) and
                not re.search(lossy, text), "partial fallback diagnostic integration")
        require(not re.search(r'#\[cfg\(test\)\]\s*mod daemon_search_diagnostics;', text),
                "diagnostics must also compile outside tests")
        return text
    old_enum = "#[derive(Clone, Copy, Debug, Eq, PartialEq)]\nenum DaemonSearchFallbackReason {"
    new_enum = "#[derive(Clone, Debug, Eq, PartialEq)]\nenum DaemonSearchFallbackReason {"
    # Insert before the enum's derive, NOT between #[cfg(test)] and the nearby
    # fallback-code constant. Display is needed by production code too.
    text = replace_once(text, old_enum, link + "\n" + new_enum, "production diagnostic module")
    start = text.index("enum DaemonSearchFallbackReason {")
    end = text.index("\n}\n", start)
    enum = replace_once(text[start:end], "    SearchResponseDrift,",
                        "    SearchResponseDrift,\n    SearchResponseValidationError(String),",
                        "validation error payload")
    text = text[:start] + enum + text[end:]
    text = replace_once(text, "impl DaemonSearchFallbackReason {\n    const fn as_str(self) -> &'static str {",
                        "impl DaemonSearchFallbackReason {\n    const fn as_str(&self) -> &'static str {",
                        "borrow fallback category")
    text = replace_once(text, 'Self::SearchResponseDrift => "search response drift",',
                        'Self::SearchResponseDrift | Self::SearchResponseValidationError(_) => "search response drift",',
                        "stable fallback category")
    # The normal search validator returns String, but the pack decoder returns
    # serde_json::Error. A generic Display adapter preserves either error.
    text, count = re.subn(lossy, ".map_err(DaemonSearchFallbackReason::search_response_drift)", text)
    require(count > 0, "missing reviewed lossy validator mapping")
    text = replace_once(text, "SearchDegradation::daemon_fallback(reason.as_str())",
                        "SearchDegradation::daemon_fallback(&reason.to_string())",
                        "diagnostic string argument")
    # Keep unit-variant callers for missing performance/handoff/request drift:
    # those paths have no underlying validation error to attach.
    return text


def updated_global_promotion(text: str) -> str:
    """Repair three pre-existing test-only count type errors blocking lib tests."""
    old = '''        fn count(&self, table: &str) -> u64 {
            self.destination
                .count_table_rows(table)
                .expect("durable count")
        }'''
    new = old.replace('.expect("durable count")',
                      '.expect("durable count")\n                .try_into()\n                .expect("non-negative row count")')
    if old in text:
        text = replace_once(text, old, new, "test fixture count conversion")
    else:
        require(normalized(new) in normalized(text), "test fixture count changed")
    for counter in ("before_audits", "before_jobs"):
        old_sum = rf'\b{counter}\s*\+\s*u64::try_from\(successes\)\.unwrap\(\)'
        new_sum = f"{counter} + i64::try_from(successes).unwrap()"
        text, count = re.subn(old_sum, new_sum, text)
        require(count == 1 or (count == 0 and normalized(new_sum) in normalized(text)),
                f"{counter}: test row-count assertion changed")
    return text


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    if os.environ.get("GITHUB_ACTIONS") == "true":
        require(os.environ.get("EE_DAEMON_SEARCH_CONTRACT_DELIVERY") == "3",
                "outdated workflow cannot publish the complete source integration")
    spec = importlib.util.spec_from_file_location("reviewed_contract", Path(__file__).with_name("apply.py"))
    require(spec is not None and spec.loader is not None, "missing reviewed transforms")
    reviewed = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(reviewed)
    shared = reviewed.SHARED.read_text(encoding="utf-8")
    contract = reviewed.CONTRACT.read_text(encoding="utf-8")
    transforms = [
        (reviewed.SERVER, lambda text: reviewed.updated_server(text, contract, shared)),
        (reviewed.CORE, reviewed.updated_core),
        (reviewed.SEARCH, lambda text: reviewed.updated_search(text, shared)),
        (reviewed.CLI, updated_cli),
        (reviewed.SCHEMA, reviewed.updated_schema),
        (ROOT / "src/core/global_promotion.rs", updated_global_promotion),
    ]
    updates = []
    for path, transform in transforms:
        old = path.read_text(encoding="utf-8")
        updates.append((path, old, transform(old)))
    for path, old, new in updates:
        if args.check:
            require(old == new, f"repair not integrated: {path.relative_to(ROOT)}")
        elif old != new:
            path.write_text(new, encoding="utf-8")
            print(f"Updated {path.relative_to(ROOT)}")
    print("GH #56 production integration verified" if args.check else "GH #56 production integration ready")


if __name__ == "__main__":
    main()
