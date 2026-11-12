//! Retention accounts for shared immutable segments without counting a live
//! or retained alias as reclaimed storage. These tests use real filesystem
//! links and the production deletion boundary.

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[cfg(unix)]
fn entry(path: &Path, reason: RetentionReason) -> RetainedGenerationEntry {
    RetainedGenerationEntry {
        path: path.to_path_buf(),
        generation: None,
        reason,
        size_bytes: directory_bytes(path),
    }
}

#[cfg(unix)]
fn measured_plan(mut entries: Vec<RetainedGenerationEntry>) -> RetentionPlan {
    measure_reclaimable_entries(&mut entries);
    RetentionPlan { limit: 2, entries }
}

#[cfg(unix)]
fn unbounded_time_budget() -> AutomaticRetentionBudget {
    AutomaticRetentionBudget {
        request: crate::core::budget::RequestBudget::unbounded(),
        work_remaining: AUTO_WORK_LIMIT,
        validations_remaining: AUTO_VALIDATION_LIMIT,
    }
}

#[test]
fn independent_copies_each_contribute_their_content_length() -> TestResult {
    let root = tempfile::tempdir()?;
    let parent = root.path().canonicalize()?;
    let index = parent.join("index");
    std::fs::create_dir(&index)?;
    for name in ["index.previous", "index.previous.001"] {
        let retained = parent.join(name);
        std::fs::create_dir(&retained)?;
        std::fs::write(retained.join("segment"), b"independent segment")?;
    }
    let retention = plan(&index, 0)?;
    let expected = 2 * b"independent segment".len() as u64;
    assert_eq!(retention.reclaimable_bytes(), expected);
    assert_eq!(
        reclaimable_bytes_for_paths(retention.reclaimable().map(|entry| entry.path.as_path())),
        expected
    );
    let outcome = apply(&index, &retention);
    assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
    assert_eq!(outcome.reclaimed.len(), 2);
    assert_eq!(outcome.reclaimed_bytes(), expected);
    assert!(index.is_dir());
    Ok(())
}

#[cfg(unix)]
#[test]
fn shared_active_and_kept_segments_survive_without_reclaimed_byte_credit() -> TestResult {
    use std::os::unix::fs::MetadataExt;

    let root = tempfile::tempdir()?;
    let parent = root.path().canonicalize()?;
    let index = parent.join("index");
    let kept = parent.join("index.previous");
    let old = parent.join("index.previous.001");
    for path in [&index, &kept, &old] {
        std::fs::create_dir(path)?;
    }
    let segment = b"immutable lexical postings shared by three generations";
    std::fs::write(index.join("segment.idx"), segment)?;
    std::fs::hard_link(index.join("segment.idx"), kept.join("segment.idx"))?;
    std::fs::hard_link(index.join("segment.idx"), old.join("segment.idx"))?;
    std::fs::write(old.join("meta.json"), b"retired metadata")?;
    let retention = measured_plan(vec![
        entry(&kept, RetentionReason::NewestValid),
        entry(&old, RetentionReason::BeyondRetentionLimit),
    ]);
    assert_eq!(retention.entries[0].size_bytes, segment.len() as u64);
    assert_eq!(
        retention.reclaimable_bytes(),
        b"retired metadata".len() as u64
    );
    assert_eq!(
        reclaimable_bytes_for_paths([old.as_path(), kept.as_path(), old.as_path()]),
        b"retired metadata".len() as u64,
        "the active alias survives, and repeated selected paths are not extra links"
    );

    let outcome = apply(&index, &retention);
    assert_eq!(outcome.kept, 1);
    assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
    assert_eq!(outcome.reclaimed_bytes(), retention.reclaimable_bytes());
    assert!(!old.exists());
    assert_eq!(std::fs::read(index.join("segment.idx"))?, segment);
    assert_eq!(std::fs::read(kept.join("segment.idx"))?, segment);
    assert_eq!(std::fs::metadata(index.join("segment.idx"))?.nlink(), 2);
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_shared_retained_group_is_counted_once_when_its_final_names_are_removed() -> TestResult {
    let root = tempfile::tempdir()?;
    let parent = root.path().canonicalize()?;
    let index = parent.join("index");
    let first = parent.join("index.previous");
    let last = parent.join("index.previous.001");
    for path in [&index, &first, &last] {
        std::fs::create_dir(path)?;
    }
    let segment = b"one inode, three selected names";
    std::fs::write(first.join("segment.idx"), segment)?;
    std::fs::hard_link(first.join("segment.idx"), first.join("second-name.idx"))?;
    std::fs::hard_link(first.join("segment.idx"), last.join("segment.idx"))?;
    std::fs::write(first.join("meta.json"), b"first")?;
    std::fs::write(last.join("meta.json"), b"last")?;
    let retention = measured_plan(vec![
        entry(&first, RetentionReason::UnusableGeneration),
        entry(&last, RetentionReason::UnusableGeneration),
    ]);
    assert_eq!(retention.entries[0].size_bytes, b"first".len() as u64);
    assert_eq!(
        retention.entries[1].size_bytes,
        (segment.len() + b"last".len()) as u64
    );
    let expected = (segment.len() + b"first".len() + b"last".len()) as u64;
    assert_eq!(retention.reclaimable_bytes(), expected);
    assert_eq!(
        reclaimable_bytes_for_paths([first.as_path(), last.as_path()]),
        expected
    );

    let outcome = apply(&index, &retention);
    assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
    assert_eq!(outcome.reclaimed.len(), 2);
    assert_eq!(outcome.reclaimed[0].size_bytes, b"first".len() as u64);
    assert_eq!(
        outcome.reclaimed[1].size_bytes,
        (segment.len() + b"last".len()) as u64
    );
    assert_eq!(outcome.reclaimed_bytes(), expected);
    assert!(!first.exists() && !last.exists());
    Ok(())
}

#[cfg(unix)]
#[test]
fn deletion_rechecks_links_instead_of_reusing_a_preview_estimate() -> TestResult {
    let root = tempfile::tempdir()?;
    let parent = root.path().canonicalize()?;
    let index = parent.join("index");
    let old = parent.join("index.previous");
    std::fs::create_dir(&index)?;
    std::fs::create_dir(&old)?;
    let segment = b"a later publication still owns these bytes";
    std::fs::write(old.join("segment.idx"), segment)?;
    let retention = plan(&index, 0)?;
    assert_eq!(retention.reclaimable_bytes(), segment.len() as u64);

    std::fs::hard_link(old.join("segment.idx"), index.join("segment.idx"))?;
    let outcome = apply(&index, &retention);
    assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
    assert_eq!(outcome.reclaimed.len(), 1);
    assert_eq!(outcome.reclaimed_bytes(), 0);
    assert_eq!(std::fs::read(index.join("segment.idx"))?, segment);
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_rejected_deletion_never_contributes_its_shared_links_to_the_outcome() -> TestResult {
    let root = tempfile::tempdir()?;
    let parent = root.path().canonicalize()?;
    let index = parent.join("index");
    let old = parent.join("index.previous");
    std::fs::create_dir(&index)?;
    std::fs::create_dir(&old)?;
    let segment = b"the active generation must survive";
    std::fs::write(index.join("segment.idx"), segment)?;
    std::fs::hard_link(index.join("segment.idx"), old.join("segment.idx"))?;
    let forged = measured_plan(vec![
        entry(&index, RetentionReason::UnusableGeneration),
        entry(&old, RetentionReason::UnusableGeneration),
    ]);
    assert_eq!(forged.reclaimable_bytes(), segment.len() as u64);

    let outcome = apply(&index, &forged);
    assert_eq!(outcome.failures.len(), 1);
    assert_eq!(outcome.failures[0].0, index);
    assert_eq!(outcome.reclaimed.len(), 1);
    assert_eq!(outcome.reclaimed_bytes(), 0);
    assert_eq!(std::fs::read(index.join("segment.idx"))?, segment);
    assert!(!old.exists());
    Ok(())
}

#[cfg(unix)]
#[test]
fn bounded_cleanup_counts_a_shared_inode_only_in_the_last_batch() -> TestResult {
    let root = tempfile::tempdir()?;
    let parent = root.path().canonicalize()?;
    let index = parent.join("index");
    std::fs::create_dir(&index)?;
    let paths: Vec<_> = (0..3)
        .map(|number| parent.join(format!(".index.reclaim-1-{number:03}")))
        .collect();
    for path in &paths {
        std::fs::create_dir(path)?;
        std::fs::write(path.join("meta.json"), b"private")?;
    }
    let segment = b"shared until the final cleanup batch";
    std::fs::write(paths[0].join("segment.idx"), segment)?;
    for path in &paths[1..] {
        std::fs::hard_link(paths[0].join("segment.idx"), path.join("segment.idx"))?;
    }
    let preview = plan(&index, 0)?;
    assert_eq!(
        preview.reclaimable_bytes(),
        (segment.len() + 3 * b"private".len()) as u64
    );

    let first = reclaim_after_publish_with_budget(&index, &mut unbounded_time_budget())
        .ok_or("first bounded cleanup")?;
    assert!(first.failures.is_empty(), "{:?}", first.failures);
    assert_eq!(first.reclaimed.len(), AUTO_RECLAIM_LIMIT);
    assert_eq!(first.reclaimed_bytes(), (2 * b"private".len()) as u64);
    assert_eq!(std::fs::read(paths[2].join("segment.idx"))?, segment);
    let last = reclaim_after_publish_with_budget(&index, &mut unbounded_time_budget())
        .ok_or("last bounded cleanup")?;
    assert!(last.failures.is_empty(), "{:?}", last.failures);
    assert_eq!(last.reclaimed.len(), 1);
    assert_eq!(
        last.reclaimed_bytes(),
        (segment.len() + b"private".len()) as u64
    );
    assert_eq!(
        first.reclaimed_bytes() + last.reclaimed_bytes(),
        preview.reclaimable_bytes()
    );
    assert!(index.is_dir());
    Ok(())
}
