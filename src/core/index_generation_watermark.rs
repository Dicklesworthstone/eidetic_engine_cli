//! One unambiguous source watermark for every index admission surface.
//!
//! A complete set of tiers is not sufficient evidence of the source snapshot.
//! Missing watermarks must not become generation zero during writer recovery,
//! and canonical/legacy aliases must not name different snapshots. Do not
//! include untrusted metadata values in the error text.

use serde_json::{Map, Value};

const FIELDS: [&str; 3] = ["sourceGeneration", "source_generation", "generation"];

pub(super) fn parse(object: &Map<String, Value>) -> Result<u64, String> {
    let mut generation = None;
    for field in FIELDS {
        let Some(value) = object.get(field) else {
            continue;
        };
        let value = value.as_u64().ok_or_else(|| {
            format!("index metadata {field} must be an unsigned 64-bit integer; rebuild the index")
        })?;
        if generation.is_some_and(|previous| previous != value) {
            return Err(
                "index metadata has conflicting source generation fields; rebuild the index"
                    .to_owned(),
            );
        }
        generation = Some(value);
    }
    generation.ok_or_else(|| {
        "index metadata is missing a source generation watermark; rebuild the index".to_owned()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::core::index::IndexGenerationLease;
    use crate::core::index::{
        INDEX_METADATA_FILE, IndexBuilder, IndexDocumentCounts, IndexPublishRecoveryAction,
        find_latest_recoverable_retained_dir, hash_fallback_embedder_stack, index_parent,
        recover_interrupted_publish, validated_index_generation, write_index_metadata,
    };
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    type TestResult = Result<(), String>;

    fn parse_value(value: Value) -> Result<u64, String> {
        parse(value.as_object().expect("object fixture"))
    }

    #[test]
    fn explicit_zero_and_full_width_watermarks_are_valid() {
        assert_eq!(
            parse_value(serde_json::json!({"sourceGeneration": 0})),
            Ok(0)
        );
        assert_eq!(
            parse_value(serde_json::json!({"sourceGeneration": u64::MAX})),
            Ok(u64::MAX)
        );
    }

    #[test]
    fn legacy_aliases_are_accepted_when_unambiguous() {
        for field in FIELDS {
            let mut metadata = Map::new();
            metadata.insert(field.to_owned(), Value::from(17_u64));
            assert_eq!(parse(&metadata), Ok(17));
        }
        assert_eq!(
            parse_value(serde_json::json!({
                "sourceGeneration": 17, "source_generation": 17, "generation": 17
            })),
            Ok(17)
        );
    }

    #[test]
    fn missing_watermark_is_not_generation_zero() {
        assert!(parse_value(serde_json::json!({})).is_err());
        assert!(parse_value(serde_json::json!({"documentCount": 0})).is_err());
    }

    #[test]
    fn every_present_alias_must_be_an_integer() {
        for field in FIELDS {
            for invalid in [
                Value::Null,
                serde_json::json!(-1),
                serde_json::json!(7.0),
                serde_json::json!(7.5),
                serde_json::json!(true),
                serde_json::json!("private-untrusted-watermark"),
                serde_json::json!([]),
                serde_json::json!({}),
                serde_json::from_str::<Value>("18446744073709551616")
                    .expect("JSON number outside u64"),
            ] {
                let mut metadata = serde_json::json!({
                    "sourceGeneration": 7, "source_generation": 7, "generation": 7
                });
                metadata[field] = invalid;
                let error = parse_value(metadata).expect_err("invalid alias cannot be ignored");
                assert!(error.contains(field));
                assert!(!error.contains("private-untrusted-watermark"));
            }
        }
    }

    #[test]
    fn conflicting_aliases_never_inherit_canonical_precedence() {
        for (left, left_field) in FIELDS.iter().enumerate() {
            for right_field in FIELDS.iter().skip(left + 1) {
                for (a, b) in [(0_u64, 1_u64), (7, 9), (u64::MAX, 0)] {
                    let mut metadata = Map::new();
                    metadata.insert((*left_field).to_owned(), Value::from(a));
                    metadata.insert((*right_field).to_owned(), Value::from(b));
                    assert!(
                        parse(&metadata)
                            .expect_err("conflicting aliases")
                            .contains("conflicting")
                    );
                }
            }
        }
    }

    // Build actual vector and (when enabled) lexical tiers. A bad
    // watermark must fail admission, not just an isolated parser test.
    async fn build_generation(cx: &asupersync::Cx, path: &Path, generation: u64) -> TestResult {
        let documents = vec![crate::search::IndexableDocument::new(
            "mem_watermark_recovery",
            "Recovery must preserve the identity of the accepted source snapshot.",
        )];
        IndexBuilder::new(path)
            .with_embedder_stack(hash_fallback_embedder_stack())
            .add_documents(documents.clone())
            .build(cx)
            .await
            .map_err(|error| error.to_string())?;
        #[cfg(feature = "lexical-bm25")]
        crate::core::index::build_lexical_tier(cx, path, &documents)
            .await
            .map_err(|error| error.to_string())?;
        write_index_metadata(path, generation, IndexDocumentCounts::memory_only(1), None)
            .map_err(|error| error.to_string())?;
        assert_eq!(validated_index_generation(path)?, generation);
        Ok(())
    }

    fn inventory(root: &Path) -> Result<BTreeMap<PathBuf, Option<Vec<u8>>>, String> {
        let mut result = BTreeMap::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(path) = pending.pop() {
            let relative = path
                .strip_prefix(root)
                .map_err(|error| error.to_string())?
                .to_path_buf();
            let metadata = std::fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
            if metadata.is_dir() {
                result.insert(relative, None);
                for entry in std::fs::read_dir(&path).map_err(|error| error.to_string())? {
                    pending.push(entry.map_err(|error| error.to_string())?.path());
                }
            } else if metadata.is_file() {
                result.insert(
                    relative,
                    Some(std::fs::read(&path).map_err(|error| error.to_string())?),
                );
            } else {
                return Err("unexpected special entry in watermark test inventory".to_owned());
            }
        }
        Ok(result)
    }

    fn damage_watermark(path: &Path, damage: &str) -> TestResult {
        let metadata_path = path.join(INDEX_METADATA_FILE);
        let mut metadata: Value = serde_json::from_slice(
            &std::fs::read(&metadata_path).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        match damage {
            "missing" => {
                let object = metadata.as_object_mut().expect("manifest");
                for field in FIELDS {
                    object.remove(field);
                }
            }
            "null" => metadata["sourceGeneration"] = Value::Null,
            "conflicting" => metadata["generation"] = Value::from(1_u64),
            "invalid_alias" => metadata["source_generation"] = Value::from("untrusted"),
            _ => return Err("unknown watermark damage fixture".to_owned()),
        }
        std::fs::write(
            &metadata_path,
            serde_json::to_vec_pretty(&metadata).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())
    }

    fn recovery_case(damage: &'static str) -> TestResult {
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let index = root
            .path()
            .canonicalize()
            .map_err(|error| error.to_string())?
            .join("index");
        crate::core::run_cli_with_cx(Duration::from_secs(40), |cx| async move {
            let _root = root;
            let accepted = index_parent(&index).join("index.previous");
            let ambiguous = index_parent(&index).join("index.previous.001");
            build_generation(&cx, &accepted, 7).await?;
            build_generation(&cx, &ambiguous, 9).await?;
            damage_watermark(&ambiguous, damage)?;
            assert!(validated_index_generation(&ambiguous).is_err());
            assert_eq!(
                find_latest_recoverable_retained_dir(&index).map_err(|error| error.to_string())?,
                Some(accepted.clone())
            );
            let before = inventory(index_parent(&index))?;
            #[cfg(unix)]
            {
                let lease = IndexGenerationLease::read(&cx, &index)
                    .await
                    .map_err(|error| error.to_string())?;
                assert_eq!(
                    lease
                        .index_for_snapshot(&cx, &index, 9)
                        .map_err(|error| error.to_string())?,
                    accepted
                );
                assert!(lease.index_for_snapshot(&cx, &index, 6).is_err());
                drop(lease);
            }
            assert_eq!(
                inventory(index_parent(&index))?,
                before,
                "selection must be read-only"
            );
            let rejected_bytes = inventory(&ambiguous)?;
            assert_eq!(
                recover_interrupted_publish(&index).map_err(|error| error.to_string())?,
                IndexPublishRecoveryAction::RetainedGenerationRestored
            );
            assert_eq!(validated_index_generation(&index)?, 7);
            assert_eq!(
                inventory(&ambiguous)?,
                rejected_bytes,
                "unadmitted bytes stay intact"
            );
            Ok(())
        })
        .map_err(|error| error.to_string())?
    }

    #[test]
    fn recovery_uses_accepted_tiers_instead_of_a_missing_watermark() -> TestResult {
        recovery_case("missing")
    }

    #[test]
    fn recovery_uses_accepted_tiers_instead_of_a_null_watermark() -> TestResult {
        recovery_case("null")
    }

    #[test]
    fn readers_and_repair_reject_conflicting_aliases() -> TestResult {
        recovery_case("conflicting")
    }

    #[test]
    fn readers_and_repair_reject_invalid_secondary_aliases() -> TestResult {
        recovery_case("invalid_alias")
    }

    #[test]
    fn missing_watermark_alone_is_never_promoted_but_explicit_zero_is() -> TestResult {
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let index = root
            .path()
            .canonicalize()
            .map_err(|error| error.to_string())?
            .join("index");
        crate::core::run_cli_with_cx(Duration::from_secs(40), |cx| async move {
            let _root = root;
            let retained = index_parent(&index).join("index.previous");
            build_generation(&cx, &retained, 0).await?;
            let valid_manifest = std::fs::read(retained.join(INDEX_METADATA_FILE))
                .map_err(|error| error.to_string())?;
            damage_watermark(&retained, "missing")?;
            let before = inventory(index_parent(&index))?;
            assert_eq!(
                recover_interrupted_publish(&index).map_err(|error| error.to_string())?,
                IndexPublishRecoveryAction::NoRecoverableGeneration
            );
            assert!(!index.exists());
            assert_eq!(inventory(index_parent(&index))?, before);
            // Explicit zero is a real snapshot, unlike a missing watermark.
            std::fs::write(retained.join(INDEX_METADATA_FILE), valid_manifest)
                .map_err(|error| error.to_string())?;
            assert_eq!(
                recover_interrupted_publish(&index).map_err(|error| error.to_string())?,
                IndexPublishRecoveryAction::RetainedGenerationRestored
            );
            assert_eq!(validated_index_generation(&index)?, 0);
            Ok(())
        })
        .map_err(|error| error.to_string())?
    }
}
