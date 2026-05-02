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
        regex_lite::Regex::new(pattern).ok().map(|re| Self {
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
                if output.contains(needle) {
                    detections.push(LeakDetection {
                        class: self.class,
                        pattern_name: self.name,
                        matched_text: needle.to_string(),
                        context: extract_context(output, needle),
                    });
                }
            }
            PatternKind::Prefix(prefix) => {
                for word in output.split_whitespace() {
                    if let Some(token) = prefixed_token(word, prefix) {
                        detections.push(LeakDetection {
                            class: self.class,
                            pattern_name: self.name,
                            matched_text: token.to_string(),
                            context: extract_context(output, token),
                        });
                    }
                }
            }
            PatternKind::Suffix(suffix) => {
                for word in output.split_whitespace() {
                    if word.ends_with(suffix) && word.len() > suffix.len() {
                        detections.push(LeakDetection {
                            class: self.class,
                            pattern_name: self.name,
                            matched_text: word.to_string(),
                            context: extract_context(output, word),
                        });
                    }
                }
            }
            PatternKind::Regex(re) => {
                for mat in re.find_iter(output) {
                    detections.push(LeakDetection {
                        class: self.class,
                        pattern_name: self.name,
                        matched_text: mat.as_str().to_string(),
                        context: extract_context(output, mat.as_str()),
                    });
                }
            }
        }

        detections
    }
}

fn prefixed_token<'a>(word: &'a str, prefix: &str) -> Option<&'a str> {
    for (index, _) in word.match_indices(prefix) {
        let (before_prefix, prefixed_fragment) = word.split_at(index);
        let prefix_is_token_start = before_prefix.chars().last().is_none_or(is_token_delimiter);
        if prefix_is_token_start {
            let candidate = trim_token_delimiters(prefixed_fragment);
            if candidate.starts_with(prefix) && candidate.len() > prefix.len() {
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
fn extract_context(output: &str, matched: &str) -> String {
    const CONTEXT_BYTES: usize = 30;

    if let Some(pos) = output.find(matched) {
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
    } else {
        matched.to_string()
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
    fn detector_clean_output_passes() -> TestResult {
        let detector = RedactionLeakDetector::new();
        let output =
            r#"{"schema": "ee.response.v1", "success": true, "data": {"command": "status"}}"#;

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
        let context = extract_context(&long_output, "SECRET");

        ensure(context.starts_with("..."), true, "starts with ellipsis")?;
        ensure(context.ends_with("..."), true, "ends with ellipsis")?;
        ensure(context.contains("SECRET"), true, "contains match")
    }
}
