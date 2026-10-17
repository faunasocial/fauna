//! The kind-manifest seam's crossing of web's account port — both halves,
//! beside the [`KindManifestStore`] trait they cross
//! (`docs/goal/architecture/account-client-lifecycle.md` § The client-side
//! lifecycle → *The account port*, decisions (c) and (h)).
//!
//! The account's `fauna.state.kind-manifest` rows live in the core chunk's
//! runtime; the consent machines that admit a third-party app's kinds — the
//! AT Protocol page's and the Connected apps tray's — live in their own
//! chunks (`third-party-kinds.md` § The record doors → *Who mints the
//! consent-time grant*). So those chunks wire [`PortKindManifestStore`] — the
//! **forwarder**, which encodes a method's arguments, calls its door over the
//! port and decodes the answer, and does nothing else — and the core chunk's
//! `accountPortCall` hands each door to [`serve`] over the seam's one
//! implementation on the account-store handle.
//!
//! **Both doors cross** (decision (h)): the overlay read and the publish.
//! What crosses is data, never a verdict: the overlay as its kind names (a
//! manifest admits only `latest-wins`, so the policy is a consequence of being
//! admitted, not a field), and the publish as the signed manifest's compact
//! JWS plus the domain it claims — [`serve`] **re-verifies** the JWS under that
//! domain before the seam sees a [`VerifiedManifest`], so a caller in another
//! chunk cannot hand the runtime a manifest nothing checked. The development
//! carve-out's unsigned manifest (empty JWS) is never publishable and does not
//! cross.
//!
//! **A port fault is never a success (decision (f)).** No runtime, another
//! account's runtime, an unknown door, undecodable bytes and broken glue all
//! answer the method's own refusal — an empty overlay is never invented, and a
//! manifest that cannot reach the runtime is not recorded.

use crate::store_seam::{KindManifestStore, StoreError};
use fauna_account_port::{PortFault, PortTransport, answer, forward};
use fauna_protocol::kind_manifest::{VerifiedManifest, verify_manifest};
use fauna_protocol::merge_policy::{AdmittedKinds, MergePolicy};
use serde::{Deserialize, Serialize};

/// The seam's doors — both cross. Prefixed with the seam, since the core
/// chunk's dispatch holds every seam's doors in one namespace.
pub mod doors {
    pub const ADMITTED_KINDS: &str = "kind_manifest.admitted_kinds";
    pub const PUBLISH: &str = "kind_manifest.publish";

    /// Every door of the seam.
    pub const ALL: [&str; 2] = [ADMITTED_KINDS, PUBLISH];
}

/// The publish door's arguments: the manifest as the signed document plus the
/// domain it was verified for — all [`serve`] needs to verify it afresh.
#[derive(Serialize, Deserialize)]
struct PublishArgs {
    client_id: String,
    jws: String,
    publisher_domain: String,
    admitted_at_ms: i64,
}

/// What the doors answer: the seam method's own result, its failure as the
/// refusal's message — the [`mail_port`](crate::mail_port) shape. The message
/// crosses bare (not `StoreError`'s `Display`, which prefixes it).
type KindsReply = Result<Vec<String>, String>;
type PublishReply = Result<(), String>;

/// A refusal's own message, without `StoreError`'s `Display` prefix.
fn message(e: StoreError) -> String {
    match e {
        StoreError::Load(m) | StoreError::Save(m) => m,
    }
}

/// The forwarder: [`KindManifestStore`] over an account-port transport.
pub struct PortKindManifestStore<T> {
    transport: T,
}

impl<T: PortTransport> PortKindManifestStore<T> {
    pub fn new(transport: T) -> Self {
        Self { transport }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl<T: PortTransport> KindManifestStore for PortKindManifestStore<T> {
    async fn admitted_kinds(&self) -> Result<AdmittedKinds, StoreError> {
        let answered: KindsReply = forward(&self.transport, doors::ADMITTED_KINDS, &())
            .await
            .map_err(|f| StoreError::Load(f.to_string()))?;
        let mut overlay = AdmittedKinds::new();
        for name in answered.map_err(StoreError::Load)? {
            let kind = name
                .parse()
                .map_err(|e| StoreError::Load(format!("admitted kind {name:?}: {e}")))?;
            overlay
                .admit(kind, MergePolicy::LatestWins)
                .map_err(|e| StoreError::Load(e.to_string()))?;
        }
        Ok(overlay)
    }

    async fn publish(
        &self,
        client_id: &str,
        manifest: &VerifiedManifest,
        admitted_at_ms: i64,
    ) -> Result<(), StoreError> {
        if manifest.jws.is_empty() {
            return Err(StoreError::Save(
                "an unsigned (development) manifest is never published".into(),
            ));
        }
        let args = PublishArgs {
            client_id: client_id.to_string(),
            jws: manifest.jws.clone(),
            publisher_domain: manifest.publisher_domain.clone(),
            admitted_at_ms,
        };
        let answered: PublishReply = forward(&self.transport, doors::PUBLISH, &args)
            .await
            .map_err(|f| StoreError::Save(f.to_string()))?;
        answered.map_err(StoreError::Save)
    }
}

/// The forwarder over the SPA's `SharedAccountPort` object — one constructor,
/// so every consumer chunk wraps the port identically. A value that is not a
/// port is refused by name, where it was wired.
#[cfg(target_arch = "wasm32")]
pub fn from_js_port(
    port: fauna_account_port::JsAccountPort,
) -> Result<std::sync::Arc<dyn KindManifestStore>, PortFault> {
    let transport = fauna_account_port::JsAccountTransport::new(port.into())?;
    Ok(std::sync::Arc::new(PortKindManifestStore::new(transport)))
}

/// Answer one door of the kind-manifest seam from `seam` — the core chunk's
/// half. `None` when `door` is not this seam's, so the dispatch can ask the
/// next seam and refuse a door nobody knows by name.
pub async fn serve(
    seam: &dyn KindManifestStore,
    door: &str,
    payload: &[u8],
) -> Option<Result<Vec<u8>, PortFault>> {
    Some(match door {
        doors::ADMITTED_KINDS => {
            answer(payload, |(): ()| async move {
                let reply: KindsReply = seam
                    .admitted_kinds()
                    .await
                    .map(|overlay| overlay.kinds().map(ToString::to_string).collect())
                    .map_err(message);
                reply
            })
            .await
        }
        doors::PUBLISH => {
            answer(payload, |args: PublishArgs| async move {
                let reply: PublishReply = match verify_manifest(&args.jws, &args.publisher_domain) {
                    Ok(manifest) => seam
                        .publish(&args.client_id, &manifest, args.admitted_at_ms)
                        .await
                        .map_err(message),
                    Err(e) => Err(format!("manifest refused on re-verify: {e}")),
                };
                reply
            })
            .await
        }
        _ => return None,
    })
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use std::sync::Arc;

    use fauna_account_port::loopback::Loopback;
    use fauna_protocol::kind_manifest::ManifestKind;

    use super::*;
    use crate::test_helpers::FakeKindManifestStore;

    fn over(seam: Arc<dyn KindManifestStore>) -> PortKindManifestStore<Loopback> {
        PortKindManifestStore::new(Loopback::new(move |door, payload| {
            let seam = Arc::clone(&seam);
            async move {
                serve(&*seam, door, &payload)
                    .await
                    .unwrap_or_else(|| Err(PortFault::UnknownDoor(door.to_string())))
            }
        }))
    }

    /// A manifest as the fake publishes it — hand-built, the JWS is not read.
    fn admitting(domain: &str, kind: &str) -> VerifiedManifest {
        VerifiedManifest {
            jws: "not-a-real-jws".into(),
            publisher_domain: domain.into(),
            publisher_key: None,
            kinds: vec![ManifestKind {
                kind: kind.parse().expect("ext kind"),
                merge: MergePolicy::LatestWins,
            }],
            bridge: None,
            service_auth: Vec::new(),
            activitypub_peers: Vec::new(),
            events_uri: None,
            ingress: Vec::new(),
            payload: Default::default(),
        }
    }

    /// The overlay crosses as the kinds the seam admits, rebuilt `latest-wins`.
    #[tokio::test]
    async fn the_overlay_crosses_as_its_kind_names() {
        let seam = FakeKindManifestStore::empty();
        seam.publish(
            "https://example.com/client.json",
            &admitting("example.com", "ext.example.com.notes"),
            1,
        )
        .await
        .expect("the fake publishes");
        let read = over(Arc::new(seam))
            .admitted_kinds()
            .await
            .expect("the overlay crosses");
        let names: Vec<String> = read.kinds().map(ToString::to_string).collect();
        assert_eq!(names, ["ext.example.com.notes"]);
        assert_eq!(
            read.merge_policy("ext.example.com.notes"),
            Some(MergePolicy::LatestWins)
        );
    }

    /// A publish is re-verified on the far side: a JWS that does not verify
    /// under its claimed domain reaches no seam, and the refusal crosses back
    /// as a `Save`; an unsigned manifest never leaves the forwarder.
    #[tokio::test]
    async fn a_publish_is_reverified_and_an_unsigned_one_never_crosses() {
        let seam = FakeKindManifestStore::empty();
        let port = over(Arc::new(seam.clone()));
        let err = port
            .publish(
                "https://example.com/client.json",
                &admitting("example.com", "ext.example.com.notes"),
                1,
            )
            .await
            .expect_err("a JWS that does not verify is refused");
        assert!(
            matches!(&err, StoreError::Save(m) if m.contains("re-verify")),
            "{err:?}"
        );
        assert!(seam.row("https://example.com/client.json").is_none());

        let mut unsigned = admitting("example.com", "ext.example.com.notes");
        unsigned.jws.clear();
        let err = port
            .publish("https://example.com/client.json", &unsigned, 1)
            .await
            .expect_err("an unsigned manifest is refused at the forwarder");
        assert!(matches!(err, StoreError::Save(_)), "{err:?}");
    }

    /// A door nobody serves is a refusal, never an empty overlay.
    #[tokio::test]
    async fn an_unserved_door_is_a_refusal_not_an_empty_overlay() {
        let port = PortKindManifestStore::new(Loopback::new(|door, _payload| async move {
            Err(PortFault::UnknownDoor(door.to_string()))
        }));
        assert!(matches!(
            port.admitted_kinds().await,
            Err(StoreError::Load(_))
        ));
    }
}
