#!/usr/bin/env python3
"""Apply GH #56's bounded contract extraction; refuse concurrent logic changes.

This follows the repository's delivery-script convention for source files too
large for the GitHub contents editor. Idempotent after integration. No downloads,
file deletions, branch changes, dependency changes, or force pushes.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[3]
SERVER = ROOT / "src/daemon/server.rs"
CONTRACT = ROOT / "src/daemon/search_result_contract.rs"
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


def without_comments(text: str) -> str:
    # Only line comments in this known validator; never transform live source
    # through this helper. Used solely to compare old and extracted logic.
    return re.sub(r"(?m)^\s*//[^\n]*", "", text)


def normalized(text: str) -> str:
    return re.sub(r"\s+", "", without_comments(text))


def fields(text: str, name: str) -> list[str]:
    match = re.search(rf"const {name}: &\[&str\] = &\[(.*?)\];", text, re.S)
    require(match is not None, f"missing {name} field list")
    return re.findall(r'"([^"\n]+)"', without_comments(match.group(1)))


def updated_server(text: str, contract: str) -> str:
    start_marker = "fn validate_canonical_search_result("
    if LINK.strip() in text and start_marker not in text:
        return text
    require(LINK.strip() not in text, "partial module integration; inspect before retry")
    require(text.count(start_marker) == 1, "expected exactly one local result validator")
    start = text.index(start_marker)
    end = text.index("\nfn dispatch_pack_search(", start)
    old = text[start:end].strip()
    require(fields(old, "REQUIRED") == fields(contract, "REQUIRED"), "required fields changed")
    require(fields(old, "OPTIONAL") == [f for f in fields(contract, "OPTIONAL") if f != "calibrationId"],
            "optional fields changed; refusing to overwrite concurrent contract work")
    new_function = contract[contract.index("pub(super) fn validate_canonical_search_result("):]
    new_function = new_function[:new_function.index("\n#[cfg(test)]")].strip()
    require(new_function.count(ID_CHECK) == 1, "calibration type check changed")
    new_function = new_function.replace(ID_CHECK, "", 1)
    old_body = old[old.index('let context = format!("canonical search result[{index}]");'):]
    new_body = new_function[new_function.index('let context = format!("canonical search result[{index}]");'):]
    require(normalized(old_body) == normalized(new_body),
            "validator logic changed; refusing to drop an existing validation check")
    anchor = "use cass_prefetch_worker::CassPrefetchWorker;\n"
    require(text.count(anchor) == 1, "server module anchor changed")
    return (text[:start] + text[end:]).replace(anchor, anchor + LINK, 1)


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
    required_anchor = '"scoreKind", "calibrationId", "scoreInterval"'
    require(tail.count(required_anchor) == 1, "schema required-list anchor changed")
    tail = tail.replace(required_anchor, '"scoreKind", "scoreInterval"', 1)
    property_anchor = '        "scoreInterval": '
    require(tail.count(property_anchor) == 1, "schema property anchor changed")
    tail = tail.replace(property_anchor, '        "calibrationId": ' + json.dumps(desired) + ',\n' + property_anchor, 1)
    result = prefix + tail
    after = json.loads(result)["$defs"]["searchDocument"]
    require(after["properties"]["calibrationId"] == desired, "calibration definition not inserted")
    require(set(after["required"]) <= set(after["properties"]), "required-but-undeclared daemon property")
    return result


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="require the repair to be integrated")
    args = parser.parse_args()
    server = SERVER.read_text()
    schema = SCHEMA.read_text()
    updates = [(SERVER, server, updated_server(server, CONTRACT.read_text())),
               (SCHEMA, schema, updated_schema(schema))]
    for path, old, new in updates:
        if args.check:
            require(old == new, f"repair not integrated: {path.relative_to(ROOT)}")
        elif old != new:
            path.write_text(new)
            print(f"Updated {path.relative_to(ROOT)}")
    print("GH #56 contract integration verified" if args.check else "GH #56 contract integration ready")


if __name__ == "__main__":
    main()
