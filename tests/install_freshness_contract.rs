//! Contract fixtures for the install-freshness authority model (bd-3utv2.5,
//! epic install-freshness bd-3utv2). These pin the deterministic verdict, the
//! blocking finding codes, the version comparison, and the recovery guidance for
//! the duplicate-PATH and version-skew states the live machine actually reports,
//! so install-check and claim-gate consumers (bd-3utv2.3) cannot drift.
//!
//! Hermetic: every case constructs `InstallPathAnalysis` / `CurrentBinary` /
//! `InstallVersionEvidence` directly (no real PATH, no filesystem, no Cargo), so
//! the contract is reproducible on any host. Verified via
//! `cargo test --test install_freshness_contract` (RCH).

use ee::core::install::evaluate_install_freshness;
use ee::models::install::{
    CurrentBinary, InstallFindingCode, InstallFreshnessVerdict, InstallPathAnalysis,
    InstallPathStatus, InstallVersionEvidence, PathBinary,
};

type TestResult = Result<(), String>;

fn source(version: Option<&str>) -> InstallVersionEvidence {
    InstallVersionEvidence {
        version: version.map(str::to_owned),
        source: "cargo_toml".to_owned(),
        status: if version.is_some() {
            "reported".to_owned()
        } else {
            "missing".to_owned()
        },
        path: None,
        path_class: None,
    }
}

fn current(path: Option<&str>, version: &str) -> CurrentBinary {
    CurrentBinary {
        path: path.map(str::to_owned),
        version: version.to_owned(),
        source: "build_metadata".to_owned(),
    }
}

/// `first_is_current = Some(true)` => the first PATH binary IS the running one
/// (not shadowed). `Some(false)` => a different binary shadows it. `None` =>
/// no binaries on PATH at all.
fn path_analysis(
    on_path: bool,
    first_is_current: Option<bool>,
    duplicate_count: usize,
) -> InstallPathAnalysis {
    let binaries = match first_is_current {
        Some(is_current) => vec![PathBinary {
            path: "/fake/bin/ee".to_owned(),
            ordinal: 0,
            is_current_binary: is_current,
            version: None,
            version_status: None,
        }],
        None => Vec::new(),
    };
    let status = if !on_path {
        InstallPathStatus::Missing
    } else if first_is_current == Some(false) {
        InstallPathStatus::Shadowed
    } else if duplicate_count > 1 {
        InstallPathStatus::Duplicate
    } else {
        InstallPathStatus::Ok
    };
    InstallPathAnalysis {
        status,
        path_entries: vec!["/fake/bin".to_owned()],
        binaries,
        first_binary: first_is_current.map(|_| "/fake/bin/ee".to_owned()),
        current_binary_on_path: on_path,
        duplicate_count,
    }
}

struct Case {
    name: &'static str,
    source: Option<&'static str>,
    current_path: Option<&'static str>,
    current_version: &'static str,
    on_path: bool,
    first_is_current: Option<bool>,
    duplicate_count: usize,
    required_surfaces: &'static [&'static str],
    expect_verdict: InstallFreshnessVerdict,
    expect_finding: Option<InstallFindingCode>,
    expect_comparison: &'static str,
    /// A stable, distinctive substring the recovery guidance must contain.
    expect_repair_contains: &'static str,
}

fn cases() -> Vec<Case> {
    use InstallFindingCode as C;
    use InstallFreshnessVerdict as V;
    vec![
        Case {
            name: "fresh: equal versions, on PATH, not shadowed",
            source: Some("0.9.0"),
            current_path: Some("/fake/bin/ee"),
            current_version: "0.9.0",
            on_path: true,
            first_is_current: Some(true),
            duplicate_count: 1,
            required_surfaces: &["install_check", "claim_gate_install_freshness"],
            expect_verdict: V::Fresh,
            expect_finding: None,
            expect_comparison: "equal",
            expect_repair_contains: "No freshness repair required.",
        },
        Case {
            name: "stale: source newer than installed (installed older)",
            source: Some("0.9.0"),
            current_path: Some("/fake/bin/ee"),
            current_version: "0.8.0",
            on_path: true,
            first_is_current: Some(true),
            duplicate_count: 1,
            required_surfaces: &[],
            expect_verdict: V::Stale,
            expect_finding: Some(C::InstalledBinaryStale),
            expect_comparison: "installed_older_than_source",
            expect_repair_contains: "ee install plan --manifest",
        },
        Case {
            name: "version-skew: installed newer than source still fails closed",
            source: Some("0.8.0"),
            current_path: Some("/fake/bin/ee"),
            current_version: "0.9.0",
            on_path: true,
            first_is_current: Some(true),
            duplicate_count: 1,
            required_surfaces: &[],
            expect_verdict: V::Stale,
            expect_finding: Some(C::InstalledBinaryStale),
            expect_comparison: "installed_newer_than_source",
            expect_repair_contains: "ee install plan --manifest",
        },
        Case {
            name: "shadowed: a different PATH binary shadows the running one",
            source: Some("0.9.0"),
            current_path: Some("/fake/run/ee"),
            current_version: "0.9.0",
            on_path: true,
            first_is_current: Some(false),
            duplicate_count: 1,
            required_surfaces: &[],
            expect_verdict: V::ShadowedBinary,
            expect_finding: Some(C::CurrentBinaryShadowed),
            expect_comparison: "equal",
            expect_repair_contains: "Run the first ee binary found in PATH",
        },
        Case {
            name: "duplicate PATH: stale shadow among duplicates fails closed as shadowed",
            source: Some("0.9.0"),
            current_path: Some("/fake/run/ee"),
            current_version: "0.9.0",
            on_path: true,
            first_is_current: Some(false),
            duplicate_count: 2,
            required_surfaces: &[],
            expect_verdict: V::ShadowedBinary,
            expect_finding: Some(C::CurrentBinaryShadowed),
            expect_comparison: "equal",
            expect_repair_contains: "Run the first ee binary found in PATH",
        },
        Case {
            name: "path binary missing: running binary not resolvable on PATH",
            source: Some("0.9.0"),
            current_path: Some("/fake/run/ee"),
            current_version: "0.9.0",
            on_path: false,
            first_is_current: None,
            duplicate_count: 0,
            required_surfaces: &[],
            expect_verdict: V::PathBinaryMissing,
            expect_finding: Some(C::BinaryNotOnPath),
            expect_comparison: "equal",
            expect_repair_contains: "Plan adoption into PATH",
        },
        Case {
            name: "offline / no manifest: source version unknown",
            source: None,
            current_path: Some("/fake/bin/ee"),
            current_version: "0.9.0",
            on_path: true,
            first_is_current: Some(true),
            duplicate_count: 1,
            required_surfaces: &[],
            expect_verdict: V::UnknownSourceVersion,
            expect_finding: Some(C::SourceVersionUnknown),
            expect_comparison: "unknown",
            expect_repair_contains: "operator-exception request",
        },
        Case {
            name: "installed version unknown: running binary reports no build version",
            source: Some("0.9.0"),
            current_path: Some("/fake/bin/ee"),
            current_version: "",
            on_path: true,
            first_is_current: Some(true),
            duplicate_count: 1,
            required_surfaces: &[],
            expect_verdict: V::UnknownInstalledVersion,
            expect_finding: Some(C::InstalledVersionUnknown),
            expect_comparison: "unknown",
            expect_repair_contains: "build version metadata before trusting",
        },
        Case {
            name: "unsupported claim-gate surface: required surface not provided by installed ee",
            source: Some("0.9.0"),
            current_path: Some("/fake/bin/ee"),
            current_version: "0.9.0",
            on_path: true,
            first_is_current: Some(true),
            duplicate_count: 1,
            required_surfaces: &["totally_unsupported_surface"],
            expect_verdict: V::MissingRequiredSurface,
            expect_finding: Some(C::RequiredSurfaceMissing),
            expect_comparison: "equal",
            expect_repair_contains: "every required automation surface",
        },
    ]
}

#[test]
fn install_freshness_verdict_matrix_is_stable_bd_3utv2_5() -> TestResult {
    for case in cases() {
        let report = evaluate_install_freshness(
            source(case.source),
            &current(case.current_path, case.current_version),
            &path_analysis(case.on_path, case.first_is_current, case.duplicate_count),
            &[],
            case.required_surfaces,
        );

        if report.verdict != case.expect_verdict {
            return Err(format!(
                "case `{}`: expected verdict {:?}, got {:?}",
                case.name, case.expect_verdict, report.verdict
            ));
        }

        // Only the fresh verdict may authoritatively represent the checkout.
        let expect_authoritative = case.expect_verdict == InstallFreshnessVerdict::Fresh;
        if report.authoritative != expect_authoritative {
            return Err(format!(
                "case `{}`: expected authoritative={expect_authoritative}, got {}",
                case.name, report.authoritative
            ));
        }

        if report.comparison != case.expect_comparison {
            return Err(format!(
                "case `{}`: expected comparison `{}`, got `{}`",
                case.name, case.expect_comparison, report.comparison
            ));
        }

        match case.expect_finding {
            None => {
                if !report.blocking_findings.is_empty() {
                    return Err(format!(
                        "case `{}`: expected no blocking findings, got {:?}",
                        case.name, report.blocking_findings
                    ));
                }
            }
            Some(code) => {
                if report.blocking_findings != vec![code] {
                    return Err(format!(
                        "case `{}`: expected blocking findings [{:?}], got {:?}",
                        case.name, code, report.blocking_findings
                    ));
                }
            }
        }

        // Recovery guidance must be present and carry the stable, distinctive
        // instruction for this state.
        if !report.repair.contains(case.expect_repair_contains) {
            return Err(format!(
                "case `{}`: recovery guidance missing `{}`, got `{}`",
                case.name, case.expect_repair_contains, report.repair
            ));
        }

        // Load-bearing safety invariant of the install-freshness epic: recovery
        // guidance must NEVER tell an agent to build with local Cargo.
        let lowered = report.repair.to_ascii_lowercase();
        if lowered.contains("cargo build") || lowered.contains("cargo install") {
            return Err(format!(
                "case `{}`: recovery guidance must never recommend a local Cargo build, got `{}`",
                case.name, report.repair
            ));
        }
    }
    Ok(())
}

#[test]
fn install_freshness_verdict_str_tokens_are_stable_bd_3utv2_5() -> TestResult {
    use InstallFreshnessVerdict as V;
    let pairs = [
        (V::Fresh, "fresh"),
        (V::Stale, "stale"),
        (V::UnknownSourceVersion, "unknown_source_version"),
        (V::UnknownInstalledVersion, "unknown_installed_version"),
        (V::MissingRequiredSurface, "missing_required_surface"),
        (V::PathBinaryMissing, "path_binary_missing"),
        (V::ShadowedBinary, "shadowed_binary"),
    ];
    for (verdict, token) in pairs {
        if verdict.as_str() != token {
            return Err(format!(
                "verdict {verdict:?} token drifted: expected `{token}`, got `{}`",
                verdict.as_str()
            ));
        }
    }
    Ok(())
}
