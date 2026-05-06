//! Short-tag vocabulary for result explainability (EE-5jdv).
//!
//! Every search result or context pack item carries a `why[]` array of
//! short tags explaining why it was selected. This enum defines the
//! closed vocabulary for those tags, ensuring stable JSON output and
//! documentation parity.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Short tag explaining why a result was included.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WhyTag {
    /// Result is in the same workspace scope as the query.
    WorkspaceScope,
    /// One or more query tags matched the result's tags.
    TagMatch,
    /// Result is a procedural rule that has been proven effective.
    ProvenRule,
    /// Result is recent (high recency multiplier).
    Fresh,
    /// Result has high confidence from multiple sources.
    HighConfidence,
    /// Result has demonstrated utility in past packs.
    HighUtility,
    /// Result is central in the memory graph (high PageRank/betweenness).
    GraphCentral,
    /// Result was explicitly pinned by the user or a rule.
    Pinned,
    /// Result is semantically similar to the query (vector match).
    SemanticMatch,
    /// Result matched query terms lexically (BM25/FTS).
    LexicalMatch,
    /// Result is linked to another selected result.
    LinkedTo,
    /// Result provides supporting evidence for a rule or claim.
    SupportsEvidence,
    /// Result is a recent decision relevant to the query context.
    RecentDecision,
    /// Result contains procedural guidance (how-to, workflow).
    ProceduralGuidance,
    /// Result is from a trusted source (high trust class).
    TrustedSource,
    /// Result was selected to fill a required context section.
    SectionFill,
    /// Result was included for diversity (MMR selection).
    DiversityPick,
    /// Result is a fallback when primary sources are unavailable.
    FallbackSource,
}

impl WhyTag {
    /// All defined why tags in canonical order.
    pub const ALL: &'static [WhyTag] = &[
        WhyTag::WorkspaceScope,
        WhyTag::TagMatch,
        WhyTag::ProvenRule,
        WhyTag::Fresh,
        WhyTag::HighConfidence,
        WhyTag::HighUtility,
        WhyTag::GraphCentral,
        WhyTag::Pinned,
        WhyTag::SemanticMatch,
        WhyTag::LexicalMatch,
        WhyTag::LinkedTo,
        WhyTag::SupportsEvidence,
        WhyTag::RecentDecision,
        WhyTag::ProceduralGuidance,
        WhyTag::TrustedSource,
        WhyTag::SectionFill,
        WhyTag::DiversityPick,
        WhyTag::FallbackSource,
    ];

    /// Short snake_case string for JSON output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            WhyTag::WorkspaceScope => "workspace_scope",
            WhyTag::TagMatch => "tag_match",
            WhyTag::ProvenRule => "proven_rule",
            WhyTag::Fresh => "fresh",
            WhyTag::HighConfidence => "high_confidence",
            WhyTag::HighUtility => "high_utility",
            WhyTag::GraphCentral => "graph_central",
            WhyTag::Pinned => "pinned",
            WhyTag::SemanticMatch => "semantic_match",
            WhyTag::LexicalMatch => "lexical_match",
            WhyTag::LinkedTo => "linked_to",
            WhyTag::SupportsEvidence => "supports_evidence",
            WhyTag::RecentDecision => "recent_decision",
            WhyTag::ProceduralGuidance => "procedural_guidance",
            WhyTag::TrustedSource => "trusted_source",
            WhyTag::SectionFill => "section_fill",
            WhyTag::DiversityPick => "diversity_pick",
            WhyTag::FallbackSource => "fallback_source",
        }
    }

    /// Human-readable explanation of this tag.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            WhyTag::WorkspaceScope => "in the same workspace scope as the query",
            WhyTag::TagMatch => "matched one or more query tags",
            WhyTag::ProvenRule => "a procedural rule proven effective",
            WhyTag::Fresh => "recently created or updated",
            WhyTag::HighConfidence => "high confidence from multiple sources",
            WhyTag::HighUtility => "demonstrated utility in past packs",
            WhyTag::GraphCentral => "central in the memory graph",
            WhyTag::Pinned => "explicitly pinned by user or rule",
            WhyTag::SemanticMatch => "semantically similar to the query",
            WhyTag::LexicalMatch => "matched query terms lexically",
            WhyTag::LinkedTo => "linked to another selected result",
            WhyTag::SupportsEvidence => "supports a rule or claim with evidence",
            WhyTag::RecentDecision => "a recent decision relevant to context",
            WhyTag::ProceduralGuidance => "contains procedural guidance",
            WhyTag::TrustedSource => "from a trusted source",
            WhyTag::SectionFill => "fills a required context section",
            WhyTag::DiversityPick => "selected for diversity",
            WhyTag::FallbackSource => "fallback when primary sources unavailable",
        }
    }
}

impl fmt::Display for WhyTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error returned when parsing an unknown why tag string.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseWhyTagError {
    pub input: String,
}

impl fmt::Display for ParseWhyTagError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown why tag: {}", self.input)
    }
}

impl std::error::Error for ParseWhyTagError {}

impl FromStr for WhyTag {
    type Err = ParseWhyTagError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "workspace_scope" => Ok(WhyTag::WorkspaceScope),
            "tag_match" => Ok(WhyTag::TagMatch),
            "proven_rule" => Ok(WhyTag::ProvenRule),
            "fresh" => Ok(WhyTag::Fresh),
            "high_confidence" => Ok(WhyTag::HighConfidence),
            "high_utility" => Ok(WhyTag::HighUtility),
            "graph_central" => Ok(WhyTag::GraphCentral),
            "pinned" => Ok(WhyTag::Pinned),
            "semantic_match" => Ok(WhyTag::SemanticMatch),
            "lexical_match" => Ok(WhyTag::LexicalMatch),
            "linked_to" => Ok(WhyTag::LinkedTo),
            "supports_evidence" => Ok(WhyTag::SupportsEvidence),
            "recent_decision" => Ok(WhyTag::RecentDecision),
            "procedural_guidance" => Ok(WhyTag::ProceduralGuidance),
            "trusted_source" => Ok(WhyTag::TrustedSource),
            "section_fill" => Ok(WhyTag::SectionFill),
            "diversity_pick" => Ok(WhyTag::DiversityPick),
            "fallback_source" => Ok(WhyTag::FallbackSource),
            _ => Err(ParseWhyTagError {
                input: s.to_owned(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_tags_round_trip_through_from_str() {
        for tag in WhyTag::ALL {
            let s = tag.as_str();
            let parsed: WhyTag = s.parse().expect("should parse");
            assert_eq!(parsed, *tag);
        }
    }

    #[test]
    fn all_tags_round_trip_through_serde() {
        for tag in WhyTag::ALL {
            let json = serde_json::to_string(tag).expect("serialize");
            let parsed: WhyTag = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(parsed, *tag);
        }
    }

    #[test]
    fn display_matches_as_str() {
        for tag in WhyTag::ALL {
            assert_eq!(tag.to_string(), tag.as_str());
        }
    }

    #[test]
    fn unknown_tag_returns_error() {
        let err = "unknown_tag".parse::<WhyTag>().unwrap_err();
        assert_eq!(err.input, "unknown_tag");
    }

    #[test]
    fn all_array_is_complete() {
        assert_eq!(WhyTag::ALL.len(), 18);
    }

    #[test]
    fn descriptions_are_non_empty() {
        for tag in WhyTag::ALL {
            assert!(!tag.description().is_empty());
        }
    }
}
