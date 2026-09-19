"""Wire the reviewed export projection without replacing concurrent source edits."""
from pathlib import Path
import hashlib

path = Path("src/core/backup.rs")
raw = path.read_bytes()
source = raw.decode()
if "evidence_export::redact_evidence(self, level, provenance_admitted)" not in source:
    blob = hashlib.sha1(b"blob " + str(len(raw)).encode() + b"\0" + raw).hexdigest()
    if blob != "14796fc47c8a3f5a453b5ece62bc844c25c01aca":
        raise SystemExit("backup.rs changed; reconcile before applying the export projection")
    anchor = '#[cfg(test)]\n#[path = "backup_history_recovery_tests.rs"]'
    assert source.count(anchor) == 1
    source = source.replace(anchor, '#[path = "backup_evidence_export.rs"]\nmod evidence_export;\n' + anchor, 1)
    begin = source.index('    fn redact_for_export(&mut self, level: RedactionLevel, provenance_admitted: bool) {')
    end = source.index('\n}\n', begin)
    source = source[:begin] + '''    fn redact_for_export(&mut self, level: RedactionLevel, provenance_admitted: bool) {
        evidence_export::redact_evidence(self, level, provenance_admitted);
    }''' + source[end:]
    begin = source.index('fn redact_recovery_identity(key: &str, redaction: RedactionLevel) -> String {')
    end = source.index('\n}\n', begin) + 2
    source = source[:begin] + '''fn redact_recovery_identity(key: &str, redaction: RedactionLevel) -> String {
    evidence_export::redact_identity(key, redaction)
}''' + source[end:]
    path.write_text(source)
