use std::path::PathBuf;
use std::{env, fs};

type TestResult = Result<(), String>;

pub struct GoldenTest {
    name: String,
    category: String,
}

impl GoldenTest {
    #[must_use]
    pub fn new(category: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            category: category.into(),
        }
    }

    fn golden_path(&self) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("golden")
            .join(&self.category)
            .join(format!("{}.golden", self.name))
    }

    pub fn assert_eq(&self, actual: &str) -> TestResult {
        let update_mode = env::var("UPDATE_GOLDEN").is_ok();

        if update_mode {
            self.update_golden(actual)?;
            return Ok(());
        }

        let expected = self.load_golden()?;
        if actual == expected {
            Ok(())
        } else {
            Err(self.format_diff(&expected, actual))
        }
    }

    fn load_golden(&self) -> Result<String, String> {
        let path = self.golden_path();
        fs::read_to_string(&path).map_err(|error| {
            format!(
                "Golden file not found: {}\nRun with UPDATE_GOLDEN=1 to create it.\nError: {}",
                path.display(),
                error
            )
        })
    }

    fn update_golden(&self, content: &str) -> TestResult {
        let path = self.golden_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                format!(
                    "Failed to create golden directory {}: {}",
                    parent.display(),
                    error
                )
            })?;
        }
        fs::write(&path, content).map_err(|error| {
            format!("Failed to write golden file {}: {}", path.display(), error)
        })?;
        eprintln!("Updated golden file: {}", path.display());
        Ok(())
    }

    fn format_diff(&self, expected: &str, actual: &str) -> String {
        let expected_lines: Vec<&str> = expected.lines().collect();
        let actual_lines: Vec<&str> = actual.lines().collect();

        let mut diff = String::new();
        diff.push_str(&format!(
            "Golden test '{}::{}' failed.\n",
            self.category, self.name
        ));
        diff.push_str(&format!("Golden file: {}\n", self.golden_path().display()));
        diff.push_str("Run with UPDATE_GOLDEN=1 to update the golden file.\n\n");

        diff.push_str("--- expected\n");
        diff.push_str("+++ actual\n\n");

        let max_lines = expected_lines.len().max(actual_lines.len());
        for i in 0..max_lines {
            let exp = expected_lines.get(i);
            let act = actual_lines.get(i);
            match (exp, act) {
                (Some(e), Some(a)) if e == a => {
                    diff.push_str(&format!("  {}\n", e));
                }
                (Some(e), Some(a)) => {
                    diff.push_str(&format!("- {}\n", e));
                    diff.push_str(&format!("+ {}\n", a));
                }
                (Some(e), None) => {
                    diff.push_str(&format!("- {}\n", e));
                }
                (None, Some(a)) => {
                    diff.push_str(&format!("+ {}\n", a));
                }
                (None, None) => {}
            }
        }

        diff
    }
}

pub fn assert_golden(category: &str, name: &str, actual: &str) -> TestResult {
    GoldenTest::new(category, name).assert_eq(actual)
}

pub fn assert_json_golden(category: &str, name: &str, actual: &str) -> TestResult {
    let normalized = normalize_json_for_comparison(actual);
    GoldenTest::new(category, format!("{}.json", name)).assert_eq(&normalized)
}

fn normalize_json_for_comparison(json: &str) -> String {
    let mut normalized = json.trim().to_string();
    normalized.push('\n');
    normalized
}

#[cfg(test)]
mod tests {
    use super::*;
    use ee::core::audit::{
        AuditDiffReport, AuditShowReport, AuditTimelineEntry, AuditTimelineReport,
        AuditVerifyReport, LinkedSnapshot, TimelinePagination, VerificationIssue,
    };
    use ee::core::index::{IndexRebuildOptions, IndexRebuildStatus, rebuild_index};
    use ee::core::swarm_brief::{
        SWARM_BRIEF_REDACTION_STATUS, SWARM_BRIEF_SCHEMA_V1, SwarmBriefAgentInventorySummary,
        SwarmBriefBead, SwarmBriefBeadsDependencyCycleSummary, SwarmBriefBvPick,
        SwarmBriefBvSummary, SwarmBriefCommit, SwarmBriefDegradation, SwarmBriefDirtyFile,
        SwarmBriefFileReservation, SwarmBriefHostProfileSummary, SwarmBriefRecommendation,
        SwarmBriefReport, SwarmBriefResourcePressureHint, SwarmBriefSourceFreshness,
        SwarmBriefSourceKind, SwarmBriefSourceProvenance, SwarmBriefSourceSnapshot,
        SwarmBriefSourceStatus, apply_swarm_brief_advice, parse_agent_mail_snapshot_json,
        parse_beads_json, parse_git_log,
    };
    use ee::db::{
        CreateCurationCandidateInput, CreateMemoryInput, CreateMemoryLinkInput,
        CreateWorkspaceInput, DbConnection, MemoryLinkRelation, MemoryLinkSource,
    };
    use ee::models::{ProducerMetadata, WorkspaceId};
    use std::path::Path;
    use std::process::{Command, Output};
    use std::time::{SystemTime, UNIX_EPOCH};

    type TestResult = Result<(), String>;
    const DOCTOR_GOLDEN_WORKSPACE: &str = "tests/fixtures";

    fn ee_binary_path() -> Result<PathBuf, String> {
        let cargo_path = PathBuf::from(env!("CARGO_BIN_EXE_ee"));
        if cargo_path.exists() {
            return Ok(cargo_path);
        }

        let current_exe = env::current_exe()
            .map_err(|error| format!("failed to resolve current test binary: {error}"))?;
        let debug_dir = current_exe.parent().and_then(Path::parent).ok_or_else(|| {
            format!(
                "failed to resolve debug directory from test binary {}",
                current_exe.display()
            )
        })?;
        let sibling = debug_dir.join("ee");
        if sibling.exists() {
            Ok(sibling)
        } else {
            Err(format!(
                "ee binary not found at {} or {}",
                cargo_path.display(),
                sibling.display()
            ))
        }
    }

    fn run_ee(args: &[&str]) -> Result<Output, String> {
        Command::new(ee_binary_path()?)
            .args(args)
            .output()
            .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
    }

    fn unique_artifact_dir(prefix: &str) -> Result<PathBuf, String> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| format!("clock moved backwards: {error}"))?
            .as_nanos();
        Ok(PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("ee-golden-artifacts")
            .join(format!("{prefix}-{}-{now}", std::process::id())))
    }

    fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
        if condition {
            Ok(())
        } else {
            Err(message.into())
        }
    }

    fn ensure_equal<T>(actual: &T, expected: &T, context: &str) -> TestResult
    where
        T: std::fmt::Debug + PartialEq,
    {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{context}: expected {expected:?}, got {actual:?}"))
        }
    }

    fn ensure_json_number_close(
        actual: &serde_json::Value,
        expected: &serde_json::Value,
        tolerance: f64,
        context: &str,
    ) -> TestResult {
        let actual_number = actual
            .as_f64()
            .ok_or_else(|| format!("{context}: actual value must be numeric, got {actual:?}"))?;
        let expected_number = expected.as_f64().ok_or_else(|| {
            format!("{context}: expected value must be numeric, got {expected:?}")
        })?;

        ensure(
            (actual_number - expected_number).abs() <= tolerance,
            format!(
                "{context}: expected {expected_number:?} within {tolerance}, got {actual_number:?}"
            ),
        )
    }

    fn ensure_contains(haystack: &str, needle: &str, context: &str) -> TestResult {
        ensure(
            haystack.contains(needle),
            format!("{context}: expected to contain {needle:?}, got {haystack:?}"),
        )
    }

    fn swarm_source_ready(
        source: SwarmBriefSourceKind,
        command: Option<(&'static str, &'static [&'static str])>,
        item_count: usize,
    ) -> SwarmBriefSourceSnapshot {
        let provenance = match command {
            Some((program, args)) => SwarmBriefSourceProvenance::command(program, args),
            None => SwarmBriefSourceProvenance::local_probe(),
        };
        SwarmBriefSourceSnapshot::ready(source, provenance, item_count)
    }

    fn swarm_source_degraded(
        source: SwarmBriefSourceKind,
        code: &str,
        message: &str,
        repair: &str,
    ) -> SwarmBriefSourceSnapshot {
        SwarmBriefSourceSnapshot {
            source,
            status: SwarmBriefSourceStatus::Degraded,
            freshness: SwarmBriefSourceFreshness::unknown(),
            provenance: SwarmBriefSourceProvenance::local_probe(),
            item_count: 0,
            degraded: vec![SwarmBriefDegradation::warning(
                source,
                code,
                message,
                Some(repair.to_string()),
            )],
        }
    }

    fn swarm_source_degraded_with_freshness(
        source: SwarmBriefSourceKind,
        code: &str,
        freshness_state: &'static str,
        message: &str,
        repair: &str,
    ) -> SwarmBriefSourceSnapshot {
        let mut snapshot = swarm_source_degraded(source, code, message, repair);
        snapshot.freshness = SwarmBriefSourceFreshness {
            observed_at: Some("2026-05-14T05:20:52Z".to_string()),
            age_seconds: None,
            stale_after_seconds: None,
            state: freshness_state,
        };
        snapshot
    }

    fn swarm_source_unavailable(
        source: SwarmBriefSourceKind,
        code: &str,
        message: &str,
        repair: &str,
    ) -> SwarmBriefSourceSnapshot {
        SwarmBriefSourceSnapshot::unavailable(
            source,
            SwarmBriefSourceProvenance::local_probe(),
            SwarmBriefDegradation::warning(source, code, message, Some(repair.to_string())),
        )
    }

    fn swarm_bead(id: &str, title: &str, status: &str) -> SwarmBriefBead {
        SwarmBriefBead {
            id: id.to_string(),
            title: title.to_string(),
            status: status.to_string(),
            priority: Some(1),
            assignee: None,
            source_bucket: status.to_string(),
        }
    }

    fn base_swarm_brief_report() -> SwarmBriefReport {
        let mut report = SwarmBriefReport::empty(Path::new("."));
        report.sources = vec![
            swarm_source_ready(SwarmBriefSourceKind::AgentInventory, None, 1),
            swarm_source_ready(SwarmBriefSourceKind::AgentMail, None, 1),
            swarm_source_ready(
                SwarmBriefSourceKind::Beads,
                Some(("br", &["ready", "--json"])),
                1,
            ),
            swarm_source_ready(
                SwarmBriefSourceKind::Bv,
                Some(("bv", &["--robot-triage", "--robot-triage-by-track"])),
                1,
            ),
            swarm_source_ready(
                SwarmBriefSourceKind::Git,
                Some((
                    "git",
                    &["status", "--short", "--branch", "--untracked-files=all"],
                )),
                1,
            ),
            swarm_source_ready(SwarmBriefSourceKind::HostProfile, None, 1),
            swarm_source_ready(
                SwarmBriefSourceKind::Rch,
                Some(("rch", &["status", "--json"])),
                1,
            ),
        ];
        report.host_profile = Some(SwarmBriefHostProfileSummary {
            recommended_profile: "workstation".to_string(),
            confidence: "high".to_string(),
            logical_cores: Some(16),
            memory_total_bytes: Some(64 * 1024 * 1024 * 1024),
            memory_available_bytes: Some(48 * 1024 * 1024 * 1024),
            rch_hint_configured: true,
        });
        report.agent_inventory = Some(SwarmBriefAgentInventorySummary {
            status: "ready".to_string(),
            detected_count: 1,
            total_count: 1,
        });
        report
    }

    fn finalize_swarm_brief_case(mut report: SwarmBriefReport) -> SwarmBriefReport {
        apply_swarm_brief_advice(&mut report);
        report.finalize();
        report
    }

    fn replace_swarm_source(report: &mut SwarmBriefReport, replacement: SwarmBriefSourceSnapshot) {
        report
            .sources
            .retain(|source| source.source != replacement.source);
        report.sources.push(replacement);
    }

    fn swarm_recommendation_case_json(
        recommendation: &SwarmBriefRecommendation,
    ) -> serde_json::Value {
        serde_json::json!({
            "id": recommendation.id,
            "kind": recommendation.kind,
            "severity": recommendation.severity,
            "confidence": recommendation.confidence,
            "reasonCodes": recommendation.reason_codes,
            "evidence": recommendation.evidence,
            "suggestedCommands": recommendation.suggested_commands,
            "mustNotDo": recommendation.must_not_do,
        })
    }

    fn swarm_brief_contract_case(
        name: &str,
        report: SwarmBriefReport,
    ) -> Result<serde_json::Value, String> {
        let report = finalize_swarm_brief_case(report);
        let degraded_codes = report
            .degraded
            .iter()
            .map(|degradation| degradation.code.clone())
            .collect::<Vec<_>>();
        ensure_sorted_strings(&degraded_codes, &format!("{name} degraded code ordering"))?;
        let recommendation_ids = report
            .recommendations
            .iter()
            .map(|recommendation| recommendation.id.clone())
            .collect::<Vec<_>>();
        ensure_sorted_strings(
            &recommendation_ids,
            &format!("{name} recommendation id ordering"),
        )?;

        let mut case = serde_json::json!({
            "case": name,
            "schema": report.schema,
            "workspace": report.workspace,
            "redactionStatus": report.redaction_status,
            "sources": report.sources.iter().map(|source| {
                serde_json::json!({
                    "source": source.source.as_str(),
                    "status": source.status.as_str(),
                    "freshness": source.freshness.state,
                    "itemCount": source.item_count,
                    "command": source.provenance.command,
                    "sideEffectFree": source.provenance.side_effect_free,
                    "redaction": source.provenance.redaction,
                    "degradedCodes": source.degraded.iter().map(|item| item.code.clone()).collect::<Vec<_>>(),
                })
            }).collect::<Vec<_>>(),
            "degraded": report.degraded.iter().map(|degradation| {
                serde_json::json!({
                    "source": degradation.source.as_str(),
                    "code": degradation.code,
                    "severity": degradation.severity,
                    "message": degradation.message,
                    "repair": degradation.repair,
                })
            }).collect::<Vec<_>>(),
            "fileSurfaceRisks": report.file_surface_risks.iter().map(|risk| {
                serde_json::json!({
                    "pathPattern": risk.path_pattern,
                    "gitStatusBuckets": risk.git_status_buckets,
                    "reservationHolders": risk.reservation_holders,
                    "relatedBeadIds": risk.related_bead_ids,
                    "severity": risk.severity,
                    "score": risk.score,
                    "riskFactors": risk.risk_factors,
                    "evidence": risk.evidence,
                    "suggestedCommands": risk.suggested_commands,
                })
            }).collect::<Vec<_>>(),
            "recommendations": report
                .recommendations
                .iter()
                .map(swarm_recommendation_case_json)
                .collect::<Vec<_>>(),
        });
        if let Some(cycles) = &report.beads.dependency_cycle_summary {
            case.as_object_mut()
                .ok_or_else(|| format!("{name} contract case must serialize to object"))?
                .insert(
                    "beadsDependencyCycleSummary".to_string(),
                    serde_json::json!({
                        "count": cycles.count,
                        "examples": &cycles.examples,
                    }),
                );
        }
        Ok(case)
    }

    fn ensure_sorted_strings(values: &[String], context: &str) -> TestResult {
        ensure(
            values.windows(2).all(|window| window[0] <= window[1]),
            format!("{context}: expected sorted values, got {values:?}"),
        )
    }

    fn swarm_brief_contract_cases() -> Result<Vec<serde_json::Value>, String> {
        let mut all_sources = base_swarm_brief_report();
        all_sources.beads.ready.push(swarm_bead(
            "eidetic_engine_cli-pdav",
            "[swarm-brief][contracts] Freeze schema and goldens",
            "ready",
        ));
        all_sources.bv = Some(SwarmBriefBvSummary {
            actionable_count: Some(1),
            blocked_count: Some(0),
            in_progress_count: Some(0),
            track_count: Some(1),
            top_picks: vec![SwarmBriefBvPick {
                id: "eidetic_engine_cli-pdav".to_string(),
                title: "[swarm-brief][contracts] Freeze schema and goldens".to_string(),
                score_milli: Some(970),
            }],
        });
        all_sources.recent_commits.push(SwarmBriefCommit {
            hash: "aaaaaaaaaaaa".to_string(),
            authored_at_epoch_seconds: Some(1_778_351_000),
            subject: "implement swarm brief contract".to_string(),
        });

        let no_ready_work = base_swarm_brief_report();

        let mut reservation_conflict = base_swarm_brief_report();
        reservation_conflict.beads.ready.push(swarm_bead(
            "eidetic_engine_cli-u7r5",
            "[swarm-brief][advisor] Add non-overlap recommendations",
            "ready",
        ));
        reservation_conflict
            .file_reservations
            .push(SwarmBriefFileReservation {
                path_pattern: "src/core/swarm_brief.rs".to_string(),
                holder: "IndigoBrook".to_string(),
                exclusive: true,
                expires_at: Some("2026-05-09T20:00:00Z".to_string()),
            });

        let mut dirty_conflict = base_swarm_brief_report();
        dirty_conflict.beads.ready.push(swarm_bead(
            "eidetic_engine_cli-u7r5",
            "[swarm-brief][advisor] Add non-overlap recommendations",
            "ready",
        ));
        dirty_conflict.dirty_files.push(SwarmBriefDirtyFile {
            path: "src/core/swarm_brief.rs".to_string(),
            status: "M".to_string(),
        });

        let mut stale_in_progress = base_swarm_brief_report();
        stale_in_progress.beads.in_progress.push(swarm_bead(
            "eidetic_engine_cli-w0xy",
            "[vision-coverage][graph] Implement graph centrality and graph refresh surfaces",
            "in_progress",
        ));

        let mut bv_unavailable = base_swarm_brief_report();
        replace_swarm_source(
            &mut bv_unavailable,
            swarm_source_unavailable(
                SwarmBriefSourceKind::Bv,
                "bv_unavailable",
                "bv --robot-triage timed out before returning graph-aware recommendations.",
                "bv --robot-triage --robot-triage-by-track",
            ),
        );

        let mut agent_mail_unavailable = base_swarm_brief_report();
        replace_swarm_source(
            &mut agent_mail_unavailable,
            swarm_source_unavailable(
                SwarmBriefSourceKind::AgentMail,
                "agent_mail_unavailable",
                "Agent Mail snapshot was unavailable, so reservations and unread mail are unknown.",
                "Provide --agent-mail-snapshot with a redacted snapshot file.",
            ),
        );

        let mut beads_stale_locked = base_swarm_brief_report();
        replace_swarm_source(
            &mut beads_stale_locked,
            swarm_source_degraded(
                SwarmBriefSourceKind::Beads,
                "beads_unavailable",
                "Beads JSON was stale or locked during read-only collection.",
                "br ready --json",
            ),
        );

        let mut beads_tracker_stale = base_swarm_brief_report();
        replace_swarm_source(
            &mut beads_tracker_stale,
            swarm_source_degraded_with_freshness(
                SwarmBriefSourceKind::Beads,
                "beads_tracker_stale",
                "stale",
                "Beads JSONL is newer than the local database; bucket reads may lag coordination history.",
                "br sync --import-only",
            ),
        );

        let mut beads_db_newer = base_swarm_brief_report();
        replace_swarm_source(
            &mut beads_db_newer,
            swarm_source_degraded_with_freshness(
                SwarmBriefSourceKind::Beads,
                "beads_tracker_stale",
                "stale",
                "Beads database is newer than JSONL; exported tracker files may lag coordination history.",
                "br sync --flush-only",
            ),
        );

        let mut beads_dependency_cycles = base_swarm_brief_report();
        replace_swarm_source(
            &mut beads_dependency_cycles,
            swarm_source_ready(
                SwarmBriefSourceKind::Beads,
                Some(("br", &["ready", "--json"])),
                3,
            ),
        );
        beads_dependency_cycles.beads.dependency_cycle_summary =
            Some(SwarmBriefBeadsDependencyCycleSummary {
                count: 2,
                examples: vec![
                    vec![
                        "bd-7g8".to_string(),
                        "bd-uvcc".to_string(),
                        "bd-7g8".to_string(),
                    ],
                    vec![
                        "bd-lp4p".to_string(),
                        "bd-z6kq".to_string(),
                        "bd-lp4p".to_string(),
                    ],
                ],
            });

        let mut rch_unavailable = base_swarm_brief_report();
        replace_swarm_source(
            &mut rch_unavailable,
            swarm_source_unavailable(
                SwarmBriefSourceKind::Rch,
                "rch_unavailable",
                "RCH status was unavailable, so remote build pressure is unknown.",
                "rch status --json",
            ),
        );

        let mut rch_worker_topology_blocked = base_swarm_brief_report();
        replace_swarm_source(
            &mut rch_worker_topology_blocked,
            swarm_source_unavailable(
                SwarmBriefSourceKind::Rch,
                "rch_worker_topology_blocked",
                "RCH-E327 worker topology blocked remote-required verification; selected worker: css; root metadata redacted; remote workers may be visible but this workspace cannot be mapped.",
                "Inspect RCH worker path mapping; remote workers are visible but this workspace cannot be mapped.",
            ),
        );

        let mut rch_remote_required_fallback_prevented = base_swarm_brief_report();
        replace_swarm_source(
            &mut rch_remote_required_fallback_prevented,
            swarm_source_unavailable(
                SwarmBriefSourceKind::Rch,
                "rch_remote_required_fallback_prevented",
                "RCH_REQUIRE_REMOTE prevented local fallback, so this Cargo gate has no valid remote evidence.",
                "Fix remote worker availability or unset the remote-required guard only with explicit approval.",
            ),
        );

        let mut high_resource_pressure = base_swarm_brief_report();
        high_resource_pressure
            .resource_pressure
            .push(SwarmBriefResourcePressureHint {
                source: SwarmBriefSourceKind::Rch,
                level: "high".to_string(),
                message: "rch queue depth: 9".to_string(),
            });
        high_resource_pressure.host_profile = Some(SwarmBriefHostProfileSummary {
            recommended_profile: "constrained".to_string(),
            confidence: "high".to_string(),
            logical_cores: Some(2),
            memory_total_bytes: Some(4 * 1024 * 1024 * 1024),
            memory_available_bytes: Some(512 * 1024 * 1024),
            rch_hint_configured: true,
        });

        let mut workspace_ambiguity = base_swarm_brief_report();
        workspace_ambiguity.workspace = "ambiguous-workspace".to_string();
        replace_swarm_source(
            &mut workspace_ambiguity,
            swarm_source_unavailable(
                SwarmBriefSourceKind::Git,
                "workspace_ambiguous",
                "Workspace selection matched multiple candidate roots.",
                "Pass an explicit --workspace path.",
            ),
        );

        let mut secret_redaction = base_swarm_brief_report();
        secret_redaction.beads.ready = parse_beads_json(
            r#"[{"id":"eidetic_engine_cli-secret","title":"Use token ghp_abcdefghijklmnopqrstuvwxyz1234567890 in swarm brief","status":"open","priority":1}]"#,
            "ready",
        )?;
        let mail_snapshot = parse_agent_mail_snapshot_json(
            r#"{
                "file_reservations": [
                    {"path_pattern":"src/core/swarm_brief.rs","holder":"Agent ghp_abcdefghijklmnopqrstuvwxyz1234567890","exclusive":true,"expires_ts":"2026-05-09T20:00:00Z"}
                ],
                "threads": [
                    {"thread_id":"eidetic_engine_cli-secret","subject":"token ghp_abcdefghijklmnopqrstuvwxyz1234567890","message_count":2,"body_md":"raw secret body"}
                ]
            }"#,
        )?;
        secret_redaction.file_reservations = mail_snapshot.file_reservations;
        secret_redaction.threads = mail_snapshot.threads;
        secret_redaction.recent_commits = parse_git_log(
            "bbbbbbbbbbbbbbbb\x1f1778352000\x1favoid token ghp_abcdefghijklmnopqrstuvwxyz1234567890\n",
        );

        Ok(vec![
            swarm_brief_contract_case("all_sources_available", all_sources)?,
            swarm_brief_contract_case("no_ready_work", no_ready_work)?,
            swarm_brief_contract_case("active_reservation_conflict", reservation_conflict)?,
            swarm_brief_contract_case("dirty_worktree_conflict", dirty_conflict)?,
            swarm_brief_contract_case("stale_in_progress_bead", stale_in_progress)?,
            swarm_brief_contract_case("bv_unavailable", bv_unavailable)?,
            swarm_brief_contract_case("agent_mail_unavailable", agent_mail_unavailable)?,
            swarm_brief_contract_case("beads_stale_locked", beads_stale_locked)?,
            swarm_brief_contract_case("rch_unavailable", rch_unavailable)?,
            swarm_brief_contract_case("beads_tracker_stale", beads_tracker_stale)?,
            swarm_brief_contract_case("beads_db_newer", beads_db_newer)?,
            swarm_brief_contract_case("beads_dependency_cycles", beads_dependency_cycles)?,
            swarm_brief_contract_case("rch_worker_topology_blocked", rch_worker_topology_blocked)?,
            swarm_brief_contract_case(
                "rch_remote_required_fallback_prevented",
                rch_remote_required_fallback_prevented,
            )?,
            swarm_brief_contract_case("high_resource_pressure", high_resource_pressure)?,
            swarm_brief_contract_case("workspace_ambiguity", workspace_ambiguity)?,
            swarm_brief_contract_case("secret_redaction", secret_redaction)?,
        ])
    }

    #[test]
    fn swarm_brief_contract_matrix_matches_golden() -> TestResult {
        let cases = swarm_brief_contract_cases()?;
        ensure_equal(&cases.len(), &17, "swarm brief contract case count")?;
        let matrix = serde_json::json!({
            "schema": "ee.swarm.brief.contract_matrix.v1",
            "payloadSchema": SWARM_BRIEF_SCHEMA_V1,
            "redactionStatus": SWARM_BRIEF_REDACTION_STATUS,
            "cases": cases,
        });
        let pretty = serde_json::to_string_pretty(&matrix)
            .map_err(|error| format!("failed to serialize swarm brief matrix: {error}"))?
            + "\n";

        ensure(
            !pretty.contains("ghp_"),
            "swarm brief golden must not expose GitHub-like tokens",
        )?;
        ensure(
            !pretty.contains("body_md") && !pretty.contains("raw secret body"),
            "swarm brief golden must not expose Agent Mail bodies",
        )?;
        assert_golden("swarm", "brief_contract_matrix.json", &pretty)
    }

    fn compute_stable_workspace_id(path: &Path) -> String {
        let hash = blake3::hash(format!("workspace:{}", path.to_string_lossy()).as_bytes());
        let mut bytes = [0_u8; 16];
        for (target, source) in bytes.iter_mut().zip(hash.as_bytes().iter().copied()) {
            *target = source;
        }
        WorkspaceId::from_uuid(uuid::Uuid::from_bytes(bytes)).to_string()
    }

    fn audit_entry(
        id: &str,
        timestamp: &str,
        surface: &str,
        mutation_kind: &str,
        prev_row_hash: Option<&str>,
        this_row_hash: &str,
    ) -> AuditTimelineEntry {
        AuditTimelineEntry {
            id: id.to_owned(),
            timestamp: timestamp.to_owned(),
            actor: Some("cod_2".to_owned()),
            surface: surface.to_owned(),
            mutation_kind: mutation_kind.to_owned(),
            before_hash: None,
            after_hash: Some(format!("blake3:{id}:after")),
            prev_row_hash: prev_row_hash.map(str::to_owned),
            this_row_hash: Some(this_row_hash.to_owned()),
            workspace_id: Some("wsp_auditgolden0000000000001".to_owned()),
            target_type: Some(surface.to_owned()),
            target_id: Some(format!("{surface}_auditgolden0000000001")),
            producer: ProducerMetadata::audit_actor(Some("cod_2"), Some(timestamp)),
            details: Some(serde_json::json!({ "source": "golden" })),
        }
    }

    fn assert_agent_stdout_golden(args: &[&str], name: &str, expect_success: bool) -> TestResult {
        let output = run_ee(args)?;
        let stdout = String::from_utf8(output.stdout)
            .map_err(|error| format!("stdout was not UTF-8 for ee {}: {error}", args.join(" ")))?;
        let stderr = String::from_utf8(output.stderr)
            .map_err(|error| format!("stderr was not UTF-8 for ee {}: {error}", args.join(" ")))?;

        ensure(
            output.status.success() == expect_success,
            format!(
                "ee {} exit status mismatch: got {:?}, stderr: {stderr}",
                args.join(" "),
                output.status.code()
            ),
        )?;
        ensure(
            stderr.is_empty(),
            format!("ee {} must keep diagnostics out of stderr", args.join(" ")),
        )?;
        ensure(
            stdout.starts_with('{'),
            format!("ee {} stdout must start with JSON data", args.join(" ")),
        )?;
        ensure(
            stdout.ends_with('\n'),
            format!("ee {} stdout must end with a newline", args.join(" ")),
        )?;

        assert_golden("agent", name, &stdout)
    }

    fn run_json_stdout(args: &[&str], expect_success: bool) -> Result<serde_json::Value, String> {
        let output = run_ee(args)?;
        let stdout = String::from_utf8(output.stdout)
            .map_err(|error| format!("stdout was not UTF-8 for ee {}: {error}", args.join(" ")))?;
        let stderr = String::from_utf8(output.stderr)
            .map_err(|error| format!("stderr was not UTF-8 for ee {}: {error}", args.join(" ")))?;

        ensure(
            output.status.success() == expect_success,
            format!(
                "ee {} exit status mismatch: got {:?}, stderr: {stderr}",
                args.join(" "),
                output.status.code()
            ),
        )?;
        ensure(
            stderr.is_empty(),
            format!("ee {} must keep diagnostics out of stderr", args.join(" ")),
        )?;
        ensure(
            stdout.starts_with('{'),
            format!("ee {} stdout must start with JSON data", args.join(" ")),
        )?;
        ensure(
            stdout.ends_with('\n'),
            format!("ee {} stdout must end with a newline", args.join(" ")),
        )?;

        serde_json::from_str(&stdout)
            .map_err(|error| format!("ee {} stdout must be JSON: {error}", args.join(" ")))
    }

    fn seed_search_workspace(workspace: &Path, database: &Path) -> TestResult {
        if let Some(parent) = database.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                format!(
                    "failed to create database parent {}: {error}",
                    parent.display()
                )
            })?;
        }

        let connection = DbConnection::open_file(database).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                "wsp_searchjson0000000000000001",
                &CreateWorkspaceInput {
                    path: workspace.to_string_lossy().into_owned(),
                    name: Some("search-json-contract".to_string()),
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_memory(
                "mem_00000000000000000000000001",
                &CreateMemoryInput {
                    workspace_id: "wsp_searchjson0000000000000001".to_string(),
                    level: "procedural".to_string(),
                    kind: "rule".to_string(),
                    content: "Run cargo fmt --check before release.".to_string(),
                    workflow_id: None,
                    confidence: 0.92,
                    utility: 0.8,
                    importance: 0.7,
                    provenance_uri: Some("file://AGENTS.md#L164-173".to_string()),
                    trust_class: "human_explicit".to_string(),
                    trust_subclass: Some("project-rule".to_string()),
                    valid_from: None,
                    valid_to: None,
                    tags: vec!["cargo".to_string(), "formatting".to_string()],
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .execute_raw(
                "UPDATE memories SET created_at = '2026-04-29T12:00:00+00:00', updated_at = '2026-04-29T12:00:00+00:00' WHERE id = 'mem_00000000000000000000000001'",
            )
            .map_err(|error| error.to_string())?;
        connection.close().map_err(|error| error.to_string())
    }

    fn seed_graph_query_workspace(workspace: &Path, database: &Path) -> TestResult {
        if let Some(parent) = database.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                format!(
                    "failed to create database parent {}: {error}",
                    parent.display()
                )
            })?;
        }

        let connection = DbConnection::open_file(database).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                "wsp_querygraph0000000000000001",
                &CreateWorkspaceInput {
                    path: workspace.to_string_lossy().into_owned(),
                    name: Some("query-graph-contract".to_string()),
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_memory(
                "mem_00000000000000000000000101",
                &CreateMemoryInput {
                    workspace_id: "wsp_querygraph0000000000000001".to_string(),
                    level: "semantic".to_string(),
                    kind: "fact".to_string(),
                    content: "Graph anchor memory for release planning.".to_string(),
                    workflow_id: None,
                    confidence: 0.91,
                    utility: 0.72,
                    importance: 0.66,
                    provenance_uri: Some("file://docs/query-schema.md#L221".to_string()),
                    trust_class: "agent_validated".to_string(),
                    trust_subclass: Some("golden-fixture".to_string()),
                    valid_from: None,
                    valid_to: None,
                    tags: vec!["graph".to_string(), "release".to_string()],
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_memory(
                "mem_00000000000000000000000102",
                &CreateMemoryInput {
                    workspace_id: "wsp_querygraph0000000000000001".to_string(),
                    level: "procedural".to_string(),
                    kind: "rule".to_string(),
                    content: "Neighbor selected only through supports edge.".to_string(),
                    workflow_id: None,
                    confidence: 0.88,
                    utility: 0.84,
                    importance: 0.71,
                    provenance_uri: Some("file://docs/query-schema.md#L230".to_string()),
                    trust_class: "agent_validated".to_string(),
                    trust_subclass: Some("golden-fixture".to_string()),
                    valid_from: None,
                    valid_to: None,
                    tags: vec!["graph".to_string(), "release".to_string()],
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .execute_raw(
                "UPDATE memories SET created_at = '2026-05-08T12:00:00+00:00', updated_at = '2026-05-08T12:00:00+00:00' WHERE id IN ('mem_00000000000000000000000101', 'mem_00000000000000000000000102')",
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_memory_link(
                "link_00000000000000000000000101",
                &CreateMemoryLinkInput {
                    src_memory_id: "mem_00000000000000000000000101".to_string(),
                    dst_memory_id: "mem_00000000000000000000000102".to_string(),
                    relation: MemoryLinkRelation::Supports,
                    weight: 0.93,
                    confidence: 0.89,
                    directed: true,
                    evidence_count: 2,
                    last_reinforced_at: Some("2026-05-08T12:01:00+00:00".to_string()),
                    source: MemoryLinkSource::Agent,
                    created_by: Some("golden-query-graph".to_string()),
                    metadata_json: None,
                },
            )
            .map_err(|error| error.to_string())?;
        connection.close().map_err(|error| error.to_string())
    }

    fn seed_pack_selection(database: &Path) -> TestResult {
        let connection = DbConnection::open_file(database).map_err(|error| error.to_string())?;
        connection
            .execute_raw(
                "INSERT INTO pack_records (id, workspace_id, query, profile, max_tokens, used_tokens, item_count, omitted_count, pack_hash, degraded_json, created_at, created_by) VALUES ('pack_00000000000000000000000001', 'wsp_searchjson0000000000000001', 'format before release', 'compact', 4000, 8, 1, 0, 'blake3:test-pack-hash', NULL, '2026-04-29T12:01:00+00:00', 'golden-test')",
            )
            .map_err(|error| error.to_string())?;
        connection
            .execute_raw(
                "INSERT INTO pack_items (pack_id, memory_id, rank, section, estimated_tokens, relevance, utility, why, diversity_key) VALUES ('pack_00000000000000000000000001', 'mem_00000000000000000000000001', 1, 'procedural_rules', 8, 0.91, 0.8, 'Selected because the memory matches release-formatting work.', 'procedural:rule:cargo')",
            )
            .map_err(|error| error.to_string())?;
        connection.close().map_err(|error| error.to_string())
    }

    fn sql_text(value: &str) -> String {
        format!("'{}'", value.replace('\'', "''"))
    }

    fn sql_json(value: Option<&serde_json::Value>) -> Result<String, String> {
        value
            .map_or_else(
                || Ok("NULL".to_string()),
                |json| serde_json::to_string(json).map(|raw| sql_text(&raw)),
            )
            .map_err(|error| format!("failed to serialize fixture JSON: {error}"))
    }

    struct PackFixtureInput<'a> {
        id: &'a str,
        pack_hash: &'a str,
        ledger_hash: Option<&'a str>,
        ledger_json: Option<serde_json::Value>,
        created_at: &'a str,
        rank: u32,
        relevance: f32,
        utility: f32,
        why: &'a str,
    }

    fn insert_pack_fixture(connection: &DbConnection, input: PackFixtureInput<'_>) -> TestResult {
        let ledger_json_sql = sql_json(input.ledger_json.as_ref())?;
        let ledger_hash_sql = input
            .ledger_hash
            .map_or_else(|| "NULL".to_string(), sql_text);
        connection
            .execute_raw(&format!(
                "INSERT INTO pack_records (id, workspace_id, query, profile, max_tokens, used_tokens, item_count, omitted_count, pack_hash, degraded_json, ledger_json, ledger_hash, created_at, created_by) VALUES ({}, 'wsp_searchjson0000000000000001', 'format before release', 'compact', 4000, 8, 1, 0, {}, NULL, {}, {}, {}, 'golden-test')",
                sql_text(input.id),
                sql_text(input.pack_hash),
                ledger_json_sql,
                ledger_hash_sql,
                sql_text(input.created_at),
            ))
            .map_err(|error| error.to_string())?;
        connection
            .execute_raw(&format!(
                "INSERT INTO pack_items (pack_id, memory_id, rank, section, estimated_tokens, relevance, utility, why, diversity_key, provenance_json, trust_class, trust_subclass) VALUES ({}, 'mem_00000000000000000000000001', {}, 'procedural_rules', 8, {}, {}, {}, 'procedural:rule:cargo', '{{\"schema\":\"ee.pack_item.provenance.v1\",\"entries\":[]}}', 'human_explicit', 'project-rule')",
                sql_text(input.id),
                input.rank,
                input.relevance,
                input.utility,
                sql_text(input.why),
            ))
            .map_err(|error| error.to_string())
    }

    struct PackFixtureLedgerInput<'a> {
        pack_id: &'a str,
        pack_hash: &'a str,
        ledger_hash: &'a str,
        rank: u32,
        relevance: f32,
        utility: f32,
        redaction_classes: &'a [&'a str],
        search_index: serde_json::Value,
        graph_snapshot: serde_json::Value,
        degraded: Vec<serde_json::Value>,
    }

    fn pack_fixture_ledger(input: PackFixtureLedgerInput<'_>) -> serde_json::Value {
        serde_json::json!({
            "core": {
                "schema": "ee.pack_replay_ledger.v1",
                "packId": input.pack_id,
                "packHash": input.pack_hash,
                "workspaceId": "wsp_searchjson0000000000000001",
                "createdAt": "2026-04-29T12:01:00+00:00",
                "createdBy": "golden-test",
                "commandSurface": "ee context",
                "request": {
                    "query": {
                        "hash": "blake3:query-fixture",
                        "redacted": false,
                        "redactionReasons": [],
                        "text": "format before release",
                        "redactedText": null
                    },
                    "profile": "compact",
                    "maxTokens": 4000
                },
                "database": {
                    "schemaVersion": 40,
                    "generation": 40
                },
                "derivedAssets": {
                    "searchIndex": input.search_index,
                    "graphSnapshot": input.graph_snapshot
                },
                "candidateCounts": {
                    "selected": 1,
                    "omitted": 0,
                    "candidatePool": 1
                },
                "selectedItems": [{
                    "memoryId": "mem_00000000000000000000000001",
                    "rank": input.rank,
                    "section": "procedural_rules",
                    "estimatedTokens": 8,
                    "scores": {
                        "relevance": input.relevance,
                        "utility": input.utility
                    },
                    "why": {
                        "hash": "blake3:why-fixture",
                        "redacted": false,
                        "redactionReasons": [],
                        "text": "Selected because the memory matches release-formatting work.",
                        "redactedText": null
                    },
                    "diversityKey": "procedural:rule:cargo",
                    "trustClass": "human_explicit",
                    "trustSubclass": "project-rule",
                    "provenance": {
                        "hash": "blake3:provenance-fixture",
                        "redacted": false,
                        "redactionReasons": []
                    },
                    "redactionClasses": input.redaction_classes,
                    "freshness": "unavailable"
                }],
                "omittedItems": [],
                "degraded": input.degraded
            },
            "ledgerHash": input.ledger_hash
        })
    }

    fn seed_pack_replay_fixtures(database: &Path) -> TestResult {
        let connection = DbConnection::open_file(database).map_err(|error| error.to_string())?;
        let unavailable_asset = serde_json::json!({"status": "not_recorded", "manifestHash": null});
        let available_search =
            serde_json::json!({"status": "available", "manifestHash": "blake3:search-v2"});
        let stale_graph =
            serde_json::json!({"status": "stale", "manifestHash": "blake3:graph-old"});

        let base_ledger = pack_fixture_ledger(PackFixtureLedgerInput {
            pack_id: "pack_00000000000000000000000011",
            pack_hash: "blake3:pack-base",
            ledger_hash: "blake3:ledger-base",
            rank: 1,
            relevance: 0.91,
            utility: 0.80,
            redaction_classes: &[],
            search_index: unavailable_asset.clone(),
            graph_snapshot: unavailable_asset.clone(),
            degraded: Vec::new(),
        });
        insert_pack_fixture(
            &connection,
            PackFixtureInput {
                id: "pack_00000000000000000000000011",
                pack_hash: "blake3:pack-base",
                ledger_hash: Some("blake3:ledger-base"),
                ledger_json: Some(base_ledger),
                created_at: "2026-04-29T12:01:00+00:00",
                rank: 1,
                relevance: 0.91,
                utility: 0.80,
                why: "Selected because the memory matches release-formatting work.",
            },
        )?;

        let ranking_ledger = pack_fixture_ledger(PackFixtureLedgerInput {
            pack_id: "pack_00000000000000000000000012",
            pack_hash: "blake3:pack-ranking",
            ledger_hash: "blake3:ledger-ranking",
            rank: 2,
            relevance: 0.84,
            utility: 0.70,
            redaction_classes: &[],
            search_index: unavailable_asset.clone(),
            graph_snapshot: unavailable_asset.clone(),
            degraded: Vec::new(),
        });
        insert_pack_fixture(
            &connection,
            PackFixtureInput {
                id: "pack_00000000000000000000000012",
                pack_hash: "blake3:pack-ranking",
                ledger_hash: Some("blake3:ledger-ranking"),
                ledger_json: Some(ranking_ledger),
                created_at: "2026-04-29T12:02:00+00:00",
                rank: 2,
                relevance: 0.84,
                utility: 0.70,
                why: "Selected after ranking changed.",
            },
        )?;

        let redaction_ledger = pack_fixture_ledger(PackFixtureLedgerInput {
            pack_id: "pack_00000000000000000000000013",
            pack_hash: "blake3:pack-redaction",
            ledger_hash: "blake3:ledger-redaction",
            rank: 1,
            relevance: 0.91,
            utility: 0.80,
            redaction_classes: &["anthropic_api_key"],
            search_index: unavailable_asset.clone(),
            graph_snapshot: unavailable_asset.clone(),
            degraded: Vec::new(),
        });
        insert_pack_fixture(
            &connection,
            PackFixtureInput {
                id: "pack_00000000000000000000000013",
                pack_hash: "blake3:pack-redaction",
                ledger_hash: Some("blake3:ledger-redaction"),
                ledger_json: Some(redaction_ledger),
                created_at: "2026-04-29T12:03:00+00:00",
                rank: 1,
                relevance: 0.91,
                utility: 0.80,
                why: "Selected after redaction classification changed.",
            },
        )?;

        let degraded_ledger = pack_fixture_ledger(PackFixtureLedgerInput {
            pack_id: "pack_00000000000000000000000014",
            pack_hash: "blake3:pack-degraded",
            ledger_hash: "blake3:ledger-degraded",
            rank: 1,
            relevance: 0.91,
            utility: 0.80,
            redaction_classes: &[],
            search_index: available_search,
            graph_snapshot: stale_graph,
            degraded: vec![serde_json::json!({
                "code": "context_graph_snapshot_stale",
                "message": "Graph snapshot was stale during pack selection.",
                "severity": "medium"
            })],
        });
        insert_pack_fixture(
            &connection,
            PackFixtureInput {
                id: "pack_00000000000000000000000014",
                pack_hash: "blake3:pack-degraded",
                ledger_hash: Some("blake3:ledger-degraded"),
                ledger_json: Some(degraded_ledger),
                created_at: "2026-04-29T12:04:00+00:00",
                rank: 1,
                relevance: 0.91,
                utility: 0.80,
                why: "Selected with degraded graph evidence.",
            },
        )?;

        connection.close().map_err(|error| error.to_string())
    }

    fn assert_pack_command_golden(
        workspace: &Path,
        database: &Path,
        pack_args: &[&str],
        golden_name: &str,
    ) -> TestResult {
        let workspace_arg = workspace.to_string_lossy().into_owned();
        let database_arg = database.to_string_lossy().into_owned();
        let mut args = vec!["--json", "--workspace", workspace_arg.as_str(), "pack"];
        args.extend_from_slice(pack_args);
        args.extend_from_slice(&["--database", database_arg.as_str()]);

        let output = run_ee(&args)?;
        let stdout = String::from_utf8(output.stdout)
            .map_err(|error| format!("pack stdout was not UTF-8: {error}"))?;
        let stderr = String::from_utf8(output.stderr)
            .map_err(|error| format!("pack stderr was not UTF-8: {error}"))?;
        ensure(
            output.status.success(),
            format!("ee {} should succeed; stderr: {stderr}", args.join(" ")),
        )?;
        ensure(
            stderr.is_empty(),
            format!(
                "ee {} stderr must be empty, got: {stderr:?}",
                args.join(" ")
            ),
        )?;
        ensure(
            stdout.starts_with('{'),
            format!("ee {} stdout must start with JSON", args.join(" ")),
        )?;
        ensure(
            stdout.ends_with('\n'),
            format!("ee {} stdout must end with newline", args.join(" ")),
        )?;
        assert_golden("pack", golden_name, &stdout)
    }

    #[test]
    fn pack_replay_available_matches_golden() -> TestResult {
        let artifact_dir = unique_artifact_dir("pack-replay-available")?;
        let workspace = artifact_dir.join("workspace");
        let database = workspace.join(".ee").join("ee.db");
        fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
        seed_search_workspace(&workspace, &database)?;
        seed_pack_replay_fixtures(&database)?;

        assert_pack_command_golden(
            &workspace,
            &database,
            &["replay", "pack_00000000000000000000000011"],
            "pack_replay_available.json",
        )
    }

    #[test]
    fn pack_replay_missing_ledger_matches_golden() -> TestResult {
        let artifact_dir = unique_artifact_dir("pack-replay-missing-ledger")?;
        let workspace = artifact_dir.join("workspace");
        let database = workspace.join(".ee").join("ee.db");
        fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
        seed_search_workspace(&workspace, &database)?;
        seed_pack_selection(&database)?;

        assert_pack_command_golden(
            &workspace,
            &database,
            &["replay", "pack_00000000000000000000000001"],
            "pack_replay_missing_ledger.json",
        )
    }

    #[test]
    fn pack_diff_no_change_matches_golden() -> TestResult {
        let artifact_dir = unique_artifact_dir("pack-diff-no-change")?;
        let workspace = artifact_dir.join("workspace");
        let database = workspace.join(".ee").join("ee.db");
        fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
        seed_search_workspace(&workspace, &database)?;
        seed_pack_replay_fixtures(&database)?;

        assert_pack_command_golden(
            &workspace,
            &database,
            &[
                "diff",
                "pack_00000000000000000000000011",
                "pack_00000000000000000000000011",
            ],
            "pack_diff_no_change.json",
        )
    }

    #[test]
    fn pack_diff_ranking_change_matches_golden() -> TestResult {
        let artifact_dir = unique_artifact_dir("pack-diff-ranking")?;
        let workspace = artifact_dir.join("workspace");
        let database = workspace.join(".ee").join("ee.db");
        fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
        seed_search_workspace(&workspace, &database)?;
        seed_pack_replay_fixtures(&database)?;

        assert_pack_command_golden(
            &workspace,
            &database,
            &[
                "diff",
                "pack_00000000000000000000000011",
                "pack_00000000000000000000000012",
            ],
            "pack_diff_ranking_change.json",
        )
    }

    #[test]
    fn pack_diff_redaction_change_matches_golden() -> TestResult {
        let artifact_dir = unique_artifact_dir("pack-diff-redaction")?;
        let workspace = artifact_dir.join("workspace");
        let database = workspace.join(".ee").join("ee.db");
        fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
        seed_search_workspace(&workspace, &database)?;
        seed_pack_replay_fixtures(&database)?;

        assert_pack_command_golden(
            &workspace,
            &database,
            &[
                "diff",
                "pack_00000000000000000000000011",
                "pack_00000000000000000000000013",
            ],
            "pack_diff_redaction_change.json",
        )
    }

    #[test]
    fn pack_diff_degraded_assets_match_golden() -> TestResult {
        let artifact_dir = unique_artifact_dir("pack-diff-degraded-assets")?;
        let workspace = artifact_dir.join("workspace");
        let database = workspace.join(".ee").join("ee.db");
        fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
        seed_search_workspace(&workspace, &database)?;
        seed_pack_replay_fixtures(&database)?;

        assert_pack_command_golden(
            &workspace,
            &database,
            &[
                "diff",
                "pack_00000000000000000000000011",
                "pack_00000000000000000000000014",
            ],
            "pack_diff_degraded_assets.json",
        )
    }

    fn build_search_index(workspace: &Path, database: &Path, index_dir: &Path) -> TestResult {
        build_search_index_expect(workspace, database, index_dir, 1)
    }

    fn build_search_index_expect(
        workspace: &Path,
        database: &Path,
        index_dir: &Path,
        expected_documents: u32,
    ) -> TestResult {
        let report = rebuild_index(&IndexRebuildOptions {
            workspace_path: workspace.to_path_buf(),
            database_path: Some(database.to_path_buf()),
            index_dir: Some(index_dir.to_path_buf()),
            dry_run: false,
        })
        .map_err(|error| error.to_string())?;

        ensure_equal(
            &report.status,
            &IndexRebuildStatus::Success,
            "index rebuild status",
        )?;
        ensure_equal(
            &report.documents_total,
            &expected_documents,
            "indexed document count",
        )
    }

    #[test]
    fn golden_path_uses_manifest_dir_and_category() -> TestResult {
        let test = GoldenTest::new("status", "json_output");
        let path = test.golden_path();
        let path_str = path.to_string_lossy();
        ensure_contains(&path_str, "tests/fixtures/golden/status", "path structure")?;
        ensure_contains(&path_str, "json_output.golden", "file name")
    }

    #[test]
    fn format_diff_shows_line_differences() -> TestResult {
        let test = GoldenTest::new("test", "diff");
        let expected = "line1\nline2\nline3";
        let actual = "line1\nchanged\nline3";
        let diff = test.format_diff(expected, actual);
        ensure_contains(&diff, "- line2", "removed line")?;
        ensure_contains(&diff, "+ changed", "added line")?;
        ensure_contains(&diff, "  line1", "unchanged line")
    }

    #[test]
    fn audit_json_contracts_match_goldens() -> TestResult {
        let first = audit_entry(
            "audit_golden_000000000000000001",
            "2026-05-06T13:00:00Z",
            "memory",
            "memory.create",
            None,
            "blake3:row-1",
        );
        let second = audit_entry(
            "audit_golden_000000000000000002",
            "2026-05-06T13:05:00Z",
            "rule",
            "rule.protect",
            Some("blake3:row-1"),
            "blake3:row-2",
        );

        assert_json_golden(
            "audit",
            "verify_empty",
            &AuditVerifyReport {
                schema: "ee.audit.verify.v1".to_owned(),
                integrity_ok: true,
                rows: 0,
                last_hash: None,
                first_break: None,
                issues: vec![],
            }
            .to_json(),
        )?;

        assert_json_golden(
            "audit",
            "timeline_single",
            &AuditTimelineReport {
                schema: "ee.audit.timeline.v1".to_owned(),
                entries: vec![first.clone()],
                pagination: TimelinePagination {
                    total_count: 1,
                    returned_count: 1,
                    has_more: false,
                    next_cursor: None,
                },
            }
            .to_json(),
        )?;

        assert_json_golden(
            "audit",
            "timeline_filtered_window",
            &AuditTimelineReport {
                schema: "ee.audit.timeline.v1".to_owned(),
                entries: vec![second.clone()],
                pagination: TimelinePagination {
                    total_count: 1,
                    returned_count: 1,
                    has_more: true,
                    next_cursor: Some("2".to_owned()),
                },
            }
            .to_json(),
        )?;

        assert_json_golden(
            "audit",
            "show_single",
            &AuditShowReport {
                schema: "ee.audit.show.v1".to_owned(),
                row: first.clone(),
                linked_snapshot: LinkedSnapshot {
                    target_type: Some("memory".to_owned()),
                    target_id: Some("memory_auditgolden0000000001".to_owned()),
                    found: true,
                    snapshot_hash: Some("blake3:memory-snapshot".to_owned()),
                    snapshot: Some(serde_json::json!({
                        "id": "memory_auditgolden0000000001",
                        "level": "procedural"
                    })),
                },
                hash_chain_valid: true,
            }
            .to_json(),
        )?;

        assert_json_golden(
            "audit",
            "diff_multi",
            &AuditDiffReport {
                schema: "ee.audit.diff.v1".to_owned(),
                from: "2026-05-06T13:00:00Z".to_owned(),
                to: "2026-05-06T13:10:00Z".to_owned(),
                entries: vec![first, second],
                row_count: 2,
            }
            .to_json(),
        )?;

        assert_json_golden(
            "audit",
            "verify_window",
            &AuditVerifyReport {
                schema: "ee.audit.verify.v1".to_owned(),
                integrity_ok: true,
                rows: 1,
                last_hash: Some("blake3:row-2".to_owned()),
                first_break: None,
                issues: vec![],
            }
            .to_json(),
        )?;

        assert_json_golden(
            "audit",
            "verify_chain_break",
            &AuditVerifyReport {
                schema: "ee.audit.verify.v1".to_owned(),
                integrity_ok: false,
                rows: 2,
                last_hash: Some("blake3:row-2".to_owned()),
                first_break: Some("audit_golden_000000000000000002".to_owned()),
                issues: vec![VerificationIssue {
                    code: "row_hash_mismatch".to_owned(),
                    audit_id: Some("audit_golden_000000000000000002".to_owned()),
                    message: "row audit_golden_000000000000000002 hash mismatch: stored blake3:row-2, recomputed blake3:tampered".to_owned(),
                }],
            }
            .to_json(),
        )
    }

    #[test]
    fn agent_status_json_matches_golden() -> TestResult {
        assert_agent_stdout_golden(
            &["--workspace", DOCTOR_GOLDEN_WORKSPACE, "status", "--json"],
            "status.json",
            true,
        )
    }

    #[test]
    fn agent_doctor_json_matches_golden() -> TestResult {
        assert_agent_stdout_golden(
            &["--workspace", DOCTOR_GOLDEN_WORKSPACE, "doctor", "--json"],
            "doctor.json",
            true,
        )
    }

    #[test]
    fn agent_docs_json_matches_golden() -> TestResult {
        assert_agent_stdout_golden(&["--agent-docs"], "agent_docs.json", true)
    }

    #[test]
    fn agent_health_unavailable_json_matches_golden() -> TestResult {
        assert_agent_stdout_golden(&["--json", "health"], "health_unavailable.json", true)
    }

    #[test]
    fn agent_search_unavailable_json_matches_golden() -> TestResult {
        let artifact_dir = unique_artifact_dir("search-unavailable")?;
        let workspace = artifact_dir.join("workspace");
        fs::create_dir_all(&workspace).map_err(|error| {
            format!(
                "failed to create workspace {}: {error}",
                workspace.display()
            )
        })?;

        let output = Command::new(env!("CARGO_BIN_EXE_ee"))
            .arg("--json")
            .arg("--workspace")
            .arg(&workspace)
            .arg("search")
            .arg("format-before-release")
            .output()
            .map_err(|error| format!("failed to run ee search --json: {error}"))?;
        let stdout = String::from_utf8(output.stdout)
            .map_err(|error| format!("search stdout was not UTF-8: {error}"))?;
        let stderr = String::from_utf8(output.stderr)
            .map_err(|error| format!("search stderr was not UTF-8: {error}"))?;

        ensure(
            !output.status.success(),
            format!("search should fail without a database; stderr: {stderr}"),
        )?;
        ensure(
            stderr.is_empty(),
            format!("search JSON diagnostics must stay out of stderr, got: {stderr:?}"),
        )?;
        ensure(
            stdout.starts_with('{'),
            format!("search stdout must start with JSON data, got: {stdout:?}"),
        )?;
        ensure(
            stdout.ends_with('\n'),
            format!("search stdout must end with a newline, got: {stdout:?}"),
        )?;

        assert_golden("agent", "search_unavailable.json", &stdout)
    }

    #[test]
    fn gate16_preflight_run_json_matches_golden() -> TestResult {
        let value = run_json_stdout(
            &[
                "--json",
                "preflight",
                "run",
                "deploy production database migration",
            ],
            true,
        )?;
        ensure_equal(
            &value["schema"],
            &serde_json::json!("ee.response.v1"),
            "preflight run response schema",
        )?;
        ensure_equal(
            &value["success"],
            &serde_json::json!(true),
            "preflight run success flag",
        )?;
        ensure_equal(
            &value["data"]["degraded"][0]["code"],
            &serde_json::json!("preflight_evidence_unavailable"),
            "preflight run degraded code",
        )?;
        ensure_equal(
            &value["data"]["next_action"],
            &serde_json::json!("collect_preflight_evidence_or_use_risk_review_skill"),
            "preflight run next action",
        )
    }

    #[test]
    fn gate16_preflight_show_json_matches_golden() -> TestResult {
        let value = run_json_stdout(
            &["--json", "preflight", "show", "pf_gate16_contract"],
            false,
        )?;
        ensure_equal(
            &value["schema"],
            &serde_json::json!("ee.error.v2"),
            "preflight show error schema",
        )?;
        ensure_equal(
            &value["error"]["code"],
            &serde_json::json!("not_found"),
            "preflight show error code",
        )?;
        ensure_equal(
            &value["error"]["details"]["id"],
            &serde_json::json!("pf_gate16_contract"),
            "preflight show error id",
        )
    }

    #[test]
    fn gate16_preflight_close_json_matches_golden() -> TestResult {
        let value = run_json_stdout(
            &[
                "--json",
                "preflight",
                "close",
                "pf_gate16_contract",
                "--cleared",
                "--reason",
                "gate 16 reviewed",
                "--task-outcome",
                "success",
                "--feedback",
                "helped",
                "--dry-run",
            ],
            false,
        )?;
        ensure_equal(
            &value["schema"],
            &serde_json::json!("ee.error.v2"),
            "preflight close error schema",
        )?;
        ensure_equal(
            &value["error"]["code"],
            &serde_json::json!("not_found"),
            "preflight close error code",
        )?;
        ensure_equal(
            &value["error"]["details"]["id"],
            &serde_json::json!("pf_gate16_contract"),
            "preflight close error id",
        )
    }

    #[test]
    fn gate16_tripwire_list_json_matches_golden() -> TestResult {
        let value = run_json_stdout(
            &[
                "--json",
                "tripwire",
                "list",
                "--state",
                "triggered",
                "--include-disarmed",
            ],
            true,
        )?;
        ensure_equal(
            &value["schema"],
            &serde_json::json!("ee.tripwire.list.v1"),
            "tripwire list schema",
        )?;
        ensure_equal(
            &value["total_count"],
            &serde_json::json!(0),
            "tripwire list total count",
        )?;
        ensure_equal(
            &value["filters_applied"],
            &serde_json::json!(["state=triggered"]),
            "tripwire list filters",
        )
    }

    #[test]
    fn gate16_tripwire_check_json_matches_golden() -> TestResult {
        let value = run_json_stdout(
            &[
                "--json",
                "tripwire",
                "check",
                "tw_004",
                "--task-outcome",
                "success",
                "--dry-run",
            ],
            true,
        )?;
        ensure_equal(
            &value["schema"],
            &serde_json::json!("ee.tripwire.check.v1"),
            "tripwire check schema",
        )?;
        ensure_equal(
            &value["result"],
            &serde_json::json!("not_found"),
            "tripwire check result",
        )?;
        ensure_equal(
            &value["degraded"][0]["code"],
            &serde_json::json!("tripwire_inputs_incomplete"),
            "tripwire check degraded code",
        )
    }

    #[test]
    fn agent_search_json_returns_indexed_memory() -> TestResult {
        let artifact_dir = unique_artifact_dir("search-json")?;
        let workspace = artifact_dir.join("workspace");
        let database = workspace.join(".ee").join("ee.db");
        let index_dir = workspace.join(".ee").join("index");
        fs::create_dir_all(&workspace).map_err(|error| {
            format!(
                "failed to create workspace {}: {error}",
                workspace.display()
            )
        })?;

        seed_search_workspace(&workspace, &database)?;
        build_search_index(&workspace, &database, &index_dir)?;

        let output = Command::new(env!("CARGO_BIN_EXE_ee"))
            .arg("--json")
            .arg("--workspace")
            .arg(&workspace)
            .arg("search")
            .arg("format before release")
            .arg("--database")
            .arg(&database)
            .arg("--index-dir")
            .arg(&index_dir)
            .output()
            .map_err(|error| format!("failed to run ee search --json: {error}"))?;

        let stdout = String::from_utf8(output.stdout)
            .map_err(|error| format!("search stdout was not UTF-8: {error}"))?;
        let stderr = String::from_utf8(output.stderr)
            .map_err(|error| format!("search stderr was not UTF-8: {error}"))?;

        ensure(
            output.status.success(),
            format!("search --json should succeed; stderr: {stderr}"),
        )?;
        ensure(
            stderr.is_empty(),
            format!("search --json stderr must be empty, got: {stderr:?}"),
        )?;
        ensure(
            stdout.starts_with('{'),
            format!("search stdout must start with JSON data, got: {stdout:?}"),
        )?;
        ensure(
            stdout.ends_with('\n'),
            format!("search stdout must end with a newline, got: {stdout:?}"),
        )?;

        let value: serde_json::Value =
            serde_json::from_str(&stdout).map_err(|error| error.to_string())?;
        ensure_equal(
            &value["schema"],
            &serde_json::json!("ee.response.v1"),
            "search schema",
        )?;
        ensure_equal(
            &value["success"],
            &serde_json::json!(true),
            "search success",
        )?;
        ensure_equal(
            &value["data"]["command"],
            &serde_json::json!("search"),
            "search command",
        )?;
        ensure_equal(
            &value["data"]["status"],
            &serde_json::json!("success"),
            "search status",
        )?;
        ensure_equal(
            &value["data"]["resultCount"],
            &serde_json::json!(1),
            "search result count",
        )?;
        ensure_equal(
            &value["data"]["results"][0]["docId"],
            &serde_json::json!("mem_00000000000000000000000001"),
            "search result memory id",
        )?;
        ensure_equal(
            &value["data"]["metrics"]["requestedLimit"],
            &serde_json::json!(10),
            "search metrics requested limit",
        )?;
        ensure_equal(
            &value["data"]["metrics"]["returnedCount"],
            &serde_json::json!(1),
            "search metrics returned count",
        )?;
        ensure_equal(
            &value["data"]["metrics"]["errorCount"],
            &serde_json::json!(0),
            "search metrics error count",
        )?;
        ensure_json_number_close(
            &value["data"]["metrics"]["scoreDistribution"]["top"],
            &value["data"]["results"][0]["score"],
            0.000_001,
            "search metrics top score",
        )?;
        let source = value["data"]["results"][0]["source"]
            .as_str()
            .ok_or_else(|| "search result source must be a string".to_string())?;
        let source_count_key = match source {
            "semantic_fast" => "semanticFast",
            "semantic_quality" => "semanticQuality",
            other => other,
        };
        ensure_equal(
            &value["data"]["metrics"]["sourceCounts"][source_count_key],
            &serde_json::json!(1),
            "search metrics source count",
        )
    }

    #[test]
    fn agent_context_json_returns_indexed_memory() -> TestResult {
        let artifact_dir = unique_artifact_dir("context-json")?;
        let workspace = artifact_dir.join("workspace");
        let database = workspace.join(".ee").join("ee.db");
        let index_dir = workspace.join(".ee").join("index");
        fs::create_dir_all(&workspace).map_err(|error| {
            format!(
                "failed to create workspace {}: {error}",
                workspace.display()
            )
        })?;

        seed_search_workspace(&workspace, &database)?;
        build_search_index(&workspace, &database, &index_dir)?;

        let output = Command::new(env!("CARGO_BIN_EXE_ee"))
            .arg("--json")
            .arg("--workspace")
            .arg(&workspace)
            .arg("context")
            .arg("format before release")
            .arg("--database")
            .arg(&database)
            .arg("--index-dir")
            .arg(&index_dir)
            .arg("--profile")
            .arg("compact")
            .arg("--max-tokens")
            .arg("4000")
            .arg("--candidate-pool")
            .arg("10")
            .output()
            .map_err(|error| format!("failed to run ee context --json: {error}"))?;

        let stdout = String::from_utf8(output.stdout)
            .map_err(|error| format!("context stdout was not UTF-8: {error}"))?;
        let stderr = String::from_utf8(output.stderr)
            .map_err(|error| format!("context stderr was not UTF-8: {error}"))?;

        ensure(
            output.status.success(),
            format!("context --json should succeed; stderr: {stderr}"),
        )?;
        ensure(
            stderr.is_empty(),
            format!("context --json stderr must be empty, got: {stderr:?}"),
        )?;
        ensure(
            stdout.starts_with('{'),
            format!("context stdout must start with JSON data, got: {stdout:?}"),
        )?;
        ensure(
            stdout.ends_with('\n'),
            format!("context stdout must end with a newline, got: {stdout:?}"),
        )?;

        let value: serde_json::Value =
            serde_json::from_str(&stdout).map_err(|error| error.to_string())?;
        ensure_equal(
            &value["schema"],
            &serde_json::json!("ee.response.v1"),
            "context schema",
        )?;
        ensure_equal(
            &value["success"],
            &serde_json::json!(true),
            "context success",
        )?;
        ensure_equal(
            &value["data"]["command"],
            &serde_json::json!("context"),
            "context command",
        )?;
        ensure_equal(
            &value["data"]["request"]["query"],
            &serde_json::json!("format before release"),
            "context request query",
        )?;
        ensure_equal(
            &value["data"]["request"]["profile"],
            &serde_json::json!("compact"),
            "context request profile",
        )?;
        ensure_equal(
            &value["data"]["request"]["candidatePool"],
            &serde_json::json!(10),
            "context request candidate pool",
        )?;
        ensure_equal(
            &value["data"]["pack"]["meta"]["algorithm"]["objective"],
            &serde_json::json!("mmr_redundancy"),
            "context algorithm objective",
        )?;
        // Bead bd-2pe1z (A1 phase 2): selectionAudit.steps[] is gone;
        // the per-item rank trace is now inline on items[]. Read from the
        // canonical items[] location instead.
        ensure_equal(
            &value["data"]["pack"]["items"][0]["memoryId"],
            &serde_json::json!("mem_00000000000000000000000001"),
            "canonical first-item memory id",
        )?;

        let items = value["data"]["pack"]["items"]
            .as_array()
            .ok_or_else(|| "context pack items must be an array".to_string())?;
        ensure_equal(&items.len(), &1_usize, "context item count")?;
        ensure_equal(
            &items[0]["memoryId"],
            &serde_json::json!("mem_00000000000000000000000001"),
            "context item memory id",
        )?;
        ensure_contains(
            items[0]["content"].as_str().unwrap_or_default(),
            "cargo fmt --check",
            "context item content",
        )?;

        let provenance = items[0]["provenance"]
            .as_array()
            .ok_or_else(|| "context item provenance must be an array".to_string())?;
        ensure(
            !provenance.is_empty(),
            "context item provenance must be present",
        )?;

        let normalized = normalize_context_pack_json(&stdout);
        assert_golden("agent", "context_pack.json", &normalized)
    }

    #[test]
    fn agent_pack_query_file_json_matches_context_pack_golden() -> TestResult {
        let artifact_dir = unique_artifact_dir("pack-query-file-json")?;
        let workspace = artifact_dir.join("workspace");
        let database = workspace.join(".ee").join("ee.db");
        let index_dir = workspace.join(".ee").join("index");
        fs::create_dir_all(&workspace).map_err(|error| {
            format!(
                "failed to create workspace {}: {error}",
                workspace.display()
            )
        })?;

        seed_search_workspace(&workspace, &database)?;
        build_search_index(&workspace, &database, &index_dir)?;
        let query_file = workspace.join("context.eeq.json");
        fs::write(
            &query_file,
            r#"{
              "version": "ee.query.v1",
              "query": {"text": "format before release", "mode": "hybrid"},
              "time": {
                "after": "2026-04-29T12:00:00+00:00",
                "before": "2026-04-29T12:00:00+00:00"
              },
              "asOf": "2026-04-29T12:00:00+00:00",
              "temporalValidity": {
                "posture": "strict",
                "referenceTime": "2026-04-29T12:00:00+00:00"
              },
              "budget": {"maxTokens": 4000, "candidatePool": 10, "maxResults": 1},
              "output": {"format": "json", "profile": "compact", "explain": true}
            }"#,
        )
        .map_err(|error| error.to_string())?;

        let output = Command::new(env!("CARGO_BIN_EXE_ee"))
            .arg("--workspace")
            .arg(&workspace)
            .arg("pack")
            .arg("build")
            .arg("--query-file")
            .arg(&query_file)
            .arg("--database")
            .arg(&database)
            .arg("--index-dir")
            .arg(&index_dir)
            .output()
            .map_err(|error| format!("failed to run ee pack --query-file: {error}"))?;

        let stdout = String::from_utf8(output.stdout)
            .map_err(|error| format!("pack query-file stdout was not UTF-8: {error}"))?;
        let stderr = String::from_utf8(output.stderr)
            .map_err(|error| format!("pack query-file stderr was not UTF-8: {error}"))?;

        ensure(
            output.status.success(),
            format!("pack query-file should succeed; stderr: {stderr}"),
        )?;
        ensure(
            stderr.is_empty(),
            format!("pack query-file stderr must be empty, got: {stderr:?}"),
        )?;
        ensure(
            stdout.starts_with('{'),
            format!("pack query-file stdout must start with JSON data, got: {stdout:?}"),
        )?;
        ensure(
            stdout.ends_with('\n'),
            format!("pack query-file stdout must end with a newline, got: {stdout:?}"),
        )?;

        let value: serde_json::Value =
            serde_json::from_str(&stdout).map_err(|error| error.to_string())?;
        ensure_equal(
            &value["schema"],
            &serde_json::json!("ee.response.v1"),
            "pack query-file schema",
        )?;
        ensure_equal(
            &value["data"]["request"]["query"],
            &serde_json::json!("format before release"),
            "pack query-file query",
        )?;
        ensure_equal(
            &value["data"]["request"]["profile"],
            &serde_json::json!("compact"),
            "pack query-file profile",
        )?;
        ensure(
            value.pointer("/data/pack/selectionAudit").is_some(),
            "pack build output must include selectionAudit",
        )?;
        ensure(
            value.pointer("/data/pack/selectionCertificate").is_none(),
            "pack build output must omit deprecated selectionCertificate by default",
        )?;
        ensure(
            value
                .pointer("/data/pack/meta/algorithm/guaranteeStatus")
                .is_none(),
            "pack build output must omit guaranteeStatus",
        )?;

        let legacy_output = Command::new(env!("CARGO_BIN_EXE_ee"))
            .env("EE_LEGACY_SELECTION_CERTIFICATE", "1")
            .arg("--workspace")
            .arg(&workspace)
            .arg("pack")
            .arg("build")
            .arg("--query-file")
            .arg(&query_file)
            .arg("--database")
            .arg(&database)
            .arg("--index-dir")
            .arg(&index_dir)
            .output()
            .map_err(|error| format!("failed to run legacy ee pack build --query-file: {error}"))?;
        let legacy_stdout = String::from_utf8(legacy_output.stdout)
            .map_err(|error| format!("legacy pack query-file stdout was not UTF-8: {error}"))?;
        let legacy_stderr = String::from_utf8(legacy_output.stderr)
            .map_err(|error| format!("legacy pack query-file stderr was not UTF-8: {error}"))?;
        ensure(
            legacy_output.status.success(),
            format!("legacy pack query-file should succeed; stderr: {legacy_stderr}"),
        )?;
        ensure(
            legacy_stderr.is_empty(),
            format!("legacy pack query-file stderr must be empty, got: {legacy_stderr:?}"),
        )?;
        let legacy_value: serde_json::Value =
            serde_json::from_str(&legacy_stdout).map_err(|error| error.to_string())?;
        ensure_equal(
            &legacy_value["data"]["pack"]["deprecation"]["deprecatedField"],
            &serde_json::json!("selectionCertificate"),
            "legacy selectionCertificate deprecation field",
        )?;
        ensure_equal(
            &legacy_value["data"]["pack"]["deprecation"]["replacementField"],
            &serde_json::json!("selectionAudit"),
            "legacy selectionCertificate replacement field",
        )?;
        ensure(
            legacy_value
                .pointer("/data/pack/selectionCertificate/algorithmId")
                .is_some(),
            "legacy pack output must include selectionCertificate when opted in",
        )?;
        ensure(
            legacy_value
                .pointer("/data/pack/selectionCertificate/guaranteeStatus")
                .is_none(),
            "legacy pack output must still omit guaranteeStatus",
        )?;

        let normalized = normalize_context_pack_json(&stdout);
        assert_golden("agent", "query_file_context_pack.json", &normalized)
    }

    #[test]
    fn agent_pack_query_file_graph_hints_match_context_pack_golden() -> TestResult {
        let artifact_dir = unique_artifact_dir("pack-query-file-graph")?;
        let workspace = artifact_dir.join("workspace");
        let database = workspace.join(".ee").join("ee.db");
        let index_dir = workspace.join(".ee").join("index");
        fs::create_dir_all(&workspace).map_err(|error| {
            format!(
                "failed to create workspace {}: {error}",
                workspace.display()
            )
        })?;

        seed_graph_query_workspace(&workspace, &database)?;
        build_search_index_expect(&workspace, &database, &index_dir, 2)?;
        let query_file = workspace.join("graph.eeq.json");
        fs::write(
            &query_file,
            r#"{
              "version": "ee.query.v1",
              "query": {"text": "graph anchor release", "mode": "hybrid"},
              "graph": {
                "seedMemories": ["mem_00000000000000000000000101"],
                "traversal": "outbound",
                "maxHops": 1,
                "linkTypes": ["supports"],
                "includeOrphans": false
              },
              "budget": {"maxTokens": 4000, "candidatePool": 10, "maxResults": 2},
              "output": {"format": "json", "profile": "compact", "explain": true}
            }"#,
        )
        .map_err(|error| error.to_string())?;

        let output = Command::new(env!("CARGO_BIN_EXE_ee"))
            .arg("--workspace")
            .arg(&workspace)
            .arg("pack")
            .arg("--query-file")
            .arg(&query_file)
            .arg("--database")
            .arg(&database)
            .arg("--index-dir")
            .arg(&index_dir)
            .output()
            .map_err(|error| format!("failed to run ee pack graph query-file: {error}"))?;

        let stdout = String::from_utf8(output.stdout)
            .map_err(|error| format!("pack graph query-file stdout was not UTF-8: {error}"))?;
        let stderr = String::from_utf8(output.stderr)
            .map_err(|error| format!("pack graph query-file stderr was not UTF-8: {error}"))?;

        ensure(
            output.status.success(),
            format!("pack graph query-file should succeed; stderr: {stderr}"),
        )?;
        ensure(
            stderr.is_empty(),
            format!("pack graph query-file stderr must be empty, got: {stderr:?}"),
        )?;

        let value: serde_json::Value =
            serde_json::from_str(&stdout).map_err(|error| error.to_string())?;
        ensure_equal(
            &value["schema"],
            &serde_json::json!("ee.response.v1"),
            "pack graph query-file schema",
        )?;
        ensure_equal(
            &value["data"]["pack"]["items"][0]["memoryId"],
            &serde_json::json!("mem_00000000000000000000000101"),
            "graph seed item rank",
        )?;
        ensure_equal(
            &value["data"]["pack"]["items"][1]["memoryId"],
            &serde_json::json!("mem_00000000000000000000000102"),
            "graph neighbor item rank",
        )?;

        let normalized = normalize_context_pack_json(&stdout);
        assert_golden("agent", "query_file_graph_context_pack.json", &normalized)
    }

    #[test]
    fn agent_context_markdown_returns_formatted_pack() -> TestResult {
        let artifact_dir = unique_artifact_dir("context-markdown")?;
        let workspace = artifact_dir.join("workspace");
        let database = workspace.join(".ee").join("ee.db");
        let index_dir = workspace.join(".ee").join("index");
        fs::create_dir_all(&workspace).map_err(|error| {
            format!(
                "failed to create workspace {}: {error}",
                workspace.display()
            )
        })?;

        seed_search_workspace(&workspace, &database)?;
        build_search_index(&workspace, &database, &index_dir)?;

        let output = Command::new(env!("CARGO_BIN_EXE_ee"))
            .arg("--format")
            .arg("markdown")
            .arg("--workspace")
            .arg(&workspace)
            .arg("context")
            .arg("format before release")
            .arg("--database")
            .arg(&database)
            .arg("--index-dir")
            .arg(&index_dir)
            .arg("--profile")
            .arg("compact")
            .arg("--max-tokens")
            .arg("4000")
            .output()
            .map_err(|error| format!("failed to run ee context --format markdown: {error}"))?;

        let stdout = String::from_utf8(output.stdout)
            .map_err(|error| format!("context markdown stdout was not UTF-8: {error}"))?;
        let stderr = String::from_utf8(output.stderr)
            .map_err(|error| format!("context markdown stderr was not UTF-8: {error}"))?;

        ensure(
            output.status.success(),
            format!("context --format markdown should succeed; stderr: {stderr}"),
        )?;
        ensure(
            stderr.is_empty(),
            format!("context --format markdown stderr must be empty, got: {stderr:?}"),
        )?;
        ensure(
            stdout.starts_with('#'),
            format!("context markdown must start with # header, got: {stdout:?}"),
        )?;
        ensure_contains(&stdout, "Context Pack:", "should have pack header")?;
        ensure_contains(&stdout, "format before release", "should have query")?;
        ensure_contains(&stdout, "cargo fmt --check", "should have memory content")?;

        let normalized = normalize_context_pack_text(&stdout);
        assert_golden("agent", "context_pack.md", &normalized)
    }

    #[test]
    fn agent_context_markdown_provenance_hash_stability_and_artifact_logging() -> TestResult {
        let artifact_dir = unique_artifact_dir("context-md-provenance")?;
        let workspace = artifact_dir.join("workspace");
        let database = workspace.join(".ee").join("ee.db");
        let index_dir = workspace.join(".ee").join("index");
        fs::create_dir_all(&workspace).map_err(|error| {
            format!(
                "failed to create workspace {}: {error}",
                workspace.display()
            )
        })?;

        seed_search_workspace(&workspace, &database)?;
        build_search_index(&workspace, &database, &index_dir)?;

        // Run context --json twice to verify pack hash stability (determinism).
        let run_context_json = || {
            Command::new(env!("CARGO_BIN_EXE_ee"))
                .arg("--json")
                .arg("--workspace")
                .arg(&workspace)
                .arg("context")
                .arg("format before release")
                .arg("--database")
                .arg(&database)
                .arg("--index-dir")
                .arg(&index_dir)
                .arg("--profile")
                .arg("compact")
                .arg("--max-tokens")
                .arg("4000")
                .output()
        };

        let output1 = run_context_json()
            .map_err(|error| format!("first context --json run failed: {error}"))?;
        ensure(
            output1.status.success(),
            format!(
                "first context --json should succeed; stderr: {}",
                String::from_utf8_lossy(&output1.stderr)
            ),
        )?;

        let stdout1 = String::from_utf8(output1.stdout)
            .map_err(|error| format!("first context stdout not UTF-8: {error}"))?;
        let json1: serde_json::Value =
            serde_json::from_str(&stdout1).map_err(|error| error.to_string())?;
        let hash1 = json1["data"]["pack"]["hash"]
            .as_str()
            .ok_or_else(|| "first run missing pack hash".to_string())?;

        let output2 = run_context_json()
            .map_err(|error| format!("second context --json run failed: {error}"))?;
        ensure(
            output2.status.success(),
            format!(
                "second context --json should succeed; stderr: {}",
                String::from_utf8_lossy(&output2.stderr)
            ),
        )?;

        let stdout2 = String::from_utf8(output2.stdout)
            .map_err(|error| format!("second context stdout not UTF-8: {error}"))?;
        let json2: serde_json::Value =
            serde_json::from_str(&stdout2).map_err(|error| error.to_string())?;
        let hash2 = json2["data"]["pack"]["hash"]
            .as_str()
            .ok_or_else(|| "second run missing pack hash".to_string())?;

        ensure_equal(&hash1, &hash2, "pack hash determinism")?;
        ensure(
            hash1.starts_with("blake3:"),
            format!("pack hash must be blake3 prefixed, got: {hash1}"),
        )?;

        // Run context --format markdown and verify provenance elements.
        let output_md = Command::new(env!("CARGO_BIN_EXE_ee"))
            .arg("--format")
            .arg("markdown")
            .arg("--workspace")
            .arg(&workspace)
            .arg("context")
            .arg("format before release")
            .arg("--database")
            .arg(&database)
            .arg("--index-dir")
            .arg(&index_dir)
            .arg("--profile")
            .arg("compact")
            .arg("--max-tokens")
            .arg("4000")
            .output()
            .map_err(|error| format!("context --format markdown failed: {error}"))?;

        let stdout_md = String::from_utf8(output_md.stdout)
            .map_err(|error| format!("markdown stdout not UTF-8: {error}"))?;

        ensure(
            output_md.status.success(),
            format!(
                "context --format markdown should succeed; stderr: {}",
                String::from_utf8_lossy(&output_md.stderr)
            ),
        )?;

        // Verify markdown contains provenance section.
        ensure_contains(&stdout_md, "**Provenance:**", "markdown provenance header")?;
        ensure_contains(
            &stdout_md,
            "file://AGENTS.md",
            "markdown provenance file reference",
        )?;

        // Verify selection rationale (the "Why:" section).
        ensure_contains(
            &stdout_md,
            "**Why:**",
            "markdown selection rationale header",
        )?;
        // Bead bd-17c65.1.3 (A3) — per-item why is now a one-line
        // reason starting "matched '<query>' via <source>". The old
        // "Deterministic retrieval explanation..." paragraph moved to
        // pack-level metadata.
        ensure_contains(
            &stdout_md,
            "matched '",
            "markdown selection rationale (A3 one-liner shape)",
        )?;
        ensure_contains(
            &stdout_md,
            "relevance ",
            "markdown selection rationale relevance score",
        )?;

        // Verify trust class is documented.
        ensure_contains(&stdout_md, "**Trust:**", "markdown trust header")?;
        ensure_contains(&stdout_md, "human_explicit", "markdown trust class")?;

        // Verify artifact logging: pack_records table should have entries.
        // Use list_pack_records_for_memory which returns pack records that include the test memory.
        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        let pack_history = connection
            .list_pack_records_for_memory("mem_00000000000000000000000001", 10)
            .map_err(|error| format!("failed to list pack records: {error}"))?;
        ensure(
            !pack_history.is_empty(),
            "pack_records should have entries for the test memory",
        )?;

        // Verify the pack record has expected fields.
        let (pack_record, pack_item) = &pack_history[0];
        ensure_equal(
            &pack_record.query,
            &"format before release".to_string(),
            "pack record query",
        )?;
        ensure_equal(
            &pack_record.profile,
            &"compact".to_string(),
            "pack record profile",
        )?;
        ensure(
            pack_record.pack_hash.starts_with("blake3:"),
            format!(
                "pack record hash must be blake3 prefixed, got: {}",
                pack_record.pack_hash
            ),
        )?;

        // Verify pack item links memory correctly.
        ensure_equal(
            &pack_item.memory_id,
            &"mem_00000000000000000000000001".to_string(),
            "pack item memory id",
        )?;
        ensure_equal(
            &pack_item.section,
            &"procedural_rules".to_string(),
            "pack item section",
        )?;
        ensure(
            !pack_item.why.is_empty(),
            "pack item why (selection rationale) must be populated",
        )?;

        connection.close().map_err(|error| error.to_string())?;

        Ok(())
    }

    #[test]
    fn agent_curate_review_dry_run_returns_preview_without_mutation() -> TestResult {
        let artifact_dir = unique_artifact_dir("curate-dry-run")?;
        let workspace = artifact_dir.join("workspace");
        let database = workspace.join(".ee").join("ee.db");
        fs::create_dir_all(workspace.join(".ee")).map_err(|error| {
            format!(
                "failed to create workspace {}: {error}",
                workspace.display()
            )
        })?;

        // Compute workspace_id using the same algorithm as stable_workspace_id.
        let workspace_id = compute_stable_workspace_id(&workspace);

        // Set up database with migrations.
        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;

        // Insert workspace with the correct stable ID.
        connection
            .insert_workspace(
                &workspace_id,
                &CreateWorkspaceInput {
                    path: workspace.to_string_lossy().into_owned(),
                    name: Some("curate-dry-run-test".to_string()),
                },
            )
            .map_err(|error| error.to_string())?;

        // Insert a memory that the curation candidate targets.
        connection
            .insert_memory(
                "mem_00000000000000curatedry001",
                &CreateMemoryInput {
                    workspace_id: workspace_id.clone(),
                    level: "procedural".to_string(),
                    kind: "guideline".to_string(),
                    content: "Original memory content for curation test.".to_string(),
                    workflow_id: None,
                    confidence: 0.6,
                    utility: 0.7,
                    importance: 0.5,
                    provenance_uri: Some("file://test.md#L1-5".to_string()),
                    trust_class: "agent_assertion".to_string(),
                    trust_subclass: None,
                    valid_from: None,
                    valid_to: None,
                    tags: vec!["curation-test".to_string()],
                },
            )
            .map_err(|error| error.to_string())?;

        // Insert a curation candidate proposing a confidence boost.
        connection
            .insert_curation_candidate(
                "curate_00000000000000000drytest01",
                &CreateCurationCandidateInput {
                    workspace_id: workspace_id.clone(),
                    candidate_type: "promote".to_string(),
                    target_memory_id: "mem_00000000000000curatedry001".to_string(),
                    proposed_content: None,
                    proposed_confidence: Some(0.85),
                    proposed_trust_class: Some("agent_validated".to_string()),
                    source_type: "feedback_event".to_string(),
                    source_id: Some("test-feedback-001".to_string()),
                    reason: "Multiple positive signals indicate higher confidence warranted."
                        .to_string(),
                    confidence: 0.8,
                    status: Some("pending".to_string()),
                    created_at: Some("2026-04-30T10:00:00+00:00".to_string()),
                    ttl_expires_at: None,
                },
            )
            .map_err(|error| error.to_string())?;

        connection.close().map_err(|error| error.to_string())?;

        // Run curate accept with --dry-run to verify preview behavior.
        let output = Command::new(env!("CARGO_BIN_EXE_ee"))
            .arg("--json")
            .arg("--workspace")
            .arg(&workspace)
            .arg("curate")
            .arg("accept")
            .arg("curate_00000000000000000drytest01")
            .arg("--dry-run")
            .arg("--database")
            .arg(&database)
            .output()
            .map_err(|error| format!("failed to run ee curate accept --dry-run: {error}"))?;

        let stdout = String::from_utf8(output.stdout)
            .map_err(|error| format!("curate accept stdout not UTF-8: {error}"))?;
        let stderr = String::from_utf8(output.stderr)
            .map_err(|error| format!("curate accept stderr not UTF-8: {error}"))?;

        ensure(
            output.status.success(),
            format!(
                "curate accept --dry-run should succeed; exit={:?} stdout={stdout} stderr={stderr}",
                output.status.code()
            ),
        )?;
        ensure(
            stderr.is_empty(),
            format!("curate accept --dry-run stderr must be empty, got: {stderr:?}"),
        )?;

        // Parse JSON output and verify preview fields.
        let json: serde_json::Value =
            serde_json::from_str(&stdout).map_err(|error| error.to_string())?;
        ensure_equal(
            &json["schema"],
            &serde_json::json!("ee.response.v1"),
            "curate accept schema",
        )?;
        ensure_equal(
            &json["success"],
            &serde_json::json!(true),
            "curate accept success",
        )?;
        ensure_equal(
            &json["data"]["command"],
            &serde_json::json!("curate accept"),
            "curate accept command",
        )?;

        // Verify key fields match the expected curate review response structure.
        ensure_equal(
            &json["data"]["candidateId"],
            &serde_json::json!("curate_00000000000000000drytest01"),
            "curate accept candidate id",
        )?;

        // Verify dry-run indicator is present.
        ensure_equal(
            &json["data"]["dryRun"],
            &serde_json::json!(true),
            "curate accept dry-run flag",
        )?;

        // Verify mutation.persisted is false (confirming dry-run did not persist).
        ensure_equal(
            &json["data"]["mutation"]["persisted"],
            &serde_json::json!(false),
            "curate accept mutation.persisted",
        )?;

        // Verify mutation shows what would change.
        ensure_equal(
            &json["data"]["mutation"]["fromStatus"],
            &serde_json::json!("pending"),
            "curate accept mutation.fromStatus",
        )?;
        ensure_equal(
            &json["data"]["mutation"]["toStatus"],
            &serde_json::json!("approved"),
            "curate accept mutation.toStatus",
        )?;

        // Verify NO mutation occurred: re-open database and check candidate status.
        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        let candidate = connection
            .get_curation_candidate(&workspace_id, "curate_00000000000000000drytest01")
            .map_err(|error| format!("failed to get candidate: {error}"))?
            .ok_or_else(|| "candidate not found".to_string())?;

        // Status should still be "pending", not "approved".
        ensure_equal(
            &candidate.status,
            &"pending".to_string(),
            "candidate status after dry-run",
        )?;
        // Review state should still be "new", not "accepted".
        ensure_equal(
            &candidate.review_state,
            &"new".to_string(),
            "candidate review_state after dry-run",
        )?;
        // reviewed_at should be None since no actual review happened.
        ensure(
            candidate.reviewed_at.is_none(),
            format!(
                "candidate reviewed_at should be None after dry-run, got: {:?}",
                candidate.reviewed_at
            ),
        )?;

        connection.close().map_err(|error| error.to_string())?;

        Ok(())
    }

    #[test]
    fn rate_distortion_report_to_json_golden() -> TestResult {
        use ee::pack::{RateDistortionReport, SectionBudgetReport};

        let mut report = RateDistortionReport::new(4000, 3200).with_candidates(10, 5);
        report.add_section(
            SectionBudgetReport::new("procedural_rules", 1200, 1000).with_candidates(4),
        );
        report.add_section(SectionBudgetReport::new("evidence", 800, 600).with_candidates(3));
        let json = report.to_json();

        let value: serde_json::Value =
            serde_json::from_str(&json).map_err(|error| error.to_string())?;
        ensure(
            value["schema"].as_str().is_some(),
            "rate distortion JSON must have schema field",
        )?;
        ensure(
            value["budgetTokens"].as_u64() == Some(4000),
            "rate distortion JSON budgetTokens must be 4000",
        )?;
        ensure(
            value["usedTokens"].as_u64() == Some(3200),
            "rate distortion JSON usedTokens must be 3200",
        )?;
        ensure(
            value["slackTokens"].as_u64() == Some(800),
            "rate distortion JSON slackTokens must be 800",
        )?;
        ensure(
            value["sections"]
                .as_array()
                .is_some_and(|arr| arr.len() == 2),
            "rate distortion JSON must have 2 sections",
        )?;

        let pretty =
            serde_json::to_string_pretty(&value).map_err(|error| error.to_string())? + "\n";
        assert_golden("pack", "rate_distortion_report.json", &pretty)
    }

    #[test]
    fn section_budget_report_to_json_golden() -> TestResult {
        use ee::pack::SectionBudgetReport;

        let section =
            SectionBudgetReport::new("test \"quoted\" name", 1000, 750).with_candidates(3);
        let json = section.to_json();

        let value: serde_json::Value =
            serde_json::from_str(&json).map_err(|error| error.to_string())?;
        ensure(
            value["name"].as_str() == Some("test \"quoted\" name"),
            "section JSON must properly escape quotes in name",
        )?;
        ensure(
            value["quotaTokens"].as_u64() == Some(1000),
            "section JSON quotaTokens must be 1000",
        )?;
        ensure(
            value["usedTokens"].as_u64() == Some(750),
            "section JSON usedTokens must be 750",
        )?;
        ensure(
            value["slackTokens"].as_u64() == Some(250),
            "section JSON slackTokens must be 250",
        )?;

        let pretty =
            serde_json::to_string_pretty(&value).map_err(|error| error.to_string())? + "\n";
        assert_golden("pack", "section_budget_report.json", &pretty)
    }

    fn normalize_context_pack_json(json: &str) -> String {
        let mut value: serde_json::Value = match serde_json::from_str(json) {
            Ok(v) => v,
            Err(_) => return json.to_string(),
        };

        normalize_context_pack_json_strings(&mut value);

        if let Some(data) = value.get_mut("data") {
            if let Some(pack) = data.get_mut("pack") {
                if pack.get("elapsedMs").is_some() {
                    pack["elapsedMs"] = serde_json::json!(0.0);
                }
                if pack.get("hash").is_some() {
                    pack["hash"] = serde_json::json!("blake3:normalized-context-pack-hash");
                }
            }
        }

        serde_json::to_string_pretty(&value).unwrap_or_else(|_| json.to_string()) + "\n"
    }

    fn normalize_context_pack_json_strings(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::String(text) => {
                *text = normalize_context_pack_text(text);
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    normalize_context_pack_json_strings(item);
                }
            }
            serde_json::Value::Object(fields) => {
                for item in fields.values_mut() {
                    normalize_context_pack_json_strings(item);
                }
            }
            serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {
            }
        }
    }

    fn normalize_context_pack_text(text: &str) -> String {
        let mut normalized = normalize_context_pack_artifact_paths(text);
        normalized = normalize_context_pack_hash_comments(&normalized);
        normalized
    }

    fn normalize_context_pack_artifact_paths(text: &str) -> String {
        let marker = format!("{}/target/ee-golden-artifacts/", env!("CARGO_MANIFEST_DIR"));
        let mut normalized = text.to_owned();
        while let Some(start) = normalized.find(&marker) {
            let suffix_start = start + marker.len();
            let Some(workspace_offset) = normalized[suffix_start..].find("/workspace") else {
                break;
            };
            let end = suffix_start + workspace_offset + "/workspace".len();
            normalized.replace_range(start..end, "<ee-golden-workspace>");
        }
        normalized
    }

    fn normalize_context_pack_hash_comments(text: &str) -> String {
        let mut normalized = text.to_owned();
        let marker = "<!-- pack.hash: blake3:";
        while let Some(start) = normalized.find(marker) {
            let Some(end_offset) = normalized[start..].find("-->") else {
                break;
            };
            let end = start + end_offset + "-->".len();
            normalized.replace_range(
                start..end,
                "<!-- pack.hash: normalized-context-pack-hash -->",
            );
        }
        normalized
    }

    #[test]
    fn agent_why_json_explains_pack_selected_memory() -> TestResult {
        let artifact_dir = unique_artifact_dir("why-json")?;
        let workspace = artifact_dir.join("workspace");
        let database = workspace.join(".ee").join("ee.db");
        fs::create_dir_all(&workspace).map_err(|error| {
            format!(
                "failed to create workspace {}: {error}",
                workspace.display()
            )
        })?;

        seed_search_workspace(&workspace, &database)?;
        seed_pack_selection(&database)?;

        let output = Command::new(env!("CARGO_BIN_EXE_ee"))
            .arg("--json")
            .arg("--workspace")
            .arg(&workspace)
            .arg("why")
            .arg("mem_00000000000000000000000001")
            .arg("--database")
            .arg(&database)
            .output()
            .map_err(|error| format!("failed to run ee why --json: {error}"))?;

        let stdout = String::from_utf8(output.stdout)
            .map_err(|error| format!("why stdout was not UTF-8: {error}"))?;
        let stderr = String::from_utf8(output.stderr)
            .map_err(|error| format!("why stderr was not UTF-8: {error}"))?;

        ensure(
            output.status.success(),
            format!("why --json should succeed; stderr: {stderr}"),
        )?;
        ensure(
            stderr.is_empty(),
            format!("why --json stderr must be empty, got: {stderr:?}"),
        )?;
        ensure(
            stdout.starts_with('{'),
            format!("why stdout must start with JSON data, got: {stdout:?}"),
        )?;
        ensure(
            stdout.ends_with('\n'),
            format!("why stdout must end with a newline, got: {stdout:?}"),
        )?;

        let value: serde_json::Value =
            serde_json::from_str(&stdout).map_err(|error| error.to_string())?;
        ensure_equal(
            &value["schema"],
            &serde_json::json!("ee.response.v1"),
            "why schema",
        )?;
        ensure_equal(&value["success"], &serde_json::json!(true), "why success")?;
        ensure_equal(
            &value["data"]["command"],
            &serde_json::json!("why"),
            "why command",
        )?;
        ensure_equal(
            &value["data"]["found"],
            &serde_json::json!(true),
            "why found",
        )?;
        ensure_equal(
            &value["data"]["selection"]["latestPackSelection"]["packId"],
            &serde_json::json!("pack_00000000000000000000000001"),
            "why latest pack id",
        )?;
        ensure_equal(
            &value["data"]["selection"]["latestPackSelection"]["rank"],
            &serde_json::json!(1),
            "why latest pack rank",
        )?;

        assert_golden("agent", "why_selected.json", &stdout)
    }

    #[test]
    fn agent_outcome_json_records_feedback_and_audit() -> TestResult {
        let artifact_dir = unique_artifact_dir("outcome-json")?;
        let workspace = artifact_dir.join("workspace");
        let database = workspace.join(".ee").join("ee.db");
        fs::create_dir_all(&workspace).map_err(|error| {
            format!(
                "failed to create workspace {}: {error}",
                workspace.display()
            )
        })?;

        seed_search_workspace(&workspace, &database)?;

        let output = Command::new(env!("CARGO_BIN_EXE_ee"))
            .arg("--json")
            .arg("--workspace")
            .arg(&workspace)
            .arg("outcome")
            .arg("mem_00000000000000000000000001")
            .arg("--database")
            .arg(&database)
            .arg("--signal")
            .arg("helpful")
            .arg("--source-type")
            .arg("human_explicit")
            .arg("--source-id")
            .arg("golden-run")
            .arg("--reason")
            .arg("The memory prevented a release workflow mistake.")
            .arg("--evidence-json")
            .arg(r#"{"outcome":"success","redacted":true}"#)
            .arg("--event-id")
            .arg("fb_31234567890123456789012345")
            .arg("--actor")
            .arg("golden-test")
            .output()
            .map_err(|error| format!("failed to run ee outcome --json: {error}"))?;

        let stdout = String::from_utf8(output.stdout)
            .map_err(|error| format!("outcome stdout was not UTF-8: {error}"))?;
        let stderr = String::from_utf8(output.stderr)
            .map_err(|error| format!("outcome stderr was not UTF-8: {error}"))?;

        ensure(
            output.status.success(),
            format!("outcome --json should succeed; stderr: {stderr}"),
        )?;
        ensure(
            stderr.is_empty(),
            format!("outcome --json stderr must be empty, got: {stderr:?}"),
        )?;
        ensure(
            stdout.starts_with('{'),
            format!("outcome stdout must start with JSON data, got: {stdout:?}"),
        )?;
        ensure(
            stdout.ends_with('\n'),
            format!("outcome stdout must end with a newline, got: {stdout:?}"),
        )?;
        ensure(
            !stdout.contains(r#""redacted""#),
            "outcome output must not echo evidence JSON keys",
        )?;

        let value: serde_json::Value =
            serde_json::from_str(&stdout).map_err(|error| error.to_string())?;
        ensure_equal(
            &value["schema"],
            &serde_json::json!("ee.response.v1"),
            "outcome schema",
        )?;
        ensure_equal(
            &value["success"],
            &serde_json::json!(true),
            "outcome success",
        )?;
        ensure_equal(
            &value["data"]["command"],
            &serde_json::json!("outcome"),
            "outcome command",
        )?;
        ensure_equal(
            &value["data"]["status"],
            &serde_json::json!("recorded"),
            "outcome status",
        )?;
        ensure_equal(
            &value["data"]["event"]["id"],
            &serde_json::json!("fb_31234567890123456789012345"),
            "outcome event id",
        )?;
        ensure_equal(
            &value["data"]["event"]["evidenceJsonPresent"],
            &serde_json::json!(true),
            "outcome evidence presence",
        )?;
        ensure_equal(
            &value["data"]["feedback"]["totalCount"],
            &serde_json::json!(1),
            "outcome feedback count",
        )?;

        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        let event = connection
            .get_feedback_event("fb_31234567890123456789012345")
            .map_err(|error| error.to_string())?;
        ensure(event.is_some(), "feedback event must be durable")?;
        let audit = connection
            .list_audit_by_target("memory", "mem_00000000000000000000000001", None)
            .map_err(|error| error.to_string())?;
        ensure_equal(&audit.len(), &1_usize, "outcome audit row count")?;

        let normalized = normalize_outcome_json(&stdout);
        assert_golden("agent", "outcome_recorded.json", &normalized)
    }

    #[test]
    fn agent_feedback_hardening_binary_flow_protects_and_reviews_quarantine() -> TestResult {
        let artifact_dir = unique_artifact_dir("feedback-hardening")?;
        let workspace = artifact_dir.join("workspace");
        fs::create_dir_all(&workspace).map_err(|error| {
            format!(
                "failed to create workspace {}: {error}",
                workspace.display()
            )
        })?;
        let workspace_arg = workspace.to_string_lossy().into_owned();
        let database = workspace.join(".ee").join("ee.db");
        let database_arg = database.to_string_lossy().into_owned();

        run_json_stdout(&["--json", "--workspace", &workspace_arg, "init"], true)?;

        let remember = run_json_stdout(
            &[
                "--json",
                "--workspace",
                &workspace_arg,
                "remember",
                "Run cargo fmt --check before release.",
                "--level",
                "procedural",
                "--kind",
                "rule",
                "--source",
                "file://AGENTS.md#L164-173",
                "--confidence",
                "0.92",
            ],
            true,
        )?;
        let memory_id = remember["data"]["memory_id"]
            .as_str()
            .ok_or_else(|| "remember response missing memory_id".to_string())?
            .to_owned();

        let rule_add = run_json_stdout(
            &[
                "--json",
                "--workspace",
                &workspace_arg,
                "rule",
                "add",
                "Require distinct harmful sources before rule inversion.",
                "--database",
                &database_arg,
                "--source-memory",
                &memory_id,
                "--maturity",
                "candidate",
                "--actor",
                "golden-test",
            ],
            true,
        )?;
        let rule_id = rule_add["data"]["ruleId"]
            .as_str()
            .ok_or_else(|| "rule add response missing ruleId".to_string())?
            .to_owned();

        let protected = run_json_stdout(
            &[
                "--json",
                "--workspace",
                &workspace_arg,
                "rule",
                "protect",
                &rule_id,
                "--database",
                &database_arg,
                "--actor",
                "golden-test",
            ],
            true,
        )?;
        ensure_equal(
            &protected["data"]["status"],
            &serde_json::json!("updated"),
            "rule protect status",
        )?;
        ensure_equal(
            &protected["data"]["protected"],
            &serde_json::json!(true),
            "rule protect marker",
        )?;
        ensure_equal(
            &protected["data"]["previousProtected"],
            &serde_json::json!(false),
            "rule protect previous marker",
        )?;
        ensure(
            protected["data"]["auditId"].is_string(),
            "rule protect audit id must be present",
        )?;

        let first_harmful = run_json_stdout(
            &[
                "--json",
                "--workspace",
                &workspace_arg,
                "outcome",
                &memory_id,
                "--database",
                &database_arg,
                "--signal",
                "harmful",
                "--source-type",
                "outcome_observed",
                "--source-id",
                "golden-source",
                "--reason",
                "First harmful signal stays live.",
                "--event-id",
                "fb_41234567890123456789012345",
                "--actor",
                "golden-test",
                "--harmful-per-source-per-hour",
                "1",
            ],
            true,
        )?;
        ensure_equal(
            &first_harmful["data"]["status"],
            &serde_json::json!("recorded"),
            "first harmful status",
        )?;

        let quarantined = run_json_stdout(
            &[
                "--json",
                "--workspace",
                &workspace_arg,
                "outcome",
                &memory_id,
                "--database",
                &database_arg,
                "--signal",
                "harmful",
                "--source-type",
                "outcome_observed",
                "--source-id",
                "golden-source",
                "--reason",
                "Second harmful signal must be quarantined.",
                "--evidence-json",
                r#"{"fixture":"feedback-hardening"}"#,
                "--event-id",
                "fb_51234567890123456789012345",
                "--actor",
                "golden-test",
                "--harmful-per-source-per-hour",
                "1",
            ],
            true,
        )?;
        ensure_equal(
            &quarantined["data"]["status"],
            &serde_json::json!("feedback_quarantined"),
            "second harmful status",
        )?;
        ensure_equal(
            &quarantined["data"]["feedback"]["totalCount"],
            &serde_json::json!(1),
            "quarantined event must not affect live feedback count",
        )?;
        let quarantine_id = quarantined["data"]["quarantine"]["id"]
            .as_str()
            .ok_or_else(|| "quarantined response missing quarantine id".to_string())?
            .to_owned();
        ensure(
            quarantine_id.starts_with("fq_"),
            "quarantine id must use feedback quarantine prefix",
        )?;

        let pending = run_json_stdout(
            &[
                "--json",
                "--workspace",
                &workspace_arg,
                "outcome",
                "quarantine",
                "list",
                "--database",
                &database_arg,
            ],
            true,
        )?;
        ensure_equal(
            &pending["data"]["queueDepth"],
            &serde_json::json!(1),
            "pending quarantine depth",
        )?;
        ensure_equal(
            &pending["data"]["records"][0]["id"],
            &serde_json::json!(quarantine_id),
            "pending quarantine id",
        )?;
        ensure(
            pending["data"]["records"][0]["rawEventHash"]
                .as_str()
                .is_some_and(|hash| hash.starts_with("blake3:")),
            "pending quarantine raw event hash",
        )?;

        let released = run_json_stdout(
            &[
                "--json",
                "--workspace",
                &workspace_arg,
                "outcome",
                "quarantine",
                "release",
                &quarantine_id,
                "--database",
                &database_arg,
                "--actor",
                "golden-test",
            ],
            true,
        )?;
        ensure_equal(
            &released["data"]["status"],
            &serde_json::json!("released"),
            "quarantine release status",
        )?;
        ensure_equal(
            &released["data"]["feedbackEventId"],
            &serde_json::json!("fb_51234567890123456789012345"),
            "released feedback event id",
        )?;
        ensure(
            released["data"]["auditId"].is_string(),
            "quarantine release audit id must be present",
        )?;

        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        let feedback = connection
            .list_feedback_events_for_target("memory", &memory_id)
            .map_err(|error| error.to_string())?;
        ensure_equal(&feedback.len(), &2_usize, "released feedback count")?;
        let released_rows = connection
            .list_feedback_quarantine(
                pending["data"]["workspaceId"]
                    .as_str()
                    .ok_or_else(|| "pending list missing workspaceId".to_string())?,
                Some("released"),
            )
            .map_err(|error| error.to_string())?;
        ensure_equal(
            &released_rows.len(),
            &1_usize,
            "released quarantine row count",
        )
    }

    fn normalize_outcome_json(json: &str) -> String {
        let mut value: serde_json::Value = match serde_json::from_str(json) {
            Ok(v) => v,
            Err(_) => return json.to_string(),
        };

        if let Some(audit_id) = value
            .get_mut("data")
            .and_then(|data| data.get_mut("event"))
            .and_then(|event| event.get_mut("auditId"))
        {
            *audit_id = serde_json::json!("audit_DYNAMIC");
        }

        serde_json::to_string_pretty(&value).unwrap_or_else(|_| json.to_string()) + "\n"
    }

    #[test]
    fn agent_context_unavailable_json_matches_golden() -> TestResult {
        assert_agent_stdout_golden(
            &[
                "--json",
                "--workspace",
                "tests/fixtures/missing-ee-workspace",
                "context",
                "prepare-release",
            ],
            "context_unavailable.json",
            false,
        )
    }

    #[test]
    fn agent_api_version_unavailable_json_matches_golden() -> TestResult {
        assert_agent_stdout_golden(
            &["--json", "api-version"],
            "api_version_unavailable.json",
            false,
        )
    }

    // =========================================================================
    // Degradation Matrix Contract Tests (EE-311)
    // =========================================================================

    fn degradation_matrix_json() -> String {
        use ee::models::degradation::ALL_DEGRADATION_CODES;

        let codes: Vec<serde_json::Value> = ALL_DEGRADATION_CODES
            .iter()
            .map(|code| {
                serde_json::json!({
                    "id": code.id,
                    "subsystem": code.subsystem.as_str(),
                    "severity": code.severity.as_str(),
                    "description": code.description,
                    "behavior_change": code.behavior_change,
                    "auto_recoverable": code.auto_recoverable,
                    "repair": code.repair,
                })
            })
            .collect();

        let matrix = serde_json::json!({
            "schema": "ee.degradation_matrix.v1",
            "count": codes.len(),
            "codes": codes,
        });

        let mut json = serde_json::to_string_pretty(&matrix).unwrap_or_default();
        json.push('\n');
        json
    }

    #[test]
    fn degradation_matrix_matches_golden() -> TestResult {
        let json = degradation_matrix_json();
        assert_golden("degradation", "matrix.json", &json)
    }

    #[test]
    fn degradation_matrix_all_codes_have_required_fields() -> TestResult {
        use ee::models::degradation::ALL_DEGRADATION_CODES;

        for code in ALL_DEGRADATION_CODES {
            ensure(!code.id.is_empty(), format!("code {:?} has empty id", code))?;
            ensure(
                code.id.starts_with('D'),
                format!("code {} id must start with 'D'", code.id),
            )?;
            ensure(
                !code.description.is_empty(),
                format!("code {} has empty description", code.id),
            )?;
            ensure(
                !code.behavior_change.is_empty(),
                format!("code {} has empty behavior_change", code.id),
            )?;
        }
        Ok(())
    }

    #[test]
    fn degradation_matrix_ids_are_unique() -> TestResult {
        use ee::models::degradation::ALL_DEGRADATION_CODES;
        use std::collections::HashSet;

        let mut seen = HashSet::new();
        for code in ALL_DEGRADATION_CODES {
            ensure(
                seen.insert(code.id),
                format!("duplicate degradation code id: {}", code.id),
            )?;
        }
        Ok(())
    }

    #[test]
    fn degradation_matrix_ids_are_sorted_by_number() -> TestResult {
        use ee::models::degradation::ALL_DEGRADATION_CODES;

        let numbers: Vec<u16> = ALL_DEGRADATION_CODES.iter().map(|c| c.number()).collect();
        for window in numbers.windows(2) {
            ensure(
                window[0] <= window[1],
                format!(
                    "degradation codes out of order: D{:03} > D{:03}",
                    window[0], window[1]
                ),
            )?;
        }
        Ok(())
    }

    #[test]
    fn degradation_matrix_honesty_checks_pass() -> TestResult {
        use ee::core::degraded_honesty::validate_all_codes;

        let report = validate_all_codes();
        if report.passed {
            Ok(())
        } else {
            let failures: Vec<String> = report
                .checks
                .iter()
                .filter(|c| !c.passed)
                .map(|c| {
                    format!(
                        "{}: {} (code: {:?})",
                        c.check_name,
                        c.issue.as_deref().unwrap_or("no details"),
                        c.code_id
                    )
                })
                .collect();
            Err(format!(
                "Honesty check failed with {} issues:\n{}",
                report.issue_count,
                failures.join("\n")
            ))
        }
    }

    #[test]
    fn degradation_matrix_repair_commands_are_valid() -> TestResult {
        use ee::core::degraded_honesty::validate_repair_command;
        use ee::models::degradation::ALL_DEGRADATION_CODES;

        for code in ALL_DEGRADATION_CODES {
            if let Some(repair) = code.repair {
                let result = validate_repair_command(repair);
                ensure(
                    result.passed,
                    format!(
                        "code {} has invalid repair command '{}': {}",
                        code.id,
                        repair,
                        result.issue.unwrap_or_default()
                    ),
                )?;
            }
        }
        Ok(())
    }

    #[test]
    fn degradation_matrix_subsystem_coverage() -> TestResult {
        use ee::models::degradation::{ALL_DEGRADATION_CODES, DegradedSubsystem};
        use std::collections::HashSet;

        let expected_subsystems = [
            DegradedSubsystem::Search,
            DegradedSubsystem::Storage,
            DegradedSubsystem::Cass,
            DegradedSubsystem::Graph,
            DegradedSubsystem::Pack,
            DegradedSubsystem::Curate,
            DegradedSubsystem::Policy,
            DegradedSubsystem::Network,
            DegradedSubsystem::Science,
        ];

        let covered: HashSet<&str> = ALL_DEGRADATION_CODES
            .iter()
            .map(|c| c.subsystem.as_str())
            .collect();

        for subsystem in &expected_subsystems {
            ensure(
                covered.contains(subsystem.as_str()),
                format!("subsystem {} has no degradation codes", subsystem.as_str()),
            )?;
        }
        Ok(())
    }
}
