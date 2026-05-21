//! Contract checks for graph-related CLI help surfaces.
//!
//! `bd-bife.14` requires every new graph-facing flag to be discoverable from
//! command help and backed by examples/docs. This contract keeps the current
//! Clap surfaces honest while the broader graph epic is still landing.

use clap::Parser;
use clap::error::ErrorKind;
use ee::cli::Cli;

type TestResult = Result<(), String>;

fn help_for(args: &[&str]) -> Result<String, String> {
    match Cli::try_parse_from(args) {
        Ok(_) => Err(format!("{} did not request help", args.join(" "))),
        Err(error) if error.kind() == ErrorKind::DisplayHelp => Ok(error.to_string()),
        Err(error) => Err(format!(
            "{} returned {:?} instead of help",
            args.join(" "),
            error.kind()
        )),
    }
}

fn assert_contains_all(haystack: &str, needles: &[&str], context: &str) -> TestResult {
    let missing: Vec<&str> = needles
        .iter()
        .copied()
        .filter(|needle| !haystack.contains(needle))
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!("{context} missing help entries: {missing:?}"))
    }
}

fn assert_contains_none(haystack: &str, needles: &[&str], context: &str) -> TestResult {
    let unexpected: Vec<&str> = needles
        .iter()
        .copied()
        .filter(|needle| haystack.contains(needle))
        .collect();
    if unexpected.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{context} unexpectedly documented entries: {unexpected:?}"
        ))
    }
}

fn read_graph_cli_reference() -> Result<String, String> {
    let reference_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("docs")
        .join("cli-reference")
        .join("graph-flags.md");
    std::fs::read_to_string(&reference_path)
        .map_err(|error| format!("failed to read {}: {error}", reference_path.display()))
}

fn graph_cli_reference_example_commands(reference: &str) -> Vec<(usize, String)> {
    let mut commands = Vec::new();
    let mut in_bash_fence = false;
    let mut current: Option<(usize, String)> = None;

    for (line_index, line) in reference.lines().enumerate() {
        let line_number = line_index + 1;
        let trimmed = line.trim();
        if trimmed == "```bash" {
            in_bash_fence = true;
            continue;
        }
        if in_bash_fence && trimmed == "```" {
            if let Some(command) = current.take() {
                commands.push(command);
            }
            in_bash_fence = false;
            continue;
        }
        if !in_bash_fence || trimmed.is_empty() {
            continue;
        }

        let continued = trimmed.ends_with('\\');
        let segment = trimmed.trim_end_matches('\\').trim_end();
        if segment.starts_with("ee ") {
            if let Some(command) = current.take() {
                commands.push(command);
            }
            current = Some((line_number, segment.to_owned()));
        } else if let Some((_, command)) = current.as_mut() {
            command.push(' ');
            command.push_str(segment);
        }

        if !continued && let Some(command) = current.take() {
            commands.push(command);
        }
    }

    commands
}

fn split_shell_words(command: &str) -> Result<Vec<String>, String> {
    #[derive(Copy, Clone, Eq, PartialEq)]
    enum Quote {
        None,
        Single,
        Double,
    }

    let mut args = Vec::new();
    let mut current = String::new();
    let mut quote = Quote::None;
    let mut chars = command.chars().peekable();

    while let Some(ch) = chars.next() {
        match quote {
            Quote::None => match ch {
                '\'' => quote = Quote::Single,
                '"' => quote = Quote::Double,
                '\\' => {
                    if let Some(next) = chars.next() {
                        current.push(next);
                    }
                }
                ch if ch.is_whitespace() => {
                    if !current.is_empty() {
                        args.push(std::mem::take(&mut current));
                    }
                }
                _ => current.push(ch),
            },
            Quote::Single => {
                if ch == '\'' {
                    quote = Quote::None;
                } else {
                    current.push(ch);
                }
            }
            Quote::Double => match ch {
                '"' => quote = Quote::None,
                '\\' => {
                    if let Some(next) = chars.next() {
                        current.push(next);
                    }
                }
                _ => current.push(ch),
            },
        }
    }

    if quote != Quote::None {
        return Err(format!("unterminated quote in `{command}`"));
    }
    if !current.is_empty() {
        args.push(current);
    }
    Ok(args)
}

#[test]
fn graph_pack_and_insights_flags_are_help_discoverable() -> TestResult {
    let context_help = help_for(&["ee", "context", "--help"])?;
    assert_contains_all(
        &context_help,
        &[
            "--profile",
            "--ppr-weight",
            "--pack-profile",
            "--resource-profile",
            "--explain-performance",
            "--explain",
            "--no-pack-dna",
            "--stream",
            "--no-coverage-fill",
            "--no-rendered-text",
            "--no-skipped",
            "--no-meta",
            "--include-tombstoned",
            "--relevance-floor",
        ],
        "ee context --help",
    )?;

    let pack_help = help_for(&["ee", "pack", "--help"])?;
    assert_contains_all(
        &pack_help,
        &[
            "--candidate-pool",
            "--speed",
            "--profile",
            "--pack-profile",
            "--resource-profile",
            "--explain-performance",
            "--no-coverage-fill",
            "--no-rendered-text",
            "--no-skipped",
            "--no-meta",
            "--coordination-snapshot",
            "--coordination-stale-after-ms",
            "--include-non-affecting-degradations",
            "--include-expired",
            "--include-future",
            "--include-stale",
        ],
        "ee pack --help",
    )?;

    let pack_build_help = help_for(&["ee", "pack", "build", "--help"])?;
    assert_contains_all(
        &pack_build_help,
        &[
            "--query-file",
            "--candidate-pool",
            "--speed",
            "--profile",
            "--pack-profile",
            "--resource-profile",
            "--explain-performance",
            "--no-coverage-fill",
            "--no-rendered-text",
            "--no-skipped",
            "--no-meta",
            "--coordination-snapshot",
            "--coordination-stale-after-ms",
            "--include-non-affecting-degradations",
            "--as-of",
            "--include-expired",
            "--include-future",
            "--include-stale",
        ],
        "ee pack build --help",
    )?;

    let pack_replay_help = help_for(&["ee", "pack", "replay", "--help"])?;
    assert_contains_all(
        &pack_replay_help,
        &["PACK_ID", "--database"],
        "ee pack replay --help",
    )?;

    let pack_diff_help = help_for(&["ee", "pack", "diff", "--help"])?;
    assert_contains_all(
        &pack_diff_help,
        &["PACK_A", "PACK_B", "--database"],
        "ee pack diff --help",
    )?;

    let search_help = help_for(&["ee", "search", "--help"])?;
    assert_contains_all(&search_help, &["--explain-performance"], "ee search --help")?;

    let insights_help = help_for(&["ee", "insights", "--help"])?;
    assert_contains_all(
        &insights_help,
        &["--section", "--explain", "--limit", "--offset"],
        "ee insights --help",
    )?;

    let why_help = help_for(&["ee", "why", "--help"])?;
    assert_contains_all(
        &why_help,
        &["--database", "--confidence-threshold", "--causal-explain"],
        "ee why --help",
    )?;

    let rule_provenance_help = help_for(&["ee", "rule", "provenance", "--help"])?;
    assert_contains_all(
        &rule_provenance_help,
        &["RULE_ID", "--database"],
        "ee rule provenance --help",
    )
}

#[test]
fn swarm_profile_and_host_readiness_flags_are_help_discoverable() -> TestResult {
    let swarm_brief_help = help_for(&["ee", "swarm", "brief", "--help"])?;
    assert_contains_all(
        &swarm_brief_help,
        &[
            "--sources",
            "--include-rch",
            "--agent-mail-snapshot",
            "--agent-inventory-only",
            "--max-recent-commits",
            "--command-timeout-ms",
            "--require-sources",
        ],
        "ee swarm brief --help",
    )?;

    let swarm_next_action_help = help_for(&["ee", "swarm", "next-action", "--help"])?;
    assert_contains_all(
        &swarm_next_action_help,
        &[
            "--sources",
            "--include-rch",
            "--agent-mail-snapshot",
            "--verifier-evidence",
            "--agent-inventory-only",
            "--max-recent-commits",
            "--command-timeout-ms",
            "--require-sources",
        ],
        "ee swarm next-action --help",
    )?;

    let diag_host_profile_help = help_for(&["ee", "diag", "host-profile", "--help"])?;
    assert_contains_all(
        &diag_host_profile_help,
        &["--full-paths"],
        "ee diag host-profile --help",
    )?;

    let profile_config_plan_help = help_for(&["ee", "profile", "config", "plan", "--help"])?;
    assert_contains_all(
        &profile_config_plan_help,
        &["--profile", "--config"],
        "ee profile config plan --help",
    )?;

    let profile_config_apply_help = help_for(&["ee", "profile", "config", "apply", "--help"])?;
    assert_contains_all(
        &profile_config_apply_help,
        &["--profile", "--config", "--dry-run"],
        "ee profile config apply --help",
    )
}

#[test]
fn graph_cli_reference_documents_swarm_profile_readiness_examples() -> TestResult {
    let reference = read_graph_cli_reference()?;

    assert_contains_all(
        &reference,
        &[
            "ee swarm brief --workspace .",
            "--sources git,beads,bv,agent-mail --require-sources --json",
            "ee swarm next-action --workspace . --sources default,host-profile",
            "--verifier-evidence proof.json --include-rch --json",
            "ee diag host-profile --workspace . --full-paths --json",
            "ee profile config plan --workspace . --profile swarm",
            "ee profile config apply --workspace . --profile portable",
            "`constrained`, `portable`, `workstation`, `swarm`",
        ],
        "docs/cli-reference/graph-flags.md",
    )
}

#[test]
fn graph_cli_reference_documents_hits_and_load_bearing_insights_examples() -> TestResult {
    let reference = read_graph_cli_reference()?;

    assert_contains_all(
        &reference,
        &[
            "ee insights --section hubs --workspace . --limit 5 --json",
            "ee insights --section authorities --workspace . --limit 5 --json",
            "ee insights --section loadBearingMemories --workspace . --limit 5 --json",
            "graph.feature.hits_profiles.enabled",
            "graph.feature.load_bearing.enabled",
            "ee config set graph.feature.hits_profiles.enabled true",
            "ee config set graph.feature.load_bearing.enabled true",
        ],
        "docs/cli-reference/graph-flags.md",
    )
}

#[test]
fn graph_cli_reference_documents_hits_centrality_examples() -> TestResult {
    let reference = read_graph_cli_reference()?;

    assert_contains_all(
        &reference,
        &[
            "`pagerank`, `betweenness`, `authority`, `hits-hubs`, `hits-authorities`",
            "ee graph centrality --workspace . --algorithm hits-hubs --limit 10 --json",
            "ee graph centrality --workspace . --algorithm hits-authorities",
            "--memory-id mem_release_policy --require-fresh --json",
        ],
        "docs/cli-reference/graph-flags.md",
    )
}

#[test]
fn load_bearing_override_is_documented_only_on_supported_curate_commands() -> TestResult {
    let reference = read_graph_cli_reference()?;
    let load_bearing_override = ["--allow-tombstone-load-bearing"];
    assert_contains_all(
        &reference,
        &[
            "--allow-tombstone-load-bearing",
            "ee curate tombstone mem_load_bearing_rule",
            "ee curate apply cand_retract_stale_rule",
        ],
        "docs/cli-reference/graph-flags.md load-bearing override section",
    )?;

    for (context, help) in [
        (
            "ee curate apply --help",
            help_for(&["ee", "curate", "apply", "--help"])?,
        ),
        (
            "ee curate tombstone --help",
            help_for(&["ee", "curate", "tombstone", "--help"])?,
        ),
    ] {
        assert_contains_all(&help, &load_bearing_override, context)?;
    }

    for (context, help) in [
        (
            "ee curate disposition --help",
            help_for(&["ee", "curate", "disposition", "--help"])?,
        ),
        (
            "ee insights --help",
            help_for(&["ee", "insights", "--help"])?,
        ),
        (
            "ee rule provenance --help",
            help_for(&["ee", "rule", "provenance", "--help"])?,
        ),
    ] {
        assert_contains_none(&help, &load_bearing_override, context)?;
    }

    let precedence = std::fs::read_to_string("docs/cli-reference/flag-precedence.md")
        .map_err(|error| format!("read docs/cli-reference/flag-precedence.md: {error}"))?;
    assert_contains_all(
        &precedence,
        &[
            "Load-bearing protection is the strongest read-side curation guard",
            "`--allow-tombstone-load-bearing` is the explicit override",
            "Graph/search include flags such as `--include-tombstoned` only change read",
        ],
        "docs/cli-reference/flag-precedence.md load-bearing precedence",
    )?;

    Ok(())
}

#[test]
fn documented_graph_cli_reference_examples_parse() -> TestResult {
    let reference = read_graph_cli_reference()?;
    let commands = graph_cli_reference_example_commands(&reference);
    if commands.is_empty() {
        return Err("docs/cli-reference/graph-flags.md has no fenced ee examples".to_owned());
    }

    for (line_number, command) in commands {
        let args = split_shell_words(&command)
            .map_err(|error| format!("graph-flags.md:{line_number}: {error}"))?;
        if args.first().map(String::as_str) != Some("ee") {
            return Err(format!(
                "graph-flags.md:{line_number}: expected an ee command, got `{command}`"
            ));
        }
        Cli::try_parse_from(args).map_err(|error| {
            format!(
                "graph-flags.md:{line_number}: documented example `{command}` failed to parse: {:?}",
                error.kind()
            )
        })?;
    }

    Ok(())
}

#[test]
fn graph_command_flags_are_help_discoverable() -> TestResult {
    let graph_help = help_for(&["ee", "graph", "--help"])?;
    assert_contains_all(
        &graph_help,
        &[
            "pagerank",
            "betweenness",
            "hits",
            "louvain",
            "communities",
            "k-core",
            "articulation",
            "path",
            "explain-link",
            "export",
            "snapshot",
            "centrality",
            "centrality-refresh",
            "feature-enrichment",
            "neighborhood",
        ],
        "ee graph --help",
    )?;

    for command in [
        "pagerank",
        "betweenness",
        "hits",
        "communities",
        "articulation",
    ] {
        let help = help_for(&["ee", "graph", command, "--help"])?;
        assert_contains_all(
            &help,
            &[
                "--database",
                "--min-weight",
                "--min-confidence",
                "--link-limit",
                "--limit",
                "--include-tombstoned",
            ],
            &format!("ee graph {command} --help"),
        )?;
    }

    let louvain_help = help_for(&["ee", "graph", "louvain", "--help"])?;
    assert_contains_all(
        &louvain_help,
        &[
            "--database",
            "--min-weight",
            "--min-confidence",
            "--link-limit",
            "--limit",
            "--resolution",
            "--threshold",
            "--max-level",
            "--seed",
        ],
        "ee graph louvain --help",
    )?;

    let k_core_help = help_for(&["ee", "graph", "k-core", "--help"])?;
    assert_contains_all(
        &k_core_help,
        &[
            "--database",
            "--min-weight",
            "--min-confidence",
            "--link-limit",
            "--k",
        ],
        "ee graph k-core --help",
    )?;

    for command in ["path", "explain-link"] {
        let help = help_for(&["ee", "graph", command, "--help"])?;
        assert_contains_all(
            &help,
            &[
                "--database",
                "--min-weight",
                "--min-confidence",
                "--link-limit",
            ],
            &format!("ee graph {command} --help"),
        )?;
    }

    let export_help = help_for(&["ee", "graph", "export", "--help"])?;
    assert_contains_all(
        &export_help,
        &[
            "--database",
            "--workspace-id",
            "--snapshot-id",
            "--graph-type",
            "--type",
        ],
        "ee graph export --help",
    )?;

    let centrality_help = help_for(&["ee", "graph", "centrality", "--help"])?;
    assert_contains_all(
        &centrality_help,
        &[
            "--database",
            "--algorithm",
            "--limit",
            "--memory-id",
            "--require-fresh",
        ],
        "ee graph centrality --help",
    )?;

    let centrality_refresh_help = help_for(&["ee", "graph", "centrality-refresh", "--help"])?;
    assert_contains_all(
        &centrality_refresh_help,
        &[
            "--database",
            "--dry-run",
            "--min-weight",
            "--min-confidence",
            "--link-limit",
        ],
        "ee graph centrality-refresh --help",
    )?;

    let snapshot_help = help_for(&["ee", "graph", "snapshot", "refresh", "--help"])?;
    assert_contains_all(
        &snapshot_help,
        &[
            "--database",
            "--dry-run",
            "--graph",
            "--min-weight",
            "--min-confidence",
            "--link-limit",
        ],
        "ee graph snapshot refresh --help",
    )?;

    let enrichment_help = help_for(&["ee", "graph", "feature-enrichment", "--help"])?;
    assert_contains_all(
        &enrichment_help,
        &[
            "--database",
            "--dry-run",
            "--min-weight",
            "--min-confidence",
            "--link-limit",
            "--max-features",
            "--min-combined-score",
            "--max-selection-boost",
        ],
        "ee graph feature-enrichment --help",
    )?;

    let neighborhood_help = help_for(&["ee", "graph", "neighborhood", "--help"])?;
    assert_contains_all(
        &neighborhood_help,
        &["--database", "--direction", "--relation", "--limit"],
        "ee graph neighborhood --help",
    )
}

#[test]
fn causal_command_flags_are_help_discoverable() -> TestResult {
    let trace_help = help_for(&["ee", "causal", "trace", "--help"])?;
    assert_contains_all(
        &trace_help,
        &[
            "FAILURE_MEMORY_ID",
            "--memory-id",
            "--run-id",
            "--pack-id",
            "--preflight-id",
            "--tripwire-id",
            "--procedure-id",
            "--agent-id",
            "--database",
            "--database-workspace-id",
            "--limit",
            "--depth",
            "--include-exposures",
            "--include-outcomes",
            "--dry-run",
        ],
        "ee causal trace --help",
    )?;

    let compare_help = help_for(&["ee", "causal", "compare", "--help"])?;
    assert_contains_all(
        &compare_help,
        &[
            "CHAIN_A",
            "CHAIN_B",
            "--fixture-replay-id",
            "--shadow-run-id",
            "--counterfactual-episode-id",
            "--experiment-id",
            "--artifact-id",
            "--decision-id",
            "--method",
            "--dry-run",
        ],
        "ee causal compare --help",
    )?;

    let estimate_help = help_for(&["ee", "causal", "estimate", "--help"])?;
    assert_contains_all(
        &estimate_help,
        &[
            "CHAIN_ID",
            "--artifact-id",
            "--decision-id",
            "--chain-id",
            "--agent-id",
            "--method",
            "--include-confounders",
            "--include-assumptions",
            "--dry-run",
        ],
        "ee causal estimate --help",
    )?;

    let promote_help = help_for(&["ee", "causal", "promote-plan", "--help"])?;
    assert_contains_all(
        &promote_help,
        &[
            "CHAIN_ID",
            "--artifact-id",
            "--decision-id",
            "--estimate-id",
            "--action",
            "--method",
            "--minimum-uplift",
            "--include-revalidation",
            "--include-narrower-routing",
            "--include-experiment-proposals",
            "--dry-run",
        ],
        "ee causal promote-plan --help",
    )
}

#[test]
fn diagnostic_graph_fixture_flags_are_help_discoverable() -> TestResult {
    let diag_graph_help = help_for(&["ee", "diag", "graph", "--help"])?;
    assert_contains_all(
        &diag_graph_help,
        &["Report graph module readiness"],
        "ee diag graph --help",
    )?;

    let causal_edge_help = help_for(&["ee", "diag", "causal-edge", "--help"])?;
    assert_contains_all(
        &causal_edge_help,
        &[
            "--database",
            "--workspace-id",
            "--edge-id",
            "--failure-id",
            "--candidate-cause-id",
            "--contribution-score",
            "--evidence-uri",
            "--computed-at",
            "--method",
        ],
        "ee diag causal-edge --help",
    )?;

    let graph_snapshot_help = help_for(&["ee", "diag", "graph-snapshot", "--help"])?;
    assert_contains_all(
        &graph_snapshot_help,
        &[
            "--database",
            "--status",
            "--metrics-json",
            "--node-count",
            "--edge-count",
            "--source-generation",
        ],
        "ee diag graph-snapshot --help",
    )
}

#[test]
fn proximity_health_and_maintenance_flags_are_help_discoverable() -> TestResult {
    let proximity_help = help_for(&["ee", "proximity", "--help"])?;
    assert_contains_all(
        &proximity_help,
        &[
            "--database",
            "--min-weight",
            "--min-confidence",
            "--link-limit",
            "--include-tombstoned",
        ],
        "ee proximity --help",
    )?;

    let health_help = help_for(&["ee", "health", "--help"])?;
    assert_contains_all(&health_help, &["--robot-insights"], "ee health --help")?;

    let status_help = help_for(&["ee", "status", "--help"])?;
    assert_contains_all(&status_help, &["--skyline"], "ee status --help")?;

    let backup_create_help = help_for(&["ee", "backup", "create", "--help"])?;
    assert_contains_all(
        &backup_create_help,
        &["--include-graph-cache"],
        "ee backup create --help",
    )?;

    let backup_restore_help = help_for(&["ee", "backup", "restore", "--help"])?;
    assert_contains_all(
        &backup_restore_help,
        &["BACKUP_ID_OR_PATH", "--side-path", "--skip-graph-cache"],
        "ee backup restore --help",
    )?;

    let curate_disposition_help = help_for(&["ee", "curate", "disposition", "--help"])?;
    assert_contains_all(
        &curate_disposition_help,
        &[
            "--database",
            "--actor",
            "--apply",
            "--no-structural-decay",
            "--now",
        ],
        "ee curate disposition --help",
    )?;

    let maintenance_help = help_for(&["ee", "maintenance", "run", "--help"])?;
    assert_contains_all(
        &maintenance_help,
        &[
            "--job",
            "--database",
            "--dry-run",
            "--include-decay",
            "--no-structural-decay",
            "--as-of",
            "--time-limit-ms",
            "--item-limit",
        ],
        "ee maintenance run --help",
    )?;

    let maintenance_status_help = help_for(&["ee", "maintenance", "status", "--help"])?;
    assert_contains_all(
        &maintenance_status_help,
        &["Report maintenance job availability"],
        "ee maintenance status --help",
    )?;

    let prune_help = help_for(&["ee", "maintenance", "graph-snapshot-prune", "--help"])?;
    assert_contains_all(
        &prune_help,
        &["--database", "--dry-run", "--time-limit-ms", "--item-limit"],
        "ee maintenance graph-snapshot-prune --help",
    )?;

    let witness_prune_help = help_for(&["ee", "maintenance", "graph-witnesses-prune", "--help"])?;
    assert_contains_all(
        &witness_prune_help,
        &[
            "--database",
            "--dry-run",
            "--retention-days",
            "--algorithm-ttl",
        ],
        "ee maintenance graph-witnesses-prune --help",
    )?;

    let wal_checkpoint_help = help_for(&["ee", "maintenance", "wal-checkpoint", "--help"])?;
    assert_contains_all(
        &wal_checkpoint_help,
        &["--database", "--mode", "--dry-run"],
        "ee maintenance wal-checkpoint --help",
    )?;

    let job_run_help = help_for(&["ee", "job", "run", "--help"])?;
    assert_contains_all(
        &job_run_help,
        &[
            "KIND",
            "--database",
            "--dry-run",
            "--time-limit-ms",
            "--item-limit",
        ],
        "ee job run --help",
    )?;

    let job_list_help = help_for(&["ee", "job", "list", "--help"])?;
    assert_contains_all(
        &job_list_help,
        &["--kind", "--since", "--limit"],
        "ee job list --help",
    )?;

    let job_show_help = help_for(&["ee", "job", "show", "--help"])?;
    assert_contains_all(&job_show_help, &["JOB_ID"], "ee job show --help")?;

    let migrate_help = help_for(&["ee", "migrate", "--help"])?;
    assert_contains_all(
        &migrate_help,
        &["status", "run", "shard-fanout"],
        "ee migrate --help",
    )?;

    let migrate_shard_fanout_help = help_for(&["ee", "migrate", "shard-fanout", "--help"])?;
    assert_contains_all(
        &migrate_shard_fanout_help,
        &["--database", "--shards-dir", "--dry-run"],
        "ee migrate shard-fanout --help",
    )
}

#[test]
fn documented_graph_flag_combinations_parse() -> TestResult {
    for args in [
        &[
            "ee", "--fields", "standard", "--cards", "summary", "--meta", "graph", "pagerank",
            "--limit", "5", "--json",
        ][..],
        &[
            "ee",
            "context",
            "prepare release",
            "--profile",
            "thorough",
            "--ppr-weight",
            "0.5",
            "--explain",
            "--explain-performance",
            "--no-pack-dna",
            "--json",
        ][..],
        &[
            "ee",
            "context",
            "prepare release",
            "--stream",
            "--format",
            "json",
        ][..],
        &[
            "ee",
            "context",
            "prepare release",
            "--ppr-weight",
            "0.4",
            "--include-tombstoned",
            "--explain",
            "--json",
        ][..],
        &[
            "ee",
            "pack",
            "build",
            "--query-file",
            "release.eeq.json",
            "--candidate-pool",
            "150",
            "--speed",
            "quality",
            "--profile",
            "thorough",
            "--pack-profile",
            "verbose",
            "--resource-profile",
            "swarm_heavy",
            "--explain-performance",
            "--no-coverage-fill=false",
            "--coordination-snapshot",
            "coordination.json",
            "--include-non-affecting-degradations",
            "--as-of",
            "2026-05-19T00:00:00Z",
            "--include-stale",
            "--json",
        ][..],
        &[
            "ee",
            "pack",
            "replay",
            "pack_release_prev",
            "--database",
            ".ee/ee.db",
            "--json",
        ][..],
        &[
            "ee",
            "pack",
            "diff",
            "pack_release_prev",
            "pack_release_next",
            "--json",
        ][..],
        &[
            "ee",
            "search",
            "release blockers",
            "--explain-performance",
            "--json",
        ][..],
        &[
            "ee",
            "insights",
            "--section",
            "proximityHotspots",
            "--limit",
            "5",
            "--offset",
            "1",
            "--json",
        ][..],
        &[
            "ee",
            "insights",
            "--section",
            "hubs",
            "--limit",
            "5",
            "--json",
        ][..],
        &[
            "ee",
            "insights",
            "--section",
            "authorities",
            "--limit",
            "5",
            "--json",
        ][..],
        &[
            "ee",
            "insights",
            "--section",
            "loadBearingMemories",
            "--limit",
            "5",
            "--json",
        ][..],
        &[
            "ee",
            "insights",
            "--explain",
            "mem_failed_release",
            "--limit",
            "5",
            "--json",
        ][..],
        &[
            "ee",
            "proximity",
            "mem_release_policy",
            "mem_rch_remote_required",
            "--min-weight",
            "0.4",
            "--min-confidence",
            "0.6",
            "--link-limit",
            "250",
            "--include-tombstoned",
            "--json",
        ][..],
        &["ee", "health", "--robot-insights", "--json"][..],
        &[
            "ee",
            "graph",
            "pagerank",
            "--min-weight",
            "0.2",
            "--min-confidence",
            "0.5",
            "--link-limit",
            "500",
            "--limit",
            "10",
            "--include-tombstoned",
            "--json",
        ][..],
        &[
            "ee",
            "graph",
            "betweenness",
            "--min-weight",
            "0.3",
            "--limit",
            "10",
            "--json",
        ][..],
        &[
            "ee",
            "graph",
            "hits",
            "--min-confidence",
            "0.6",
            "--limit",
            "10",
            "--json",
        ][..],
        &[
            "ee",
            "graph",
            "communities",
            "--link-limit",
            "500",
            "--limit",
            "5",
            "--json",
        ][..],
        &[
            "ee",
            "graph",
            "articulation",
            "--include-tombstoned",
            "--limit",
            "10",
            "--json",
        ][..],
        &[
            "ee",
            "graph",
            "louvain",
            "--resolution",
            "1.2",
            "--threshold",
            "0.000001",
            "--max-level",
            "4",
            "--seed",
            "42",
            "--limit",
            "5",
            "--json",
        ][..],
        &[
            "ee",
            "graph",
            "export",
            "--graph-type",
            "memory_links",
            "--workspace-id",
            "ws_release",
            "--snapshot-id",
            "snap_release",
            "--format",
            "mermaid",
        ][..],
        &[
            "ee",
            "graph",
            "export",
            "--type",
            "memory_links",
            "--workspace-id",
            "ws_release",
            "--snapshot-id",
            "snap_release",
            "--format",
            "mermaid",
        ][..],
        &[
            "ee",
            "graph",
            "centrality",
            "--algorithm",
            "pagerank",
            "--limit",
            "10",
            "--require-fresh",
            "--json",
        ][..],
        &[
            "ee",
            "graph",
            "centrality",
            "--algorithm",
            "hits-hubs",
            "--limit",
            "10",
            "--json",
        ][..],
        &[
            "ee",
            "graph",
            "centrality",
            "--algorithm",
            "hits-authorities",
            "--memory-id",
            "mem_release_policy",
            "--require-fresh",
            "--json",
        ][..],
        &[
            "ee",
            "graph",
            "centrality-refresh",
            "--dry-run",
            "--min-confidence",
            "0.6",
            "--json",
        ][..],
        &[
            "ee",
            "graph",
            "snapshot",
            "refresh",
            "--graph",
            "memory_links",
            "--dry-run",
            "--json",
        ][..],
        &[
            "ee",
            "graph",
            "k-core",
            "--k",
            "3",
            "--min-confidence",
            "0.6",
            "--json",
        ][..],
        &[
            "ee",
            "graph",
            "path",
            "mem_source",
            "mem_target",
            "--min-weight",
            "0.4",
            "--json",
        ][..],
        &[
            "ee",
            "graph",
            "explain-link",
            "mem_source",
            "mem_target",
            "--link-limit",
            "250",
            "--json",
        ][..],
        &[
            "ee",
            "graph",
            "feature-enrichment",
            "--dry-run",
            "--min-confidence",
            "0.6",
            "--max-features",
            "25",
            "--min-combined-score",
            "0.2",
            "--max-selection-boost",
            "0.4",
            "--json",
        ][..],
        &[
            "ee",
            "graph",
            "neighborhood",
            "mem_release_policy",
            "--direction",
            "incoming",
            "--relation",
            "supports",
            "--limit",
            "20",
            "--json",
        ][..],
        &[
            "ee",
            "backup",
            "create",
            "--label",
            "pre-refactor",
            "--include-graph-cache=false",
            "--dry-run",
            "--json",
        ][..],
        &[
            "ee",
            "backup",
            "restore",
            "bk_release",
            "--side-path",
            "./restore-check",
            "--skip-graph-cache",
            "--dry-run",
            "--json",
        ][..],
        &[
            "ee",
            "why",
            "mem_failed_release",
            "--causal-explain",
            "--confidence-threshold",
            "0.7",
            "--json",
        ][..],
        &[
            "ee",
            "causal",
            "trace",
            "mem_failed_release",
            "--memory-id",
            "mem_release_policy",
            "--run-id",
            "run_release",
            "--pack-id",
            "pack_release",
            "--preflight-id",
            "preflight_release",
            "--tripwire-id",
            "tripwire_release",
            "--procedure-id",
            "procedure_release",
            "--agent-id",
            "agent_release",
            "--database",
            ".ee/ee.db",
            "--database-workspace-id",
            "ws_release",
            "--limit",
            "5",
            "--depth",
            "3",
            "--include-exposures",
            "--include-outcomes",
            "--dry-run",
            "--json",
        ][..],
        &[
            "ee",
            "causal",
            "compare",
            "chain_baseline",
            "chain_candidate",
            "--fixture-replay-id",
            "fixture_release",
            "--shadow-run-id",
            "shadow_release",
            "--counterfactual-episode-id",
            "counterfactual_release",
            "--experiment-id",
            "experiment_release",
            "--artifact-id",
            "artifact_release",
            "--decision-id",
            "decision_release",
            "--method",
            "replay",
            "--dry-run",
            "--json",
        ][..],
        &[
            "ee",
            "causal",
            "estimate",
            "chain_release",
            "--artifact-id",
            "artifact_release",
            "--decision-id",
            "decision_release",
            "--chain-id",
            "chain_release",
            "--agent-id",
            "agent_release",
            "--method",
            "matching",
            "--include-confounders",
            "--include-assumptions",
            "--dry-run",
            "--json",
        ][..],
        &[
            "ee",
            "causal",
            "promote-plan",
            "chain_release",
            "--artifact-id",
            "artifact_release",
            "--decision-id",
            "decision_release",
            "--estimate-id",
            "estimate_release",
            "--action",
            "promote",
            "--method",
            "replay",
            "--minimum-uplift",
            "0.08",
            "--include-revalidation",
            "--include-narrower-routing",
            "--include-experiment-proposals",
            "--dry-run",
            "--json",
        ][..],
        &[
            "ee",
            "rule",
            "provenance",
            "rule_release_policy",
            "--database",
            ".ee/ee.db",
            "--json",
        ][..],
        &[
            "ee",
            "maintenance",
            "run",
            "--job",
            "decay_sweep",
            "--no-structural-decay",
            "--dry-run",
            "--json",
        ][..],
        &[
            "ee",
            "curate",
            "disposition",
            "--no-structural-decay",
            "--now",
            "2026-05-19T00:00:00Z",
            "--json",
        ][..],
        &[
            "ee",
            "job",
            "run",
            "centrality_refresh",
            "--dry-run",
            "--time-limit-ms",
            "500",
            "--item-limit",
            "25",
            "--json",
        ][..],
        &["ee", "maintenance", "status", "--json"][..],
        &[
            "ee",
            "job",
            "list",
            "--kind",
            "centrality_refresh",
            "--since",
            "2026-05-19T00:00:00Z",
            "--limit",
            "10",
            "--json",
        ][..],
        &["ee", "job", "show", "job_release", "--json"][..],
        &[
            "ee",
            "maintenance",
            "graph-snapshot-prune",
            "--dry-run",
            "--time-limit-ms",
            "500",
            "--item-limit",
            "25",
            "--json",
        ][..],
        &["ee", "status", "--skyline", "--json"][..],
        &[
            "ee",
            "maintenance",
            "graph-witnesses-prune",
            "--dry-run",
            "--retention-days",
            "30",
            "--algorithm-ttl",
            "pagerank=14",
            "--json",
        ][..],
        &[
            "ee",
            "maintenance",
            "wal-checkpoint",
            "--database",
            ".ee/ee.db",
            "--mode",
            "truncate",
            "--dry-run",
            "--json",
        ][..],
        &[
            "ee",
            "swarm",
            "brief",
            "--sources",
            "git,beads,bv,agent-mail",
            "--agent-mail-snapshot",
            "coordination.json",
            "--agent-inventory-only",
            "codex,claude",
            "--max-recent-commits",
            "4",
            "--command-timeout-ms",
            "750",
            "--require-sources",
            "--json",
        ][..],
        &[
            "ee",
            "swarm",
            "next-action",
            "--sources",
            "default,host-profile",
            "--include-rch",
            "--verifier-evidence",
            "proof.json",
            "--json",
        ][..],
        &["ee", "diag", "host-profile", "--full-paths", "--json"][..],
        &[
            "ee",
            "profile",
            "config",
            "plan",
            "--profile",
            "swarm",
            "--config",
            ".ee/config.toml",
            "--json",
        ][..],
        &[
            "ee",
            "profile",
            "config",
            "apply",
            "--profile",
            "portable",
            "--config",
            ".ee/config.toml",
            "--dry-run",
            "--json",
        ][..],
        &["ee", "diag", "graph", "--json"][..],
        &[
            "ee",
            "diag",
            "causal-edge",
            "--database",
            ".ee/ee.db",
            "--workspace-id",
            "ws_release",
            "--edge-id",
            "edge_release_failure",
            "--failure-id",
            "mem_failed_release",
            "--candidate-cause-id",
            "mem_missing_rch_proof",
            "--contribution-score",
            "0.8",
            "--evidence-uri",
            "file://proof.json",
            "--computed-at",
            "2026-05-19T00:00:00Z",
            "--method",
            "manual",
            "--json",
        ][..],
        &[
            "ee",
            "diag",
            "graph-snapshot",
            "--database",
            ".ee/ee.db",
            "--status",
            "stale",
            "--metrics-json",
            "{\"pagerank\":1}",
            "--node-count",
            "42",
            "--edge-count",
            "77",
            "--source-generation",
            "3",
            "--json",
        ][..],
        &[
            "ee",
            "migrate",
            "shard-fanout",
            "--database",
            ".ee/ee.db",
            "--shards-dir",
            "/tmp/ee-shards",
            "--dry-run",
            "--json",
        ][..],
    ] {
        Cli::try_parse_from(args)
            .map_err(|error| format!("{} failed to parse: {:?}", args.join(" "), error.kind()))?;
    }
    Ok(())
}
