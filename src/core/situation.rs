//! Situation analysis for task classification and explanation (EE-421).
//!
//! Provides commands for:
//! - Classifying task text into situation categories
//! - Showing situation details
//! - Explaining situation context and recommendations

use std::fmt;
use std::str::FromStr;

use super::build_info;

// ============================================================================
// Schema Constants
// ============================================================================

/// Schema for situation classification results.
pub const SITUATION_CLASSIFY_SCHEMA_V1: &str = "ee.situation.classify.v1";

/// Schema for situation details.
pub const SITUATION_SHOW_SCHEMA_V1: &str = "ee.situation.show.v1";

/// Schema for situation explanations.
pub const SITUATION_EXPLAIN_SCHEMA_V1: &str = "ee.situation.explain.v1";

// ============================================================================
// Situation Category
// ============================================================================

/// Category of task situation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SituationCategory {
    BugFix,
    Feature,
    Refactor,
    Investigation,
    Documentation,
    Testing,
    Configuration,
    Deployment,
    Review,
    Unknown,
}

impl SituationCategory {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BugFix => "bug_fix",
            Self::Feature => "feature",
            Self::Refactor => "refactor",
            Self::Investigation => "investigation",
            Self::Documentation => "documentation",
            Self::Testing => "testing",
            Self::Configuration => "configuration",
            Self::Deployment => "deployment",
            Self::Review => "review",
            Self::Unknown => "unknown",
        }
    }

    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::BugFix => "Fixing a bug or defect in existing functionality",
            Self::Feature => "Adding new functionality or capability",
            Self::Refactor => "Restructuring code without changing behavior",
            Self::Investigation => "Exploring or debugging to understand a problem",
            Self::Documentation => "Writing or updating documentation",
            Self::Testing => "Adding or modifying tests",
            Self::Configuration => "Changing configuration or settings",
            Self::Deployment => "Deploying or releasing changes",
            Self::Review => "Reviewing code or design",
            Self::Unknown => "Situation category could not be determined",
        }
    }

    /// All known categories for enumeration.
    pub const ALL: &'static [Self] = &[
        Self::BugFix,
        Self::Feature,
        Self::Refactor,
        Self::Investigation,
        Self::Documentation,
        Self::Testing,
        Self::Configuration,
        Self::Deployment,
        Self::Review,
        Self::Unknown,
    ];
}

impl fmt::Display for SituationCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseSituationCategoryError(String);

impl fmt::Display for ParseSituationCategoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid situation category: {}", self.0)
    }
}

impl std::error::Error for ParseSituationCategoryError {}

impl FromStr for SituationCategory {
    type Err = ParseSituationCategoryError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().replace('-', "_").as_str() {
            "bug_fix" | "bugfix" | "fix" => Ok(Self::BugFix),
            "feature" | "feat" => Ok(Self::Feature),
            "refactor" | "refactoring" => Ok(Self::Refactor),
            "investigation" | "investigate" | "debug" => Ok(Self::Investigation),
            "documentation" | "docs" | "doc" => Ok(Self::Documentation),
            "testing" | "test" | "tests" => Ok(Self::Testing),
            "configuration" | "config" | "cfg" => Ok(Self::Configuration),
            "deployment" | "deploy" | "release" => Ok(Self::Deployment),
            "review" | "code_review" => Ok(Self::Review),
            "unknown" => Ok(Self::Unknown),
            _ => Err(ParseSituationCategoryError(s.to_string())),
        }
    }
}

// ============================================================================
// Situation Confidence
// ============================================================================

/// Confidence level for classification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfidenceLevel {
    High,
    Medium,
    Low,
}

impl ConfidenceLevel {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
        }
    }

    #[must_use]
    pub const fn threshold(self) -> f32 {
        match self {
            Self::High => 0.8,
            Self::Medium => 0.5,
            Self::Low => 0.0,
        }
    }
}

impl fmt::Display for ConfidenceLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ============================================================================
// Classification Result
// ============================================================================

/// Result of classifying task text.
#[derive(Clone, Debug, PartialEq)]
pub struct ClassifyResult {
    pub version: &'static str,
    pub input_text: String,
    pub category: SituationCategory,
    pub confidence: ConfidenceLevel,
    pub confidence_score: f32,
    pub signals: Vec<ClassificationSignal>,
    pub alternative_categories: Vec<(SituationCategory, f32)>,
}

/// A signal that contributed to classification.
#[derive(Clone, Debug, PartialEq)]
pub struct ClassificationSignal {
    pub signal_type: &'static str,
    pub pattern: String,
    pub weight: f32,
}

impl ClassifyResult {
    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut output = format!("Situation: {}\n", self.category.as_str());
        output.push_str(&format!(
            "Confidence: {} ({:.0}%)\n",
            self.confidence,
            self.confidence_score * 100.0
        ));
        output.push_str(&format!("Description: {}\n", self.category.description()));

        if !self.signals.is_empty() {
            output.push_str("\nSignals:\n");
            for signal in &self.signals {
                output.push_str(&format!(
                    "  - {}: \"{}\" (weight: {:.2})\n",
                    signal.signal_type, signal.pattern, signal.weight
                ));
            }
        }

        if !self.alternative_categories.is_empty() {
            output.push_str("\nAlternative categories:\n");
            for (cat, score) in &self.alternative_categories {
                output.push_str(&format!("  - {}: {:.0}%\n", cat.as_str(), score * 100.0));
            }
        }

        output
    }

    #[must_use]
    pub fn toon_output(&self) -> String {
        format!(
            "CLASSIFY|{}|{}|{:.2}",
            self.category.as_str(),
            self.confidence.as_str(),
            self.confidence_score
        )
    }

    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        let signals: Vec<serde_json::Value> = self
            .signals
            .iter()
            .map(|s| {
                serde_json::json!({
                    "signalType": s.signal_type,
                    "pattern": s.pattern,
                    "weight": s.weight,
                })
            })
            .collect();

        let alternatives: Vec<serde_json::Value> = self
            .alternative_categories
            .iter()
            .map(|(cat, score)| {
                serde_json::json!({
                    "category": cat.as_str(),
                    "score": score,
                })
            })
            .collect();

        serde_json::json!({
            "command": "situation classify",
            "version": self.version,
            "inputText": self.input_text,
            "category": self.category.as_str(),
            "categoryDescription": self.category.description(),
            "confidence": self.confidence.as_str(),
            "confidenceScore": self.confidence_score,
            "signals": signals,
            "alternativeCategories": alternatives,
        })
    }
}

// ============================================================================
// Situation Show
// ============================================================================

/// Situation details for show command.
#[derive(Clone, Debug, PartialEq)]
pub struct SituationDetails {
    pub version: &'static str,
    pub situation_id: String,
    pub category: SituationCategory,
    pub original_text: String,
    pub created_at: String,
    pub context_hints: Vec<String>,
    pub related_memories: Vec<String>,
}

impl SituationDetails {
    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut output = format!("Situation: {}\n", self.situation_id);
        output.push_str(&format!("Category: {}\n", self.category.as_str()));
        output.push_str(&format!("Created: {}\n", self.created_at));
        output.push_str(&format!("Text: {}\n", self.original_text));

        if !self.context_hints.is_empty() {
            output.push_str("\nContext hints:\n");
            for hint in &self.context_hints {
                output.push_str(&format!("  - {hint}\n"));
            }
        }

        if !self.related_memories.is_empty() {
            output.push_str("\nRelated memories:\n");
            for mem in &self.related_memories {
                output.push_str(&format!("  - {mem}\n"));
            }
        }

        output
    }

    #[must_use]
    pub fn toon_output(&self) -> String {
        format!(
            "SHOW|{}|{}|{}",
            self.situation_id,
            self.category.as_str(),
            self.context_hints.len()
        )
    }

    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "command": "situation show",
            "version": self.version,
            "situationId": self.situation_id,
            "category": self.category.as_str(),
            "originalText": self.original_text,
            "createdAt": self.created_at,
            "contextHints": self.context_hints,
            "relatedMemories": self.related_memories,
        })
    }
}

// ============================================================================
// Situation Explain
// ============================================================================

/// Explanation of a situation.
#[derive(Clone, Debug, PartialEq)]
pub struct SituationExplanation {
    pub version: &'static str,
    pub situation_id: String,
    pub category: SituationCategory,
    pub explanation: String,
    pub recommendations: Vec<String>,
    pub relevant_rules: Vec<String>,
    pub potential_risks: Vec<String>,
}

impl SituationExplanation {
    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut output = format!("Situation: {}\n", self.situation_id);
        output.push_str(&format!("Category: {}\n\n", self.category.as_str()));
        output.push_str(&format!("Explanation:\n{}\n", self.explanation));

        if !self.recommendations.is_empty() {
            output.push_str("\nRecommendations:\n");
            for rec in &self.recommendations {
                output.push_str(&format!("  - {rec}\n"));
            }
        }

        if !self.relevant_rules.is_empty() {
            output.push_str("\nRelevant rules:\n");
            for rule in &self.relevant_rules {
                output.push_str(&format!("  - {rule}\n"));
            }
        }

        if !self.potential_risks.is_empty() {
            output.push_str("\nPotential risks:\n");
            for risk in &self.potential_risks {
                output.push_str(&format!("  - {risk}\n"));
            }
        }

        output
    }

    #[must_use]
    pub fn toon_output(&self) -> String {
        format!(
            "EXPLAIN|{}|{}|{}",
            self.situation_id,
            self.category.as_str(),
            self.recommendations.len()
        )
    }

    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "command": "situation explain",
            "version": self.version,
            "situationId": self.situation_id,
            "category": self.category.as_str(),
            "explanation": self.explanation,
            "recommendations": self.recommendations,
            "relevantRules": self.relevant_rules,
            "potentialRisks": self.potential_risks,
        })
    }
}

// ============================================================================
// Classification Logic
// ============================================================================

/// Classify task text into a situation category.
#[must_use]
pub fn classify_task(text: &str) -> ClassifyResult {
    let version = build_info().version;
    let lower = text.to_lowercase();

    let mut scores: Vec<(SituationCategory, f32, Vec<ClassificationSignal>)> = Vec::new();

    // Bug fix signals
    let mut bug_signals = Vec::new();
    let mut bug_score: f32 = 0.0;
    for pattern in [
        "fix", "bug", "error", "issue", "broken", "crash", "fail", "wrong",
    ] {
        if lower.contains(pattern) {
            bug_score += 0.3;
            bug_signals.push(ClassificationSignal {
                signal_type: "keyword",
                pattern: pattern.to_string(),
                weight: 0.3,
            });
        }
    }
    scores.push((SituationCategory::BugFix, bug_score.min(1.0), bug_signals));

    // Feature signals
    let mut feat_signals = Vec::new();
    let mut feat_score: f32 = 0.0;
    for pattern in [
        "add",
        "new",
        "feature",
        "implement",
        "create",
        "build",
        "support",
    ] {
        if lower.contains(pattern) {
            feat_score += 0.3;
            feat_signals.push(ClassificationSignal {
                signal_type: "keyword",
                pattern: pattern.to_string(),
                weight: 0.3,
            });
        }
    }
    scores.push((
        SituationCategory::Feature,
        feat_score.min(1.0),
        feat_signals,
    ));

    // Refactor signals
    let mut refactor_signals = Vec::new();
    let mut refactor_score: f32 = 0.0;
    for pattern in [
        "refactor",
        "clean",
        "reorganize",
        "restructure",
        "simplify",
        "extract",
    ] {
        if lower.contains(pattern) {
            refactor_score += 0.4;
            refactor_signals.push(ClassificationSignal {
                signal_type: "keyword",
                pattern: pattern.to_string(),
                weight: 0.4,
            });
        }
    }
    scores.push((
        SituationCategory::Refactor,
        refactor_score.min(1.0),
        refactor_signals,
    ));

    // Investigation signals
    let mut invest_signals = Vec::new();
    let mut invest_score: f32 = 0.0;
    for pattern in [
        "investigate",
        "debug",
        "understand",
        "explore",
        "why",
        "how",
        "what",
    ] {
        if lower.contains(pattern) {
            invest_score += 0.3;
            invest_signals.push(ClassificationSignal {
                signal_type: "keyword",
                pattern: pattern.to_string(),
                weight: 0.3,
            });
        }
    }
    scores.push((
        SituationCategory::Investigation,
        invest_score.min(1.0),
        invest_signals,
    ));

    // Documentation signals
    let mut doc_signals = Vec::new();
    let mut doc_score: f32 = 0.0;
    for pattern in [
        "document", "readme", "comment", "explain", "describe", "docs",
    ] {
        if lower.contains(pattern) {
            doc_score += 0.4;
            doc_signals.push(ClassificationSignal {
                signal_type: "keyword",
                pattern: pattern.to_string(),
                weight: 0.4,
            });
        }
    }
    scores.push((
        SituationCategory::Documentation,
        doc_score.min(1.0),
        doc_signals,
    ));

    // Testing signals
    let mut test_signals = Vec::new();
    let mut test_score: f32 = 0.0;
    for pattern in [
        "test",
        "spec",
        "assert",
        "verify",
        "coverage",
        "unit",
        "integration",
    ] {
        if lower.contains(pattern) {
            test_score += 0.35;
            test_signals.push(ClassificationSignal {
                signal_type: "keyword",
                pattern: pattern.to_string(),
                weight: 0.35,
            });
        }
    }
    scores.push((
        SituationCategory::Testing,
        test_score.min(1.0),
        test_signals,
    ));

    // Configuration signals
    let mut config_signals = Vec::new();
    let mut config_score: f32 = 0.0;
    for pattern in [
        "config",
        "setting",
        "env",
        "variable",
        "option",
        "parameter",
    ] {
        if lower.contains(pattern) {
            config_score += 0.35;
            config_signals.push(ClassificationSignal {
                signal_type: "keyword",
                pattern: pattern.to_string(),
                weight: 0.35,
            });
        }
    }
    scores.push((
        SituationCategory::Configuration,
        config_score.min(1.0),
        config_signals,
    ));

    // Deployment signals
    let mut deploy_signals = Vec::new();
    let mut deploy_score: f32 = 0.0;
    for pattern in [
        "deploy",
        "release",
        "publish",
        "ship",
        "push",
        "production",
        "staging",
    ] {
        if lower.contains(pattern) {
            deploy_score += 0.35;
            deploy_signals.push(ClassificationSignal {
                signal_type: "keyword",
                pattern: pattern.to_string(),
                weight: 0.35,
            });
        }
    }
    scores.push((
        SituationCategory::Deployment,
        deploy_score.min(1.0),
        deploy_signals,
    ));

    // Review signals
    let mut review_signals = Vec::new();
    let mut review_score: f32 = 0.0;
    for pattern in ["review", "pr", "feedback", "approve", "check", "audit"] {
        if lower.contains(pattern) {
            review_score += 0.35;
            review_signals.push(ClassificationSignal {
                signal_type: "keyword",
                pattern: pattern.to_string(),
                weight: 0.35,
            });
        }
    }
    scores.push((
        SituationCategory::Review,
        review_score.min(1.0),
        review_signals,
    ));

    // Sort by score descending
    scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let (category, confidence_score, signals) = if scores[0].1 > 0.0 {
        scores[0].clone()
    } else {
        (SituationCategory::Unknown, 0.0, Vec::new())
    };

    let confidence = if confidence_score >= 0.8 {
        ConfidenceLevel::High
    } else if confidence_score >= 0.5 {
        ConfidenceLevel::Medium
    } else {
        ConfidenceLevel::Low
    };

    let alternative_categories: Vec<(SituationCategory, f32)> = scores
        .iter()
        .skip(1)
        .filter(|(_, score, _)| *score > 0.0)
        .take(3)
        .map(|(cat, score, _)| (*cat, *score))
        .collect();

    ClassifyResult {
        version,
        input_text: text.to_string(),
        category,
        confidence,
        confidence_score,
        signals,
        alternative_categories,
    }
}

/// Show details for a situation (stub - would look up from storage).
#[must_use]
pub fn show_situation(situation_id: &str) -> Option<SituationDetails> {
    let version = build_info().version;

    // Stub implementation - in real version would look up from database
    Some(SituationDetails {
        version,
        situation_id: situation_id.to_string(),
        category: SituationCategory::Unknown,
        original_text: "[stored situation text]".to_string(),
        created_at: chrono::Utc::now().to_rfc3339(),
        context_hints: vec![
            "Check related memories for similar tasks".to_string(),
            "Review procedural rules for this category".to_string(),
        ],
        related_memories: vec![],
    })
}

/// Explain a situation (stub - would analyze from storage and rules).
#[must_use]
pub fn explain_situation(situation_id: &str) -> Option<SituationExplanation> {
    let version = build_info().version;

    // Stub implementation - in real version would analyze from database
    Some(SituationExplanation {
        version,
        situation_id: situation_id.to_string(),
        category: SituationCategory::Unknown,
        explanation: "This situation has been classified but no detailed analysis is available yet. \
            The explain command will provide richer context once memory and rule retrieval is wired.".to_string(),
        recommendations: vec![
            "Use `ee context` to retrieve relevant memories".to_string(),
            "Check `ee search` for similar past situations".to_string(),
        ],
        relevant_rules: vec![],
        potential_risks: vec![],
    })
}

// ============================================================================
// Tests
// ============================================================================

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
    fn situation_category_strings_are_stable() -> TestResult {
        ensure(SituationCategory::BugFix.as_str(), "bug_fix", "bug_fix")?;
        ensure(SituationCategory::Feature.as_str(), "feature", "feature")?;
        ensure(SituationCategory::Refactor.as_str(), "refactor", "refactor")?;
        ensure(
            SituationCategory::Investigation.as_str(),
            "investigation",
            "investigation",
        )?;
        ensure(
            SituationCategory::Documentation.as_str(),
            "documentation",
            "documentation",
        )?;
        ensure(SituationCategory::Testing.as_str(), "testing", "testing")?;
        ensure(
            SituationCategory::Configuration.as_str(),
            "configuration",
            "configuration",
        )?;
        ensure(
            SituationCategory::Deployment.as_str(),
            "deployment",
            "deployment",
        )?;
        ensure(SituationCategory::Review.as_str(), "review", "review")?;
        ensure(SituationCategory::Unknown.as_str(), "unknown", "unknown")
    }

    #[test]
    fn situation_category_parse_roundtrip() -> TestResult {
        for cat in SituationCategory::ALL {
            let parsed: SituationCategory = cat.as_str().parse().map_err(|e| format!("{e}"))?;
            ensure(parsed, *cat, "roundtrip")?;
        }
        Ok(())
    }

    #[test]
    fn classify_task_detects_bug_fix() -> TestResult {
        let result = classify_task("fix the broken login button");
        ensure(
            result.category,
            SituationCategory::BugFix,
            "bug fix category",
        )?;
        ensure(result.confidence_score > 0.0, true, "has confidence")
    }

    #[test]
    fn classify_task_detects_feature() -> TestResult {
        let result = classify_task("add new user profile page");
        ensure(
            result.category,
            SituationCategory::Feature,
            "feature category",
        )
    }

    #[test]
    fn classify_task_detects_refactor() -> TestResult {
        let result = classify_task("refactor the auth module to simplify flow");
        ensure(
            result.category,
            SituationCategory::Refactor,
            "refactor category",
        )
    }

    #[test]
    fn classify_task_returns_unknown_for_empty() -> TestResult {
        let result = classify_task("");
        ensure(
            result.category,
            SituationCategory::Unknown,
            "unknown for empty",
        )
    }

    #[test]
    fn classify_result_json_has_required_fields() -> TestResult {
        let result = classify_task("fix bug in login");
        let json = result.data_json();

        ensure(json.get("command").is_some(), true, "has command")?;
        ensure(json.get("category").is_some(), true, "has category")?;
        ensure(json.get("confidence").is_some(), true, "has confidence")?;
        ensure(json.get("signals").is_some(), true, "has signals")
    }

    #[test]
    fn confidence_level_thresholds_are_ordered() -> TestResult {
        ensure(
            ConfidenceLevel::High.threshold() > ConfidenceLevel::Medium.threshold(),
            true,
            "high > medium",
        )?;
        ensure(
            ConfidenceLevel::Medium.threshold() > ConfidenceLevel::Low.threshold(),
            true,
            "medium > low",
        )
    }

    #[test]
    fn schema_constants_are_stable() -> TestResult {
        ensure(
            SITUATION_CLASSIFY_SCHEMA_V1,
            "ee.situation.classify.v1",
            "classify schema",
        )?;
        ensure(
            SITUATION_SHOW_SCHEMA_V1,
            "ee.situation.show.v1",
            "show schema",
        )?;
        ensure(
            SITUATION_EXPLAIN_SCHEMA_V1,
            "ee.situation.explain.v1",
            "explain schema",
        )
    }
}
