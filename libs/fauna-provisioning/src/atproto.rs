//! ATProto identity keygen + seal-to-bridge (feature `atproto-seal`).
//!
//! Feeds nest-side **provision-on-read** for the ATProto PDS bridge: mint the
//! two bridge-custodied K-256 keys for a user's did:plc identity — the repo
//! signing key and the bridge's *junior* PLC rotation key — seal the secrets to
//! the `atproto.pds` bridge's attested X25519 (the DKIM/TLS blob pattern), and
//! return the public halves as `did:key` strings for nest to persist as
//! plaintext data (`atproto-pds-bridge.md` § State & data shape).
//!
//! The user-custodied **senior** rotation key is deliberately absent: it is
//! generated and held in the user's client credential store and only its
//! `did:key` pubkey ever reaches the nest. Nothing here may grow a path that
//! carries a user rotation *secret* — that would collapse the ratified custody
//! split.

use k256::elliptic_curve::sec1::ToEncodedPoint as _;
use rand::rngs::OsRng;

use crate::error::ProvisionError;

/// The output of [`seal_atproto_identity_for_provision`]: the sealed secret
/// blob (openable only by the bridge) plus the plaintext public halves.
#[derive(Debug, Clone)]
pub struct AtprotoProvisionedIdentityKeys {
    /// Canonical-CBOR [`fauna_mls::wrapped_blob::AtprotoIdentityBlob`] bytes —
    /// nest persists these beside the identity row.
    pub sealed_blob: Vec<u8>,
    /// The repo signing key's public half (`did:key:zQ3sh…`).
    pub signing_pub_did_key: String,
    /// The bridge rotation key's public half (`did:key:zQ3sh…`).
    pub rotation_pub_did_key: String,
}

/// Curve tag the bundle carries for both bridge-custodied keys.
const CURVE_K256: &str = "k256";

fn fresh_k256() -> (Vec<u8>, String) {
    let secret = k256::SecretKey::random(&mut OsRng);
    let compressed = secret.public_key().to_encoded_point(true);
    let did_key = fauna_protocol::atproto::encode_did_key(
        fauna_protocol::atproto::DidKeyCurve::K256,
        compressed.as_bytes(),
    )
    .expect("compressed SEC1 point from k256 is always 33 bytes");
    (secret.to_bytes().to_vec(), did_key)
}

/// Mint a user's bridge-custodied ATProto identity keys and seal the secrets
/// to the bridge's attested X25519 pubkey.
///
/// # Errors
///
/// Returns `ProvisionError::Other` for seal/encode failures.
pub fn seal_atproto_identity_for_provision(
    actor_id: &[u8; 32],
    bridge_x25519_pubkey: &[u8; 32],
) -> Result<AtprotoProvisionedIdentityKeys, ProvisionError> {
    use fauna_mls::wrapped_blob::{AtprotoIdentityKeyBundle, seal_atproto_identity};

    let (signing_priv, signing_pub_did_key) = fresh_k256();
    let (rotation_priv, rotation_pub_did_key) = fresh_k256();

    let bundle = AtprotoIdentityKeyBundle {
        actor_id: actor_id.to_vec(),
        signing_priv,
        signing_curve: CURVE_K256.into(),
        signing_pub_did_key: signing_pub_did_key.clone(),
        rotation_priv,
        rotation_curve: CURVE_K256.into(),
        rotation_pub_did_key: rotation_pub_did_key.clone(),
        issued_at: fauna_core::data::Timestamp::now_secs().max(0) as u64,
    };
    let blob = seal_atproto_identity(&bundle, actor_id, bridge_x25519_pubkey)
        .map_err(|e| ProvisionError::Other(format!("atproto identity seal: {e}")))?;
    let sealed_blob = blob
        .to_canonical_bytes()
        .map_err(|e| ProvisionError::Other(format!("atproto identity blob encode: {e}")))?;

    Ok(AtprotoProvisionedIdentityKeys {
        sealed_blob,
        signing_pub_did_key,
        rotation_pub_did_key,
    })
}

/// Mint the bridge-wide ATProto session-token secret (32 random bytes for
/// HS256) and seal it to the bridge's attested X25519 pubkey — the
/// provision-on-read mint for `fauna.bridges.atproto.fetch_session_secret_blob`
/// (`atproto-pds-full.md` § Key material inventory, bridge-wide row). The
/// plaintext secret lives only in this function's frame; nest persists and
/// returns ciphertext.
///
/// # Errors
///
/// Returns `ProvisionError::Other` for seal/encode failures.
pub fn seal_atproto_session_secret_for_provision(
    bridge_role: &str,
    bridge_id: &str,
    bridge_x25519_pubkey: &[u8; 32],
) -> Result<Vec<u8>, ProvisionError> {
    use fauna_mls::wrapped_blob::{AtprotoSessionSecretBundle, seal_atproto_session_secret};
    use rand::RngCore as _;

    let mut secret = vec![0u8; 32];
    OsRng.fill_bytes(&mut secret);
    let bundle = AtprotoSessionSecretBundle {
        secret,
        issued_at: fauna_core::data::Timestamp::now_secs().max(0) as u64,
    };
    let blob = seal_atproto_session_secret(&bundle, bridge_role, bridge_id, bridge_x25519_pubkey)
        .map_err(|e| ProvisionError::Other(format!("atproto session-secret seal: {e}")))?;
    blob.to_canonical_bytes()
        .map_err(|e| ProvisionError::Other(format!("atproto session-secret blob encode: {e}")))
}

/// RFC-7638 JWK thumbprint of a P-256 verifying key — re-exported from
/// [`crate::oauth_issuer`], which owns it.
///
/// It moved when the nest-held OAuth issuer needed the same recipe outside
/// this module's feature: the thumbprint is a JOSE primitive over a P-256 key,
/// not an ATProto one. The re-export keeps `atproto::rfc7638_p256_thumbprint`
/// resolving for every existing caller and for the Go `ecThumbprint` parity
/// chain, and there is still exactly ONE implementation — two copies of one
/// recipe is the drift its own doc comment warns about.
pub use crate::oauth_issuer::rfc7638_p256_thumbprint;

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_mls::wrapped_blob::{
        AtprotoIdentityBlob, AtprotoIdentityPublishedKeys, generate_x25519_keypair,
        unseal_atproto_identity,
    };

    /// The RFC 7638 recipe, pinned to the thumbprint of a fixed scalar so a
    /// change to it is a red test here before it is a `kid` or `jkt` that
    /// another implementation computes differently.
    #[test]
    fn rfc7638_thumbprint_is_pinned_to_a_fixed_scalar() {
        let scalar = [0x42u8; 32];
        let signing_key = p256::ecdsa::SigningKey::from_bytes(&scalar.into())
            .expect("fixed scalar is a valid key");
        assert_eq!(
            rfc7638_p256_thumbprint(signing_key.verifying_key()),
            "Dwhk3O9GXmaAbt1mdgUKbjwl127gQtvRuBzEJcPhLDo"
        );
    }

    #[test]
    fn session_secret_provision_roundtrips() {
        use fauna_mls::wrapped_blob::{AtprotoSessionSecretBlob, unseal_atproto_session_secret};

        let (bridge_sk, bridge_pk) = generate_x25519_keypair();
        let sealed =
            seal_atproto_session_secret_for_provision("atproto.pds", "pds-1", &bridge_pk).unwrap();
        let blob = AtprotoSessionSecretBlob::from_canonical_bytes(&sealed).unwrap();
        let bundle = unseal_atproto_session_secret(&blob, &bridge_sk).unwrap();
        assert_eq!(bundle.secret.len(), 32);
        // Two provisions mint independent secrets.
        let sealed2 =
            seal_atproto_session_secret_for_provision("atproto.pds", "pds-1", &bridge_pk).unwrap();
        let blob2 = AtprotoSessionSecretBlob::from_canonical_bytes(&sealed2).unwrap();
        let bundle2 = unseal_atproto_session_secret(&blob2, &bridge_sk).unwrap();
        assert_ne!(bundle.secret, bundle2.secret);
    }

    #[test]
    fn provision_roundtrips_and_pubkeys_rederive() {
        let (bridge_sk, bridge_pk) = generate_x25519_keypair();
        let actor = [0x51u8; 32];
        let keys = seal_atproto_identity_for_provision(&actor, &bridge_pk).expect("provision");

        assert!(keys.signing_pub_did_key.starts_with("did:key:zQ3s"));
        assert!(keys.rotation_pub_did_key.starts_with("did:key:zQ3s"));
        assert_ne!(keys.signing_pub_did_key, keys.rotation_pub_did_key);

        let blob = AtprotoIdentityBlob::from_canonical_bytes(&keys.sealed_blob).expect("decode");
        let published = AtprotoIdentityPublishedKeys {
            signing_pub_did_key: &keys.signing_pub_did_key,
            rotation_pub_did_key: &keys.rotation_pub_did_key,
        };
        let bundle = unseal_atproto_identity(&blob, &bridge_sk, &published).expect("unseal");
        assert_eq!(bundle.actor_id, actor.to_vec());
        assert_eq!(bundle.signing_curve, "k256");
        assert_eq!(bundle.rotation_curve, "k256");

        // The sealed scalars re-derive exactly the advertised did:key pubkeys —
        // the property the Go bridge's unseal depends on.
        for (scalar, did_key) in [
            (&bundle.signing_priv, &keys.signing_pub_did_key),
            (&bundle.rotation_priv, &keys.rotation_pub_did_key),
        ] {
            let sk = k256::SecretKey::from_slice(scalar).expect("32-byte scalar");
            let compressed = sk.public_key().to_encoded_point(true);
            let rederived = fauna_protocol::atproto::encode_did_key(
                fauna_protocol::atproto::DidKeyCurve::K256,
                compressed.as_bytes(),
            )
            .unwrap();
            assert_eq!(&rederived, did_key);
        }
    }

    #[test]
    fn wrong_bridge_secret_cannot_open() {
        let (_, bridge_pk) = generate_x25519_keypair();
        let (other_sk, _) = generate_x25519_keypair();
        let keys = seal_atproto_identity_for_provision(&[0x52u8; 32], &bridge_pk).unwrap();
        let blob = AtprotoIdentityBlob::from_canonical_bytes(&keys.sealed_blob).unwrap();
        let published = AtprotoIdentityPublishedKeys {
            signing_pub_did_key: &keys.signing_pub_did_key,
            rotation_pub_did_key: &keys.rotation_pub_did_key,
        };
        assert!(unseal_atproto_identity(&blob, &other_sk, &published).is_err());
    }
}
