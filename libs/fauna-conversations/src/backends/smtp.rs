//! SMTP rail backend.
//!
//! `resolve_address` / `bucket_inbound` are pure (no I/O). `send` assembles an
//! RFC 5322 message (shared, [`crate::rfc5322`]) and submits it through the
//! injected [`OutboundMailSink`], which performs the `fauna.email.send` WS-RPC
//! call. The backend carries the logged-in user's `<handle>@<domain>` so it
//! can set a `From:` the nest accepts (same-domain From local-part must equal
//! the caller's handle — see `bins/fauna-nest/src/email_handlers.rs`).

use crate::address::{Rail, TypedAddress};
use crate::backend::{
    BackendError, InboundBucket, InboundMailRecord, InboundMailSource, MailFeed, MailFlagCallError,
    OutboundMailSink, RailBackend, RailInboundMessage, ResolveResult, ResolvedAttachment,
    SelfAddress, SendOutcome,
};
use crate::capabilities::{ThreadCapabilities, derive_capabilities};
use crate::compose::ComposeState;
use crate::manager::ConversationsManager;
use crate::message::{AttachmentSnapshot, MessageBadges, MessageId};
use crate::snapshot::ThreadDetail;
use crate::store::{AttachmentCoordinates, MailRecordCoordinates};
use async_trait::async_trait;
use std::collections::HashSet;
use std::sync::Arc;

pub struct SmtpBackend {
    sink: Arc<dyn OutboundMailSink>,
    /// The logged-in user's canonical mail address (`<handle>@<domain>`), used
    /// as the RFC 5322 `From:` — a live cell read at send time
    /// (`conversations.md` § State & data shape → *Self-address: live, never
    /// baked*), so a late-resolving or renamed handle heals this backend with
    /// no rebuild.
    self_address: Arc<SelfAddress>,
}

impl SmtpBackend {
    /// Standalone construction over a fixed address — callers that re-register
    /// a fresh backend per address change (and tests). Session/manager wirings
    /// share one live cell across rails via [`Self::new_shared`] instead.
    pub fn new(sink: Arc<dyn OutboundMailSink>, self_address: impl Into<String>) -> Self {
        Self::new_shared(sink, SelfAddress::new(self_address))
    }

    /// Construction over the session's shared live cell — one
    /// `set_self_address` then heals this rail together with FaunaMls.
    pub fn new_shared(sink: Arc<dyn OutboundMailSink>, self_address: Arc<SelfAddress>) -> Self {
        Self { sink, self_address }
    }
}

/// The domain part of `self_address`, for Message-ID composition.
fn domain_of(self_address: &str) -> &str {
    self_address
        .rsplit_once('@')
        .map(|(_, d)| d)
        .unwrap_or("localhost")
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl RailBackend for SmtpBackend {
    fn rail(&self) -> Rail {
        Rail::Smtp
    }

    fn capabilities(&self, thread: &ThreadDetail) -> ThreadCapabilities {
        derive_capabilities(Rail::Smtp, thread.flavor.clone())
    }

    /// A mail thread is a transport-only room: the mail transfer agent sits on
    /// its floor, so the class — and with it `encryption` — derives from the
    /// member set like every other rail's (`conversation-rooms.md` § The three
    /// classes → *Transport-only*), never from a per-rail constant.
    fn room_state(&self, thread: &ThreadDetail) -> Option<crate::room::RoomSnapshot> {
        Some(crate::room::transport_room(thread.participants.len()))
    }

    fn self_address(&self) -> Option<TypedAddress> {
        Some(TypedAddress::Email {
            email_address: self.self_address.get(),
        })
    }

    async fn resolve_address(&self, raw: &str) -> ResolveResult {
        // Any well-formed user@host resolves syntactically; real MX check deferred.
        if raw.contains('@') && !raw.starts_with('@') && !raw.ends_with('@') {
            ResolveResult::Resolved(TypedAddress::Email {
                email_address: raw.to_string(),
            })
        } else {
            ResolveResult::NotFound
        }
    }

    fn bucket_inbound(
        &self,
        msg: RailInboundMessage,
        mailbox: Option<MailFeed>,
    ) -> Result<InboundBucket, BackendError> {
        let subject = msg.subject.clone();
        // Ownership is **provenance**, never the `From:` header: `From:` is
        // written by whoever sends the mail, so a sender-vs-self compare here
        // (as `FaunaMlsBackend::bucket_inbound` does against its MLS-ratchet-
        // authenticated sender, `fauna_mls.rs:5332`) would frame any forged
        // `From: <this account's address>` as the user's own message — the
        // standing phishing lever this field exists to close (mark-as-spam is
        // gated `!is_own`, `mail-spam.md:66`). The `Sent` mailbox has no such
        // hole: the nest only ever files a record there via a submission this
        // account authenticated (a first-party `fauna.email.send`, or another
        // MUA/device's own submission on the same account —
        // `mail-app-surface.md` § Inbound client receive → *Sibling Sent
        // feed*), never from an inbound delivery — not even one a filter rule
        // names `Sent` for (`email-filters.md` § Email filter rules) — so
        // `mailbox == Some(MailFeed::Sent)` is sound evidence of authorship an
        // `INBOX` record structurally cannot fake. Not a client-side auth-verdict
        // check either: `RailInboundMessage` carries no verdict, and no nest
        // path files an account's own sends into `INBOX` for one to check
        // against (`backend.rs:21-48`).
        //
        // Both mailboxes are always polled — mail sent from another
        // MUA/device on the same account has no local echo to dedup against,
        // so it only ever arrives here as a `Sent`-mailbox record
        // (`session.rs:664-671`, `conversations.md:1571`). Self-addressed mail
        // is the one case that needs both: it lands in `INBOX` too (this
        // record, `is_own == false`, correctly — an `INBOX` copy is never
        // proof of authorship even when it is genuinely this account's own
        // send) and the durable `Sent` copy that proves it arrives under the
        // SAME Message-ID, `INBOX` polled first (`session.rs:1416-1431`).
        // `ConversationsManager::ingest_inbound_identified`'s id-dedup upgrades
        // the already-held copy to own when that happens
        // ([`crate::store::threads::ThreadStore::mark_message_own`]) — this
        // function only ever needs to get today's record right.
        let is_own = mailbox == Some(MailFeed::Sent);
        Ok(crate::backend::bucket_inbound_common(msg, subject, is_own))
    }

    async fn send(
        &self,
        thread: &ThreadDetail,
        compose: &ComposeState,
        attachments: &[ResolvedAttachment],
    ) -> Result<SendOutcome, BackendError> {
        // Sender pre-check (mail-app-surface.md § First-party client send: "the nest
        // handler does sender-handle verification"). We may only claim an
        // address this account actually owns, and `self_address` is the app's
        // resolution of it. When it is absent or not a full `<local>@<domain>`,
        // there is nothing legitimate to put in `From:` — refuse here, before
        // the wire, exactly as the inline-ceiling pre-check below does.
        //
        // Refusing is not merely tidier than sending: an unusable address is
        // carried verbatim into `rfc5322::build_message`, which emits
        // `From: \r\n`, and the nest's From-handle gate keys on
        // `from_addr.rsplit_once('@')` — so a From with no `@` **evades** the
        // gate and relays a malformed message to a real external MX. The state
        // is reachable in production: an actor an admin admitted directly
        // (`fauna.admin.users.create`) has no handle, so no app can resolve an
        // address for it (public-mode.md § User Registration). An empty local
        // part (`"@nest.example"` — an unresolved handle glued to a URL host)
        // is the sibling failure: it claims a handle the nest does not back,
        // trading the honest local refusal for an opaque RPC code.
        //
        // Read the live cell ONCE — From:, self-drop, and Message-ID must agree
        // within one send even if a rename races it.
        let self_address = self.self_address.get();
        if !SelfAddress::usable(&self_address) {
            return Err(BackendError::Refusal(
                fauna_i18n::strings::error::email::NO_HANDLE.to_string(),
            ));
        }

        // Envelope recipients. When the reply draft carries an editable
        // recipient set (`dm-reply-button`/`dm-reply-all-button` seeded it, the
        // user may have edited the To line), send to exactly those. Otherwise
        // fall back to the thread's historical participants — so a plain send
        // with no reply seed (or a client that hasn't wired the To line yet)
        // still addresses the whole thread. Either way drop self and keep only
        // Email addresses (`conversations.md` § Participants vs reply recipients).
        let recipient_source: &[TypedAddress] = if compose.reply_recipients.is_empty() {
            &thread.participants
        } else {
            &compose.reply_recipients
        };
        let recipients: Vec<String> = recipient_source
            .iter()
            .filter_map(|a| match a {
                TypedAddress::Email { email_address } => Some(email_address.clone()),
                _ => None,
            })
            .filter(|e| !e.eq_ignore_ascii_case(&self_address))
            .collect();
        if recipients.is_empty() {
            return Err(BackendError::Refusal(
                fauna_i18n::strings::error::send::NO_RECIPIENTS.to_string(),
            ));
        }

        let now_secs = fauna_core::data::Timestamp::now_secs();
        let message_id = crate::rfc5322::new_message_id(domain_of(&self_address));

        let in_reply_to = compose.reply_to.as_ref().map(|m| m.0.clone());
        // Inline each staged attachment as a multipart/mixed part — borrowing the
        // resolved bytes the manager pulled from its attachment store.
        let mime_attachments: Vec<crate::rfc5322::MimeAttachment<'_>> = attachments
            .iter()
            .map(|a| crate::rfc5322::MimeAttachment {
                filename: &a.filename,
                mime_type: &a.mime_type,
                bytes: &a.bytes,
            })
            .collect();
        let raw = crate::rfc5322::build_message(
            &self_address,
            &recipients,
            compose.subject_draft.as_deref(),
            &compose.body_draft,
            &message_id,
            in_reply_to.as_deref(),
            now_secs,
            &mime_attachments,
        );

        // Sever-prevention pre-check (smtp-server.md § Message size limits): the
        // assembled request rides the 2 MiB WS-RPC frame inline, so an
        // over-inline-ceiling raw message would overflow it and sever the
        // connection before the nest ever answers — refuse locally instead, with
        // the same identifier a server-side fauna.email.too_large would render
        // (`fauna_protocol::RpcError::localized`), so the compose surface shows
        // one consistent message either way. This is a transport-shape guard,
        // not the product ceiling: `effective_max_raw_message_bytes` (the admin's
        // policy knob) can be lower still and is enforced authoritatively
        // nest-side — this check cannot replace that RPC round trip.
        if raw.len() > fauna_mail::transport_limits::MAX_INLINE_RAW_MESSAGE_BYTES as usize {
            return Err(BackendError::Refusal(
                fauna_i18n::strings::error::email::TOO_LARGE.to_string(),
            ));
        }

        self.sink
            .submit(recipients, raw)
            .await
            .map_err(BackendError::transport_from_seam)?;

        Ok(SendOutcome {
            message_id: MessageId(message_id),
            timestamp_ms: now_secs * 1000,
            sender: TypedAddress::Email {
                email_address: self_address,
            },
            // Not on the account data plane: this rail's records are not
            // content-scope feed records, so there is no T1 observation to
            // report for them.
            plane_ref: None,
            // Nothing to hand over yet: the parts rest in the Sent record the
            // mailbox numbers when it files it, and that record's ingest
            // remembers its `(mailbox, uid)` like any other
            // (`ingest_inbound_record`, before its message-id dedup).
            attachment_coordinates: Vec::new(),
        })
    }
}

// ── Inbound mail receive driver ───────────────────────────────────────

/// Drive the inbound mail feed end-to-end: page `source` (the platform's
/// `fauna.email.inbox.fetch` + decrypt seam) until exhausted, parse each
/// decrypted RFC 5322 record, and ingest it into `manager` as an SMTP-rail
/// message. This is the receive counterpart to [`SmtpBackend::send`] and lives
/// in shared Rust so every app reuses the same parse + bucket + ingest path
/// (`docs/goal/behavior/smtp-server.md` § Inbound client receive).
///
/// `after_uid` is the paging cursor: it's advanced to the highest UID seen so
/// the caller can persist it (cursor persistence across restarts is the
/// platform's job — Slice 3). `seen` dedups by the server segment-record id
/// across calls, so an overlapping re-poll doesn't double-ingest (`ingest_inbound`
/// is not idempotent). Records that fail to parse into a usable message (no
/// `From`) are skipped, not fatal — one malformed message can't stall the feed.
///
/// Returns the number of newly-ingested messages.
pub async fn poll_inbound_mail(
    source: &dyn InboundMailSource,
    manager: &ConversationsManager,
    after_uid: &mut u32,
    seen: &mut HashSet<Vec<u8>>,
    page_limit: u32,
) -> Result<usize, BackendError> {
    // Per-pass batch lifecycle for a source that scores on-device (the INBOX
    // spam scorer): `begin_pass` builds this pass's scorer; `end_pass` flushes its
    // `apply_spam_disposition` exactly once. `end_pass` runs even when a `fetch`
    // errors mid-pass, so a junk verdict recorded on an earlier page (already
    // suppressed from the view + advanced past by the cursor) is still watermarked
    // + moved to Junk — never suppressed-but-left-in-INBOX. Both default to no-ops
    // for the ordinary (Sent / non-scoring) sources.
    source.begin_pass().await;
    let result = poll_inbound_mail_pages(source, manager, after_uid, seen, page_limit).await;
    source.end_pass().await;
    result
}

/// The page loop of [`poll_inbound_mail`], factored out so `end_pass` runs on both
/// the ok and the error path (see the lifecycle note there).
async fn poll_inbound_mail_pages(
    source: &dyn InboundMailSource,
    manager: &ConversationsManager,
    after_uid: &mut u32,
    seen: &mut HashSet<Vec<u8>>,
    page_limit: u32,
) -> Result<usize, BackendError> {
    let mut ingested = 0usize;
    loop {
        let page = source
            .fetch(*after_uid, page_limit)
            .await
            .map_err(BackendError::transport_from_seam)?;
        let more = page.more;
        // The flag-change baseline (`mail-app-surface.md` § Read state): the
        // manager keeps the first one offered, which is this launch drain's
        // first `INBOX` page. Zero is no baseline — the `Sent` feed,
        // or a mailbox nothing was ever stored in, whose
        // next page offers again.
        if page.highest_modseq > 0 {
            manager.offer_mail_flag_baseline(page.highest_modseq);
        }
        // A record the source could not open is skipped, never blocking: the
        // cursor moves past it like any other, so everything after it keeps
        // arriving, and the manager records it so the page can tell the user
        // (`mail-app-surface.md` § Inbound client receive → *Unopenable
        // records*; the containment condition of `nest/common.md` § Client-state
        // recoverability). The open is deterministic for this key set, so a
        // retry would only meet the same answer — a relaunch re-drains from
        // UID 0 with whatever keys the account holds by then, which is the one
        // retry that can differ.
        for skipped in page.skipped {
            tracing::warn!(
                mailbox = ?skipped.mailbox,
                uid = skipped.uid,
                "inbound mail record could not be opened; skipped: {}",
                skipped.reason
            );
            if skipped.uid > *after_uid {
                *after_uid = skipped.uid;
            }
            manager.note_unopenable_mail(skipped.mailbox, skipped.uid);
        }
        for rec in page.records {
            if rec.uid > *after_uid {
                *after_uid = rec.uid;
            }
            // An opened record retires any earlier skip of the same record —
            // the re-drain after a key change is what lets a notice clear.
            manager.retire_unopenable_mail(rec.mailbox, rec.uid);
            if !seen.insert(rec.message_id.clone()) {
                continue; // already ingested this segment record
            }
            // On-device spam: the source scored this message as junk (moving it
            // INBOX→Junk this pass), so advance the cursor + dedup it but keep it
            // OUT of the thread view — the user never sees on-device-detected spam
            // in their inbox (`mail-spam.md` § Re-file timing).
            if rec.suppress_from_view {
                continue;
            }
            if ingest_inbound_record(manager, &rec)? {
                ingested += 1;
            }
        }
        if !more {
            break;
        }
    }
    Ok(ingested)
}

/// Bring the mail rail's read state level with the nest over the `INBOX`
/// source (`conversation-read-state.md` § Mail: `\Seen` is the marker): send
/// the `\Seen` writes reads have owed ([`flush_owed_mail_seen`]), then apply
/// every flag change made elsewhere since the cursor
/// ([`drain_mail_flag_changes`]). Writes first, so a change delivered in the
/// same pass can never predate a read still in flight. The receive loop runs
/// it on every mail sweep — the periodic backstop — and on the
/// `fauna.mail.flags_changed` wake.
pub async fn sync_mail_read_state(inbox: &dyn InboundMailSource, manager: &ConversationsManager) {
    flush_owed_mail_seen(inbox, manager).await;
    drain_mail_flag_changes(inbox, manager).await;
}

/// Send every owed `\Seen` write as ONE batched `mark_seen`. A transient
/// failure keeps the batch owed for the next sweep; a source that does not
/// serve the kind (`Unsupported` — anything but `INBOX`) ends syncing for the
/// run, silently. Every nest refusal is a transient failure here: the
/// older-nest `unknown_kind` reading left with the 2026-09-24 compat-remnant
/// sweep (`mail-app-surface.md` § Read state → *Compatibility*).
pub async fn flush_owed_mail_seen(inbox: &dyn InboundMailSource, manager: &ConversationsManager) {
    let uids = manager.take_owed_mail_seen();
    if uids.is_empty() {
        return;
    }
    match inbox.mark_seen(uids.clone()).await {
        Ok(()) => {}
        Err(MailFlagCallError::Unsupported) => manager.note_mail_read_sync_unsupported(),
        Err(MailFlagCallError::Failed(reason)) => {
            tracing::warn!(
                count = uids.len(),
                "mail mark_seen failed; kept for the next sweep: {reason}"
            );
            manager.requeue_mail_seen(uids);
        }
    }
}

/// Page `flag_changes` from the manager's cursor to the mailbox's tip,
/// applying each page. Nothing to do before the launch drain has named a
/// baseline. A failure leaves the cursor where the last applied page put it.
pub async fn drain_mail_flag_changes(
    inbox: &dyn InboundMailSource,
    manager: &ConversationsManager,
) {
    while let Some((since_modseq, after_uid)) = manager.mail_flag_cursor() {
        match inbox.flag_changes(since_modseq, after_uid, 0).await {
            Ok(page) => {
                manager.apply_mail_flag_changes(&page);
                // An empty page that claims more cannot move the cursor
                // forward by its own changes; stop rather than spin.
                if !page.more || page.changes.is_empty() {
                    return;
                }
            }
            Err(MailFlagCallError::Unsupported) => {
                manager.note_mail_read_sync_unsupported();
                return;
            }
            Err(MailFlagCallError::Failed(reason)) => {
                tracing::warn!("mail flag_changes failed; retried on the next sweep: {reason}");
                return;
            }
        }
    }
}

/// Parse one decrypted inbound record and ingest it into `manager` as an
/// SMTP-rail message. Returns `true` when a usable message was ingested, `false`
/// when the record carried no usable `From` (an unaddressable message can't be
/// bucketed into a thread, so it's skipped — not fatal).
///
/// Shared by [`poll_inbound_mail`] (the native async-`InboundMailSource` driver)
/// **and** the wasm client's JS-driven poll: the wasm WS-RPC client is `Rc`-based
/// and `!Send`, so it can't go through the `InboundMailSource: Send + Sync` seam;
/// JS fetches + decrypts a record itself and calls this directly. Both paths reuse
/// the same parse + bucket + ingest logic (priority #2).
///
/// The caller owns *per-feed* dedup (`poll_inbound_mail`'s `seen` set, keyed by
/// server segment-record id; the wasm manager's own seen set) — the cheap guard
/// that skips re-decoding a record already pulled this session. `ingest_inbound`
/// additionally dedups by **message id**, which catches what the segment-id `seen`
/// set structurally cannot: a server-side Sent copy whose RFC `Message-ID` matches
/// a message already in the view (the sender's local echo on send). See
/// `ConversationsManager::ingest_inbound`.
pub fn ingest_inbound_record(
    manager: &ConversationsManager,
    rec: &InboundMailRecord,
) -> Result<bool, BackendError> {
    match inbound_record_to_message(rec) {
        Some((msg, attachment_bytes)) => {
            // Cache each extracted attachment's plaintext bytes under its
            // `blob_hash` before ingesting, so the rendered bubble can load the
            // real file via `ConversationsManager::attachment_bytes` — and
            // remember which record they came from: the store is a bounded
            // cache, and the bytes keep resting in this record's MIME, so an
            // evicted one is re-read from `(mailbox, uid)` rather than
            // rendering declared (`conversations.md` § Attachments →
            // *Retention*; `refill_evicted_mail_attachments`). The mailbox is
            // half the key: INBOX and Sent number their UIDs independently.
            for (blob_hash, bytes) in attachment_bytes {
                if manager.cache_attachment_bytes_checked(&blob_hash, bytes) {
                    manager.remember_attachment_coordinates(
                        blob_hash,
                        AttachmentCoordinates::Smtp(MailRecordCoordinates {
                            mailbox: rec.mailbox,
                            uid: rec.uid,
                        }),
                    );
                }
            }
            // The segment-record id rides beside the message so the index sink
            // can stamp it as the doc's secondary identity — the spelling the
            // MDA's `SEARCH` coverage asks in (`content-index.md` § Where the
            // index is built → the 2026-08-10 carrier ruling).
            manager.ingest_inbound_identified(
                msg,
                Some(rec.message_id.as_slice()),
                Some(crate::store::threads::MailArrival {
                    mailbox: rec.mailbox,
                    uid: rec.uid,
                    has_seen_flag: rec.has_seen_flag,
                }),
            )?;
            Ok(true)
        }
        None => Ok(false),
    }
}

/// The attachments one mail record must yield again: every wanted handle
/// whose bytes rest in that record, grouped so the record is read once however
/// many of its attachments a render missed. Serialized because the web
/// receive loop does the re-read in JS and hands each record back
/// (`WasmConversationsManager::refill_mail_record`).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WantedMailRecord {
    pub record: MailRecordCoordinates,
    pub blob_hashes: Vec<String>,
}

/// Drain the mail sweep's share of the attachment store's wants, one entry
/// per record in first-miss order. FaunaMls wants stay for the conversation
/// sweep.
pub fn take_wanted_mail_records(manager: &ConversationsManager) -> Vec<WantedMailRecord> {
    let mut grouped: Vec<WantedMailRecord> = Vec::new();
    for (blob_hash, record) in manager.take_wanted_mail_attachments() {
        match grouped.iter_mut().find(|w| w.record == record) {
            Some(want) => want.blob_hashes.push(blob_hash),
            None => grouped.push(WantedMailRecord {
                record,
                blob_hashes: vec![blob_hash],
            }),
        }
    }
    grouped
}

/// Cache again the wanted attachments of one re-read mail record. `rfc5322`
/// is the record's decrypted message, or `None` when the mailbox no longer
/// holds it. The MIME is re-parsed through the same
/// [`crate::html_markdown::extract_attachments`] the first ingest used and each
/// wanted part goes through the store's one door, so it is verified against its
/// handle exactly as on first receive. A wanted handle the record does not
/// yield — or every handle of a record that is gone — is forgotten, so it
/// renders declared from then on instead of being asked for every cycle.
/// Returns how many attachments were cached again; notifies nobody.
fn cache_mail_record_attachments(
    manager: &ConversationsManager,
    want: &WantedMailRecord,
    rfc5322: Option<&[u8]>,
) -> usize {
    let mut extracted = rfc5322
        .map(crate::html_markdown::extract_attachments)
        .unwrap_or_default();
    let mut refilled = 0usize;
    for blob_hash in &want.blob_hashes {
        let bytes = extracted
            .iter()
            .position(|(snap, _)| &snap.blob_hash == blob_hash)
            .map(|i| extracted.swap_remove(i).1);
        let cached = bytes.is_some_and(|b| manager.cache_attachment_bytes_checked(blob_hash, b));
        if cached {
            refilled += 1;
        } else {
            manager.forget_attachment_coordinates(blob_hash);
        }
    }
    refilled
}

/// One record's refill plus the observer notification it owes — the entry
/// point for a receive loop that re-reads records one at a time outside Rust
/// (the web loop's JS-driven fetch; `fetched` is `None` when the mailbox no
/// longer holds the record). See [`refill_evicted_mail_attachments`] for the
/// rule; this is its per-record half.
pub fn refill_mail_record_attachments(
    manager: &ConversationsManager,
    want: &WantedMailRecord,
    rfc5322: Option<&[u8]>,
) -> usize {
    let refilled = cache_mail_record_attachments(manager, want, rfc5322);
    if refilled > 0 {
        manager.notify();
    }
    refilled
}

/// Fetch again every mail attachment a render has missed since the last
/// cycle — the SMTP half of the attachment store's retention rule
/// (`docs/goal/ui/conversations.md` § Attachments → *Retention*;
/// `crate::store::attachments`). The store holds at most its budget; the bytes
/// of an evicted mail attachment still rest in its record's MIME, and the ingest
/// remembered which record (`ingest_inbound_record`, `(mailbox, uid)`), so a
/// miss is repaired by re-reading exactly that record from its own mailbox
/// ([`InboundMailSource::fetch_one`]) and re-parsing it. Observers are
/// notified once, so the render that missed asks again and hits.
///
/// Runs once per mail sweep, after the feeds are polled (native `mail_sweep!`
/// and `ConversationsSession::poll_mail`; the web loop drives
/// [`refill_mail_record_attachments`] per record). A record the mailbox no
/// longer holds — moved INBOX→Junk, expunged — forgets its handles; a transport
/// failure, or a mailbox with no wired source, keeps them remembered for the
/// next render's ask. Returns how many attachments were cached again.
pub async fn refill_evicted_mail_attachments(
    inbox: Option<&dyn InboundMailSource>,
    sent: Option<&dyn InboundMailSource>,
    manager: &ConversationsManager,
) -> usize {
    let mut refilled = 0usize;
    for want in take_wanted_mail_records(manager) {
        let source = match want.record.mailbox {
            MailFeed::Inbox => inbox,
            MailFeed::Sent => sent,
        };
        let Some(source) = source else {
            continue;
        };
        match source.fetch_one(want.record.uid).await {
            Ok(Some(record)) => {
                refilled += cache_mail_record_attachments(manager, &want, Some(&record.rfc5322))
            }
            Ok(None) => {
                cache_mail_record_attachments(manager, &want, None);
            }
            Err(e) => tracing::warn!(
                mailbox = ?want.record.mailbox,
                uid = want.record.uid,
                error = %e,
                "re-reading a mail record for an evicted attachment failed; asked again on the next miss"
            ),
        }
    }
    if refilled > 0 {
        manager.notify();
    }
    refilled
}

/// An extracted attachment's content handle (`blob_hash`) paired with its
/// plaintext bytes, for the receive path to cache before ingesting.
type CachedAttachment = (String, Vec<u8>);

/// Map one decrypted inbound record onto a `RailInboundMessage`, plus the
/// `(blob_hash, bytes)` of each extracted attachment for the caller to cache.
/// Returns `None` when the message has no usable `From` (an unaddressable
/// message can't be bucketed into a thread).
fn inbound_record_to_message(
    rec: &InboundMailRecord,
) -> Option<(RailInboundMessage, Vec<CachedAttachment>)> {
    // Headers (From/To/Subject/Message-ID/In-Reply-To) come from the lean
    // hand-rolled header parser; the *body* goes through MIME-aware selection:
    // a `text/html` alternative is converted to markdown and stamped
    // `BodyFormat::Markdown`, a plain message stays `PlainText`
    // (`docs/goal/behavior/html-mail.md`). This replaces the old raw-MIME-as-body
    // + hard-coded `PlainText`, which showed inbound HTML as literal tags.
    let p = crate::rfc5322::parse_message(&rec.rfc5322);
    let (body, body_format) = crate::html_markdown::inbound_mail_body(&rec.rfc5322);
    let sender = TypedAddress::Email {
        email_address: p.from?,
    };
    let recipients =
        p.to.into_iter()
            .map(|email_address| TypedAddress::Email { email_address })
            .collect();
    // Prefer the RFC `Message-ID`; fall back to a stable synthetic id from the
    // server segment-record id so threading/reply-matching always has a key.
    let message_id = MessageId(
        p.message_id
            .unwrap_or_else(|| format!("<{}@fauna.inbox>", hex::encode(&rec.message_id))),
    );
    // Extract MIME attachment parts (mail-parser decodes the transfer-encoding).
    // The snapshot carries the metadata + `blob_hash`; the bytes ride out
    // separately so the caller can cache them keyed by the same hash.
    let extracted = crate::html_markdown::extract_attachments(&rec.rfc5322);
    let attachments: Vec<AttachmentSnapshot> =
        extracted.iter().map(|(snap, _)| snap.clone()).collect();
    let attachment_bytes: Vec<CachedAttachment> = extracted
        .into_iter()
        .map(|(snap, bytes)| (snap.blob_hash, bytes))
        .collect();
    Some((
        RailInboundMessage {
            rail: Rail::Smtp,
            sender,
            recipients,
            subject: p.subject,
            body,
            body_format,
            timestamp_ms: rec.internal_date_ms,
            message_id,
            in_reply_to: p.in_reply_to.map(MessageId),
            attachments,
            badges: MessageBadges::default(),
            legal_takedown_ref: None,
            // Not on the account data plane — see the rail note on `SendOutcome`.
            plane_ref: None,
        },
        attachment_bytes,
    ))
}
