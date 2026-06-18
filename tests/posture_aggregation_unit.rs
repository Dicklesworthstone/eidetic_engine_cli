use ee::core::doctor::{CheckResult, CheckTier, Posture as DoctorPosture};
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
fn core_vs_advisory_empty_inputs_stay_green() -> TestResult {
    use SubsystemPostureStatus as S;

    let no_doctor_checks: Vec<CheckResult> = Vec::new();
    ensure_equal(
        DoctorPosture::from_checks(&no_doctor_checks, None),
        DoctorPosture::Ok,
        "doctor empty input",
    )?;
    ensure_equal(
        no_doctor_checks.iter().all(CheckResult::is_topline_healthy),
        true,
        "doctor empty input healthy",
    )?;

    let status_without_core_rows = WorkspacePostureReport::new_core_overall(
        vec![
            SubsystemPostureReport::new("graph_compute", S::Unimplemented),
            SubsystemPostureReport::new("rch_worker_pressure", S::DegradedRecoverable),
        ],
        OperationPostureReport::ok(["runtime"]),
    );
    ensure_equal(
        status_without_core_rows.overall,
        S::Ok,
        "status empty core input",
    )?;
    ensure_equal(
        status_without_core_rows.subsystems.len(),
        2,
        "status empty core input keeps advisory rows",
    )
}

#[test]
fn advisory_errors_remain_visible_without_blocking_topline() -> TestResult {
    use SubsystemPostureStatus as S;

    let doctor_checks = vec![
        CheckResult::ok("runtime", "ok"),
        CheckResult::ok("workspace", "ok"),
        CheckResult::ok("database", "ok"),
        CheckResult::ok("search_index", "ok"),
        CheckResult::error("rch_worker_pressure", "worker unavailable", INDEX_STALE).advisory(),
    ];
    let status_report = WorkspacePostureReport::new_core_overall(
        vec![
            SubsystemPostureReport::new("runtime", S::Ok),
            SubsystemPostureReport::new("storage", S::Ok),
            SubsystemPostureReport::new("search", S::Ok),
            SubsystemPostureReport::new("memory", S::Ok),
            SubsystemPostureReport::new("pack", S::Ok),
            SubsystemPostureReport::new("rch_worker_pressure", S::Blocked),
        ],
        OperationPostureReport::ok(["runtime", "storage", "search", "memory", "pack"]),
    );

    ensure_equal(
        DoctorPosture::from_checks(&doctor_checks, None),
        DoctorPosture::Ok,
        "doctor advisory error does not block top-line",
    )?;
    ensure_equal(
        doctor_checks.iter().all(CheckResult::is_topline_healthy),
        true,
        "doctor advisory error is top-line healthy",
    )?;
    ensure_equal(
        status_report.overall,
        S::Ok,
        "status advisory blocked subsystem does not block top-line",
    )?;
    ensure_equal(
        status_report.subsystems.iter().any(|subsystem| {
            subsystem.id == "rch_worker_pressure" && subsystem.status == S::Blocked
        }),
        true,
        "status advisory blocked subsystem remains visible",
    )
}

#[test]
fn embedding_posture_warning_is_advisory_boundary() -> TestResult {
    let checks = vec![
        CheckResult::ok("runtime", "ok"),
        CheckResult::ok("workspace", "ok"),
        CheckResult::ok("database", "ok"),
        CheckResult::ok("search_index", "ok"),
        CheckResult::warning(
            "embedding_posture",
            "hash fallback active but retrieval remains usable",
            INDEX_STALE,
        )
        .advisory(),
    ];

    let embedding_check = checks
        .iter()
        .find(|check| check.name == "embedding_posture")
        .ok_or_else(|| "missing embedding_posture fixture check".to_string())?;

    ensure_equal(
        embedding_check.tier,
        CheckTier::Advisory,
        "embedding posture check tier",
    )?;
    ensure_equal(
        embedding_check.is_topline_healthy(),
        true,
        "embedding posture warning stays top-line healthy",
    )?;
    ensure_equal(
        DoctorPosture::from_checks(&checks, None),
        DoctorPosture::Ok,
        "embedding posture warning does not degrade doctor posture",
    )
}

#[test]
fn core_errors_block_both_toplines_even_with_advisories_present() -> TestResult {
    use SubsystemPostureStatus as S;

    let doctor_checks = vec![
        CheckResult::ok("runtime", "ok"),
        CheckResult::ok("workspace", "ok"),
        CheckResult::error("database", "database unavailable", INDEX_STALE),
        CheckResult::warning("cass", "cass limited", INDEX_STALE).advisory(),
    ];
    let status_report = WorkspacePostureReport::new_core_overall(
        vec![
            SubsystemPostureReport::new("runtime", S::Ok),
            SubsystemPostureReport::new("storage", S::Blocked),
            SubsystemPostureReport::new("search", S::Ok),
            SubsystemPostureReport::new("memory", S::Ok),
            SubsystemPostureReport::new("pack", S::Ok),
            SubsystemPostureReport::new("rch_worker_pressure", S::DegradedRecoverable),
        ],
        OperationPostureReport::ok(["runtime", "storage", "search", "memory", "pack"]),
    );

    ensure_equal(
        DoctorPosture::from_checks(&doctor_checks, None),
        DoctorPosture::Blocked,
        "doctor core error blocks top-line",
    )?;
    ensure_equal(
        doctor_checks.iter().all(CheckResult::is_topline_healthy),
        false,
        "doctor core error flips healthy flag",
    )?;
    ensure_equal(
        status_report.overall,
        S::Blocked,
        "status core blocked subsystem blocks top-line",
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
