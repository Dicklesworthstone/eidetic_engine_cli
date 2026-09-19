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
