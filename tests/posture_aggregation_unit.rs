use ee::models::posture::{
    OperationPostureReport, SubsystemPostureReport, SubsystemPostureStatus, WorkspacePostureReport,
};

type TestResult = Result<(), String>;

fn ensure_equal<T>(actual: T, expected: T, context: &str) -> TestResult
where
    T: PartialEq + std::fmt::Debug,
{
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected:?}, got {actual:?}"))
    }
}

#[test]
fn posture_aggregation_covers_closeout_states() -> TestResult {
    use SubsystemPostureStatus as S;

    let cases = [
        ("ok", vec![S::Ok, S::Ok], S::Ok),
        (
            "initializing",
            vec![S::Initializing, S::Initializing],
            S::Initializing,
        ),
        (
            "degraded_recoverable",
            vec![S::Ok, S::DegradedRecoverable],
            S::DegradedRecoverable,
        ),
        (
            "degraded_required",
            vec![S::Ok, S::DegradedRequired, S::DegradedRecoverable],
            S::DegradedRequired,
        ),
        (
            "blocked",
            vec![S::Ok, S::Blocked, S::DegradedRequired],
            S::Blocked,
        ),
    ];

    for (label, input, expected) in cases {
        ensure_equal(S::aggregate(&input), expected, label)?;
    }

    Ok(())
}

#[test]
fn workspace_posture_uses_same_aggregation_rule() -> TestResult {
    use SubsystemPostureStatus as S;

    let report = WorkspacePostureReport::new(
        vec![
            SubsystemPostureReport::new("runtime", S::Ok),
            SubsystemPostureReport::new("storage", S::DegradedRequired),
            SubsystemPostureReport::new("search", S::DegradedRecoverable),
        ],
        OperationPostureReport::ok(["runtime", "storage", "search"]),
    );

    ensure_equal(report.overall, S::DegradedRequired, "workspace aggregate")?;
    ensure_equal(
        report.this_operation.status,
        S::Ok,
        "this operation stays separate from workspace aggregate",
    )
}
