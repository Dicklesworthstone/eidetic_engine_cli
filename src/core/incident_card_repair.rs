//! Conservative exclusions for source-backed repair explanations.
//!
//! A successful retry verifies a command, not every nearby sentence. Plans,
//! questions, guesses and explicit non-actions must not become helpful repair
//! links merely because they repeat a compiler code or a file name. This is
//! a deterministic exclusion filter, not a natural-language causality proof.
//! The caller retains source admission and exact invocation/outcome checks.

/// Ignore quoted code/values for language classification, while retaining
/// contraction apostrophes. Never change the original sentence shown to users.
fn language_words(sentence: &str) -> Vec<String> {
    let mut prose = String::with_capacity(sentence.len());
    let mut quote = None;
    let mut escaped = false;
    let chars = sentence.chars().collect::<Vec<_>>();
    for (index, &character) in chars.iter().enumerate() {
        if let Some(delimiter) = quote {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == delimiter {
                quote = None;
            }
            continue;
        }
        let apostrophe = matches!(character, '\'' | '’');
        let contraction = apostrophe
            && index > 0
            && chars[index - 1].is_alphanumeric()
            && chars.get(index + 1).is_some_and(|next| next.is_alphanumeric());
        if matches!(character, '`' | '"') || (character == '\'' && !contraction) {
            quote = Some(character);
            prose.push(' ');
        } else if contraction {
            prose.push('\'');
        } else {
            prose.push(character);
        }
    }
    prose
        .split(|character: char| !character.is_alphanumeric() && character != '\'')
        .filter(|word| !word.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

fn phrase(words: &[String], expected: &[&str]) -> bool {
    words.windows(expected.len()).any(|window| {
        window.iter().zip(expected).all(|(word, expected)| word.as_str() == *expected)
    })
}

fn action(word: &str) -> bool {
    matches!(
        word,
        "fix" | "fixed" | "fixing" | "repair" | "repaired" | "resolve" | "resolved"
            | "work" | "worked" | "working" | "help" | "helped" | "succeed" | "succeeded"
            | "add" | "added" | "adding" | "change" | "changed" | "changing"
            | "remove" | "removed" | "removing" | "replace" | "replaced" | "replacing"
            | "update" | "updated" | "updating" | "implement" | "implemented"
            | "apply" | "applied" | "applying" | "convert" | "converted" | "converting"
            | "rename" | "renamed" | "renaming" | "restore" | "restored"
            | "revert" | "reverted" | "enable" | "enabled" | "disable" | "disabled"
            | "modify" | "modified" | "modifying" | "patch" | "patched"
    )
}

/// Reject explicit uncertainty and non-actions before relevance scoring.
/// Anchor overlap must never compensate for these exclusions. Ordinary causal
/// explanations remain eligible, including descriptions of the original bug.
/// False negatives abstain; no sentence is rewritten into an affirmative claim.
pub(super) fn excluded(sentence: &str) -> bool {
    if sentence.trim_end().ends_with('?') {
        return true;
    }
    let words = language_words(sentence);
    if words.is_empty() {
        return true;
    }
    if words.iter().any(|word| {
        matches!(
            word.as_str(),
            "will" | "would" | "should" | "could" | "might" | "maybe" | "perhaps"
                | "probably" | "possibly" | "hopefully" | "hypothetically" | "likely"
                | "unlikely" | "unverified" | "untested" | "unresolved" | "unfixed"
        ) || word.ends_with("'ll")
    }) {
        return true;
    }
    if [
        &["let", "me"][..],
        &["let", "us"],
        &["let's"],
        &["todo"],
        &["plan"],
        &["planned"],
        &["proposed"],
        &["possible", "fix"],
        &["potential", "fix"],
        &["suggested", "fix"],
        &["hypothesis"],
        &["try"],
        &["please"],
    ]
    .iter()
    .any(|prefix| {
        words.len() >= prefix.len()
            && words.iter().zip(*prefix).all(|(word, expected)| word.as_str() == *expected)
    }) {
        return true;
    }
    if [
        &["going", "to"][..],
        &["plan", "to"],
        &["plan", "is", "to"],
        &["planning", "to"],
        &["intend", "to"],
        &["intended", "to"],
        &["want", "to"],
        &["hope", "to"],
        &["expect", "to"],
        &["need", "to", "try"],
        &["not", "sure"],
        &["not", "yet"],
        &["no", "fix"],
        &["no", "repair"],
        &["no", "change"],
        &["no", "changes"],
        &["nothing", "changed"],
        &["not", "a", "fix"],
        &["not", "the", "fix"],
        &["not", "a", "repair"],
        &["still", "fails"],
        &["still", "failing"],
        &["still", "broken"],
        &["failed", "again"],
    ]
    .iter()
    .any(|expected| phrase(&words, expected)) {
        return true;
    }
    for (index, word) in words.iter().enumerate() {
        let negated = matches!(word.as_str(), "not" | "never" | "cannot" | "without")
            || word.ends_with("n't");
        if !negated {
            continue;
        }
        // A completed edit may explain the original defect: "I added the
        // derive because Widget did not implement Serialize". That is not
        // the same as "I added the derive but it did not work".
        let original_cause = words[..index]
            .iter()
            .rposition(|word| word == "because")
            .is_some_and(|cause| {
                words[..cause].iter().any(|word| {
                    action(word) && (word.ends_with("ed") || word == "fixed")
                })
            });
        if original_cause {
            continue;
        }
        // Auxiliary/adverb words may separate negation from the denied action:
        // "hasn't actually been fixed" is no more a repair than "not fixed".
        let mut following = words[index + 1..].iter();
        let next = following.find(|word| {
            !matches!(word.as_str(), "yet" | "actually" | "really" | "been" | "be" | "ever")
        });
        if next.is_some_and(|word| action(word)) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn future_plans_and_questions_cannot_gain_authority_from_an_error_code() {
        for text in [
            "I will fix E0277 by adding Serialize to Widget.",
            "I'll update src/widget.rs next.",
            "I’ll update src/widget.rs next.",
            "We are going to replace the E0277 call site.",
            "My plan is to fix E0277 tomorrow.",
            "I plan to add the missing Serialize derive.",
            "Let me fix E0277 by adding the derive.",
            "Maybe adding Serialize fixes E0277.",
            "Adding Serialize should fix E0277.",
            "Can adding Serialize fix E0277?",
            "Proposed fix: add Serialize to Widget.",
            "I fixed E0277, but this is still unverified.",
        ] {
            assert!(excluded(text), "{text}");
        }
    }

    #[test]
    fn explicitly_negated_or_ineffective_actions_are_not_repairs() {
        for text in [
            "I have not fixed E0277 in src/widget.rs.",
            "I haven't actually been fixing E0277.",
            "I didn’t add Serialize to Widget.",
            "The E0277 change did not work.",
            "The change never really helped the build.",
            "I made no changes to src/widget.rs.",
            "This is not a fix for E0277.",
            "After the change the build still fails with E0277.",
            "Without changing Widget the E0277 error remains.",
            "The build failed again after the change.",
        ] {
            assert!(excluded(text), "{text}");
        }
    }

    #[test]
    fn actual_edits_and_concrete_causal_explanations_remain_eligible() {
        for text in [
            "I fixed E0277 by deriving Serialize for Widget.",
            "Added the derive in src/widget.rs.",
            "Ledger::flush_all was renamed, so I updated the call site.",
            "Widget needs to derive Serialize, because the store serializes it.",
            "The widget store serializes it.",
            "I added Serialize because Widget did not implement the trait.",
            "I fixed the cache by flushing pending writes before reads.",
            "Now I have added the missing generated native header.",
        ] {
            assert!(!excluded(text), "{text}");
        }
    }

    #[test]
    fn quoted_identifiers_do_not_masquerade_as_modal_or_negated_prose() {
        for text in [
            "I renamed `maybe` to `resolved` in the parser.",
            "I removed the \"not fixed\" placeholder in the handler.",
            "I replaced 'unverified' with the actual status.",
        ] {
            assert!(!excluded(text), "{text}");
        }
        assert!(excluded("`I fixed E0277 by changing Widget`"));
        assert!(excluded("\"I fixed E0277 by changing Widget\""));
        assert!(excluded("'I fixed E0277 by changing Widget'"));
    }

    #[test]
    fn empty_unicode_and_identifier_boundaries_are_deterministic() {
        assert!(excluded(""));
        assert!(excluded("   "));
        for text in ["I updated the willow buffer.", "I added the mayfly parser.", "I fixed café handling."] {
            assert!(!excluded(text));
            assert_eq!(excluded(text), excluded(text));
        }
    }
}
