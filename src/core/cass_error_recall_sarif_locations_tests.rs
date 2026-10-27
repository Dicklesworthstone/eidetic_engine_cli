#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::tests::{decoded, failed, output, passed};
use super::*;
use crate::core::cass_error_recall::{failure_diagnostics, structured_error_diagnostics};
use serde_json::json;

fn location(value: &mut Value) -> &mut Value {
    &mut value["runs"][0]["results"][0]["locations"][0]["physicalLocation"]["artifactLocation"]
}

fn indexed(mut value: Value) -> Value {
    value["runs"][0]["artifacts"] = json!([{"location": {"uri": "src/cache.rs"}}]);
    *location(&mut value) = json!({"index": 0});
    value
}

fn based(mut value: Value) -> Value {
    value["runs"][0]["originalUriBaseIds"] = json!({
        "ROOT": {"uri": "file:///repo/"},
        "SRC": {"uri": "src/", "uriBaseId": "ROOT"}
    });
    *location(&mut value) = json!({"uri": "cache.rs", "uriBaseId": "SRC"});
    value
}

fn absolute(mut value: Value) -> Value {
    *location(&mut value) = json!({"uri": "file:///repo/src/cache.rs"});
    value
}

/// Refusing an unresolved proof must not erase a useful diagnostic. Exercise
/// both polarities instead of accepting an empty decoder as a successful fix.
fn assert_unresolved(mut value: Value) {
    let pass = decoded(&value);
    assert!(!pass.output.allows_completion, "{value}");
    value["runs"][0]["results"][0]["kind"] = json!("fail");
    value["runs"][0]["results"][0]["level"] = json!("error");
    let failure = decoded(&value);
    assert_eq!(failure.output.diagnostics.len(), 1, "{value}");
    assert!(!failure.execution.has_known_failures(), "{value}");
    assert!(!failure.execution.verified_by(&pass.execution), "{value}");
}

#[test]
fn indexed_findings_and_literal_findings_share_actual_repair_coverage() {
    use super::super::{Proof, Requirement};

    for (failure, pass) in [
        (indexed(failed()), passed()),
        (failed(), indexed(passed())),
        (indexed(failed()), indexed(passed())),
    ] {
        let expected = decoded(&failed()).output.diagnostics;
        assert_eq!(decoded(&failure).output.diagnostics, expected);
        assert_eq!(failure_diagnostics(&output(&failure, Some(1))), expected);
        assert_eq!(structured_error_diagnostics(&failure.to_string()), expected);
        let required = Requirement::capture(&output(&failure, Some(1)), true);
        assert!(required.is_guarded());
        assert!(required.is_verified_by(&Proof::capture(&output(&pass, Some(0)))));
        assert!(!required.is_verified_by(&Proof::capture(&output(&pass, None))));
    }
}

#[test]
fn uri_and_index_must_agree_and_dangling_indices_cannot_fall_back() {
    let required = decoded(&failed()).execution;
    let mut agreeing = indexed(passed());
    *location(&mut agreeing) = json!({"uri": "src/cache.rs", "index": 0});
    assert!(required.verified_by(&decoded(&agreeing).execution));
    for artifact_location in [
        json!({"uri": "other/cache.rs", "index": 0}),
        json!({"uri": "src/cache.rs", "index": 99}),
        json!({"uri": "src/cache.rs", "index": null}),
        json!({"uri": "src/cache.rs", "index": -2}),
        json!({"uri": null, "index": 0}),
        json!({"uriBaseId": null, "index": 0}),
        json!({"uri": "src/cache.rs", "uriBaseId": "MISSING", "index": 0}),
    ] {
        let mut value = indexed(passed());
        *location(&mut value) = artifact_location;
        assert_unresolved(value);
    }
    let mut unspecified = passed();
    location(&mut unspecified)["index"] = json!(-1);
    assert!(required.verified_by(&decoded(&unspecified).execution));
}

#[test]
fn an_artifacts_own_index_may_repeat_its_position_but_not_redirect() {
    let required = decoded(&failed()).execution;
    let mut value = indexed(passed());
    value["runs"][0]["artifacts"][0]["location"]["index"] = json!(0);
    assert!(required.verified_by(&decoded(&value).execution));
    for index in [json!(1), json!(null), json!(-2)] {
        let mut invalid = value.clone();
        invalid["runs"][0]["artifacts"][0]["location"]["index"] = index;
        assert_unresolved(invalid);
    }
    let mut absent = value;
    absent["runs"][0]["artifacts"][0]
        .as_object_mut()
        .unwrap()
        .remove("location");
    assert_unresolved(absent);
}

#[test]
fn archive_members_and_offset_artifacts_are_not_top_level_files() {
    for extra in [
        json!({"parentIndex": 1}),
        json!({"parentIndex": null}),
        json!({"offset": 0}),
    ] {
        let mut value = indexed(passed());
        value["runs"][0]["artifacts"][0]
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        assert_unresolved(value.clone());
        *location(&mut value) = json!({"uri": "src/cache.rs"});
        assert_unresolved(value);
    }
    let mut top_level = indexed(passed());
    top_level["runs"][0]["artifacts"][0]["parentIndex"] = json!(-1);
    assert!(
        decoded(&failed())
            .execution
            .verified_by(&decoded(&top_level).execution)
    );
}

#[test]
fn chained_uri_bases_and_expanded_uris_name_the_same_subject() {
    for (failure, pass) in [
        (based(failed()), absolute(passed())),
        (absolute(failed()), based(passed())),
        (based(failed()), based(passed())),
    ] {
        let required = decoded(&failure);
        assert!(required.execution.has_known_failures());
        assert!(required.execution.verified_by(&decoded(&pass).execution));
    }
    let required = decoded(&based(failed())).execution;
    let mut renamed = based(passed());
    renamed["runs"][0]["originalUriBaseIds"] = json!({
        "SOURCE": {"uri": "src/", "uriBaseId": "%NEW_ROOT%"},
        "%NEW_ROOT%": {"uri": "file:///repo/"}
    });
    location(&mut renamed)["uriBaseId"] = json!("SOURCE");
    assert!(required.verified_by(&decoded(&renamed).execution));
    let mut reordered = based(passed());
    reordered["runs"][0]["originalUriBaseIds"] = serde_json::from_str(
        r#"{"SRC":{"uriBaseId":"ROOT","uri":"src/"},"ROOT":{"uri":"file:///repo/"}}"#,
    )
    .unwrap();
    assert!(required.verified_by(&decoded(&reordered).execution));
}

#[test]
fn changed_root_prefix_or_artifact_is_still_a_run_but_not_the_same_repair() {
    let required = decoded(&based(failed())).execution;
    for (pointer, value) in [
        ("/runs/0/originalUriBaseIds/ROOT/uri", "file:///other-repo/"),
        ("/runs/0/originalUriBaseIds/SRC/uri", "different-src/"),
        (
            "/runs/0/results/0/locations/0/physicalLocation/artifactLocation/uri",
            "other.rs",
        ),
    ] {
        let mut changed = based(passed());
        *changed.pointer_mut(pointer).unwrap() = json!(value);
        let pass = decoded(&changed);
        assert!(pass.output.allows_completion, "{pointer}");
        assert!(!required.verified_by(&pass.execution), "{pointer}");
    }
}

#[test]
fn finding_and_execution_locations_use_the_same_reference_resolver() {
    use super::super::{Proof, Requirement};

    let mut failure = absolute(failed());
    failure["runs"][0]["invocations"][0]["workingDirectory"] = json!({"uri": "file:///repo/"});
    failure["runs"][0]["invocations"][0]["executableLocation"] =
        json!({"uri": "file:///tools/analyzer"});
    let mut pass = based(passed());
    pass["runs"][0]["artifacts"] = json!([
        {"location": {"uri": "file:///repo/", "index": 0}},
        {"location": {"uri": "file:///tools/analyzer", "index": 1}},
        {"location": {"uri": "cache.rs", "uriBaseId": "SRC", "index": 2}}
    ]);
    pass["runs"][0]["invocations"][0]["workingDirectory"] = json!({"index": 0});
    pass["runs"][0]["invocations"][0]["executableLocation"] = json!({"index": 1});
    *location(&mut pass) = json!({"index": 2});
    let required = Requirement::capture(&output(&failure, Some(1)), true);
    assert!(required.is_verified_by(&Proof::capture(&output(&pass, Some(0)))));
    pass["runs"][0]["artifacts"][0]["location"]["uri"] = json!("file:///different-root/");
    assert!(output(&pass, Some(0)).succeeded());
    assert!(!required.is_verified_by(&Proof::capture(&output(&pass, Some(0)))));
}

#[test]
fn missing_cyclic_and_unsupported_bases_preserve_diagnostics_without_proof() {
    for bases in [
        json!({}),
        json!({"SRC": {"uri": "src/", "uriBaseId": "MISSING"}}),
        json!({"SRC": {"uri": "src/", "uriBaseId": "ROOT"}, "ROOT": {}}),
        json!({"SRC": {"uri": "src/", "uriBaseId": "SRC"}}),
        json!({"SRC": {"uri": "src/", "uriBaseId": "ROOT"}, "ROOT": {"uri": "repo/", "uriBaseId": "SRC"}}),
    ] {
        let mut value = based(passed());
        value["runs"][0]["originalUriBaseIds"] = bases;
        assert_unresolved(value);
    }
    for uri in [
        "src",
        "../",
        "%2e%2E/",
        "./",
        "src//",
        "src/?query",
        "src/#fragment",
        "src/%ZZ/",
    ] {
        let mut value = based(passed());
        value["runs"][0]["originalUriBaseIds"]["SRC"]["uri"] = json!(uri);
        assert_unresolved(value);
    }
    for artifact_location in [
        json!({"uri": "sarif:/runs/0/artifacts/0"}),
        json!({"uri": "file:///repo/src/cache.rs", "uriBaseId": "ROOT"}),
        json!({"uri": "src/cache.rs", "uriBaseId": null}),
    ] {
        let mut value = based(passed());
        *location(&mut value) = artifact_location;
        assert_unresolved(value);
    }
}

fn chained(depth: usize) -> Value {
    let mut value = passed();
    let mut bases = serde_json::Map::new();
    for index in 0..depth {
        bases.insert(
            format!("B{index}"),
            if index + 1 == depth {
                json!({"uri": "file:///repo/"})
            } else {
                json!({"uri": "dir/", "uriBaseId": format!("B{}", index + 1)})
            },
        );
    }
    value["runs"][0]["originalUriBaseIds"] = Value::Object(bases);
    *location(&mut value) = json!({"uri": "cache.rs", "uriBaseId": "B0"});
    value
}

#[test]
fn reference_depth_uri_length_and_inventory_all_have_nonvacuous_bounds() {
    assert!(decoded(&chained(16)).output.allows_completion);
    assert_unresolved(chained(17));
    let mut long = absolute(passed());
    location(&mut long)["uri"] = json!(format!("file:///{}", "x".repeat(8192 - 8)));
    assert!(decoded(&long).output.allows_completion);
    location(&mut long)["uri"] = json!(format!("file:///{}", "x".repeat(8193 - 8)));
    assert_unresolved(long);
    let mut small = indexed(passed());
    small["runs"][0]["artifacts"] = json!(vec![json!({"location": {"uri": "src/cache.rs"}}); 8]);
    assert!(decoded(&small).output.allows_completion);
    small["runs"][0]["artifacts"] = json!(vec![
        json!({"location": {"uri": "src/cache.rs"}});
        MAX_RECORDS
    ]);
    assert!(decode(&small.to_string()).is_err());
}

#[test]
fn original_bytes_reject_duplicate_base_keys_and_location_discriminators() {
    let valid = based(passed()).to_string();
    let duplicate = valid.replacen(
        "\"ROOT\":{",
        "\"R\\u004fOT\":{\"uri\":\"file:///other/\"},\"ROOT\":{",
        1,
    );
    assert_ne!(duplicate, valid);
    assert!(decode(&duplicate).is_err());
    let valid = indexed(passed()).to_string();
    let duplicate = valid.replacen("\"index\":0", "\"index\":99,\"index\":0", 1);
    assert_ne!(duplicate, valid);
    assert!(decode(&duplicate).is_err());
}
