#!/usr/bin/env python3
"""Emit an ee.release_candidate_proof.v1 SKELETON from a real graded run.

bd-reality-core-convergence-1azkt.5. The bead asks for a capsule that binds, in
one tamper-detecting record, the identities a release decision depends on.
Nothing in this repo emitted one. This does, from facts a run actually produced.

IT NEVER SETS completeness.populated = true. That belongs to .19, which emits a
complete capsule for an immutable clean-main candidate. Everything this cannot
establish is NAMED in completeness.unestablished rather than defaulted, guessed,
or left out -- because a capsule whose empty fields are indistinguishable from
its verified ones is the empty-digest defect with more fields.

THE IDENTITY / COMPLETENESS SEPARATION IS ENFORCED HERE, not just documented.
capsuleHash is computed over SOURCE + MANIFEST ONLY. Run completeness, host,
timing and results are deliberately EXCLUDED from it. If degradation entered the
hash, every degraded run would mint a different identity and no two runs would be
comparable -- and ee already learned this in the other direction, where pack's
degraded[] is hash-bearing precisely because it is kept free of load-dependent
entries. Here the run facts ARE load-dependent, so they sit outside identity.

INVOCATION, stated honestly: this is called by scripts/rch_run.sh, which agents
may choose to use. It therefore inherits that wrapper's open adoption question
(bd-pnj3s). It is not yet on a path every verdict takes, and this docstring is
not going to pretend otherwise.

USAGE
  emit_proof_capsule.py --log L --base SHA --dirty N --host H --run-exit E
                        --command C --out PATH
  emit_proof_capsule.py --self-test
"""

from __future__ import annotations

import argparse
import datetime
import hashlib
import json
import pathlib
import platform
import subprocess
import sys

HERE = pathlib.Path(__file__).resolve().parent
REPO = HERE.parents[1]
SCHEMA = REPO / "docs" / "schemas" / "ee.release_candidate_proof.v1.json"

sys.path.insert(0, str(HERE))


def _tool_version(binary: str) -> str | None:
    try:
        out = subprocess.run(
            [binary, "--version"], capture_output=True, text=True, timeout=30
        )
        return out.stdout.strip() or None
    except (FileNotFoundError, subprocess.TimeoutExpired):
        return None


def host_cfg_label() -> str:
    system = platform.system().lower()
    family = "unix" if system in {"darwin", "linux", "freebsd"} else system
    return f"target_family={family}, target_os={system}"


def build_capsule(
    log: pathlib.Path | None,
    base: str,
    dirty: int,
    host: str | None,
    run_exit: int,
    command: str,
) -> dict:
    from grade_test_log import parse, strip_ansi  # noqa: F401  (same directory)

    unestablished: list[str] = []
    results: list[dict] = []

    pairs: list[dict] = []
    problems: list[str] = []
    if log is not None and log.is_file():
        pairs, problems = parse(log.read_text(errors="replace").splitlines())
    else:
        unestablished.append("results: no readable log was supplied")

    for pair in pairs:
        reconciles = pair["announced"] == pair["counted"]
        if pair["announced"] == 0:
            status, cause = "INFRA_ERROR", "NOT_RUN"
        elif not reconciles:
            status, cause = "INFRA_ERROR", "NOT_RUN"
        elif pair["verdict"] == "ok" and pair["failed"] == 0:
            status, cause = "PASS", None
        else:
            status, cause = "FAIL", None
        results.append(
            {
                "stage": pair["target"],
                "status": status,
                "exitCode": run_exit,
                "target": pair["target"],
                "announced": pair["announced"],
                "executed": pair["counted"],
                "workerHost": host,
                "noVerdictCause": cause,
            }
        )
    for problem in problems:
        results.append(
            {
                "stage": "unattributed",
                "status": "INFRA_ERROR",
                "noVerdictCause": "NOT_RUN",
                "reason": problem,
                "workerHost": host,
            }
        )

    if host is None:
        unestablished.append(
            "results[].workerHost: the run log did not name a worker; a verdict "
            "without a host is reported, not reproducible"
        )

    # The cfg set belongs to the machine that RAN the tests, not to the machine
    # writing the capsule. Emitting the local cfg for a remote run is the exact
    # defect this field was added to prevent, one level up: it would have said
    # `target_os=darwin` for a verdict produced on a Linux worker, and it would
    # have looked right. If the run was remote, this host cannot know it.
    local_host = platform.node().split(".")[0]
    run_was_remote = host is not None and host != local_host
    if run_was_remote:
        evaluated_cfg = None
        unestablished.append(
            f"toolchain.evaluatedUnderCfg: the tests ran on {host}, not on this "
            f"machine ({local_host}), so the cfg set they were evaluated under is "
            "not knowable here. Reporting the emitter's own cfg would be a "
            "plausible-looking falsehood."
        )
    else:
        evaluated_cfg = host_cfg_label()
    if dirty:
        unestablished.append(
            f"source.treeish: the tree carried {dirty} uncommitted file(s), so this "
            "verdict is bound to a working state rather than to a commit"
        )

    # These are genuinely not knowable from a single graded run. Named, never
    # defaulted: an absent field and a verified-empty field must not look alike.
    for missing in (
        "effectiveDependencies.siblingPins: not resolved by this emitter",
        "binaries: no candidate binary was hashed by this run",
        "testInventory: no cross-shard logical-test inventory was computed",
        "performance.samples: this emitter records no timing samples",
        "evidenceHashes: no evidence artifacts were hashed",
        "runIdentifiers.hostedCi: hosted workflows are disabled (bd-hvlm2), so "
        "there is no hosted run id to bind -- recorded as null, never invented",
    ):
        unestablished.append(missing)

    # manifestHash is NULL ON PURPOSE for a run that did not execute through
    # scripts/verify.sh, and that is not a gap this emitter should paper over.
    #
    # scripts/verify-budget.toml IS the manifest -- it declares stage names,
    # requirement policy ("advisory" / "tracked_red" + a mandatory owning bead)
    # and per-stage budgets, verify.sh reads it at :141 and resolves policy at
    # :662-671, and tests/verification_drift_guard.rs:530 cross-checks it against
    # verify.sh's own run_stage labels under a local variable literally named
    # `manifest`.
    #
    # But it governs VERIFY.SH RUNS. A `cargo test` dispatched straight at a
    # worker never consults it, so binding its hash into such a capsule would
    # assert a relationship that did not exist -- inventing provenance to fill a
    # required field, which is the defect this emitter was written to refuse.
    # So the capsule says WHY it is null instead, on every emission, and in doing
    # so records bd-pnj3s's gap as data rather than as a bead somebody has to
    # remember.
    unestablished.append(
        "manifest.manifestHash: this run did not execute through scripts/verify.sh, "
        "so no manifest governed it. verify-budget.toml is the manifest for "
        "verify.sh runs; a directly dispatched remote command is outside it "
        "(bd-pnj3s)."
    )
    identity = {
        "source": {"treeish": base, "commit": base, "dirty": bool(dirty)},
        "manifest": {"stageCount": len(results), "manifestHash": None},
    }
    # IDENTITY ONLY. Run facts are excluded on purpose; see the module docstring.
    capsule_hash = hashlib.blake2b(
        json.dumps(identity, sort_keys=True).encode(), digest_size=32
    ).hexdigest()

    lock = REPO / "franken-stack.lock"
    lock_hash = (
        hashlib.blake2b(lock.read_bytes(), digest_size=32).hexdigest()
        if lock.is_file()
        else None
    )

    return {
        "schema": "ee.release_candidate_proof.v1",
        "capsuleHash": f"blake2b:{capsule_hash}",
        "generatedAt": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "completeness": {"populated": False, "unestablished": unestablished},
        "source": identity["source"],
        "effectiveDependencies": {"lockfileHash": lock_hash, "siblingPins": []},
        "toolchain": {
            "rustc": _tool_version("rustc") or "unknown",
            "cargo": _tool_version("cargo") or "unknown",
            "channel": "unknown",
            "target": platform.machine(),
            "evaluatedUnderCfg": evaluated_cfg,
        },
        "manifest": identity["manifest"],
        "binaries": [],
        "testInventory": {"logicalTests": None, "shards": []},
        "results": results,
        "performance": {"samples": []},
        "runIdentifiers": {"rch": command, "hostedCi": None},
        "evidenceHashes": [],
    }


def validate(capsule: dict) -> list[str]:
    """Structural check against the committed schema, without a jsonschema dep."""
    problems: list[str] = []
    schema = json.loads(SCHEMA.read_text())
    for field in schema["required"]:
        if field not in capsule:
            problems.append(f"missing required top-level field: {field}")
    for key in capsule:
        if key not in schema["properties"]:
            problems.append(f"field not permitted by additionalProperties:false: {key}")
    for obj_name, spec in schema["properties"].items():
        if spec.get("type") != "object" or obj_name not in capsule:
            continue
        for field in spec.get("required", []):
            if field not in capsule[obj_name]:
                problems.append(f"missing required {obj_name}.{field}")
        if spec.get("additionalProperties") is False:
            for key in capsule[obj_name]:
                if key not in (spec.get("properties") or {}):
                    problems.append(f"{obj_name}.{key} not permitted by the schema")
    allowed = set(
        (schema["properties"]["results"]["items"].get("properties") or {}).keys()
    )
    for row in capsule.get("results", []):
        for key in row:
            if key not in allowed:
                problems.append(f"results[].{key} not permitted by the schema")
    return problems


GREEN_LOG = """     Running unittests src/lib.rs (target/debug/deps/ee-aaa)
running 4 tests
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 10 filtered out
"""
ZERO_LOG = """     Running unittests src/lib.rs (target/debug/deps/ee-aaa)
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 99 filtered out
"""


def self_test() -> int:
    import tempfile

    failures = 0

    def check(name: str, condition: bool) -> None:
        nonlocal failures
        if condition:
            print(f"  [self-test] OK   {name}")
        else:
            print(f"  [self-test] FAIL {name}")
            failures += 1

    with tempfile.TemporaryDirectory() as directory:
        green = pathlib.Path(directory) / "green.log"
        green.write_text(GREEN_LOG)
        zero = pathlib.Path(directory) / "zero.log"
        zero.write_text(ZERO_LOG)

        capsule = build_capsule(green, "abc123", 0, "hz4", 0, "rch exec ...")
        check("a green run validates against the committed schema", not validate(capsule))
        check("populated is never true", capsule["completeness"]["populated"] is False)
        check("unestablished is non-empty", bool(capsule["completeness"]["unestablished"]))
        check("host is recorded", capsule["results"][0]["workerHost"] == "hz4")
        check("announced is recorded", capsule["results"][0]["announced"] == 4)
        # capsule above was built with host="hz4", i.e. a REMOTE run.
        check(
            "a REMOTE run does NOT claim this machine's cfg",
            capsule["toolchain"]["evaluatedUnderCfg"] is None,
        )
        check(
            "and says why the cfg is unknown",
            any("evaluatedUnderCfg" in u for u in capsule["completeness"]["unestablished"]),
        )
        local = build_capsule(green, "abc123", 0, platform.node().split(".")[0], 0, "x")
        check(
            "a LOCAL run DOES record the cfg",
            bool(local["toolchain"]["evaluatedUnderCfg"]),
        )
        check("hostedCi is null, not invented", capsule["runIdentifiers"]["hostedCi"] is None)
        check("a green run reports PASS", capsule["results"][0]["status"] == "PASS")

        vacuous = build_capsule(zero, "abc123", 0, "hz4", 0, "rch exec ...")
        check(
            "a ZERO-test run is NOT recorded as PASS",
            vacuous["results"][0]["status"] != "PASS",
        )
        check(
            "a ZERO-test run carries a no-verdict cause",
            vacuous["results"][0]["noVerdictCause"] == "NOT_RUN",
        )

        # Identity must not move when only run facts move. This is the design
        # constraint the whole capsule rests on, so it is asserted, not assumed.
        other_host = build_capsule(green, "abc123", 0, "hz9", 0, "rch exec ...")
        check(
            "capsuleHash is identity-only: a different host does not change it",
            capsule["capsuleHash"] == other_host["capsuleHash"],
        )
        moved = build_capsule(green, "def456", 0, "hz4", 0, "rch exec ...")
        check(
            "capsuleHash DOES change when the source changes",
            capsule["capsuleHash"] != moved["capsuleHash"],
        )
        dirty = build_capsule(green, "abc123", 3, "hz4", 0, "rch exec ...")
        check(
            "a dirty tree is named in unestablished",
            any("uncommitted" in u for u in dirty["completeness"]["unestablished"]),
        )
        check(
            "a dirty tree changes identity (it is a different source)",
            capsule["capsuleHash"] != dirty["capsuleHash"],
        )

    if failures:
        print(f"[emit-proof-capsule] SELF-TEST FAILED: {failures} arm(s)")
        return 1
    print("[emit-proof-capsule] self-test: all arms passed")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(add_help=True)
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--log")
    parser.add_argument("--base", default="unknown")
    parser.add_argument("--dirty", type=int, default=0)
    parser.add_argument("--host")
    parser.add_argument("--run-exit", type=int, default=0)
    parser.add_argument("--command", default="")
    parser.add_argument("--out")
    args = parser.parse_args()

    if args.self_test:
        return self_test()

    capsule = build_capsule(
        pathlib.Path(args.log) if args.log else None,
        args.base,
        args.dirty,
        args.host,
        args.run_exit,
        args.command,
    )
    problems = validate(capsule)
    if problems:
        for problem in problems:
            print(f"[emit-proof-capsule] INVALID: {problem}", file=sys.stderr)
        return 1
    text = json.dumps(capsule, indent=2) + "\n"
    if args.out:
        pathlib.Path(args.out).write_text(text)
        print(f"[emit-proof-capsule] wrote {args.out}")
    else:
        print(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
