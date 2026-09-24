//! Keep inability to inspect a store separate from evidence of lost data.
//!
//! In particular, migration drift and a writer that holds a lock must never
//! inherit the damaged-file recovery plan. Header-detected corruption remains
//! the responsibility of `database_unreadable`; an opaque storage error alone
//! is not sufficient evidence to recommend discarding a store (bd-ixxzq).
//!
//! The route is decided from the error's own kind, never from its rendered
//! text. The first version keyed on message prefixes and missed both real
//! producers: the flock gate's error is wrapped as "database transaction begin
//! failed for path '...': database write lock holder made no progress ...",
//! and drift renders as "EE-E040 migration_drift: applied migration 1
//! drifted; ...". Both fell through to "unavailable" (measured at 80ed63d5d,
//! bd-ixxzq c10142).

use std::fmt::Display;

use crate::db::DbError;
use crate::models::DomainError;
use crate::models::error_codes::{self, ErrorCode};

use super::CheckResult;

/// A failed store inspection that can say what kind of failure it is from its
/// own structure. The DB layer and the workspace layer both reach doctor's
/// database check with different error types, so each says what it knows.
pub(super) trait InspectionFailure: Display {
    /// The typed route for this failure, or `None` when its kind is not known.
    fn inspection_code(&self) -> Option<ErrorCode>;
}

impl InspectionFailure for DbError {
    fn inspection_code(&self) -> Option<ErrorCode> {
        if self.error_id() == Some(crate::db::MIGRATION_DRIFT_ERROR_ID) {
            Some(error_codes::MIGRATION_DRIFT)
        } else if self.is_write_lock_contention() {
            Some(error_codes::DATABASE_LOCKED)
        } else {
            None
        }
    }
}

/// A workspace-layer error carries no typed storage kind, so it is never read
/// as a lock or as drift. It stays "unavailable", which is also guidance-only.
impl InspectionFailure for DomainError {
    fn inspection_code(&self) -> Option<ErrorCode> {
        None
    }
}

/// Unknown failures stay unavailable, not corrupt, and block dependent writes.
/// The original failure message is kept in the check's message.
pub(super) fn check(error: &impl InspectionFailure) -> CheckResult {
    let code = error
        .inspection_code()
        .unwrap_or(error_codes::DATABASE_UNAVAILABLE);
    let mut result = CheckResult::error(
        "database",
        format!("Database readiness check failed: {error}"),
        code,
    );
    // Fix-plan includes only checks with a repair hint. The shared lock code
    // intentionally has no default repair, but doctor has safe retry guidance.
    if code == error_codes::DATABASE_LOCKED {
        result.repair = Some(
            "Wait for the active writer to finish, then rerun `ee doctor --workspace . --json`; leave the database, sidecars and lock file in place",
        );
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::doctor_fixers::{
        FixMode, fix_dispatch_for_finding, fix_finding_for_check, fix_mode_for_check,
        store_unreadable, unresolved_core_checks,
    };
    use crate::core::doctor_runtime::Op;
    use crate::db::{DbError, DbOperation};
    use crate::models::DomainError;
    use std::path::Path;
    use std::time::Duration;

    // Verbatim doctor messages captured on a stamped 80ed63d5d release build
    // (vmi1227854, 2026-09-24 16:01-16:06Z; bd-ixxzq c10142). The first is a
    // live python3 flock(2) holder on .ee/ee.write.lock; the second is the
    // lowest applied migration's checksum set to blake3 zeros. The prefix
    // classifier sent both to EE-E207. The tests below rebuild each error
    // through its real producer and assert the rendered message is
    // byte-identical to the capture, so they cannot drift into invented text.
    const CAPTURED_LOCK_MESSAGE: &str = "Database readiness check failed: database transaction begin failed for path '/tmp/ee-rl-RustMarten.1vNOjG/lh/.ee/ee.write.lock': database write lock holder made no progress for 38000ms: Resource temporarily unavailable (os error 11)";
    const CAPTURED_DRIFT_MESSAGE: &str = "Database readiness check failed: EE-E040 migration_drift: applied migration 1 drifted; expected init_schema (blake3:d6c4d1a45780310b2d7cb218f35872c216d77192dc086b8da050d0496c61f449), found init_schema (blake3:0000000000000000000000000000000000000000000000000000000000000000)";

    fn captured_lock_error() -> DbError {
        crate::db::write_lock_stagnant_error(
            "/tmp/ee-rl-RustMarten.1vNOjG/lh/.ee/ee.write.lock".into(),
            Duration::from_secs(38),
            &"Resource temporarily unavailable (os error 11)",
        )
    }

    fn captured_drift_error() -> DbError {
        DbError::MigrationDrift {
            version: 1,
            expected_name: Some("init_schema".to_owned()),
            actual_name: "init_schema".to_owned(),
            expected_checksum: Some(
                "blake3:d6c4d1a45780310b2d7cb218f35872c216d77192dc086b8da050d0496c61f449"
                    .to_owned(),
            ),
            actual_checksum:
                "blake3:0000000000000000000000000000000000000000000000000000000000000000".to_owned(),
        }
    }

    fn sql_error(kind: sqlmodel_core::error::QueryErrorKind, message: &str) -> DbError {
        DbError::SqlModel {
            operation: DbOperation::Query,
            source: Box::new(sqlmodel_core::Error::Query(
                sqlmodel_core::error::QueryError {
                    kind,
                    sql: None,
                    sqlstate: None,
                    message: message.to_owned(),
                    detail: None,
                    hint: None,
                    position: None,
                    source: None,
                },
            )),
        }
    }

    #[test]
    fn the_captured_migration_drift_routes_to_drift_not_corruption_or_pending() {
        let result = check(&captured_drift_error());
        assert_eq!(result.message, CAPTURED_DRIFT_MESSAGE);
        assert_eq!(result.name, "database");
        assert_eq!(result.error_code, Some(error_codes::MIGRATION_DRIFT));
        assert!(!result.is_topline_healthy());
        assert_eq!(
            fix_mode_for_check(Some("EE-E702"), "database", true),
            (FixMode::AutoGuidance, Some("database_migration_drift"))
        );
        assert_ne!(result.error_code, Some(error_codes::MIGRATION_REQUIRED));
        assert_ne!(result.error_code, Some(error_codes::DATABASE_CORRUPTED));
    }

    #[test]
    fn the_captured_held_lock_and_typed_contention_select_wait_only_recovery() {
        let captured = check(&captured_lock_error());
        assert_eq!(captured.message, CAPTURED_LOCK_MESSAGE);
        let deadline = crate::db::write_lock_deadline_error(
            "/tmp/ws/.ee/ee.write.lock".into(),
            Duration::from_secs(300),
            &"Resource temporarily unavailable (os error 11)",
        );
        for (label, result) in [
            ("captured stagnant holder", captured),
            ("flock wait deadline", check(&deadline)),
            (
                "typed busy timeout",
                check(&sql_error(
                    sqlmodel_core::error::QueryErrorKind::Timeout,
                    "database is locked",
                )),
            ),
            (
                "typed deadlock",
                check(&sql_error(
                    sqlmodel_core::error::QueryErrorKind::Deadlock,
                    "deadlock detected",
                )),
            ),
        ] {
            assert_eq!(
                result.error_code,
                Some(error_codes::DATABASE_LOCKED),
                "{label}"
            );
            assert!(result.repair.is_some(), "{label}");
            assert!(!result.is_topline_healthy(), "{label}");
        }
        assert_eq!(
            fix_mode_for_check(Some("EE-E201"), "database", true),
            (FixMode::AutoGuidance, Some("database_locked"))
        );
    }

    /// Text never manufactures a route: only the error's kind does. Each of
    /// these carries lock or drift words, or is an unknown failure, and none
    /// may be read as a lock, as drift, or as corruption.
    #[test]
    fn words_in_untyped_errors_do_not_infer_a_lock_drift_or_corruption() {
        let failures: Vec<(&str, CheckResult)> = vec![
            (
                "flock failure that is not contention",
                check(&DbError::InvalidPath {
                    operation: DbOperation::BeginTransaction,
                    path: "/tmp/ws/.ee/ee.write.lock".into(),
                    message: "database write lock acquisition failed: EPERM".to_owned(),
                }),
            ),
            (
                "lock words on another operation",
                check(&DbError::InvalidPath {
                    operation: DbOperation::OpenReadWrite,
                    path: "/tmp/database write lock holder made no progress/ee.db".into(),
                    message: "database write lock holder made no progress".to_owned(),
                }),
            ),
            (
                "lock words under a non-contention query kind",
                check(&sql_error(
                    sqlmodel_core::error::QueryErrorKind::Syntax,
                    "near \"database is locked\": syntax error",
                )),
            ),
            (
                "malformed row naming drift",
                check(&DbError::MalformedRow {
                    operation: DbOperation::Query,
                    message: "EE-E040 migration_drift: applied migration 1 drifted".to_owned(),
                }),
            ),
            (
                "workspace-layer error quoting both captures",
                check(&DomainError::Storage {
                    message: format!("{CAPTURED_LOCK_MESSAGE}; {CAPTURED_DRIFT_MESSAGE}"),
                    repair: None,
                }),
            ),
        ];
        for (label, result) in failures {
            assert_eq!(
                result.error_code,
                Some(error_codes::DATABASE_UNAVAILABLE),
                "{label}"
            );
            assert_ne!(
                result.error_code,
                Some(error_codes::DATABASE_CORRUPTED),
                "{label}"
            );
            // CheckResult::error fills repair from the code's default, so an
            // untyped failure carries the "unavailable" guidance, never the
            // lock's wait-for-the-writer guidance.
            assert_eq!(
                result.repair,
                error_codes::DATABASE_UNAVAILABLE.default_repair,
                "{label}"
            );
            assert!(!result.is_topline_healthy(), "{label}");
        }
        assert_eq!(
            fix_mode_for_check(Some("EE-E207"), "database", true),
            (FixMode::AutoGuidance, Some("database_unavailable"))
        );
    }

    #[test]
    fn pending_migration_retains_its_distinct_existing_contract() {
        // Only a successful needs_migration() result grants migration admission;
        // a failed inspection must not be downgraded to a pending schema change.
        assert!(!store_unreadable([("database", Some("EE-E700"))]));
        assert_eq!(
            fix_finding_for_check(Some("EE-E700"), "database", false),
            Some("schema_migration_pending")
        );
    }

    #[test]
    fn every_blocked_source_suppresses_dependent_index_and_migration_dispatches() {
        for code in [
            "EE-E200", "EE-E201", "EE-E202", "EE-E206", "EE-E207", "EE-E702",
        ] {
            let blocked = store_unreadable([("database", Some(code))]);
            assert!(blocked, "{code}");
            for (name, child_code) in [
                ("search_index", Some("EE-E300")),
                ("search_index", Some("EE-E301")),
                ("search_index", None),
                ("schema", Some("EE-E700")),
            ] {
                assert_eq!(
                    fix_finding_for_check(child_code, name, blocked),
                    None,
                    "{code}"
                );
                assert_eq!(
                    fix_mode_for_check(child_code, name, blocked),
                    (FixMode::Manual, None)
                );
            }
        }
    }

    #[test]
    fn a_healthy_retry_reenables_index_repair_without_a_persistent_blocker() {
        let blocked = store_unreadable([("database", None)]);
        assert!(!blocked);
        assert_eq!(
            fix_mode_for_check(Some("EE-E300"), "search_index", blocked),
            (FixMode::AutoRepair, Some("search_index_missing"))
        );
        // A similarly coded optional check is not a database diagnosis.
        assert!(!store_unreadable([("other_check", Some("EE-E201"))]));
    }

    #[test]
    fn non_corruption_findings_have_only_non_destructive_guidance() {
        for finding in [
            "database_locked",
            "database_unavailable",
            "database_migration_drift",
        ] {
            let dispatch = fix_dispatch_for_finding(Path::new("workspace"), finding)
                .expect("registered guidance dispatcher");
            assert_eq!(dispatch.finding_code, finding);
            assert_eq!(
                dispatch.path,
                Path::new("workspace").join(".ee").join("ee.db")
            );
            assert!(dispatch.op.is_advisory());
            let Op::Manual { steps } = dispatch.op else {
                panic!("no writing operation is allowed for {finding}");
            };
            assert!(!steps.is_empty());
            let guidance = steps.join(" ");
            for unsafe_hint in [
                "accept the loss",
                "move it aside",
                "start an empty",
                "ee init",
                "ee backup restore",
            ] {
                assert!(!guidance.contains(unsafe_hint), "{finding}: {guidance}");
            }
        }
    }

    #[test]
    fn unresolved_checks_expose_static_causes_not_private_error_details() {
        let private_lock = crate::db::write_lock_stagnant_error(
            "/tmp/PRIVATE-workspace/.ee/ee.write.lock".into(),
            Duration::from_secs(38),
            &"PRIVATE os detail",
        );
        let mut private_drift = captured_drift_error();
        if let DbError::MigrationDrift {
            actual_checksum, ..
        } = &mut private_drift
        {
            *actual_checksum = "PRIVATE-MIGRATION-HISTORY".to_owned();
        }
        let checks = [check(&private_lock), check(&private_drift)];
        assert!(checks.iter().all(|check| check.message.contains("PRIVATE")));
        let pending = unresolved_core_checks(&checks);
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].fix_finding, Some("database_locked"));
        assert_eq!(pending[1].fix_finding, Some("database_migration_drift"));
        assert!(
            pending
                .iter()
                .all(|entry| entry.fix_mode == "auto_guidance")
        );
        let json = serde_json::to_string(&pending).expect("serializable pending checks");
        assert!(!json.contains("PRIVATE"));
    }

    #[test]
    fn fix_plan_retains_access_guidance_and_never_admits_dependent_repairs() {
        use super::super::{
            CheckSeverity, DoctorReport, FlightRecorderStatusReport, Posture,
            RchVerifyLedgerStatusReport, RchWorkerPressureReport, VerificationPostureReport,
            gather_qos_posture, singleflight_posture_report,
        };

        for (database_check, finding) in [
            (check(&captured_lock_error()), "database_locked"),
            (
                check(&DbError::InvalidPath {
                    operation: DbOperation::OpenReadWrite,
                    path: "/tmp/ws/.ee/ee.db".into(),
                    message: "permission denied".to_owned(),
                }),
                "database_unavailable",
            ),
            (check(&captured_drift_error()), "database_migration_drift"),
        ] {
            let report = DoctorReport {
                version: "test",
                overall_healthy: false,
                posture: Posture::Blocked,
                singleflight_posture: singleflight_posture_report(),
                qos_posture: gather_qos_posture(None),
                rch_worker_pressure: RchWorkerPressureReport::pressure_unknown(),
                verification_posture: VerificationPostureReport::not_inspected(),
                verification_ledger: RchVerifyLedgerStatusReport::not_inspected(),
                host_calibration: None,
                flight_recorder: FlightRecorderStatusReport::disabled(
                    Path::new("obs/flight_recorder").to_path_buf(),
                ),
                checks: vec![
                    database_check,
                    CheckResult::warning(
                        "search_index",
                        "inspection failed",
                        error_codes::INDEX_NOT_FOUND,
                    ),
                    CheckResult::warning(
                        "schema",
                        "dependent migration",
                        error_codes::MIGRATION_REQUIRED,
                    ),
                ],
            };
            let plan = report.to_fix_plan();
            assert_eq!(
                plan.steps.len(),
                3,
                "all guidance remains visible: {finding}"
            );
            assert_eq!(plan.fixable_issues, 0);
            assert_eq!(plan.steps[0].severity, CheckSeverity::Error);
            assert_eq!(plan.steps[0].fix_finding, Some(finding));
            assert_eq!(plan.steps[0].fix_mode, FixMode::AutoGuidance);
            assert!(!plan.steps[0].command.is_empty());
            for step in &plan.steps[1..] {
                assert_eq!(step.fix_mode, FixMode::Manual);
                assert_eq!(step.fix_finding, None);
            }
        }
    }

    #[test]
    fn new_codes_are_registered_without_changing_existing_damage_codes() {
        assert_eq!(
            error_codes::lookup("EE-E207"),
            Some(error_codes::DATABASE_UNAVAILABLE)
        );
        assert_eq!(
            error_codes::lookup("EE-E702"),
            Some(error_codes::MIGRATION_DRIFT)
        );
        assert_eq!(
            error_codes::lookup("EE-E202"),
            Some(error_codes::DATABASE_CORRUPTED)
        );
        assert_eq!(
            fix_finding_for_check(Some("EE-E202"), "database", true),
            Some("database_corrupted")
        );
        assert_eq!(
            fix_finding_for_check(Some("EE-E206"), "database", true),
            Some("database_empty")
        );
    }
}
