//! Independent staging inodes, using copy-on-write only when the filesystem permits it.
//!
//! A reflink is not a hard link: later writes to either file must not change the
//! other generation. Unsupported cloning falls back to a bounded byte copy;
//! capacity and I/O failures remain failures. The caller owns the private
//! staging directory, publication lease, flush barrier, and abandoned-file cleanup.

use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

const COPY_BUFFER_BYTES: usize = 1024 * 1024;

#[derive(Debug, Default)]
pub(super) struct CopyStats {
    pub copied_files: u64,
    pub copied_bytes: u64,
    pub cloned_files: u64,
    /// Logical file bytes whose content was cloned, not measured disk allocation.
    pub cloned_bytes: u64,
    pub clone_refusals: u64,
}

#[derive(Default)]
pub(super) struct StagingCopier {
    clone_unavailable: bool,
    buffer: Vec<u8>,
    stats: CopyStats,
}

impl StagingCopier {
    pub(super) fn stats(&self) -> &CopyStats {
        &self.stats
    }

    pub(super) fn copy_file(&mut self, source: &Path, target: &Path) -> io::Result<()> {
        self.copy_with(source, target, clone_file)
    }

    fn copy_with(
        &mut self,
        source_path: &Path,
        target: &Path,
        clone: impl FnOnce(&File, &Path) -> io::Result<CloneAttempt>,
    ) -> io::Result<()> {
        let before = fs::symlink_metadata(source_path)?;
        require_regular(&before)?;
        let mut source = open_source(source_path)?;
        require_unchanged(&before, &source.metadata()?)?;
        let length = before.len();
        let (mut destination, cloned) = if length == 0 || self.clone_unavailable {
            (create_destination(target)?, false)
        } else {
            match clone(&source, target)? {
                CloneAttempt::Cloned(file) => (file, true),
                CloneAttempt::Unavailable { file, error } => {
                    if let Some(error) = error {
                        if !clone_is_unavailable(&error) {
                            return Err(error);
                        }
                        self.stats.clone_refusals = self.stats.clone_refusals.saturating_add(1);
                        tracing::info!(
                            target: "ee::index",
                            errno = error.raw_os_error(),
                            "index staging clone unavailable; using independent byte copies for this generation"
                        );
                    }
                    // Cache only for this copy operation, not globally by OS or
                    // pathname. Another workspace/mount gets its own real probe.
                    self.clone_unavailable = true;
                    (
                        match file {
                            Some(file) => file,
                            None => create_destination(target)?,
                        },
                        false,
                    )
                }
            }
        };
        if !cloned {
            // A refused clone may have touched our exclusively created inode.
            // Never reopen/truncate a target supplied by somebody else.
            destination.set_len(0)?;
            destination.seek(SeekFrom::Start(0))?;
            copy_bytes(&mut source, &mut destination, length, &mut self.buffer)?;
        }
        let written = destination.metadata()?;
        require_regular(&written)?;
        if written.len() != length {
            return Err(changed("staged file length differs from its source"));
        }
        require_unchanged(&before, &source.metadata()?)?;
        require_unchanged(&before, &fs::symlink_metadata(source_path)?)?;
        require_unchanged(&written, &fs::symlink_metadata(target)?)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if before.dev() == written.dev() && before.ino() == written.ino() {
                return Err(changed("staging must own an independent inode"));
            }
        }
        if cloned {
            self.stats.cloned_files = self.stats.cloned_files.saturating_add(1);
            self.stats.cloned_bytes = self.stats.cloned_bytes.saturating_add(length);
        } else {
            self.stats.copied_files = self.stats.copied_files.saturating_add(1);
            self.stats.copied_bytes = self.stats.copied_bytes.saturating_add(length);
        }
        Ok(())
    }
}

// Linux clones into an already-created inode. Apple atomically creates a new
// pathname instead. Preserve any owned descriptor on a refused Linux clone so
// the fallback cannot truncate an unrelated pre-existing destination.
enum CloneAttempt {
    #[cfg_attr(
        not(any(
            target_vendor = "apple",
            all(
                any(target_os = "linux", target_os = "android"),
                not(any(target_arch = "sparc", target_arch = "sparc64"))
            )
        )),
        allow(dead_code)
    )]
    Cloned(File),
    Unavailable {
        file: Option<File>,
        error: Option<io::Error>,
    },
}

fn changed(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn require_regular(metadata: &Metadata) -> io::Result<()> {
    if !metadata.file_type().is_file() {
        return Err(changed("index staging source or destination is not a regular file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 {
            return Err(changed("index staging refuses multiply linked files"));
        }
    }
    Ok(())
}

/// These checks detect path replacement and observable concurrent mutations;
/// they do not replace the publisher's generation lease or attest arbitrary
/// uncooperative writers on filesystems with coarse timestamps. Reading atime
/// is intentionally excluded because ordinary byte-copy fallback may update it.
fn require_unchanged(before: &Metadata, after: &Metadata) -> io::Result<()> {
    require_regular(after)?;
    let mut same = before.len() == after.len() && before.permissions() == after.permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        same &= before.dev() == after.dev()
            && before.ino() == after.ino()
            && before.uid() == after.uid()
            && before.gid() == after.gid()
            && before.mtime() == after.mtime()
            && before.mtime_nsec() == after.mtime_nsec()
            && before.ctime() == after.ctime()
            && before.ctime_nsec() == after.ctime_nsec();
    }
    #[cfg(not(unix))]
    {
        same &= before.modified()? == after.modified()?;
    }
    if !same {
        return Err(changed("index staging file changed during copying"));
    }
    Ok(())
}

fn open_source(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // NONBLOCK also prevents a file-to-FIFO replacement from hanging open.
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    options.open(path)
}

fn create_destination(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

#[cfg(all(
    any(target_os = "linux", target_os = "android"),
    not(any(target_arch = "sparc", target_arch = "sparc64"))
))]
fn clone_file(source: &File, target: &Path) -> io::Result<CloneAttempt> {
    let destination = create_destination(target)?;
    match rustix::fs::ioctl_ficlone(&destination, source) {
        Ok(()) => Ok(CloneAttempt::Cloned(destination)),
        Err(error) => Ok(CloneAttempt::Unavailable {
            file: Some(destination),
            error: Some(error.into()),
        }),
    }
}

#[cfg(target_vendor = "apple")]
fn clone_file(source: &File, target: &Path) -> io::Result<CloneAttempt> {
    match rustix::fs::fclonefileat(source, rustix::fs::CWD, target, rustix::fs::CloneFlags::empty()) {
        Ok(()) => {
            // clonefile creates exclusively but carries source permissions.
            // The staged generation is private and must remain writable even
            // when its source was sealed read-only after publication.
            use std::os::unix::fs::PermissionsExt;
            let destination = open_source(target)?;
            destination.set_permissions(fs::Permissions::from_mode(0o600))?;
            Ok(CloneAttempt::Cloned(destination))
        }
        Err(error) => Ok(CloneAttempt::Unavailable {
            file: None,
            error: Some(error.into()),
        }),
    }
}

#[cfg(not(any(
    target_vendor = "apple",
    all(
        any(target_os = "linux", target_os = "android"),
        not(any(target_arch = "sparc", target_arch = "sparc64"))
    )
)))]
fn clone_file(_source: &File, _target: &Path) -> io::Result<CloneAttempt> {
    Ok(CloneAttempt::Unavailable {
        file: None,
        error: None,
    })
}

fn clone_is_unavailable(error: &io::Error) -> bool {
    #[cfg(unix)]
    if let Some(code) = error.raw_os_error() {
        // The descriptors refer to regular files with compatible access modes.
        // These refusals describe clone support/policy, never lost capacity,
        // corruption, permission to create a destination, or a failed transfer.
        return [
            libc::EXDEV,
            libc::EOPNOTSUPP,
            libc::ENOSYS,
            libc::ENOTTY,
            libc::EINVAL,
            libc::EPERM,
        ]
        .contains(&code);
    }
    error.kind() == io::ErrorKind::Unsupported
}

fn copy_bytes(
    source: &mut impl Read,
    target: &mut impl Write,
    length: u64,
    buffer: &mut Vec<u8>,
) -> io::Result<()> {
    let desired = usize::try_from(length)
        .unwrap_or(usize::MAX)
        .min(COPY_BUFFER_BYTES);
    if buffer.len() < desired {
        buffer.resize(desired, 0);
    }
    let mut remaining = length;
    while remaining != 0 {
        let count = usize::try_from(remaining)
            .unwrap_or(usize::MAX)
            .min(buffer.len());
        source.read_exact(&mut buffer[..count])?;
        target.write_all(&buffer[..count])?;
        remaining -= count as u64;
    }
    let mut extra = [0_u8; 1];
    loop {
        match source.read(&mut extra) {
            Ok(0) => return Ok(()),
            Ok(_) => return Err(changed("index staging source grew during copying")),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn unavailable() -> io::Result<CloneAttempt> {
        Ok(CloneAttempt::Unavailable {
            file: None,
            error: Some(io::Error::from(io::ErrorKind::Unsupported)),
        })
    }

    #[test]
    fn unavailable_clone_is_probed_once_and_every_byte_is_copied() -> TestResult {
        let root = tempfile::tempdir()?;
        let source = root.path().join("source");
        let bytes: Vec<_> = (0..COPY_BUFFER_BYTES * 2 + 137)
            .map(|i| (i % 251) as u8)
            .collect();
        fs::write(&source, &bytes)?;
        let mut copier = StagingCopier::default();
        let mut probes = 0;
        for number in 0..3 {
            let target = root.path().join(format!("target-{number}"));
            copier.copy_with(&source, &target, |_, _| {
                probes += 1;
                unavailable()
            })?;
            assert_eq!(fs::read(&target)?, bytes);
            fs::write(&target, b"changed only in staging")?;
            assert_eq!(fs::read(&source)?, bytes);
        }
        assert_eq!(probes, 1);
        assert_eq!(copier.stats().clone_refusals, 1);
        assert_eq!(copier.stats().copied_files, 3);
        assert_eq!(copier.stats().copied_bytes, bytes.len() as u64 * 3);
        assert_eq!(copier.stats().cloned_bytes, 0);
        assert!(copier.buffer.len() <= COPY_BUFFER_BYTES);
        Ok(())
    }

    #[test]
    fn refused_clone_preserves_its_owned_destination_for_exact_fallback() -> TestResult {
        let root = tempfile::tempdir()?;
        let source = root.path().join("source");
        let target = root.path().join("target");
        fs::write(&source, b"the whole source")?;
        let mut copier = StagingCopier::default();
        copier.copy_with(&source, &target, |_, target| {
            let mut file = create_destination(target)?;
            file.write_all(b"a longer partial destination left by the attempted clone")?;
            Ok(CloneAttempt::Unavailable {
                file: Some(file),
                error: Some(io::Error::from(io::ErrorKind::Unsupported)),
            })
        })?;
        assert_eq!(fs::read(&target)?, b"the whole source");
        assert_eq!(copier.stats().copied_files, 1);
        Ok(())
    }

    #[test]
    fn native_clone_or_filesystem_fallback_keeps_mutations_isolated() -> TestResult {
        let root = tempfile::tempdir()?;
        let source = root.path().join("source");
        let target = root.path().join("target");
        let bytes = vec![37_u8; COPY_BUFFER_BYTES + 19];
        fs::write(&source, &bytes)?;
        let mut copier = StagingCopier::default();
        copier.copy_file(&source, &target)?;
        assert_eq!(fs::read(&target)?, bytes);
        assert_eq!(copier.stats().cloned_files + copier.stats().copied_files, 1);
        assert_eq!(
            copier.stats().cloned_bytes + copier.stats().copied_bytes,
            bytes.len() as u64
        );
        eprintln!("native staging strategy: {:?}", copier.stats());
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let left = fs::metadata(&source)?;
            let right = fs::metadata(&target)?;
            assert_ne!((left.dev(), left.ino()), (right.dev(), right.ino()));
            assert_eq!(left.nlink(), 1);
            assert_eq!(right.nlink(), 1);
            assert_eq!(right.mode() & 0o777, 0o600);
        }
        fs::write(&target, b"staging update")?;
        assert_eq!(fs::read(&source)?, bytes);
        fs::write(&source, b"live update")?;
        assert_eq!(fs::read(&target)?, b"staging update");
        Ok(())
    }

    #[test]
    fn existing_destination_is_never_truncated() -> TestResult {
        let root = tempfile::tempdir()?;
        let source = root.path().join("source");
        let target = root.path().join("target");
        fs::write(&source, b"live")?;
        fs::write(&target, b"existing destination")?;
        for native in [false, true] {
            let mut copier = StagingCopier::default();
            let result = if native {
                copier.copy_file(&source, &target)
            } else {
                copier.copy_with(&source, &target, |_, _| unavailable())
            };
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::AlreadyExists);
            assert_eq!(fs::read(&target)?, b"existing destination");
            assert_eq!(fs::read(&source)?, b"live");
        }
        Ok(())
    }

    #[test]
    fn empty_files_do_not_disable_later_clone_probes() -> TestResult {
        let root = tempfile::tempdir()?;
        let source = root.path().join("source");
        fs::write(&source, [])?;
        let mut copier = StagingCopier::default();
        copier.copy_with(&source, &root.path().join("empty"), |_, _| {
            panic!("an empty file must not probe or disable clone support")
        })?;
        assert!(!copier.clone_unavailable);
        fs::write(&source, b"now populated")?;
        let mut probes = 0;
        copier.copy_with(&source, &root.path().join("populated"), |_, _| {
            probes += 1;
            unavailable()
        })?;
        assert_eq!(probes, 1);
        assert_eq!(copier.stats().copied_files, 2);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn capacity_and_io_errors_are_not_hidden_by_a_copy_retry() -> TestResult {
        let root = tempfile::tempdir()?;
        let source = root.path().join("source");
        fs::write(&source, b"live")?;
        for errno in [
            libc::ENOSPC,
            libc::EIO,
            libc::EDQUOT,
            libc::EACCES,
            libc::EEXIST,
        ] {
            let target = root.path().join(format!("target-{errno}"));
            let mut copier = StagingCopier::default();
            let error = copier
                .copy_with(&source, &target, |_, _| {
                    Ok(CloneAttempt::Unavailable {
                        file: None,
                        error: Some(io::Error::from_raw_os_error(errno)),
                    })
                })
                .unwrap_err();
            assert_eq!(error.raw_os_error(), Some(errno));
            assert!(
                !target.exists(),
                "no fallback may follow an operational failure"
            );
            assert_eq!(copier.stats().copied_files, 0);
            assert_eq!(copier.stats().clone_refusals, 0);
        }
        for errno in [
            libc::EXDEV,
            libc::EOPNOTSUPP,
            libc::ENOSYS,
            libc::ENOTTY,
            libc::EINVAL,
            libc::EPERM,
        ] {
            assert!(clone_is_unavailable(&io::Error::from_raw_os_error(errno)));
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_and_hardlinks_are_not_admitted_as_private_copy_inputs() -> TestResult {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir()?;
        let source = root.path().join("source");
        let link = root.path().join("link");
        fs::write(&source, b"live")?;
        symlink(&source, &link)?;
        assert!(
            StagingCopier::default()
                .copy_file(&link, &root.path().join("symlink-copy"))
                .is_err()
        );
        fs::hard_link(&source, root.path().join("hardlink"))?;
        assert!(
            StagingCopier::default()
                .copy_file(&source, &root.path().join("hardlink-copy"))
                .is_err()
        );
        assert_eq!(fs::read(&source)?, b"live");
        Ok(())
    }

    #[test]
    fn fallback_rejects_short_or_growing_inputs_without_unbounded_reads() {
        let mut buffer = Vec::new();
        let mut output = Vec::new();
        let error = copy_bytes(&mut &b"short"[..], &mut output, 9, &mut buffer).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
        output.clear();
        let error = copy_bytes(&mut &b"longer"[..], &mut output, 4, &mut buffer).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(output, b"long");
        assert!(buffer.len() <= COPY_BUFFER_BYTES);
    }

    #[test]
    fn source_replacement_during_clone_refusal_aborts_the_generation() -> TestResult {
        let root = tempfile::tempdir()?;
        let source = root.path().join("source");
        let displaced = root.path().join("displaced");
        let target = root.path().join("target");
        fs::write(&source, b"old source")?;
        let result = StagingCopier::default().copy_with(&source, &target, |_, _| {
            fs::rename(&source, &displaced)?;
            fs::write(&source, b"replacement of a different length")?;
            unavailable()
        });
        assert!(result.is_err());
        assert_eq!(fs::read(&displaced)?, b"old source");
        assert_eq!(fs::read(&source)?, b"replacement of a different length");
        Ok(())
    }

    #[test]
    fn staged_vector_writer_coexists_with_a_live_reader_without_changing_it() -> TestResult {
        use frankensearch::VectorIndex;

        let root = tempfile::tempdir()?;
        let source = root.path().join("live.idx");
        let target = root.path().join("staged.idx");
        let mut builder = VectorIndex::create(&source, "staging-isolation-control", 4)?;
        builder.write_record("removed", &[1.0, 0.0, 0.0, 0.0])?;
        builder.write_record("kept", &[0.0, 1.0, 0.0, 0.0])?;
        builder.finish()?;
        let before = fs::read(&source)?;
        let reader = VectorIndex::open_read_only(&source)?;
        let query = [1.0, 1.0, 0.0, 0.0];
        let original_hits = reader.search_top_k(&query, 10, None)?;
        assert_eq!(original_hits.len(), 2);
        let mut copier = StagingCopier::default();
        copier.copy_file(&source, &target)?;
        let mut writer = VectorIndex::open(&target)?;
        assert!(writer.soft_delete("removed")?);
        writer.append("added", &[0.0, 0.0, 1.0, 0.0])?;
        writer.compact()?;
        drop(writer);
        let staged = VectorIndex::open_read_only(&target)?;
        let ids = staged.live_doc_ids()?;
        assert!(ids.contains("kept"));
        assert!(ids.contains("added"));
        assert!(!ids.contains("removed"));
        assert_eq!(ids.len(), 2);
        assert_eq!(staged.wal_record_count(), 0);
        assert_eq!(staged.tombstone_count(), 0);
        assert_eq!(reader.search_top_k(&query, 10, None)?, original_hits);
        assert_eq!(fs::read(&source)?, before);
        Ok(())
    }
}
