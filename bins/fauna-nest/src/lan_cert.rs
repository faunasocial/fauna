//! Installing a **client-issued** TLS certificate on this nest
//! (`tls-certificates.md` § B tier 2, "the issued cert ships to the nest").
//!
//! The admin's client holds the DNS-provider credential and drives a DNS-01 ACME
//! order (`dns-management.md` § Where the credential lives — the nest is out of
//! the DNS-write path end-to-end). It then HPKE-seals the issued
//! `(chain, key)` to **one specific nest's** identity-derived x25519 key
//! (`fauna_mls::wrapped_blob::seal_lan_tls_cert_entry`) and signs the ciphertext
//! with its own actor key.
//!
//! That sealed entry reaches a nest by exactly two routes, and this module is
//! the single installer both of them call:
//!
//! 1. **Direct** — the client publishes to the nest the cert is *for*
//!    (`fauna.tls.publish_cert`, the standalone-deployment case: one nest, public
//!    or private, with no pair). `tls_handlers` calls [`install_client_issued_cert`]
//!    right after storing the entry.
//! 2. **Relayed** — the client publishes to a reachable *relay* nest, which
//!    stores the entry opaquely (it cannot open a blob sealed to its peer) and a
//!    paired private nest pulls it over namespace-sync. `nest_sync_worker` calls
//!    [`install_client_issued_cert`] on that pull.
//!
//! **The seal target authorizes the nest.** A nest installs a blob iff it can
//! *open* it — which requires the blob to have been sealed to that nest's own
//! identity key — and iff the enclosing signature verifies against the actor who
//! delivered it. So the relay in route 2 physically cannot install its peer's
//! cert, and no route needs a NAT-axis gate to stay safe.
//!
//! **The signer must be an admin of this nest — checked here, for both
//! routes**. The seal target is the nest's published
//! identity key, so anybody can seal to it; and route 2 delivers whatever any
//! *paired* actor signed, where pairing is every user's own act. The listener
//! cert is an admin's setting, so the installer asks this nest's own role
//! state. (The direct route's handler is Admin-class as well; this check is
//! what makes the single installer safe whoever calls it.) It fails safe: a
//! nest with no admin installs nothing.

use std::sync::Arc;

use crate::routes::AppState;

/// Verify, unseal, and install a client-issued LAN-TLS cert entry on **this**
/// nest: check the delivering actor's Ed25519 signature, open the HPKE seal with
/// this nest's identity-derived x25519 secret, and hand the PEM to
/// `store_acme_material` (which writes `{acme_dir}/fullchain.pem` + `privkey.pem`
/// and pre-seeds the sealed bridge fan-out — the same path as an ACME finish; the
/// `cert_watcher_task` then hot-reloads the listener within ~2 s).
///
/// Returns `true` iff the cert was installed. A blob sealed to a *different*
/// nest returns `false` **without** logging an error — that is the relay's normal
/// path in route 2 above, not a fault. Every other failure logs and returns
/// `false`; the nest keeps serving its current cert and is never left worse off
/// (no delete-first, no partial state). Idempotent: re-installing the same cert
/// is a harmless atomic rewrite.
pub(crate) async fn install_client_issued_cert(
    state: &Arc<AppState>,
    actor_id: &[u8],
    ciphertext: &[u8],
    actor_sig: &[u8],
) -> bool {
    let Some(actor_vk) = <[u8; 32]>::try_from(actor_id)
        .ok()
        .and_then(|b| ed25519_dalek::VerifyingKey::from_bytes(&b).ok())
    else {
        tracing::warn!("client-issued TLS cert: actor id is not a valid ed25519 key, skipping");
        return false;
    };
    // Before the seal is even tried: a non-admin's entry is refused whatever
    // it holds. A failed read refuses too — never an install on an unknown.
    match state.db.is_admin(actor_id).await {
        Ok(true) => {}
        Ok(false) => {
            tracing::warn!(
                "client-issued TLS cert: signer {} is not an admin of this nest, skipping",
                hex::encode(actor_id)
            );
            return false;
        }
        Err(e) => {
            tracing::warn!("client-issued TLS cert: admin check failed: {e:#} — skipping");
            return false;
        }
    }
    // This nest's identity Ed25519 → x25519 secret (the seal target the producer
    // derived from the published, pinned identity pubkey).
    let seed = state.nest_identity.signing_key.to_bytes();
    let nest_x25519_sec = fauna_core::identity::ActorKeypair::from_secret(seed)
        .to_x25519_secret()
        .to_bytes();

    let opened = match fauna_mls::wrapped_blob::open_lan_tls_cert_entry(
        ciphertext,
        actor_sig,
        &actor_vk,
        &nest_x25519_sec,
    ) {
        Ok(opened) => opened,
        Err(e) => {
            // Sealed to another nest (the relay case) or genuinely malformed. We
            // cannot tell the two apart without the plaintext, so this is `debug`,
            // not `warn`: on a relay it happens on every single pull cycle.
            tracing::debug!("client-issued TLS cert not openable by this nest: {e}");
            return false;
        }
    };

    let material = crate::storage::AcmeMaterial {
        domain: &opened.domain,
        cert_chain_pem: &opened.bundle.cert_chain,
        priv_key_pem: &opened.bundle.priv_key,
    };
    match state.storage().store_acme_material(&material).await {
        Ok(()) => {
            tracing::info!(
                "installed client-issued TLS cert for {} into acme_dir (listener hot-reloads; \
                 bridges re-fetch their sealed blob)",
                opened.domain
            );
            true
        }
        Err(e) => {
            tracing::warn!("client-issued TLS cert: store_acme_material failed: {e:?}");
            false
        }
    }
}
