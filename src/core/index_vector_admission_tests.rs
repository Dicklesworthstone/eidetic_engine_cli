//! Real native sidecars must not borrow authority from unchanged main counts.

use std::path::PathBuf;
use std::sync::Arc;

use frankensearch::VectorIndex;
use frankensearch::index::{StrictWalInspection, inspect_wal_strict, wal_path_for};

use super::super::{
    IndexDocumentCounts, VECTOR_INDEX_FAST_FILE, VECTOR_INDEX_QUALITY_FILE,
    build_index_generation_sync, read_fast_vector_record_count, read_index_metadata,
    validate_built_generation, validated_index_generation_with_identity,
    verify_published_tier_counts, write_index_metadata,
};
use crate::search::{EmbedderStack, HashEmbedder, IndexableDocument};

type TestResult = Result<(), String>;

struct Fixture {
    root: tempfile::TempDir,
    directory: PathBuf,
    quality: bool,
}

impl Fixture {
    fn new(quality: bool) -> Result<Self, String> {
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let directory = root.path().join("index");
        let stack = EmbedderStack::from_parts(
            Arc::new(HashEmbedder::default_256()),
            quality
                .then(|| Arc::new(HashEmbedder::default_256()) as Arc<dyn crate::search::Embedder>),
        );
        let documents = vec![
            IndexableDocument::new("first", "Record the successful deployment."),
            IndexableDocument::new("second", "Verify the database backup."),
        ];
        let counts = IndexDocumentCounts::memory_only(2);
        let stats = build_index_generation_sync(&directory, stack, documents)?;
        validate_built_generation(&directory, stats, counts)?;
        write_index_metadata(&directory, 7, counts, None).map_err(|error| error.to_string())?;
        let fixture = Self {
            root,
            directory,
            quality,
        };
        fixture.assert_ready()?;
        Ok(fixture)
    }

    fn tier(&self, quality: bool) -> PathBuf {
        self.directory.join(if quality {
            VECTOR_INDEX_QUALITY_FILE
        } else {
            VECTOR_INDEX_FAST_FILE
        })
    }

    fn assert_ready(&self) -> TestResult {
        verify_published_tier_counts(&self.directory, 2, self.quality)?;
        assert_eq!(
            validated_index_generation_with_identity(&self.directory, None)?,
            7
        );
        assert_eq!(read_fast_vector_record_count(&self.directory), Some(2));
        Ok(())
    }

    fn append_replacement(&self, quality: bool) -> Result<PathBuf, String> {
        let path = self.tier(quality);
        let mut index = VectorIndex::open(&path).map_err(|error| error.to_string())?;
        let mut vector = vec![0.0; index.dimension()];
        vector[0] = 1.0;
        index
            .append("first", &vector)
            .map_err(|error| error.to_string())?;
        assert_eq!(index.record_count(), 2);
        assert_eq!(index.wal_record_count(), 1);
        drop(index);
        Ok(wal_path_for(&path))
    }

    fn assert_withheld_without_repair(&self, quality: bool) -> TestResult {
        let tier = self.tier(quality);
        let wal = wal_path_for(&tier);
        let main_before = std::fs::read(&tier).map_err(|error| error.to_string())?;
        let wal_before = std::fs::read(&wal).map_err(|error| error.to_string())?;
        assert!(verify_published_tier_counts(&self.directory, 2, self.quality).is_err());
        assert!(validated_index_generation_with_identity(&self.directory, None).is_err());
        let status = read_index_metadata(&self.directory);
        assert!(status.compatibility_error.is_some(), "{status:?}");
        if !quality {
            assert_eq!(read_fast_vector_record_count(&self.directory), None);
        }
        assert_eq!(
            std::fs::read(&tier).map_err(|error| error.to_string())?,
            main_before
        );
        assert_eq!(
            std::fs::read(&wal).map_err(|error| error.to_string())?,
            wal_before
        );
        Ok(())
    }
}

#[test]
fn compacted_vector_admission_accepts_absent_empty_and_header_only_sidecars() -> TestResult {
    for quality in [false, true] {
        let fixture = Fixture::new(quality)?;
        for tier in [false, true].into_iter().filter(|&tier| !tier || quality) {
            let wal = wal_path_for(&fixture.tier(tier));
            std::fs::write(&wal, b"").map_err(|error| error.to_string())?;
            fixture.assert_ready()?;
            fixture.append_replacement(tier)?;
            let bytes = std::fs::read(&wal).map_err(|error| error.to_string())?;
            // Keep the exact native legacy header, without its first batch.
            std::fs::write(&wal, &bytes[..20]).map_err(|error| error.to_string())?;
            let inspection = inspect_wal_strict(&wal).map_err(|error| error.to_string())?;
            assert!(
                matches!(inspection, StrictWalInspection::LegacyV1(image) if image.entry_count == 0)
            );
            fixture.assert_ready()?;
        }
    }
    Ok(())
}

#[test]
fn pending_same_id_replacements_cannot_pass_published_tier_counts() -> TestResult {
    for quality in [false, true] {
        let fixture = Fixture::new(quality)?;
        fixture.append_replacement(quality)?;
        let native = VectorIndex::open_read_only(&fixture.tier(quality))
            .map_err(|error| error.to_string())?;
        assert_eq!(
            native.record_count(),
            2,
            "the old header-only check would pass"
        );
        assert_eq!(native.wal_record_count(), 1);
        drop(native);
        fixture.assert_withheld_without_repair(quality)?;
    }
    Ok(())
}

#[test]
fn corrupt_and_stale_wal_images_are_withheld_without_native_recovery() -> TestResult {
    for quality in [false, true] {
        for fault in ["header", "tail", "crc", "stale", "oversize"] {
            let fixture = Fixture::new(quality)?;
            let wal = fixture.append_replacement(quality)?;
            let mut bytes = std::fs::read(&wal).map_err(|error| error.to_string())?;
            match fault {
                "header" => bytes.truncate(5),
                "tail" => {
                    bytes.pop();
                }
                "crc" => {
                    *bytes.last_mut().ok_or("empty native WAL")? ^= 1;
                }
                "stale" => {
                    let mut writer = VectorIndex::open(&fixture.tier(quality))
                        .map_err(|error| error.to_string())?;
                    writer.compact().map_err(|error| error.to_string())?;
                    drop(writer);
                    // Restore a complete WAL from the previous compaction
                    // generation. Native recovery silently drops its entries.
                }
                "oversize" => bytes.resize(super::MAX_PUBLISHED_WAL_BYTES as usize + 1, 0),
                _ => unreachable!(),
            }
            std::fs::write(&wal, &bytes).map_err(|error| error.to_string())?;
            if fault != "oversize" {
                let native = VectorIndex::open_read_only(&fixture.tier(quality))
                    .map_err(|error| error.to_string())?;
                assert_eq!(native.record_count(), 2);
                assert_eq!(native.wal_record_count(), 0, "{fault}");
                drop(native);
            }
            fixture.assert_withheld_without_repair(quality)?;
        }
    }
    Ok(())
}

#[test]
fn sidecar_changes_during_native_open_are_withheld() -> TestResult {
    let fixture = Fixture::new(false)?;
    let tier = fixture.tier(false);
    let wal = wal_path_for(&tier);
    if wal.exists() {
        std::fs::rename(&wal, fixture.root.path().join("initial.wal"))
            .map_err(|error| error.to_string())?;
    }
    let before = std::fs::read(&tier).map_err(|error| error.to_string())?;
    let result = super::open_with(&tier, |path| {
        let index = VectorIndex::open_read_only(path)?;
        // Deterministic race injection; the actual native reader remains in
        // use. Even a new empty sidecar changes the inspected file image.
        std::fs::write(&wal, b"").map_err(frankensearch::SearchError::Io)?;
        Ok(index)
    });
    assert!(result.is_err());
    assert_eq!(
        std::fs::read(&tier).map_err(|error| error.to_string())?,
        before
    );
    assert_eq!(std::fs::read(&wal).map_err(|error| error.to_string())?, b"");
    fixture.assert_ready()?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn published_vector_wal_refuses_aliases_without_modifying_the_target() -> TestResult {
    for symlink in [false, true] {
        let fixture = Fixture::new(false)?;
        let tier = fixture.tier(false);
        let wal = wal_path_for(&tier);
        let outside = fixture.root.path().join("outside.wal");
        std::fs::write(&outside, b"").map_err(|error| error.to_string())?;
        if symlink {
            std::os::unix::fs::symlink(&outside, &wal).map_err(|error| error.to_string())?;
        } else {
            std::fs::hard_link(&outside, &wal).map_err(|error| error.to_string())?;
        }
        assert!(super::open_read_only(&tier).is_err());
        assert_eq!(
            std::fs::read(&outside).map_err(|error| error.to_string())?,
            b""
        );
    }
    Ok(())
}
