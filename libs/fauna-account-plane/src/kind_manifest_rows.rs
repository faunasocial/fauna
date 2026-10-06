//! The admitted-kinds overlay's carriage — the typed door for
//! `fauna.state.kind-manifest` (`third-party-kinds.md` § The kinds vocabulary
//! → *The registry overlay* owns the concept; `fauna_protocol::kind_manifest`
//! owns the value and the one verifier).
//!
//! **One row per admitted third-party principal, keyed by its metadata
//! document's `client_id`**, whole-record latest-wins: a consent writes the
//! document's current manifest, and a re-consent re-writes it. The value is
//! the compact JWS verbatim, so the overlay is a READ fold that **re-verifies
//! every row** against the host its key names — never a policy a writer (or
//! the nest relaying the row) asserted. A row that does not decode, whose key
//! names no `https` host, or whose manifest no longer verifies admits nothing
//! and is counted, never fatal: the overlay is the union of what verifies.
//!
//! The kind is `GenerationTip`-sealed, so a put while no tip resolves is
//! refused at the fleet plane's REAL writer door and surfaces to the caller.
//! Each put is the **local write only** ([`put_lww_row_local`]); the account
//! runtime's publish step ships it.

use anyhow::{Context, Result};
use fauna_account_store::backend::StoreBackend;
use fauna_account_store::store::AccountStore;
use fauna_account_store::types::StateEntry;
use fauna_protocol::RpcRequester;
use fauna_protocol::kind_manifest::{
    KindManifestRecord, VerifiedManifest, client_id_host, verify_manifest,
};
use fauna_protocol::merge_policy::{AdmittedKinds, KIND_KIND_MANIFEST};

use crate::account_state_plane::{AccountStatePlane, put_lww_row_local};

/// The overlay a set of `fauna.state.kind-manifest` rows admits, and the
/// rows that admitted nothing (`(client_id, why)`) — for logs and audit.
#[derive(Debug, Default)]
pub struct AdmittedOverlay {
    /// Every kind a verifying row declared.
    pub kinds: AdmittedKinds,
    /// Rows refused on read.
    pub refused: Vec<(String, String)>,
}

/// Fold the live `fauna.state.kind-manifest` rows among `entries` into the
/// overlay, re-verifying each row's JWS against the host of its `client_id`.
pub fn admitted_kinds_of(entries: &[StateEntry]) -> AdmittedOverlay {
    let mut out = AdmittedOverlay::default();
    for entry in entries
        .iter()
        .filter(|e| e.kind == KIND_KIND_MANIFEST && !e.tombstone)
    {
        let admitted = (|| -> Result<()> {
            let record: KindManifestRecord = fauna_core::encoding::canonical_decode(&entry.value)
                .context("the row does not decode")?;
            let host = client_id_host(&entry.key)
                .with_context(|| format!("{:?} names no https host", entry.key))?;
            let manifest = verify_manifest(&record.jws, &host)?;
            manifest.admit_into(&mut out.kinds)?;
            Ok(())
        })();
        if let Err(why) = admitted {
            tracing::warn!(
                client_id = %entry.key,
                "kind manifest row admits nothing: {why:#}"
            );
            out.refused.push((entry.key.clone(), format!("{why:#}")));
        }
    }
    out
}

/// The overlay this store's merged rows admit.
pub async fn read_admitted_kinds<B: StoreBackend>(
    store: &AccountStore<B>,
) -> Result<AdmittedOverlay> {
    Ok(admitted_kinds_of(
        &store.states_of_kind(KIND_KIND_MANIFEST).await?,
    ))
}

/// Publish the consent's verified manifest as the account's row for
/// `client_id`, stamped `(now, device_id)` — step (1) of the consent-time
/// mint (`third-party-kinds.md` § The record doors). `manifest` must have been
/// verified against `client_id`'s own host: the row is re-verified on every
/// read, so a mismatched pair would only publish a row that admits nothing,
/// and is refused here instead.
pub async fn write_kind_manifest<B, R>(
    fleet: &AccountStatePlane<'_, B, R>,
    client_id: &str,
    manifest: &VerifiedManifest,
    admitted_at_ms: i64,
    device_id: [u8; 32],
) -> Result<u64>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
{
    let host =
        client_id_host(client_id).with_context(|| format!("{client_id:?} names no https host"))?;
    anyhow::ensure!(
        host == manifest.publisher_domain,
        "the manifest was verified for {:?}, not {client_id:?}'s host {host:?}",
        manifest.publisher_domain
    );
    let value = fauna_core::encoding::canonical_encode(&KindManifestRecord {
        jws: manifest.jws.clone(),
        admitted_at_ms,
        ..Default::default()
    })
    .context("encode kind manifest row")?;
    put_lww_row_local(
        fleet,
        KIND_KIND_MANIFEST,
        client_id,
        value.to_vec(),
        device_id,
    )
    .await
    .context("kind manifest row: plane put")
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_protocol::kind_manifest::sign_manifest;
    use fauna_protocol::merge_policy::MergePolicy;

    fn manifest(domain: &str, kinds: &[&str]) -> String {
        let key = ed25519_dalek::SigningKey::from_bytes(&[0x42; 32]);
        let did = fauna_protocol::kind_manifest::ed25519_did_key(&key.verifying_key().to_bytes());
        let payload = serde_json::json!({
            "version": 1,
            "publisher": { "domain": domain, "key": did },
            "kinds": kinds.iter().map(|k| serde_json::json!({
                "kind": k, "class": "state", "merge": "latest-wins", "floor": "none"
            })).collect::<Vec<_>>(),
        });
        sign_manifest(&key, &payload, None)
    }

    fn row(client_id: &str, value: Vec<u8>) -> StateEntry {
        StateEntry {
            kind: KIND_KIND_MANIFEST.into(),
            key: client_id.into(),
            scope: fauna_protocol::account_state::ACCOUNT_STATE_FLEET_SCOPE.into(),
            value,
            merge_meta: None,
            entry_version: 1,
            tombstone: false,
        }
    }

    fn encoded(jws: String) -> Vec<u8> {
        fauna_core::encoding::canonical_encode(&KindManifestRecord {
            jws,
            admitted_at_ms: 1,
            ..Default::default()
        })
        .unwrap()
        .to_vec()
    }

    /// Every row re-verifies against its OWN key's host: a manifest moved
    /// under another publisher's `client_id`, a non-https key and a value
    /// that does not decode each admit nothing, and the good row still does.
    #[test]
    fn the_overlay_is_the_union_of_rows_that_verify_against_their_own_host() {
        let good = row(
            "https://example.com/client.json",
            encoded(manifest("example.com", &["ext.example.com.notes"])),
        );
        let moved = row(
            "https://other.org/client.json",
            encoded(manifest("example.com", &["ext.example.com.todo"])),
        );
        let plain_http = row(
            "http://example.net/client.json",
            encoded(manifest("example.net", &["ext.example.net.x"])),
        );
        let garbage = row("https://example.io/c.json", b"not cbor".to_vec());
        let overlay = admitted_kinds_of(&[good, moved, plain_http, garbage]);
        assert_eq!(
            overlay.kinds.merge_policy("ext.example.com.notes"),
            Some(MergePolicy::LatestWins)
        );
        for unadmitted in ["ext.example.com.todo", "ext.example.net.x"] {
            assert_eq!(overlay.kinds.merge_policy(unadmitted), None, "{unadmitted}");
        }
        assert_eq!(overlay.refused.len(), 3, "{:?}", overlay.refused);
    }
}
