import hashlib
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from contextlib import closing
from pathlib import Path


class CatalogRecoveryTest(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.source = Path(directory.name) / "source #1.db"
        self.output = Path(directory.name) / "copy.db"
        self.writer = sqlite3.connect(self.source)
        self.addCleanup(self.writer.close)
        self.writer.executescript(
            "CREATE TABLE curation_candidates_v029 (id TEXT PRIMARY KEY, status TEXT);"
            "CREATE INDEX idx_status ON curation_candidates_v029 (status);"
            "CREATE TABLE history (id TEXT REFERENCES curation_candidates_v029(id));"
            "INSERT INTO curation_candidates_v029 VALUES ('one','pending');"
            "INSERT INTO history VALUES ('one');"
        )

    def reorder(self):
        self.writer.executescript(
            "PRAGMA writable_schema=ON;"
            "UPDATE sqlite_master SET rowid=100 WHERE name='curation_candidates_v029';"
        )
        # New connections must fail stock SQLite's ordinary schema parse.
        with closing(sqlite3.connect(self.source)) as db:
            with self.assertRaisesRegex(sqlite3.DatabaseError, "malformed database schema"):
                db.execute("SELECT count(*) FROM sqlite_schema").fetchone()

    def invoke(self, *extra):
        return subprocess.run(
            [sys.executable, str(Path(__file__).with_name("recover_sqlite_catalog.py")),
             str(self.source), *map(str, extra)],
            text=True, capture_output=True, timeout=15,
        )

    def test_inspection_and_verified_recovery_from_wal_snapshot(self):
        self.writer.execute("PRAGMA journal_mode=WAL")
        self.writer.execute("PRAGMA wal_autocheckpoint=0")
        self.writer.executescript(
            "INSERT INTO curation_candidates_v029 VALUES ('two','approved');"
            "INSERT INTO history VALUES ('two');"
        )
        self.reorder()
        files = [self.source, Path(str(self.source) + "-wal")]
        before = [hashlib.sha256(p.read_bytes()).digest() for p in files]
        diagnosed = self.invoke()
        self.assertEqual(diagnosed.returncode, 1, diagnosed.stderr)
        self.assertIn("idx_status", diagnosed.stdout)
        recovered = self.invoke("--output", self.output)
        self.assertEqual(recovered.returncode, 0, recovered.stderr)
        self.assertEqual(before, [hashlib.sha256(p.read_bytes()).digest() for p in files])
        with closing(sqlite3.connect(self.output)) as db:
            self.assertEqual(db.execute("PRAGMA integrity_check").fetchall(), [("ok",)])
            self.assertEqual(db.execute("PRAGMA foreign_key_check").fetchall(), [])
            self.assertEqual(
                db.execute("SELECT id FROM curation_candidates_v029 INDEXED BY idx_status "
                           "WHERE status='approved'").fetchall(), [("two",)]
            )
            self.assertEqual(db.execute("SELECT id FROM history ORDER BY id").fetchall(),
                             [("one",), ("two",)])

    def test_unrelated_integrity_failure_refuses_publication(self):
        self.writer.execute("INSERT INTO history VALUES ('missing')")
        self.reorder()
        run = self.invoke("--output", self.output)
        self.assertEqual(run.returncode, 2)
        self.assertIn("foreign_key_check", run.stderr)
        self.assertFalse(self.output.exists())

    def test_healthy_catalog_and_protected_destinations(self):
        self.assertEqual(self.invoke().returncode, 0)
        self.assertEqual(self.invoke("--output", self.output).returncode, 2)
        self.assertFalse(self.output.exists())
        self.reorder()
        self.output.write_bytes(b"existing")
        self.assertEqual(self.invoke("--output", self.output).returncode, 2)
        self.assertEqual(self.output.read_bytes(), b"existing")
        other = self.output.with_name("other.db")
        Path(str(other) + "-wal").write_bytes(b"unrelated")
        self.assertEqual(self.invoke("--output", other).returncode, 2)
        self.assertFalse(other.exists())


if __name__ == "__main__":
    unittest.main()
