const CLI_SOURCE: &str = include_str!("../src/cli/mod.rs");
const INVENTORY: &str = include_str!("../docs/mechanical-boundary-command-inventory.md");
const README_SOURCE: &str = include_str!("../README.md");
const AGENTS_SOURCE: &str = include_str!("../AGENTS.md");
const AGENT_INTEGRATION_SOURCE: &str = include_str!("../docs/agent_integration.md");
const SKILL_STANDARDS: &str = include_str!("../skills/ee-skill-standards/SKILL.md");
const PROCEDURE_DISTILLATION_SKILL: &str =
    include_str!("../skills/procedure-distillation/SKILL.md");

const KNOWN_PROJECT_LOCAL_SKILL_PATHS: &[(&str, &str)] = &[
    ("skills/ee-skill-standards/SKILL.md", SKILL_STANDARDS),
    (
        "skills/procedure-distillation/SKILL.md",
        PROCEDURE_DISTILLATION_SKILL,
    ),
];

fn assert_marker_before(
    source_name: &str,
    source: &str,
    earlier_marker: &str,
    later_marker: &str,
    context: &str,
) -> Result<(), String> {
    let earlier_pos = source
        .find(earlier_marker)
        .ok_or_else(|| format!("{source_name} missing earlier marker `{earlier_marker}`"))?;
    let later_pos = source
        .find(later_marker)
        .ok_or_else(|| format!("{source_name} missing later marker `{later_marker}`"))?;

    if earlier_pos >= later_pos {
        return Err(format!(
            "{source_name} must introduce `{earlier_marker}` before `{later_marker}` for {context}"
        ));
    }

    Ok(())
}

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
            "pack build",
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
            "pack build",
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
            "init",
            "status",
            "doctor",
            "search",
            "remember",
            "outcome",
            "why",
            "pack build",
            "pack replay",
            "pack diff",
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
            "playbook extract",
            "playbook list",
            "playbook export",
            "playbook import",
            "rule add",
            "rule list",
            "rule show",
            "rule mark",
            "rule protect",
            "rule update",
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
            "memory expire",
            "memory link",
            "memory tags",
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
            "index vacuum",
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
            "export",
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
            "diag quarantine list",
            "diag quarantine show",
            "diag streams",
            "eval run",
            "eval list",
            "eval report",
            "perf compare",
            "perf budget check",
            "perf explain-latency",
            "daemon",
            "analyze science-status",
        ],
    ),
    (
        "configuration-context-profiles",
        "## Configuration",
        "Configuration and Context Profiles",
        &[
            "pack build",
            "status",
            "workspace resolve",
            "profile config plan",
            "profile config apply",
        ],
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
            "pack build",
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
            "rule mark",
            "rule protect",
            "rule update",
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
            "memory expire",
            "memory link",
            "memory tags",
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
        &[
            "index status",
            "index rebuild",
            "index reembed",
            "index vacuum",
        ],
    ),
    (
        "backup and restore side paths",
        &[
            "export",
            "backup create",
            "backup list",
            "backup inspect",
            "backup verify",
            "backup restore",
        ],
    ),
    (
        "export renderers currently present",
        &[
            "schema export",
            "graph export",
            "procedure export",
            "playbook export",
        ],
    ),
    (
        "deterministic evaluation and performance entrypoints",
        &[
            "eval run",
            "eval list",
            "eval report",
            "perf compare",
            "perf budget check",
            "perf explain-latency",
        ],
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
            "diag quarantine list",
            "diag quarantine show",
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

/// Terms the baseline ledger records as NOT being command paths.
///
/// `db status`, `db check` and `completion` were removed on 2026-09-18
/// (bd-integration-gm-six-red-concealed-uhp28): each has since become a real
/// path emitted by `extract_command_path`, so asserting their absence was
/// asserting a falsehood. This list shrinks as the CLI grows, and an entry
/// leaving it is a signal to confirm the path gained a row -- all three had
/// one already, verified by first-cell match before they were dropped here.
const BASELINE_ABSENT_COMMANDS: &[&str] = &[
    "profile list",
    "profile show",
    "db migrate",
    "db backup",
    "restore",
    "export jsonl",
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

    // No pinned command count here, deliberately.
    //
    // This used to assert `commands.len() == 204`. That number was typed by a
    // human on 2026-05-19 and re-breaks on every CLI addition -- it failed at
    // 453 not because coverage regressed but because the CLI grew, and bumping
    // it to 453 would guarantee the identical failure at 454.
    //
    // The `missing` check below is strictly stronger: it enforces coverage
    // against the LIVE CLI surface rather than against a literal, so it cannot
    // be satisfied by editing a number. Removing the count is the same move as
    // 9e9782c85, which dropped a migration-range counter because a sibling
    // assertion already checked it against the live catalog.
    //
    // This is a removal because something stronger already covers it, which is
    // the only reason that is not a weakening.

    // bd-o74n4: require a TABLE ROW, not a mention anywhere in the file.
    //
    // This predicate used to be `INVENTORY.contains("`{command}`")` — a
    // substring scan over the whole document. A path named in prose, in a
    // heading, or in a flat bullet list satisfied it, so the tier could be
    // cleared without the path ever acquiring a row.
    //
    // MEASURED before changing it, because "the gate is weak" and "the gate is
    // being exploited" are different claims and only the second would make
    // this urgent: at HEAD, 211 paths satisfy the old predicate and the SAME
    // 211 satisfy this one. The prose-only set is EMPTY. So this closes a
    // latent hole and changes no verdict today — which is the honest reason to
    // do it now, while it is free, rather than after someone has filled the
    // document with prose to clear the tier.
    // bd-6hp3w: require the path in the row's FIRST CELL, not in any cell.
    //
    // The table-row predicate above was still satisfied by a path that some
    // OTHER command's row merely mentions, because every cell after the first
    // is prose. That is how it was found: a team row whose description read
    // "`pause`/`resume` toggle network exchange" made the standalone `resume`
    // command "documented" by naming it while describing a different one.
    //
    // MEASURED before changing it, the same discipline as the change above: at
    // the parent commit 454 paths satisfied the any-cell predicate and 439
    // satisfied this one. The gap was real, so the fifteen were given rows --
    // ten rows, grouped only where boundary facts actually coincide -- and
    // writing them out is what surfaced `db check-integrity` sitting first in a
    // row whose lead-in read "Non-mutating database surfaces" while the effect
    // manifest declares it an append_only_write over `audit_log`.
    //
    // BOTH predicates now return 454, so this tightening changes no verdict
    // today. That is the honest moment to close a hole -- while it is free --
    // rather than after the two numbers have silently diverged again.
    let first_cells = INVENTORY
        .lines()
        .filter_map(first_cell_of)
        .collect::<Vec<_>>();
    let has_a_row_of_its_own = |command: &str| {
        let cell = format!("`{command}`");
        first_cells.iter().any(|first| first.contains(&cell))
    };

    let missing = commands
        .iter()
        .filter(|command| !has_a_row_of_its_own(command.as_str()))
        .cloned()
        .collect::<Vec<_>>();

    // State the CONTENT tier here even on the coverage failure. Without this,
    // the obvious repair for the missing list below (paste the absent paths
    // into the Full Command Inventory) turns this assertion green while
    // twelve-column enforcement stays where it is, and no assertion anywhere
    // would ever have named that number.
    let enforced = matrix_enforced_command_paths(&commands)?;
    assert!(
        missing.is_empty(),
        "mechanical boundary inventory command path(s) with no row of their own: {missing:?}\n\
         NOTE (bd-6hp3w): a path is covered by the FIRST cell of a row. Naming it in a later \
         cell of some other command's row documents nothing about its own boundary, so adding \
         it to a description will not clear this list.\n\
         NOTE (bd-o74n4): clearing this list does NOT raise enforcement. The twelve-column \
         Command Boundary Matrix covers {} of {} command paths; the other {} carry no \
         side-effect class, runtime posture, degraded code, fixture coverage, or schema \
         expectation.",
        enforced.len(),
        commands.len(),
        commands.len() - enforced.len()
    );
    assert!(
        INVENTORY.contains("Unmapped command count: 0"),
        "inventory must record the unmapped command count"
    );
    Ok(())
}

/// bd-6hp3w: the coverage predicate must DISCRIMINATE, and be seen to.
///
/// `has_a_row_of_its_own` is only stronger than the any-cell predicate it
/// replaced for as long as `first_cell_of` really returns one cell. If it ever
/// returns the whole line -- a stray `strip_prefix`, a `split` that becomes a
/// `splitn(1)` -- the coverage gate reverts to "mentioned anywhere in a row"
/// and goes on passing, because today every path satisfies BOTH predicates.
/// The regression would be invisible at exactly the moment it was introduced
/// and would only surface as a path quietly acquiring coverage from someone
/// else's prose months later.
///
/// So this asserts the discrimination directly, on a row built to contain one
/// path in its first cell and a different one in a later cell.
#[test]
fn first_cell_coverage_predicate_rejects_a_mention_in_a_later_cell() -> Result<(), String> {
    let row = "| `alpha beta` | `src/cli/mod.rs:1` | see also `gamma delta` | keep mechanical |";
    let first = first_cell_of(row)
        .ok_or_else(|| format!("a table row must yield a first cell; none for {row:?}"))?;

    assert!(
        first.contains("`alpha beta`"),
        "the first cell must carry the path the row documents; got {first:?}"
    );
    assert!(
        !first.contains("`gamma delta`"),
        "a path named in a LATER cell must not be visible to the coverage \
         predicate -- first_cell_of returned {first:?}, which spans more than \
         one cell, and the coverage gate has silently reverted to bd-6hp3w's \
         any-cell behaviour"
    );

    // The negative arm's partner: a line that is not a table row yields no
    // cell at all, so prose outside the tables cannot confer coverage either.
    assert_eq!(
        first_cell_of("`alpha beta` is described in the prose above."),
        None,
        "a non-row line must yield no first cell"
    );

    // And a separator row yields a cell that no backticked path can match,
    // which is what keeps the `|---|` lines from being a coverage surface.
    assert_eq!(first_cell_of("| --- | --- |"), Some(" --- "));

    Ok(())
}

/// CLI command paths that carry a row in the twelve-column Command Boundary
/// Matrix — the only tier that checks a side-effect class, a runtime posture,
/// a degraded code, fixture coverage, or a schema expectation.
fn matrix_enforced_command_paths(commands: &[String]) -> Result<Vec<String>, String> {
    let rows = matrix_rows(INVENTORY)?;
    Ok(commands
        .iter()
        .filter(|command| {
            let cell = format!("`{command}`");
            rows.iter()
                .skip(2)
                .any(|row| row.iter().any(|value| value.contains(&cell)))
        })
        .cloned()
        .collect())
}

/// bd-o74n4: the content tier is a RATCHET with a declared floor.
///
/// The three `command_boundary_matrix_*` assertions iterate `matrix_rows`, so
/// they are structurally blind to any path without a row: dropping a row
/// silently reduces what is enforced, and nothing notices. This pins the
/// count against a floor declared in the document itself.
///
/// Deliberately a floor and not an equality. An exact pin is the shape that
/// rotted `NORMALIZED_CLI_COMMAND_COUNT` (416 against a live 453 for months):
/// a number that must be edited on every unrelated change gets edited without
/// being re-measured, or not at all. A floor only has to move when someone
/// deliberately raises enforcement, and it fails in the direction that matters
/// — enforcement shrinking.
///
/// This does NOT answer what the matrix should cover. That is the open
/// contract question on bd-o74n4 and it needs an operator ruling.
#[test]
fn command_boundary_matrix_enforcement_floor_never_shrinks() -> Result<(), String> {
    let commands = command_paths_from_extract_function(CLI_SOURCE)?;
    let enforced = matrix_enforced_command_paths(&commands)?;

    let declared = INVENTORY
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("- Matrix enforcement floor:")
                .and_then(|value| value.trim().parse::<usize>().ok())
        })
        .ok_or_else(|| {
            "docs/mechanical-boundary-command-inventory.md must declare \
             `- Matrix enforcement floor: <n>` so the content tier has an asserted \
             denominator (bd-o74n4)"
                .to_owned()
        })?;

    assert!(
        enforced.len() >= declared,
        "twelve-column matrix enforcement SHRANK: {} command paths carry a matrix row, \
         below the declared floor of {declared}. Restore the removed row(s), or lower the \
         floor in the same commit with a stated reason. Currently enforced: {enforced:?}",
        enforced.len()
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
        // Accepts either bead-id scheme. The assertion's stated intent is
        // "name an owning bead"; it required the LEGACY `eidetic_engine_cli-`
        // prefix, so the one row carrying a current `bd-xxxxx` id failed while
        // naming its owner perfectly well. 48 of 49 rows use the legacy form
        // and `share` uses the modern one -- requiring the old prefix forever
        // would force a future row to invent a fake id to pass
        // (bd-integration-gm-six-red-concealed-uhp28).
        //
        // ADR 0011 is still REQUIRED of every row and was not relaxed: it is
        // the ADR that governs this matrix, and a row citing only its own
        // feature ADRs is missing the one that makes it a boundary row.
        assert!(
            (owner.contains("eidetic_engine_cli-") || owner.contains("`bd-"))
                && owner.contains("ADR 0011"),
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
            schema.contains("ee.response.v2") || schema.contains("ee.error.v2"),
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
        if skill == "none" {
            assert_eq!(
                skill, "none",
                "workflow row without a skill must use the exact `none` marker: {row:?}"
            );
        } else {
            assert!(
                skill.contains("skill"),
                "workflow row must explicitly name a project-local skill: {row:?}"
            );
            let skill_paths = backticked_tokens_with_prefix(skill, "skills/");
            assert!(
                !skill_paths.is_empty(),
                "workflow row with a skill handoff must cite a backticked skill path: {row:?}"
            );
            for path in skill_paths {
                let source = known_project_local_skill(path).ok_or_else(|| {
                    format!("workflow row references unknown project-local skill path `{path}`")
                })?;
                assert!(
                    !source.trim().is_empty(),
                    "known project-local skill path `{path}` must load non-empty content"
                );
            }
        }
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

    let required_markers = [
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
    ];
    ensure_all_markers_present(
        "workflow parity e2e log contract",
        INVENTORY,
        &required_markers,
    )?;

    Ok(())
}

#[test]
fn readme_pins_swarm_brief_operator_workflow() -> Result<(), String> {
    let required_markers = [
        // `br ready --json` until 2026-09-18. 08b30bfde deliberately replaced
        // the bare form with the flagged one below, which suppresses
        // auto-import and auto-flush side effects during discovery -- a real
        // improvement the marker then failed to follow, so this assertion had
        // been red since 2026-08-05 and invisible
        // (bd-integration-gm-six-red-concealed-uhp28). Quoting the command the
        // README actually documents, not the one it used to.
        "br ready --limit 0 --json --no-auto-import --no-auto-flush --allow-stale",
        "### Swarm brief workflow",
        "ee swarm brief --workspace . --json",
        "ee --fields full swarm brief --workspace . --include-rch --json",
        "ee swarm brief --workspace . --sources git,beads,bv,agent-mail --require-sources --json",
        "ee swarm brief --workspace . --agent-mail-snapshot <snapshot.json> --json",
        "ee swarm work-packet --workspace . --include-rch --claim-gate --candidate <id> --json",
        ".data.topRecommendations[]",
        "safe_surface_candidate",
        ".data.beads.blocked[]",
        ".data.fileSurfaceRisks[]",
        "active_exclusive_reservation",
        ".data.degraded[]",
        "rec.resource_pressure.use_rch_for_cargo",
        "rec.work_selection.no_ready_beads",
        "bv --robot-triage",
        "safeToClaim=true",
        "verdict=safe_to_claim",
        "claimCommandAction",
        "claim-safety gate",
        // README says "BV copy-paste claim command" (README.md:1136) and
        // always has: `git log -S 'bv copy-paste claim command' -- README.md`
        // is EMPTY, so the lowercase form never existed in that file. This
        // marker was wrong the day it was written and only became visible
        // when integration_g_m compiled again
        // (bd-integration-gm-six-red-concealed-uhp28).
        "BV copy-paste claim command",
        "unexpected argument",
        "stale relative to the current source/docs contract",
        "approved RCH/release-path rebuild",
        // README.md:1097-1098 reads "run no BV claim command, and do not use
        // local Cargo install as a workaround" -- the same rewording AGENTS.md
        // received. The marker quoted the older phrasing, which matched
        // neither literally nor with whitespace collapsed.
        "run no BV claim command, and do not use local Cargo install as a workaround",
        "swarm_brief_summary.json",
        "never claims work",
        "never reserves files",
        "never runs builds",
        "never mutates Beads",
        "never schedules agents",
        "paths_counts_subjects_only_no_content",
    ];
    ensure_all_markers_present(
        "README.md swarm brief workflow docs",
        README_SOURCE,
        &required_markers,
    )?;

    assert_marker_before(
        "README.md",
        README_SOURCE,
        "ee swarm work-packet --workspace . --include-rch --claim-gate --candidate <id> --json",
        "br update <id> --status in_progress --json",
        "swarm brief claim workflow",
    )?;

    Ok(())
}

#[test]
fn agent_integration_pins_work_packet_claim_gate_consumer() -> Result<(), String> {
    let required_markers = [
        "## Work-Packet Claim Gate",
        "ee swarm work-packet --workspace . --include-rch --claim-gate --candidate <id> --json",
        "unexpected\nargument",
        "stale relative to the current source/docs\ncontract",
        "approved RCH/release-path\nrebuild",
        "do not run a BV claim command or local Cargo install as a\nworkaround",
        "scripts/agent_consume_work_packet_gate.py",
        "ee.agent.work_packet_gate_decision.v1",
        "safeToClaim=true",
        "runnable=true",
        "Every other\nverdict is inspection-only",
        "intentionally read-only",
        "must not claim Beads",
        "mutate git",
        "run Cargo",
        "do not substitute local Cargo proof",
    ];
    ensure_all_markers_present(
        "docs/agent_integration.md claim-gate markers",
        AGENT_INTEGRATION_SOURCE,
        &required_markers,
    )?;

    assert_marker_before(
        "docs/agent_integration.md",
        AGENT_INTEGRATION_SOURCE,
        "ee swarm work-packet --workspace . --include-rch --claim-gate --candidate <id> --json",
        "do not run a BV claim command or local Cargo install as a\nworkaround",
        "stale-binary stop guidance",
    )?;
    assert_marker_before(
        "docs/agent_integration.md",
        AGENT_INTEGRATION_SOURCE,
        "ee swarm work-packet --workspace . --include-rch --claim-gate --candidate <id> --json",
        "must not claim Beads",
        "read-only consumer guidance",
    )?;

    Ok(())
}

#[test]
fn agents_md_pins_claim_gate_stale_binary_stop_condition() -> Result<(), String> {
    // Compared with whitespace COLLAPSED on both sides. These markers used to
    // embed the document's line wrapping (`"approved\n   RCH/release-path
    // rebuild"`), so reflowing a paragraph broke the assertion while the rule
    // it guards was untouched. Two of the five had rotted that way by
    // 2026-09-18 and neither was a real regression
    // (bd-integration-gm-six-red-concealed-uhp28).
    //
    // This is not a relaxation: the full wording is still required, in order,
    // as a contiguous run of words. Only the position of the line breaks stops
    // being part of the contract, and a line break was never the thing worth
    // pinning.
    let required_markers = [
        "if the installed `ee` rejects `--claim-gate` or `--candidate`",
        "stale relative to the current source/docs contract",
        "approved RCH/release-path rebuild",
        // The document says "run no BV claim command". The marker used to read
        // "do not run a BV claim command", which matched neither literally nor
        // collapsed -- a genuine wording change, not a wrap. Same rule, and
        // the marker now quotes the text that exists.
        "run no BV claim command",
        "do not rebuild or install `ee` locally with Cargo as a workaround",
    ];
    ensure_all_markers_present(
        "AGENTS.md claim-gate stale-binary stop markers",
        AGENTS_SOURCE,
        &required_markers,
    )?;

    assert_marker_before(
        "AGENTS.md",
        AGENTS_SOURCE,
        "ee swarm work-packet --workspace . --include-rch --claim-gate --candidate <id> --json",
        "br update <id> --status=in_progress --json",
        "typical agent flow claim guidance",
    )?;

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
    let required_markers = [
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
    ];
    // This test returns `()`, so the shared helper's Err is surfaced as a
    // panic here rather than with `?`. The point is the same: one failure
    // listing every missing marker, not the first one encountered.
    if let Err(report) =
        ensure_all_markers_present("matrix E2E log schema", INVENTORY, &required_markers)
    {
        panic!("{report}");
    }
}

/// Risky mock/stub surfaces the inventory must keep naming, as (surface, file).
///
/// KEYED ON (SURFACE, FILE) AND NEVER ON A LINE NUMBER -- bd-apvhh ruling C,
/// and bd-blj5n is the reason the rule exists.
///
/// This assertion used to pin eleven literal `file:line` strings. Eight of them
/// left the document on 2026-05-23 (fd9914c5f) when the anchors were updated,
/// so it had been red for roughly four months. Worse than merely stale: it
/// RATCHETED AGAINST ITS OWN REPAIR. The anchors it demanded were the drifted
/// positions, so correcting an anchor in the document deleted the exact string
/// this test required, and any honest fix of the documentation made the test
/// redder. A drift detector keyed on the thing that drifts cannot be satisfied
/// and repaired at the same time.
///
/// The surface NAME is the durable key. Line numbers move every time the file
/// above them changes; `src/cli/mod.rs` alone is ~98k lines and grows
/// continuously. A surface is retired or renamed deliberately, by someone
/// editing this table on purpose.
///
/// Measured 2026-09-18: all thirteen rows below are present. Add a row when a
/// risky surface is documented; remove one only when the surface itself is
/// gone, never to make a failure go away.
const RISKY_SURFACES: &[(&str, &str)] = &[
    ("Causal trace", "src/core/causal.rs"),
    ("Causal estimate", "src/core/causal.rs"),
    ("Procedure verify", "src/core/procedure.rs"),
    ("Rehearse run/inspect", "src/core/rehearse.rs"),
    ("Eval output renderers", "src/output/mod.rs"),
    ("Tripwire list/check", "src/core/tripwire.rs"),
    ("Preflight show", "src/core/preflight.rs"),
    ("Preflight run", "src/core/preflight.rs"),
    ("Situation show/explain", "src/core/situation.rs"),
    ("Situation compare/link", "src/core/situation.rs"),
    ("Certificate list/show/verify", "src/core/certificate.rs"),
    ("Economy reports/plans", "src/core/economy.rs"),
    ("Memory revise internal", "src/core/memory.rs"),
];

/// The `## Mock, Sample, Stub, Or Simulated Data Anchors` table rows.
fn risky_surface_rows(inventory: &str) -> Result<Vec<Vec<String>>, String> {
    let (_, after) = inventory
        .split_once("## Mock, Sample, Stub, Or Simulated Data Anchors")
        .ok_or_else(|| {
            "inventory must carry a `Mock, Sample, Stub, Or Simulated Data Anchors` section"
                .to_owned()
        })?;
    // Bounded by the next `## ` heading so a later section cannot be read as
    // risky-surface rows.
    let section = after.split("\n## ").next().unwrap_or(after);
    let rows = section
        .lines()
        .filter(|line| line.trim_start().starts_with('|'))
        .map(markdown_row_cells)
        .filter(|cells| cells.len() >= 2 && !cells[0].is_empty() && !cells[0].starts_with("---"))
        .collect::<Vec<_>>();
    if rows.len() < 2 {
        return Err(format!(
            "risky-surface section parsed {} row(s); the table must have a header and data rows, \
             so this is a parser or document problem rather than a passing test",
            rows.len()
        ));
    }
    Ok(rows)
}

#[test]
fn mechanical_boundary_inventory_names_mock_and_stub_surfaces() {
    let rows = match risky_surface_rows(INVENTORY) {
        Ok(rows) => rows,
        Err(error) => panic!("{error}"),
    };

    let mut missing = Vec::new();
    for (surface, file) in RISKY_SURFACES {
        let Some(row) = rows
            .iter()
            .find(|cells| cells[0].trim_matches('`').trim() == *surface)
        else {
            missing.push(format!(
                "no risky-surface row named `{surface}`; if the surface was renamed, rename it \
                 here too, and if it was retired, drop the entry"
            ));
            continue;
        };
        // The anchor cell must still point at the right module. The LINE inside
        // it is deliberately not checked -- that is the drift this test used to
        // encode (bd-blj5n).
        if !row[1].contains(file) {
            missing.push(format!(
                "risky-surface row `{surface}` no longer cites {file}; its anchor cell is {:?}",
                row[1]
            ));
        }
    }

    assert!(
        missing.is_empty(),
        "inventory stopped naming risky mock/stub surfaces:\n{}",
        missing.join("\n")
    );
}

#[test]
fn the_risky_surface_lookup_discriminates_instead_of_always_agreeing() {
    // A gate that only ever passes against today's document repeats the defect
    // one level up, so both arms run against fixtures.
    let rows = risky_surface_rows(INVENTORY).expect("section must parse");
    assert!(
        rows.iter()
            .any(|cells| cells[0].trim_matches('`').trim() == "Causal trace"),
        "a surface that IS present must be found, or the negative arm below proves nothing"
    );
    assert!(
        !rows
            .iter()
            .any(|cells| cells[0].trim_matches('`').trim() == "Surface That Does Not Exist"),
        "a surface that is absent must NOT be found"
    );

    // And the section must be bounded: the following section's rows must not
    // leak in, or a renamed surface could be 'found' in an unrelated table.
    assert!(
        !rows
            .iter()
            .any(|cells| cells[1].contains("Immediate Follow-Up")),
        "risky-surface parsing must stop at the next `## ` heading"
    );
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

fn backticked_tokens_with_prefix<'a>(cell: &'a str, prefix: &str) -> Vec<&'a str> {
    let mut tokens = Vec::new();
    let mut rest = cell;

    while let Some((_, after_open)) = rest.split_once('`') {
        let Some((token, after_close)) = after_open.split_once('`') else {
            break;
        };
        if token.starts_with(prefix) {
            tokens.push(token);
        }
        rest = after_close;
    }

    tokens
}

fn known_project_local_skill(path: &str) -> Option<&'static str> {
    KNOWN_PROJECT_LOCAL_SKILL_PATHS
        .iter()
        .find_map(|(known_path, source)| (*known_path == path).then_some(*source))
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

/// The first cell of a markdown table row, or `None` for a line that is not one.
///
/// bd-6hp3w: coverage is about a path HAVING A ROW, and the first cell is what
/// says so. Every later cell is prose, and a path named in prose was mentioned
/// while describing a different command.
fn first_cell_of(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    let body = trimmed.strip_prefix('|')?;
    // `split` rather than `split_once`: a row with a single trailing pipe still
    // yields its one cell instead of vanishing.
    body.split('|').next()
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

/// bd-o74n4: the twelve-column matrix must agree with the effect manifest.
///
/// The content tier checked that a command path HAS a row. Nothing checked
/// that the row says the TRUE thing, so 430 new rows could each contradict
/// `src/core/effect.rs` and every gate here would stay green. That gap was
/// not theoretical: reading two rows against the manifest found two wrong.
/// `install`/`update` named a path the CLI cannot emit and attributed an
/// atomic-replace write to a command declared `read_only` (f9e82e6da), and
/// `daemon` declared a writer "unavailable now" while the manifest gives it
/// three write tables and two workspace files (d66a67bf2).
///
/// READS THE BUILT MANIFEST, NOT THE SOURCE, and that is load-bearing.
/// `daemon` is constructed by `external_io_write` (AuditedMutation) and then
/// overrides `mutation_contract.side_effect_class` to `Mixed` afterwards
/// (src/core/effect.rs:1945). Any check that greps the constructor call reads
/// the wrong class for it. `EffectManifest::build()` cannot be wrong about
/// this in the way a regex can.
///
/// MULTI-PATH ROWS may declare `class=mixed`. That is not an escape hatch: a
/// row covering `context`, `pack`, `search` and `why` describes four paths
/// whose declared classes genuinely differ, and `mixed` is the vocabulary's
/// word for exactly that. A row covering paths that all share one class must
/// name that class, and a row covering a single path must match it exactly.
#[test]
fn matrix_row_classes_agree_with_the_effect_manifest() -> Result<(), String> {
    use ee::core::effect::EffectManifest;

    let manifest = EffectManifest::build();
    let commands = command_paths_from_extract_function(CLI_SOURCE)?;
    let rows = matrix_rows(INVENTORY)?;

    let mut checked_pairs = 0usize;
    let mut violations: Vec<String> = Vec::new();

    for row in rows.iter().skip(2) {
        let surface = row_cell(row, 0, "surface")?;
        let side_effect = row_cell(row, 7, "side-effect")?;
        let declared = side_effect_class(side_effect)?;

        let covered: Vec<&String> = commands
            .iter()
            .filter(|command| {
                let cell = format!("`{command}`");
                row.iter().any(|value| value.contains(&cell))
            })
            .collect();
        if covered.is_empty() {
            continue;
        }

        let mut manifest_classes: Vec<(&str, &'static str)> = Vec::new();
        for command in &covered {
            let effect = manifest.get(command.as_str()).ok_or_else(|| {
                format!("{surface}: matrix names `{command}`, absent from the effect manifest")
            })?;
            manifest_classes.push((
                command.as_str(),
                effect.mutation_contract.side_effect_class.as_str(),
            ));
            checked_pairs += 1;
        }

        // `as_str()` already yields the `class=...` token the row carries, so
        // the two sides are compared in the same vocabulary rather than
        // through a hand-written translation table. A translation table is
        // what made my first three attempts at this measurement disagree with
        // each other.
        let distinct: std::collections::BTreeSet<&str> =
            manifest_classes.iter().map(|(_, c)| *c).collect();
        let declared_token = format!("class={declared}");

        if covered.len() == 1 {
            let (command, class) = manifest_classes[0];
            if declared_token != class {
                violations.push(format!(
                    "{surface}: row says {declared_token} for `{command}`, manifest says {class}"
                ));
            }
        } else if declared != "mixed" {
            if distinct.len() > 1 {
                violations.push(format!(
                    "{surface}: row says {declared_token} but covers paths of differing declared \
                     classes {distinct:?}; use class=mixed or split the row"
                ));
            } else if let Some(only) = distinct.iter().next()
                && declared_token != *only
            {
                violations.push(format!(
                    "{surface}: row says {declared_token}, every covered path declares {only}"
                ));
            }
        }
    }

    // NON-VACUITY. Every branch above is keyed on a row covering at least one
    // command path; if the surface-cell format drifts, `covered` is empty
    // everywhere, the loop asserts nothing and this test passes having read
    // the document and checked none of it.
    //
    // The floor is DERIVED from the same `- Matrix enforcement floor:` line
    // the content tier parses, not a literal. An earlier version hardcoded 26
    // and described it as "the content tier's own floor". That was wrong twice
    // over: the declared floor is 24, and 26 was simply what this gate
    // happened to measure the night it was written, so raising the document
    // floor would have ratcheted the content tier while this guard sat at 26
    // forever.
    //
    // The two tiers count DIFFERENT UNITS and the comparison is still sound.
    // `matrix_enforced_command_paths` counts command PATHS carrying a row;
    // this counts (row, path) PAIRS, which is >= that, since a path credited
    // by two rows contributes two pairs. So pairs >= paths >= declared holds
    // by construction, and the assertion tracks the ratchet without pretending
    // to measure the same thing.
    let declared_floor = INVENTORY
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("- Matrix enforcement floor:")
                .and_then(|value| value.trim().parse::<usize>().ok())
        })
        .ok_or_else(|| {
            "inventory must declare `- Matrix enforcement floor: <n>`; this gate derives its \
             non-vacuity floor from the same line the content tier does"
                .to_owned()
        })?;
    if checked_pairs < declared_floor {
        return Err(format!(
            "this gate checked only {checked_pairs} (row, command path) pairs against a declared \
             matrix enforcement floor of {declared_floor}; every (row, path) pair counts at least \
             one enforced path, so falling below the floor means the surface-cell matcher has \
             stopped finding rows rather than the matrix having shrunk"
        ));
    }

    if violations.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "matrix rows contradict the effect manifest ({} of {checked_pairs} checked):\n{}",
            violations.len(),
            violations.join("\n")
        ))
    }
}

/// Collapse every run of whitespace to a single space.
///
/// Used by the AGENTS.md and README marker loops so a required phrase is
/// matched by its WORDS and not by where the document happens to wrap. Those
/// markers were written by pasting prose out of the documents, line breaks and
/// indentation included; reflowing a paragraph then broke assertions whose
/// subject had not changed at all. Three such markers had rotted that way by
/// 2026-09-18 (bd-integration-gm-six-red-concealed-uhp28).
///
/// It does NOT weaken the checks: the full phrase is still required, in order,
/// as one contiguous run of words. It removes only the line-break positions,
/// which were never the thing worth pinning.
fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Check EVERY marker and report all misses at once.
///
/// The three marker loops in this file used to be
/// `for required in [...] { if !source.contains(required) { return Err(..) } }`
/// which returns on the FIRST mismatch. Such a loop can report at most one
/// defect no matter how many exist, so fixing the marker it names tells you
/// nothing about what is behind it — and twice on 2026-09-18 something was:
/// `readme_pins_swarm_brief_operator_workflow` named `br ready --json`, and
/// once that was repaired it named `bv copy-paste claim command`, which had
/// been wrong since the day it was written
/// (bd-integration-gm-six-red-concealed-uhp28).
///
/// The cost is a dispatch per concealed marker, discovered one at a time. The
/// deeper problem is that an instrument which can only ever report one answer
/// is indistinguishable from an instrument that is right.
///
/// Whitespace is collapsed on both sides so a phrase is matched by its WORDS
/// and not by where the document happens to wrap; see [`collapse_whitespace`].
fn ensure_all_markers_present(
    source_name: &str,
    source: &str,
    markers: &[&str],
) -> Result<(), String> {
    let collapsed = collapse_whitespace(source);
    let missing: Vec<&str> = markers
        .iter()
        .copied()
        .filter(|marker| !collapsed.contains(&collapse_whitespace(marker)))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    Err(format!(
        "{source_name} is missing {} of {} required markers (ALL listed, not just the first):\n  `{}`",
        missing.len(),
        markers.len(),
        missing.join("`\n  `")
    ))
}
