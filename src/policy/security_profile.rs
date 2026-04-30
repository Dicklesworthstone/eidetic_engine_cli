//! Security profiles and file-permission diagnostics (EE-279).
//!
//! Provides security profiles that control trust thresholds, redaction policies,
//! and file permission requirements. Profiles are validated at startup and can
//! be selected via `EE_SECURITY_PROFILE` or `--security-profile`.
//!
//! Available profiles:
//! - `default`: Balanced security for normal operation
//! - `strict`: High-security mode with aggressive redaction and low trust ceilings
//! - `permissive`: Relaxed security for development/debugging
//!
//! File permission diagnostics check that the workspace database and config files
//! have appropriate permissions (not world-readable, owned by current user, etc.).

use std::fmt;
use std::path::Path;
use std::str::FromStr;

/// Security profile controlling trust and access policies.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum SecurityProfile {
    /// Balanced security for normal operation.
    #[default]
    Default,
    /// High-security mode with aggressive policies.
    Strict,
    /// Relaxed security for development/debugging.
    Permissive,
}

impl SecurityProfile {
    /// Stable lowercase wire form for JSON output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Strict => "strict",
            Self::Permissive => "permissive",
        }
    }

    /// All profiles in stable order.
    #[must_use]
    pub const fn all() -> [Self; 3] {
        [Self::Default, Self::Strict, Self::Permissive]
    }

    /// Trust ceiling for import sources under this profile.
    #[must_use]
    pub const fn trust_ceiling(self) -> f32 {
        match self {
            Self::Default => 1.0,
            Self::Strict => 0.8,
            Self::Permissive => 1.0,
        }
    }

    /// Trust floor for import sources under this profile.
    #[must_use]
    pub const fn trust_floor(self) -> f32 {
        match self {
            Self::Default => 0.05,
            Self::Strict => 0.01,
            Self::Permissive => 0.10,
        }
    }

    /// Whether redaction is enforced in output.
    #[must_use]
    pub const fn enforce_redaction(self) -> bool {
        match self {
            Self::Default => true,
            Self::Strict => true,
            Self::Permissive => false,
        }
    }

    /// Whether file permission checks are enforced.
    #[must_use]
    pub const fn enforce_file_permissions(self) -> bool {
        match self {
            Self::Default => true,
            Self::Strict => true,
            Self::Permissive => false,
        }
    }

    /// Maximum allowed permission bits for database files (octal).
    /// 0o600 = owner read/write only.
    #[must_use]
    pub const fn max_db_permissions(self) -> u32 {
        match self {
            Self::Default => 0o600,
            Self::Strict => 0o600,
            Self::Permissive => 0o666,
        }
    }

    /// Maximum allowed permission bits for config files (octal).
    /// 0o644 = owner read/write, group/other read.
    #[must_use]
    pub const fn max_config_permissions(self) -> u32 {
        match self {
            Self::Default => 0o644,
            Self::Strict => 0o600,
            Self::Permissive => 0o666,
        }
    }

    /// Whether to allow importing from untrusted sources.
    #[must_use]
    pub const fn allow_untrusted_imports(self) -> bool {
        match self {
            Self::Default => true,
            Self::Strict => false,
            Self::Permissive => true,
        }
    }
}

impl fmt::Display for SecurityProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for SecurityProfile {
    type Err = ParseSecurityProfileError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "default" => Ok(Self::Default),
            "strict" => Ok(Self::Strict),
            "permissive" => Ok(Self::Permissive),
            _ => Err(ParseSecurityProfileError {
                input: s.to_owned(),
            }),
        }
    }
}

/// Error when parsing a security profile string.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseSecurityProfileError {
    /// The invalid input string.
    pub input: String,
}

impl fmt::Display for ParseSecurityProfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown security profile `{}`; expected one of: default, strict, permissive",
            self.input
        )
    }
}

impl std::error::Error for ParseSecurityProfileError {}

/// Result of a file permission check.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FilePermissionCheck {
    /// Path that was checked.
    pub path: String,
    /// Whether the file exists.
    pub exists: bool,
    /// Current permission mode (octal), if readable.
    pub current_mode: Option<u32>,
    /// Maximum allowed mode for this file type.
    pub max_allowed_mode: u32,
    /// Whether the check passed.
    pub passed: bool,
    /// Issue description if check failed.
    pub issue: Option<String>,
    /// Suggested fix if check failed.
    pub repair: Option<String>,
}

impl FilePermissionCheck {
    /// Create a passing check result.
    #[must_use]
    pub fn pass(path: impl Into<String>, mode: u32, max_allowed: u32) -> Self {
        Self {
            path: path.into(),
            exists: true,
            current_mode: Some(mode),
            max_allowed_mode: max_allowed,
            passed: true,
            issue: None,
            repair: None,
        }
    }

    /// Create a failing check result.
    #[must_use]
    pub fn fail(
        path: impl Into<String>,
        mode: u32,
        max_allowed: u32,
        issue: impl Into<String>,
        repair: impl Into<String>,
    ) -> Self {
        Self {
            path: path.into(),
            exists: true,
            current_mode: Some(mode),
            max_allowed_mode: max_allowed,
            passed: false,
            issue: Some(issue.into()),
            repair: Some(repair.into()),
        }
    }

    /// Create a result for a file that doesn't exist.
    #[must_use]
    pub fn not_found(path: impl Into<String>, max_allowed: u32) -> Self {
        Self {
            path: path.into(),
            exists: false,
            current_mode: None,
            max_allowed_mode: max_allowed,
            passed: true,
            issue: None,
            repair: None,
        }
    }
}

/// Summary of all file permission checks for a workspace.
#[derive(Clone, Debug)]
pub struct FilePermissionReport {
    /// Security profile used for checks.
    pub profile: SecurityProfile,
    /// Individual file check results.
    pub checks: Vec<FilePermissionCheck>,
    /// Overall pass/fail verdict.
    pub passed: bool,
    /// Total number of issues found.
    pub issue_count: u32,
}

impl FilePermissionReport {
    /// Create a new report from check results.
    #[must_use]
    pub fn from_checks(profile: SecurityProfile, checks: Vec<FilePermissionCheck>) -> Self {
        let issue_count = checks.iter().filter(|c| !c.passed).count() as u32;
        let passed = issue_count == 0;
        Self {
            profile,
            checks,
            passed,
            issue_count,
        }
    }
}

/// Check file permissions for a workspace against a security profile.
#[must_use]
pub fn check_workspace_permissions(
    workspace: &Path,
    profile: SecurityProfile,
) -> FilePermissionReport {
    let mut checks = Vec::new();

    let db_path = workspace.join(".ee").join("ee.db");
    checks.push(check_file_permissions(
        &db_path,
        profile.max_db_permissions(),
        "database",
    ));

    let config_path = workspace.join(".ee").join("config.toml");
    checks.push(check_file_permissions(
        &config_path,
        profile.max_config_permissions(),
        "config",
    ));

    let index_dir = workspace.join(".ee").join("index");
    if index_dir.exists() {
        checks.push(check_directory_permissions(
            &index_dir,
            profile.max_db_permissions(),
            "index directory",
        ));
    }

    FilePermissionReport::from_checks(profile, checks)
}

fn check_file_permissions(path: &Path, max_mode: u32, file_type: &str) -> FilePermissionCheck {
    if !path.exists() {
        return FilePermissionCheck::not_found(path.display().to_string(), max_mode);
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        match std::fs::metadata(path) {
            Ok(metadata) => {
                let mode = metadata.permissions().mode() & 0o777;
                if mode <= max_mode {
                    FilePermissionCheck::pass(path.display().to_string(), mode, max_mode)
                } else {
                    FilePermissionCheck::fail(
                        path.display().to_string(),
                        mode,
                        max_mode,
                        format!(
                            "{} has mode {:04o}, exceeds max allowed {:04o}",
                            file_type, mode, max_mode
                        ),
                        format!("chmod {:04o} {}", max_mode, path.display()),
                    )
                }
            }
            Err(e) => FilePermissionCheck {
                path: path.display().to_string(),
                exists: true,
                current_mode: None,
                max_allowed_mode: max_mode,
                passed: false,
                issue: Some(format!("failed to read {} metadata: {}", file_type, e)),
                repair: None,
            },
        }
    }

    #[cfg(not(unix))]
    {
        FilePermissionCheck::pass(path.display().to_string(), 0, max_mode)
    }
}

fn check_directory_permissions(path: &Path, max_mode: u32, dir_type: &str) -> FilePermissionCheck {
    if !path.exists() {
        return FilePermissionCheck::not_found(path.display().to_string(), max_mode);
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        match std::fs::metadata(path) {
            Ok(metadata) => {
                let mode = metadata.permissions().mode() & 0o777;
                let max_dir_mode = max_mode | 0o111;
                if mode <= max_dir_mode {
                    FilePermissionCheck::pass(path.display().to_string(), mode, max_dir_mode)
                } else {
                    FilePermissionCheck::fail(
                        path.display().to_string(),
                        mode,
                        max_dir_mode,
                        format!(
                            "{} has mode {:04o}, exceeds max allowed {:04o}",
                            dir_type, mode, max_dir_mode
                        ),
                        format!("chmod {:04o} {}", max_dir_mode, path.display()),
                    )
                }
            }
            Err(e) => FilePermissionCheck {
                path: path.display().to_string(),
                exists: true,
                current_mode: None,
                max_allowed_mode: max_mode | 0o111,
                passed: false,
                issue: Some(format!("failed to read {} metadata: {}", dir_type, e)),
                repair: None,
            },
        }
    }

    #[cfg(not(unix))]
    {
        FilePermissionCheck::pass(path.display().to_string(), 0, max_mode | 0o111)
    }
}

/// Load security profile from environment or use default.
#[must_use]
pub fn load_profile_from_env() -> SecurityProfile {
    std::env::var("EE_SECURITY_PROFILE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::{FilePermissionCheck, FilePermissionReport, SecurityProfile};

    type TestResult = Result<(), String>;

    fn ensure<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, ctx: &str) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    #[test]
    fn security_profile_round_trips_through_str() -> TestResult {
        for profile in SecurityProfile::all() {
            let s = profile.as_str();
            let parsed = SecurityProfile::from_str(s).map_err(|e| e.to_string())?;
            ensure(parsed, profile, &format!("round-trip {s}"))?;
        }
        Ok(())
    }

    #[test]
    fn security_profile_accepts_case_insensitive() -> TestResult {
        for (input, expected) in [
            ("DEFAULT", SecurityProfile::Default),
            ("Strict", SecurityProfile::Strict),
            ("PERMISSIVE", SecurityProfile::Permissive),
        ] {
            let parsed = SecurityProfile::from_str(input).map_err(|e| e.to_string())?;
            ensure(parsed, expected, input)?;
        }
        Ok(())
    }

    #[test]
    fn security_profile_rejects_unknown() {
        assert!(SecurityProfile::from_str("unknown").is_err());
        assert!(SecurityProfile::from_str("").is_err());
    }

    #[test]
    fn security_profile_default_is_default() -> TestResult {
        ensure(
            SecurityProfile::default(),
            SecurityProfile::Default,
            "default",
        )
    }

    #[test]
    fn strict_profile_has_lower_ceilings() -> TestResult {
        let default = SecurityProfile::Default;
        let strict = SecurityProfile::Strict;

        ensure(
            default.trust_ceiling() > strict.trust_ceiling(),
            true,
            "ceiling",
        )?;
        ensure(default.trust_floor() > strict.trust_floor(), true, "floor")
    }

    #[test]
    fn permissive_profile_disables_enforcement() -> TestResult {
        let permissive = SecurityProfile::Permissive;

        ensure(permissive.enforce_redaction(), false, "redaction")?;
        ensure(permissive.enforce_file_permissions(), false, "file perms")
    }

    #[test]
    fn strict_profile_blocks_untrusted_imports() -> TestResult {
        ensure(
            SecurityProfile::Strict.allow_untrusted_imports(),
            false,
            "strict",
        )
    }

    #[test]
    fn file_permission_check_pass_records_mode() {
        let check = FilePermissionCheck::pass("/test/db", 0o600, 0o600);
        assert!(check.passed);
        assert!(check.exists);
        assert_eq!(check.current_mode, Some(0o600));
        assert!(check.issue.is_none());
    }

    #[test]
    fn file_permission_check_fail_records_issue() {
        let check =
            FilePermissionCheck::fail("/test/db", 0o644, 0o600, "too permissive", "chmod 600");
        assert!(!check.passed);
        assert!(check.issue.is_some());
        assert!(check.repair.is_some());
    }

    #[test]
    fn file_permission_check_not_found_passes() {
        let check = FilePermissionCheck::not_found("/nonexistent", 0o600);
        assert!(check.passed);
        assert!(!check.exists);
    }

    #[test]
    fn file_permission_report_counts_issues() {
        let checks = vec![
            FilePermissionCheck::pass("/a", 0o600, 0o600),
            FilePermissionCheck::fail("/b", 0o644, 0o600, "bad", "fix"),
            FilePermissionCheck::pass("/c", 0o600, 0o600),
        ];
        let report = FilePermissionReport::from_checks(SecurityProfile::Default, checks);

        assert!(!report.passed);
        assert_eq!(report.issue_count, 1);
        assert_eq!(report.checks.len(), 3);
    }

    #[test]
    fn file_permission_report_passes_when_no_issues() {
        let checks = vec![
            FilePermissionCheck::pass("/a", 0o600, 0o600),
            FilePermissionCheck::not_found("/b", 0o600),
        ];
        let report = FilePermissionReport::from_checks(SecurityProfile::Strict, checks);

        assert!(report.passed);
        assert_eq!(report.issue_count, 0);
    }
}
