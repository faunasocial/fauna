use crate::address::{Rail, TypedAddress};
use crate::capabilities::ThreadCapabilities;
use crate::compose::ComposeState;
use crate::message::MessageId;
use crate::snapshot::ThreadDetail;
use crate::thread::ThreadId;
use async_trait::async_trait;
use fauna_mls::types::{ChannelId, ReactionOp};
// `Send + Sync` natively, empty on wasm — the supertrait that lets each seam
// below have one trait body across both targets, paired with the dual
// `async_trait` arms. Canonical in `fauna-core` (this crate deliberately has no
// `fauna-protocol` dep; `fauna_protocol::MaybeSendSync` re-exports the same
// trait).
use fauna_core::MaybeSendSync;
use fauna_core::data::ArrivalDisposition;
use thiserror::Error;

/// Wire-shaped inbound message handed to the manager by a backend.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct RailInboundMessage {
    pub rail: Rail,
    pub sender: TypedAddress,
    pub recipients: Vec<TypedAddress>, // includes self
    pub subject: Option<String>,
    pub body: String,
    pub body_format: crate::message::BodyFormat,
    pub timestamp_ms: i64,
    pub message_id: MessageId,
    pub in_reply_to: Option<MessageId>,
    pub attachments: Vec<crate::message::AttachmentSnapshot>,
    pub badges: crate::message::MessageBadges,
    /// `Some(reference)` iff the nest has taken this message down under a legal
    /// obligation (the conversation twin of the post takedown; `moderation.md`
    /// § Categories & enforcement item 1). The sealed envelope was **withheld**
    /// (empty) from the relay fetch, so there is nothing to decrypt — the driver
    /// ([`crate::backends::fauna_mls::poll_inbound_conv`]) builds a tombstone
    /// inbound carrying only this reference, and the client renders the shared
    /// `legalTakedownTombstone(reference)` in place of the bubble body. `None`
    /// for every normal message.
    pub legal_takedown_ref: Option<String>,
    /// This record's account-data-plane identity, when the rail has one — see
    /// [`crate::message::MessageSnapshot::plane_ref`]. Set by the FaunaMls
    /// driver, which is the layer that holds all three inputs the derivation
    /// needs (channel, seq, the sealed envelope as the nest stored it); `None`
    /// on every rail that is not on the plane.
    pub plane_ref: Option<crate::message::PlaneRef>,
}

/// What the manager extracts from `RailInboundMessage` to route it.
#[derive(Clone, Debug)]
pub struct InboundBucket {
    pub rail: Rail,
    pub participants: Vec<TypedAddress>,
    pub subject: Option<String>,
    pub in_reply_to: Option<MessageId>,
    pub message: crate::message::MessageSnapshot,
}

/// The `RailInboundMessage` -> `InboundBucket` shape every real backend's
/// [`RailBackend::bucket_inbound`] delegates to (`backends::{smtp,bridged,
/// fauna_mls}::bucket_inbound`, each doc-commented as a mirror of the
/// others) — a pure transform once the driver has already decoded (and, for
/// FaunaMls, MLS-decrypted) the message. `subject`/`is_own` are the two
/// fields backends genuinely diverge on: SMTP/FaunaMls carry a subject line;
/// FaunaMls resolves a multi-device self-echo (its own `self_actor`,
/// authenticated by the MLS ratchet) and SMTP resolves the record's **mailbox
/// provenance** — `Some(MailFeed::Sent)`, never the forgeable `From:` header
/// (`backends::smtp::SmtpBackend::bucket_inbound`). Both are computed by the
/// caller, next to its own rail-specific reasoning for the value, before
/// `msg` moves in here.
pub(crate) fn bucket_inbound_common(
    msg: RailInboundMessage,
    subject: Option<String>,
    is_own: bool,
) -> InboundBucket {
    // Computed before `msg.body` is moved below — the complete render
    // document (body + attachment blocks; render-model.md § D1/D2).
    let document =
        crate::message::document_for_message(&msg.body, msg.body_format, &msg.attachments);
    InboundBucket {
        rail: msg.rail,
        participants: {
            let mut v = msg.recipients.clone();
            v.push(msg.sender.clone());
            v.sort_by_key(|a| a.display());
            v.dedup_by_key(|a| a.display());
            v
        },
        subject,
        in_reply_to: msg.in_reply_to.clone(),
        message: crate::message::MessageSnapshot {
            message_id: msg.message_id,
            sender: msg.sender,
            sender_display: String::new(),
            body: msg.body,
            document,
            timestamp_ms: msg.timestamp_ms,
            subject_line: None, // manager fills in
            badges: msg.badges,
            reply_to: msg.in_reply_to,
            reactions: vec![],
            deleted: false,
            is_own,
            legal_takedown_ref: msg.legal_takedown_ref,
            labels: vec![],
            plane_ref: msg.plane_ref,
            can_delete: false,
        },
    }
}

/// A rail's answer to [`RailBackend::resolve_address`].
///
/// The manager probes rails in order (FaunaMls first) and the variants mean:
/// `Resolved` — this rail claims the address, stop; `NotFound` / `Pending` —
/// not this rail's (or not confirmable as its own), ask the next rail and
/// finally the format-only parse; **`Error` — this rail recognises the address
/// as its own but could not confirm it right now, and that is the answer: the
/// chain stops, no later rail may claim the string, and the picker lands in
/// `error`.** `Error` is terminal so that a Fauna peer that does not answer can
/// never be re-read by the SMTP rail as "just an email address" and sent in
/// the clear — the collapse `docs/goal/architecture/federation.md` § Peer-auth
/// model → *Discovery-failure semantics* rules out (ratified 2026-08-29). A rail
/// that merely does not recognise the shape must answer `NotFound`, never
/// `Error`.
#[derive(Clone, Debug, PartialEq)]
pub enum ResolveResult {
    Resolved(TypedAddress),
    Pending,
    NotFound,
    Error(String),
}

/// A handle resolved to its actor on a nest — the seam-level result of
/// `fauna.actor.by_handle` ([`ConversationsRpc::actor_by_handle`]). Kept
/// protocol-agnostic (hex id + the nest's handle domain, no `fauna-protocol`
/// types cross the seam). The `domain` lets the backend confirm a typed
/// `localpart@domain` actually targets this nest before promoting it to a Fauna
/// address — distinguishing it from an email with the same localpart.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedHandle {
    /// 64-char hex of the resolved 32-byte actor public key.
    pub actor_id_hex: String,
    /// The domain the **answering nest named for itself** in its reply.
    ///
    /// ⚠ This is the answerer's own assertion, never a verified fact, which is
    /// why the field is not called `domain`: a nest serving `attacker.test` may
    /// echo `trusted.test`, and nothing in the reply binds the two. It is
    /// trusted on the **same-nest** arm ([`ConversationsRpc::actor_by_handle`])
    /// only — there the answerer is this account's own home nest, and the echo
    /// is precisely how the client learns its own live handle domain
    /// (`docs/goal/behavior/mail-multidomain.md` § Resolution and login report
    /// the live identity domain).
    ///
    /// On the **foreign** arm ([`ConversationsRpc::actor_by_handle_remote`]) it
    /// is *not* read for identity: the dialed domain — the one authenticated
    /// TLS bound to the nest that answered — names the peer
    /// (`docs/goal/architecture/federation.md` § Peer-auth model →
    /// *Discovery-failure semantics*, **The dial names the peer**).
    pub echoed_domain: String,
    /// Whether the resolved actor has ≥1 usable key package (one-time or
    /// last-resort), i.e. is reachable for MLS — folded from the
    /// `fauna.actor.by_handle` reply's `addressable` boolean
    /// (`docs/goal/architecture/federation.md` § Key packages). The cross-nest
    /// resolve path keys reachability on this (a foreign client cannot run a
    /// `keypackage.count` probe; same-nest resolution still uses `keypackage_count`).
    pub addressable: bool,
}

#[derive(Clone, Debug)]
pub struct SendOutcome {
    pub message_id: MessageId,
    pub timestamp_ms: i64,
    /// The local user's address on this rail — the sender of the message just
    /// sent. The manager stamps this onto the "Sent" copy it appends to the
    /// thread (the backend knows the local identity; the manager does not).
    pub sender: TypedAddress,
    /// The plane identity of the record the nest just sequenced, when the rail
    /// has one — see [`crate::message::MessageSnapshot::plane_ref`].
    ///
    /// A sender's own copy needs this as much as a received one: `conv` is a
    /// **member** scope, so every record on it is browse content under T1's
    /// classification regardless of who wrote it, and a sender can never
    /// re-derive the ref later (it cannot MLS-decrypt its own application
    /// messages, so the record never comes back through the inbound poll).
    pub plane_ref: Option<crate::message::PlaneRef>,
    /// Where each attachment this send put on the wire rests, keyed by its
    /// `blob_hash`, on a rail whose bytes rest where they can be fetched again.
    /// The manager remembers them beside the Sent copy (`conversations.md`
    /// § Attachments → *Retention*), and the sender's own attachments get
    /// coordinates from nowhere else: a sender never walks its own record back
    /// — the end-to-end class cannot open it, and the community class's poll
    /// skips a record whose id the Sent copy already holds.
    pub attachment_coordinates: Vec<(String, crate::store::AttachmentCoordinates)>,
}

/// A compose attachment with its plaintext bytes, resolved by the manager just
/// before [`RailBackend::send`] from the light `ComposeState.attachments` drafts
/// and the manager's attachment store (`add_attachment` cached the bytes under
/// `blob_hash`). Carried as a separate `send` arg rather than on `ComposeState`
/// so the **observed snapshot stays byte-free** (a multi-MB staged image never
/// crosses the UniFFI snapshot boundary). The SMTP rail inlines these as
/// `multipart/mixed` parts; rails whose blobs live on the nest (FaunaMls) seal
/// then upload them instead (follow-on) — every other rail ignores the slice
/// (`docs/goal/ui/conversations.md` § Attachments).
#[derive(Clone, Debug)]
pub struct ResolvedAttachment {
    pub blob_hash: String,
    pub filename: String,
    pub mime_type: String,
    pub is_image: bool,
    pub bytes: Vec<u8>,
}

/// The live self-address cell (`docs/goal/ui/conversations.md` § State & data
/// shape → *Self-address: live, never baked*): the logged-in account's canonical
/// `<handle>@<domain>`, shared by every rail backend of one session/manager
/// wiring and read **at use time** — the SMTP send-time `From:` (+ its
/// unresolved-address refusal), the FaunaMls routing-time domain comparison, the
/// reply-all self-drop — so a late-resolving or renamed handle heals every rail
/// through one setter with no backend rebuilt. Empty, or missing either half,
/// means "unresolved": sends refuse locally (`error.email.no_handle`), never a
/// synthesized placeholder.
///
/// Construction never waits for the address (identity resolution must not delay
/// MLS/DM delivery — user ruling 1, 2026-07-23); the owner
/// (`ConversationsSession` natively, the wasm wrapper on web) exposes the one
/// setter clients call from wherever identity state lands.
pub struct SelfAddress {
    address: std::sync::RwLock<String>,
}

impl SelfAddress {
    pub fn new(initial: impl Into<String>) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            address: std::sync::RwLock::new(initial.into()),
        })
    }

    /// The current address (possibly empty / partial while unresolved).
    pub fn get(&self) -> String {
        self.address.read().unwrap().clone()
    }

    /// Replace the address — login-time resolution, the background identity
    /// refresh, or a server-side handle rename. Takes effect on the next read;
    /// in-flight operations keep the value they already read.
    pub fn set(&self, address: impl Into<String>) {
        *self.address.write().unwrap() = address.into();
    }

    /// The domain half (after the last `@`), or empty while unresolved/bare.
    pub fn domain(&self) -> String {
        self.get()
            .rsplit_once('@')
            .map(|(_, d)| d.to_string())
            .unwrap_or_default()
    }

    /// Whether `address` is a usable `<local>@<domain>` — BOTH halves
    /// non-empty. `""`, a bare `"alice"`, and `"@nest.example"` (the shape a
    /// client synthesizes from an unresolved handle plus a URL host) all count
    /// as unresolved for the local-refusal floor (`conversations.md` § Errors &
    /// edge cases). Associated (not a method) so a caller that already read the
    /// cell once can judge that same snapshot, not a racing re-read.
    pub fn usable(address: &str) -> bool {
        matches!(address.rsplit_once('@'), Some((local, domain)) if !local.is_empty() && !domain.is_empty())
    }

    /// [`Self::usable`] over the current value.
    pub fn is_sendable(&self) -> bool {
        Self::usable(&self.get())
    }
}

/// Transport seam for outbound mail submission.
///
/// `SmtpBackend` assembles the RFC 5322 message in shared Rust (see
/// [`crate::rfc5322`]) and hands the bytes to a platform-provided sink that
/// performs the actual `fauna.email.send` WS-RPC call. Keeping the transport
/// behind this object-safe trait lets the conversations crate stay
/// dependency-light (no `fauna-protocol` / `fauna-client-email`) and sidesteps
/// the `RpcRequester` async-fn-in-trait `Send`-future mismatch: native glue
/// implements this over `fauna_client_email::EmailClient<Arc<NestClient>>`
/// (whose futures are `Send`), while the wasm SPA's implementation awaits the
/// rust-area `RpcRequester`/`Send` unification before it can register a real
/// `SmtpBackend` (a tracked follow-on).
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait OutboundMailSink: MaybeSendSync {
    /// Submit a fully-assembled RFC 5322 message to the given envelope
    /// recipients via `fauna.email.send`. Implementations return `Err` with a
    /// human-readable reason on transport failure or partial delivery
    /// (`SendEmailReply.remote_errors` non-empty).
    async fn submit(&self, recipients: Vec<String>, raw_rfc5322: Vec<u8>) -> Result<(), String>;

    /// The account's own mailing lists and today's per-account list meter
    /// (`fauna.bridges.list_account_lists`) — what lets a compose addressed to
    /// a list send as a list send (`mail-mass-mailing.md` § Composing a list
    /// message). Default: no lists, for a sink with no list rail.
    async fn own_lists(&self) -> Result<crate::list_send::OwnMailLists, String> {
        Ok(crate::list_send::OwnMailLists::default())
    }

    /// Send a composed RFC 5322 message to one of the account's own lists
    /// (`fauna.bridges.send_list_message` — the nest fans it out, one copy per
    /// subscribed member). Default: refused, for a sink with no list rail.
    async fn submit_to_list(
        &self,
        _list_id_hex: &str,
        _raw_rfc5322: Vec<u8>,
    ) -> Result<(), String> {
        Err("this app cannot send to a mailing list".to_string())
    }

    /// The newest send to a list, as a whole
    /// (`fauna.bridges.list_list_send_history`). Default: none.
    async fn latest_list_send(
        &self,
        _list_id_hex: &str,
    ) -> Result<Option<crate::list_send::ListSendProgress>, String> {
        Ok(None)
    }
}

/// One of the two server-side mailboxes the mail receive path reads
/// (`docs/goal/behavior/smtp-server.md` § Inbound client receive): `INBOX`
/// (`fauna.email.inbox.fetch`) and `Sent` (`fauna.email.sent.fetch`). Each
/// numbers its UIDs independently, so a UID names a record only beside its
/// mailbox. Serialized lowercase — the spelling the web receive loop passes
/// across the wasm boundary.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum MailFeed {
    Inbox,
    Sent,
}

/// One decrypted inbound mail record, as handed back by an [`InboundMailSource`].
///
/// The source has already done the WS-RPC `fauna.email.inbox.fetch` and the
/// two-layer decrypt via the shared `fauna_mail::open_inbound_record` helper
/// (decode the outer `fauna_mail::segments::MailRecordEnvelope`, open its inner
/// `.encrypted_body` under the recipient's MSEK-derived secret), so `rfc5322` is
/// the original plaintext message. Keeping
/// the crypto behind the seam lets `fauna-conversations` stay dependency-light
/// (no `fauna-mls` / `fauna-mail`), mirroring how [`OutboundMailSink`] keeps the
/// WS-RPC call out of this crate.
#[derive(Clone, Debug)]
pub struct InboundMailRecord {
    /// The record's UID in [`Self::mailbox`] — monotonic per mailbox; the
    /// paging cursor.
    pub uid: u32,
    /// Which server-side mailbox the record was read from — the other half of
    /// its identity, since INBOX and Sent number their UIDs independently. The
    /// receive path records `(mailbox, uid)` as the attachment store's SMTP
    /// coordinates, so an evicted attachment is re-read from the right feed
    /// ([`InboundMailSource::fetch_one`]).
    pub mailbox: MailFeed,
    /// The server-assigned segment-record id (the reliable dedup key — the
    /// RFC `Message-ID` header may be absent or duplicated across senders).
    pub message_id: Vec<u8>,
    /// Delivery time in **milliseconds** (the source converts the feed's
    /// epoch-seconds `internal_date`); used as the message timestamp.
    pub internal_date_ms: i64,
    /// The decrypted original RFC 5322 message bytes.
    pub rfc5322: Vec<u8>,
    /// `true` when the source's on-device spam scorer classified this message
    /// as spam this pass — it is moving INBOX→Junk (the source flushes the
    /// `apply_spam_disposition` in [`InboundMailSource::end_pass`]), so
    /// [`crate::backends::smtp::poll_inbound_mail`] advances the cursor + dedups
    /// it but does **not** append it to the thread view (the native twin of the
    /// wasm `WasmConversationsManager::ingest_sealed_inbound` early-return on a
    /// junk verdict, and of the MDA moving spam out of INBOX *before* the
    /// `SELECT` snapshot — `docs/goal/behavior/mail-spam.md` § Re-file timing).
    /// A source with no scorer (Sent feed, cold-start / mail-off) always sets
    /// `false`.
    pub suppress_from_view: bool,
    /// Whether the record's feed flag set carries IMAP `\Seen`
    /// ([`carries_seen_flag`]) — the mail rail's read marker
    /// (`docs/goal/behavior/conversation-read-state.md` § Mail: `\Seen` is the
    /// marker). An `INBOX` message that is not own and lacks it enters the
    /// thread's unread set at ingest, whatever its date; one carrying it never
    /// does. Meaningless on a `Sent` record, whose messages are own.
    pub has_seen_flag: bool,
}

/// IMAP's read flag, as the mail feeds spell it in a record's flag set.
pub const SEEN_FLAG: &str = "\\Seen";

/// Whether a feed flag set carries [`SEEN_FLAG`]. System flags are
/// case-insensitive (RFC 9051 § 2.3.2), so any spelling counts.
pub fn carries_seen_flag(flags: &[String]) -> bool {
    flags.iter().any(|f| f.eq_ignore_ascii_case(SEEN_FLAG))
}

/// A record on a page the source **could not open** — the seal is addressed to
/// no generation of the client's complete standing key set (a rotation past the
/// grace window, a mailbox torn down and re-enabled under a fresh MSEK) or is
/// tampered/malformed. Deterministic for this key set, so the page carries it
/// as a *skip*, not a failure: [`crate::backends::smtp::poll_inbound_mail`]
/// advances the cursor past it exactly as for an opened record, keeps
/// receiving, and records it on the manager
/// ([`crate::manager::ConversationsManager::note_unopenable_mail`]) so the
/// user is told (`ui/conversations.md` § Errors & edge cases). A failure to
/// *fetch* a record's bytes is not this — that stays a page error, retried
/// next tick (`mail-app-surface.md` § Inbound client receive → *Unopenable
/// records*).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkippedMailRecord {
    pub mailbox: MailFeed,
    pub uid: u32,
    /// The opener's own error text, for the log line — never shown to the user.
    pub reason: String,
}

/// One page of decrypted inbound mail (`fauna.email.inbox.fetch` semantics).
#[derive(Clone, Debug, Default)]
pub struct InboundMailPage {
    pub records: Vec<InboundMailRecord>,
    /// `true` when more pages remain past this one; re-fetch with
    /// `after_uid` = the last record's `uid`.
    pub more: bool,
    /// The page's records that did not open (see [`SkippedMailRecord`]); the
    /// cursor advances over these too. Empty on every ordinary page.
    pub skipped: Vec<SkippedMailRecord>,
    /// The mailbox's highest modseq when the page was built — the baseline
    /// the flag-change cursor starts from, taken from the first page of the
    /// launch drain (`mail-app-surface.md` § Read state). `0` from every source
    /// that does not read `INBOX`, and for an empty mailbox.
    pub highest_modseq: u64,
}

/// One `INBOX` message whose flag set changed, as
/// [`InboundMailSource::flag_changes`] delivered it — reduced to the one flag
/// the conversations store reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MailFlagChange {
    pub uid: u32,
    pub modseq: u64,
    /// Whether the message's whole current flag set carries `\Seen`.
    pub has_seen_flag: bool,
}

/// One page of `fauna.email.inbox.flag_changes`: the changes past the cursor
/// in `(modseq, uid)` order, the mailbox's `highest_modseq` read under the same
/// lock, and whether more pages remain (`mail-app-surface.md` § Read state).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MailFlagChangesPage {
    pub changes: Vec<MailFlagChange>,
    pub highest_modseq: u64,
    pub more: bool,
}

/// Why a mail read-state call ([`InboundMailSource::mark_seen`],
/// [`InboundMailSource::flag_changes`]) did not complete.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MailFlagCallError {
    /// The source does not serve the kind (only the `INBOX` source does).
    /// Permanent for the run: the app keeps reads in memory and syncs
    /// nothing, with no error shown.
    Unsupported,
    /// Anything else — transport down, a refusal worth retrying. The caller
    /// keeps what it was sending and tries again on a later sweep.
    Failed(String),
}

/// Transport + decrypt seam for the inbound mail feed (the receive twin of
/// [`OutboundMailSink`]).
///
/// A platform-provided source performs the `fauna.email.inbox.fetch` WS-RPC
/// (`docs/goal/behavior/smtp-server.md` § Inbound client receive) and decrypts
/// each sealed record client-side, returning plaintext RFC 5322. The shared
/// driver [`crate::backends::smtp::poll_inbound_mail`] consumes it — parsing +
/// bucketing + `ConversationsManager::ingest_inbound` stay in shared Rust. Same
/// `RpcRequester`-`Send`-future rationale as `OutboundMailSink`: native glue
/// implements this over `fauna_client_email::EmailClient<Arc<NestClient>>` (whose
/// futures are `Send`); the wasm SPA awaits the rust-area `Send` unification.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait InboundMailSource: MaybeSendSync {
    /// Fetch one page of decrypted INBOX messages with `uid > after_uid`.
    /// `limit` is the max records per page (`0` = source default). Implementations
    /// return `Err` with a human-readable reason on transport / decrypt failure.
    async fn fetch(&self, after_uid: u32, limit: u32) -> Result<InboundMailPage, String>;

    /// Re-read the ONE record at `uid`, or `Ok(None)` when this mailbox no
    /// longer holds a record at that UID — moved INBOX→Junk, or expunged: a
    /// genuine *gone*, never a transport failure (that is `Err`). What the
    /// attachment store's mail refill asks
    /// ([`crate::backends::smtp::refill_evicted_mail_attachments`]): an evicted
    /// attachment's bytes still rest in its record's MIME.
    ///
    /// The default rides [`Self::fetch`] — one record after `uid - 1` is the
    /// record at `uid` when it still exists and a later one when it does not —
    /// so the seam needs no wire kind of its own. A source whose `fetch` does
    /// per-delivery work (the INBOX spam scorer, the iTIP REPLY route)
    /// overrides it: a re-read is not a delivery.
    async fn fetch_one(&self, uid: u32) -> Result<Option<InboundMailRecord>, String> {
        let Some(after_uid) = uid.checked_sub(1) else {
            return Ok(None);
        };
        let page = self.fetch(after_uid, 1).await?;
        Ok(page.records.into_iter().find(|r| r.uid == uid))
    }

    /// Called once by [`crate::backends::smtp::poll_inbound_mail`] **before** the
    /// page loop of one drain pass. The default is a no-op; a source with a
    /// per-pass batch step (the INBOX on-device spam scorer) refreshes it here —
    /// the native twin of the web loop's `prepareSpamScoring`, which fetches the
    /// sealed per-user model + effective policy and builds the pass's scorer
    /// (`docs/goal/behavior/mail-spam.md` § Scoring placement). Best-effort:
    /// a failure must leave the source able to `fetch` (it just scores nothing
    /// this pass), never abort the receive loop.
    async fn begin_pass(&self) {}

    /// Called once by [`crate::backends::smtp::poll_inbound_mail`] **after** the
    /// page loop (always, even if a `fetch` errored mid-pass), so a per-pass batch
    /// step flushes exactly once. The INBOX scorer drains its accumulated
    /// `(scored_uids, junk_uids)` here into one `apply_spam_disposition`
    /// (watermark + move the junk subset INBOX→Junk) — the native twin of the web
    /// loop's `flushSpamScoring`. Default no-op; best-effort.
    async fn end_pass(&self) {}

    /// `fauna.email.inbox.mark_seen` — set `\Seen` on each named `INBOX` UID
    /// (`mail-app-surface.md` § Read state). Idempotent, and an unknown UID is
    /// skipped by the nest, so a retry after a failure is always safe. The
    /// default answers [`MailFlagCallError::Unsupported`]: only the `INBOX`
    /// source serves it.
    async fn mark_seen(&self, _uids: Vec<u32>) -> Result<(), MailFlagCallError> {
        Err(MailFlagCallError::Unsupported)
    }

    /// `fauna.email.inbox.flag_changes` — one page of `INBOX` flag changes past
    /// the cursor `(since_modseq, after_uid)`; `limit` `0` is the nest's
    /// default. Default [`MailFlagCallError::Unsupported`], like
    /// [`Self::mark_seen`].
    async fn flag_changes(
        &self,
        _since_modseq: u64,
        _after_uid: u32,
        _limit: u32,
    ) -> Result<MailFlagChangesPage, MailFlagCallError> {
        Err(MailFlagCallError::Unsupported)
    }
}

/// One bridged message, already opened, as the [`BridgedSource`] hands it back
/// — the bridged family's twin of [`InboundMailRecord`] (`ui/conversations.md`
/// § Where logic lives → *The `Bridged` adapter*, the user-side contract).
///
/// The nest stores every row sealed to the user's own recipient key — inbound
/// deposits sealed there by the bridge, the user's own Sent copies sealed there
/// by the app — and the seam impl (client glue, which holds the recipient
/// secret) opens it before handing it across, so this crate takes no HPKE
/// dependency.
#[derive(Clone, Debug)]
pub struct BridgedRecord {
    /// The nest's monotonic row id — the receive cursor and the dedup key.
    pub id: i64,
    /// The bridge that carried it — the manifest's `bridge.id`.
    pub bridge_id: String,
    /// Who sent it, in the far network's own spelling.
    pub sender: String,
    /// Everyone else it went to, in the far network's own spelling.
    pub recipients: Vec<String>,
    /// The opened message text.
    pub body: String,
    /// `true` for the user's own Sent copy, `false` for an inbound deposit.
    pub outbound: bool,
    /// The nest's `received_at`, in milliseconds — never the far side's
    /// claimed timestamp (`ui/nostr.md` § Implementation status today, the
    /// lesson the family's ordering rule carries).
    pub timestamp_ms: i64,
}

/// What the nest answered for a typed address — the bridge whose declared
/// grammar admits it, and the far spelling it admitted (`ui/conversations.md`
/// § Where logic lives → *The `Bridged` adapter*, ruling 2 (d)).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BridgedResolved {
    pub bridge_id: String,
    pub address: String,
}

/// One outbound bridged message for the glue to seal and submit — the user-side
/// `fauna.bridges.conversation.send` (`architecture/apps/bridges.md` § Bridge-kind
/// catalogue → Phase G). The glue seals `body` twice — to `bridge_x25519`
/// (`sealed_for_bridge`, the item the bridge drains) and to the user's own
/// recipient key (`sealed_for_self`, the Sent row) — finds or mints the room
/// for `(bridge_id, peers)` (`conversation.rooms.open`, idempotent), and sends.
#[derive(Clone, Debug)]
pub struct BridgedOutbound {
    pub bridge_id: String,
    /// The far-network addresses the message goes to — the thread's
    /// participants on this bridge, the user's own excluded.
    pub peers: Vec<String>,
    pub body: String,
    /// The bridge principal's X25519 public key, from the registry the glue
    /// filled off `conversation.rooms.list`.
    pub bridge_x25519: [u8; 32],
}

/// What the nest recorded for a [`BridgedOutbound`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BridgedSent {
    /// The Sent row's id — the same [`BridgedRecord::id`] the inbox later
    /// serves it under, so the send and its echo derive one [`MessageId`]
    /// (`backends::bridged::bridged_message_id`).
    pub row_id: i64,
    /// The account's own address on the far network, as the bridge reports it —
    /// the sender stamped on the Sent copy.
    pub self_address: String,
}

/// Transport seam for the bridged family's user-side calls — the outbound half
/// (`resolve`, `send`); [`BridgedSource`] is the inbound twin. Together they
/// are what [`OutboundMailSink`] / [`InboundMailSource`] are for mail:
/// [`crate::backends::bridged::BridgedBackend`] keeps no wire, HPKE or regex
/// dependency, and the glue performs the `fauna.bridges.conversation.*` calls.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait BridgedSink: MaybeSendSync {
    /// Ask the nest which bridge's declared grammar admits `raw` — `Ok(None)`
    /// when none does. The grammar is matched nest-side only; no app compiles a
    /// third party's pattern (`ui/conversations.md` § Where logic lives → *The
    /// `Bridged` adapter*, ruling 2 (d)).
    async fn resolve(&self, raw: String) -> Result<Option<BridgedResolved>, String>;

    /// Seal and submit one outbound message (see [`BridgedOutbound`]).
    async fn send(&self, outbound: BridgedOutbound) -> Result<BridgedSent, String>;
}

/// Transport seam for the bridged inbox — `fauna.bridges.conversation.rooms.list`
/// and `fauna.bridges.conversation.inbox.fetch` over every room, opened (see
/// [`BridgedRecord`]). The shared driver
/// [`crate::backends::bridged::poll_inbound_bridged`] consumes it.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait BridgedSource: MaybeSendSync {
    /// The account's bridged rooms across every bridge serving it — what the
    /// driver loads into the backend before each read
    /// ([`crate::backends::bridged::BridgedBackend::set_rooms`]): the bridge
    /// identities a thread paints and the family gate's marker per room.
    async fn rooms(&self) -> Result<Vec<crate::backends::bridged::BridgedRoomRecord>, String>;

    /// Rows with `id > after_id`, oldest first; `limit` is the page size (`0` =
    /// the source's default).
    async fn fetch(&self, after_id: i64, limit: u32) -> Result<Vec<BridgedRecord>, String>;
}

/// Which kind of MLS Welcome the backend is delivering, in the
/// **protocol-agnostic** terms this seam speaks (the native/wasm glue maps it
/// onto the wire `fauna_protocol::conversations::WelcomeKind`, mirroring it
/// variant-for-variant). Replaces the older `(is_group, group_id_hex)` pair so
/// the third, orthogonal scheduling case has a home that isn't a boolean
/// overload.
#[derive(Clone, Debug, PartialEq)]
pub enum WelcomeChannelKind {
    /// A 1:1 chat DM (no group id).
    Dm,
    /// An n-way chat group, carrying the hex-encoded 32-byte group id.
    Group {
        /// Hex of the MLS group id (empty if the engine couldn't resolve it,
        /// matching the legacy `group_id_hex.unwrap_or_default()` behaviour).
        group_id_hex: String,
    },
    /// A one-off channel carrying a CalDAV scheduling iMIP to a **mailbox-less**
    /// Fauna attendee (`docs/goal/behavior/caldav-server.md` § Server-side
    /// auto-schedule). Like [`Dm`](Self::Dm) it carries no group id (the channel
    /// is a 1:1 organizer→attendee delivery); the tag tells the recipient's
    /// receive loop to route the channel's application messages to the
    /// calendar-apply path, never the chat UI.
    Scheduling,
    /// A Welcome admitting the recipient to a **cross-user shared folder**'s
    /// MLS group (`docs/goal/ui/folders.md` § Sharing a folder), carrying the
    /// hex group id like [`Group`](Self::Group). Mirrors the wire
    /// `WelcomeKind::Folder`. The tag tells the recipient's receive loop to route
    /// this Welcome to the **folder pending-share surface**, never the chat UI —
    /// a shared folder is not a conversation, so a `Group` welcome here would
    /// surface a phantom chat thread. The recipient contact-status gate +
    /// Welcome-staging (auto vs knock) is a separate slice; this variant only
    /// keeps the routing off the chat rail.
    Folder {
        /// Hex of the MLS group id (empty if the engine couldn't resolve it,
        /// matching the [`Group`](Self::Group) convention).
        group_id_hex: String,
    },
}

/// One record of a channel's log as [`ConversationsRpc::channel_fetch`] returns
/// it — the protocol-agnostic image of the wire `ChannelFetchEntry` (this
/// crate names no `fauna-protocol` type, priority #2).
///
/// A named record rather than a tuple so a field the wire grows reaches the
/// receive walk without touching every seam implementation: construct it with
/// `..Default::default()`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FetchedRecord {
    pub seq: i64,
    /// The sealed envelope — empty when [`Self::legal_takedown_ref`] is `Some`.
    pub envelope: Vec<u8>,
    /// The **legal-takedown reference** (`ChannelFetchEntry.legal_takedown`):
    /// `Some` iff the nest withheld this record's envelope under a legal
    /// obligation, so the walk renders the tombstone in its place instead of
    /// decrypting an empty envelope (`moderation.md` § Categories & enforcement
    /// item 1). `None` for every normal message.
    pub legal_takedown_ref: Option<String>,
    /// A **community room's** category verdicts for this message, from the
    /// labelers the room names (`conversation-rooms.md` § The three classes →
    /// *What the home nest does with its read*, purpose 2). The nest serves
    /// them only to a live floor member; empty for every other class and
    /// caller. The walk merges them into the message's own labels, a server
    /// verdict winning its category
    /// (`fauna_core::content_category::merge_server_labels`).
    pub labels: Vec<fauna_core::content_category::ContentLabelEntry>,
    /// The record's **nest-attested author** (`ChannelFetchEntry.author`):
    /// 64-hex id of the actor the channel's home nest authenticated as its
    /// poster. `None` when the home nest reports no author for the record —
    /// *no answer*, never permission. Only as honest as the home nest,
    /// so a consumer binds it with the channel's home ([`SchedulingOrigin`]).
    pub author: Option<String>,
}

/// Where one inbound scheduling iMIP came from, as far as anything the sender
/// does not control can say — the pair the inbound-mutation rule binds an event
/// to (`docs/goal/behavior/caldav-server.md` § Who may mutate an existing event
/// over the inbound rail). Deliberately NOT the MLS sender: the MDA gateway
/// signs each delivery with a throwaway identity, so that credential names
/// nobody. Plain strings, so this seam stays crypto- and calendar-free.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SchedulingOrigin {
    /// [`FetchedRecord::author`] of the record that carried the iMIP.
    pub author: Option<String>,
    /// The channel's home-nest base URL as this client's **own nest** stamped
    /// it from the handshake-verified federation peer — never a sender-declared
    /// value. Empty for a channel homed on the recipient's own nest.
    pub home_nest_url: String,
}

/// Protocol-agnostic classification of a [`ConversationsRpc`] seam failure — the
/// conversations-rail counterpart of `fauna_protocol::RpcErrorAction`, which this
/// crate deliberately cannot name (no `fauna-protocol` dependency, priority #2).
/// The glue
/// (`fauna-client-conversations`, which *does* depend on `fauna-protocol`) builds
/// it from `RpcError::action()` + `RpcError::localized()`, so the
/// version-mismatch-vs-transient distinction (`version-compatibility.md`
/// Dimension 4) survives the seam instead of flattening to a raw string — the
/// named `backend.rs` leak that surfaced a server-internal string in the
/// conversations UI. `message` is already the user-facing rendered string
/// (localized for a recognised wire code, else the raw transport error), so the
/// UI never re-derives "is this retryable?" or re-renders the code.
#[derive(Debug, Clone)]
pub enum ConvRpcError {
    /// The nest is running an **outdated version** (`fauna.nest.outdated`,
    /// `RpcErrorAction::NeedsUpdate`) — route to a non-retry "update your nest"
    /// affordance, never an auto-retry. `message` is the localized actionable
    /// string.
    NeedsUpdate { message: String },
    /// A definite server **rejection** that won't change on retry
    /// (`RpcErrorAction::Rejected` — auth/permission/not-found/malformed, and any
    /// unrecognised wire code). Show `message`; do not auto-retry.
    Rejected { message: String },
    /// A **transient** transport/connectivity/server fault
    /// (`RpcErrorAction::Transient`, or a non-`RpcError` transport fault that
    /// never reached the nest). Safe to auto-retry. `message` is the localized
    /// string for a recognised transient code, else the raw transport error.
    Transient { message: String },
    /// The device-owned-epoch commit gate rejected a gate-send: a
    /// `ChannelEnvelope::Commit` landed on the channel after the caller's
    /// `expect_no_commit_since` seq, so the caller is committing from a stale
    /// epoch (`fauna.conversations.channel.stale`,
    /// `docs/goal/behavior/devices.md` § Cross-device MLS group-state sync). This
    /// is **not** a terminal rejection — it is the rebase signal: the commit
    /// author clears its pending commit, processes the intervening records, and
    /// retries with the new seq. `latest_commit_seq` is the nest's commit
    /// high-water mark parsed from the error details (best-effort — the rebase
    /// re-polls from its own processed cursor regardless, so a `None` here is
    /// harmless and only costs the diagnostic). Caught directly by the
    /// commit-send path; if it ever escapes uncaught it degrades to a retryable
    /// [`BackendError::Transport`](crate::backend::BackendError::Transport).
    StaleCommit { latest_commit_seq: Option<i64> },
}

impl ConvRpcError {
    /// A transport fault with no wire `RpcError` to classify — a connect failure,
    /// or an HTTP error on the content-addressed blob byte-source surface (which
    /// rides plain HTTP, not WS-RPC). Always retryable, so it maps to
    /// [`Self::Transient`]. The seam glue uses this where the underlying error
    /// isn't an `RpcErrorClass` (so `action()`/`localized()` don't apply).
    pub fn transient(message: impl Into<String>) -> Self {
        Self::Transient {
            message: message.into(),
        }
    }

    /// A failed attachment upload to a room homed on a **foreign** nest, whose
    /// own words must never be the sentence the user reads
    /// (`conversations.md` § Errors & edge cases → *who answered decides whether
    /// their words may reach the user*): that nest is chosen by whoever created
    /// the room, so its error body is text a stranger wrote. Retryable exactly
    /// like [`Self::transient`] — only the words differ — and the caller logs
    /// the responder's text beside its sealed cid.
    ///
    /// A named constructor here, in the crate that already renders, rather than
    /// the sentence being spelled at the seam: `fauna-client-conversations`
    /// classifies and deliberately renders nothing (it holds `fauna-i18n` as a
    /// dev-dependency only).
    pub fn foreign_attachment_upload_failed() -> Self {
        Self::Transient {
            message: fauna_i18n::strings::error::send::ATTACHMENT_UPLOAD_FOREIGN_FAILED.to_string(),
        }
    }
}

impl std::fmt::Display for ConvRpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NeedsUpdate { message }
            | Self::Rejected { message }
            | Self::Transient { message } => f.write_str(message),
            Self::StaleCommit { latest_commit_seq } => write!(
                f,
                "commit rejected: a newer commit landed (latest_commit_seq={latest_commit_seq:?})"
            ),
        }
    }
}

impl std::error::Error for ConvRpcError {}

/// The terminal outcome of a `fauna.linkpreview.resolve` call (render-model.md § D4),
/// in the **protocol-agnostic** shape this crate speaks (`fauna-conversations` carries
/// no `fauna-protocol` dependency — the seam glue maps the wire reply onto this, exactly
/// as it does for [`ResolvedHandle`] / the channel-fetch entries). The manager maps it
/// onto `fauna_core::render::PreviewState` and folds it into the matching bubble
/// `RenderBlock::LinkPreview` block. Mirrors `fauna_protocol::linkpreview::LinkPreviewResolveReply`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkPreviewResolution {
    /// Resolved OpenGraph/meta. `image_hash` is the (optional) content-addressed og:image
    /// the nest fetched + stored, served back through this client's own nest blob surface.
    Resolved {
        title: String,
        description: String,
        image_hash: Option<String>,
    },
    /// The fetch/parse failed, was SSRF/size/time-blocked, or the page had no usable
    /// metadata — the render model's terminal `Failed` (the card falls back to the inline link).
    Failed,
}

/// Nest-global seam for the user-facing `fauna.linkpreview.resolve` WS-RPC kind
/// (render-model.md § D4). Distinct from [`ConversationsRpc`] (channel / keypackage /
/// blob, the FaunaMls-rail RPC) because link-preview resolution is **rail-agnostic** — a
/// bubble on ANY rail (FaunaMls, mail, …) whose body is a bare URL carries a
/// `LinkPreview` block, and the user's **home nest** resolves it (it owns the
/// SSRF/size/time guards and the og:image blob), so [`ConversationsManager`] holds one
/// directly rather than reaching through a per-rail backend.
///
/// Object-safe + protocol-agnostic (raw `String` in, [`LinkPreviewResolution`] out) so
/// `fauna-conversations` gains no `fauna-protocol` dependency; the native + wasm glue
/// (`NestConversationsRpc` / `WsConversationsRpc`) back it over a `RpcRequester` via
/// `fauna_client_linkpreview::LinkPreviewClient`. Drives
/// [`ConversationsManager::resolve_link_preview`], the conversations twin of
/// `FeedManager::resolve_link_preview`.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait LinkPreviewRpc: MaybeSendSync {
    /// Resolve a bare URL's OpenGraph/meta preview. A transient/transport error returns
    /// `Err`; the manager collapses BOTH `Err` and `Ok(Failed)` to the render model's
    /// terminal `PreviewState::Failed` (a non-retried failure for render purposes, § D4).
    async fn link_preview_resolve(
        &self,
        url: String,
    ) -> Result<LinkPreviewResolution, ConvRpcError>;
}

/// Transport seam for the fauna-native MLS conversation rail — the multi-method
/// counterpart of [`OutboundMailSink`] for `FaunaMlsBackend`.
///
/// `FaunaMlsBackend` owns all MLS crypto (via `fauna-mls`) and drives the nest
/// through this object-safe, **protocol-agnostic** seam (raw bytes + hex ids),
/// so `fauna-conversations` gains no `fauna-protocol` dependency. Native glue
/// backs it over a `RpcRequester` (the typed `fauna.conversations.*` kinds and
/// their request/reply structs live in `fauna-protocol::conversations`); the
/// wasm SPA awaits the same `RpcRequester`/`Send`-future unification as
/// [`OutboundMailSink`].
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait ConversationsRpc: MaybeSendSync {
    /// `fauna.conversations.channel.send` — post one wire `ChannelEnvelope`
    /// (canonical dag-cbor) to a channel; returns the server-assigned sequence.
    ///
    /// `expect_no_commit_since` is the **device-owned-epoch commit gate**
    /// (`docs/goal/behavior/devices.md` § Cross-device MLS group-state sync): when
    /// `Some(seq)`, the nest rejects the send with
    /// [`ConvRpcError::StaleCommit`] if any `ChannelEnvelope::Commit` landed on the
    /// channel after `seq`, so a commit built from a stale epoch never appends.
    /// `None` is a blind (ungated) append — every application send and every
    /// pre-gate caller passes `None`; only an MLS commit built through the rebase
    /// discipline passes `Some(last_processed_seq)`. Additive on the wire (a
    /// nest without the gate ignores it), so bidirectional-compatible within the major
    /// version.
    ///
    /// `attachment_refs` is the **conversation kind's blob-reachability
    /// floor** (`docs/goal/architecture/encryption-at-rest.md` § Per-content-kind
    /// conformance → Conversation messages row, ratified 2026-09-08): the
    /// 64-hex content addresses of the sealed attachment blobs this envelope's
    /// sealed body names (`ChannelAttachment::sealed_cid`, already uploaded via
    /// [`Self::blob_put`]). The nest cannot open the body, so this list is the
    /// only thing on the box that names those blobs — without it the blob GC
    /// sweeps them ~30 minutes after upload. Empty for every send that carries
    /// no attachment (commits, reactions, deletes, group meta, …); the nest
    /// records it beside the record's mirror row and never serves it back.
    /// Additive on the wire (skipped when empty; a nest without the field ignores it).
    async fn channel_send(
        &self,
        channel_id_hex: String,
        envelope: Vec<u8>,
        expect_no_commit_since: Option<i64>,
        attachment_refs: Vec<String>,
    ) -> Result<i64, ConvRpcError>;

    /// `fauna.conversations.channel.send_remote` — the foreign-member send for
    /// a channel whose log lives on `home_nest_url` (the recorded Welcome
    /// `nest_url`): the caller's own nest relays the envelope to the channel's
    /// home nest via `fauna.federation.channel.append`. A distinct wire kind —
    /// never an additive field on `channel.send` — so an old, relay-unaware
    /// nest fails loud instead of silently appending to its own local log (the
    /// send blackhole; `direct-messages.md` § step 3b). Callers don't pick a
    /// method by hand: [`FaunaMlsBackend`] routes every send through
    /// `channel_home_url` — the same signal that drives the `channel_fetch`
    /// relay — so this is reached exactly when the channel is foreign-homed.
    /// `attachment_refs` rides through to the home nest exactly as on
    /// [`Self::channel_send`].
    async fn channel_send_remote(
        &self,
        channel_id_hex: String,
        home_nest_url: String,
        envelope: Vec<u8>,
        expect_no_commit_since: Option<i64>,
        attachment_refs: Vec<String>,
    ) -> Result<i64, ConvRpcError>;

    /// `fauna.conversations.channel.fetch` — the [`FetchedRecord`]s with
    /// `seq > after`, oldest first, capped at `limit`.
    ///
    /// `home_nest_url` is `Some(url)` when the channel's message log lives on a
    /// **foreign** nest (the group creator's nest): the caller's home nest
    /// originates `fauna.federation.channel.fetch` there and returns the ciphertext
    /// entries (`docs/goal/behavior/direct-messages.md` § Technical Flow —
    /// Cross-Nest, step 3; `docs/goal/architecture/federation.md` § Federation
    /// residue surface). `None` fetches from this (the caller's own) nest — the
    /// same-nest path.
    ///
    /// Unlike `keypackage_fetch` / `welcome_deliver`, whose `peer_domain` is derived
    /// from a peer *handle* and mapped to a URL by the seam, this is already a
    /// resolved base URL: the receiver learned the channel's home nest from the
    /// Welcome envelope's `nest_url` (a URL), so the seam passes it straight onto the
    /// wire `ChannelFetchRequest.nest_url`.
    async fn channel_fetch(
        &self,
        channel_id_hex: String,
        after: i64,
        limit: i64,
        home_nest_url: Option<String>,
    ) -> Result<Vec<FetchedRecord>, ConvRpcError>;

    /// `fauna.conversations.keypackage.count` — non-destructive remaining
    /// key-package count for a target actor.
    async fn keypackage_count(&self, actor_id_hex: String) -> Result<u64, ConvRpcError>;

    /// `fauna.conversations.channel.actors` — the hex actor ids on a channel's
    /// routing roster (`actor_channels`), which the nest writes at Welcome
    /// delivery. Its *absence* for an actor who is nonetheless an MLS leaf is
    /// how [`crate::backends::fauna_mls::FaunaMlsBackend::add_participant`]
    /// tells a healthy member from a post-crash **phantom leaf**
    /// (`mls-group-key-material.md` § M2 *Admitting a member*).
    ///
    /// **`Ok(None)` means "roster unreadable here", never "the roster is
    /// empty"** — and the default impl returns exactly that, so a seam without
    /// the read (a test double, the loopback doubles, a transport error, a
    /// foreign-homed channel whose roster is not readable here) stays compiling and, more importantly, *fails safe*:
    /// an unreadable roster refuses to guess, so the add path surfaces a clear
    /// error instead of evicting a member it cannot vouch for. Implementors
    /// should likewise degrade a transport error to `Ok(None)` rather than
    /// propagate it — the caller's fallback is already the honest one.
    ///
    /// `home_nest_url` is `Some(url)` when the channel is **foreign-homed**
    /// (the caller joined from a cross-nest Welcome; the authoritative roster
    /// union lives on that home nest): the impl relays the read there via the
    /// **distinct kind** `fauna.conversations.channel.actors_remote` — never
    /// an additive field on `channel.actors`, whose silent partial
    /// answer from a nest that ignores the field would feed the membership-mutating heal (`federation.md`
    /// § Cross-nest, the fetch-vs-actors dividing line). `None` = the
    /// same-nest `channel.actors`. Either way a nest that does not answer
    /// degrades to `Ok(None)`, the refuse arm.
    async fn channel_actors(
        &self,
        _channel_id_hex: String,
        _home_nest_url: Option<String>,
    ) -> Result<Option<Vec<String>>, ConvRpcError> {
        Ok(None)
    }

    /// `fauna.actor.by_handle` — resolve a bare handle (its localpart, no
    /// domain) to its actor on the logged-in nest. `Ok(None)` when the handle
    /// is unknown or malformed *here* (so the manager's resolution chain falls
    /// through to other rails); `Err` only on a transport / internal failure.
    /// Backs [`crate::backends::fauna_mls::FaunaMlsBackend::resolve_address`]'s
    /// handle→actor form. Same-nest only; cross-nest (federated) handle
    /// resolution is a later slice.
    async fn actor_by_handle(&self, handle: String)
    -> Result<Option<ResolvedHandle>, ConvRpcError>;

    /// `fauna.actor.by_handle` on a **foreign** nest — cross-nest (federated)
    /// discovery. `domain` is the peer's handle domain; the impl resolves it to
    /// the peer's base URL and runs anonymous discovery directly against that
    /// nest over TLS (`docs/goal/architecture/federation.md` § Peer-auth model —
    /// discovery is anonymous + TLS, no home-nest relay). The returned
    /// [`ResolvedHandle::addressable`] carries the peer's reachability boolean.
    ///
    /// **The outcome is structural, so the backend can tell an answer from a
    /// non-answer** (`docs/goal/architecture/federation.md` § Peer-auth model →
    /// *Discovery-failure semantics*): `Ok(Some)` — the nest answered with the
    /// actor; `Ok(None)` — the nest answered `fauna.actor.not_found` (the
    /// caller's chain falls through, e.g. to email); `Err(Rejected)` — the nest
    /// answered with another definite refusal (`domain_not_local`,
    /// `handle.invalid`, …); `Err(NeedsUpdate)` — a version-incompatible nest;
    /// `Err(Transient)` — **no usable answer**: DNS / connect / TLS / WS
    /// handshake failure, timeout, protocol fault, or a transient wire refusal
    /// such as a rate limit. An impl must never fold a wire `RpcError` into
    /// `Transient`'s raw-string arm — route it through the shared classifier so
    /// `not_found` reaches the backend as `Ok(None)`. Same-nest resolution
    /// stays on [`Self::actor_by_handle`].
    async fn actor_by_handle_remote(
        &self,
        domain: String,
        localpart: String,
    ) -> Result<Option<ResolvedHandle>, ConvRpcError>;

    /// `fauna.conversations.keypackage.fetch` — consume one TLS-serialized key
    /// package for a target actor (`None` when the queue is empty).
    ///
    /// `peer_domain` is `Some(domain)` when the target lives on a **foreign**
    /// nest: the home nest signs + relays the fetch to that peer (the request's
    /// `nest_url`, `docs/goal/architecture/federation.md` § Federation residue).
    /// `None` fetches from this nest.
    async fn keypackage_fetch(
        &self,
        actor_id_hex: String,
        peer_domain: Option<String>,
    ) -> Result<Option<Vec<u8>>, ConvRpcError>;

    /// `fauna.conversations.keypackage.upload` — store the local actor's own
    /// key packages; returns the count stored. `last_resort` mirrors the wire
    /// `KeypackageUploadRequest.last_resort`: `false` uploads consumable one-time
    /// pool entries (the [`ensure_keypackages`](crate::backends::fauna_mls::FaunaMlsBackend::ensure_keypackages)
    /// top-up); `true` publishes the single reusable last-resort key package the
    /// nest never consumes (the onboarding publication —
    /// [`ensure_last_resort_keypackage`](crate::backends::fauna_mls::FaunaMlsBackend::ensure_last_resort_keypackage),
    /// `docs/goal/architecture/federation.md` § Key packages — privacy &
    /// exhaustion).
    async fn keypackage_upload(
        &self,
        packages: Vec<Vec<u8>>,
        last_resort: bool,
    ) -> Result<u64, ConvRpcError>;

    /// `fauna.conversations.welcome.deliver` — push an MLS Welcome to a
    /// recipient (same-nest); the nest stores it via `push_inbox` and fires the
    /// `fauna.conversations.welcome.received` push.
    ///
    /// `peer_domain` is `Some(domain)` for a **foreign** recipient (relayed via
    /// the home nest using the request's `nest_url`), `None` for same-nest.
    /// `kind` selects the wire `WelcomeKind` the glue sends (DM / group /
    /// scheduling — [`WelcomeChannelKind`]).
    async fn welcome_deliver(
        &self,
        recipient_actor_id_hex: String,
        channel_id_hex: String,
        welcome_bytes: Vec<u8>,
        kind: WelcomeChannelKind,
        peer_domain: Option<String>,
    ) -> Result<(), ConvRpcError>;

    /// Store a sealed conversation-attachment blob on the channel's **home**
    /// nest under its content-address (`POST /api/v1/blob` — the canonical
    /// byte-source surface, `docs/goal/architecture/message-segment-store.md`
    /// § HTTP residue). `sealed_cid_hex` is the lowercase-hex BLAKE3 of `bytes`
    /// (the *sealed* bytes — the bytes the nest holds opaque). The blob seals
    /// client-side under the channel's `derive_blob_key(epoch_secret)` (the seal
    /// happens in [`crate::backends::fauna_mls::FaunaMlsBackend::send`] via the
    /// MLS engine), so the nest never holds an opening key — audience-scoping is
    /// implicit (`docs/goal/architecture/encryption-at-rest.md` Media row).
    ///
    /// **A room's attachment bytes rest on the room's home nest** — beside the
    /// record and the plaintext `attachment_refs` that pin them past the blob
    /// GC (`conversation-rooms.md` § The home nest → *Attachment bytes*,
    /// ratified 2026-09-09; `conversations.md` § Encryption at rest →
    /// *Attachment reachability*). `home_nest_url` is `Some(url)` when the
    /// channel's log lives on a **foreign** nest (the backend's recorded
    /// `ChannelHome`, the same signal that routes `channel.send_remote`), and
    /// the impl then POSTs DIRECT to that nest under a short-lived write token
    /// its own nest relays (`fauna.conversations.blob.write_token.get`); `None`
    /// is a same-nest channel, uploaded under the member's own session bearer.
    /// `channel_id_hex` is what the token is minted for. Idempotent —
    /// re-putting an existing blob is a no-op.
    async fn blob_put(
        &self,
        channel_id_hex: String,
        home_nest_url: Option<String>,
        sealed_cid_hex: String,
        bytes: Vec<u8>,
    ) -> Result<(), ConvRpcError>;

    /// Fetch a sealed conversation-attachment blob by its content-address from
    /// the channel's **home** nest (`GET /api/v1/blob/{sealed_cid_hex}` —
    /// public, no bearer; a foreign home is reached DIRECT, integrity resting on
    /// the content address exactly as a cross-nest shared-folder read does).
    /// `home_nest_url` carries the same `ChannelHome` signal as [`Self::blob_put`].
    /// `Ok(None)` when the blob is absent (not yet uploaded / GC'd), so the
    /// receive path skips that attachment rather than stalling the feed; `Err`
    /// on transport failure. The caller (`poll_inbound_conv`) opens the returned
    /// bytes with the MLS engine.
    async fn blob_get(
        &self,
        channel_id_hex: String,
        home_nest_url: Option<String>,
        sealed_cid_hex: String,
    ) -> Result<Option<Vec<u8>>, ConvRpcError>;
}

/// A decoded inbound conversations push event the receive loop reacts to — the
/// minimal protocol-agnostic shape [`ConversationsPush`] hands the loop, so
/// `fauna-conversations` gains no `fauna-client` / `fauna-protocol` dependency
/// for its push source. The typed `PushEvent` decode lives in the seam impl
/// (`fauna-client-conversations::NestConversationsPush`, alongside
/// [`ConversationsRpc`]'s `NestConversationsRpc`).
// `Welcome(WelcomeNudge)` is much larger than the bare `ChannelMessage`/mail
// wakes (which carry no payload). Boxing it would cascade `Box::new` through the
// construction sites and risk move-out-of-`Box` friction at the match arms for a
// negligible win: these are transient, low-volume receive-loop wakes, not a hot
// data structure held in bulk, so the "always allocate the large variant" cost
// is trivial. Kept unboxed on purpose.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug)]
pub enum ConvPushEvent {
    /// `fauna.conversations.welcome.received` — an MLS Welcome to join + bind a
    /// channel. The loop feeds it to `ingest_welcome`, then polls the now-bound
    /// channel for any history posted before the join.
    Welcome(WelcomeNudge),
    /// `fauna.conversations.channel.message` — new ciphertext on a bound channel.
    /// A bare "poll now" nudge (the loop re-polls every bound channel; the
    /// per-channel cursor dedups), carrying no payload.
    ChannelMessage,
    /// `fauna.mail.received` — a newly-placed inbound mail message for this actor
    /// (`docs/goal/behavior/smtp-server.md` § Inbound client receive → Arrival
    /// push). A bare "poll mail now" wake carrying no payload: the unified receive
    /// loop ([`crate::session::ConversationsSession::start_receive_loop`]) drives
    /// **both** the conv and mail rails, so the mail-arrival wake rides this one
    /// push seam alongside the conv events — on it the loop re-polls the `INBOX` +
    /// `Sent` read-feeds (the per-mailbox `after_uid` cursor dedups). Lets a
    /// delivered message render promptly instead of within one backstop-ticker
    /// cycle. The native twin of linux's `mail_sink.rs::start_inbound_poll`
    /// `subscribe_kind("fauna.mail.received")` arm.
    MailReceived,
    /// `fauna.mail.flags_changed` — a flag on one of this actor's `INBOX`
    /// messages changed, from another Fauna device or a mail client
    /// (`mail-app-surface.md` § Read state). A bare wake like
    /// [`Self::MailReceived`]: the loop answers it with one cursored
    /// `flag_changes` drain ([`crate::backends::smtp::sync_mail_read_state`]),
    /// and the periodic sweep backstops a missed one.
    MailFlagsChanged,
    /// `fauna.bridges.push.conversation_changed` — a bridged room changed: a
    /// deposit, a room report or a receipt (`architecture/apps/bridges.md`
    /// § Bridge-kind catalogue → Phase G). A bare wake like
    /// [`Self::MailReceived`]: the loop re-reads the rooms and the inbox, and
    /// the row cursor dedups.
    BridgedChanged,
    /// The WS reconnected (`fauna_client::NestClient::subscribe_reconnects`, which
    /// bumps on every `Connected` **after the first**) — a bare "sweep now" wake
    /// carrying no payload.
    ///
    /// **Why this rides the push seam** (`transport.md` § Reconnect & resync): a
    /// push is a *transient* broadcast, so anything the nest wanted to deliver
    /// while the socket was down never arrives — it must be *pulled*. Every other
    /// live surface (feed / knocks / contacts / notifications / account) already
    /// re-pulls on this signal; conversations used to rely on its 30 s backstop
    /// ticker alone, so a reconnect left MLS delivery up to one full tick behind
    /// every other surface (worst case ~one reconnect-backoff window **plus** one
    /// tick). The loop answers this with the *same* full sweep a tick runs —
    /// durable-inbox drain first, then every rail's cursor poll — which is
    /// idempotent by construction: every rail dedups on its own cursor, so a
    /// reconnect that raced a tick costs one redundant (cheap) poll, never a
    /// duplicate delivery.
    Reconnected,
    /// `fauna.addressbook.changed` — a durable card/book write landed in one of
    /// this actor's CardDAV address books, on any device or through the MDA
    /// (`transport.md` § Push events; the CardDAV twin of `fauna.calendar.changed`).
    ///
    /// A bare "reconcile now" wake carrying no payload, deliberately: the walk
    /// re-reads whichever books the ctag says moved, so telling it *which* book
    /// changed would buy nothing and would put a card-identity hint on a wire
    /// that carries none. **Freshness only** — the walk at attach and on every
    /// sweep is what carries correctness, so a dropped event costs latency and
    /// never coverage (`content-index.md` § Ingest triggers, v1 → the class
    /// template, piece 3).
    AddressBookChanged,
    /// `fauna.sync.changed` — a sync record landed in one of this actor's file
    /// sets, from any device or any member of a shared set (`transport.md`
    /// § Push events; `file-sync.md` § Remote-change nudge).
    ///
    /// A bare "reconcile now" wake, like its address-book sibling — and here the
    /// payload is discarded *deliberately* even though the wire carries a
    /// `folder` name: the File walk is a cross-set drain with no per-set
    /// cursor, so a set name would narrow nothing, and the name is a sealed
    /// label this seat may not even be able to render. **Freshness only** — the
    /// walk at attach and on every sweep carries correctness, so a dropped event
    /// costs latency and never coverage.
    ///
    /// ⚠ This event's *primary* consumer is the sync engine, which pulls the
    /// named set. Riding it here is a second, independent subscriber on the same
    /// broadcast — the index walk neither consumes nor delays the engine's copy.
    SyncFilesChanged,
}

/// The minimal Welcome shape the receive loop needs: the channel id (hex) the
/// Welcome binds, the HPKE-sealed Welcome bytes, and the channel `kind` (decoded
/// from the wire `WelcomePayload.channel_type`). `channel_id_hex` carries the
/// channel id for **both** same-nest and cross-nest Welcomes — the federation
/// relay wraps `channel_id` alongside `nest_url`/`channel_type` before pushing it
/// to the remote recipient (`docs/goal/behavior/direct-messages.md` § Steps —
/// "wraps the Welcome bytes with `nest_url`/`channel_type`/`channel_id`"), so a
/// cross-nest scheduling/DM welcome binds + drains exactly like a same-nest one.
/// It is `None` only from a non-conforming peer that omits it (the loop then
/// skips the welcome). `kind` lets the loop route a [`WelcomeChannelKind::Scheduling`]
/// welcome to the calendar-apply drain (`ingest_scheduling_welcome`) instead of
/// materializing a chat thread (`caldav-server.md` § Server-side auto-schedule —
/// a scheduling delivery never surfaces as a conversation).
#[derive(Clone, Debug)]
pub struct WelcomeNudge {
    pub channel_id_hex: Option<String>,
    pub welcome_bytes: Vec<u8>,
    pub kind: WelcomeChannelKind,
    /// The channel's **home** nest URL for a **cross-nest** Welcome (the wire
    /// `PushEvent::Welcome.nest_url` — the group creator's nest, where the channel
    /// log lives), or `None`/empty for a same-nest Welcome. The receive loop hands
    /// it to `ingest_welcome` / `ingest_scheduling_welcome` so a later drain
    /// (`channel.fetch`) of this channel relays to that home nest
    /// (`docs/goal/behavior/direct-messages.md` § Technical Flow — Cross-Nest, step
    /// 3). A same-nest channel keeps `None` and drains locally.
    pub home_nest_url: Option<String>,
    /// The nest-stamped sharer identity (hex ActorId) for a
    /// [`WelcomeChannelKind::Folder`] welcome — the authenticated `welcome.deliver`
    /// caller the nest stamped on the envelope (`WelcomePayload.shared_by`,
    /// unspoofable; `docs/goal/ui/folders.md` § Sharing). The recipient contact
    /// gate reads it to decide auto/knock/suppress. `None` for a DM/group/scheduling
    /// welcome (never stamped) or a cross-nest folder relay (not yet stamped ⇒ the
    /// gate treats it as a stranger knock, the safe default).
    pub shared_by: Option<String>,
    /// The shared set's home-nest-resolved display name for a
    /// [`WelcomeChannelKind::Folder`] welcome (`WelcomePayload.set_name`) —
    /// threaded into the accept-time foreign-set record
    /// (`fauna.state.folder-keys`) for a cross-nest share (Phase 2 client read-side). `None` for other kinds or a
    /// non-conforming relay that omits it.
    pub set_name: Option<String>,
    /// The member's home-nest-resolved access grant for a
    /// [`WelcomeChannelKind::Folder`] welcome (`WelcomePayload.access`) —
    /// threaded into the accept-time foreign-set record so the client
    /// knows whether to offer a folder binding. **Advisory-for-UI only, never an
    /// authz input** (`federation.md` § Cross-nest → *Recipient-side access
    /// discovery*). `None` for other kinds or a non-conforming relay that omits it ⇒ reader.
    pub access: Option<String>,
    /// The home nest's deployment `nest_actor_id` (`WelcomePayload.home_nest_actor_id`)
    /// for a cross-nest [`WelcomeChannelKind::Folder`] welcome — threaded into the
    /// accept-time foreign-set record as the byte-plane SPKI-pin trust
    /// root. `None` for other kinds / same-nest / a relay-unaware origin.
    pub home_nest_actor_id: Option<String>,
    /// The sharer's handle + handle domain (`WelcomePayload.shared_by_handle` /
    /// `shared_by_domain`) — the cross-nest owner label, paired only when the
    /// recipient's own nest bound the domain to the origin's key
    /// (`federation.md` § Cross-nest shared folders + channel append → *The
    /// cross-nest owner label*). Recorded on the accept-time foreign-set record
    /// when both halves are present. Display-only.
    pub shared_by_handle: Option<String>,
    /// See [`Self::shared_by_handle`].
    pub shared_by_domain: Option<String>,
    /// The set name as the folder Welcome carries it sealed
    /// (`WelcomePayload.set_name_sealed` + `set_name_hash`) — once the
    /// plaintext is scrubbed, the only form of the name a cross-nest member
    /// receives; the join opens it after ingesting the set's content keys.
    /// `None` for other kinds or a carrier missing either half.
    pub set_name_seal: Option<fauna_core::label_custody::SealedSetName>,
}

/// Inbound push seam for the fauna-native MLS conversation rail — the receive twin
/// of [`ConversationsRpc`].
///
/// [`crate::session::ConversationsSession::start_receive_loop`] drives this
/// object-safe, **protocol-agnostic** seam (the decoded [`ConvPushEvent`]; no
/// `PushEvent` / `fauna-client` types cross it), so `fauna-conversations` stays
/// free of `fauna-client`. Native glue backs it over a `NestClient`'s
/// `subscribe_kind` (`fauna-client-conversations::NestConversationsPush`, the push
/// twin of `NestConversationsRpc`); the loop calls [`Self::next_event`] in a
/// `tokio::select!` against a backstop ticker. Same `RpcRequester`/`Send`-future
/// rationale as [`ConversationsRpc`] for the dual `async_trait` arms.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait ConversationsPush: MaybeSendSync {
    /// Await the next inbound push event (a Welcome to ingest, or a
    /// channel-message poll nudge), skipping lagged / unrelated frames. Returns
    /// `None` when the underlying push subscription is permanently closed (the
    /// loop then falls back to its ticker backstop alone).
    async fn next_event(&self) -> Option<ConvPushEvent>;
}

/// Sink for an inbound CalDAV scheduling iMIP (`REQUEST`/`REPLY`/`CANCEL`)
/// delivered over the **mailbox-less WS-RPC rail** — the application messages of
/// a [`WelcomeChannelKind::Scheduling`] one-off MLS channel
/// (`docs/goal/behavior/caldav-server.md` § Server-side auto-schedule, Half-1).
/// The receive loop ([`crate::session::ConversationsSession::start_receive_loop`])
/// MLS-decrypts each such channel's `ChannelMessageBody::Scheduling`
/// body — the raw RFC 5322 iMIP the email rail also carries — and hands it here;
/// the impl extracts the `text/calendar` part, routes by METHOD, and applies it
/// to the actor's calendar via `fauna_client_caldav::CalDavClient` (native glue:
/// `fauna_client_conversations::NestSchedulingSink`). Kept behind this
/// **protocol-agnostic, crypto-free** seam (raw bytes only, no `fauna-client` /
/// `fauna-client-caldav` types) so `fauna-conversations` gains no calendar or
/// HPKE dependency — the receive twin of how [`InboundMailSource`] keeps the mail
/// decrypt out of this crate (priority #2). A scheduling-unconfigured session
/// simply registers no sink and the loop never drains scheduling channels.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait SchedulingSink: MaybeSendSync {
    /// Apply one decrypted scheduling iMIP (raw RFC 5322) that arrived from
    /// `origin`. The drain only reports the origin; whether it may create,
    /// change or cancel an event is the implementation's shared-Rust apply to
    /// decide — a refusal is a successful outcome (`Ok`), not a failure.
    /// Implementations return `Err` with a human-readable reason on a transport
    /// / decode / calendar-write failure; the loop logs it and retries the
    /// channel on the next poll (the apply is idempotent — a re-drain of the
    /// same record is a no-op).
    async fn apply_scheduling_imip(
        &self,
        raw_rfc5322: Vec<u8>,
        origin: SchedulingOrigin,
    ) -> Result<(), String>;
}

/// The **recipient contact gate** for a cross-user shared folder — the decision
/// half of the "auto for contacts, knock for strangers" arrival gate
/// (`docs/goal/ui/folders.md` § Sharing). A [`WelcomeChannelKind::Folder`]
/// welcome carries a nest-stamped `shared_by` (the sharer's actor id); the receive
/// rail hands it here to learn how the recipient's *existing relationship* to that
/// sharer gates the arrival, then acts on the returned [`ArrivalDisposition`]:
/// `Auto` → join the group off the chat rail ([`join_folder_welcome`]) + ack;
/// `Knock` → leave the Welcome un-acked (it *is* the pending-share); `Suppress` →
/// ack-and-drop (a blocked sharer never joins).
///
/// Kept behind this **protocol-agnostic, contacts-free** seam (the only input is
/// the raw hex `shared_by`, the only output the shared [`ArrivalDisposition`]) so
/// `fauna-conversations` gains no contacts-client dependency — the receive twin of
/// how [`SchedulingSink`] keeps the calendar client out of this crate (priority
/// #2). The native glue (`fauna_client_conversations::NestFolderGate`) reads the
/// sharer's contact-status over `fauna.contacts.status` and maps it through
/// `fauna_core::data::contact_arrival_disposition`; a gate-unconfigured session
/// simply registers none and every folder Welcome stays un-acked (retained).
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait FolderGateSink: MaybeSendSync {
    /// How the recipient's contact relationship to `shared_by` (hex sharer actor
    /// id; `None` for an unstamped / cross-nest-relayed welcome) gates this
    /// folder Welcome's arrival. Implementations resolve the sharer's
    /// contact-status and return the shared decision; any lookup failure or an
    /// absent `shared_by` MUST resolve to [`ArrivalDisposition::Knock`] — never
    /// auto-join on uncertainty (the recipient stays in control).
    async fn arrival_for(&self, shared_by: Option<String>) -> ArrivalDisposition;

    /// Drop the recipient's own roster row on the shared set — the mutation a
    /// [`ArrivalDisposition::Suppress`] arrival owes on top of its ack
    /// (`docs/goal/ui/folders.md` § Sharing → *Adding the 2nd..Nth member*:
    /// "A **suppressed** (Blocked-sharer) knock drops the roster row the same way"
    /// a decline does). The recipient is rostered *before* the arrival is gated —
    /// the owner's share writes the row at Welcome delivery — so an ack-and-drop
    /// alone would leave a blocked sharer's target rostered forever: the owner's
    /// "Shared with" list would over-report, and the 2nd..Nth-member add path,
    /// which discriminates on that roster, would treat a post-unblock re-share as
    /// a no-op access refresh instead of a genuine re-invite.
    ///
    /// `group_id_hex` is the raw MLS group id the Welcome carried (the leave is
    /// addressed by group id, never the owner-only set name); `home_nest_url` is
    /// `Some` for a cross-nest share, whose roster row lives on the set's home nest
    /// and is dropped through the federation relay.
    ///
    /// **Best-effort by contract — implementations MUST NOT propagate failure.**
    /// The caller has already decided to suppress, and the ack must still happen:
    /// a retained Welcome from a blocked sharer would re-surface the arrival the
    /// block exists to hide. A failed drop leaves a stale roster row, which the
    /// re-share heal path already repairs. Implementations log and return.
    ///
    /// Kept on this seam (rather than the caller reaching for a folders client)
    /// for the same reason as [`Self::arrival_for`]: `fauna-conversations` must
    /// gain no `fauna-client-folders` dependency — and here it *cannot*, since
    /// `fauna-client-folders/mls` depends on `fauna-client-conversations`, so the
    /// reverse edge would be a package cycle. The native glue
    /// (`fauna_client_conversations::NestFolderGate`) issues `fauna.folders.leave`
    /// over its own transport; the shape's owner is
    /// `fauna_client_folders::FoldersClient::leave_with_home`.
    async fn drop_roster_row(&self, group_id_hex: String, home_nest_url: Option<String>);
}

/// A content-key envelope whose signature verified
/// ([`FolderCustodySink::fetch_sealed_envelope`]): who signed it — which the
/// driver compares against the owner its MLS state records — and the sealed
/// payload to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedEnvelope {
    pub signer: fauna_core::identity::ActorId,
    pub sealed: Vec<u8>,
}

/// The member-side **content-key custody ingest** seam for cross-user shared file
/// sets — the Phase 0 read leg (`docs/goal/ui/folders.md` § Sharing;
/// `docs/goal/architecture/mls-group-key-material.md` § M2 Multi-writer). The M2
/// model already holds each member's copy of the set's content-key bundle in their
/// own folder-key custody (`fauna.state.folder-keys`); this seam populates it so a *member* (not just the owner) can
/// decrypt a shared set's content. The session drives the ingest — it holds the
/// [`fauna_mls::engine::MlsEngine`] the open needs — and splits **around** the
/// open: it fetches the owner's sealed envelope through [`Self::fetch_sealed_envelope`],
/// opens it via `MlsEngine::open_content_key_envelope` at the group's current
/// epoch, then folds the generations into the member's own custody + persists them
/// through [`Self::merge_and_persist`].
///
/// Kept behind this seam so `fauna-conversations` gains no `fauna-client-folders`
/// / `fauna-client-config` dependency — the custody twin of how [`FolderGateSink`]
/// keeps the contacts client out of this crate (priority #2). The native glue
/// (`fauna_client_conversations::NestFolderCustodySink`) resolves the set's name
/// from the member-visible roster, fetches over `fauna.folders.content_key.get`,
/// and persists through the account store; a session that registers none
/// simply never ingests custody (a member lists a shared set but cannot decrypt
/// its bytes — the pre-Phase-0 behaviour).
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait FolderCustodySink: MaybeSendSync {
    /// Fetch the owner's latest content-key envelope for a joined folder
    /// channel (`fauna.folders.content_key.get`) and verify its signature
    /// (`writer-signed-change-records.md` ruling (11)(b): every envelope is
    /// signed, and none is ingested without a signature that verifies) —
    /// answering who signed it and the sealed bytes to open. `None` when the
    /// owner has not published yet (`not_published`), the caller is not a
    /// readable member (`not_found`), the channel's set name cannot be
    /// resolved, the blob is unsigned or its signature does not verify, or any
    /// transient error — **all best-effort**: the driver must NOT fail the
    /// join/poll, and the poll-cadence retry re-attempts on the next pass.
    async fn fetch_sealed_envelope(&self, channel_id_hex: &str) -> Option<FetchedEnvelope>;

    /// Merge an opened content-key bundle into this member's own folder-key
    /// custody and persist it (the idempotent CRDT merge + durable write). Returns
    /// whether the persist **succeeded** (custody is now durable) — a `false`
    /// leaves the driver to re-ingest next pass rather than mark the channel done.
    ///
    /// Takes the whole opened payload: the generations merge by the custody
    /// CRDT, and the set's nonce (when the owner sealed one) replaces this
    /// member's copy — the binding its writer-signed change records verify
    /// under — forward only, with its lineage (`writer-signed-change-records.md`
    /// ruling (11)(b)). `may_move` is the driver's verdict on the envelope's
    /// signer: `true` only for the owner this member's MLS state records; any
    /// other signer's payload may confirm what custody holds and nothing more.
    /// A refused payload is discarded whole (keys and nonce together) and
    /// answers `false`, so the driver retries.
    async fn merge_and_persist(
        &self,
        channel_id: &[u8; 32],
        payload: fauna_core::folder_keys::ContentKeyEnvelopePayload,
        may_move: bool,
    ) -> bool;

    /// Whether this channel's envelope fetch is **re-owed** although no epoch
    /// advanced and the last attempt merged
    /// (`writer-signed-change-records.md` ruling (11)(b)): the nest's echo of
    /// the set's nonce differs from the one this member holds, or the host of
    /// this member's sync engine refused a row of the set `signature_invalid`.
    /// Either says the owner re-minted the nonce without an epoch advance, so
    /// the published envelope names a nonce this member has not ingested.
    ///
    /// Asked by the driver on every pass, the quiet poll included, so an
    /// implementor **must not make a network call per ask** — it answers from
    /// what a throttled check last found. A `true` is spent by the asking: the
    /// driver attempts once, and the answer is `false` again until the next
    /// check finds the fetch owed. Default `false` — a sink that cannot tell
    /// leaves the member to its epoch advances.
    async fn refetch_owed(&self, channel_id: &[u8; 32]) -> bool {
        let _ = channel_id;
        false
    }

    /// Record a **foreign** (cross-nest) shared-set membership in this member's
    /// own folder-key custody (`fauna_core::data::ForeignFolder`; Phase 2 client
    /// read-side). Called by `join_folder_welcome` when the accepted share's
    /// `home_nest_url` names another nest — the member's own nest holds no row
    /// for the set, so this record is what makes it listable and routes its
    /// reads (`nest_url` relay + direct byte fetch). Best-effort like every
    /// sink write — a `false` (persist failure) must not fail the join; the
    /// record is re-written on the next accepted re-delivery. Default no-op
    /// (`true`) so custody-only sinks/tests are unaffected.
    ///
    /// Takes the whole [`fauna_core::data::ForeignFolder`] rather than its
    /// fields: the record has grown two adjacent `Option<String>`s
    /// (`set_name`, `access`) that a positional signature would let a call site
    /// transpose with no compile error, and a future field-add then touches one
    /// struct instead of every impl.
    async fn record_foreign_set(&self, record: fauna_core::data::ForeignFolder) -> bool {
        let _ = record;
        true
    }

    /// Name a held, still-nameless foreign set from the share's sealed name —
    /// called by `join_folder_welcome` after the join's custody ingest, once
    /// this member holds the content keys that open it (a cross-nest Welcome
    /// carries the set name only sealed, so the accept-time
    /// [`Self::record_foreign_set`] records it nameless). Best-effort and
    /// gain-only; `false` when nothing changed. Default no-op.
    async fn name_foreign_set_from_seal(
        &self,
        channel_id: &[u8; 32],
        sealed: &[u8],
        name_hash: &[u8],
    ) -> bool {
        let _ = (channel_id, sealed, name_hash);
        false
    }

    /// Drop the foreign-set record on leave — the reverse of
    /// [`Self::record_foreign_set`], called by `leave_folder` so a left set
    /// stops appearing in the member-visible list. Best-effort; default no-op.
    async fn forget_foreign_set(&self, channel_id: &[u8; 32]) -> bool {
        let _ = channel_id;
        true
    }

    /// The recorded home-nest URL for a **foreign** set, if this member holds a
    /// record for `channel_id` — the durable twin of the backend's RAM
    /// `channel_home` map (which only covers channels joined *this* session),
    /// so a relaunched client can still route a leave/read to the set's home
    /// nest. `None` for same-nest sets or a custody-only sink (default).
    async fn foreign_home_url(&self, channel_id: &[u8; 32]) -> Option<String> {
        let _ = channel_id;
        None
    }

    /// Every foreign (cross-nest) set this member holds a record for, as
    /// `(channel_id, home_nest_url)` pairs — the **population** read whose
    /// single-channel twin is [`Self::foreign_home_url`].
    ///
    /// It exists because of *when* the launch restore needs the datum, not
    /// because the lookup differs. A restored slice carrying no
    /// `home_nest_url` is either a same-nest channel or a slice-less folder
    /// channel whose custody seed is this `foreign_homes` answer, and the
    /// restore cannot tell which — so recovering it means asking about
    /// **every** restored channel,
    /// the same-nest majority included. Per channel that is one custody
    /// fetch each; as a population it is one fetch for the whole launch, which
    /// is why the restore takes this direction and not a loop over
    /// [`Self::foreign_home_url`].
    ///
    /// Same-nest sets are simply absent from the record — a foreign set is the
    /// only kind the custody holds one for — so the returned pairs are exactly
    /// the channels whose home is *not* this nest. Default empty, so a
    /// custody-only sink or a test double is unaffected.
    async fn foreign_homes(&self) -> Vec<([u8; 32], String)> {
        Vec::new()
    }
}

/// Verifier seam for the **in-group succession statement**
/// (`docs/goal/behavior/identity-succession.md` § Propagation → *MLS groups*):
/// a member that receives [`fauna_mls::types::GroupMetaMessage::Succession`]
/// hands the decoded statement here before rendering anything.
///
/// The statement arrives as a **claim** — a seed thief can author a
/// structurally perfect one — so `verify` must apply the § The succession
/// statement verification rule: `SignedIdentitySuccession::verify` against a
/// chain head the consumer *independently* knows (a cached
/// `Profile.recovery_head`, a previously-seen registration), or the anchored
/// chain walk to the old identity's home nest (`resolve_successor`). Never the
/// statement's own `recovery_pubkey`, and never a nest's say-so.
///
/// Wired by the session layer, which is where the anchor sources live — this
/// crate deliberately has no recovery dependency and no way to locate a
/// foreign identity's home nest (the `SchedulingSink` / [`FolderCustodySink`]
/// priority-#2 pattern). A session that registers no witness, a malformed
/// statement, and a failed or unanchorable verification all degrade the same
/// way: the member renders the bare add — exactly what an older client that
/// cannot decode the variant renders — and stays in the group. Best-effort by
/// contract: the inbound poll calls this inline, so implementations bound
/// their own dials rather than stall the feed on an unreachable anchor.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait SuccessionWitness: MaybeSendSync {
    /// `Some` iff the statement verifies under the rule above; the returned
    /// pair is what the consumer may act on (`resolve_successor`'s contract:
    /// what the chain authorizes, never what was delivered).
    async fn verify(
        &self,
        statement: fauna_core::recovery::SignedIdentitySuccession,
    ) -> Option<fauna_core::recovery::VerifiedSuccession>;

    /// Whom the bare identity `name` has since become — its verified
    /// succession line — for the one consumer that holds a name and **no
    /// statement**: a community room's policy chain, whose owner and admin
    /// names a member must resolve before a successor's signature or floor
    /// delete record counts (`conversation-rooms.md` § Roles and authorization
    /// → *A name designates its verified line*).
    ///
    /// The rule is [`Self::verify`]'s, and so is the anchor: the implementation
    /// dials only what this device **independently** knows of `name` (the
    /// owner's own anchor-grade handle, else the identity's harvested profile
    /// domain). That is why no room, home URL or hint is a parameter — the
    /// room's home nest is the party the chain exists to distrust, and it must
    /// have no way to choose the nest that answers. Bounded and best-effort
    /// like `verify`; the caller parks on [`SuccessionLine::NotYet`]. Default
    /// `NotYet`: a witness that resolves no lines grants nothing.
    async fn succession_line(&self, name: &fauna_core::identity::ActorId) -> SuccessionLine {
        let _ = name;
        SuccessionLine::NotYet
    }

    /// [`Self::succession_line`] for a name this caller already holds a
    /// verified line for — empty or positive: an answer true only *so far*
    /// (the identity, or the newest holder of its line, may succeed later in
    /// the session), so an implementation that memoizes it walks again here
    /// rather than repeating it. Each call may dial, so the caller bounds how
    /// often it asks (the room backend's `LINE_REASK_PASSES`), and the caller
    /// keeps a line that only grows (`SuccessionLines::insert`). Default:
    /// [`Self::succession_line`], right for a witness that memoizes nothing.
    async fn recheck_line(&self, name: &fauna_core::identity::ActorId) -> SuccessionLine {
        self.succession_line(name).await
    }

    /// A peer-anchor harvest just seeded something new, so whatever this witness
    /// caches about *not* holding an anchor is now stale.
    ///
    /// **This is the only signal an anchor store's reader gets.** The harvest is
    /// a separate producer writing the owner's account store on its own read-path
    /// schedule (rule 4), through a store handle the witness does not share, so
    /// a witness that caches "no anchor for this actor" has no way to notice the
    /// seed land — and a witness that therefore *refuses* to cache pays a fresh
    /// anchor-store read on every statement, which an in-group member forging
    /// `old_actor_id`s can drive without bound.
    ///
    /// Called from [`redrive_parked_successions`] — the one path the ratified
    /// re-drive rule already routes every seed through
    /// (`identity-succession.md` § The succession statement → *the peer-profile
    /// harvest*: "a harvest that seeds something new for that peer **re-drives**
    /// the parked statement"), and unconditionally, before the parked set is
    /// consulted, so a seed that lands with nothing parked still invalidates.
    /// Default: no-op, for a witness that caches nothing.
    ///
    /// [`redrive_parked_successions`]: crate::backends::fauna_mls::redrive_parked_successions
    async fn anchor_seed_landed(&self) {}

    /// This session runs a peer-anchor harvest sweep, so a verdict that rests
    /// on a *held* head may wait for the sweep to settle that peer
    /// (`identity-succession.md` § The succession statement → *the harvest
    /// wait*).
    ///
    /// Called by whoever starts the sweep, **before the first inbound poll can
    /// run** — the receive loop's prologue natively, the session constructor on
    /// web — because the wait is only safe where something will end it: a
    /// witness told to wait in a session with no sweep would never settle a
    /// statement offline again. That is why this is an announcement and not a
    /// constructor default. Default: no-op, for a witness that never waits.
    async fn harvest_armed(&self) {}

    /// The sweep has **settled** `actor` for this session — seeded, found
    /// nothing new, was refused, or spent its retry budget — so nothing more
    /// will be learned about that peer before the session ends, and a verdict
    /// waiting on the harvest may now be given.
    ///
    /// Carries no store news: a settle that seeded something is announced
    /// separately, and first, by [`Self::anchor_seed_landed`], and this one
    /// must never be treated as a seed (the anchor store is read once per seed
    /// *generation*, and most settles seed nothing). Called from the re-drive,
    /// before the parked set is consulted, for the same reason the seed
    /// announcement is. Default: no-op.
    async fn harvest_settled(&self, actor: &fauna_core::identity::ActorId) {
        let _ = actor;
    }
}

/// [`SuccessionWitness::succession_line`]'s answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SuccessionLine {
    /// Verified by the anchored walk: every successor, oldest first — empty
    /// when the identity never succeeded.
    Verified(Vec<fauna_core::identity::ActorId>),
    /// Nothing is established *yet* — no independent anchor for the identity,
    /// or one that could not be reached. Grants nothing; judge again later.
    NotYet,
}

/// One edit to a governed room's policy (`conversation-rooms.md` § Roles and
/// authorization) — what the page's editor asks for, applied by the rail onto
/// the current signed policy: the version advances, the acting principal
/// signs, and the change rides a group-context commit every member judges.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RoomPolicyEdit {
    /// The room's name (owner or admin).
    Rename(String),
    /// Who may invite (owner or admin).
    JoinRule(crate::room::JoinRule),
    /// What a newcomer sees of the room (owner or admin).
    HistoryPolicy(crate::room::HistoryPolicy),
    /// Appoint an admin (owner only).
    AppointAdmin(fauna_core::identity::ActorId),
    /// Demote an admin to member (owner only).
    DemoteAdmin(fauna_core::identity::ActorId),
    /// Hand the room to another member (owner only) — the **ownership
    /// transfer ceremony** (`conversation-rooms.md` § Roles and authorization
    /// → *Ownership transfer*): the outgoing owner countersigns the policy
    /// naming its successor and posts it as an offer on the channel; the
    /// incoming owner's device signs the same policy and commits it, and
    /// every member admits the commit only with both signatures. The edit
    /// returns once the **offer** is on the channel; the roles flip when the
    /// incoming owner's commit is folded.
    TransferOwnership(fauna_core::identity::ActorId),
}

/// How a gated room-policy commit derives the extension it installs — re-run
/// by the rebase loop on **every** attempt against the policy the channel
/// holds *then*, never once against the policy it held when the gesture was
/// made. Replaying fixed policy bytes after a catch-up that folded another
/// governor's change would re-stage a commit whose version no longer advances
/// by one: every other member refuses it while the sender merges it — a fork.
/// A rebuild that finds the change no longer applicable returns the typed
/// refusal and the gated send aborts (pending cleared, nothing merged).
pub type RoomPolicyRebuild = Box<
    dyn Fn(
            &fauna_mls::room_policy::RoomPolicyExtension,
        )
            -> Result<fauna_mls::room_policy::RoomPolicyExtension, fauna_mls::error::MlsError>
        + Send
        + Sync,
>;

/// One principal on a room's reported floor roster.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoomRosterEntry {
    pub actor: fauna_core::identity::ActorId,
    /// `None` on a policy-less room (no policy, no roles).
    pub role: Option<crate::room::RoomRole>,
}

/// The **member-reported floor roster** of an end-to-end room after one of
/// this device's membership or policy commits (`conversation-rooms.md` § The floor
/// roster → *End-to-end rooms — the floor roster is a member-reported
/// mirror*): the MLS roster as the committing device now holds it, with each
/// member's role under the room policy. *Report, never guess* — the nest
/// stores it and decides routing, custody and succession targets off it, and
/// nothing about confidentiality.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoomRosterReport {
    pub channel_hex: String,
    pub members: Vec<RoomRosterEntry>,
    /// The policy version the roles were read under; `None` on a
    /// policy-less room.
    pub policy_version: Option<u64>,
    /// The room log's `seq` of the commit this roster follows — what that
    /// commit's own send answered (`CommitGate`'s accepted seq, or the
    /// gate-less send's). It is what lets the home nest order reports that
    /// arrive out of order (`conversation-rooms.md` § The floor roster);
    /// `None` only where the reporting path had no commit of its own to name.
    pub commit_seq: Option<i64>,
    /// The room's **home nest**, `Some(url)` when the channel is foreign-homed
    /// on this device — the same signal [`RoomRosterReader::read_roster`]
    /// routes on (`FaunaMlsBackend::channel_home_url`). A room's floor roster
    /// lives on its home nest alone (`conversation-rooms.md` § The home nest),
    /// so the glue rides the distinct relay kind
    /// `fauna.conversations.room.roster_report_remote` on `Some` — the
    /// reporter's own nest originates the leg to the home — and the same-nest
    /// `room.roster_report` on `None`. Sending `None` for a foreign-homed
    /// room is not a slower answer but a wrong one: the device's own nest
    /// holds no room record, and the home's floor never hears of the commit.
    pub home_nest_url: Option<String>,
}

/// The **roster-report** seam: carries a [`RoomRosterReport`] to the room's
/// home nest through the room family's roster-report kind — or its relayed
/// twin when the room is homed elsewhere (`RoomRosterReport::home_nest_url`).
/// Declared here and injected by the session layer, like every other
/// nest-facing seam.
///
/// A backend with no reporter registered tallies every report it could not
/// send (`FaunaMlsBackend::roster_report_counts`) and drops it — the custody
/// serve door then keeps failing closed for such rooms. The nest's report
/// kind and both glue implementations exist
/// (`fauna_client_conversations::NestConversationsRpc` /
/// `WsConversationsRpc`), wired by the Rust-native apps' session builders.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait RoomRosterReporter: MaybeSendSync {
    /// Deliver one report and say what the home nest did with it.
    async fn report(&self, report: RoomRosterReport) -> RoomRosterReportOutcome;
}

/// What one floor-roster report's delivery answered.
///
/// The middle arm is the point of the type. A report the home nest
/// understands but does not apply — because its floor already holds a report
/// at or above the position this one names — is not an error and not a
/// success, and collapsing it into either is how a device stops being able to
/// tell that its own report was never the one applied. The nest has always
/// answered which position superseded it
/// (`RoomRosterReportReply::superseded_by`); until this type the glue dropped
/// that answer on the floor, so no client could read it at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomRosterReportOutcome {
    /// The home nest stored this report: the floor now holds this roster.
    Stored,
    /// Delivered, understood, and **not applied** — the floor already holds a
    /// report at the named position. Not a failure: the floor is at least as
    /// new as this report (`conversation-rooms.md` § The floor roster).
    Superseded {
        /// The position the floor holds, from the ack.
        by: Option<i64>,
    },
    /// Not delivered — no seam registered, a transport error, or a refusal.
    Undelivered,
}

impl RoomRosterReportOutcome {
    /// Whether the home nest took delivery, superseded or not — the old
    /// `bool` answer, kept for the callers that only ever asked that.
    pub fn delivered(self) -> bool {
        !matches!(self, Self::Undelivered)
    }
}

/// One principal as the home nest's floor roster *reads back* — the read
/// half's row, carrying what the report never could.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoomRosterKnownMember {
    pub actor: fauna_core::identity::ActorId,
    /// The principal's handle as its home nest knows it — **joined
    /// nest-side**, never read off a member's roster report: from the serving
    /// nest's own `users` row for a local member, or, for a member homed on
    /// another nest, the `handle@domain` that member's own home nest announced
    /// on the member's relayed drain and the serving nest verified
    /// (`federation.md` § Cross-nest shared folders + channel append, the
    /// id→handle bullet). `None` for a foreign member whose home nest has not
    /// announced one yet — it keeps its elided actor id, and the read asks
    /// again later — a handle-less local user, or a nest/bridge principal.
    pub handle: Option<String>,
    /// The handle's domain, `None` whenever `handle` is. Pairs with it to
    /// form the canonical `handle@domain` a resolved Fauna address shows.
    pub domain: Option<String>,
    /// What kind of principal this row seats — `user`, the room's home
    /// `nest`, or a `bridge` (`conversation-rooms.md` § The room →
    /// *Principals*). Carried rather than filtered away in the glue because
    /// the class is a pure function of the member set (§ The three classes,
    /// rule TP8): a reader that never sees the nest row cannot tell a
    /// community room from an end-to-end one, and a minter that never sees it
    /// cannot grant the home nest its read.
    ///
    /// An unrecognised word decodes as [`RoomPrincipalKind::Other`] rather
    /// than defaulting to `User`: a principal this build does not know about
    /// is not a user, and calling it one would seat it in the participant
    /// list and hand it a wrap.
    pub kind: RoomPrincipalKind,
    /// The rank the floor holds for this principal, `None` on a policy-less room
    /// (which carries no roles at all — the distinction the report
    /// deliberately preserves).
    pub role: Option<crate::room::RoomRole>,
    /// This seating's **roster entry id** — the slot a generation wrap is
    /// bound to, and half the AAD its open re-derives. `None` for a seating
    /// with no keyed ceremony entry, and when the nest's reply omits
    /// the field.
    pub entry_id: Option<[u8; 32]>,
    /// The group-reception public key this principal handed the room when it
    /// was seated — the X-Wing wrap target a mint addresses to it. `None` for
    /// a principal seated without one, which the coverage rule skips as
    /// *unkeyable* rather than treating as uncovered.
    pub reception_pubkey: Option<Vec<u8>>,
    /// When this principal joined, epoch millis — the enrolment instant a
    /// wrap target carries (advisory in the scheme, and carried here rather
    /// than re-invented so a mint's roster member is the room's own record).
    pub joined_at_ms: i64,
    /// Whether the room's **current** generation carries a wrap bound to this
    /// seating. On the home nest's row it is whether the nest reads the room
    /// (the members' standing grant, which a rotation keeps rather than
    /// resets); on a user's row it is whether an owner or admin still owes it
    /// a key-in. `None` when the room has no generation yet, for a seating with
    /// no entry id, and when the nest's reply omits it — **unknown, never
    /// "no"**: a reader that took that silence for `false` would
    /// paint the grant withdrawn and re-key every member on every poll.
    pub tip_wrapped: Option<bool>,
}

/// What kind of principal a floor-roster row seats
/// (`conversation-rooms.md` § The room → *Principals*), in this crate's own
/// vocabulary so the backend gains no wire dependency (the
/// [`RoomRosterKnownMember`] pattern).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoomPrincipalKind {
    /// A user's device fleet — the only kind that renders as a participant.
    User,
    /// The room's home nest, seated as an ordinary member; its presence is
    /// what derives the class as community.
    Nest,
    /// A bridge or mail transfer agent — its presence derives the class as
    /// transport-only.
    Bridge,
    /// A kind this build does not know. Never treated as any of the above:
    /// an unknown principal is neither rendered nor wrapped to.
    Other,
}

impl RoomPrincipalKind {
    /// Decode the wire's kind word. Total, and deliberately not `FromStr` —
    /// there is no failure to report, only [`Self::Other`].
    pub fn from_wire(word: &str) -> Self {
        match word {
            "user" => Self::User,
            "nest" => Self::Nest,
            "bridge" => Self::Bridge,
            _ => Self::Other,
        }
    }
}

impl RoomRosterKnownMember {
    /// The canonical `handle@domain` to seat, or `None` when this nest served
    /// no handle for the principal. Mirrors
    /// `FaunaMlsBackend::resolve_address`'s "canonical `localpart@domain`
    /// display, regardless of which form was typed", so a member resolved
    /// this way reads exactly like one the user typed into the picker.
    pub fn qualified_handle(&self) -> Option<String> {
        fauna_core::format::qualified_handle(self.handle.as_deref(), self.domain.as_deref())
    }

    /// This row as a **wrap target** for a generation mint, or `None` when it
    /// is not one.
    ///
    /// A row is a target exactly when the room seated it with both halves of
    /// the pair — an entry id (the slot the wrap binds to) and a reception
    /// key (what it seals to). A principal missing either is **unkeyable**,
    /// which the scheme's roster-coverage rule skips rather than fails on
    /// (`conversation-rooms.md` § The three classes → *Community*: "A member
    /// with no reception key yet is not coverable and is skipped — the state
    /// the scheme's member top-up heals"). So a founder never has to choose
    /// between refusing to key its room and building a mint the nest will
    /// refuse.
    ///
    /// Every kind that has the pair is a target, the home **nest** included:
    /// wrapping to it is what grants the materialization read, and a later
    /// mint that omits it is the revoke (§ *The home nest's read, and its
    /// revoke*). [`RoomPrincipalKind::Other`] is the one exclusion — a
    /// principal this build cannot name is not handed a key.
    /// This principal as the render vocabulary's kind — what the room's
    /// **class** is derived from (`conversation-rooms.md` § Architectural
    /// rules, rule 1: the class is a function of the member set).
    ///
    /// [`RoomPrincipalKind::Other`] maps to [`crate::room::PrincipalKind::Bridge`],
    /// and the direction is deliberate. The class is a statement about **who
    /// can read**, so the two ways of being wrong are not equal: naming a room
    /// more open than it is makes its members more careful, while naming it
    /// more private than it is is a privacy claim this build cannot support. A
    /// principal it cannot name is a seated reader of unknown character —
    /// exactly what transport-only says honestly — so an older app meeting a
    /// kind a newer nest introduced degrades to the cautious label rather than
    /// quietly reporting end-to-end.
    ///
    /// The mirror of [`Self::wrap_target`]'s treatment of the same variant, in
    /// the same spirit: there, an unnameable principal is not handed a key;
    /// here, it is not vouched for.
    pub fn render_kind(&self) -> crate::room::PrincipalKind {
        match self.kind {
            RoomPrincipalKind::User => crate::room::PrincipalKind::User,
            RoomPrincipalKind::Nest => crate::room::PrincipalKind::Nest,
            RoomPrincipalKind::Bridge | RoomPrincipalKind::Other => {
                crate::room::PrincipalKind::Bridge
            }
        }
    }

    pub fn wrap_target(&self) -> Option<fauna_core::group_scope::RosterMember> {
        if self.kind == RoomPrincipalKind::Other {
            return None;
        }
        let reception_pubkey = self.reception_pubkey.clone().filter(|k| !k.is_empty())?;
        Some(fauna_core::group_scope::RosterMember {
            entry_id: self.entry_id?,
            member_actor: self.actor,
            reception_pubkey,
            enrolled_at_ms: self.joined_at_ms,
        })
    }
}

/// The **roster-read** seam: the id-keyed handle read that resolves a room
/// member this device has never met (`conversation-rooms.md` § Implementation
/// status today — the roster bullet). The read half of
/// [`RoomRosterReporter`], and deliberately a *separate* trait on the same
/// glue object for that trait's own reason.
///
/// Why a network read at all, when [`crate::ConversationsManager::seat_address_for`]
/// already resolves seat-time handles: that path scans only what this device
/// is *already* rendering, so a member nobody here has met resolves to
/// nothing. The MLS engine roster carries actor ids and nothing else, and no
/// other kind serves the id→handle direction — `fauna.actor.by_handle`
/// resolves the other way and `fauna.profile.get`'s `Profile` has no handle
/// field.
///
/// **Never an identity claim.** What comes back is what to *show*; every
/// membership decision still keys on the actor id
/// ([`crate::address::TypedAddress::same_participant`]). A backend with no
/// reader registered simply leaves such members elided, which is the
/// pre-existing honest fallback rather than a failure.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait RoomRosterReader: MaybeSendSync {
    /// Read the floor roster of the room a channel hosts.
    ///
    /// `home_nest_url` is `Some(url)` when the room is **foreign-homed**, and
    /// the pick it drives is the same one [`ConversationsRpc::channel_actors`]
    /// makes for the same reason: a room's floor roster lives on its home nest
    /// alone (`conversation-rooms.md` § The home nest), so a member homed
    /// elsewhere must reach it through its own nest's relay — the **distinct
    /// kind** `fauna.conversations.room.list_roster_remote`, never an additive
    /// field on `room.list_roster`, whose old-nest degrade would be a clean
    /// `permission_denied` indistinguishable from "you are not a member".
    /// `None` = the same-nest `room.list_roster`.
    ///
    /// Answers [`RoomRosterRead`], not `Option<RoomFloor>`: a clean "no floor
    /// here" and a read that simply failed to reach an answer are two
    /// different facts, and [`super::backends::fauna_mls::FaunaMlsBackend::backfill_floor_roster`]
    /// is a caller that must not confuse them ( — folding both into
    /// one `None` let a transient failure be taken as a confirmed-empty floor
    /// and roll a live one back). Every other caller's degradation is
    /// unchanged — [`RoomRosterRead::or_absent`] collapses both non-floor
    /// cases back to `None`, "this member stays elided", never an error
    /// surfaced to the user.
    async fn read_roster(
        &self,
        channel_hex: String,
        home_nest_url: Option<String>,
    ) -> RoomRosterRead;

    /// Read the signed policy a community room held **at `version`** — a
    /// superseded one or the current — with the room's birth salt beside it:
    /// the two things a member needs to anchor a policy version before it
    /// lets one grant a rank (`conversation-rooms.md` § Roles and
    /// authorization → *Delete any message — the mechanism* → *Members verify
    /// what they paint*). The same roster read, asked with its additive
    /// `at_policy_version`; `home_nest_url` picks the relay exactly as in
    /// [`Self::read_roster`].
    ///
    /// Nothing served here is believed: the caller verifies the chain. The
    /// default answers [`RoomPolicyVersionRead::NotHeld`], so a reader that
    /// cannot fetch a version fails closed — no tombstone is painted.
    async fn read_policy_version(
        &self,
        _channel_hex: String,
        _home_nest_url: Option<String>,
        _version: u64,
    ) -> RoomPolicyVersionRead {
        RoomPolicyVersionRead::NotHeld
    }
}

/// The answer to [`RoomRosterReader::read_policy_version`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RoomPolicyVersionRead {
    /// The home nest served the version: canonical dag-cbor
    /// `fauna_mls::room_policy::SignedRoomPolicy` bytes, **unverified**, and
    /// the room's birth salt when it served one.
    Served {
        policy: Vec<u8>,
        birth_salt: Option<[u8; 32]>,
    },
    /// A clean answer without the version: the room never held it there, the
    /// nest does not serve it, or this member is refused the read. Final for
    /// the act that asked.
    NotHeld,
    /// The read did not reach an answer — worth asking again later.
    Unavailable,
}

/// The three-way answer to a floor-roster read
/// ([`RoomRosterReader::read_roster`]).
///
/// A clean "no floor" and a read that never reached an answer are
/// distinguished because exactly one caller —
/// [`super::backends::fauna_mls::FaunaMlsBackend::backfill_floor_roster`] —
/// must not confuse them: its unpositioned report is safe only when the
/// floor is confirmed absent (`NoFloor`), never when the read merely
/// couldn't say (`Unavailable`), since the home nest takes an unpositioned
/// report wholesale (`conversation-rooms.md` § The floor roster). Every other
/// caller doesn't care and calls [`Self::or_absent`] to get the pre-existing
/// `Option<RoomFloor>` degradation back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RoomRosterRead {
    /// A floor exists and this device is a live member of it.
    Floor(RoomFloor),
    /// The read reached a clean answer of "no floor here" — no floor exists
    /// yet, or this device is not a member of one that does. The two are
    /// indistinguishable by design: the door answers the same
    /// `permission_denied` to both.
    NoFloor,
    /// The read did not reach an answer at all — a transport fault, a nest
    /// restart, anything short of the door's own clean refusal.
    Unavailable,
}

impl RoomRosterRead {
    /// Collapse to the pre-existing two-way shape every caller but the
    /// backfill wants: `Some` only when a floor came back, `None` for both
    /// "no floor" and "the read failed".
    pub fn or_absent(self) -> Option<RoomFloor> {
        match self {
            RoomRosterRead::Floor(floor) => Some(floor),
            RoomRosterRead::NoFloor | RoomRosterRead::Unavailable => None,
        }
    }
}

/// One read of a room's floor: its live principals, and the policy version
/// their roles were read under.
///
/// The version rides along rather than being a second read because it is only
/// ever meaningful *paired* with the roles it explains — an invitation records
/// "the version I read the join rule under"
/// (`fauna_mls::room_policy::RoomInvite`), so a version fetched separately
/// from the roles could name a policy under which the inviter's own rank was
/// different. `None` on a policy-less room, which carries no policy at all — the
/// same distinction the roster report deliberately preserves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoomFloor {
    /// Live principals only; a `Removed`-absorbed row is history the read does
    /// not render.
    pub members: Vec<RoomRosterKnownMember>,
    /// The policy version the roles were read under, `None` on a policy-less room.
    pub policy_version: Option<u64>,
    /// The **signed policy** [`Self::policy_version`] names, canonical dag-cbor
    /// and still unverified — the reader checks the signature before trusting a
    /// field of it ([`super::backends::fauna_mls::FaunaMlsBackend::read_room_policy`]).
    ///
    /// The roles above are this policy's projection, which is what the nest
    /// enforces against; these are the bytes a member renders for itself, and
    /// the bytes a policy change is authored *from* — a replacement built
    /// without them would silently reset every field the caller did not mean to
    /// touch. `None` on a policy-less room and when the nest's reply omits it.
    pub policy: Option<Vec<u8>>,
    /// The room's **signed labeler set** — canonical dag-cbor
    /// `fauna_mls::room_policy::SignedRoomLabelers`, still unverified: which
    /// transparent labelers a community room's home nest applies
    /// (`conversation-rooms.md` § The three classes → *What the home nest does
    /// with its read*, purpose 2). The reader checks the signature and the room
    /// binding before rendering a name of it, and authors the next version from
    /// it. `None` when the room named none, and when the nest's reply omits
    /// it.
    pub labelers: Option<Vec<u8>>,
}

impl RoomFloor {
    /// The floor as **wrap targets** — every principal seated with both halves
    /// of the pair, in the order the room served them. A principal missing
    /// either is *unkeyable*, which the coverage rule skips rather than fails
    /// on ([`RoomRosterKnownMember::wrap_target`]).
    pub fn wrap_targets(&self) -> Vec<fauna_core::group_scope::RosterMember> {
        self.members
            .iter()
            .filter_map(|m| m.wrap_target())
            .collect()
    }
}

/// One of a community room's generations, as the **caller's own wrap** serves
/// it back — the row of `fauna.conversations.room.generations`, in this
/// crate's own vocabulary so the backend gains no wire dependency (the
/// [`RoomRosterKnownMember`] pattern).
///
/// The wrap is still sealed here: opening it needs the account's
/// group-reception secret ([`GroupReceptionKeys`]), and the open re-checks
/// [`Self::key_commitment`], so a substituted wrap is refused at the reader
/// rather than trusted from the wire
/// (`fauna_mls::wrapped_blob::group_generation_wraps::open_group_generation_key_as_entry`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoomGenerationWrap {
    /// The 32-byte content-derived generation id — what a `RoomSealed`
    /// envelope names in cleartext.
    pub generation_id: [u8; 32],
    /// The mint's key commitment, which the open verifies the unwrapped key
    /// against.
    pub key_commitment: [u8; 32],
    /// The X-Wing wrap sealed to this caller's own roster entry.
    pub wrap: Vec<u8>,
    /// The roster entry id the wrap is bound to — half the open's AAD.
    pub entry_id: [u8; 32],
    /// True for the generation new content seals under. Exactly one is the
    /// tip; the rest are retained for content already sealed.
    pub is_tip: bool,
}

/// The **generation-read** seam: a community room's key material, as far as
/// this caller is entitled to see it
/// (`conversation-rooms.md` § The three classes → *Community*).
///
/// The read half of the community class, and the twin of [`RoomRosterReader`]
/// on the same glue object. Without it a backend holds no key for a
/// `RoomSealed` envelope and the receive walk skips it — which is exactly the
/// declared absence the class carried before any app called a room kind, so an
/// **unset** seam degrades to that same honest state rather than to an error.
///
/// `home_nest_url` drives the same pick [`RoomRosterReader::read_roster`]
/// makes, for the same reason: a room's generations live on its home nest
/// alone, so a member homed elsewhere reaches them through its own nest's
/// relay — the distinct kind
/// `fauna.conversations.room.generations_remote`, never an additive field on
/// `room.generations`, whose old-nest degrade would be an empty generation
/// list, i.e. a silent wrong answer about key material.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait RoomGenerationReader: MaybeSendSync {
    /// Every generation of the room a channel hosts, **oldest first, tip
    /// last**, each carrying only this caller's own wrap. `None` on any
    /// failure — the caller's degradation is "this room's sealed records stay
    /// unopened this pass", never an error surfaced to the user, because a
    /// nest that was down is retried on the next poll.
    async fn read_generations(
        &self,
        channel_hex: String,
        home_nest_url: Option<String>,
    ) -> Option<Vec<RoomGenerationWrap>>;
}

/// The **group-reception key** seam: this account's own wrap-target keypairs
/// (`fauna.state.group-reception-key`), whose secret halves open the wraps
/// [`RoomGenerationReader`] serves.
///
/// Kept behind a seam because the records rest on the **account plane**, which
/// lives above this crate (`fauna_sync_engine::AccountRuntimeHandle::
/// group_reception_keys`) — `fauna-conversations` must not grow a
/// `fauna-sync-engine` dependency any more than it grew a capabilities one for
/// custody ([`CustodyCeremonySink`]'s priority-#2 pattern). The record type
/// itself is `fauna-core`'s, which both sides already depend on, so nothing
/// re-derives a keypair across the boundary.
///
/// **A list, not a "current key".** A generation minted before this account's
/// last reception-key rotation addressed its wrap to the key of *that* moment,
/// so a reader that kept only the newest key would lose the room's own history
/// the first time it rotated. Newest first, and the reader tries each; the
/// *first* is the account's current wrap target, which is what a seating hands
/// a room.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait GroupReceptionKeys: MaybeSendSync {
    /// Every group-reception keypair record this account holds, newest first.
    /// Empty on any failure, and empty is honest: an account that has never
    /// been seated in a keyed room holds none.
    async fn reception_keys(&self) -> Vec<fauna_core::group_generation::GroupReceptionKeyRecord>;

    /// Persist one freshly minted record, `true` once it is durable.
    ///
    /// **The write half, and the reason this seam is not a reader.** An
    /// account that has never been seated in a keyed room holds no wrap
    /// target, so the first room it founds or joins has to mint one — and the
    /// record must be durable *before* its public half goes out, because a
    /// crash between the two leaves the room addressing wraps to a secret
    /// this account no longer has (`AccountRuntimeHandle::
    /// put_group_reception_key`'s own record-then-act rule, and the shape
    /// `consent_to_group_share` already follows).
    ///
    /// `false` is a refusal to proceed, never a degradation: the caller must
    /// abandon the seating rather than hand out a key it cannot open. That is
    /// the opposite of [`Self::reception_keys`]'s empty-on-failure, and
    /// deliberately so — a read that fails leaves a room unread, a write that
    /// fails would leave it permanently unreadable.
    async fn put_reception_key(
        &self,
        record: fauna_core::group_generation::GroupReceptionKeyRecord,
    ) -> bool;
}

/// The **contact-overlay fold** seam: the outward half of the private contact
/// overlay (`contacts.md` § The private overlay → *When a person's identity
/// succeeds*), whose items rest on the account plane this crate never reaches.
///
/// [`ReadPositions`]' shape, for its reason, and implemented once, in
/// `fauna-client-account-runtime`: this trait is the **outward** direction,
/// [`Self::fold`]; the inward one is the implementation loading the manager's
/// projection (`ConversationsManager::apply_contact_overlays`) at registration
/// and whenever the items may have moved.
///
/// Unset is the honest state of a host with no account runtime (web's
/// declared absence): there is no overlay to fold.
pub trait ContactOverlayFolds: MaybeSendSync {
    /// Fold the overlay on `predecessor_hex` forward onto its witness-verified
    /// terminal successor `successor_hex`.
    ///
    /// Called synchronously from the manager, so it must not block: an
    /// implementation queues the write. The fold is idempotent and
    /// deterministic, so a repeat — the reconcile re-asking before the last
    /// fold's reload — writes nothing, and one that cannot be written is
    /// asked again at the next projection load.
    fn fold(&self, predecessor_hex: &str, successor_hex: &str);
}

/// The **refused-change log** seam: where the refused inbound scheduling
/// changes rest (`fauna.state.refused-scheduling-changes`, on the account
/// plane this crate never reaches — `inbound-scheduling-authority.md`
/// § *Where the record rests*).
///
/// [`ContactOverlayFolds`]' shape, for its reason, and implemented once, in
/// `fauna-account-seams`, over the account store. The inbound sinks never call
/// it directly: they record into the manager's
/// [`crate::refused_changes::RefusedChangeInbox`], which holds what arrives
/// before this seam is registered at the account-store-ready edge and hands
/// it over through [`Self::adopt`].
pub trait RefusedChangeLog: MaybeSendSync {
    /// Record one refused change. Called synchronously, so it must not
    /// block: an implementation queues the write.
    fn record(&self, row: fauna_core::data::RefusedSchedulingChange);
    /// Join a list held before registration into the stored one — queued,
    /// like [`Self::record`].
    fn adopt(&self, held: fauna_core::data::RefusedSchedulingChanges);
}

/// The **read-position** seam: the fauna-native rail's read markers
/// (`fauna.state.read-marker`), which rest on the account plane
/// (`conversation-read-state.md` § The read-marker record → *How the manager
/// reaches the plane*).
///
/// A seam for [`GroupReceptionKeys`]' reason — `fauna-conversations` must
/// not grow a `fauna-sync-engine` dependency — and implemented once, in
/// `fauna-client-account-runtime`, over the account store. Two directions:
/// this trait is the **outward** one, [`Self::raise`]; the inward one is the
/// implementation handing the manager the current positions
/// (`ConversationsManager::apply_read_positions`) when it is registered and
/// after every pump pass that moved one.
///
/// Unset is the honest state of a host with no account runtime (web's
/// declared absence) and of a native app before its account-store-ready edge:
/// reads stay in memory and the native rail keeps the launch floor.
pub trait ReadPositions: MaybeSendSync {
    /// The user read channel `channel_id_hex` through channel `seq` `through`.
    ///
    /// Called from the manager's one read chokepoint, synchronously, so it
    /// must not block: an implementation queues the write and may coalesce a
    /// burst into its latest value. The raise is monotone and idempotent — a
    /// raise the stored marker already covers writes nothing — and one that
    /// cannot be written changes nothing the user sees: the thread is read in
    /// memory, and the next read of it raises again.
    fn raise(&self, channel_id_hex: &str, through: u64);
}

/// The **peer-anchor store** seam: the succession witness's durable anchors
/// for other identities — held chain heads and harvested home domains —
/// which rest on the account plane as `fauna.state.peer-anchors`
/// (`identity-succession.md` § The succession statement → *the peer-profile
/// harvest* owns the anchors; `config-dissolution.md` § The `__config`
/// dissolution schedule the kind).
///
/// A seam for [`GroupReceptionKeys`]' reason — this crate must not grow an
/// account-plane dependency — implemented once, in `fauna-account-seams`,
/// over the account store, and registered on the manager at the
/// account-store-ready edge beside the read positions and the contact overlay
/// ([`crate::manager::ConversationsManager::register_peer_anchor_store`]).
/// The manager only HOLDS it: the witness, the harvest sweep and the
/// organizer-succession dialer (`fauna-client-recovery`,
/// `fauna-client-conversations`, `fauna-wasm`) reach it through the manager
/// they already hold, so no host threads a store through its own glue.
///
/// Unset is the honest state before the store-ready edge: every consumer
/// reads it as an unreadable store (the witness at TOFU grade with its
/// backoff, the harvest's retryable `StoreFailed`), never as an empty one.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait PeerAnchorStore: MaybeSendSync {
    /// Every anchor this account holds, folded — empty when none is stored.
    async fn peer_anchors(&self) -> Result<fauna_core::data::PeerAnchors, String>;

    /// Join `replica` into the stored anchors and answer them as they now
    /// stand. A harvest's seed, a walk's advance and an outrun mark are all
    /// merges of the edited anchors; nothing is ever deleted, so a behind
    /// replica rewinds nothing.
    async fn merge_peer_anchors(
        &self,
        replica: fauna_core::data::PeerAnchors,
    ) -> Result<fauna_core::data::PeerAnchors, String>;
}

/// An in-memory [`PeerAnchorStore`] for tests — the account store's door
/// reduced to its rule: a merge is `PeerAnchors::merge` (the ceiling
/// included) and answers the joined anchors. One double for every crate
/// that drives the witness or the harvest, so none re-derives the join.
#[cfg(any(test, feature = "test-helpers"))]
#[derive(Default)]
pub struct MemoryPeerAnchorStore {
    anchors: std::sync::Mutex<fauna_core::data::PeerAnchors>,
}

#[cfg(any(test, feature = "test-helpers"))]
impl MemoryPeerAnchorStore {
    /// A store already holding `anchors`.
    pub fn with(anchors: fauna_core::data::PeerAnchors) -> Self {
        Self {
            anchors: std::sync::Mutex::new(anchors),
        }
    }

    /// What the store holds now.
    pub fn current(&self) -> fauna_core::data::PeerAnchors {
        self.anchors.lock().unwrap().clone()
    }
}

#[cfg(any(test, feature = "test-helpers"))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl PeerAnchorStore for MemoryPeerAnchorStore {
    async fn peer_anchors(&self) -> Result<fauna_core::data::PeerAnchors, String> {
        Ok(self.current())
    }

    async fn merge_peer_anchors(
        &self,
        replica: fauna_core::data::PeerAnchors,
    ) -> Result<fauna_core::data::PeerAnchors, String> {
        let mut stored = self.anchors.lock().unwrap();
        *stored = stored.merge(&replica);
        Ok(stored.clone())
    }
}

/// The **room-ceremony** seam: the two doors that put this account on a
/// community room's floor, and the door that keys it.
///
/// A separate trait on the same glue object for [`RoomRosterReporter`]'s own
/// reason — [`ConversationsRpc`]'s ten implementors are mostly test doubles
/// with no room plane. Unlike the reads, every method here returns a
/// `Result`: a ceremony that half-happened is not something to degrade
/// quietly past, and the caller's next step depends on this one having
/// landed.
///
/// A backend with no ceremony seam registered cannot found a room, which is
/// the honest state of a target whose account plane this crate cannot reach
/// (web's declared W3 absence): the refusal names it rather than founding a
/// room nobody can key.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait RoomCeremonyRpc: MaybeSendSync {
    /// `fauna.conversations.room.create` — the birth ceremony
    /// (`conversation-rooms.md` § Implementation status today). `salt_hex` is
    /// the 32-byte birth salt, `policy` the canonical dag-cbor
    /// `SignedRoomPolicy` the creator signed, `reception_pubkey` the
    /// creator's wrap target. Answers the room's **derived** id, which the
    /// caller re-derives and checks rather than trusts.
    async fn room_create(
        &self,
        salt_hex: String,
        policy: Vec<u8>,
        reception_pubkey: Vec<u8>,
    ) -> Result<String, ConvRpcError>;

    /// `fauna.conversations.room.publish_generation` — publish one assembled
    /// mint (canonical dag-cbor `GroupGenerationMintRecord::Minted`) as the
    /// room's next generation. The nest *admits* it and never performs it:
    /// key authority is the room's owner and admins (§ Don't do these).
    async fn room_publish_generation(
        &self,
        room_id_hex: String,
        mint: Vec<u8>,
    ) -> Result<(), ConvRpcError>;

    /// `fauna.conversations.room.invite` — the inviter's signed act
    /// (`conversation-rooms.md` § Join rules and invites). `invite` is a
    /// canonical dag-cbor `SignedRoomInvite`; `invitee_node` is the invitee's
    /// home nest as the inviter knows it, empty for this account's own nest.
    /// Answers the role the invitee will hold once they accept.
    ///
    /// `home_nest_url` is the room's recorded home
    /// ([`ChannelHome`](crate::backends::fauna_mls::ChannelHome)): `Some(url)`
    /// when the room is homed on another nest — this account is a foreign
    /// member issuing under `member-invite` — and then the glue picks the
    /// distinct relay kind `room.invite_remote`, which this account's own nest
    /// forwards to the room's home (`conversation-rooms.md` § Join rules and
    /// invites → *A cross-nest invitation*, the foreign-inviter leg) — the
    /// same pick [`Self::room_leave`] makes off the same signal.
    ///
    /// It does **not** seat anybody. Acceptance does.
    async fn room_invite(
        &self,
        invite: Vec<u8>,
        invitee_node: String,
        home_nest_url: Option<String>,
    ) -> Result<String, ConvRpcError>;

    /// `fauna.conversations.room.accept_invite` — the act that seats **this**
    /// account on the room's floor, carrying the wrap target its roster row
    /// will hold. Answers the role now held.
    ///
    /// `home_nest_url` is the invitation's `room_node`
    /// ([`PendingRoomInvitation::room_node`]): `Some(url)` when the room is
    /// homed on another nest, and then the glue picks the distinct relay
    /// kind `room.accept_invite_remote`, which this account's own nest
    /// forwards to the room's home (`conversation-rooms.md` § Join rules and
    /// invites → *A cross-nest invitation*) — the same pick
    /// [`Self::room_leave`] makes off the same signal.
    async fn room_accept_invite(
        &self,
        room_id_hex: String,
        reception_pubkey: Vec<u8>,
        home_nest_url: Option<String>,
    ) -> Result<String, ConvRpcError>;

    /// `fauna.conversations.room.backfill_generations` — cover a newly-seated
    /// member with the generations its room's history policy allows. `wraps`
    /// are canonical dag-cbor `GroupTopupRecord` values, admitted as a batch.
    async fn room_backfill_generations(
        &self,
        room_id_hex: String,
        target_actor_id_hex: String,
        wraps: Vec<Vec<u8>>,
    ) -> Result<(), ConvRpcError>;

    /// Every invitation standing for **this** account — the read half of
    /// [`Self::room_invite`], and the only way an invitee learns the room id
    /// [`Self::room_accept_invite`] needs.
    ///
    /// The room plane mints no read kind for it: an invitation is "delivered to
    /// the invitee's home nest through the inbox plane"
    /// (`conversation-rooms.md` § Join rules and invites), so the glue reads it
    /// off the generic inbox every app already drains and hands back room
    /// vocabulary. It lives on **this** trait rather than a second seam of its
    /// own so an app cannot wire the ceremony and forget the discovery — the two
    /// are one nest-facing surface, and a room nobody can be invited *into* is
    /// not a room plane (priority #1).
    ///
    /// A **peek**: listing consumes nothing, so it is safe to poll.
    async fn room_pending_invitations(&self) -> Result<Vec<PendingRoomInvitation>, ConvRpcError>;

    /// Consume one standing invitation — after accepting it, or on a decline
    /// (which is a settle and nothing else: an invitation the invitee refuses
    /// simply stops standing). `id` is [`PendingRoomInvitation::id`].
    async fn room_settle_invitation(&self, id: i64) -> Result<(), ConvRpcError>;

    /// `fauna.conversations.room.list_invites` — the invitations pending on a
    /// community room that **this** account may withdraw
    /// (`conversation-rooms.md` § Join rules and invites → *Pending invitations
    /// are visible to whoever may withdraw them*): every one for the owner and
    /// admins, the ones it issued for any other seated member, oldest first.
    /// The nest decides the scope; a caller off the floor is refused.
    ///
    /// The room-side twin of [`Self::room_pending_invitations`], and not the
    /// same read: that one is the *invitee's* inbox, this is the *room's*
    /// standing offers. A pure read — safe to poll.
    async fn room_list_invites(
        &self,
        room_id_hex: String,
    ) -> Result<Vec<PendingRoomInvite>, ConvRpcError>;

    /// `fauna.conversations.room.revoke_invite` — withdraw the invitation
    /// pending for `invitee_hex`: the row, its standing envelope and the
    /// envelope's quota charge, as one act on the nest. The invitee is told
    /// nothing. Answers whether an invitation was consumed — `false` is an
    /// answer, not a refusal: nothing was pending, which is what the caller
    /// wanted.
    async fn room_revoke_invite(
        &self,
        room_id_hex: String,
        invitee_hex: String,
    ) -> Result<bool, ConvRpcError>;

    /// `fauna.conversations.room.remove` — unseat another principal
    /// (`conversation-rooms.md` § Roles and authorization). Answers the live
    /// floor count.
    ///
    /// **Unseating is not the severance.** The removed member still holds every
    /// generation key it was ever wrapped into, so what stops it reading *new*
    /// traffic is the rotation that follows — a caller of this method owes one
    /// ([`super::backends::fauna_mls::FaunaMlsBackend::remove_room_member`]
    /// performs both).
    async fn room_remove(
        &self,
        room_id_hex: String,
        principal_hex: String,
    ) -> Result<u32, ConvRpcError>;

    /// `fauna.conversations.room.leave` — unseat **this** account. Answers the
    /// live floor count. The owner is refused: a room is never owner-less, so
    /// an owner's exit is a transfer and then a leave.
    ///
    /// `home_nest_url` carries the same `ChannelHome` signal as
    /// [`Self::blob_put`]: `Some` routes the departure over the distinct relay
    /// kind `room.leave_remote`, which the leaver's own nest originates on to
    /// the room's home. It is required rather than convenient — a room's floor
    /// lives on its home nest alone, so the same-nest door aimed at a room
    /// homed elsewhere answers "no such room" and leaves the member seated.
    async fn room_leave(
        &self,
        room_id_hex: String,
        home_nest_url: Option<String>,
    ) -> Result<u32, ConvRpcError>;

    /// `fauna.conversations.room.set_policy` — store a replacement policy at
    /// `stored + 1`. `policy` is a canonical dag-cbor `SignedRoomPolicy`.
    /// Answers the version now stored.
    ///
    /// The nest stores it and cannot author it: what it enforces is that the
    /// signature covers the change (the **owner's** for the admin set, an
    /// owner's or an admin's for name, join rule and history policy) and that
    /// the version is exactly one past the stored one.
    async fn room_set_policy(
        &self,
        room_id_hex: String,
        policy: Vec<u8>,
    ) -> Result<u64, ConvRpcError>;

    /// `fauna.conversations.room.set_labelers` — store a community room's
    /// replacement labeler set at `stored + 1`. `labelers` is a canonical
    /// dag-cbor `SignedRoomLabelers`. Answers the version now stored.
    ///
    /// The policy's sibling record and governed like it: signed by the owner
    /// or an admin, stored and never authored by the nest — which also refuses
    /// any id it would not run (anything but a published `wasm` or
    /// `text-model` labeler).
    async fn room_set_labelers(
        &self,
        room_id_hex: String,
        labelers: Vec<u8>,
    ) -> Result<u64, ConvRpcError>;

    /// `fauna.conversations.room.transfer_ownership` — hand the room to
    /// another live user member. `policy` is the replacement signed by the
    /// **outgoing** owner, whose role in the previous version is what lets it
    /// change the owner field. Answers the version now stored.
    ///
    /// Its own door rather than a field of [`Self::room_set_policy`] because it
    /// moves the roster row's role as well as the bytes.
    async fn room_transfer_ownership(
        &self,
        room_id_hex: String,
        policy: Vec<u8>,
    ) -> Result<u64, ConvRpcError>;

    /// `fauna.conversations.room.set_reception_key` — supply, or rotate, the
    /// wrap target of the seat **this** account already holds
    /// (`community-rooms.md` § Implementation status today, *A seat gains or
    /// rotates its wrap target*). The seat keeps its roster entry; a key
    /// already set is replaced. Answers what the seat still owes.
    async fn room_set_reception_key(
        &self,
        room_id_hex: String,
        reception_pubkey: Vec<u8>,
    ) -> Result<RoomReceptionKeyBound, ConvRpcError>;
}

/// The room plane's four **nest-backed** client seams, registered together:
/// the floor-roster report and read, the community class's generation read,
/// and its ceremony (`conversation-rooms.md` § The floor roster,
/// § The three classes → *Community*).
///
/// Every glue site registers them through this one bundle
/// (`FaunaMlsBackend::set_room_seams`, or its session twin) rather than one
/// setter per seam, so no app can wire two of them and forget the other two.
/// Two glue sites once did exactly that and were left unable to found, join or
/// open a community room. A new nest-backed room seam becomes a field here,
/// which every glue site then picks up.
///
/// The fifth room seam, [`GroupReceptionKeys`], is not in the bundle: it rests
/// on the account plane rather than on the nest, so it is registered from the
/// account-store-ready edge (`fauna_client_account_runtime::conversation_seams`).
pub struct RoomSeams {
    pub reporter: std::sync::Arc<dyn RoomRosterReporter>,
    pub reader: std::sync::Arc<dyn RoomRosterReader>,
    pub generations: std::sync::Arc<dyn RoomGenerationReader>,
    pub ceremony: std::sync::Arc<dyn RoomCeremonyRpc>,
}

impl RoomSeams {
    /// All four seams from the one conversations-RPC object that implements
    /// them, which is what every production glue site holds
    /// (`NestConversationsRpc` natively, `WsConversationsRpc` on web).
    pub fn from_rpc<R>(rpc: &std::sync::Arc<R>) -> RoomSeams
    where
        R: RoomRosterReporter + RoomRosterReader + RoomGenerationReader + RoomCeremonyRpc + 'static,
    {
        RoomSeams {
            reporter: rpc.clone(),
            reader: rpc.clone(),
            generations: rpc.clone(),
            ceremony: rpc.clone(),
        }
    }
}

/// What [`RoomCeremonyRpc::room_set_reception_key`] did to this account's
/// seat, in this crate's own vocabulary (the [`RoomGenerationWrap`] pattern).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoomReceptionKeyBound {
    /// The seat's roster entry — unchanged by a rotation, minted for a seat
    /// that had none.
    pub entry_id: [u8; 32],
    /// True when a different key was bound before and this call replaced it.
    pub rotated: bool,
    /// The room's current generation when this seat holds no wrap for it:
    /// what an owner or admin's top-up covers, or what a fresh mint by this
    /// seat names as its parent. `None` when covered, or when the room has no
    /// generation yet.
    pub uncovered_tip: Option<[u8; 32]>,
}

/// One invitation standing for this account, as
/// [`RoomCeremonyRpc::room_pending_invitations`] serves it — **unverified**.
///
/// `signed_invite` is the inviter's signed act verbatim, which is what names the
/// room, the invitee, the role and the policy version. It is deliberately still
/// bytes here: the seam is a transport, and the record is a *claim* until the
/// reader decodes it and checks the signature binds it to the named inviter
/// (`FaunaMlsBackend::pending_room_invitations` is that reader). Nothing on this
/// struct restates what the signed bytes say, so there is no unsigned copy for a
/// reader to be shown instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingRoomInvitation {
    /// Opaque handle this invitation is settled by
    /// ([`RoomCeremonyRpc::room_settle_invitation`]).
    pub id: i64,
    /// Canonical dag-cbor `fauna_mls::room_policy::SignedRoomInvite`.
    pub signed_invite: Vec<u8>,
    /// The room's home nest when it is not this account's own; `None`
    /// same-nest. On a cross-nest delivery this account's nest wrote it from
    /// the delivering peer's verified identity (`conversation-rooms.md`
    /// § Join rules and invites → *A cross-nest invitation*), and it is what
    /// the acceptance is relayed to.
    pub room_node: Option<String>,
}

/// One invitation pending on a community room, as
/// [`RoomCeremonyRpc::room_list_invites`] serves it — the room-side row, in
/// this crate's own vocabulary (the [`RoomRosterKnownMember`] pattern).
///
/// **The home nest's word, like the floor roster it is read beside.** A
/// community room's floor is its home nest's to state, and so is what stands
/// pending on it; nothing here is rendered as a principal's *signed* claim.
/// What it gates is a withdrawal the nest judges again on its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingRoomInvite {
    /// Who was invited — what [`RoomCeremonyRpc::room_revoke_invite`] names.
    pub invitee: fauna_core::identity::ActorId,
    /// The invitee's canonical `handle@domain` when the nest knows one.
    /// Display only.
    pub invitee_handle: Option<String>,
    /// The identity that issued it. Attribution: never re-pointed by a
    /// succession.
    pub inviter: fauna_core::identity::ActorId,
    /// The inviter's canonical `handle@domain` when the nest knows one.
    /// Display only.
    pub inviter_handle: Option<String>,
    /// The rank on offer — admin or member, never owner.
    pub role: crate::room::RoomRole,
    /// When the invitation was (last) issued, epoch millis.
    pub invited_at_ms: i64,
    /// Whether the accept door would still seat the invitee — its own
    /// judgement, run as the list was served. `false` is an invitation lapsed
    /// in waiting: it seats nobody, and is listed so it can be cleared.
    pub still_acceptable: bool,
}

/// Ingest seam for **custody-ceremony payloads**
/// (`docs/goal/architecture/account-data-plane.md` § Replica posture → *The
/// custody grant + ceremony*, W8.4 (account-data-plane.md § Workstreams)): the receive loop hands every
/// [`fauna_mls::types::ChannelMessageBody::Custody`] record's verbatim bytes
/// here as a thread *effect* — never a chat bubble — and moves on.
///
/// The bytes are a **claim** until the implementor decodes and verifies them
/// (`fauna_core::custody_ceremony::verify_custody_*` — signer bound to the
/// MLS-authenticated `sender`, an offer's addressee bound to the reading
/// actor). Kept behind this seam so `fauna-conversations` gains no custody
/// semantics or `fauna-client-capabilities` dependency (the
/// [`FolderCustodySink`] / [`SuccessionWitness`] priority-#2 pattern); the
/// glue (`fauna_client_conversations`) wires the ceremony machine in. A
/// session that registers no sink skips the record — tallied, and honest:
/// the payload sits store-and-forward in the channel history for a capable
/// session (the same degradation an older build's failed decode gives).
///
/// **Must capture durably before returning `true`**: a consumed MLS
/// application message cannot be re-decrypted (the succession-parking
/// lesson), so a `true` asserts the payload is on record (the implementor's
/// custody-ceremony row write succeeded). A `false` is tallied
/// (`CustodyPayloadCounts`) but does NOT stall the channel walk — bubbles
/// must keep flowing past a transient CAS hiccup. The residual is honest
/// and healed twice over: the default 0-seeded per-launch cursor re-feeds
/// every custody record on relaunch (ingest is idempotent per grant id),
/// and an offer/deliver that never lands decays to re-offer at the
/// ceremony's own shelf life. Best-effort by contract — the inbound poll
/// calls this inline, so implementations bound their own I/O rather than
/// stall the feed.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait CustodyCeremonySink: MaybeSendSync {
    /// Ingest one ceremony payload: `channel_hex` is the carrying channel,
    /// `sender` the MLS-authenticated author, `bytes` the verbatim
    /// `CustodyCeremonyMessage` encoding. Returns whether the payload is now
    /// durably captured.
    async fn custody_payload(
        &self,
        channel_hex: &str,
        sender: fauna_core::identity::ActorId,
        bytes: &[u8],
    ) -> bool;

    /// Ingest one **custody receipt** — the periodic A7 attestation, arriving
    /// as a [`fauna_mls::types::ChannelMessageBody::CustodyReceipt`] body
    /// rather than a ceremony payload (that split, and why, is documented on
    /// the variant). `bytes` is the verbatim signed envelope; the
    /// implementation verifies it against the custodian key its own grant
    /// named before folding it onto the custody row. Returns whether the
    /// receipt is now durably recorded — `false` covers both "could not
    /// verify" and "could not write", which is the honest report either way:
    /// coverage the owner cannot check is coverage it must not render.
    ///
    /// Defaulted so an implementation that predates receipts still compiles
    /// and simply records none; the row then reads *no receipt yet*, which is
    /// a state `ui/nests.md` § Trust facet already requires be rendered.
    async fn custody_receipt(
        &self,
        _channel_hex: &str,
        _sender: fauna_core::identity::ActorId,
        _bytes: &[u8],
    ) -> bool {
        false
    }
}

/// Ingest seam for **share-set endpoint advertisements**
/// (`docs/goal/behavior/p2p.md` § Cross-user shared-set transfer →
/// *Discovery*; the W8 share twin's slice F): the receive loop hands every
/// [`fauna_mls::types::ChannelMessageBody::ShareEndpoints`] record's verbatim
/// bytes here as a thread *effect* — never a chat bubble — and moves on.
///
/// The bytes are a **claim**. The implementor decodes them and binds the
/// self-asserted `member_actor` to the MLS-authenticated `sender` and to the
/// carrying channel (`fauna_peer_share::bind_share_advertisement`) before
/// writing any dial row; a mismatch is refused, never repaired. Kept behind
/// this seam for the [`CustodyCeremonySink`] reason exactly:
/// `fauna-conversations` gains no share-plane semantics and no
/// `fauna-peer-share` dependency, so the gated plane stays gated and this
/// crate stays out of the excision spine (priority #2).
///
/// **Best-effort, and honestly so.** Unlike a custody payload, a lost
/// advertisement costs nothing irrecoverable: the nest remains the always-on
/// source the contract names, so a session with no sink — or one whose write
/// fails — simply dials no peer for that set until the next advertisement.
/// That is why there is no re-drive machinery here and why `false` never
/// stalls the walk.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait ShareEndpointsSink: MaybeSendSync {
    /// Ingest one advertisement: `channel_hex` is the carrying channel (which
    /// **is** the shared set), `sender` the MLS-authenticated author, `bytes`
    /// the verbatim `ShareEndpoints` encoding. Returns whether a dial row is
    /// now durably cached — `false` covers both "refused the binding" and
    /// "could not write", which is the honest report either way: a candidate
    /// this replica could not verify is a candidate it must not dial.
    async fn share_endpoints(
        &self,
        channel_hex: &str,
        sender: fauna_core::identity::ActorId,
        bytes: &[u8],
    ) -> bool;
}

/// Launch seam for the **cross-device MLS group-state sync plane** — the async,
/// crypto-bearing half of what linux drives on login via `wire_mls_state_sync` +
/// `attach_replica_autosave` (`docs/goal/behavior/devices.md` § Cross-device MLS
/// group-state sync, slice 5). Run **once, before the first poll** at the top of
/// [`crate::session::ConversationsSession::start_receive_loop`] (design §5
/// restore-before-first-poll), so the loop resumes each restored channel from its
/// watermark instead of seq 0.
///
/// Kept behind this **MLS-type-free** seam (the launcher owns the `MlsStateSync` /
/// `orchestration` / `FaunaMlsBackend` handles; this crate learns nothing of them)
/// so `fauna-conversations` gains no `fauna-client-mls-sync` dependency — the launch
/// twin of how [`SchedulingSink`] keeps the calendar client out of this crate
/// (priority #2). The native FFI factory (`fauna-ffi`
/// `FfiNestClient::conversations_session`) injects one via
/// [`crate::session::ConversationsSession::set_mls_sync_launcher`], so all three
/// native legs (apple/windows/android) inherit the plane with no per-app glue;
/// linux wires it through its own glib path and web through its wasm bootstrap, so
/// both leave this unset and [`start_receive_loop`] no-ops here.
///
/// [`start_receive_loop`]: crate::session::ConversationsSession::start_receive_loop
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait MlsSyncLauncher: MaybeSendSync {
    /// Restore the state replica, inject the device-owned-epoch gate + cursor, and
    /// attach the debounced replica autosave — the whole design-§5 launch. Runs
    /// before the receive loop's first poll; a load failure is self-logged and
    /// leaves the session single-device (the un-injected gate is today's optimistic
    /// behavior) — it never aborts the loop.
    async fn launch(&self);
}

/// Launch seam for the **peer-anchor harvest sweep** — the producer half of the
/// member-path succession anchor (`identity-succession.md` § The succession
/// statement → *the peer-profile harvest*). The third of this file's launcher
/// seams, and it exists for exactly the reason the other two do.
///
/// A member who joined a group by Welcome holds neither anchor the
/// [`SuccessionWitness`] admits — the roster row carries no handle and nothing
/// seeded a cached head — so without this sweep the witness has no anchor for
/// precisely the peers an in-group succession statement is *about*, and every
/// such statement degrades to the bare add.
///
/// **Why a seam rather than a spawn at build time.** The sweep is driven by
/// `fauna-client-recovery`, which depends on this crate, so its types cannot be
/// named here; and spawning it needs a runtime context the session factories do
/// not all have (the native FFI factory is a *synchronous* UniFFI export, and
/// `fauna-ffi` deliberately owns no fallback runtime — `account_runtime.rs`'s
/// "there is no fallback runtime on purpose"). Injecting synchronously and
/// launching from [`start_receive_loop`]'s prologue — already inside the app's
/// own runtime — is how [`MlsSyncLauncher`] and [`IndexBuilderLauncher`] answer
/// the identical problem, so all 7 apps inherit the sweep with **no per-app
/// glue** rather than each owing a post-auth call it can forget. An app that
/// forgets is not a build error; it is a member who silently renders "a
/// stranger joined" forever, which is the failure this seam is here to make
/// structurally impossible.
///
/// Launched **first** in the prologue, ahead of the awaited replica restore:
/// the sweep is fire-and-forget, and the race it has to win is against a
/// *ceremony's* statement, so every second it is not running is a member who
/// may never anchor (the measured 2026-08-10 ordering race — same §).
///
/// [`start_receive_loop`]: crate::session::ConversationsSession::start_receive_loop
/// [`SuccessionWitness`]: crate::backend::SuccessionWitness
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait PeerAnchorSweepLauncher: MaybeSendSync {
    /// Start the roster sweep for this session. Returns as soon as the sweep is
    /// running — the implementation owns its own cadence and ends with the
    /// session, so this must not be awaited to completion.
    async fn launch(&self);
}

/// Launch seam for the client's **content-index builder** — the twin of
/// [`MlsSyncLauncher`], for the same reason and at the same moment.
///
/// Resolving a builder is asynchronous (the mail/calendar index-segment key
/// derives from the actor's MSEK (`fauna.state.mail`), and resuming reads
/// the published manifest off the `__index` rail), but the observer it produces
/// **must be registered before the first receive poll**: the client keeps no
/// restart-durable mail cursor, so every launch re-pages the whole mailbox and
/// the index seam fires for all of it — an observer registered late misses that
/// launch's mail entirely (`docs/goal/behavior/content-index.md` § Ingest
/// triggers, v1). [`start_receive_loop`] awaits this in its prologue, before the
/// poll task spawns, which makes the ordering **structural** rather than a race
/// a spawned resume would have to win.
///
/// Kept behind this **index-type-free** seam (the launcher owns the builder, the
/// rail publisher and the flush debounce; this crate learns nothing of tantivy)
/// so `fauna-conversations` gains no `fauna-client-index` dependency — the same
/// shape that keeps `fauna-client-mls-sync` and the calendar client out of here
/// (priority #2). Web registers none: there is no browser tantivy, so the SPA is
/// structurally unaffected (`content-index.md` § Where queries run).
///
/// [`start_receive_loop`]: crate::session::ConversationsSession::start_receive_loop
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait IndexBuilderLauncher: MaybeSendSync {
    /// Resume this actor's index builder and return the observer to register.
    ///
    /// `None` means "this launcher will never produce an observer" and is a
    /// **normal, non-fatal** outcome, not an error the loop should act on. It is
    /// *not* the answer for an arm that merely cannot resume **yet**: a launcher
    /// whose arm set can still grow returns its (possibly still-empty) container
    /// here and fills it through [`Self::ensure_arm`] — which is exactly what
    /// the native launcher does, so it never answers `None`.
    async fn launch(&self) -> Option<std::sync::Arc<dyn crate::index_sink::MessageIndexObserver>>;

    /// Attach `kind`'s arm if it is missing and can be built *now*; answer
    /// whether it **newly** attached.
    ///
    /// The one-shot [`Self::launch`] is not sufficient on its own, because an
    /// arm's precondition can arrive mid-session: mail enabled after login
    /// leaves the MSEK absent at launch, so the mail arm would
    /// never exist for the rest of the process's life — the mail arrives and
    /// renders, and is silently never staged for search until a restart
    /// (`docs/goal/behavior/content-index.md` § Ingest triggers, v1 → *Where the
    /// builder lives*). A transient rail failure at launch strands an arm the
    /// same way. So the loop re-asks on every sweep of that kind, **before** the
    /// poll, mirroring the lazy re-check the receive path already performs
    /// (`MailKeyCache::get` re-derives on every call while unset — which is why
    /// receiving already tolerates mid-session enablement and indexing did not).
    ///
    /// Attaching must **mutate** the container [`Self::launch`] handed back, not
    /// mint a new one: the seam holds exactly one observer slot, and replacing
    /// it would discard the already-running arms' live state — their seeded
    /// re-index guard and anything staged but not yet flushed.
    ///
    /// Answering `true` tells the loop this kind's catch-up window must be
    /// **reopened**, because a fresh arm has a fresh backlog: the boundary the
    /// loop already closed says nothing about an arm that did not exist then.
    /// Kept per kind for the reason the boundary itself is
    /// (`MessageIndexObserver::observe_catch_up_complete`) — reopening one
    /// kind's window must not disturb another's.
    ///
    /// Default: `false` — the honest answer for a launcher whose arms all
    /// resolve at [`Self::launch`] time and can never grow later.
    async fn ensure_arm(&self, _kind: crate::index_sink::IndexableKind) -> bool {
        false
    }

    /// A **third-ingest-class corpus changed on the nest** — re-run that kind's
    /// reconcile walk now instead of waiting for the next sweep
    /// (`content-index.md` § Ingest triggers, v1 → the class template, piece 3:
    /// *the walk carries correctness, the push carries freshness*).
    ///
    /// Today's only caller is `fauna.addressbook.changed`, which is why the
    /// argument is the corpus rather than a kind: a third-class corpus is
    /// nest-resident and externally mutated, so the signal names *what moved on
    /// the nest*, not what a local seam observed. Posts, files and media join
    /// the same enum as their arms land.
    ///
    /// **Deliberately NOT defaulted**, unlike [`Self::ensure_arm`] above. Every
    /// silent-no-op in this subsystem has cost a dark arm — the drafts corpus
    /// swallowed by a wrapper that never overrode a defaulted seam method, and
    /// the contacts corpus that staged without pulsing — so a new launcher must
    /// answer this question rather than inherit "do nothing" from the compiler.
    /// A launcher with no walk writes an empty body and says so.
    async fn corpus_changed(&self, corpus: NestCorpus);
}

/// Which nest-resident corpus a [`IndexBuilderLauncher::corpus_changed`] signal
/// is about. Not [`crate::index_sink::IndexableKind`]: that enum names the kinds
/// arriving through the *observer seam* (mail, conversations), and a third-class
/// corpus by definition has no seam to arrive through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NestCorpus {
    /// The actor's CardDAV address books (`fauna.addressbook.changed`).
    AddressBook,
    /// The actor's readable Sync- and backup-type folders
    /// (`fauna.sync.changed`) — the File arm's corpus
    /// (`content-index.md` § Ingest triggers, v1 → *The files/media arms are
    /// SCOPED*).
    ///
    /// Corpus-wide rather than per-set even though the wire nudge names one set:
    /// the walk is a cross-set `fauna.media.list` drain with no per-set cursor to
    /// narrow, so telling it *which* set moved would buy nothing it could act on.
    Files,
}

/// Transport seam for the **durable inbox-apply backstop** — the missed-push
/// recovery rail (`docs/goal/architecture/api-layers.md` § Inbox & Messaging,
/// layer 3). The best-effort `ConvPushEvent::Welcome` is the prompt; this drain
/// is the delivery *guarantee*: a Welcome whose push was missed (client offline
/// at push time) is recovered from the per-actor durable queue
/// (`fauna.inbox.{fetch,ack}`) on the next backstop tick.
///
/// One self-contained method runs **one** drain pass. The native glue impl
/// (`fauna_client_conversations::NestInboxDrainSource`) owns the whole pass — it
/// drives the **shared** orchestration `fauna_client_inbox::drain` (fetch → decode
/// the canonical `InboxEnvelope` → dispatch by `kind` → ack the durably-applied
/// ids; priority #2, written once for native + web) over an
/// `InboxClient<Arc<NestClient>>`, with an `InboxApply` that routes a Welcome back
/// into this session's ingest via [`crate::session::ConversationsSession::ingest_welcome_by_kind`]
/// (the SAME free fn the push arm calls — no fork). Kept behind this object-safe,
/// **protocol-agnostic** seam (no `fauna-protocol` / `fauna-client-inbox` types
/// cross it) so `fauna-conversations` gains no `RpcRequester` dependency — the
/// drain twin of how [`SchedulingSink`] / [`InboundMailSource`] keep the transport
/// out of this crate (priority #2). A session without the seam registered simply
/// never runs the backstop (push-only — the pre-layer-3 behaviour).
///
/// **`ack` means durably applied:** the impl's `InboxApply` returns `Ok` only when
/// the item was actually applied, so a crash before apply re-delivers rather than
/// dropping (the data-loss fix the fetch/ack split exists for). The drain is the
/// *only* surface that acks (display-only badges peek-never-ack).
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait InboxDrainSource: MaybeSendSync {
    /// Run one drain pass: fetch the caller's undelivered inbox items, durably
    /// apply each (Welcome → ingest; un-wired kinds left un-acked), ack the
    /// applied ids, paging while making progress. Returns `Err` with a
    /// human-readable reason on a `fetch` / `ack` transport failure (per-item
    /// apply errors are absorbed by the drain as skips, never surfaced here); the
    /// loop logs it and retries on the next tick. The pass is idempotent — a
    /// Welcome already applied via the push arm re-applies as a no-op (the MLS
    /// engine + channel cursor dedup).
    async fn drain_once(&self) -> Result<(), String>;
}

/// The payload of [`BackendError::Transport`] — a **transport seam's own
/// best-effort user string**, and the reason that variant may pass through
/// [`user_detail`](BackendError::user_detail) verbatim.
///
/// It is a newtype with a private field so the taxonomy is enforced by the
/// compiler rather than by discipline. `Transport(e.to_string())` — wrapping
/// some inner library's error text because it happened to be at hand — no
/// longer type-checks anywhere outside this module; a producer must instead
/// reach for [`BackendError::transport_from_seam`], whose name asks the question
/// the bug kept getting wrong: *is this string the seam's user-facing sentence?*
/// If it is not, the value is a diagnostic and belongs in
/// [`Internal`](BackendError::Internal).
///
/// This exists because the map-side pins cannot see producers. Seven MLS-engine
/// sites in `backends::fauna_mls` wrapped raw openmls diagnostics in `Transport`
/// and put them straight onto `error-message` on all 7 apps, surviving the
/// 2026-08-02 taxonomy sweep precisely because nothing at the construction site
/// disagreed with them (`conversations.md` § Errors & edge cases).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeamMessage(String);

impl SeamMessage {
    /// The user-facing sentence itself.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SeamMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Error)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Error))]
#[cfg_attr(feature = "uniffi", uniffi(flat_error))]
pub enum BackendError {
    #[error("not implemented for this rail/flavor combination")]
    NotSupported,
    /// A transport seam failed, carrying **that seam's** user-facing string (see
    /// [`SeamMessage`], which is also why this variant's payload cannot be built
    /// from an arbitrary error's `to_string()`).
    #[error("transport failure: {0}")]
    Transport(SeamMessage),
    /// The nest is running an **outdated version** (`fauna.nest.outdated`,
    /// `version-compatibility.md` Dimension 4) — distinct from
    /// [`Transport`](Self::Transport) so the conversations UI routes it to a
    /// non-retry "update your nest" affordance instead of an auto-retry, and
    /// renders this already-localized `message` rather than the raw transport
    /// `details`. Built from [`ConvRpcError::NeedsUpdate`] via the seam glue's
    /// shared `RpcError::action()`/`localized()` classifier (so every app
    /// shares the one mapping; priority #2). As a `uniffi(flat_error)` variant it
    /// crosses the FFI boundary by name (the field is dropped, but the `Display`
    /// — this `message` — is carried), so native apps catch the variant
    /// exactly as they catch `FfiError::NestOutdated` on the connect path.
    #[error("{message}")]
    NeedsUpdate { message: String },
    #[error("authentication required")]
    AuthRequired,
    /// A **product refusal** — the payload IS the user-facing sentence, drawn
    /// from the i18n table (`fauna_i18n::strings`, e.g. the inline-ceiling
    /// refusal's `error.email.too_large`) or already localized upstream
    /// (`ConvRpcError::Rejected`'s classified `message`). It rides `{message}`
    /// inside `conversations.unified.error_send` verbatim
    /// (`ConversationsManager::send` → [`SendState::failed`] via
    /// [`user_detail`](Self::user_detail)), so `conversations.md`
    /// § Architectural rules 3 ("never hardcode English") governs every
    /// producer: **constructing a `Refusal` from raw English is the bug this
    /// variant's name exists to make visible.** Its `Display` is the payload
    /// alone — no variant tag (a tag is hardcoded English in front of a
    /// localized sentence on all 7 apps at once, which is what the retired
    /// `Other`'s `"other: {0}"` did until 2026-07-30).
    #[error("{0}")]
    Refusal(String),
    /// A **developer diagnostic** — "no key package available for {actor}",
    /// "serialize welcome: {e:?}", an internal-signal string. The payload is
    /// **never rendered to the user**: [`user_detail`](Self::user_detail) maps
    /// this variant to the localized generic send failure
    /// (`error.send.generic`) and the raw payload belongs in the log
    /// (`{e}` / `{e:?}` at the catch site). Split from the retired catch-all
    /// `Other` (taxonomy ratified 2026-08-02, `conversations.md` § Errors &
    /// edge cases): one variant carried both product refusals and diagnostics,
    /// so ~30 raw internals reached `error-message` verbatim on all 7 apps.
    #[error("{0}")]
    Internal(String),
    /// A Welcome this device holds **no addressed key package** for — the
    /// multi-device steady state, never a fault
    /// (`docs/goal/behavior/devices.md` § Cross-device MLS group-state sync →
    /// *Who may consume a Welcome — any device that holds its key*). Carries
    /// [`fauna_mls::error::MlsError::NotAddressedToThisDevice`] across the
    /// backend seam, unchanged in meaning.
    ///
    /// Split from [`Internal`](Self::Internal) for the reason that variant was
    /// split from the retired `Other`: one variant carrying both a diagnostic
    /// and an expected outcome makes the two indistinguishable at every catch
    /// site. Here the catch site is the receive loop's push arm, which logged
    /// every device-not-addressed push at `error` — the same line a genuine
    /// ingest fault produces — on the long-open device of any two-device
    /// account. Producers/consumers may add nothing to that: the ingest is
    /// **not retried** and the row is **not acked**; the group reaches this
    /// device by the other door (the targeted sibling-group import) once the
    /// minting sibling's flush lands.
    ///
    /// Never rendered: like `Internal` it maps to the generic send sentence in
    /// [`user_detail`](Self::user_detail), and no send path can produce it.
    #[error("welcome addressed to no key package this device holds")]
    WelcomeNotAddressedHere,
}

impl BackendError {
    /// Build a [`Transport`](Self::Transport) from a **transport seam's own
    /// user-facing string** — the only way to construct that variant outside
    /// this module (see [`SeamMessage`]).
    ///
    /// Callers are the seams themselves: [`ConvRpcError`]'s `From` impl below,
    /// and the mail/nostr sinks whose `Err(String)` is likewise a best-effort
    /// user sentence. **If the string you have is an inner library's error text,
    /// this is the wrong constructor** — that is a diagnostic, so use
    /// [`Internal`](Self::Internal) and let `user_detail` map it to the generic
    /// sentence.
    pub fn transport_from_seam(message: impl Into<String>) -> Self {
        Self::Transport(SeamMessage(message.into()))
    }

    /// The `{message}` detail for the send-failure slot — the single map from
    /// this enum onto **user-renderable text** (`conversations.md` § Errors &
    /// edge cases, send-slot taxonomy). Product statements pass their payload
    /// through verbatim ([`NeedsUpdate`](Self::NeedsUpdate) /
    /// [`Refusal`](Self::Refusal) — both localized at the producer, and
    /// [`Transport`](Self::Transport), whose payload is the seam's best-effort
    /// user string by [`ConvRpcError`]'s contract); conditions and diagnostics
    /// map to their i18n sentence with the raw detail left to the caller's log
    /// line. Render paths use this, never `Display` (which keeps raw payloads
    /// and tags for logs/`Debug`).
    pub fn user_detail(&self) -> String {
        use fauna_i18n::strings::error::send;
        match self {
            Self::NeedsUpdate { message } => message.clone(),
            Self::Refusal(text) => text.clone(),
            Self::Transport(message) => message.as_str().to_string(),
            Self::AuthRequired => send::AUTH_REQUIRED.to_string(),
            Self::NotSupported => send::NOT_SUPPORTED.to_string(),
            Self::Internal(_) | Self::WelcomeNotAddressedHere => send::GENERIC.to_string(),
        }
    }
}

impl From<ConvRpcError> for BackendError {
    /// Collapse the seam's three-way classification onto `BackendError`,
    /// preserving the version-mismatch signal (the only *new* routing this fix
    /// adds): `NeedsUpdate` keeps its own variant (non-retry update affordance),
    /// `Rejected` → [`Refusal`](BackendError::Refusal) (its classified `message`
    /// is already user-facing; show it, no retry), `Transient` →
    /// [`Transport`](BackendError::Transport) (the pre-existing retryable path;
    /// its `message` is the seam's user string). Lets every seam call site that
    /// built `BackendError::Transport` by hand become a bare `?`.
    fn from(e: ConvRpcError) -> Self {
        match e {
            ConvRpcError::NeedsUpdate { message } => BackendError::NeedsUpdate { message },
            ConvRpcError::Rejected { message } => BackendError::Refusal(message),
            ConvRpcError::Transient { message } => BackendError::transport_from_seam(message),
            // A `StaleCommit` reaching here means a gate-send escaped the rebase
            // loop uncaught — a shouldn't-happen diagnostic, so it routes to
            // `Internal` (the user sees the generic send failure and may retry;
            // the seq detail is for the log). NOT `Transport`: that variant's
            // payload is user-facing by `ConvRpcError`'s contract, and this
            // string is not.
            ConvRpcError::StaleCommit { latest_commit_seq } => BackendError::Internal(format!(
                "commit gate rejected (latest_commit_seq={latest_commit_seq:?})"
            )),
        }
    }
}

/// What this account's own **positive evidence** says about a foreign domain —
/// the second input to [`classify_foreign_non_answer`], and the reason a
/// non-answer is read as a failed lookup rather than a licence to fall through
/// to plaintext email (`docs/goal/architecture/federation.md` § Peer-auth model
/// → *Discovery-failure semantics*, case 2).
///
/// **Three states, not two.** The ruling's evidence clause (b) is scoped to
/// *this account's conversations*, but the evidence a client can consult is
/// whatever it has **loaded** — and the conversation history lives on the home
/// nest, restored over the network at launch
/// (`fauna_client_mls_sync::orchestration::restore_and_wire`). Between launch
/// and restore an account that has conversed with a peer for months holds
/// nothing that names it. Collapsing that window into "not known" is what let a
/// cold start downgrade a known Fauna peer to plaintext SMTP: the carve-out is
/// justified by the user holding **no Fauna expectation**, and an unread store
/// cannot establish that. Only [`Self::AbsentFromLoadedEvidence`] — a positively
/// *established* absence — opens the email arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DomainEvidence {
    /// Positive evidence that a Fauna nest serves the domain: the local actor's
    /// own handle domain (clause a), a `TypedAddress::Fauna` participant of a
    /// loaded thread (clause b), or a nest there that answered a resolve earlier
    /// this session (clause c). A non-answer is a **failed lookup**.
    KnownFauna,
    /// This account's conversations **have been loaded**, and nothing in them
    /// names the domain. The only state that establishes "the user holds no
    /// Fauna expectation here", so the only one the email carve-out rests on.
    AbsentFromLoadedEvidence,
    /// This account's conversations have **not been loaded yet** — the replica
    /// restore has not landed, so clause (b) is unevaluable. Not evidence of
    /// absence; treated as a failed lookup, exactly like [`Self::KnownFauna`].
    Unloaded,
}

/// What a foreign nest's **non-answer** to the anonymous `fauna.actor.by_handle`
/// discovery probe means for the rail-resolution chain — the decision half of
/// [`classify_foreign_non_answer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ForeignNonAnswer {
    /// `true` → the resolve is a **terminal** `ResolveResult::Error`: no later
    /// rail may claim the address, the picker commits no chip, nothing is sent.
    /// `false` → `ResolveResult::NotFound`, and the chain falls through to the
    /// email rail.
    pub terminal: bool,
    /// `true` → this reply is positive evidence that a Fauna nest serves the
    /// domain, so the caller records it as a known Fauna domain.
    pub proves_fauna_domain: bool,
}

/// Decide what a foreign discovery **non-answer** means, from the seam's
/// structural error and the positive evidence the client already holds —
/// `docs/goal/architecture/federation.md` § Peer-auth model → *Discovery-failure
/// semantics*, cases 1 and 2.
///
/// Split out of [`crate::backends::fauna_mls::FaunaMlsBackend::resolve_foreign`]
/// so the rule is one named, directly testable function rather than three match
/// arms reachable only through a live backend. That matters here more than it
/// usually would: the *inputs* to this decision are produced two crates away
/// (`fauna-protocol`'s `RpcError::action()`, then `fauna-client-conversations`'s
/// `remote_by_handle_outcome`), and the security defect this function's
/// extraction closes survived precisely because the suites on either side of
/// that classifier had nothing that joined them — each half was tested against a
/// hand-built value, so nothing ever asked *which wire codes* reach which arm.
/// A test upstream can now drive a real wire code through both and land here.
///
/// - [`ConvRpcError::Rejected`] — a nest answered and **disowned** the handle or
///   the domain (case 1: `domain_not_local`, `handle.invalid`). Fall through to
///   email, and vouch for nothing: a refusal is not evidence the nest *serves*
///   the domain. The seam guarantees this variant is the closed, allowlisted
///   set (`remote_by_handle_outcome`), never the classifier's open default.
/// - [`ConvRpcError::NeedsUpdate`] — a Fauna nest the client cannot talk to.
///   Terminal, never email, and it proves the domain is Fauna.
/// - Anything else — **no usable answer** (case 2: DNS / connect / TLS / WS /
///   timeout / protocol fault, or a transient wire refusal such as a rate
///   limit). The transport-error kind is deliberately not consulted, because a
///   browser cannot see it and a rule reading it would be native-only
///   (priority #1); the decision is made from positive evidence alone — and
///   only a [`DomainEvidence::AbsentFromLoadedEvidence`] verdict, an absence
///   this client actually established, falls through to email.
pub fn classify_foreign_non_answer(
    err: &ConvRpcError,
    evidence: DomainEvidence,
) -> ForeignNonAnswer {
    match err {
        ConvRpcError::Rejected { .. } => ForeignNonAnswer {
            terminal: false,
            proves_fauna_domain: false,
        },
        ConvRpcError::NeedsUpdate { .. } => ForeignNonAnswer {
            terminal: true,
            proves_fauna_domain: true,
        },
        // Terminal unless the account's evidence was actually READ and came
        // back empty. `Unloaded` is not an absence — see [`DomainEvidence`].
        _ => ForeignNonAnswer {
            terminal: !matches!(evidence, DomainEvidence::AbsentFromLoadedEvidence),
            proves_fauna_domain: false,
        },
    }
}

/// The **device-owned-epoch commit gate** injected into [`FaunaMlsBackend`]
/// (`crate::backends::fauna_mls`) so its commit-producing paths route through the
/// multi-writer-safe rebase loop instead of the optimistic
/// `add_member`/`remove_member` merge (`docs/goal/behavior/devices.md` §
/// Cross-device MLS group-state sync; design tracked internally, §3).
///
/// **The inverse of [`ConversationsRpc`].** The rebase loop
/// ([`fauna_client_mls_sync::MlsStateSync::send_commit_gated`]) needs the
/// `provider` CAS-put + the processed-seq cursor, both of which live *above*
/// `fauna-conversations` (in `fauna-client-mls-sync`), so the loop cannot be
/// called from here directly without a dependency cycle. This object-safe seam
/// inverts the dependency: the trait is declared here, implemented in
/// `fauna-client-mls-sync` (`FaunaCommitGate`), and injected as an
/// `Option<Arc<dyn CommitGate>>` on the backend. When **`Some`**, the commit
/// paths route through it (device-owned-epoch safe); when **`None`**, the
/// gate-less staged path stands — stage → send → merge-on-accept, per
/// `devices.md` § Cross-device MLS group-state sync Rule 1 (merge-ordering) — a
/// nest/deployment without the multi-device plane, or a single-device client,
/// needs no gate (additive/bidirectional compat, design §3a). The per-app leg
/// constructs + injects the impl (slice 5).
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait CommitGate: MaybeSendSync {
    /// Stage → gate-send → merge an `add_member` commit through the rebase loop,
    /// rebasing on a `fauna.conversations.channel.stale` rejection. Returns the
    /// accepted server `seq` and the Welcome bytes for the newcomer (the backend
    /// still drives the `welcome_deliver` — the gate owns only the commit).
    async fn gated_add_member(
        &self,
        channel: ChannelId,
        key_package_bytes: Vec<u8>,
    ) -> Result<(i64, Vec<u8>), BackendError>;

    /// Stage → gate-send → merge a `remove_member` commit through the rebase loop.
    /// Returns the accepted server `seq`. No Welcome (the removed member falls off
    /// the epoch).
    async fn gated_remove_member(&self, channel: ChannelId, leaf: u32)
    -> Result<i64, BackendError>;

    /// Stage → gate-send → merge a **room policy** commit (a
    /// GroupContextExtensions proposal installing the extension `rebuild`
    /// derives from the channel's current policy,
    /// `MlsEngine::set_room_policy_staged`) through the rebase loop — the
    /// same device-owned-epoch discipline the membership commits ride, since
    /// a policy change advances the epoch exactly as they do
    /// (`conversation-rooms.md` § Roles and authorization). `rebuild` runs
    /// on every attempt ([`RoomPolicyRebuild`]). Returns the accepted server
    /// `seq`.
    async fn gated_set_room_policy(
        &self,
        channel: ChannelId,
        rebuild: RoomPolicyRebuild,
    ) -> Result<i64, BackendError>;

    /// Post the §3c takeover self-`Update` commit iff this device did not author
    /// the channel's current epoch — a **no-op** otherwise. Called before the
    /// first application send in a foreign epoch (a shared single leaf must never
    /// fork a ratchet generation); membership commits skip it because the
    /// add/remove commit is itself the epoch-authoring commit.
    async fn ensure_takeover(&self, channel: ChannelId) -> Result<(), BackendError>;

    /// The §5 **own-leaf-foreign-commit resync**: the inbound driver met a
    /// commit from this identity's own leaf that this device did not author
    /// (another of the user's devices took the epoch over — the typed
    /// `MlsError::OwnLeafCommit`, which MLS itself can never process). Refetch
    /// the `provider` replica, reload the group from it, and clear this
    /// device's epoch authorship. `logged_commit_epoch` is the unprocessable
    /// commit's epoch (from its unprotected header): a replica restored at
    /// exactly that epoch is the other device's crash-window upload whose
    /// still-pending commit IS the logged record, so the impl merges it to
    /// converge (design §3 takeover crash-safety). `logged_commit_hash` is
    /// `blake3` of the logged commit's wire bytes — the impl merges the restored
    /// step-2 pending **only when its stamped identity equals this**, so a
    /// malicious nest cannot roll the replica back to a superseded pending and
    /// fork this device onto a never-landed commit (design §3 resync-identity
    /// hardening).
    async fn resync_channel(
        &self,
        channel: ChannelId,
        logged_commit_epoch: u64,
        logged_commit_hash: [u8; 32],
    ) -> Result<(), BackendError>;

    /// Record that a **foreign member's** commit advanced `channel`'s epoch
    /// (the inbound driver processed it Ok): this device no longer authors the
    /// epoch, so its next application send must take over first (§3c). Without
    /// this, a device that ever authored an epoch would skip the takeover
    /// forever and fork the leaf's sender chains on its next send.
    fn note_foreign_commit(&self, channel: ChannelId);
}

/// The cross-device **processed-seq cursor** seam (`docs/goal/behavior/devices.md`
/// § Cross-device MLS group-state sync; design tracked internally, §5).
///
/// The background inbound poll ([`crate::backends::fauna_mls::poll_inbound_conv`],
/// driven by the receive loop) resumes each channel from the seq this **identity**
/// last folded. On a device restored from the `history/<ch>` replica that seq is
/// the replica's `watermark`, **not `0`** — a restored device must not re-walk (and
/// re-order / double-classify) pre-restore history, and its own application
/// messages before the restore point are recoverable only from the replica, never
/// by MLS-decrypting the log. So the poll's start cursor is the replica watermark,
/// and the seqs it folds are reported back so the next `history/<ch>` save
/// snapshots the right watermark.
///
/// Like [`CommitGate`], the cursor authority
/// ([`fauna_client_mls_sync::MlsStateSync`]) lives *above* `fauna-conversations`
/// (it owns the `provider`/`history` replicas), so this object-safe seam inverts
/// the dependency: declared here, implemented in `fauna-client-mls-sync`, injected
/// on the backend (`set_channel_cursor`). When **unset** the loop keeps a fresh
/// per-launch cursor from `0` — today's single-device behavior; a client without
/// the multi-device plane is untouched (additive compat).
///
/// Not `#[async_trait]`: both methods are cheap in-memory cursor-map touches
/// (`MlsStateSync` guards its cursor with a `std::sync::Mutex`), so they stay
/// synchronous — the poll consults them off the hot path.
pub trait ChannelCursor: MaybeSendSync {
    /// The seq to **resume** `channel`'s inbound poll from — the restored
    /// `history/<ch>` watermark, or `0` for a channel this device has never
    /// folded. Consulted once, when the poll first encounters the channel.
    fn resume_seq(&self, channel: &ChannelId) -> i64;

    /// Report that the poll folded `channel` up to `seq` (monotonic — a lower
    /// value is ignored, matching the loop's `if seq > after_seq` advance), so
    /// the next `history/<ch>` save snapshots the resulting watermark.
    fn advance(&self, channel: &ChannelId, seq: i64);
}

/// Awaited durable persistence of one channel's `history/<ch>` replica slice —
/// the `devices.md` § Durability rules **Rule 3 (durable-before-done)** seam,
/// the third injected sibling of [`CommitGate`] and [`ChannelCursor`]. The
/// slice carries state only this device can produce (own message plaintext,
/// folded reaction/delete state, labels, membership): a sender cannot
/// MLS-decrypt its own application messages, and the thread store is RAM-only
/// (re-seeded *from* the replica at launch), so anything here that misses
/// durable storage before a quit is user-irrecoverable. The debounced replica
/// autosave is only the steady-state coalescer; this seam is what the manager
/// awaits so an own mutation is durable before its action completes.
///
/// Injected by `fauna-client-mls-sync::orchestration::restore_and_wire` (the
/// slice-5 launch) via `FaunaMlsBackend::set_history_persist`; when **unset**
/// (single-device / no multi-device plane) every flush is a no-op — exactly
/// the launch-gated behavior of the other saves (additive compat).
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait HistoryPersist: MaybeSendSync {
    /// Snapshot `channel`'s bound thread as a `history/<ch>` slice and
    /// CAS-save it durably, returning only once it landed (or failed). A
    /// channel with no bound thread is a no-op `Ok`. Errors are the caller's
    /// to warn-log — the debounced autosave retries on the next mutation.
    async fn persist_channel(&self, channel: ChannelId) -> Result<(), BackendError>;
}

/// The **mid-session sibling-group adoption** seam — the fifth injected
/// sibling of [`CommitGate`] / [`ChannelCursor`] / [`HistoryPersist`] /
/// [`ProviderPersist`], and the one that makes a group another of the user's
/// devices joined appear on this device without a relaunch (`devices.md`
/// § Cross-device MLS group-state sync → *A sibling-joined group is adopted
/// mid-session by a targeted import*).
///
/// The receive sweep calls [`Self::adopt_if_changed`] once at its start, on
/// every trigger (ticker, push, reconnect), before it walks
/// `bound_channels()`: the impl probes the replica tip (a hash, never the
/// blob, in the steady state), imports every group the engine lacks, binds a
/// thread for each one whose history slice has landed, and returns how many
/// it bound — so a channel bound this pass is polled this same pass. Declared
/// here and implemented in `fauna-client-mls-sync` for the same reason the
/// other four are: the replica authority lives above this crate. When
/// **unset** (single-device / no multi-device plane) the sweep skips it —
/// today's behaviour, additive compat.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait SiblingGroupAdopter: MaybeSendSync {
    /// Probe the tip; if another device wrote, import what this engine lacks
    /// and bind what can be bound. Returns the number of channels bound this
    /// call. An `Err` is a transport/plane fault the sweep warn-logs and moves
    /// past — the next sweep asks again.
    async fn adopt_if_changed(&self) -> Result<usize, BackendError>;
}

/// The awaited **durable provider-replica flush** seam — the Rule-3
/// (durable-before-done) analogue of [`HistoryPersist`] for a **key-package
/// mint**. A mint writes fresh private init keys into the engine's `provider`
/// storage, and those keys are user-unreconstructable (`devices.md`
/// § Cross-device MLS group-state sync — "fresh random HPKE keys …
/// user-unreconstructable the instant its engine state is swapped"), so they
/// must be durable in the nest replica **before** the public key package is
/// published to the pool. Otherwise a provider swap — the launch restore, a
/// mid-session `resync_provider`, or a relaunch — wipes the init key while a
/// peer has already fetched the package, stranding a group whose Welcome this
/// device can never open (the web-slice-6 bug class).
///
/// [`RailBackend::ensure_keypackages`] / [`RailBackend::ensure_last_resort_keypackage`]
/// await this **between the mint and the upload**: `persist_provider` runs the
/// Rule-2-ordered replica save (every `history/<ch>` slice first, then the
/// `provider` — never a provider ahead of unsaved history) and returns whether
/// the `provider` blob **durably landed**. A `false` return means the save was
/// a no-op — the launch gate has not lifted (a mint before the cross-device
/// restore has merged) or the provider was unchanged — and the backend then
/// **refuses to publish** the package rather than ship one whose init key the
/// imminent restore will wipe. This closes the launch-window strand the
/// debounced autosave could not.
///
/// Injected by `fauna-client-mls-sync::orchestration::restore_and_wire` (the
/// launch) via [`FaunaMlsBackend::set_provider_persist`](crate::backends::fauna_mls::FaunaMlsBackend::set_provider_persist);
/// when **unset** (single-device / no multi-device plane — no replica exists to
/// swap over the init keys) the mint publishes directly, exactly as before
/// (additive compat).
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait ProviderPersist: MaybeSendSync {
    /// Rule-2-ordered CAS-save of the whole replica (history first, then the
    /// `provider` blob that holds the fresh init keys), awaited. Returns
    /// `Ok(true)` when the `provider` blob durably landed, `Ok(false)` when its
    /// save was skipped (pre-launch-restore gate down, or unchanged). An `Err`
    /// is a genuine transport failure — the caller treats **both** non-`true`
    /// outcomes as "not durable, do not publish".
    async fn persist_provider(&self) -> Result<bool, BackendError>;
}

/// One engine-held channel with **no bound thread** in which some person is
/// seated — the answer shape of [`RailBackend::unbound_seats_of`]. The channel
/// id rides as hex: it names the seat for logs and pins, and deliberately not a
/// `ThreadId` (there is none — that absence is the whole point of the type).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnboundSeat {
    pub channel_hex: String,
    pub class: UnboundChannelClass,
}

/// What kind of channel an unbound engine seat sits in — the rail's *fact*;
/// what each class means for the cross-group eviction is § Propagation rule
/// (5)'s ruling, applied by the driver
/// ([`crate::ConversationsManager::evict_person_everywhere`]), never here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnboundChannelClass {
    /// Durably chat-marked (`MlsEngine::is_channel_chat`) but no thread points
    /// at it this run — a chat group whose thread↔channel binding has not been
    /// rebuilt (the binding is RAM-only, restored from history slices).
    Chat,
    /// The folder rail, by the same derivation the folder poll uses
    /// (engine group, unbound, not scheduling, not chat-marked) — which
    /// over-approximates onto unmarked chat channels, accepted here for the
    /// same reason it is accepted there.
    Folder,
    /// A one-off scheduling (iMIP) delivery channel
    /// (`FaunaMlsBackend::mark_scheduling_channel`).
    Scheduling,
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait RailBackend: MaybeSendSync {
    fn rail(&self) -> Rail;

    fn capabilities(&self, thread: &ThreadDetail) -> ThreadCapabilities;

    /// Probe `raw` on this rail. [`ResolveResult::Error`] is **terminal** for the
    /// manager's chain — see its doc; return it only for an address this rail
    /// recognises as its own and cannot confirm right now, never for "not mine"
    /// (that is [`ResolveResult::NotFound`], which lets the chain fall through).
    async fn resolve_address(&self, raw: &str) -> ResolveResult;

    /// The participants of every thread this account holds, shown to the rail
    /// right before each probe (`ConversationsManager::probe_address`) so a
    /// rail can recognise a domain the account already converses with on it.
    /// The FaunaMls rail harvests the domains of `TypedAddress::Fauna`
    /// participants as its *known Fauna domain* evidence
    /// (`docs/goal/architecture/federation.md` § Peer-auth model →
    /// *Discovery-failure semantics*); every other rail ignores the call.
    /// Default no-op. Cheap by construction (one pass over the participant
    /// list), and idempotent.
    fn observe_participants(&self, _participants: &[TypedAddress]) {}

    /// The compose form's list-send view for a compose addressed to
    /// `recipients`, when this rail sends it to one of the account's own
    /// mailing lists (`mail-mass-mailing.md` § Composing a list message). Only
    /// the SMTP rail has lists; every other rail answers `None`.
    async fn list_send_view(
        &self,
        _recipients: &[TypedAddress],
    ) -> Option<crate::list_send::ListSendView> {
        None
    }

    /// Identities this rail wants the peer-anchor harvest sweep to walk beyond
    /// the thread rosters — read by `ConversationsManager::harvest_walk_actors`
    /// on every pass, so the answer is derived, never cached here. The FaunaMls
    /// rail answers the **recorded owner of every folder channel** it holds a
    /// seat on: a folder channel has no thread, so an owner the member shares
    /// no conversation with was on no roster, and the folder commit walk's
    /// hold behind a parked succession statement waits on exactly that owner's
    /// settle (`federation.md` § Cross-nest shared folders + channel append →
    /// *The marker follows the owner's verified succession*). Same door, same
    /// grade, same once-per-session guard as a roster peer; bounded by the
    /// seats this device holds. Default empty.
    fn harvest_anchor_wants(&self) -> Vec<fauna_core::identity::ActorId> {
        Vec::new()
    }

    /// Give up whatever exclusive, process-wide resource this rail holds, because
    /// its registration is being replaced by a successor over the same account
    /// state. Default no-op — only the FaunaMls rail has one (its `MlsEngine`'s
    /// conversations-engine role lock over `mls_state.db`).
    ///
    /// Called by [`crate::ConversationsManager::retire_conversations_engine`],
    /// which the shared native session factory runs *before* building the
    /// successor. It exists as a trait method rather than a downcast because the
    /// manager holds its rails as `dyn RailBackend` and must not learn what an
    /// MLS engine is to hand one over. After it returns, the backend is expected
    /// to fail its own calls rather than silently serve a store it no longer
    /// owns — see `fauna_mls::storage::MlsStateRetired`.
    ///
    /// Must be idempotent: a rail can be retired and then dropped, and a
    /// registration can be replaced more than once in a process's life.
    fn retire(&self) {}

    /// The local user's own address on this rail, if it is an addressable
    /// recipient. Used by the manager to seed reply-all recipients *minus self*
    /// (`ConversationsManager::start_reply`). Default `None` — only the SMTP
    /// rail (the one with `supports_recipient_selection`) overrides it, returning
    /// the `<handle>@<domain>` it sends `From:`. Kept on the backend (not the
    /// manager) because self-identity is rail-specific and already lives here.
    fn self_address(&self) -> Option<TypedAddress> {
        None
    }

    /// `mailbox` is the server-side [`MailFeed`] the record was read from, when
    /// the caller has one — only the mail receive path
    /// ([`crate::manager::ConversationsManager::ingest_inbound_identified`])
    /// does; every other route (the FaunaMls channel-keyed ingest, direct
    /// `ingest_inbound` calls) passes `None`. A non-FFI parameter, not a
    /// `RailInboundMessage` field: `MailFeed` carries no `uniffi::Record`
    /// derive (this is a plain-Rust seam, like `ingest_inbound_identified`'s
    /// own `nest_message_id` parameter) and only [`crate::backends::smtp::
    /// SmtpBackend::bucket_inbound`] reads it.
    fn bucket_inbound(
        &self,
        msg: RailInboundMessage,
        mailbox: Option<MailFeed>,
    ) -> Result<InboundBucket, BackendError>;

    async fn send(
        &self,
        thread: &ThreadDetail,
        compose: &ComposeState,
        attachments: &[ResolvedAttachment],
    ) -> Result<SendOutcome, BackendError>;

    /// Default `NotSupported` — only rails with real participant-membership
    /// semantics (currently just FaunaMls) override this.
    async fn add_participant(
        &self,
        _thread_id: ThreadId,
        _addr: TypedAddress,
    ) -> Result<(), BackendError> {
        Err(BackendError::NotSupported)
    }

    /// Default `NotSupported` — see [`RailBackend::add_participant`].
    async fn remove_participant(
        &self,
        _thread_id: ThreadId,
        _addr: TypedAddress,
    ) -> Result<(), BackendError> {
        Err(BackendError::NotSupported)
    }

    /// Default `NotSupported` — see [`RailBackend::add_participant`].
    async fn rename(&self, _thread_id: ThreadId, _new_label: String) -> Result<(), BackendError> {
        Err(BackendError::NotSupported)
    }

    /// Apply one [`RoomPolicyEdit`] to a governed room's policy
    /// (`conversation-rooms.md` § Roles and authorization): the rail
    /// re-derives the policy, has the acting principal sign it, and commits
    /// the group-context change — refusing **before** any commit is authored
    /// when the viewer's role does not permit the edit
    /// ([`BackendError::Refusal`], a product statement). Default
    /// `NotSupported`: only the FaunaMls rail governs a room.
    async fn update_room_policy(
        &self,
        _thread_id: ThreadId,
        _edit: RoomPolicyEdit,
    ) -> Result<(), BackendError> {
        Err(BackendError::NotSupported)
    }

    /// Found a **community room** for `thread_id` and invite `invitees` into
    /// it (`conversation-rooms.md` § The three classes → *Community*): the
    /// rail runs the birth ceremony, keys the room with its home nest among
    /// the wrap targets — the grant the class is — binds the thread to the
    /// room's channel, and signs one invitation per invitee.
    ///
    /// An invitation seats nobody; each invitee joins by accepting, and the
    /// inviter's device keys them in once they have. So on success the room
    /// exists and the invitations stand, and nobody but the founder and the
    /// home nest is on its floor yet.
    ///
    /// The caller tells a founding that failed from one that founded and then
    /// failed an invitation by [`Self::channel_binding_hex`]: bound means the
    /// room exists. Default `NotSupported`: only the FaunaMls rail seats a
    /// home nest.
    async fn found_room(
        &self,
        _thread_id: ThreadId,
        _name: Option<String>,
        _invitees: &[TypedAddress],
    ) -> Result<(), BackendError> {
        Err(BackendError::NotSupported)
    }

    /// Every community-room invitation standing for this account, verified
    /// against its signer (`conversation-rooms.md` § Join rules and invites).
    /// A peek — listing consumes nothing. Default empty: no other rail is
    /// invited into anything.
    async fn room_invitations(&self) -> Result<Vec<crate::room::RoomInvitation>, BackendError> {
        Ok(Vec::new())
    }

    /// Accept `invitation` and bind the room to `thread_id` — the act that
    /// seats this account on the room's floor. It reads nothing until a member
    /// with key authority keys it in. Default `NotSupported`.
    async fn accept_room_invitation(
        &self,
        _thread_id: ThreadId,
        _invitation: &crate::room::RoomInvitation,
    ) -> Result<(), BackendError> {
        Err(BackendError::NotSupported)
    }

    /// Decline `invitation`: it stops standing for this account, and the room
    /// is not told — a refusal the inviter could read would give declining an
    /// audience. Default `NotSupported`.
    async fn decline_room_invitation(
        &self,
        _invitation: &crate::room::RoomInvitation,
    ) -> Result<(), BackendError> {
        Err(BackendError::NotSupported)
    }

    /// Withdraw the invitation pending for `invitee` on the community room
    /// `thread_id` is (`conversation-rooms.md` § Join rules and invites →
    /// *Pending invitations are visible to whoever may withdraw them*). The
    /// nest judges who may — whoever was served the row — and the invitee is
    /// told nothing. Withdrawing an invitation that is no longer pending
    /// succeeds: the caller wanted it gone and it is. On success the room's
    /// pending list ([`crate::room::RoomSnapshot::pending_invites`]) has been
    /// read again. Default `NotSupported`: no other rail holds a floor that
    /// invitations stand on.
    async fn withdraw_room_invite(
        &self,
        _thread_id: ThreadId,
        _invitee: fauna_core::identity::ActorId,
    ) -> Result<(), BackendError> {
        Err(BackendError::NotSupported)
    }

    /// Walk this account out of the room `thread` is — the roles table's
    /// *leave (remove self)* row, and the producer of the departing-member
    /// report the nest has always admitted (`conversation-rooms.md`
    /// § Roles and authorization → *Leaving — the mechanism*).
    ///
    /// **One verb, one self-scoped door** — `room.leave`, on every class; what
    /// it severs beside the seat is the class's business, complementary and
    /// never cumulative, exactly as the two removals are. The caller does not
    /// choose: it asks to leave and the rail knows which door it owns.
    ///
    /// Takes the whole [`ThreadDetail`] rather than a [`ThreadId`] for that
    /// reason — the class is read off the room state, as
    /// [`Self::capabilities`] and [`Self::room_state`] read theirs.
    ///
    /// The owner is refused: a room is never owner-less, so an owner who
    /// wants out transfers ownership first. Both nest doors enforce it
    /// independently; the manager refuses it before the wire so the user
    /// reads a sentence instead of a wire refusal.
    ///
    /// Default `NotSupported`: no other rail models a room to walk out of.
    async fn leave_room(&self, _thread: &ThreadDetail) -> Result<(), BackendError> {
        Err(BackendError::NotSupported)
    }

    /// Grant (`true`) or withdraw (`false`) the home nest's read of the
    /// community room `thread_id` is — a key rotation, never a roster edit,
    /// and the owner's or an admin's act (`conversation-rooms.md`
    /// § Implementation status today, the revoke). Default `NotSupported`.
    async fn set_room_nest_read(
        &self,
        _thread_id: ThreadId,
        _reads: bool,
    ) -> Result<(), BackendError> {
        Err(BackendError::NotSupported)
    }

    /// Replace the transparent labelers the home nest of the community room
    /// `thread_id` is applies to its messages — `labelers` are published
    /// labeler ids, lowercase hex (`conversation-rooms.md` § The three classes →
    /// *What the home nest does with its read*, purpose 2). The owner's or an
    /// admin's act. Default `NotSupported`.
    async fn set_room_labelers(
        &self,
        _thread_id: ThreadId,
        _labelers: Vec<String>,
    ) -> Result<(), BackendError> {
        Err(BackendError::NotSupported)
    }

    /// Deliver `slice` — the thread's history as this device holds it — to a
    /// newcomer just admitted, under the room's `full` history policy
    /// (`conversation-rooms.md` § History for joiners). The FaunaMls rail
    /// posts it as a `GroupMetaMessage::HistorySlice` application message in
    /// the newcomer's first epoch; the caller (`ConversationsManager::
    /// confirm_add_participant`) checks the policy first, and the rail
    /// refuses a slice the policy does not authorize. Default no-op: no
    /// other rail has a joiner to inform.
    async fn deliver_history_slice(
        &self,
        _thread_id: ThreadId,
        _slice: &crate::store::history::ChannelHistorySlice,
    ) -> Result<(), BackendError> {
        Ok(())
    }

    /// The **room** `thread` is, as this rail models it
    /// (`conversation-rooms.md` § The room; [`crate::room::RoomSnapshot`]):
    /// the members' kinds and roles, the class they derive, the policy and
    /// the viewer's role. Read on every `ConversationsManager::thread_detail`
    /// emit, which projects it onto `ThreadDetail::room` and overlays the
    /// role gating onto `capabilities` — so the projection must be cheap
    /// (a lock and a decode, no I/O).
    ///
    /// **Reads the thread's identity and participants, never its messages.**
    /// `ConversationsManager::thread_state_facts` hands this a detail whose
    /// message list is empty — the per-publish state read must not clone
    /// message bodies — and relies on getting the same room back.
    ///
    /// Default `None`: the rail models no room. FaunaMls answers from its
    /// group and floor, SMTP and the bridged rail with [`crate::room::
    /// transport_room`].
    fn room_state(&self, _thread: &ThreadDetail) -> Option<crate::room::RoomSnapshot> {
        None
    }

    /// Who the bridge `bridge_id` is, as its manifest declared it — what the
    /// manager projects onto a [`Rail::Bridged`] thread's
    /// [`crate::snapshot::ThreadSummary::bridge`] and `glyph`
    /// (`ui/conversations.md` § Where logic lives → *The `Bridged` adapter*,
    /// ruling 2 (a)). Cheap like [`Self::room_state`]: a lock and a lookup.
    /// Default `None`: only the bridged rail keeps a registry.
    fn bridge_identity(
        &self,
        _bridge_id: &str,
    ) -> Option<fauna_core::source_glyph::BridgeIdentitySnapshot> {
        None
    }

    /// Every bridge this rail currently knows, by its declared identity,
    /// ordered by label — what the recipient picker lists so the user can see
    /// which far networks a typed address may reach (`ui/conversations.md`
    /// § Where logic lives → *The `Bridged` adapter*, ruling 2 (d)). Default
    /// empty: only the bridged rail keeps a registry.
    fn bridge_identities(&self) -> Vec<fauna_core::source_glyph::BridgeIdentitySnapshot> {
        Vec::new()
    }

    /// The family gate's marker for the thread these `participants` make —
    /// what the manager projects onto
    /// [`crate::snapshot::ThreadSummary::guardian_state`]
    /// (`behavior/family-safety.md` § The bridge-DM gate). The nest computes
    /// it; a rail only remembers the newest answer. Cheap like
    /// [`Self::bridge_identity`]. Default `None`: only the bridged rail
    /// carries the gate.
    fn guardian_state(
        &self,
        _participants: &[TypedAddress],
    ) -> Option<crate::snapshot::GuardianState> {
        None
    }

    /// Whether this device, as far as the rail can tell, still **holds its
    /// seat** on `thread`'s room — the liveness `ConversationsManager::
    /// room_post_rooms` filters the composer's room offers on (`ui/feed.md`
    /// § Encryption at rest → *Room-restricted — the app half* → *The rooms
    /// offered*). Cheap like [`Self::room_state`]: a lock and a flag, no I/O.
    ///
    /// Default `true`: a rail that models no eviction has no reason to say
    /// otherwise. Only the FaunaMls rail answers `false` today — for an
    /// end-to-end room whose MLS group this device has been removed from and
    /// has processed the removal of (`MlsEngine::is_group_active`); a device
    /// that never processed its removal has nothing to answer from, which is
    /// the residue `ui/feed.md` ruling 5 states rather than closes.
    fn seated_on_room(&self, _thread: &ThreadDetail) -> bool {
        true
    }

    /// The rail's opaque transport binding for a thread, as a hex string, if one
    /// exists. Only the FaunaMls rail has one (its bound `ChannelId`, populated
    /// once a group bootstraps); every other rail returns `None`. The manager
    /// uses this to re-key a sender-bootstrapped MLS thread from participant-keyed
    /// to channel-keyed after `send` binds the channel — matching the receiver
    /// side (`ingest_welcome`), so two groups with identical membership stay
    /// distinct. The hex stays out of the snapshot; clients never see a
    /// `ChannelId` (`docs/goal/ui/conversations.md` § Architectural rules #2).
    fn channel_binding_hex(&self, _thread_id: &ThreadId) -> Option<String> {
        None
    }

    /// The rail's **authoritative** membership roster for `thread_id`, when the
    /// rail keeps one of its own. `None` means it does not, and the thread
    /// store's `participants` list *is* the roster there — true of every
    /// non-MLS rail, and of an MLS thread whose group has not bootstrapped yet
    /// (its membership is still only a local intention, with no engine to ask).
    ///
    /// **Why a rail is asked at all, instead of the snapshot being trusted.**
    /// The cross-group eviction driver
    /// ([`crate::ConversationsManager::evict_person_everywhere`]) has to answer
    /// "is this person seated in this group?" from the **same** source as the
    /// flag it is acting on, which is raised off the MLS engine roster
    /// (`SweepReport::unattested_members` → `MlsEngine::group_members`).
    /// `ThreadDetail::participants` is not that source: it is a local,
    /// incrementally-maintained view written at join and by the owner's own
    /// add/remove gestures, and **no inbound Commit reconciles it** — so a
    /// foreign-authored membership Commit seats someone in the engine and not
    /// in the snapshot. Deciding membership from the snapshot there skips the
    /// group silently, reports no failure, and lets the review item earn
    /// `Removed` while the person is still in the group — the exact
    /// "surface goes quiet while they are still seated" harm
    /// `identity-succession.md` § Propagation rule (3) exists to prevent,
    /// arriving through the enumeration door.
    ///
    /// `ActorId`s rather than `TypedAddress`es because that is all a roster
    /// read off MLS can honestly give: a leaf credential carries an actor id
    /// and no handle (`FaunaMlsBackend::group_members`).
    fn authoritative_roster(
        &self,
        _thread_id: &ThreadId,
    ) -> Option<Vec<fauna_core::identity::ActorId>> {
        None
    }

    /// Every **thread-less** engine seat of `person` on this rail — the
    /// channels the engine holds that no thread points at, classified so the
    /// eviction driver can apply § Propagation rule (5) (`identity-succession.md`
    /// § Propagation → *Removing a flagged member*). The bound-thread seats are
    /// the driver's own loop over the thread store; this method exists because
    /// an unbound channel is invisible from the thread list *by construction*,
    /// and the enumeration must follow the same source rule as the roster
    /// ([`Self::authoritative_roster`]): the flag being acted on was raised off
    /// the engine's group set, unfiltered.
    ///
    /// **Facts, not rulings.** This reports every unbound seat honestly
    /// classified — the *scheduling* class included, though the driver skips it
    /// (a scheduling channel's roster cannot change after join, so a seat there
    /// cannot be planted). The class dispatch is deliberately the driver's:
    /// the rail states what is, § Propagation decides what to do about it.
    ///
    /// Default: no rail but FaunaMls keeps engine seats, so the default is
    /// empty — true of every non-MLS rail, mirroring `authoritative_roster`'s
    /// `None`.
    fn unbound_seats_of(&self, _person: &fauna_core::identity::ActorId) -> Vec<UnboundSeat> {
        Vec::new()
    }

    /// Top up the local actor's one-time key-package pool on the nest to
    /// `target`, generating + uploading the shortfall. MLS-rail concept only —
    /// every non-MLS rail has no key packages, so the default is a no-op
    /// returning `Ok(0)`. The FaunaMls rail overrides it (it owns the
    /// `MlsEngine` that mints packages). The manager exposes a thin
    /// FFI-facing wrapper ([`crate::ConversationsManager::ensure_keypackages`])
    /// that routes here, so all apps call it identically over their FFI.
    async fn ensure_keypackages(&self, _target: u64) -> Result<u64, BackendError> {
        Ok(0)
    }

    /// Publish the actor's mandatory **last-resort** key package
    /// (`docs/goal/architecture/federation.md` § Key packages — privacy &
    /// exhaustion) if the rail has one. MLS-rail concept only; the default is a
    /// no-op. The FaunaMls rail overrides it; the manager wrapper
    /// ([`crate::ConversationsManager::ensure_last_resort_keypackage`]) routes
    /// here. Idempotent (the nest keeps a single last-resort row per actor), so
    /// client glue calls it on every login.
    async fn ensure_last_resort_keypackage(&self) -> Result<(), BackendError> {
        Ok(())
    }

    /// Post a reaction to an existing message on the thread's MLS channel.
    /// `target_seq` identifies the message being reacted to; `emoji` is the
    /// reaction glyph; `op` is `Add` or `Remove`. MLS-rail only — every other
    /// rail returns `NotSupported`. The FaunaMls rail overrides this, sealing
    /// a `ChannelMessageBody::Reaction` application message on the thread's
    /// bound channel (same mechanism as `rename`; no Commit, no epoch change).
    async fn send_reaction(
        &self,
        _thread: &ThreadId,
        _target_seq: u64,
        _emoji: &str,
        _op: ReactionOp,
    ) -> Result<(), BackendError> {
        Err(BackendError::NotSupported)
    }

    /// Post a delete marker for an existing message on the thread's MLS
    /// channel. `target_seq` identifies the message to be deleted. MLS-rail
    /// only — every other rail returns `NotSupported`. The FaunaMls rail
    /// overrides this, sealing a `ChannelMessageBody::Delete` application
    /// message on the thread's bound channel (same mechanism as `rename`; no
    /// Commit, no epoch change).
    async fn send_delete(&self, _thread: &ThreadId, _target_seq: u64) -> Result<(), BackendError> {
        Err(BackendError::NotSupported)
    }

    /// Post the delete of **another member's** message — the owner's or
    /// admin's act (`conversation-rooms.md` § Roles and authorization → *Delete
    /// any message — the mechanism*). A seam of its own beside
    /// [`Self::send_delete`] because one class carries the two on different
    /// wire shapes: an end-to-end room seals the same `Delete` either way (the
    /// default), while a community room files a signed **floor delete record**
    /// its home nest judges, which the FaunaMls rail overrides this for. The
    /// manager picks by whether the target is the viewer's own.
    async fn send_delete_any(
        &self,
        thread: &ThreadId,
        target_seq: u64,
    ) -> Result<(), BackendError> {
        self.send_delete(thread, target_seq).await
    }

    /// Durably persist `thread`'s history slice now, **awaited** — Rule 3
    /// (durable-before-done, `devices.md` § Durability rules). The manager
    /// calls this after every own store mutation (send append, reaction,
    /// delete, rename, membership edit) so the action does not complete before
    /// the user-irrecoverable state is durable. Only the FaunaMls rail
    /// overrides it (routing to the injected [`HistoryPersist`] seam); rails
    /// whose history persistence lives elsewhere (SMTP: the nest mail store;
    /// a bridge: the nest's sealed store and the far network) no-op.
    async fn persist_history(&self, _thread: &ThreadId) -> Result<(), BackendError> {
        Ok(())
    }
}
