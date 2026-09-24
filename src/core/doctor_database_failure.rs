//! Keep inability to inspect a store separate from evidence of lost data.
//!
//! In particular, migration drift and a writer that holds a lock must never
//! inherit the damaged-file recovery plan. Header-detected corruption remains
//! the responsibility of `database_unreadable`; an opaque storage error alone
//! is not sufficient evidence to recommend discarding a store (bd-ixxzq).

use std::fmt::Display;

use crate::models::error_codes::{self, ErrorCode};

use super::CheckResult;

/// Both the DB layer and workspace layer reach this boundary. Do not assume
/// they have the same error type, or discard the original failure message.
/// Unknown failures stay unavailable, not corrupt, and block dependent writes.
pub(super) fn check(error: &impl Display) -> CheckResult {
    let message = error.to_string();
    let code = classify_message(&message);
    let mut result = CheckResult::error(
        "database",
        format!("Database readiness check failed: {message}"),
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

fn classify_message(message: &str) -> ErrorCode {
    if is_lock_failure(message) {
        error_codes::DATABASE_LOCKED
    } else if message
        .trim_start()
        .to_ascii_lowercase()
        .starts_with("migration history drifted for version ")
    {
        error_codes::MIGRATION_DRIFT
    } else {
        // A changed diagnostic, permission failure, or unfamiliar engine error
        // cannot establish corruption. The safe fallback is guidance-only.
        error_codes::DATABASE_UNAVAILABLE
    }
}

/// The two error layers do not share a typed lock variant. Recognize only a
/// leading producer diagnostic; an incidental word in a path, quoted SQL or
/// memory body is not a lock failure.
/// A changed/unknown producer format safely falls back to unavailable, which
/// is also guidance-only and suppresses dependent writes.
fn is_lock_failure(message: &str) -> bool {
    let message = message.trim_start().to_ascii_lowercase();
    message.starts_with("database write lock holder made no progress")
        || message.starts_with("database write lock acquisition timed out")
        || message.starts_with("database group-commit gate acquisition timed out")
        || message == "database is locked"
        || message.starts_with("database is locked:")
        || message == "database table is locked"
        || message.starts_with("database table is locked:")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::doctor_fixers::{
        FixMode, fix_dispatch_for_finding, fix_finding_for_check, fix_mode_for_check,
        store_unreadable, unresolved_core_checks,
    };
    use crate::core::doctor_runtime::Op;
    use std::path::Path;

    #[test]
    fn migration_history_drift_is_not_corruption_or_pending_migration() {
        let result = check(&"migration history drifted for version 42: expected abc, found def");
        assert_eq!(result.name, "database");
        assert_eq!(result.error_code, Some(error_codes::MIGRATION_DRIFT));
        assert!(!result.is_topline_healthy());
        assert_eq!(
            fix_mode_for_check(Some("EE-E702"), "database", true),
            (FixMode::AutoGuidance, Some("database_migration_drift"))
        );
        assert_ne!(result.error_code, Some(error_codes::MIGRATION_REQUIRED));
    }

    #[test]
    fn known_writer_and_engine_lock_failures_select_wait_only_recovery() {
        for message in [
            "database write lock holder made no progress for 38000ms",
            "database write lock acquisition timed out after 30000ms",
            "database group-commit gate acquisition timed out after 30000ms",
            "database is locked",
            "database is locked: active transaction",
            "database table is locked",
            "database table is locked: memories",
            "  DATABASE IS LOCKED",
        ] {
            let result = check(&message);
            assert_eq!(
                result.error_code,
                Some(error_codes::DATABASE_LOCKED),
                "{message}"
            );
            assert!(!result.is_topline_healthy());
            assert_eq!(
                fix_mode_for_check(Some("EE-E201"), "database", true),
                (FixMode::AutoGuidance, Some("database_locked"))
            );
        }
    }

    #[test]
    fn unrelated_lock_words_and_unknown_storage_failures_do_not_infer_corruption() {
        for message in [
            "permission denied opening the database",
            "I/O error while reading a page",
            "failed to open /private/database-is-locked/ee.db",
            "query contains 'database is locked'",
            "migration_drift is a user-supplied identifier",
            "storage engine returned an unrecognized error",
            "",
        ] {
            let result = check(&message);
            assert_eq!(
                result.error_code,
                Some(error_codes::DATABASE_UNAVAILABLE),
                "{message}"
            );
            assert!(!result.is_topline_healthy());
            assert_eq!(
                fix_mode_for_check(Some("EE-E207"), "database", true),
                (FixMode::AutoGuidance, Some("database_unavailable"))
            );
        }
    }

    #[test]
    fn display_errors_are_supported_without_an_error_type_conversion() {
        let error = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "permission denied");
        let result = check(&error);
        assert_eq!(result.error_code, Some(error_codes::DATABASE_UNAVAILABLE));
        assert!(result.message.contains("permission denied"));
    }

    #[test]
    fn quoted_or_prefixed_migration_words_do_not_manufacture_a_drift_diagnosis() {
        for message in [
            "query contains migration history drifted for version 42",
            "failed to read /migration-history-drifted/ee.db",
            "unknown migration engine failure",
        ] {
            assert_eq!(classify_message(message), error_codes::DATABASE_UNAVAILABLE);
        }
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
        let checks = [
            check(&"database write lock holder made no progress for PRIVATEms"),
            check(&"migration history drifted for version 42: PRIVATE-MIGRATION-HISTORY"),
        ];
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

        for (message, finding) in [
            ("database is locked", "database_locked"),
            ("permission denied", "database_unavailable"),
            (
                "migration history drifted for version 42: expected abc, found def",
                "database_migration_drift",
            ),
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
                    check(&message),
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
