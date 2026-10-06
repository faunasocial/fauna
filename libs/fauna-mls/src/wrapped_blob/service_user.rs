//! Bridge service-user keyfile. Holds Ed25519 (sign for WS-RPC) and
//! X25519 (HPKE decrypt for sealed blobs) secrets in one DAG-CBOR file.

use crate::wrapped_blob::envelope::generate_x25519_keypair;
use crate::wrapped_blob::format::{UnwrapError, WrapError};
use ed25519_dalek::SigningKey;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;
use std::path::Path;
use zeroize::Zeroizing;

/// Keyfile format version.
pub const KEYFILE_FORMAT_VERSION: u8 = 1;

/// On-disk service-user keyfile.
///
/// Carries an Ed25519 signing seed (used to sign WS-RPC frames) and
/// an X25519 private key (used to HPKE-decrypt TLS cert bundles).
/// Both secret fields zeroize on drop; metadata fields don't.
#[derive(Debug, Clone, Serialize, Deserialize, zeroize::Zeroize, zeroize::ZeroizeOnDrop)]
pub struct ServiceUserKeyfile {
    #[zeroize(skip)]
    #[serde(rename = "v")]
    pub version: u8,
    #[zeroize(skip)]
    pub role: String, // "mta" or "mda"
    #[zeroize(skip)]
    pub bridge_id: String,
    /// Ed25519 32-byte signing seed.
    pub ed25519_seed: ByteBuf,
    /// X25519 32-byte HPKE decryption private key.
    pub x25519_priv: ByteBuf,
    #[zeroize(skip)]
    pub created_at: u64,
}

impl ServiceUserKeyfile {
    /// Generate a fresh keyfile for a bridge of the given role and id.
    #[must_use]
    pub fn generate(role: &str, bridge_id: &str, created_at: u64) -> Self {
        let mut ed_seed = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut ed_seed);
        let (x_priv, _x_pub) = generate_x25519_keypair();

        Self {
            version: KEYFILE_FORMAT_VERSION,
            role: role.into(),
            bridge_id: bridge_id.into(),
            ed25519_seed: ByteBuf::from(ed_seed.to_vec()),
            x25519_priv: ByteBuf::from(x_priv.to_vec()),
            created_at,
        }
    }

    /// Encode to DAG-CBOR bytes.
    ///
    /// # Errors
    ///
    /// Returns `WrapError::CborEncode` on encoding failure.
    pub fn to_bytes(&self) -> Result<Vec<u8>, WrapError> {
        fauna_cbor::encode_canonical(self).map_err(|e| WrapError::CborEncode(e.to_string()))
    }

    /// Decode from DAG-CBOR bytes.
    ///
    /// # Errors
    ///
    /// Returns `UnwrapError::InvalidFormat` for bad CBOR, unknown
    /// version, or wrong key-seed lengths.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, UnwrapError> {
        let kf: Self = fauna_cbor::decode_strict(bytes)
            .map_err(|e| UnwrapError::InvalidFormat(format!("cbor decode: {e}")))?;
        if kf.version != KEYFILE_FORMAT_VERSION {
            return Err(UnwrapError::InvalidFormat(format!(
                "unsupported keyfile version: {}",
                kf.version
            )));
        }
        if kf.ed25519_seed.len() != 32 {
            return Err(UnwrapError::InvalidFormat(
                "ed25519_seed must be 32 bytes".into(),
            ));
        }
        if kf.x25519_priv.len() != 32 {
            return Err(UnwrapError::InvalidFormat(
                "x25519_priv must be 32 bytes".into(),
            ));
        }
        Ok(kf)
    }

    /// Read keyfile from disk.
    ///
    /// # Errors
    ///
    /// Returns `UnwrapError::InvalidFormat` for read failure or any
    /// `from_bytes` failure (bad CBOR, wrong version, wrong lengths).
    pub fn load(path: &Path) -> Result<Self, UnwrapError> {
        let bytes = std::fs::read(path)
            .map_err(|e| UnwrapError::InvalidFormat(format!("read {path:?}: {e}")))?;
        Self::from_bytes(&bytes)
    }

    /// Atomically write keyfile to disk with mode 0600.
    ///
    /// Writes via a temporary file in the parent directory then
    /// renames; ensures crash-safety.
    ///
    /// # Errors
    ///
    /// Returns `WrapError::InvalidInput` for path/IO problems.
    /// Returns `WrapError::CborEncode` on encoding failure.
    #[cfg(unix)]
    pub fn save(&self, path: &Path) -> Result<(), WrapError> {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;

        let bytes = self.to_bytes()?;
        let parent = path
            .parent()
            .ok_or_else(|| WrapError::InvalidInput("keyfile path must have a parent".into()))?;
        // `tempfile_in` creates the temp file exclusively (O_CREAT|O_EXCL) under
        // a random, unpredictable name, so — unlike a write-then-chmod on a
        // FIXED path — there is no pre-existing target an attacker could plant
        // and no crash-residue reuse. The explicit chmod below still lands
        // BEFORE `write_all`, so no secret byte is ever written at a mode wider
        // than 0600. Reviewed and cleared, no rewrite needed.
        let mut tmp = tempfile::Builder::new()
            .prefix(".fauna-keyfile-")
            .tempfile_in(parent)
            .map_err(|e| WrapError::InvalidInput(format!("tempfile: {e}")))?;
        // Set the temp file mode to 0600 before writing.
        let perms = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(tmp.path(), perms)
            .map_err(|e| WrapError::InvalidInput(format!("chmod: {e}")))?;
        tmp.write_all(&bytes)
            .map_err(|e| WrapError::InvalidInput(format!("write: {e}")))?;
        tmp.flush()
            .map_err(|e| WrapError::InvalidInput(format!("flush: {e}")))?;
        tmp.persist(path)
            .map_err(|e| WrapError::InvalidInput(format!("persist: {e}")))?;
        Ok(())
    }

    /// Construct an Ed25519 signing key.
    ///
    /// `SigningKey` implements `ZeroizeOnDrop` natively, so the
    /// returned value zeroes its secret material on drop without an
    /// outer `Zeroizing` wrapper.
    ///
    /// # Errors
    ///
    /// Returns `UnwrapError::InvalidFormat` if `ed25519_seed` is the
    /// wrong length (caught by `from_bytes` for wire-decoded keyfiles
    /// but defensive).
    pub fn signing_key(&self) -> Result<SigningKey, UnwrapError> {
        if self.ed25519_seed.len() != 32 {
            return Err(UnwrapError::InvalidFormat("ed25519_seed length".into()));
        }
        let mut seed = Zeroizing::new([0u8; 32]);
        seed.copy_from_slice(&self.ed25519_seed);
        Ok(SigningKey::from_bytes(&seed))
    }

    /// Construct the X25519 secret as a 32-byte array (zeroized on drop).
    ///
    /// # Errors
    ///
    /// Returns `UnwrapError::InvalidFormat` if `x25519_priv` is the
    /// wrong length.
    pub fn x25519_secret(&self) -> Result<Zeroizing<[u8; 32]>, UnwrapError> {
        if self.x25519_priv.len() != 32 {
            return Err(UnwrapError::InvalidFormat("x25519_priv length".into()));
        }
        let mut out = Zeroizing::new([0u8; 32]);
        out.copy_from_slice(&self.x25519_priv);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Only the `#[cfg(unix)]` permission tests below use a temp dir; scope the
    // import so the win build doesn't flag it as unused.
    #[cfg(unix)]
    use tempfile::TempDir;

    #[test]
    fn generate_creates_well_formed_keyfile() {
        let kf = ServiceUserKeyfile::generate("mta", "bridge-1", 1_700_000_000);
        assert_eq!(kf.version, 1);
        assert_eq!(kf.role, "mta");
        assert_eq!(kf.bridge_id, "bridge-1");
        assert_eq!(kf.ed25519_seed.len(), 32);
        assert_eq!(kf.x25519_priv.len(), 32);
        // Two fresh generations differ.
        let kf2 = ServiceUserKeyfile::generate("mta", "bridge-1", 1_700_000_000);
        assert_ne!(kf.ed25519_seed, kf2.ed25519_seed);
        assert_ne!(kf.x25519_priv, kf2.x25519_priv);
    }

    #[test]
    fn cbor_roundtrip() {
        let kf = ServiceUserKeyfile::generate("mda", "bridge-2", 1_700_000_000);
        let bytes = kf.to_bytes().unwrap();
        let decoded = ServiceUserKeyfile::from_bytes(&bytes).unwrap();
        assert_eq!(decoded.role, kf.role);
        assert_eq!(decoded.bridge_id, kf.bridge_id);
        assert_eq!(decoded.ed25519_seed, kf.ed25519_seed);
        assert_eq!(decoded.x25519_priv, kf.x25519_priv);
        assert_eq!(decoded.created_at, kf.created_at);
    }

    #[cfg(unix)]
    #[test]
    fn save_then_load_preserves_keyfile() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("mta.key");
        let kf = ServiceUserKeyfile::generate("mta", "b1", 1_700_000_000);
        kf.save(&path).unwrap();
        let loaded = ServiceUserKeyfile::load(&path).unwrap();
        assert_eq!(loaded.bridge_id, "b1");
        assert_eq!(loaded.ed25519_seed, kf.ed25519_seed);
        assert_eq!(loaded.x25519_priv, kf.x25519_priv);
    }

    #[test]
    fn unknown_version_rejected() {
        let mut kf = ServiceUserKeyfile::generate("mta", "b1", 1);
        kf.version = 99;
        let bytes = kf.to_bytes().unwrap();
        let err = ServiceUserKeyfile::from_bytes(&bytes).unwrap_err();
        assert!(matches!(err, UnwrapError::InvalidFormat(_)));
    }

    #[test]
    fn signing_key_is_consistent() {
        let kf = ServiceUserKeyfile::generate("mta", "b1", 1);
        let sk1 = kf.signing_key().unwrap();
        let sk2 = kf.signing_key().unwrap();
        assert_eq!(sk1.to_bytes(), sk2.to_bytes());
    }

    #[cfg(unix)]
    #[test]
    fn save_produces_0600_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("mta.key");
        let kf = ServiceUserKeyfile::generate("mta", "b1", 1_700_000_000);
        kf.save(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "keyfile must be mode 0600, got {:o}",
            mode & 0o777
        );
    }
}
