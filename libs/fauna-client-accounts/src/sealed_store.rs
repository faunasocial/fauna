//! The sealing [`SecretStore`] wrapper the web app's identity store sits
//! behind (`architecture/apps/web.md` § Identity store at rest; the posture is
//! `apps/common.md` § Credential storage → *The web app's posture*).
//!
//! **An envelope, sealed row by row.** One header row ([`SEAL_HEADER_KEY`])
//! carries a version and one or more *wraps* of a random 32-byte **store
//! key**; a passphrase wrap is the store key sealed under an Argon2id-derived
//! key (`fauna_core::kdf`, the interactive parameters, a 16-byte random salt,
//! the parameters recorded beside it so a later cost tightening reads old
//! headers unchanged). Opening the wrap **is** the passphrase check — no
//! separate verifier rests beside it, and a wrong passphrase and a damaged
//! header are one failure ([`SealError::WrongPassphraseOrCorrupt`]), the
//! terminal app's rule. Every other row holds its value sealed under the
//! store key with a fresh nonce, the row's own logical key in the associated
//! data, so a sealed value cannot be moved to another slot. Row *names* stay
//! readable.
//!
//! **Changing the passphrase is one row write** ([`SealedSecretStore::change_passphrase`]):
//! only the header's wrap is re-sealed (fresh salt, current parameters), no row
//! is rewritten, and wraps of another kind are kept verbatim.
//!
//! **A passkey is a second wrap of the same store key** ([`SealedSecretStore::add_passkey`],
//! [`SealedSecretStore::unlock_with_passkey`], [`SealedSecretStore::remove_passkey`]):
//! the wrapping key is a WebAuthn PRF output through `BLAKE3::derive_key`, the
//! credential id and PRF salt recorded in the wrap (and the credential id bound
//! into its associated data). Adding or removing one is one header write, and
//! the passphrase wrap stays beside it as the fallback.
//!
//! **Rows a store already holds plain** are rewritten sealed in place by
//! [`SealedSecretStore::resume_sealing`]. A sealed value is self-describing
//! ([`SEALED_VALUE_PREFIX`]), so a pass that finds both kinds resumes the
//! rewrite, and no plain value is replaced before its sealed form has been
//! read back from a staging row (`principles.md` § No user-data loss —
//! client-only key material).
//!
//! The AEAD is the shared `fauna_core::aead_seal` shape the terminal app's
//! sealed file uses; this module owns only the container. Platform-neutral and
//! unit-tested natively over an in-memory store; web wraps
//! [`crate::LocalStorageSecretStore`] in it.

use std::sync::Mutex;

use fauna_core::aead_seal;
use fauna_core::kdf::{Argon2idParams, KdfError, derive_key_argon2id, derive_key_blake3};
use fauna_core::secret::{SecretArray32, fresh_secret_32};
use serde::{Deserialize, Serialize};

use crate::SecretStore;

/// The header row's logical key.
pub const SEAL_HEADER_KEY: &str = "fauna/seal";
/// Prefix of the staging row [`SealedSecretStore::resume_sealing`] writes a
/// row's sealed form to before it replaces the plain value.
pub const SEAL_STAGING_PREFIX: &str = "fauna/seal-staging/";
/// Prefix every sealed row value starts with — what makes a sealed value
/// self-describing beside a plain one. No plain identity-store value (a JSON
/// index, a hex secret, a URL, a device id) begins with it.
pub const SEALED_VALUE_PREFIX: &str = "sealed:v1:";

/// The one header version this build writes and reads.
const HEADER_VERSION: u8 = 1;
/// The wrap kind a passphrase wrap carries.
const PASSPHRASE_WRAP: &str = "passphrase";
/// Associated-data context of a row's sealed value; the row's logical key is
/// appended (versioned, NUL-terminated — `key-material-hierarchy.md`
/// § Architectural rules, rule 3).
const ROW_AAD_CONTEXT: &[u8] = b"fauna.identity-store.row.v1\0";
/// Associated-data context of a header wrap; the wrap kind is appended.
const WRAP_AAD_CONTEXT: &[u8] = b"fauna.identity-store.wrap.v1\0";
/// Associated-data context of the header's store-key check.
const CHECK_AAD: &[u8] = b"fauna.identity-store.check.v1\0";
/// The wrap kind a passkey wrap carries.
const PASSKEY_WRAP: &str = "passkey";
/// BLAKE3 `derive_key` context turning a passkey's PRF output into its wrap's
/// key.
const PASSKEY_WRAP_KEY_CONTEXT: &str = "fauna.identity-store.passkey-wrap.v1";
/// Length of a passkey wrap's PRF salt (the PRF extension's eval input) and of
/// the PRF output an authenticator returns for it.
pub const PASSKEY_PRF_LEN: usize = 32;

/// Whether `value` is a sealed row value (as opposed to a plain one).
#[must_use]
pub fn is_sealed_value(value: &str) -> bool {
    value.starts_with(SEALED_VALUE_PREFIX)
}

/// Whether `key` names a row of the identity store — the rows a sealing pass
/// rewrites ([`SealedSecretStore::resume_sealing`] takes its key list from the
/// caller, since a [`SecretStore`] cannot enumerate): the `fauna/…` index and
/// per-actor slots, the bare `{actor}/…` slots (64 lowercase hex, then `/`),
/// `install/…`, and the sign-out record. The seal's own reserved rows are not.
///
/// An inclusion list on purpose: a row it misses still rests plain only until
/// its next write, which the unlocked store seals, while sealing a row some
/// code reads raw (a push-intent flag, a harness knob) would break that code.
#[must_use]
pub fn is_identity_store_key(key: &str) -> bool {
    if is_reserved(key) {
        return false;
    }
    if key.starts_with("fauna/") || key.starts_with("install/") {
        return true;
    }
    if key == crate::SIGN_OUT_RECORD_KEY {
        return true;
    }
    key.split_once('/')
        .is_some_and(|(actor, _)| fauna_core::hex32::is_lowercase_hex64(actor))
}

fn is_reserved(key: &str) -> bool {
    key == SEAL_HEADER_KEY || key.starts_with(SEAL_STAGING_PREFIX)
}

fn staging_key(key: &str) -> String {
    format!("{SEAL_STAGING_PREFIX}{key}")
}

fn row_aad(key: &str) -> Vec<u8> {
    [ROW_AAD_CONTEXT, key.as_bytes()].concat()
}

fn wrap_aad(kind: &str) -> Vec<u8> {
    [WRAP_AAD_CONTEXT, kind.as_bytes()].concat()
}

/// A passkey wrap's associated data also binds its credential id, so a wrap
/// moved to another credential's record does not open.
fn passkey_wrap_aad(credential_id: &[u8]) -> Vec<u8> {
    [wrap_aad(PASSKEY_WRAP).as_slice(), b"\0", credential_id].concat()
}

fn kind_of(wrap: &serde_json::Value) -> Option<&str> {
    wrap.get("kind").and_then(|k| k.as_str())
}

/// A failure of the envelope's create / open / re-wrap or of the sealing pass.
#[derive(Debug, thiserror::Error)]
pub enum SealError {
    #[error("this store has no seal header")]
    NoHeader,
    #[error("this store already has a seal header")]
    HeaderExists,
    /// AEAD cannot tell a wrong passphrase from a damaged header, so the error
    /// honestly names both.
    #[error("wrong passphrase, or the store's seal header is damaged")]
    WrongPassphraseOrCorrupt,
    /// No passkey wrap for this credential, or its PRF output does not open
    /// it (another authenticator, or a damaged header — AEAD cannot tell).
    #[error("this passkey does not open the store, or the store's seal header is damaged")]
    PasskeyDidNotOpen,
    /// An empty credential id, or a PRF salt or output that is not
    /// [`PASSKEY_PRF_LEN`] bytes.
    #[error("the passkey's credential id, PRF salt or PRF output is malformed")]
    MalformedPasskey,
    /// The header was written by a newer build: the rows are intact, the app
    /// needs updating.
    #[error("the store's seal header is version {0}, written by a newer build")]
    NewerHeader(u8),
    #[error("the sealed store is locked")]
    Locked,
    /// A handed store key does not open the header's check: a stale key from
    /// a store that was started over, or another store's.
    #[error("the handed store key does not open this store's seal header")]
    WrongStoreKey,
    /// A write did not read back as written; nothing plain was replaced.
    #[error("the sealed form of {0} did not read back")]
    ReadBackFailed(String),
    #[error(transparent)]
    Kdf(#[from] KdfError),
}

/// The header row: a version, the wraps of the store key, and the store-key
/// check. Wraps are kept as raw JSON so a kind this build does not know (a
/// newer build's passkey wrap) survives a re-wrap verbatim.
///
/// `check` is an empty plaintext sealed under the store key itself: it lets a
/// runtime handed the key ([`SealedSecretStore::unlock_with_store_key`])
/// confirm it before sealing a write under it. It is no passphrase verifier —
/// guessing against it means guessing a random 32-byte key.
#[derive(Serialize, Deserialize)]
struct SealHeader {
    v: u8,
    wraps: Vec<serde_json::Value>,
    /// Absent (empty) opens nothing: such a header unlocks only by passphrase.
    #[serde(default)]
    check: String,
}

fn mint_check(store_key: &[u8; 32]) -> String {
    aead_seal::seal_token(store_key, CHECK_AAD, b"")
}

/// A passphrase wrap of the store key.
#[derive(Serialize, Deserialize)]
struct PassphraseWrap {
    kind: String,
    kdf: Argon2idParams,
    salt: Vec<u8>,
    /// `aead_seal` token of the store key.
    sealed: String,
}

impl PassphraseWrap {
    fn mint(
        store_key: &[u8; 32],
        passphrase: &str,
        kdf: Argon2idParams,
    ) -> Result<Self, SealError> {
        let salt = aead_seal::fresh_salt();
        let wrapping = derive_key_argon2id(passphrase.as_bytes(), &salt, kdf)?;
        Ok(Self {
            kind: PASSPHRASE_WRAP.into(),
            kdf,
            salt: salt.to_vec(),
            sealed: aead_seal::seal_token(&wrapping, &wrap_aad(PASSPHRASE_WRAP), store_key),
        })
    }

    /// The store key, or `None` when the passphrase does not open this wrap.
    fn open(&self, passphrase: &str) -> Result<Option<SecretArray32>, SealError> {
        if self.salt.len() != aead_seal::SALT_LEN {
            return Ok(None);
        }
        // An out-of-envelope triple is refused before Argon2 runs — a damaged
        // header, never a reason to allocate what it names.
        if self.kdf.validate().is_err() {
            return Ok(None);
        }
        let wrapping = derive_key_argon2id(passphrase.as_bytes(), &self.salt, self.kdf)?;
        let Some(key) = aead_seal::open_token(&wrapping, &wrap_aad(PASSPHRASE_WRAP), &self.sealed)
        else {
            return Ok(None);
        };
        Ok(<[u8; 32]>::try_from(key.as_slice())
            .ok()
            .map(SecretArray32::new))
    }
}

/// A passkey wrap of the store key: a WebAuthn credential created in the page
/// with the PRF extension, its PRF output (for `prf_salt`) run through
/// `BLAKE3::derive_key` to give the wrapping key (`web.md` § Identity store at
/// rest → *A passkey wrap*).
#[derive(Serialize, Deserialize)]
struct PasskeyWrap {
    kind: String,
    /// The WebAuthn credential's raw id.
    credential_id: Vec<u8>,
    /// The PRF extension's eval input for this credential.
    prf_salt: Vec<u8>,
    /// `aead_seal` token of the store key.
    sealed: String,
}

impl PasskeyWrap {
    fn mint(
        store_key: &[u8; 32],
        credential_id: &[u8],
        prf_salt: &[u8],
        prf_output: &[u8],
    ) -> Self {
        let wrapping = derive_key_blake3(PASSKEY_WRAP_KEY_CONTEXT, prf_output);
        Self {
            kind: PASSKEY_WRAP.into(),
            credential_id: credential_id.to_vec(),
            prf_salt: prf_salt.to_vec(),
            sealed: aead_seal::seal_token(&wrapping, &passkey_wrap_aad(credential_id), store_key),
        }
    }

    /// The store key, or `None` when `prf_output` does not open this wrap.
    fn open(&self, prf_output: &[u8]) -> Option<SecretArray32> {
        let wrapping = derive_key_blake3(PASSKEY_WRAP_KEY_CONTEXT, prf_output);
        let key = aead_seal::open_token(
            &wrapping,
            &passkey_wrap_aad(&self.credential_id),
            &self.sealed,
        )?;
        <[u8; 32]>::try_from(key.as_slice())
            .ok()
            .map(SecretArray32::new)
    }

    /// The well-formed passkey wraps of `wraps`; any other kind, and a passkey
    /// wrap this build cannot read, is passed over (never fatal — the
    /// passphrase still opens the store).
    fn all(wraps: &[serde_json::Value]) -> impl Iterator<Item = Self> + '_ {
        wraps
            .iter()
            .filter(|w| kind_of(w) == Some(PASSKEY_WRAP))
            .filter_map(|w| serde_json::from_value::<Self>(w.clone()).ok())
    }
}

/// What the page needs to ask an authenticator for a passkey wrap's PRF
/// output — readable while the store is locked; neither field is secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PasskeyWrapInfo {
    /// The WebAuthn credential's raw id (the `allowCredentials` entry).
    pub credential_id: Vec<u8>,
    /// The PRF extension's eval input for this credential.
    pub prf_salt: Vec<u8>,
}

/// A fresh PRF salt for a new passkey wrap.
#[must_use]
pub fn fresh_passkey_prf_salt() -> [u8; PASSKEY_PRF_LEN] {
    *fresh_secret_32()
}

/// What one [`SealedSecretStore::resume_sealing`] pass did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SealingReport {
    /// Rows rewritten from plain to sealed in this pass.
    pub sealed: usize,
    /// Rows already sealed (left untouched).
    pub already_sealed: usize,
    /// Sealed rows that do not open under the store key and had no staged copy
    /// to restore from — evidence, left exactly as found.
    pub unreadable: Vec<String>,
}

/// The sealing [`SecretStore`] wrapper (module docs).
///
/// Constructed **locked**: the app shows its unlock / create surface before
/// launch routing reads anything, so the defensive locked-state behavior — a
/// read is `None`, a write is dropped with a warning, the terminal app's
/// sealed store's stance — should never fire in a correctly sequenced app.
pub struct SealedSecretStore<S: SecretStore> {
    inner: S,
    kdf: Argon2idParams,
    key: Mutex<Option<SecretArray32>>,
}

impl<S: SecretStore> SealedSecretStore<S> {
    /// A locked wrapper over `inner`, minting wraps at the interactive
    /// Argon2id parameters.
    pub fn new(inner: S) -> Self {
        Self::with_kdf(inner, Argon2idParams::interactive())
    }

    /// A locked wrapper minting wraps at `kdf` — tests and harnesses, which
    /// cannot afford 64 MiB per derivation. Opening always uses the parameters
    /// a wrap records.
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn with_kdf_params(inner: S, kdf: Argon2idParams) -> Self {
        Self::with_kdf(inner, kdf)
    }

    fn with_kdf(inner: S, kdf: Argon2idParams) -> Self {
        Self {
            inner,
            kdf,
            key: Mutex::new(None),
        }
    }

    /// The wrapped store.
    pub fn inner(&self) -> &S {
        &self.inner
    }

    /// Whether the store has a seal header — the unlock-vs-create branch.
    pub fn has_header(&self) -> bool {
        self.inner.get(SEAL_HEADER_KEY).is_some()
    }

    /// Whether the store still needs [`Self::unlock`] / [`Self::create`].
    pub fn is_locked(&self) -> bool {
        self.key.lock().expect("seal key mutex").is_none()
    }

    /// Forget the store key.
    pub fn lock(&self) {
        *self.key.lock().expect("seal key mutex") = None;
    }

    /// The unlocked store key, for handing to another runtime over the same
    /// rows — the page's other wasm chunks, a newly opened tab — which installs
    /// it with [`Self::unlock_with_store_key`]. `None` while locked. The key
    /// lives in memory only; never persist it (`web.md` § Identity store at
    /// rest).
    pub fn store_key(&self) -> Option<SecretArray32> {
        self.key.lock().expect("seal key mutex").clone()
    }

    /// Install a store key handed by another runtime, after confirming it opens
    /// the header's check — so a stale or foreign key never seals a write.
    ///
    /// # Errors
    ///
    /// [`SealError::NoHeader`]; [`SealError::NewerHeader`];
    /// [`SealError::WrongStoreKey`] (the store stays as it was).
    pub fn unlock_with_store_key(&self, store_key: [u8; 32]) -> Result<(), SealError> {
        let header = self.read_header()?;
        if aead_seal::open_token(&store_key, CHECK_AAD, &header.check).is_none() {
            return Err(SealError::WrongStoreKey);
        }
        *self.key.lock().expect("seal key mutex") = Some(SecretArray32::new(store_key));
        Ok(())
    }

    fn read_header(&self) -> Result<SealHeader, SealError> {
        let raw = self.inner.get(SEAL_HEADER_KEY).ok_or(SealError::NoHeader)?;
        let header: SealHeader =
            serde_json::from_str(&raw).map_err(|_| SealError::WrongPassphraseOrCorrupt)?;
        if header.v > HEADER_VERSION {
            return Err(SealError::NewerHeader(header.v));
        }
        if header.v != HEADER_VERSION {
            return Err(SealError::WrongPassphraseOrCorrupt);
        }
        Ok(header)
    }

    fn write_header(&self, header: &SealHeader) -> Result<(), SealError> {
        let raw = serde_json::to_string(header).expect("a seal header serializes");
        self.inner.set(SEAL_HEADER_KEY, &raw);
        if self.inner.get(SEAL_HEADER_KEY).as_deref() != Some(raw.as_str()) {
            return Err(SealError::ReadBackFailed(SEAL_HEADER_KEY.into()));
        }
        Ok(())
    }

    /// Mint a store key, write the header with one passphrase wrap of it, and
    /// hold the key (the store is unlocked on success).
    ///
    /// # Errors
    ///
    /// [`SealError::HeaderExists`] when a header is already there (never
    /// overwritten — it is the only way into every sealed row);
    /// [`SealError::ReadBackFailed`] when the header write did not stick.
    pub fn create(&self, passphrase: &str) -> Result<(), SealError> {
        if self.has_header() {
            return Err(SealError::HeaderExists);
        }
        let store_key = SecretArray32::from(fresh_secret_32());
        let wrap = PassphraseWrap::mint(&store_key, passphrase, self.kdf)?;
        self.write_header(&SealHeader {
            v: HEADER_VERSION,
            wraps: vec![serde_json::to_value(wrap).expect("a wrap serializes")],
            check: mint_check(&store_key),
        })?;
        *self.key.lock().expect("seal key mutex") = Some(store_key);
        Ok(())
    }

    fn open_store_key(
        &self,
        header: &SealHeader,
        passphrase: &str,
    ) -> Result<SecretArray32, SealError> {
        for wrap in &header.wraps {
            if kind_of(wrap) != Some(PASSPHRASE_WRAP) {
                continue;
            }
            let Ok(wrap) = serde_json::from_value::<PassphraseWrap>(wrap.clone()) else {
                continue;
            };
            if let Some(key) = wrap.open(passphrase)? {
                return Ok(key);
            }
        }
        Err(SealError::WrongPassphraseOrCorrupt)
    }

    /// Open the header's passphrase wrap and hold the store key.
    ///
    /// # Errors
    ///
    /// [`SealError::NoHeader`]; [`SealError::WrongPassphraseOrCorrupt`] (the
    /// store stays locked); [`SealError::NewerHeader`].
    pub fn unlock(&self, passphrase: &str) -> Result<(), SealError> {
        let header = self.read_header()?;
        let key = self.open_store_key(&header, passphrase)?;
        *self.key.lock().expect("seal key mutex") = Some(key);
        Ok(())
    }

    /// Re-wrap the store key under `new`: the header's passphrase wrap is
    /// replaced (fresh salt, this build's parameters) in ONE row write, every
    /// other wrap is kept verbatim, and no row is touched — so at every crash
    /// instant exactly one passphrase opens the store. Unlocks the store.
    ///
    /// # Errors
    ///
    /// As [`Self::unlock`] for `current` (nothing is written), and
    /// [`SealError::ReadBackFailed`] for the header write.
    pub fn change_passphrase(&self, current: &str, new: &str) -> Result<(), SealError> {
        let header = self.read_header()?;
        let key = self.open_store_key(&header, current)?;
        let mut wraps: Vec<serde_json::Value> = header
            .wraps
            .into_iter()
            .filter(|w| kind_of(w) != Some(PASSPHRASE_WRAP))
            .collect();
        let wrap = PassphraseWrap::mint(&key, new, self.kdf)?;
        wraps.insert(0, serde_json::to_value(wrap).expect("a wrap serializes"));
        self.write_header(&SealHeader {
            v: HEADER_VERSION,
            wraps,
            check: mint_check(&key),
        })?;
        *self.key.lock().expect("seal key mutex") = Some(key);
        Ok(())
    }

    /// The header's passkey wraps — what the page asks an authenticator for,
    /// before the store is unlocked.
    ///
    /// # Errors
    ///
    /// As [`Self::unlock`]'s header read.
    pub fn passkey_wraps(&self) -> Result<Vec<PasskeyWrapInfo>, SealError> {
        let header = self.read_header()?;
        Ok(PasskeyWrap::all(&header.wraps)
            .map(|w| PasskeyWrapInfo {
                credential_id: w.credential_id,
                prf_salt: w.prf_salt,
            })
            .collect())
    }

    /// Open the passkey wrap for `credential_id` with the PRF output its
    /// authenticator returned and hold the store key.
    ///
    /// # Errors
    ///
    /// [`SealError::NoHeader`]; [`SealError::NewerHeader`];
    /// [`SealError::PasskeyDidNotOpen`] (the store stays locked).
    pub fn unlock_with_passkey(
        &self,
        credential_id: &[u8],
        prf_output: &[u8],
    ) -> Result<(), SealError> {
        let header = self.read_header()?;
        let key = PasskeyWrap::all(&header.wraps)
            .filter(|w| w.credential_id == credential_id)
            .find_map(|w| w.open(prf_output))
            .ok_or(SealError::PasskeyDidNotOpen)?;
        *self.key.lock().expect("seal key mutex") = Some(key);
        Ok(())
    }

    /// Add a passkey wrap of the held store key — one header write, no row
    /// touched, every other wrap kept verbatim; a wrap already there for the
    /// same credential is replaced.
    ///
    /// # Errors
    ///
    /// [`SealError::MalformedPasskey`]; [`SealError::Locked`] (nothing is
    /// written); as [`Self::unlock`]'s header read; [`SealError::ReadBackFailed`].
    pub fn add_passkey(
        &self,
        credential_id: &[u8],
        prf_salt: &[u8],
        prf_output: &[u8],
    ) -> Result<(), SealError> {
        if credential_id.is_empty()
            || prf_salt.len() != PASSKEY_PRF_LEN
            || prf_output.len() != PASSKEY_PRF_LEN
        {
            return Err(SealError::MalformedPasskey);
        }
        let key = self.store_key().ok_or(SealError::Locked)?;
        let mut header = self.read_header()?;
        header
            .wraps
            .retain(|w| !Self::is_passkey_for(w, credential_id));
        let wrap = PasskeyWrap::mint(&key, credential_id, prf_salt, prf_output);
        header
            .wraps
            .push(serde_json::to_value(wrap).expect("a wrap serializes"));
        self.write_header(&header)
    }

    /// Remove the passkey wrap for `credential_id` — one header write, no row
    /// touched; the passphrase wrap still opens the store. `false` when there
    /// was none.
    ///
    /// # Errors
    ///
    /// [`SealError::Locked`] (removing a way in is the unlocked person's act);
    /// as [`Self::unlock`]'s header read; [`SealError::ReadBackFailed`].
    pub fn remove_passkey(&self, credential_id: &[u8]) -> Result<bool, SealError> {
        if self.is_locked() {
            return Err(SealError::Locked);
        }
        let mut header = self.read_header()?;
        let before = header.wraps.len();
        header
            .wraps
            .retain(|w| !Self::is_passkey_for(w, credential_id));
        if header.wraps.len() == before {
            return Ok(false);
        }
        self.write_header(&header)?;
        Ok(true)
    }

    fn is_passkey_for(wrap: &serde_json::Value, credential_id: &[u8]) -> bool {
        kind_of(wrap) == Some(PASSKEY_WRAP)
            && serde_json::from_value::<PasskeyWrap>(wrap.clone())
                .is_ok_and(|w| w.credential_id == credential_id)
    }

    fn seal_value(key: &[u8; 32], logical_key: &str, value: &str) -> String {
        format!(
            "{SEALED_VALUE_PREFIX}{}",
            aead_seal::seal_token(key, &row_aad(logical_key), value.as_bytes())
        )
    }

    /// The plaintext of a sealed `raw` value at `logical_key`, or `None` when
    /// it does not open there.
    fn open_value(key: &[u8; 32], logical_key: &str, raw: &str) -> Option<String> {
        let token = raw.strip_prefix(SEALED_VALUE_PREFIX)?;
        let plain = aead_seal::open_token(key, &row_aad(logical_key), token)?;
        String::from_utf8(plain.to_vec()).ok()
    }

    /// Write `sealed` to `slot` and confirm it reads back opening to `plain`
    /// as the value of `logical_key`.
    fn write_verified(
        &self,
        key: &[u8; 32],
        slot: &str,
        logical_key: &str,
        sealed: &str,
        plain: &str,
    ) -> Result<(), SealError> {
        self.inner.set(slot, sealed);
        match self.inner.get(slot) {
            Some(back) if Self::open_value(key, logical_key, &back).as_deref() == Some(plain) => {
                Ok(())
            }
            _ => Err(SealError::ReadBackFailed(slot.into())),
        }
    }

    /// Rewrite every plain row among `keys` sealed, in place, resuming a pass
    /// a crash interrupted. For each plain row the sealed form is first written
    /// to a staging row and read back; only then is the plain value replaced,
    /// and that write is read back too before the staging row goes. A sealed
    /// row that does not open but whose staged copy does (a crash mid-replace)
    /// is restored from the staging row.
    ///
    /// # Errors
    ///
    /// [`SealError::Locked`]; [`SealError::ReadBackFailed`] at the first row
    /// whose write did not stick — that row's plain value is still in place,
    /// or its staged copy is, and a later pass resumes from there.
    pub fn resume_sealing<'a>(
        &self,
        keys: impl IntoIterator<Item = &'a str>,
    ) -> Result<SealingReport, SealError> {
        let key = self.store_key().ok_or(SealError::Locked)?;
        let mut report = SealingReport::default();
        for logical_key in keys {
            if is_reserved(logical_key) {
                continue;
            }
            let staging = staging_key(logical_key);
            let staged = self
                .inner
                .get(&staging)
                .and_then(|raw| Self::open_value(&key, logical_key, &raw).map(|p| (raw, p)));
            match self.inner.get(logical_key) {
                None => {}
                Some(raw) if is_sealed_value(&raw) => {
                    if Self::open_value(&key, logical_key, &raw).is_some() {
                        report.already_sealed += 1;
                    } else if let Some((sealed, plain)) = &staged {
                        self.write_verified(&key, logical_key, logical_key, sealed, plain)?;
                        report.sealed += 1;
                    } else {
                        report.unreadable.push(logical_key.to_string());
                        continue;
                    }
                }
                Some(plain) => {
                    let sealed = Self::seal_value(&key, logical_key, &plain);
                    self.write_verified(&key, &staging, logical_key, &sealed, &plain)?;
                    self.write_verified(&key, logical_key, logical_key, &sealed, &plain)?;
                    report.sealed += 1;
                }
            }
            self.inner.delete(&staging);
        }
        Ok(report)
    }
}

impl<S: SecretStore> SealedSecretStore<S> {
    /// Start over: erase this store — every identity-store row among `keys`
    /// (the caller enumerates, as for [`Self::resume_sealing`]), every staging
    /// row among them, and the header last — and forget the store key, so the
    /// next launch is in create mode. The user-confirmed way out of a
    /// forgotten passphrase on a store with no file to delete (`web.md`
    /// § Identity store at rest); the identity comes back from the recovery
    /// kit, an exported key or another device. Keys that name neither an
    /// identity-store row nor a seal row are left alone.
    pub fn start_over<'a>(&self, keys: impl IntoIterator<Item = &'a str>) {
        self.lock();
        for key in keys {
            if key != SEAL_HEADER_KEY && (is_identity_store_key(key) || is_reserved(key)) {
                self.inner.delete(key);
            }
        }
        // The header goes last: until it does, a crash leaves unlock mode over
        // whatever rows remain, and a second start-over finishes the erase.
        self.inner.delete(SEAL_HEADER_KEY);
    }
}

impl<S: SecretStore> SecretStore for SealedSecretStore<S> {
    /// A sealed row's plaintext; a plain row as it rests (a store mid-way
    /// through [`SealedSecretStore::resume_sealing`]). A sealed row that does
    /// not open is returned **as it rests** rather than as absent: absent
    /// would read as "no account here" and invite a write over the only copy,
    /// while the raw value fails every parse as malformed evidence (the
    /// registry's `IndexUnreadable` stance). Locked → `None`.
    fn get(&self, key: &str) -> Option<String> {
        let raw = self.inner.get(key)?;
        if is_reserved(key) {
            return Some(raw);
        }
        let store_key = self.store_key()?;
        if !is_sealed_value(&raw) {
            return Some(raw);
        }
        Some(Self::open_value(&store_key, key, &raw).unwrap_or_else(|| {
            tracing::warn!(
                key,
                "sealed identity-store row does not open under the store key"
            );
            raw
        }))
    }

    fn set(&self, key: &str, value: &str) {
        if is_reserved(key) {
            tracing::warn!(key, "refusing a plain write to a seal-reserved row");
            return;
        }
        let Some(store_key) = self.store_key() else {
            tracing::warn!(key, "sealed identity store is locked; write dropped");
            return;
        };
        self.inner
            .set(key, &Self::seal_value(&store_key, key, value));
    }

    fn delete(&self, key: &str) {
        self.inner.delete(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InMemorySecretStore;

    const FAST: Argon2idParams = Argon2idParams { m: 8, t: 1, p: 1 };

    fn store() -> SealedSecretStore<InMemorySecretStore> {
        SealedSecretStore::with_kdf_params(InMemorySecretStore::new(), FAST)
    }

    fn relaunched(
        s: SealedSecretStore<InMemorySecretStore>,
    ) -> SealedSecretStore<InMemorySecretStore> {
        SealedSecretStore::with_kdf_params(s.inner, FAST)
    }

    #[test]
    fn a_created_store_round_trips_and_a_relaunch_unlocks() {
        let s = store();
        assert!(!s.has_header() && s.is_locked());
        s.create("correct horse").unwrap();
        assert!(!s.is_locked());
        s.set("fauna/index", r#"{"active":"ab"}"#);
        s.set("fauna/ab/secret", "00ff");
        let raw = s.inner().get("fauna/ab/secret").unwrap();
        assert!(is_sealed_value(&raw), "the row rests sealed: {raw}");
        assert!(!raw.contains("00ff"));

        let s = relaunched(s);
        assert!(s.has_header() && s.is_locked());
        assert_eq!(
            s.get("fauna/ab/secret"),
            None,
            "a locked store reads nothing"
        );
        s.unlock("correct horse").unwrap();
        assert_eq!(s.get("fauna/ab/secret").as_deref(), Some("00ff"));
        assert_eq!(s.get("fauna/index").as_deref(), Some(r#"{"active":"ab"}"#));
    }

    #[test]
    fn a_wrong_passphrase_is_refused_and_leaves_the_store_locked() {
        let s = store();
        s.create("right").unwrap();
        let s = relaunched(s);
        assert!(matches!(
            s.unlock("wrong"),
            Err(SealError::WrongPassphraseOrCorrupt)
        ));
        assert!(s.is_locked());
    }

    #[test]
    fn create_never_overwrites_a_header() {
        let s = store();
        s.create("one").unwrap();
        let header = s.inner().get(SEAL_HEADER_KEY);
        assert!(matches!(s.create("two"), Err(SealError::HeaderExists)));
        assert_eq!(s.inner().get(SEAL_HEADER_KEY), header);
    }

    #[test]
    fn a_sealed_value_moved_to_another_slot_does_not_open() {
        let s = store();
        s.create("pw").unwrap();
        s.set("fauna/aa/secret", "aaaa");
        s.set("fauna/bb/secret", "bbbb");
        let moved = s.inner().get("fauna/aa/secret").unwrap();
        s.inner().set("fauna/bb/secret", &moved);
        let read = s.get("fauna/bb/secret").unwrap();
        assert_ne!(read, "aaaa", "the slot is bound into the associated data");
        assert_eq!(
            read, moved,
            "an unopenable row reads as it rests, never as absent"
        );
    }

    #[test]
    fn a_damaged_or_newer_header_is_refused() {
        let s = store();
        s.inner().seed(SEAL_HEADER_KEY, "{not json");
        assert!(matches!(
            s.unlock("pw"),
            Err(SealError::WrongPassphraseOrCorrupt)
        ));
        s.inner().seed(SEAL_HEADER_KEY, r#"{"v":2,"wraps":[]}"#);
        assert!(matches!(s.unlock("pw"), Err(SealError::NewerHeader(2))));
        let s = store();
        assert!(matches!(s.unlock("pw"), Err(SealError::NoHeader)));
    }

    #[test]
    fn an_out_of_envelope_kdf_triple_is_refused_before_argon2_runs() {
        let s = store();
        s.create("pw").unwrap();
        let mut header: serde_json::Value =
            serde_json::from_str(&s.inner().get(SEAL_HEADER_KEY).unwrap()).unwrap();
        header["wraps"][0]["kdf"]["m"] = serde_json::json!(u32::MAX);
        s.inner().seed(SEAL_HEADER_KEY, &header.to_string());
        let s = relaunched(s);
        assert!(matches!(
            s.unlock("pw"),
            Err(SealError::WrongPassphraseOrCorrupt)
        ));
    }

    #[test]
    fn a_rewrap_refuses_the_old_passphrase_opens_the_new_and_changes_no_row() {
        let s = store();
        s.create("old").unwrap();
        s.set("fauna/index", "{}");
        s.set("fauna/ab/secret", "00ff");
        s.set("install/device_secret", "dd");
        let rows = |s: &SealedSecretStore<InMemorySecretStore>| {
            ["fauna/index", "fauna/ab/secret", "install/device_secret"]
                .map(|k| s.inner().get(k).unwrap())
        };
        let before = rows(&s);
        // A wrap of a kind this build does not know survives the re-wrap.
        let mut header: serde_json::Value =
            serde_json::from_str(&s.inner().get(SEAL_HEADER_KEY).unwrap()).unwrap();
        let passkey = serde_json::json!({"kind": "passkey", "credential": "xyz"});
        header["wraps"]
            .as_array_mut()
            .unwrap()
            .push(passkey.clone());
        s.inner().seed(SEAL_HEADER_KEY, &header.to_string());

        assert!(matches!(
            s.change_passphrase("not it", "new"),
            Err(SealError::WrongPassphraseOrCorrupt)
        ));
        s.change_passphrase("old", "new").unwrap();
        assert_eq!(rows(&s), before, "no row's sealed value changed");

        let s = relaunched(s);
        assert!(matches!(
            s.unlock("old"),
            Err(SealError::WrongPassphraseOrCorrupt)
        ));
        s.unlock("new").unwrap();
        assert_eq!(s.get("fauna/ab/secret").as_deref(), Some("00ff"));
        let header: serde_json::Value =
            serde_json::from_str(&s.inner().get(SEAL_HEADER_KEY).unwrap()).unwrap();
        assert!(header["wraps"].as_array().unwrap().contains(&passkey));
    }

    #[test]
    fn locked_writes_drop_and_reserved_rows_take_no_plain_write() {
        let s = store();
        s.set("fauna/index", "{}");
        assert_eq!(s.inner().get("fauna/index"), None);
        s.create("pw").unwrap();
        let header = s.inner().get(SEAL_HEADER_KEY);
        s.set(SEAL_HEADER_KEY, "clobber");
        assert_eq!(s.inner().get(SEAL_HEADER_KEY), header);
    }

    const ROWS: [&str; 3] = ["fauna/index", "fauna/ab/secret", "install/device_secret"];

    #[test]
    fn a_mixed_store_resumes_the_in_place_sealing() {
        let s = store();
        s.create("pw").unwrap();
        // One row already sealed by an interrupted pass, two still plain.
        s.set("fauna/index", r#"{"active":"ab"}"#);
        let already = s.inner().get("fauna/index").unwrap();
        s.inner().seed("fauna/ab/secret", "00ff");
        s.inner().seed("install/device_secret", "dd");
        assert_eq!(
            s.get("fauna/ab/secret").as_deref(),
            Some("00ff"),
            "plain reads through"
        );

        let report = s.resume_sealing(ROWS).unwrap();
        assert_eq!(
            report,
            SealingReport {
                sealed: 2,
                already_sealed: 1,
                unreadable: vec![]
            }
        );
        assert_eq!(
            s.inner().get("fauna/index").unwrap(),
            already,
            "a sealed row is untouched"
        );
        for k in ROWS {
            assert!(
                is_sealed_value(&s.inner().get(k).unwrap()),
                "{k} rests sealed"
            );
            assert_eq!(
                s.inner().get(&staging_key(k)),
                None,
                "{k}'s staging row is gone"
            );
        }
        let s = relaunched(s);
        s.unlock("pw").unwrap();
        assert_eq!(s.get("fauna/ab/secret").as_deref(), Some("00ff"));
        assert_eq!(s.get("install/device_secret").as_deref(), Some("dd"));
        assert_eq!(
            s.resume_sealing(ROWS).unwrap().sealed,
            0,
            "a second pass is a no-op"
        );
    }

    #[test]
    fn resume_sealing_needs_the_store_key() {
        let s = store();
        s.inner().seed("fauna/index", "{}");
        assert!(matches!(s.resume_sealing(ROWS), Err(SealError::Locked)));
        assert_eq!(s.inner().get("fauna/index").as_deref(), Some("{}"));
    }

    /// A store whose writes to rows matching `broken` land damaged — a write
    /// that did not stick.
    struct DamagingStore {
        inner: InMemorySecretStore,
        broken: Mutex<Option<String>>,
    }

    impl SecretStore for DamagingStore {
        fn get(&self, key: &str) -> Option<String> {
            self.inner.get(key)
        }
        fn set(&self, key: &str, value: &str) {
            let damaged = self.broken.lock().unwrap().as_deref() == Some(key);
            self.inner
                .set(key, if damaged { "sealed:v1:AAAA" } else { value });
        }
        fn delete(&self, key: &str) {
            self.inner.delete(key);
        }
    }

    #[test]
    fn no_plain_value_is_replaced_before_its_sealed_form_reads_back() {
        let s = SealedSecretStore::with_kdf_params(
            DamagingStore {
                inner: InMemorySecretStore::new(),
                broken: Mutex::new(None),
            },
            FAST,
        );
        s.create("pw").unwrap();
        s.inner().inner.seed("fauna/ab/secret", "00ff");

        // The staging write does not stick: the plain value stays in place.
        *s.inner().broken.lock().unwrap() = Some(staging_key("fauna/ab/secret"));
        assert!(matches!(
            s.resume_sealing(ROWS),
            Err(SealError::ReadBackFailed(_))
        ));
        assert_eq!(s.inner().get("fauna/ab/secret").as_deref(), Some("00ff"));

        // The replacing write does not stick: the staged copy survives, and the
        // next pass restores the row from it.
        *s.inner().broken.lock().unwrap() = Some("fauna/ab/secret".into());
        assert!(matches!(
            s.resume_sealing(ROWS),
            Err(SealError::ReadBackFailed(_))
        ));
        assert!(s.inner().get(&staging_key("fauna/ab/secret")).is_some());
        *s.inner().broken.lock().unwrap() = None;
        let report = s.resume_sealing(ROWS).unwrap();
        assert_eq!(report.sealed, 1);
        assert_eq!(s.get("fauna/ab/secret").as_deref(), Some("00ff"));
        assert_eq!(s.inner().get(&staging_key("fauna/ab/secret")), None);
    }

    #[test]
    fn an_unopenable_sealed_row_with_no_staged_copy_is_left_as_evidence() {
        let s = store();
        s.create("pw").unwrap();
        s.inner().seed("fauna/ab/secret", "sealed:v1:AAAA");
        let report = s.resume_sealing(ROWS).unwrap();
        assert_eq!(report.unreadable, vec!["fauna/ab/secret".to_string()]);
        assert_eq!(
            s.inner().get("fauna/ab/secret").as_deref(),
            Some("sealed:v1:AAAA")
        );
    }

    #[test]
    fn a_handed_store_key_unlocks_another_runtime_once_the_check_opens() {
        let s = store();
        assert!(s.store_key().is_none(), "a locked store hands no key");
        s.create("pw").unwrap();
        s.set("fauna/ab/secret", "00ff");
        let handed = s.store_key().unwrap();

        // Another runtime over the same rows (another wasm chunk, another tab).
        let other = relaunched(s);
        other.unlock_with_store_key(handed.to_array()).unwrap();
        assert_eq!(other.get("fauna/ab/secret").as_deref(), Some("00ff"));
    }

    #[test]
    fn a_stale_or_foreign_store_key_is_refused_and_installs_nothing() {
        let s = store();
        s.create("pw").unwrap();
        let foreign = store();
        foreign.create("pw").unwrap();
        let s = relaunched(s);
        assert!(matches!(
            s.unlock_with_store_key(foreign.store_key().unwrap().to_array()),
            Err(SealError::WrongStoreKey)
        ));
        assert!(
            s.is_locked(),
            "a key that fails the check is never installed"
        );
        let s = store();
        assert!(matches!(
            s.unlock_with_store_key([7; 32]),
            Err(SealError::NoHeader)
        ));
    }

    #[test]
    fn the_identity_store_rows_are_the_registry_slots_and_nothing_else() {
        let actor = "ab".repeat(32);
        for key in [
            crate::INDEX_KEY.to_string(),
            crate::secret_key(&actor),
            format!("{actor}/device-auth"),
            crate::INSTALL_DEVICE_SECRET.to_string(),
            crate::SIGN_OUT_RECORD_KEY.to_string(),
        ] {
            assert!(
                is_identity_store_key(&key),
                "{key} is an identity-store row"
            );
        }
        for key in [
            SEAL_HEADER_KEY.to_string(),
            format!("{SEAL_STAGING_PREFIX}fauna/index"),
            "fauna-push-subscribed".to_string(),
            "FAUNA_DNS_RECHECK_SECS".to_string(),
            "ab/device-auth".to_string(),
            format!("{}/x", "AB".repeat(32)),
        ] {
            assert!(!is_identity_store_key(&key), "{key} is not");
        }
    }

    #[test]
    fn start_over_erases_the_identity_rows_and_the_seal_and_leaves_the_rest() {
        let s = store();
        s.create("forgotten").unwrap();
        s.set("fauna/index", r#"{"active":"ab"}"#);
        s.set("install/device_secret", "dd");
        s.inner.set(&staging_key("fauna/index"), "staged");
        s.inner.set("fauna_nest_pins", "{}");
        let keys = [
            "fauna/index",
            "install/device_secret",
            "fauna/seal-staging/fauna/index",
            SEAL_HEADER_KEY,
            "fauna_nest_pins",
        ];
        s.start_over(keys);
        assert!(!s.has_header() && s.is_locked());
        for k in &keys[..4] {
            assert_eq!(s.inner.get(k), None, "{k} must be erased");
        }
        assert_eq!(s.inner.get("fauna_nest_pins").as_deref(), Some("{}"));
        // Back in create mode: a new passphrase seals a fresh store.
        let s = relaunched(s);
        s.create("new one").unwrap();
        assert!(s.has_header() && !s.is_locked());
    }

    #[test]
    fn a_rewrap_keeps_the_store_key_check() {
        let s = store();
        s.create("old").unwrap();
        let handed = s.store_key().unwrap();
        s.change_passphrase("old", "new").unwrap();
        let s = relaunched(s);
        s.unlock_with_store_key(handed.to_array()).unwrap();
    }

    const CRED: &[u8] = b"credential-one";
    const PRF: [u8; 32] = [0x5a; 32];

    /// A store created under "pw" with one secret row and a passkey wrap for
    /// [`CRED`] whose authenticator answers [`PRF`].
    fn store_with_passkey() -> (SealedSecretStore<InMemorySecretStore>, [u8; 32]) {
        let s = store();
        s.create("pw").unwrap();
        s.set("fauna/ab/secret", "00ff");
        let salt = fresh_passkey_prf_salt();
        s.add_passkey(CRED, &salt, &PRF).unwrap();
        (relaunched(s), salt)
    }

    #[test]
    fn a_passkey_wrap_opens_the_same_store_and_the_passphrase_still_does() {
        let (s, salt) = store_with_passkey();
        assert_eq!(
            s.passkey_wraps().unwrap(),
            vec![PasskeyWrapInfo {
                credential_id: CRED.to_vec(),
                prf_salt: salt.to_vec(),
            }],
            "what the page asks the authenticator for is readable while locked"
        );
        s.unlock_with_passkey(CRED, &PRF).unwrap();
        assert_eq!(s.get("fauna/ab/secret").as_deref(), Some("00ff"));

        let s = relaunched(s);
        s.unlock("pw").unwrap();
        assert_eq!(s.get("fauna/ab/secret").as_deref(), Some("00ff"));
    }

    #[test]
    fn a_wrong_prf_output_or_unknown_credential_is_refused_and_leaves_the_store_locked() {
        let (s, _) = store_with_passkey();
        assert!(matches!(
            s.unlock_with_passkey(CRED, &[0x5b; 32]),
            Err(SealError::PasskeyDidNotOpen)
        ));
        assert!(matches!(
            s.unlock_with_passkey(b"another credential", &PRF),
            Err(SealError::PasskeyDidNotOpen)
        ));
        assert!(s.is_locked());
    }

    #[test]
    fn a_passkey_wrap_moved_to_another_credential_does_not_open() {
        let (s, _) = store_with_passkey();
        let mut header: serde_json::Value =
            serde_json::from_str(&s.inner().get(SEAL_HEADER_KEY).unwrap()).unwrap();
        let other = b"credential-two".to_vec();
        header["wraps"][1]["credential_id"] = serde_json::json!(other);
        s.inner().seed(SEAL_HEADER_KEY, &header.to_string());
        assert!(matches!(
            s.unlock_with_passkey(&other, &PRF),
            Err(SealError::PasskeyDidNotOpen)
        ));
    }

    #[test]
    fn adding_a_passkey_needs_the_unlocked_store_and_a_well_formed_credential() {
        let s = store();
        s.create("pw").unwrap();
        let salt = fresh_passkey_prf_salt();
        assert!(matches!(
            s.add_passkey(b"", &salt, &PRF),
            Err(SealError::MalformedPasskey)
        ));
        assert!(matches!(
            s.add_passkey(CRED, &salt[..16], &PRF),
            Err(SealError::MalformedPasskey)
        ));
        assert!(matches!(
            s.add_passkey(CRED, &salt, &PRF[..31]),
            Err(SealError::MalformedPasskey)
        ));
        let header = s.inner().get(SEAL_HEADER_KEY);
        let s = relaunched(s);
        assert!(matches!(
            s.add_passkey(CRED, &salt, &PRF),
            Err(SealError::Locked)
        ));
        assert_eq!(s.inner().get(SEAL_HEADER_KEY), header, "nothing written");
    }

    #[test]
    fn re_adding_a_credential_replaces_its_wrap() {
        let (s, _) = store_with_passkey();
        s.unlock("pw").unwrap();
        let salt = fresh_passkey_prf_salt();
        s.add_passkey(CRED, &salt, &[0x11; 32]).unwrap();
        assert_eq!(s.passkey_wraps().unwrap().len(), 1);
        let s = relaunched(s);
        assert!(s.unlock_with_passkey(CRED, &PRF).is_err());
        s.unlock_with_passkey(CRED, &[0x11; 32]).unwrap();
    }

    #[test]
    fn removing_the_passkey_wrap_leaves_the_passphrase_path_and_changes_no_row() {
        let (s, _) = store_with_passkey();
        let row = s.inner().get("fauna/ab/secret");
        assert!(matches!(s.remove_passkey(CRED), Err(SealError::Locked)));
        s.unlock_with_passkey(CRED, &PRF).unwrap();
        assert!(s.remove_passkey(CRED).unwrap());
        assert!(!s.remove_passkey(CRED).unwrap(), "already gone");
        assert_eq!(s.inner().get("fauna/ab/secret"), row);
        assert!(s.passkey_wraps().unwrap().is_empty());

        let s = relaunched(s);
        assert!(matches!(
            s.unlock_with_passkey(CRED, &PRF),
            Err(SealError::PasskeyDidNotOpen)
        ));
        s.unlock("pw").unwrap();
        assert_eq!(s.get("fauna/ab/secret").as_deref(), Some("00ff"));
    }

    #[test]
    fn a_passphrase_change_keeps_the_passkey_and_the_check() {
        let (s, _) = store_with_passkey();
        s.unlock("pw").unwrap();
        let handed = s.store_key().unwrap();
        s.change_passphrase("pw", "new").unwrap();
        let s = relaunched(s);
        s.unlock_with_passkey(CRED, &PRF).unwrap();
        let s = relaunched(s);
        s.unlock_with_store_key(handed.to_array()).unwrap();
    }

    #[test]
    fn a_malformed_passkey_wrap_is_skipped_never_fatal() {
        let s = store();
        s.create("pw").unwrap();
        let mut header: serde_json::Value =
            serde_json::from_str(&s.inner().get(SEAL_HEADER_KEY).unwrap()).unwrap();
        header["wraps"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"kind": "passkey", "credential": "xyz"}));
        s.inner().seed(SEAL_HEADER_KEY, &header.to_string());
        let s = relaunched(s);
        assert!(s.passkey_wraps().unwrap().is_empty());
        s.unlock("pw").unwrap();
    }
}
