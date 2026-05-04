const CLI_SOURCE: &str = include_str!("../src/cli/mod.rs");
const INVENTORY: &str = include_str!("../docs/mechanical-boundary-command-inventory.md");
const README_SOURCE: &str = include_str!("../README.md");

const REQUIRED_MATRIX_HEADERS: &[&str] = &[
    "Surface",
    "Classification",
    "Owner / ADR",
    "README workflow(s)",
    "Mechanical data source",
    "Skill handoff",
    "Degraded code if unavailable",
    "Side-effect / idempotency",
    "Runtime / cancellation posture",
    "Fixture / golden coverage",
    "JSON schema expectation",
    "Required coverage owner",
];

const BASELINE_LEDGER_HEADERS: &[&str] = &[
    "Baseline surface",
    "Actual command paths or absence finding",
    "Mechanical source",
    "Side-effect contract",
    "Runtime contract",
    "Degraded / repair posture",
    "Evidence required",
];

const WORKFLOW_PARITY_HEADERS: &[&str] = &[
    "Workflow ID",
    "README surface",
    "Post-migration user path",
    "Required ee commands",
    "Project-local skill",
    "Degraded / unavailable behavior",
    "Repair command",
    "Owning bead IDs",
    "Test / E2E coverage",
    "No-feature-loss status",
];

const README_WORKFLOW_ROWS: &[(&str, &str, &str, &[&str])] = &[
    (
        "install-verify",
        "### Verify",
        "Installation and Verify",
        &[
            "version",
            "doctor",
            "status",
            "install check",
            "install plan",
            "update",
        ],
    ),
    (
        "quick-example-context-loop",
        "## Quick Example",
        "TLDR and Quick Example",
        &[
            "init",
            "remember",
            "import cass",
            "context",
            "why",
            "outcome",
            "search",
        ],
    ),
    (
        "quick-start-core-loop",
        "## Quick Start",
        "Quick Start",
        &[
            "init",
            "import cass",
            "context",
            "remember",
            "review session",
            "curate candidates",
            "curate apply",
            "search",
        ],
    ),
    (
        "core-command-reference",
        "### Core workflow",
        "Command Reference core workflow",
        &[
            "init", "status", "doctor", "context", "search", "remember", "outcome", "why", "pack",
        ],
    ),
    (
        "import-ingestion",
        "### Import & ingestion",
        "Command Reference Import and ingestion",
        &[
            "import cass",
            "import jsonl",
            "import eidetic-legacy",
            "review session",
        ],
    ),
    (
        "curation-rules",
        "### Curation & rules",
        "Curation and rules",
        &[
            "curate candidates",
            "curate validate",
            "curate apply",
            "curate accept",
            "curate reject",
            "curate snooze",
            "curate merge",
            "curate disposition",
            "rule add",
            "rule list",
            "rule show",
            "rule protect",
        ],
    ),
    (
        "memory-inspection",
        "### Memory inspection",
        "Memory inspection",
        &[
            "memory show",
            "memory list",
            "memory history",
            "memory revise",
            "why",
        ],
    ),
    (
        "graph-index-derived-assets",
        "### Graph",
        "Graph and Index",
        &[
            "graph export",
            "graph neighborhood",
            "graph centrality-refresh",
            "graph feature-enrichment",
            "index status",
            "index rebuild",
            "index reembed",
        ],
    ),
    (
        "workspace-model-schema-adapters",
        "### Workspace, models, schemas",
        "Workspace, models, schemas, and MCP",
        &[
            "workspace resolve",
            "workspace list",
            "workspace alias",
            "model status",
            "model list",
            "schema list",
            "schema export",
            "mcp manifest",
            "agent-docs",
            "help",
            "introspect",
        ],
    ),
    (
        "backup-restore",
        "### Backup & restore",
        "Backup and Restore",
        &[
            "backup create",
            "backup list",
            "backup inspect",
            "backup verify",
            "backup restore",
        ],
    ),
    (
        "diagnostics-eval-ops",
        "### Diagnostics, eval, ops",
        "Diagnostics, eval, and ops",
        &[
            "capabilities",
            "check",
            "health",
            "doctor",
            "diag claims",
            "diag dependencies",
            "diag graph",
            "diag integrity",
            "diag quarantine",
            "diag streams",
            "eval run",
            "eval list",
            "daemon",
            "analyze science-status",
        ],
    ),
    (
        "configuration-context-profiles",
        "## Configuration",
        "Configuration and Context Profiles",
        &["context", "pack", "status", "workspace resolve"],
    ),
    (
        "cass-integration",
        "## CASS Integration",
        "CASS Integration",
        &["import cass", "review session", "status", "doctor"],
    ),
    (
        "agent-harness-integration",
        "## Agent Harness Integration",
        "Agent Harness Integration",
        &[
            "context",
            "remember",
            "outcome",
            "curate candidates",
            "memory show",
            "mcp manifest",
            "handoff create",
            "handoff inspect",
            "handoff resume",
        ],
    ),
    (
        "privacy-trust",
        "## Privacy & Trust",
        "Privacy and Trust",
        &[
            "remember",
            "outcome",
            "curate candidates",
            "rule protect",
            "handoff create",
            "handoff preview",
            "why",
        ],
    ),
    (
        "troubleshooting",
        "## Troubleshooting",
        "Troubleshooting",
        &[
            "index rebuild",
            "index reembed",
            "import cass",
            "init",
            "workspace list",
            "workspace alias",
            "status",
            "doctor",
            "model status",
        ],
    ),
    (
        "limitations-faq-docs",
        "## Limitations",
        "Limitations, FAQ, and Documentation",
        &[
            "status",
            "agent-docs",
            "doctor",
            "mcp manifest",
            "backup create",
            "index rebuild",
        ],
    ),
];

const BASELINE_ACTUAL_COMMANDS: &[(&str, &[&str])] = &[
    (
        "workspace setup and registry",
        &[
            "init",
            "workspace resolve",
            "workspace list",
            "workspace alias",
        ],
    ),
    (
        "manual memory write/read",
        &[
            "remember",
            "memory list",
            "memory show",
            "memory history",
            "memory revise",
        ],
    ),
    (
        "outcome feedback and quarantine",
        &[
            "outcome",
            "outcome quarantine list",
            "outcome quarantine release",
        ],
    ),
    (
        "explicit imports",
        &["import cass", "import jsonl", "import eidetic-legacy"],
    ),
    (
        "derived search index",
        &["index status", "index rebuild", "index reembed"],
    ),
    (
        "backup and restore side paths",
        &[
            "backup create",
            "backup list",
            "backup inspect",
            "backup verify",
            "backup restore",
        ],
    ),
    (
        "export renderers currently present",
        &["schema export", "graph export", "procedure export"],
    ),
    (
        "deterministic evaluation entrypoint",
        &["eval run", "eval list"],
    ),
    (
        "status, health, and config-sensitive probes",
        &[
            "status",
            "health",
            "check",
            "capabilities",
            "doctor",
            "diag claims",
            "diag dependencies",
            "diag graph",
            "diag integrity",
            "diag quarantine",
            "diag streams",
        ],
    ),
    (
        "static discovery and schemas",
        &[
            "help",
            "version",
            "introspect",
            "schema list",
            "model status",
            "model list",
            "mcp manifest",
            "agent-docs",
        ],
    ),
];

const BASELINE_ABSENT_COMMANDS: &[&str] = &[
    "profile list",
    "profile show",
    "db status",
    "db migrate",
    "db check",
    "db backup",
    "index vacuum",
    "restore",
    "export jsonl",
    "eval report",
    "completion",
    "config",
];

const SIDE_EFFECT_CLASSES: &[&str] = &[
    "read_only",
    "read_only_now",
    "report_only",
    "read_only_or_unavailable",
    "append_only",
    "audited_mutation",
    "derived_asset_rebuild",
    "side_path_artifact",
    "supervised_jobs",
    "mixed",
    "degraded_unavailable",
    "report_only_or_append",
    "report_only_or_audited_mutation",
];

const RUNTIME_CLASSES: &[&str] = &[
    "immediate",
    "bounded_read",
    "bounded_query",
    "bounded_write",
    "side_path_artifact",
    "derived_rebuild",
    "supervised",
    "streaming",
    "degraded_unavailable",
];

#[test]
fn mechanical_boundary_inventory_covers_all_cli_command_paths() -> Result<(), String> {
    let commands = command_paths_from_extract_function(CLI_SOURCE)?;
    assert_eq!(
        commands.len(),
        145,
        "unexpected CLI command count; update the mechanical boundary inventory"
    );

    let missing = commands
        .iter()
        .filter(|command| !INVENTORY.contains(&format!("`{command}`")))
        .cloned()
        .collect::<Vec<_>>();

    assert!(
        missing.is_empty(),
        "mechanical boundary inventory missing command path(s): {missing:?}"
    );
    assert!(
        INVENTORY.contains("Unmapped command count: 0"),
        "inventory must record the unmapped command count"
    );
    Ok(())
}

#[test]
fn command_boundary_matrix_covers_public_command_families() -> Result<(), String> {
    let commands = command_paths_from_extract_function(CLI_SOURCE)?;
    let matrix = matrix_section(INVENTORY)?;
    let mut families = commands
        .iter()
        .filter_map(|command| command.split_whitespace().next())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    families.sort();
    families.dedup();

    let missing = families
        .iter()
        .filter(|family| !matrix.contains(&format!("`{family}`")))
        .cloned()
        .collect::<Vec<_>>();

    assert!(
        missing.is_empty(),
        "command boundary matrix missing public command families: {missing:?}"
    );
    Ok(())
}

#[test]
fn command_boundary_matrix_has_required_columns_and_complete_rows() -> Result<(), String> {
    let rows = matrix_rows(INVENTORY)?;
    let header = rows
        .first()
        .ok_or_else(|| "command boundary matrix must include a header row".to_owned())?;

    assert_eq!(
        header, REQUIRED_MATRIX_HEADERS,
        "command boundary matrix headers changed; update tests and frv3 contract together"
    );

    let allowed_classifications = [
        "mechanical CLI",
        "mechanical CLI or optional adapter wrapper",
        "optional adapter wrapper",
        "fix backing data",
        "split",
        "move to skill unless static lookup",
        "move to skill or split deterministic tagging",
        "degrade/unavailable pending implementation",
    ];

    for row in rows.iter().skip(2) {
        assert_eq!(
            row.len(),
            header.len(),
            "matrix row has wrong cell count: {row:?}"
        );
        let classification = row_cell(row, 1, "classification")?;
        let owner = row_cell(row, 2, "owner")?;
        let workflow = row_cell(row, 3, "workflow")?;
        let data_source = row_cell(row, 4, "data source")?;
        let side_effect = row_cell(row, 7, "side-effect")?;
        let runtime = row_cell(row, 8, "runtime")?;
        let schema = row_cell(row, 10, "schema")?;
        let coverage = row_cell(row, 11, "coverage")?;

        assert!(
            allowed_classifications
                .iter()
                .any(|allowed| classification.contains(allowed)),
            "matrix row has unsupported classification: {row:?}"
        );
        assert!(
            owner.contains("eidetic_engine_cli-") && owner.contains("ADR 0011"),
            "matrix row must name owning bead and ADR: {row:?}"
        );
        assert!(
            !workflow.is_empty()
                && !data_source.is_empty()
                && !side_effect.is_empty()
                && !runtime.is_empty(),
            "matrix row must include workflow, data source, side-effect, and runtime posture: {row:?}"
        );
        assert!(
            schema.contains("ee.response.v1") || schema.contains("ee.error.v1"),
            "matrix row must name machine schema expectation: {row:?}"
        );
        assert!(
            [
                "unit",
                "contract",
                "e2e",
                "golden",
                "smoke",
                "fixture",
                "skill-boundary",
                "runtime",
            ]
            .iter()
            .any(|needle| coverage.contains(needle)),
            "matrix row must name a concrete coverage owner/type: {row:?}"
        );
    }

    Ok(())
}

#[test]
fn command_boundary_matrix_side_effect_contracts_are_machine_checkable() -> Result<(), String> {
    assert!(
        INVENTORY.contains("### Side-Effect Contract Vocabulary"),
        "inventory must define side-effect contract vocabulary"
    );
    for class in SIDE_EFFECT_CLASSES {
        assert!(
            INVENTORY.contains(&format!("`class={class}`")),
            "side-effect vocabulary missing class={class}"
        );
    }

    for row in matrix_rows(INVENTORY)?.iter().skip(2) {
        let surface = row_cell(row, 0, "surface")?;
        let classification = row_cell(row, 1, "classification")?;
        let degraded_code = row_cell(row, 6, "degraded code")?;
        let side_effect = row_cell(row, 7, "side-effect")?;
        let class = side_effect_class(side_effect)?;

        assert!(
            SIDE_EFFECT_CLASSES.contains(&class),
            "matrix row uses unknown side-effect class `{class}`: {row:?}"
        );
        assert!(
            !side_effect.contains(" may "),
            "side-effect contract must be explicit, not permissive: {row:?}"
        );

        if classification.contains("fix backing data")
            || classification.contains("split")
            || classification.contains("degrade/unavailable")
        {
            assert_ne!(
                degraded_code, "none",
                "risky or unavailable rows must name a degraded code: {row:?}"
            );
        }

        match class {
            "read_only" | "report_only" | "read_only_or_unavailable" => {
                assert!(
                    side_effect.contains("mutation=none"),
                    "read/report-only rows must state mutation=none: {row:?}"
                );
            }
            "read_only_now" => {
                assert!(
                    side_effect.contains("future") && side_effect.contains("audit"),
                    "read_only_now rows must constrain future writes: {row:?}"
                );
            }
            "append_only" => {
                assert!(
                    side_effect.contains("append")
                        || side_effect.contains("keyed")
                        || side_effect.contains("retry returns existing"),
                    "append-only rows must name append/idempotency behavior: {row:?}"
                );
                assert!(
                    side_effect.contains("audit"),
                    "append-only rows must name audit behavior: {row:?}"
                );
            }
            "audited_mutation" => {
                assert!(
                    side_effect.contains("transaction") && side_effect.contains("audit"),
                    "audited mutation rows must name transaction and audit behavior: {row:?}"
                );
                assert!(
                    side_effect.contains("idempot")
                        || side_effect.contains("dry-run")
                        || side_effect.contains("rule key"),
                    "audited mutation rows must name retry/idempotency or dry-run posture: {row:?}"
                );
            }
            "derived_asset_rebuild" => {
                assert!(
                    side_effect.contains("generation")
                        && side_effect.contains("source DB unchanged"),
                    "derived rebuild rows must name generation and source DB immutability: {row:?}"
                );
            }
            "side_path_artifact" => {
                assert!(
                    side_effect.contains("side path")
                        || side_effect.contains("side-path")
                        || side_effect.contains("sandbox"),
                    "side-path rows must name the side-path/sandbox artifact boundary: {row:?}"
                );
                assert!(
                    side_effect.contains("no-overwrite") || side_effect.contains("no-delete"),
                    "side-path rows must name no-overwrite or no-delete behavior: {row:?}"
                );
            }
            "supervised_jobs" => {
                for required in ["job ledger", "audit", "runtime budget", "cancellation"] {
                    assert!(
                        side_effect.contains(required),
                        "supervised job row missing `{required}`: {row:?}"
                    );
                }
            }
            "mixed" => {
                assert!(
                    side_effect.contains("append")
                        && side_effect.contains("read-only")
                        && side_effect.contains("rollback"),
                    "mixed rows must split mutating, read-only, and rollback behavior: {row:?}"
                );
            }
            "degraded_unavailable" => {
                assert!(
                    side_effect.contains("no mutation") && degraded_code != "none",
                    "degraded rows must state no mutation and a degraded code: {row:?}"
                );
            }
            "report_only_or_append" => {
                assert!(
                    side_effect.contains("read-only")
                        && side_effect.contains("append_only")
                        && side_effect.contains("audit"),
                    "report-or-append rows must split read and candidate-write behavior: {row:?}"
                );
            }
            "report_only_or_audited_mutation" => {
                assert!(
                    side_effect.contains("read-only")
                        && side_effect.contains("audited transaction"),
                    "report-or-audited rows must split read and relation-write behavior: {row:?}"
                );
            }
            unknown => {
                return Err(format!(
                    "unhandled side-effect class `{unknown}` for surface {surface}"
                ));
            }
        }
    }

    Ok(())
}

#[test]
fn command_boundary_matrix_runtime_contracts_are_machine_checkable() -> Result<(), String> {
    assert!(
        INVENTORY.contains("### Runtime / Cancellation Contract Vocabulary"),
        "inventory must define runtime/cancellation contract vocabulary"
    );
    for class in RUNTIME_CLASSES {
        assert!(
            INVENTORY.contains(&format!("`runtime={class}`")),
            "runtime vocabulary missing runtime={class}"
        );
    }

    for row in matrix_rows(INVENTORY)?.iter().skip(2) {
        let runtime = row_cell(row, 8, "runtime")?;
        let class = runtime_class(runtime)?;

        assert!(
            RUNTIME_CLASSES.contains(&class),
            "matrix row uses unknown runtime class `{class}`: {row:?}"
        );
        for required in ["budget=", "cancel=", "partial=", "outcome="] {
            assert!(
                runtime.contains(required),
                "runtime contract missing `{required}`: {row:?}"
            );
        }
        assert!(
            !runtime.contains("cancellable"),
            "runtime contract must use structured cancel= fields, not prose: {row:?}"
        );

        match class {
            "immediate" => {
                for required in ["budget=none", "cancel=not_applicable", "partial=none"] {
                    assert!(
                        runtime.contains(required),
                        "immediate runtime must state `{required}`: {row:?}"
                    );
                }
            }
            "bounded_read" | "bounded_query" => {
                assert!(
                    runtime.contains("cancel=checkpoint"),
                    "bounded read/query runtime must name checkpoint cancellation: {row:?}"
                );
            }
            "bounded_write" => {
                assert!(
                    runtime.contains("pre_") || runtime.contains("cancel=checkpoint"),
                    "bounded write runtime must name pre-commit/pre-write or checkpoint cancellation: {row:?}"
                );
                assert!(
                    runtime.contains("rollback") || runtime.contains("existing_record"),
                    "bounded write runtime must name rollback or idempotent existing-record partial state: {row:?}"
                );
            }
            "side_path_artifact" => {
                assert!(
                    runtime.contains("side_path") || runtime.contains("blocked"),
                    "side-path runtime must name side-path or blocked partial state: {row:?}"
                );
            }
            "derived_rebuild" => {
                assert!(
                    runtime.contains("partial=derived_asset_discard"),
                    "derived rebuild runtime must discard incomplete derived assets: {row:?}"
                );
            }
            "supervised" => {
                assert!(
                    runtime.contains("cancel=job_signal") && runtime.contains("job_ledger"),
                    "supervised runtime must name job signal and job ledger: {row:?}"
                );
            }
            "streaming" => {
                assert!(
                    runtime.contains("cancel=job_signal") && runtime.contains("append"),
                    "streaming runtime must name job signal and append checkpoint policy: {row:?}"
                );
            }
            "degraded_unavailable" => {
                assert!(
                    runtime.contains("outcome=degraded"),
                    "degraded runtime must map to degraded outcome: {row:?}"
                );
            }
            unknown => {
                return Err(format!("unhandled runtime class `{unknown}`"));
            }
        }
    }

    Ok(())
}

#[test]
fn baseline_infrastructure_ledger_covers_actual_and_absent_command_paths() -> Result<(), String> {
    let commands = command_paths_from_extract_function(CLI_SOURCE)?;
    let rows = baseline_rows(INVENTORY)?;
    let header = rows
        .first()
        .ok_or_else(|| "baseline infrastructure ledger must include a header row".to_owned())?;

    assert_eq!(
        header, BASELINE_LEDGER_HEADERS,
        "baseline ledger headers changed; update hy6y contract tests with the docs"
    );

    for row in rows.iter().skip(2) {
        assert_eq!(
            row.len(),
            header.len(),
            "baseline ledger row has wrong cell count: {row:?}"
        );

        for index in 2..header.len() {
            assert!(
                !row_cell(row, index, "baseline ledger cell")?.is_empty(),
                "baseline ledger row has an empty required cell: {row:?}"
            );
        }

        assert!(
            row_cell(row, 3, "baseline side-effect")?.contains("class="),
            "baseline ledger row must name a side-effect class: {row:?}"
        );
        assert!(
            row_cell(row, 4, "baseline runtime")?.contains("runtime="),
            "baseline ledger row must name a runtime class: {row:?}"
        );
        let evidence = row_cell(row, 6, "baseline evidence")?;
        assert!(
            ["test", "e2e", "golden", "contract", "fixture"]
                .iter()
                .any(|needle| evidence.contains(needle)),
            "baseline ledger row must name concrete evidence: {row:?}"
        );
    }

    for (surface, expected_commands) in BASELINE_ACTUAL_COMMANDS {
        let row = baseline_row_for(&rows, surface)?;
        let command_cell = row_cell(row, 1, "baseline command paths")?;

        for command in *expected_commands {
            assert!(
                commands.iter().any(|actual| actual == command),
                "baseline expected command `{command}` is not in the CLI extractor"
            );
            assert!(
                command_cell.contains(&format!("`{command}`")),
                "baseline ledger row `{surface}` missing command `{command}`"
            );
        }
    }

    let absence_row = baseline_row_for(&rows, "non-present baseline terms")?;
    let absence_cell = row_cell(absence_row, 1, "baseline absence findings")?;
    for command in BASELINE_ABSENT_COMMANDS {
        assert!(
            !commands.iter().any(|actual| actual == command),
            "absence finding `{command}` is now an actual CLI path; add matrix and ledger coverage"
        );
        assert!(
            absence_cell.contains(&format!("`{command}`")),
            "baseline absence row missing `{command}`"
        );
    }

    Ok(())
}

#[test]
fn readme_workflow_parity_matrix_covers_advertised_surfaces() -> Result<(), String> {
    let commands = command_paths_from_extract_function(CLI_SOURCE)?;
    let rows = workflow_parity_rows(INVENTORY)?;
    let header = rows
        .first()
        .ok_or_else(|| "README workflow parity matrix must include a header row".to_owned())?;

    assert_eq!(
        header, WORKFLOW_PARITY_HEADERS,
        "workflow parity headers changed; update the myk6 contract with the docs"
    );
    assert!(
        README_SOURCE.contains("## TL;DR"),
        "README TLDR workflow surface must remain discoverable"
    );

    let mut workflow_ids = std::collections::BTreeSet::new();
    for row in rows.iter().skip(2) {
        assert_eq!(
            row.len(),
            header.len(),
            "workflow parity row has wrong cell count: {row:?}"
        );
        for index in 0..header.len() {
            assert!(
                !row_cell(row, index, "workflow parity cell")?.is_empty(),
                "workflow parity row has an empty required cell: {row:?}"
            );
        }

        let workflow_id = row_cell(row, 0, "workflow id")?;
        assert!(
            workflow_ids.insert(workflow_id),
            "workflow parity matrix has duplicate workflow ID `{workflow_id}`"
        );

        let required_commands = row_cell(row, 3, "required commands")?;
        let skill = row_cell(row, 4, "project-local skill")?;
        let degraded = row_cell(row, 5, "degraded behavior")?;
        let repair = row_cell(row, 6, "repair command")?;
        let owners = row_cell(row, 7, "owning beads")?;
        let coverage = row_cell(row, 8, "coverage")?;
        let no_feature_loss = row_cell(row, 9, "no-feature-loss status")?;

        assert!(
            required_commands.contains('`'),
            "workflow row must name backticked ee command paths: {row:?}"
        );
        assert!(
            skill == "none" || skill.contains("skill"),
            "workflow row must explicitly say no skill or name a project-local skill: {row:?}"
        );
        assert!(
            degraded.contains('`') || degraded.contains("intentionally deferred"),
            "workflow row must name degraded/unavailable behavior: {row:?}"
        );
        assert!(
            repair.contains("ee "),
            "workflow row must name a copy-pasteable repair command: {row:?}"
        );
        assert!(
            owners.contains("eidetic_engine_cli-"),
            "workflow row must name owning bead IDs: {row:?}"
        );
        assert!(
            ["test", "e2e", "golden", "contract", "fixture", "docs"]
                .iter()
                .any(|needle| coverage.contains(needle)),
            "workflow row must name concrete test/e2e/docs coverage: {row:?}"
        );
        assert!(
            no_feature_loss.contains("no feature was dropped")
                || no_feature_loss.contains("deferred with rationale"),
            "workflow row must explicitly document no-feature-loss or deferred rationale: {row:?}"
        );
    }

    let expected_ids = README_WORKFLOW_ROWS
        .iter()
        .map(|(workflow_id, _, _, _)| *workflow_id)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        workflow_ids, expected_ids,
        "workflow parity matrix must cover every advertised README workflow row exactly"
    );

    for (workflow_id, readme_marker, surface_fragment, expected_commands) in README_WORKFLOW_ROWS {
        assert!(
            README_SOURCE.contains(readme_marker),
            "README is missing advertised workflow marker `{readme_marker}`"
        );
        let row = workflow_parity_row_for(&rows, workflow_id)?;
        let surface = row_cell(row, 1, "README surface")?;
        let command_cell = row_cell(row, 3, "required commands")?;

        assert!(
            surface.contains(surface_fragment),
            "workflow `{workflow_id}` must cite README surface `{surface_fragment}`"
        );
        for command in *expected_commands {
            assert!(
                commands.iter().any(|actual| actual == command),
                "workflow `{workflow_id}` expects command `{command}` but it is not in the CLI extractor"
            );
            assert!(
                command_cell.contains(&format!("`{command}`")),
                "workflow `{workflow_id}` missing command `{command}` in required commands cell"
            );
        }
    }

    for required in [
        "ee.workflow_parity.e2e_log.v1",
        "workflow ID",
        "generated command list",
        "commands run",
        "skill paths used",
        "degraded states observed",
        "artifact paths",
        "stdout and stderr artifact paths",
        "parsed JSON schema or golden status",
        "first-failure diagnosis",
    ] {
        assert!(
            INVENTORY.contains(required),
            "workflow parity e2e log contract missing required field text: {required}"
        );
    }

    Ok(())
}

#[test]
fn skill_only_matrix_rows_have_skill_handoff_and_boundary_coverage() -> Result<(), String> {
    for row in matrix_rows(INVENTORY)?.iter().skip(2) {
        let classification = row_cell(row, 1, "classification")?;
        let skill_handoff = row_cell(row, 5, "skill handoff")?;
        let coverage = row_cell(row, 11, "coverage")?;
        if classification.contains("move to skill") {
            assert_ne!(
                skill_handoff, "none",
                "skill-only row must name handoff: {row:?}"
            );
            assert!(
                coverage.contains("skill-boundary"),
                "skill-only row must require skill-boundary coverage: {row:?}"
            );
        }
    }
    Ok(())
}

#[test]
fn matrix_e2e_log_schema_records_required_fields() {
    for required in [
        "ee.command_boundary_matrix.e2e_log.v1",
        "generated command list",
        "matrix path and BLAKE3 hash",
        "missing and extra command rows",
        "classification summary",
        "side-effect coverage summary",
        "schema coverage summary",
        "workflow parity coverage",
        "fixture/evidence bundle hashes",
        "runtime budget, deadline, and budget exhaustion signal",
        "cancellation injection point and observed cancellation phase",
        "observed `Outcome` and process exit code",
        "before/after DB and index generation",
        "changed record IDs and audit IDs",
        "records written, rolled back, or audited",
        "filesystem artifacts created",
        "forbidden filesystem operations checked",
        "stdout and stderr artifact paths",
        "first-failure diagnosis",
    ] {
        assert!(
            INVENTORY.contains(required),
            "matrix E2E log schema missing required field text: {required}"
        );
    }
}

#[test]
fn mechanical_boundary_inventory_names_mock_and_stub_surfaces() {
    for anchor in [
        "src/core/causal.rs:335",
        "src/core/causal.rs:892",
        "src/core/procedure.rs:1174",
        "src/core/rehearse.rs:364",
        "src/output/mod.rs:4092",
        "src/core/tripwire.rs:376",
        "src/core/preflight.rs:870",
        "src/core/situation.rs:1926",
        "src/core/certificate.rs:457",
        "src/core/economy.rs:735",
        "src/core/memory.rs:1572",
    ] {
        assert!(
            INVENTORY.contains(anchor),
            "inventory missing risky surface anchor {anchor}"
        );
    }
}

fn matrix_section(inventory: &str) -> Result<&str, String> {
    let (_, after_start) = inventory
        .split_once("## Command Boundary Matrix")
        .ok_or_else(|| "Command Boundary Matrix section must exist".to_owned())?;
    let (section, _) = after_start
        .split_once("### Matrix Maintenance Rules")
        .ok_or_else(|| "Matrix Maintenance Rules section must follow matrix".to_owned())?;
    Ok(section)
}

fn matrix_rows(inventory: &str) -> Result<Vec<Vec<String>>, String> {
    let rows = matrix_section(inventory)?
        .lines()
        .filter(|line| line.starts_with('|'))
        .map(markdown_row_cells)
        .collect::<Vec<_>>();

    if rows.len() < 3 {
        Err("command boundary matrix must include header, delimiter, and data rows".to_owned())
    } else {
        Ok(rows)
    }
}

fn baseline_section(inventory: &str) -> Result<&str, String> {
    let (_, after_start) = inventory
        .split_once("## Baseline Infrastructure Coverage Ledger")
        .ok_or_else(|| "Baseline Infrastructure Coverage Ledger section must exist".to_owned())?;
    let (section, _) = after_start
        .split_once("## Full Command Inventory")
        .ok_or_else(|| "Full Command Inventory section must follow baseline ledger".to_owned())?;
    Ok(section)
}

fn baseline_rows(inventory: &str) -> Result<Vec<Vec<String>>, String> {
    let rows = baseline_section(inventory)?
        .lines()
        .filter(|line| line.starts_with('|'))
        .map(markdown_row_cells)
        .collect::<Vec<_>>();

    if rows.len() < 3 {
        Err(
            "baseline infrastructure ledger must include header, delimiter, and data rows"
                .to_owned(),
        )
    } else {
        Ok(rows)
    }
}

fn workflow_parity_section(inventory: &str) -> Result<&str, String> {
    let (_, after_start) = inventory
        .split_once("## README Workflow Parity Matrix")
        .ok_or_else(|| "README Workflow Parity Matrix section must exist".to_owned())?;
    let (section, _) = after_start
        .split_once("## Baseline Infrastructure Coverage Ledger")
        .ok_or_else(|| {
            "Baseline Infrastructure Coverage Ledger section must follow parity matrix".to_owned()
        })?;
    Ok(section)
}

fn workflow_parity_rows(inventory: &str) -> Result<Vec<Vec<String>>, String> {
    let rows = workflow_parity_section(inventory)?
        .lines()
        .filter(|line| line.starts_with('|'))
        .map(markdown_row_cells)
        .collect::<Vec<_>>();

    if rows.len() < 3 {
        Err(
            "README workflow parity matrix must include header, delimiter, and data rows"
                .to_owned(),
        )
    } else {
        Ok(rows)
    }
}

fn baseline_row_for<'a>(rows: &'a [Vec<String>], surface: &str) -> Result<&'a [String], String> {
    rows.iter()
        .skip(2)
        .find(|row| row.first().is_some_and(|cell| cell == surface))
        .map(Vec::as_slice)
        .ok_or_else(|| format!("baseline ledger missing row for `{surface}`"))
}

fn workflow_parity_row_for<'a>(
    rows: &'a [Vec<String>],
    workflow_id: &str,
) -> Result<&'a [String], String> {
    rows.iter()
        .skip(2)
        .find(|row| row.first().is_some_and(|cell| cell == workflow_id))
        .map(Vec::as_slice)
        .ok_or_else(|| format!("workflow parity matrix missing row for `{workflow_id}`"))
}

fn markdown_row_cells(line: &str) -> Vec<String> {
    line.trim_matches('|')
        .split('|')
        .map(|cell| cell.trim().to_owned())
        .collect()
}

fn row_cell<'a>(row: &'a [String], index: usize, context: &str) -> Result<&'a str, String> {
    row.get(index)
        .map(String::as_str)
        .ok_or_else(|| format!("matrix row missing {context} cell at index {index}: {row:?}"))
}

fn side_effect_class(side_effect: &str) -> Result<&str, String> {
    let rest = side_effect
        .strip_prefix("class=")
        .ok_or_else(|| format!("side-effect cell must start with class=: {side_effect}"))?;
    rest.split(';')
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("side-effect class is empty: {side_effect}"))
}

fn runtime_class(runtime: &str) -> Result<&str, String> {
    let rest = runtime
        .strip_prefix("runtime=")
        .ok_or_else(|| format!("runtime cell must start with runtime=: {runtime}"))?;
    rest.split(';')
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("runtime class is empty: {runtime}"))
}

fn command_paths_from_extract_function(source: &str) -> Result<Vec<String>, String> {
    let start_marker = "fn extract_command_path(cli: &Cli) -> String {";
    let end_marker = "\n    /// Returns a stable identifier";
    let (_, after_start) = source
        .split_once(start_marker)
        .ok_or_else(|| "extract_command_path function must exist".to_owned())?;
    let (body, _) = after_start
        .split_once(end_marker)
        .ok_or_else(|| "extract_command_path function end marker must exist".to_owned())?;

    let mut strings = Vec::new();
    let bytes = body.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes.get(index) != Some(&b'"') {
            index += 1;
            continue;
        }

        index += 1;
        let start = index;
        while let Some(byte) = bytes.get(index) {
            if *byte == b'"' {
                break;
            }

            if *byte == b'\\' {
                index += 2;
            } else {
                index += 1;
            }
        }
        let value = body
            .get(start..index)
            .ok_or_else(|| format!("invalid string literal span {start}..{index}"))?;
        if value
            .chars()
            .next()
            .is_some_and(|first| first.is_ascii_lowercase())
        {
            strings.push(value.to_owned());
        }
        index += 1;
    }

    strings.sort();
    strings.dedup();
    Ok(strings)
}
