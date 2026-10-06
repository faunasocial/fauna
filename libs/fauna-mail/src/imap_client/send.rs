//! The client-glue call layer over `fauna.bridges.*_import_session` /
//! `import_message[_batch]` — the producer-side SEND path
//! `mailbox-migration.md` § Implementation status today names as the one
//! remaining unbuilt piece of Track 2/D ("no shipping app calls the import
//! RPCs at all yet … nothing turns them into an RPC").
//!
//! [`BatchPacker`](super::BatchPacker) already turns fetched messages into
//! [`ImportUnit`]s; this module turns an `ImportUnit` into the matching
//! WS-RPC call. Generic over [`RpcRequester`] — wasm-clean, like the rest of
//! `imap_client` — so one implementation serves every app: native (linux,
//! tui, and the four UniFFI apps via `fauna-client`) and the web SPA (via
//! `fauna-wasm`) alike, mirroring
//! `fauna_client_conversations::ConversationsClient` (priority #2).
//!
//! **Scope:** every `ImportUnit` [`BatchPacker`](super::BatchPacker) can
//! produce today carries an inline `body` — nothing client-side stages an
//! over-ceiling body yet (`to_item`'s `staged_body: None` comment), so this
//! module sends exactly what it is given and does not itself decide when to
//! stage. Wiring a staged-body producer (AEAD-seal + chunk-upload over the
//! HTTP byte plane, `mail-message-size.md` § Message size limits) is a
//! separate, HTTP-plane-owning slice.

use fauna_protocol::RpcRequester;
use fauna_protocol::bridge_routing::{
    FailImportSessionRequest, ImportMessageBatchReply, ImportMessageBatchRequest,
    ImportMessageReply, ImportMessageRequest, ImportSessionActionReply, ImportSessionActionRequest,
    ImportSessionInfo, ListImportSessionsReply, ListImportSessionsRequest, StartImportSessionReply,
    StartImportSessionRequest,
};

use super::ImportUnit;

/// One sent [`ImportUnit`]'s reply — the single- and batch-shaped wire
/// replies stay distinct types (they are, on the wire), but a caller
/// tallying progress usually wants the three counters either shape carries.
#[derive(Debug, Clone, PartialEq)]
pub enum ImportUnitReply {
    Single(ImportMessageReply),
    Batch(ImportMessageBatchReply),
}

impl ImportUnitReply {
    pub fn imported_count(&self) -> u64 {
        match self {
            Self::Single(r) => r.imported_count,
            Self::Batch(r) => r.imported_count,
        }
    }

    pub fn skipped_count(&self) -> u64 {
        match self {
            Self::Single(r) => r.skipped_count,
            Self::Batch(r) => r.skipped_count,
        }
    }

    pub fn errored_count(&self) -> u64 {
        match self {
            Self::Single(r) => r.errored_count,
            Self::Batch(r) => r.errored_count,
        }
    }
}

/// Typed WS-RPC client for the nine-kind import RPC surface
/// (`mailbox-migration.md` § RPC surface). Holds nothing but the requester —
/// the wizard's session-id/progress state lives in the caller (the app
/// controller), since it differs per-platform in how it is persisted/rendered.
pub struct MailImportClient<R> {
    nest: R,
}

impl<R: RpcRequester> MailImportClient<R> {
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    /// `fauna.bridges.start_import_session` — the wizard's durable commit
    /// point (§ UX shape step 4). Replay-forbidden: the session id is
    /// server-minted, so a caller that disconnects mid-call reconciles via
    /// [`Self::list_sessions`] rather than blindly retrying.
    ///
    /// `scope` is the wizard's selected source mailbox names — recorded so a
    /// session resumed after a client restart can learn which mailboxes to
    /// re-`EXAMINE` (`mailbox-migration.md` § Resume protocol; the nest
    /// echoes the stored scope row).
    /// `source_sealed` is the label this client minted over the descriptor
    /// with `fauna_core::label_custody::seal_import_source`, so the descriptor
    /// stops resting in plaintext nest-side (`encryption-at-rest.md`
    /// § Implementation status today, bullet 15 (b)). It is a parameter
    /// rather than something minted here because this type is generic over its
    /// requester and holds no keypair: the root is the owner's, and only the
    /// connection-owning facade can reach it — the same split as
    /// `SyncClient::seal_device_label`. `None` from a keyless caller rests
    /// sealless, the ratified degrade.
    /// `date_from` is the scope step's "since" date, the bare `YYYY-MM-DD`
    /// the user typed (empty = unbounded). It rides the start call because the
    /// row is the only durable record of the range: a resumed session that
    /// never recorded it can only import everything the user excluded
    /// (`mailbox-migration.md` § Wizard steps step 3).
    pub async fn start_session(
        &self,
        source_descriptor: impl Into<String>,
        total_count: u64,
        scope: Vec<String>,
        date_from: impl Into<String>,
        source_sealed: Option<Vec<u8>>,
    ) -> Result<StartImportSessionReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.start_import_session",
                StartImportSessionRequest {
                    source_descriptor: source_descriptor.into(),
                    total_count,
                    scope,
                    date_from: date_from.into(),
                    source_sealed: source_sealed.map(serde_bytes::ByteBuf::from),
                },
            )
            .await
    }

    /// Send one packed [`ImportUnit`] as the matching wire kind — `Single` as
    /// `fauna.bridges.import_message`, `Batch` as
    /// `fauna.bridges.import_message_batch` (§ Per-message flow step 3,
    /// § Batching).
    ///
    /// `revised_total_count` replaces the session's estimate when the caller
    /// has learned something truer than the source's `EXISTS` — which a date
    /// filter always does, since `EXISTS` counts a whole mailbox and the range
    /// takes only part of it (`mailbox-migration.md` § Progress lives
    /// nest-side). `None` leaves the stored total alone. It rides the batch
    /// kind only, which is the kind a walk overwhelmingly sends.
    pub async fn send_unit(
        &self,
        session_id: impl Into<String>,
        unit: ImportUnit,
        skip_dedup: bool,
        revised_total_count: Option<u64>,
    ) -> Result<ImportUnitReply, R::Error> {
        let session_id = session_id.into();
        match unit {
            ImportUnit::Single(item) => {
                let reply: ImportMessageReply = self
                    .nest
                    .request(
                        "fauna.bridges.import_message",
                        ImportMessageRequest {
                            session_id,
                            message: *item,
                            skip_dedup,
                        },
                    )
                    .await?;
                Ok(ImportUnitReply::Single(reply))
            }
            ImportUnit::Batch(items) => {
                let reply: ImportMessageBatchReply = self
                    .nest
                    .request(
                        "fauna.bridges.import_message_batch",
                        ImportMessageBatchRequest {
                            session_id,
                            messages: items,
                            skip_dedup,
                            revised_total_count,
                        },
                    )
                    .await?;
                Ok(ImportUnitReply::Batch(reply))
            }
        }
    }

    /// `fauna.bridges.list_import_sessions` — resume protocol step 1: every
    /// non-expired session for the caller, `running`/`paused` resumable.
    pub async fn list_sessions(&self) -> Result<Vec<ImportSessionInfo>, R::Error> {
        let reply: ListImportSessionsReply = self
            .nest
            .request(
                "fauna.bridges.list_import_sessions",
                ListImportSessionsRequest {},
            )
            .await?;
        Ok(reply.sessions)
    }

    pub async fn pause_session(
        &self,
        session_id: impl Into<String>,
    ) -> Result<ImportSessionInfo, R::Error> {
        self.session_action("fauna.bridges.pause_import_session", session_id.into())
            .await
    }

    pub async fn resume_session(
        &self,
        session_id: impl Into<String>,
    ) -> Result<ImportSessionInfo, R::Error> {
        self.session_action("fauna.bridges.resume_import_session", session_id.into())
            .await
    }

    /// Already-imported messages are kept — cancelling never deletes
    /// (§ UX shape step 5).
    pub async fn cancel_session(
        &self,
        session_id: impl Into<String>,
    ) -> Result<ImportSessionInfo, R::Error> {
        self.session_action("fauna.bridges.cancel_import_session", session_id.into())
            .await
    }

    pub async fn finalize_session(
        &self,
        session_id: impl Into<String>,
    ) -> Result<ImportSessionInfo, R::Error> {
        self.session_action("fauna.bridges.finalize_import_session", session_id.into())
            .await
    }

    async fn session_action(
        &self,
        kind: &'static str,
        session_id: String,
    ) -> Result<ImportSessionInfo, R::Error> {
        let reply: ImportSessionActionReply = self
            .nest
            .request(kind, ImportSessionActionRequest { session_id })
            .await?;
        Ok(reply.session)
    }

    /// `fauna.bridges.fail_import_session` — the client reports a
    /// session-fatal source-side condition (source auth failure, error
    /// budget exhausted).
    pub async fn fail_session(
        &self,
        session_id: impl Into<String>,
        reason: impl Into<String>,
    ) -> Result<ImportSessionInfo, R::Error> {
        let reply: ImportSessionActionReply = self
            .nest
            .request(
                "fauna.bridges.fail_import_session",
                FailImportSessionRequest {
                    session_id: session_id.into(),
                    reason: reason.into(),
                },
            )
            .await?;
        Ok(reply.session)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::{RecordingRequester, RejectingRequester};
    use fauna_protocol::bridge_routing::ImportMessageOutcome;
    use std::sync::Arc;

    fn item(uid: u32) -> fauna_protocol::bridge_routing::ImportMessageItem {
        fauna_protocol::bridge_routing::ImportMessageItem {
            mailbox: "INBOX".into(),
            flags: vec![],
            body: b"Subject: x\r\n\r\nbody".to_vec(),
            timestamp: 0,
            body_size: 19,
            sender_domain: "example.com".into(),
            source_uid: uid,
            source_uid_validity: 1,
            dedup_key: format!("key-{uid}"),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn start_session_sends_source_descriptor_and_count() {
        let m = RejectingRequester::new().reply(
            "fauna.bridges.start_import_session",
            &StartImportSessionReply {
                session_id: "sess-1".into(),
            },
        );
        let client = MailImportClient::new(m);
        let reply = client
            .start_session("gmail:user@example.com", 42, vec!["INBOX".into()], "", None)
            .await
            .unwrap();
        assert_eq!(reply.session_id, "sess-1");
    }

    #[tokio::test]
    async fn start_session_puts_the_selected_scope_on_the_wire() {
        fn reply(kind: &'static str) -> Vec<u8> {
            match kind {
                "fauna.bridges.start_import_session" => {
                    fauna_protocol::encode_canonical(&StartImportSessionReply {
                        session_id: "sess-1".into(),
                    })
                    .expect("encode reply")
                    .to_vec()
                }
                other => panic!("unhandled kind {other}"),
            }
        }
        let recorder = Arc::new(RecordingRequester::new(reply));
        let client = MailImportClient::new(recorder.clone());
        client
            .start_session(
                "gmail:user@example.com",
                42,
                vec!["INBOX".into(), "Sent".into()],
                "2023-11-14",
                None,
            )
            .await
            .unwrap();
        let (kind, payload) = recorder.recorded();
        assert_eq!(kind, "fauna.bridges.start_import_session");
        let decoded: StartImportSessionRequest =
            fauna_protocol::decode_strict(&payload).expect("decode request");
        assert_eq!(decoded.scope, vec!["INBOX".to_string(), "Sent".to_string()]);
        // The since date rides the same call, and for the same reason: the row
        // is the only durable record of what the session was allowed to take.
        assert_eq!(decoded.date_from, "2023-11-14");
    }

    /// The seal the caller minted reaches the nest **on the wire**, and a
    /// keyless caller's `None` is omitted rather than sent as an empty blob
    /// (`skip_serializing_if`) — an empty `source_sealed` would satisfy the
    /// nest's NOT-NULL-ness while opening to nothing, which is worse than
    /// resting sealless.
    ///
    /// This is the arm the original defect lived in: nest-side coverage that
    /// supplies its own seal cannot see a client that stopped minting one.
    #[tokio::test]
    async fn start_session_puts_the_minted_source_seal_on_the_wire() {
        fn reply(kind: &'static str) -> Vec<u8> {
            match kind {
                "fauna.bridges.start_import_session" => {
                    fauna_protocol::encode_canonical(&StartImportSessionReply {
                        session_id: "sess-1".into(),
                    })
                    .expect("encode reply")
                    .to_vec()
                }
                other => panic!("unhandled kind {other}"),
            }
        }

        let key = fauna_core::crypto::BackupKey::derive(&[0x5eu8; 32]);
        let root = fauna_core::path_crypto::LabelRoot::owner_of(&key);
        let sealed = fauna_core::label_custody::seal_import_source(&root, "gmail:user@example.com")
            .expect("seal");

        let recorder = Arc::new(RecordingRequester::new(reply));
        let client = MailImportClient::new(recorder.clone());
        client
            .start_session(
                "gmail:user@example.com",
                42,
                vec![],
                "",
                Some(sealed.clone()),
            )
            .await
            .unwrap();
        let (_, payload) = recorder.recorded();
        let decoded: StartImportSessionRequest =
            fauna_protocol::decode_strict(&payload).expect("decode request");
        assert_eq!(
            decoded.source_sealed.as_ref().map(|b| &b[..]),
            Some(&sealed[..]),
            "the minted seal must reach the nest — without it the descriptor \
             rests in plaintext and the boot scrub destroys the label"
        );

        // A keyless caller sends no field at all.
        let recorder = Arc::new(RecordingRequester::new(reply));
        let client = MailImportClient::new(recorder.clone());
        client
            .start_session("gmail:user@example.com", 42, vec![], "", None)
            .await
            .unwrap();
        let (_, payload) = recorder.recorded();
        let decoded: StartImportSessionRequest =
            fauna_protocol::decode_strict(&payload).expect("decode request");
        assert!(decoded.source_sealed.is_none());
    }

    #[tokio::test]
    async fn a_single_unit_sends_import_message_not_the_batch_kind() {
        let m = RejectingRequester::new().reply(
            "fauna.bridges.import_message",
            &ImportMessageReply {
                outcome: ImportMessageOutcome::Imported {
                    message_id: b"id".to_vec(),
                    uid: 7,
                    uid_validity: 1,
                },
                imported_count: 1,
                skipped_count: 0,
                errored_count: 0,
            },
        );
        let client = MailImportClient::new(m);
        let reply = client
            .send_unit("sess-1", ImportUnit::Single(Box::new(item(7))), false, None)
            .await
            .unwrap();
        assert_eq!(reply.imported_count(), 1);
        assert!(matches!(reply, ImportUnitReply::Single(_)));
    }

    #[tokio::test]
    async fn a_batch_unit_sends_import_message_batch_with_every_item() {
        let m = RejectingRequester::new().reply(
            "fauna.bridges.import_message_batch",
            &ImportMessageBatchReply {
                outcomes: vec![
                    ImportMessageOutcome::Imported {
                        message_id: b"a".to_vec(),
                        uid: 1,
                        uid_validity: 1,
                    },
                    ImportMessageOutcome::Skipped {
                        reason: "dedup".into(),
                    },
                ],
                imported_count: 1,
                skipped_count: 1,
                errored_count: 0,
            },
        );
        let client = MailImportClient::new(m);
        let unit = ImportUnit::Batch(vec![item(1), item(2)]);
        let reply = client
            .send_unit("sess-1", unit, false, Some(9))
            .await
            .unwrap();
        assert_eq!(reply.imported_count(), 1);
        assert_eq!(reply.skipped_count(), 1);
        assert!(matches!(reply, ImportUnitReply::Batch(_)));
    }

    /// A revised total reaches the nest on the batch kind — the only call a
    /// date-filtered walk has to correct the source's whole-mailbox `EXISTS`
    /// with (`mailbox-migration.md` § Progress lives nest-side).
    #[tokio::test]
    async fn a_batch_carries_the_revised_total_count() {
        fn reply(kind: &'static str) -> Vec<u8> {
            match kind {
                "fauna.bridges.import_message_batch" => {
                    fauna_protocol::encode_canonical(&ImportMessageBatchReply {
                        outcomes: vec![ImportMessageOutcome::Imported {
                            message_id: b"a".to_vec(),
                            uid: 1,
                            uid_validity: 1,
                        }],
                        imported_count: 1,
                        skipped_count: 0,
                        errored_count: 0,
                    })
                    .expect("encode reply")
                    .to_vec()
                }
                other => panic!("unhandled kind {other}"),
            }
        }
        let recorder = Arc::new(RecordingRequester::new(reply));
        let client = MailImportClient::new(recorder.clone());
        client
            .send_unit("sess-1", ImportUnit::Batch(vec![item(1)]), false, Some(2))
            .await
            .unwrap();
        let (kind, payload) = recorder.recorded();
        assert_eq!(kind, "fauna.bridges.import_message_batch");
        let decoded: ImportMessageBatchRequest =
            fauna_protocol::decode_strict(&payload).expect("decode request");
        assert_eq!(decoded.revised_total_count, Some(2));

        // And `None` leaves the session's stored estimate alone.
        let recorder = Arc::new(RecordingRequester::new(reply));
        let client = MailImportClient::new(recorder.clone());
        client
            .send_unit("sess-1", ImportUnit::Batch(vec![item(1)]), false, None)
            .await
            .unwrap();
        let (_, payload) = recorder.recorded();
        let decoded: ImportMessageBatchRequest =
            fauna_protocol::decode_strict(&payload).expect("decode request");
        assert_eq!(decoded.revised_total_count, None);
    }

    #[tokio::test]
    async fn session_lifecycle_calls_hit_the_right_kinds_and_unwrap_the_shared_reply() {
        let session = ImportSessionInfo {
            session_id: "sess-1".into(),
            source_descriptor: "gmail:user@example.com".into(),
            state: "paused".into(),
            ..Default::default()
        };
        let m = RejectingRequester::new()
            .reply(
                "fauna.bridges.pause_import_session",
                &ImportSessionActionReply {
                    session: session.clone(),
                },
            )
            .reply(
                "fauna.bridges.resume_import_session",
                &ImportSessionActionReply {
                    session: session.clone(),
                },
            )
            .reply(
                "fauna.bridges.cancel_import_session",
                &ImportSessionActionReply {
                    session: session.clone(),
                },
            )
            .reply(
                "fauna.bridges.finalize_import_session",
                &ImportSessionActionReply {
                    session: session.clone(),
                },
            )
            .reply(
                "fauna.bridges.fail_import_session",
                &ImportSessionActionReply {
                    session: session.clone(),
                },
            )
            .reply(
                "fauna.bridges.list_import_sessions",
                &ListImportSessionsReply {
                    sessions: vec![session.clone()],
                },
            );
        let client = MailImportClient::new(m);

        assert_eq!(
            client.pause_session("sess-1").await.unwrap().session_id,
            "sess-1"
        );
        assert_eq!(
            client.resume_session("sess-1").await.unwrap().state,
            "paused"
        );
        assert_eq!(
            client.cancel_session("sess-1").await.unwrap().session_id,
            "sess-1"
        );
        assert_eq!(
            client.finalize_session("sess-1").await.unwrap().session_id,
            "sess-1"
        );
        assert_eq!(
            client
                .fail_session("sess-1", "source auth failed")
                .await
                .unwrap()
                .session_id,
            "sess-1"
        );
        let sessions = client.list_sessions().await.unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "sess-1");
    }
}
