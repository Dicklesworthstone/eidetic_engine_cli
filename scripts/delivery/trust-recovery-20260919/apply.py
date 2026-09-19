"""Connect exact trust-state checks to the existing publication snapshot."""
from pathlib import Path
import hashlib

path = Path("src/core/backup_history_recovery.rs")
raw = path.read_bytes()
source = raw.decode()
if "trust: trust::TrustExpectation," not in source:
    blob = hashlib.sha1(b"blob " + str(len(raw)).encode() + b"\0" + raw).hexdigest()
    if blob != "f7ab03da6a635b281a2b2ab6e80b393ac9ded4c1":
        raise SystemExit("History verifier changed; reconcile before integrating trust checks")
    changes = [
        ('mod packs;', 'mod packs;\n#[path = "backup_trust_recovery.rs"]\nmod trust;'),
        ('    cass: cass::CassExpectation,', '    cass: cass::CassExpectation,\n    trust: trust::TrustExpectation,'),
        ('            cass: cass::CassExpectation::from_assets(assets, workspace_id)?,', '            cass: cass::CassExpectation::from_assets(assets, workspace_id)?,\n            trust: trust::TrustExpectation::from_assets(assets, backup_id, workspace_id)?,'),
        ('        self.cass.verify_connection(db)', '        self.cass.verify_connection(db)?;\n        self.trust.verify_connection(db)'),
    ]
    for old, new in changes:
        assert source.count(old) == 1, old
        source = source.replace(old, new, 1)
    path.write_text(source)

# Opaque targets may outlive their original pack/artifact. Rehashing the
# archive's own canonical reference changes their cross-generation identity.
path = Path("src/core/backup.rs")
source = path.read_text()
start = source.index("fn redact_learning_reference(")
end = source.index("\nfn quarantine_payload_hash(", start)
block = source[start:end]
old = """    if redacted == value {
        redacted
    } else {
        format!("backup-ref:{}", blake3::hash(value.as_bytes()).to_hex())
    }
"""
new = """    // Recovery's own opaque references are already scrubbed identifiers.
    // Preserve only the exact emitted grammar, never an arbitrary prefix.
    let opaque = value.strip_prefix("backup-ref:").is_some_and(|suffix| {
        suffix.len() == 64
            && suffix.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    });
    if redacted == value || opaque {
        value.to_owned()
    } else {
        format!("backup-ref:{}", blake3::hash(value.as_bytes()).to_hex())
    }
"""
if old in block:
    assert block.count(old) == 1
    source = source[:start] + block.replace(old, new, 1) + source[end:]
    path.write_text(source)
else:
    assert 'let opaque = value.strip_prefix("backup-ref:")' in block, "Reference redaction changed"

path = Path("src/core/backup_trust_recovery_tests.rs")
source = path.read_text()
if "mod references;" not in source:
    path.write_text(source + '\n#[path = "backup_reference_recovery_tests.rs"]\nmod references;\n')

# Index publication changes derived jobs and generations but must never change
# admitted authority. Reopen a read-only snapshot after rebuilding succeeds.
path = Path("src/core/backup_history_recovery.rs")
source = path.read_text()
if "mod publication;" not in source:
    anchor = 'mod packs;'
    assert source.count(anchor) == 1
    path.write_text(source.replace(anchor, anchor + '\n#[path = "backup_publication_recovery.rs"]\nmod publication;', 1))

path = Path("src/core/backup.rs")
source = path.read_text()
old = """fn restore_backup_to_side_path_with_verification_hook(
    options: &BackupRestoreOptions,
    before_verification: impl FnOnce(&Path) -> Result<(), DomainError>,
) -> Result<BackupRestoreReport, DomainError> {
    let workspace_path = normalize_path(&options.workspace_path);"""
new = """fn restore_backup_to_side_path_with_verification_hook(
    options: &BackupRestoreOptions,
    before_verification: impl FnOnce(&Path) -> Result<(), DomainError>,
) -> Result<BackupRestoreReport, DomainError> {
    restore_backup_to_side_path_with_recovery_hooks(options, before_verification, |_| Ok(()))
}

fn restore_backup_to_side_path_with_recovery_hooks(
    options: &BackupRestoreOptions,
    before_verification: impl FnOnce(&Path) -> Result<(), DomainError>,
    before_publication: impl FnOnce(&Path) -> Result<(), DomainError>,
) -> Result<BackupRestoreReport, DomainError> {
    let workspace_path = normalize_path(&options.workspace_path);"""
if old in source:
    assert source.count(old) == 1
    source = source.replace(old, new, 1)
else:
    assert "fn restore_backup_to_side_path_with_recovery_hooks(" in source
if "expected_history.verify_before_publication(" not in source:
    anchor = "    let restore_issue_count = import_report"
    assert source.count(anchor) == 1
    source = source.replace(anchor, """    // Rebuilding may consume jobs, but cannot grant or rewrite durable
    // authority. Recheck the admitted history after the last rebuilding stage.
    before_publication(&restored_database_path)?;
    expected_history.verify_before_publication(&restored_database_path)?;

""" + anchor, 1)
path.write_text(source)

path = Path("src/core/backup_trust_recovery_tests.rs")
source = path.read_text()
if "mod publication;" not in source:
    path.write_text(source + '\n#[path = "backup_publication_recovery_tests.rs"]\nmod publication;\n')
