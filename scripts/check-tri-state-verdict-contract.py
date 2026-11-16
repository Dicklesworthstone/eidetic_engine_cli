#!/usr/bin/env python3
"""Enforce the tri-state verdict contract across health, gate, and metric surfaces
(bd-reality-core-convergence-1azkt.70).

Every verdict surface that depends on runtime state, external tools, a built
binary, a populated corpus, or a dispatched subprocess must distinguish three
outcomes, never two:

  1. MEASURED_HEALTHY   -- the check ran against a valid target and passed
  2. MEASURED_UNHEALTHY -- the check ran against a valid target and found a defect
  3. COULD_NOT_MEASURE  -- the target, tool, binary, corpus, or lane could not be
                           evaluated (never collapsed into healthy or unhealthy)

Pure boolean static properties over local repository text that are always
evaluable when the script runs (for example, "this file parses" or "workflow
action refs are 40-hex SHAs") are classified explicitly as
`two_state_total_boolean` so the gate never cries wolf on total predicates.

EXIT CODES
  0  every enumerated verdict surface honours the tri-state contract
  1  RED -- a surface collapses COULD_NOT_MEASURE, is unregistered, or a
     total-boolean exemption is stale / unearned
  2  usage error
  3  CANNOT ANSWER -- empty-world guard or broken scope assumption (never 0)
"""

from __future__ import annotations

import argparse
import ast
import json
import os
import pathlib
import re
import stat
import sys
import tempfile

EXIT_OK = 0
EXIT_RED = 1
EXIT_USAGE = 2
EXIT_CANNOT = 3

MIN_TOTAL_SURFACES = 30
REQUIRED_KNOWN_SURFACES = (
    "cli:SubsystemPostureStatus",
    "cli:SemanticReadiness",
    "cli:status_semantic_readiness",
    "cli:doctor_semantic_readiness",
    "probe:scripts/real_shape_corpus/probe.py",
    "dispatch:scripts/verify.sh",
    "dispatch:scripts/lib/emit_proof_capsule.py",
    "schema:docs/schemas/ee.release_candidate_proof.v1.json",
)

TERMINATOR_CLASSES = (
    "product_verdict",
    "infra_refusal",
    "timeout",
    "signal",
)

# Pure boolean static checks over committed repository files that do not spawn
# external runtime probes and are always evaluable when the repository tree is
# present. Each entry is re-earned on every run:
#   1. The file must exist under scripts/check-*.py.
#   2. If the script upgrades to a native tri-state gate (declaring
#      EXIT_CANNOT = 3 / CannotAnswer), its row here becomes stale and fails
#      the gate until removed.
#   3. If the script invokes subprocesses to measure a live binary or remote
#      worker without declaring EXIT_CANNOT = 3, it is rejected as a non-total
#      probe masquerading as a static boolean check.
TOTAL_BOOLEAN_GATES: dict[str, str] = {
    "scripts/check-action-pins.py": (
        "Pure static YAML pin syntax check over .github/workflows/*.yml; "
        "always evaluable from committed workflow files."
    ),
    "scripts/check-adr-evidence-citations.py": (
        "Pure static Markdown/path citation check over docs/adr/*.md; "
        "always evaluable from committed files."
    ),
    "scripts/check-boundary-inventory-coverage.py": (
        "Pure static Rust/fixture boundary inventory check; "
        "always evaluable from committed source files."
    ),
    "scripts/check-dead-exit-code-handlers.py": (
        "Pure static shell/Rust exit-code handler correspondence check; "
        "always evaluable from committed files."
    ),
    "scripts/check-degraded-code-classified.py": (
        "Pure static taxonomy vs Rust degraded-code constant check with "
        "internal empty-world guards; always evaluable from committed files."
    ),
    "scripts/check-deleted-symbol-orphans.py": (
        "Pure static symbol reference scan across committed Rust and shell files."
    ),
    "scripts/check-ee-spawn-isolation.py": (
        "Pure static AST/lexer scan of tests/*.rs for unisolated Command::new "
        "spawns against a shrink-only baseline."
    ),
    "scripts/check-exit-code-docs.py": (
        "Pure static correspondence check between ProcessExitCode and prose "
        "documentation tables."
    ),
    "scripts/check-fleet-identifier-census.py": (
        "Static tracked-file disclosure scan extracting rules from "
        "tests/fleet_identifier_guard.rs with non-empty token guards."
    ),
    "scripts/check-golden-consumption.py": (
        "Pure static include_str!/read floor check over tests/golden/."
    ),
    "scripts/check-graph-metric-fnx-routing.py": (
        "Pure static Rust lexer scan proving graph metric modules call fnx "
        "in code, not only in comments."
    ),
    "scripts/check-jq-vacuity.py": (
        "Pure static shell jq expression scan against a judged baseline."
    ),
    "scripts/check-lexical-analyzer-revision.py": (
        "Pure static hash-input and analyzer-token correspondence check over "
        "committed Rust sources."
    ),
    "scripts/check-open-coded-success-baseline.py": (
        "Pure static test-source scan for open-coded status.success() calls "
        "against a shrink-only baseline."
    ),
    "scripts/check-production-reachability.py": (
        "Pure static call-graph reachability scan over src/**/*.rs with "
        "shrink-only baseline."
    ),
    "scripts/check-rch-doc-examples.py": (
        "Pure static Markdown code-fence command classifier over docs and runbooks."
    ),
    "scripts/check-rch-remediation-guidance.py": (
        "Pure static error-remediation guidance check over RCH scripts and docs."
    ),
    "scripts/check-shard-coverage.py": (
        "Pure static test-shard inclusion bijectivity check over tests/ and Cargo.toml."
    ),
    "scripts/check-shell-ee-spawn-isolation.py": (
        "Pure static shell script scan for unisolated ee invocations."
    ),
    "scripts/check-shell-schema-ownership.py": (
        "Pure static shell-emitted schema ownership check against docs/schemas/."
    ),
    "scripts/check-step-assertion-isolation.py": (
        "Pure static shell e2e step-assertion isolation check."
    ),
}

FORBIDDEN_LIVE_PROBE_RE = re.compile(
    r"\b(?:rch\s+exec|ssh\s+|curl\s+|cargo\s+(?:test|build|check|clippy|run))\b"
)


class CannotAnswer(Exception):
    """Raised when the census cannot enumerate its world."""


def _spawns_live_probe_ast(src: str) -> bool:
    """Return True if `src` has an AST Call to `subprocess.*` whose literal
    command arguments invoke a live probe (`rch exec`, `ssh`, `curl`, or
    `cargo test/build/check/clippy/run`). Docstrings and comments are ignored."""
    try:
        tree = ast.parse(src)
    except SyntaxError:
        return True
    for node in ast.walk(tree):
        if not isinstance(node, ast.Call):
            continue
        func = node.func
        is_subproc = (
            isinstance(func, ast.Attribute)
            and isinstance(func.value, ast.Name)
            and func.value.id == "subprocess"
        )
        if not is_subproc:
            continue
        literals: list[str] = []
        for arg in node.args:
            for sub in ast.walk(arg):
                if isinstance(sub, ast.Constant) and isinstance(sub.value, str):
                    literals.append(sub.value)
        joined = " ".join(literals)
        if FORBIDDEN_LIVE_PROBE_RE.search(joined):
            return True
    return False


def repo_root() -> pathlib.Path:
    return pathlib.Path(__file__).resolve().parents[1]


def _read_text(root: pathlib.Path, rel: str) -> str | None:
    path = root / rel
    if not path.is_file():
        return None
    return path.read_text(encoding="utf-8", errors="replace")


def collect_census(root: pathlib.Path) -> tuple[list[dict], list[str]]:
    """Enumerate every verdict surface in `root` and verify its tri-state contract."""
    surfaces: list[dict] = []
    problems: list[str] = []

    # 1. CLI / Posture surfaces (.66 family)
    posture_src = _read_text(root, "src/models/posture.rs")
    if posture_src is not None:
        has_ok = "Ok," in posture_src and '"ok"' in posture_src
        has_unhealthy = (
            "DegradedRecoverable" in posture_src
            and "DegradedRequired" in posture_src
            and "Blocked" in posture_src
        )
        has_unmeasured = (
            "Initializing" in posture_src
            and '"initializing"' in posture_src
            and "Unimplemented" in posture_src
            and '"unimplemented"' in posture_src
        )
        ok = has_ok and has_unhealthy and has_unmeasured
        surfaces.append(
            {
                "id": "cli:SubsystemPostureStatus",
                "category": "cli_posture_surface",
                "path": "src/models/posture.rs",
                "classification": "tri_state",
                "measured_healthy": "Ok (\"ok\")",
                "measured_unhealthy": "DegradedRecoverable | DegradedRequired | Blocked",
                "could_not_measure": "Initializing (\"initializing\") | Unimplemented (\"unimplemented\")",
                "valid": ok,
            }
        )
        if not ok:
            problems.append(
                "cli:SubsystemPostureStatus (src/models/posture.rs): missing distinct "
                "MEASURED_HEALTHY, MEASURED_UNHEALTHY, or COULD_NOT_MEASURE variants"
            )

    index_src = _read_text(root, "src/core/index.rs")
    if index_src is not None:
        has_enum = "pub enum SemanticReadiness" in index_src
        has_healthy = "Available," in index_src
        has_unhealthy = "LexicalOnly," in index_src and "ModelUnavailable," in index_src
        has_unmeasured = "Pending," in index_src and "Unconfirmed," in index_src
        ok = has_enum and has_healthy and has_unhealthy and has_unmeasured
        surfaces.append(
            {
                "id": "cli:SemanticReadiness",
                "category": "cli_posture_surface",
                "path": "src/core/index.rs",
                "classification": "tri_state",
                "measured_healthy": "SemanticReadiness::Available",
                "measured_unhealthy": "SemanticReadiness::LexicalOnly | ModelUnavailable",
                "could_not_measure": "SemanticReadiness::Pending | Unconfirmed",
                "valid": ok,
            }
        )
        if not ok:
            problems.append(
                "cli:SemanticReadiness (src/core/index.rs): missing tri-state variants "
                "(Available, LexicalOnly/ModelUnavailable, Pending/Unconfirmed) (.66 regression)"
            )

    status_src = _read_text(root, "src/core/status.rs")
    if status_src is not None:
        # .66 positive control: status must consume shared SemanticReadiness and
        # SubsystemPostureStatus::Initializing rather than collapsing unconfirmed
        # or lexical-only stores into Ready.
        has_semantic_readiness = "SemanticReadiness" in status_src
        has_initializing = "SubsystemPostureStatus::Initializing" in status_src
        has_degraded = "SubsystemPostureStatus::DegradedRecoverable" in status_src
        ok = has_semantic_readiness and has_initializing and has_degraded
        surfaces.append(
            {
                "id": "cli:status_semantic_readiness",
                "category": "cli_posture_surface",
                "path": "src/core/status.rs",
                "classification": "tri_state",
                "measured_healthy": "SubsystemPostureStatus::Ok with SemanticReadiness::Available",
                "measured_unhealthy": "SubsystemPostureStatus::DegradedRecoverable | DegradedRequired | Blocked",
                "could_not_measure": "SubsystemPostureStatus::Initializing + SemanticReadiness::Pending | Unconfirmed",
                "valid": ok,
            }
        )
        if not ok:
            problems.append(
                "cli:status_semantic_readiness (src/core/status.rs): collapses unmeasured/unconfirmed "
                "semantic readiness into healthy posture (.66 regression)"
            )

    doctor_src = _read_text(root, "src/core/doctor.rs")
    if doctor_src is not None:
        has_semantic_readiness = (
            "pub semantic_readiness: Option<SemanticReadiness>" in doctor_src
            and "readiness: SemanticReadiness" in doctor_src
        )
        has_retrieval_degradations = "pub fn retrieval_degradations" in doctor_src
        ok = has_semantic_readiness and has_retrieval_degradations
        surfaces.append(
            {
                "id": "cli:doctor_semantic_readiness",
                "category": "cli_posture_surface",
                "path": "src/core/doctor.rs",
                "classification": "tri_state",
                "measured_healthy": "CheckSeverity::Ok when SemanticReadiness::Available",
                "measured_unhealthy": "CheckSeverity::Warning with SemanticReadiness::degradation()",
                "could_not_measure": "semantic_readiness: None | SemanticReadiness::Pending | Unconfirmed (not boolean semantic: bool)",
                "valid": ok,
            }
        )
        if not ok:
            problems.append(
                "cli:doctor_semantic_readiness (src/core/doctor.rs): collapses unevaluated "
                "semantic retrieval readiness into boolean semantic: bool / posture: ok (.66 regression)"
            )

    # 2. Real-shape corpus probe metric surfaces (.67 family)
    probe_src = _read_text(root, "scripts/real_shape_corpus/probe.py")
    if probe_src is not None:
        has_unavailable = '"status": "unavailable"' in probe_src or "'status': 'unavailable'" in probe_src
        has_zero_guard = "zero_guard" in probe_src
        has_physical_root = "allocate_physical_root" in probe_src
        has_describe_envelope = "describe_envelope" in probe_src
        has_harness_code = "harness_error_code" in probe_src
        ok = (
            has_unavailable
            and has_zero_guard
            and has_physical_root
            and has_describe_envelope
            and has_harness_code
        )
        surfaces.append(
            {
                "id": "probe:scripts/real_shape_corpus/probe.py",
                "category": "probe_metric_surface",
                "path": "scripts/real_shape_corpus/probe.py",
                "classification": "tri_state",
                "measured_healthy": "numeric metrics with positive zero_guard on populated corpus",
                "measured_unhealthy": "numeric metrics below threshold with positive zero_guard",
                "could_not_measure": "status=unavailable / harness_error_code / zero_guard refusal on unmeasured corpus",
                "valid": ok,
            }
        )
        if not ok:
            problems.append(
                "probe:scripts/real_shape_corpus/probe.py: missing fail-closed COULD_NOT_MEASURE "
                "guards (status=unavailable, zero_guard, allocate_physical_root, describe_envelope, "
                "or harness_error_code) (.67 regression)"
            )

    # 3. Dispatched verdict surfaces (verify.sh, emit_proof_capsule.py, proof schema)
    verify_src = _read_text(root, "scripts/verify.sh")
    if verify_src is not None:
        has_vocab = "STAGE_STATUS_VOCABULARY=" in verify_src
        has_status_fn = "stage_status_for_exit_code()" in verify_src
        has_terminator_fn = "stage_terminator_class_for_exit_code()" in verify_src
        has_unrunnable = "ENV_UNRUNNABLE" in verify_src and "TIMEOUT" in verify_src and "INFRA_ERROR" in verify_src
        ok = has_vocab and has_status_fn and has_terminator_fn and has_unrunnable
        surfaces.append(
            {
                "id": "dispatch:scripts/verify.sh",
                "category": "dispatched_verdict_surface",
                "path": "scripts/verify.sh",
                "classification": "tri_state",
                "measured_healthy": "PASS (terminator_class=product_verdict)",
                "measured_unhealthy": "FAIL | TRACKED_RED (terminator_class=product_verdict)",
                "could_not_measure": (
                    "SKIP | ENV_UNRUNNABLE | NOT_APPLICABLE | TIMEOUT | INFRA_ERROR | CANCELLED "
                    "(terminator_class=infra_refusal|timeout|signal)"
                ),
                "valid": ok,
            }
        )
        if not ok:
            problems.append(
                "dispatch:scripts/verify.sh: must declare STAGE_STATUS_VOCABULARY, "
                "stage_status_for_exit_code(), and stage_terminator_class_for_exit_code() "
                "distinguishing product_verdict from infra_refusal/timeout/signal"
            )

    capsule_src = _read_text(root, "scripts/lib/emit_proof_capsule.py")
    if capsule_src is not None:
        has_classify = "def classify_terminator(" in capsule_src
        has_field = '"terminatorClass"' in capsule_src
        has_no_verdict = '"noVerdictCause"' in capsule_src
        has_all_classes = all(f'"{cls}"' in capsule_src for cls in TERMINATOR_CLASSES)
        ok = has_classify and has_field and has_no_verdict and has_all_classes
        surfaces.append(
            {
                "id": "dispatch:scripts/lib/emit_proof_capsule.py",
                "category": "dispatched_verdict_surface",
                "path": "scripts/lib/emit_proof_capsule.py",
                "classification": "tri_state",
                "measured_healthy": "status=PASS, noVerdictCause=null, terminatorClass=product_verdict",
                "measured_unhealthy": "status=FAIL, noVerdictCause=null, terminatorClass=product_verdict",
                "could_not_measure": (
                    "status=INFRA_ERROR|TIMEOUT|CANCELLED, noVerdictCause=NOT_RUN|TIMEOUT|CANCELLED|INFRA_ERROR, "
                    "terminatorClass=infra_refusal|timeout|signal"
                ),
                "valid": ok,
            }
        )
        if not ok:
            problems.append(
                "dispatch:scripts/lib/emit_proof_capsule.py: must emit terminatorClass "
                "and noVerdictCause across all four terminator classes "
                "(product_verdict, infra_refusal, timeout, signal)"
            )

    schema_src = _read_text(root, "docs/schemas/ee.release_candidate_proof.v1.json")
    if schema_src is not None:
        try:
            schema_json = json.loads(schema_src)
        except json.JSONDecodeError as exc:
            raise CannotAnswer(f"invalid JSON in ee.release_candidate_proof.v1.json: {exc}") from exc
        item_props = (
            schema_json.get("properties", {})
            .get("results", {})
            .get("items", {})
            .get("properties", {})
        )
        term_enum = (item_props.get("terminatorClass") or {}).get("enum") or []
        no_verdict_enum = (item_props.get("noVerdictCause") or {}).get("enum") or []
        ok = all(cls in term_enum for cls in TERMINATOR_CLASSES) and "NOT_RUN" in no_verdict_enum
        surfaces.append(
            {
                "id": "schema:docs/schemas/ee.release_candidate_proof.v1.json",
                "category": "dispatched_verdict_surface",
                "path": "docs/schemas/ee.release_candidate_proof.v1.json",
                "classification": "tri_state",
                "measured_healthy": "results[].status=PASS, terminatorClass=product_verdict",
                "measured_unhealthy": "results[].status=FAIL, terminatorClass=product_verdict",
                "could_not_measure": "results[].noVerdictCause + terminatorClass in {infra_refusal, timeout, signal}",
                "valid": ok,
            }
        )
        if not ok:
            problems.append(
                "schema:docs/schemas/ee.release_candidate_proof.v1.json: results.items.properties "
                "must declare terminatorClass enum {product_verdict, infra_refusal, timeout, signal} "
                "and noVerdictCause"
            )

    # 4. Enumerate every scripts/check-*.py static gate from disk
    scripts_dir = root / "scripts"
    discovered_py_gates: set[str] = set()
    if scripts_dir.is_dir():
        for path in sorted(scripts_dir.glob("check-*.py")):
            rel = f"scripts/{path.name}"
            if rel == "scripts/check-tri-state-verdict-contract.py":
                # Self is always tri-state (EXIT_OK=0, EXIT_RED=1, EXIT_CANNOT=3).
                pass
            discovered_py_gates.add(rel)
            src = path.read_text(encoding="utf-8", errors="replace")
            has_exit_cannot = (
                re.search(r"^\s*EXIT_CANNOT\s*=\s*3\b", src, re.MULTILINE) is not None
                and ("CannotAnswer" in src or "EXIT_CANNOT" in src)
            )
            in_boolean_registry = rel in TOTAL_BOOLEAN_GATES

            if has_exit_cannot and in_boolean_registry:
                surfaces.append(
                    {
                        "id": f"gate:{rel}",
                        "category": "static_gate_surface",
                        "path": rel,
                        "classification": "stale_boolean_exemption",
                        "valid": False,
                    }
                )
                problems.append(
                    f"gate:{rel}: declares EXIT_CANNOT = 3 (native tri-state) but is still "
                    "listed in TOTAL_BOOLEAN_GATES; remove the stale exemption row"
                )
            elif has_exit_cannot:
                surfaces.append(
                    {
                        "id": f"gate:{rel}",
                        "category": "static_gate_surface",
                        "path": rel,
                        "classification": "tri_state",
                        "measured_healthy": "EXIT_OK (0)",
                        "measured_unhealthy": "EXIT_RED (1)",
                        "could_not_measure": "EXIT_CANNOT (3) via CannotAnswer",
                        "valid": True,
                    }
                )
            elif in_boolean_registry:
                # Verify via AST that it does not shell out to live cargo/rch/ssh
                # probes without declaring a third state (EXIT_CANNOT = 3).
                runs_live_probe = _spawns_live_probe_ast(src)
                valid = not runs_live_probe
                surfaces.append(
                    {
                        "id": f"gate:{rel}",
                        "category": "static_gate_surface",
                        "path": rel,
                        "classification": "two_state_total_boolean",
                        "reason": TOTAL_BOOLEAN_GATES[rel],
                        "valid": valid,
                    }
                )
                if not valid:
                    problems.append(
                        f"gate:{rel}: registered as two_state_total_boolean but spawns a live "
                        "cargo/rch/ssh probe without EXIT_CANNOT = 3"
                    )
            else:
                surfaces.append(
                    {
                        "id": f"gate:{rel}",
                        "category": "static_gate_surface",
                        "path": rel,
                        "classification": "unclassified_two_state",
                        "valid": False,
                    }
                )
                problems.append(
                    f"gate:{rel}: unregistered two-state gate. Either add EXIT_CANNOT = 3 / "
                    "CannotAnswer for unmeasurable states or prove totality in TOTAL_BOOLEAN_GATES"
                )

    # Check for stale entries in TOTAL_BOOLEAN_GATES when scanning a full tree
    if len(discovered_py_gates) >= 15:
        for registered_rel in sorted(TOTAL_BOOLEAN_GATES):
            if registered_rel not in discovered_py_gates:
                problems.append(
                    f"TOTAL_BOOLEAN_GATES names `{registered_rel}`, which does not exist on disk"
                )

    # Empty-world guards: fail closed with CannotAnswer (exit 3) if the census
    # found too few surfaces or missed any required anchor surface.
    if len(surfaces) < MIN_TOTAL_SURFACES:
        raise CannotAnswer(
            f"census enumerated only {len(surfaces)} surface(s), below minimum "
            f"{MIN_TOTAL_SURFACES}; refusing to pass vacuously"
        )
    seen_ids = {s["id"] for s in surfaces}
    for required_id in REQUIRED_KNOWN_SURFACES:
        if required_id not in seen_ids:
            raise CannotAnswer(
                f"census missed required known surface `{required_id}`; refusing "
                "to pass on an incomplete enumeration"
            )

    return surfaces, problems


def _write_fixture_tree(base: pathlib.Path, overrides: dict[str, str | None]) -> pathlib.Path:
    """Create a synthetic repo fixture seeded from the real repo's verdict files,
    applying `overrides` (where value=None omits the file)."""
    real = repo_root()
    seed_files = [
        "src/models/posture.rs",
        "src/core/index.rs",
        "src/core/status.rs",
        "src/core/doctor.rs",
        "scripts/real_shape_corpus/probe.py",
        "scripts/verify.sh",
        "scripts/lib/emit_proof_capsule.py",
        "docs/schemas/ee.release_candidate_proof.v1.json",
    ]
    for path in sorted((real / "scripts").glob("check-*.py")):
        seed_files.append(f"scripts/{path.name}")

    for rel in seed_files:
        if rel in overrides and overrides[rel] is None:
            continue
        dst = base / rel
        dst.parent.mkdir(parents=True, exist_ok=True)
        if rel in overrides and overrides[rel] is not None:
            dst.write_text(overrides[rel] or "", encoding="utf-8")
        else:
            src_path = real / rel
            if src_path.is_file():
                dst.write_text(
                    src_path.read_text(encoding="utf-8", errors="replace"),
                    encoding="utf-8",
                )
    for rel, content in overrides.items():
        if content is not None and rel not in seed_files:
            dst = base / rel
            dst.parent.mkdir(parents=True, exist_ok=True)
            dst.write_text(content, encoding="utf-8")
    return base


def self_test() -> int:
    failures = 0
    arms = 0

    def check(label: str, cond: bool, detail: str = "") -> None:
        nonlocal failures, arms
        arms += 1
        if cond:
            print(f"  [self-test] OK   {label}")
        else:
            suffix = f" ({detail})" if detail else ""
            print(f"  [self-test] FAIL {label}{suffix}")
            failures += 1

    real = repo_root()

    # Arm 1: Live repository passes the census with >= MIN_TOTAL_SURFACES
    surfaces, problems = collect_census(real)
    check(
        "live repository satisfies the tri-state verdict contract",
        len(problems) == 0 and len(surfaces) >= MIN_TOTAL_SURFACES,
        "; ".join(problems),
    )

    # Arm 2: Positive control for .66 (status/doctor collapsing unconfirmed semantic tier into boolean semantic: bool)
    with tempfile.TemporaryDirectory() as tmp:
        status_text = (real / "src/core/status.rs").read_text(encoding="utf-8")
        doctor_text = (real / "src/core/doctor.rs").read_text(encoding="utf-8")
        broken_66 = _write_fixture_tree(
            pathlib.Path(tmp),
            {
                "src/core/status.rs": status_text.replace("SemanticReadiness", "RemovedReadiness"),
                "src/core/doctor.rs": doctor_text.replace("readiness: SemanticReadiness", "semantic: bool"),
            },
        )
        _, probs_66 = collect_census(broken_66)
        check(
            "positive control (.66): detects status and doctor collapsing unmeasured semantic readiness into boolean",
            any("cli:status_semantic_readiness" in p for p in probs_66)
            and any("cli:doctor_semantic_readiness" in p for p in probs_66),
            str(probs_66),
        )

    # Arm 3: Positive control for .67 (probe.py missing zero_guard / allocate_physical_root)
    with tempfile.TemporaryDirectory() as tmp:
        probe_text = (real / "scripts/real_shape_corpus/probe.py").read_text(encoding="utf-8")
        broken_67 = _write_fixture_tree(
            pathlib.Path(tmp),
            {
                "scripts/real_shape_corpus/probe.py": probe_text.replace("zero_guard", "no_guard"),
            },
        )
        _, probs_67 = collect_census(broken_67)
        check(
            "positive control (.67): detects probe.py collapsing unmeasured zero into a valid metric",
            any("probe:scripts/real_shape_corpus/probe.py" in p for p in probs_67),
            str(probs_67),
        )

    # Arm 4: Positive control for a third surface (unregistered two-state gate)
    with tempfile.TemporaryDirectory() as tmp:
        third_surface = _write_fixture_tree(
            pathlib.Path(tmp),
            {
                "scripts/check-unregistered-two-state.py": (
                    "#!/usr/bin/env python3\n"
                    "import sys\n"
                    "EXIT_OK = 0\n"
                    "EXIT_RED = 1\n"
                    "sys.exit(EXIT_OK)\n"
                ),
            },
        )
        _, probs_third = collect_census(third_surface)
        check(
            "positive control (3rd surface): flags an unregistered two-state gate",
            any("gate:scripts/check-unregistered-two-state.py" in p for p in probs_third),
            str(probs_third),
        )

    # Arm 5: Positive control for a total-boolean gate that secretly spawns a live cargo/rch probe
    with tempfile.TemporaryDirectory() as tmp:
        fake_boolean = _write_fixture_tree(
            pathlib.Path(tmp),
            {
                "scripts/check-action-pins.py": (
                    "#!/usr/bin/env python3\n"
                    "import subprocess\n"
                    "subprocess.run(['rch', 'exec', '--', 'cargo', 'test'])\n"
                ),
            },
        )
        _, probs_fake_bool = collect_census(fake_boolean)
        check(
            "positive control: rejects a total-boolean gate that spawns a live rch/cargo probe without EXIT_CANNOT",
            any("gate:scripts/check-action-pins.py" in p for p in probs_fake_bool),
            str(probs_fake_bool),
        )

    # Arm 6: Positive control for missing terminatorClass in emit_proof_capsule.py
    with tempfile.TemporaryDirectory() as tmp:
        capsule_text = (real / "scripts/lib/emit_proof_capsule.py").read_text(encoding="utf-8")
        broken_capsule = _write_fixture_tree(
            pathlib.Path(tmp),
            {
                "scripts/lib/emit_proof_capsule.py": capsule_text.replace("terminatorClass", "omittedField"),
            },
        )
        _, probs_capsule = collect_census(broken_capsule)
        check(
            "positive control: detects missing terminatorClass in emit_proof_capsule.py",
            any("dispatch:scripts/lib/emit_proof_capsule.py" in p for p in probs_capsule),
            str(probs_capsule),
        )

    # Arm 7: Negative control — a legitimate two-state total boolean property is NOT flagged
    boolean_surfaces = [
        s for s in surfaces if s.get("classification") == "two_state_total_boolean"
    ]
    check(
        "negative control: pure boolean static gates are accepted without false positives",
        len(boolean_surfaces) >= 15 and all(s["valid"] for s in boolean_surfaces),
        f"count={len(boolean_surfaces)}",
    )

    # Arm 8: Negative control — a new gate with EXIT_CANNOT = 3 / CannotAnswer is automatically accepted
    with tempfile.TemporaryDirectory() as tmp:
        good_tri_state = _write_fixture_tree(
            pathlib.Path(tmp),
            {
                "scripts/check-new-tri-state.py": (
                    "#!/usr/bin/env python3\n"
                    "EXIT_OK = 0\n"
                    "EXIT_RED = 1\n"
                    "EXIT_CANNOT = 3\n"
                    "class CannotAnswer(Exception):\n"
                    "    pass\n"
                ),
            },
        )
        _, probs_good_tri = collect_census(good_tri_state)
        check(
            "negative control: a new gate declaring EXIT_CANNOT = 3 / CannotAnswer passes without registry edits",
            len(probs_good_tri) == 0,
            str(probs_good_tri),
        )

    # Arm 9: Empty-world guard on an empty directory
    with tempfile.TemporaryDirectory() as tmp:
        caught_empty = False
        try:
            collect_census(pathlib.Path(tmp))
        except CannotAnswer:
            caught_empty = True
        check(
            "empty-world guard: empty directory raises CannotAnswer (exit 3)",
            caught_empty,
        )

    # Arm 10: Empty-world guard when a required known surface is missing
    with tempfile.TemporaryDirectory() as tmp:
        missing_known = _write_fixture_tree(
            pathlib.Path(tmp),
            {"scripts/real_shape_corpus/probe.py": None},
        )
        caught_missing = False
        try:
            collect_census(missing_known)
        except CannotAnswer as exc:
            caught_missing = "probe:scripts/real_shape_corpus/probe.py" in str(exc)
        check(
            "empty-world guard: missing required known surface raises CannotAnswer (exit 3)",
            caught_missing,
        )

    if failures:
        print(f"[tri-state-verdict-contract] SELF-TEST FAILED: {failures} of {arms} arm(s)")
        return EXIT_RED
    print(f"[tri-state-verdict-contract] self-test: all {arms} arms passed.")
    return EXIT_OK


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(
        description="Enforce the tri-state verdict contract across health, gate, and metric surfaces (1azkt.70).",
        allow_abbrev=False,
    )
    parser.add_argument("--self-test", action="store_true", help="run adversarial self-test suite")
    parser.add_argument("--json", action="store_true", help="emit JSON census report")
    parser.add_argument(
        "--report-only",
        action="store_true",
        help="report findings without exiting 1 (still exits 3 on empty-world CannotAnswer)",
    )
    try:
        args = parser.parse_args(argv)
    except SystemExit:
        return EXIT_USAGE

    if args.self_test:
        return self_test()

    try:
        surfaces, problems = collect_census(repo_root())
    except CannotAnswer as exc:
        if args.json:
            print(
                json.dumps(
                    {
                        "schema": "ee.tri_state_verdict_census.v1",
                        "status": "could_not_measure",
                        "error": str(exc),
                    },
                    indent=2,
                )
            )
        else:
            print(f"CANNOT ANSWER: {exc}", file=sys.stderr)
        return EXIT_CANNOT

    status = "measured_healthy" if not problems else "measured_unhealthy"
    if args.json:
        print(
            json.dumps(
                {
                    "schema": "ee.tri_state_verdict_census.v1",
                    "status": status,
                    "surfaceCount": len(surfaces),
                    "triStateCount": sum(
                        1 for s in surfaces if s.get("classification") == "tri_state"
                    ),
                    "totalBooleanCount": sum(
                        1
                        for s in surfaces
                        if s.get("classification") == "two_state_total_boolean"
                    ),
                    "problemCount": len(problems),
                    "problems": problems,
                    "surfaces": surfaces,
                },
                indent=2,
            )
        )
    else:
        tri_count = sum(1 for s in surfaces if s.get("classification") == "tri_state")
        bool_count = sum(
            1 for s in surfaces if s.get("classification") == "two_state_total_boolean"
        )
        print(
            f"[tri-state-verdict-contract] status={status} surfaces={len(surfaces)} "
            f"(tri_state={tri_count}, two_state_total_boolean={bool_count}, problems={len(problems)})"
        )
        for problem in problems:
            print(f"  - {problem}", file=sys.stderr)

    if problems and not args.report_only:
        return EXIT_RED
    return EXIT_OK


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
