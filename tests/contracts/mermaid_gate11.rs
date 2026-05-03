//! Gate 11 Mermaid contract coverage.
//!
//! Freezes deterministic diagram output for graph export, why explanations,
//! doctor diagnostics, and curation promotion plans. These fixtures keep
//! diagram renderers offline and text-only while protecting stable node labels
//! and edge ordering.

use ee::core::doctor::{CheckResult, CheckSeverity, DoctorReport};
use ee::core::procedure::{
    PROCEDURE_PROMOTE_REPORT_SCHEMA_V1, PROCEDURE_PROMOTION_AUDIT_SCHEMA_V1,
    PROCEDURE_PROMOTION_CURATION_SCHEMA_V1, ProcedurePromoteReport, ProcedurePromotionAuditPlan,
    ProcedurePromotionCurationPlan, ProcedurePromotionEffect,
    ProcedurePromotionVerificationSummary,
};
use ee::core::why::{
    MemoryLinkSummary, PackSelectionExplanation, RationaleTraceSummary, RetrievalExplanation,
    SelectionExplanation, StorageExplanation, WhyDegradation, WhyReport,
};
use ee::graph::{
    GRAPH_EXPORT_SCHEMA_V1, GraphExportFormat, GraphExportReport, GraphExportSnapshot,
    GraphExportStatus,
};
use ee::output::{render_doctor_mermaid, render_procedure_promote_mermaid, render_why_mermaid};
use std::env;
use std::fs;
use std::path::PathBuf;

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("golden")
        .join("mermaid")
        .join(format!("{name}.mmd.golden"))
}

fn assert_golden(name: &str, actual: &str) -> TestResult {
    let path = golden_path(name);
    if env::var("UPDATE_GOLDEN").is_ok() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                format!("failed to create golden dir {}: {error}", parent.display())
            })?;
        }
        fs::write(&path, actual)
            .map_err(|error| format!("failed to write golden {}: {error}", path.display()))?;
        eprintln!("Updated golden file: {}", path.display());
        return Ok(());
    }

    let expected = fs::read_to_string(&path)
        .map_err(|error| format!("missing golden {}: {error}", path.display()))?;
    ensure(
        actual == expected,
        format!(
            "mermaid golden mismatch for {name}\n--- expected\n{expected}\n+++ actual\n{actual}"
        ),
    )
}

fn graph_export_fixture() -> GraphExportReport {
    GraphExportReport {
        schema: GRAPH_EXPORT_SCHEMA_V1,
        version: env!("CARGO_PKG_VERSION"),
        status: GraphExportStatus::Exported,
        format: GraphExportFormat::Mermaid,
        workspace_id: "wsp_gate11_mermaid".to_string(),
        graph_type: "memory_links".to_string(),
        snapshot: Some(GraphExportSnapshot {
            id: "gsnap_gate11_mermaid".to_string(),
            schema_version: "ee.graph.snapshot.v1".to_string(),
            snapshot_version: 1,
            source_generation: 7,
            status: "active".to_string(),
            content_hash: "blake3:gate11graphhash".to_string(),
            created_at: "2026-01-02T03:04:05Z".to_string(),
        }),
        node_count: 3,
        edge_count: 2,
        diagram: concat!(
            "flowchart TD\n",
            "  n1[\"Release rule\"]\n",
            "  n2[\"Prior incident\"]\n",
            "  n3[\"Verification checklist\"]\n",
            "  n1 -->|supported by| n2\n",
            "  n1 -->|requires| n3\n",
        )
        .to_string(),
        degraded: Vec::new(),
    }
}

fn why_fixture() -> WhyReport {
    WhyReport::found(
        "mem_gate11_release_rule".to_string(),
        StorageExplanation {
            origin: "remember".to_string(),
            trust_class: "human_explicit".to_string(),
            trust_subclass: Some("release_rule".to_string()),
            provenance_uri: Some("cass://session-gate11#L40-L44".to_string()),
            created_at: "2026-01-02T03:04:05Z".to_string(),
            valid_from: None,
            valid_to: None,
            validity_status: "valid".to_string(),
            validity_window_kind: "unbounded".to_string(),
        },
        RetrievalExplanation {
            confidence: 0.91,
            utility: 0.84,
            importance: 0.88,
            tags: vec!["release".to_string(), "verification".to_string()],
            level: "procedural".to_string(),
            kind: "rule".to_string(),
        },
        SelectionExplanation {
            selection_score: 9.04,
            above_confidence_threshold: true,
            is_active: true,
            score_breakdown: "confidence + recency + profile bonus".to_string(),
            latest_pack_selection: Some(PackSelectionExplanation {
                pack_id: "pack_gate11_release".to_string(),
                query: "prepare release".to_string(),
                profile: "release".to_string(),
                rank: 2,
                section: "Project Rules".to_string(),
                estimated_tokens: 21,
                relevance: 0.88,
                utility: 0.84,
                why: "release profile matched procedural rule".to_string(),
                pack_hash: "blake3:gate11packhash".to_string(),
                selected_at: "2026-01-02T03:05:06Z".to_string(),
            }),
        },
    )
    .with_links(vec![MemoryLinkSummary {
        link_id: "mlink_gate11_supports".to_string(),
        linked_memory_id: "mem_gate11_prior_incident".to_string(),
        relation: "supports".to_string(),
        direction: "outgoing".to_string(),
        confidence: 0.77,
        weight: 0.61,
        evidence_count: 3,
        source: "agent".to_string(),
        created_at: "2026-01-02T03:06:07Z".to_string(),
    }])
    .with_rationale_traces(vec![RationaleTraceSummary {
        schema: ee::models::RATIONALE_TRACE_SCHEMA_V1,
        trace_id: "rat_gate11_release_decision".to_string(),
        kind: "decision".to_string(),
        posture: "supported".to_string(),
        visibility: "public".to_string(),
        author: "agent:gate11".to_string(),
        summary: "Visible release evidence explains why the rule was selected.".to_string(),
        confidence_basis_points: 8400,
        evidence_uris: vec!["cass://session-gate11#L40-L44".to_string()],
        linked_memory_ids: vec!["mem_gate11_release_rule".to_string()],
        linked_context_pack_ids: vec!["pack_gate11_release".to_string()],
        linked_recorder_run_ids: Vec::new(),
        linked_recorder_event_ids: Vec::new(),
        linked_causal_trace_ids: Vec::new(),
        supersedes_trace_ids: Vec::new(),
        contradicted_by_trace_ids: Vec::new(),
        created_at: "2026-01-02T03:06:30Z".to_string(),
    }])
    .with_degradation(WhyDegradation {
        code: "graph_snapshot_stale",
        severity: "medium",
        message: "Graph boost omitted from latest explanation.".to_string(),
        repair: Some("ee graph centrality-refresh".to_string()),
    })
}

fn doctor_fixture() -> DoctorReport {
    DoctorReport {
        version: env!("CARGO_PKG_VERSION"),
        overall_healthy: false,
        checks: vec![
            CheckResult {
                name: "database",
                severity: CheckSeverity::Ok,
                message: "database opened".to_string(),
                error_code: None,
                repair: None,
            },
            CheckResult {
                name: "search_index",
                severity: CheckSeverity::Warning,
                message: "index generation lags database".to_string(),
                error_code: None,
                repair: Some("ee index rebuild --workspace ."),
            },
            CheckResult {
                name: "cass",
                severity: CheckSeverity::Error,
                message: "cass binary not found".to_string(),
                error_code: None,
                repair: Some("install cass or disable cass.enabled"),
            },
        ],
    }
}

fn curation_fixture() -> ProcedurePromoteReport {
    ProcedurePromoteReport {
        schema: PROCEDURE_PROMOTE_REPORT_SCHEMA_V1.to_string(),
        promotion_id: "pprom_gate11_release".to_string(),
        procedure_id: "proc_gate11_release".to_string(),
        dry_run: true,
        status: "ready_for_review".to_string(),
        from_status: "candidate".to_string(),
        to_status: "verified".to_string(),
        curation: ProcedurePromotionCurationPlan {
            schema: PROCEDURE_PROMOTION_CURATION_SCHEMA_V1.to_string(),
            candidate_id: "curate_gate11_promote".to_string(),
            candidate_type: "promote".to_string(),
            target_type: "procedure".to_string(),
            target_id: "proc_gate11_release".to_string(),
            target_title: "Stabilize release workflow".to_string(),
            source_type: "procedure_verification".to_string(),
            source_id: Some("ver_gate11_release".to_string()),
            reason: "verification passed with enough evidence".to_string(),
            confidence: 0.82,
            evidence_ids: vec!["ev_gate11_ci_trace".to_string()],
            status: "pending_review".to_string(),
            would_persist: false,
            applied: false,
        },
        audit: ProcedurePromotionAuditPlan {
            schema: PROCEDURE_PROMOTION_AUDIT_SCHEMA_V1.to_string(),
            operation_id: "audit_gate11_promote".to_string(),
            action: "procedure_promote".to_string(),
            effect_class: "curation_plan".to_string(),
            outcome: "planned".to_string(),
            dry_run: true,
            actor: "gate11-test".to_string(),
            target_type: "procedure".to_string(),
            target_id: "proc_gate11_release".to_string(),
            changed_surfaces: vec!["procedures".to_string(), "curation_candidates".to_string()],
            transaction_status: "not_started".to_string(),
            would_record: true,
            recorded: false,
        },
        verification: ProcedurePromotionVerificationSummary {
            verification_id: "ver_gate11_release".to_string(),
            status: "passed".to_string(),
            overall_result: "passed".to_string(),
            pass_count: 2,
            fail_count: 0,
            skip_count: 0,
            confidence: 0.82,
            evidence_checked: vec!["ev_gate11_ci_trace".to_string()],
        },
        planned_effects: vec![
            ProcedurePromotionEffect {
                surface: "procedures".to_string(),
                operation: "update".to_string(),
                target_id: "proc_gate11_release".to_string(),
                before: Some("candidate".to_string()),
                after: Some("verified".to_string()),
                would_write: true,
                applied: false,
            },
            ProcedurePromotionEffect {
                surface: "curation_candidates".to_string(),
                operation: "insert".to_string(),
                target_id: "curate_gate11_promote".to_string(),
                before: None,
                after: Some("pending_review".to_string()),
                would_write: true,
                applied: false,
            },
        ],
        warnings: Vec::new(),
        next_actions: vec!["ee procedure show proc_gate11_release --json".to_string()],
        generated_at: "2026-01-02T03:07:08Z".to_string(),
    }
}

#[test]
fn gate11_graph_export_mermaid_matches_golden() -> TestResult {
    let report = graph_export_fixture();
    ensure(
        report.data_json()["artifact"]["mediaType"] == "text/vnd.mermaid",
        "graph export must advertise Mermaid media type",
    )?;
    ensure(
        report.diagram.starts_with("flowchart TD\n"),
        "graph export must render Mermaid flowchart",
    )?;
    assert_golden("graph_export", &report.diagram)
}

#[test]
fn gate11_why_mermaid_matches_golden() -> TestResult {
    let rendered = render_why_mermaid(&why_fixture());
    ensure(
        rendered.starts_with("flowchart TD\n"),
        "why Mermaid must render flowchart",
    )?;
    assert_golden("why_explanation", &rendered)
}

#[test]
fn gate11_doctor_mermaid_matches_golden() -> TestResult {
    let rendered = render_doctor_mermaid(&doctor_fixture());
    ensure(
        rendered.contains("repair"),
        "doctor Mermaid must retain repair edges",
    )?;
    assert_golden("doctor_diagnostics", &rendered)
}

#[test]
fn gate11_curation_mermaid_matches_golden() -> TestResult {
    let rendered = render_procedure_promote_mermaid(&curation_fixture());
    ensure(
        rendered.contains("curation"),
        "curation Mermaid must include curation node",
    )?;
    assert_golden("curation_promotion", &rendered)
}
