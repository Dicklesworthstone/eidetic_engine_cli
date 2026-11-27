#!/usr/bin/env python3
"""Diagnose index-before-table catalog rows and recover a separate store copy."""

import argparse
import os
import sqlite3
import sys
import tempfile
from contextlib import closing
from pathlib import Path


def catalog(db):
    # The raw catalog is still queryable when normal schema parsing fails.
    # Read the physical rowids using stock SQLite. FrankenSQLite synthesizes
    # rowids for its SQL view of sqlite_master, hiding this particular defect.
    db.execute("PRAGMA writable_schema=ON")
    return db.execute(
        "SELECT rowid, type, name, tbl_name, rootpage, sql FROM sqlite_schema ORDER BY rowid"
    ).fetchall()


def misplaced_indexes(rows):
    # SQLite identifiers are case-insensitive for ASCII only.
    fold = str.maketrans("ABCDEFGHIJKLMNOPQRSTUVWXYZ", "abcdefghijklmnopqrstuvwxyz")
    tables = {name.translate(fold): rid for rid, kind, name, *_ in rows if kind == "table"}
    return [
        (rid, name, owner)
        for rid, kind, name, owner, _, _ in rows
        if kind == "index" and owner.translate(fold) in tables and rid < tables[owner.translate(fold)]
    ]


def check_output(path):
    # Old sidecars could be replayed against an otherwise good recovered file.
    if path.name.endswith(("-wal", "-shm", "-journal")):
        raise ValueError("Choose a database filename outside SQLite's sidecar namespace")
    for suffix in ("", "-wal", "-shm", "-journal"):
        if os.path.lexists(str(path) + suffix):
            raise ValueError(f"Output path or SQLite sidecar already exists: {path}{suffix}")


def read_only(path):
    db = sqlite3.connect(path.resolve().as_uri() + "?mode=ro", uri=True)
    db.execute("PRAGMA trusted_schema=OFF")
    return db


def recover(source, output):
    if sqlite3.sqlite_version_info < (3, 38, 0):
        raise ValueError("SQLite 3.38 or later is required to inspect a malformed catalog")
    if output is not None:
        check_output(output)
    with closing(read_only(source)) as src:
        # Pin both the inspection and online backup to this committed snapshot.
        # mode=ro (without immutable=1) includes committed data in a live WAL.
        src.execute("BEGIN")
        rows = catalog(src)
        indexes = misplaced_indexes(rows)
        if not indexes:
            if output is not None:
                raise ValueError("No index-before-table rows to repair")
            print("No index-before-table rows found.")
            return 0
        for rid, name, owner in indexes:
            print(f"Index {name!r} (catalog row {rid}) precedes its table {owner!r}.")
        if output is None:
            return 1
        with tempfile.TemporaryDirectory(dir=output.parent) as staging:
            copy = Path(staging) / "recovered.db"
            with closing(sqlite3.connect(copy)) as dst:
                src.backup(dst)
                dst.execute("PRAGMA trusted_schema=OFF")
                if catalog(dst) != rows:
                    raise ValueError("Copy and source catalogs differ before repair")
                # A standalone copy must carry every committed page in its main file.
                if dst.execute("PRAGMA journal_mode=DELETE").fetchone() != ("delete",):
                    raise ValueError("Could not produce a standalone journal-free database")
                with dst:
                    # Assign new rowids above every existing object; leave DDL,
                    # index B-trees, and all application tables untouched.
                    last = rows[-1][0]
                    if last > (2**63 - 1) - len(indexes):
                        raise ValueError("Catalog has insufficient unused rowids")
                    for i, (rid, _, _) in enumerate(indexes, 1):
                        cursor = dst.execute(
                            "UPDATE sqlite_schema SET rowid=? WHERE rowid=?", (last + i, rid)
                        )
                        if cursor.rowcount != 1:
                            raise ValueError("Expected exactly one catalog row to move")
            # A fresh ordinary connection must parse the repaired schema and
            # validate its data, indexes and constraints with writable_schema off.
            with closing(read_only(copy)) as checked:
                if checked.execute("PRAGMA integrity_check").fetchall() != [("ok",)]:
                    raise ValueError("Recovered copy failed SQLite integrity_check")
                if checked.execute("PRAGMA foreign_key_check").fetchone() is not None:
                    raise ValueError("Recovered copy failed SQLite foreign_key_check")
                after = catalog(checked)
                if misplaced_indexes(after) or sorted(r[1:] for r in after) != sorted(r[1:] for r in rows):
                    raise ValueError("Recovered catalog failed the schema-preservation check")
            os.chmod(copy, 0o600)
            check_output(output)
            os.link(copy, output)  # atomic publication; refuses an existing destination
    print(f"Verified recovered database: {output}")
    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("database", type=Path)
    parser.add_argument("--output", type=Path, help="Publish the verified copy to a new file")
    args = parser.parse_args()
    try:
        return recover(args.database, args.output)
    except (sqlite3.Error, OSError, ValueError) as error:
        print(f"Catalog recovery failed: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
