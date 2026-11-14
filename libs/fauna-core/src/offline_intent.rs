//! `OfflineIntentQueue` — where an `OfflineQueued` gesture goes while the app
//! has no nest (W4 (account-data-plane.md § Workstreams), the composer seam).
//!
//! Charter: `account-offline-mutation.md` § The offline-mutation contract,
//! discriminator 1 — an `OfflineQueued` intent is not a store item; it awaits
//! an effect the nest assigns, so with no nest it rests in the device's durable
//! outbox and drains when the nest is back. The decision *whether* to queue is
//! `fauna_protocol::offline_intent::send_or_queue`'s (one owner, the same
//! `is_online` polarity the UI's offline gate reads); this trait is only the two
//! facts that decision needs from the app: its current connection word, and a
//! durable place to put the intent.
//!
//! It lives here, not beside the composer, so a shared manager crate (the feed
//! manager is the first) can hold an `Arc<dyn OfflineIntentQueue>` without a
//! store or runtime dependency: the one implementation every app builds, native
//! and web alike, sits over the account runtime's
//! `AccountStoreHandle::enqueue_intent`
//! (`fauna_account_plane::account_driver::offline_outbox`). `MaybeSendSync` +
//! the dual `async_trait` arm, exactly as
//! [`crate::nest_reseal::ChunkStoreSink`].

/// The app's outbox door plus the connection fact the composer reads.
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait OfflineIntentQueue: crate::MaybeSendSync {
    /// The app's current transport word — the lowercase
    /// `fauna_core::format::connection_state_label` input
    /// (`"connected"`, `"connecting"`, `"disconnected"`, `"unreachable"`), the
    /// same word the app's offline gate reads, so the gate that keeps a
    /// control live and the composer that queues its gesture never disagree
    /// about whether there is a nest.
    fn connection_state(&self) -> String;

    /// Append one durable intent: `kind` (must be `OfflineQueued`), the FIFO
    /// `scope` it drains in (a rejection parks the scope's remainder), and the
    /// canonical DAG-CBOR request `payload` the drain re-sends verbatim under
    /// the intent id as its idempotency key. Durable before it returns; answers
    /// the minted intent id. An `Err` is the reason, already rendered.
    async fn enqueue(
        &self,
        kind: &'static str,
        scope: String,
        payload: Vec<u8>,
    ) -> Result<[u8; 16], String>;

    /// Append one MLS plaintext intent — `fauna.conversations.channel.send`'s
    /// application payload, **not** a sealed message — drained only by the
    /// conversations engine's [`MlsIntentSender`], which seals it at the
    /// channel's then-current epoch (`devices.md` § Offline compose). `scope`
    /// is the channel's FIFO scope. Default: refused — an outbox with no MLS
    /// drainer behind it must never hold an intent nothing will send.
    async fn enqueue_mls(&self, scope: String, payload: Vec<u8>) -> Result<[u8; 16], String> {
        let _ = (scope, payload);
        Err("this outbox has no MLS drainer".to_string())
    }

    /// Append one identity-bound intent (`account-offline-mutation.md` § The
    /// offline-mutation contract → *Identity-bound intents compose at
    /// drain*): `payload` is the kind's INPUTS in canonical CBOR, never the
    /// signed wire request — the app's [`IdentityIntentSender`] composes and
    /// signs that at drain, as the then-current identity. `kind` must be
    /// `OfflineQueued`; `scope` is its FIFO scope. Default: refused — an
    /// outbox with no identity drainer behind it must never hold an intent
    /// nothing will send.
    async fn enqueue_identity(
        &self,
        kind: &'static str,
        scope: String,
        payload: Vec<u8>,
    ) -> Result<[u8; 16], String> {
        let _ = (kind, scope, payload);
        Err("this outbox has no identity drainer".to_string())
    }

    /// Every unsent change this outbox holds — the unsent-changes read model
    /// (`account-offline-mutation.md` § The offline-mutation contract →
    /// *The unsent-changes surface*, point 4), scope order then compose
    /// order, each in its derived [`UnsentState`] and described in user words.
    /// The feed and conversations managers read it to derive the like's state
    /// word and the queued own copy's state, so no app decides either.
    /// Default: nothing held — an outbox with no durable store behind it (a
    /// test double) lists nothing.
    async fn unsent_changes(&self) -> Result<Vec<UnsentChange>, String> {
        Ok(Vec::new())
    }

    /// The user's retry of a not-sent change: it returns to waiting at its own
    /// place in its scope and a drain pass starts, so it is sent first and the
    /// scope's remainder follows. `Ok(false)` = no such not-sent change
    /// (unknown, or still pending). Default: nothing to retry.
    async fn retry_intent(&self, intent_id: [u8; 16]) -> Result<bool, String> {
        let _ = intent_id;
        Ok(false)
    }

    /// The user's discard of a not-sent change — the one user-initiated
    /// deletion of a pending write, behind the surface's confirm. Deletes a
    /// not-sent change only, then starts a drain pass so its scope's next
    /// intent tries. `Ok(false)` = no such not-sent change. Default: nothing
    /// to discard.
    async fn discard_intent(&self, intent_id: [u8; 16]) -> Result<bool, String> {
        let _ = intent_id;
        Ok(false)
    }
}

/// Where an unsent change stands — the three states of
/// `account-offline-mutation.md` § The offline-mutation contract →
/// *The unsent-changes surface*, derived exactly as the drain derives its hold
/// ([`derive_unsent_states`]).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum UnsentState {
    /// Pending, its scope free: the next drain pass sends it.
    Waiting,
    /// Pending behind its scope's parked head: it waits for the user to retry
    /// or discard that head.
    Held,
    /// Parked by the drain. `reason_code` is the stored stable code (the
    /// nest's error code, or one of [`park_code`]'s); empty when the store
    /// recorded none.
    Unsent { reason_code: String },
}

/// What an unsent change is about, for the surfaces that mark the affordance
/// in place (point 2 of the ruling) and the list row's target.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(tag = "target", rename_all = "snake_case")]
pub enum IntentTarget {
    /// An interaction on a post (`fauna.posts.interact`): `action` is the
    /// wire action (`like`, `unlike`, `repost`, …).
    Post { post_id: String, action: String },
    /// A message on a conversation's MLS channel (hex); `excerpt` is the
    /// body's head, for the list row.
    Conversation { channel_id: String, excerpt: String },
}

/// An unsent change in user words: the label the list row shows and, when the
/// describer could read one, its target.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct IntentDescription {
    pub label: crate::localized::LocalizedText,
    pub target: Option<IntentTarget>,
}

/// One unsent change — a row of the unsent-changes list.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct UnsentChange {
    /// The intent id (lowercase hex) — its idempotency key, and what retry
    /// and discard take ([`parse_intent_id`]).
    pub intent_id: String,
    pub kind: String,
    pub scope: String,
    pub state: UnsentState,
    /// Seconds since the epoch at compose.
    pub created_at: u64,
    /// Attempts that did not conclude.
    pub attempts: u32,
    pub description: IntentDescription,
    /// Why a [`UnsentState::Held`] row waits, when it is not the scope's
    /// parked head: a stable code of [`hold_code`]'s. `None` on every row of
    /// this device's own outbox — those are held by their scope's park, the
    /// three-state derivation's own hold. `Some` on a retired identity's row
    /// not yet carried into this outbox (`succession-aftermath.md` § Re-key
    /// scope, the device-local outbox row): the state still says *held*, and
    /// this says by what. Additive: an older reader drops it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold: Option<String>,
}

impl UnsentChange {
    /// The intent id as the store's key.
    pub fn intent_id_bytes(&self) -> Option<[u8; 16]> {
        parse_intent_id(&self.intent_id)
    }
}

/// An intent id from its lowercase-hex form; `None` for anything that is not
/// 32 hex digits.
pub fn parse_intent_id(hex_id: &str) -> Option<[u8; 16]> {
    hex::decode(hex_id).ok()?.try_into().ok()
}

/// One outbox row as the state derivation reads it: its scope and, when it is
/// parked, its stored failure reason (`Some("")` for a parked row with none).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutboxRowFacts<'a> {
    pub scope: &'a str,
    /// `Some(reason)` on a parked (`Failed`) row; `None` on a pending one.
    pub parked: Option<&'a str>,
}

/// Each row's [`UnsentState`], index-parallel with `rows` — the drain's own
/// hold rule (`fauna_account_plane::outbox::drain_outbox`): a parked row is
/// [`UnsentState::Unsent`]; a pending row in a scope that has any parked row
/// is [`UnsentState::Held`]; every other pending row is
/// [`UnsentState::Waiting`]. The hold is per scope, not per position — a
/// scope's parked rows hold every pending row of it, which is exactly what
/// the drain skips.
pub fn derive_unsent_states(rows: &[OutboxRowFacts<'_>]) -> Vec<UnsentState> {
    let parked_scopes: std::collections::BTreeSet<&str> = rows
        .iter()
        .filter(|r| r.parked.is_some())
        .map(|r| r.scope)
        .collect();
    rows.iter()
        .map(|r| match r.parked {
            Some(reason) => UnsentState::Unsent {
                reason_code: reason.to_string(),
            },
            None if parked_scopes.contains(r.scope) => UnsentState::Held,
            None => UnsentState::Waiting,
        })
        .collect()
}

/// The kind an MLS plaintext intent drains as (`devices.md` § Offline compose →
/// *Idempotency*: the intent id rides this kind into the nest's durable
/// idempotency table).
pub const MLS_INTENT_KIND: &str = "fauna.conversations.channel.send";

/// What [`MlsIntentSender::send_mls_intent`] made of one queued MLS intent —
/// the three outcomes the outbox's FIFO policy needs (`devices.md` § Offline
/// compose → *FIFO per channel, loud on permanent failure*).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MlsIntentOutcome {
    /// The nest accepted the sealed message (or answered a replay of the same
    /// intent id with its recorded ack): the intent is done.
    Sent,
    /// A retryable failure — no nest, a transport fault, a stalled log walk.
    /// The intent stays, an attempt is counted, and the drain stops for this
    /// pass (the next pass retries from here).
    Retry(String),
    /// A permanent failure — removed from the group, the channel deleted, a
    /// payload this build cannot read. The intent is parked, and the rest of
    /// its channel's queue with it: never dropped, never reordered around.
    /// `code` is one of [`park_code`]'s, stored as the parked row's
    /// `failure_reason` (the unsent-changes surface renders it); `why` is the
    /// developer diagnostic, logged and never stored.
    Park { code: &'static str, why: String },
}

/// The stable codes an outbox park stores as the intent's `failure_reason`
/// when the drain itself, not a nest error code, names the cause
/// (`account-offline-mutation.md` § The offline-mutation contract →
/// *The unsent-changes surface*: "a stable code (the nest's error code, or
/// the MLS outcome's kind), never free text"). A rejection the nest answered
/// stores the nest's own error code instead. Never renamed: a stored code is
/// read back by every later build.
pub mod park_code {
    /// The intent's kind is one this build cannot drain (an app downgrade
    /// reading a newer store).
    pub const UNKNOWN_KIND: &str = "fauna.outbox.unknown_kind";
    /// The intent's payload does not decode.
    pub const UNDECODABLE: &str = "fauna.outbox.undecodable";
    /// The nest refused the intent without a wire error code.
    pub const REJECTED: &str = "fauna.outbox.rejected";
    /// An MLS intent names no channel.
    pub const MLS_NO_CHANNEL: &str = "fauna.outbox.mls.no_channel";
    /// This device is no longer a member of the intent's conversation
    /// (removed, or the conversation deleted).
    pub const MLS_NOT_A_MEMBER: &str = "fauna.outbox.mls.not_a_member";
    /// The sealing send failed permanently.
    pub const MLS_SEND_FAILED: &str = "fauna.outbox.mls.send_failed";
    /// A staged attachment the intent names is not on this device any more
    /// (a retired identity's intent carried across a succession, or bytes
    /// evicted): the remedy is discard and recompose, never a send without
    /// the attachment.
    pub const MLS_ATTACHMENT_MISSING: &str = "fauna.outbox.mls.attachment_missing";
}

/// The stable codes an [`UnsentChange::hold`] carries — why a held row waits
/// when no park in its scope holds it. Never renamed: a surface renders each
/// by name.
pub mod hold_code {
    /// A retired identity's intent not yet carried into its successor's
    /// outbox — an MLS intent waiting for the successor's engine to hold its
    /// conversation's group (`succession-aftermath.md` § Re-key scope, the
    /// device-local outbox row, point 4), or any intent the next carry pass
    /// has not reached yet.
    pub const CARRY_AWAITING_GROUP: &str = "fauna.outbox.carry.awaiting_group";
}

/// The conversations engine's half of the MLS outbox drain: seal one queued
/// plaintext intent at the channel's then-current epoch and send it under the
/// intent id as its idempotency key (`devices.md` § Offline compose → *Drain =
/// the ordinary send path, from an MLS-hosting process*).
///
/// Implemented by the process that hosts the account's conversations engine —
/// an app, never the bearer-only sync agent — and handed to the outbox's MLS
/// drain, which owns the order and the park policy. It lives here, beside
/// [`OfflineIntentQueue`], so neither the store crate nor the conversations
/// crate needs the other.
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait MlsIntentSender: crate::MaybeSendSync {
    /// Seal and send one intent's `payload` (the bytes its composer queued)
    /// under `intent_id`.
    async fn send_mls_intent(&self, intent_id: [u8; 16], payload: &[u8]) -> MlsIntentOutcome;

    /// Whether this engine holds the group of the channel `payload` (a queued
    /// MLS intent's bytes) names — the outbox carry's MLS hold
    /// (`succession-aftermath.md` § Re-key scope, the device-local outbox
    /// row, point 4): a retired identity's MLS intent moves into this outbox
    /// only once its group is here, since the drain would park it *not a
    /// member* before. Default `false`: a sender that cannot answer carries no
    /// MLS row, and the row waits listed as held.
    fn holds_group_of(&self, payload: &[u8]) -> bool {
        let _ = payload;
        false
    }
}

/// What [`IdentityIntentSender::send_identity_intent`] made of one queued
/// identity-bound intent — the same three outcomes as [`MlsIntentOutcome`],
/// except that a park carries the nest's own error code when the nest
/// refused (the generic drain's rule), so the code is owned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityIntentOutcome {
    /// The nest accepted the composed request (or answered a replay of the
    /// same intent id with its recorded outcome): the intent is done.
    Sent,
    /// A retryable failure — no nest, a transport fault, no identity seated
    /// yet. The intent stays, an attempt is counted, and the drain stops for
    /// this pass.
    Retry(String),
    /// A permanent failure — the nest refused the request, the inputs do not
    /// decode, a kind this build has no composer for. The intent is parked,
    /// and the rest of its scope with it. `code` is the nest's error code or
    /// one of [`park_code`]'s, stored as the row's `failure_reason`; `why` is
    /// the developer diagnostic, logged and never stored.
    Park { code: String, why: String },
}

/// The app's half of the identity drain (`account-offline-mutation.md` § The
/// offline-mutation contract → *Identity-bound intents compose at
/// drain*): compose one queued intent's wire request from its stored inputs
/// and the identity the app holds NOW, sign it, and send it under the intent
/// id as its idempotency key.
///
/// Implemented only by a process holding the identity seed — an app, never
/// the bearer-only sync agent — and handed to the outbox's identity drain,
/// which owns the order and the park policy. It lives here, beside
/// [`MlsIntentSender`], so neither the store crate nor a kind's client crate
/// needs the other.
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait IdentityIntentSender: crate::MaybeSendSync {
    /// Compose, sign and send one intent of `kind` from its queued inputs
    /// `payload` under `intent_id`.
    async fn send_identity_intent(
        &self,
        intent_id: [u8; 16],
        kind: &str,
        payload: &[u8],
    ) -> IdentityIntentOutcome;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(scope: &str) -> OutboxRowFacts<'_> {
        OutboxRowFacts {
            scope,
            parked: None,
        }
    }

    /// The three states follow the drain's hold: a parked row is not sent
    /// with its code, a pending row in a scope with a parked row is held
    /// (behind or ahead of it — the drain skips the whole scope), and a
    /// pending row in a free scope waits.
    #[test]
    fn the_states_follow_the_drains_hold() {
        let rows = [
            pending("posts.interact:aa"),
            OutboxRowFacts {
                scope: "posts.interact:bb",
                parked: Some("fauna.outbox.rejected"),
            },
            pending("posts.interact:bb"),
            pending("conv:cc"),
            OutboxRowFacts {
                scope: "conv:dd",
                parked: Some(""),
            },
        ];
        assert_eq!(
            derive_unsent_states(&rows),
            vec![
                UnsentState::Waiting,
                UnsentState::Unsent {
                    reason_code: "fauna.outbox.rejected".into()
                },
                UnsentState::Held,
                UnsentState::Waiting,
                UnsentState::Unsent {
                    reason_code: String::new()
                },
            ]
        );
    }

    #[test]
    fn an_intent_id_round_trips_through_its_hex() {
        let id = [0xAB; 16];
        assert_eq!(parse_intent_id(&hex::encode(id)), Some(id));
        assert_eq!(parse_intent_id("ab"), None);
        assert_eq!(parse_intent_id("not hex"), None);
    }
}
