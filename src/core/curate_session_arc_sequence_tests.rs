use super::*;

fn session() -> StoredSession {
    StoredSession {
        id: "ses_resource0000000000000000000".into(),
        workspace_id: "wsp_resource0000000000000000000".into(),
        cass_session_id: "ordinary-repair-prose".into(),
        source_path: None,
        agent_name: None,
        model: None,
        started_at: None,
        ended_at: None,
        message_count: 0,
        token_count: None,
        content_hash: "blake3:resource-session".into(),
        metadata_json: None,
        imported_at: "2026-09-22T00:00:00Z".into(),
        updated_at: "2026-09-22T00:00:00Z".into(),
    }
}

pub(super) fn span(id: &str, line: u32, text: &str) -> StoredEvidenceSpan {
    let hash = format!("blake3:{}", blake3::hash(text.as_bytes()).to_hex());
    StoredEvidenceSpan {
        id: id.into(),
        workspace_id: session().workspace_id,
        session_id: session().id,
        memory_id: None,
        cass_span_id: format!("cass-{id}"),
        span_kind: "message".into(),
        start_line: line,
        end_line: line,
        start_byte: None,
        end_byte: None,
        role: Some("assistant".into()),
        excerpt: text.into(),
        reader_projection: None,
        content_hash: hash.clone(),
        metadata_json: None,
        producer_kind: "cass_import".into(),
        screening_version: 1,
        secret_redaction_status: "clean".into(),
        redaction_classes_json: "[]".into(),
        instruction_risk: "none".into(),
        search_eligibility: "admitted".into(),
        pack_eligibility: "admitted".into(),
        canonical_provenance_revision: 1,
        canonical_excerpt_hash: Some(hash),
        security_policy_epoch: 1,
        upstream_ref_hash: Some(format!("blake3:cass-{id}")),
        created_at: "2026-09-22T00:00:00Z".into(),
        updated_at: "2026-09-22T00:00:00Z".into(),
    }
}

pub(super) fn mine(spans: &[StoredEvidenceSpan]) -> Vec<ReviewSessionCandidate> {
    let session = session();
    candidates(&session.workspace_id, &session, spans)
}

pub(super) fn endpoints(rows: &[ReviewSessionCandidate]) -> Vec<(&str, &str)> {
    rows.iter()
        .filter(|row| row.candidate_kind == "session_arc_rule")
        .map(|row| {
            let arc = row.session_arc.as_ref().expect("source-backed arc");
            (
                arc.failure_span.evidence_span_id.as_str(),
                arc.resolution_span.evidence_span_id.as_str(),
            )
        })
        .collect()
}

#[test]
fn session_arc_zero_failure_summary_cannot_mint_an_episode() {
    let report = "cargo test src/api.rs passed (21 passed, 0 failed).";
    let repair = "Fixed src/api.rs by restoring the expected type.";
    assert!(mine(&[span("clean", 1, report), span("later", 2, repair)]).is_empty());
    assert!(mine(&[span("combined", 1, &format!("{report}\n{repair}"))]).is_empty());
}

#[test]
fn session_arc_counted_success_closes_a_real_failure_with_original_provenance() {
    let failure = "cargo test src/api.rs failed.";
    let repair = "cargo test src/api.rs passed (21 passed, 0 failed).";
    let spans = [span("failure", 1, failure), span("repair", 2, repair)];
    let rows = mine(&spans);
    assert_eq!(rows.len(), 2);
    assert_eq!(endpoints(&rows), [("failure", "repair")]);
    for row in &rows {
        let arc = row.session_arc.as_ref().expect("source-backed arc");
        assert_eq!(arc.failure_span.content_hash, spans[0].content_hash);
        assert_eq!(arc.resolution_span.content_hash, spans[1].content_hash);
        assert_eq!(arc.resolution_span.excerpt, repair);
        assert!(row.proposed_content.contains("0 failed"));
    }
    let combined = span("combined", 1, &format!("{failure}\n{repair}"));
    let inline = mine(std::slice::from_ref(&combined));
    assert_eq!(inline.len(), 2);
    assert_eq!(endpoints(&inline), [("combined", "combined")]);
    for row in &inline {
        let arc = row.session_arc.as_ref().unwrap();
        assert_eq!(arc.failure_span.content_hash, combined.content_hash);
        assert_eq!(arc.resolution_span.content_hash, combined.content_hash);
        assert_eq!(arc.resolution_span.excerpt, repair);
    }
}

#[test]
fn session_arc_mixed_test_results_and_zero_runs_do_not_close_a_failure() {
    for report in [
        "cargo test src/api.rs: 21 passed, 1 failed.",
        "cargo test src/api.rs passed (errors: 1).",
        "cargo test src/api.rs: 0 passed, 0 failed.",
    ] {
        let failure = "cargo test src/api.rs failed.";
        assert!(
            mine(&[span("failure", 1, failure), span("report", 2, report)]).is_empty(),
            "{report}"
        );
    }
}

#[test]
fn session_arc_nonzero_exit_is_a_failed_attempt_with_original_provenance() {
    let failure = "cargo test src/api.rs exited with code 101.";
    let repair = "Fixed src/api.rs by restoring the guard and cargo test passed, exit_code=0.";
    let spans = [span("failure", 1, failure), span("repair", 2, repair)];
    let rows = mine(&spans);
    assert_eq!(rows.len(), 2);
    assert_eq!(endpoints(&rows), [("failure", "repair")]);
    for row in &rows {
        let arc = row.session_arc.as_ref().unwrap();
        assert_eq!(arc.failure_span.excerpt, failure);
        assert_eq!(arc.resolution_span.excerpt, repair);
        assert_eq!(arc.failure_span.content_hash, spans[0].content_hash);
        assert_eq!(arc.resolution_span.content_hash, spans[1].content_hash);
    }
    let combined = span("combined", 1, &format!("{failure}\n{repair}"));
    let inline = mine(std::slice::from_ref(&combined));
    assert_eq!(inline.len(), 2);
    for row in inline {
        let arc = row.session_arc.unwrap();
        assert_eq!(arc.failure_span.content_hash, combined.content_hash);
        assert_eq!(arc.resolution_span.content_hash, combined.content_hash);
    }
}

#[test]
fn session_arc_process_trailers_cannot_turn_a_failed_retry_into_a_repair() {
    let failure = "cargo test src/api.rs failed.";
    let claim = "Fixed src/api.rs and cargo test passed (21 passed, 0 failed).";
    for trailer in [
        "Process exited with code 101.",
        "exit_code=unknown",
        "Process exited with code 0.\nProcess exited with code 1.",
    ] {
        let repair = format!("{claim}\n{trailer}");
        let sources = [span("failure", 1, failure), span("retry", 2, &repair)];
        assert!(mine(&sources).is_empty(), "cross-window: {repair}");
        let body = format!("{failure}\n{repair}");
        assert!(
            mine(&[span("combined", 1, &body)]).is_empty(),
            "inline: {body}"
        );
    }
    let good = format!("{claim}\nProcess exited with code 0.");
    assert_eq!(
        mine(&[span("failure", 1, failure), span("repair", 2, &good)]).len(),
        2
    );
    assert_eq!(
        mine(&[span("combined", 1, &format!("{failure}\n{good}"))]).len(),
        2
    );
}

#[test]
fn session_arc_later_independent_process_failure_keeps_an_earlier_lesson() {
    let failure = "cargo test src/api.rs failed.";
    let repair = "Fixed src/api.rs by restoring the guard.";
    let body =
        format!("{failure}\n{repair}\nThe lint command for src/ui.rs exited with code 1.");
    let rows = mine(&[span("combined", 1, &body)]);
    assert_eq!(rows.len(), 2);
    for row in rows {
        let arc = row.session_arc.unwrap();
        assert_eq!(arc.failure_span.excerpt, failure);
        assert_eq!(arc.resolution_span.excerpt, repair);
    }
}

#[test]
fn session_arc_ordinary_repairs_do_not_need_repeated_subsystem_words() {
    for (failure, repair) in [
        (
            "The build failed in `src/cache.rs`.",
            "Fixed `src/cache.rs` by replacing the unstable key.",
        ),
        (
            "A regression broke `Cache::get()`.",
            "`Cache::get()` was repaired by preserving identity bytes.",
        ),
        (
            "The database migration failed in migrations/0085.sql.",
            "Guarding null inputs in migrations/0085.sql resolved the issue.",
        ),
        (
            "The search cache failed in src/ranker.rs.",
            "Fixed src/ranker.rs by clearing the obsolete generation.",
        ),
        (
            "The agent helper failed in src/owner.rs.",
            "Repaired src/owner.rs by retaining the lock descriptor.",
        ),
        (
            "The transcript parser failed in src/reader.rs.",
            "Fixed src/reader.rs by preserving message order.",
        ),
        (
            "The import failed while calling Store::insert().",
            "Store::insert() was fixed by preserving the original source hash.",
        ),
        (
            "The worker failed while opening Cargo.lock.",
            "We fixed it by restoring the pinned dependency versions.",
        ),
    ] {
        let spans = [span("failure", 10, failure), span("repair", 11, repair)];
        let rows = mine(&spans);
        assert_eq!(rows.len(), 2, "{failure} / {repair}");
        assert_eq!(endpoints(&rows), [("failure", "repair")]);
        for row in &rows {
            let arc = row.session_arc.as_ref().unwrap();
            assert_eq!(arc.failure_span.content_hash, spans[0].content_hash);
            assert_eq!(arc.resolution_span.content_hash, spans[1].content_hash);
            assert!(row.proposed_content.contains(failure));
            assert!(row.proposed_content.contains(repair));
        }
        let reversed = [spans[1].clone(), spans[0].clone()];
        assert_eq!(mine(&reversed), rows);

        // The same lesson must survive when CASS puts both observations
        // in one message window rather than two separate spans.
        let body = format!("{failure}\n{repair}");
        let combined = span("combined", 10, &body);
        let inline = mine(std::slice::from_ref(&combined));
        assert_eq!(inline.len(), 2, "single window: {body}");
        assert_eq!(endpoints(&inline), [("combined", "combined")]);
        for row in &inline {
            let arc = row.session_arc.as_ref().unwrap();
            assert_eq!(row.source_ids, std::slice::from_ref(&combined.id));
            assert_eq!(arc.failure_span.excerpt, failure);
            assert_eq!(arc.resolution_span.excerpt, repair);
            assert_eq!(arc.failure_span.content_hash, combined.content_hash);
            assert_eq!(arc.resolution_span.content_hash, combined.content_hash);
        }
    }
}

#[test]
fn session_arc_interleaved_failures_share_a_topic_not_a_resource() {
    let spans = [
        span("failure-a", 1, "cargo test src/api.rs failed."),
        span("failure-b", 2, "cargo test src/ui.rs failed."),
        span(
            "repair-a",
            3,
            "Fixed src/api.rs by adding the missing guard.",
        ),
        span(
            "repair-b",
            4,
            "Fixed src/ui.rs by retaining the expected state.",
        ),
    ];
    let rows = mine(&spans);
    assert_eq!(
        endpoints(&rows),
        [("failure-a", "repair-a"), ("failure-b", "repair-b")]
    );
    for row in &rows {
        let sources: Vec<_> = spans
            .iter()
            .filter(|source| row.source_ids.contains(&source.id))
            .cloned()
            .collect();
        assert!(
            mine(&sources).contains(row),
            "application must reconstruct identical source-bound candidates"
        );
    }
    let mut reversed = spans.to_vec();
    reversed.reverse();
    assert_eq!(mine(&reversed), rows);
}

#[test]
fn session_arc_explicit_repairs_follow_named_subjects_not_nearest_markers() {
    let observations = [
        "Failure arc: cargo test src/api.rs failed.",
        "Failure arc: cargo test src/ui.rs failed.",
        "Fix: cargo test src/api.rs passed after adding the missing guard.",
        "Fix: cargo test src/ui.rs passed after retaining the expected state.",
    ];
    // Exercise both historical plain evidence and real-shaped transcript
    // envelopes. The body chooses the subject; the original span remains
    // the source of the hash, locator, and application reconstruction.
    for structured in [false, true] {
        let spans: Vec<_> = observations
            .iter()
            .enumerate()
            .map(|(index, text)| {
                let excerpt = if structured {
                    serde_json::json!({"type":"assistant","message":{
                        "role":"assistant","content":[{"type":"text","text":text}]
                    }})
                    .to_string()
                } else {
                    (*text).to_owned()
                };
                let line = u32::try_from(index).expect("bounded fixture") + 1;
                span(&format!("observation-{index}"), line, &excerpt)
            })
            .collect();
        let rows = mine(&spans);
        assert_eq!(rows.len(), 4);
        assert_eq!(
            endpoints(&rows),
            [
                ("observation-0", "observation-2"),
                ("observation-1", "observation-3")
            ]
        );
        for row in &rows {
            let sources: Vec<_> = spans
                .iter()
                .filter(|source| row.source_ids.contains(&source.id))
                .cloned()
                .collect();
            assert!(mine(&sources).contains(row));
            let arc = row.session_arc.as_ref().unwrap();
            for source in &sources {
                let hash = if source.id == arc.failure_span.evidence_span_id {
                    &arc.failure_span.content_hash
                } else {
                    &arc.resolution_span.content_hash
                };
                assert_eq!(hash, &source.content_hash);
            }
        }
        let mut reversed = spans.clone();
        reversed.reverse();
        assert_eq!(mine(&reversed), rows);

        let body = observations.join("\n");
        let excerpt = if structured {
            serde_json::json!({"type":"assistant","content":body}).to_string()
        } else {
            body
        };
        let combined = span("combined", 1, &excerpt);
        let inline = mine(std::slice::from_ref(&combined));
        let rules: Vec<_> = inline
            .iter()
            .filter(|row| row.candidate_kind == "session_arc_rule")
            .collect();
        assert_eq!(inline.len(), 4);
        for (index, rule) in rules.iter().enumerate() {
            let arc = rule.session_arc.as_ref().unwrap();
            assert_eq!(arc.failure_span.excerpt, observations[index]);
            assert_eq!(arc.resolution_span.excerpt, observations[index + 2]);
            assert_eq!(arc.failure_span.content_hash, combined.content_hash);
            assert_eq!(arc.resolution_span.content_hash, combined.content_hash);
        }
    }
}

#[test]
fn session_arc_fix_marker_cannot_resolve_an_ambiguous_named_resource() {
    let observations = [
        "Failure arc: cargo fmt src/main.rs failed.",
        "Failure arc: cargo clippy src/main.rs failed.",
        "Fix: src/main.rs was repaired by changing its imports.",
    ];
    let mut spans: Vec<_> = observations
        .iter()
        .enumerate()
        .map(|(index, text)| {
            let line = u32::try_from(index).expect("bounded fixture") + 1;
            span(&format!("source-{index}"), line, text)
        })
        .collect();
    assert!(mine(&spans).is_empty());
    assert!(mine(&[span("combined", 1, &observations.join("\n"))]).is_empty());

    // Rejecting the ambiguous repair must not consume either failure.
    let repair = "Fix: cargo fmt src/main.rs passed after normalizing its imports.";
    spans.push(span("format-repair", 4, repair));
    assert_eq!(endpoints(&mine(&spans)), [("source-0", "format-repair")]);
    let body = format!("{}\n{repair}", observations.join("\n"));
    let inline = mine(&[span("combined", 1, &body)]);
    assert_eq!(inline.len(), 2);
    for row in inline {
        let arc = row.session_arc.unwrap();
        assert_eq!(arc.failure_span.excerpt, observations[0]);
        assert_eq!(arc.resolution_span.excerpt, repair);
    }
}

#[test]
fn session_arc_shared_topic_cannot_override_different_concrete_resources() {
    for (failure, repair) in [
        (
            "cargo test src/api.rs failed.",
            "cargo test src/ui.rs passed.",
        ),
        (
            "cargo test src/Cache.rs failed.",
            "cargo test src/cache.rs passed.",
        ),
        ("cargo test src/api.rs failed.", "cargo test api.rs passed."),
        (
            "cargo test Store::open() failed.",
            "cargo test OtherStore::open() passed.",
        ),
    ] {
        assert!(
            mine(&[span("failure", 1, failure), span("repair", 2, repair)]).is_empty(),
            "{failure} / {repair}"
        );
        assert!(
            mine(&[span("combined", 1, &format!("{failure} {repair}"))]).is_empty(),
            "same-topic different-resource clauses must not pair: {failure} / {repair}"
        );
    }
}

#[test]
fn session_arc_resource_ambiguity_requires_a_disambiguating_topic() {
    let mut spans = vec![
        span("format-failed", 1, "cargo fmt src/main.rs failed."),
        span("lint-failed", 2, "cargo clippy src/main.rs failed."),
        span("ambiguous", 3, "Fixed src/main.rs by changing its imports."),
    ];
    assert!(
        mine(&spans).is_empty(),
        "shared file alone does not identify the failed operation"
    );
    spans.push(span("format-fixed", 4, "cargo fmt src/main.rs passed."));
    assert_eq!(
        endpoints(&mine(&spans)),
        [("format-failed", "format-fixed")]
    );
}

#[test]
fn session_arc_pronoun_repairs_require_one_adjacent_unresolved_failure() {
    let failure = span("failure", 1, "The worker failed while opening Cargo.lock.");
    let repair = "We fixed it by restoring the pinned dependency versions.";
    assert_eq!(mine(&[failure.clone(), span("repair", 2, repair)]).len(), 2);
    assert!(mine(&[failure.clone(), span("repair", 3, repair)]).is_empty());
    assert!(
        mine(&[
            failure.clone(),
            span("another-failure", 2, "cargo clippy failed."),
            span("repair", 3, repair)
        ])
        .is_empty()
    );
    for non_repair in [
        "It works now.",
        "Fixed the unrelated database writes.",
        "We will fix it by restoring the pinned versions.",
        "We fixed it by restoring the pinned versions but the retry failed.",
        "We fixed it by replacing Another.lock.",
    ] {
        assert!(
            mine(&[failure.clone(), span("repair", 2, non_repair)]).is_empty(),
            "{non_repair}"
        );
    }
}

#[test]
fn session_arc_resources_come_from_decoded_bodies_not_json_metadata() {
    let failure = serde_json::json!({"type":"assistant", "content":"The build failed.",
        "metadata":{"file":"src/cache.rs"}})
    .to_string();
    let repair = serde_json::json!({"type":"assistant", "content":"Fixed src/cache.rs by changing the key."}).to_string();
    assert!(mine(&[span("failure", 1, &failure), span("repair", 2, &repair)]).is_empty());
    let failure =
        serde_json::json!({"type":"assistant", "content":"The build failed in src/cache.rs.",
        "metadata":{"file":"src/unrelated.rs"}})
        .to_string();
    let spans = [span("failure", 1, &failure), span("repair", 2, &repair)];
    let rows = mine(&spans);
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0]
            .session_arc
            .as_ref()
            .unwrap()
            .failure_span
            .content_hash,
        spans[0].content_hash
    );
    assert!(
        rows.iter()
            .all(|row| !row.proposed_content.contains("src/unrelated.rs"))
    );
}

#[test]
fn session_arc_resource_spelling_is_not_a_basename_or_expression_alias() {
    assert_eq!(
        resource_keys("`src/api.rs`, Store::open() Cargo.lock (migrations/001.sql)."),
        BTreeSet::from([
            "src/api.rs".into(),
            "Store::open".into(),
            "Cargo.lock".into(),
            "migrations/001.sql".into()
        ])
    );
    for text in [
        "cache policy fix",
        "https://host/api.rs",
        "${ROOT}/api.rs",
        "src/*.rs",
        "src/a?.rs",
        "accept/reject",
    ] {
        assert!(resource_keys(text).is_empty(), "{text}");
    }
    assert_ne!(resource_keys("src/api.rs"), resource_keys("api.rs"));
    assert_ne!(resource_keys("src/Cache.rs"), resource_keys("src/cache.rs"));
}

#[test]
fn session_arc_policy_windows_keep_observed_risk_repair_and_both_sources() {
    let failure = "Failure arc: storing silently would violate the no-loop-takeover policy.";
    let repair = "Fix: require accept/reject commands and audit every accepted capture.";
    let sources = [
        span("policy-failure", 3, failure),
        span("policy-repair", 4, repair),
    ];
    let rows = mine(&sources);
    assert_eq!(rows.len(), 2);
    for row in &rows {
        let specificity = crate::curate::specificity_score(&row.proposed_content);
        assert!(
            specificity.passes_threshold,
            "{specificity:?}: {}",
            row.proposed_content
        );
        assert!(specificity.structural_signals.has_provenance_uri);
        assert!(row.proposed_content.contains(&format!("Risk: {failure}")));
        assert!(
            row.proposed_content
                .contains(&format!("Mitigation: {repair}"))
        );
        for source in &sources {
            assert!(
                row.proposed_content
                    .contains(&source.canonical_provenance_uri())
            );
        }
        assert!(
            !row.proposed_content.contains("cargo"),
            "never invent a command to pass specificity"
        );
        let arc = row.session_arc.as_ref().unwrap();
        assert_eq!(arc.failure_span.content_hash, sources[0].content_hash);
        assert_eq!(arc.resolution_span.content_hash, sources[1].content_hash);
    }
}

#[test]
fn session_arc_already_specific_two_window_content_keeps_its_identity() {
    let failure = "cargo test failed because the cache key was stale.";
    let repair = "Fixed the cache key and cargo test passed.";
    let sources = [span("failure", 1, failure), span("repair", 2, repair)];
    let expected = [
        format!(
            "Anti-pattern for `testing`: this session hit a failure after `{failure}`. The later fix was `{repair}`."
        ),
        format!(
            "Rule for `testing`: when `{failure}` appears, apply the later repair: `{repair}`."
        ),
    ];
    let rows = mine(&sources);
    assert_eq!(rows.len(), 2);
    for (row, expected) in rows.iter().zip(expected) {
        assert!(crate::curate::specificity_score(&expected).passes_threshold);
        assert_eq!(row.proposed_content, expected);
        assert_eq!(
            row.content_hash,
            format!("blake3:{}", blake3::hash(expected.as_bytes()).to_hex())
        );
    }
}

#[test]
fn session_arc_diagnostic_locations_keep_exact_file_identity() {
    for (location, path) in [
        ("src/api.rs:12", "src/api.rs"),
        ("`src/api.rs:12:9`", "src/api.rs"),
        ("(src/api.rs:12:0).", "src/api.rs"),
        ("src/api.rs:12:9:", "src/api.rs"),
        ("src/api.rs(12,9)", "src/api.rs"),
        ("src/api.rs(12)", "src/api.rs"),
        (r"C:\repo\src\api.rs:12:9", r"C:\repo\src\api.rs"),
        (r"C:\repo\src\api.rs(12,9)", r"C:\repo\src\api.rs"),
        ("Cargo.lock:00012", "Cargo.lock"),
    ] {
        assert_eq!(
            resource_keys(location),
            BTreeSet::from([path.to_owned()]),
            "{location}"
        );
    }
    assert_eq!(
        resource_keys("Store::open()"),
        BTreeSet::from(["Store::open".to_owned()])
    );
}

#[test]
fn session_arc_malformed_diagnostic_locations_never_become_file_aliases() {
    for location in [
        "src/api.rs:unknown",
        "src/api.rs:0",
        "src/api.rs:0:9",
        "src/api.rs:12:-9",
        "src/api.rs:12:+9",
        "src/api.rs:12:9:4",
        "src/api.rs:4294967296",
        "src/api.rs:12:4294967296",
        "src/api.rs(0,9)",
        "src/api.rs(12,-9)",
        "src/api.rs(12,9,4)",
        "src/api.rs(12,9",
        "src/api.rs:12/9",
        "src/*.rs:12:9",
        "https://host/src/api.rs:12:9",
        "${ROOT}/src/api.rs:12:9",
    ] {
        assert!(resource_keys(location).is_empty(), "{location}");
    }
    assert_ne!(resource_keys("src/api.rs:12"), resource_keys("api.rs:12"));
    assert_ne!(
        resource_keys("src/Api.rs:12"),
        resource_keys("src/api.rs:12")
    );
}

#[test]
fn session_arc_diagnostic_repairs_keep_original_positions_and_hashes() {
    for (failure, repair) in [
        (
            "The build failed in src/cache.rs:42:9.",
            "Fixed src/cache.rs by replacing the unstable key.",
        ),
        (
            "The database migration failed in migrations/0085.sql:17.",
            "Guarding null inputs in migrations/0085.sql:23 resolved the issue.",
        ),
        (
            r"The build failed in C:\repo\src\cache.rs(42,9).",
            r"Fixed C:\repo\src\cache.rs(46,2) by replacing the unstable key.",
        ),
    ] {
        let sources = [span("failure", 10, failure), span("repair", 11, repair)];
        let rows = mine(&sources);
        assert_eq!(rows.len(), 2, "{failure} / {repair}");
        assert_eq!(endpoints(&rows), [("failure", "repair")]);
        for row in &rows {
            let arc = row.session_arc.as_ref().unwrap();
            assert_eq!(arc.failure_span.excerpt, failure);
            assert_eq!(arc.resolution_span.excerpt, repair);
            assert_eq!(arc.failure_span.content_hash, sources[0].content_hash);
            assert_eq!(arc.resolution_span.content_hash, sources[1].content_hash);
        }
        let combined = span("combined", 10, &format!("{failure}\n{repair}"));
        let inline = mine(std::slice::from_ref(&combined));
        assert_eq!(inline.len(), 2);
        for row in inline {
            let arc = row.session_arc.unwrap();
            assert_eq!(arc.failure_span.excerpt, failure);
            assert_eq!(arc.resolution_span.excerpt, repair);
            assert_eq!(arc.failure_span.content_hash, combined.content_hash);
            assert_eq!(arc.resolution_span.content_hash, combined.content_hash);
        }
    }
}

#[test]
fn session_arc_diagnostic_subjects_cannot_collapse_to_one_testing_topic() {
    for (failed, repaired) in [
        ("src/api.rs:12:9", "src/ui.rs:12:9"),
        ("src/Api.rs:12", "src/api.rs:14"),
        ("src/api.rs(12,9)", "api.rs(14,2)"),
        (r"C:\one\api.rs:12:9", r"C:\two\api.rs:14:2"),
    ] {
        let failure = format!("cargo test {failed} failed.");
        let repair = format!("cargo test {repaired} passed.");
        assert!(mine(&[span("failure", 1, &failure), span("repair", 2, &repair)]).is_empty());
        assert!(mine(&[span("combined", 1, &format!("{failure}\n{repair}"))]).is_empty());
        let actual_repair = format!("cargo test {failed} passed.");
        assert_eq!(
            mine(&[
                span("failure", 1, &failure),
                span("repair", 2, &actual_repair)
            ])
            .len(),
            2
        );
    }
}

#[test]
fn session_arc_interleaved_diagnostic_failures_keep_their_own_repairs() {
    let sources = [
        span("failure-a", 1, "cargo test src/api.rs:12:9 failed."),
        span("failure-b", 2, "cargo test src/ui.rs:23:4 failed."),
        span(
            "repair-a",
            3,
            "Fixed src/api.rs:16:2 by restoring the guard.",
        ),
        span(
            "repair-b",
            4,
            "Fixed src/ui.rs:28:0 by preserving the state.",
        ),
    ];
    let rows = mine(&sources);
    assert_eq!(rows.len(), 4);
    assert_eq!(
        endpoints(&rows),
        [("failure-a", "repair-a"), ("failure-b", "repair-b")]
    );
    let mut reversed = sources.to_vec();
    reversed.reverse();
    assert_eq!(mine(&reversed), rows);
    for row in &rows {
        let pair: Vec<_> = sources
            .iter()
            .filter(|source| row.source_ids.contains(&source.id))
            .cloned()
            .collect();
        assert!(mine(&pair).contains(row));
    }
}

#[test]
fn session_arc_decoded_diagnostic_locations_do_not_rewrite_cass_evidence() {
    let failure_text = "The build failed in src/cache.rs:42:9.";
    let repair_text = "Fixed src/cache.rs:46:2 by replacing the unstable key.";
    let failure = serde_json::json!({"type":"assistant","message":{
        "role":"assistant","content":[{"type":"text","text":failure_text}]
    },"metadata":{"file":"src/unrelated.rs:1:1"}})
    .to_string();
    let repair = serde_json::json!({"type":"assistant","message":{
        "role":"assistant","content":[{"type":"text","text":repair_text}]
    }})
    .to_string();
    let sources = [span("failure", 1, &failure), span("repair", 2, &repair)];
    let before = sources.clone();
    let rows = mine(&sources);
    assert_eq!(rows.len(), 2);
    assert_eq!(sources, before);
    for row in rows {
        assert!(!row.proposed_content.contains("src/unrelated.rs"));
        let arc = row.session_arc.unwrap();
        assert_eq!(arc.failure_span.excerpt, failure_text);
        assert_eq!(arc.resolution_span.excerpt, repair_text);
        assert_eq!(arc.failure_span.content_hash, sources[0].content_hash);
        assert_eq!(arc.resolution_span.content_hash, sources[1].content_hash);
    }
}

#[test]
fn session_arc_msvc_diagnostic_punctuation_keeps_the_named_file() {
    for (location, path) in [
        ("src/api.rs(12):", "src/api.rs"),
        ("src/api.rs(12,0):", "src/api.rs"),
        ("`src/api.rs(12,9):`", "src/api.rs"),
        (r"C:\repo\src\api.rs(12,9):", r"C:\repo\src\api.rs"),
    ] {
        assert_eq!(
            resource_keys(location),
            BTreeSet::from([path.to_owned()]),
            "{location}"
        );
    }
    for location in [
        "src/api.rs(0,9):",
        "src/api.rs(12,unknown):",
        "src/api.rs(12,9,4):",
        "src/api.rs(12,9)::",
        "https://host/src/api.rs(12,9):",
    ] {
        assert!(resource_keys(location).is_empty(), "{location}");
    }
    let failure = "cargo test src/api.rs(12,9): failed.";
    let wrong_repair = "cargo test src/ui.rs(16,2): passed.";
    let right_repair = "cargo test src/api.rs(16,2): passed.";
    assert!(mine(&[span("failure", 1, failure), span("wrong", 2, wrong_repair)]).is_empty());
    let rows = mine(&[span("failure", 1, failure), span("repair", 2, right_repair)]);
    assert_eq!(rows.len(), 2);
    assert_eq!(endpoints(&rows), [("failure", "repair")]);
    for row in rows {
        let arc = row.session_arc.unwrap();
        assert_eq!(arc.failure_span.excerpt, failure);
        assert_eq!(arc.resolution_span.excerpt, right_repair);
    }
}

pub(super) fn marked_span(
    id: &str,
    line: u32,
    body: &str,
    structured: bool,
) -> StoredEvidenceSpan {
    let excerpt = if structured {
        serde_json::json!({
            "type": "assistant",
            "message": {
                "role": "assistant",
                "content": [{"type": "text", "text": body}]
            },
            // A coincidentally matching metadata path is not a subject.
            "metadata": {"file": "src/api.rs"}
        })
        .to_string()
    } else {
        body.to_owned()
    };
    span(id, line, &excerpt)
}

#[test]
fn session_arc_explicit_markers_cannot_bridge_disjoint_sources() {
    for (failed, repaired) in [
        ("src/api.rs", "src/ui.rs"),
        ("src/Api.rs", "src/api.rs"),
        ("src/api.rs", "api.rs"),
        ("Store::open()", "OtherStore::open()"),
        ("src/api.rs:12:9", "src/ui.rs:14:2"),
        (r"C:\one\api.rs(12,9):", r"C:\two\api.rs(14,2):"),
    ] {
        let failure = format!("Failure arc: cargo test {failed} failed.");
        let repair = format!("Fix: cargo test {repaired} passed after restoring the guard.");
        assert!(failure_signal(&failure));
        assert!(resolution_signal(&repair));
        let left = resource_keys(&failure);
        let right = resource_keys(&repair);
        assert!(!left.is_empty() && !right.is_empty());
        assert!(left.is_disjoint(&right));
        for structured in [false, true] {
            let sources = [
                marked_span("failure", 10, &failure, structured),
                marked_span("wrong", 11, &repair, structured),
            ];
            let before = sources.clone();
            assert!(mine(&sources).is_empty(), "{failure} / {repair}");
            assert_eq!(sources, before);
            assert!(mine(&[sources[1].clone(), sources[0].clone()]).is_empty());
            let combined =
                marked_span("combined", 10, &format!("{failure}\n{repair}"), structured);
            assert!(mine(&[combined]).is_empty());
        }
    }
}

#[test]
fn session_arc_rejected_marked_repair_keeps_exact_later_pair() {
    let failure = "Failure arc: cargo test src/api.rs failed.";
    let wrong = "Fix: cargo test src/ui.rs passed after restoring the guard.";
    let right = "Fix: cargo test src/api.rs passed after restoring the guard.";
    for structured in [false, true] {
        let sources = [
            marked_span("failure", 10, failure, structured),
            marked_span("wrong", 11, wrong, structured),
            marked_span("right", 12, right, structured),
        ];
        let before = sources.clone();
        assert!(mine(&sources[..2]).is_empty());
        let rows = mine(&sources);
        assert_eq!(rows.len(), 2);
        assert_eq!(endpoints(&rows), [("failure", "right")]);
        assert_eq!(sources, before);
        let mut reversed = sources.clone();
        reversed.reverse();
        assert_eq!(mine(&reversed), rows);
        let exact_pair = [sources[0].clone(), sources[2].clone()];
        assert_eq!(mine(&exact_pair), rows);
        for row in &rows {
            assert!(!row.source_ids.contains(&sources[1].id));
            let arc = row.session_arc.as_ref().unwrap();
            assert_eq!(arc.failure_span.excerpt, failure);
            assert_eq!(arc.resolution_span.excerpt, right);
            assert_eq!(arc.failure_span.content_hash, sources[0].content_hash);
            assert_eq!(arc.resolution_span.content_hash, sources[2].content_hash);
        }
    }
}

#[test]
fn session_arc_contradictory_marker_cannot_choose_older_unanchored_failure() {
    let older = "Failure arc: cargo test failed.";
    let current = "Failure arc: cargo test src/api.rs failed.";
    let wrong = "Fix: cargo test src/ui.rs passed after restoring the guard.";
    let right = "Fix: cargo test src/api.rs passed after restoring the guard.";
    for structured in [false, true] {
        let mut sources = vec![
            marked_span("older", 1, older, structured),
            marked_span("current", 2, current, structured),
            marked_span("wrong", 3, wrong, structured),
        ];
        assert!(mine(&sources).is_empty());
        sources.push(marked_span("right", 4, right, structured));
        let rows = mine(&sources);
        assert_eq!(rows.len(), 2);
        assert_eq!(endpoints(&rows), [("current", "right")]);
        for row in rows {
            assert!(!row.source_ids.contains(&sources[0].id));
            assert!(!row.source_ids.contains(&sources[2].id));
        }
    }
}

#[test]
fn session_arc_markers_preserve_valid_partial_and_matching_subjects() {
    for (failure, repair) in [
        (
            "Failure arc: cargo test src/api.rs:12:9 failed.",
            "Fix: cargo test src/api.rs:14:2 passed after restoring the guard.",
        ),
        (
            "Failure arc: cargo test src/api.rs failed.",
            "Fix: cargo test passed after restoring the guard.",
        ),
        (
            "Failure arc: cargo test failed.",
            "Fix: cargo test src/api.rs passed after restoring the guard.",
        ),
    ] {
        for structured in [false, true] {
            let sources = [
                marked_span("failure", 1, failure, structured),
                marked_span("repair", 2, repair, structured),
            ];
            let rows = mine(&sources);
            assert_eq!(rows.len(), 2, "{failure} / {repair}");
            assert_eq!(endpoints(&rows), [("failure", "repair")]);
            for row in rows {
                let arc = row.session_arc.unwrap();
                assert_eq!(arc.failure_span.content_hash, sources[0].content_hash);
                assert_eq!(arc.resolution_span.content_hash, sources[1].content_hash);
                assert_eq!(arc.failure_span.excerpt, failure);
                assert_eq!(arc.resolution_span.excerpt, repair);
            }
        }
    }
}

const WINDOW_FAILURE: &str = "cargo test src/api.rs failed.";
const WINDOW_REPAIR: &str = "Fixed src/api.rs by restoring the guard.";

#[test]
fn session_arc_next_window_results_veto_optimistic_repairs() {
    for report in [
        "1 failed.",
        "errors: unknown.",
        "test result: FAILED.\n21 passed; 1 failed.",
        "Process exited with code 101.",
        "Process exited with code -9.",
        "Process exited with code 0.5.",
        "exit_code=unknown",
    ] {
        for structured in [false, true] {
            let sources = [
                marked_span("failure", 1, WINDOW_FAILURE, structured),
                marked_span("claim", 2, WINDOW_REPAIR, structured),
                marked_span("report", 3, report, structured),
            ];
            assert_eq!(mine(&sources[..2]).len(), 2, "nonvacuous positive control");
            assert!(mine(&sources).is_empty(), "{report}");
            let mut reversed = sources.clone();
            reversed.reverse();
            assert!(mine(&reversed).is_empty(), "source order, not caller order");
        }
    }
}

#[test]
fn session_arc_result_runs_propagate_failure_across_successful_windows() {
    for structured in [false, true] {
        let sources = [
            marked_span("failure", 1, WINDOW_FAILURE, structured),
            marked_span("claim", 2, WINDOW_REPAIR, structured),
            marked_span("counts", 3, "21 passed; 0 failed.", structured),
            marked_span("elapsed", 4, "finished in 0.03s.", structured),
            marked_span("exit", 5, "Process exited with code 101.", structured),
            marked_span("clean", 6, "Process exited with code 0.", structured),
        ];
        let original = sources.clone();
        assert!(mine(&sources).is_empty());
        assert_eq!(sources, original);
        let grouped: Vec<_> = observation_groups(sources.iter()).collect();
        assert_eq!(grouped.len(), 2);
        assert_eq!(grouped[1].0.id, "claim");
        assert!(grouped[1].2);
        assert_eq!(grouped[1].1.as_deref(), Some(WINDOW_REPAIR));
    }
}

#[test]
fn session_arc_clean_result_windows_keep_exact_candidates_and_sources() {
    for structured in [false, true] {
        let original = [
            marked_span("failure", 1, WINDOW_FAILURE, structured),
            marked_span("repair", 2, WINDOW_REPAIR, structured),
        ];
        let expected = mine(&original);
        assert_eq!(expected.len(), 2);
        let mut sources = original.to_vec();
        sources.push(marked_span("counts", 3, "21 passed; 0 failed.", structured));
        sources.push(marked_span(
            "exit",
            4,
            "Process exited with code 0.",
            structured,
        ));
        assert_eq!(mine(&sources), expected);
        for row in &expected {
            let arc = row.session_arc.as_ref().unwrap();
            assert_eq!(arc.failure_span.content_hash, original[0].content_hash);
            assert_eq!(arc.resolution_span.content_hash, original[1].content_hash);
            assert_eq!(arc.failure_span.excerpt, WINDOW_FAILURE);
            assert_eq!(arc.resolution_span.excerpt, WINDOW_REPAIR);
            assert!(!row.source_ids.contains(&sources[2].id));
            assert!(!row.source_ids.contains(&sources[3].id));
        }
    }
}

#[test]
fn session_arc_failed_report_does_not_consume_the_original_failure() {
    let sources = [
        span("failure", 1, WINDOW_FAILURE),
        span("claim", 2, WINDOW_REPAIR),
        span("report", 3, "1 failed."),
        span("actual-repair", 4, WINDOW_REPAIR),
    ];
    let rows = mine(&sources);
    assert_eq!(rows.len(), 2);
    assert_eq!(endpoints(&rows), [("failure", "actual-repair")]);
    assert_eq!(rows, mine(&[sources[0].clone(), sources[3].clone()]));
}

#[test]
fn session_arc_report_windows_do_not_cross_independent_or_unreadable_bodies() {
    for barrier in [
        "The independent command for src/ui.rs started.",
        "1 failed. Another independent command started.",
        "`1 failed`",
        "Example: 1 failed.",
        r#"{"type":"tool_result","content":"1 failed"}"#,
        r#"{"type":"assistant","content":"unfinished"#,
    ] {
        let sources = [
            span("failure", 1, WINDOW_FAILURE),
            span("repair", 2, WINDOW_REPAIR),
            span("barrier", 3, barrier),
            span("report", 4, "1 failed."),
        ];
        assert_eq!(mine(&sources), mine(&sources[..2]), "{barrier}");
    }
    let sources = [
        span("failure", 1, WINDOW_FAILURE),
        span(
            "barrier",
            2,
            r#"{"type":"tool_result","content":"ignored"}"#,
        ),
        span("repair", 3, WINDOW_REPAIR),
    ];
    assert!(
        mine(&sources).is_empty(),
        "projection denial remains a causal barrier"
    );
}

#[test]
fn session_arc_result_windows_require_complete_adjacent_positions() {
    let prefix = [
        span("failure", 1, WINDOW_FAILURE),
        span("repair", 2, WINDOW_REPAIR),
    ];
    let expected = mine(&prefix);
    assert_eq!(expected.len(), 2);
    for (start, end) in [(4, 4), (2, 2), (3, 2)] {
        let mut report = span("report", start, "1 failed.");
        report.end_line = end;
        let sources = [prefix[0].clone(), prefix[1].clone(), report];
        assert_eq!(mine(&sources), expected, "{start}..{end}");
    }
    let sources = [
        span("maximum", u32::MAX, WINDOW_REPAIR),
        span("zero", 0, "1 failed."),
    ];
    let grouped: Vec<_> = observation_groups(sources.iter()).collect();
    assert_eq!(grouped.len(), 2);
    assert!(!grouped[0].2);
}

#[test]
fn session_arc_later_result_window_also_withholds_a_complete_inline_claim() {
    let body = format!("{WINDOW_FAILURE}\n{WINDOW_REPAIR}");
    for structured in [false, true] {
        let sources = [
            marked_span("combined", 10, &body, structured),
            marked_span("report", 11, "1 failed.", structured),
        ];
        assert_eq!(mine(&sources[..1]).len(), 2);
        assert!(mine(&sources).is_empty());
        let clean = [
            sources[0].clone(),
            marked_span("clean", 11, "0 failed.", structured),
        ];
        assert_eq!(mine(&clean), mine(&sources[..1]));

        // An earlier complete window stays independent; a later report
        // does not retrospectively invalidate every lesson in the session.
        let earlier = [
            span("earlier-failure", 1, "cargo test src/ui.rs failed."),
            span(
                "earlier-repair",
                2,
                "Fixed src/ui.rs by restoring the guard.",
            ),
        ];
        let expected = mine(&earlier);
        assert_eq!(expected.len(), 2);
        let all = [
            earlier[0].clone(),
            earlier[1].clone(),
            sources[0].clone(),
            sources[1].clone(),
        ];
        assert_eq!(mine(&all), expected);
    }
}

#[test]
fn session_arc_long_result_runs_decode_each_window_once() {
    let mut sources = vec![span("claim", 1, WINDOW_REPAIR)];
    for index in 0..2_048_u32 {
        sources.push(marked_span(
            &format!("report-{index}"),
            index + 2,
            "21 passed; 0 failed; finished in 0.03s.",
            true,
        ));
    }
    sources.push(span("exit", 2_050, "Process exited with code 101."));
    let before = crate::cass::transcript::projection_calls_for_test();
    let grouped: Vec<_> = observation_groups(sources.iter()).collect();
    let after = crate::cass::transcript::projection_calls_for_test();
    assert_eq!(after - before, sources.len() as u64);
    assert_eq!(grouped.len(), 1);
    assert!(grouped[0].2);
    assert_eq!(grouped[0].1.as_deref(), Some(WINDOW_REPAIR));
}
