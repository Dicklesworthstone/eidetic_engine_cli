#!/usr/bin/env python3
"""Integrate GH #56's shared search contract and lossless fallback diagnostics.

The connected contents editor cannot replace these large source files safely.
Keep edits bounded to the reviewed producer, validator, and fallback sites;
refuse unexpected shapes and prepare every edit before writing any file.
No downloads, file deletions, branch changes, dependency changes, or force pushes.
"""
from __future__ import annotations

import argparse
from collections.abc import Mapping
import json
import os
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[3]
SERVER = ROOT / "src/daemon/server.rs"
CONTRACT = ROOT / "src/daemon/search_result_contract.rs"
SHARED = ROOT / "src/core/search_result_document.rs"
CORE = ROOT / "src/core/mod.rs"
SEARCH = ROOT / "src/core/search.rs"
CLI = ROOT / "src/cli/mod.rs"
SCHEMA = ROOT / "docs/schemas/ee.daemon.search.response.v3.json"
LINK = ('\n#[path = "search_result_contract.rs"]\nmod search_result_contract;\n'
        'use search_result_contract::validate_canonical_search_result;\n')
ID_CHECK = '''    if result
        .get("calibrationId")
        .is_some_and(|value| !value.is_string() && !value.is_null())
    {
        return Err(format!("{context}.calibrationId must be a string or null"));
    }
'''


def require(condition: bool, message: str) -> None:
    if not condition:
        raise RuntimeError(message)


def validate_delivery_environment(environment: Mapping[str, str]) -> None:
    # Actions uses the workflow definition from the triggering commit even
    # when checkout reads newer main. Older queued definitions stage only the
    # daemon files: letting them apply this extraction would publish a validator
    # that imports a core module they never committed. Refuse before any edit.
    if environment.get("GITHUB_ACTIONS") == "true":
        require(environment.get("EE_DAEMON_SEARCH_CONTRACT_DELIVERY") == "2",
                "outdated workflow cannot publish the shared-contract integration; "
                "use the current Daemon search contract workflow")


def replace_once(text: str, old: str, new: str, label: str) -> str:
    require(text.count(old) == 1, f"{label}: expected exactly one reviewed anchor")
    return text.replace(old, new, 1)


def without_comments(text: str) -> str:
    return re.sub(r"(?m)^\s*//[^\n]*", "", text)


def normalized(text: str) -> str:
    return re.sub(r"\s+", "", without_comments(text))


def shared_fields(shared: str, name: str) -> dict[str, str]:
    declaration = shared[shared.index("search_result_fields! {"):]
    match = re.search(rf"\b{name.lower()}\s*\{{(.*?)\}}", declaration, re.S)
    require(match is not None, f"missing shared {name} declaration")
    pairs = re.findall(r'(\w+)\s*=>\s*"([^"\n]+)"', without_comments(match.group(1)))
    result = {wire: variant for variant, wire in pairs}
    require(bool(result) and len(result) == len(pairs), "duplicate or empty shared field declaration")
    return result


def fields(text: str, name: str, shared: str) -> list[str]:
    match = re.search(rf"const {name}: &\[&str\] = &\[(.*?)\];", text, re.S)
    if match:
        return re.findall(r'"([^"\n]+)"', without_comments(match.group(1)))
    require("use crate::core::search_result_document::{OPTIONAL, REQUIRED};" in text,
            f"missing {name} fields or shared contract import")
    return list(shared_fields(shared, name))


def updated_server(text: str, contract: str, shared: str) -> str:
    start_marker = "fn validate_canonical_search_result("
    if LINK.strip() in text and start_marker not in text:
        return text
    require(LINK.strip() not in text, "partial server integration; inspect before retry")
    require(text.count(start_marker) == 1, "expected exactly one local result validator")
    start = text.index(start_marker)
    end = text.index("\nfn dispatch_pack_search(", start)
    old = text[start:end].strip()
    require(fields(old, "REQUIRED", shared) == fields(contract, "REQUIRED", shared),
            "required fields changed")
    require(fields(old, "OPTIONAL", shared) == [f for f in fields(contract, "OPTIONAL", shared)
                                              if f != "calibrationId"],
            "optional fields changed; refusing to overwrite concurrent contract work")
    new_function = contract[contract.index("pub(super) fn validate_canonical_search_result("):]
    new_function = new_function[:new_function.index("\n#[cfg(test)]")].strip()
    require(new_function.count(ID_CHECK) == 1, "calibration type check changed")
    new_function = new_function.replace(ID_CHECK, "", 1)
    body = 'let context = format!("canonical search result[{index}]");'
    require(normalized(old[old.index(body):]) == normalized(new_function[new_function.index(body):]),
            "validator logic changed; refusing to drop an existing validation check")
    anchor = "use cass_prefetch_worker::CassPrefetchWorker;\n"
    return replace_once(text[:start] + text[end:], anchor, anchor + LINK, "server module")


def updated_core(text: str) -> str:
    declaration = "pub(crate) mod search_result_document;\n"
    if declaration in text:
        require(text.count(declaration) == 1, "duplicate shared module")
        return text
    return replace_once(text, "pub mod search;\n", "pub mod search;\n" + declaration, "core module")


def updated_search(text: str, shared: str) -> str:
    method = text.index("    fn data_json_with_advisory_session_inner(")
    start = text.index("        let results: Vec<serde_json::Value> = visible_results", method)
    end = text.index("        let consensus_conflicts =", start)
    block = text[start:end]
    if "SearchResultDocument::from([" in block:
        require("obj.as_object_mut()" not in block and "obj_map.into_json()" in block,
                "partial typed producer integration")
        require(not re.search(r'obj_map\s*\.\s*insert\(\s*"', block),
                "raw string key bypasses the canonical field vocabulary")
        return text
    required = shared_fields(shared, "REQUIRED")
    optional = shared_fields(shared, "OPTIONAL")
    vocabulary = required | optional
    initial = re.search(r'let mut obj = serde_json::json!\(\{(.*?)\n\s*\}\);', block, re.S)
    require(initial is not None, "canonical document initializer changed")
    entries = re.findall(r'^\s*"(\w+)": (.*),$', initial.group(1), re.M)
    require(len(entries) == len(required) + 1 and
            {field for field, _ in entries} == set(required) | {"calibrationId"},
            "canonical required emission changed")
    typed = "let mut obj_map = SearchResultDocument::from([\n" + "".join(
        f"                    (Field::{vocabulary[field]}, serde_json::json!({value})),\n"
        for field, value in entries) + "                ]);"
    block = block[:initial.start()] + typed + block[initial.end():]
    seen: list[str] = []

    def typed_insert(match: re.Match[str]) -> str:
        field = match.group(1)
        require(field in optional, f"unreviewed optional producer field: {field}")
        seen.append(field)
        return f"obj_map.insert(Field::{vocabulary[field]},"

    block = re.sub(r'obj_map\s*\.\s*insert\(\s*"([^"\n]+)"\.to_string\(\),', typed_insert, block)
    require(set(seen) == set(optional) - {"calibrationId"} and len(seen) == len(set(seen)),
            "optional emission changed; inspect before converting")
    block = replace_once(block, "if let Some(obj_map) = obj.as_object_mut() {", "{", "document scope")
    block = replace_once(block, "                obj\n", "                obj_map.into_json()\n", "document output")
    block = replace_once(block, "            .map(|hit| {\n", "            .map(|hit| {\n"
                         "                use crate::core::search_result_document::{\n"
                         "                    SearchResultDocument, SearchResultField as Field,\n"
                         "                };\n", "document imports")
    return text[:start] + block + text[end:]


def updated_cli(text: str) -> str:
    link = "mod daemon_search_diagnostics;\n"
    lossy = r'\.map_err\(\|_\|\s*DaemonSearchFallbackReason::SearchResponseDrift\s*\)'
    if link in text:
        require("SearchResponseDrift(String)" in text and
                "Self::SearchResponseDrift(_)" in text and not re.search(lossy, text),
                "partial fallback diagnostic integration")
        return text
    text = replace_once(text,
                        "#[derive(Clone, Copy, Debug, Eq, PartialEq)]\nenum DaemonSearchFallbackReason {",
                        "#[derive(Clone, Debug, Eq, PartialEq)]\nenum DaemonSearchFallbackReason {",
                        "owned fallback error")
    start = text.index("enum DaemonSearchFallbackReason {")
    end = text.index("\n}\n", start)
    enum = replace_once(text[start:end], "    SearchResponseDrift,", "    SearchResponseDrift(String),",
                        "fallback error payload")
    text = text[:start] + enum + text[end:]
    text = replace_once(text,
                        "impl DaemonSearchFallbackReason {\n    const fn as_str(self) -> &'static str {",
                        "impl DaemonSearchFallbackReason {\n    const fn as_str(&self) -> &'static str {",
                        "borrow stable fallback category")
    text = replace_once(text, "Self::SearchResponseDrift =>", "Self::SearchResponseDrift(_) =>",
                        "stable fallback category")
    text, count = re.subn(lossy, ".map_err(DaemonSearchFallbackReason::SearchResponseDrift)", text)
    require(count > 0, "missing reviewed lossy validator mapping")
    start = text.index("fn daemon_search_fallback_degradation(")
    end = text.index("\nfn ", start + 1)
    degradation = replace_once(text[start:end], "reason.as_str()", "reason", "diagnostic message")
    text = text[:start] + degradation + text[end:]
    anchor = "const DAEMON_SEARCH_FALLBACK_CODE: &str = \"daemon_search_fallback\";"
    return replace_once(text, anchor, link + "\n" + anchor, "diagnostic module")


def updated_schema(text: str) -> str:
    schema = json.loads(text)
    document = schema["$defs"]["searchDocument"]
    desired = {"type": ["string", "null"],
               "description": "Optional for older v3 daemons; null means calibration metadata is unavailable."}
    if document["properties"].get("calibrationId", {}).get("type") == ["string", "null"] and "calibrationId" not in document["required"]:
        return text
    require("calibrationId" in document["required"] and "calibrationId" not in document["properties"],
            "daemon calibration schema changed; inspect before editing")
    start = text.index('    "searchDocument": {')
    prefix, tail = text[:start], text[start:]
    tail = replace_once(tail, '"scoreKind", "calibrationId", "scoreInterval"',
                        '"scoreKind", "scoreInterval"', "schema required fields")
    anchor = '        "scoreInterval": '
    tail = replace_once(tail, anchor, '        "calibrationId": ' + json.dumps(desired) + ',\n' + anchor,
                        "schema calibration property")
    result = prefix + tail
    expected = json.loads(text)
    expected["$defs"]["searchDocument"]["required"].remove("calibrationId")
    expected["$defs"]["searchDocument"]["properties"]["calibrationId"] = desired
    require(json.loads(result) == expected, "unexpected JSON schema mutation")
    return result


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="require every production path to be integrated")
    args = parser.parse_args()
    validate_delivery_environment(os.environ)
    shared = SHARED.read_text(encoding="utf-8")
    transforms = [(SERVER, lambda text: updated_server(text, CONTRACT.read_text(encoding="utf-8"), shared)),
                  (CORE, updated_core), (SEARCH, lambda text: updated_search(text, shared)),
                  (CLI, updated_cli), (SCHEMA, updated_schema)]
    # Compute all edits first. An unrecognized CLI/producer leaves every source
    # file untouched rather than publishing only the validator half of the fix.
    updates = []
    for path, transform in transforms:
        old = path.read_text(encoding="utf-8")
        updates.append((path, old, transform(old)))
    if args.check:
        for path, old, new in updates:
            require(old == new, f"repair not integrated: {path.relative_to(ROOT)}")
    else:
        for path, old, new in updates:
            if old != new:
                path.write_text(new, encoding="utf-8")
                print(f"Updated {path.relative_to(ROOT)}")
    print("GH #56 production integration verified" if args.check else "GH #56 production integration ready")


if __name__ == "__main__":
    main()
