use std::collections::BTreeSet;

use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct InferenceCase {
    id: String,
    content: String,
    level: String,
    kind: String,
}

#[test]
fn note_inference_fixture_meets_i3_accuracy_gate() {
    let cases = parse_cases();
    assert!(
        cases.len() >= 100,
        "I3 fixture must contain at least 100 labeled cases; got {}",
        cases.len()
    );

    let mut ids = BTreeSet::new();
    let mut exact_matches = 0usize;
    let mut failures = Vec::new();

    for case in &cases {
        assert!(
            ids.insert(case.id.clone()),
            "duplicate fixture id {}",
            case.id
        );
        let (predicted_level, predicted_kind) = ee::cli::infer_note_level_kind(&case.content);
        let matches = predicted_level == case.level && predicted_kind == case.kind;
        if matches {
            exact_matches += 1;
        } else {
            failures.push(format!(
                "{}: expected {}/{}, got {}/{} for {:?}",
                case.id, case.level, case.kind, predicted_level, predicted_kind, case.content
            ));
        }
    }

    // The wrapper emits one label pair per case, so micro precision and recall
    // are both exact-match accuracy for this fixed multiclass fixture.
    let micro_precision = exact_matches as f64 / cases.len() as f64;
    let micro_recall = micro_precision;
    assert!(
        micro_precision >= 0.80,
        "I3 precision gate failed: {:.3}; first failures: {}",
        micro_precision,
        failures
            .iter()
            .take(10)
            .cloned()
            .collect::<Vec<_>>()
            .join(" | ")
    );
    assert!(
        micro_recall >= 0.80,
        "I3 recall gate failed: {:.3}; first failures: {}",
        micro_recall,
        failures
            .iter()
            .take(10)
            .cloned()
            .collect::<Vec<_>>()
            .join(" | ")
    );
}

fn parse_cases() -> Vec<InferenceCase> {
    include_str!("fixtures/note_inference_cases.jsonl")
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(idx, line)| {
            serde_json::from_str::<InferenceCase>(line).unwrap_or_else(|error| {
                panic!("invalid JSONL at line {}: {error}: {line}", idx + 1)
            })
        })
        .collect()
}
