//! Execution coverage for the ordinary `cargo test` / libtest pretty formatter.
//!
//! A complete start, named terminal results and reconciled summary are needed.
//! Dots, summaries without identities, benchmarks and damaged output abstain.
//! Only the private proof projection changes: stored transcript bytes, hashes
//! and source locators remain the authority. The existing structured validator
//! checks counters, duplicate names, suite inventories and failed-test coverage.

use serde_json::{Value, json};

use super::{MAX_SCANNED_OUTPUT_BYTES, ToolOutput};

fn append(events: &mut String, value: Value) -> Result<(), ()> {
    let line = serde_json::to_string(&value).map_err(|_| ())?;
    let separator = usize::from(!events.is_empty());
    if events
        .len()
        .saturating_add(separator)
        .saturating_add(line.len())
        > MAX_SCANNED_OUTPUT_BYTES
    {
        return Err(());
    }
    if separator != 0 {
        events.push('\n');
    }
    events.push_str(&line);
    Ok(())
}

fn count(value: &str) -> Result<u64, ()> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(());
    }
    value.parse().map_err(|_| ())
}

fn summary(line: &str) -> Result<Value, ()> {
    let (status, rest) = line
        .strip_prefix("test result: ")
        .and_then(|body| body.split_once(". "))
        .ok_or(())?;
    let event = match status {
        "ok" => "ok",
        "FAILED" => "failed",
        _ => return Err(()),
    };
    let fields: Vec<_> = rest.split(';').map(str::trim).collect();
    if fields.len() != 6 {
        return Err(());
    }
    let mut counters = [0_u64; 5];
    for ((field, suffix), value) in fields
        .iter()
        .zip([
            " passed",
            " failed",
            " ignored",
            " measured",
            " filtered out",
        ])
        .zip(&mut counters)
    {
        *value = count(field.strip_suffix(suffix).ok_or(())?)?;
    }
    let elapsed = fields[5]
        .strip_prefix("finished in ")
        .and_then(|value| value.strip_suffix('s'))
        .ok_or(())?;
    if elapsed.is_empty()
        || !elapsed
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.')
        || !elapsed
            .parse::<f64>()
            .is_ok_and(|time| time.is_finite() && time >= 0.0)
    {
        return Err(());
    }
    Ok(json!({"type":"suite", "event":event,
        "passed":counters[0], "failed":counters[1], "ignored":counters[2],
        "measured":counters[3], "filtered_out":counters[4]}))
}

/// None is not a plain libtest report. Err is recognized but unprovable input
/// and must never fall through to an unguarded generic command success.
pub(super) fn project(output: &ToolOutput) -> Result<Option<ToolOutput>, ()> {
    if output.text.len() > MAX_SCANNED_OUTPUT_BYTES {
        return Err(());
    }
    let visible = super::super::outcome::terminal_text(&output.text)?;
    let text = visible.as_ref();
    if text.trim_start().starts_with(['{', '[']) {
        return Ok(None);
    }
    if matches!(visible, std::borrow::Cow::Owned(_))
        && crate::policy::screen_external_text_for_ingestion(text).instruction_like
    {
        return Err(());
    }
    // Only presentation changes. Names, counters, suite boundaries and the
    // original process status still pass the existing coverage validator.
    let mut events = String::new();
    let mut active = false;
    let mut seen = false;
    for line in text.lines().map(str::trim) {
        if let Some(tests) = line.strip_prefix("running ").and_then(|tail| {
            tail.strip_suffix(" tests")
                .or_else(|| tail.strip_suffix(" test"))
        }) {
            if active {
                return Err(());
            }
            append(
                &mut events,
                json!({"type":"suite", "event":"started", "test_count":count(tests)?}),
            )?;
            active = true;
            seen = true;
        } else if line.starts_with("test result:") {
            if !active {
                return Err(());
            }
            append(&mut events, summary(line)?)?;
            active = false;
        } else if let Some(result) = line.strip_prefix("test ") {
            let Some((name, result)) = result.rsplit_once(" ... ") else {
                // This formatter can report a slow running test before its
                // eventual terminal row. The notification is not a result.
                if active && line.ends_with(" has been running for over 60 seconds") {
                    continue;
                }
                return Err(());
            };
            if !active || name.is_empty() || name.chars().any(char::is_control) {
                return Err(());
            }
            // Pretty-format decoration is not part of the libtest identity.
            let name = name.strip_suffix(" - should panic").unwrap_or(name);
            let event = match result {
                "ok" => "ok",
                "FAILED" => "failed",
                "ignored" => "ignored",
                reason if reason.starts_with("ignored, ") => "ignored",
                _ => return Err(()),
            };
            append(
                &mut events,
                json!({"type":"test", "event":"started", "name":name}),
            )?;
            append(
                &mut events,
                json!({"type":"test", "event":event, "name":name}),
            )?;
        }
    }
    if active {
        return Err(());
    }
    Ok(seen.then(|| ToolOutput {
        text: events,
        is_error: output.is_error,
        exit_code: output.exit_code,
    }))
}

#[cfg(test)]
mod tests {
    use super::super::{Proof, Requirement};
    use super::*;

    fn report(results: &[(&str, &str)]) -> String {
        let mut text = format!("running {} tests\n", results.len());
        for (name, result) in results {
            text.push_str(&format!("test {name} ... {result}\n"));
        }
        let n = |value| {
            results
                .iter()
                .filter(|(_, result)| *result == value)
                .count()
        };
        let failed = n("FAILED");
        text.push_str(&format!(
            "\ntest result: {}. {} passed; {failed} failed; {} ignored; 0 measured; 0 filtered out; finished in 0.01s\n",
            if failed == 0 { "ok" } else { "FAILED" }, n("ok"), n("ignored"),
        ));
        text
    }

    fn output(text: String) -> ToolOutput {
        ToolOutput {
            text,
            is_error: Some(false),
            exit_code: Some(0),
        }
    }

    #[test]
    fn styled_libtest_runs_keep_exact_coverage_and_original_process_authority() {
        let failed = output(report(&[
            ("cache::round_trip", "FAILED"),
            ("control", "ok"),
        ]));
        let passed = output(report(&[("control", "ok"), ("cache::round_trip", "ok")]));
        let decorate = |text: &str| format!("\u{1b}[32m{text}\u{1b}[0m");
        let styled_failed = output(decorate(&failed.text));
        let styled_passed = output(decorate(&passed.text));
        for failure in [&failed, &styled_failed] {
            let required = Requirement::capture(failure, true);
            assert!(required.is_guarded());
            for proof in [&passed, &styled_passed] {
                assert!(required.is_verified_by(&Proof::capture(proof)));
                assert_eq!(
                    project(proof).unwrap().unwrap().text,
                    project(&passed).unwrap().unwrap().text
                );
            }
            let no_status = ToolOutput {
                is_error: None,
                exit_code: None,
                ..styled_passed.clone()
            };
            assert!(!required.is_verified_by(&Proof::capture(&no_status)));
            let nonzero = ToolOutput {
                exit_code: Some(101),
                ..styled_passed.clone()
            };
            assert!(!required.is_verified_by(&Proof::capture(&nonzero)));
        }
        assert_eq!(styled_failed.text, decorate(&failed.text));
        assert_eq!(styled_passed.text, decorate(&passed.text));
    }

    #[test]
    fn styling_cannot_replace_skip_or_drop_required_libtest_subjects() {
        let failed = output(report(&[
            ("cache::round_trip", "FAILED"),
            ("control", "ok"),
        ]));
        let required = Requirement::capture(&failed, true);
        for results in [
            vec![("cache::round_trip", "ignored"), ("control", "ok")],
            vec![("Cache::round_trip", "ok"), ("control", "ok")],
            vec![("cache::round_trip", "ok"), ("replacement", "ok")],
            vec![("control", "ok")],
            vec![],
        ] {
            let plain = report(&results);
            for text in [
                format!("\u{1b}[32m{plain}\u{1b}[0m"),
                format!("\u{1b}]8;;https://example.test/report\u{7}{plain}\u{1b}]8;;\u{7}"),
            ] {
                let proof = output(text);
                assert!(proof.succeeded(), "process success is insufficient");
                assert!(
                    !required.is_verified_by(&Proof::capture(&proof)),
                    "{results:?}"
                );
            }
        }
    }

    #[test]
    fn colored_partial_and_oversized_coverage_never_downgrades_to_command_equality() {
        let failed = output(report(&[("case", "FAILED")]));
        let passed = output(report(&[("case", "ok")]));
        let proof = Proof::capture(&passed);
        let mut required = Requirement::capture(&failed, true);
        for text in [
            "\u{1b}[31mrunning 1 test\ntest case ... FAILED\u{1b}[0m".to_owned(),
            format!("\u{1b}[31m{}\u{1b}[", failed.text),
            format!("\u{1b}[31m{}\u{1b}[2K", failed.text),
        ] {
            required.refresh(&output(text), true);
            assert!(required.is_guarded());
            assert!(!required.is_verified_by(&proof));
        }
        required.refresh(&output(format!("\u{1b}[31m{}\u{1b}[0m", failed.text)), true);
        assert!(required.is_verified_by(&proof));
        let huge = output(format!(
            "{}{}",
            "\u{1b}[0m".repeat(MAX_SCANNED_OUTPUT_BYTES / 4 + 1),
            passed.text
        ));
        assert!(project(&huge).is_err());
        assert!(!required.is_verified_by(&Proof::capture(&huge)));
    }

    #[test]
    fn plain_failed_tests_must_really_pass_in_the_same_inventory() {
        let failed = output(report(&[("cache::round_trip", "FAILED"), ("other", "ok")]));
        let requirement = Requirement::capture(&failed, true);
        assert!(requirement.is_guarded());
        for results in [
            vec![("cache::round_trip", "ignored"), ("other", "ok")],
            vec![("other", "ok")],
            vec![("Cache::round_trip", "ok"), ("other", "ok")],
            vec![("cache::round_trip", "ok"), ("replacement", "ok")],
            vec![],
        ] {
            let proof = output(report(&results));
            assert!(proof.succeeded(), "process success alone would accept this");
            assert!(!requirement.is_verified_by(&Proof::capture(&proof)));
        }
        let passed = output(report(&[("other", "ok"), ("cache::round_trip", "ok")]));
        assert!(requirement.is_verified_by(&Proof::capture(&passed)));
    }

    #[test]
    fn plain_and_json_runs_share_exact_test_identity_without_rewriting_sources() {
        let failed = output(report(&[("cache::round_trip", "FAILED")]));
        let passed = output(report(&[("cache::round_trip", "ok")]));
        let original = failed.text.clone();
        let json_failed = project(&failed).unwrap().unwrap();
        let json_passed = project(&passed).unwrap().unwrap();
        for failure in [&failed, &json_failed] {
            let required = Requirement::capture(failure, true);
            for proof in [&passed, &json_passed] {
                assert!(required.is_verified_by(&Proof::capture(proof)));
            }
        }
        assert_eq!(failed.text, original);
        assert_eq!(json_failed.exit_code, failed.exit_code);
    }

    #[test]
    fn summaries_partial_runs_duplicates_and_counter_laundering_abstain() {
        let failed = output(report(&[("case", "FAILED")]));
        let passed = output(report(&[("case", "ok")]));
        let required = Requirement::capture(&failed, true);
        let proof = Proof::capture(&passed);
        let complete = passed.text.clone();
        for damaged in [
            complete.lines().skip(1).collect::<Vec<_>>().join("\n"),
            complete.lines().take(2).collect::<Vec<_>>().join("\n"),
            complete.replace("1 tests", "2 tests"),
            complete.replace("1 passed", "2 passed"),
            complete.replace("0 failed", "1 failed"),
            complete.replace("0 measured", "1 measured"),
            complete.replace("1 tests", "many tests"),
            complete.replace("0.01s", "NaNs"),
            complete.replace("test case ... ok", "test case ... ok\ntest case ... ok"),
            format!("{complete}\n{complete}"),
        ] {
            let damaged = output(damaged);
            assert!(!required.is_verified_by(&Proof::capture(&damaged)));
            assert!(!Requirement::capture(&damaged, true).is_verified_by(&proof));
        }
        assert!(!Requirement::capture(&failed, false).is_verified_by(&proof));
    }

    #[test]
    fn original_process_vetoes_survive_projection_and_refresh() {
        let failed = output(report(&[("case", "FAILED")]));
        let passed = output(report(&[("case", "ok")]));
        let mut required = Requirement::capture(&failed, false);
        let proof = Proof::capture(&passed);
        assert!(!required.is_verified_by(&proof));
        required.refresh(&failed, true);
        assert!(required.is_verified_by(&proof));
        for tail in [
            "Exit code 1",
            "Warning: output truncated",
            "Process running with session ID 42",
        ] {
            let partial = output(format!("{}\n{tail}", passed.text));
            assert!(!required.is_verified_by(&Proof::capture(&partial)));
        }
        let mut nonzero = passed;
        nonzero.exit_code = Some(101);
        assert!(!required.is_verified_by(&Proof::capture(&nonzero)));
        required.refresh(
            &output("running 2 tests\ntest case ... FAILED".into()),
            false,
        );
        required.refresh(&output("unrelated output".into()), true);
        assert!(!required.is_verified_by(&proof));
    }

    #[test]
    fn multiple_suites_use_inventories_not_completion_order() {
        let failed = output(format!(
            "{}\n{}",
            report(&[("a", "FAILED")]),
            report(&[("b", "FAILED")])
        ));
        let passed = output(format!(
            "{}\n{}",
            report(&[("b", "ok")]),
            report(&[("a", "ok")])
        ));
        let required = Requirement::capture(&failed, true);
        assert!(required.is_verified_by(&Proof::capture(&passed)));
        assert!(!required.is_verified_by(&Proof::capture(&output(report(&[("a", "ok")])))));
    }

    #[test]
    fn unrelated_outputs_are_unchanged_and_projection_is_bounded() {
        for text in ["Finished dev profile", "", "{\"success\":true}"] {
            assert!(project(&output(text.into())).unwrap().is_none());
        }
        assert!(project(&output("x".repeat(MAX_SCANNED_OUTPUT_BYTES + 1))).is_err());
        let name = "x".repeat(MAX_SCANNED_OUTPUT_BYTES / 2);
        let raw = output(report(&[(&name, "FAILED")]));
        assert!(raw.text.len() < MAX_SCANNED_OUTPUT_BYTES);
        assert!(
            project(&raw).is_err(),
            "the derived allocation has its own byte cap"
        );
    }
}
