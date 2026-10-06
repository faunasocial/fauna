//! The passphrase-encrypted sealed file store — the **headless** credential
//! backend (`architecture/apps/tui.md` § Credential storage).
//!
//! Selected when no OS secret store is reachable (the SSH / server case — the
//! *absence of a desktop session*, not the OS, selects it). The whole logical
//! namespace map (the same account→value map the e2e file backend keeps as
//! plaintext JSON) rests as ONE AEAD blob under a passphrase-derived key:
//!
//! - **KDF:** Argon2id v1.3 (`fauna_core::kdf`, the same parameter set and
//!   DoS-bounds validation as the wrapped-blob PLAIN path), params + 16-byte
//!   random salt recorded in the file header.
//! - **AEAD:** ChaCha20-Poly1305, 12-byte random nonce per write, AAD-bound to
//!   `"fauna.credential-store.v1\0" || namespace` so a blob cannot be
//!   substituted across namespaces.
//! - **File:** `{dir}/{app}.sealed`, self-describing versioned header (magic,
//!   version, KDF triple, salt, nonce, ciphertext+tag), written 0600 via
//!   tmp+rename so a crash mid-write can never truncate the only copy of an
//!   identity secret.
//!
//! Wrong passphrase and tampered file are indistinguishable by construction
//! (both are an AEAD open failure) — the honest error names both.
//!
//! This is a client-side wrap of the identity seed: rule #6 of
//! `key-material-hierarchy.md` § Architectural rules (seed wrapping is a
//! per-app UX decision, no new key category; the versioned context string
//! satisfies rule #3's domain-separation requirement).

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use fauna_core::kdf::{Argon2idParams, derive_key_argon2id};
use rand::RngCore;
use zeroize::Zeroizing;

/// File magic — first bytes of every sealed store file.
const MAGIC: &[u8; 15] = b"FAUNACREDSTORE\0";
/// The one format version this build writes and reads. The plaintext schema
/// (`BTreeMap<String, String>`, encoded in [`SealedFileStore::write_map`])
/// and this byte move together: any change to what the plaintext deserializes
/// into must bump `VERSION` in the same change, so an old build's
/// [`SealedFileStore::try_read_map`] refuses the file loudly (`BadFormat`)
/// instead of failing to deserialize a plaintext that authenticates fine.
const VERSION: u8 = 1;
/// AAD context (versioned, NUL-terminated per the repo's domain-separation
/// convention); the store's namespace is appended.
const AAD_CONTEXT: &[u8] = b"fauna.credential-store.v1\0";

const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;
const HEADER_LEN: usize = MAGIC.len() + 1 + 12 + SALT_LEN + NONCE_LEN;

/// A sealed-store failure. `WrongPassphraseOrCorrupt` is the user-facing arm:
/// AEAD cannot distinguish a wrong passphrase from a tampered file, so the
/// error honestly names both.
#[derive(Debug, thiserror::Error)]
pub enum SealedStoreError {
    #[error("no sealed credential store exists at {0}")]
    Missing(PathBuf),
    #[error("a sealed credential store already exists at {0}")]
    AlreadyExists(PathBuf),
    #[error("the sealed credential store is not unlocked")]
    Locked,
    #[error("wrong passphrase, or the store file is corrupt")]
    WrongPassphraseOrCorrupt,
    #[error("unreadable sealed store file: {0}")]
    BadFormat(String),
    #[error(transparent)]
    Kdf(#[from] fauna_core::kdf::KdfError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Path of the sealed file for one `application` namespace — the sealed twin
/// of [`crate::cred_file_path`]'s `{app}.json`.
pub fn sealed_file_path(dir: &Path, app: &str) -> PathBuf {
    dir.join(format!("{app}.sealed"))
}

/// Lock file guarding one sealed namespace's read-modify-write — the sealed
/// twin of [`crate::cred_file_lock_path`].
pub fn sealed_lock_path(dir: &Path, app: &str) -> PathBuf {
    dir.join(format!("{app}.sealed.lock"))
}

/// Take the sealed namespace's exclusive lock for one mutation.
///
/// Same defect, same shape as the plain-file arm ([`crate::lock_cred_file`],
/// which carries the full reasoning): [`SealedFileStore::set`] and
/// [`SealedFileStore::delete`] are whole-map read-modify-writes, and the
/// in-process `state` mutex guards only the *key*, not the file — it is
/// released between the read and the write, and it is invisible to the second
/// process. This is the production headless backend (`apps/tui.md`
/// § Credential storage), so it needs the guarantee at least as much as the
/// e2e-only file arm does. Advisory degrade on an unlockable path.
fn lock_sealed_file(dir: &Path, app: &str) -> Option<std::fs::File> {
    crate::lock_file_at(&sealed_lock_path(dir, app))
}

/// The parsed header + ciphertext of a sealed file.
struct SealedFile {
    params: Argon2idParams,
    salt: [u8; SALT_LEN],
    nonce: [u8; NONCE_LEN],
    ciphertext: Vec<u8>,
}

impl SealedFile {
    fn parse(bytes: &[u8]) -> Result<Self, SealedStoreError> {
        if bytes.len() < HEADER_LEN {
            return Err(SealedStoreError::BadFormat("truncated header".into()));
        }
        let (magic, rest) = bytes.split_at(MAGIC.len());
        if magic != MAGIC {
            return Err(SealedStoreError::BadFormat("bad magic".into()));
        }
        let (&version, rest) = rest.split_first().expect("length checked");
        if version != VERSION {
            // An unknown version is a file a NEWER build wrote. Refuse loudly
            // rather than guessing — the accounts are intact; update the app
            // (the AccountIndex `IndexUnreadable` stance, applied at-rest).
            return Err(SealedStoreError::BadFormat(format!(
                "unsupported version {version} (this build reads {VERSION}); \
                 the store was written by a newer build"
            )));
        }
        let (kdf, rest) = rest.split_at(12);
        let params = Argon2idParams {
            m: u32::from_le_bytes(kdf[0..4].try_into().expect("length checked")),
            t: u32::from_le_bytes(kdf[4..8].try_into().expect("length checked")),
            p: u32::from_le_bytes(kdf[8..12].try_into().expect("length checked")),
        };
        let (salt, rest) = rest.split_at(SALT_LEN);
        let (nonce, ciphertext) = rest.split_at(NONCE_LEN);
        Ok(Self {
            params,
            salt: salt.try_into().expect("length checked"),
            nonce: nonce.try_into().expect("length checked"),
            ciphertext: ciphertext.to_vec(),
        })
    }

    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_LEN + self.ciphertext.len());
        out.extend_from_slice(MAGIC);
        out.push(VERSION);
        out.extend_from_slice(&self.params.m.to_le_bytes());
        out.extend_from_slice(&self.params.t.to_le_bytes());
        out.extend_from_slice(&self.params.p.to_le_bytes());
        out.extend_from_slice(&self.salt);
        out.extend_from_slice(&self.nonce);
        out.extend_from_slice(&self.ciphertext);
        out
    }
}

/// The passphrase-encrypted [`fauna_client_accounts::SecretStore`] backend.
///
/// Constructed **locked**: the client shows its unlock/create surface before
/// launch routing reads anything, so the defensive locked-state `get`/`set`
/// behavior (read `None`, drop the write with a warning) should never fire in
/// a correctly-sequenced client.
pub struct SealedFileStore {
    dir: PathBuf,
    app: String,
    /// The derived AEAD key, cached for the process once unlocked; the salt +
    /// params it was derived under (needed to re-seal on every write).
    state: Mutex<Option<UnlockedState>>,
}

struct UnlockedState {
    key: Zeroizing<[u8; 32]>,
    params: Argon2idParams,
    salt: [u8; SALT_LEN],
}

impl SealedFileStore {
    /// A locked store over `{dir}/{app}.sealed`. Cheap; no I/O.
    pub fn new(app: impl Into<String>, dir: PathBuf) -> Self {
        Self {
            dir,
            app: app.into(),
            state: Mutex::new(None),
        }
    }

    pub fn path(&self) -> PathBuf {
        sealed_file_path(&self.dir, &self.app)
    }

    /// Whether a sealed file exists on disk — the unlock-vs-create branch.
    pub fn file_exists(&self) -> bool {
        self.path().exists()
    }

    /// Whether the store still needs a successful [`Self::unlock`] /
    /// [`Self::create`] before it can serve reads and writes.
    pub fn is_locked(&self) -> bool {
        self.state.lock().expect("not poisoned").is_none()
    }

    fn aad(&self) -> Vec<u8> {
        [AAD_CONTEXT, self.app.as_bytes()].concat()
    }

    /// Derive the key for the existing file's header and open it. On success
    /// the key is cached for the process and the store serves reads/writes.
    ///
    /// # Errors
    ///
    /// [`SealedStoreError::Missing`] when no file exists (callers branch to
    /// [`Self::create`]); [`SealedStoreError::WrongPassphraseOrCorrupt`] when
    /// the AEAD open fails; [`SealedStoreError::BadFormat`] / `Kdf` for a
    /// malformed header (including a tampered KDF triple outside the DoS
    /// envelope — validated before argon2 runs).
    pub fn unlock(&self, passphrase: &str) -> Result<(), SealedStoreError> {
        let path = self.path();
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(SealedStoreError::Missing(path));
            }
            Err(e) => return Err(e.into()),
        };
        let file = SealedFile::parse(&bytes)?;
        let key = derive_key_argon2id(passphrase.as_bytes(), &file.salt, file.params)?;
        // Proof-of-key: the open IS the passphrase check (AEAD success is the
        // only verifier — no separate hash rests in the file to attack).
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&*key));
        cipher
            .decrypt(
                Nonce::from_slice(&file.nonce),
                Payload {
                    msg: &file.ciphertext,
                    aad: &self.aad(),
                },
            )
            .map_err(|_| SealedStoreError::WrongPassphraseOrCorrupt)
            .map(Zeroizing::new)?;
        *self.state.lock().expect("not poisoned") = Some(UnlockedState {
            key,
            params: file.params,
            salt: file.salt,
        });
        Ok(())
    }

    /// Create a fresh sealed store (empty map) under `passphrase` and unlock
    /// it. Refuses when a file already exists — creation must never silently
    /// re-key over stored identities.
    ///
    /// # Errors
    ///
    /// [`SealedStoreError::AlreadyExists`], KDF or I/O failures.
    pub fn create(&self, passphrase: &str) -> Result<(), SealedStoreError> {
        let path = self.path();
        if path.exists() {
            return Err(SealedStoreError::AlreadyExists(path));
        }
        let params = Argon2idParams::interactive();
        let mut salt = [0u8; SALT_LEN];
        rand::rngs::OsRng.fill_bytes(&mut salt);
        let key = derive_key_argon2id(passphrase.as_bytes(), &salt, params)?;
        {
            let mut state = self.state.lock().expect("not poisoned");
            *state = Some(UnlockedState { key, params, salt });
        }
        self.write_map(&BTreeMap::new())
    }

    /// Read + decrypt the whole namespace map. Locked or unreadable states read
    /// as empty **with a warning** — the `SecretStore` seam is infallible by
    /// contract, and the client flow guarantees unlock-before-first-read.
    ///
    /// This is the **read** contract, and it belongs to `get` alone. A write
    /// path must use [`Self::try_read_map`] instead: an empty map here can mean
    /// "unreadable", and a read-modify-write that cannot tell the two apart
    /// writes the whole namespace away — see that method's doc.
    pub fn read_map(&self) -> BTreeMap<String, String> {
        self.try_read_map().unwrap_or_default()
    }

    /// The fallible twin of [`Self::read_map`], for the read half of a
    /// read-modify-write.
    ///
    /// `Ok(empty)` means the namespace is **known** to hold nothing — the file
    /// does not exist. Every other empty is an `Err`, because the map that
    /// could not be read is still on disk and a write built over `{}` would
    /// rename a one-entry map over it: the account index and
    /// every per-actor secret gone, with no second copy anywhere
    /// (`principles.md` § No user-data loss — client-only-resident key
    /// material is iron-clad even under the alpha carve-out).
    ///
    /// The three destructive arms this exists to separate are a read error
    /// (EIO, EACCES, ENOMEM), a parse failure (truncation, bad magic, a newer
    /// version byte) and an AEAD open failure under the cached key (an external
    /// re-key). `read_map`'s own comment had the reasoning right — *"read empty
    /// rather than serving another key's map"* — it just never reached the
    /// write side, where "read empty" became "write empty".
    fn try_read_map(&self) -> Result<BTreeMap<String, String>, SealedStoreError> {
        let state = self.state.lock().expect("not poisoned");
        let Some(unlocked) = state.as_ref() else {
            tracing::warn!("[cred-store] read from a locked sealed store");
            return Err(SealedStoreError::Locked);
        };
        let bytes = match std::fs::read(self.path()) {
            Ok(b) => b,
            // The one honest empty: there is no namespace yet.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(e) => {
                tracing::warn!("[cred-store] sealed store read failed: {e:#}");
                return Err(e.into());
            }
        };
        let file = SealedFile::parse(&bytes).inspect_err(|e| {
            tracing::warn!("[cred-store] sealed store unreadable: {e:#}");
        })?;
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&*unlocked.key));
        let plaintext = match cipher.decrypt(
            Nonce::from_slice(&file.nonce),
            Payload {
                msg: &file.ciphertext,
                aad: &self.aad(),
            },
        ) {
            Ok(p) => Zeroizing::new(p),
            Err(_) => {
                // The file changed under the cached key (an external re-key?).
                // Read empty rather than serving another key's map.
                tracing::warn!("[cred-store] sealed store no longer opens under the cached key");
                return Err(SealedStoreError::WrongPassphraseOrCorrupt);
            }
        };
        serde_json::from_slice(&plaintext).map_err(|e| {
            // The AEAD open succeeded — this plaintext is authentic, not
            // tampered — but it does not deserialize into the namespace map
            // shape. That is a genuine unreadable, never "empty": the map
            // this book-keeps could still be on disk under a shape this
            // build no longer understands, and defaulting here is the same
            // destructive collapse removed from the other three
            // arms.
            tracing::warn!("[cred-store] sealed store plaintext did not deserialize: {e:#}");
            SealedStoreError::BadFormat(format!("plaintext did not deserialize: {e}"))
        })
    }

    /// Seal + write the whole namespace map (fresh nonce), 0600, tmp+rename.
    fn write_map(&self, map: &BTreeMap<String, String>) -> Result<(), SealedStoreError> {
        let state = self.state.lock().expect("not poisoned");
        let Some(unlocked) = state.as_ref() else {
            return Err(SealedStoreError::Locked);
        };
        let plaintext = Zeroizing::new(serde_json::to_vec(map).expect("string map serializes"));
        let mut nonce = [0u8; NONCE_LEN];
        rand::rngs::OsRng.fill_bytes(&mut nonce);
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&*unlocked.key));
        let ciphertext = cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &plaintext,
                    aad: &self.aad(),
                },
            )
            .expect("ChaCha20-Poly1305 encrypt is infallible for in-memory buffers");
        let file = SealedFile {
            params: unlocked.params,
            salt: unlocked.salt,
            nonce,
            ciphertext,
        };
        self.persist(&file)
    }

    /// Write a sealed file 0600 via tmp + fsync + rename: a crash mid-write
    /// must never truncate the only copy of an identity secret, and the
    /// rename is the single atomic decision point (`change_passphrase` leans
    /// on exactly this). The fsync before rename ensures the tmp file's
    /// contents actually reached disk before the rename makes it visible —
    /// without it, a crash between rename and the next flush of the
    /// filesystem's own metadata cache could leave the renamed file
    /// zero-length or truncated. The tmp name is per-process; two stores of
    /// one namespace in one process are already serialized by `state`'s lock.
    fn persist(&self, file: &SealedFile) -> Result<(), SealedStoreError> {
        std::fs::create_dir_all(&self.dir)?;
        let path = self.path();
        let tmp = self
            .dir
            .join(format!(".{}.sealed.tmp-{}", self.app, std::process::id()));
        // 0600 from `open(2)`, for the same reason as `cred_file_write`'s temp:
        // a chmod after the write leaves the file at the umask default in
        // between. The payload here is already sealed, so this is uniformity
        // rather than a plaintext exposure — but the two writers should not
        // differ in posture, and the next reader should not have to work out
        // which one is the careful one.
        {
            #[cfg(unix)]
            let mut f = {
                use std::os::unix::fs::OpenOptionsExt as _;
                std::fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .mode(0o600)
                    .open(&tmp)?
            };
            #[cfg(not(unix))]
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(&file.encode())?;
            f.sync_all()?;
        }
        // The mode above binds only on creation, so a temp left behind by a
        // crashed writer keeps its old permissions — demote it explicitly.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Re-seal the store under a NEW passphrase (`tui.md` § Credential
    /// storage, design ratified 2026-08-06; driven from the settings
    /// credential-store section — `ui/settings.md` § Credential store).
    ///
    /// `current` is verified against the FILE itself — parse the header,
    /// derive under its own salt + params, AEAD-open; the open IS the check,
    /// deliberately independent of any cached process key, so a file some
    /// other actor re-keyed is never clobbered on the strength of a stale
    /// cache. The decrypted map is re-sealed under a fresh salt, the CURRENT
    /// interactive params (a re-key is the one moment a future cost
    /// tightening propagates into old files) and a fresh nonce, then written
    /// through [`Self::persist`]: at any crash instant the file is wholly-old
    /// or wholly-new, so exactly one of the two passphrases opens it and the
    /// unlock surface recovers either outcome with no special handling.
    ///
    /// On success the cached process state swaps to the new derivation, so a
    /// running (or even still-locked) store serves reads/writes afterwards.
    /// The state lock is held across the whole read → re-seal → rename, so a
    /// concurrent `set`/`delete` cannot land in between and be resealed away
    /// (~two Argon2id derives — the settings modal's synchronous-submit
    /// budget, the unlock-submit precedent).
    ///
    /// The **namespace lock** ([`lock_sealed_file`]) is held across it too, and
    /// that is the half that carries the guarantee: a re-key is a whole-file
    /// read-modify-write exactly as `set` and `delete` are, and the state mutex
    /// "guards only the *key*, not the file" — it is invisible to the second
    /// process, which is the entire topology this surface lives in (two
    /// `fauna-tui` sessions over one sealed namespace). Unlocked, another
    /// process's `set` lands between the read and the persist and is resealed
    /// away.
    ///
    /// # Errors
    ///
    /// [`SealedStoreError::Missing`] when no file exists;
    /// [`SealedStoreError::WrongPassphraseOrCorrupt`] when `current` fails
    /// the open; `BadFormat` / `Kdf` / `Io` as for [`Self::unlock`].
    pub fn change_passphrase(&self, current: &str, new: &str) -> Result<(), SealedStoreError> {
        // Taken BEFORE the state mutex, the order `set`/`delete`/
        // `delete_namespace` all use — the reverse order would deadlock a
        // second thread of this process against them.
        let _lock = lock_sealed_file(&self.dir, &self.app);
        #[cfg(test)]
        tests::run_rekey_hook(&self.dir, &self.app);
        let mut state = self.state.lock().expect("not poisoned");
        let path = self.path();
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(SealedStoreError::Missing(path));
            }
            Err(e) => return Err(e.into()),
        };
        let file = SealedFile::parse(&bytes)?;
        let old_key = derive_key_argon2id(current.as_bytes(), &file.salt, file.params)?;
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&*old_key));
        let plaintext = cipher
            .decrypt(
                Nonce::from_slice(&file.nonce),
                Payload {
                    msg: &file.ciphertext,
                    aad: &self.aad(),
                },
            )
            .map(Zeroizing::new)
            .map_err(|_| SealedStoreError::WrongPassphraseOrCorrupt)?;

        let params = Argon2idParams::interactive();
        let mut salt = [0u8; SALT_LEN];
        rand::rngs::OsRng.fill_bytes(&mut salt);
        let key = derive_key_argon2id(new.as_bytes(), &salt, params)?;
        let mut nonce = [0u8; NONCE_LEN];
        rand::rngs::OsRng.fill_bytes(&mut nonce);
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&*key));
        let ciphertext = cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &plaintext,
                    aad: &self.aad(),
                },
            )
            .expect("ChaCha20-Poly1305 encrypt is infallible for in-memory buffers");
        self.persist(&SealedFile {
            params,
            salt,
            nonce,
            ciphertext,
        })?;
        // The rename landed: the file now opens only under `new`. Swap the
        // cached derivation so the running session keeps serving.
        *state = Some(UnlockedState { key, params, salt });
        Ok(())
    }

    /// The `SecretStore::get` arm.
    pub fn get(&self, account: &str) -> Option<String> {
        self.read_map().get(account).cloned()
    }

    /// The `SecretStore::set` arm — infallible by seam contract; failures warn.
    ///
    /// Refuses rather than overwrites when the existing map does not read back
    /// ([`Self::try_read_map`]): dropping one write is recoverable, destroying
    /// the namespace is not.
    pub fn set(&self, account: &str, value: &str) {
        let _lock = lock_sealed_file(&self.dir, &self.app);
        let Some(mut map) = self.map_to_modify(account, "set") else {
            return;
        };
        map.insert(account.to_string(), value.to_string());
        if let Err(e) = self.write_map(&map) {
            tracing::warn!("[cred-store] sealed set {account:?} failed: {e:#}");
        }
    }

    /// The `SecretStore::delete` arm. Refuses on an unreadable map, as `set`
    /// does — a delete is equally a whole-map rewrite.
    pub fn delete(&self, account: &str) {
        let _lock = lock_sealed_file(&self.dir, &self.app);
        let Some(mut map) = self.map_to_modify(account, "delete") else {
            return;
        };
        if map.remove(account).is_some()
            && let Err(e) = self.write_map(&map)
        {
            tracing::warn!("[cred-store] sealed delete {account:?} failed: {e:#}");
        }
    }

    /// The read half of `set`/`delete`: the current map, or `None` when it did
    /// not read back and the write must be refused so the bytes on disk survive.
    ///
    /// The seam stays infallible — the caller warns and drops, exactly as it
    /// already does for a failed *write*.
    fn map_to_modify(&self, account: &str, op: &str) -> Option<BTreeMap<String, String>> {
        match self.try_read_map() {
            Ok(map) => Some(map),
            Err(e) => {
                tracing::warn!(
                    "[cred-store] sealed {op} {account:?} REFUSED: the existing store did not \
                     read back ({e:#}); writing over it would destroy every other credential \
                     in the namespace"
                );
                None
            }
        }
    }

    /// Sign-out / factory-reset: remove the file **and relock**. Relocking is
    /// deliberate — keeping the derived key would let the next onboarding
    /// silently re-seal under a passphrase the user never re-chose; the client
    /// routes back to its create surface instead. A missing file is success
    /// (the delete-nothing-found contract every backend shares).
    pub fn delete_namespace(&self) -> Result<(), anyhow::Error> {
        // Held across the relock + removal so no in-flight `set`/`delete` can
        // straddle the wipe and write the pre-wipe map back (`lock_sealed_file`).
        let _lock = lock_sealed_file(&self.dir, &self.app);
        *self.state.lock().expect("not poisoned") = None;
        match std::fs::remove_file(self.path()) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    fn fresh_dir() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut d = std::env::temp_dir();
        d.push(format!(
            "fauna-sealed-store-test-{}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    /// Fast KDF for tests: the interactive default costs ~100ms per derive,
    /// which across this suite would dominate the whole crate's test time.
    /// Production params are pinned by `create_uses_the_interactive_default`.
    fn fast_store(dir: &Path) -> SealedFileStore {
        let store = SealedFileStore::new("fauna-tui", dir.to_path_buf());
        store
            .create("correct horse")
            .expect("create a fresh sealed store");
        // Re-key the cached state down to the minimal triple so per-write
        // re-seals stay fast. (Params live in the file header; the re-seal
        // below rewrites the header from the cached state.)
        {
            let mut state = store.state.lock().unwrap();
            let unlocked = state.as_mut().unwrap();
            unlocked.params = Argon2idParams { m: 8, t: 1, p: 1 };
            unlocked.key =
                derive_key_argon2id(b"correct horse", &unlocked.salt, unlocked.params).unwrap();
        }
        // Re-seal the (empty) map under the swapped key, straight through
        // `write_map`. This deliberately does NOT go through `set`: between the
        // swap above and this line the cached key no longer opens the file the
        // `create` wrote, and a `set` there is exactly the destructive
        // read-empty-write-empty the split contract now refuses
        // ([`SealedFileStore::try_read_map`]). The fixture used to *rely* on
        // that refusal not existing.
        store
            .write_map(&BTreeMap::new())
            .expect("re-seal the empty map under the fast params");
        store
    }

    #[test]
    fn create_round_trips_and_relaunch_unlocks() {
        let dir = fresh_dir();
        let store = fast_store(&dir);
        store.set("fauna/abc/secret", "11".repeat(32).as_str());
        store.set("fauna/index", r#"{"active":"abc"}"#);
        assert_eq!(
            store.get("fauna/abc/secret").as_deref(),
            Some(&*"11".repeat(32))
        );

        // A fresh (relaunched) store over the same file: locked until the
        // right passphrase lands, then serves the same map.
        let relaunch = SealedFileStore::new("fauna-tui", dir.clone());
        assert!(relaunch.is_locked());
        assert!(relaunch.file_exists());
        relaunch.unlock("correct horse").expect("right passphrase");
        assert!(!relaunch.is_locked());
        assert_eq!(
            relaunch.get("fauna/index").as_deref(),
            Some(r#"{"active":"abc"}"#)
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wrong_passphrase_is_refused_and_leaves_the_store_locked() {
        let dir = fresh_dir();
        let _ = fast_store(&dir);
        let relaunch = SealedFileStore::new("fauna-tui", dir.clone());
        let err = relaunch
            .unlock("wrong horse")
            .expect_err("wrong passphrase");
        assert!(matches!(err, SealedStoreError::WrongPassphraseOrCorrupt));
        assert!(relaunch.is_locked());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tampered_ciphertext_and_foreign_namespace_fail_the_open() {
        let dir = fresh_dir();
        let store = fast_store(&dir);
        store.set("k", "v");

        // Flip one ciphertext byte: the AEAD tag catches it.
        let path = store.path();
        let mut bytes = std::fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0x01;
        std::fs::write(&path, &bytes).unwrap();
        let tampered = SealedFileStore::new("fauna-tui", dir.clone());
        assert!(matches!(
            tampered.unlock("correct horse"),
            Err(SealedStoreError::WrongPassphraseOrCorrupt)
        ));

        // Restore the honest bytes, but present them under ANOTHER namespace:
        // the AAD binding refuses a blob substituted across namespaces.
        bytes[last] ^= 0x01;
        std::fs::write(sealed_file_path(&dir, "other-app"), &bytes).unwrap();
        let foreign = SealedFileStore::new("other-app", dir.clone());
        assert!(matches!(
            foreign.unlock("correct horse"),
            Err(SealedStoreError::WrongPassphraseOrCorrupt)
        ));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unlock_branch_signals() {
        let dir = fresh_dir();
        let absent = SealedFileStore::new("fauna-tui", dir.clone());
        assert!(!absent.file_exists());
        assert!(matches!(
            absent.unlock("pw"),
            Err(SealedStoreError::Missing(_))
        ));

        let store = fast_store(&dir);
        assert!(matches!(
            store.create("again"),
            Err(SealedStoreError::AlreadyExists(_))
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn locked_reads_are_empty_and_locked_writes_drop() {
        let dir = fresh_dir();
        let seeded = fast_store(&dir);
        seeded.set("fauna/alice/secret", &"aa".repeat(32));
        let locked = SealedFileStore::new("fauna-tui", dir.clone());
        assert_eq!(locked.get("k"), None);
        locked.set("k", "v"); // warns + drops
        locked.delete("fauna/alice/secret"); // warns + drops
        locked.unlock("correct horse").unwrap();
        assert_eq!(locked.get("k"), None, "the locked write must not land");
        assert_eq!(
            locked.get("fauna/alice/secret").as_deref(),
            Some(&*"aa".repeat(32)),
            "a locked write must drop HARMLESSLY — never take the namespace \
             with it"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_namespace_removes_the_file_and_relocks() {
        let dir = fresh_dir();
        let store = fast_store(&dir);
        store.set("k", "v");
        store.delete_namespace().unwrap();
        assert!(!store.file_exists());
        assert!(
            store.is_locked(),
            "a reset must relock — the next onboarding re-chooses its passphrase"
        );
        store.delete_namespace().unwrap(); // idempotent
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_tampered_kdf_triple_is_refused_before_argon2_runs() {
        let dir = fresh_dir();
        let store = fast_store(&dir);
        let path = store.path();
        let mut bytes = std::fs::read(&path).unwrap();
        // The m field sits right after magic+version; write u32::MAX (a 4 TiB
        // allocation if it reached argon2).
        let off = MAGIC.len() + 1;
        bytes[off..off + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        std::fs::write(&path, &bytes).unwrap();
        let attacked = SealedFileStore::new("fauna-tui", dir.clone());
        assert!(matches!(
            attacked.unlock("correct horse"),
            Err(SealedStoreError::Kdf(_))
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_newer_version_byte_is_refused_loudly() {
        let dir = fresh_dir();
        let store = fast_store(&dir);
        let path = store.path();
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[MAGIC.len()] = 2;
        std::fs::write(&path, &bytes).unwrap();
        let newer = SealedFileStore::new("fauna-tui", dir.clone());
        match newer.unlock("correct horse") {
            Err(SealedStoreError::BadFormat(msg)) => {
                assert!(msg.contains("newer build"), "honest verdict, got: {msg}")
            }
            other => panic!("expected BadFormat, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The re-key round trip (`tui.md` § Credential storage, design ratified
    /// 2026-08-06): the same map opens under the new passphrase, the old one
    /// is refused afterwards, and no tmp file survives the rename.
    #[test]
    fn change_passphrase_round_trips_and_the_old_passphrase_is_refused() {
        let dir = fresh_dir();
        let store = fast_store(&dir);
        store.set("fauna/abc/secret", "22".repeat(32).as_str());
        store
            .change_passphrase("correct horse", "new horse")
            .expect("re-key with the right current passphrase");
        // The running store keeps serving across the re-key (cached key swapped).
        assert_eq!(
            store.get("fauna/abc/secret").as_deref(),
            Some(&*"22".repeat(32))
        );
        store.set("fauna/index", r#"{"active":"abc"}"#);
        // No half-state artifacts: the tmp file is gone, the file parses.
        assert!(
            std::fs::read_dir(&dir).unwrap().all(|e| !e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".tmp-")),
            "the tmp file must not survive the rename"
        );
        // A relaunch opens ONLY under the new passphrase.
        let relaunch = SealedFileStore::new("fauna-tui", dir.clone());
        assert!(matches!(
            relaunch.unlock("correct horse"),
            Err(SealedStoreError::WrongPassphraseOrCorrupt)
        ));
        relaunch.unlock("new horse").expect("the new passphrase");
        assert_eq!(
            relaunch.get("fauna/index").as_deref(),
            Some(r#"{"active":"abc"}"#),
            "the post-re-key write must be under the new seal"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A wrong current passphrase is refused with nothing changed: the file
    /// still opens under the real passphrase and the running store still
    /// serves under its cached key.
    #[test]
    fn change_passphrase_with_the_wrong_current_is_refused_and_changes_nothing() {
        let dir = fresh_dir();
        let store = fast_store(&dir);
        store.set("k", "v");
        assert!(matches!(
            store.change_passphrase("wrong horse", "new horse"),
            Err(SealedStoreError::WrongPassphraseOrCorrupt)
        ));
        assert_eq!(store.get("k").as_deref(), Some("v"), "cache untouched");
        let relaunch = SealedFileStore::new("fauna-tui", dir.clone());
        relaunch
            .unlock("correct horse")
            .expect("the file must still open under the old passphrase");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The verification runs against the FILE, never the cached process key: a
    /// still-locked store re-keys fine, and is unlocked (serving) afterwards.
    /// A re-key also rewrites the header at the CURRENT interactive triple —
    /// the one moment a future cost tightening propagates into old files
    /// (fast_store leaves a minimal triple in the header, so the upgrade is
    /// observable).
    #[test]
    fn change_passphrase_works_on_a_locked_store_and_upgrades_the_kdf_params() {
        let dir = fresh_dir();
        fast_store(&dir).set("k", "v");
        let locked = SealedFileStore::new("fauna-tui", dir.clone());
        assert!(locked.is_locked());
        let salt_before = SealedFile::parse(&std::fs::read(locked.path()).unwrap())
            .unwrap()
            .salt;
        locked
            .change_passphrase("correct horse", "new horse")
            .expect("a locked store re-keys from the file alone");
        assert!(!locked.is_locked(), "a successful re-key unlocks");
        assert_eq!(locked.get("k").as_deref(), Some("v"));
        let bytes = std::fs::read(locked.path()).unwrap();
        let file = SealedFile::parse(&bytes).unwrap();
        assert_eq!(
            file.params,
            Argon2idParams::interactive(),
            "a re-key rewrites the header at the current interactive triple"
        );
        assert_ne!(file.salt, salt_before, "a re-key mints a fresh salt");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// No file → `Missing`, same branch signal as unlock.
    #[test]
    fn change_passphrase_on_a_missing_file_is_missing() {
        let dir = fresh_dir();
        let absent = SealedFileStore::new("fauna-tui", dir.clone());
        assert!(matches!(
            absent.change_passphrase("a", "b"),
            Err(SealedStoreError::Missing(_))
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn create_uses_the_interactive_default() {
        let dir = fresh_dir();
        let store = SealedFileStore::new("fauna-tui", dir.clone());
        store.create("pw").unwrap();
        let bytes = std::fs::read(store.path()).unwrap();
        let file = SealedFile::parse(&bytes).unwrap();
        assert_eq!(file.params, Argon2idParams::interactive());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------------
    // A re-key observes the namespace lock from *inside* its critical section.
    //
    // `change_passphrase` is a whole-file read-modify-write exactly as `set`
    // and `delete` are, so it needs the same cross-process exclusion. The hook
    // below runs from inside the re-key and `try_lock`s a fresh handle on the
    // same lock file: deterministic, latency-independent, no threads and no
    // sleeps (`testing.md` convention 14). The shape is the file arm's
    // `run_rmw_hook` (`lib.rs`), applied to the sealed arm's third writer.
    //
    // `#[cfg(test)]` on both the hook and its call site, so a shipped build has
    // neither the static nor the branch.
    // -----------------------------------------------------------------------
    type RekeyHook = Box<dyn Fn(&Path, &str) + Send + 'static>;
    static REKEY_HOOK: Mutex<Option<RekeyHook>> = Mutex::new(None);

    /// Called from inside `change_passphrase`'s locked read → re-seal → rename.
    pub(super) fn run_rekey_hook(dir: &Path, app: &str) {
        // Taken OUT for the call: a hook that itself drives a store operation
        // would otherwise re-enter and recurse forever.
        let hook = REKEY_HOOK.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(h) = hook {
            h(dir, app);
        }
    }

    fn set_rekey_hook(h: impl Fn(&Path, &str) + Send + 'static) {
        *REKEY_HOOK.lock().unwrap_or_else(|e| e.into_inner()) = Some(Box::new(h));
    }

    /// An external re-key must not turn the next `set` into a namespace wipe.
    ///
    /// The live topology this reproduces: two `fauna-tui` processes over the
    /// tui's own sealed namespace — a second SSH session, or any out-of-band
    /// re-key of the file — where `change_passphrase` is a shipped user-facing
    /// surface (`ui/settings.md` § Credential store). Handle A holds the store
    /// unlocked under the old key; handle B re-keys the file; A's next write
    /// reads a map its cached key can no longer open.
    ///
    /// Before the `try_read_map` split, A's `set` read `{}` (the AEAD refusing
    /// the new file) and wrote a ONE-entry map back under the OLD key: both
    /// pre-existing credentials destroyed, **and the passphrase change silently
    /// reverted** — the store reopened under the old passphrase and refused the
    /// new one, so the user believed their store was re-keyed and it was not.
    /// This is client-only-resident key material with no second copy
    /// (`principles.md` § No user-data loss).
    #[test]
    fn an_external_rekey_cannot_make_the_next_set_wipe_the_namespace() {
        let dir = fresh_dir();
        let handle_a = fast_store(&dir);
        handle_a.set("fauna/alice/secret", &"aa".repeat(32));
        handle_a.set("fauna/index", r#"{"active":"alice"}"#);

        // A second process over the same namespace re-keys the file. `handle_a`
        // keeps serving under the OLD key: it is never told.
        let handle_b = SealedFileStore::new("fauna-tui", dir.clone());
        handle_b
            .change_passphrase("correct horse", "battery staple")
            .expect("the re-key lands");

        // A's next write. Its cached key cannot open the re-keyed file.
        handle_a.set("fauna/carol/secret", &"cc".repeat(32));

        // The re-key stands, and the namespace it protects is intact.
        let after = SealedFileStore::new("fauna-tui", dir.clone());
        assert!(
            matches!(
                after.unlock("correct horse"),
                Err(SealedStoreError::WrongPassphraseOrCorrupt)
            ),
            "the OLD passphrase still opens the store: A's `set` re-sealed the \
             file under its stale key and silently reverted the re-key"
        );
        after
            .unlock("battery staple")
            .expect("the NEW passphrase opens the re-keyed store");
        assert_eq!(
            after.get("fauna/alice/secret").as_deref(),
            Some(&*"aa".repeat(32)),
            "A's `set` destroyed a pre-existing credential it could not read"
        );
        assert_eq!(
            after.get("fauna/index").as_deref(),
            Some(r#"{"active":"alice"}"#),
            "A's `set` destroyed the account index it could not read"
        );
        assert_eq!(
            after.get("fauna/carol/secret"),
            None,
            "A's write must be REFUSED, not applied under the stale key"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A corrupt file is evidence; a write must not overwrite it.
    ///
    /// No concurrency at all — one handle, unlocked, file truncated under it
    /// (bit-rot, a partial restore, a foreign writer). Before the fix the next
    /// `set` grew it back into a **valid, healthy, unlockable store that was
    /// simply missing everything**, converting a loud, possibly-recoverable
    /// corruption into a silent unrecoverable loss. `delete` took the same
    /// path.
    #[test]
    fn a_truncated_file_is_evidence_not_a_map_to_overwrite() {
        let dir = fresh_dir();
        let store = fast_store(&dir);
        store.set("fauna/alice/secret", &"aa".repeat(32));
        store.set("fauna/index", r#"{"active":"alice"}"#);

        // One byte short of a whole header — the review's own probe shape, and
        // the arm that makes `SealedFile::parse` fail rather than the AEAD.
        let path = store.path();
        let whole = std::fs::read(&path).unwrap();
        let truncated = whole[..HEADER_LEN - 1].to_vec();
        std::fs::write(&path, &truncated).unwrap();

        // Both write arms, against a file that no longer parses.
        store.set("fauna/carol/secret", &"cc".repeat(32));
        store.delete("fauna/alice/secret");

        assert_eq!(
            std::fs::read(&path).unwrap(),
            truncated,
            "a write grew the truncated file back — the corruption is the only \
             evidence the user has that something ate their store"
        );
        let after = SealedFileStore::new("fauna-tui", dir.clone());
        assert!(
            matches!(
                after.unlock("correct horse"),
                Err(SealedStoreError::BadFormat(_))
            ),
            "the truncated store must keep failing LOUDLY on the next unlock, \
             not open as a healthy store missing every credential"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The fourth collapse-to-empty arm: a plaintext that decrypts cleanly
    /// under the cached key but does not deserialize into the namespace map
    /// shape. `try_read_map`'s last line used to default this to `Ok(empty)`
    /// exactly like the three arms fixed — and a write over that
    /// empty map is the same destructive rename `try_read_map` exists to
    /// prevent.
    #[test]
    fn a_plaintext_that_decrypts_but_wont_deserialize_is_refused_not_overwritten() {
        let dir = fresh_dir();
        let store = fast_store(&dir);
        store.set("fauna/alice/secret", &"aa".repeat(32));
        store.set("fauna/index", r#"{"active":"alice"}"#);

        // Re-seal under the SAME cached key/salt/params with a plaintext that
        // authenticates fine but is not a `BTreeMap<String, String>` — a
        // number, not a string, per the review's own probe.
        let (params, salt, key) = {
            let state = store.state.lock().unwrap();
            let unlocked = state.as_ref().unwrap();
            (unlocked.params, unlocked.salt, unlocked.key.clone())
        };
        let mut nonce = [0u8; NONCE_LEN];
        rand::rngs::OsRng.fill_bytes(&mut nonce);
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&*key));
        let ciphertext = cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: br#"{"a": 1}"#.as_ref(),
                    aad: &store.aad(),
                },
            )
            .unwrap();
        store
            .persist(&SealedFile {
                params,
                salt,
                nonce,
                ciphertext,
            })
            .expect("write the crafted file directly, bypassing write_map's own type");

        let before = std::fs::read(store.path()).unwrap();
        store.set("fauna/carol/secret", &"cc".repeat(32));
        store.delete("fauna/alice/secret");
        assert_eq!(
            std::fs::read(store.path()).unwrap(),
            before,
            "a write treated an undeserializable-but-decryptable plaintext as an \
             empty map and overwrote it — try_read_map's fourth collapse-to-empty arm"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The re-key holds the namespace lock across its read → re-seal → rename.
    ///
    /// The in-process `state` mutex is held across the body, so a same-handle
    /// `set` already cannot interleave — but that mutex is invisible to the
    /// second process, which is the whole topology `change_passphrase` exists
    /// in (`lock_sealed_file`'s own doc). Without the file lock a concurrent
    /// `set` from another process lands between the read and the persist and is
    /// resealed away.
    #[test]
    fn change_passphrase_holds_the_namespace_lock_across_its_read_reseal_rename() {
        let dir = fresh_dir();
        let store = fast_store(&dir);
        let excluded = Arc::new(AtomicBool::new(false));

        let seen = Arc::clone(&excluded);
        set_rekey_hook(move |dir, app| {
            // A fresh handle on the same lock file, from inside the critical
            // section. `try_lock` must report the lock already held.
            let f = fauna_core::fs_lock::open_lock_file(&sealed_lock_path(dir, app))
                .expect("lock file opens");
            seen.store(
                matches!(f.try_lock(), Err(std::fs::TryLockError::WouldBlock)),
                Ordering::SeqCst,
            );
        });
        store
            .change_passphrase("correct horse", "battery staple")
            .expect("the re-key lands");

        assert!(
            excluded.load(Ordering::SeqCst),
            "`change_passphrase` left its read-modify-write unserialized: a \
             second process could take the namespace lock mid-re-key, so its \
             `set` lands between the read and the persist and is resealed away"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
