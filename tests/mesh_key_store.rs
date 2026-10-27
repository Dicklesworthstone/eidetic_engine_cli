//! T2.1 (`bd-tc-epic-qzk7o.3.2`) hardened mesh key-store contract tests.
//!
//! Exercises the public surface: hardened directory layout under
//! `<workspace>/.ee/keys/mesh/`, owner-only modes, symlink refusal, the
//! stable `mesh_key_store_unavailable` degraded contract, and the reusable
//! [`SecureLocalDir`] primitive T5.9 will consume.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(all(
    unix,
    any(
        target_os = "linux",
        target_os = "android",
        target_os = "redox",
        target_vendor = "apple"
    )
))]

use std::num::NonZeroU64;
use std::os::unix::fs::PermissionsExt;

use ee::mesh::key_store::{
    KEY_STORE_RECORD_SCHEMA, KeyStoreError, KeyStoreFailureClass, MAX_RECORD_BYTES,
    MESH_KEY_STORE_UNAVAILABLE_CODE, MESH_KEY_STORE_UNAVAILABLE_SEVERITY,
    MeshCredentialStorePlatform, MeshKeyStore, PAIR_KEY_LEN, PairKeyClass, SecretBytes,
    SecureLocalDir, mesh_credential_store_platform, mesh_keys_dir,
    require_mesh_credential_store_platform,
};

const CREATED_AT: &str = "2026-08-03T00:00:00Z";

fn generation(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).expect("test generation must be nonzero")
}

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
        .store_pair_key(
            "peer-77",
            PairKeyClass::Current,
            generation(7),
            &key,
            CREATED_AT,
            false,
        )
        .expect("store");
    let record = store
        .load_pair_key("peer-77", PairKeyClass::Current)
        .expect("load")
        .expect("present");
    assert_eq!(record.peer_handle, "peer-77");
    assert_eq!(record.generation, generation(7));
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
    let guidance = error.guidance();
    assert_eq!(guidance.code, MESH_KEY_STORE_UNAVAILABLE_CODE);
    assert_eq!(guidance.severity, MESH_KEY_STORE_UNAVAILABLE_SEVERITY);
    assert_eq!(guidance.class, KeyStoreFailureClass::PlatformUnsupported);
    assert!(guidance.credential_operation_blocked);
    assert!(guidance.ordinary_local_commands_available);
}

#[test]
fn compiled_platform_contract_names_only_the_reviewed_unix_adapter() {
    assert_eq!(
        mesh_credential_store_platform(),
        MeshCredentialStorePlatform::HardenedUnix
    );
    assert_eq!(
        require_mesh_credential_store_platform("test credential operation")
            .expect("Unix adapter is compiled"),
        MeshCredentialStorePlatform::HardenedUnix
    );
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
    // Print the variant on failure. The control demonstrably fails closed here
    // (`expect_err` succeeds), so the only open question is which variant it
    // chose — and a bare `matches!` leaves that unanswerable from a CI log.
    assert!(
        matches!(error, KeyStoreError::SymlinkComponent { .. }),
        "symlinked keys directory must fail closed as SymlinkComponent, got {error:?}"
    );
    assert_eq!(error.degraded_code(), MESH_KEY_STORE_UNAVAILABLE_CODE);
    assert_eq!(error.guidance().class, KeyStoreFailureClass::PathSafety);
    assert!(error.guidance().credential_operation_blocked);
}

#[test]
fn opened_store_rejects_planted_intermediate_parent_substitution() {
    let workspace = temp_workspace();
    let store = MeshKeyStore::open_or_create(workspace.path()).expect("open store");
    let marker = workspace.path().join(".ee");
    let keys = marker.join("keys");
    let moved_keys = marker.join("keys-real");
    std::fs::rename(&keys, &moved_keys).expect("move real keys directory");
    std::os::unix::fs::symlink(&moved_keys, &keys).expect("plant keys-parent symlink");

    let error = store
        .load_pair_key("peer-77", PairKeyClass::Current)
        .expect_err("opened store must rewalk and refuse substituted parent");

    assert!(matches!(error, KeyStoreError::SymlinkComponent { .. }));
    assert_eq!(error.guidance().class, KeyStoreFailureClass::PathSafety);
}

#[test]
fn secure_local_dir_primitive_supports_replace_rename_exists() {
    let workspace = temp_workspace();
    let dir = SecureLocalDir::open_or_create(workspace.path(), workspace.path().join("cache"))
        .expect("open primitive");
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
fn exclusive_publish_ignores_crash_retained_temp_and_exposes_only_complete_record() {
    let workspace = temp_workspace();
    let store = MeshKeyStore::open_or_create(workspace.path()).expect("open store");
    let planted_name = ".pair.peer-77.current.json.tmp.crash-retained";
    let planted_path = store.secure_dir().path().join(planted_name);
    std::fs::write(&planted_path, b"planted-incomplete-record").expect("plant crash-retained temp");
    std::fs::set_permissions(&planted_path, std::fs::Permissions::from_mode(0o600))
        .expect("make planted temp owner-only");

    let key = SecretBytes::new([0xA5; PAIR_KEY_LEN]);
    store
        .store_pair_key(
            "peer-77",
            PairKeyClass::Current,
            generation(7),
            &key,
            CREATED_AT,
            false,
        )
        .expect("atomically publish complete pair record");

    let record = store
        .load_pair_key("peer-77", PairKeyClass::Current)
        .expect("load published record")
        .expect("published record exists");
    assert_eq!(record.key.as_bytes(), &[0xA5; PAIR_KEY_LEN]);
    assert_eq!(
        std::fs::read(&planted_path).expect("planted temp remains isolated"),
        b"planted-incomplete-record"
    );
    let live_temp_count = std::fs::read_dir(store.secure_dir().path())
        .expect("read key directory")
        .filter_map(Result::ok)
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.starts_with(".pair.peer-77.current.json.tmp.") && name.as_ref() != planted_name
        })
        .count();
    assert_eq!(live_temp_count, 0, "successful publish left a live temp");
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

    // bd-key-store-write-replace-dead-end-1kgjt: this phase used to
    // `write_replace` the oversized record planted above, and it could never
    // run. `write_replace` validates the DESTINATION — it stats the resident
    // file and calls `verify_record_stat`, which rejects the 65537-byte record
    // — so the replacement bytes were never evaluated and the `.expect()`
    // panicked with `CapExceeded` reporting the RESIDENT file's length. The
    // malformed-path assertion below was unreachable.
    //
    // Refusing to operate on an anomalous file is the intended posture, not a
    // bug: no supported path produces an oversized record. The one production
    // caller that hands unbounded bytes to the uncapped `write_replace` is
    // `share_team_bodies_represented`, bounded by `models::MAX_CONTENT_BYTES`
    // being equal to `MAX_RECORD_BYTES` (now asserted in key_store's own
    // tests), and the remote-body route through `apply_fetched_team_body` has
    // no production caller at all.
    //
    // So the malformed record goes under a FRESH name. The store is not asked
    // to repair an anomalous file, and the tampered-record property this test
    // is named for is finally exercised.
    store
        .secure_dir()
        .write_exclusive("pair.peer-78.current.json", b"not json at all")
        .expect("plant malformed record under a fresh name");
    let error = store
        .load_pair_key("peer-78", PairKeyClass::Current)
        .expect_err("garbage refused");
    assert!(matches!(error, KeyStoreError::Malformed { .. }));

    // The oversized record is still there and still refused: the fresh-name
    // write must not be read as having repaired it.
    let error = store
        .load_pair_key("peer-77", PairKeyClass::Current)
        .expect_err("oversize still refused");
    assert!(matches!(error, KeyStoreError::CapExceeded { .. }));
}

#[test]
fn retirement_renames_and_never_deletes() {
    let workspace = temp_workspace();
    let store = MeshKeyStore::open_or_create(workspace.path()).expect("open store");
    let key = SecretBytes::new([3; PAIR_KEY_LEN]);
    store
        .store_pair_key(
            "peer-77",
            PairKeyClass::Current,
            generation(7),
            &key,
            CREATED_AT,
            false,
        )
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
