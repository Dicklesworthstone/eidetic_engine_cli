//! Native segment sharing must survive real delete commits, upserts and merges
//! without changing the live generation or a second retained generation.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use crate::search::{IndexableDocument, LexicalRead};

use super::{Delta, apply, copy_generation, immutable_lexical_segments, lexical_maintenance};

type TestResult = Result<(), String>;

fn snapshot(
    directory: &Path,
    paths: &BTreeSet<PathBuf>,
) -> Result<BTreeMap<PathBuf, Vec<u8>>, String> {
    paths
        .iter()
        .map(|path| {
            fs::read(directory.join(path))
                .map(|bytes| (path.clone(), bytes))
                .map_err(|error| error.to_string())
        })
        .collect()
}

fn identity(path: &Path) -> Result<(u64, u64), String> {
    fs::metadata(path)
        .map(|metadata| (metadata.dev(), metadata.ino()))
        .map_err(|error| error.to_string())
}

async fn assert_projection(cx: &asupersync::Cx, directory: &Path, changed: bool) -> TestResult {
    let lexical = crate::search::open_lexical_index_read_only(&directory.join("lexical"))
        .map_err(|error| error.to_string())?;
    assert_eq!(lexical.doc_count().map_err(|error| error.to_string())?, 23);
    let retained = lexical
        .search(cx, "retainedmarker", 32)
        .await
        .map_err(|error| error.to_string())?;
    let expected = (0..23)
        .filter(|number| !changed || *number > 1)
        .map(|number| format!("source-{number:02}"))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        retained
            .into_iter()
            .map(|hit| hit.doc_id.as_str().to_owned())
            .collect::<BTreeSet<_>>(),
        expected,
    );
    let changed_ids = lexical
        .search(cx, "changedmarker", 32)
        .await
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(|hit| hit.doc_id.as_str().to_owned())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        changed_ids,
        if changed {
            BTreeSet::from(["source-00".to_owned(), "source-new".to_owned()])
        } else {
            BTreeSet::new()
        },
    );
    Ok(())
}

#[test]
fn shared_native_segments_keep_live_and_retained_generations_unchanged() -> TestResult {
    let root = tempfile::tempdir().map_err(|error| error.to_string())?;
    let parent = root
        .path()
        .canonicalize()
        .map_err(|error| error.to_string())?;
    crate::core::run_cli_with_cx(std::time::Duration::from_secs(120), |cx| async move {
        let _root = root;
        let live = parent.join("live");
        let staged = parent.join("staged");
        let retained = parent.join("retained");
        let stack = super::super::hash_fallback_embedder_stack();
        super::super::IndexBuilder::new(&live)
            .with_embedder_stack(stack.clone())
            .with_tantivy_lexical()
            .add_documents(
                (0..24)
                    .map(|number| {
                        IndexableDocument::new(
                            format!("source-{number:02}"),
                            format!("retainedmarker repair note {number}"),
                        )
                    })
                    .collect::<Vec<_>>(),
            )
            .build(&cx)
            .await
            .map_err(|error| error.to_string())?;
        // A prior deletion produces a generation-qualified bitset. It is an
        // immutable component too, even though future deletes use new files.
        apply(
            &cx,
            &live,
            &stack,
            &Delta {
                removals: vec!["source-23".to_owned()],
                ..Delta::default()
            },
        )
        .await
        .map_err(|error| error.detail)?;
        let unrelated = PathBuf::from("lexical/unreferenced.store");
        fs::write(live.join(&unrelated), b"not an authoritative segment")
            .map_err(|error| error.to_string())?;
        let immutable = immutable_lexical_segments(&live)
            .map_err(|error| error.to_string())?
            .into_iter()
            .filter(|path| live.join(path).is_file())
            .collect::<BTreeSet<_>>();
        assert!(!immutable.is_empty());
        assert!(
            immutable
                .iter()
                .any(|path| path.extension().is_some_and(|ext| ext == "del"))
        );
        assert!(!immutable.contains(&unrelated));
        let mutable = BTreeSet::from([
            PathBuf::from("vector.fast.idx"),
            PathBuf::from("lexical/meta.json"),
            PathBuf::from("lexical/.managed.json"),
            PathBuf::from("lexical/ord_table.json"),
            unrelated,
        ]);
        let protected = immutable.union(&mutable).cloned().collect::<BTreeSet<_>>();
        let before = snapshot(&live, &protected)?;
        fs::create_dir(&staged).map_err(|error| error.to_string())?;
        fs::create_dir(&retained).map_err(|error| error.to_string())?;
        copy_generation(&live, &staged).map_err(|error| error.to_string())?;
        // The next source already has multiple links. It remains admissible
        // only for the exact components named by its own native metadata.
        copy_generation(&staged, &retained).map_err(|error| error.to_string())?;
        assert_eq!(snapshot(&staged, &protected)?, before);
        assert_eq!(snapshot(&retained, &protected)?, before);
        for path in &immutable {
            assert_eq!(identity(&live.join(path))?, identity(&staged.join(path))?);
            assert_eq!(identity(&live.join(path))?, identity(&retained.join(path))?);
            assert_eq!(
                fs::metadata(live.join(path))
                    .map_err(|error| error.to_string())?
                    .nlink(),
                3
            );
        }
        for path in &mutable {
            assert_ne!(identity(&live.join(path))?, identity(&staged.join(path))?);
            assert_ne!(
                identity(&staged.join(path))?,
                identity(&retained.join(path))?
            );
        }
        apply(
            &cx,
            &staged,
            &stack,
            &Delta {
                upserts: vec![
                    IndexableDocument::new("source-00", "changedmarker revised repair"),
                    IndexableDocument::new("source-new", "changedmarker additional repair"),
                ],
                removals: vec!["source-01".to_owned()],
                ..Delta::default()
            },
        )
        .await
        .map_err(|error| error.detail)?;
        // Native merging and garbage collection may unlink staging's shared
        // names; live/retained names must preserve all their original bytes.
        let compacted = lexical_maintenance::compact_if_needed(&cx, &staged, 1)
            .map_err(|error| error.detail)?;
        assert_eq!(compacted.segments_after, 1);
        assert_eq!(compacted.documents, 23);
        assert!(immutable.iter().any(|path| !staged.join(path).exists()));
        assert_eq!(snapshot(&live, &protected)?, before);
        assert_eq!(snapshot(&retained, &protected)?, before);
        assert_projection(&cx, &live, false).await?;
        assert_projection(&cx, &retained, false).await?;
        assert_projection(&cx, &staged, true).await?;
        for directory in [&live, &retained, &staged] {
            super::super::verify_published_tier_counts(directory, 23, false)?;
        }
        Ok(())
    })
    .map_err(|error| error.to_string())?
}

#[test]
fn malformed_or_symlinked_lexical_metadata_never_authorizes_sharing() -> TestResult {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().map_err(|error| error.to_string())?;
    let parent = root
        .path()
        .canonicalize()
        .map_err(|error| error.to_string())?;
    for linked in [false, true] {
        let live = parent.join(format!("live-{linked}"));
        let staged = parent.join(format!("staged-{linked}"));
        fs::create_dir_all(live.join("lexical")).map_err(|error| error.to_string())?;
        fs::create_dir(&staged).map_err(|error| error.to_string())?;
        let metadata = live.join("lexical/meta.json");
        if linked {
            let outside = parent.join("outside.json");
            fs::write(&outside, b"{}").map_err(|error| error.to_string())?;
            symlink(&outside, &metadata).map_err(|error| error.to_string())?;
        } else {
            fs::write(&metadata, b"not Tantivy metadata").map_err(|error| error.to_string())?;
        }
        assert!(copy_generation(&live, &staged).is_err());
        assert_eq!(
            fs::read_dir(&staged)
                .map_err(|error| error.to_string())?
                .count(),
            0
        );
    }
    // The native metadata reader also opens its managed-file inventory.
    // Reject special entries there before entering the native read path.
    let live = parent.join("special-managed");
    let staged = parent.join("special-managed-stage");
    fs::create_dir_all(live.join("lexical/.managed.json")).map_err(|error| error.to_string())?;
    fs::write(live.join("lexical/meta.json"), b"{}").map_err(|error| error.to_string())?;
    fs::create_dir(&staged).map_err(|error| error.to_string())?;
    let refusal = copy_generation(&live, &staged).expect_err("special managed metadata");
    assert!(
        refusal
            .to_string()
            .contains("managed metadata is not a regular file")
    );
    assert_eq!(
        fs::read_dir(&staged)
            .map_err(|error| error.to_string())?
            .count(),
        0
    );
    Ok(())
}
