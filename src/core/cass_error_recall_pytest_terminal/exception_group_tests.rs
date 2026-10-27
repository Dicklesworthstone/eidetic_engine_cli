use super::super::{diagnostic, inspect};
use super::*;
use crate::core::cass_error_recall::test_coverage::{Proof, Requirement};
use crate::core::cass_error_recall::{
    ToolOutput, failure_diagnostics, structured_error_diagnostics,
};

fn group(leaves: &[&str]) -> String {
    let count = leaves.len();
    let mut text = format!(
        "{ROOT_HEADER}\n  |   File \"task.py\", line 9, in main\n  | ExceptionGroup: task batch ({count} sub-exception{})\n",
        if count == 1 { "" } else { "s" },
    );
    for (index, leaf) in leaves.iter().enumerate() {
        if index == 0 {
            text.push_str("  +-+---------------- 1 ----------------\n");
        } else {
            text.push_str(&format!(
                "    +---------------- {} ----------------\n",
                index + 1
            ));
        }
        text.push_str(&format!("    | {leaf}\n"));
    }
    text.push_str("    +------------------------------------\n");
    text
}

fn output(text: &str) -> ToolOutput {
    ToolOutput {
        text: text.to_owned(),
        is_error: Some(false),
        exit_code: Some(0),
    }
}

fn refused(text: &str) {
    assert!(summaries(text).is_none(), "{text}");
    let decoded = inspect(text).expect("recognized group failure");
    assert!(decoded.failed && !decoded.allows_completion);
    assert!(
        decoded.diagnostics.is_empty(),
        "no prefix may escape: {text}"
    );
    assert!(!output(text).succeeded());
}

#[test]
fn grouped_leaves_share_plain_exception_fingerprints_and_never_certify_success() {
    let leaves = [
        "ValueError: invalid configuration",
        "TypeError: expected bytes",
    ];
    let text = group(&leaves);
    let expected = leaves
        .iter()
        .map(|leaf| diagnostic(leaf))
        .collect::<Vec<_>>();
    assert_eq!(summaries(&text).unwrap(), leaves);
    let decoded = inspect(&text).unwrap();
    assert_eq!(decoded.diagnostics, expected);
    assert_eq!(decoded.symptom.as_deref(), Some(leaves[0]));
    assert_eq!(structured_error_diagnostics(&text), expected);
    for exit_code in [None, Some(0), Some(1)] {
        let mut result = output(&text);
        result.exit_code = exit_code;
        assert_eq!(failure_diagnostics(&result), expected);
        assert!(result.failed());
        assert!(!result.succeeded());
        assert_eq!(result.text, text, "source bytes are never rewritten");
    }
}

#[test]
fn nested_last_children_use_the_native_shared_closing_border() {
    // CPython emits only the deepest closing border for a last-child group.
    let text = concat!(
        "  + Exception Group Traceback (most recent call last):\n",
        "  |   File \"task.py\", line 9, in main\n",
        "  | ExceptionGroup: outer (2 sub-exceptions)\n",
        "  +-+---------------- 1 ----------------\n",
        "    | ValueError: first\n",
        "    +---------------- 2 ----------------\n",
        "    | ExceptionGroup: inner (2 sub-exceptions)\n",
        "    +-+---------------- 1 ----------------\n",
        "      | TypeError: second\n",
        "      +---------------- 2 ----------------\n",
        "      | RuntimeError: third\n",
        "      +------------------------------------\n",
    );
    assert_eq!(
        summaries(text).unwrap(),
        [
            "ValueError: first",
            "TypeError: second",
            "RuntimeError: third"
        ]
    );
    refused(&format!(
        "{text}    +------------------------------------\n"
    ));
    let mut sibling = text.replace("outer (2 sub-exceptions)", "outer (3 sub-exceptions)");
    sibling.push_str("    +---------------- 3 ----------------\n    | LookupError: fourth\n    +------------------------------------\n");
    assert_eq!(summaries(&sibling).unwrap().len(), 4);
}

#[test]
fn group_and_leaf_stacks_are_structural_not_diagnostic_payloads() {
    let plain = group(&["ValueError: bad", "KeyboardInterrupt"]);
    let with_stacks = plain
        .replace("ExceptionGroup: task batch", "BaseExceptionGroup: private_group_label")
        .replace(
            "    | ValueError: bad",
            "    | Traceback (most recent call last):\n    |   File \"/private/leaf.py\", line 17, in child\n    |     RuntimeError: source_sentinel\n    |     ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^\n    | ValueError: bad",
        );
    assert_eq!(summaries(&with_stacks), summaries(&plain));
    let rendered = format!("{:?}", inspect(&with_stacks).unwrap().diagnostics);
    for hidden in ["private_group_label", "/private/leaf.py", "source_sentinel"] {
        assert!(!rendered.contains(hidden));
    }
}

#[test]
fn counts_ordinals_margins_and_tail_corruption_refuse_all_leaf_keys() {
    let valid = group(&["ValueError: first", "TypeError: second"]);
    for invalid in [
        valid.replace("(2 sub-exceptions)", "(3 sub-exceptions)"),
        valid.replace("(2 sub-exceptions)", "(1 sub-exception)"),
        valid.replace("(2 sub-exceptions)", "(02 sub-exceptions)"),
        valid.replace("(2 sub-exceptions)", "(0 sub-exceptions)"),
        valid.replace(
            "(2 sub-exceptions)",
            "(999999999999999999999999 sub-exceptions)",
        ),
        valid.replace(
            "---------------- 2 ----------------",
            "---------------- 1 ----------------",
        ),
        valid.replace("    | TypeError", "   | TypeError"),
        valid.replace("    +------------------------------------\n", ""),
        valid.replace("TypeError: second", "OK"),
        valid.replace("line 9", "line -9"),
        format!("{valid}Process exited with code 0\n"),
    ] {
        refused(&invalid);
    }
}

#[test]
fn formatter_elisions_notes_and_chains_cannot_salvage_a_partial_group() {
    let valid = group(&["ValueError: first", "TypeError: second"]);
    for invalid in [
        valid.replace("---------------- 2 ----------------", "---------------- ... ----------------"),
        valid.replace("TypeError: second", "... (max_group_depth is 10)"),
        valid.replace("TypeError: second", "and 3 more exceptions"),
        valid.replace("    | TypeError: second", "    | TypeError: second\n    | private note"),
        valid.replace("  +-+", "  | group note\n  +-+"),
        valid.replace(
            "    | ValueError: first",
            "    | ValueError: first\n    | \n    | The above exception was the direct cause of the following exception:\n    | \n    | RuntimeError: later",
        ),
    ] {
        refused(&invalid);
    }
}

#[test]
fn the_diagnostic_display_cap_never_short_circuits_tree_validation() {
    let leaves = [
        "ValueError: first",
        "TypeError: second",
        "LookupError: third",
        "RuntimeError: fourth",
        "AssertionError: fifth",
        "OSError: sixth",
    ];
    let text = group(&leaves);
    assert_eq!(summaries(&text).unwrap().len(), leaves.len());
    assert_eq!(
        inspect(&text).unwrap().diagnostics.len(),
        super::super::MAX_DIAGNOSTICS_PER_FAILURE
    );
    refused(&text.replace("OSError: sixth", "unfinished"));
    let duplicate = group(&[leaves[0], leaves[0]]);
    assert_eq!(inspect(&duplicate).unwrap().diagnostics.len(), 1);
}

fn nested(groups: usize) -> String {
    let mut text = format!("{ROOT_HEADER}\n  |   File \"task.py\", line 9, in main\n");
    for depth in 1..=groups {
        let margin = "  ".repeat(depth);
        text.push_str(&format!("{margin}| ExceptionGroup: nested (1 sub-exception)\n{margin}+-+---------------- 1 ----------------\n"));
    }
    let margin = "  ".repeat(groups + 1);
    text.push_str(&format!(
        "{margin}| ValueError: leaf\n{margin}+------------------------------------\n"
    ));
    text
}

#[test]
fn depth_nodes_bytes_and_frames_are_bounded_before_emitting_any_diagnostic() {
    assert_eq!(
        summaries(&nested(MAX_GROUP_DEPTH)).unwrap(),
        ["ValueError: leaf"]
    );
    refused(&nested(MAX_GROUP_DEPTH + 1));
    let leaves = vec!["ValueError: leaf"; MAX_GROUP_NODES - 1];
    assert_eq!(summaries(&group(&leaves)).unwrap().len(), leaves.len());
    refused(&group(&vec!["ValueError: leaf"; MAX_GROUP_NODES]));
    let valid = group(&["ValueError: leaf"]);
    let extra_frames = "  |   File \"task.py\", line 9, in main\n".repeat(MAX_FRAMES);
    refused(&valid.replace(
        "  | ExceptionGroup:",
        &format!("{extra_frames}  | ExceptionGroup:"),
    ));
    refused(&format!("{valid}{}", " ".repeat(MAX_SCANNED_OUTPUT_BYTES)));
    refused(&format!("{}\n{valid}", "\n".repeat(MAX_LINES)));
}

#[test]
fn presentation_changes_preserve_leaf_identity_and_credentials_are_redacted_first() {
    let text = group(&["ValueError: first", "TypeError: second"]);
    let expected = inspect(&text).unwrap().diagnostics;
    for visible in [
        format!("ordinary stdout\n{text}"),
        text.replace('\n', "\r\n"),
        format!("\u{1b}[31m{text}\u{1b}[0m"),
        text.replace("ValueError", "\u{1b}[1mValueError\u{1b}[0m"),
    ] {
        assert_eq!(structured_error_diagnostics(&visible), expected);
    }
    let secret = format!("ghp_{}", "Q".repeat(36));
    let summary = format!("RuntimeError: api_key={secret}");
    let safe = crate::policy::redact_secret_like_content(&summary).content;
    let raw = inspect(&group(&[&summary])).unwrap();
    assert_eq!(raw.diagnostics, vec![diagnostic(&safe)]);
    assert!(!format!("{:?}", raw.diagnostics).contains(&secret));
    assert!(!raw.symptom.unwrap().contains(&secret));
}

#[test]
fn grouping_never_downgrades_an_existing_executed_test_requirement() {
    let failed_test = ToolOutput {
        text: "=== test session starts ===\ncollected 1 item\ntests/test_task.py::test_run FAILED [100%]\n=== 1 failed in 0.01s ===\n".to_owned(),
        is_error: Some(true),
        exit_code: Some(1),
    };
    let grouped = output(&group(&["ValueError: task failed"]));
    assert!(!Requirement::capture(&grouped, true).is_guarded());
    let mut requirement = Requirement::capture(&failed_test, true);
    assert!(requirement.is_guarded());
    requirement.refresh(&grouped, true);
    assert!(requirement.is_guarded());
    assert!(!requirement.is_verified_by(&Proof::capture(&output("done"))));
    // A pytest traceback tree is still governed by the native test report.
    let native = failed_test
        .text
        .replace("=== 1 failed", &format!("{}=== 1 failed", grouped.text));
    assert!(Requirement::capture(&output(&native), true).is_guarded());
}

#[test]
fn quoted_and_shifted_group_examples_do_not_supply_exception_keys() {
    let text = group(&["ValueError: example"]);
    let quoted = serde_json::json!({"example": text}).to_string();
    assert!(inspect(&quoted).is_none());
    let shifted = text
        .lines()
        .map(|line| format!("  {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(inspect(&shifted).unwrap().diagnostics.is_empty());
    let earlier_damaged =
        format!("    + Exception Group Traceback (most recent call last):\n{text}");
    refused(&earlier_damaged);
}
