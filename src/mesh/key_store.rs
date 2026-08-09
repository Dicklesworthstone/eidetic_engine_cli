//! Hardened mesh credential key store (T2.1, `bd-tc-epic-qzk7o.3.2`).
//!
//! ADR 0086 TC-D5 owns the key-storage contract this module implements:
//!
//! - Unix: `0700` key directory, `0600` files, opened **without following
//!   symlinks** (`O_NOFOLLOW`), owner/type checks performed on the *opened*
//!   file descriptor, atomic write+rename, and both file **and directory**
//!   `fsync`.
//! - Windows ("client-only" does not mean weaker storage): requires a reviewed
//!   reparse-safe, opened-identity-pinned, non-inherited-DACL write-through
//!   adapter. That adapter has not shipped, so every non-Unix operation fails
//!   closed with [`MESH_KEY_STORE_UNAVAILABLE_CODE`] (severity `high`) and
//!   credential-bearing team commands are blocked; ordinary local `ee`
//!   commands are unaffected.
//!
//! The file-safety layer is deliberately exposed as a narrow reusable
//! primitive ([`SecureLocalDir`]) rather than a key-store special case so the
//! T5.9 body-cache publication path can consume it instead of duplicating
//! platform logic.
//!
//! Layout: records live under `<workspace>/.ee/keys/mesh/`. Pair-key documents
//! use [`KEY_STORE_RECORD_SCHEMA`]; local node signing-seed documents use
//! [`SIGNING_KEY_STORE_RECORD_SCHEMA`]. Retirement renames records in place
//! (`retired.<label>.<name>`); nothing in this module ever deletes a file.

use std::fmt;
use std::io::{Read as _, Write as _};
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering, compiler_fence};

use serde::{Deserialize, Serialize};

/// Stable degraded code emitted when the hardened mesh key store cannot be
/// established, verified, or safely used. Severity is always `high`:
/// credential-bearing team surfaces must fail closed rather than fall back to
/// weaker storage.
pub const MESH_KEY_STORE_UNAVAILABLE_CODE: &str = "mesh_key_store_unavailable";

/// Severity for every [`MESH_KEY_STORE_UNAVAILABLE_CODE`] emission.
pub const MESH_KEY_STORE_UNAVAILABLE_SEVERITY: &str = "high";

/// Schema identifier pinned inside every key-store record document.
pub const KEY_STORE_RECORD_SCHEMA: &str = "ee.mesh.key_store.record.v2";

/// Schema identifier pinned inside every local origin-signing-key record.
/// Signing lineage transitions are authorized by T3.6; this schema only
/// stores the already-authorized current or staged-next private seed.
pub const SIGNING_KEY_STORE_RECORD_SCHEMA: &str = "ee.mesh.key_store.signing_record.v1";

/// Width shared by mesh pair keys and Ed25519 private signing seeds.
pub const SECRET_BYTES_LEN: usize = 32;

/// Pair keys are 32 bytes (ADR 0086 TC-D5).
pub const PAIR_KEY_LEN: usize = SECRET_BYTES_LEN;

/// Ed25519 signing seeds are exactly 32 bytes (ADR 0086 TC-D4).
pub const SIGNING_KEY_SEED_LEN: usize = SECRET_BYTES_LEN;

/// Hard cap on any single key-store record file. Matches the store-auth
/// key-file cap; a larger file is treated as corruption, not data.
pub const MAX_RECORD_BYTES: u64 = 64 * 1024;

/// Maximum accepted length for peer handles and retirement labels.
const MAX_NAME_COMPONENT_LEN: usize = 64;

#[cfg(unix)]
static SECURE_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// Hardened credential-storage adapter compiled for this target.
///
/// This reports implementation availability only. It does not claim that an
/// individual path is safe; every open and record operation still performs
/// descriptor-relative path, owner, type, mode, and identity verification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MeshCredentialStorePlatform {
    /// The reviewed Unix `openat`/`O_NOFOLLOW` + owner/mode + atomic
    /// no-replace publication + fsync adapter is present.
    HardenedUnix,
    /// No reviewed adapter with equivalent guarantees is present.
    Unsupported,
}

/// Return the credential-store posture of the compiled target without
/// touching the filesystem.
#[must_use]
pub const fn mesh_credential_store_platform() -> MeshCredentialStorePlatform {
    if cfg!(all(
        unix,
        any(
            target_os = "linux",
            target_os = "android",
            target_os = "redox",
            target_vendor = "apple"
        )
    )) {
        MeshCredentialStorePlatform::HardenedUnix
    } else {
        MeshCredentialStorePlatform::Unsupported
    }
}

/// Gate a credential-bearing operation on a reviewed platform adapter.
/// Windows and every other unsupported target fail closed; callers must not
/// substitute ordinary file APIs or shell-based ACL repair.
pub fn require_mesh_credential_store_platform(
    operation: impl Into<String>,
) -> Result<MeshCredentialStorePlatform, KeyStoreError> {
    let platform = mesh_credential_store_platform();
    match platform {
        MeshCredentialStorePlatform::HardenedUnix => Ok(platform),
        MeshCredentialStorePlatform::Unsupported => Err(KeyStoreError::PlatformUnsupported {
            operation: operation.into(),
        }),
    }
}

/// Canonical on-disk location of a workspace's mesh credential directory.
/// Shares the `.ee/keys` root with the store-authentication root so operators
/// have exactly one hardened keys tree per workspace.
#[must_use]
pub fn mesh_keys_dir(workspace_path: &Path) -> PathBuf {
    crate::policy::store_auth::workspace_keys_dir(workspace_path).join("mesh")
}

/// Fail-closed error surface for the mesh key store. Every variant maps to
/// [`MESH_KEY_STORE_UNAVAILABLE_CODE`] at severity `high`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum KeyStoreError {
    /// Underlying filesystem I/O failed.
    Io {
        /// Path the operation was acting on.
        path: String,
        /// Operating-system error text.
        message: String,
    },
    /// A path component is a symbolic link (or an open was refused by
    /// `O_NOFOLLOW`). Symlinked key material is never trusted.
    SymlinkComponent {
        /// The offending path.
        path: String,
    },
    /// Directory or file permissions grant access beyond the owner.
    InsecurePermissions {
        /// The offending path.
        path: String,
        /// Human-readable detail naming the observed mode.
        detail: String,
    },
    /// The directory or file is owned by a different user.
    ForeignOwner {
        /// The offending path.
        path: String,
        /// Observed owner uid.
        uid: u32,
        /// Effective uid of this process.
        euid: u32,
    },
    /// The path exists but is not a regular file / directory as required.
    WrongFileType {
        /// The offending path.
        path: String,
        /// What was required ("regular file" / "directory").
        expected: &'static str,
    },
    /// A record file exceeds [`MAX_RECORD_BYTES`].
    CapExceeded {
        /// The offending path.
        path: String,
        /// Observed length in bytes.
        len: u64,
    },
    /// Record contents failed validation (schema, hex, field bounds).
    Malformed {
        /// Human-readable description of the malformation.
        message: String,
    },
    /// Exclusive creation failed because the record already exists.
    AlreadyExists {
        /// The offending path.
        path: String,
    },
    /// No reviewed hardened-storage adapter exists for this platform, so the
    /// key store refuses to operate at all (fail closed, never fall back).
    PlatformUnsupported {
        /// The operation that was refused.
        operation: String,
    },
}

/// Stable failure class for callers that must decide whether credential
/// operations remain blocked without parsing human prose.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyStoreFailureClass {
    /// File or directory I/O/durability could not be established.
    IoOrDurability,
    /// A path component, entry type, or opened identity was unsafe.
    PathSafety,
    /// Owner or permission/ACL protection was insufficient.
    AccessControl,
    /// Stored bytes violated the bounded canonical record contract.
    RecordIntegrity,
    /// An exclusive publication or retirement target already existed.
    Conflict,
    /// The compiled target has no reviewed hardened adapter.
    PlatformUnsupported,
}

/// Structured fail-closed guidance shared by key-store consumers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeyStoreFailureGuidance {
    /// Stable degraded code.
    pub code: &'static str,
    /// Stable severity.
    pub severity: &'static str,
    /// Typed root failure class.
    pub class: KeyStoreFailureClass,
    /// Whether the attempted credential-bearing operation was refused.
    pub credential_operation_blocked: bool,
    /// Ordinary commands outside the mesh credential boundary remain usable.
    pub ordinary_local_commands_available: bool,
    /// Bounded operator repair guidance.
    pub repair: String,
}

impl KeyStoreError {
    /// The stable degraded code for this failure.
    #[must_use]
    pub const fn degraded_code(&self) -> &'static str {
        MESH_KEY_STORE_UNAVAILABLE_CODE
    }

    /// The stable severity for this failure.
    #[must_use]
    pub const fn severity(&self) -> &'static str {
        MESH_KEY_STORE_UNAVAILABLE_SEVERITY
    }

    /// Typed failure posture for downstream command/status renderers. Every
    /// key-store failure blocks the current credential operation; ordinary
    /// non-team commands remain outside this credential boundary.
    #[must_use]
    pub fn guidance(&self) -> KeyStoreFailureGuidance {
        let class = match self {
            Self::Io { .. } => KeyStoreFailureClass::IoOrDurability,
            Self::SymlinkComponent { .. } | Self::WrongFileType { .. } => {
                KeyStoreFailureClass::PathSafety
            }
            Self::InsecurePermissions { .. } | Self::ForeignOwner { .. } => {
                KeyStoreFailureClass::AccessControl
            }
            Self::CapExceeded { .. } | Self::Malformed { .. } => {
                KeyStoreFailureClass::RecordIntegrity
            }
            Self::AlreadyExists { .. } => KeyStoreFailureClass::Conflict,
            Self::PlatformUnsupported { .. } => KeyStoreFailureClass::PlatformUnsupported,
        };
        KeyStoreFailureGuidance {
            code: self.degraded_code(),
            severity: self.severity(),
            class,
            credential_operation_blocked: true,
            ordinary_local_commands_available: true,
            repair: self.repair(),
        }
    }

    /// Human-readable message. Always names the mesh key store so agents can
    /// attribute the failure without consulting source.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::Io { path, message } => {
                format!("Mesh key store I/O failed at {path}: {message}")
            }
            Self::SymlinkComponent { path } => {
                format!("Mesh key store refused {path}: path is (or traverses) a symbolic link")
            }
            Self::InsecurePermissions { path, detail } => {
                format!("Mesh key store refused {path}: {detail}")
            }
            Self::ForeignOwner { path, uid, euid } => format!(
                "Mesh key store refused {path}: owned by uid {uid}, expected effective uid {euid}"
            ),
            Self::WrongFileType { path, expected } => {
                format!("Mesh key store refused {path}: not a {expected}")
            }
            Self::CapExceeded { path, len } => format!(
                "Mesh key store refused {path}: {len} bytes exceeds the {MAX_RECORD_BYTES}-byte record cap"
            ),
            Self::Malformed { message } => {
                format!("Mesh key store record is malformed: {message}")
            }
            Self::AlreadyExists { path } => {
                format!("Mesh key store record already exists at {path}")
            }
            Self::PlatformUnsupported { operation } => format!(
                "Mesh key store refused {operation}: no reviewed hardened credential-storage adapter exists for this platform; credential-bearing team commands are blocked"
            ),
        }
    }

    /// Repair guidance paired with every emission of
    /// [`MESH_KEY_STORE_UNAVAILABLE_CODE`].
    #[must_use]
    pub fn repair(&self) -> String {
        match self {
            Self::SymlinkComponent { .. } => {
                "Replace the symlinked path with a real owner-only directory or file under <workspace>/.ee/keys/mesh, then retry.".to_owned()
            }
            Self::InsecurePermissions { .. } => {
                "Restore owner-only permissions (0700 directory, 0600 files) under <workspace>/.ee/keys/mesh, then retry.".to_owned()
            }
            Self::ForeignOwner { .. } => {
                "Restore ownership of <workspace>/.ee/keys/mesh to the user running ee, then retry.".to_owned()
            }
            Self::AlreadyExists { .. } => {
                "Use an atomic replace (rotation) path instead of exclusive creation, or retire the existing record first.".to_owned()
            }
            Self::PlatformUnsupported { .. } => {
                "Run credential-bearing team commands from a platform with a reviewed hardened key-store adapter (Unix today); ordinary local ee commands remain available.".to_owned()
            }
            Self::Io { .. }
            | Self::WrongFileType { .. }
            | Self::CapExceeded { .. }
            | Self::Malformed { .. } => {
                "Inspect <workspace>/.ee/keys/mesh for corruption; restore the directory from a trusted state or re-enroll the affected peer.".to_owned()
            }
        }
    }
}

impl fmt::Display for KeyStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for KeyStoreError {}

/// A 32-byte pair key or Ed25519 signing seed with a redacted `Debug` and
/// best-effort `Drop`
/// zeroization (slice fill + compiler fence; `#![forbid(unsafe_code)]` rules
/// out volatile writes, mirroring the store-auth root's `Secret`).
pub struct SecretBytes([u8; SECRET_BYTES_LEN]);

impl SecretBytes {
    /// Wrap raw key material.
    #[must_use]
    pub const fn new(bytes: [u8; SECRET_BYTES_LEN]) -> Self {
        Self(bytes)
    }

    /// Borrow the raw key material.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; SECRET_BYTES_LEN] {
        &self.0
    }
}

impl fmt::Debug for SecretBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretBytes(<redacted>)")
    }
}

impl Drop for SecretBytes {
    fn drop(&mut self) {
        self.0.fill(0);
        compiler_fence(Ordering::SeqCst);
    }
}

/// Which pair key a record holds. `Current` authenticates live sessions;
/// `Next` exists only during the two-phase rotation window (T3.2).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PairKeyClass {
    /// The active pair key.
    Current,
    /// The staged next key during rotation.
    Next,
}

impl PairKeyClass {
    /// Stable on-disk token for this class.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Next => "next",
        }
    }
}

/// Which local origin-signing seed a record holds. `Current` signs new origin
/// events; `Next` is staged before T3.6's dual-signed, hash-linked transition.
/// Merely storing or replacing a slot does not authorize a lineage change.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SigningKeyClass {
    /// The active local signing seed.
    Current,
    /// A staged next seed awaiting an authorized lineage transition.
    Next,
}

impl SigningKeyClass {
    /// Stable on-disk token for this class.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Next => "next",
        }
    }
}

/// Serde field wrapper whose backing allocation is wiped on every drop,
/// including partial-deserialization error paths.
#[derive(Deserialize, Serialize)]
#[serde(transparent)]
struct SecretHex(String);

impl SecretHex {
    fn new(value: String) -> Self {
        Self(value)
    }

    fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

impl Drop for SecretHex {
    fn drop(&mut self) {
        wipe_secret_string(&mut self.0);
    }
}

/// On-disk record document (schema [`KEY_STORE_RECORD_SCHEMA`]). Field names
/// are part of the stored contract; unknown fields are rejected so a tampered
/// or future-version record fails closed instead of partially loading.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RecordDocument {
    schema: String,
    peer_handle: String,
    key_class: String,
    generation: NonZeroU64,
    key_hex: SecretHex,
    created_at: String,
}

/// Versioned local origin-signing-key document. Generation uses
/// [`NonZeroU64`] so zero is rejected during deserialization as well as by the
/// public store API. Unknown fields fail closed.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SigningRecordDocument {
    schema: String,
    node_handle: String,
    key_class: String,
    generation: NonZeroU64,
    seed_hex: SecretHex,
    created_at: String,
}

/// A loaded pair-key record.
pub struct PairKeyRecord {
    /// Opaque enrolled peer handle the key belongs to.
    pub peer_handle: String,
    /// Which rotation slot the key occupies.
    pub key_class: PairKeyClass,
    /// Nonzero pair-key generation bound into the authenticated session.
    pub generation: NonZeroU64,
    /// The 32-byte pair key.
    pub key: SecretBytes,
    /// Caller-supplied RFC 3339 creation timestamp (informational; rotation
    /// grace policy lands with T3.2).
    pub created_at: String,
}

impl fmt::Debug for PairKeyRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PairKeyRecord")
            .field("peer_handle", &self.peer_handle)
            .field("key_class", &self.key_class)
            .field("generation", &self.generation)
            .field("key", &"<redacted>")
            .field("created_at", &self.created_at)
            .finish()
    }
}

/// A loaded local origin-signing-key record.
pub struct SigningKeyRecord {
    /// Opaque random ee node handle whose origin lineage owns this seed.
    pub node_handle: String,
    /// Which lifecycle slot the seed occupies.
    pub key_class: SigningKeyClass,
    /// Nonzero signing-lineage generation bound into origin events.
    pub generation: NonZeroU64,
    /// Raw 32-byte Ed25519 signing seed. Callers construct the pinned
    /// `ed25519_dalek::SigningKey` only at the T2.0 signing boundary.
    pub seed: SecretBytes,
    /// Caller-supplied RFC 3339 creation timestamp.
    pub created_at: String,
}

impl fmt::Debug for SigningKeyRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SigningKeyRecord")
            .field("node_handle", &self.node_handle)
            .field("key_class", &self.key_class)
            .field("generation", &self.generation)
            .field("seed", &"<redacted>")
            .field("created_at", &self.created_at)
            .finish()
    }
}

/// Narrow hardened-directory primitive: owner-only directory of owner-only
/// regular files, symlink-refusing opens, atomic replace, file + directory
/// fsync. This is the reusable piece T5.9's body cache consumes.
///
/// On non-Unix platforms construction fails closed with
/// [`KeyStoreError::PlatformUnsupported`].
#[derive(Debug)]
pub struct SecureLocalDir {
    boundary: PathBuf,
    dir: PathBuf,
    #[cfg(unix)]
    dir_handle: std::fs::File,
}

#[cfg(unix)]
impl SecureLocalDir {
    /// Open (creating if needed) a hardened owner-only directory beneath a
    /// trusted boundary. Every existing component strictly beneath `boundary`
    /// is checked without following symlinks before creation, and the same
    /// component walk is repeated by every record operation. Newly created
    /// directories use mode `0700`; an existing directory with any other mode
    /// fails closed and is never silently chmodded.
    pub fn open_or_create(
        boundary: impl AsRef<Path>,
        dir: impl AsRef<Path>,
    ) -> Result<Self, KeyStoreError> {
        require_mesh_credential_store_platform("open hardened local directory")?;
        let boundary = boundary.as_ref();
        let dir = dir.as_ref();
        let dir_handle =
            open_secure_directory(boundary, dir, true)?.ok_or_else(|| KeyStoreError::Io {
                path: dir.display().to_string(),
                message: "directory remained absent after descriptor-relative creation".to_owned(),
            })?;
        let this = Self {
            boundary: boundary.to_path_buf(),
            dir: dir.to_path_buf(),
            dir_handle,
        };
        this.verify_dir()?;
        Ok(this)
    }

    /// Open an existing hardened owner-only directory without creating or
    /// chmodding any path. `Ok(None)` means one or more components beneath the
    /// trusted boundary are absent. Unsafe existing components fail closed.
    pub fn open_existing(
        boundary: impl AsRef<Path>,
        dir: impl AsRef<Path>,
    ) -> Result<Option<Self>, KeyStoreError> {
        require_mesh_credential_store_platform("open existing hardened local directory")?;
        let boundary = boundary.as_ref();
        let dir = dir.as_ref();
        let Some(dir_handle) = open_secure_directory(boundary, dir, false)? else {
            return Ok(None);
        };
        let this = Self {
            boundary: boundary.to_path_buf(),
            dir: dir.to_path_buf(),
            dir_handle,
        };
        this.verify_dir()?;
        Ok(Some(this))
    }

    /// The hardened directory path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.dir
    }

    fn verify_dir(&self) -> Result<(), KeyStoreError> {
        use rustix::fs::FileType;
        let opened = rustix::fs::fstat(&self.dir_handle).map_err(|error| KeyStoreError::Io {
            path: self.dir.display().to_string(),
            message: format!("inspect opened secure directory: {error}"),
        })?;
        verify_directory_stat(&opened, &self.dir, true)?;
        let named_handle =
            open_secure_directory(&self.boundary, &self.dir, false)?.ok_or_else(|| {
                KeyStoreError::Io {
                    path: self.dir.display().to_string(),
                    message:
                        "secure directory path no longer resolves beneath its trusted boundary"
                            .to_owned(),
                }
            })?;
        let named = rustix::fs::fstat(&named_handle).map_err(|error| KeyStoreError::Io {
            path: self.dir.display().to_string(),
            message: format!("inspect re-opened secure directory: {error}"),
        })?;
        if FileType::from_raw_mode(named.st_mode) != FileType::Directory
            || named.st_dev != opened.st_dev
            || named.st_ino != opened.st_ino
        {
            return Err(KeyStoreError::SymlinkComponent {
                path: self.dir.display().to_string(),
            });
        }
        Ok(())
    }

    /// Open a record with `O_NOFOLLOW` and verify owner/type/mode/size on the
    /// opened descriptor before reading. `Ok(None)` when the record is absent.
    pub fn read(&self, name: &str) -> Result<Option<Vec<u8>>, KeyStoreError> {
        use rustix::fs::{Mode, OFlags};
        validate_file_name(name)?;
        self.verify_dir()?;
        let path = self.dir.join(name);
        let descriptor = match rustix::fs::openat(
            &self.dir_handle,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0),
        ) {
            Ok(descriptor) => descriptor,
            Err(error) if error == rustix::io::Errno::NOENT => return Ok(None),
            Err(error) if error == rustix::io::Errno::LOOP => {
                return Err(KeyStoreError::SymlinkComponent {
                    path: path.display().to_string(),
                });
            }
            Err(error) => {
                return Err(KeyStoreError::Io {
                    path: path.display().to_string(),
                    message: format!("descriptor-relative record open: {error}"),
                });
            }
        };
        let mut file = std::fs::File::from(descriptor);
        self.verify_open_file(&file, &path)?;
        let mut bytes = Vec::new();
        if let Err(error) = (&mut file)
            .take(MAX_RECORD_BYTES.saturating_add(1))
            .read_to_end(&mut bytes)
        {
            bytes.fill(0);
            compiler_fence(Ordering::SeqCst);
            return Err(KeyStoreError::Io {
                path: path.display().to_string(),
                message: error.to_string(),
            });
        }
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_RECORD_BYTES {
            bytes.fill(0);
            compiler_fence(Ordering::SeqCst);
            return Err(KeyStoreError::CapExceeded {
                path: path.display().to_string(),
                len: MAX_RECORD_BYTES.saturating_add(1),
            });
        }
        Ok(Some(bytes))
    }

    fn verify_open_file(&self, file: &std::fs::File, path: &Path) -> Result<(), KeyStoreError> {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let metadata = file.metadata().map_err(|error| KeyStoreError::Io {
            path: path.display().to_string(),
            message: error.to_string(),
        })?;
        if !metadata.is_file() {
            return Err(KeyStoreError::WrongFileType {
                path: path.display().to_string(),
                expected: "regular file",
            });
        }
        let mode = metadata.permissions().mode() & 0o777;
        if mode != 0o600 {
            return Err(KeyStoreError::InsecurePermissions {
                path: path.display().to_string(),
                detail: format!("file mode {mode:04o}, expected exactly 0600"),
            });
        }
        let euid = rustix::process::geteuid().as_raw();
        if metadata.uid() != euid {
            return Err(KeyStoreError::ForeignOwner {
                path: path.display().to_string(),
                uid: metadata.uid(),
                euid,
            });
        }
        if metadata.nlink() != 1 {
            return Err(KeyStoreError::WrongFileType {
                path: path.display().to_string(),
                expected: "single-link regular file",
            });
        }
        if metadata.len() > MAX_RECORD_BYTES {
            return Err(KeyStoreError::CapExceeded {
                path: path.display().to_string(),
                len: metadata.len(),
            });
        }
        Ok(())
    }

    /// Exclusively publish a record through a unique `0600` temp sibling,
    /// file fsync, no-replace rename, and directory fsync. A crash-retained
    /// temp is never a visible record and cannot wedge a later unique attempt.
    pub fn write_exclusive(&self, name: &str, bytes: &[u8]) -> Result<(), KeyStoreError> {
        use rustix::fs::{AtFlags, Mode, OFlags};
        validate_file_name(name)?;
        self.verify_dir()?;
        let path = self.dir.join(name);
        match rustix::fs::statat(&self.dir_handle, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => {
                verify_record_stat(&stat, &path)?;
                return Err(KeyStoreError::AlreadyExists {
                    path: path.display().to_string(),
                });
            }
            Err(error) if error == rustix::io::Errno::NOENT => {}
            Err(error) => {
                return Err(key_store_errno(
                    &path,
                    "inspect exclusive destination",
                    error,
                ));
            }
        }
        let sequence = SECURE_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let tmp_name = format!(".{name}.tmp.{}.{}", std::process::id(), sequence);
        let tmp = self.dir.join(&tmp_name);
        let descriptor = rustix::fs::openat(
            &self.dir_handle,
            tmp_name.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|error| {
            if error == rustix::io::Errno::EXIST {
                KeyStoreError::AlreadyExists {
                    path: tmp.display().to_string(),
                }
            } else {
                KeyStoreError::Io {
                    path: tmp.display().to_string(),
                    message: error.to_string(),
                }
            }
        })?;
        let mut file = std::fs::File::from(descriptor);
        file.write_all(bytes).map_err(|error| KeyStoreError::Io {
            path: tmp.display().to_string(),
            message: error.to_string(),
        })?;
        file.sync_all().map_err(|error| KeyStoreError::Io {
            path: tmp.display().to_string(),
            message: error.to_string(),
        })?;
        drop(file);
        publish_noreplace(&self.dir_handle, tmp_name.as_str(), name, &path)?;
        self.sync_dir()
    }

    /// Atomically replace a record via an exclusively created, hardened temp
    /// sibling + rename, then fsync the file and the directory. Every attempt
    /// uses a unique temp name, so a crash-retained temp cannot wedge later
    /// rotations and is never truncated or reused.
    pub fn write_replace(&self, name: &str, bytes: &[u8]) -> Result<(), KeyStoreError> {
        use rustix::fs::{AtFlags, Mode, OFlags};
        validate_file_name(name)?;
        self.verify_dir()?;
        let sequence = SECURE_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let tmp_name = format!(".{name}.tmp.{}.{}", std::process::id(), sequence);
        let tmp = self.dir.join(&tmp_name);
        let path = self.dir.join(name);
        match rustix::fs::statat(&self.dir_handle, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => verify_record_stat(&stat, &path)?,
            Err(error) if error == rustix::io::Errno::NOENT => {}
            Err(error) => {
                return Err(key_store_errno(
                    &path,
                    "inspect replacement destination",
                    error,
                ));
            }
        }
        let descriptor = rustix::fs::openat(
            &self.dir_handle,
            tmp_name.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|error| {
            if error == rustix::io::Errno::EXIST {
                KeyStoreError::AlreadyExists {
                    path: tmp.display().to_string(),
                }
            } else {
                KeyStoreError::Io {
                    path: tmp.display().to_string(),
                    message: error.to_string(),
                }
            }
        })?;
        let mut file = std::fs::File::from(descriptor);
        file.write_all(bytes).map_err(|error| KeyStoreError::Io {
            path: tmp.display().to_string(),
            message: error.to_string(),
        })?;
        file.sync_all().map_err(|error| KeyStoreError::Io {
            path: tmp.display().to_string(),
            message: error.to_string(),
        })?;
        drop(file);
        rustix::fs::renameat(&self.dir_handle, tmp_name.as_str(), &self.dir_handle, name).map_err(
            |error| KeyStoreError::Io {
                path: path.display().to_string(),
                message: format!("atomic replace: {error}"),
            },
        )?;
        self.sync_dir()
    }

    /// Rename a record in place (used for retirement; never deletes).
    pub fn rename(&self, from: &str, to: &str) -> Result<(), KeyStoreError> {
        use rustix::fs::RenameFlags;
        validate_file_name(from)?;
        validate_file_name(to)?;
        self.verify_dir()?;
        let from_path = self.dir.join(from);
        let to_path = self.dir.join(to);
        rustix::fs::renameat_with(
            &self.dir_handle,
            from,
            &self.dir_handle,
            to,
            RenameFlags::NOREPLACE,
        )
        .map_err(|error| {
            if error == rustix::io::Errno::EXIST {
                KeyStoreError::AlreadyExists {
                    path: to_path.display().to_string(),
                }
            } else {
                KeyStoreError::Io {
                    path: from_path.display().to_string(),
                    message: format!("descriptor-relative no-replace rename: {error}"),
                }
            }
        })?;
        self.sync_dir()
    }

    /// Whether a record exists (without following symlinks).
    pub fn exists(&self, name: &str) -> Result<bool, KeyStoreError> {
        use rustix::fs::AtFlags;
        validate_file_name(name)?;
        self.verify_dir()?;
        let path = self.dir.join(name);
        match rustix::fs::statat(&self.dir_handle, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => {
                verify_record_stat(&stat, &path)?;
                Ok(true)
            }
            Err(error) if error == rustix::io::Errno::NOENT => Ok(false),
            Err(error) => Err(key_store_errno(&path, "inspect record existence", error)),
        }
    }

    fn sync_dir(&self) -> Result<(), KeyStoreError> {
        self.dir_handle
            .sync_all()
            .map_err(|error| KeyStoreError::Io {
                path: self.dir.display().to_string(),
                message: format!("directory fsync: {error}"),
            })
    }
}

#[cfg(not(unix))]
impl SecureLocalDir {
    /// Non-Unix platforms have no reviewed hardened-storage adapter yet; the
    /// store fails closed (ADR 0086 TC-D5).
    pub fn open_or_create(
        _boundary: impl AsRef<Path>,
        _dir: impl AsRef<Path>,
    ) -> Result<Self, KeyStoreError> {
        Err(KeyStoreError::PlatformUnsupported {
            operation: "open mesh key store".to_owned(),
        })
    }

    /// Non-Unix platforms have no reviewed hardened-storage adapter yet; even
    /// a non-mutating credential-store lookup fails closed.
    pub fn open_existing(
        _boundary: impl AsRef<Path>,
        _dir: impl AsRef<Path>,
    ) -> Result<Option<Self>, KeyStoreError> {
        Err(KeyStoreError::PlatformUnsupported {
            operation: "open existing mesh key store".to_owned(),
        })
    }

    /// The hardened directory path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.dir
    }

    /// Fails closed on non-Unix platforms.
    pub fn read(&self, _name: &str) -> Result<Option<Vec<u8>>, KeyStoreError> {
        Err(KeyStoreError::PlatformUnsupported {
            operation: "read mesh key record".to_owned(),
        })
    }

    /// Fails closed on non-Unix platforms.
    pub fn write_exclusive(&self, _name: &str, _bytes: &[u8]) -> Result<(), KeyStoreError> {
        Err(KeyStoreError::PlatformUnsupported {
            operation: "create mesh key record".to_owned(),
        })
    }

    /// Fails closed on non-Unix platforms.
    pub fn write_replace(&self, _name: &str, _bytes: &[u8]) -> Result<(), KeyStoreError> {
        Err(KeyStoreError::PlatformUnsupported {
            operation: "replace mesh key record".to_owned(),
        })
    }

    /// Fails closed on non-Unix platforms.
    pub fn rename(&self, _from: &str, _to: &str) -> Result<(), KeyStoreError> {
        Err(KeyStoreError::PlatformUnsupported {
            operation: "rename mesh key record".to_owned(),
        })
    }

    /// Fails closed on non-Unix platforms.
    pub fn exists(&self, _name: &str) -> Result<bool, KeyStoreError> {
        Err(KeyStoreError::PlatformUnsupported {
            operation: "inspect mesh key record".to_owned(),
        })
    }
}

/// The mesh credential store: pair-key records for enrolled peers, kept in a
/// [`SecureLocalDir`] under `<workspace>/.ee/keys/mesh/`.
#[derive(Debug)]
pub struct MeshKeyStore {
    dir: SecureLocalDir,
}

impl MeshKeyStore {
    /// Open (creating if needed) the workspace's mesh credential store.
    pub fn open_or_create(workspace_path: &Path) -> Result<Self, KeyStoreError> {
        require_mesh_credential_store_platform("open mesh key store")?;
        Ok(Self {
            dir: SecureLocalDir::open_or_create(workspace_path, mesh_keys_dir(workspace_path))?,
        })
    }

    /// Open the workspace's mesh credential store without creating or
    /// chmodding anything. Returns `Ok(None)` when the store path is absent;
    /// existing unsafe components still fail closed.
    pub fn open_existing(workspace_path: &Path) -> Result<Option<Self>, KeyStoreError> {
        require_mesh_credential_store_platform("open existing mesh key store")?;
        let Some(dir) =
            SecureLocalDir::open_existing(workspace_path, mesh_keys_dir(workspace_path))?
        else {
            return Ok(None);
        };
        Ok(Some(Self { dir }))
    }

    /// The hardened directory backing this store.
    #[must_use]
    pub fn secure_dir(&self) -> &SecureLocalDir {
        &self.dir
    }

    /// Store a pair key for `(peer_handle, class)`. With `overwrite` false the
    /// record is claimed exclusively; with `overwrite` true it is replaced
    /// atomically (rotation staging).
    pub fn store_pair_key(
        &self,
        peer_handle: &str,
        class: PairKeyClass,
        generation: NonZeroU64,
        key: &SecretBytes,
        created_at: &str,
        overwrite: bool,
    ) -> Result<(), KeyStoreError> {
        validate_name_component(peer_handle, "peer handle")?;
        validate_created_at(created_at)?;
        let document = RecordDocument {
            schema: KEY_STORE_RECORD_SCHEMA.to_owned(),
            peer_handle: peer_handle.to_owned(),
            key_class: class.token().to_owned(),
            generation,
            key_hex: SecretHex::new(hex_lower(key.as_bytes())),
            created_at: created_at.to_owned(),
        };
        let serialized = serde_json::to_vec_pretty(&document);
        drop(document);
        let mut bytes = serialized.map_err(|error| KeyStoreError::Malformed {
            message: format!("serialize record: {error}"),
        })?;
        bytes.push(b'\n');
        let name = record_file_name(peer_handle, class);
        let result = if overwrite {
            self.dir.write_replace(&name, &bytes)
        } else {
            self.dir.write_exclusive(&name, &bytes)
        };
        bytes.fill(0);
        compiler_fence(Ordering::SeqCst);
        result
    }

    /// Load the pair key for `(peer_handle, class)`. `Ok(None)` when absent.
    pub fn load_pair_key(
        &self,
        peer_handle: &str,
        class: PairKeyClass,
    ) -> Result<Option<PairKeyRecord>, KeyStoreError> {
        validate_name_component(peer_handle, "peer handle")?;
        let name = record_file_name(peer_handle, class);
        let Some(mut bytes) = self.dir.read(&name)? else {
            return Ok(None);
        };
        let parsed: Result<RecordDocument, _> = serde_json::from_slice(&bytes);
        bytes.fill(0);
        compiler_fence(Ordering::SeqCst);
        let document = parsed.map_err(|_| KeyStoreError::Malformed {
            message: "pair-key record is not valid canonical JSON".to_owned(),
        })?;
        if document.schema != KEY_STORE_RECORD_SCHEMA {
            return Err(KeyStoreError::Malformed {
                message: "unexpected pair-key record schema".to_owned(),
            });
        }
        if document.peer_handle != peer_handle {
            return Err(KeyStoreError::Malformed {
                message: "pair-key record peer handle does not match its requested path".to_owned(),
            });
        }
        if document.key_class != class.token() {
            return Err(KeyStoreError::Malformed {
                message: "pair-key record key class does not match its requested path".to_owned(),
            });
        }
        validate_created_at(&document.created_at)?;
        let key = decode_secret_hex(document.key_hex.as_bytes(), "key_hex")?;
        Ok(Some(PairKeyRecord {
            peer_handle: document.peer_handle,
            key_class: class,
            generation: document.generation,
            key,
            created_at: document.created_at,
        }))
    }

    /// Store a local origin-signing seed for `(node_handle, class)`. The
    /// nonzero generation and slot are persisted and checked on load. This is
    /// a storage primitive only: `overwrite` does not authorize or claim the
    /// dual-signed lineage transition required by ADR 0086 TC-D5/T3.6.
    pub fn store_signing_key(
        &self,
        node_handle: &str,
        class: SigningKeyClass,
        generation: NonZeroU64,
        seed: &SecretBytes,
        created_at: &str,
        overwrite: bool,
    ) -> Result<(), KeyStoreError> {
        validate_name_component(node_handle, "node handle")?;
        validate_created_at(created_at)?;
        let document = SigningRecordDocument {
            schema: SIGNING_KEY_STORE_RECORD_SCHEMA.to_owned(),
            node_handle: node_handle.to_owned(),
            key_class: class.token().to_owned(),
            generation,
            seed_hex: SecretHex::new(hex_lower(seed.as_bytes())),
            created_at: created_at.to_owned(),
        };
        let serialized = serde_json::to_vec_pretty(&document);
        drop(document);
        let mut bytes = serialized.map_err(|error| KeyStoreError::Malformed {
            message: format!("serialize signing record: {error}"),
        })?;
        bytes.push(b'\n');
        let name = signing_record_file_name(node_handle, class);
        let result = if overwrite {
            self.dir.write_replace(&name, &bytes)
        } else {
            self.dir.write_exclusive(&name, &bytes)
        };
        bytes.fill(0);
        compiler_fence(Ordering::SeqCst);
        result
    }

    /// Load the local origin-signing seed for `(node_handle, class)`.
    /// `Ok(None)` means the slot is absent. Schema, node, class, nonzero
    /// generation, seed width, owner, mode, and file type all fail closed.
    pub fn load_signing_key(
        &self,
        node_handle: &str,
        class: SigningKeyClass,
    ) -> Result<Option<SigningKeyRecord>, KeyStoreError> {
        validate_name_component(node_handle, "node handle")?;
        let name = signing_record_file_name(node_handle, class);
        let Some(mut bytes) = self.dir.read(&name)? else {
            return Ok(None);
        };
        let parsed: Result<SigningRecordDocument, _> = serde_json::from_slice(&bytes);
        bytes.fill(0);
        compiler_fence(Ordering::SeqCst);
        let document = parsed.map_err(|_| KeyStoreError::Malformed {
            message: "signing record is not valid canonical JSON".to_owned(),
        })?;
        if document.schema != SIGNING_KEY_STORE_RECORD_SCHEMA {
            return Err(KeyStoreError::Malformed {
                message: "unexpected signing record schema".to_owned(),
            });
        }
        if document.node_handle != node_handle {
            return Err(KeyStoreError::Malformed {
                message: "signing record node handle does not match its requested path".to_owned(),
            });
        }
        if document.key_class != class.token() {
            return Err(KeyStoreError::Malformed {
                message: "signing record key class does not match its requested path".to_owned(),
            });
        }
        validate_created_at(&document.created_at)?;
        let seed = decode_secret_hex(document.seed_hex.as_bytes(), "seed_hex")?;
        Ok(Some(SigningKeyRecord {
            node_handle: document.node_handle,
            key_class: class,
            generation: document.generation,
            seed,
            created_at: document.created_at,
        }))
    }

    /// Retire a record by renaming it to `retired.<label>.<original-name>`.
    /// Nothing is deleted; retired records stay auditable on disk.
    pub fn retire_pair_key(
        &self,
        peer_handle: &str,
        class: PairKeyClass,
        label: &str,
    ) -> Result<(), KeyStoreError> {
        validate_name_component(peer_handle, "peer handle")?;
        validate_name_component(label, "retirement label")?;
        let name = record_file_name(peer_handle, class);
        let retired = format!("retired.{label}.{name}");
        self.dir.rename(&name, &retired)
    }

    /// Retire a local signing-key slot by rename. This does not assert that a
    /// lineage transition was valid; the T3.6 caller must authorize that
    /// transition before invoking this storage operation.
    pub fn retire_signing_key(
        &self,
        node_handle: &str,
        class: SigningKeyClass,
        label: &str,
    ) -> Result<(), KeyStoreError> {
        validate_name_component(node_handle, "node handle")?;
        validate_name_component(label, "retirement label")?;
        let name = signing_record_file_name(node_handle, class);
        let retired = format!("retired.{label}.{name}");
        self.dir.rename(&name, &retired)
    }
}

fn record_file_name(peer_handle: &str, class: PairKeyClass) -> String {
    format!("pair.{peer_handle}.{}.json", class.token())
}

fn signing_record_file_name(node_handle: &str, class: SigningKeyClass) -> String {
    format!("signing.{node_handle}.{}.json", class.token())
}

fn validate_name_component(value: &str, label: &str) -> Result<(), KeyStoreError> {
    if value.is_empty() || value.len() > MAX_NAME_COMPONENT_LEN {
        return Err(KeyStoreError::Malformed {
            message: format!(
                "{label} must be 1..={MAX_NAME_COMPONENT_LEN} characters, got {}",
                value.len()
            ),
        });
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(KeyStoreError::Malformed {
            message: format!("{label} may only contain ASCII alphanumerics, '-', and '_'"),
        });
    }
    Ok(())
}

fn validate_file_name(name: &str) -> Result<(), KeyStoreError> {
    if name.is_empty() || name.len() > 4 * MAX_NAME_COMPONENT_LEN {
        return Err(KeyStoreError::Malformed {
            message: "record file name length is out of bounds".to_owned(),
        });
    }
    if name.starts_with('.')
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(KeyStoreError::Malformed {
            message: format!("record file name {name:?} contains forbidden characters"),
        });
    }
    Ok(())
}

fn validate_created_at(created_at: &str) -> Result<(), KeyStoreError> {
    if created_at.is_empty() || created_at.len() > 64 {
        return Err(KeyStoreError::Malformed {
            message: "created_at must be a short RFC 3339 timestamp".to_owned(),
        });
    }
    chrono::DateTime::parse_from_rfc3339(created_at).map_err(|_| KeyStoreError::Malformed {
        message: "created_at must be a valid RFC 3339 timestamp".to_owned(),
    })?;
    Ok(())
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from_digit(u32::from(byte >> 4), 16).unwrap_or('0'));
        out.push(char::from_digit(u32::from(byte & 0x0f), 16).unwrap_or('0'));
    }
    out
}

fn decode_secret_hex(value: &[u8], label: &str) -> Result<SecretBytes, KeyStoreError> {
    if value.len() != SECRET_BYTES_LEN * 2 {
        return Err(KeyStoreError::Malformed {
            message: format!(
                "{label} must be {} characters, got {}",
                SECRET_BYTES_LEN * 2,
                value.len()
            ),
        });
    }
    let mut bytes = [0_u8; SECRET_BYTES_LEN];
    for (index, chunk) in value.chunks_exact(2).enumerate() {
        let high = match hex_nibble(chunk[0], label) {
            Ok(value) => value,
            Err(error) => {
                bytes.fill(0);
                compiler_fence(Ordering::SeqCst);
                return Err(error);
            }
        };
        let low = match hex_nibble(chunk[1], label) {
            Ok(value) => value,
            Err(error) => {
                bytes.fill(0);
                compiler_fence(Ordering::SeqCst);
                return Err(error);
            }
        };
        bytes[index] = (high << 4) | low;
    }
    Ok(SecretBytes::new(bytes))
}

fn hex_nibble(byte: u8, label: &str) -> Result<u8, KeyStoreError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(KeyStoreError::Malformed {
            message: format!("{label} contains a non-lowercase-hex character"),
        }),
    }
}

fn wipe_secret_string(value: &mut String) {
    let mut bytes = std::mem::take(value).into_bytes();
    bytes.fill(0);
    compiler_fence(Ordering::SeqCst);
}

/// Open every descendant below the trusted workspace boundary with
/// descriptor-relative `O_NOFOLLOW` traversal. Existing descendants must be
/// process-owned and not group/other writable; the final secure directory is
/// exactly `0700`. Newly created directory entries are parent-fsynced before
/// traversal continues.
#[cfg(unix)]
fn open_secure_directory(
    boundary: &Path,
    target: &Path,
    create: bool,
) -> Result<Option<std::fs::File>, KeyStoreError> {
    use std::path::Component;

    use rustix::fs::{Mode, OFlags};

    let relative = target
        .strip_prefix(boundary)
        .map_err(|_| KeyStoreError::Malformed {
            message: format!(
                "secure directory {} is outside trusted boundary {}",
                target.display(),
                boundary.display()
            ),
        })?;
    let mut components = relative.components().peekable();
    if components.peek().is_none() {
        return Err(KeyStoreError::Malformed {
            message: "secure directory must be a strict descendant of its boundary".to_owned(),
        });
    }
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let boundary_descriptor =
        rustix::fs::openat(rustix::fs::CWD, boundary, flags, Mode::from_raw_mode(0))
            .map_err(|error| key_store_errno(boundary, "open trusted boundary", error))?;
    let mut directory = std::fs::File::from(boundary_descriptor);
    let mut current_path = boundary.to_path_buf();

    while let Some(component) = components.next() {
        let Component::Normal(component) = component else {
            return Err(KeyStoreError::Malformed {
                message: "secure directory path contains a non-normal component".to_owned(),
            });
        };
        current_path.push(component);
        let observed_before_open =
            inspect_secure_directory_component(&directory, component, &current_path)?;
        let descriptor =
            match rustix::fs::openat(&directory, component, flags, Mode::from_raw_mode(0)) {
                Ok(descriptor) => descriptor,
                Err(error)
                    if error == rustix::io::Errno::NOENT && observed_before_open.is_some() =>
                {
                    return Err(KeyStoreError::SymlinkComponent {
                        path: current_path.display().to_string(),
                    });
                }
                Err(error) if error == rustix::io::Errno::NOENT && !create => return Ok(None),
                Err(error) if error == rustix::io::Errno::NOENT => {
                    match rustix::fs::mkdirat(&directory, component, Mode::from_raw_mode(0o700)) {
                        Ok(()) => directory
                            .sync_all()
                            .map_err(|sync_error| KeyStoreError::Io {
                                path: current_path.display().to_string(),
                                message: format!(
                                    "fsync parent after directory creation: {sync_error}"
                                ),
                            })?,
                        Err(create_error) if create_error == rustix::io::Errno::EXIST => {}
                        Err(create_error) => {
                            return Err(key_store_errno(
                                &current_path,
                                "create secure directory component",
                                create_error,
                            ));
                        }
                    }
                    rustix::fs::openat(&directory, component, flags, Mode::from_raw_mode(0))
                        .map_err(|open_error| {
                            secure_directory_component_open_error(
                                &directory,
                                component,
                                &current_path,
                                "open newly created secure directory component",
                                open_error,
                            )
                        })?
                }
                Err(error) => {
                    return Err(secure_directory_component_open_error(
                        &directory,
                        component,
                        &current_path,
                        "open secure directory component",
                        error,
                    ));
                }
            };
        let child = std::fs::File::from(descriptor);
        let stat = rustix::fs::fstat(&child).map_err(|error| {
            key_store_errno(&current_path, "inspect secure directory component", error)
        })?;
        if let Some(observed) = observed_before_open
            && (observed.st_dev != stat.st_dev || observed.st_ino != stat.st_ino)
        {
            return Err(KeyStoreError::SymlinkComponent {
                path: current_path.display().to_string(),
            });
        }
        verify_directory_stat(&stat, &current_path, components.peek().is_none())?;
        directory = child;
    }
    Ok(Some(directory))
}

#[cfg(unix)]
fn inspect_secure_directory_component(
    parent: &std::fs::File,
    component: &std::ffi::OsStr,
    path: &Path,
) -> Result<Option<rustix::fs::Stat>, KeyStoreError> {
    use rustix::fs::{AtFlags, FileType};

    match rustix::fs::statat(parent, component, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) if FileType::from_raw_mode(stat.st_mode) == FileType::Symlink => {
            Err(KeyStoreError::SymlinkComponent {
                path: path.display().to_string(),
            })
        }
        Ok(stat) if FileType::from_raw_mode(stat.st_mode) != FileType::Directory => {
            Err(KeyStoreError::WrongFileType {
                path: path.display().to_string(),
                expected: "directory",
            })
        }
        Ok(stat) => Ok(Some(stat)),
        Err(error) if error == rustix::io::Errno::NOENT => Ok(None),
        Err(error) => Err(key_store_errno(
            path,
            "inspect secure directory component before open",
            error,
        )),
    }
}

#[cfg(unix)]
fn secure_directory_component_open_error(
    parent: &std::fs::File,
    component: &std::ffi::OsStr,
    path: &Path,
    operation: &str,
    error: rustix::io::Errno,
) -> KeyStoreError {
    use rustix::fs::{AtFlags, FileType};

    if matches!(error, rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR) {
        match rustix::fs::statat(parent, component, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) if FileType::from_raw_mode(stat.st_mode) == FileType::Symlink => {
                return KeyStoreError::SymlinkComponent {
                    path: path.display().to_string(),
                };
            }
            Ok(_) if error == rustix::io::Errno::NOTDIR => {
                return KeyStoreError::WrongFileType {
                    path: path.display().to_string(),
                    expected: "directory",
                };
            }
            Ok(_) | Err(_) => {}
        }
    }
    key_store_errno(path, operation, error)
}

#[cfg(unix)]
fn verify_directory_stat(
    stat: &rustix::fs::Stat,
    path: &Path,
    exact_owner_only: bool,
) -> Result<(), KeyStoreError> {
    use rustix::fs::FileType;

    if FileType::from_raw_mode(stat.st_mode) != FileType::Directory {
        return Err(KeyStoreError::WrongFileType {
            path: path.display().to_string(),
            expected: "directory",
        });
    }
    let mode = stat.st_mode & 0o777;
    if (exact_owner_only && mode != 0o700) || (!exact_owner_only && mode & 0o022 != 0) {
        let expectation = if exact_owner_only {
            "expected exactly 0700"
        } else {
            "ancestor is group/other writable"
        };
        return Err(KeyStoreError::InsecurePermissions {
            path: path.display().to_string(),
            detail: format!("directory mode {mode:04o}, {expectation}"),
        });
    }
    let euid = rustix::process::geteuid().as_raw();
    if stat.st_uid != euid {
        return Err(KeyStoreError::ForeignOwner {
            path: path.display().to_string(),
            uid: stat.st_uid,
            euid,
        });
    }
    Ok(())
}

#[cfg(unix)]
fn verify_record_stat(stat: &rustix::fs::Stat, path: &Path) -> Result<(), KeyStoreError> {
    use rustix::fs::FileType;

    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile || stat.st_nlink != 1 {
        return Err(KeyStoreError::WrongFileType {
            path: path.display().to_string(),
            expected: "single-link regular file",
        });
    }
    let mode = stat.st_mode & 0o777;
    if mode != 0o600 {
        return Err(KeyStoreError::InsecurePermissions {
            path: path.display().to_string(),
            detail: format!("file mode {mode:04o}, expected exactly 0600"),
        });
    }
    let euid = rustix::process::geteuid().as_raw();
    if stat.st_uid != euid {
        return Err(KeyStoreError::ForeignOwner {
            path: path.display().to_string(),
            uid: stat.st_uid,
            euid,
        });
    }
    let len = u64::try_from(stat.st_size).unwrap_or(u64::MAX);
    if len > MAX_RECORD_BYTES {
        return Err(KeyStoreError::CapExceeded {
            path: path.display().to_string(),
            len,
        });
    }
    Ok(())
}

#[cfg(all(
    unix,
    any(
        target_os = "linux",
        target_os = "android",
        target_os = "redox",
        target_vendor = "apple"
    )
))]
fn publish_noreplace(
    dir: &std::fs::File,
    from: &str,
    to: &str,
    destination: &Path,
) -> Result<(), KeyStoreError> {
    use rustix::fs::RenameFlags;

    rustix::fs::renameat_with(dir, from, dir, to, RenameFlags::NOREPLACE).map_err(|error| {
        if error == rustix::io::Errno::EXIST {
            KeyStoreError::AlreadyExists {
                path: destination.display().to_string(),
            }
        } else {
            KeyStoreError::Io {
                path: destination.display().to_string(),
                message: format!("atomic exclusive publish: {error}"),
            }
        }
    })
}

#[cfg(all(
    unix,
    not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "redox",
        target_vendor = "apple"
    ))
))]
fn publish_noreplace(
    _dir: &std::fs::File,
    _from: &str,
    _to: &str,
    _destination: &Path,
) -> Result<(), KeyStoreError> {
    Err(KeyStoreError::PlatformUnsupported {
        operation: "atomically publish mesh key record without replacement".to_owned(),
    })
}

#[cfg(unix)]
fn key_store_errno(path: &Path, operation: &str, error: rustix::io::Errno) -> KeyStoreError {
    if error == rustix::io::Errno::LOOP {
        KeyStoreError::SymlinkComponent {
            path: path.display().to_string(),
        }
    } else {
        KeyStoreError::Io {
            path: path.display().to_string(),
            message: format!("{operation}: {error}"),
        }
    }
}

#[cfg(all(
    test,
    unix,
    any(
        target_os = "linux",
        target_os = "android",
        target_os = "redox",
        target_vendor = "apple"
    )
))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn temp_workspace() -> tempfile::TempDir {
        tempfile::TempDir::new().expect("tempdir")
    }

    fn sample_key(fill: u8) -> SecretBytes {
        SecretBytes::new([fill; PAIR_KEY_LEN])
    }

    fn generation(value: u64) -> NonZeroU64 {
        NonZeroU64::new(value).expect("test generation must be nonzero")
    }

    const CREATED_AT: &str = "2026-08-03T00:00:00Z";

    #[test]
    fn open_or_create_creates_directory_at_exact_owner_only_mode() {
        let workspace = temp_workspace();
        let store = MeshKeyStore::open_or_create(workspace.path()).expect("open store");
        let metadata = std::fs::metadata(store.secure_dir().path()).expect("dir metadata");
        assert!(metadata.is_dir());
        assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
    }

    #[test]
    fn open_existing_absence_is_strictly_non_mutating() {
        let workspace = temp_workspace();
        let marker = workspace.path().join(".ee");
        std::fs::create_dir(&marker).expect("create existing marker");
        std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o751))
            .expect("set distinctive marker mode");

        let store = MeshKeyStore::open_existing(workspace.path()).expect("inspect absent store");

        assert!(store.is_none());
        assert_eq!(
            std::fs::symlink_metadata(&marker)
                .expect("marker metadata")
                .permissions()
                .mode()
                & 0o777,
            0o751,
            "non-mutating lookup must not chmod an existing parent"
        );
        assert!(
            std::fs::symlink_metadata(marker.join("keys")).is_err(),
            "non-mutating lookup must not create the missing keys tree"
        );
    }

    #[test]
    fn opening_insecure_existing_store_fails_without_repairing_it() {
        let workspace = temp_workspace();
        let store = MeshKeyStore::open_or_create(workspace.path()).expect("create store");
        std::fs::set_permissions(
            store.secure_dir().path(),
            std::fs::Permissions::from_mode(0o750),
        )
        .expect("loosen store mode");

        for error in [
            MeshKeyStore::open_or_create(workspace.path())
                .expect_err("mutating open must not repair an existing store"),
            MeshKeyStore::open_existing(workspace.path())
                .expect_err("non-mutating open must fail closed"),
        ] {
            assert!(matches!(error, KeyStoreError::InsecurePermissions { .. }));
        }
        assert_eq!(
            std::fs::symlink_metadata(mesh_keys_dir(workspace.path()))
                .expect("store metadata")
                .permissions()
                .mode()
                & 0o777,
            0o750,
            "neither open path may silently chmod the store"
        );
    }

    #[test]
    fn store_and_load_round_trips() {
        let workspace = temp_workspace();
        let store = MeshKeyStore::open_or_create(workspace.path()).expect("open store");
        store
            .store_pair_key(
                "peer-a1",
                PairKeyClass::Current,
                generation(1),
                &sample_key(7),
                CREATED_AT,
                false,
            )
            .expect("store key");
        let record = store
            .load_pair_key("peer-a1", PairKeyClass::Current)
            .expect("load key")
            .expect("record present");
        assert_eq!(record.peer_handle, "peer-a1");
        assert_eq!(record.key_class, PairKeyClass::Current);
        assert_eq!(record.generation, generation(1));
        assert_eq!(record.key.as_bytes(), &[7; PAIR_KEY_LEN]);
        assert_eq!(record.created_at, CREATED_AT);
    }

    #[test]
    fn signing_slots_round_trip_nonzero_generations_and_owner_only_seeds() {
        let workspace = temp_workspace();
        let store = MeshKeyStore::open_or_create(workspace.path()).expect("open store");
        store
            .store_signing_key(
                "node-local-1",
                SigningKeyClass::Current,
                generation(1),
                &sample_key(0xA5),
                CREATED_AT,
                false,
            )
            .expect("store current signing seed");
        store
            .store_signing_key(
                "node-local-1",
                SigningKeyClass::Next,
                generation(2),
                &sample_key(0x5A),
                CREATED_AT,
                false,
            )
            .expect("stage next signing seed");

        let current = store
            .load_signing_key("node-local-1", SigningKeyClass::Current)
            .expect("load current")
            .expect("current present");
        let next = store
            .load_signing_key("node-local-1", SigningKeyClass::Next)
            .expect("load next")
            .expect("next present");
        assert_eq!(current.node_handle, "node-local-1");
        assert_eq!(current.key_class, SigningKeyClass::Current);
        assert_eq!(current.generation, generation(1));
        assert_eq!(current.seed.as_bytes(), &[0xA5; SIGNING_KEY_SEED_LEN]);
        assert_eq!(next.key_class, SigningKeyClass::Next);
        assert_eq!(next.generation, generation(2));
        assert_eq!(next.seed.as_bytes(), &[0x5A; SIGNING_KEY_SEED_LEN]);

        for class in [SigningKeyClass::Current, SigningKeyClass::Next] {
            let path = store
                .secure_dir()
                .path()
                .join(signing_record_file_name("node-local-1", class));
            let metadata = std::fs::symlink_metadata(path).expect("signing record metadata");
            assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        }
    }

    #[test]
    fn signing_rotation_storage_keeps_current_and_staged_slots_explicit() {
        let workspace = temp_workspace();
        let store = MeshKeyStore::open_or_create(workspace.path()).expect("open store");
        store
            .store_signing_key(
                "node-local-1",
                SigningKeyClass::Current,
                generation(1),
                &sample_key(1),
                CREATED_AT,
                false,
            )
            .expect("store current");
        store
            .store_signing_key(
                "node-local-1",
                SigningKeyClass::Next,
                generation(2),
                &sample_key(2),
                CREATED_AT,
                false,
            )
            .expect("stage next");

        store
            .store_signing_key(
                "node-local-1",
                SigningKeyClass::Current,
                generation(2),
                &sample_key(2),
                CREATED_AT,
                true,
            )
            .expect("caller-authorized atomic current replacement");
        let current = store
            .load_signing_key("node-local-1", SigningKeyClass::Current)
            .expect("load promoted current")
            .expect("current present");
        let staged = store
            .load_signing_key("node-local-1", SigningKeyClass::Next)
            .expect("load staged recovery slot")
            .expect("staged slot remains present");
        assert_eq!(current.generation, generation(2));
        assert_eq!(staged.generation, generation(2));
        assert_eq!(current.seed.as_bytes(), staged.seed.as_bytes());
        let live_temp_count = std::fs::read_dir(store.secure_dir().path())
            .expect("read key directory")
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".signing.node-local-1.current.json.tmp.")
            })
            .count();
        assert_eq!(
            live_temp_count, 0,
            "successful atomic replacement must not leave its unique temp sibling"
        );

        store
            .retire_signing_key("node-local-1", SigningKeyClass::Next, "transition-2")
            .expect("retire staged slot after external transition authorization");
        assert!(
            store
                .load_signing_key("node-local-1", SigningKeyClass::Next)
                .expect("load retired slot")
                .is_none()
        );
        assert!(
            store
                .secure_dir()
                .exists("retired.transition-2.signing.node-local-1.next.json")
                .expect("retired signing record exists")
        );
    }

    #[test]
    fn record_files_are_owner_only() {
        let workspace = temp_workspace();
        let store = MeshKeyStore::open_or_create(workspace.path()).expect("open store");
        store
            .store_pair_key(
                "peer-a1",
                PairKeyClass::Current,
                generation(1),
                &sample_key(1),
                CREATED_AT,
                false,
            )
            .expect("store key");
        let path = store.secure_dir().path().join("pair.peer-a1.current.json");
        let metadata = std::fs::metadata(&path).expect("file metadata");
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn absent_record_loads_as_none() {
        let workspace = temp_workspace();
        let store = MeshKeyStore::open_or_create(workspace.path()).expect("open store");
        assert!(
            store
                .load_pair_key("peer-a1", PairKeyClass::Next)
                .expect("load")
                .is_none()
        );
    }

    #[test]
    fn exclusive_create_refuses_second_writer() {
        let workspace = temp_workspace();
        let store = MeshKeyStore::open_or_create(workspace.path()).expect("open store");
        store
            .store_pair_key(
                "peer-a1",
                PairKeyClass::Current,
                generation(1),
                &sample_key(1),
                CREATED_AT,
                false,
            )
            .expect("first store");
        let error = store
            .store_pair_key(
                "peer-a1",
                PairKeyClass::Current,
                generation(2),
                &sample_key(2),
                CREATED_AT,
                false,
            )
            .expect_err("second exclusive store must fail");
        assert!(matches!(error, KeyStoreError::AlreadyExists { .. }));
    }

    #[test]
    fn overwrite_replaces_atomically() {
        let workspace = temp_workspace();
        let store = MeshKeyStore::open_or_create(workspace.path()).expect("open store");
        store
            .store_pair_key(
                "peer-a1",
                PairKeyClass::Current,
                generation(1),
                &sample_key(1),
                CREATED_AT,
                false,
            )
            .expect("first store");
        store
            .store_pair_key(
                "peer-a1",
                PairKeyClass::Current,
                generation(2),
                &sample_key(9),
                CREATED_AT,
                true,
            )
            .expect("replace");
        let record = store
            .load_pair_key("peer-a1", PairKeyClass::Current)
            .expect("load")
            .expect("present");
        assert_eq!(record.generation, generation(2));
        assert_eq!(record.key.as_bytes(), &[9; PAIR_KEY_LEN]);
        assert!(
            !store
                .secure_dir()
                .exists("pair.peer-a1.current.json.tmp")
                .expect("tmp check"),
            "temp sibling must not linger after atomic replace"
        );
    }

    #[test]
    fn atomic_replace_survives_crash_retained_temp_without_reusing_or_disclosing() {
        let workspace = temp_workspace();
        let store = MeshKeyStore::open_or_create(workspace.path()).expect("open store");
        store
            .store_signing_key(
                "node-local-1",
                SigningKeyClass::Current,
                generation(1),
                &sample_key(1),
                CREATED_AT,
                false,
            )
            .expect("store current");
        let temp_name = "signing.node-local-1.current.json.tmp";
        store
            .secure_dir()
            .write_exclusive(temp_name, b"attacker-controlled")
            .expect("plant temp sibling");
        let temp_path = store.secure_dir().path().join(temp_name);
        std::fs::set_permissions(&temp_path, std::fs::Permissions::from_mode(0o644))
            .expect("make planted temp broadly readable");

        store
            .store_signing_key(
                "node-local-1",
                SigningKeyClass::Current,
                generation(2),
                &sample_key(0xA5),
                CREATED_AT,
                true,
            )
            .expect("a unique temp name must bypass the crash-retained sibling");

        let planted = std::fs::read_to_string(temp_path).expect("read planted temp");
        assert_eq!(planted, "attacker-controlled");
        assert!(!planted.contains(&"a5".repeat(SIGNING_KEY_SEED_LEN)));
        let current = store
            .load_signing_key("node-local-1", SigningKeyClass::Current)
            .expect("load replaced current")
            .expect("current present");
        assert_eq!(current.generation, generation(2));
        assert_eq!(current.seed.as_bytes(), &[0xA5; SIGNING_KEY_SEED_LEN]);
    }

    #[test]
    fn symlinked_store_directory_is_refused() {
        let workspace = temp_workspace();
        let real = workspace.path().join("elsewhere");
        std::fs::create_dir_all(&real).expect("mkdir");
        let keys_dir = mesh_keys_dir(workspace.path());
        std::fs::create_dir_all(keys_dir.parent().expect("parent")).expect("mkdir parents");
        std::os::unix::fs::symlink(&real, &keys_dir).expect("symlink");
        let error = MeshKeyStore::open_or_create(workspace.path()).expect_err("must refuse");
        assert!(matches!(error, KeyStoreError::SymlinkComponent { .. }));
        assert_eq!(error.degraded_code(), MESH_KEY_STORE_UNAVAILABLE_CODE);
    }

    #[test]
    fn symlinked_workspace_marker_is_refused_before_creating_through_it() {
        let workspace = temp_workspace();
        let elsewhere = workspace.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).expect("create target");
        std::os::unix::fs::symlink(&elsewhere, workspace.path().join(".ee"))
            .expect("symlink marker");

        for error in [
            MeshKeyStore::open_or_create(workspace.path()).expect_err("create must refuse"),
            MeshKeyStore::open_existing(workspace.path()).expect_err("open must refuse"),
        ] {
            assert!(matches!(error, KeyStoreError::SymlinkComponent { .. }));
        }
        assert!(
            std::fs::symlink_metadata(elsewhere.join("keys")).is_err(),
            "no directory may be created through the marker symlink"
        );
    }

    #[test]
    fn regular_file_directory_component_is_reported_as_wrong_type() {
        let workspace = temp_workspace();
        std::fs::write(workspace.path().join(".ee"), b"not a directory")
            .expect("write regular-file marker");

        let error = MeshKeyStore::open_or_create(workspace.path()).expect_err("must refuse file");
        assert!(matches!(
            error,
            KeyStoreError::WrongFileType {
                expected: "directory",
                ..
            }
        ));
    }

    #[test]
    fn symlinked_keys_parent_is_refused_before_creating_mesh_directory() {
        let workspace = temp_workspace();
        let marker = workspace.path().join(".ee");
        let elsewhere = workspace.path().join("elsewhere");
        std::fs::create_dir(&marker).expect("create marker");
        std::fs::create_dir(&elsewhere).expect("create target");
        std::os::unix::fs::symlink(&elsewhere, marker.join("keys")).expect("symlink keys parent");

        for error in [
            MeshKeyStore::open_or_create(workspace.path()).expect_err("create must refuse"),
            MeshKeyStore::open_existing(workspace.path()).expect_err("open must refuse"),
        ] {
            assert!(matches!(error, KeyStoreError::SymlinkComponent { .. }));
        }
        assert!(
            std::fs::symlink_metadata(elsewhere.join("mesh")).is_err(),
            "no mesh directory may be created through the keys symlink"
        );
    }

    #[test]
    fn opened_store_revalidates_parent_components_before_each_operation() {
        let workspace = temp_workspace();
        let store = MeshKeyStore::open_or_create(workspace.path()).expect("open store");
        let marker = workspace.path().join(".ee");
        let keys = marker.join("keys");
        let moved_keys = marker.join("keys-real");
        std::fs::rename(&keys, &moved_keys).expect("move real keys directory");
        std::os::unix::fs::symlink(&moved_keys, &keys).expect("replace keys with symlink");

        let error = store
            .load_pair_key("peer-a1", PairKeyClass::Current)
            .expect_err("existing handle must notice parent substitution");

        assert!(matches!(error, KeyStoreError::SymlinkComponent { .. }));
    }

    #[test]
    fn symlinked_record_is_refused() {
        let workspace = temp_workspace();
        let store = MeshKeyStore::open_or_create(workspace.path()).expect("open store");
        let target = workspace.path().join("target.json");
        std::fs::write(&target, b"{}").expect("write target");
        let link = store.secure_dir().path().join("pair.peer-a1.current.json");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        let error = store
            .load_pair_key("peer-a1", PairKeyClass::Current)
            .expect_err("must refuse symlinked record");
        assert!(matches!(error, KeyStoreError::SymlinkComponent { .. }));
    }

    #[test]
    fn group_readable_record_is_refused() {
        let workspace = temp_workspace();
        let store = MeshKeyStore::open_or_create(workspace.path()).expect("open store");
        store
            .store_pair_key(
                "peer-a1",
                PairKeyClass::Current,
                generation(1),
                &sample_key(1),
                CREATED_AT,
                false,
            )
            .expect("store key");
        let path = store.secure_dir().path().join("pair.peer-a1.current.json");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640))
            .expect("loosen mode");
        let error = store
            .load_pair_key("peer-a1", PairKeyClass::Current)
            .expect_err("must refuse group-readable record");
        assert!(matches!(error, KeyStoreError::InsecurePermissions { .. }));
    }

    #[test]
    fn group_readable_signing_record_is_refused() {
        let workspace = temp_workspace();
        let store = MeshKeyStore::open_or_create(workspace.path()).expect("open store");
        store
            .store_signing_key(
                "node-local-1",
                SigningKeyClass::Current,
                generation(1),
                &sample_key(0xA5),
                CREATED_AT,
                false,
            )
            .expect("store signing seed");
        let path = store
            .secure_dir()
            .path()
            .join("signing.node-local-1.current.json");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640))
            .expect("loosen mode");
        let error = store
            .load_signing_key("node-local-1", SigningKeyClass::Current)
            .expect_err("must refuse group-readable signing record");
        assert!(matches!(error, KeyStoreError::InsecurePermissions { .. }));
    }

    #[test]
    fn group_accessible_directory_is_refused() {
        let workspace = temp_workspace();
        let store = MeshKeyStore::open_or_create(workspace.path()).expect("open store");
        std::fs::set_permissions(
            store.secure_dir().path(),
            std::fs::Permissions::from_mode(0o750),
        )
        .expect("loosen dir mode");
        let error = store
            .load_pair_key("peer-a1", PairKeyClass::Current)
            .expect_err("must refuse group-accessible directory");
        assert!(matches!(error, KeyStoreError::InsecurePermissions { .. }));
    }

    #[test]
    fn oversize_record_is_refused() {
        let workspace = temp_workspace();
        let store = MeshKeyStore::open_or_create(workspace.path()).expect("open store");
        let name = "pair.peer-a1.current.json";
        let oversized = vec![b'{'; usize::try_from(MAX_RECORD_BYTES).expect("cap fits") + 1];
        store
            .secure_dir()
            .write_exclusive(name, &oversized)
            .expect("raw write");
        let error = store
            .load_pair_key("peer-a1", PairKeyClass::Current)
            .expect_err("must refuse oversize record");
        assert!(matches!(error, KeyStoreError::CapExceeded { .. }));
    }

    #[test]
    fn tampered_schema_or_unknown_fields_fail_closed() {
        let workspace = temp_workspace();
        let store = MeshKeyStore::open_or_create(workspace.path()).expect("open store");
        let name = "pair.peer-a1.current.json";
        let body = format!(
            "{{\"schema\":\"{KEY_STORE_RECORD_SCHEMA}\",\"peer_handle\":\"peer-a1\",\"key_class\":\"current\",\"generation\":1,\"key_hex\":\"{}\",\"created_at\":\"{CREATED_AT}\",\"extra\":1}}",
            "0".repeat(PAIR_KEY_LEN * 2)
        );
        store
            .secure_dir()
            .write_exclusive(name, body.as_bytes())
            .expect("raw write");
        let error = store
            .load_pair_key("peer-a1", PairKeyClass::Current)
            .expect_err("unknown fields must fail closed");
        assert!(matches!(error, KeyStoreError::Malformed { .. }));
    }

    #[test]
    fn pair_key_generation_zero_fails_closed() {
        let workspace = temp_workspace();
        let store = MeshKeyStore::open_or_create(workspace.path()).expect("open store");
        let key_hex = "a5".repeat(PAIR_KEY_LEN);
        let body = format!(
            "{{\"schema\":\"{KEY_STORE_RECORD_SCHEMA}\",\"peer_handle\":\"peer-a1\",\"key_class\":\"current\",\"generation\":0,\"key_hex\":\"{key_hex}\",\"created_at\":\"{CREATED_AT}\"}}"
        );
        store
            .secure_dir()
            .write_exclusive("pair.peer-a1.current.json", body.as_bytes())
            .expect("write zero-generation record");
        let error = store
            .load_pair_key("peer-a1", PairKeyClass::Current)
            .expect_err("zero pair-key generation must fail structurally");
        assert!(matches!(error, KeyStoreError::Malformed { .. }));
    }

    #[test]
    fn signing_record_tamper_generation_zero_and_path_mismatch_fail_closed() {
        let workspace = temp_workspace();
        let store = MeshKeyStore::open_or_create(workspace.path()).expect("open store");
        let seed_hex = "a5".repeat(SIGNING_KEY_SEED_LEN);
        let path_name = "signing.node-local-1.next.json";

        let generation_zero = format!(
            "{{\"schema\":\"{SIGNING_KEY_STORE_RECORD_SCHEMA}\",\"node_handle\":\"node-local-1\",\"key_class\":\"next\",\"generation\":0,\"seed_hex\":\"{seed_hex}\",\"created_at\":\"{CREATED_AT}\"}}"
        );
        store
            .secure_dir()
            .write_exclusive(path_name, generation_zero.as_bytes())
            .expect("write generation-zero record");
        let error = store
            .load_signing_key("node-local-1", SigningKeyClass::Next)
            .expect_err("generation zero must fail structurally");
        assert!(matches!(error, KeyStoreError::Malformed { .. }));
        assert!(!error.message().contains(&seed_hex));

        let wrong_binding = format!(
            "{{\"schema\":\"{SIGNING_KEY_STORE_RECORD_SCHEMA}\",\"node_handle\":\"node-other\",\"key_class\":\"current\",\"generation\":2,\"seed_hex\":\"{seed_hex}\",\"created_at\":\"{CREATED_AT}\"}}"
        );
        store
            .secure_dir()
            .write_replace(path_name, wrong_binding.as_bytes())
            .expect("write path-mismatched record");
        let error = store
            .load_signing_key("node-local-1", SigningKeyClass::Next)
            .expect_err("node and class path mismatch must fail");
        assert!(matches!(error, KeyStoreError::Malformed { .. }));
        assert!(!error.message().contains(&seed_hex));

        let wrong_schema = format!(
            "{{\"schema\":\"{seed_hex}\",\"node_handle\":\"node-local-1\",\"key_class\":\"next\",\"generation\":2,\"seed_hex\":\"{seed_hex}\",\"created_at\":\"{CREATED_AT}\"}}"
        );
        store
            .secure_dir()
            .write_replace(path_name, wrong_schema.as_bytes())
            .expect("write schema-tampered record");
        let error = store
            .load_signing_key("node-local-1", SigningKeyClass::Next)
            .expect_err("schema mismatch must fail");
        assert!(matches!(error, KeyStoreError::Malformed { .. }));
        assert!(
            !error.message().contains(&seed_hex),
            "errors must never echo secret seed material"
        );
    }

    #[test]
    fn wrong_class_or_handle_binding_is_refused() {
        let workspace = temp_workspace();
        let store = MeshKeyStore::open_or_create(workspace.path()).expect("open store");
        store
            .store_pair_key(
                "peer-a1",
                PairKeyClass::Current,
                generation(1),
                &sample_key(1),
                CREATED_AT,
                false,
            )
            .expect("store key");
        let current = store.secure_dir().path().join("pair.peer-a1.current.json");
        let moved = store.secure_dir().path().join("pair.peer-a1.next.json");
        std::fs::rename(&current, &moved).expect("misfile record");
        let error = store
            .load_pair_key("peer-a1", PairKeyClass::Next)
            .expect_err("class binding must be checked");
        assert!(matches!(error, KeyStoreError::Malformed { .. }));
    }

    #[test]
    fn retire_renames_without_deleting() {
        let workspace = temp_workspace();
        let store = MeshKeyStore::open_or_create(workspace.path()).expect("open store");
        store
            .store_pair_key(
                "peer-a1",
                PairKeyClass::Current,
                generation(1),
                &sample_key(1),
                CREATED_AT,
                false,
            )
            .expect("store key");
        store
            .retire_pair_key("peer-a1", PairKeyClass::Current, "rot-0001")
            .expect("retire");
        assert!(
            store
                .load_pair_key("peer-a1", PairKeyClass::Current)
                .expect("load")
                .is_none()
        );
        assert!(
            store
                .secure_dir()
                .exists("retired.rot-0001.pair.peer-a1.current.json")
                .expect("retired record exists")
        );
    }

    #[test]
    fn invalid_handles_and_labels_are_rejected() {
        let workspace = temp_workspace();
        let store = MeshKeyStore::open_or_create(workspace.path()).expect("open store");
        for bad in [
            "",
            "a/b",
            "a b",
            "..",
            &"x".repeat(MAX_NAME_COMPONENT_LEN + 1),
        ] {
            assert!(
                store.load_pair_key(bad, PairKeyClass::Current).is_err(),
                "handle {bad:?} must be rejected"
            );
        }
        let error = store
            .retire_pair_key("peer-a1", PairKeyClass::Current, "bad/label")
            .expect_err("label must be validated");
        assert!(matches!(error, KeyStoreError::Malformed { .. }));

        let error = store
            .store_signing_key(
                "node-local-1",
                SigningKeyClass::Current,
                generation(1),
                &sample_key(1),
                "2026-99-99T25:61:00Z",
                false,
            )
            .expect_err("created_at must be parsed as RFC 3339");
        assert!(matches!(error, KeyStoreError::Malformed { .. }));
    }

    #[test]
    fn error_surface_maps_to_stable_degraded_code() {
        let error = KeyStoreError::PlatformUnsupported {
            operation: "open mesh key store".to_owned(),
        };
        assert_eq!(error.degraded_code(), "mesh_key_store_unavailable");
        assert_eq!(error.severity(), "high");
        assert!(!error.message().is_empty());
        assert!(!error.repair().is_empty());
        assert!(error.message().contains("Mesh key store"));
    }

    #[test]
    fn secret_debug_is_redacted() {
        let key = sample_key(3);
        assert_eq!(format!("{key:?}"), "SecretBytes(<redacted>)");
        let record = PairKeyRecord {
            peer_handle: "peer-a1".to_owned(),
            key_class: PairKeyClass::Current,
            generation: generation(1),
            key: sample_key(3),
            created_at: CREATED_AT.to_owned(),
        };
        let rendered = format!("{record:?}");
        assert!(rendered.contains("<redacted>"));
        assert!(!rendered.contains("0303"));

        let signing = SigningKeyRecord {
            node_handle: "node-local-1".to_owned(),
            key_class: SigningKeyClass::Current,
            generation: generation(7),
            seed: sample_key(0xA5),
            created_at: CREATED_AT.to_owned(),
        };
        let rendered = format!("{signing:?}");
        assert!(rendered.contains("<redacted>"));
        assert!(!rendered.contains(&"a5".repeat(SIGNING_KEY_SEED_LEN)));
        assert!(!rendered.contains("165, 165"));
    }
}

#[cfg(all(
    test,
    not(all(
        unix,
        any(
            target_os = "linux",
            target_os = "android",
            target_os = "redox",
            target_vendor = "apple"
        )
    ))
))]
mod unsupported_platform_tests {
    use super::*;

    #[test]
    fn unsupported_target_blocks_credential_operations_without_a_fallback() {
        assert_eq!(
            mesh_credential_store_platform(),
            MeshCredentialStorePlatform::Unsupported
        );
        let error = require_mesh_credential_store_platform("join team")
            .expect_err("an unreviewed platform adapter must fail closed");
        assert!(matches!(error, KeyStoreError::PlatformUnsupported { .. }));
        let guidance = error.guidance();
        assert_eq!(guidance.class, KeyStoreFailureClass::PlatformUnsupported);
        assert!(guidance.credential_operation_blocked);
        assert!(guidance.ordinary_local_commands_available);
    }
}
