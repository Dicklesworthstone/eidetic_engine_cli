//! Contract coverage for workspace-hygiene secret-risk evidence rendering.
//!
//! The policy helper is intentionally redaction-safe: downstream JSON, human,
//! and support-summary renderers should only receive pattern ids, placeholders,
//! line numbers, and short hashes. This test keeps that no-leak contract pinned
//! with a synthetic fixture.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ee::policy::{WorkspaceSecretRiskReport, workspace_secret_risk_evidence};
use serde::Deserialize;
use serde_json::{Value, json};

type TestResult = Result<(), String>;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SecretRiskFixture {
    description: String,
    path: String,
    content: String,
    raw_secret: String,
    benign_lookalike: String,
    expected_pattern_any_of: Vec<String>,
}

fn fixture() -> Result<SecretRiskFixture, String> {
    serde_json::from_str(include_str!(
        "../fixtures/secrets/workspace_hygiene_secret_risk.json"
    ))
    .map_err(|error| format!("parse workspace secret-risk fixture: {error}"))
}

fn report_json(report: &WorkspaceSecretRiskReport) -> Value {
    json!({
        "schema": report.schema,
        "path": report.path,
        "secretRisk": report.secret_risk,
        "skippedContentScan": report.skipped_content_scan,
        "riskClasses": report.risk_classes,
        "reasons": report.reasons,
        "evidence": report.evidence.iter().map(|evidence| {
            json!({
                "riskClass": evidence.risk_class,
                "patternId": evidence.pattern_id,
                "line": evidence.line,
                "hashPrefix": evidence.hash_prefix,
                "redacted": evidence.redacted,
            })
        }).collect::<Vec<_>>(),
    })
}

fn report_human(report: &WorkspaceSecretRiskReport) -> String {
    let mut output = format!(
        "secret risk: {} path={} classes={}\n",
        report.secret_risk,
        report.path,
        report.risk_classes.join(",")
    );
    for evidence in &report.evidence {
        output.push_str(&format!(
            "- {} line={:?} pattern={} hash={} value={}\n",
            evidence.risk_class,
            evidence.line,
            evidence.pattern_id,
            evidence.hash_prefix.as_deref().unwrap_or("none"),
            evidence.redacted
        ));
    }
    output
}

fn report_support_summary(report: &WorkspaceSecretRiskReport) -> String {
    let evidence = report
        .evidence
        .iter()
        .map(|evidence| {
            format!(
                "{}:{}:{}",
                evidence.risk_class,
                evidence.pattern_id,
                evidence.hash_prefix.as_deref().unwrap_or("none")
            )
        })
        .collect::<Vec<_>>()
        .join("|");
    format!(
        "workspaceSecretRisk schema={} path={} reasons={} evidence={}",
        report.schema,
        report.path,
        report.reasons.join(","),
        evidence
    )
}

fn assert_no_raw_secret(surface: &str, rendered: &str, raw_secret: &str) -> TestResult {
    if rendered.contains(raw_secret) {
        return Err(format!("{surface} leaked raw synthetic secret: {rendered}"));
    }
    Ok(())
}

#[test]
fn workspace_secret_risk_renderings_do_not_leak_raw_fixture_secret() -> TestResult {
    let fixture = fixture()?;
    assert!(
        fixture.description.contains("synthetic"),
        "fixture must document that its secret sample is synthetic"
    );
    assert!(
        fixture.content.contains(&fixture.raw_secret),
        "fixture must exercise the raw synthetic secret"
    );
    assert!(
        fixture.content.contains(&fixture.benign_lookalike),
        "fixture must include a benign lookalike for contrast"
    );

    let report =
        workspace_secret_risk_evidence(&fixture.path, Some(fixture.content.as_bytes()), 4096);
    assert!(report.secret_risk);
    assert!(report.risk_classes.contains(&"content_secret"));
    assert!(!report.evidence.is_empty());
    assert!(report.evidence.iter().all(|evidence| {
        fixture
            .expected_pattern_any_of
            .iter()
            .any(|expected| expected == evidence.pattern_id)
    }));
    assert!(report.evidence.iter().all(|evidence| {
        evidence.redacted.starts_with("[REDACTED:")
            && evidence
                .hash_prefix
                .as_ref()
                .is_some_and(|hash| hash.len() == 12)
    }));

    let rendered_json = report_json(&report).to_string();
    let rendered_human = report_human(&report);
    let support_summary = report_support_summary(&report);
    let debug_report = format!("{report:?}");

    for (surface, rendered) in [
        ("json", rendered_json.as_str()),
        ("human", rendered_human.as_str()),
        ("support_summary", support_summary.as_str()),
        ("debug", debug_report.as_str()),
    ] {
        assert_no_raw_secret(surface, rendered, &fixture.raw_secret)?;
    }

    assert!(
        rendered_json.contains("[REDACTED:") && rendered_human.contains("[REDACTED:"),
        "rendered output should expose redaction placeholders, not raw values"
    );
    assert!(
        support_summary.contains("workspaceSecretRisk")
            && !support_summary.contains(&fixture.benign_lookalike),
        "support summary should be compact and omit non-secret content"
    );

    Ok(())
}
