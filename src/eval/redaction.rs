//! Redaction leak evaluation (EE-254).
//!
//! Detects potential sensitive data leaks in command output by checking
//! against configurable patterns for secrets, PII, internal paths, and
//! other sensitive content classes.

use super::RedactionClass;

/// Pattern-based redaction leak detector.
#[derive(Clone, Debug)]
pub struct RedactionLeakDetector {
    patterns: Vec<LeakPattern>,
}

impl Default for RedactionLeakDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl RedactionLeakDetector {
    /// Create a detector with default patterns for common sensitive data.
    #[must_use]
    pub fn new() -> Self {
        Self {
            patterns: default_leak_patterns(),
        }
    }

    /// Create an empty detector (no patterns).
    #[must_use]
    pub fn empty() -> Self {
        Self {
            patterns: Vec::new(),
        }
    }

    /// Add a custom leak pattern.
    #[must_use]
    pub fn with_pattern(mut self, pattern: LeakPattern) -> Self {
        self.patterns.push(pattern);
        self
    }

    /// Check output for potential leaks across all configured classes.
    #[must_use]
    pub fn detect_leaks(&self, output: &str) -> Vec<LeakDetection> {
        let mut detections = Vec::new();

        for pattern in &self.patterns {
            for detection in pattern.detect(output) {
                detections.push(detection);
            }
        }

        detections
    }

    /// Check output for leaks in specific redaction classes only.
    #[must_use]
    pub fn detect_leaks_in_classes(
        &self,
        output: &str,
        classes: &[RedactionClass],
    ) -> Vec<LeakDetection> {
        self.detect_leaks(output)
            .into_iter()
            .filter(|d| classes.contains(&d.class))
            .collect()
    }

    /// Returns true if no leaks detected in the given output.
    #[must_use]
    pub fn is_clean(&self, output: &str) -> bool {
        self.detect_leaks(output).is_empty()
    }

    /// Returns true if output is clean for specific classes only.
    #[must_use]
    pub fn is_clean_for_classes(&self, output: &str, classes: &[RedactionClass]) -> bool {
        self.detect_leaks_in_classes(output, classes).is_empty()
    }
}

/// A pattern for detecting a specific type of sensitive data leak.
#[derive(Clone, Debug)]
pub struct LeakPattern {
    pub class: RedactionClass,
    pub name: &'static str,
    pub description: &'static str,
    kind: PatternKind,
}

#[derive(Clone, Debug)]
enum PatternKind {
    Contains(&'static str),
    Prefix(&'static str),
    Suffix(&'static str),
    Regex(regex_lite::Regex),
}

impl LeakPattern {
    /// Create a pattern that matches if output contains the given substring.
    #[must_use]
    pub fn contains(
        class: RedactionClass,
        name: &'static str,
        description: &'static str,
        needle: &'static str,
    ) -> Self {
        Self {
            class,
            name,
            description,
            kind: PatternKind::Contains(needle),
        }
    }

    /// Create a pattern that matches if any word starts with the given prefix.
    #[must_use]
    pub fn prefix(
        class: RedactionClass,
        name: &'static str,
        description: &'static str,
        prefix: &'static str,
    ) -> Self {
        Self {
            class,
            name,
            description,
            kind: PatternKind::Prefix(prefix),
        }
    }

    /// Create a pattern that matches if any word ends with the given suffix.
    #[must_use]
    pub fn suffix(
        class: RedactionClass,
        name: &'static str,
        description: &'static str,
        suffix: &'static str,
    ) -> Self {
        Self {
            class,
            name,
            description,
            kind: PatternKind::Suffix(suffix),
        }
    }

    /// Create a pattern using a regex.
    #[must_use]
    pub fn regex(
        class: RedactionClass,
        name: &'static str,
        description: &'static str,
        pattern: &str,
    ) -> Option<Self> {
        // bd-3j60j: the leak detector is a privacy ORACLE (deny-list), so match
        // case-insensitively — a non-canonical casing of a field/secret marker
        // (e.g. aws_secret_access_key vs AWS_SECRET_ACCESS_KEY) must not slip past.
        let pattern = if pattern.starts_with("(?i)") {
            pattern.to_string()
        } else {
            format!("(?i){pattern}")
        };
        regex_lite::Regex::new(&pattern).ok().map(|re| Self {
            class,
            name,
            description,
            kind: PatternKind::Regex(re),
        })
    }

    /// Detect leaks matching this pattern in the output.
    fn detect(&self, output: &str) -> Vec<LeakDetection> {
        let mut detections = Vec::new();

        match &self.kind {
            PatternKind::Contains(needle) => {
                // bd-3j60j: case-insensitive literal match. to_ascii_lowercase
                // preserves byte length, so positions in the lowered copy align
                // with the original output; slice the original for the real text.
                let lowered_output = output.to_ascii_lowercase();
                let lowered_needle = needle.to_ascii_lowercase();
                for (pos, _) in lowered_output.match_indices(&lowered_needle) {
                    let matched = &output[pos..pos + needle.len()];
                    detections.push(LeakDetection {
                        class: self.class,
                        pattern_name: self.name,
                        matched_text: matched.to_string(),
                        context: extract_context(output, matched, pos),
                    });
                }
            }
            PatternKind::Prefix(prefix) => {
                let mut seen = std::collections::HashSet::new();
                for word in output.split_whitespace() {
                    if let Some(token) = prefixed_token(word, prefix) {
                        if seen.insert(token) {
                            for (pos, _) in output.match_indices(token) {
                                detections.push(LeakDetection {
                                    class: self.class,
                                    pattern_name: self.name,
                                    matched_text: token.to_string(),
                                    context: extract_context(output, token, pos),
                                });
                            }
                        }
                    }
                }
            }
            PatternKind::Suffix(suffix) => {
                // bd-3j60j: case-insensitive, and trim surrounding delimiters
                // (matching the prefix branch) so a quoted/punctuated value such
                // as "...secret.env" still matches a registered suffix pattern.
                let lowered_suffix = suffix.to_ascii_lowercase();
                let mut seen = std::collections::HashSet::new();
                for word in output.split_whitespace() {
                    let candidate = trim_token_delimiters(word);
                    if candidate.len() > suffix.len()
                        && candidate.to_ascii_lowercase().ends_with(&lowered_suffix)
                        && seen.insert(candidate)
                    {
                        for (pos, _) in output.match_indices(candidate) {
                            detections.push(LeakDetection {
                                class: self.class,
                                pattern_name: self.name,
                                matched_text: candidate.to_string(),
                                context: extract_context(output, candidate, pos),
                            });
                        }
                    }
                }
            }
            PatternKind::Regex(re) => {
                for mat in re.find_iter(output) {
                    detections.push(LeakDetection {
                        class: self.class,
                        pattern_name: self.name,
                        matched_text: mat.as_str().to_string(),
                        context: extract_context(output, mat.as_str(), mat.start()),
                    });
                }
            }
        }

        detections
    }
}

fn prefixed_token<'a>(word: &'a str, prefix: &str) -> Option<&'a str> {
    // bd-3j60j: match the prefix case-insensitively, and register it even when
    // glued to preceding text in two safe cases: (a) the prefix self-bounds
    // because it begins with a delimiter (e.g. "/Users/" in "opening/Users/..."),
    // or (b) the char immediately before it is a start delimiter (now including
    // '-'/'_', so "bearer-sk-..." registers). A prefix that begins mid-word right
    // after a plain alphanumeric (e.g. "sk-" inside "task-management") is still
    // rejected to avoid false positives.
    let lowered_word = word.to_ascii_lowercase();
    let lowered_prefix = prefix.to_ascii_lowercase();
    let prefix_self_bounds = prefix.chars().next().is_some_and(is_prefix_start_delimiter);
    for (index, _) in lowered_word.match_indices(&lowered_prefix) {
        let prefix_is_token_start = prefix_self_bounds
            || word[..index]
                .chars()
                .last()
                .is_none_or(is_prefix_start_delimiter);
        if prefix_is_token_start {
            let candidate = trim_token_delimiters(&word[index..]);
            if candidate.len() > prefix.len()
                && candidate.to_ascii_lowercase().starts_with(&lowered_prefix)
            {
                return Some(candidate);
            }
        }
    }
    None
}

fn trim_token_delimiters(fragment: &str) -> &str {
    fragment.trim_matches(is_token_delimiter)
}

fn is_token_delimiter(ch: char) -> bool {
    matches!(
        ch,
        '"' | '\'' | '`' | '{' | '}' | '[' | ']' | '(' | ')' | '<' | '>' | ',' | ':' | ';' | '='
    )
}

fn is_prefix_start_delimiter(ch: char) -> bool {
    // bd-3j60j: '-' and '_' are common secret-glue separators ("bearer-sk-...",
    // "x_ghp_..."), so treat them as token starts for prefix detection.
    is_token_delimiter(ch) || matches!(ch, '/' | '\\' | '?' | '&' | '#' | '-' | '_')
}

/// A detected potential leak.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeakDetection {
    pub class: RedactionClass,
    pub pattern_name: &'static str,
    pub matched_text: String,
    pub context: String,
}

impl LeakDetection {
    /// Format for human-readable display.
    #[must_use]
    pub fn display(&self) -> String {
        format!(
            "[{}] {}: \"{}\" in context \"{}\"",
            self.class.as_str(),
            self.pattern_name,
            self.matched_text,
            self.context
        )
    }
}

/// Result of running redaction leak evaluation on a scenario.
#[derive(Clone, Debug)]
pub struct RedactionLeakEvaluation {
    pub scenario_id: String,
    pub passed: bool,
    pub total_checks: usize,
    pub leaks_detected: Vec<LeakDetection>,
}

impl RedactionLeakEvaluation {
    /// Create a passing evaluation result.
    #[must_use]
    pub fn pass(scenario_id: impl Into<String>, total_checks: usize) -> Self {
        Self {
            scenario_id: scenario_id.into(),
            passed: true,
            total_checks,
            leaks_detected: Vec::new(),
        }
    }

    /// Create a failing evaluation result.
    #[must_use]
    pub fn fail(
        scenario_id: impl Into<String>,
        total_checks: usize,
        leaks: Vec<LeakDetection>,
    ) -> Self {
        Self {
            scenario_id: scenario_id.into(),
            passed: false,
            total_checks,
            leaks_detected: leaks,
        }
    }
}

/// Extract surrounding context for a matched substring.
///
/// Rounds the start/end byte positions onto UTF-8 character boundaries
/// before slicing so this function never panics when `output` contains
/// multi-byte characters (e.g. non-ASCII text or emoji in user-supplied
/// command output).
fn extract_context(output: &str, matched: &str, pos: usize) -> String {
    const CONTEXT_BYTES: usize = 30;

    let raw_start = pos.saturating_sub(CONTEXT_BYTES);
    let raw_end = (pos + matched.len() + CONTEXT_BYTES).min(output.len());
    let start = floor_char_boundary(output, raw_start);
    let end = ceil_char_boundary(output, raw_end);
    let context = &output[start..end];
    if start > 0 || end < output.len() {
        format!("...{}...", context.replace('\n', " "))
    } else {
        context.replace('\n', " ")
    }
}

/// Largest byte index `<= idx` that is a valid UTF-8 char boundary.
fn floor_char_boundary(s: &str, idx: usize) -> usize {
    let idx = idx.min(s.len());
    let mut boundary = idx;
    while boundary > 0 && !s.is_char_boundary(boundary) {
        boundary -= 1;
    }
    boundary
}

/// Smallest byte index `>= idx` that is a valid UTF-8 char boundary.
fn ceil_char_boundary(s: &str, idx: usize) -> usize {
    let mut boundary = idx.min(s.len());
    while boundary < s.len() && !s.is_char_boundary(boundary) {
        boundary += 1;
    }
    boundary
}

/// Default patterns for common sensitive data types.
fn default_leak_patterns() -> Vec<LeakPattern> {
    let mut patterns = vec![
        // Secret patterns
        LeakPattern::prefix(
            RedactionClass::Secret,
            "api_key_prefix",
            "API key with common prefix",
            "sk-",
        ),
        LeakPattern::prefix(
            RedactionClass::Secret,
            "api_key_prefix",
            "API key with common prefix",
            "sk_",
        ),
        LeakPattern::prefix(
            RedactionClass::Secret,
            "anthropic_key",
            "Anthropic API key prefix",
            "sk-ant-",
        ),
        LeakPattern::prefix(
            RedactionClass::Secret,
            "openai_key",
            "OpenAI API key prefix",
            "sk-proj-",
        ),
        LeakPattern::contains(
            RedactionClass::Secret,
            "password_field",
            "Password field in JSON",
            "\"password\":",
        ),
        LeakPattern::contains(
            RedactionClass::Secret,
            "secret_field",
            "Secret field in JSON",
            "\"secret\":",
        ),
        LeakPattern::contains(
            RedactionClass::Secret,
            "token_field",
            "Token field in JSON",
            "\"token\":",
        ),
        LeakPattern::contains(
            RedactionClass::Secret,
            "api_key_field",
            "API key field in JSON",
            "\"api_key\":",
        ),
        LeakPattern::contains(
            RedactionClass::Secret,
            "apikey_field",
            "API key field in JSON (alt)",
            "\"apiKey\":",
        ),
    ];

    for (name, description, pattern) in [
        (
            "password_field_spaced",
            "Password field in pretty JSON",
            r#""password"\s+:"#,
        ),
        (
            "secret_field_spaced",
            "Secret field in pretty JSON",
            r#""secret"\s+:"#,
        ),
        (
            "token_field_spaced",
            "Token field in pretty JSON",
            r#""token"\s+:"#,
        ),
        (
            "api_key_field_spaced",
            "API key field in pretty JSON",
            r#""api_key"\s+:"#,
        ),
        (
            "apikey_field_spaced",
            "API key field in pretty JSON (alt)",
            r#""apiKey"\s+:"#,
        ),
    ] {
        if let Some(pattern) =
            LeakPattern::regex(RedactionClass::Secret, name, description, pattern)
        {
            patterns.push(pattern);
        }
    }

    if let Some(p) = LeakPattern::regex(
        RedactionClass::Secret,
        "aws_secret_access_key",
        "AWS secret access key environment value",
        r"\bAWS_SECRET_ACCESS_KEY\s*[:=]\s*[A-Za-z0-9/+=]{20,}",
    ) {
        patterns.push(p);
    }
    if let Some(p) = LeakPattern::regex(
        RedactionClass::Secret,
        "jwt_token",
        "JWT token with base64url header, claims, and signature",
        r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\b",
    ) {
        patterns.push(p);
    }
    if let Some(p) = LeakPattern::regex(
        RedactionClass::Secret,
        "pem_private_key",
        "PEM private key header",
        r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----",
    ) {
        patterns.push(p);
    }

    // PII patterns
    if let Some(p) = LeakPattern::regex(
        RedactionClass::Pii,
        "email_address",
        "Email address pattern",
        r"[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}",
    ) {
        patterns.push(p);
    }
    if let Some(p) = LeakPattern::regex(
        RedactionClass::Pii,
        "phone_number",
        "Phone number pattern",
        r"\b\d{3}[-.]?\d{3}[-.]?\d{4}\b",
    ) {
        patterns.push(p);
    }
    if let Some(p) = LeakPattern::regex(
        RedactionClass::Pii,
        "ssn",
        "Social security number pattern",
        r"\b\d{3}-\d{2}-\d{4}\b",
    ) {
        patterns.push(p);
    }

    patterns.extend([
        // Internal path patterns
        LeakPattern::prefix(
            RedactionClass::InternalPath,
            "home_path",
            "User home directory path",
            "/home/",
        ),
        LeakPattern::prefix(
            RedactionClass::InternalPath,
            "users_path",
            "macOS user directory path",
            "/Users/",
        ),
        LeakPattern::contains(
            RedactionClass::InternalPath,
            "dotenv_file",
            "Environment file reference",
            ".env",
        ),
        LeakPattern::contains(
            RedactionClass::InternalPath,
            "ssh_key_path",
            "SSH key directory",
            ".ssh/",
        ),
        LeakPattern::contains(
            RedactionClass::InternalPath,
            "credentials_file",
            "Credentials file reference",
            "credentials",
        ),
    ]);

    patterns
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    fn ensure<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, ctx: &str) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    #[test]
    fn detector_detects_api_key_prefix() -> TestResult {
        let detector = RedactionLeakDetector::new();
        let output = r#"{"key": "sk-abc123"}"#;

        let leaks = detector.detect_leaks(output);
        ensure(!leaks.is_empty(), true, "should detect api key prefix")?;
        ensure(
            leaks.iter().any(|l| l.class == RedactionClass::Secret),
            true,
            "should be secret class",
        )
    }

    #[test]
    fn detector_catches_case_insensitive_and_glued_prefixes_bd_3j60j() -> TestResult {
        let detector = RedactionLeakDetector::new();

        // Class 1 (case-insensitive): non-canonical casings of field/secret
        // markers must still be flagged by the privacy oracle.
        for (label, output) in [
            ("PascalCase password field", r#"{"Password": "hunter2"}"#),
            ("uppercase sk- prefix", "token SK-ABCDEFGHIJ123456"),
            (
                "lowercase aws secret key",
                "aws_secret_access_key=abcdefghij0123456789ABCD",
            ),
        ] {
            ensure(
                !detector.detect_leaks(output).is_empty(),
                true,
                &format!("case-insensitive leak must be detected: {label}"),
            )?;
        }

        // Class 2 (glued prefix): a secret/path prefix concatenated to preceding
        // text must register — both a self-bounding path prefix and a '-'-glued key.
        ensure(
            detector
                .detect_leaks("opening/Users/jeff/.ssh/id_rsa")
                .iter()
                .any(|l| l.pattern_name == "users_path"),
            true,
            "glued /Users/ path must be detected",
        )?;
        ensure(
            !detector.detect_leaks("bearer-sk-secretvalue123").is_empty(),
            true,
            "glued sk- key prefix must be detected",
        )?;

        // Guard against over-broad matching: a benign hyphenated word that merely
        // contains a short prefix substring mid-token must NOT be flagged.
        ensure(
            detector
                .detect_leaks("the task-management board")
                .iter()
                .all(|l| l.pattern_name != "api_key_prefix"),
            true,
            "benign 'task-management' must not be a key leak",
        )
    }

    #[test]
    fn detector_detects_anthropic_key() -> TestResult {
        let detector = RedactionLeakDetector::new();
        let output = "API key: sk-ant-api03-xyz123";

        let leaks = detector.detect_leaks(output);
        ensure(!leaks.is_empty(), true, "should detect anthropic key")?;
        ensure(
            leaks.iter().any(|l| l.pattern_name == "anthropic_key"),
            true,
            "pattern name",
        )
    }

    #[test]
    fn detector_detects_email() -> TestResult {
        let detector = RedactionLeakDetector::new();
        let output = r#"{"email": "test@example.com"}"#;

        let leaks = detector.detect_leaks(output);
        ensure(!leaks.is_empty(), true, "should detect email")?;
        ensure(
            leaks.iter().any(|l| l.class == RedactionClass::Pii),
            true,
            "should be pii class",
        )
    }

    #[test]
    fn detector_detects_phone_number() -> TestResult {
        let detector = RedactionLeakDetector::new();
        let output = "Contact: 555-123-4567";

        let leaks = detector.detect_leaks(output);
        ensure(!leaks.is_empty(), true, "should detect phone number")?;
        ensure(leaks[0].pattern_name, "phone_number", "pattern name")
    }

    #[test]
    fn detector_detects_ssn() -> TestResult {
        let detector = RedactionLeakDetector::new();
        let output = "SSN: 123-45-6789";

        let leaks = detector.detect_leaks(output);
        ensure(!leaks.is_empty(), true, "should detect ssn")?;
        ensure(leaks[0].pattern_name, "ssn", "pattern name")
    }

    #[test]
    fn detector_detects_home_path() -> TestResult {
        let detector = RedactionLeakDetector::new();
        let output = r#"{"path": "/home/ubuntu/.config"}"#;

        let leaks = detector.detect_leaks(output);
        ensure(!leaks.is_empty(), true, "should detect home path")?;
        ensure(
            leaks
                .iter()
                .any(|l| l.class == RedactionClass::InternalPath),
            true,
            "should be internal_path class",
        )
    }

    #[test]
    fn detector_detects_uri_wrapped_internal_path() -> TestResult {
        let detector = RedactionLeakDetector::new();
        let output = r#"{"uri": "file:///Users/alice/private/project.log"}"#;

        let leaks = detector.detect_leaks(output);
        ensure(
            leaks.iter().any(|leak| {
                leak.class == RedactionClass::InternalPath && leak.pattern_name == "users_path"
            }),
            true,
            "file URI should still expose a macOS user path leak",
        )
    }

    #[test]
    fn detector_detects_url_path_secret_prefix() -> TestResult {
        let detector = RedactionLeakDetector::new();
        let output = "GET https://example.invalid/v1/sk-proj-redaction-fixture";

        let leaks = detector.detect_leaks(output);
        ensure(
            leaks.iter().any(|leak| {
                leak.class == RedactionClass::Secret && leak.pattern_name == "openai_key"
            }),
            true,
            "URL path segment should still expose an API key prefix leak",
        )
    }

    #[test]
    fn detector_detects_password_field() -> TestResult {
        let detector = RedactionLeakDetector::new();
        let output = r#"{"username": "admin", "password": "secret123"}"#;

        let leaks = detector.detect_leaks(output);
        ensure(!leaks.is_empty(), true, "should detect password field")?;
        ensure(
            leaks.iter().any(|l| l.pattern_name == "password_field"),
            true,
            "should match password_field pattern",
        )
    }

    #[test]
    fn detector_detects_pretty_json_secret_fields() -> TestResult {
        let detector = RedactionLeakDetector::new();
        let output = r#"{
            "password" : "secret123",
            "secret" : "value",
            "token" : "value",
            "api_key" : "value",
            "apiKey" : "value"
        }"#;

        let leaks = detector.detect_leaks(output);
        for pattern in [
            "password_field_spaced",
            "secret_field_spaced",
            "token_field_spaced",
            "api_key_field_spaced",
            "apikey_field_spaced",
        ] {
            ensure(
                leaks.iter().any(|leak| leak.pattern_name == pattern),
                true,
                pattern,
            )?;
        }
        ensure(
            leaks
                .iter()
                .filter(|leak| leak.pattern_name.ends_with("_field_spaced"))
                .all(|leak| leak.class == RedactionClass::Secret),
            true,
            "pretty-json field detections should all be secret class",
        )
    }

    #[test]
    fn detector_detects_cloud_jwt_and_pem_secret_leaks() -> TestResult {
        let detector = RedactionLeakDetector::new();
        let output = [
            "AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
            "Authorization: Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c",
            "-----BEGIN RSA PRIVATE KEY-----",
        ]
        .join("\n");

        let leaks = detector.detect_leaks(&output);
        for pattern in ["aws_secret_access_key", "jwt_token", "pem_private_key"] {
            ensure(
                leaks.iter().any(|leak| leak.pattern_name == pattern),
                true,
                pattern,
            )?;
        }
        ensure(
            leaks
                .iter()
                .all(|leak| leak.class == RedactionClass::Secret),
            true,
            "resolved detector gaps should all be secret class",
        )
    }

    #[test]
    fn detector_clean_output_passes() -> TestResult {
        let detector = RedactionLeakDetector::new();
        let output =
            r#"{"schema": "ee.response.v2", "success": true, "data": {"command": "status"}}"#;

        ensure(detector.is_clean(output), true, "clean output should pass")
    }

    #[test]
    fn detector_class_filter_works() -> TestResult {
        let detector = RedactionLeakDetector::new();
        let output = r#"sk-abc123 and test@example.com"#;

        let secret_only = detector.detect_leaks_in_classes(output, &[RedactionClass::Secret]);
        let pii_only = detector.detect_leaks_in_classes(output, &[RedactionClass::Pii]);

        ensure(
            secret_only
                .iter()
                .all(|l| l.class == RedactionClass::Secret),
            true,
            "secret filter",
        )?;
        ensure(
            pii_only.iter().all(|l| l.class == RedactionClass::Pii),
            true,
            "pii filter",
        )
    }

    #[test]
    fn custom_pattern_works() -> TestResult {
        let detector = RedactionLeakDetector::empty().with_pattern(LeakPattern::contains(
            RedactionClass::Custom,
            "custom_secret",
            "Custom secret marker",
            "CUSTOM_SECRET_MARKER",
        ));

        let output = "data: CUSTOM_SECRET_MARKER here";
        let leaks = detector.detect_leaks(output);

        ensure(!leaks.is_empty(), true, "should detect custom pattern")?;
        ensure(leaks[0].class, RedactionClass::Custom, "custom class")
    }

    #[test]
    fn evaluation_result_pass_is_correct() -> TestResult {
        let result = RedactionLeakEvaluation::pass("test_scenario", 5);
        ensure(result.passed, true, "passed")?;
        ensure(result.leaks_detected.is_empty(), true, "no leaks")
    }

    #[test]
    fn evaluation_result_fail_is_correct() -> TestResult {
        let leak = LeakDetection {
            class: RedactionClass::Secret,
            pattern_name: "test",
            matched_text: "sk-test".to_string(),
            context: "context".to_string(),
        };
        let result = RedactionLeakEvaluation::fail("test_scenario", 5, vec![leak]);

        ensure(result.passed, false, "not passed")?;
        ensure(result.leaks_detected.len(), 1, "one leak")
    }

    #[test]
    fn leak_detection_display_is_readable() -> TestResult {
        let detection = LeakDetection {
            class: RedactionClass::Secret,
            pattern_name: "api_key_prefix",
            matched_text: "sk-abc".to_string(),
            context: "key is sk-abc here".to_string(),
        };

        let display = detection.display();
        ensure(display.contains("[secret]"), true, "contains class")?;
        ensure(display.contains("api_key_prefix"), true, "contains pattern")?;
        ensure(display.contains("sk-abc"), true, "contains matched")
    }

    #[test]
    fn context_extraction_adds_ellipsis() -> TestResult {
        let long_output = "a".repeat(100) + "SECRET" + &"b".repeat(100);
        let pos = long_output
            .find("SECRET")
            .ok_or_else(|| "fixture must contain SECRET".to_string())?;
        let context = extract_context(&long_output, "SECRET", pos);

        ensure(context.starts_with("..."), true, "starts with ellipsis")?;
        ensure(context.ends_with("..."), true, "ends with ellipsis")?;
        ensure(context.contains("SECRET"), true, "contains match")
    }
}
