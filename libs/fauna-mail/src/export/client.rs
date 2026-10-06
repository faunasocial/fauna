//! The client-glue call layer over the twelve `fauna.bridges.*export*` kinds —
//! the export twin of [`imap_client::send::MailImportClient`](crate::imap_client::MailImportClient).
//!
//! Authority: `docs/goal/behavior/mail-export.md` § Wire shapes (the User-class
//! kinds, ratified 2026-09-20 with `list_own_mailboxes`; `restart_export_session`
//! and `fail_export_session` joined them 2026-09-21 and 2026-09-22),
//! § Export pipeline (the client drives; the nest
//! relays) and § Resume (the stream generation every driver call carries).
//!
//! Generic over [`RpcRequester`] — wasm-clean, like the import twin — so one
//! implementation serves every app: native (linux, tui, and the four UniFFI
//! apps via `fauna-client`) and the web SPA (via `fauna-wasm`) alike. It holds
//! nothing but the requester: the session id, the per-session key and the
//! wizard's progress state live in the caller, because each differs per
//! platform in how it is held and rendered.
//!
//! **Why the whole surface lives here rather than in the client machine's
//! glue.** `fauna-client-mail-settings` could call `nest.request(…)` itself, as
//! the export conformance test does by hand. But the kinds and their request
//! shapes are protocol, not UI: putting them here means the wizard glue,
//! any future non-wizard caller, and the tests all speak one typed surface
//! instead of three hand-built payloads that drift apart at the first
//! additive field (priority #2/#3, and the reason `MailImportClient` sits where
//! it does).
//!
//! **Not here: the seal.** [`super::seal`] frames and seals the chunk; this
//! module only carries the already-sealed bytes. Keeping them apart is what
//! makes it obvious that no wire-facing code ever holds the session key.

use fauna_protocol::RpcRequester;
use fauna_protocol::bridge_routing::{
    DiscardExportBlobReply, ExportSessionActionReply, ExportSessionActionRequest,
    ExportSessionInfo, FailExportSessionRequest, FetchExportChunkCiphertextReply,
    FetchExportChunkCiphertextRequest, ListExportSessionsReply, ListExportSessionsRequest,
    ListOwnMailboxesReply, ListOwnMailboxesRequest, RestartExportSessionRequest,
    StartExportSessionReply, StartExportSessionRequest, UploadExportChunkReply,
    UploadExportChunkRequest,
};

/// Typed WS-RPC client for the export surface (§ Wire shapes).
pub struct MailExportClient<R> {
    nest: R,
}

impl<R: RpcRequester> MailExportClient<R> {
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    /// `fauna.bridges.list_own_mailboxes` — the scope step's options.
    ///
    /// The reply is already ordered by the name's **raw bytes**, which is the
    /// § Container shape total order the serializer enforces. A caller that
    /// re-sorts for display must still export in *this* order, or
    /// `ExportSerializer::push` refuses the run.
    pub async fn list_own_mailboxes(&self) -> Result<ListOwnMailboxesReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.list_own_mailboxes",
                ListOwnMailboxesRequest {},
            )
            .await
    }

    /// `fauna.bridges.start_export_session` — the wizard's durable commit point
    /// (§ UX shape step 3).
    ///
    /// `wrapped_session_key` is the per-session key of § Key material, minted
    /// client-side and wrapped under the user's actor key. It is a parameter
    /// rather than something minted here for the same reason the import twin's
    /// sealed source descriptor is: this type is generic over its requester and
    /// holds no keypair, so only the connection-owning facade can reach the
    /// actor key. The nest **requires** it — a session with no wrapped key has
    /// produced a blob no client can ever open.
    ///
    /// Replay-forbidden: the session id is nest-minted, so a caller that
    /// disconnects mid-call reconciles via [`Self::list_export_sessions`]
    /// rather than blindly retrying.
    pub async fn start_export_session(
        &self,
        format: impl Into<String>,
        scope_descriptor: Vec<u8>,
        wrapped_session_key: Vec<u8>,
        total_count: u64,
    ) -> Result<StartExportSessionReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.start_export_session",
                StartExportSessionRequest {
                    format: format.into(),
                    scope_descriptor,
                    wrapped_session_key: Some(serde_bytes::ByteBuf::from(wrapped_session_key)),
                    total_count,
                    ..Default::default()
                },
            )
            .await
    }

    /// `fauna.bridges.list_export_sessions` — the resume list on client
    /// restart. Caller-scoped; takes no actor.
    pub async fn list_export_sessions(&self) -> Result<Vec<ExportSessionInfo>, R::Error> {
        let reply: ListExportSessionsReply = self
            .nest
            .request(
                "fauna.bridges.list_export_sessions",
                ListExportSessionsRequest {},
            )
            .await?;
        Ok(reply.sessions)
    }

    /// `fauna.bridges.fetch_export_chunk_ciphertext` — the **down-leg**.
    ///
    /// Pages one mailbox by ascending UID. `after_uid` is exclusive; 0 starts
    /// the mailbox. Passing 0 for the two ceilings means "the nest's own",
    /// which is what a caller should do unless it has a reason not to — the
    /// numbers are `MAX_EXPORT_FETCH_{MESSAGES,BYTES}` and the nest clamps to
    /// them regardless.
    ///
    /// Read [`FetchExportChunkCiphertextReply::mailbox_done`], never an empty
    /// page, to decide the mailbox ended: a UID gap yields a short page in the
    /// middle of a mailbox, and inferring the end from it truncates the archive
    /// silently.
    pub async fn fetch_export_chunk_ciphertext(
        &self,
        session_id: impl Into<String>,
        mailbox: impl Into<String>,
        after_uid: u32,
    ) -> Result<FetchExportChunkCiphertextReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.fetch_export_chunk_ciphertext",
                FetchExportChunkCiphertextRequest {
                    session_id: session_id.into(),
                    mailbox: mailbox.into(),
                    after_uid,
                    max_messages: 0,
                    max_bytes: 0,
                    ..Default::default()
                },
            )
            .await
    }

    /// `fauna.bridges.upload_export_chunk` — the **up-leg**.
    ///
    /// `sealed_chunk` is already framed and sealed ([`super::seal::
    /// ExportBlobSealer`]); the nest appends it verbatim and parses nothing.
    /// `chunk_idx` must be the session's `next_chunk_idx` — the nest refuses
    /// anything else, which is what turns a reordered or replayed upload into a
    /// typed refusal here instead of an archive that fails AEAD at download.
    /// Take the index from the sealer, never from a counter of your own.
    ///
    /// `stream_generation` is the generation this driver's stream was opened
    /// under — 0 from `start_export_session`, the reply's from
    /// [`Self::restart_export_session`]. A stale one is refused with
    /// [`fauna_protocol::bridge_routing::EXPORT_STREAM_SUPERSEDED`] (§ Resume).
    #[allow(clippy::too_many_arguments)]
    pub async fn upload_export_chunk(
        &self,
        session_id: impl Into<String>,
        stream_generation: u64,
        chunk_idx: u64,
        sealed_chunk: Vec<u8>,
        exported_delta: u64,
        skipped_delta: u64,
        errored_delta: u64,
        last_processed_message_id: String,
        revised_total_count: Option<u64>,
    ) -> Result<UploadExportChunkReply, R::Error> {
        self.nest
            .request(
                "fauna.bridges.upload_export_chunk",
                UploadExportChunkRequest {
                    session_id: session_id.into(),
                    chunk_idx,
                    sealed_chunk,
                    exported_delta,
                    skipped_delta,
                    errored_delta,
                    last_processed_message_id,
                    revised_total_count,
                    stream_generation,
                    ..Default::default()
                },
            )
            .await
    }

    /// `fauna.bridges.pause_export_session`.
    ///
    /// `as_driver_of` — here and on the three transitions below — is § Resume's
    /// only-while-I-am-still-the-driver condition. `Some(generation)` is what a
    /// drive loop passes for its OWN pause / resume / finalize / failure-cancel:
    /// the nest applies it only while that generation is current and answers
    /// `EXPORT_STREAM_SUPERSEDED` otherwise, so a driver another device has
    /// restarted over can never stop the stream that replaced its own. `None`
    /// is the user's control and applies unconditionally.
    pub async fn pause_export_session(
        &self,
        session_id: impl Into<String>,
        as_driver_of: Option<u64>,
    ) -> Result<ExportSessionInfo, R::Error> {
        self.session_action(
            "fauna.bridges.pause_export_session",
            session_id,
            as_driver_of,
        )
        .await
    }

    /// `fauna.bridges.resume_export_session`.
    pub async fn resume_export_session(
        &self,
        session_id: impl Into<String>,
        as_driver_of: Option<u64>,
    ) -> Result<ExportSessionInfo, R::Error> {
        self.session_action(
            "fauna.bridges.resume_export_session",
            session_id,
            as_driver_of,
        )
        .await
    }

    /// `fauna.bridges.restart_export_session` — the **cold** resume (§ Resume):
    /// open a new stream generation on a session whose stream this client
    /// cannot continue. The reply's `stream_generation` is what every later
    /// call of the restarted run carries.
    ///
    /// `wrapped_session_key` is a FRESH key, minted and wrapped exactly as
    /// [`Self::start_export_session`]'s — one key per generation. Replay-
    /// forbidden: a second application would supersede the first one's stream.
    /// `fauna.protocol.unknown_kind` surfaces as an ordinary error
    /// (`ExportSeamError::Nest`).
    pub async fn restart_export_session(
        &self,
        session_id: impl Into<String>,
        wrapped_session_key: Vec<u8>,
        total_count: u64,
    ) -> Result<ExportSessionInfo, R::Error> {
        let reply: ExportSessionActionReply = self
            .nest
            .request(
                "fauna.bridges.restart_export_session",
                RestartExportSessionRequest {
                    session_id: session_id.into(),
                    wrapped_session_key: Some(serde_bytes::ByteBuf::from(wrapped_session_key)),
                    total_count,
                    ..Default::default()
                },
            )
            .await?;
        Ok(reply.session)
    }

    /// `fauna.bridges.cancel_export_session` — abort and unlink the partial
    /// blob.
    pub async fn cancel_export_session(
        &self,
        session_id: impl Into<String>,
        as_driver_of: Option<u64>,
    ) -> Result<ExportSessionInfo, R::Error> {
        self.session_action(
            "fauna.bridges.cancel_export_session",
            session_id,
            as_driver_of,
        )
        .await
    }

    /// `fauna.bridges.fail_export_session` — record a condition that is fatal
    /// to the whole export: the session goes `errored` carrying `reason`, the
    /// partial blob is unlinked, and `BridgeExportError` reaches the user's
    /// other devices. The export twin of `fail_import_session`, and the reason
    /// a client-detected failure no longer has to disguise itself as a cancel
    /// (`mail-export.md` § Resume).
    ///
    /// `as_driver_of` is the same condition as the transitions above, and a
    /// driver always sets it: a failure that is this device's alone must not
    /// dispose of a stream another device restarted. `fauna.protocol.unknown_kind`
    /// surfaces as an ordinary error (`ExportSeamError::Nest`).
    pub async fn fail_export_session(
        &self,
        session_id: impl Into<String>,
        reason: impl Into<String>,
        as_driver_of: Option<u64>,
    ) -> Result<ExportSessionInfo, R::Error> {
        let reply: ExportSessionActionReply = self
            .nest
            .request(
                "fauna.bridges.fail_export_session",
                FailExportSessionRequest {
                    session_id: session_id.into(),
                    reason: reason.into(),
                    stream_generation: as_driver_of,
                    ..Default::default()
                },
            )
            .await?;
        Ok(reply.session)
    }

    /// `fauna.bridges.finalize_export_session` — the blob's terminator frame
    /// has been uploaded; close the session and mint the download URL.
    pub async fn finalize_export_session(
        &self,
        session_id: impl Into<String>,
        as_driver_of: Option<u64>,
    ) -> Result<ExportSessionInfo, R::Error> {
        self.session_action(
            "fauna.bridges.finalize_export_session",
            session_id,
            as_driver_of,
        )
        .await
    }

    /// `fauna.bridges.discard_export_blob` — immediate unlink + row delete
    /// ("Discard now", § Expiry). Idempotent: `existed = false` is the second
    /// discard of the same session, deliberately not an error.
    pub async fn discard_export_blob(
        &self,
        session_id: impl Into<String>,
    ) -> Result<bool, R::Error> {
        let reply: DiscardExportBlobReply = self
            .nest
            .request(
                "fauna.bridges.discard_export_blob",
                ExportSessionActionRequest {
                    session_id: session_id.into(),
                    stream_generation: None,
                    ..Default::default()
                },
            )
            .await?;
        Ok(reply.existed)
    }

    /// The four state-mutating kinds share one request and one reply shape, so
    /// they share one call. Spelling each out separately would be four chances
    /// to send the wrong kind's name with the right payload — which the wire
    /// would accept.
    async fn session_action(
        &self,
        kind: &'static str,
        session_id: impl Into<String>,
        as_driver_of: Option<u64>,
    ) -> Result<ExportSessionInfo, R::Error> {
        let reply: ExportSessionActionReply = self
            .nest
            .request(
                kind,
                ExportSessionActionRequest {
                    session_id: session_id.into(),
                    stream_generation: as_driver_of,
                    ..Default::default()
                },
            )
            .await?;
        Ok(reply.session)
    }
}
