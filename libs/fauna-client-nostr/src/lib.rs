//! Typed-call wrappers for the nest's Nostr WS-RPC kinds. Each is a thin
//! client holding the WS-RPC transport, one async method per kind, no state
//! machine (the `fauna-client-email` pattern); the kind-composition logic is
//! written once here and shared across native + wasm (priority #2).
//!
//! Direct messages are not here: a Nostr DM is a bridged room, read and sent
//! through `fauna.bridges.conversation.*` (`docs/goal/ui/nostr.md`
//! § Implementation status today → DMs).
//!
//! `NostrBunkerClient` — the typed client for the
//! `fauna.nostr.bunker.{create_invite,list,revoke,set_label}` control plane
//! (the NIP-46 bunker roster, `docs/goal/ui/nostr.md` § The nest as the user's
//! NIP-46 signer), consumed by the Nostr page's *Connected apps* section on
//! all 7 apps (`nostr-bunker-*` IDs).
//!
//! And `NostrContentClient` — the typed client for the protocol-native
//! content kinds `nostr.{zaps.total,badges.list,events.publish_signed}` (the
//! prefix-less family, the `bluesky.feed.thread` precedent), the WS-RPC
//! successors to the deleted `/api/v1/nostr/{zaps,badges,publish-signed}`
//! HTTP routes (the native-content HTTP→WS-RPC rip, 2026-07-22).

use fauna_protocol::RpcRequester;
use fauna_protocol::nostr::{
    BunkerAppEntry, CreateBunkerInviteReply, CreateBunkerInviteRequest, ListBunkerAppsReply,
    ListBunkerAppsRequest, ListNostrBadgesReply, ListNostrBadgesRequest, NostrBadgeItem,
    PublishSignedNostrEventReply, PublishSignedNostrEventRequest, RevokeBunkerAppReply,
    RevokeBunkerAppRequest, SetBunkerAppLabelReply, SetBunkerAppLabelRequest,
};
// The `zaps` registry feature's wire types (`dynamic-features.md` § Charter
// members). Separate `use` so the whole import disappears with the feature
// rather than needing a per-name cfg.
#[cfg(feature = "zaps")]
use fauna_protocol::nostr::{
    AddZapSignerReply, AddZapSignerRequest, ListZapSignersReply, ListZapSignersRequest,
    NostrZapTotalReply, NostrZapTotalRequest, RemoveZapSignerReply, RemoveZapSignerRequest,
    ZapSignerEntry,
};

pub use fauna_protocol::nostr;

/// Typed `fauna.nostr.bunker.*` call surface — the NIP-46 bunker control plane
/// (the *Connected apps* roster, `docs/goal/ui/nostr.md` § The nest as the
/// user's NIP-46 signer). A thin wrapper, generic
/// over the WS-RPC transport (`R: RpcRequester`): native call sites pass
/// `Arc<NestClient>`, the wasm SPA passes its `WsRpcClient`. The
/// kind-composition logic is written once here and shared across native + wasm
/// (priority #2). Errors propagate as the transport's `R::Error`.
///
/// All four kinds are **User-class, caller-scoped**: the nest handler keys
/// every query on the authenticated connection's actor, never a request field —
/// so mint/list/revoke/label all operate on the caller's own roster.
pub struct NostrBunkerClient<R: RpcRequester> {
    nest: R,
}

impl<R: RpcRequester> NostrBunkerClient<R> {
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    /// `fauna.nostr.bunker.create_invite` — mint a pending connection with a
    /// one-time secret. Returns the roster row id, the full
    /// `bunker://<signer-pubkey>?relay=…&secret=…` connect string (composed
    /// nest-side, rendered as `nostr-bunker-connect-string`), the dedicated
    /// signer pubkey, and the invite TTL. The secret rests only as a hash on
    /// the nest — this reply is the single one-time reveal (mail-credentials
    /// precedent). Requires the caller's Nostr account to be in a custodial
    /// mode (`generated`/`imported`); the nest rejects otherwise.
    pub async fn create_invite(&self) -> Result<CreateBunkerInviteReply, R::Error> {
        self.nest
            .request(
                "fauna.nostr.bunker.create_invite",
                CreateBunkerInviteRequest {
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.nostr.bunker.list` — the caller's connection roster (pending +
    /// active rows; revoked tombstones are not served). Replay-safe pure read;
    /// the subject is the authenticated connection's actor (empty request).
    pub async fn list(&self) -> Result<Vec<BunkerAppEntry>, R::Error> {
        let reply: ListBunkerAppsReply = self
            .nest
            .request(
                "fauna.nostr.bunker.list",
                ListBunkerAppsRequest {
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(reply.apps)
    }

    /// `fauna.nostr.bunker.revoke` — immediate disconnect of one connection
    /// (authorization is evaluated per request; no cached authority survives).
    /// `false` when no live caller-owned row matched (already revoked, or not
    /// the caller's).
    pub async fn revoke(&self, connection_id: i64) -> Result<bool, R::Error> {
        let reply: RevokeBunkerAppReply = self
            .nest
            .request(
                "fauna.nostr.bunker.revoke",
                RevokeBunkerAppRequest {
                    connection_id,
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(reply.revoked)
    }

    /// `fauna.nostr.bunker.set_label` — name a connection row. `false` when no
    /// live caller-owned row matched.
    pub async fn set_label(&self, connection_id: i64, label: String) -> Result<bool, R::Error> {
        let reply: SetBunkerAppLabelReply = self
            .nest
            .request(
                "fauna.nostr.bunker.set_label",
                SetBunkerAppLabelRequest {
                    connection_id,
                    label,
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(reply.updated)
    }
}

/// Typed `fauna.nostr.zap_signers.*` call surface — the NIP-57 trust root
/// (`docs/goal/behavior/monetization.md` § Zap receipts — the trust model).
/// Same thin-wrapper shape as [`NostrBunkerClient`], generic over the WS-RPC
/// transport (`R: RpcRequester`); written once here and shared across native
/// + wasm so the 7 apps consume rather than re-derive (priority #2).
///
/// A kind-9735 zap receipt is signed by the *recipient's* LNURL/wallet
/// server and is plain signed JSON anyone may mint, so its own signature
/// proves nothing about payment. This roster is what makes one believable:
/// the payee designates which signer pubkey(s) may speak for their money.
///
/// **An empty roster is the meaningful default, not an unconfigured state**:
/// a payee who has designated nobody believes nobody, so every receipt stays
/// inert. A UI rendering this list should say so rather than presenting
/// emptiness as an error.
///
/// All three kinds are **User-class, caller-scoped**: the nest handler keys
/// every query on the authenticated connection's actor, never a request
/// field — so list/add/remove all operate on the caller's own trust root.
///
/// Gated on the `zaps` registry feature (`dynamic-features.md` § Charter
/// members — a subset member of `payments`), so a Damus-flavor build carries
/// neither this face nor the kind strings it composes.
#[cfg(feature = "zaps")]
pub struct NostrZapSignerClient<R: RpcRequester> {
    nest: R,
}

#[cfg(feature = "zaps")]
impl<R: RpcRequester> NostrZapSignerClient<R> {
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    /// `fauna.nostr.zap_signers.list` — the caller's designated signers,
    /// newest first. Replay-safe pure read; the subject is the authenticated
    /// connection's actor (empty request). An empty vec means this payee
    /// believes no zap receipt at all.
    pub async fn list(&self) -> Result<Vec<ZapSignerEntry>, R::Error> {
        let reply: ListZapSignersReply = self
            .nest
            .request(
                "fauna.nostr.zap_signers.list",
                ListZapSignersRequest {
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(reply.signers)
    }

    /// `fauna.nostr.zap_signers.add` — designate a signer (64-hex pubkey,
    /// normalized to lowercase nest-side) with an optional provider label.
    /// Idempotent: re-adding a designated signer refreshes its label. The
    /// nest refuses a malformed pubkey rather than storing a designation
    /// that could never match a real receipt. Returns the stored row, so a
    /// client renders the normalized key rather than whatever case it sent.
    pub async fn add(
        &self,
        signer_pubkey: String,
        label: String,
    ) -> Result<ZapSignerEntry, R::Error> {
        let reply: AddZapSignerReply = self
            .nest
            .request(
                "fauna.nostr.zap_signers.add",
                AddZapSignerRequest {
                    signer_pubkey,
                    label,
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(reply.signer)
    }

    /// `fauna.nostr.zap_signers.remove` — stop trusting a signer, keyed by
    /// the pubkey itself (no roster round-trip needed first). `false` when
    /// the caller had not designated it. Removal takes effect at the next
    /// receipt: the gate is applied at ingest, so already-stored rows are
    /// unaffected — undesignating stops future belief, it does not retract
    /// past acceptances.
    pub async fn remove(&self, signer_pubkey: String) -> Result<bool, R::Error> {
        let reply: RemoveZapSignerReply = self
            .nest
            .request(
                "fauna.nostr.zap_signers.remove",
                RemoveZapSignerRequest {
                    signer_pubkey,
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(reply.removed)
    }
}

/// A `nostr-zap-signer-item` row's display text: the user's label, falling
/// back to "Unnamed signer" when blank, followed by the signer pubkey's
/// short id ([`fauna_core::format::short_id`], the one 12-char shape every
/// app's long-hex display uses — `value-formatting.md` § Short id). tui and
/// linux each independently assembled this exact two-branch format — both
/// call sites' own doc comments named the other as the thing they mirrored,
/// but the shared home was never built.
///
/// ⚠ **The STORED pubkey, never the typed input.** The nest validates 64-hex
/// and normalizes to lowercase on write, and only that stored form ever
/// matches an incoming receipt — so a client echoing a user's uppercase
/// paste would paint a correct-looking roster that believes nothing. This
/// renders `entry.signer_pubkey` straight off the reply, which is why both
/// callers re-list after `add` instead of pushing the input locally.
#[cfg(feature = "zaps")]
pub fn zap_signer_row_text(entry: &ZapSignerEntry) -> String {
    let label = if entry.label.trim().is_empty() {
        fauna_i18n::strings::nostr::zap_signers::UNNAMED.to_string()
    } else {
        entry.label.clone()
    };
    format!(
        "{label} — {}",
        fauna_core::format::short_id(&entry.signer_pubkey)
    )
}

/// Typed call surface for the protocol-native Nostr content kinds
/// (`nostr.{zaps.total,badges.list,events.publish_signed}` — the prefix-less
/// family, `fauna_protocol::nostr`'s module doc owns the naming split). Same
/// thin-wrapper shape as [`NostrBunkerClient`], generic over the WS-RPC transport
/// (`R: RpcRequester`); the kind-composition logic is written once here and
/// shared across native + wasm (priority #2).
///
/// `zap_total` / `badges` are display reads over sync-worker-ingested
/// NIP-57/NIP-58 rows; `publish_signed` is the NIP-07 leg — the client's
/// browser extension signed the event and the nest verifies + relay-enqueues
/// it (only meaningful where a NIP-07 extension exists, i.e. web).
pub struct NostrContentClient<R: RpcRequester> {
    nest: R,
}

impl<R: RpcRequester> NostrContentClient<R> {
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    /// `nostr.zaps.total` — aggregate zap receipts (NIP-57) for one event.
    /// Unknown event ids are zeros, not an error. Replay-safe pure read.
    ///
    /// `zaps`-gated while its siblings on this client are not: the carve is
    /// per-surface, because `badges`/`publish_signed` are not registry members.
    #[cfg(feature = "zaps")]
    pub async fn zap_total(&self, event_id: String) -> Result<NostrZapTotalReply, R::Error> {
        self.nest
            .request(
                "nostr.zaps.total",
                NostrZapTotalRequest {
                    event_id,
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `nostr.badges.list` — badge awards (NIP-58) for one pubkey, newest
    /// first. Replay-safe pure read.
    pub async fn badges(&self, pubkey: String) -> Result<Vec<NostrBadgeItem>, R::Error> {
        let reply: ListNostrBadgesReply = self
            .nest
            .request(
                "nostr.badges.list",
                ListNostrBadgesRequest {
                    pubkey,
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(reply.badges)
    }

    /// `nostr.events.publish_signed` — relay-enqueue an event the client
    /// signed (NIP-07). `event_json` is the signed event's NIP-01 wire JSON,
    /// exactly as the extension returned it; the nest verifies the signature
    /// and that the pubkey is the caller's linked account.
    pub async fn publish_signed(&self, event_json: String) -> Result<(), R::Error> {
        let _reply: PublishSignedNostrEventReply = self
            .nest
            .request(
                "nostr.events.publish_signed",
                PublishSignedNostrEventRequest {
                    event_json,
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::{RecordingRequester, block_on};

    /// This crate's reply table for the shared [`RecordingRequester`]:
    /// one arm per kind, each the minimal valid shape its `Reply` decodes.
    fn reply(kind: &'static str) -> Vec<u8> {
        // Answer with a reply the requested `Reply` type decodes — one arm
        // per kind, each the minimal valid shape.
        match kind {
            "fauna.nostr.bunker.create_invite" => {
                fauna_protocol::encode_canonical(&CreateBunkerInviteReply {
                    connection_id: 7,
                    connect_string: "bunker://abcd?relay=wss://x/nostr&secret=s".into(),
                    signer_pubkey: "abcd".into(),
                    expires_at: 1000,
                    extra: Default::default(),
                })
            }
            "fauna.nostr.bunker.list" => fauna_protocol::encode_canonical(&ListBunkerAppsReply {
                apps: vec![],
                extra: Default::default(),
            }),
            "fauna.nostr.bunker.revoke" => {
                fauna_protocol::encode_canonical(&RevokeBunkerAppReply {
                    revoked: true,
                    extra: Default::default(),
                })
            }
            "fauna.nostr.bunker.set_label" => {
                fauna_protocol::encode_canonical(&SetBunkerAppLabelReply {
                    updated: true,
                    extra: Default::default(),
                })
            }
            #[cfg(feature = "zaps")]
            "fauna.nostr.zap_signers.list" => {
                fauna_protocol::encode_canonical(&ListZapSignersReply {
                    signers: vec![],
                    extra: Default::default(),
                })
            }
            #[cfg(feature = "zaps")]
            "fauna.nostr.zap_signers.add" => fauna_protocol::encode_canonical(&AddZapSignerReply {
                signer: ZapSignerEntry {
                    id: 3,
                    signer_pubkey: "ab".repeat(32),
                    label: "Alby".into(),
                    created_at: 1000,
                    extra: Default::default(),
                },
                extra: Default::default(),
            }),
            #[cfg(feature = "zaps")]
            "fauna.nostr.zap_signers.remove" => {
                fauna_protocol::encode_canonical(&RemoveZapSignerReply {
                    removed: true,
                    extra: Default::default(),
                })
            }
            #[cfg(feature = "zaps")]
            "nostr.zaps.total" => fauna_protocol::encode_canonical(&NostrZapTotalReply {
                total_msats: 2500,
                zap_count: 3,
                extra: Default::default(),
            }),
            "nostr.badges.list" => fauna_protocol::encode_canonical(&ListNostrBadgesReply {
                badges: vec![],
                extra: Default::default(),
            }),
            "nostr.events.publish_signed" => {
                fauna_protocol::encode_canonical(&PublishSignedNostrEventReply {
                    extra: Default::default(),
                })
            }
            other => panic!("RecordingRequester: unhandled kind {other}"),
        }
        .expect("encode reply")
        .to_vec()
    }

    // ── fauna.nostr.bunker.* wire-contract tests ─────────────────────────────

    fn bunker_client() -> (
        std::sync::Arc<RecordingRequester>,
        NostrBunkerClient<std::sync::Arc<RecordingRequester>>,
    ) {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = NostrBunkerClient::new(rec.clone());
        (rec, client)
    }

    #[test]
    fn bunker_create_invite_composes_kind_and_payload() {
        let (rec, c) = bunker_client();
        let reply = block_on(c.create_invite()).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.nostr.bunker.create_invite");
        let _req: nostr::CreateBunkerInviteRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        // The reply carries the one-time reveal fields the client renders.
        assert_eq!(reply.connection_id, 7);
        assert!(reply.connect_string.starts_with("bunker://"));
        assert_eq!(reply.signer_pubkey, "abcd");
    }

    #[test]
    fn bunker_list_composes_kind_and_payload() {
        let (rec, c) = bunker_client();
        block_on(c.list()).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.nostr.bunker.list");
        let _req: nostr::ListBunkerAppsRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
    }

    #[test]
    fn bunker_revoke_composes_kind_and_payload() {
        let (rec, c) = bunker_client();
        assert!(block_on(c.revoke(42)).expect("infallible mock"));
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.nostr.bunker.revoke");
        let req: nostr::RevokeBunkerAppRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.connection_id, 42);
    }

    #[test]
    fn bunker_set_label_composes_kind_and_payload() {
        let (rec, c) = bunker_client();
        assert!(block_on(c.set_label(42, "My phone".into())).expect("infallible mock"));
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.nostr.bunker.set_label");
        let req: nostr::SetBunkerAppLabelRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.connection_id, 42);
        assert_eq!(req.label, "My phone");
    }

    // ── fauna.nostr.zap_signers.* wire-contract tests ────────────────────────
    //
    // `zaps`-gated with the surface they cover, so the Damus flavor runs the
    // rest of this module rather than failing to compile.

    #[cfg(feature = "zaps")]
    fn zap_signer_client() -> (
        std::sync::Arc<RecordingRequester>,
        NostrZapSignerClient<std::sync::Arc<RecordingRequester>>,
    ) {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = NostrZapSignerClient::new(rec.clone());
        (rec, client)
    }

    #[test]
    #[cfg(feature = "zaps")]
    fn zap_signers_list_composes_kind_and_payload() {
        let (rec, c) = zap_signer_client();
        let signers = block_on(c.list()).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.nostr.zap_signers.list");
        let _req: nostr::ListZapSignersRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        // An empty roster is a valid answer, not an error: it is the
        // out-of-the-box state in which no zap receipt is believed.
        assert!(signers.is_empty());
    }

    #[test]
    #[cfg(feature = "zaps")]
    fn zap_signers_add_composes_kind_and_payload() {
        let (rec, c) = zap_signer_client();
        let signer = block_on(c.add("AB".repeat(32), "Alby".into())).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.nostr.zap_signers.add");
        let req: nostr::AddZapSignerRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        // The wrapper transmits the caller's string verbatim — normalization
        // is the nest's job, and the reply is what the client renders.
        assert_eq!(req.signer_pubkey, "AB".repeat(32));
        assert_eq!(req.label, "Alby");
        assert_eq!(signer.signer_pubkey, "ab".repeat(32));
    }

    #[test]
    #[cfg(feature = "zaps")]
    fn zap_signers_remove_composes_kind_and_payload() {
        let (rec, c) = zap_signer_client();
        let removed = block_on(c.remove("cd".repeat(32))).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.nostr.zap_signers.remove");
        let req: nostr::RemoveZapSignerRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        // Keyed by the pubkey itself — no roster round-trip needed first.
        assert_eq!(req.signer_pubkey, "cd".repeat(32));
        assert!(removed);
    }

    // ── nostr.* content wire-contract tests ──────────────────────────────────

    fn content_client() -> (
        std::sync::Arc<RecordingRequester>,
        NostrContentClient<std::sync::Arc<RecordingRequester>>,
    ) {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = NostrContentClient::new(rec.clone());
        (rec, client)
    }

    #[test]
    #[cfg(feature = "zaps")]
    fn zap_total_composes_kind_and_payload() {
        let (rec, c) = content_client();
        let reply = block_on(c.zap_total("evt1".into())).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "nostr.zaps.total");
        let req: nostr::NostrZapTotalRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.event_id, "evt1");
        assert_eq!(reply.total_msats, 2500);
        assert_eq!(reply.zap_count, 3);
    }

    #[test]
    fn badges_composes_kind_and_payload() {
        let (rec, c) = content_client();
        block_on(c.badges("awardee_pk".into())).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "nostr.badges.list");
        let req: nostr::ListNostrBadgesRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.pubkey, "awardee_pk");
    }

    #[test]
    fn publish_signed_composes_kind_and_payload() {
        let (rec, c) = content_client();
        block_on(c.publish_signed("{\"kind\":1}".into())).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "nostr.events.publish_signed");
        let req: nostr::PublishSignedNostrEventRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.event_json, "{\"kind\":1}");
    }

    #[test]
    #[cfg(feature = "zaps")]
    fn zap_signer_row_text_prefers_label_then_falls_back_to_unnamed() {
        let entry = |label: &str| ZapSignerEntry {
            signer_pubkey: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcd"
                .to_string(),
            label: label.to_string(),
            ..Default::default()
        };
        let labeled = zap_signer_row_text(&entry("Alby"));
        assert!(labeled.starts_with("Alby — "), "got {labeled}");

        let unlabeled = zap_signer_row_text(&entry("  "));
        assert!(
            unlabeled.starts_with("Unnamed signer — "),
            "got {unlabeled}"
        );
    }
}
