use serde::{Deserialize, Serialize};

pub const COMPLETION_AUDIT_CHECKLIST_SCHEMA_V1: &str = "ee.completion_audit.checklist.v1";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequirementKind {
    DocumentationRead,
    CodeInvestigation,
    Coordination,
    Tracker,
    Verification,
    Command,
    FileArtifact,
    SkillApplication,
    CompletionAudit,
    PermissionGate,
    Unknown,
}

impl RequirementKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DocumentationRead => "documentation_read",
            Self::CodeInvestigation => "code_investigation",
            Self::Coordination => "coordination",
            Self::Tracker => "tracker",
            Self::Verification => "verification",
            Self::Command => "command",
            Self::FileArtifact => "file_artifact",
            Self::SkillApplication => "skill_application",
            Self::CompletionAudit => "completion_audit",
            Self::PermissionGate => "permission_gate",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChecklistSource {
    pub label: String,
    pub kind: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceSpan {
    pub label: String,
    pub start: usize,
    pub end: usize,
    pub text: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceExpectation {
    pub kind: String,
    pub target: String,
    pub strength: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationExpectation {
    pub kind: String,
    pub target: String,
    pub required: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletionRequirement {
    pub id: String,
    pub kind: RequirementKind,
    pub summary: String,
    pub evidence_expectations: Vec<EvidenceExpectation>,
    pub verification_expectations: Vec<VerificationExpectation>,
    pub source_spans: Vec<SourceSpan>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnknownClause {
    pub id: String,
    pub reason: String,
    pub source_span: SourceSpan,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChecklistSummary {
    pub requirement_count: usize,
    pub unknown_count: usize,
    pub has_unknowns: bool,
    pub source_bytes: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletionChecklist {
    pub schema: String,
    pub source: ChecklistSource,
    pub objective_hash: String,
    pub objective_text: String,
    pub requirements: Vec<CompletionRequirement>,
    pub unknown_clauses: Vec<UnknownClause>,
    pub summary: ChecklistSummary,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceRecordStatus {
    Pass,
    Fail,
    CapacityBlocked,
    StaticOnly,
    Stale,
    Missing,
    Inconclusive,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequirementSupport {
    Direct,
    Supporting,
    Weak,
    Blocked,
    Stale,
    Missing,
    Contradicted,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceRecord {
    pub kind: String,
    pub target: String,
    pub source: String,
    pub status: EvidenceRecordStatus,
    pub strength: String,
    pub summary: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceBundle {
    pub records: Vec<EvidenceRecord>,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MissingExpectation {
    pub kind: String,
    pub target: String,
    pub required: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequirementEvidence {
    pub requirement_id: String,
    pub support: RequirementSupport,
    pub confidence: String,
    pub evidence_records: Vec<EvidenceRecord>,
    pub missing_expectations: Vec<MissingExpectation>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimContradictionKind {
    StaleAcceptance,
    UnsupportedDuplicateClaim,
    VerifierInconclusive,
    SourceDocsConflict,
    PermissionGateUnresolved,
    NeedsOwnerDecision,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaimContradiction {
    pub kind: ClaimContradictionKind,
    pub target: String,
    pub blocks_closure: bool,
    pub suggested_tracker_action: String,
    pub evidence_records: Vec<EvidenceRecord>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RequirementAccumulator {
    kind: RequirementKind,
    summary: String,
    evidence_expectations: Vec<EvidenceExpectation>,
    verification_expectations: Vec<VerificationExpectation>,
    source_spans: Vec<SourceSpan>,
}

#[derive(Clone, Copy, Debug)]
struct PhraseRequirementSpec<'a> {
    phrases: &'a [&'a str],
    kind: RequirementKind,
    summary: &'a str,
    evidence: EvidenceSpec<'a>,
    verification: VerificationSpec<'a>,
}

#[derive(Clone, Copy, Debug)]
struct EvidenceSpec<'a> {
    kind: &'a str,
    target: &'a str,
    strength: &'a str,
}

#[derive(Clone, Copy, Debug)]
struct VerificationSpec<'a> {
    kind: &'a str,
    target: &'a str,
    required: bool,
}

#[must_use]
pub fn extract_completion_checklist(
    source_label: &str,
    objective_text: &str,
) -> CompletionChecklist {
    let label = if source_label.trim().is_empty() {
        "objective"
    } else {
        source_label.trim()
    };
    let mut requirements = Vec::new();
    let mut unknowns = Vec::new();

    add_literal_documentation_requirements(&mut requirements, label, objective_text);
    add_keyword_requirements(&mut requirements, label, objective_text);
    add_backtick_requirements(&mut requirements, label, objective_text);
    add_token_requirements(&mut requirements, label, objective_text);
    add_unknown_clauses(&mut unknowns, label, objective_text);

    if objective_text.trim().is_empty() {
        unknowns.push(UnknownClause {
            id: String::new(),
            reason: "empty_objective".to_owned(),
            source_span: SourceSpan {
                label: label.to_owned(),
                start: 0,
                end: 0,
                text: String::new(),
            },
        });
    }

    requirements.sort_by(|left, right| {
        first_span_start(&left.source_spans)
            .cmp(&first_span_start(&right.source_spans))
            .then_with(|| left.kind.cmp(&right.kind))
            .then_with(|| left.summary.cmp(&right.summary))
    });
    unknowns.sort_by(|left, right| {
        left.source_span
            .start
            .cmp(&right.source_span.start)
            .then_with(|| left.reason.cmp(&right.reason))
    });

    let requirements = requirements
        .into_iter()
        .enumerate()
        .map(|(index, mut item)| {
            item.source_spans.sort();
            item.source_spans.dedup();
            item.evidence_expectations.sort();
            item.evidence_expectations.dedup();
            item.verification_expectations.sort();
            item.verification_expectations.dedup();
            CompletionRequirement {
                id: format!("req_{:03}", index + 1),
                kind: item.kind,
                summary: item.summary,
                evidence_expectations: item.evidence_expectations,
                verification_expectations: item.verification_expectations,
                source_spans: item.source_spans,
            }
        })
        .collect::<Vec<_>>();

    let unknown_clauses = unknowns
        .into_iter()
        .enumerate()
        .map(|(index, mut clause)| {
            clause.id = format!("unk_{:03}", index + 1);
            clause
        })
        .collect::<Vec<_>>();

    CompletionChecklist {
        schema: COMPLETION_AUDIT_CHECKLIST_SCHEMA_V1.to_owned(),
        source: ChecklistSource {
            label: label.to_owned(),
            kind: "objective_text".to_owned(),
        },
        objective_hash: objective_hash(objective_text),
        objective_text: objective_text.to_owned(),
        summary: ChecklistSummary {
            requirement_count: requirements.len(),
            unknown_count: unknown_clauses.len(),
            has_unknowns: !unknown_clauses.is_empty(),
            source_bytes: objective_text.len(),
        },
        requirements,
        unknown_clauses,
    }
}

#[must_use]
pub fn evaluate_completion_evidence(
    checklist: &CompletionChecklist,
    bundle: &EvidenceBundle,
) -> Vec<RequirementEvidence> {
    checklist
        .requirements
        .iter()
        .map(|requirement| evaluate_requirement_evidence(requirement, bundle))
        .collect()
}

fn evaluate_requirement_evidence(
    requirement: &CompletionRequirement,
    bundle: &EvidenceBundle,
) -> RequirementEvidence {
    let mut records = Vec::new();
    let mut missing = Vec::new();

    for expectation in &requirement.evidence_expectations {
        let matches = matching_records(bundle, &expectation.kind, &expectation.target);
        if matches.is_empty() {
            missing.push(MissingExpectation {
                kind: expectation.kind.clone(),
                target: expectation.target.clone(),
                required: expectation.strength == "direct",
            });
        }
        records.extend(matches);
    }

    for expectation in &requirement.verification_expectations {
        let matches = matching_records(bundle, &expectation.kind, &expectation.target);
        if matches.is_empty() {
            missing.push(MissingExpectation {
                kind: expectation.kind.clone(),
                target: expectation.target.clone(),
                required: expectation.required,
            });
        }
        records.extend(matches);
    }

    records.sort();
    records.dedup();
    missing.sort();
    missing.dedup();

    let support = classify_requirement_support(&records, &missing);
    RequirementEvidence {
        requirement_id: requirement.id.clone(),
        support,
        confidence: support_confidence(support).to_owned(),
        evidence_records: records,
        missing_expectations: missing,
    }
}

fn matching_records(bundle: &EvidenceBundle, kind: &str, target: &str) -> Vec<EvidenceRecord> {
    bundle
        .records
        .iter()
        .filter(|record| record.kind == kind && target_matches(target, &record.target))
        .cloned()
        .collect()
}

fn target_matches(expected: &str, observed: &str) -> bool {
    expected == observed || observed == "*" || expected == "*"
}

fn classify_requirement_support(
    records: &[EvidenceRecord],
    missing: &[MissingExpectation],
) -> RequirementSupport {
    if records
        .iter()
        .any(|record| record.status == EvidenceRecordStatus::Fail)
    {
        return RequirementSupport::Contradicted;
    }
    if records
        .iter()
        .any(|record| record.status == EvidenceRecordStatus::CapacityBlocked)
    {
        return RequirementSupport::Blocked;
    }
    if records
        .iter()
        .any(|record| record.status == EvidenceRecordStatus::Stale)
    {
        return RequirementSupport::Stale;
    }
    if records
        .iter()
        .any(|record| record.status == EvidenceRecordStatus::StaticOnly)
    {
        return RequirementSupport::Weak;
    }
    if !missing.is_empty() {
        return RequirementSupport::Missing;
    }
    if records
        .iter()
        .any(|record| record.status == EvidenceRecordStatus::Inconclusive)
    {
        return RequirementSupport::Weak;
    }
    if records
        .iter()
        .any(|record| record.status == EvidenceRecordStatus::Pass && record.strength == "direct")
    {
        return RequirementSupport::Direct;
    }
    if records.iter().any(|record| {
        record.status == EvidenceRecordStatus::Pass && record.strength == "supporting"
    }) {
        return RequirementSupport::Supporting;
    }
    RequirementSupport::Missing
}

fn support_confidence(support: RequirementSupport) -> &'static str {
    match support {
        RequirementSupport::Direct => "direct",
        RequirementSupport::Supporting => "supporting",
        RequirementSupport::Weak => "weak",
        RequirementSupport::Blocked => "blocked",
        RequirementSupport::Stale => "stale",
        RequirementSupport::Missing => "missing",
        RequirementSupport::Contradicted => "contradicted",
    }
}

#[must_use]
pub fn detect_claim_contradictions(bundle: &EvidenceBundle) -> Vec<ClaimContradiction> {
    let mut contradictions = Vec::new();

    push_status_issue(
        &mut contradictions,
        bundle,
        ClaimContradictionKind::StaleAcceptance,
        &["acceptance", "beads_acceptance"],
        &[EvidenceRecordStatus::Stale],
        true,
    );
    push_status_issue(
        &mut contradictions,
        bundle,
        ClaimContradictionKind::UnsupportedDuplicateClaim,
        &["duplicate_claim", "duplicate_audit"],
        &[EvidenceRecordStatus::Fail, EvidenceRecordStatus::Missing],
        true,
    );
    push_verifier_inconclusive_issues(&mut contradictions, bundle);
    push_status_issue(
        &mut contradictions,
        bundle,
        ClaimContradictionKind::SourceDocsConflict,
        &["source_docs", "docs_contract"],
        &[EvidenceRecordStatus::Fail],
        true,
    );
    push_status_issue(
        &mut contradictions,
        bundle,
        ClaimContradictionKind::PermissionGateUnresolved,
        &["permission_record", "permission_gate"],
        &[
            EvidenceRecordStatus::Missing,
            EvidenceRecordStatus::Inconclusive,
        ],
        true,
    );
    push_status_issue(
        &mut contradictions,
        bundle,
        ClaimContradictionKind::NeedsOwnerDecision,
        &["scope_decision", "owner_decision"],
        &[
            EvidenceRecordStatus::Missing,
            EvidenceRecordStatus::Inconclusive,
        ],
        true,
    );

    contradictions.sort_by(|left, right| {
        left.kind
            .cmp(&right.kind)
            .then_with(|| left.target.cmp(&right.target))
            .then_with(|| {
                left.suggested_tracker_action
                    .cmp(&right.suggested_tracker_action)
            })
    });
    contradictions
}

fn push_status_issue(
    contradictions: &mut Vec<ClaimContradiction>,
    bundle: &EvidenceBundle,
    kind: ClaimContradictionKind,
    record_kinds: &[&str],
    statuses: &[EvidenceRecordStatus],
    blocks_closure: bool,
) {
    let records = bundle
        .records
        .iter()
        .filter(|record| {
            record_kinds.contains(&record.kind.as_str()) && statuses.contains(&record.status)
        })
        .cloned()
        .collect::<Vec<_>>();
    push_grouped_issues(contradictions, kind, records, blocks_closure);
}

fn push_verifier_inconclusive_issues(
    contradictions: &mut Vec<ClaimContradiction>,
    bundle: &EvidenceBundle,
) {
    let records = bundle
        .records
        .iter()
        .filter(|record| verifier_record_kind(&record.kind))
        .cloned()
        .collect::<Vec<_>>();
    let mut inconclusive_records = records
        .iter()
        .filter(|record| {
            matches!(
                record.status,
                EvidenceRecordStatus::Inconclusive | EvidenceRecordStatus::Missing
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    inconclusive_records.retain(|record| !has_direct_verifier_pass(&records, &record.target));
    push_grouped_issues(
        contradictions,
        ClaimContradictionKind::VerifierInconclusive,
        inconclusive_records,
        true,
    );
}

fn verifier_record_kind(kind: &str) -> bool {
    matches!(kind, "rch" | "remote_rch" | "verifier" | "test_result")
}

fn has_direct_verifier_pass(records: &[EvidenceRecord], target: &str) -> bool {
    records.iter().any(|record| {
        record.target == target
            && record.status == EvidenceRecordStatus::Pass
            && record.strength == "direct"
    })
}

fn push_grouped_issues(
    contradictions: &mut Vec<ClaimContradiction>,
    kind: ClaimContradictionKind,
    mut records: Vec<EvidenceRecord>,
    blocks_closure: bool,
) {
    records.sort();
    records.dedup();
    let mut targets = records
        .iter()
        .map(|record| record.target.clone())
        .collect::<Vec<_>>();
    targets.sort();
    targets.dedup();

    for target in targets {
        let mut evidence_records = records
            .iter()
            .filter(|record| record.target == target)
            .cloned()
            .collect::<Vec<_>>();
        evidence_records.sort();
        evidence_records.dedup();
        contradictions.push(ClaimContradiction {
            kind,
            target,
            blocks_closure,
            suggested_tracker_action: tracker_action_for(kind).to_owned(),
            evidence_records,
        });
    }
}

fn tracker_action_for(kind: ClaimContradictionKind) -> &'static str {
    match kind {
        ClaimContradictionKind::StaleAcceptance => "refresh_acceptance_or_create_followup",
        ClaimContradictionKind::UnsupportedDuplicateClaim => {
            "replace_duplicate_claim_with_diff_evidence"
        }
        ClaimContradictionKind::VerifierInconclusive => "rerun_or_record_remote_verifier_result",
        ClaimContradictionKind::SourceDocsConflict => "align_docs_schema_and_source_contract",
        ClaimContradictionKind::PermissionGateUnresolved => "request_explicit_user_permission",
        ClaimContradictionKind::NeedsOwnerDecision => "record_owner_decision_before_closure",
    }
}

fn add_literal_documentation_requirements(
    requirements: &mut Vec<RequirementAccumulator>,
    label: &str,
    text: &str,
) {
    for file in ["AGENTS.md", "README.md"] {
        let mut spans = positive_read_spans(label, text, file);
        if spans.is_empty() {
            spans = document_reference_spans(label, text, file);
        }
        if spans.is_empty() {
            continue;
        }
        add_requirement(
            requirements,
            RequirementKind::DocumentationRead,
            format!("Read and understand {file}"),
            vec![evidence("file_read", file, "direct")],
            vec![verification("prompt_requirement", file, true)],
            spans,
        );
    }
}

fn add_keyword_requirements(
    requirements: &mut Vec<RequirementAccumulator>,
    label: &str,
    text: &str,
) {
    add_phrase_requirement(
        requirements,
        label,
        text,
        PhraseRequirementSpec {
            phrases: &[
                "code investigation",
                "technical architecture",
                "purpose of the project",
            ],
            kind: RequirementKind::CodeInvestigation,
            summary: "Investigate the codebase architecture and project purpose",
            evidence: EvidenceSpec {
                kind: "code_inspection",
                target: "repository",
                strength: "direct",
            },
            verification: VerificationSpec {
                kind: "read_only_architecture_audit",
                target: "repository",
                required: true,
            },
        },
    );
    add_phrase_requirement(
        requirements,
        label,
        text,
        PhraseRequirementSpec {
            phrases: &["mcp agent mail", "agent mail", "introduce yourself"],
            kind: RequirementKind::Coordination,
            summary: "Coordinate through MCP Agent Mail",
            evidence: EvidenceSpec {
                kind: "agent_mail",
                target: "project inbox/outbox",
                strength: "direct",
            },
            verification: VerificationSpec {
                kind: "coordination_receipt",
                target: "agent mail",
                required: true,
            },
        },
    );
    add_phrase_requirement(
        requirements,
        label,
        text,
        PhraseRequirementSpec {
            phrases: &["beads", "br ", "mark beads", "tracking your progress"],
            kind: RequirementKind::Tracker,
            summary: "Track progress through Beads",
            evidence: EvidenceSpec {
                kind: "beads",
                target: ".beads/issues.jsonl",
                strength: "direct",
            },
            verification: VerificationSpec {
                kind: "tracker_comment_or_status",
                target: "beads",
                required: true,
            },
        },
    );
    add_phrase_requirement(
        requirements,
        label,
        text,
        PhraseRequirementSpec {
            phrases: &["bv tool", "bv ", "prioritize"],
            kind: RequirementKind::Tracker,
            summary: "Use bv as the ranking aid when selecting work",
            evidence: EvidenceSpec {
                kind: "bv",
                target: "bv robot output",
                strength: "supporting",
            },
            verification: VerificationSpec {
                kind: "triage_command",
                target: "bv --robot-next or bv --robot-triage",
                required: true,
            },
        },
    );
    add_phrase_requirement(
        requirements,
        label,
        text,
        PhraseRequirementSpec {
            phrases: &["rch", "cargo builds", "cargo tests", "builds and tests"],
            kind: RequirementKind::Verification,
            summary: "Run Cargo/build/test verification through RCH only",
            evidence: EvidenceSpec {
                kind: "rch",
                target: "remote build metadata",
                strength: "direct",
            },
            verification: VerificationSpec {
                kind: "remote_rch",
                target: "cargo/build/test command",
                required: true,
            },
        },
    );
    add_phrase_requirement(
        requirements,
        label,
        text,
        PhraseRequirementSpec {
            phrases: &["stalled out", "stalled", "in progress"],
            kind: RequirementKind::Tracker,
            summary: "Reopen clearly stalled in-progress beads",
            evidence: EvidenceSpec {
                kind: "beads",
                target: "in-progress bead freshness",
                strength: "direct",
            },
            verification: VerificationSpec {
                kind: "stale_bead_audit",
                target: "br list --status in_progress",
                required: true,
            },
        },
    );
    add_phrase_requirement(
        requirements,
        label,
        text,
        PhraseRequirementSpec {
            phrases: &["acknowledge", "communication requests", "promptly respond"],
            kind: RequirementKind::Coordination,
            summary: "Acknowledge and respond to coordination requests",
            evidence: EvidenceSpec {
                kind: "agent_mail",
                target: "ack/read state",
                strength: "direct",
            },
            verification: VerificationSpec {
                kind: "mail_ack_audit",
                target: "ack_required messages",
                required: true,
            },
        },
    );
    add_phrase_requirement(
        requirements,
        label,
        text,
        PhraseRequirementSpec {
            phrases: &["completion audit", "prompt-to-artifact", "checklist"],
            kind: RequirementKind::CompletionAudit,
            summary: "Perform a completion audit before claiming done",
            evidence: EvidenceSpec {
                kind: "completion_audit",
                target: "prompt-to-artifact checklist",
                strength: "direct",
            },
            verification: VerificationSpec {
                kind: "completion_audit",
                target: "explicit objective requirements",
                required: true,
            },
        },
    );
    add_phrase_requirement(
        requirements,
        label,
        text,
        PhraseRequirementSpec {
            phrases: &["explicit permission", "permission", "no file deletion"],
            kind: RequirementKind::PermissionGate,
            summary: "Respect permission-gated filesystem actions",
            evidence: EvidenceSpec {
                kind: "permission_record",
                target: "user authorization text",
                strength: "direct",
            },
            verification: VerificationSpec {
                kind: "permission_gate",
                target: "filesystem mutation",
                required: true,
            },
        },
    );
}

fn add_phrase_requirement(
    requirements: &mut Vec<RequirementAccumulator>,
    label: &str,
    text: &str,
    spec: PhraseRequirementSpec<'_>,
) {
    let spans = spec
        .phrases
        .iter()
        .flat_map(|phrase| literal_spans(label, text, phrase))
        .collect::<Vec<_>>();
    if spans.is_empty() {
        return;
    }
    add_requirement(
        requirements,
        spec.kind,
        spec.summary.to_owned(),
        vec![evidence(
            spec.evidence.kind,
            spec.evidence.target,
            spec.evidence.strength,
        )],
        vec![verification(
            spec.verification.kind,
            spec.verification.target,
            spec.verification.required,
        )],
        spans,
    );
}

fn add_backtick_requirements(
    requirements: &mut Vec<RequirementAccumulator>,
    label: &str,
    text: &str,
) {
    for span in backtick_spans(label, text) {
        let value = span.text.trim();
        if looks_like_skill(value) {
            add_requirement(
                requirements,
                RequirementKind::SkillApplication,
                format!("Apply skill {value}"),
                vec![evidence("skill", value, "supporting")],
                vec![verification("skill_workflow", value, true)],
                vec![span],
            );
        } else if looks_like_command(value) {
            add_requirement(
                requirements,
                RequirementKind::Command,
                format!("Run or verify command `{value}`"),
                vec![evidence("command", value, "direct")],
                vec![verification("command_output", value, true)],
                vec![span],
            );
        }
    }
}

fn add_token_requirements(requirements: &mut Vec<RequirementAccumulator>, label: &str, text: &str) {
    for span in token_spans(label, text) {
        let value = span.text.trim();
        if looks_like_skill(value) {
            add_requirement(
                requirements,
                RequirementKind::SkillApplication,
                format!("Apply skill {value}"),
                vec![evidence("skill", value, "supporting")],
                vec![verification("skill_workflow", value, true)],
                vec![span],
            );
        } else if looks_like_file_path(value) && !is_documentation_file(value) {
            add_requirement(
                requirements,
                RequirementKind::FileArtifact,
                format!("Account for file or path `{value}`"),
                vec![evidence("file_or_path", value, "direct")],
                vec![verification("artifact_inspection", value, true)],
                vec![span],
            );
        }
    }
}

fn add_unknown_clauses(unknowns: &mut Vec<UnknownClause>, label: &str, text: &str) {
    for phrase in [
        "dramatically enhance",
        "take it to the next level",
        "amazing features",
        "world-class",
        "as compelling",
    ] {
        for span in literal_spans(label, text, phrase) {
            unknowns.push(UnknownClause {
                id: String::new(),
                reason: "broad_ambition".to_owned(),
                source_span: span,
            });
        }
    }

    for target in ["AGENTS.md", "README.md"] {
        let positive_spans = positive_read_spans(label, text, target);
        let negative = !negative_read_spans(label, text, target).is_empty();
        if !positive_spans.is_empty() && negative {
            if let Some(span) = positive_spans.into_iter().next() {
                unknowns.push(UnknownClause {
                    id: String::new(),
                    reason: format!("contradictory_instruction:{target}"),
                    source_span: span,
                });
            }
        }
    }
}

fn positive_read_spans(label: &str, text: &str, target: &str) -> Vec<SourceSpan> {
    let negative_spans = negative_read_spans(label, text, target);
    literal_spans(label, text, &format!("read {target}"))
        .into_iter()
        .filter(|span| {
            negative_spans
                .iter()
                .all(|negative| span.start < negative.start || span.end > negative.end)
        })
        .collect()
}

fn negative_read_spans(label: &str, text: &str, target: &str) -> Vec<SourceSpan> {
    ["do not read", "don't read"]
        .into_iter()
        .flat_map(|prefix| literal_spans(label, text, &format!("{prefix} {target}")))
        .collect()
}

fn document_reference_spans(label: &str, text: &str, target: &str) -> Vec<SourceSpan> {
    let negative_spans = negative_read_spans(label, text, target);
    literal_spans(label, text, target)
        .into_iter()
        .filter(|span| {
            negative_spans
                .iter()
                .all(|negative| span.start < negative.start || span.end > negative.end)
        })
        .collect()
}

fn add_requirement(
    requirements: &mut Vec<RequirementAccumulator>,
    kind: RequirementKind,
    summary: String,
    evidence_expectations: Vec<EvidenceExpectation>,
    verification_expectations: Vec<VerificationExpectation>,
    source_spans: Vec<SourceSpan>,
) {
    if source_spans.is_empty() {
        return;
    }
    if let Some(existing) = requirements
        .iter_mut()
        .find(|item| item.kind == kind && item.summary == summary)
    {
        existing.source_spans.extend(source_spans);
        existing.evidence_expectations.extend(evidence_expectations);
        existing
            .verification_expectations
            .extend(verification_expectations);
        return;
    }
    requirements.push(RequirementAccumulator {
        kind,
        summary,
        evidence_expectations,
        verification_expectations,
        source_spans,
    });
}

fn evidence(kind: &str, target: &str, strength: &str) -> EvidenceExpectation {
    EvidenceExpectation {
        kind: kind.to_owned(),
        target: target.to_owned(),
        strength: strength.to_owned(),
    }
}

fn verification(kind: &str, target: &str, required: bool) -> VerificationExpectation {
    VerificationExpectation {
        kind: kind.to_owned(),
        target: target.to_owned(),
        required,
    }
}

fn objective_hash(text: &str) -> String {
    let hash = blake3::hash(text.as_bytes()).to_hex().to_string();
    hash.chars().take(24).collect()
}

fn first_span_start(spans: &[SourceSpan]) -> usize {
    spans
        .iter()
        .map(|span| span.start)
        .min()
        .unwrap_or(usize::MAX)
}

fn literal_spans(label: &str, text: &str, needle: &str) -> Vec<SourceSpan> {
    if needle.is_empty() {
        return Vec::new();
    }
    let lower_text = text.to_ascii_lowercase();
    let lower_needle = needle.to_ascii_lowercase();
    let mut spans = Vec::new();
    let mut offset = 0;
    while offset <= lower_text.len() {
        let Some(found) = lower_text[offset..].find(&lower_needle) else {
            break;
        };
        let start = offset + found;
        let end = start + lower_needle.len();
        spans.push(SourceSpan {
            label: label.to_owned(),
            start,
            end,
            text: text[start..end].to_owned(),
        });
        offset = end;
    }
    spans
}

fn backtick_spans(label: &str, text: &str) -> Vec<SourceSpan> {
    let bytes = text.as_bytes();
    let mut spans = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        let Some(open_rel) = bytes[index..].iter().position(|byte| *byte == b'`') else {
            break;
        };
        let open = index + open_rel;
        let content_start = open + 1;
        let Some(close_rel) = bytes[content_start..].iter().position(|byte| *byte == b'`') else {
            break;
        };
        let content_end = content_start + close_rel;
        if content_start < content_end {
            spans.push(SourceSpan {
                label: label.to_owned(),
                start: content_start,
                end: content_end,
                text: text[content_start..content_end].to_owned(),
            });
        }
        index = content_end + 1;
    }
    spans
}

fn token_spans(label: &str, text: &str) -> Vec<SourceSpan> {
    let mut spans = Vec::new();
    let mut token_start = None;
    for (index, ch) in text.char_indices() {
        if ch.is_whitespace() {
            if let Some(start) = token_start.take() {
                push_trimmed_token(label, text, start, index, &mut spans);
            }
        } else if token_start.is_none() {
            token_start = Some(index);
        }
    }
    if let Some(start) = token_start {
        push_trimmed_token(label, text, start, text.len(), &mut spans);
    }
    spans
}

fn push_trimmed_token(
    label: &str,
    text: &str,
    start: usize,
    end: usize,
    spans: &mut Vec<SourceSpan>,
) {
    let raw = &text[start..end];
    let trimmed = raw.trim_matches(token_boundary_punctuation);
    if trimmed.is_empty() {
        return;
    }
    let leading = raw.find(trimmed).unwrap_or(0);
    let token_start = start + leading;
    let token_end = token_start + trimmed.len();
    spans.push(SourceSpan {
        label: label.to_owned(),
        start: token_start,
        end: token_end,
        text: trimmed.to_owned(),
    });
}

fn token_boundary_punctuation(ch: char) -> bool {
    matches!(
        ch,
        ',' | '.' | ';' | ':' | '(' | ')' | '[' | ']' | '{' | '}' | '"' | '\'' | '`' | '*'
    )
}

fn looks_like_command(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    ["cargo", "br", "bv", "rch", "ee", "git", "./"]
        .iter()
        .any(|prefix| lower == *prefix || lower.starts_with(&format!("{prefix} ")))
}

fn looks_like_skill(value: &str) -> bool {
    value.strip_prefix('$').is_some_and(|tail| {
        tail.chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    })
}

fn looks_like_file_path(value: &str) -> bool {
    if value.starts_with("http://") || value.starts_with("https://") {
        return false;
    }
    value.contains('/')
        || [
            ".md", ".rs", ".toml", ".json", ".jsonl", ".yaml", ".yml", ".sh",
        ]
        .iter()
        .any(|suffix| value.ends_with(suffix))
}

fn is_documentation_file(value: &str) -> bool {
    matches!(value, "AGENTS.md" | "README.md")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(checklist: &CompletionChecklist) -> Vec<RequirementKind> {
        checklist
            .requirements
            .iter()
            .map(|requirement| requirement.kind)
            .collect()
    }

    fn has_summary(checklist: &CompletionChecklist, needle: &str) -> bool {
        checklist
            .requirements
            .iter()
            .any(|requirement| requirement.summary.contains(needle))
    }

    fn requirement_with_summary<'a>(
        checklist: &'a CompletionChecklist,
        needle: &str,
    ) -> &'a CompletionRequirement {
        for requirement in &checklist.requirements {
            if requirement.summary.contains(needle) {
                return requirement;
            }
        }
        panic!("missing requirement containing {needle}");
    }

    fn evidence_for_requirement<'a>(
        evidence: &'a [RequirementEvidence],
        requirement_id: &str,
    ) -> &'a RequirementEvidence {
        for item in evidence {
            if item.requirement_id == requirement_id {
                return item;
            }
        }
        panic!("missing evidence for requirement {requirement_id}");
    }

    fn record(
        kind: &str,
        target: &str,
        source: &str,
        status: EvidenceRecordStatus,
        strength: &str,
    ) -> EvidenceRecord {
        EvidenceRecord {
            kind: kind.to_owned(),
            target: target.to_owned(),
            source: source.to_owned(),
            status,
            strength: strength.to_owned(),
            summary: format!("{source}: {kind} {target}"),
        }
    }

    #[test]
    fn broad_swarm_objective_extracts_explicit_obligations_and_unknowns() {
        let objective = "First read ALL of the AGENTS.md file and README.md file. \
            Use code investigation to understand the technical architecture and purpose. \
            Register with MCP Agent Mail, acknowledge communication requests, track progress via Beads, \
            use the bv tool to prioritize, and run all cargo builds and tests through RCH. \
            Look for stalled in progress beads, apply $idea-wizard, and perform a prompt-to-artifact completion audit. \
            Also come up with amazing features that take it to the next level.";

        let checklist = extract_completion_checklist("thread_goal", objective);
        let kinds = kinds(&checklist);

        assert_eq!(checklist.schema, COMPLETION_AUDIT_CHECKLIST_SCHEMA_V1);
        assert!(kinds.contains(&RequirementKind::DocumentationRead));
        assert!(kinds.contains(&RequirementKind::CodeInvestigation));
        assert!(kinds.contains(&RequirementKind::Coordination));
        assert!(kinds.contains(&RequirementKind::Tracker));
        assert!(kinds.contains(&RequirementKind::Verification));
        assert!(kinds.contains(&RequirementKind::SkillApplication));
        assert!(kinds.contains(&RequirementKind::CompletionAudit));
        assert!(has_summary(&checklist, "AGENTS.md"));
        assert!(has_summary(&checklist, "README.md"));
        assert!(
            checklist
                .unknown_clauses
                .iter()
                .any(|clause| clause.reason == "broad_ambition")
        );
    }

    #[test]
    fn duplicate_requirements_merge_source_spans() {
        let checklist =
            extract_completion_checklist("objective", "Read AGENTS.md, then read AGENTS.md again.");
        let agents_requirements = checklist
            .requirements
            .iter()
            .filter(|requirement| requirement.summary.contains("AGENTS.md"))
            .collect::<Vec<_>>();

        assert_eq!(agents_requirements.len(), 1);
        assert_eq!(agents_requirements[0].source_spans.len(), 2);
    }

    #[test]
    fn commands_and_paths_are_extracted_deterministically() {
        let checklist = extract_completion_checklist(
            "objective",
            "Update src/core/lab.rs and verify with `cargo fmt --check`.",
        );

        assert!(has_summary(&checklist, "src/core/lab.rs"));
        assert!(has_summary(&checklist, "cargo fmt --check"));
        assert_eq!(
            checklist
                .requirements
                .first()
                .map(|requirement| requirement.id.as_str()),
            Some("req_001")
        );
    }

    #[test]
    fn empty_objective_is_unknown_not_satisfied() {
        let checklist = extract_completion_checklist("objective", "   ");

        assert!(checklist.requirements.is_empty());
        assert_eq!(checklist.unknown_clauses.len(), 1);
        assert_eq!(checklist.unknown_clauses[0].reason, "empty_objective");
        assert!(checklist.summary.has_unknowns);
    }

    #[test]
    fn contradictory_doc_instruction_becomes_unknown_clause() {
        let checklist = extract_completion_checklist(
            "objective",
            "Read AGENTS.md carefully, but do not read AGENTS.md.",
        );

        assert!(
            checklist
                .unknown_clauses
                .iter()
                .any(|clause| clause.reason == "contradictory_instruction:AGENTS.md")
        );
    }

    #[test]
    fn negative_doc_instruction_alone_is_not_a_contradiction() {
        let checklist =
            extract_completion_checklist("objective", "Do not read AGENTS.md for this task.");

        assert!(
            !checklist
                .requirements
                .iter()
                .any(|requirement| requirement.summary.contains("AGENTS.md"))
        );
        assert!(
            checklist
                .unknown_clauses
                .iter()
                .all(|clause| clause.reason != "contradictory_instruction:AGENTS.md")
        );
    }

    #[test]
    fn evidence_adapter_distinguishes_remote_rch_pass() {
        let checklist = extract_completion_checklist(
            "objective",
            "Run all cargo builds and tests through RCH.",
        );
        let requirement = requirement_with_summary(&checklist, "RCH only");
        let bundle = EvidenceBundle {
            records: vec![
                record(
                    "rch",
                    "remote build metadata",
                    "rch job 159 on csd",
                    EvidenceRecordStatus::Pass,
                    "direct",
                ),
                record(
                    "remote_rch",
                    "cargo/build/test command",
                    "cargo test --lib completion_audit",
                    EvidenceRecordStatus::Pass,
                    "direct",
                ),
            ],
        };

        let evidence = evaluate_completion_evidence(&checklist, &bundle);
        let item = evidence_for_requirement(&evidence, &requirement.id);

        assert_eq!(item.support, RequirementSupport::Direct);
        assert!(item.missing_expectations.is_empty());
        assert_eq!(item.evidence_records.len(), 2);
    }

    #[test]
    fn evidence_adapter_distinguishes_rch_capacity_blocker_from_pass() {
        let checklist = extract_completion_checklist(
            "objective",
            "Run all cargo builds and tests through RCH.",
        );
        let requirement = requirement_with_summary(&checklist, "RCH only");
        let bundle = EvidenceBundle {
            records: vec![record(
                "remote_rch",
                "cargo/build/test command",
                "rch all_workers_at_capacity",
                EvidenceRecordStatus::CapacityBlocked,
                "direct",
            )],
        };

        let evidence = evaluate_completion_evidence(&checklist, &bundle);
        let item = evidence_for_requirement(&evidence, &requirement.id);

        assert_eq!(item.support, RequirementSupport::Blocked);
        assert_eq!(item.confidence, "blocked");
    }

    #[test]
    fn evidence_adapter_treats_static_only_check_as_weak_proxy() {
        let checklist = extract_completion_checklist(
            "objective",
            "Run all cargo builds and tests through RCH.",
        );
        let requirement = requirement_with_summary(&checklist, "RCH only");
        let bundle = EvidenceBundle {
            records: vec![record(
                "remote_rch",
                "cargo/build/test command",
                "git diff --check only",
                EvidenceRecordStatus::StaticOnly,
                "weak",
            )],
        };

        let evidence = evaluate_completion_evidence(&checklist, &bundle);
        let item = evidence_for_requirement(&evidence, &requirement.id);

        assert_eq!(item.support, RequirementSupport::Weak);
        assert_eq!(item.confidence, "weak");
    }

    #[test]
    fn evidence_adapter_flags_stale_beads_claim() {
        let checklist = extract_completion_checklist("objective", "Track progress via Beads.");
        let requirement = requirement_with_summary(&checklist, "Beads");
        let bundle = EvidenceBundle {
            records: vec![record(
                "beads",
                ".beads/issues.jsonl",
                "bd-abc123 updated yesterday",
                EvidenceRecordStatus::Stale,
                "direct",
            )],
        };

        let evidence = evaluate_completion_evidence(&checklist, &bundle);
        let item = evidence_for_requirement(&evidence, &requirement.id);

        assert_eq!(item.support, RequirementSupport::Stale);
        assert_eq!(item.confidence, "stale");
    }

    #[test]
    fn evidence_adapter_reports_missing_requirement_evidence() {
        let checklist = extract_completion_checklist("objective", "Coordinate through Agent Mail.");
        let requirement = requirement_with_summary(&checklist, "MCP Agent Mail");

        let evidence = evaluate_completion_evidence(&checklist, &EvidenceBundle::default());
        let item = evidence_for_requirement(&evidence, &requirement.id);

        assert_eq!(item.support, RequirementSupport::Missing);
        assert!(!item.missing_expectations.is_empty());
    }

    #[test]
    fn evidence_adapter_prioritizes_contradictory_failures() {
        let checklist = extract_completion_checklist("objective", "Coordinate through Agent Mail.");
        let requirement = requirement_with_summary(&checklist, "MCP Agent Mail");
        let bundle = EvidenceBundle {
            records: vec![record(
                "agent_mail",
                "project inbox/outbox",
                "mail service unavailable",
                EvidenceRecordStatus::Fail,
                "direct",
            )],
        };

        let evidence = evaluate_completion_evidence(&checklist, &bundle);
        let item = evidence_for_requirement(&evidence, &requirement.id);

        assert_eq!(item.support, RequirementSupport::Contradicted);
        assert_eq!(item.confidence, "contradicted");
    }

    #[test]
    fn claim_detector_flags_stale_acceptance_text() {
        let bundle = EvidenceBundle {
            records: vec![record(
                "acceptance",
                "bd-mcp manifest acceptance",
                "bead description expects default-build error exit",
                EvidenceRecordStatus::Stale,
                "direct",
            )],
        };

        let contradictions = detect_claim_contradictions(&bundle);

        assert_eq!(contradictions.len(), 1);
        assert_eq!(
            contradictions[0].kind,
            ClaimContradictionKind::StaleAcceptance
        );
        assert!(contradictions[0].blocks_closure);
    }

    #[test]
    fn claim_detector_flags_unsupported_duplicate_claim() {
        let bundle = EvidenceBundle {
            records: vec![record(
                "duplicate_claim",
                "orphan plan bead",
                "diff audit found unique content",
                EvidenceRecordStatus::Fail,
                "direct",
            )],
        };

        let contradictions = detect_claim_contradictions(&bundle);

        assert_eq!(contradictions.len(), 1);
        assert_eq!(
            contradictions[0].kind,
            ClaimContradictionKind::UnsupportedDuplicateClaim
        );
        assert_eq!(
            contradictions[0].suggested_tracker_action,
            "replace_duplicate_claim_with_diff_evidence"
        );
    }

    #[test]
    fn claim_detector_ignores_vanished_verifier_when_direct_remote_pass_exists() {
        let bundle = EvidenceBundle {
            records: vec![
                record(
                    "remote_rch",
                    "cargo test --lib completion_audit",
                    "old queued job vanished",
                    EvidenceRecordStatus::Inconclusive,
                    "direct",
                ),
                record(
                    "remote_rch",
                    "cargo test --lib completion_audit",
                    "rch job 162 on csd",
                    EvidenceRecordStatus::Pass,
                    "direct",
                ),
            ],
        };

        let contradictions = detect_claim_contradictions(&bundle);

        assert!(contradictions.is_empty());
    }

    #[test]
    fn claim_detector_flags_verifier_inconclusive_without_remote_pass() {
        let bundle = EvidenceBundle {
            records: vec![record(
                "remote_rch",
                "cargo test --test smoke mcp_manifest_json_real_binary_smoke",
                "queued job disappeared before terminal status",
                EvidenceRecordStatus::Inconclusive,
                "direct",
            )],
        };

        let contradictions = detect_claim_contradictions(&bundle);

        assert_eq!(contradictions.len(), 1);
        assert_eq!(
            contradictions[0].kind,
            ClaimContradictionKind::VerifierInconclusive
        );
    }

    #[test]
    fn claim_detector_flags_docs_source_conflict() {
        let bundle = EvidenceBundle {
            records: vec![record(
                "source_docs",
                "README command table",
                "docs list command absent from source enum",
                EvidenceRecordStatus::Fail,
                "direct",
            )],
        };

        let contradictions = detect_claim_contradictions(&bundle);

        assert_eq!(contradictions.len(), 1);
        assert_eq!(
            contradictions[0].kind,
            ClaimContradictionKind::SourceDocsConflict
        );
    }

    #[test]
    fn claim_detector_separates_permission_gate_from_owner_decision() {
        let bundle = EvidenceBundle {
            records: vec![
                record(
                    "permission_record",
                    "file deletion authorization",
                    "no user authorization text found",
                    EvidenceRecordStatus::Missing,
                    "direct",
                ),
                record(
                    "scope_decision",
                    "mcp parity public command",
                    "scope decision unresolved",
                    EvidenceRecordStatus::Inconclusive,
                    "supporting",
                ),
            ],
        };

        let contradictions = detect_claim_contradictions(&bundle);
        let kinds = contradictions
            .iter()
            .map(|contradiction| contradiction.kind)
            .collect::<Vec<_>>();

        assert_eq!(contradictions.len(), 2);
        assert!(kinds.contains(&ClaimContradictionKind::PermissionGateUnresolved));
        assert!(kinds.contains(&ClaimContradictionKind::NeedsOwnerDecision));
    }
}
