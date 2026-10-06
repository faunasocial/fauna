//! The hosted principal's holder key — minted by the host at install, held on
//! the host side of the sandbox, and reached by the plugin through operations
//! only (`third-party.md` § The principal model, rule 2; `wit/plugin.wit`
//! `holder`).

use fauna_mls::wrapped_blob::{WrappedScopeKey, generate_x25519_keypair, unseal_capability};

/// An X25519 keypair whose secret half never leaves this value: the public
/// half is what the principal row holds, the secret is what
/// [`HolderKey::open_grant`] uses. `Debug` prints the public half only.
#[derive(Clone)]
pub struct HolderKey {
    secret: [u8; 32],
    public: [u8; 32],
}

impl std::fmt::Debug for HolderKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HolderKey")
            .field("public", &self.public)
            .finish_non_exhaustive()
    }
}

impl Drop for HolderKey {
    fn drop(&mut self) {
        // Best-effort scrub; the compiler may elide it, which is the usual
        // caveat and no worse than any other key the nest holds in memory.
        for b in &mut self.secret {
            unsafe { std::ptr::write_volatile(b, 0) };
        }
    }
}

impl HolderKey {
    /// Mint a fresh key — the install's act.
    pub fn mint() -> Self {
        let (secret, public) = generate_x25519_keypair();
        Self { secret, public }
    }

    /// Rebuild from the secret the state scope stored (the public half is
    /// re-derived through the same KEM, so a stored secret can never pair with
    /// a public key it does not own).
    pub fn from_secret(secret: [u8; 32]) -> Self {
        let public = x25519_public(&secret);
        Self { secret, public }
    }

    /// The public half — the principal row's `holder_x25519`.
    pub fn public(&self) -> &[u8; 32] {
        &self.public
    }

    /// The secret half, for the embedder's state scope ONLY — never lowered
    /// into a plugin.
    pub fn secret_for_storage(&self) -> &[u8; 32] {
        &self.secret
    }

    /// `holder.open-grant`: open one canonical-DAG-CBOR [`WrappedScopeKey`]
    /// that `owner` sealed to this key. Errors are strings for the plugin;
    /// nothing in them names the secret.
    pub fn open_grant(&self, owner: &[u8], wrapped: &[u8]) -> Result<Vec<u8>, String> {
        let owner: [u8; 32] = owner
            .try_into()
            .map_err(|_| "owner must be a 32-byte actor id".to_string())?;
        let wrapped = WrappedScopeKey::from_canonical_bytes(wrapped)
            .map_err(|e| format!("wrapped key refused: {e}"))?;
        unseal_capability(&wrapped, &owner, &self.secret).map_err(|e| format!("open failed: {e}"))
    }
}

/// The X25519 public key of `secret` — the base-point multiplication every
/// X25519 implementation agrees on, so a key minted by [`HolderKey::mint`]
/// and one rebuilt by [`HolderKey::from_secret`] are the same pair.
fn x25519_public(secret: &[u8; 32]) -> [u8; 32] {
    x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(*secret)).to_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_mls::wrapped_blob::{ScopeTuple, seal_capability};

    #[test]
    fn a_rebuilt_key_pairs_with_its_minted_public_half() {
        let minted = HolderKey::mint();
        let rebuilt = HolderKey::from_secret(*minted.secret_for_storage());
        assert_eq!(rebuilt.public(), minted.public());
    }

    #[test]
    fn opens_what_was_sealed_to_it_and_nothing_else() {
        let key = HolderKey::mint();
        let other = HolderKey::mint();
        let owner = [0xA1u8; 32];
        let scope = ScopeTuple {
            class: "content.read".into(),
            kind: Some("ext.example.com.notes".into()),
            tier: None,
            set: None,
            factor: None,
        };
        let wrapped = seal_capability(b"the content key", &owner, &scope, None, key.public())
            .unwrap()
            .to_canonical_bytes()
            .unwrap();
        assert_eq!(
            key.open_grant(&owner, &wrapped).unwrap(),
            b"the content key".to_vec()
        );
        assert!(other.open_grant(&owner, &wrapped).is_err());
        assert!(key.open_grant(&[0xB2u8; 32], &wrapped).is_err());
        assert!(key.open_grant(&owner[..31], &wrapped).is_err());
        assert!(key.open_grant(&owner, b"not cbor").is_err());
    }
}
