//! UniFFI façade for the `fauna.subscriptions.*` Layer-3 WS-RPC kinds — the
//! subscription / tiers surface an author drives (tier CRUD, the
//! subscribe/unsubscribe flow, the author-side request queue, and the
//! KeyBlob fetch).
//!
//! [`FfiSubscriptionsClient`] wraps `fauna_client_subscriptions::
//! SubscriptionsClient` (which in turn wraps the shared `NestClient`); the
//! mirror enums/records below are the FFI-visible shape of
//! `fauna_protocol::subscriptions::*`. The Rust-native Linux app will call
//! the same `SubscriptionsClient` directly — this seam gives Apple / Windows /
//! Android the identical surface over UniFFI.
//!
//! `ActorId` crosses as `Vec<u8>` (32 bytes), `Timestamp` as `u64`
//! (microseconds), and the signed `EmbedAsBytes` payloads reuse the existing
//! [`crate::FfiEmbedAsBytes`] from the author-mint primitive (`subscription.rs`)
//! — the `key_blob` an author mints via `mint_key_blob` drops straight into
//! [`FfiEncryptedKeyBlobUpload`].
//!
//! All conversions are exhaustive matches / total field maps: adding a variant
//! or field to the protocol types is a compile error here, so the mirror can't
//! silently drift (priority #1/#4).

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_subscriptions::SubscriptionsClient;
use fauna_client_subscriptions::subscriptions::{
    ApproveRequestReply, EncryptedKeyBlobUpload, KeyBlobGetReply, MineSubscription, PendingRequest,
    RemoveSubscriberReply, StatusGetReply, SubscribeReply, SubscriberEntry, TierItem,
    UnsubscribeReply,
};
use fauna_core::encoding::EmbedAsBytes;

use crate::{FfiEmbedAsBytes, FfiError, bytes_to_actor_id, general_err, stringify};

// ── EncryptedKeyBlobUpload mirror (input) ──────────────────────────────

/// FFI mirror of [`fauna_protocol::subscriptions::EncryptedKeyBlobUpload`] —
/// the encrypted-mode envelope an author submits with `requests_approve` /
/// `subscribers_remove`. Both fields are signed embed-as-bytes payloads: the
/// freshly minted broadcast `KeyBlob` (from `mint_key_blob`) and the author's
/// `DeviceAuthorization` carrying `ManageSubscribers`.
#[derive(uniffi::Record, Clone)]
pub struct FfiEncryptedKeyBlobUpload {
    pub key_blob: FfiEmbedAsBytes,
    pub signer_auth: FfiEmbedAsBytes,
}

impl From<FfiEncryptedKeyBlobUpload> for EncryptedKeyBlobUpload {
    fn from(u: FfiEncryptedKeyBlobUpload) -> Self {
        EncryptedKeyBlobUpload {
            key_blob: embed_from_ffi(u.key_blob),
            signer_auth: embed_from_ffi(u.signer_auth),
            extra: Default::default(),
        }
    }
}

// ── SubscribeReply / UnsubscribeReply mirrors ──────────────────────────

/// FFI mirror of [`fauna_protocol::subscriptions::SubscribeReply`].
#[derive(uniffi::Enum, Clone, Debug, PartialEq)]
pub enum FfiSubscribeReply {
    Approved {
        tier: String,
        /// Subscription expiry in microseconds since the Unix epoch, if the
        /// tier sets one.
        expires_at: Option<u64>,
    },
    Queued {
        request_id: i64,
    },
}

/// The error an outcome a newer nest added becomes: the request reached the
/// nest, but this build cannot tell what it did, so the app re-reads the
/// subscription status rather than render a guess (`transport.md` § Schema and
/// forward-compat discipline, rule 3). The mirror itself stays closed, so no
/// app gains a case to render.
const UNKNOWN_SUBSCRIPTION_OUTCOME: &str =
    "the nest answered with an outcome this app does not know; re-read the subscription status";

impl TryFrom<SubscribeReply> for FfiSubscribeReply {
    type Error = FfiError;
    fn try_from(r: SubscribeReply) -> Result<Self, FfiError> {
        Ok(match r {
            SubscribeReply::Approved { tier, expires_at } => FfiSubscribeReply::Approved {
                tier,
                expires_at: expires_at.map(|t| t.0),
            },
            SubscribeReply::Queued { request_id } => FfiSubscribeReply::Queued { request_id },
            SubscribeReply::Unknown => return Err(general_err(UNKNOWN_SUBSCRIPTION_OUTCOME)),
        })
    }
}

/// FFI mirror of [`fauna_protocol::subscriptions::UnsubscribeReply`].
#[derive(uniffi::Enum, Clone, Debug, PartialEq)]
pub enum FfiUnsubscribeReply {
    Removed,
    Queued { request_id: i64 },
}

impl TryFrom<UnsubscribeReply> for FfiUnsubscribeReply {
    type Error = FfiError;
    fn try_from(r: UnsubscribeReply) -> Result<Self, FfiError> {
        Ok(match r {
            UnsubscribeReply::Removed => FfiUnsubscribeReply::Removed,
            UnsubscribeReply::Queued { request_id } => FfiUnsubscribeReply::Queued { request_id },
            UnsubscribeReply::Unknown => return Err(general_err(UNKNOWN_SUBSCRIPTION_OUTCOME)),
        })
    }
}

// ── status / requests / roster record mirrors ──────────────────────────

/// FFI mirror of [`fauna_protocol::subscriptions::StatusGetReply`].
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiSubscriptionStatus {
    pub tier: Option<String>,
    pub expires_at: Option<u64>,
    pub auto_approve: bool,
}

impl From<StatusGetReply> for FfiSubscriptionStatus {
    fn from(r: StatusGetReply) -> Self {
        FfiSubscriptionStatus {
            tier: r.tier,
            expires_at: r.expires_at.map(|t| t.0),
            auto_approve: r.auto_approve,
        }
    }
}

/// FFI mirror of [`fauna_protocol::subscriptions::MineSubscription`] — one of the
/// caller's own subscriptions (the consumer-side `mine.list` enumeration powering
/// the `subscription-settings` page `subscription-mine-list`). `status` is the raw
/// wire string (`"active"` | `"pending"`), rendered verbatim; `handle` is the
/// creator's nest-resolved handle if local, else `None`.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiMineSubscription {
    /// 32-byte `ActorId` of the creator the caller subscribes to.
    pub author_id: Vec<u8>,
    pub tier: String,
    pub status: String,
    pub handle: Option<String>,
    /// When the subscription reached its current state, in microseconds since
    /// the Unix epoch (`approved_at` for `active`, `created_at` for `pending`).
    pub since: u64,
    /// The `subscription-mine-author` row label: [`handle`](Self::handle) when
    /// non-blank, else the full hex [`author_id`](Self::author_id). Pre-computed
    /// here through `fauna_core::format::author_display_label` so the label is
    /// derived once at this mirror rather than re-chosen by each app — the
    /// same shape as `FfiFolderActorMember.display`. Consumers render it
    /// verbatim; do **not** re-derive the fallback client-side.
    /// See `docs/goal/behavior/value-formatting.md` § Subscription author label.
    pub author_display: String,
}

impl From<MineSubscription> for FfiMineSubscription {
    fn from(s: MineSubscription) -> Self {
        FfiMineSubscription {
            author_display: fauna_core::format::author_display_label(
                s.handle.as_deref(),
                &s.author_id.0,
            ),
            author_id: s.author_id.0.to_vec(),
            tier: s.tier,
            status: s.status,
            handle: s.handle,
            since: s.since.0,
        }
    }
}

/// FFI mirror of [`fauna_protocol::subscriptions::PendingRequest`].
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiPendingRequest {
    pub request_id: i64,
    /// 32-byte `ActorId` of the requesting subscriber.
    pub subscriber_id: Vec<u8>,
    pub tier_name: String,
    /// `"subscribe"` | `"unsubscribe"`.
    pub kind: String,
    /// Request creation time in microseconds since the Unix epoch.
    pub created_at: u64,
    /// The pending subscriber's 1184-byte ML-KEM-768 encapsulation key
    /// (post-quantum surface B, slice S4b), or `None` if they published none.
    /// An encrypted-mode native author threads this back through
    /// `subscriptions_approve_subscriber` so the mint wraps hybrid to the
    /// brand-new subscriber before they reach the roster.
    pub mlkem_encaps_key: Option<Vec<u8>>,
    /// Verified-payment marker (monetization.md § Pillar 3): the request was
    /// paid for through a configured payment provider, so the author's drain
    /// pump approves it without creator judgment; the UI renders it as paid
    /// rather than awaiting-decision.
    pub payment_entitled: bool,
}

impl From<PendingRequest> for FfiPendingRequest {
    fn from(p: PendingRequest) -> Self {
        FfiPendingRequest {
            request_id: p.request_id,
            subscriber_id: p.subscriber_id.0.to_vec(),
            tier_name: p.tier_name,
            kind: p.kind,
            created_at: p.created_at.0,
            mlkem_encaps_key: p.mlkem_encaps_key.map(|b| b.into_vec()),
            payment_entitled: p.payment_entitled,
        }
    }
}

/// FFI mirror of [`fauna_protocol::subscriptions::ApproveRequestReply`].
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiApproveReply {
    /// 32-byte `ActorId` of the now-approved subscriber.
    pub subscriber: Vec<u8>,
    pub tier: String,
    pub key_version: u64,
}

impl From<ApproveRequestReply> for FfiApproveReply {
    fn from(r: ApproveRequestReply) -> Self {
        FfiApproveReply {
            subscriber: r.subscriber.0.to_vec(),
            tier: r.tier,
            key_version: r.key_version,
        }
    }
}

/// FFI mirror of [`fauna_protocol::subscriptions::SubscriberEntry`].
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiSubscriberEntry {
    /// 32-byte `ActorId` of the subscriber.
    pub subscriber_id: Vec<u8>,
    /// Join time in microseconds since the Unix epoch.
    pub joined_at: u64,
    /// The subscriber's 1184-byte ML-KEM-768 encapsulation key (post-quantum
    /// surface B, slice S4b), or `None` if they published none. An
    /// encrypted-mode native author wraps the period key to the subscriber's
    /// X-Wing hybrid key when present (else classical).
    pub mlkem_encaps_key: Option<Vec<u8>>,
}

impl From<SubscriberEntry> for FfiSubscriberEntry {
    fn from(e: SubscriberEntry) -> Self {
        FfiSubscriberEntry {
            subscriber_id: e.subscriber_id.0.to_vec(),
            joined_at: e.joined_at.0,
            mlkem_encaps_key: e.mlkem_encaps_key.map(|b| b.into_vec()),
        }
    }
}

/// FFI mirror of [`fauna_protocol::subscriptions::RemoveSubscriberReply`].
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiRemoveSubscriberReply {
    /// 32-byte `ActorId` of the removed subscriber.
    pub subscriber: Vec<u8>,
    pub tier: String,
    pub key_version: u64,
}

impl From<RemoveSubscriberReply> for FfiRemoveSubscriberReply {
    fn from(r: RemoveSubscriberReply) -> Self {
        FfiRemoveSubscriberReply {
            subscriber: r.subscriber.0.to_vec(),
            tier: r.tier,
            key_version: r.key_version,
        }
    }
}

// ── encrypted-mode key-material mirrors ────────────────────────────────

/// FFI mirror of [`fauna_protocol::subscriptions::KeyBlobGetReply`].
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiKeyBlob {
    pub version: u64,
    pub blob_hash: Vec<u8>,
    /// dag-cbor-encoded `KeyBlob`.
    pub blob_data: Vec<u8>,
}

impl From<KeyBlobGetReply> for FfiKeyBlob {
    fn from(r: KeyBlobGetReply) -> Self {
        FfiKeyBlob {
            version: r.version,
            blob_hash: r.blob_hash.into_vec(),
            blob_data: r.blob_data.into_vec(),
        }
    }
}

/// FFI mirror of [`fauna_protocol::subscriptions::TierItem`] — one of the
/// calling author's own tier definitions (the authenticated `tiers.list`
/// own-read powering the profile Tiers-tab §1 "My tiers"). Superset of the
/// public HTTP `list_tiers` shape: it carries `auto_approve` so the edit form
/// round-trips it.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiTierItem {
    pub name: String,
    pub rank: u32,
    pub description: Option<String>,
    pub price_hint: Option<String>,
    /// The machine-comparable price in sats, the reverse of the create/update
    /// sats conversion (`monetization.md` § The asking price) — independent
    /// of `price_hint`. `None` when the tier is not for sale to an inferring
    /// mechanism, or a unit this build cannot interpret (fail-closed), so an
    /// edit form pre-fills from this rather than hand-deriving it.
    pub asking_price_sats: Option<u64>,
    pub payment_url: Option<String>,
    pub auto_approve: bool,
    /// Tier creation time in microseconds since the Unix epoch.
    pub created_at: u64,
    /// Per-post pay-to-unlock designation — the hex `post_id` this tier sells
    /// access to (`monetization.md` § Per-post pay-to-unlock); `None` on every
    /// ordinary tier. Present on the author's own `tiers_list` read only: the
    /// generic offer surfaces filter designated tiers out nest-side, and a
    /// client excludes them from its §1 My-tiers list and compose gate picker
    /// off this field.
    pub unlocks_post: Option<String>,
    /// Hidden from every offer surface (`monetization.md` § The unifying
    /// model — *A tier may be hidden*); `false` on every ordinary tier. Present
    /// on the author's own `tiers_list` read only. Defaulted at the FFI
    /// boundary so memberwise construction in app tests keeps compiling
    /// (`version-compatibility.md` § I4 — the FFI binding boundary rule).
    #[uniffi(default = false)]
    pub hidden: bool,
}

impl From<TierItem> for FfiTierItem {
    fn from(t: TierItem) -> Self {
        FfiTierItem {
            name: t.name,
            rank: t.rank,
            description: t.description,
            price_hint: t.price_hint,
            asking_price_sats: t.asking_price.as_ref().and_then(|p| p.to_sats()),
            payment_url: t.payment_url,
            auto_approve: t.auto_approve,
            created_at: t.created_at.0,
            unlocks_post: t.unlocks_post,
            hidden: t.hidden,
        }
    }
}

// ── FfiSubscriptionsClient ─────────────────────────────────────────────

/// UniFFI handle for the `fauna.subscriptions.*` kinds. Construct via
/// [`crate::nest_client::FfiNestClient::subscriptions`]; methods are exposed
/// to Swift as `async throws` and Kotlin as `suspend fun`.
#[derive(uniffi::Object)]
pub struct FfiSubscriptionsClient {
    nest: Arc<NestClient>,
}

impl FfiSubscriptionsClient {
    pub(crate) fn from_nest(nest: Arc<NestClient>) -> Arc<Self> {
        Arc::new(Self { nest })
    }

    fn client(&self) -> SubscriptionsClient<Arc<NestClient>> {
        SubscriptionsClient::new(Arc::clone(&self.nest))
    }
}

#[fauna_uniffi_async::export]
impl FfiSubscriptionsClient {
    // NOTE (2026-07-15 dark-rail audit): the legacy pre-orchestration write
    // wrappers (`tiers_create`, `requests_approve`, `subscribers_remove`,
    // `key_blob_get`) were deleted — every app drives the author side
    // through the `subscriptions_author` orchestration free fns (which mint
    // the KeyBlob / delegations internally), and the subscriber unlock reads
    // `key_blob_get` through the feed manager. Do not re-add thin write
    // wrappers here; wire new author flows through the orchestration.

    /// `fauna.subscriptions.tiers.list` — the calling author's own tier
    /// definitions (ascending by rank), the authenticated own-read powering the
    /// profile Tiers-tab §1 "My tiers" list. Replay-safe pure read.
    pub async fn tiers_list(&self) -> Result<Vec<FfiTierItem>, FfiError> {
        self.client()
            .tiers_list()
            .await
            .map(|tiers| tiers.into_iter().map(FfiTierItem::from).collect())
            .map_err(stringify)
    }

    /// `fauna.subscriptions.offers.list` — **another** author's offered tiers
    /// (`author_id` is their 32-byte `ActorId`), ascending by rank. The OTHER-profile
    /// subscriber-browse read powering the profile Tiers-tab offers section
    /// (`subscription-offers-section`): the bearer-keyed [`Self::tiers_list`] can only
    /// read the caller's own tiers, so a prospective subscriber reads a creator's
    /// offers through this distinct kind. Replay-safe pure read.
    pub async fn offers_list(&self, author_id: Vec<u8>) -> Result<Vec<FfiTierItem>, FfiError> {
        let author = bytes_to_actor_id(&author_id)?;
        self.client()
            .offers_list(author)
            .await
            .map(|tiers| tiers.into_iter().map(FfiTierItem::from).collect())
            .map_err(stringify)
    }

    /// `fauna.subscriptions.tiers.update` — each `None` field is left
    /// unchanged. Returns whether a row was updated.
    ///
    /// `asking_price_sats` is the **machine-comparable** purchase threshold in
    /// the author's own unit, sats (`monetization.md` § The asking price); the
    /// shared pair converts it to the msat wire value, so no app writes that
    /// arithmetic itself. `None` keeps the tier's current price — this method
    /// has no clear verb for any of its fields — and an amount too large to
    /// express in msats is refused rather than clamped.
    #[allow(clippy::too_many_arguments)]
    pub async fn tiers_update(
        &self,
        name: String,
        rank: Option<u32>,
        description: Option<String>,
        price_hint: Option<String>,
        payment_url: Option<String>,
        auto_approve: Option<bool>,
        asking_price_sats: Option<u64>,
    ) -> Result<bool, FfiError> {
        let asking_price = match asking_price_sats {
            Some(sats) => Some(
                fauna_protocol::subscriptions::TierAskingPrice::from_sats(sats).ok_or_else(
                    || FfiError::from("asking price is too large to express in msats".to_string()),
                )?,
            ),
            None => None,
        };
        self.client()
            .tiers_update(
                name,
                rank,
                description,
                price_hint,
                payment_url,
                auto_approve,
                asking_price,
            )
            .await
            .map_err(stringify)
    }

    /// `fauna.subscriptions.tiers.delete`
    pub async fn tiers_delete(&self, name: String) -> Result<bool, FfiError> {
        self.client().tiers_delete(name).await.map_err(stringify)
    }

    /// `fauna.subscriptions.subscribe` — `author_id` is the 32-byte `ActorId`
    /// of the author being subscribed to.
    pub async fn subscribe(
        &self,
        author_id: Vec<u8>,
        tier: String,
    ) -> Result<FfiSubscribeReply, FfiError> {
        let author = bytes_to_actor_id(&author_id)?;
        let reply = self
            .client()
            .subscribe(author, tier)
            .await
            .map_err(stringify)?;
        reply.try_into()
    }

    /// `fauna.subscriptions.unsubscribe`
    pub async fn unsubscribe(&self, author_id: Vec<u8>) -> Result<FfiUnsubscribeReply, FfiError> {
        let author = bytes_to_actor_id(&author_id)?;
        let reply = self.client().unsubscribe(author).await.map_err(stringify)?;
        reply.try_into()
    }

    /// `fauna.subscriptions.mine.list` — the calling actor's own subscriptions
    /// across **every** creator (active + pending), the consumer-side enumeration
    /// powering the `subscription-settings` page. Caller-scoped, replay-safe pure
    /// read; distinct from [`Self::status_get`], which is per-creator.
    pub async fn mine_list(&self) -> Result<Vec<FfiMineSubscription>, FfiError> {
        let subs = self.client().mine_list().await.map_err(stringify)?;
        Ok(subs.into_iter().map(Into::into).collect())
    }

    /// `fauna.subscriptions.status.get`
    pub async fn status_get(&self, author_id: Vec<u8>) -> Result<FfiSubscriptionStatus, FfiError> {
        let author = bytes_to_actor_id(&author_id)?;
        let reply = self.client().status_get(author).await.map_err(stringify)?;
        Ok(reply.into())
    }

    /// `fauna.subscriptions.requests.list`
    pub async fn requests_list(&self) -> Result<Vec<FfiPendingRequest>, FfiError> {
        let requests = self.client().requests_list().await.map_err(stringify)?;
        Ok(requests.into_iter().map(Into::into).collect())
    }

    /// `fauna.subscriptions.requests.reject`
    pub async fn requests_reject(&self, request_id: i64) -> Result<bool, FfiError> {
        self.client()
            .requests_reject(request_id)
            .await
            .map_err(stringify)
    }

    /// `fauna.subscriptions.subscribers.list`
    pub async fn subscribers_list(
        &self,
        tier_name: String,
    ) -> Result<Vec<FfiSubscriberEntry>, FfiError> {
        let subscribers = self
            .client()
            .subscribers_list(tier_name)
            .await
            .map_err(stringify)?;
        Ok(subscribers.into_iter().map(Into::into).collect())
    }

    /// `fauna.subscriptions.delegate.upload` — `authorization` is the signed
    /// `DeviceAuthorization` in embed-as-bytes shape. Returns whether stored.
    ///
    /// NOT WIRED (2026-07-15 dark-rail audit): no client drives a raw
    /// `DeviceAuthorization` upload — the author orchestration mints
    /// self-delegations internally. Kept as the parked multi-device-author
    /// surface.
    pub async fn delegate_upload(&self, authorization: FfiEmbedAsBytes) -> Result<bool, FfiError> {
        self.client()
            .delegate_upload(embed_from_ffi(authorization))
            .await
            .map_err(stringify)
    }
}

/// `FfiEmbedAsBytes` → `fauna_core::encoding::EmbedAsBytes` (the inverse of the
/// `from_signed`/`into_signed` round-trip `subscription.rs::mint_key_blob`
/// uses). Both carry the same `{ envelope, bytes }` shape — this is a pure
/// field move, no validation (the nest re-verifies the signature).
fn embed_from_ffi(e: FfiEmbedAsBytes) -> EmbedAsBytes {
    EmbedAsBytes {
        envelope: e.envelope,
        bytes: e.bytes,
        signer_auth: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_subscriptions::subscriptions::{
        ApproveRequestReply, MineSubscription, PendingRequest, StatusGetReply, SubscribeReply,
        UnsubscribeReply,
    };
    use fauna_core::data::Timestamp;
    use fauna_core::identity::ActorId;

    #[test]
    fn subscribe_reply_maps_both_outcomes() {
        let approved = SubscribeReply::Approved {
            tier: "gold".into(),
            expires_at: Some(Timestamp(1_700_000_000_000)),
        };
        assert_eq!(
            FfiSubscribeReply::try_from(approved).unwrap(),
            FfiSubscribeReply::Approved {
                tier: "gold".into(),
                expires_at: Some(1_700_000_000_000),
            }
        );
        assert_eq!(
            FfiSubscribeReply::try_from(SubscribeReply::Queued { request_id: 7 }).unwrap(),
            FfiSubscribeReply::Queued { request_id: 7 }
        );
    }

    #[test]
    fn unsubscribe_reply_maps_both_outcomes() {
        assert_eq!(
            FfiUnsubscribeReply::try_from(UnsubscribeReply::Removed).unwrap(),
            FfiUnsubscribeReply::Removed
        );
        assert_eq!(
            FfiUnsubscribeReply::try_from(UnsubscribeReply::Queued { request_id: 3 }).unwrap(),
            FfiUnsubscribeReply::Queued { request_id: 3 }
        );
    }

    /// An outcome a newer nest added reaches the app as an error — sent, state
    /// unknown, re-read — never as a case the app renders.
    #[test]
    fn an_unknown_subscription_outcome_is_an_error_not_a_case() {
        assert!(FfiSubscribeReply::try_from(SubscribeReply::Unknown).is_err());
        assert!(FfiUnsubscribeReply::try_from(UnsubscribeReply::Unknown).is_err());
    }

    #[test]
    fn status_maps() {
        let proto = StatusGetReply {
            tier: Some("silver".into()),
            expires_at: Some(Timestamp(42)),
            auto_approve: true,
            extra: Default::default(),
        };
        let ffi: FfiSubscriptionStatus = proto.into();
        assert_eq!(ffi.tier.as_deref(), Some("silver"));
        assert_eq!(ffi.expires_at, Some(42));
        assert!(ffi.auto_approve);
    }

    #[test]
    fn pending_request_maps_actor_and_timestamp() {
        let proto = PendingRequest {
            request_id: 11,
            subscriber_id: ActorId([9u8; 32]),
            tier_name: "gold".into(),
            kind: "subscribe".into(),
            created_at: Timestamp(123),
            mlkem_encaps_key: Some(fauna_protocol::ByteBuf::from(vec![4u8; 1184])),
            payment_entitled: true,
            extra: Default::default(),
        };
        let ffi: FfiPendingRequest = proto.into();
        assert_eq!(ffi.subscriber_id, vec![9u8; 32]);
        assert_eq!(ffi.created_at, 123);
        assert_eq!(ffi.kind, "subscribe");
        assert_eq!(ffi.mlkem_encaps_key.as_deref(), Some(&[4u8; 1184][..]));
        assert!(ffi.payment_entitled, "payment marker maps through");
    }

    #[test]
    fn mine_subscription_maps_actor_status_and_handle() {
        let proto = MineSubscription {
            author_id: ActorId([3u8; 32]),
            tier: "gold".into(),
            status: "pending".into(),
            handle: Some("alice".into()),
            since: Timestamp(123),
            extra: Default::default(),
        };
        let ffi: FfiMineSubscription = proto.into();
        assert_eq!(ffi.author_id, vec![3u8; 32]);
        assert_eq!(ffi.tier, "gold");
        assert_eq!(ffi.status, "pending");
        assert_eq!(ffi.handle.as_deref(), Some("alice"));
        assert_eq!(ffi.since, 123);
        // The row label is pre-computed at this mirror, not re-chosen per client.
        assert_eq!(ffi.author_display, "alice");
    }

    /// The fallback half of the pre-computed label: with no resolvable handle the
    /// mirror carries the full hex actor id, so a client that renders
    /// `author_display` verbatim needs no chooser of its own.
    #[test]
    fn mine_subscription_author_display_falls_back_to_full_hex() {
        let mine = |handle: Option<&str>| -> FfiMineSubscription {
            MineSubscription {
                author_id: ActorId([0xabu8; 32]),
                tier: "gold".into(),
                status: "active".into(),
                handle: handle.map(str::to_string),
                since: Timestamp(1),
                extra: Default::default(),
            }
            .into()
        };
        let hex = "ab".repeat(32);
        assert_eq!(mine(None).author_display, hex);
        assert_eq!(mine(Some("")).author_display, hex);
        // The drift this closed: a whitespace-only handle rendered a blank row on
        // every app except web.
        assert_eq!(mine(Some("   ")).author_display, hex);
        assert_eq!(mine(Some("  alice  ")).author_display, "alice");
    }

    #[test]
    fn approve_reply_maps_actor() {
        let proto = ApproveRequestReply {
            subscriber: ActorId([5u8; 32]),
            tier: "gold".into(),
            key_version: 4,
            extra: Default::default(),
        };
        let ffi: FfiApproveReply = proto.into();
        assert_eq!(ffi.subscriber, vec![5u8; 32]);
        assert_eq!(ffi.key_version, 4);
    }

    #[test]
    fn encrypted_upload_field_moves() {
        let upload = FfiEncryptedKeyBlobUpload {
            key_blob: FfiEmbedAsBytes {
                envelope: vec![1, 2, 3],
                bytes: vec![4, 5],
            },
            signer_auth: FfiEmbedAsBytes {
                envelope: vec![6],
                bytes: vec![7, 8],
            },
        };
        let proto: EncryptedKeyBlobUpload = upload.into();
        assert_eq!(proto.key_blob.envelope, vec![1, 2, 3]);
        assert_eq!(proto.signer_auth.bytes, vec![7, 8]);
    }
}
