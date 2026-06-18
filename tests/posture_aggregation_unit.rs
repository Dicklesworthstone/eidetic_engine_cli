use ee::core::doctor::{CheckResult, Posture as DoctorPosture};
use ee::models::error_codes::INDEX_STALE;
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

#[test]
fn doctor_and_status_toplines_agree_on_core_vs_advisory_health() -> TestResult {
    use SubsystemPostureStatus as S;

    let doctor_advisory_only = vec![
        CheckResult::ok("runtime", "ok"),
        CheckResult::ok("workspace", "ok"),
        CheckResult::ok("database", "ok"),
        CheckResult::ok("search_index", "ok"),
        CheckResult::warning("cass", "cass limited", INDEX_STALE).advisory(),
        CheckResult::warning("rch_worker_pressure", "worker pressure", INDEX_STALE).advisory(),
    ];
    let status_advisory_only = WorkspacePostureReport::new_core_overall(
        vec![
            SubsystemPostureReport::new("runtime", S::Ok),
            SubsystemPostureReport::new("storage", S::Ok),
            SubsystemPostureReport::new("search", S::Ok),
            SubsystemPostureReport::new("memory", S::Ok),
            SubsystemPostureReport::new("pack", S::Ok),
            SubsystemPostureReport::new("graph_compute", S::Unimplemented),
            SubsystemPostureReport::new("rch_worker_pressure", S::DegradedRecoverable),
        ],
        OperationPostureReport::ok(["runtime", "storage", "search", "memory", "pack"]),
    );

    ensure_equal(
        DoctorPosture::from_checks(&doctor_advisory_only, None),
        DoctorPosture::Ok,
        "doctor top-line with advisory-only warnings",
    )?;
    ensure_equal(
        doctor_advisory_only
            .iter()
            .all(CheckResult::is_topline_healthy),
        true,
        "doctor healthy flag with advisory-only warnings",
    )?;
    ensure_equal(
        status_advisory_only.overall,
        S::Ok,
        "status top-line with advisory-only warnings",
    )?;
    ensure_equal(
        status_advisory_only.subsystems.len(),
        7,
        "status advisory rows remain visible",
    )?;

    let doctor_core_degraded = vec![
        CheckResult::ok("runtime", "ok"),
        CheckResult::ok("workspace", "ok"),
        CheckResult::ok("database", "ok"),
        CheckResult::warning("search_index", "stale", INDEX_STALE),
        CheckResult::warning("cass", "cass limited", INDEX_STALE).advisory(),
    ];
    let status_core_degraded = WorkspacePostureReport::new_core_overall(
        vec![
            SubsystemPostureReport::new("runtime", S::Ok),
            SubsystemPostureReport::new("storage", S::Ok),
            SubsystemPostureReport::new("search", S::DegradedRecoverable),
            SubsystemPostureReport::new("memory", S::Ok),
            SubsystemPostureReport::new("pack", S::Ok),
            SubsystemPostureReport::new("rch_worker_pressure", S::DegradedRecoverable),
        ],
        OperationPostureReport::ok(["runtime", "storage", "search", "memory", "pack"]),
    );

    ensure_equal(
        DoctorPosture::from_checks(&doctor_core_degraded, None),
        DoctorPosture::DegradedRecoverable,
        "doctor top-line with core degradation",
    )?;
    ensure_equal(
        doctor_core_degraded
            .iter()
            .all(CheckResult::is_topline_healthy),
        false,
        "doctor healthy flag with core degradation",
    )?;
    ensure_equal(
        status_core_degraded.overall,
        S::DegradedRecoverable,
        "status top-line with core degradation",
    )
}
