//! Typed-call wrapper for the Layer-3 mail-server adjacent WS-RPC kinds —
//! the user-facing `fauna.email.*` surface clients hit from the
//! account-detail / mail-policy UI: filter-rule CRUD, outbound
//! submission (`fauna.email.send`), and inbound receive
//! (`fauna.email.inbox.fetch`), and the `\Seen` read-state kinds
//! (`fauna.email.inbox.{mark_seen,flag_changes}`).
//!
//! Namespace split from `fauna-client-bridges` (tracked internally) —
//! the `/api/v1/email/*` URL hierarchy and `mail-*` goal-doc family
//! are conceptually mail-server surface, not bridge-management surface.
//! Separate crate keeps the cold-read story honest: a session looking
//! for `EmailClient::filters_list` or `EmailClient::send` lands here,
//! not in the bridges crate.
//!
//! Pattern matches `fauna-client-bridges`: a thin `EmailClient`
//! holding an `Arc<NestClient>`, one async method per kind, no state
//! machine. Higher-level mail state machines (credential rotation,
//! sync, …) live in `fauna-client-mail-settings`; this crate is the
//! typed RPC seam.

use fauna_protocol::RpcRequester;
use fauna_protocol::email::{
    ApplySpamDispositionReply, ApplySpamDispositionRequest, CreateEmailFilterReply,
    CreateEmailFilterRequest, DeleteEmailFilterReply, DeleteEmailFilterRequest, EmailFilter,
    EmailFilterAction, EmailFilterRule, FlagChangesReply, FlagChangesRequest, GetEmailFilterReply,
    GetEmailFilterRequest, InboxFetchReply, InboxFetchRequest, ListEmailFiltersReply,
    ListEmailFiltersRequest, MarkSeenReply, MarkSeenRequest, SendEmailReply, SendEmailRequest,
    UpdateEmailFilterReply, UpdateEmailFilterRequest,
};

pub use fauna_protocol::email;

/// Typed `fauna.email.*` call surface, generic over the WS-RPC transport
/// (`R: RpcRequester`): native call sites pass `Arc<NestClient>`, the wasm SPA
/// passes its `WsRpcClient`. The kind-composition logic is written once here
/// and shared across native + wasm (priority #2). Errors propagate as the
/// transport's `R::Error` (native `NestClientError`, wasm rpc-wasm error).
pub struct EmailClient<R: RpcRequester> {
    nest: R,
}

impl<R: RpcRequester> EmailClient<R> {
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    /// `fauna.email.filters.list` — snapshot of every email-filter rule
    /// the calling actor owns. Replay-safe pure read; the 5 s default
    /// deadline is the `KindRegistry` value (see
    /// `fauna_protocol::KindRegistry::register_email_kinds`).
    pub async fn filters_list(&self) -> Result<Vec<EmailFilter>, R::Error> {
        let reply: ListEmailFiltersReply = self
            .nest
            .request("fauna.email.filters.list", ListEmailFiltersRequest {})
            .await?;
        Ok(reply.filters)
    }

    /// `fauna.email.filters.create` — create a new server-side filter
    /// rule. Replay-safe (the idempotency cache replays the prior reply
    /// on retry, returning the same server-assigned id). Returns the
    /// new row id.
    pub async fn filters_create(
        &self,
        name: impl Into<String>,
        rules: Vec<EmailFilterRule>,
        combination: impl Into<String>,
        action: EmailFilterAction,
        priority: i32,
    ) -> Result<i64, R::Error> {
        let reply: CreateEmailFilterReply = self
            .nest
            .request(
                "fauna.email.filters.create",
                CreateEmailFilterRequest {
                    name: name.into(),
                    rules,
                    combination: combination.into(),
                    action,
                    priority,
                    // Defaults to first-match-wins. Exposing the Sieve `continue`
                    // flag through this typed wrapper + the client UIs is the
                    // client-area follow-up;
                    // the wire field already round-trips so a raw create can set it.
                    continue_on_match: false,
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(reply.id)
    }

    /// `fauna.email.filters.get` — fetch a single filter rule by id.
    /// Replay-safe pure read. Returns `fauna.email.not_found` when no
    /// row matches `(id, calling actor)`.
    pub async fn filters_get(&self, id: i64) -> Result<EmailFilter, R::Error> {
        let reply: GetEmailFilterReply = self
            .nest
            .request(
                "fauna.email.filters.get",
                GetEmailFilterRequest {
                    id,
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(reply.filter)
    }

    /// `fauna.email.filters.update` — overwrite an existing filter
    /// rule (name + rules + combination + action + priority all
    /// replaced). Idempotent overwrite — replay-safe. Returns
    /// `fauna.email.not_found` when no row matches `(id, calling
    /// actor)`. The `{ ok: true }` reply is discarded by the wrapper.
    #[allow(clippy::too_many_arguments)]
    pub async fn filters_update(
        &self,
        id: i64,
        name: impl Into<String>,
        rules: Vec<EmailFilterRule>,
        combination: impl Into<String>,
        action: EmailFilterAction,
        priority: i32,
    ) -> Result<(), R::Error> {
        let _: UpdateEmailFilterReply = self
            .nest
            .request(
                "fauna.email.filters.update",
                UpdateEmailFilterRequest {
                    id,
                    name: name.into(),
                    rules,
                    combination: combination.into(),
                    action,
                    priority,
                    // First-match-wins default; see filters_create — exposing the
                    // `continue` flag is the client-UI follow-up.
                    continue_on_match: false,
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(())
    }

    /// `fauna.email.filters.delete` — remove a filter rule by id.
    /// Idempotent (already-deleted maps to `fauna.email.not_found`);
    /// replay-safe. The `{ ok: true }` reply is discarded.
    pub async fn filters_delete(&self, id: i64) -> Result<(), R::Error> {
        let _: DeleteEmailFilterReply = self
            .nest
            .request(
                "fauna.email.filters.delete",
                DeleteEmailFilterRequest {
                    id,
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(())
    }

    /// `fauna.email.send` — submit an outbound RFC 5322 message for
    /// delivery. In-domain recipients go to local inboxes; out-of-domain
    /// recipients land on the MTA's outbound queue. `forbid_replay=true`
    /// at 30 s — the connection-recovery auto-retry path won't replay
    /// this kind (double-send risk to remote MX). Callers wanting retry
    /// semantics handle that explicitly.
    pub async fn send(
        &self,
        recipients: Vec<String>,
        raw_rfc5322: Vec<u8>,
    ) -> Result<SendEmailReply, R::Error> {
        let reply: SendEmailReply = self
            .nest
            .request(
                "fauna.email.send",
                SendEmailRequest {
                    recipients,
                    raw_rfc5322,
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(reply)
    }

    /// `fauna.email.inbox.fetch` — read one page of the calling actor's own
    /// sealed `INBOX` (the inbound twin of [`send`](Self::send)). User-class,
    /// caller-scoped: the reading actor is the authenticated caller, so this
    /// only ever returns the caller's own mailbox (`docs/goal/behavior/smtp-server.md`
    /// § Inbound client receive). Returns messages with `uid > after_uid`
    /// (`0` = from the start) up to `limit` (`0` = server default; clamped);
    /// page with `after_uid = ` the last message's `uid` until `more` is false.
    ///
    /// Each `sealed_envelope` is the **outer** `fauna_mail::segments::MailRecordEnvelope`
    /// canonical bytes — the caller decodes it and opens its inner
    /// `.encrypted_body` with `open_mail_record` + its MSEK-derived recipient key
    /// (the nest holds no opening key). Replay-safe pure read; the 60 s deadline
    /// is the `KindRegistry` value (bodies can be large).
    pub async fn inbox_fetch(
        &self,
        after_uid: u32,
        limit: u32,
    ) -> Result<InboxFetchReply, R::Error> {
        let reply: InboxFetchReply = self
            .nest
            .request(
                "fauna.email.inbox.fetch",
                InboxFetchRequest {
                    after_uid,
                    limit,
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(reply)
    }

    /// `fauna.email.sent.fetch` — the **Sent** sibling of
    /// [`inbox_fetch`](Self::inbox_fetch): read one page of the calling
    /// actor's own sealed `Sent` mailbox. A message the user sent from an
    /// external MUA (e.g. macOS Mail via SMTP submission) leaves a
    /// server-side `Sent` copy sealed to the sender's own MSEK-derived
    /// recipient key, so the native app can surface its outbound mail in
    /// the unified conversations view (`docs/goal/behavior/smtp-server.md`
    /// § Inbound client receive). User-class, caller-scoped — only ever the
    /// caller's own `Sent`. Same wire types, paging and decrypt as
    /// `inbox_fetch`; the only difference is the kind string (the mailbox is
    /// chosen server-side). Replay-safe pure read; 60 s deadline.
    pub async fn sent_fetch(
        &self,
        after_uid: u32,
        limit: u32,
    ) -> Result<InboxFetchReply, R::Error> {
        let reply: InboxFetchReply = self
            .nest
            .request(
                "fauna.email.sent.fetch",
                InboxFetchRequest {
                    after_uid,
                    limit,
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(reply)
    }

    /// `fauna.email.apply_spam_disposition` — apply the on-device spam
    /// scorer's outcome to the caller's OWN INBOX: watermark every
    /// `scored_uids` message with the internal `$FaunaSpamScored` keyword
    /// (the same watermark the MDA sets), **then** move the `junk_uids`
    /// subset INBOX→Junk (watermark-before-move, so a later "not spam"
    /// move-back isn't re-Junked). `junk_uids` MUST be a subset of
    /// `scored_uids` — the handler rejects a stray UID.
    ///
    /// This is the `User`-class re-file the Fauna app needs: the MDA's
    /// `fauna.bridges.{store_flags,move}` are `BridgeMda`-only, so a client
    /// can't reach them; this single least-privilege kind expresses *only*
    /// this outcome (no arbitrary flag / mailbox). Caller-scoped — a `User`
    /// acts only on their own INBOX. `forbid_replay` is off: the op is
    /// idempotent (re-watermarking a scored message and re-moving an
    /// already-Junk'd one are both no-ops), so the connection-recovery retry
    /// is safe (`docs/goal/behavior/mail-spam.md` § Wire shapes, § Re-file
    /// timing). The reply carries how many messages were watermarked / moved.
    pub async fn apply_spam_disposition(
        &self,
        scored_uids: Vec<u32>,
        junk_uids: Vec<u32>,
    ) -> Result<ApplySpamDispositionReply, R::Error> {
        let reply: ApplySpamDispositionReply = self
            .nest
            .request(
                "fauna.email.apply_spam_disposition",
                ApplySpamDispositionRequest {
                    scored_uids,
                    junk_uids,
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(reply)
    }

    /// `fauna.email.inbox.mark_seen` — mark the caller's own `INBOX` messages
    /// read: add `\Seen` to each named UID that lacks it, and nothing else (no
    /// other flag, no removal, no other mailbox). Unknown UIDs are skipped and
    /// a repeat is a no-op, so the connection-recovery retry is safe. Returns
    /// how many rows gained the flag (`docs/goal/behavior/mail-app-surface.md`
    /// § Read state).
    pub async fn inbox_mark_seen(&self, uids: Vec<u32>) -> Result<MarkSeenReply, R::Error> {
        self.nest
            .request(
                "fauna.email.inbox.mark_seen",
                MarkSeenRequest {
                    uids,
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.email.inbox.flag_changes` — the caller's `INBOX` rows changed
    /// past the cursor, each with its whole current flag set, ordered by
    /// `(modseq, uid)`. Start from the `highest_modseq` of the first
    /// [`inbox_fetch`](Self::inbox_fetch) page (`after_uid = 0`); while `more`
    /// is true resume from the last change's `(modseq, uid)`, then continue
    /// from the reply's `(highest_modseq, 0)`. Call it on the
    /// `fauna.mail.flags_changed` wake and on the periodic backstop.
    pub async fn inbox_flag_changes(
        &self,
        since_modseq: u64,
        after_uid: u32,
        limit: u32,
    ) -> Result<FlagChangesReply, R::Error> {
        self.nest
            .request(
                "fauna.email.inbox.flag_changes",
                FlagChangesRequest {
                    since_modseq,
                    limit,
                    after_uid,
                    extra: Default::default(),
                },
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::{MockRequester, RecordingRequester, block_on};

    #[test]
    fn constructor_builds_over_generic_requester() {
        let _c = EmailClient::new(MockRequester);
    }

    // ── Wire-contract tests ─────────────────────────────────────────────────
    //
    // The smoke test above instantiates `EmailClient` but never issues a call,
    // so it can't catch a wrong kind string or a request that no longer
    // serializes to the shape the nest handler decodes. These tests pin both:
    // each `EmailClient` method must send its exact `fauna.email.*` kind and a
    // payload that round-trips back to the typed request. No nest-side
    // conformance suite routes through this adapter's literal kind strings, so
    // an adapter-method kind rename would otherwise be caught by nothing. The
    // pattern mirrors the `RecordingRequester` in `fauna-client-conversations`
    // / `-events` / `-snapshots` / `-sync` (transport-free, so it runs on every
    // target including wasm); real end-to-end round-trip conformance lives in
    // `bins/fauna-nest/tests/conformance_email_filters.rs`.

    /// This crate's reply table for the shared [`RecordingRequester`]:
    /// one arm per kind, each the minimal valid shape its `Reply` decodes.
    fn reply(kind: &'static str) -> Vec<u8> {
        use email::*;
        match kind {
            "fauna.email.filters.list" => {
                fauna_protocol::encode_canonical(&ListEmailFiltersReply {
                    extra: Default::default(),
                    filters: vec![],
                })
            }
            "fauna.email.filters.create" => {
                fauna_protocol::encode_canonical(&CreateEmailFilterReply {
                    extra: Default::default(),
                    id: 7,
                })
            }
            "fauna.email.filters.get" => fauna_protocol::encode_canonical(&GetEmailFilterReply {
                extra: Default::default(),
                filter: EmailFilter {
                    extra: Default::default(),
                    id: 7,
                    name: "row".into(),
                    rules: vec![],
                    combination: "all".into(),
                    action: EmailFilterAction::Allow,
                    priority: 0,
                    continue_on_match: false,
                    created_at: 0,
                },
            }),
            "fauna.email.filters.update" => {
                fauna_protocol::encode_canonical(&UpdateEmailFilterReply {
                    extra: Default::default(),
                    ok: true,
                })
            }
            "fauna.email.filters.delete" => {
                fauna_protocol::encode_canonical(&DeleteEmailFilterReply {
                    extra: Default::default(),
                    ok: true,
                })
            }
            "fauna.email.send" => fauna_protocol::encode_canonical(&SendEmailReply::default()),
            "fauna.email.inbox.fetch" | "fauna.email.sent.fetch" => {
                fauna_protocol::encode_canonical(&InboxFetchReply::default())
            }
            "fauna.email.apply_spam_disposition" => {
                fauna_protocol::encode_canonical(&ApplySpamDispositionReply::default())
            }
            "fauna.email.inbox.mark_seen" => {
                fauna_protocol::encode_canonical(&MarkSeenReply::default())
            }
            "fauna.email.inbox.flag_changes" => {
                fauna_protocol::encode_canonical(&FlagChangesReply::default())
            }
            other => panic!("RecordingRequester: unhandled kind {other}"),
        }
        .expect("encode reply")
        .to_vec()
    }

    fn client() -> (
        std::sync::Arc<RecordingRequester>,
        EmailClient<std::sync::Arc<RecordingRequester>>,
    ) {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = EmailClient::new(rec.clone());
        (rec, client)
    }

    #[test]
    fn filters_list_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.filters_list()).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.email.filters.list");
        let _req: email::ListEmailFiltersRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
    }

    #[test]
    fn filters_create_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.filters_create(
            "Move newsletters",
            vec![email::EmailFilterRule::SenderDomain {
                domain: "news.example".into(),
            }],
            "any",
            email::EmailFilterAction::FileInto {
                mailbox: "Reading".into(),
            },
            10,
        ))
        .expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.email.filters.create");
        let req: email::CreateEmailFilterRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.name, "Move newsletters");
        assert_eq!(req.combination, "any");
        assert_eq!(req.priority, 10);
    }

    #[test]
    fn filters_get_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.filters_get(42)).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.email.filters.get");
        let req: email::GetEmailFilterRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.id, 42);
    }

    #[test]
    fn filters_update_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.filters_update(
            17,
            "Updated name",
            vec![email::EmailFilterRule::BodyContains {
                text: "spam".into(),
            }],
            "all",
            email::EmailFilterAction::AddLabel {
                label: "Newsletters".into(),
            },
            20,
        ))
        .expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.email.filters.update");
        let req: email::UpdateEmailFilterRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.id, 17);
        assert_eq!(req.name, "Updated name");
        assert_eq!(req.priority, 20);
    }

    #[test]
    fn filters_delete_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.filters_delete(17)).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.email.filters.delete");
        let req: email::DeleteEmailFilterRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.id, 17);
    }

    #[test]
    fn send_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.send(
            vec!["bob@example.com".into(), "carol@other.example".into()],
            b"From: alice@example.com\r\n\r\nHello.".to_vec(),
        ))
        .expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.email.send");
        let req: email::SendEmailRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(
            req.recipients,
            vec![
                "bob@example.com".to_string(),
                "carol@other.example".to_string()
            ]
        );
        assert_eq!(
            req.raw_rfc5322,
            b"From: alice@example.com\r\n\r\nHello.".to_vec()
        );
    }

    #[test]
    fn inbox_fetch_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.inbox_fetch(41, 25)).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.email.inbox.fetch");
        let req: email::InboxFetchRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.after_uid, 41);
        assert_eq!(req.limit, 25);
    }

    #[test]
    fn sent_fetch_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.sent_fetch(63, 10)).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.email.sent.fetch");
        let req: email::InboxFetchRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.after_uid, 63);
        assert_eq!(req.limit, 10);
    }

    #[test]
    fn apply_spam_disposition_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.apply_spam_disposition(vec![10, 11, 12], vec![11])).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.email.apply_spam_disposition");
        let req: email::ApplySpamDispositionRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.scored_uids, vec![10, 11, 12]);
        assert_eq!(req.junk_uids, vec![11]);
    }

    #[test]
    fn inbox_mark_seen_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.inbox_mark_seen(vec![3, 5])).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.email.inbox.mark_seen");
        let req: email::MarkSeenRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.uids, vec![3, 5]);
    }

    #[test]
    fn inbox_flag_changes_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.inbox_flag_changes(40, 7, 100)).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.email.inbox.flag_changes");
        let req: email::FlagChangesRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!((req.since_modseq, req.after_uid, req.limit), (40, 7, 100));
    }
}
