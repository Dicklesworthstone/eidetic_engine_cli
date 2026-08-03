//! T2.1 (`bd-tc-epic-qzk7o.3.2`) hardened mesh key-store contract tests.
//!
//! Exercises the public surface: hardened directory layout under
//! `<workspace>/.ee/keys/mesh/`, owner-only modes, symlink refusal, the
//! stable `mesh_key_store_unavailable` degraded contract, and the reusable
//! [`SecureLocalDir`] primitive T5.9 will consume.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;

use ee::mesh::key_store::{
    KEY_STORE_RECORD_SCHEMA, KeyStoreError, MAX_RECORD_BYTES, MESH_KEY_STORE_UNAVAILABLE_CODE,
    MESH_KEY_STORE_UNAVAILABLE_SEVERITY, MeshKeyStore, PAIR_KEY_LEN, PairKeyClass, SecretBytes,
    SecureLocalDir, mesh_keys_dir,
};

const CREATED_AT: &str = "2026-08-03T00:00:00Z";

fn temp_workspace() -> tempfile::TempDir {
    tempfile::TempDir::new().expect("tempdir")
}

#[test]
fn store_lives_under_workspace_keys_root_with_owner_only_modes() {
    let workspace = temp_workspace();
    let store = MeshKeyStore::open_or_create(workspace.path()).expect("open store");
    let expected = workspace.path().join(".ee").join("keys").join("mesh");
    assert_eq!(store.secure_dir().path(), expected.as_path());
    assert_eq!(mesh_keys_dir(workspace.path()), expected);
    let metadata = std::fs::metadata(&expected).expect("dir metadata");
    assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
}

#[test]
fn round_trip_preserves_key_material_and_binding() {
    let workspace = temp_workspace();
    let store = MeshKeyStore::open_or_create(workspace.path()).expect("open store");
    let key = SecretBytes::new([0xA5; PAIR_KEY_LEN]);
    store
        .store_pair_key("peer-77", PairKeyClass::Current, &key, CREATED_AT, false)
        .expect("store");
    let record = store
        .load_pair_key("peer-77", PairKeyClass::Current)
        .expect("load")
        .expect("present");
    assert_eq!(record.peer_handle, "peer-77");
    assert_eq!(record.key_class, PairKeyClass::Current);
    assert_eq!(record.key.as_bytes(), &[0xA5; PAIR_KEY_LEN]);
    assert_eq!(record.created_at, CREATED_AT);

    let raw = std::fs::read_to_string(store.secure_dir().path().join("pair.peer-77.current.json"))
        .expect("read raw record");
    assert!(raw.contains(KEY_STORE_RECORD_SCHEMA));
    let file_metadata =
        std::fs::metadata(store.secure_dir().path().join("pair.peer-77.current.json"))
            .expect("file metadata");
    assert_eq!(file_metadata.permissions().mode() & 0o777, 0o600);
}

#[test]
fn degraded_contract_is_stable_high_severity_with_repair() {
    assert_eq!(
        MESH_KEY_STORE_UNAVAILABLE_CODE,
        "mesh_key_store_unavailable"
    );
    assert_eq!(MESH_KEY_STORE_UNAVAILABLE_SEVERITY, "high");
    let error = KeyStoreError::PlatformUnsupported {
        operation: "open mesh key store".to_owned(),
    };
    assert_eq!(error.degraded_code(), MESH_KEY_STORE_UNAVAILABLE_CODE);
    assert_eq!(error.severity(), MESH_KEY_STORE_UNAVAILABLE_SEVERITY);
    assert!(error.message().contains("Mesh key store"));
    assert!(error.repair().contains("team commands"));
}

#[test]
fn symlinked_keys_directory_fails_closed() {
    let workspace = temp_workspace();
    let elsewhere = workspace.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).expect("mkdir");
    let keys_dir = mesh_keys_dir(workspace.path());
    std::fs::create_dir_all(keys_dir.parent().expect("parent")).expect("mkdir parents");
    std::os::unix::fs::symlink(&elsewhere, &keys_dir).expect("symlink");
    let error = MeshKeyStore::open_or_create(workspace.path()).expect_err("must refuse");
    assert!(matches!(error, KeyStoreError::SymlinkComponent { .. }));
    assert_eq!(error.degraded_code(), MESH_KEY_STORE_UNAVAILABLE_CODE);
}

#[test]
fn secure_local_dir_primitive_supports_replace_rename_exists() {
    let workspace = temp_workspace();
    let dir =
        SecureLocalDir::open_or_create(workspace.path().join("cache")).expect("open primitive");
    dir.write_exclusive("body.blob.json", b"{\"v\":1}\n")
        .expect("create");
    assert!(dir.exists("body.blob.json").expect("exists"));
    dir.write_replace("body.blob.json", b"{\"v\":2}\n")
        .expect("replace");
    let bytes = dir.read("body.blob.json").expect("read").expect("present");
    assert_eq!(bytes, b"{\"v\":2}\n");
    dir.rename("body.blob.json", "body.blob.retired.json")
        .expect("rename");
    assert!(!dir.exists("body.blob.json").expect("exists after rename"));
    assert!(
        dir.exists("body.blob.retired.json")
            .expect("retired exists")
    );
    assert!(dir.read("missing.json").expect("absent read").is_none());
}

#[test]
fn oversize_and_tampered_records_fail_closed() {
    let workspace = temp_workspace();
    let store = MeshKeyStore::open_or_create(workspace.path()).expect("open store");
    let oversized = vec![b'{'; usize::try_from(MAX_RECORD_BYTES).expect("cap fits") + 1];
    store
        .secure_dir()
        .write_exclusive("pair.peer-77.current.json", &oversized)
        .expect("raw write");
    let error = store
        .load_pair_key("peer-77", PairKeyClass::Current)
        .expect_err("oversize refused");
    assert!(matches!(error, KeyStoreError::CapExceeded { .. }));

    store
        .secure_dir()
        .write_replace("pair.peer-77.current.json", b"not json at all")
        .expect("replace with garbage");
    let error = store
        .load_pair_key("peer-77", PairKeyClass::Current)
        .expect_err("garbage refused");
    assert!(matches!(error, KeyStoreError::Malformed { .. }));
}

#[test]
fn retirement_renames_and_never_deletes() {
    let workspace = temp_workspace();
    let store = MeshKeyStore::open_or_create(workspace.path()).expect("open store");
    let key = SecretBytes::new([3; PAIR_KEY_LEN]);
    store
        .store_pair_key("peer-77", PairKeyClass::Current, &key, CREATED_AT, false)
        .expect("store");
    store
        .retire_pair_key("peer-77", PairKeyClass::Current, "gen-2")
        .expect("retire");
    assert!(
        store
            .load_pair_key("peer-77", PairKeyClass::Current)
            .expect("load after retire")
            .is_none()
    );
    assert!(
        store
            .secure_dir()
            .exists("retired.gen-2.pair.peer-77.current.json")
            .expect("retired record present")
    );
}
