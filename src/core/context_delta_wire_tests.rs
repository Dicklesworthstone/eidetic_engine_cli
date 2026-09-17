use super::*;
use crate::core::context_delta::{
    ContextDeltaOptions, ContextDeltaPackSnapshot, compute_context_delta,
};
use serde_json::json;

fn raw(items: &str) -> String {
    format!(
        r#"{{"schema":"ee.context.delta.v2","success":true,"data":{{"priorPackHash":"prior","newPackHash":"next","baseDbGeneration":7,"newDbGeneration":8,"items":{items},"tokenSavings":{{"fullBytes":1000000,"deltaBytes":500,"savedBytes":999500,"savedPercent":99.95,"netPackTokens":100}},"serverDecision":{{"computedFromServerVerifiedPackRecord":false,"deltaChained":false,"format":"json"}}}},"degraded":[]}}"#
    )
}

fn no_op_json() -> String {
    raw(r#"{"added":[],"removed":[],"modified":[]}"#)
}

fn prior() -> ContextDeltaPackSnapshot {
    ContextDeltaPackSnapshot::new(
        "prior",
        7,
        1_000_000,
        100,
        vec![ContextDeltaItemSnapshot::new("mem_a").with_field("content", json!("old"))],
    )
}

fn decode(value: &JsonValue) -> Result<ContextDeltaEnvelope, ContextDeltaError> {
    ContextDeltaEnvelope::from_json_slice(&serde_json::to_vec(value).expect("fixture JSON"))
}

#[test]
fn kernel_output_decodes_and_applies_without_a_server_roundtrip() {
    let prior = prior();
    let mut next = ContextDeltaPackSnapshot::new(
        "next",
        8,
        1_000_000,
        90,
        vec![
            ContextDeltaItemSnapshot::new("ev_b")
                .with_field("content", json!("café\n\"quoted\" \\ evidence"))
                .with_field("source", json!({"uri": "cass-session://session#L2-4"})),
            ContextDeltaItemSnapshot::new("mem_a").with_field("content", json!("updated")),
        ],
    );
    next.items[0].fields.insert("optional".into(), JsonValue::Null);
    let generated = compute_context_delta(&prior, &next, ContextDeltaOptions::new(None))
        .expect("generated delta");
    let encoded = serde_json::to_vec(&generated).expect("encode delta");
    let decoded = ContextDeltaEnvelope::from_json_slice(&encoded).expect("decode delta");
    assert_eq!(decoded, generated);
    assert_eq!(decoded.apply_to_snapshot(&prior).expect("apply delta"), next);
    assert_eq!(prior, self::prior(), "baseline was not mutated");
}

#[test]
fn standard_serde_decoding_owns_input_without_requiring_a_static_buffer() {
    let decoded: ContextDeltaEnvelope = {
        let input = no_op_json();
        serde_json::from_slice(input.as_bytes()).expect("ordinary serde decoding")
    };
    assert_eq!(decoded.schema, CONTEXT_DELTA_SCHEMA_V2);
    assert_eq!(decoded.data.server_decision.format, "json");
    assert_eq!(decoded.data.prior_pack_hash, "prior");
    let roundtrip = serde_json::to_vec(&decoded).expect("re-encode");
    assert_eq!(
        ContextDeltaEnvelope::from_json_slice(&roundtrip).expect("decode again"),
        decoded
    );
}

#[test]
fn optional_metadata_and_all_degradation_severities_survive_decoding() {
    let mut value: JsonValue = serde_json::from_str(&no_op_json()).expect("fixture");
    value["data"]["workspaceId"] = json!("workspace-a");
    value["data"]["priorFeatureFlagSetHash"] = json!("flags-a");
    value["data"]["newFeatureFlagSetHash"] = json!("flags-a");
    value["data"]["trace"] = json!({"stages": ["read", null, 7], "mode": "local"});
    value["degraded"] = json!(["info", "low", "warning", "medium", "high", "critical"])
        .as_array()
        .expect("severity array")
        .iter()
        .map(|severity| {
            json!({"code":"example", "severity":severity, "message":"explanation",
                "repair":"retry", "details":{"source":"bounded", "count":2}})
        })
        .collect();
    let decoded = decode(&value).expect("metadata");
    assert_eq!(decoded.data.workspace_id.as_deref(), Some("workspace-a"));
    assert_eq!(decoded.data.trace.as_ref().unwrap()["stages"], json!(["read", null, 7]));
    assert_eq!(decoded.degraded.len(), 6);
    assert_eq!(decoded.degraded[5].severity, "critical");
    assert_eq!(decoded.degraded[0].details.as_ref().unwrap()["count"], 2);
    assert_eq!(serde_json::to_value(&decoded).expect("encode metadata"), value);
}

#[test]
fn rejects_unsupported_schema_success_chaining_and_protocol_vocabulary() {
    for (pointer, invalid) in [
        ("/schema", json!("ee.context.delta.v999")),
        ("/success", json!(false)),
        ("/data/serverDecision/deltaChained", json!(true)),
        ("/data/serverDecision/format", json!("html")),
        ("/data/baseDbGeneration", json!(-1)),
        ("/data/tokenSavings/fullBytes", json!(-1)),
    ] {
        let mut value: JsonValue = serde_json::from_str(&no_op_json()).unwrap();
        *value.pointer_mut(pointer).expect("known protocol field") = invalid;
        assert!(decode(&value).is_err(), "accepted invalid {pointer}");
    }
    let mut value: JsonValue = serde_json::from_str(&no_op_json()).unwrap();
    value["degraded"] = json!([{"code":"x", "severity":"unknown", "message":"x"}]);
    assert!(decode(&value).is_err());
}

#[test]
fn closed_protocol_objects_do_not_silently_drop_future_operations() {
    for pointer in ["", "/data", "/data/items", "/data/serverDecision", "/data/tokenSavings"] {
        let mut value: JsonValue = serde_json::from_str(&no_op_json()).unwrap();
        value.pointer_mut(pointer).unwrap().as_object_mut().unwrap()
            .insert("unrecognizedOperation".into(), json!({"doNotIgnore":true}));
        assert!(decode(&value).is_err(), "accepted extension at {pointer}");
    }
    for items in [
        r#"{"added":[{"id":"mem_a","content":"flattened instead of fields"}],"removed":[],"modified":[]}"#,
        r#"{"added":[{"id":"mem_a","fields":{},"other":1}],"removed":[],"modified":[]}"#,
        r#"{"added":[],"removed":[],"modified":[{"id":"mem_a","fieldChanges":{},"other":1}]}"#,
    ] {
        assert!(ContextDeltaEnvelope::from_json_slice(raw(items).as_bytes()).is_err());
    }
}

#[test]
fn rejects_duplicate_structural_keys_and_duplicate_field_edits() {
    let duplicate_top =
        no_op_json().replace("\"success\":true", "\"success\":true,\"success\":true");
    assert!(ContextDeltaEnvelope::from_json_slice(duplicate_top.as_bytes()).is_err());
    for items in [
        r#"{"added":[{"id":"mem_b","id":"mem_c","fields":{}}],"removed":[],"modified":[]}"#,
        r#"{"added":[{"id":"mem_b","fields":{"content":"one","content":"two"}}],"removed":[],"modified":[]}"#,
        r#"{"added":[],"removed":[],"modified":[{"id":"mem_a","fieldChanges":{"content":["old","one"],"content":["old","two"]}}]}"#,
    ] {
        assert!(ContextDeltaEnvelope::from_json_slice(raw(items).as_bytes()).is_err());
    }
}

#[test]
fn rejects_malformed_pairs_and_ambiguous_redaction_objects() {
    for change in [
        r#"["old"]"#,
        r#"["old","new","extra"]"#,
        r#"{"newValue":"safe","oldValueOmitted":false,"reason":"redaction_drift"}"#,
        r#"{"newValue":"safe","oldValueOmitted":true,"reason":"unknown"}"#,
        r#"{"newValue":"safe","oldValueOmitted":true,"reason":"redaction_drift","oldValue":"private"}"#,
        r#"{"newValue":"safe","oldValueOmitted":true,"oldValueOmitted":true,"reason":"redaction_drift"}"#,
    ] {
        let items = format!(
            r#"{{"added":[],"removed":[],"modified":[{{"id":"mem_a","fieldChanges":{{"content":{change}}}}}]}}"#
        );
        assert!(ContextDeltaEnvelope::from_json_slice(raw(&items).as_bytes()).is_err());
    }
}

#[test]
fn redacted_changes_apply_without_recovering_or_emitting_old_content() {
    let input = raw(
        r#"{"added":[],"removed":[],"modified":[{"id":"mem_a","fieldChanges":{"content":{"newValue":"[REDACTED]","oldValueOmitted":true,"reason":"policy_restricted"}}}]}"#,
    );
    let mut prior = prior();
    prior.items[0].fields.insert("content".into(), json!("private-old-body"));
    let decoded = ContextDeltaEnvelope::from_json_slice(input.as_bytes()).unwrap();
    let applied = decoded.apply_to_snapshot(&prior).expect("one-way update");
    assert_eq!(applied.items[0].fields["content"], "[REDACTED]");
    assert_eq!(prior.items[0].fields["content"], "private-old-body");
    assert!(!serde_json::to_string(&decoded).unwrap().contains("private-old-body"));
}

#[test]
fn fallback_and_markdown_can_be_inspected_but_not_applied() {
    for reason in [
        "prior_unknown", "delta_larger_than_full", "redaction_drift",
        "compute_budget_exceeded", "envelope_oversized", "prior_corrupted",
        "format_unsupported", "feature_flag_drift",
    ] {
        let mut value: JsonValue = serde_json::from_str(&no_op_json()).unwrap();
        value["data"]["serverDecision"]["fallbackReason"] = json!(reason);
        let decoded = decode(&value).expect("recognized fallback envelope");
        assert!(!decoded.emits_delta());
        assert!(decoded.apply_to_snapshot(&prior()).is_err());
    }
    let markdown = no_op_json().replace("\"format\":\"json\"", "\"format\":\"markdown\"");
    let decoded = ContextDeltaEnvelope::from_json_slice(markdown.as_bytes()).unwrap();
    assert_eq!(decoded.data.server_decision.format, "markdown");
    assert!(decoded.apply_to_snapshot(&prior()).is_err());
}

#[test]
fn exact_input_limit_includes_utf8_and_trailing_newline() {
    let input = raw(r#"{"added":[{"id":"ev_b","fields":{"content":"café"}}],"removed":[],"modified":[]}"#) + "\n";
    assert!(
        ContextDeltaEnvelope::from_json_slice_with_limit(input.as_bytes(), input.len()).is_ok()
    );
    assert!(
        ContextDeltaEnvelope::from_json_slice_with_limit(input.as_bytes(), input.len() - 1)
            .is_err()
    );
    assert!(ContextDeltaEnvelope::from_json_slice_with_limit(input.as_bytes(), 0).is_err());
    assert!(ContextDeltaEnvelope::from_json_slice(b"").is_err());
}

#[test]
fn rejects_trailing_documents_invalid_utf8_and_excessive_nesting() {
    assert!(ContextDeltaEnvelope::from_json_slice((no_op_json() + "{}").as_bytes()).is_err());
    assert!(ContextDeltaEnvelope::from_json_slice(&[0xff, 0xfe]).is_err());
    let nested = format!("{}0{}", "[".repeat(150), "]".repeat(150));
    let items = format!(r#"{{"added":[{{"id":"ev_b","fields":{{"nested":{nested}}}}}],"removed":[],"modified":[]}}"#);
    assert!(ContextDeltaEnvelope::from_json_slice(raw(&items).as_bytes()).is_err());
}

#[test]
fn diagnostics_never_echo_private_scalars_keys_or_enum_values() {
    for input in [
        no_op_json().replace("\"success\":true", "\"success\":\"private-scalar\""),
        no_op_json().replace("\"format\":\"json\"", "\"format\":\"private-format\""),
        no_op_json().replace("\"schema\":", "\"private-key\":0,\"schema\":"),
    ] {
        let bounded = ContextDeltaEnvelope::from_json_slice(input.as_bytes())
            .unwrap_err()
            .to_string();
        let standard = serde_json::from_slice::<ContextDeltaEnvelope>(input.as_bytes())
            .unwrap_err()
            .to_string();
        assert!(!bounded.contains("private-"));
        assert!(!standard.contains("private-"));
    }
}

#[test]
fn decoded_stale_or_invalid_edits_never_partially_mutate_the_baseline() {
    for items in [
        r#"{"added":[],"removed":[],"modified":[{"id":"mem_a","fieldChanges":{"content":["wrong","new"]}}]}"#,
        r#"{"added":[{"id":"mem_b","fields":{}},{"id":"mem_b","fields":{}}],"removed":[],"modified":[]}"#,
        r#"{"added":[],"removed":["absent"],"modified":[]}"#,
    ] {
        let decoded =
            ContextDeltaEnvelope::from_json_slice(raw(items).as_bytes()).expect("wire shape");
        let baseline = prior();
        let before = baseline.clone();
        assert!(decoded.apply_to_snapshot(&baseline).is_err());
        assert_eq!(baseline, before);
    }
}

#[test]
fn arbitrary_json_numbers_arrays_and_objects_remain_field_data() {
    let value: JsonValue = serde_json::from_str(
        r#"{"integer":18446744073709551615,"decimal":123456789012345678901234567890.123456789,"array":[null,true,-12.25],"object":{"reason":"not-a-protocol-enum"}}"#,
    ).unwrap();
    let items = json!({
        "added": [{"id": "ev_b", "fields": {"payload": value}}],
        "removed": [],
        "modified": [],
    });
    let decoded =
        ContextDeltaEnvelope::from_json_slice(raw(&items.to_string()).as_bytes()).unwrap();
    assert_eq!(decoded.data.items.added[0].fields["payload"], value);
    let encoded = serde_json::to_vec(&decoded).unwrap();
    let again = ContextDeltaEnvelope::from_json_slice(&encoded).unwrap();
    assert_eq!(again.data.items.added[0].fields["payload"], value);
}

#[test]
fn decoded_server_claim_does_not_authorize_the_reconstructed_snapshot() {
    let input = no_op_json().replace(
        "\"computedFromServerVerifiedPackRecord\":false",
        "\"computedFromServerVerifiedPackRecord\":true",
    );
    let decoded = ContextDeltaEnvelope::from_json_slice(input.as_bytes()).unwrap();
    assert!(decoded.data.server_decision.computed_from_server_verified_pack_record);
    let next = decoded.apply_to_snapshot(&prior()).unwrap();
    let followup = compute_context_delta(&next, &next, ContextDeltaOptions::new(None)).unwrap();
    assert!(!followup.data.server_decision.computed_from_server_verified_pack_record);
}
