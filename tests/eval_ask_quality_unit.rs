use ee::eval::{
    ASK_REPORT_SCHEMA_V1, AskQualityActual, AskQualityCase, AskQualityCitationActual,
    AskQualityExpectations, AskQualityExpectedSide, AskQualityGateMode, AskQualitySideActual,
    AskQualityThresholds, PackQualityVerdict, compare_ask_quality, evaluate_ask_quality,
};

type TestResult = Result<(), String>;

fn ensure<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, ctx: &str) -> TestResult {
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
    }
}

fn ensure_close(actual: f64, expected: f64, ctx: &str) -> TestResult {
    if (actual - expected).abs() <= 0.000_001 {
        Ok(())
    } else {
        Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
    }
}

fn answer_case(case_id: &str, expected_ids: &[&str], expected_terms: &[&str]) -> AskQualityCase {
    AskQualityCase {
        case_id: case_id.to_string(),
        scenario_id: "ask_direct_fact".to_string(),
        command_step: 1,
        question: "What Rust toolchain does Project Zephyr use?".to_string(),
        expected_cited_memory_ids: expected_ids.iter().map(|id| (*id).to_string()).collect(),
        expected_answer_terms: expected_terms
            .iter()
            .map(|term| (*term).to_string())
            .collect(),
        expect_abstention: false,
        expect_conflict: false,
        expected_sides: Vec::new(),
    }
}

fn abstention_case() -> AskQualityCase {
    AskQualityCase {
        case_id: "ask.unanswerable_lunar_invoice".to_string(),
        scenario_id: "ask_unanswerable_abstention".to_string(),
        command_step: 4,
        question: "Who approved the lunar invoice for Project Zephyr?".to_string(),
        expected_cited_memory_ids: Vec::new(),
        expected_answer_terms: Vec::new(),
        expect_abstention: true,
        expect_conflict: false,
        expected_sides: Vec::new(),
    }
}

fn conflict_case() -> AskQualityCase {
    AskQualityCase {
        case_id: "ask.conflict_remote_cache".to_string(),
        scenario_id: "ask_conflicting_evidence".to_string(),
        command_step: 3,
        question: "Is remote cache delta enabled for Project Zephyr?".to_string(),
        expected_cited_memory_ids: vec![
            "mem_ask_conflict_affirm".to_string(),
            "mem_ask_conflict_negate".to_string(),
        ],
        expected_answer_terms: vec!["remote cache delta".to_string()],
        expect_abstention: false,
        expect_conflict: true,
        expected_sides: vec![
            AskQualityExpectedSide {
                label: "affirming".to_string(),
                cited_memory_ids: vec!["mem_ask_conflict_affirm".to_string()],
            },
            AskQualityExpectedSide {
                label: "negating".to_string(),
                cited_memory_ids: vec!["mem_ask_conflict_negate".to_string()],
            },
        ],
    }
}

fn actual(case_id: &str, ids_and_texts: &[(&str, &str)]) -> AskQualityActual {
    AskQualityActual {
        case_id: case_id.to_string(),
        answer_text: Some(
            ids_and_texts
                .iter()
                .map(|(_, text)| *text)
                .collect::<Vec<_>>()
                .join(" "),
        ),
        abstained: false,
        citations: ids_and_texts
            .iter()
            .map(|(id, text)| AskQualityCitationActual {
                memory_id: (*id).to_string(),
                text: (*text).to_string(),
            })
            .collect(),
        sides: Vec::new(),
    }
}

#[test]
fn ask_quality_compare_perfect_grounded_answer() -> TestResult {
    let case = answer_case(
        "ask.direct_toolchain",
        &["mem_ask_direct_toolchain"],
        &["Rust nightly 1.96.0"],
    );
    let actual = actual(
        "ask.direct_toolchain",
        &[(
            "mem_ask_direct_toolchain",
            "Project Zephyr uses Rust nightly 1.96.0 as its active toolchain.",
        )],
    );

    let comparison = compare_ask_quality(&case, &actual);

    ensure(comparison.verdict, PackQualityVerdict::Within, "verdict")?;
    ensure_close(
        comparison.scores.citation_precision,
        1.0,
        "citation precision",
    )?;
    ensure_close(comparison.scores.answer_exactness, 1.0, "exactness")?;
    ensure_close(
        comparison.scores.abstention_calibration,
        1.0,
        "abstention calibration",
    )?;
    ensure_close(comparison.scores.conflict_recall, 1.0, "conflict recall")
}

#[test]
fn ask_quality_compare_flags_wrong_citation_as_drift() -> TestResult {
    let case = answer_case(
        "ask.direct_toolchain",
        &["mem_ask_direct_toolchain"],
        &["Rust nightly 1.96.0"],
    );
    let actual = actual(
        "ask.direct_toolchain",
        &[(
            "mem_wrong",
            "This span does not contain the expected answer.",
        )],
    );

    let comparison = compare_ask_quality(&case, &actual);

    ensure(comparison.verdict, PackQualityVerdict::Drift, "verdict")?;
    ensure_close(
        comparison.scores.citation_precision,
        0.0,
        "citation precision",
    )?;
    ensure_close(comparison.scores.answer_exactness, 0.0, "exactness")
}

#[test]
fn ask_quality_compare_enforces_abstention_calibration() -> TestResult {
    let case = abstention_case();
    let hallucinated = actual(
        "ask.unanswerable_lunar_invoice",
        &[(
            "mem_ask_unrelated_billing",
            "The invoice was approved by nobody in the corpus.",
        )],
    );

    let comparison = compare_ask_quality(&case, &hallucinated);

    ensure(
        comparison.verdict,
        PackQualityVerdict::Regression,
        "verdict",
    )?;
    ensure_close(
        comparison.scores.abstention_calibration,
        0.0,
        "abstention calibration",
    )
}

#[test]
fn ask_quality_compare_recalls_conflict_sides() -> TestResult {
    let case = conflict_case();
    let actual = AskQualityActual {
        case_id: "ask.conflict_remote_cache".to_string(),
        answer_text: None,
        abstained: false,
        citations: vec![
            AskQualityCitationActual {
                memory_id: "mem_ask_conflict_affirm".to_string(),
                text: "Remote cache delta is enabled for Project Zephyr.".to_string(),
            },
            AskQualityCitationActual {
                memory_id: "mem_ask_conflict_negate".to_string(),
                text: "Remote cache delta is not enabled for Project Zephyr.".to_string(),
            },
        ],
        sides: vec![
            AskQualitySideActual {
                label: "affirming".to_string(),
                cited_memory_ids: vec!["mem_ask_conflict_affirm".to_string()],
            },
            AskQualitySideActual {
                label: "negating".to_string(),
                cited_memory_ids: vec!["mem_ask_conflict_negate".to_string()],
            },
        ],
    };

    let comparison = compare_ask_quality(&case, &actual);

    ensure(comparison.verdict, PackQualityVerdict::Within, "verdict")?;
    ensure_close(comparison.scores.conflict_recall, 1.0, "conflict recall")
}

#[test]
fn ask_quality_report_blocks_threshold_regressions() -> TestResult {
    let expectations = AskQualityExpectations {
        schema: "ee.eval.ask_quality_expectations.v1".to_string(),
        thresholds: AskQualityThresholds {
            citation_precision_min: 0.9,
            answer_exactness_min: 0.9,
            abstention_calibration_min: 1.0,
            conflict_recall_min: 1.0,
            gate_mode: AskQualityGateMode::Blocking,
        },
        cases: vec![
            answer_case(
                "ask.direct_toolchain",
                &["mem_ask_direct_toolchain"],
                &["Rust nightly 1.96.0"],
            ),
            abstention_case(),
        ],
    };
    let actuals = vec![actual(
        "ask.direct_toolchain",
        &[(
            "mem_wrong",
            "This span does not contain the expected answer.",
        )],
    )];

    let report = evaluate_ask_quality("ask_v1", &expectations, &actuals);

    ensure(report.schema, ASK_REPORT_SCHEMA_V1, "schema")?;
    ensure(
        report.aggregate_verdict,
        PackQualityVerdict::Regression,
        "aggregate verdict",
    )?;
    ensure(report.cases_total, 2, "case count")?;
    ensure(
        report.cases_inconclusive,
        1,
        "missing actual is inconclusive",
    )?;
    ensure(
        report
            .threshold_failures
            .iter()
            .any(|failure| failure.contains("citation_precision")),
        true,
        "threshold failure names citation precision",
    )
}
