use super::*;
use crate::core::cass_error_recall::{
    ToolOutput, failure_diagnostics, structured_error_diagnostics,
};
use crate::core::cass_error_recall::test_coverage::{Proof, Requirement};

fn ordinary(summary: &str) -> String {
    format!("{HEADER}\n  File \"private_task.py\", line 7, in main\n    private_source()\n{summary}\n")
}

fn group(summary: &str) -> String {
    format!(
        "{}\n  |   File \"private_group.py\", line 8, in run\n  | ExceptionGroup: private_group_label (1 sub-exception)\n  +-+---------------- 1 ----------------\n    | {summary}\n    +------------------------------------\n",
        exception_group::ROOT_HEADER,
    )
}

fn joined(first: &str, connector: &str, last: &str) -> String {
    format!("{first}\n{connector}\n\n{last}")
}

fn optimistic(text: &str) -> ToolOutput {
    ToolOutput {
        text: text.to_owned(),
        is_error: Some(false),
        exit_code: Some(0),
    }
}

#[test]
fn mixed_top_level_causes_and_contexts_retain_every_exception_in_order() {
    let first = "ValueError: invalid queue configuration";
    let last = "TypeError: worker received stale queue";
    for connector in [CAUSE, CONTEXT] {
        for (a, b) in [
            (ordinary(first), group(last)),
            (group(first), ordinary(last)),
            (group(first), group(last)),
        ] {
            let text = joined(&a, connector, &b);
            assert_eq!(chained_summaries(&text), Some(vec![first, last]));
            let expected = vec![diagnostic(first), diagnostic(last)];
            assert_eq!(structured_error_diagnostics(&text), expected);
            assert_eq!(failure_diagnostics(&optimistic(&text)), expected);
            assert!(!optimistic(&text).succeeded());
            assert_eq!(inspect(&text).unwrap().symptom.as_deref(), Some(first));
        }
    }
}

#[test]
fn malformed_group_prefix_cannot_be_replaced_by_a_valid_ordinary_suffix() {
    let first = group("ValueError: invalid queue configuration");
    for bad in [
        first.replace("(1 sub-exception)", "(2 sub-exceptions)"),
        first.replace("    +------------------------------------\n", ""),
        first.replace("  +-+---------------- 1", "  +-+---------------- 2"),
        first.replace("  |   File", "  |    File"),
        first.replace("ValueError: invalid queue configuration", "unfinished"),
    ] {
        let text = joined(&bad, CAUSE, &ordinary("RuntimeError: queue startup aborted"));
        assert!(chained_summaries(&text).is_none());
        let decoded = inspect(&text).unwrap();
        assert!(decoded.failed && !decoded.allows_completion);
        assert!(decoded.diagnostics.is_empty());
        assert!(decoded.symptom.is_none());
        assert!(structured_error_diagnostics(&text).is_empty());
    }
}

#[test]
fn every_section_must_be_complete_even_after_the_diagnostic_display_limit() {
    let mut parts = vec![group("ValueError: bad queue")];
    for summary in [
        "TypeError: bad worker", "RuntimeError: bad state", "LookupError: bad key",
        "OSError: bad resource", "IndexError: bad offset",
    ] {
        parts.push(ordinary(summary));
    }
    let complete = parts.join(&format!("\n{CAUSE}\n\n"));
    assert_eq!(chained_summaries(&complete).unwrap().len(), 6);
    assert_eq!(inspect(&complete).unwrap().diagnostics.len(), MAX_DIAGNOSTICS_PER_FAILURE);
    for tail in [
        group("ValueError: bad queue").replace("(1 sub-exception)", "(3 sub-exceptions)"),
        format!("{HEADER}\n  File \"unfinished"),
        "ordinary stdout, not a traceback\n".to_owned(),
    ] {
        let text = joined(&complete, CONTEXT, &tail);
        assert!(inspect(&text).unwrap().diagnostics.is_empty());
        assert!(!optimistic(&text).succeeded());
    }
    let repeated = joined(&group("ValueError: bad queue"), CAUSE, &ordinary("ValueError: bad queue"));
    assert_eq!(inspect(&repeated).unwrap().diagnostics.len(), 1);
}

#[test]
fn connectors_cannot_hide_missing_sections_or_arbitrary_intermediate_output() {
    let a = group("ValueError: invalid queue configuration");
    let b = ordinary("RuntimeError: queue startup aborted");
    for text in [
        joined(&a, CAUSE, ""),
        joined("", CAUSE, &a),
        joined(&a, CAUSE, &format!("unexpected output\n{b}")),
        format!("{a}\n{b}"),
        format!("{a}\n{CAUSE}\n\n{CONTEXT}\n\n{b}"),
        joined(&a, CAUSE, b.trim_end_matches('\n')),
        joined(&a, CAUSE, &format!("{b}Process exited with code 0\n")),
    ] {
        assert!(chained_summaries(&text).is_none(), "{text}");
        assert!(failure_diagnostics(&optimistic(&text)).is_empty());
        assert!(!optimistic(&text).succeeded());
    }
}

#[test]
fn chain_and_frame_budgets_are_shared_across_ordinary_and_group_sections() {
    let one = group("ValueError: invalid queue configuration");
    let separator = format!("\n{CAUSE}\n\n");
    let exact = std::iter::repeat_n(one.as_str(), MAX_CHAIN).collect::<Vec<_>>().join(&separator);
    assert_eq!(chained_summaries(&exact).unwrap().len(), MAX_CHAIN);
    assert!(chained_summaries(&joined(&exact, CAUSE, &one)).is_none());

    let a = ordinary("ValueError: invalid queue configuration").replace(
        "  File \"private_task.py\", line 7, in main\n",
        &"  File \"a\", line 1\n".repeat(MAX_FRAMES / 2),
    );
    let b = one.replace(
        "  |   File \"private_group.py\", line 8, in run\n",
        &"  |   File \"b\", line 1\n".repeat(MAX_FRAMES / 2),
    );
    for text in [joined(&a, CAUSE, &b), joined(&b, CONTEXT, &a)] {
        assert!(text.len() < MAX_SCANNED_OUTPUT_BYTES);
        assert_eq!(chained_summaries(&text).unwrap().len(), 2);
        assert!(chained_summaries(&joined(&text, CAUSE, &one)).is_none());
    }
}

#[test]
fn group_and_source_margins_do_not_become_top_level_chain_boundaries() {
    let a = ordinary("ValueError: invalid queue configuration")
        .replace("    private_source()", &format!("    {CAUSE}"));
    let b = group("TypeError: worker received stale queue");
    let text = joined(&a, CONTEXT, &b);
    let expected = vec![
        diagnostic("ValueError: invalid queue configuration"),
        diagnostic("TypeError: worker received stale queue"),
    ];
    for text in [
        text.clone(),
        format!("ordinary preamble\n{text}"),
        text.replace('\n', "\r\n"),
        format!("\u{1b}[31m{text}\u{1b}[0m"),
    ] {
        assert_eq!(structured_error_diagnostics(&text), expected);
    }
    // Branch-local chains still require a separate tree grammar; do not
    // mistake their indented connector for an outer section and salvage it.
    let nested = b.replace("    | TypeError:", &format!("    | {CAUSE}\n    | TypeError:"));
    assert!(chained_summaries(&nested).is_none());
}

#[test]
fn mixed_chains_preserve_redaction_and_do_not_expose_group_or_source_payloads() {
    let secret = format!("ghp_{}", "Q".repeat(36));
    let summary = format!("ValueError: api_key={secret}");
    let text = joined(&group(&summary), CAUSE, &ordinary("TypeError: stale queue"));
    let safe = crate::policy::redact_secret_like_content(&summary).content;
    let expected = joined(&group(&safe), CAUSE, &ordinary("TypeError: stale queue"));
    let decoded = inspect(&text).unwrap();
    assert_eq!(decoded.diagnostics, inspect(&expected).unwrap().diagnostics);
    let projection = format!("{:?} {:?}", decoded.diagnostics, decoded.symptom);
    for hidden in [secret.as_str(), "private_group_label", "private_group.py", "private_source"] {
        assert!(!projection.contains(hidden));
    }
}

#[test]
fn mixed_command_failures_never_weaken_a_test_inventory_requirement() {
    let chain = optimistic(&joined(
        &group("ValueError: invalid queue configuration"),
        CAUSE,
        &ordinary("RuntimeError: queue startup aborted"),
    ));
    assert!(!Requirement::capture(&chain, true).is_guarded());
    let tests = optimistic("=== test session starts ===\ncollected 1 item\ntests/test_queue.py::test_drain FAILED [100%]\n=== 1 failed in 0.01s ===\n");
    let mut required = Requirement::capture(&tests, true);
    required.refresh(&chain, true);
    assert!(required.is_guarded());
    assert!(!required.is_verified_by(&Proof::capture(&optimistic("queue ready\n"))));
}
