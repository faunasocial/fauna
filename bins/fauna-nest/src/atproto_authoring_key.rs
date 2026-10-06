//! The D10 delegated per-account authoring sub-key `K`
//! (`docs/goal/behavior/atproto-pds-full.md` D10; the mint / provision /
//! revoke lifecycle its three User-class kinds drive).
//!
//! `K` is an Ed25519 keypair the nest mints on first `fetch_authoring_key`
//! (provision-on-read, first-write-wins). Its secret rests wrapped under the
//! **nest-internal key-encryption key** with its own domain-separated context
//! — the Nostr / NIP-46-bunker key-crypto pattern
//! (`docs/goal/architecture/key-material-hierarchy.md` § Nest-internal
//! key-encryption key), **not** the bridge-attested x25519 seal used for the
//! session-secret blob. The secret never leaves the nest process; only `K_pub`
//! is public. A client then identity-signs a `DeviceAuthorization` cert over
//! `K_pub` and uploads it via `provision_authoring_delegation`; the round-trip
//! write path (F2.2 slice 3) unwraps `K` to sign external-app posts, embedding
//! that cert in the post wire's `signer_auth` so any nest verifies the chain.

use anyhow::Result;
use fauna_core::data::Capability;

use crate::db::CacheDb;
use crate::db::atproto_pds::AuthoringKeyRow;

/// Wrap the authoring sub-key's Ed25519 secret for storage at rest.
pub fn encrypt_authoring_key_secret(
    nest_signing_key_bytes: &[u8; 32],
    secret: &[u8; 32],
) -> Result<Vec<u8>> {
    crate::nest_kek::wrap_32(
        crate::nest_kek::ATPROTO_AUTHORING_CONTEXT,
        nest_signing_key_bytes,
        secret,
    )
}

/// Unwrap the authoring sub-key's Ed25519 secret.
pub fn decrypt_authoring_key_secret(
    nest_signing_key_bytes: &[u8; 32],
    ciphertext: &[u8],
) -> Result<[u8; 32]> {
    crate::nest_kek::unwrap_32(
        crate::nest_kek::ATPROTO_AUTHORING_CONTEXT,
        nest_signing_key_bytes,
        ciphertext,
    )
}

/// Whether a stored delegation cert still authorizes signing **right now**.
///
/// This is the *authoring-time* half of D10's expiry story, and it asks a
/// question neither existing gate answers:
///
/// - [`verify_delegation_cert`] check 5 runs at **provision** time — it refuses
///   a cert that is born expired, then never runs again.
/// - `verify_signed_or_delegated` step 5 (`fauna_core::encoding`) runs at
///   **verify** time and compares `expires_at` against *the value's own*
///   `created_at`. That is correct where it lives: a post authorized at
///   creation must stay verifiable forever (`atproto-pds-full.md` D10 §
///   Revocation), so re-verification must not be relative to today's clock.
///
/// Neither answers *"may this cert author a NEW value now"* — and the
/// round-trip write arm adopts the external record's `created_at` verbatim
/// (slice 3's deliberate ruling, since the rkey is derived from it), so a
/// `created_at`-relative comparison is measured against a value chosen by the
/// very party the expiry constrains. Only a wall-clock check *here*, before
/// signing, makes the grant genuinely time-bounded as
/// `docs/goal/principles.md` § *The user always controls their data* requires.
///
/// Fails **closed**: a cert that will not decode does not authorize.
///
/// ⚠ `now_micros` is **MICROSECONDS** — `Timestamp`'s own unit, so pass
/// `Timestamp::now().0`. A millisecond clock reads ~1000x too small, putting
/// every real expiry in its own "future" and silently disabling the check.
/// That is precisely how the provision-time gate sat dead until it was fixed.
pub fn delegation_is_live(cert_bytes: &[u8], now_micros: u64) -> bool {
    use fauna_core::data::DeviceAuthorization;
    use fauna_core::encoding::{EmbedAsBytes, canonical_decode, decode_signed_bytes};

    let Ok(wire) = canonical_decode::<EmbedAsBytes>(cert_bytes) else {
        return false;
    };
    let Ok((bytes, _env)) = wire.into_signed() else {
        return false;
    };
    let Ok(cert) = decode_signed_bytes::<DeviceAuthorization>(&bytes) else {
        return false;
    };
    match cert.expires_at {
        Some(exp) => exp.0 >= now_micros,
        None => true,
    }
}

/// Whether the stored delegation cert grants `required`.
///
/// The pre-dispatch twin of [`delegation_is_live`], for per-write capability
/// scoping: provisioning accepts any subset of the enumerated authoring set,
/// so a Post-only cert (a non-Fauna client minting its own) is a legitimate
/// stored state — and a profile write under it must be *refused with the
/// re-authorize remedy*, not surfaced as an opaque verify failure. The chain
/// verify inside each round-trip arm remains the guarantee (it re-checks the
/// capability on the real bytes); this is the good error message — the same
/// two-site pattern as the first-emit gate and the expiry gate.
///
/// No `Capability::All` arm on purpose: provisioning refuses `All` for this
/// surface (scope minimalism, D10 § Cert), so no stored cert carries it.
///
/// Fails **closed**: a cert that will not decode grants nothing.
pub fn delegation_grants(cert_bytes: &[u8], required: &fauna_core::data::Capability) -> bool {
    use fauna_core::data::DeviceAuthorization;
    use fauna_core::encoding::{EmbedAsBytes, canonical_decode, decode_signed_bytes};

    let Ok(wire) = canonical_decode::<EmbedAsBytes>(cert_bytes) else {
        return false;
    };
    let Ok((bytes, _env)) = wire.into_signed() else {
        return false;
    };
    let Ok(cert) = decode_signed_bytes::<DeviceAuthorization>(&bytes) else {
        return false;
    };
    cert.capabilities.contains(required)
}

fn pubkey_from_row(row: &AuthoringKeyRow) -> Result<[u8; 32]> {
    row.k_pub
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("stored authoring k_pub is not 32 bytes"))
}

/// Return the account's authoring sub-key public key, minting the sub-key on
/// the first call (provision-on-read). **First-write-wins:** a concurrent
/// mint that inserted the row first wins, and this call's freshly-generated
/// secret is discarded — so we always re-read to return the *stored* pubkey,
/// never the one we just generated (they would diverge under a race).
///
/// The mint seals under the deployment seed `nest_keypair` holds, read on the
/// connection the row is inserted on, with the read, the seal, the insert and
/// the re-read under one hold of the database guard — never a serving
/// generation's copy (`crate::nest_kek`'s module docs). `K` is per-account, so
/// it has no boot moment: a first fetch is its mint, and the generation that
/// answers it may be one a deployment-seed rotation has just retired but not yet
/// torn down. `K` sealed under that generation's seed would never open again,
/// and the satellite walk would refuse it on every later rotation.
pub async fn mint_or_fetch(db: &CacheDb, actor_id: &[u8; 32]) -> Result<[u8; 32]> {
    if let Some(row) = db.get_atproto_authoring_key(actor_id).await? {
        return pubkey_from_row(&row);
    }
    let kp = fauna_core::identity::ActorKeypair::generate();
    let conn = db.conn().await;
    let deployment_seed = crate::nest_kek::require_deployment_seed(&conn)?;
    let wrapped = encrypt_authoring_key_secret(&deployment_seed, kp.secret_bytes())?;
    crate::db::atproto_pds::insert_authoring_key_if_absent(
        &conn,
        actor_id,
        &kp.actor_id().0,
        &wrapped,
    )?;
    let row = crate::db::atproto_pds::authoring_key(&conn, actor_id)?
        .ok_or_else(|| anyhow::anyhow!("authoring key row absent immediately after insert"))?;
    pubkey_from_row(&row)
}

/// Load the account's authoring signer: the sub-key `K` unwrapped into a
/// usable keypair, plus the identity-signed delegation cert that authorizes it
/// (both halves live in the same row). The round-trip write arm signs with the
/// keypair and embeds the cert in the post wire's `signer_auth`, which is what
/// makes the four verify sites accept the result.
///
/// `Ok(None)` means the account cannot author: no sub-key has been minted; or
/// one has but no cert was ever provisioned (slice 2b leaves `cert` `None`
/// between mint and provision); or the provisioned cert has **since lapsed**
/// ([`delegation_is_live`]). All three are the same *user-visible* state — the
/// account is not currently authorized to author — so the caller answers with
/// the D6 **fauna-surface** refusal naming the Fauna-app authorization step
/// (D10 § Mint ceremony), never a "not yet implemented" one. For the lapsed
/// case that refusal is also the correct remedy: re-provisioning is exactly
/// what the user must do.
pub async fn load_signer(
    db: &CacheDb,
    nest_signing_key_bytes: &[u8; 32],
    actor_id: &[u8; 32],
) -> Result<Option<(fauna_core::identity::ActorKeypair, Vec<u8>)>> {
    let Some(row) = db.get_atproto_authoring_key(actor_id).await? else {
        return Ok(None);
    };
    let Some(cert) = row.cert.clone() else {
        return Ok(None);
    };
    // The grant must still be live. This is the chokepoint every holder of the
    // signing capability reaches, so the wall-clock gate belongs here as well
    // as at the row-only pre-check the write path runs first — a future caller
    // that forgets the pre-check still cannot sign with a lapsed delegation.
    if !delegation_is_live(&cert, fauna_core::data::Timestamp::now().0) {
        return Ok(None);
    }
    let secret = decrypt_authoring_key_secret(nest_signing_key_bytes, &row.k_secret_wrapped)?;
    let kp = fauna_core::identity::ActorKeypair::from_secret(secret);
    // The row's two halves must agree. They cannot diverge by any code path
    // that exists today (`mint_or_fetch` writes both from one keypair), but a
    // mismatch would produce posts signed by a key the cert does not name —
    // which every verify site rejects, far from the cause. Fail here, where
    // the diagnosis is one line.
    if kp.actor_id().0.as_slice() != row.k_pub.as_slice() {
        anyhow::bail!("unwrapped authoring sub-key does not match the stored k_pub");
    }
    Ok(Some((kp, cert)))
}

/// Whether `cap` is one of the enumerated authoring capabilities a delegation
/// for this surface may carry (`Post` for post create/delete, `UpdateProfile`
/// for the F2.3 profile path). Scope minimalism: everything else — `All`,
/// `ManageSubscribers`, `RenewBearer`, `Follow`, `React` — is refused.
fn is_authoring_capability(cap: &Capability) -> bool {
    matches!(cap, Capability::Post | Capability::UpdateProfile)
}

/// Verify a client-uploaded delegation cert against the account's minted
/// sub-key before it is stored (`atproto-pds-full.md` D10 § Mint ceremony).
/// Pure — no I/O — so the whole matrix is unit-testable. Returns the refusal
/// reason string on any failure; the handler maps it to a malformed-request
/// XRPC error.
///
/// The five checks (mirroring `provision_app_credential`'s validate-before-
/// write discipline and `verify_key_blob_signature`'s chain):
/// 1. the cert's own envelope verifies under `cert.actor_id` (identity-signed);
/// 2. `cert.actor_id` is the caller (you provision only your own delegation);
/// 3. `cert.device_key` is *this account's* minted sub-key `K_pub`;
/// 4. every granted capability is in the enumerated authoring set (non-empty);
/// 5. `expires_at`, when present, is not already past.
///
/// `now_micros` is **microseconds** since the epoch — `Timestamp`'s own unit
/// (`fauna_core::data::Timestamp`), so pass `Timestamp::now().0`, never a
/// millisecond clock. A millisecond `now` reads ~1000x too small, putting every
/// real expiry in its "future" and silently disabling check 5 altogether.
pub fn verify_delegation_cert(
    cert_bytes: &[u8],
    caller_actor_id: &[u8; 32],
    expected_k_pub: &[u8; 32],
    now_micros: u64,
) -> std::result::Result<(), String> {
    use fauna_core::data::DeviceAuthorization;
    use fauna_core::encoding::{
        EmbedAsBytes, canonical_decode, decode_signed_bytes, verify_envelope,
    };

    let wire: EmbedAsBytes =
        canonical_decode(cert_bytes).map_err(|e| format!("cert is not embed-as-bytes: {e}"))?;
    let (bytes, env) = wire
        .into_signed()
        .map_err(|e| format!("cert envelope split failed: {e}"))?;
    let cert: DeviceAuthorization =
        decode_signed_bytes(&bytes).map_err(|e| format!("cert decode failed: {e}"))?;

    // 1. identity-signed (DeviceAuthorization::signer_public_key == actor_id).
    verify_envelope(&cert, &bytes, &env).map_err(|_| "cert signature invalid".to_string())?;
    // 2. the caller is the grantor.
    if cert.actor_id.0 != *caller_actor_id {
        return Err("cert actor_id is not the caller".into());
    }
    // 3. the cert names this account's own minted sub-key.
    if cert.device_key != *expected_k_pub {
        return Err("cert device_key does not match the account's authoring sub-key".into());
    }
    // 4. scope minimalism — only the enumerated authoring capabilities.
    if cert.capabilities.is_empty() {
        return Err("cert grants no capability".into());
    }
    if !cert.capabilities.iter().all(is_authoring_capability) {
        return Err("cert grants a capability outside the authoring set".into());
    }
    // 5. not already expired.
    if let Some(exp) = cert.expires_at
        && exp.0 < now_micros
    {
        return Err("cert is already expired".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::data::{DeviceAuthorization, Timestamp};
    use fauna_core::encoding::{EmbedAsBytes, canonical_encode, sign_envelope};
    use fauna_core::identity::ActorKeypair;

    #[test]
    fn secret_wrap_roundtrips_and_is_domain_separated() {
        let nest_key = [42u8; 32];
        let secret = [7u8; 32];
        let ct = encrypt_authoring_key_secret(&nest_key, &secret).unwrap();
        assert_ne!(ct.as_slice(), secret.as_slice());
        assert_eq!(
            decrypt_authoring_key_secret(&nest_key, &ct).unwrap(),
            secret
        );
        // A different nest key cannot open it.
        assert!(decrypt_authoring_key_secret(&[99u8; 32], &ct).is_err());
    }

    /// Build the cert wire a client would upload: an identity-signed
    /// `DeviceAuthorization` over `device_key`, as canonical embed-as-bytes.
    fn cert_wire(
        identity: &ActorKeypair,
        device_key: [u8; 32],
        capabilities: Vec<Capability>,
        expires_at: Option<Timestamp>,
    ) -> Vec<u8> {
        let da = DeviceAuthorization {
            actor_id: identity.actor_id(),
            device_key,
            capabilities,
            created_at: Timestamp(1_000),
            expires_at,
        };
        let (bytes, env) = sign_envelope(identity, &da).unwrap();
        canonical_encode(&EmbedAsBytes::from_signed(bytes, env)).unwrap()
    }

    #[test]
    fn verify_delegation_cert_accepts_a_well_formed_cert() {
        let identity = ActorKeypair::from_secret([3u8; 32]);
        let k_pub = ActorKeypair::from_secret([4u8; 32]).actor_id().0;
        let cert = cert_wire(&identity, k_pub, vec![Capability::Post], None);
        assert!(verify_delegation_cert(&cert, &identity.actor_id().0, &k_pub, 5_000).is_ok());
        // UpdateProfile alongside Post is also in the enumerated set.
        let cert = cert_wire(
            &identity,
            k_pub,
            vec![Capability::Post, Capability::UpdateProfile],
            Some(Timestamp(10_000)),
        );
        assert!(verify_delegation_cert(&cert, &identity.actor_id().0, &k_pub, 5_000).is_ok());
    }

    #[test]
    fn verify_delegation_cert_rejects_the_whole_refusal_matrix() {
        let identity = ActorKeypair::from_secret([3u8; 32]);
        let other = ActorKeypair::from_secret([9u8; 32]);
        let k_pub = ActorKeypair::from_secret([4u8; 32]).actor_id().0;
        let wrong_k = ActorKeypair::from_secret([5u8; 32]).actor_id().0;

        // (2) grantor is not the caller.
        let cert = cert_wire(&identity, k_pub, vec![Capability::Post], None);
        assert!(verify_delegation_cert(&cert, &other.actor_id().0, &k_pub, 0).is_err());

        // (3) device_key is not the account's sub-key.
        assert!(verify_delegation_cert(&cert, &identity.actor_id().0, &wrong_k, 0).is_err());

        // (1) forged signature: a cert signed by someone other than its actor_id.
        let forged = {
            let da = DeviceAuthorization {
                actor_id: identity.actor_id(),
                device_key: k_pub,
                capabilities: vec![Capability::Post],
                created_at: Timestamp(1_000),
                expires_at: None,
            };
            let (bytes, env) = sign_envelope(&other, &da).unwrap();
            canonical_encode(&EmbedAsBytes::from_signed(bytes, env)).unwrap()
        };
        assert!(verify_delegation_cert(&forged, &identity.actor_id().0, &k_pub, 0).is_err());

        // (4) a capability outside the authoring set.
        let over = cert_wire(&identity, k_pub, vec![Capability::All], None);
        assert!(verify_delegation_cert(&over, &identity.actor_id().0, &k_pub, 0).is_err());
        let manage = cert_wire(&identity, k_pub, vec![Capability::ManageSubscribers], None);
        assert!(verify_delegation_cert(&manage, &identity.actor_id().0, &k_pub, 0).is_err());
        let empty = cert_wire(&identity, k_pub, vec![], None);
        assert!(verify_delegation_cert(&empty, &identity.actor_id().0, &k_pub, 0).is_err());

        // (5) already expired.
        let expired = cert_wire(
            &identity,
            k_pub,
            vec![Capability::Post],
            Some(Timestamp(100)),
        );
        assert!(verify_delegation_cert(&expired, &identity.actor_id().0, &k_pub, 200).is_err());
        // …but exactly-at-now is still valid (expiry is a strict past check).
        let at_now = cert_wire(
            &identity,
            k_pub,
            vec![Capability::Post],
            Some(Timestamp(200)),
        );
        assert!(verify_delegation_cert(&at_now, &identity.actor_id().0, &k_pub, 200).is_ok());
    }
}
