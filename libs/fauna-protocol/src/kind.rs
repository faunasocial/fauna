//! RPC kind registry — per-kind metadata (deadline, replay-forbid).
//! Per spec § 2.5.
//!
//! The registry is built once at app startup (server-side) or lazy-initialized
//! (client-side via `KindRegistry::default_with_protocol_kinds`). Per-feature
//! crates extend it via `register_<area>_handlers`-style functions that
//! consume an `RpcRouterBuilder` (see Plan 3 for the nest-side router).
//! The protocol crate's KindRegistry is metadata-only; handler dispatch
//! is the consumer's concern.

use std::collections::BTreeMap;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RpcKindMeta {
    pub forbid_replay: bool,
    pub default_deadline: Duration,
}

impl RpcKindMeta {
    pub const fn new(forbid_replay: bool, default_deadline: Duration) -> Self {
        Self {
            forbid_replay,
            default_deadline,
        }
    }
}

/// Metadata-only registry. Maps kind string → metadata. Read-only after
/// construction; consumers (nest-side `RpcRouter`, client-side request API)
/// look up metadata when issuing/receiving requests.
#[derive(Debug, Default, Clone)]
pub struct KindRegistry {
    kinds: BTreeMap<String, RpcKindMeta>,
}

impl KindRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, kind: impl Into<String>, meta: RpcKindMeta) {
        self.kinds.insert(kind.into(), meta);
    }

    pub fn meta(&self, kind: &str) -> Option<RpcKindMeta> {
        self.kinds.get(kind).copied()
    }

    pub fn contains(&self, kind: &str) -> bool {
        self.kinds.contains_key(kind)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, RpcKindMeta)> {
        self.kinds.iter().map(|(k, m)| (k.as_str(), *m))
    }

    /// Pre-populated with **only** the protocol-level kinds shipped by Spec Y
    /// itself (`fauna.protocol.echo`).
    ///
    /// ⚠️ This is *not* the production constructor — a client built on it sees
    /// one kind, so every other kind silently falls back to the spec defaults
    /// (30 s deadline, replay permitted). Production clients must use
    /// [`KindRegistry::full`]; this one exists for tests that want an empty
    /// table plus echo.
    pub fn default_with_protocol_kinds() -> Self {
        let mut r = Self::new();
        r.add(
            "fauna.protocol.echo",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        r
    }

    /// **The production constructor** — `fauna.protocol.echo` plus every kind
    /// family the protocol crate declares ([`KindRegistry::register_all_kinds`]).
    ///
    /// Every client that talks to a nest must build its registry this way, so
    /// that per-kind `forbid_replay` and `default_deadline` actually take
    /// effect. `forbid_replay` in particular is load-bearing: it is the *only*
    /// thing standing between a mutation and a blind auto-retry after a
    /// reconnect (the nest's idempotency cache lives on `RpcConnection` and so
    /// starts empty on the new connection — it cannot deduplicate the retry).
    pub fn full() -> Self {
        let mut r = Self::default_with_protocol_kinds();
        r.register_all_kinds();
        r
    }
}

impl KindRegistry {
    /// Register the bridge wrapped-blob kinds. Spec § Kind registry entries.
    pub fn register_bridge_kinds(&mut self) {
        use std::time::Duration;

        // DECIDED, not inherited (82nd pass): all 33 non-atproto kinds still on
        // this constant were read to their SQL. (The nine `fauna.bridges.atproto.*`
        // kinds are deliberately skipped while the full-PDS work is live on
        // them, and the five import state transitions are scoped separately —
        // their replay answers an error where the first call answered success,
        // which is a lookup fix, not a flag question.)
        //
        // Most are genuine reads. Of the rest:
        //   * `fetch_tls_cert_blob` re-seals the on-disk cert and persists on
        //     *every* call. Still idempotent: the write is a keyed upsert on
        //     `(role, bridge_id, domain)` with no accumulator, and the reply is a
        //     fresh-but-equivalent seal of the same cert to the same recipient.
        //   * `select_mailbox` / `list_messages` / `fetch_message_metadata` /
        //     `fetch_index_segments_since` seed the standard mailboxes on first
        //     touch, gated on the `newly_seeded` set the seeder returns — the
        //     same threading that makes the mail ingestion kinds safe.
        //   * `provision_calendar` / `provision_addressbook` key on a
        //     *client-supplied* collection id; the PROPPATCH branch detects a
        //     byte-identical retry and leaves `highestmodseq` alone.
        //   * the CalDAV/CardDAV deletes and `expunge` gate every write on a
        //     non-empty outcome, so a replay writes nothing at all.
        // The two that were NOT idempotent are no longer on this constant or no
        // longer unsafe: `copy` was lifted out and flipped (see C.5 below), and
        // `store_flags` was FIXED to be idempotent (see C.4). Both are pinned.
        let fetch = RpcKindMeta::new(false, Duration::from_secs(5));
        // Also DECIDED, not inherited (79th pass), for the two kinds on it that
        // are not reads: `report_auth_event` and `report_session_close` are
        // idempotent **by construction** — each derives an `idempotency_hash`
        // over its full content tuple and `INSERT OR IGNORE`s against a UNIQUE
        // index (`db/bridge_audit.rs`), so a replay writes nothing. They were
        // read precisely *because* a telemetry-shaped verb is where a hidden
        // accumulator likes to sit — the predicate that caught
        // `check_submission_quota` below — and here it came back clean.
        let fetch_short = RpcKindMeta::new(false, Duration::from_secs(2));
        // DECIDED, not inherited (81st pass): all seven non-atproto kinds on this
        // constant were read to their SQL and are idempotent **by construction**
        // — every one is a caller-keyed `INSERT OR REPLACE` whose key comes from
        // the request (or from the canonical index of the caller's own blob),
        // and every one replies a constant `{ ok: true }`, so state AND reply
        // converge on a replay:
        //
        // - `provision_{wrapped_mls,wrapped_submission_token}_blob` key on
        //   `(actor, credential_id)`; `provision_{mls_snapshot,webdav_keys}_blob`
        //   on the connection's own `actor_id` (no target param at all).
        // - `provision_tls_cert_blob` keys on the `(role, bridge_id, domain)`
        //   its blob header carries; a replay rewrites the same row.
        // - `register_service_user`'s two writes are set-once-frozen
        //   (`upsert_bridge_{x25519,mlkem_ek}`, identical value ⇒ confirm, a
        //   different one ⇒ refused), its confinement record is a diagnostic
        //   upsert gating nothing, and its reply is a pure function of actor +
        //   status. Its second ingress — `request_enrollment`, which reaches
        //   `upsert_bridge_x25519` via `bind_enrollment_x25519` — carries
        //   `false` too, so the shared-accumulator predicate is satisfied at
        //   both ingresses.
        //
        // The predicate that caught `moderation.train` was run over this whole
        // constant: every mutation helper here was grepped for further callers,
        // and no second WIRE ingress into any of them disagrees (the extras are
        // nest-internal — the DKIM rotation-mint task, ACME's
        // `store_acme_material` / `seal_current_tls_cert_for_bridge_impl`, and
        // the in-process web-serve holder's boot-time self-enrollment).
        //
        // The four `bridges.atproto.*` kinds also on this constant are NOT
        // covered here — they carry their own per-kind rationale and belong to
        // the in-flight PDS build.
        let provision = RpcKindMeta::new(false, Duration::from_secs(30));
        // Also DECIDED, not inherited (80th pass): the three non-atproto kinds
        // on this constant were read to their SQL and are idempotent **by
        // construction** — `revoke_dkim_blob`, `revoke_wrapped_mls_blob` and
        // `revoke_wrapped_submission_token` (`bridge_blob_handlers.rs`) are each
        // a `DELETE` keyed on a caller-supplied key whose rows-affected is
        // explicitly discarded, replying a **constant** `{ ok: true }` whether
        // or not a row was there. State AND reply converge on a replay, which
        // makes them the clean counter-example to the consume-shaped class the
        // 76th pass named (converge in state, diverge in reply — a misleading
        // answer): replying a constant is precisely what avoids it. The four
        // `bridges.atproto.*` kinds also on this constant are NOT covered here —
        // they carry their own per-kind rationale and belong to the in-flight
        // PDS build.
        let revoke = RpcKindMeta::new(false, Duration::from_secs(5));
        // Inbound/submitted mail can carry tens of MB encrypted bodies plus
        // a SQLite write of the same size; keep the deadline generous.
        //
        // `forbid_replay = false` here is DECIDED, not inherited (79th pass —
        // audited against `transport.md` § Idempotency and reconnect-with-
        // resume). All five kinds on this constant are content-addressed
        // writes that dedup, which is why a retry is safe:
        //
        // - `ingest_inbound_mail` / `submit_inbound_mail` / `append` derive
        //   their `message_id` as `blake3(domain_tag ‖ actor ‖ timestamp ‖
        //   body)` where `timestamp` is the caller's `public_metadata.timestamp`
        //   — no server clock, no nonce — so a replay lands the *same* id, the
        //   `segment_records` lookup hits, and every downstream effect is gated
        //   on the resulting `inserted` / placement-outcome flag: no second
        //   segment append, no second UID allocation, no duplicate placement
        //   journal record, no duplicate arrival push.
        // - `put_event_ciphertext` / `put_card_ciphertext` key on the caller's
        //   32-byte `uid_hash` and upsert, so a replay rewrites one row.
        //
        // One non-idempotent effect is known and ACCEPTED on the two inbound
        // kinds: the family-safety null-reverse-path probe
        // (`consume_sent_msgid_correlation`) decrements a correlation budget at
        // verdict time, *before* the dedup gate, so a replay spends a second
        // unit. Accepted on three grounds, the third decisive: it fails toward
        // *holding* a DSN for guardian review, never toward delivering one
        // (`bridge_routing_handlers.rs` states this at the spend site); the
        // path is SMTP, already at-least-once, so the remote MTA's own retry
        // does the same thing and forbidding the WS replay would not remove it;
        // and forbidding it would push the bridge toward not retrying a
        // delivery whose message half IS safely dedup-guarded — trading an
        // over-held bounce for actual mail loss. That is the wrong trade for a
        // mail path.
        let routing = RpcKindMeta::new(false, Duration::from_secs(60));

        self.add("fauna.bridges.fetch_wrapped_mls_blob", fetch);
        self.add("fauna.bridges.fetch_mls_snapshot_blob", fetch);
        self.add("fauna.bridges.fetch_webdav_keys_blob", fetch);
        self.add("fauna.bridges.fetch_wrapped_submission_token", fetch);
        self.add("fauna.bridges.fetch_tls_cert_blob", fetch);
        self.add("fauna.bridges.fetch_bridge_pubkey", fetch_short);
        self.add("fauna.bridges.provision_wrapped_mls_blob", provision);
        self.add("fauna.bridges.provision_mls_snapshot_blob", provision);
        self.add("fauna.bridges.provision_webdav_keys_blob", provision);
        self.add(
            "fauna.bridges.provision_wrapped_submission_token",
            provision,
        );
        self.add("fauna.bridges.provision_tls_cert_blob", provision);
        // Admin read/manage surface for the DNS page (public metadata; the
        // key itself never leaves the nest).
        self.add("fauna.bridges.list_dkim_selectors", fetch);
        self.add("fauna.bridges.revoke_dkim_blob", revoke);
        // Admin enumeration of mail service-user bridges (find/gate the
        // approved MTA to seal to; also feeds Track-3 pending approval).
        self.add("fauna.bridges.list_service_users", fetch);
        self.add("fauna.bridges.revoke_wrapped_mls_blob", revoke);
        self.add("fauna.bridges.revoke_wrapped_submission_token", revoke);
        self.add("fauna.bridges.register_service_user", provision);
        self.add("fauna.bridges.report_auth_event", fetch_short);

        // I2b routing/data plane (Phase A — additional kinds land in
        // later sessions as the mail-bridge I2b routing/data-plane work
        // continues; tracked internally).
        self.add("fauna.bridges.validate_recipient", fetch);
        self.add("fauna.bridges.fetch_recipient_mls_pubkey", fetch);
        self.add("fauna.bridges.fetch_config", fetch);
        self.add("fauna.bridges.report_session_close", fetch_short);
        // `forbid_replay = true` (79th pass) — the first value in this family
        // to be decided rather than inherited. Lifted out of `fetch_short`
        // because the flag now differs from it, per the rule that a flip
        // splits the constant rather than diverging silently inside it.
        //
        // The name reads like a query; the handler is a consuming debit. Full
        // rationale + hazard pin at the nest declaration site
        // (`bridge_routing_handlers.rs`, `register_bridge_handlers`).
        self.add(
            "fauna.bridges.check_submission_quota",
            RpcKindMeta::new(true, Duration::from_secs(2)),
        );
        self.add("fauna.bridges.ingest_inbound_mail", routing);
        self.add("fauna.bridges.submit_inbound_mail", routing);
        // I2b Phase-C IMAP metadata plane.
        self.add("fauna.bridges.list_mailboxes", fetch);
        self.add("fauna.bridges.select_mailbox", fetch);
        // C.2 — message-metadata fetch.
        self.add("fauna.bridges.list_messages", fetch);
        self.add("fauna.bridges.fetch_message_metadata", fetch);
        // C.3 — body-fetch (60 s: bodies can be tens of MB) + index-segments.
        self.add("fauna.bridges.fetch_message_ciphertext", routing);
        self.add("fauna.bridges.fetch_index_segments_since", fetch);
        // C.4 — flag-store + expunge. Both `false`, **decided** by the 82nd-pass
        // audit rather than inherited from `fetch`; full rationale at the nest
        // declaration site (`bridge_imap_handlers.rs`). In short: STORE's
        // Set/Add/Remove are idempotent set ops, but the handler had to stop
        // re-stamping already-satisfied rows before that was true of `modseq`
        // and of the reply (a replayed CONDSTORE used to be answered with a
        // false `MODIFIED` conflict); EXPUNGE resolves its target set from rows
        // still flagged `\Deleted`, so a replay writes nothing at all.
        self.add("fauna.bridges.store_flags", fetch);
        self.add("fauna.bridges.expunge", fetch);
        // C.5 — copy + move. The two share one mutation helper
        // (`copy_within_locked`) and still take **opposite** values, because
        // only one of them consumes the helper's own input — see the 82nd-pass
        // audit note at the nest declaration site (`bridge_imap_handlers.rs`).
        //
        // `copy` is `forbid_replay = true` (82nd pass) — lifted out of `fetch`
        // because the flag now differs from it, per the rule that a flip splits
        // the constant rather than diverging silently inside it. The
        // destination UID is **server-allocated** from the dest mailbox's
        // `uid_next`, and the placement row goes in via a bare `INSERT` keyed on
        // that fresh UID — so nothing in the request identifies the copy. A
        // replay therefore stores a *second* placement for every source UID: the
        // user sees duplicate mail in the destination mailbox, and the RFC 9208
        // quota is charged twice for it. This is a true double-apply, not the
        // 76th pass's misleading-answer class, so no lookup remedy applies. IMAP
        // COPY is non-idempotent by protocol construction (each COPY mints new
        // UIDs), which is why the honest declaration is the flip rather than a
        // server-side dedup table.
        self.add(
            "fauna.bridges.copy",
            RpcKindMeta::new(true, Duration::from_secs(5)),
        );
        // `move` keeps `forbid_replay = false`, now **decided** rather than
        // inherited: MOVE is copy-then-expunge under one lock scope, so its
        // first call consumes the very source rows `copy_within_locked` reads.
        // A replay finds them gone, copies nothing, and skips every source-side
        // touch — state converges with no duplicate. The reply does diverge
        // (`moved: []` where the first call listed the pairs), which is the
        // 76th pass's consume-shaped class and its ruling applies: a diverging
        // answer is not grounds for a flip.
        self.add("fauna.bridges.move", fetch);
        // C.6 — append.  60 s routing deadline (bodies can be tens of MB).
        self.add("fauna.bridges.append", routing);
        // I2b Phase-D CalDAV r/w + provisioning.
        self.add("fauna.bridges.provision_calendar", fetch);
        self.add("fauna.bridges.list_calendars", fetch);
        self.add("fauna.bridges.query_events", fetch);
        // D.4 — put_event_ciphertext.  60 s routing deadline (encrypted bodies can be tens of MB).
        self.add("fauna.bridges.put_event_ciphertext", routing);
        // D.5 — delete_event.  5 s fetch deadline (lookup + tombstone insert, no body upload).
        self.add("fauna.bridges.delete_event", fetch);
        // D.6 — sync_calendar_since.  5 s fetch deadline (metadata-only query; no body upload).
        self.add("fauna.bridges.sync_calendar_since", fetch);
        // The MTA places an emailed invitation on the recipient's calendar
        // (caldav-server.md § Server-side auto-schedule, "Inbound invite").
        // Routing deadline: it carries a sealed event body like a PUT does.
        self.add("fauna.bridges.place_inbound_invite", routing);
        // Phase-E CardDAV r/w + provisioning (structural twin of Phase-D CalDAV;
        // CardDAV design proposal, tracked internally).
        self.add("fauna.bridges.provision_addressbook", fetch);
        self.add("fauna.bridges.list_addressbooks", fetch);
        self.add("fauna.bridges.query_cards", fetch);
        // E.4 — put_card_ciphertext.  60 s routing deadline (encrypted bodies can be large).
        self.add("fauna.bridges.put_card_ciphertext", routing);
        // E.5 — delete_card.  5 s fetch deadline (lookup + tombstone insert, no body upload).
        self.add("fauna.bridges.delete_card", fetch);
        // E.6 — sync_addressbook_since.  5 s fetch deadline (metadata-only query; no body upload).
        self.add("fauna.bridges.sync_addressbook_since", fetch);
        // E.7 — delete_addressbook.  5 s fetch deadline (existence check + cascade delete, no body upload).
        self.add("fauna.bridges.delete_addressbook", fetch);
        // Mailbox-migration import surface (`mailbox-migration.md` § Per-
        // message flow) — User-class, caller-scoped. The two body-carrying
        // kinds get the 60 s routing deadline (batches up to 16 MiB); the
        // session-lifecycle kinds are light row reads/writes (5 s).
        //
        // `import_message{,_batch}` are `forbid_replay = true` (2026-08-01):
        // the session's progress counters are accumulators (a deduped replay
        // still recounts), and under `skip_dedup: true` there is no dedup at
        // all — a replay stores the same message twice. Per-kind flag → the
        // weakest branch decides. Interrupted imports resume via
        // `list_import_sessions` + the per-mailbox cursors, never via a blind
        // wire retry. Full rationale at the nest declaration site
        // (`bridge_import_handlers.rs`).
        // `forbid_replay = true` (81st pass) — lifted out of `fetch` because the
        // flag now differs from it, per the rule that a flip splits the constant
        // rather than diverging silently inside it. The session id is minted
        // server-side (`Uuid::new_v4`), so a replay cannot land on the first
        // call's id: it mints a second one, trips the `(actor,
        // source_descriptor)` lock its own first call took, and answers
        // `import_source_locked`. State converges (one session) but the caller
        // is left holding no id AND is told an import is running "on another
        // device" — a confident falsehood, not merely a diverging answer. The
        // 76th pass's lookup remedy is unavailable: `SourceLocked` is also the
        // genuine second-device case, and nothing distinguishes a replay from a
        // second device once the id is server-minted. Full rationale + hazard
        // pin at the nest declaration site (`bridge_import_handlers.rs`).
        self.add(
            "fauna.bridges.start_import_session",
            RpcKindMeta::new(true, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.import_message",
            RpcKindMeta::new(true, Duration::from_secs(60)),
        );
        self.add(
            "fauna.bridges.import_message_batch",
            RpcKindMeta::new(true, Duration::from_secs(60)),
        );
        self.add("fauna.bridges.list_import_sessions", fetch);
        self.add("fauna.bridges.pause_import_session", fetch);
        self.add("fauna.bridges.resume_import_session", fetch);
        self.add("fauna.bridges.cancel_import_session", fetch);
        self.add("fauna.bridges.finalize_import_session", fetch);
        self.add("fauna.bridges.fail_import_session", fetch);
        // Mailbox-export surface (`mail-export.md` § Wire shapes) — twelve
        // User-class, caller-scoped kinds, the read-out twin of the import
        // block above. The two body-carrying legs of the chunk relay take the
        // 60 s routing deadline (a page of sealed records down, a 16 MiB
        // sealed frame up); everything else is a light row read/write.
        //
        // `forbid_replay` splits three ways, each read against the criterion
        // in `transport.md` § Idempotency and reconnect-with-resume (the flag
        // asserts the HANDLER is naturally idempotent — the idempotency cache
        // is per-`RpcConnection`, so it never dedups an auto-retry on a fresh
        // one):
        //
        // - `start_export_session` = true. The id is minted server-side, and
        //   unlike its import twin there is no per-source lock to trip: a
        //   replay simply opens a SECOND session. That burns one of the three
        //   concurrency slots § Quota composition grants, leaves an orphan row
        //   resting for 30 days, and hands the caller one of two ids with no
        //   way to tell which. The honest `RpcDisconnected { was_in_flight }`
        //   is what sends the client to `list_export_sessions`, which is the
        //   documented resume path anyway.
        // - `upload_export_chunk` = true. It is an accumulator (the counters)
        //   AND an append (the frame), so a replay would double-count and
        //   double-append. The `chunk_idx` guard makes the second attempt a
        //   typed `export_chunk_out_of_order` carrying the expected index —
        //   recoverable, but an answer the caller should reach deliberately.
        // - `restart_export_session` = true. Each application opens a NEW
        //   stream generation (`mail-export.md` § Resume), so a replay would
        //   supersede the very stream the first application just opened — and
        //   whose generation the caller never learned, its reply being the
        //   thing that was lost. The honest disconnect sends the client back
        //   to the cold view, where Resume restarts once more, knowingly.
        // - everything else = false. The five transitions are one conditional
        //   `UPDATE … WHERE state IN (allowed_from)` with a target state never
        //   in its own allowed_from; `discard_export_blob` answers
        //   `existed: false` the second time; the three reads are reads.
        //   `fail_export_session` unlinks the blob beside its transition, and
        //   the unlink is idempotent, so a replay is still the same answer.
        self.add(
            "fauna.bridges.start_export_session",
            RpcKindMeta::new(true, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.upload_export_chunk",
            RpcKindMeta::new(true, Duration::from_secs(60)),
        );
        self.add(
            "fauna.bridges.restart_export_session",
            RpcKindMeta::new(true, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.fetch_export_chunk_ciphertext",
            RpcKindMeta::new(false, Duration::from_secs(60)),
        );
        self.add("fauna.bridges.list_own_mailboxes", fetch);
        self.add("fauna.bridges.list_export_sessions", fetch);
        self.add("fauna.bridges.pause_export_session", fetch);
        self.add("fauna.bridges.resume_export_session", fetch);
        self.add("fauna.bridges.cancel_export_session", fetch);
        self.add("fauna.bridges.finalize_export_session", fetch);
        self.add("fauna.bridges.fail_export_session", fetch);
        self.add("fauna.bridges.discard_export_blob", fetch);
        // ATProto PDS F1 auth core (`atproto-pds-full.md` § WS-RPC kind
        // surface). Bridge-class session registry + verifier fetch; the
        // verifier fetch stays fetch-weight (light row read — Argon2id runs
        // bridge-side, never here). User-class mint/list/revoke +
        // kill-switch. Wire types: `atproto_pds.rs`.
        //
        // ── The `fauna.bridges.atproto.*` fetch pocket: DECIDED 2026-08-02,
        // no longer inherited ──
        //
        // Nine kinds sat on the shared `fetch` constant at
        // `forbid_replay = false` having never been read against the criterion
        // (`transport.md` § Idempotency and reconnect-with-resume: the flag
        // asserts the HANDLER is naturally idempotent — the idempotency cache
        // is per-`RpcConnection` and `request_auto_retry` re-issues on a fresh
        // one, so it never dedups a retry). All nine keep `false`; eight of
        // them trivially, one only after a handler fix.
        //
        // Trivially idempotent — pure single-`SELECT` reads with no write on
        // any path, verified at the DB method rather than inferred from the
        // verb: `fetch_app_credential_verifiers`, `list_app_credentials`,
        // `list_grants`, `list_sessions`, `list_pending_consents`,
        // `get_integration_status`, `fetch_authoring_delegation` (and
        // `fetch_consent`, since retired with the bridge's own authorization
        // server). Two were read closely because the name
        // invites the read-shaped-verb-hiding-a-write trap that caught
        // `check_submission_quota` and `train`: `get_atproto_consent_request`
        // does not consume the request it returns, and
        // `get_atproto_authoring_key` does not lazily mint (the insert,
        // `atproto_pds::insert_authoring_key_if_absent`, is a separate call,
        // made only by the mint path).
        //
        // ⚠️ `refresh_session` was the exception, and it was NOT idempotent —
        // it rotates a refresh-token jti. A replay presented a jti that the
        // first call had already rotated away, which read as token reuse and
        // **family-killed the session**: a dropped connection mid-refresh
        // logged the user out of a connected app and wrote a false theft
        // signal. Per the 82nd pass's fix-vs-flip rule it is non-idempotent by
        // OMISSION, not by construction — the request carries a reachable
        // dedup key (`new_jti`, which the row stores) — and the kind must stay
        // retryable, since refusing a refresh retry is itself the logout. So
        // the handler was fixed rather than the flag flipped:
        // `refresh_atproto_session` now recognizes its own already-applied
        // rotation and returns `Rotated` without writing (reasoning at that
        // call site). A stolen token still family-kills, because a thief's
        // call carries a freshly-minted `new_jti`.
        self.add(
            "fauna.bridges.atproto.fetch_app_credential_verifiers",
            fetch,
        );
        self.add("fauna.bridges.atproto.record_session", provision);
        self.add("fauna.bridges.atproto.refresh_session", fetch);
        self.add("fauna.bridges.atproto.end_session", revoke);
        self.add("fauna.bridges.atproto.provision_app_credential", provision);
        self.add("fauna.bridges.atproto.list_app_credentials", fetch);
        self.add("fauna.bridges.atproto.revoke_app_credential", revoke);
        self.add("fauna.bridges.atproto.list_sessions", fetch);
        // F4 slice 8a — the connected-apps registry read. A light row read, so
        // fetch weight, exactly like its session sibling. No `revoke_grant`
        // joins it: `grant_id` is the session-family id, so `revoke_session`
        // already revokes a grant.
        self.add("fauna.bridges.atproto.list_grants", fetch);
        self.add("fauna.bridges.atproto.revoke_session", revoke);
        self.add("fauna.bridges.atproto.set_external_apps_enabled", provision);
        // F4 consent ceremony (D3 rung 2), the user's half. The list is a
        // light read; the resolve grants, and is provision-weight for the same
        // reason the other grant-shaped kinds are. `forbid_replay` stays false:
        // a repeated resolve matches nothing and answers `resolved: false` — an
        // approval cannot be replayed into a second grant.
        self.add("fauna.bridges.atproto.list_pending_consents", fetch);
        self.add("fauna.bridges.atproto.resolve_consent", provision);
        // The bridge→nest half of the permission-set request call: releases
        // an in-memory waiter (no DB touch), fetch weight; a repeat matches
        // nothing and answers `accepted: false`, so replay is harmless.
        self.add("fauna.bridges.atproto.deliver_permission_set", fetch);
        // The integration-depth ladder (`ui/atproto.md` § Transition
        // semantics): the status read is a light row read; the transition can
        // boot the bridge and round-trip the consume-side unlink, so it gets
        // provision weight.
        self.add("fauna.bridges.atproto.get_integration_status", fetch);
        self.add("fauna.bridges.atproto.set_integration_level", provision);
        // D10 delegated authoring (F2.2 slices 2b/5), User-class self-scoped.
        // `fetch` mints K on its first call and `provision` verifies an
        // identity-signed cert, so both are provision-weight; the revoke is a
        // row delete. All replay-safe: mint is first-write-wins, provisioning
        // is idempotent for the same cert, and a repeated revoke is a no-op
        // success.
        self.add("fauna.bridges.atproto.fetch_authoring_key", provision);
        // The status read the AT Protocol page renders. Genuinely `fetch`-weight,
        // unlike `fetch_authoring_key` above: it mints nothing, which is the
        // reason it exists as its own kind rather than reusing that one.
        self.add("fauna.bridges.atproto.fetch_authoring_delegation", fetch);
        self.add(
            "fauna.bridges.atproto.provision_authoring_delegation",
            provision,
        );
        self.add("fauna.bridges.atproto.revoke_authoring_delegation", revoke);
        // F2 — the external write path. Bridge-class. Provision weight: one
        // call carries a whole `applyWrites` batch and does the Fauna-side
        // work for every row. `forbid_replay` stays false because the batch
        // is idempotent by construction — a journal write is keyed
        // `(actor, collection, rkey)` and re-applying the same rows lands the
        // same state.
        self.add("fauna.bridges.atproto.ingest_external_write", provision);
    }

    /// Register the `fauna.capabilities.*` kinds — the user-minted, scope-
    /// limited, revocable capability-grant plane (design § Phase 2 Step 2 § 2.3),
    /// the client-facing twin of the nest's `register_capability_handlers`.
    /// Deadlines mirror the nest router: `mint`/`renew` are provision-weight
    /// (30 s — a client-built blob upload / read-modify-write), `fetch` is a
    /// light holder read and `revoke` a row delete (5 s each). Owner-minted
    /// (mint/renew/revoke) vs. holder-fetched, gated nest-side in
    /// `bridge_method_allowlist::is_permitted`.
    /// Nest-side segment-backup grant plane (`backup.rs`; nest-side segment
    /// backup slice 2, design tracked internally). A user
    /// grants/revokes its own `NestBackupKey` to its source nest and reads the
    /// uniform Backups-page status projection. `grant` is provision-weight (30 s
    /// — a key write); `revoke` a row delete and `status` a light read (5 s
    /// each). USER-class, gated nest-side in
    /// `bridge_method_allowlist::is_permitted`.
    pub fn register_backup_kinds(&mut self) {
        use std::time::Duration;

        let provision = RpcKindMeta::new(false, Duration::from_secs(30));
        let light = RpcKindMeta::new(false, Duration::from_secs(5));

        self.add("fauna.backup.nest_key.grant", provision);
        self.add("fauna.backup.nest_key.revoke", light);
        self.add("fauna.backup.status", light);

        // Lifted from the nest router (see `register_all_kinds`).
        self.add(
            "fauna.backup.custody.list",
            RpcKindMeta::new(false, Duration::from_secs(15)),
        );
        // Reconstituting a set is I/O-bounded by the corpus, not by a page:
        // the deadline is the generous one a bounded recovery ceremony needs,
        // not the read kinds' 15s.
        self.add(
            "fauna.backup.custody.materialize",
            RpcKindMeta::new(false, Duration::from_secs(300)),
        );
        // The lived-in recovery reads and appends a delivered corpus as
        // materialize does, so it takes the same recovery-ceremony deadline.
        self.add(
            "fauna.backup.custody.recover",
            RpcKindMeta::new(false, Duration::from_secs(300)),
        );
        self.add(
            "fauna.backup.custodian.checkin",
            RpcKindMeta::new(false, Duration::from_secs(10)),
        );
        self.add(
            "fauna.backup.destination.list",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.backup.destination.register",
            RpcKindMeta::new(false, Duration::from_secs(10)),
        );
        self.add(
            "fauna.backup.destination.remove",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.backup.destination.attach_folder",
            RpcKindMeta::new(false, Duration::from_secs(10)),
        );
        self.add(
            "fauna.backup.destination.detach_folder",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.backup.generation.list",
            RpcKindMeta::new(false, Duration::from_secs(15)),
        );
        self.add(
            "fauna.backup.generation.restore",
            RpcKindMeta::new(false, Duration::from_secs(15)),
        );
        self.add(
            "fauna.backup.writer_grant.list",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.backup.writer_grant.register",
            RpcKindMeta::new(false, Duration::from_secs(10)),
        );
        self.add(
            "fauna.backup.writer_grant.revoke",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
    }

    /// RecoveryKey registration-chain kinds (`recovery.rs`; identity-succession
    /// slice 2, `docs/goal/behavior/identity-succession.md` § The RecoveryKey).
    ///
    /// `submit` is USER-class and `chain` is pre-identity, but both are
    /// replay-safe: `submit` carries an explicit `seq` that the store refuses as
    /// non-advancing on a re-delivery, so a retry cannot fork a chain, and
    /// `chain` is a pure read. 10 s for the submit (a signature verify plus one
    /// row write), 5 s for the read.
    ///
    /// ⚠️ **Caveat, true of every `register_*_kinds` method on this type as of
    /// 2026-07-24, not just this one:** none of them is called in production —
    /// `default_with_protocol_kinds` registers only `fauna.protocol.echo`, and
    /// all 50 call sites live in this file's `mod tests`. Every kind therefore
    /// falls back to the spec defaults in
    /// `fauna_client::client::{request_inner, request_auto_retry}`. This method
    /// exists so the recovery family is uniform with its 52 siblings and so the
    /// intended metadata is recorded at the point of authorship; wiring the
    /// registry up is tracked internally as its own track, because flipping
    /// ~every kind's deadline and replay semantics at once is a behavior change
    /// that needs its own red-first pass, not a drive-by.
    pub fn register_recovery_kinds(&mut self) {
        use std::time::Duration;

        self.add(
            "fauna.recovery.registration.submit",
            RpcKindMeta::new(false, Duration::from_secs(10)),
        );
        self.add(
            "fauna.recovery.registration.chain",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.recovery.escrow.put",
            RpcKindMeta::new(false, Duration::from_secs(10)),
        );
        self.add(
            "fauna.recovery.escrow.challenge",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        // Replay-forbidding: its nonce is single-use, so a blind auto-retry
        // over a reconnect can only fail. The client must request a fresh
        // challenge instead.
        self.add(
            "fauna.recovery.escrow.fetch",
            RpcKindMeta::new(true, Duration::from_secs(10)),
        );
        // The presence read — USER class, unlike the rest of the escrow family.
        // A pure read, so replay-safe.
        self.add(
            "fauna.recovery.escrow.status",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        // The seed-initiated replacement window. `request` is replay-safe by
        // construction: a re-delivered identical record upserts the same
        // pending row and the store keeps the ORIGINAL `requested_at` for an
        // unchanged record digest, so a retry cannot extend the window.
        self.add(
            "fauna.recovery.replacement.request",
            RpcKindMeta::new(false, Duration::from_secs(10)),
        );
        self.add(
            "fauna.recovery.replacement.challenge",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        // Same single-use-nonce reasoning as `escrow.fetch`.
        self.add(
            "fauna.recovery.replacement.veto",
            RpcKindMeta::new(true, Duration::from_secs(10)),
        );
        self.add(
            "fauna.recovery.replacement.status",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        // Succession (slice 3). `submit` is replay-safe by *outcome* rather
        // than by idempotence: a second delivery of the same statement finds
        // the old identity already recorded in `actor_successions` and gets a
        // typed `already_succeeded` refusal, so it can never apply twice. That
        // is a weaker property than `registration.submit`'s — the retry
        // surfaces a refusal rather than the original success — but a blind
        // auto-retry is still safe, which is what this flag governs.
        self.add(
            "fauna.recovery.succession.submit",
            RpcKindMeta::new(false, Duration::from_secs(10)),
        );
        self.add(
            "fauna.recovery.succession.lookup",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        // `status` is the authenticated, self-scoped read of the caller's own
        // commit stamp — a pure read, and the only USER-class kind in this
        // family's succession half (`crate::recovery::SUCCESSION_STATUS_KIND`
        // owns the name and the reason it is not served on `lookup`).
        self.add(
            crate::recovery::SUCCESSION_STATUS_KIND,
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        // `owed_settle` clears one of the caller's owed nests. Replay-safe by
        // idempotence: settling an entry that is already gone is a success.
        self.add(
            crate::recovery::SUCCESSION_OWED_SETTLE_KIND,
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
    }

    /// The generation **escrow doors** — R14 (account-data-plane.md § The ratified decisions) build step 4, the nest as v1
    /// escrow holder (`account-data-plane.md` § The generation machinery →
    /// *The escrow doors*; handlers: `bins/fauna-nest/src/
    /// generation_escrow_handlers.rs`). All three are replay-safe by
    /// construction: `put` is idempotent per `(generation_id, wrap_hash)` and
    /// returns a byte-identical receipt on a retry; `get`/`delete` are a read
    /// and an idempotent removal.
    pub fn register_generation_escrow_kinds(&mut self) {
        use std::time::Duration;

        let meta = RpcKindMeta::new(false, Duration::from_secs(5));
        self.add(crate::generation_escrow::KIND_ESCROW_PUT, meta);
        self.add(crate::generation_escrow::KIND_ESCROW_GET, meta);
        self.add(crate::generation_escrow::KIND_ESCROW_DELETE, meta);
    }

    /// The custodian-nest runtime's hosting pair (`account-data-plane.md`
    /// § Replica posture → The custody grant + ceremony, item 6 stage (b)).
    /// Replay-safe by construction, so neither forbids replay: `register` is
    /// an LWW upsert keyed `(caller, grant_id)` where `grant_id` comes from
    /// the caller's own ceremony record (a replay rewrites one row with the
    /// same values), and `list` is a pure read.
    pub fn register_custody_kinds(&mut self) {
        use std::time::Duration;

        self.add(
            crate::custody::HOSTING_REGISTER_KIND,
            RpcKindMeta::new(false, Duration::from_secs(10)),
        );
        self.add(
            crate::custody::HOSTING_LIST_KIND,
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        // The reclaim: replay-safe by construction — removing an
        // already-removed row answers `removed: false`, an honest no-op.
        self.add(
            crate::custody::HOSTING_REMOVE_KIND,
            RpcKindMeta::new(false, Duration::from_secs(10)),
        );
        // Stage (c) — the receipt deposit arm. `deposit` is replay-safe by
        // construction: staging is latest-per-grant and monotone in the
        // receipt's own `attested_at`, so a replay is a not-newer no-op with
        // a constant-shaped reply. `list` is a pure owner-scoped read.
        self.add(
            crate::custody::RECEIPT_DEPOSIT_KIND,
            RpcKindMeta::new(false, Duration::from_secs(10)),
        );
        self.add(
            crate::custody::RECEIPT_LIST_KIND,
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        // The admin hosting surface (list+remove door). Both are
        // replay-safe: `list` is a pure read, and `remove` is idempotent by
        // construction — an absent `(host, grant_id)` returns
        // `removed: false, store_dropped: false` and touches nothing, and the
        // store teardown fires only with the pair's LAST row.
        self.add(
            crate::custody::ADMIN_HOSTING_LIST_KIND,
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            crate::custody::ADMIN_HOSTING_REMOVE_KIND,
            RpcKindMeta::new(false, Duration::from_secs(10)),
        );
    }

    pub fn register_capability_kinds(&mut self) {
        use std::time::Duration;

        let fetch = RpcKindMeta::new(false, Duration::from_secs(5));
        // DECIDED, not inherited (81st pass), for both kinds on it. `mint` and
        // `renew` share one storage primitive — `put_capability_grant`, an
        // `INSERT OR REPLACE` on `(owner, grant_id)` — and `grant_id` comes from
        // the caller's own blob, never the nest, so a replay rewrites one row.
        // The per-owner quota beside it is the accumulator to check, and it is
        // explicitly replace-aware (`is_replace` skips the `COUNT(*)` gate), so
        // a replayed mint is not charged a second slot. `renew` assigns
        // `window.1` absolutely rather than by delta and dedups appended keys by
        // `(scope, epoch)`; its monotonicity guard admits an equal `epoch_end`,
        // so the replay passes it as a no-op instead of erroring. Both reply a
        // constant, and their two extra legs — the web-serve holder refresh and
        // the delegation-runner wake — are nudges. `put_capability_grant` has no
        // other wire ingress (its remaining callers are tests).
        let provision = RpcKindMeta::new(false, Duration::from_secs(30));
        // DECIDED, not inherited (80th pass), for its one kind
        // `fauna.capabilities.revoke`: idempotent by construction — a
        // caller-keyed `delete_capability_grant` whose rows-affected is
        // discarded, replying a constant `{ ok: true }` (its own handler doc
        // says so). Its three extra legs are idempotent too: the paired
        // spam-model holder-copy delete is skipped on a replay because the grant
        // snapshot is already gone — the copy went with the first call — and the
        // web-serve refresh + delegation-runner wake are nudges.
        let revoke = RpcKindMeta::new(false, Duration::from_secs(5));

        self.add("fauna.capabilities.mint", provision);
        self.add("fauna.capabilities.fetch", fetch);
        self.add("fauna.capabilities.renew", provision);
        self.add("fauna.capabilities.revoke", revoke);
        // The reconcile sweep (`ui/nests.md` § Trust facet — grants →
        // *Reconcile*, ratified 2026-08-15): an owner-scoped ids-only read,
        // same light weight as `fetch`. Idempotent by construction (a pure
        // read), so replay-safe.
        self.add("fauna.capabilities.reconcile", fetch);
        // The re-score drain plane (design § 2.5 step 4): a holder reads its
        // obligation worklist (light read, 5 s) and writes back re-computed
        // scores (provision-weight batch write, 30 s). Holder-gated nest-side
        // (`BridgeMda | ContentProcessor`).
        self.add(
            "fauna.capabilities.rescore_worklist",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.capabilities.submit_scores",
            RpcKindMeta::new(false, Duration::from_secs(30)),
        );
        // The spam-baseline publish drain (`mail-spam.md` § Encrypted-mode
        // interaction, ratified 2026-07-13) — the third drain-plane instance:
        // the poked holder reads the pending run's grant-gated sealed-copy
        // worklist (copies can approach the model cap, so provision-weight
        // 30 s) and writes back its merged half (30 s). Holder-gated nest-side
        // (`BridgeMda | ContentProcessor`), valid only inside a publish window.
        self.add(
            "fauna.capabilities.spam_baseline_worklist",
            RpcKindMeta::new(false, Duration::from_secs(30)),
        );
        self.add(
            "fauna.capabilities.submit_spam_baseline",
            RpcKindMeta::new(false, Duration::from_secs(30)),
        );
    }

    /// Register the `fauna.labelers.*` kinds — the community-labeler
    /// publish/subscribe registry (labeler-registry design § 1), the
    /// client-facing twin of the nest's `register_labeler_handlers`.
    /// `publish`/`inspect` carry the WASM module bytes (up to 1 MiB), so they
    /// get the 30 s provision-weight deadline; `list`/`subscribe`/`unsubscribe`
    /// are light reads / row writes (5 s). All replay-safe: publish is an
    /// idempotent monotonic upsert, subscribe/unsubscribe are idempotent
    /// `INSERT OR REPLACE` / `DELETE`. All gated to class `User` nest-side
    /// (`bridge_method_allowlist::is_permitted`).
    pub fn register_labeler_kinds(&mut self) {
        use std::time::Duration;

        let provision = RpcKindMeta::new(false, Duration::from_secs(30));
        let read = RpcKindMeta::new(false, Duration::from_secs(5));

        self.add("fauna.labelers.publish", provision);
        self.add("fauna.labelers.inspect", provision);
        self.add("fauna.labelers.list", read);
        self.add("fauna.labelers.subscribe", read);
        self.add("fauna.labelers.unsubscribe", read);
    }

    /// Register the Layer-3 Bridge Management user-facing kinds (the
    /// surface end-user clients call from the bridges page). Distinct
    /// from `register_bridge_kinds` above, which is the daemon-internal
    /// wrapped-blob plane. Both share the `fauna.bridges.*` namespace
    /// and are caller-class-gated at dispatch.
    ///
    /// Per-kind replay semantics + rationale: see the per-kind comments
    /// below (tracked internally). Slice grows as per-endpoint tasks land.
    pub fn register_bridges_ui_kinds(&mut self) {
        use std::time::Duration;

        let read = RpcKindMeta::new(false, Duration::from_secs(5));

        self.add("fauna.bridges.list", read);
        // T2 — idempotent PUT-overwrite of the per-bridge settings object;
        // replay-safe (same payload twice yields the same state).
        self.add("fauna.bridges.set_settings", read);
        // T2 — pure read of the per-bridge follow list.
        self.add("fauna.bridges.list_follows", read);
        // T3 — OAuth-flow start. `forbid_replay=true` because a
        // double-invocation on disconnect-and-retry can corrupt the
        // in-progress flow state (e.g. invalidate the pending nonce on
        // the provider side); 30 s default deadline matches the
        // upstream OAuth round-trip envelope.
        self.add(
            "fauna.bridges.link",
            RpcKindMeta::new(true, Duration::from_secs(30)),
        );
        // Proof-of-possession challenge ahead of an external-signer `link`
        // (Nostr `nip07`). Replay-safe: a re-issue mints a fresh nonce that
        // supersedes the last, and the app signs whichever reply it holds.
        // Local mint, no upstream — the 5 s read envelope.
        self.add("fauna.bridges.link_challenge", read);
        // T3 — idempotent (already-unlinked is a no-op); replay-safe.
        self.add("fauna.bridges.unlink", read);
        // T4 — add-follow. `forbid_replay=true` because the
        // duplicate-follow constraint is server-enforced and the
        // auto-retry path would surface as a spurious conflict error;
        // safer to make the caller re-decide. 5 s default deadline
        // matches the existing follow-list read.
        self.add(
            "fauna.bridges.add_follow",
            RpcKindMeta::new(true, Duration::from_secs(5)),
        );
        // T4 — remove-follow is idempotent (already-removed is a no-op);
        // replay-safe.
        self.add("fauna.bridges.remove_follow", read);
        // Follow requests (`bridges.md` § Follow requests): the list is a
        // pure read; the answer is idempotent — a request already gone
        // (withdrawn, or answered from another device) succeeds — so both
        // are replay-safe.
        self.add("fauna.bridges.list_follow_requests", read);
        self.add("fauna.bridges.resolve_follow_request", read);
        // T5 — cross-bridge feed-subscription surface
        // (fauna.bridges.feeds.*). All three are replay-safe at 5 s:
        //   - feeds.list is a pure read.
        //   - feeds.create is idempotent — the DB row carries
        //     UNIQUE(actor_id, bridge, feed_uri) and the route uses
        //     INSERT OR IGNORE, so duplicate-create returns the same
        //     server-assigned id rather than a constraint failure.
        //     Matches the email.filters.create precedent.
        //   - feeds.delete is idempotent (already-deleted maps to
        //     not_found regardless of whether the row ever existed).
        self.add("fauna.bridges.feeds.list", read);
        self.add("fauna.bridges.feeds.create", read);
        self.add("fauna.bridges.feeds.delete", read);
    }

    /// Register the user-facing `fauna.email.*` kinds — the surface
    /// end-user clients call from the per-account mail UI. Covers the
    /// CRUD filter-rule kinds (`fauna.email.filters.{list,create,get,
    /// update,delete}`) plus outbound submission (`fauna.email.send`).
    /// Separate top namespace from `fauna.bridges.*` — the
    /// `/api/v1/email/*` routes are conceptually mail-server surface,
    /// not bridge-management surface, and the goal-doc home is
    /// `smtp-server.md` + `mail-content-scanning.md`, not `bridges.md`.
    ///
    /// Filter kinds are all replay-safe at 5 s:
    ///   - `filters.list` / `filters.get` are pure reads.
    ///   - `filters.create` is replay-safe because the idempotency cache
    ///     replays the prior reply on retry, returning the same
    ///     server-assigned id (the DB row carries no UNIQUE constraint
    ///     on `(owner, name)` — successive creates with identical bodies
    ///     produce distinct ids on the first try, but the cache pins the
    ///     first reply to subsequent attempts of the same request).
    ///   - `filters.update` is an idempotent overwrite (UPDATE … WHERE
    ///     id=? AND owner=?). Replay-safe.
    ///   - `filters.delete` is idempotent (already-deleted maps to
    ///     `fauna.email.not_found` regardless of whether the row ever
    ///     existed; the DB DELETE filters by both id and owner so the
    ///     unknown-id and wrong-actor paths take the same shape).
    ///
    /// `fauna.email.send` is `forbid_replay=true` at 30 s — replaying
    /// outbound delivery on a recovered connection could double-send
    /// to remote MX; the caller must explicitly re-issue. 30 s deadline
    /// envelopes the per-recipient routing + outbound-queue insert
    /// (mirrors the `fauna.bridges.link` shape's per-kind replay semantics).
    pub fn register_email_kinds(&mut self) {
        use std::time::Duration;
        let read = RpcKindMeta::new(false, Duration::from_secs(5));
        self.add("fauna.email.filters.list", read);
        self.add("fauna.email.filters.create", read);
        self.add("fauna.email.filters.get", read);
        self.add("fauna.email.filters.update", read);
        self.add("fauna.email.filters.delete", read);
        self.add(
            "fauna.email.send",
            RpcKindMeta::new(true, Duration::from_secs(30)),
        );
        // `fauna.email.inbox.fetch` — the inbound client mail-receive
        // feed (caller-scoped INBOX read). `forbid_replay=false`
        // (idempotent read); 60 s deadline like
        // `fauna.bridges.fetch_message_ciphertext` — mail bodies can be
        // large, so a larger page can take longer than the 5 s
        // filter-CRUD reads.
        self.add(
            "fauna.email.inbox.fetch",
            RpcKindMeta::new(false, Duration::from_secs(60)),
        );
        // `fauna.email.apply_spam_disposition` — the on-device scorer's
        // outcome (watermark + INBOX→Junk) for the caller's own INBOX.
        // `forbid_replay=false` (idempotent on replay — set-union watermark,
        // and a re-move finds the UIDs already gone from INBOX); 30 s write
        // deadline. `mail-spam.md` § Wire shapes.
        self.add(
            "fauna.email.apply_spam_disposition",
            RpcKindMeta::new(false, Duration::from_secs(30)),
        );
        // `fauna.email.inbox.mark_seen` — add `\Seen` to the caller's own
        // INBOX UIDs. `forbid_replay=false` (idempotent: a re-add is a no-op
        // that bumps no modseq); 30 s write deadline like the spam
        // disposition. `fauna.email.inbox.flag_changes` — the INBOX flag
        // delta since a modseq cursor; a small pure read, 5 s.
        // `mail-app-surface.md` § Read state.
        self.add(
            "fauna.email.inbox.mark_seen",
            RpcKindMeta::new(false, Duration::from_secs(30)),
        );
        self.add("fauna.email.inbox.flag_changes", read);

        // Lifted from the nest router (see `register_all_kinds`).
        self.add(
            "fauna.email.sent.fetch",
            RpcKindMeta::new(false, Duration::from_secs(60)),
        );
    }

    /// Register the fauna-native inbox kinds (`fauna.inbox.{fetch,ack,send}`)
    /// — the caller-scoped drain of, and the outbound send into, the
    /// per-actor store-and-forward queue (contact-requests, knocks, MLS
    /// Welcomes, security notices, cross-nest DMs).
    /// See `inbox.rs` for the wire types.
    ///
    /// - `fetch` is `forbid_replay=false` at 5 s — an idempotent peek of
    ///   undelivered items (no status change), payloads are small
    ///   fauna-native control frames, so a far shorter deadline than the
    ///   mail `inbox.fetch` (which carries large bodies).
    /// - `ack` is `forbid_replay=false` at 5 s — marking ids delivered is
    ///   naturally idempotent (already-delivered ids are no-ops), so a
    ///   replay is safe; a quick SQLite write.
    /// - `send` is `forbid_replay=true` at 30 s — the client→home-nest
    ///   bearer leg of social inbox delivery; a cross-nest recipient makes
    ///   the home nest originate `fauna.federation.inbox.deliver` (a
    ///   federation dial, so the longer deadline). It was `false` until the
    ///   71st pass on the ground that "the federation leg is idempotent
    ///   (idempotency cache); the local leg matches the retiring HTTP twin's
    ///   at-least-once semantics". **Both halves of that ground fail.** The
    ///   cache is per-`RpcConnection` and `request_auto_retry` waits for the
    ///   reconnect, so it cannot deduplicate a retry; and the local leg's
    ///   `content_id` mixes a timestamp *and* a monotonic nonce
    ///   (`db/inbox.rs::inbox_content_id`), making the row key unique by
    ///   construction — a replay inserts a second delivery and charges the
    ///   recipient's `inbox_bytes_used` twice. "At-least-once" is a
    ///   description of non-idempotence, which § Idempotency and
    ///   reconnect-with-resume requires be `true`.
    pub fn register_inbox_kinds(&mut self) {
        use std::time::Duration;
        let quick = RpcKindMeta::new(false, Duration::from_secs(5));
        self.add("fauna.inbox.fetch", quick);
        self.add("fauna.inbox.ack", quick);
        self.add(
            "fauna.inbox.send",
            RpcKindMeta::new(true, Duration::from_secs(30)),
        );
    }

    /// Register the Bluesky-native thread-view kind (`bluesky.feed.thread`) —
    /// the one genuinely protocol-unique consume-side Bluesky surface that
    /// keeps a `bluesky.*` kind (`bridges.md` § Bluesky-native thread view).
    /// See `bluesky.rs` for the wire types; nest handler is feature-gated in
    /// `bins/fauna-nest/src/bluesky/bluesky_handlers.rs` (only registered under
    /// `--features bluesky`).
    ///
    /// `forbid_replay=false` — an idempotent read (re-fetching a thread has no
    /// side effects). 30 s deadline: unlike the 5 s local-DB reads, the handler
    /// makes a live `app.bsky.feed.getPostThread` XRPC round-trip to Bluesky,
    /// so it gets the generous envelope of the external-call kinds
    /// (`fauna.bridges.link` shape) rather than the local-read 5 s.
    pub fn register_bluesky_kinds(&mut self) {
        use std::time::Duration;
        self.add(
            "bluesky.feed.thread",
            RpcKindMeta::new(false, Duration::from_secs(30)),
        );
    }

    /// Register the user-facing `fauna.nostr.bunker.*` kinds — the NIP-46
    /// bunker control plane (`docs/goal/ui/nostr.md` § The nest as the user's
    /// NIP-46 signer): a nest-enforced roster with mint/revoke verbs,
    /// consumed by the Nostr page's *Connected apps* section. All
    /// caller-scoped, gated User-class nest-side.
    ///
    /// Deadlines mirror the capability-roster twin
    /// (`register_capability_kinds`): `create_invite` is provision-weight
    /// (30 s — mints the signer keypair on first use + a secret), the rest
    /// are light row reads/writes (5 s). All replay-safe, like the
    /// capabilities mint/revoke precedent (a replayed `create_invite` mints
    /// a redundant pending invite that lapses on its TTL; a replayed
    /// `revoke`/`set_label` is idempotent).
    ///
    /// `bind` is the one principal-side kind (TP11 — a third-party principal
    /// binding its NIP-46 client key under a live `identity.op` grant): light
    /// and replay-safe, since a re-bind re-points the principal's one row.
    pub fn register_nostr_bunker_kinds(&mut self) {
        use std::time::Duration;
        let light = RpcKindMeta::new(false, Duration::from_secs(5));
        self.add(
            "fauna.nostr.bunker.create_invite",
            RpcKindMeta::new(false, Duration::from_secs(30)),
        );
        self.add("fauna.nostr.bunker.list", light);
        self.add("fauna.nostr.bunker.revoke", light);
        self.add("fauna.nostr.bunker.set_label", light);
        self.add("fauna.nostr.bunker.bind", light);
    }

    /// Register the bridged-conversation family (`apps/bridges.md`
    /// § Bridge-kind catalogue → Phase G) — the nest's
    /// `register_bridged_conversation_handlers` twin: all light; only the
    /// user's `send`, which mints a row per call, forbids replay.
    pub fn register_bridged_conversation_kinds(&mut self) {
        use crate::bridged_conversations as bc;
        use std::time::Duration;
        let light = RpcKindMeta::new(false, Duration::from_secs(5));
        for kind in [
            bc::KIND_DEPOSIT,
            bc::KIND_OUTBOX_FETCH,
            bc::KIND_OUTBOX_ACK,
            bc::KIND_ROOM_UPSERT,
            bc::KIND_ROOM_MEMBERS,
            bc::KIND_RECEIPT,
            bc::KIND_ROOMS_LIST,
            bc::KIND_ROOMS_OPEN,
            bc::KIND_INBOX_FETCH,
        ] {
            self.add(kind, light);
        }
        self.add(
            bc::KIND_SEND,
            RpcKindMeta::new(true, Duration::from_secs(5)),
        );
    }

    /// Register the user-facing `fauna.nostr.zap_signers.*` kinds — the
    /// NIP-57 trust root (`docs/goal/behavior/monetization.md` § Zap
    /// receipts — the trust model): the payee's designated list of signers
    /// allowed to speak for their money, which is what decides whether a
    /// kind-9735 receipt is believed at either ingress point. Caller-scoped,
    /// gated User-class nest-side, and consumed by the Nostr page's
    /// zap-signer section.
    ///
    /// All three are light row reads/writes (5 s) and replay-safe: `add`
    /// upserts on `(actor_id, signer_pubkey)` and `remove` deletes, so a
    /// replayed envelope is idempotent (the bunker `revoke`/`set_label`
    /// precedent). Nothing here mints key material or makes an external
    /// round-trip — deliberately: deriving the designation from the payee's
    /// LNURL metadata was the rejected alternative, because it moves the
    /// trust root onto an attacker-influenceable outbound fetch.
    ///
    /// Gated with the rest of the `zaps` plane: the kind STRINGS are what an
    /// excised artifact is checked for (`dynamic-features.md` § What
    /// "completely compiled away" means, item 2 — no wire senders), so leaving
    /// this table ungated would leave `fauna.nostr.zap_signers.*` in the binary
    /// even in a Damus-flavor build whose whole point is their absence.
    #[cfg(feature = "zaps")]
    pub fn register_nostr_zap_signer_kinds(&mut self) {
        use std::time::Duration;
        let light = RpcKindMeta::new(false, Duration::from_secs(5));
        self.add("fauna.nostr.zap_signers.list", light);
        self.add("fauna.nostr.zap_signers.add", light);
        self.add("fauna.nostr.zap_signers.remove", light);
    }

    /// Register the protocol-native Nostr content kinds
    /// (`nostr.{zaps.total,badges.list,events.publish_signed}`) — the WS-RPC
    /// successors to the deleted `/api/v1/nostr/{zaps,badges,publish-signed}`
    /// HTTP routes (the native-content HTTP→WS-RPC rip, 2026-07-22;
    /// `nostr.md` § WS-RPC migration contract). Prefix-less `nostr.*` per the
    /// `bluesky.feed.thread` precedent — a bridge's protocol-unique
    /// consume-side surface keeps a `<bridge>.*` kind, while nest-backed
    /// feeds stay `fauna.nostr.*` (see `nostr.rs`'s module doc).
    ///
    /// - `zaps.total` / `badges.list` are pure local-DB reads (replay-safe,
    ///   5 s) — unlike `bluesky.feed.thread` they make no external round-trip
    ///   (the sync worker already ingested the rows).
    /// - `events.publish_signed` is `forbid_replay=true` at 30 s — it
    ///   relay-enqueues a one-shot outbound event (the `fauna.email.send`
    ///   shape; a replayed envelope must not re-broadcast).
    pub fn register_nostr_content_kinds(&mut self) {
        use std::time::Duration;
        let read = RpcKindMeta::new(false, Duration::from_secs(5));
        // A carve, not a whole-family gate: `zaps.total` is a registry surface
        // and its two siblings are not (`dynamic-features.md` § Charter
        // members — the `zaps` row names zap-total display a gate surface).
        #[cfg(feature = "zaps")]
        self.add("nostr.zaps.total", read);
        self.add("nostr.badges.list", read);
        self.add(
            "nostr.events.publish_signed",
            RpcKindMeta::new(true, Duration::from_secs(30)),
        );
    }

    /// Register the user-facing `fauna.conversations.channel.*` kinds —
    /// the MLS-channel ciphertext plane end-user clients call from the
    /// conversations page (DM + group chat ciphertext send/poll +
    /// per-actor channel list). T1 of the WS-RPC conversations migration
    /// (tracked internally); subsequent slices
    /// add `fauna.conversations.{keypackage,welcome}.*`, and the room family
    /// (`fauna.conversations.room.*`) carries the rest.
    ///
    /// - `channel.send` is `forbid_replay=true` at 30 s. The nest assigns
    ///   `MAX(seq)+1` per channel on each accepted message; a replay
    ///   inserts a duplicate ciphertext row at a new seq and re-pushes
    ///   `fauna.conversations.channel.message` to subscribers. The 30 s
    ///   deadline envelopes the storage-mode-driven ingest
    ///   (`Storage::ingest_channel_envelope` plus the behavioral-anomaly
    ///   scorer side-path), matching the `fauna.bridges.link` shape for
    ///   "rare significant ops".
    /// - `channel.fetch` is a pure read with a `since`-cursor pager;
    ///   replay-safe at 5 s.
    /// - `channel.list_for_actor` is a pure read; replay-safe at 5 s.
    ///   (The HTTP twin's path-param-vs-bearer match check is implicit
    ///   on the WS-RPC plane — the caller is the connection's actor.)
    pub fn register_conversations_channel_kinds(&mut self) {
        use std::time::Duration;

        let read = RpcKindMeta::new(false, Duration::from_secs(5));
        self.add(
            "fauna.conversations.channel.send",
            RpcKindMeta::new(true, Duration::from_secs(30)),
        );
        // The foreign-member send relay (`direct-messages.md` § step 3b) — same
        // replay posture as `send` (server-assigned seq + push duplication
        // risk on the home nest); 30 s covers the extra federation hop.
        self.add(
            "fauna.conversations.channel.send_remote",
            RpcKindMeta::new(true, Duration::from_secs(30)),
        );
        // The cross-nest attachment upload's mint relay — a read-shaped mint
        // (an in-memory short-TTL token, no row), so `forbid_replay = false`
        // and the same 5 s the folders twin `write_token.get` runs with.
        self.add(
            crate::conversations::KIND_CONVERSATIONS_BLOB_WRITE_TOKEN_GET,
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add("fauna.conversations.channel.fetch", read);
        self.add("fauna.conversations.channel.list_for_actor", read);
        // The roster read `list_for_actor` inverts (actors on one channel) —
        // the phantom-leaf discriminator for `add_participant`'s heal
        // (`mls-group-key-material.md` § M2 *Admitting a member*). A pure read.
        self.add("fauna.conversations.channel.actors", read);
        // The foreign-member roster-read relay (`federation.md` § Cross-nest) —
        // a pure read like `actors`, but 30 s covers the federation hop.
        self.add(
            "fauna.conversations.channel.actors_remote",
            RpcKindMeta::new(false, Duration::from_secs(30)),
        );
    }

    /// Register the user-facing `fauna.conversations.keypackage.*` kinds —
    /// the MLS key-package plane backing chat initiation (publish + FIFO
    /// fetch + non-destructive count). T2 of the WS-RPC conversations
    /// migration (tracked internally).
    ///
    /// - `keypackage.upload` is replay-safe at 5 s. The DB stores each
    ///   uploaded blob under a fresh random `id`; a replay double-stores
    ///   the bytes — wasted entries (30-day expiry) but no corruption
    ///   risk. FIFO consumption on `fetch` tolerates duplicates.
    /// - `keypackage.fetch` is `forbid_replay=true` at 5 s. The handler
    ///   consumes the oldest non-expired KP for the target actor (DELETE
    ///   after SELECT); a replay consumes a second package on top of the
    ///   one the caller already received, which is observable to other
    ///   senders (queue depletion) and to the recipient (forced re-publish).
    /// - `keypackage.count` is a pure read; replay-safe at 5 s.
    pub fn register_conversations_keypackage_kinds(&mut self) {
        use std::time::Duration;

        let read = RpcKindMeta::new(false, Duration::from_secs(5));
        self.add("fauna.conversations.keypackage.upload", read);
        self.add(
            "fauna.conversations.keypackage.fetch",
            RpcKindMeta::new(true, Duration::from_secs(5)),
        );
        self.add("fauna.conversations.keypackage.count", read);
    }

    /// Register the user-facing `fauna.conversations.welcome.deliver`
    /// kind — same-nest MLS Welcome delivery. T3 of the WS-RPC
    /// conversations migration (tracked internally).
    ///
    /// `forbid_replay=true` at 30 s. The handler pushes a Welcome row
    /// into the recipient's inbox (`push_inbox`) AND fires a
    /// `fauna.conversations.welcome.received` push event AND emits a
    /// best-effort APNS/FCM notification — replay duplicates the inbox
    /// row (the MLS engine would reject the re-processed Welcome as
    /// already consumed, producing noisy logs) and re-fires the push +
    /// mobile notification. 30 s deadline matches the
    /// `fauna.bridges.link` shape for "rare significant ops".
    pub fn register_conversations_welcome_kinds(&mut self) {
        use std::time::Duration;
        self.add(
            "fauna.conversations.welcome.deliver",
            RpcKindMeta::new(true, Duration::from_secs(30)),
        );
    }

    /// Register the `fauna.conversations.room.*` family — the **room
    /// plane**, minted additively beside the group kinds it replaced, which
    /// are since retired
    /// (`../../docs/goal/behavior/conversation-rooms.md` § The group plane's
    /// fate, steps 2–3).
    ///
    /// The family carries the floor roster's two doors — the **ceremony**
    /// (`room.create`) that founds a community room and the **mirror**
    /// (`room.roster_report`) an end-to-end room's committing device reports
    /// through — plus the roster read, the membership and governance doors,
    /// and the **sealing plane**: the two keying acts
    /// (`publish_generation`, `backfill_generations`) and the two reads
    /// (`generations`, and its relayed twin `generations_remote` for a member
    /// homed on another nest).
    ///
    /// Replay semantics:
    /// - `room.create` is idempotent on the derived `room_id` — the id is
    ///   `derive_room_id(owner, salt)`, so a replay of the byte-identical
    ///   birth record re-derives the same room. It is idempotent *including
    ///   against a room whose policy has moved on*: `CacheDb::found_room`
    ///   writes nothing once `policy_version > 1`, so a replay can never roll
    ///   a ratcheted policy back to the founding version — the claim holds
    ///   permanently rather than only while nothing can raise the version.
    ///   No externally-visible side effect: nothing is fanned out and no peer
    ///   is contacted. Replay-safe at 30 s — a signature verification plus a
    ///   small transaction.
    /// - `room.roster_report` is idempotent by construction — it replaces
    ///   the room's floor roster wholesale, so re-applying the same report
    ///   is a no-op, and it has no externally-visible side effect (nothing
    ///   is fanned out). Replay-safe at 5 s: two row writes plus the
    ///   membership re-derivation.
    /// - `room.list_roster` is a pure read — replay-safe at 5 s. Its relayed
    ///   twin `room.list_roster_remote` is the same read for a room homed on
    ///   another nest, replay-safe on the same grounds.
    /// - `room.roster_report_remote` is the report's relayed twin — the same
    ///   wholesale replace, originated by the reporter's own nest to the
    ///   room's home — idempotent on both hops and replay-safe at 30 s (the
    ///   peer round trip).
    /// - `room.leave_remote` is the departure's relayed twin, and the one
    ///   relayed twin that does NOT inherit its door's replay flag: the
    ///   room home's federated leave is idempotent by construction (a caller
    ///   already off the floor is a converged success), precisely so the
    ///   §4.D auto re-send the relay adds cannot turn a departure that landed
    ///   into a reported failure. Replay-safe at 30 s (the peer round trip).
    /// - `room.invite_remote` is the invitation's relayed twin for an
    ///   inviter homed on another nest, and it DOES inherit its door's replay
    ///   flag: the room home's issue door delivers a knock per call, so a
    ///   re-sent frame would notify the invitee twice. 30 s, the peer round
    ///   trip.
    pub fn register_conversations_room_kinds(&mut self) {
        use std::time::Duration;
        self.add(
            "fauna.conversations.room.create",
            RpcKindMeta::new(false, Duration::from_secs(30)),
        );
        self.add(
            "fauna.conversations.room.roster_report",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.conversations.room.list_roster",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        // The pending-invitation read (`conversation-rooms.md` § Join rules and
        // invites): pure, replay-safe, the roster read's posture.
        self.add(
            "fauna.conversations.room.list_invites",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        // The roster read's relayed twin, for a member homed on another nest.
        // Pure read like the door it forwards to, so replay-safe; the longer
        // deadline is the peer round trip (the `channel.actors_remote` /
        // `generations_remote` posture).
        self.add(
            "fauna.conversations.room.list_roster_remote",
            RpcKindMeta::new(false, Duration::from_secs(30)),
        );
        // The report's relayed twin, for a member homed on another nest:
        // idempotent like the door it forwards to, on the peer round-trip's
        // deadline.
        self.add(
            "fauna.conversations.room.roster_report_remote",
            RpcKindMeta::new(false, Duration::from_secs(30)),
        );
        // The DEPARTURE's relayed twin, for a member homed on another nest —
        // and the one twin whose replay flag deliberately differs from the
        // door it forwards to (`room.leave` below forbids replay). The relay
        // is the reason: it adds the §4.D auto re-send the same-nest door has
        // no equivalent of, so the room home's federated door is idempotent —
        // a caller already off the floor is a converged success — and a
        // re-sent frame must be able to report the departure that landed
        // rather than a failure that never happened. Peer-round-trip
        // deadline, like the two relayed twins above it.
        self.add(
            "fauna.conversations.room.leave_remote",
            RpcKindMeta::new(false, Duration::from_secs(30)),
        );
        // The acceptance's relayed twin, the same posture as `leave_remote`
        // and for the same reason: the room home's federated accept is
        // idempotent (a member the recorded invitation already seated from
        // this nest is answered with its role), so the §4.D re-send the
        // relay adds reports a seating that landed rather than a failure.
        self.add(
            "fauna.conversations.room.accept_invite_remote",
            RpcKindMeta::new(false, Duration::from_secs(30)),
        );
        // The membership doors — all forbid replay, each being externally
        // visible: `invite` notifies the invitee, `accept_invite` seats a
        // member, and `remove`/`leave` change who the room fans out to.
        // The governance doors join them: a policy change is externally
        // visible (every member renders it) and the strict version ratchet
        // refuses a replay on its own merits.
        for kind in [
            "fauna.conversations.room.invite",
            // The invitation's relayed twin, for an INVITER homed on another
            // nest — and, unlike `leave_remote`/`accept_invite_remote`, it
            // inherits its door's flag: the room home's federated issue door
            // records a row and delivers a knock per call, so a re-sent frame
            // would notify the invitee twice. The inviter re-issues instead.
            "fauna.conversations.room.invite_remote",
            "fauna.conversations.room.accept_invite",
            "fauna.conversations.room.remove",
            "fauna.conversations.room.leave",
            // Withdrawing an invitation consumes the invitee's inbox envelope.
            "fauna.conversations.room.revoke_invite",
            "fauna.conversations.room.set_policy",
            // The policy's sibling record — which transparent labelers the
            // home nest applies to a community room — governed the same way.
            "fauna.conversations.room.set_labelers",
            "fauna.conversations.room.transfer_ownership",
        ] {
            self.add(kind, RpcKindMeta::new(true, Duration::from_secs(30)));
        }
        // The sealing plane (`conversation-rooms.md` § The three classes →
        // *Community*). Both keying acts forbid replay because both hand out
        // key material: a mint changes which key the room seals under and can
        // revoke the home nest's read, and a backfill covers a new member
        // over generations that already exist. (The mint's parent ratchet
        // refuses a replayed mint on its own merits — the flag is the honest
        // declaration, not the only defence.)
        for kind in [
            "fauna.conversations.room.publish_generation",
            "fauna.conversations.room.backfill_generations",
        ] {
            self.add(kind, RpcKindMeta::new(true, Duration::from_secs(30)));
        }
        // A seat's own wrap target — bound at seating by the ceremony and
        // the accept, and supplied or rotated afterwards through this door
        // (a successor's seat, a seat with no roster entry, a member's key rotation).
        // Forbids replay because a replayed older request would roll a
        // rotated seat back to a key its owner retired.
        self.add(
            "fauna.conversations.room.set_reception_key",
            RpcKindMeta::new(true, Duration::from_secs(30)),
        );
        // The two reads: a member's own wraps, same-nest and relayed. Pure
        // reads — serving a wrap registers nothing and mints nothing — so
        // both are replay-safe. The relayed one carries a peer round trip,
        // hence the longer deadline (the `channel.actors_remote` posture).
        self.add(
            "fauna.conversations.room.generations",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.conversations.room.generations_remote",
            RpcKindMeta::new(false, Duration::from_secs(30)),
        );
        // The read position's own door — searching a community room through
        // the derived view its home nest built (§ The three classes → *What
        // the home nest does with its read*). A pure read of that view, and
        // the only room kind whose answer is not key material: it names log
        // positions and never the text at them, so a member that holds no
        // wrap for the tip learns where to look and still cannot read.
        self.add(
            "fauna.conversations.room.search",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
    }

    /// Register the task-delegation heartbeat-lease kinds
    /// (`fauna.delegation.{heartbeat,observe}`). Both replay-safe at 5 s:
    /// `heartbeat` is an idempotent last-writer-wins write (replaying it just
    /// re-records the same holder), `observe` is a pure read. The
    /// `fauna.delegation.lease_changed` push is registered separately (it is a
    /// push, not an RPC kind). Design tracked internally.
    pub fn register_delegation_kinds(&mut self) {
        use std::time::Duration;
        let read = RpcKindMeta::new(false, Duration::from_secs(5));
        self.add(crate::delegation::KIND_HEARTBEAT, read);
        self.add(crate::delegation::KIND_OBSERVE, read);
    }

    /// Register the user-facing spam-classifier preferences kinds
    /// (`fauna.spam.{get_preferences,set_preferences}`). Both replay-safe
    /// at 5 s: `get_preferences` is a pure read; `set_preferences` is an
    /// idempotent upsert (the same preferences applied twice yields the
    /// same stored state). Scoped as part of the WS-RPC spam-preferences
    /// migration (tracked internally).
    pub fn register_spam_kinds(&mut self) {
        use std::time::Duration;
        let read = RpcKindMeta::new(false, Duration::from_secs(5));
        self.add("fauna.spam.get_preferences", read);
        self.add("fauna.spam.set_preferences", read);
    }

    /// Register the user-facing moderation kinds
    /// (`fauna.moderation.{stats,actions,appeal,train}` + the report/signal kinds)
    /// — the WS-RPC migration of the former `/api/v1/moderation/*` HTTP routes.
    ///
    /// `forbid_replay` values, decided per kind (audit 2026-08-02): every kind
    /// in the family is `false` because its **handler** is naturally idempotent
    /// (`transport.md` § Idempotency and reconnect-with-resume: `false` asserts
    /// handler idempotency, NOT that the cache catches a double-apply — the
    /// cache is per-connection and an auto-retry always lands on a fresh one):
    /// `stats`/`actions`/`*.status` are pure reads; `train` no longer trains —
    /// it checks the read gate and captures the opt-in report, an idempotent
    /// insert/delete on the `(content_hash, factor, reporter)` key;
    /// `report_share.set`/`signal_share.set` are last-wins bool writes;
    /// `signal_contribute` goes through `capture_signal`, whose per-factor
    /// insert/delete report their own "changed" bit and gate the recompute;
    /// `appeal`/`legal_takedown` append an audit or obligation
    /// row that a replay duplicates — a redundant transparency entry, never a
    /// second application of an effect (`legal_takedown`'s withhold flag is a
    /// last-wins set, and it is Admin-only).
    ///
    /// `train` keeps a 10 s deadline (post fetch + decode through the post-read
    /// core); the rest 5 s. Slice tracked internally as part of
    /// the WS-RPC-everywhere migration.
    pub fn register_moderation_kinds(&mut self) {
        use std::time::Duration;
        let quick = RpcKindMeta::new(false, Duration::from_secs(5));
        let heavy = RpcKindMeta::new(false, Duration::from_secs(10));
        self.add("fauna.moderation.stats", quick);
        self.add("fauna.moderation.actions", quick);
        self.add("fauna.moderation.appeal", quick);
        // Replay-safe since the nest stopped training (the per-user model rests
        // sealed and only a capability holder mutates it): the handler is a
        // read gate plus the report capture, whose insert/delete on the
        // `(content_hash, factor, reporter)` key is idempotent — a replay
        // records the same verdict once. Mirror any change in
        // `register_moderation_handlers`.
        self.add("fauna.moderation.train", heavy);
        // The narrow legal-obligation social-takedown carve-out (Admin-only;
        // moderation.md § Categories & enforcement item 1). A quick set/clear of
        // a content_meta flag + an obligation/audit row.
        self.add("fauna.moderation.legal_takedown", quick);
        // Report-sharing opt-in + transparency (report-sharing.md § Client wire).
        // `set` is an idempotent bool write (+ a bounded opt-out sweep);
        // `status` is a pure read of the k-gated export view. Both quick,
        // replay-safe (the idempotency cache replays a retried `set`).
        self.add("fauna.moderation.report_share.set", quick);
        self.add("fauna.moderation.report_share.status", quick);
        // Layer-B engagement-signal sharing (engagement-cues.md § Layer B nest
        // legs) — the opt-in sibling of report sharing, INDEPENDENT preference.
        // `signal_share.{set,status}` mirror `report_share.*`; `signal_contribute`
        // is the client's per-item cue-verdict write (a few small content_reports
        // writes + a bounded recompute of <=2 aggregates). All quick, replay-safe
        // (idempotent last-wins verdict; the idempotency cache replays a retry).
        self.add("fauna.moderation.signal_share.set", quick);
        self.add("fauna.moderation.signal_share.status", quick);
        self.add("fauna.moderation.signal_contribute", quick);
        // User-initiated reporting (moderation.md § User-initiated reporting).
        // All replay-safe: `submit` dedupes one open report per (reporter,
        // subject) and replies with the existing one; `withdraw` is idempotent
        // on a withdrawn report; `resolve` repeated with the same outcome is a
        // no-op (the reporter's notification is written once). `submit` and
        // `resolve` may cross to a peer nest (the forward / the outcome), hence
        // the `heavy` deadline. Mirror any change in `register_moderation_handlers`.
        self.add("fauna.moderation.abuse_report.submit", heavy);
        self.add("fauna.moderation.abuse_report.mine", quick);
        self.add("fauna.moderation.abuse_report.withdraw", heavy);
        self.add("fauna.moderation.abuse_report.queue", quick);
        self.add("fauna.moderation.abuse_report.resolve", heavy);
    }

    /// Register the family-safety kinds (`family-safety.md` § Wire & data
    /// shape) — the guardianship relationship/lifecycle surface. All quick:
    /// `status` is a pure caller-scoped read; `policy.update` an idempotent
    /// per-ward document write; `graduate`/`transfer` idempotent one-row
    /// lifecycle transitions. Replay-safe (the idempotency cache replays a
    /// retried mutation). Payload types: `family.rs`; handlers:
    /// `bins/fauna-nest/src/family_handlers.rs`. The reach-approval kinds
    /// (`approvals.*`, `contact.add`) and the guardian-device marker
    /// (`device.mark`) register here when their slices land.
    pub fn register_family_kinds(&mut self) {
        use std::time::Duration;
        let quick = RpcKindMeta::new(false, Duration::from_secs(5));
        self.add("fauna.family.status", quick);
        self.add("fauna.family.policy.update", quick);
        self.add("fauna.family.graduate", quick);
        self.add("fauna.family.transfer", quick);
        // The transfer consent handshake (family-safety.md § Graduation &
        // transfer, ratified 2026-07-12): `transfer` proposes; the proposed
        // guardian accepts/declines; the initiator side cancels. All quick,
        // idempotent one-row transitions.
        self.add("fauna.family.transfer.accept", quick);
        self.add("fauna.family.transfer.decline", quick);
        self.add("fauna.family.transfer.cancel", quick);
        // Reach approvals (slice 3): the guardian queue read + the per-item
        // decision + the pre-approve. All quick, idempotent (decide re-runs
        // the same accept/block upsert; add re-upserts the accepted edge).
        self.add("fauna.family.approvals.list", quick);
        self.add("fauna.family.approvals.decide", quick);
        self.add("fauna.family.contact.add", quick);
        // The ward's in-app contact ask (v1.x, family-safety.md § Child-
        // initiated contact requests). Quick, replay-safe: the pending row's
        // primary key makes a re-ask while pending a quiet no-op, so a retry
        // never double-rings the guardian.
        self.add("fauna.family.contact.request", quick);
        // The ward's in-app feed-source ask (v1.x, family-safety.md
        // § Feed-source approvals). Quick, replay-safe for the same reason as
        // the contact ask: the row's UNIQUE key makes a re-ask against an open
        // row a quiet no-op, so a retry never double-rings the guardian.
        self.add("fauna.family.feed_source.request", quick);
        // Guardian Notify (v1.x, family-safety.md § Guardian Notify): the ward's
        // conforming client reports coarse per-category enforcement counts.
        // Quick, forbid_replay = false — the per-connection idempotency cache
        // replays the first reply on a retry rather than re-executing, so a
        // retried report never double-counts its delta into the day total.
        self.add("fauna.family.notify_report", quick);
        // Screen-time budget accounting (v1.x, family-safety.md § Screen
        // time): the ward's client heartbeats coarse foreground minutes; the
        // reply returns the day's cross-device total. Replay-safe — the
        // accumulate is a clamped delta and a retried report over-counts a
        // heartbeat at worst (toward stricter enforcement, guardian-editable).
        self.add("fauna.family.usage_report", quick);
        // The guardian-enrolled-device marker (v1.x, family-safety.md § Full
        // visibility): the guardian sets/clears one flag on the ward's device
        // row. Quick, idempotent (re-marking an already-marked device writes
        // the same value), so forbid_replay = false.
        self.add("fauna.family.device.mark", quick);
    }

    /// Register the pre-identity auth-bootstrap kinds
    /// (`fauna.auth.{handshake,challenge,verify}`) — the WS-RPC migration of
    /// `POST /api/v1/auth/{token,challenge,verify}`. These run on the
    /// **anonymous** WS connection (no bearer) per `transport.md`
    /// § Pre-identity (anonymous) connection; Track A1 of
    /// the WS-RPC-everywhere migration (tracked internally).
    ///
    /// All three are `forbid_replay = false` @5 s. None is re-executed on a
    /// retry: the per-connection idempotency cache replays the first reply,
    /// so a retried `handshake` returns the same minted token (not a second),
    /// and a retried `verify` returns the cached token without re-consuming
    /// the one-shot nonce (re-execution would 404 on the spent nonce).
    pub fn register_auth_kinds(&mut self) {
        use std::time::Duration;
        let auth = RpcKindMeta::new(false, Duration::from_secs(5));
        self.add("fauna.auth.handshake", auth);
        self.add("fauna.auth.challenge", auth);
        self.add("fauna.auth.verify", auth);
        // Pre-identity nest-identity handshake (`crate::auth::NEST_HANDSHAKE_KIND`)
        // — a pure read (the nest signs a channel binding over the client's
        // nonce; nothing is mutated, each reply is nonce-unique so replay is
        // moot). Runs as the opening step of a client-provisioned box's
        // pre-claim connections (security.md § Transport trust, Axis 2).
        self.add("fauna.auth.nest_handshake", auth);
        // Renewal-grant bearer mint (`crate::auth::DEVICE_HANDSHAKE_KIND`,
        // sync-agent.md § Credential model; additive 2026-07-19). Same replay
        // posture as `fauna.auth.handshake`: the idempotency cache replays the
        // first reply (same minted token), and the signature replay guard makes
        // a re-executed request fail closed.
        self.add("fauna.auth.device_handshake", auth);
        // Custody-session bearer mint (`crate::auth::CUSTODY_HANDSHAKE_KIND`,
        // W8.6 (account-data-plane.md § Workstreams) — `account-data-plane.md` § Replica posture; additive
        // 2026-08-16). Same replay posture as its device-handshake sibling:
        // the idempotency cache replays the first reply (same minted token),
        // and the signature replay guard fails a re-executed request closed.
        self.add("fauna.auth.custody_handshake", auth);
        // The deployment-seed rotation chain
        // (`crate::nest_rotation::ROTATION_CHAIN_KIND`, `nest/box-recovery.md`
        // § Client acceptance). It sits in the `fauna.auth.*` family because it
        // rides the anonymous door, but unlike its five neighbours it is a
        // **pure read** — it mints nothing, consumes no nonce and touches no
        // actor, so `forbid_replay = false` is the honest value rather than the
        // audited-idempotent one: a replay simply re-reads the log. @5 s like
        // the other reads; the chain is deployment-rare and needs no paging.
        self.add(crate::nest_rotation::ROTATION_CHAIN_KIND, auth);
    }

    /// Register the pre-identity public-discovery kinds
    /// (`fauna.nest.info`, `fauna.handle.available`, `fauna.nest.resolve`,
    /// `fauna.actor.by_handle`, `fauna.setup.status`) — the WS-RPC migration of
    /// the public GET routes `/api/v1/{node-info,handle-available/{h},
    /// resolve-node/{d},actor/by-handle/{h},setup-status}`. Like the auth
    /// bootstrap kinds these run on the **anonymous** WS connection (no bearer)
    /// per `transport.md` § Pre-identity (anonymous) connection; Track A2 of
    /// the WS-RPC-everywhere migration (tracked internally).
    ///
    /// All five are pure reads — `forbid_replay = false` @5 s. They have no
    /// side effects, so a replayed request simply re-reads current state; the
    /// per-connection idempotency cache replays the first reply on a recovered
    /// connection regardless.
    pub fn register_discovery_kinds(&mut self) {
        use std::time::Duration;
        let read = RpcKindMeta::new(false, Duration::from_secs(5));
        self.add("fauna.nest.info", read);
        self.add("fauna.handle.available", read);
        self.add("fauna.nest.resolve", read);
        self.add("fauna.actor.by_handle", read);
        self.add("fauna.setup.status", read);
    }

    /// Register the pre-identity account-registration kind
    /// `fauna.account.register` — the WS-RPC migration of the public
    /// `POST /api/v1/register` route. Like the auth-bootstrap + discovery kinds
    /// it runs on the **anonymous** WS connection (no bearer) per `transport.md`
    /// § Pre-identity (anonymous) connection; Track A3 of
    /// the WS-RPC-everywhere migration (tracked internally).
    ///
    /// `forbid_replay = false` @30 s (a write with a DB transaction + spawned
    /// DNS, matching `fauna.posts.create`). A replay is safe by construction:
    /// the `is_actor_registered` / `resolve_handle` / `UNIQUE` checks make a
    /// re-run of the same registration return the corresponding conflict, and
    /// the per-connection idempotency cache replays the first reply on a
    /// recovered connection regardless.
    ///
    /// The authenticated account surface (`fauna.account.{get,delete,upgrade,
    /// quota}`, Track B1 of the content umbrella) registers separately on the
    /// bearer connection — this method covers only the pre-identity `register`.
    pub fn register_account_register_kind(&mut self) {
        use std::time::Duration;
        self.add(
            "fauna.account.register",
            RpcKindMeta::new(false, Duration::from_secs(30)),
        );
    }

    /// `fauna.account.age_nonce` — mint the short-lived, single-use nonce an
    /// attested age claim commits to (`family-safety.md` § The account age
    /// band; payload contract `age::age_claim_signed_message`). Anonymous
    /// pre-identity kind like `register` — the registering actor has no
    /// account yet.
    ///
    /// `forbid_replay = false` @5 s — a pure in-memory mint; a replay just
    /// mints another nonce (each is independent and single-use), and the
    /// per-connection idempotency cache replays the first reply on a
    /// recovered connection.
    pub fn register_account_age_nonce_kind(&mut self) {
        use std::time::Duration;
        self.add(
            "fauna.account.age_nonce",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
    }

    /// Register the pre-identity **emergency-lockout** kind
    /// `fauna.account.lockout` — the WS-RPC migration of the no-token recovery
    /// route `POST /api/v1/account/lockout`. Like `register` it rides the
    /// **anonymous** WS connection (no bearer) per `transport.md` § Pre-identity
    /// (anonymous) connection and authenticates from the request payload (an
    /// Ed25519 signature over `actor_id ‖ timestamp_be`), not a bearer. The
    /// authed sibling is `fauna.sessions.lockout`.
    ///
    /// `forbid_replay = false` @5 s — a quick DB write (revoke tokens +
    /// `set_locked_until`); a client's blind auto-retry (identical bytes)
    /// re-applies the same protective lock, and the per-connection idempotency
    /// cache replays the first reply on a recovered connection. The lock
    /// window is the nest's hard-coded 24-hour constant — the request carries
    /// no duration at all, which is what makes even an on-path replay unable to
    /// change the outcome beyond re-locking.
    pub fn register_account_lockout_kind(&mut self) {
        use std::time::Duration;
        self.add(
            "fauna.account.lockout",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
    }

    /// Register the pre-identity one-time admin-claim kind
    /// `fauna.auth.claim_admin` — the sole transport for admin claim since the
    /// public `POST /api/v1/claim-admin` route was removed (S4d). Like the
    /// auth-bootstrap + discovery +
    /// registration kinds it runs on the **anonymous** WS connection (no bearer)
    /// per `transport.md` § Pre-identity (anonymous) connection; Track A4 of
    /// the WS-RPC-everywhere migration (tracked internally).
    ///
    /// `forbid_replay = false` @5 s — it mints a bearer token like
    /// `fauna.auth.handshake`, and a one-time replay is safe by construction:
    /// the claim-code file is deleted on success, so a second invocation fails
    /// with `fauna.auth.already_claimed`, and the per-connection idempotency
    /// cache replays the first reply on a recovered connection.
    pub fn register_claim_admin_kind(&mut self) {
        use std::time::Duration;
        self.add(
            "fauna.auth.claim_admin",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
    }

    /// Register the **authenticated** account-management surface — Track B1 of
    /// the content umbrella (tracked internally), a
    /// behavior-preserving transport migration of the bearer-authed HTTP routes
    /// `GET /api/v1/account`, `GET /api/v1/quota`, `GET /api/v1/am-i-admin`,
    /// `PUT /api/v1/profile/handle`, `POST /api/v1/upgrade`,
    /// `DELETE /api/v1/account`. One fn spanning three top namespaces
    /// (`fauna.account.*`, `fauna.quota.*`, `fauna.profile.*`) for the single
    /// "account-management" feature, exactly as `register_contacts_kinds` spans
    /// knocks/contacts/inbox.mode. These ride the **bearer** connection (unlike
    /// the pre-identity `register` above). Caller class `User | Admin` (an
    /// admin manages their own account too) — enforced in
    /// `bridge_method_allowlist::is_permitted`. (`/api/v1/export` is **not**
    /// here — a zstd-tar byte download, HTTP residue.)
    ///
    /// Replay semantics:
    /// - The three reads (`account.get`, `quota.get`, `account.am_i_admin`) are
    ///   pure reads — `forbid_replay = false` @5 s.
    /// - `profile.handle.change` + `account.delete` each create a *pending
    ///   action* (the destructive-op delay window, api-layers.md § Destructive
    ///   operations are delayed). A 5 s connection-recovery replay re-creating a
    ///   near-identical queued row is harmless (the user/admin cancels
    ///   duplicates within the window) and neither increments a non-idempotent
    ///   score → `forbid_replay = false` @5 s.
    /// - `account.upgrade` consumes an invite code (`validate_invite_code` →
    ///   `update_user`) — the one non-idempotent side effect. A genuine replay
    ///   can't double-consume: the per-connection idempotency cache replays the
    ///   first reply on a recovered connection, and a re-run against the
    ///   now-consumed code returns `invalid_request` → `forbid_replay = false`
    ///   @30 s (write + invite validation, matching `fauna.posts.create`).
    pub fn register_account_kinds(&mut self) {
        use std::time::Duration;
        let read = RpcKindMeta::new(false, Duration::from_secs(5));
        self.add("fauna.account.get", read);
        self.add("fauna.quota.get", read);
        self.add("fauna.account.am_i_admin", read);
        // Pending-action creators — idempotent-enough at the 5 s window.
        self.add("fauna.profile.handle.change", read);
        self.add("fauna.account.delete", read);
        // Invite-consuming write — 30 s, matching posts.create.
        self.add(
            "fauna.account.upgrade",
            RpcKindMeta::new(false, Duration::from_secs(30)),
        );
    }

    /// Register the authenticated pending-actions surface
    /// `fauna.pending_actions.{list,get,cancel,approve}` — Track B20 of
    /// the WS-RPC-everywhere migration (tracked internally), a behavior-preserving
    /// transport migration of the (now-deleted) bearer-authed HTTP routes
    /// `/api/v1/pending-actions{,/{id},/{id}/cancel,/{id}/approve}`.
    /// The read/manage complement to the pending-action *creators* on the
    /// account surface (`register_account_kinds`' `account.delete` /
    /// `profile.handle.change`). These ride the **bearer** connection. Caller
    /// class is `User | Admin` for list/get/cancel (a user manages their own
    /// queued actions; an admin manages theirs too) and **Admin-only** for
    /// `approve` (quorum approval — the twin used `AdminBearerAuth`) — enforced
    /// in `bridge_method_allowlist::is_permitted`.
    ///
    /// Replay semantics — all four `forbid_replay = false` @5 s:
    /// - `list` / `get` are pure reads.
    /// - `cancel` is idempotent: a replay against an already-cancelled row
    ///   returns `not cancellable`, and the per-connection idempotency cache
    ///   replays the first reply on a recovered connection.
    /// - `approve` is idempotent by construction: the DB silently succeeds on a
    ///   duplicate approval from the same approver.
    pub fn register_pending_actions_kinds(&mut self) {
        use std::time::Duration;
        let read = RpcKindMeta::new(false, Duration::from_secs(5));
        self.add("fauna.pending_actions.list", read);
        self.add("fauna.pending_actions.get", read);
        self.add("fauna.pending_actions.cancel", read);
        self.add("fauna.pending_actions.approve", read);
    }

    /// Register the authenticated stats surface `fauna.stats.get` — Track B17 of
    /// the WS-RPC-everywhere migration (tracked internally), a behavior-preserving transport
    /// migration of the (now-deleted) bearer-authed HTTP route `GET /api/v1/stats`.
    /// One kind: the `?folder=` query param of the
    /// twin rides as `StatsGetRequest.folder` and selects the discriminated
    /// `StatsGetReply` shape (global vs folder). Rides the **bearer**
    /// connection; gate `User | Admin` (the twin used plain `BearerAuth`) —
    /// enforced in `bridge_method_allowlist::is_permitted`.
    ///
    /// Replay semantics: `forbid_replay = false` @5 s — a pure read.
    pub fn register_stats_kinds(&mut self) {
        use std::time::Duration;
        self.add(
            "fauna.stats.get",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
    }

    /// Register the authenticated link-preview resolve surface
    /// `fauna.linkpreview.resolve` — the wire side of render-model.md § D4. The
    /// client's page manager calls it to resolve a `RenderBlock::LinkPreview`'s
    /// metadata; the nest fetches the url's OpenGraph/meta (behind its
    /// SSRF/size/time guards), caches by url, and replies
    /// `linkpreview::LinkPreviewResolveReply::{Resolved,Failed}`. See
    /// `linkpreview.rs` for the wire types; the nest handler + OG-fetcher + cache
    /// is entrusted to the nest side.
    ///
    /// Replay semantics: `forbid_replay = false` @30 s — a pure read that performs
    /// a (cached) external fetch, matching the `bluesky.feed.thread` external-fetch
    /// read precedent.
    pub fn register_linkpreview_kinds(&mut self) {
        use std::time::Duration;
        self.add(
            crate::linkpreview::KIND_LINKPREVIEW_RESOLVE,
            RpcKindMeta::new(false, Duration::from_secs(30)),
        );
    }

    /// Register the authenticated file-versions surface
    /// `fauna.files.versions.{list,get}` — Track B16 of
    /// the WS-RPC-everywhere migration (tracked internally), a behavior-preserving
    /// transport migration of the bearer-authed HTTP routes
    /// `GET /api/v1/files/{path_hash}/versions[/{version_num}]` (now deleted). Both return JSON
    /// metadata (the version *content* is reconstructed client-side from
    /// `manifest_hash` via the blob routes). Ride the **bearer** connection;
    /// gate `User | Admin` (the twin used plain `BearerAuth`) — enforced in
    /// `bridge_method_allowlist::is_permitted`.
    ///
    /// Replay semantics: both `forbid_replay = false` @5 s — pure reads.
    pub fn register_files_versions_kinds(&mut self) {
        use std::time::Duration;
        let read = RpcKindMeta::new(false, Duration::from_secs(5));
        self.add("fauna.files.versions.list", read);
        self.add("fauna.files.versions.get", read);
        // Retention recovery verb (file-versions.md § Retention (3)): an
        // idempotent single-row restore — replaying it re-asserts "keep this
        // version", so replay is safe like the reads above.
        self.add(
            "fauna.files.versions.undelete",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
    }

    /// Register the authenticated folder management surface
    /// `fauna.folders.*` (CRUD + members + devices + schedule + lease +
    /// cross-user `share` + the M2 `content_key.{put,get}` envelope transport) and
    /// `fauna.sync.conflicts.{list,report,resolve}` — Track B14 of
    /// the WS-RPC-everywhere migration (tracked internally), a behavior-preserving
    /// transport migration of the bearer-authed user folder routes
    /// (`user_folder_routes` + `lease_routes`). Ride the **bearer** connection
    /// and ARE actor-scoped; gate `User | Admin` (the twins used plain
    /// `BearerAuth` — an admin owns folders too, the
    /// web/files.versions/stats/calendars precedent) — enforced in
    /// `bridge_method_allowlist::is_permitted`.
    ///
    /// Replay semantics: all 20 are `forbid_replay = false` @5 s — reads
    /// (`list` / `members.list` / `members.list_actors` / `devices` /
    /// `conflicts.list` / `content_key.get`)
    /// and fast local DB mutations. The mutations are idempotent under the
    /// per-connection idempotency cache (a replayed `create` returns the original id
    /// via the cache, or — on a fresh connection — the UNIQUE-name conflict; `delete`
    /// / `members.remove` / `members.evict` / `lease.release` / `conflicts.resolve`
    /// re-run harmlessly; `share` re-binds the same set to the same group — the same
    /// UPDATE, harmless; `content_key.put` upserts the same envelope row — harmless;
    /// `members.evict` re-runs the same scoped `DELETE` — harmless;
    /// `members.set_access` upserts the same role row — harmless).
    ///
    /// `share` is the cross-user sharing op (Slice 2 of shared folders): it
    /// binds an owner-only set to a client-created MLS group (payloads in
    /// `folders::FolderShare{Request,Reply}`). The nest-side handler +
    /// per-class allowlist arm land in Slice 2's S2-P2 (this is the client-side
    /// metadata twin; the nest router carries its own inline meta).
    pub fn register_folders_kinds(&mut self) {
        use std::time::Duration;
        let m = RpcKindMeta::new(false, Duration::from_secs(5));
        for kind in [
            crate::folders::KIND_FOLDERS_CREATE,
            crate::folders::KIND_FOLDERS_LIST,
            crate::folders::KIND_FOLDERS_UPDATE,
            crate::folders::KIND_FOLDERS_DELETE,
            crate::folders::KIND_FOLDERS_DEVICES,
            crate::folders::KIND_FOLDERS_MEMBERS_LIST,
            crate::folders::KIND_FOLDERS_MEMBERS_LIST_ACTORS,
            crate::folders::KIND_FOLDERS_MEMBERS_SET_ACCESS,
            crate::folders::KIND_FOLDERS_MEMBERS_REMOVE,
            crate::folders::KIND_FOLDERS_MEMBERS_EVICT,
            crate::folders::KIND_FOLDERS_PLACES_SET,
            crate::folders::KIND_FOLDERS_LEAVE,
            crate::folders::KIND_FOLDERS_SHARE,
            crate::folders::KIND_FOLDERS_CONTENT_KEY_PUT,
            crate::folders::KIND_FOLDERS_CONTENT_KEY_GET,
            crate::folders::KIND_FOLDERS_LEASE_ACQUIRE,
            crate::folders::KIND_FOLDERS_LEASE_RELEASE,
            // The publicly-synced follow's client twin (phase 4 slice 4f-i) —
            // a read, so `forbid_replay = false` like every read beside it.
            crate::folders::KIND_FOLDERS_PUBLIC_FETCH,
            "fauna.sync.conflicts.list",
            "fauna.sync.conflicts.report",
            "fauna.sync.conflicts.resolve",
        ] {
            self.add(kind, m);
        }

        // Lifted from the nest router (see `register_all_kinds`).
        self.add(
            crate::folders::KIND_FOLDERS_SET_WEB_PAYWALL,
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            crate::folders::KIND_FOLDERS_WRITE_TOKEN_GET,
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            crate::folders::KIND_FOLDERS_READ_TOKEN_GET,
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        // The cross-nest writer roster read's client leg — a pure read like
        // `members.list_actors`; 30 s covers the federation hop (the
        // `channel.actors_remote` posture).
        self.add(
            crate::folders::KIND_FOLDERS_MEMBERS_LIST_ACTORS_REMOTE,
            RpcKindMeta::new(false, Duration::from_secs(30)),
        );
        // The served-era adoption (`writer-signed-change-records.md` ruling
        // (7)(b)): idempotent, so replay-safe; 30 s covers a full page's
        // signature verifications in one transaction.
        self.add(
            crate::folders::KIND_FOLDERS_SERVED_ROWS_ADOPT,
            RpcKindMeta::new(false, Duration::from_secs(30)),
        );
        // A principal's folder deposit (`file-sync.md` § Third-party deposit
        // ingress): NOT replay-safe — every accepted call parks one more
        // item, so a replay would park the same file twice. 30 s covers a
        // full-size body's seal.
        self.add(
            crate::folders::KIND_FOLDERS_DEPOSIT,
            RpcKindMeta::new(true, Duration::from_secs(30)),
        );
        // The owner's half of the inbox: a pure read, and a retire that is
        // idempotent (a second retire of the same item answers `false`).
        self.add(
            crate::folders::KIND_FOLDERS_DEPOSITS_LIST,
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            crate::folders::KIND_FOLDERS_DEPOSITS_RETIRE,
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
    }

    /// Register the share-link control plane `fauna.share.{create,list,revoke}` —
    /// Track E1 of the WS-RPC-everywhere migration (tracked internally).
    /// A **net-new** feature
    /// (not an HTTP→WS-RPC migration): a ShareToken is client-minted, stateless
    /// and self-verifying, so `share.create` does not mint — it registers a
    /// minted token's metadata in `share_tokens` so the author can list/revoke
    /// their live shares. The public `GET /share/{token}` stays HTTP residue and
    /// gains a revocation check. Ride the **bearer** connection and ARE
    /// actor-scoped; gate `User | Admin` (a personal share surface; an admin
    /// shares too) — enforced in `bridge_method_allowlist::is_permitted`. Wire
    /// types: `libs/fauna-protocol/src/share.rs`.
    ///
    /// Replay semantics: all three `forbid_replay = false` @5 s — `list` is a
    /// read; `create` is idempotent (INSERT OR IGNORE on the derived token-id);
    /// `revoke` is an idempotent UPDATE (re-revoking a revoked row re-succeeds).
    pub fn register_share_kinds(&mut self) {
        use std::time::Duration;
        let m = RpcKindMeta::new(false, Duration::from_secs(5));
        for kind in [
            "fauna.share.create",
            "fauna.share.list",
            "fauna.share.revoke",
        ] {
            self.add(kind, m);
        }
    }

    /// `fauna.sync.{register,changes.{list,record},status,files,backup_status,
    /// devices.{list,delete}}` — Track B13 of
    /// the WS-RPC-everywhere migration (tracked internally), a behavior-preserving transport
    /// migration of the bearer-authed device-sync routes (`sync_routes`). Ride
    /// the **bearer** connection and ARE actor-scoped; gate `User | Admin` (the
    /// twins used plain `BearerAuth` — an admin owns sync devices too, the
    /// folders/web/files.versions precedent) — enforced in
    /// `bridge_method_allowlist::is_permitted`. The sibling
    /// `fauna.sync.conflicts.*` kinds register in `register_folders_kinds`;
    /// the bulk byte routes stay HTTP residue.
    ///
    /// Replay semantics: all 9 are `forbid_replay = false` @5 s — reads
    /// (`changes.list` / `status` / `files` / `backup_status` / `devices.list`)
    /// and fast local DB mutations. `register` is an `INSERT OR REPLACE` upsert;
    /// `devices.delete` re-runs harmlessly; `changes.record` is a non-idempotent
    /// `INSERT` (new `seq` per call, like `folders.create`) — the
    /// per-connection idempotency cache replays the first reply on a recovered
    /// connection, so a replay returns the original `seq` rather than a duplicate
    /// row, matching the HTTP twin's at-most-once-per-POST semantics.
    /// `changes.supersede` is a head-bound idempotent mark (re-runs mark 0 rows).
    /// `device_grant.register` is an idempotent per-row UPDATE (re-runs store
    /// the same verified grant). `device_grant.revoke` is idempotent for the
    /// same reason from the other side — a re-run clears an already-cleared
    /// grant and re-writes the same tombstone, answering `revoked: false`,
    /// which the ruled retirement loop treats as success.
    pub fn register_sync_kinds(&mut self) {
        use std::time::Duration;
        let m = RpcKindMeta::new(false, Duration::from_secs(5));
        for kind in [
            "fauna.sync.register",
            "fauna.sync.device_grant.register",
            "fauna.sync.device_grant.revoke",
            "fauna.sync.changes.list",
            "fauna.sync.changes.record",
            "fauna.sync.changes.supersede",
            "fauna.sync.status",
            "fauna.sync.files",
            "fauna.sync.backup_status",
            "fauna.sync.devices.list",
            "fauna.sync.devices.delete",
            "fauna.sync.devices.p2p_participation.set",
            // Relay serving's announce (`file-sync.md` § Relay serving): it
            // replaces the calling connection's announced set whole, so a
            // replay re-sets the same set — idempotent.
            crate::sync::KIND_SYNC_SERVE_ANNOUNCE,
            // W2.3, the generalized account-data feed's class-2 write leg. It
            // shares this metadata rather than needing the queued shape:
            // although it INSERTs, the coordinates are **client-assigned**
            // (`writer_id` + `writer_seq`) and the nest refuses a `writer_seq`
            // that does not advance that writer's high-water for the item — so
            // a replay is refused as stale rather than allocating a second row,
            // which is exactly the at-most-once property `changes.record` needs
            // the idempotency cache for.
            crate::account_state::KIND_STATE_PUT,
            // `state.put`'s retire counterpart (`account-data-taxonomy.md` §
            // The generation machinery → *Fleet-scope reclamation*). Decided,
            // not inherited from the retired `rotate_as_key` precedent
            // (transport.md:1767-1787, later overturned to
            // `forbid_replay = true` @30s): unlike a key rotation, this
            // handler mints no fresh identity a replay could shift out from
            // under a caller. The row is named by the caller's own cleartext
            // coordinates, never a server-allocated one; the retire probe
            // early-returns before any write once the row is already
            // superseded, so a replay reaches no second mutation; and every
            // reply consumer already treats `Retired` and `Gone` (the
            // already-retired case) identically, so a replay's answer
            // converges too.
            crate::account_state::KIND_STATE_RETIRE,
        ] {
            self.add(kind, m);
        }
    }

    /// `fauna.media.list` — the cross-set all-media aggregating read powering the
    /// Media page's default view (`docs/goal/ui/media.md` § State & data shape).
    /// Aggregates media items across every folder the caller may read; rides
    /// the **bearer** connection (actor-scoped); gate `User | Admin` — enforced in
    /// `bridge_method_allowlist::is_permitted`.
    ///
    /// Replay semantics: `forbid_replay = false` @5 s — a pure keyset-paginated
    /// read with no side effects.
    ///
    /// `fauna.media.playback_ticket` (`media_ticket.rs`) — mint the media
    /// proxy's playback ticket for one proxied path (render-model.md § D6c →
    /// *Inline playback*, answer 4); gate `User`. `forbid_replay = false` @5 s:
    /// it writes nothing, and a replay only mints a second ticket for the same
    /// path, which the caller could have asked for anyway.
    pub fn register_media_kinds(&mut self) {
        use std::time::Duration;
        let m = RpcKindMeta::new(false, Duration::from_secs(5));
        self.add("fauna.media.list", m);
        self.add(crate::media_ticket::KIND_MEDIA_PLAYBACK_TICKET, m);
    }

    /// Register the authenticated web-content-publishing surface
    /// `fauna.web.{publish.{set,unset,list},domain.{set,get,delete},
    /// {set,get}_apex_actor}` — Track B18 of
    /// the WS-RPC-everywhere migration (tracked internally), a behavior-preserving
    /// transport migration of the bearer-authed web-content-hosting HTTP
    /// routes (`web_content::{publish_routes, domain}`), since extended with
    /// `domain.delete` (no HTTP twin) and the Admin apex-actor designation.
    /// Ride the **bearer** connection and ARE actor-scoped; the publish/domain
    /// kinds gate `User | Admin` (the twins used plain `BearerAuth`), the apex
    /// kinds gate `Admin` only — enforced in `bridge_method_allowlist::is_permitted`.
    ///
    /// Replay semantics — all `forbid_replay = false`:
    /// - `publish.list` / `domain.get` / `get_apex_actor` are pure reads @5 s.
    /// - `publish.set` (upsert) / `publish.unset` (delete) / `domain.delete`
    ///   (delete) / `set_apex_actor` (singleton upsert/clear) are idempotent
    ///   mutations @5 s — a replay re-runs harmlessly / the idempotency cache
    ///   replays the first reply.
    /// - `domain.set` is a non-idempotent registration write (generates a verify
    ///   token) @30 s; the per-connection idempotency cache replays the first
    ///   reply (with its token) on a recovered connection.
    pub fn register_web_kinds(&mut self) {
        use std::time::Duration;
        let read = RpcKindMeta::new(false, Duration::from_secs(5));
        self.add("fauna.web.publish.set", read);
        self.add("fauna.web.publish.unset", read);
        self.add("fauna.web.publish.list", read);
        self.add(
            "fauna.web.domain.set",
            RpcKindMeta::new(false, Duration::from_secs(30)),
        );
        self.add("fauna.web.domain.get", read);
        self.add(
            "fauna.web.domain.delete",
            RpcKindMeta::new(false, Duration::from_secs(30)),
        );
        // Admin apex-actor designation (web-content-hosting.md § Admin apex
        // hosting) — idempotent singleton upsert/clear + a pure read.
        self.add("fauna.web.set_apex_actor", read);
        self.add("fauna.web.get_apex_actor", read);
        // Per-user subdomain-hosting opt-in (web-content-hosting.md § Routing /
        // Architectural rule 8) — idempotent caller-scoped upsert/clear + a read.
        self.add("fauna.web.set_subdomain_enabled", read);
        self.add("fauna.web.get_subdomain_enabled", read);
        // Web-paywall capability-URL mint (monetization.md § Pillar 2) — a
        // caller-scoped read-shaped mint (no state mutates; a replay just
        // re-serves the first token, which stays valid within its TTL).
        self.add("fauna.web.paywall.mint_token", read);
        // The owner client's complete-set declaration for one folder's SEALED
        // `web_files` projection — an idempotent, owner-scoped reconcile: replaying
        // it with the same set drops nothing the first call did not.
        self.add("fauna.web.files.prune_sealed", read);
    }

    /// Register the content-label kinds `fauna.labels.{attach,list}` — the
    /// WS-RPC migration of `POST /api/v1/labels` + `GET /api/v1/labels/{id}`
    /// (twins now deleted). Hub Track B8 of
    /// the WS-RPC-everywhere migration (tracked internally). Both `forbid_replay =
    /// false` @5 s: `list` is a pure read; `attach` is an idempotent UPSERT keyed
    /// on `(content_type, content_id, category, classifier_id)`, so a replayed
    /// attach overwrites the same rows (no duplication).
    pub fn register_labels_kinds(&mut self) {
        use std::time::Duration;
        let meta = RpcKindMeta::new(false, Duration::from_secs(5));
        self.add("fauna.labels.attach", meta);
        self.add("fauna.labels.list", meta);
    }

    // The four admin storage-migration kinds (B19: start/status/reset/
    // delete_source) were retired 2026-09-27 under the never-had-a-caller
    // exception (version-compatibility.md § Dimension 2): the blob backend is
    // artifact-set local disk, not an admin choice (`nest/common.md` § Blob
    // Store → Backend).

    /// Register the admin user-management cluster (Track C / C1 of
    /// the WS-RPC-everywhere migration; tracked internally), a behavior-preserving
    /// transport migration of the bearer-authed `admin::*` user/eviction HTTP
    /// handlers. Admin is a Fauna app (product invariant: nest configuration
    /// is set from clients); every kind gates `Admin` in
    /// `bridge_method_allowlist::is_permitted` (the
    /// `fauna.pending_actions.approve` B20 admin precedent). Wire types: `libs/fauna-protocol/src/admin.rs`.
    ///
    /// - Reads: `users.{list,get}`, `evictions.list` (≡ GET `/admin/api/users*`
    ///   + `/admin/api/evictions`) — pure reads.
    /// - Mutations: `users.{create,update,delete,clear_handle}` (≡ POST `users`,
    ///   PUT/DELETE `users/{id}`, DELETE `users/{id}/handle`).
    /// - Actions: `users.{evict,cancel_eviction,suspend}` (≡ POST
    ///   `users/{id}/{evict,cancel-eviction,suspend}`).
    ///
    /// All `forbid_replay = false` @5 s **except `invite_codes.create`** (see
    /// below), matching the account cluster's pending-action creators
    /// (`register_account_kinds`): the mutations are either idempotent (update
    /// / clear_handle / cancel) or guard-and-return on a second genuine run
    /// (create → conflict, evict → conflict). `delete` / `suspend` schedule a
    /// `pending_actions` row exactly as the twins did (no twin-side dedup
    /// either).
    ///
    /// ⚠️ This paragraph used to also cite "the per-connection idempotency
    /// cache replays the first reply on a recovered connection". That clause
    /// was deleted in the 71st pass because it is **false** and was carrying
    /// weight it never had: the cache lives on `RpcConnection`, and
    /// `request_auto_retry` waits for the *reconnect* before re-issuing, so it
    /// can never deduplicate a retry. Only the handler-side guards named above
    /// make these kinds replay-safe. Do not re-add the cache as a justification
    /// for a `false` anywhere.
    ///
    /// `invite_codes.create` is `forbid_replay = true`: with an empty `code`
    /// the handler mints a fresh random one and inserts a row keyed on it, so a
    /// replay leaves a second independently redeemable admission credential
    /// (the `fauna.payments.claims.mint` shape).
    pub fn register_admin_kinds(&mut self) {
        use std::time::Duration;
        let meta = RpcKindMeta::new(false, Duration::from_secs(5));
        self.add("fauna.admin.users.list", meta);
        self.add("fauna.admin.users.get", meta);
        self.add("fauna.admin.users.create", meta);
        self.add("fauna.admin.users.update", meta);
        self.add("fauna.admin.users.delete", meta);
        self.add("fauna.admin.users.clear_handle", meta);
        self.add("fauna.admin.users.evict", meta);
        self.add("fauna.admin.users.cancel_eviction", meta);
        self.add("fauna.admin.users.suspend", meta);
        self.add("fauna.admin.evictions.list", meta);

        // C2 — admin-management cluster (tiers / invite codes / invite requests
        // / admins; ≡ `/admin/api/{tiers,invite-codes,invite-requests,admins}`).
        // Same `forbid_replay = false` @5 s as C1: the list/get reads + the
        // idempotent-or-guarded mutations (create → conflict on dup, update /
        // delete / deny guard-and-return, approve deletes the request so a
        // replay finds it gone → not_found) and the `admins.{add,remove}`
        // pending-action creators (no twin-side dedup either). Distinct from the
        // pre-identity public `fauna.account.invite_{request,code}.*` (Track A5).
        self.add("fauna.admin.tiers.list", meta);
        self.add("fauna.admin.tiers.create", meta);
        self.add("fauna.admin.tiers.update", meta);
        // Membership designation (monetization.md § Pillar 4) — the link making
        // one of the admin's own *subscription* tiers mean paid nest access.
        // WS-RPC-native (no HTTP twin); same replay posture as the rest of C2:
        // `set` is an upsert and `clear` guards-and-returns `not_found`, so a
        // replayed first reply is correct.
        self.add("fauna.admin.membership_tiers.list", meta);
        self.add("fauna.admin.membership_tiers.set", meta);
        self.add("fauna.admin.membership_tiers.clear", meta);
        self.add("fauna.admin.invite_codes.list", meta);
        self.add(
            "fauna.admin.invite_codes.create",
            RpcKindMeta::new(true, Duration::from_secs(5)),
        );
        self.add("fauna.admin.invite_codes.delete", meta);
        self.add("fauna.admin.invite_requests.list", meta);
        self.add("fauna.admin.invite_requests.approve", meta);
        self.add("fauna.admin.invite_requests.deny", meta);
        self.add("fauna.admin.admins.list", meta);
        self.add("fauna.admin.admins.add", meta);
        self.add("fauna.admin.admins.remove", meta);

        // The co-admin seed hand-off (`nest/box-recovery.md` § Mechanism, the
        // co-admin bullet) — the claim hand-off generalized from the claiming
        // admin to any current roster admin. Ordinary `forbid_replay = false`
        // @5 s: a pure read of the nest's own signing key, no state changes.
        self.add("fauna.admin.deployment_seed.get", meta);

        // The deployment-seed rotation ceremony (`nest/box-recovery.md`
        // § Deployment-seed rotation). `forbid_replay = false` on purpose,
        // unlike most mutations: the client mints its successor seed *before*
        // dispatch, so a replay carries the seed the box already adopted and
        // lands on the handler's idempotent `already_rotated` ack. Forbidding
        // the replay would turn the one retry the ceremony is designed for into
        // an error. The timeout is generous because the transaction re-keys
        // every nest-internal KEK satellite (step 4) — deployment-scale rows,
        // but AEAD work per row.
        self.add(
            "fauna.admin.deployment_seed.rotate",
            RpcKindMeta::new(false, Duration::from_secs(60)),
        );

        // C3 — stats / audit / ops (≡ `/admin/api/{stats,status,audit,
        // audit/integrity,cluster/status,gc,worker/status,pending-actions}`).
        // Same `forbid_replay = false` @5 s: the reads are pure, and `gc` is the
        // one action — it is idempotent on a replay (a re-run after the first GC
        // finds the already-collected blobs gone → a zero/near-zero delta, no
        // twin-side dedup either). The cross-actor `pending_actions.list` is
        // distinct from B20's user-scoped `fauna.pending_actions.list`.
        self.add("fauna.admin.stats", meta);
        self.add("fauna.admin.status", meta);
        self.add("fauna.admin.audit.list", meta);
        self.add("fauna.admin.audit.integrity", meta);
        self.add("fauna.admin.cluster.status", meta);
        self.add("fauna.admin.gc", meta);
        self.add("fauna.admin.worker.status", meta);
        self.add("fauna.admin.pending_actions.list", meta);

        // The deployment's declared region (`region-blocking.md` § Region
        // determination — *declared, never detected*). Same `forbid_replay =
        // false` @5 s as the rest of this block: `get` is a pure read, and `set`
        // is an idempotent whole-value replace of one singleton row, so a replay
        // lands the same declaration a second time. Nothing accumulates.
        //
        // There is deliberately no `fauna.admin.region.policy.*` kind: the
        // region's *policy* is the authority's, published through the sanctioned
        // channel, and region-blocking.md invariant 5 puts it out of the admin's
        // reach. The admin declares the situs; that is all.
        self.add("fauna.admin.region.get", meta);
        self.add("fauna.admin.region.set", meta);

        // The admin's web-app origin choice — what this nest's `/app` answers
        // (`web-content-hosting.md` § Same-origin security model → *The
        // nest-served `/app/` and the central origin*). Same `forbid_replay =
        // false` @5 s: `get` is a pure read, `set` an idempotent whole-value
        // replace of one singleton row.
        self.add("fauna.admin.web_app_origin.get", meta);
        self.add("fauna.admin.web_app_origin.set", meta);

        // C5 — folders / services (≡ `/admin/api/{folders*,services*}`). Same
        // `forbid_replay = false` @5 s: the folders/services reads are pure; the
        // folders `create`/`add_member` writes are idempotent or guard-and-return
        // (create → conflict on a duplicate name, `add_member`
        // `INSERT OR REPLACE`); the former `add_destination` died with the
        // phantom folder_destinations rail (2026-08-18);
        // `services.update` is an idempotent flag set. The admin
        // `fauna.admin.folders.*` are distinct from the user `fauna.folders.*`
        // (B14) cluster. The `fauna.admin.wireguard.{status,peers,keygen}` kinds
        // that shared this bucket were removed 2026-08-23 with the WireGuard
        // stack (user population ruling — version-compatibility.md § Dim 2).
        self.add("fauna.admin.folders.create", meta);
        self.add("fauna.admin.folders.get", meta);
        self.add("fauna.admin.folders.add_member", meta);
        self.add("fauna.admin.services.list", meta);
        self.add("fauna.admin.services.update", meta);

        // Factory reset — `forbid_replay = false` @5 s like the rest. A replay
        // is harmless: the handler stages a marker carrying the (same) claim
        // code and the per-connection idempotency cache returns the first reply;
        // the process exits exactly once. Destructive restart-wipe — see
        // `admin.rs::FactoryResetRequest` + `factory_reset.rs`.
        self.add("fauna.admin.factory_reset", meta);

        // Client-set `[nest]`-policy knobs (bool toggles + the nullable-int
        // storage cap + the string-list CORS allow-list) — `forbid_replay = false`
        // @5 s like the rest (re-setting the same value is idempotent; the
        // per-connection idempotency cache replays the first reply on recovery).
        // The authed Admin-class twins of `fauna.bridges.set_mail_enabled`; see
        // `node_policy_handlers` + `fauna_protocol::node_policy`.
        // `set_require_registration` was RETIRED 2026-07-12 (its posture folded
        // into `fauna.admin.set_registration_mode`) and left the wire 2026-09-24
        // with the compat-remnant sweep — it never had a client caller.
        self.add("fauna.admin.set_registration_mode", meta);
        self.add("fauna.admin.set_subhandles", meta);
        // The "accept only signups carrying app age verification" gate
        // (family-safety.md § The account age band; public-mode.md § Age at
        // registration). Same bool-toggle shape as `set_subhandles`.
        self.add("fauna.admin.set_age_verification_required", meta);
        self.add("fauna.admin.set_max_storage_bytes", meta);
        self.add("fauna.admin.set_cors_origins", meta);
        // The admin's chosen client-facing API serving port (the nest's own HTTPS
        // listener). Same Admin-class `@5 s` shape; apply-on-restart (the nest
        // cannot hot-rebind its own listener). See `node_policy` + `nest/common.md`
        // § Serving ports.
        self.add("fauna.admin.set_serving_port", meta);

        // Host-OS maintenance: the admin's "restart now" affordance for an
        // onboarded VPS host (writes a flag the host reboot-coordinator picks
        // up). Same Admin-class `@5 s` shape; idempotent (`forbid_replay = false`
        // — re-requesting is harmless, the coordinator consumes the flag once).
        // See `host_maintenance.rs` + `installers/vps.md` § Host OS Maintenance.
        self.add("fauna.admin.request_host_restart", meta);

        // C4 — pairings RETIRED (per-user-pairing design, 2026-05-25): the
        // admin `fauna.admin.pairings.{list,approve}` kinds are gone; pairing is
        // authorized/revoked by the user via `fauna.pair.{add,revoke}` and the
        // admin's only control is the nest-level `pairing` service knob.

        // Lifted from the nest router (see `register_all_kinds`).
        self.add(
            "fauna.admin.logs",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
    }

    /// Register the **app-facing** region kind `fauna.region.artifact.get` —
    /// the relay an app fetches its declared region's published artifact
    /// through (`../../docs/goal/behavior/region-blocking.md` § The content
    /// plane → *How an app obtains its region's policy*).
    ///
    /// The admin-side region kinds live in [`Self::register_admin_kinds`] and
    /// declare the deployment's situs; this one serves any app its OWN region,
    /// so it rides the bearer connection at class `User`
    /// (`bridge_method_allowlist::is_permitted`). There is deliberately no
    /// sibling that submits an artifact (region-blocking.md invariant 5).
    ///
    /// `forbid_replay = false` @5 s: a pure read of the nest's cache. A first
    /// ask for a pair records the demand and schedules a refill in the
    /// background — it never waits on the log — so replaying it records the
    /// same demand twice and reads the same row.
    pub fn register_region_kinds(&mut self) {
        use std::time::Duration;
        self.add(
            "fauna.region.artifact.get",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
    }

    /// Register the push-subscription-management surface
    /// `fauna.push.{vapid_key,subscribe,unsubscribe}` — Track B22 of
    /// the WS-RPC-everywhere migration (tracked internally), a behavior-preserving transport
    /// migration of the three push-management HTTP routes (`push_routes`). Ride
    /// the **bearer** connection (the no-auth HTTP `vapid-key` migrates onto the
    /// authenticated connection — push subscription is inherently post-login, so
    /// there is no pre-identity caller); gate `User | Admin` (`subscribe` /
    /// `unsubscribe` are actor-scoped device management, and an admin has devices
    /// and must reach the VAPID key) — enforced in
    /// `bridge_method_allowlist::is_permitted`.
    ///
    /// Replay semantics — all `forbid_replay = false` @5 s:
    /// - `vapid_key` is a pure read.
    /// - `subscribe` is an idempotent upsert (the twin's "upserting the same
    ///   (actor_id, device_id) pair is safe").
    /// - `unsubscribe` is an idempotent delete (a replay finds no row → still ok).
    /// - `presence` (ruled 2026-09-26) tags the calling connection with the
    ///   device it serves; a replay re-sets the same tag, so it is idempotent.
    ///   Same gate — it can only suppress or receive the connection actor's own
    ///   push, which `unsubscribe` already lets that actor do
    ///   (`apps/common.md` § Registration → *Every connection announces*).
    pub fn register_push_kinds(&mut self) {
        use std::time::Duration;
        let read = RpcKindMeta::new(false, Duration::from_secs(5));
        self.add("fauna.push.vapid_key", read);
        self.add("fauna.push.subscribe", read);
        self.add("fauna.push.unsubscribe", read);
        self.add("fauna.push.presence", read);
    }

    /// Register the session-management surface
    /// `fauna.sessions.{list,revoke,revoke_all,lockout}` — Track B2 of
    /// the WS-RPC-everywhere migration (tracked internally), a behavior-preserving
    /// transport migration of the bearer-authed session routes
    /// (`session_routes::{list_sessions, revoke_session, revoke_all_sessions}`)
    /// plus an authed variant of the emergency lockout. Ride the **bearer**
    /// connection; gate `User | Admin` (an admin manages their own sessions) —
    /// enforced in `bridge_method_allowlist::is_permitted`. The no-token
    /// Ed25519 `POST /api/v1/account/lockout` stays HTTP (recovery channel,
    /// `api-layers.md` § Sessions) and is NOT migrated.
    ///
    /// Replay semantics — all `forbid_replay = false` @5 s:
    /// - `list` is a pure read.
    /// - `revoke` / `revoke_all` are idempotent (a replay re-revokes the same
    ///   already-gone session(s) → still ok).
    /// - `lockout` is idempotent (re-revoking + re-stamping `locked_until` to
    ///   the same clamped window is a no-op effect).
    pub fn register_sessions_kinds(&mut self) {
        use std::time::Duration;
        let read = RpcKindMeta::new(false, Duration::from_secs(5));
        self.add("fauna.sessions.list", read);
        self.add("fauna.sessions.revoke", read);
        self.add("fauna.sessions.revoke_all", read);
        self.add("fauna.sessions.lockout", read);
    }

    /// Register the pre-identity in-band invite kinds
    /// `fauna.account.invite_request.{submit,status,cancel}` +
    /// `fauna.account.invite_code.verify` — the WS-RPC migration of the public
    /// invite flow (`POST /api/v1/invite-requests`, `GET …/{actor}/status`,
    /// `DELETE …/{actor}`, `POST /api/v1/invite-code/verify`). Like the
    /// auth-bootstrap + discovery + registration + claim kinds they run on the
    /// **anonymous** WS connection (no bearer) per `transport.md` § Pre-identity
    /// (anonymous) connection; Track A5 of
    /// the WS-RPC-everywhere migration (tracked internally).
    ///
    /// Replay semantics:
    /// - `invite_request.submit` is a write @30 s (matching `account.register`):
    ///   the existing-row / actor-registered / handle-taken checks make a replay
    ///   return the corresponding conflict, and the idempotency cache replays
    ///   the first reply on a recovered connection.
    /// - `invite_request.status` and `invite_code.verify` are pure reads @5 s.
    /// - `invite_request.cancel` is an idempotent delete @5 s (a replay finds no
    ///   row → `invite_request_not_found`).
    pub fn register_invite_kinds(&mut self) {
        use std::time::Duration;
        let read = RpcKindMeta::new(false, Duration::from_secs(5));
        self.add(
            "fauna.account.invite_request.submit",
            RpcKindMeta::new(false, Duration::from_secs(30)),
        );
        self.add("fauna.account.invite_request.status", read);
        self.add("fauna.account.invite_request.cancel", read);
        self.add("fauna.account.invite_code.verify", read);
    }

    /// Register the pre-identity NAT-mode commit kind `fauna.setup.nat_mode`
    /// — the admin-signed set that makes the nest's public/private NAT axis
    /// client-set (`2026-06-15-nest-nat-mode-client-set-design.md`; the
    /// `nat_mode_choice` onboarding step + the admin-panel toggle). (Its
    /// byte-twin `fauna.setup.storage_mode`, the retired storage-mode axis's
    /// validate-then-discard shim, left the wire
    /// 2026-09-24 with the compat-remnant sweep.)
    ///
    /// `forbid_replay = false` @5 s. The set is **mutable** (no
    /// `mode_conflict`): a replayed or repeated set is idempotent, and the
    /// idempotency cache replays the first reply on a recovered connection.
    pub fn register_nat_mode_kind(&mut self) {
        use std::time::Duration;
        self.add(
            "fauna.setup.nat_mode",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
    }

    /// Register the user-facing `fauna.posts.{create,get,interact}` kinds
    /// — the create / get / interact plane end-user clients call from the
    /// feed + post-composer + interaction affordances. T1 of
    /// the WS-RPC-everywhere migration (tracked internally).
    ///
    /// Replay semantics:
    /// - `posts.get` is a pure read — replay-safe at 5 s.
    /// - `posts.create` is replay-safe at 30 s. `post_id = blake3(body)`
    ///   is content-addressed: `CacheDb::put_post` is idempotent on the
    ///   content-addressed PK, `spawn_replicate_post` stores the bytes
    ///   under the same content id (idempotent), and the bluesky
    ///   write-through re-publishes the byte-identical content-addressed
    ///   post. A replay of the identical body re-derives the same
    ///   `post_id` and re-runs the idempotent pipeline with no
    ///   double-fire. 30 s deadline covers ingest verify +
    ///   classify/obligation + insert + spawn fan-out.
    /// - `posts.delete` is replay-safe at 10 s: the end state is idempotent
    ///   (`tombstone_by_cid` + the projection removal are both
    ///   already-gone-tolerant, and the reference-counter reversal dedups
    ///   through the `engagement_events` log), and the reply's `deleted`
    ///   flag distinguishes newly-removed from already-gone without erroring.
    /// - `posts.interact` **forbids replay** at 5 s. Unlike the other two,
    ///   the `like` action increments a non-content-addressed counter
    ///   (`CacheDb::increment_engagement_count`, which also recomputes the
    ///   `engagement` scalar) and inserts a notification — a replay would
    ///   double-count the like and re-notify. The
    ///   `reply`/`repost`/`quote` actions are pure (they only echo target
    ///   info) and the bridged-protocol actions are origin-protocol-
    ///   specific, but per-action replay metadata isn't expressible on a
    ///   single kind, so the kind takes the conservative
    ///   `forbid_replay=true` to protect the non-idempotent like path.
    pub fn register_posts_kinds(&mut self) {
        use std::time::Duration;
        let read = RpcKindMeta::new(false, Duration::from_secs(5));
        self.add("fauna.posts.get", read);
        // list: the self-scoped author enumeration — a pure paged read, so the
        // same class as `get` (replay-safe @5s). Self-scoping is structural
        // (the request carries no `actor_id`), not a handler check.
        self.add(
            "fauna.posts.list",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.posts.create",
            RpcKindMeta::new(false, Duration::from_secs(30)),
        );
        self.add(
            "fauna.posts.delete",
            RpcKindMeta::new(false, Duration::from_secs(10)),
        );
        self.add(
            "fauna.posts.interact",
            RpcKindMeta::new(true, Duration::from_secs(5)),
        );
        // room_labels: a community room's verdicts for room-restricted posts,
        // read by a live floor member — a pure read of a derived view the
        // nest already holds, the `room.search` class (replay-safe @5s).
        self.add(
            "fauna.posts.room_labels",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        // The verdict read's relayed twin, for a member homed on another
        // nest. Pure read like the door it forwards to, so replay-safe; the
        // longer deadline is the peer round trip (the
        // `room.list_roster_remote` / `generations_remote` posture).
        self.add(
            "fauna.posts.room_labels_remote",
            RpcKindMeta::new(false, Duration::from_secs(30)),
        );
    }

    /// Register the user-facing profile-fetch kind (`fauna.profile.get`) —
    /// the read half of the Profile-page shell's one net-new shared-Rust
    /// dependency (`docs/goal/ui/profile.md` § Where logic lives). The
    /// singular `fauna.profile.*` namespace, alongside the account surface's
    /// `fauna.profile.handle.change` (`register_account_kinds`).
    ///
    /// `get` is a pure read (`forbid_replay = false` @5 s), like
    /// `fauna.posts.get` / `fauna.account.get`.
    pub fn register_profile_kinds(&mut self) {
        use std::time::Duration;
        let read = RpcKindMeta::new(false, Duration::from_secs(5));
        self.add("fauna.profile.get", read);
        // set is a client-signed own-write (`profile.md` § Where logic lives →
        // *Profile publish/edit*). Replay-safe (`forbid_replay = false`) @10s:
        // the content row id is `blake3(body)` and the insert is `INSERT OR
        // REPLACE`, so a byte-identical re-submit is idempotent (the same row
        // re-written, the same keep-latest prune), like `fauna.posts.create`.
        self.add(
            "fauna.profile.set",
            RpcKindMeta::new(false, Duration::from_secs(10)),
        );
    }

    /// Register the user-facing notifications cluster
    /// (`fauna.notifications.{list,mark_read,count}`) — T1 of
    /// the WS-RPC-everywhere migration (tracked internally). A behavior-preserving
    /// transport migration of the `/api/v1/notifications/*` HTTP routes.
    ///
    /// Replay semantics — every kind here is `forbid_replay = false` @5 s:
    ///
    /// - `list` and `count` are pure reads, trivially replay-safe.
    /// - `mark_read` is an idempotent upsert: it flips `is_read = 1` on rows
    ///   created at/before `up_to`. A replay re-runs the same `UPDATE …
    ///   WHERE is_read = 0` and converges (a second pass marks nothing new),
    ///   so it cannot double-count — unlike `fauna.posts.interact` whose
    ///   `like` increments a non-content-addressed score. 5 s window.
    /// - `dismiss` and `clear` (2026-09-24, `behavior/notifications.md`
    ///   § Retention rule 2 — the user's own deletes, the only ones short of
    ///   the account going) are idempotent deletes: a replay finds the rows
    ///   gone and deletes nothing new. 5 s window.
    pub fn register_notifications_kinds(&mut self) {
        use std::time::Duration;
        let read = RpcKindMeta::new(false, Duration::from_secs(5));
        self.add("fauna.notifications.list", read);
        self.add(
            "fauna.notifications.mark_read",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.notifications.count",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.notifications.dismiss",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.notifications.clear",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
    }

    /// Register the user-facing connection-management cluster — knocks
    /// (`fauna.knocks.{list,accept,block,dismiss}`), the contact roster
    /// (`fauna.contacts.{list,confirm}`), and the inbox-acceptance policy
    /// (`fauna.inbox.mode.{get,set}`). One fn spanning three top namespaces
    /// for the single "connection-management" feature, exactly as
    /// `register_feed_kinds` spans `fauna.feed.*` + the `contributors`
    /// sub-namespace. T2 of the WS-RPC-everywhere migration (tracked internally) — a
    /// behavior-preserving transport migration of the `knock_routes.rs` HTTP
    /// routes (`/api/v1/{knocks,contacts,inbox-mode}/*`).
    ///
    /// Replay semantics — all seven kinds are `forbid_replay = false` @5 s:
    ///
    /// - The three reads (`knocks.list`, `contacts.list`, `inbox.mode.get`)
    ///   are pure reads, trivially replay-safe.
    /// - The five writes are all idempotent and converge on re-issue:
    ///   `knocks.accept` upserts the contact to `accepted` + deletes the
    ///   knock; `knocks.block` upserts to `blocked` + deletes the knock and
    ///   its doorbell (the Bayesian auto-train it once also fired is gone —
    ///   `mail-spam.md` § Implicit signals are forbidden — so a replay is an
    ///   idempotent own-row rewrite); `knocks.unblock` clears a `blocked` edge
    ///   (a guarded `DELETE` — replaying it on the now-absent edge is a no-op);
    ///   `knocks.dismiss` deletes the knock + deletes the contact;
    ///   `contacts.confirm` promotes an `accepted` contact to `confirmed` (a
    ///   no-op once confirmed); `inbox.mode.set` overwrites the mode. None
    ///   increments a non-content-addressed score (contrast
    ///   `fauna.posts.interact`), so none forbids replay.
    pub fn register_contacts_kinds(&mut self) {
        use std::time::Duration;
        let entry = RpcKindMeta::new(false, Duration::from_secs(5));
        // fauna.knocks.*
        self.add("fauna.knocks.list", entry);
        self.add("fauna.knocks.accept", entry);
        self.add("fauna.knocks.block", entry);
        self.add("fauna.knocks.unblock", entry);
        self.add("fauna.knocks.dismiss", entry);
        // fauna.contacts.*
        self.add("fauna.contacts.list", entry);
        self.add("fauna.contacts.status", entry);
        self.add("fauna.contacts.confirm", entry);
        // fauna.inbox.mode.*
        self.add("fauna.inbox.mode.get", entry);
        self.add("fauna.inbox.mode.set", entry);
    }

    /// Register the user-facing feed cluster (`fauna.feed.*`) — T2 of
    /// the WS-RPC-everywhere migration (tracked internally). A behavior-preserving
    /// transport migration of the `/api/v1/feeds/*` HTTP routes.
    ///
    /// Replay semantics — every kind here is `forbid_replay = false` @5 s:
    ///
    /// - The five reads (`list`, `get`, `posts`, `local.posts`,
    ///   `contributors.list`) are pure reads, trivially replay-safe.
    /// - The mutations (`create`, `update`, `delete`, `contributors.grant`,
    ///   `contributors.revoke`) are owner-keyed and idempotent-enough to make
    ///   the conservative 5 s replay window acceptable — unlike
    ///   `fauna.posts.interact` (whose `like` increments a non-content-
    ///   addressed score), none of these double-count on a replay. `create`
    ///   is the loosest case: it inserts a row with a fresh random `feed_id`,
    ///   so a replay would create a duplicate feed row; we keep it
    ///   `forbid_replay = false` because feed creation is owner-scoped and a
    ///   rare interactive action where the 5 s replay window is a non-issue
    ///   in practice (a dropped connection within 5 s of a create is the only
    ///   trigger, and a duplicate feed is harmlessly user-deletable —
    ///   contrast the score-increment hazard `posts.interact` guards
    ///   against). `update`/`delete`/`grant`/`revoke` are idempotent
    ///   overwrites/removals on the owner-keyed `(feed_id, owner)` /
    ///   `(feed_id, nest_url, author_id)` keys.
    pub fn register_feed_kinds(&mut self) {
        use std::time::Duration;
        let read = RpcKindMeta::new(false, Duration::from_secs(5));
        self.add("fauna.feed.list", read);
        self.add("fauna.feed.get", read);
        self.add("fauna.feed.posts", read);
        self.add("fauna.feed.local.posts", read);
        self.add("fauna.feed.trending.posts", read);
        self.add("fauna.feed.contributors.list", read);
        let mutation = RpcKindMeta::new(false, Duration::from_secs(5));
        self.add("fauna.feed.create", mutation);
        self.add("fauna.feed.update", mutation);
        self.add("fauna.feed.delete", mutation);
        self.add("fauna.feed.contributors.grant", mutation);
        self.add("fauna.feed.contributors.revoke", mutation);
        // The user's global factor set (frame § Composition): a pure read +
        // an idempotent whole-set overwrite — both replay-safe at 5 s.
        self.add("fauna.feed.factors.get", read);
        self.add("fauna.feed.factors.set", mutation);
    }

    /// Register the sealed personalization-model kinds
    /// (`fauna.personalization.model.{fetch,put,delete}` —
    /// `docs/goal/behavior/topic-factors.md` § Wire & registry; payload types
    /// in `personalization.rs`).
    ///
    /// Replay semantics — all three are `forbid_replay = false` @5 s:
    /// `fetch` is a pure read; `put` is an idempotent whole-row overwrite
    /// keyed on the caller's `(actor, factor)` (same bytes twice → same
    /// state); `delete` is an idempotent removal. Blobs are capped at
    /// 512 KiB (nest-enforced), comfortably inside the 5 s deadline.
    pub fn register_personalization_kinds(&mut self) {
        use std::time::Duration;
        let entry = RpcKindMeta::new(false, Duration::from_secs(5));
        self.add(crate::personalization::KIND_MODEL_FETCH, entry);
        self.add(crate::personalization::KIND_MODEL_PUT, entry);
        self.add(crate::personalization::KIND_MODEL_DELETE, entry);
    }

    /// Register the full-text search kind (`fauna.search.query`). A pure
    /// read — replay-safe at 5 s. Scoped as part of the WS-RPC search
    /// migration (tracked internally).
    pub fn register_search_kinds(&mut self) {
        use std::time::Duration;
        self.add(
            "fauna.search.query",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
    }

    /// Register the cross-location segment-backup kinds Plan 5 ships.
    /// Both kinds are replay-safe (the list is idempotent; the push is
    /// informational and dedup-friendly via segment_id). 5 s deadline
    /// matches the existing `fetch` shape — listing is metadata-only.
    pub fn register_segments_kinds(&mut self) {
        use std::time::Duration;
        let read = RpcKindMeta::new(false, Duration::from_secs(5));
        self.add("fauna.segments.list", read);
        // Push event — registered for telemetry visibility per the
        // existing fauna.protocol.resync_required precedent. Decoded
        // by `PushEvent::from_push` on the push channel.
        self.add("fauna.segments.changed", read);

        // Lifted from the nest router (see `register_all_kinds`).
        self.add(
            "fauna.segments.compact",
            RpcKindMeta::new(false, Duration::from_secs(30)),
        );
        // The counter floor: monotonic and idempotent, a metadata write.
        self.add(
            crate::segments::KIND_SEGMENTS_COUNTER_FLOOR,
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
    }

    /// Register the `__index` rail's four kinds (`crate::content_index`) — the
    /// user plane and its MDA bridge-plane twins.
    ///
    /// Values lifted verbatim from the nest's own registration in
    /// `bins/fauna-nest/src/content_index_handlers.rs::
    /// register_content_index_handlers`. All four are replay-safe: `record`
    /// converges on the same live row for a repeated `(path, blob_hash)`, and
    /// `list` is a pure read.
    pub fn register_content_index_kinds(&mut self) {
        use std::time::Duration;
        self.add(
            crate::content_index::KIND_RECORD,
            RpcKindMeta::new(false, Duration::from_secs(10)),
        );
        self.add(
            crate::content_index::KIND_LIST,
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        // `fauna.bridges.index_record` was retired 2026-08-10 with the MDA's
        // build half (`content-index.md` § Where the index is built — the
        // carrier ruling): the MDA is query-only now, so the write kind is
        // gone from router and registry together. `index_list` stays — the
        // read half is the leg's whole remaining job.
        self.add(
            crate::content_index::KIND_BRIDGE_LIST,
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
    }

    /// Register the `fauna.bridges.*` kinds — the mail / MDA / mail-admin operational cluster (the Go bridge's own
    /// call surface plus the admin-app mail settings pages).
    ///
    /// Values are lifted verbatim from the nest's own `RpcRouter` registration
    /// for each kind, which is where this metadata has been declared (and
    /// deliberately chosen) all along; the two tables are held in lockstep by
    /// the parity test in `bins/fauna-nest/src/rpc_router.rs`.
    pub fn register_bridges_mail_kinds(&mut self) {
        use std::time::Duration;

        self.add(
            "fauna.bridges.abort_primary_domain_rename",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.add_list_member",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.add_local_domain",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.approve_pending_bridge",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.atproto.delete_presence",
            RpcKindMeta::new(false, Duration::from_secs(30)),
        );
        self.add(
            "fauna.bridges.atproto.fetch_identities",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.atproto.fetch_identity_key_blob",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.atproto.fetch_issuer_jwks",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.atproto.fetch_preferences",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.atproto.fetch_profile",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.atproto.fetch_public_posts",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.atproto.fetch_session_secret_blob",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.atproto.record_blob",
            RpcKindMeta::new(false, Duration::from_secs(30)),
        );
        self.add(
            "fauna.bridges.atproto.record_minted_identity",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.atproto.record_tombstone",
            RpcKindMeta::new(false, Duration::from_secs(30)),
        );
        self.add(
            "fauna.bridges.atproto.request_tombstone",
            RpcKindMeta::new(false, Duration::from_secs(30)),
        );
        self.add(
            "fauna.bridges.atproto.store_preferences",
            RpcKindMeta::new(false, Duration::from_secs(30)),
        );
        self.add(
            "fauna.bridges.batch_import_list_members",
            RpcKindMeta::new(false, Duration::from_secs(15)),
        );
        self.add(
            "fauna.bridges.blocklist_self_check_run",
            RpcKindMeta::new(false, Duration::from_secs(30)),
        );
        self.add(
            "fauna.bridges.check_greylist",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.complete_primary_domain_rename",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.create_account_alias",
            RpcKindMeta::new(true, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.create_account_list",
            RpcKindMeta::new(true, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.create_forwarder",
            RpcKindMeta::new(true, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.create_mailbox",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.decode_srs_bounce",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.delete_account_alias",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.delete_account_list",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.delete_forwarder",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.delete_mailbox",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.deliver_sealed_scheduling",
            RpcKindMeta::new(true, Duration::from_secs(30)),
        );
        self.add(
            "fauna.bridges.enable_account_alias",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.enqueue_outbound_mail",
            RpcKindMeta::new(false, Duration::from_secs(10)),
        );
        self.add(
            "fauna.bridges.extend_primary_domain_rename_grace",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.fetch_mta_sts_policy",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.fetch_outbound_due",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.fetch_recipient_filters",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.fetch_recipient_forward_config",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.fetch_recipient_index_key",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.fetch_spam_model",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.fetch_tlsa",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.resolve_mx",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.force_rotate_dkim",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.forward_message",
            RpcKindMeta::new(false, Duration::from_secs(10)),
        );
        self.add(
            "fauna.bridges.generate_disposable_alias",
            RpcKindMeta::new(true, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.get_alias_policy",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.get_caldav_port",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.get_forward_all_to",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.get_mail_config",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.get_mail_serving_enabled",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.get_primary_domain_rename_status",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.get_quota",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.get_spam_baseline_state",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.get_spam_scoring_policy",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.import_account_aliases",
            RpcKindMeta::new(true, Duration::from_secs(10)),
        );
        self.add(
            "fauna.bridges.list_account_alias_hits",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.list_account_aliases",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.list_account_lists",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.list_blocklist_self_check_history",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.list_deliverability_diagnostic_runs",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.list_forwarders",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.list_list_members",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.list_list_send_history",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.list_local_domains",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.list_pending_bridges",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.list_primary_domain_renames",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.list_spam_training_history",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.mail_health",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.mark_outbound_bounced",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.mark_outbound_delivered",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.mark_outbound_failed",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.mint_bulk_byte_token",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.outbound_warmup_reset",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.outbound_warmup_status",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.provision_recipient_mls_pubkey",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.provision_self_signed_cert",
            RpcKindMeta::new(false, Duration::from_secs(30)),
        );
        self.add(
            "fauna.bridges.publish_spam_baseline",
            RpcKindMeta::new(false, Duration::from_secs(30)),
        );
        self.add(
            "fauna.bridges.put_alias_policy",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.put_auth_policy",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.put_imap_policy",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.put_outbound_policy",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.put_spam_model",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.put_spam_policy",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.put_submission_policy",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.reject_pending_bridge",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.remove_local_domain",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.rename_mailbox",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.report_log_events",
            RpcKindMeta::new(false, Duration::from_secs(2)),
        );
        self.add(
            "fauna.bridges.report_rejected_scan",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.report_tls_attempt",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.request_enrollment",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.reset_spam_model",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.resolve_recipient",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.restore_local_domain",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.restore_real_tls_cert",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.resubscribe_list_member",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.revoke_account_alias",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.revoke_service_user",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.rotate_list_unsubscribe_secret",
            RpcKindMeta::new(true, Duration::from_secs(30)),
        );
        self.add(
            "fauna.bridges.rotate_srs_secret",
            RpcKindMeta::new(true, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.run_deliverability_diagnostics",
            RpcKindMeta::new(false, Duration::from_secs(45)),
        );
        self.add(
            "fauna.bridges.search_messages",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.send_auto_reply",
            RpcKindMeta::new(false, Duration::from_secs(10)),
        );
        self.add(
            "fauna.bridges.send_list_message",
            RpcKindMeta::new(true, Duration::from_secs(60)),
        );
        self.add(
            "fauna.bridges.set_auto_enable_mail_for_new_users",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.set_baseline_contribution",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.set_caldav_enabled",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.set_caldav_port",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.set_carddav_enabled",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.set_catch_all_actor",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.set_dkim_rotation_days",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.set_forward_all_to",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.set_mail_enabled",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.set_mail_serving_enabled",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.set_role_address",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        // The per-account delivery-time spam-threshold override:
        // a read and an idempotent full-overwrite, both replay-safe @5 s, matching
        // the meta the router registers.
        self.add(
            "fauna.bridges.get_spam_threshold_override",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.set_spam_threshold_override",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        // The per-account hourly forward cap: a read and an idempotent
        // full-overwrite, both replay-safe @5 s, matching the router's meta.
        self.add(
            "fauna.bridges.get_forward_per_hour",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.set_forward_per_hour",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.set_webdav_enabled",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.start_primary_domain_rename",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.subscribe_mailbox",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.subscribe_mailbox_state",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.unsubscribe_list_member",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.unsubscribe_mailbox",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.update_account_alias",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.update_account_list",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.update_local_domain_config",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.webdav_list_folders",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.webdav_list_files",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.webdav_quota",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.webdav_admit_principal",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.webdav_record_change",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.bridges.whoami",
            RpcKindMeta::new(false, Duration::from_secs(2)),
        );
    }

    /// Register the `fauna.subscriptions.*` kinds — paid-subscription and unlock kinds.
    ///
    /// Values are lifted verbatim from the nest's own `RpcRouter` registration
    /// for each kind, which is where this metadata has been declared (and
    /// deliberately chosen) all along; the two tables are held in lockstep by
    /// the parity test in `bins/fauna-nest/src/rpc_router.rs`.
    pub fn register_subscriptions_kinds(&mut self) {
        use std::time::Duration;

        self.add(
            "fauna.subscriptions.delegate.upload",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.subscriptions.key_blob.get",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.subscriptions.key_blob.rotate",
            RpcKindMeta::new(false, Duration::from_secs(15)),
        );
        self.add(
            "fauna.subscriptions.mine.list",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.subscriptions.offers.list",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.subscriptions.post_unlock.get",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.subscriptions.requests.approve",
            RpcKindMeta::new(false, Duration::from_secs(15)),
        );
        self.add(
            "fauna.subscriptions.requests.list",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.subscriptions.requests.reject",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.subscriptions.status.get",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.subscriptions.subscribe",
            RpcKindMeta::new(false, Duration::from_secs(15)),
        );
        self.add(
            "fauna.subscriptions.subscribers.list",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.subscriptions.subscribers.remove",
            RpcKindMeta::new(false, Duration::from_secs(15)),
        );
        self.add(
            "fauna.subscriptions.tiers.clear_field",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.subscriptions.tiers.create",
            RpcKindMeta::new(false, Duration::from_secs(15)),
        );
        self.add(
            "fauna.subscriptions.tiers.delete",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.subscriptions.tiers.list",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.subscriptions.tiers.update",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.subscriptions.unsubscribe",
            RpcKindMeta::new(false, Duration::from_secs(15)),
        );
    }

    /// Register the `fauna.filesync.*` kinds — file-sync snapshot, placement and restore kinds.
    ///
    /// Values are lifted verbatim from the nest's own `RpcRouter` registration
    /// for each kind, which is where this metadata has been declared (and
    /// deliberately chosen) all along; the two tables are held in lockstep by
    /// the parity test in `bins/fauna-nest/src/rpc_router.rs`.
    pub fn register_filesync_kinds(&mut self) {
        use std::time::Duration;

        self.add(
            "fauna.filesync.snapshot.check",
            RpcKindMeta::new(false, Duration::from_secs(60)),
        );
        self.add(
            "fauna.filesync.snapshot.create_folder",
            RpcKindMeta::new(false, Duration::from_secs(30)),
        );
        self.add(
            "fauna.filesync.snapshot.create_message_kind",
            RpcKindMeta::new(false, Duration::from_secs(30)),
        );
        self.add(
            "fauna.filesync.snapshot.delete",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.filesync.snapshot.delete_immediate",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.filesync.snapshot.diff",
            RpcKindMeta::new(false, Duration::from_secs(30)),
        );
        self.add(
            "fauna.filesync.snapshot.get",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.filesync.snapshot.list",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.filesync.snapshot.stamp_labels",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.filesync.snapshot.list_restore_divergence",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.filesync.snapshot.list_restore_history",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.filesync.snapshot.prune",
            RpcKindMeta::new(false, Duration::from_secs(30)),
        );
        self.add(
            "fauna.filesync.snapshot.prune_set_policy",
            RpcKindMeta::new(false, Duration::from_secs(30)),
        );
        self.add(
            "fauna.filesync.snapshot.restore_message_kind",
            RpcKindMeta::new(false, Duration::from_secs(60)),
        );
        self.add(
            "fauna.filesync.snapshot.undelete",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
    }

    /// Register the `fauna.payments.*` kinds — payment-provider kinds.
    ///
    /// Values are lifted verbatim from the nest's own `RpcRouter` registration
    /// for each kind, which is where this metadata has been declared (and
    /// deliberately chosen) all along; the two tables are held in lockstep by
    /// the parity test in `bins/fauna-nest/src/rpc_router.rs`.
    ///
    /// Gated with the rest of the `payments` plane: the kind STRINGS are what
    /// an excised artifact is checked for (`dynamic-features.md` § What
    /// "completely compiled away" means, item 2 — no wire senders), so leaving
    /// this table ungated would leave `fauna.payments.*` in the binary.
    #[cfg(feature = "payments")]
    pub fn register_payments_kinds(&mut self) {
        use std::time::Duration;

        self.add(
            "fauna.payments.claims.list",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            // `forbid_replay: true` — minting is not idempotent (a fresh
            // random code + a new row per call), so a replay leaves a second
            // redeemable credential. Audited 2026-07-31; mirrored nest-side in
            // `payment_handlers::register_payment_handlers`, which carries the
            // full reasoning.
            "fauna.payments.claims.mint",
            RpcKindMeta::new(true, Duration::from_secs(5)),
        );
        self.add(
            "fauna.payments.claims.redeem",
            RpcKindMeta::new(false, Duration::from_secs(15)),
        );
        self.add(
            "fauna.payments.providers.list",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.payments.providers.remove",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.payments.providers.set",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
    }

    /// Register the `fauna.tips.*` kinds — the post-addressed tip attribution
    /// read (`docs/goal/behavior/monetization.md` § Tips).
    ///
    /// Its own family rather than a member of `fauna.subscriptions.*`,
    /// because a tip is deliberately **not** an entitlement: it names
    /// *(payee, post)* and grants nothing, so filing it under the
    /// subscription kinds would put the one value that must never reach the
    /// waist inside the waist's own namespace.
    ///
    /// Gated with the `payments` plane, same reason as
    /// [`Self::register_payments_kinds`].
    #[cfg(feature = "payments")]
    pub fn register_tips_kinds(&mut self) {
        use std::time::Duration;

        self.add(
            // `forbid_replay: false` — a bounded aggregate point read that
            // writes nothing, so a replay returns the same answer (modulo
            // tips arriving in between, which is ordinary read freshness, not
            // a double-apply).
            "fauna.tips.list",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
    }

    /// The controversial-class feature plane's transparency read
    /// (`dynamic-features.md` § Transparency & auditability).
    ///
    /// **Deliberately NOT gated on `payments`**, unlike the two families above:
    /// the plane gates three registry members, and its own read must answer for
    /// whichever of them a build ships. An excised-payments nest still has
    /// `p2p-share` to report on — and a client asking a nest that ships none of
    /// them should get an honest empty answer, not an unknown kind.
    pub fn register_features_kinds(&mut self) {
        use std::time::Duration;

        self.add(
            // `forbid_replay: false` — a pure read that writes nothing, so a
            // replay returns the same answer modulo ordinary read freshness.
            "fauna.features.status",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        // The two policy-update kinds (§ Wire & data shape). `forbid_replay:
        // false` for both, and the reason is a property of the operation rather
        // than an oversight: each is an **idempotent whole-document replace** at
        // one (tier, subject, feature) key, so replaying one lands the byte-identical
        // document a second time. Nothing accumulates — in particular neither
        // touches `feature_usage`, the one place in this plane where a repeat
        // would cost something, and which no policy write may ever decrement.
        self.add(
            "fauna.features.policy.update",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.features.self_limits.update",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        // The two authored-document reads — the editors' seed. Pure reads:
        // `forbid_replay: false`, as for `status`.
        self.add(
            "fauna.features.policy.get",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.features.self_limits.get",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
    }

    /// Register the `fauna.dns.*` kinds — DNS-record inspection kinds.
    ///
    /// Values are lifted verbatim from the nest's own `RpcRouter` registration
    /// for each kind, which is where this metadata has been declared (and
    /// deliberately chosen) all along; the two tables are held in lockstep by
    /// the parity test in `bins/fauna-nest/src/rpc_router.rs`.
    pub fn register_dns_kinds(&mut self) {
        use std::time::Duration;

        self.add(
            "fauna.dns.list_records",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.dns.set_host_address",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.dns.verify_records",
            RpcKindMeta::new(false, Duration::from_secs(15)),
        );
        // The DNS-01 propagation gate's readiness probe, run nest-side for the
        // web app (a browser has no raw DNS). One authoritative-NS round of UDP
        // queries — a 5 s per-NS timeout over a handful of servers — so a budget
        // above `verify_records`'s: this must not time out *before* the query
        // itself does, or the gate would read a slow NS set as "not visible".
        self.add(
            "fauna.dns.probe_txt_visible",
            RpcKindMeta::new(false, Duration::from_secs(30)),
        );
    }

    /// Register the `fauna.domain.*` kinds — the deployment's domain
    /// *registration* surface.
    ///
    /// Deliberately **not** in `fauna.dns.*`: that namespace is Admin-only by its
    /// own declaration (`domain_expiry.rs`'s owner doc puts every authenticated
    /// user in this feeder's audience), and a registration's lifecycle at the
    /// registrar is a different plane from the records published in a zone.
    pub fn register_domain_kinds(&mut self) {
        use std::time::Duration;

        self.add(
            "fauna.domain.expiry.get",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
    }

    /// Register the `fauna.pair.*` kinds — device-pairing kinds.
    ///
    /// Values are lifted verbatim from the nest's own `RpcRouter` registration
    /// for each kind, which is where this metadata has been declared (and
    /// deliberately chosen) all along; the two tables are held in lockstep by
    /// the parity test in `bins/fauna-nest/src/rpc_router.rs`.
    pub fn register_pair_kinds(&mut self) {
        use std::time::Duration;

        self.add(
            "fauna.pair.add",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.pair.list",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.pair.revoke",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        // The forward-queue actions (2026-09-23): owner-scoped UPDATE / DELETE
        // over the caller's own outbox rows, idempotent on replay.
        self.add(
            "fauna.pair.forward_retry",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.pair.forward_discard",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
    }

    /// Register the `fauna.drafts.*` kinds — post-draft kinds.
    ///
    /// Values are lifted verbatim from the nest's own `RpcRouter` registration
    /// for each kind, which is where this metadata has been declared (and
    /// deliberately chosen) all along; the two tables are held in lockstep by
    /// the parity test in `bins/fauna-nest/src/rpc_router.rs`.
    pub fn register_drafts_kinds(&mut self) {
        use std::time::Duration;

        self.add(
            "fauna.drafts.get",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.drafts.put",
            RpcKindMeta::new(false, Duration::from_secs(10)),
        );
    }

    /// Register the `fauna.mls.*` kinds — MLS key-material kinds.
    ///
    /// Values are lifted verbatim from the nest's own `RpcRouter` registration
    /// for each kind, which is where this metadata has been declared (and
    /// deliberately chosen) all along; the two tables are held in lockstep by
    /// the parity test in `bins/fauna-nest/src/rpc_router.rs`.
    pub fn register_mls_kinds(&mut self) {
        use std::time::Duration;

        self.add(
            "fauna.mls.get",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.mls.put",
            RpcKindMeta::new(false, Duration::from_secs(10)),
        );
    }

    /// Register the `fauna.oauth.*` kinds — the nest-held OAuth issuer key set's
    /// admin surface (`authorization-server.md` § The issuer).
    ///
    /// Both rotate arms forbid replay for `rotate_srs_secret`'s reason: each
    /// call mints a key, so a replayed frame must not silently rotate twice
    /// (a genuine duplicate is covered by the router's idempotency cache).
    /// The status read does not.
    pub fn register_oauth_kinds(&mut self) {
        use std::time::Duration;

        self.add(
            "fauna.oauth.issuer_key_status",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.oauth.rotate_issuer_key",
            RpcKindMeta::new(true, Duration::from_secs(5)),
        );
        self.add(
            "fauna.oauth.force_rotate_issuer_key",
            RpcKindMeta::new(true, Duration::from_secs(5)),
        );
        // The second signer's forced arm: re-mints the refresh-token secret,
        // so a replay would kill the generation the first call just minted.
        self.add(
            "fauna.oauth.force_rotate_session_secret",
            RpcKindMeta::new(true, Duration::from_secs(5)),
        );
        // The consent starts' user half (`authorization-server.md` § Consent).
        // None forbids replay: a repeated lookup re-claims the row the caller
        // already holds and answers it again, a repeated handoff open finds the
        // handle already spent and answers the one empty reply, and a repeated
        // block (or lift) lands the same state twice.
        self.add(
            "fauna.oauth.consent.lookup_code",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.oauth.consent.open_handoff",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.oauth.consent.block_client",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.oauth.consent.list_blocked_clients",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
    }

    /// Register the `fauna.principals.*` kinds — the third-party principal
    /// roster (`third-party.md` § The principal model).
    ///
    /// Neither forbids replay: the read is a read, and the revoke converges —
    /// a replayed frame finds no row and answers `revoked: false`, the state
    /// the first call already reached.
    pub fn register_principals_kinds(&mut self) {
        use std::time::Duration;

        self.add(
            "fauna.principals.list",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.principals.revoke",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        // The events doors' poll (`transport.md` § Push events → *Third-party
        // event doors*): a read, answered at once — the HTTP door does the
        // waiting.
        self.add(
            crate::push_events::KIND_EVENTS_POLL,
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
    }

    /// Register the `fauna.plugins.*` kinds — the admin's surface over
    /// nest-hosted plugins (`third-party.md` § The principal model → *Hosted
    /// principals*).
    ///
    /// None forbids replay: list is a read, install opens a fresh card each
    /// time (an unanswered one expires), and uninstall converges — a replayed
    /// frame finds no plugin and answers `uninstalled: false`. Install waits on
    /// two outbound fetches and a compile, so its deadline is the longest.
    pub fn register_plugins_kinds(&mut self) {
        use std::time::Duration;

        self.add(
            "fauna.plugins.install",
            RpcKindMeta::new(false, Duration::from_secs(120)),
        );
        self.add(
            "fauna.plugins.list",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.plugins.uninstall",
            RpcKindMeta::new(false, Duration::from_secs(30)),
        );
    }

    /// Register the `fauna.tls.*` kinds — TLS certificate kinds.
    ///
    /// Values are lifted verbatim from the nest's own `RpcRouter` registration
    /// for each kind, which is where this metadata has been declared (and
    /// deliberately chosen) all along; the two tables are held in lockstep by
    /// the parity test in `bins/fauna-nest/src/rpc_router.rs`.
    pub fn register_tls_kinds(&mut self) {
        use std::time::Duration;

        self.add(
            "fauna.tls.cert_status",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.tls.publish_cert",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
    }

    /// Register the `fauna.transport.*` kinds — transport-level control kinds.
    ///
    /// Values are lifted verbatim from the nest's own `RpcRouter` registration
    /// for each kind, which is where this metadata has been declared (and
    /// deliberately chosen) all along; the two tables are held in lockstep by
    /// the parity test in `bins/fauna-nest/src/rpc_router.rs`.
    pub fn register_transport_kinds(&mut self) {
        use std::time::Duration;

        self.add(
            "fauna.transport.get_policy",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
        self.add(
            "fauna.transport.put_policy",
            RpcKindMeta::new(false, Duration::from_secs(5)),
        );
    }

    /// Register **every** kind family the protocol crate declares.
    ///
    /// This is the aggregate the production constructor
    /// ([`KindRegistry::default_with_protocol_kinds`]) is built from, and the
    /// reason the 54 `register_*_kinds` methods below are not dead code. Keep
    /// it exhaustive: a family added without a call here is a family whose
    /// per-kind `forbid_replay` / `default_deadline` silently reverts to the
    /// client-side spec defaults (30 s, replay permitted), which is precisely
    /// the failure this method exists to prevent.
    ///
    /// Exhaustiveness is pinned nest-side by the router/registry parity test
    /// (`bins/fauna-nest/src/rpc_router.rs`), which fails on any kind the live
    /// `RpcRouter` dispatches but this registry does not declare.
    pub fn register_all_kinds(&mut self) {
        self.register_bridges_mail_kinds();
        self.register_subscriptions_kinds();
        self.register_filesync_kinds();
        #[cfg(feature = "payments")]
        self.register_payments_kinds();
        #[cfg(feature = "payments")]
        self.register_tips_kinds();
        self.register_features_kinds();
        self.register_dns_kinds();
        self.register_domain_kinds();
        self.register_pair_kinds();
        self.register_drafts_kinds();
        self.register_mls_kinds();
        self.register_oauth_kinds();
        self.register_principals_kinds();
        self.register_plugins_kinds();
        self.register_tls_kinds();
        self.register_transport_kinds();
        self.register_bridge_kinds();
        self.register_backup_kinds();
        self.register_recovery_kinds();
        self.register_generation_escrow_kinds();
        self.register_custody_kinds();
        self.register_capability_kinds();
        self.register_labeler_kinds();
        self.register_bridges_ui_kinds();
        self.register_email_kinds();
        self.register_inbox_kinds();
        self.register_bluesky_kinds();
        self.register_nostr_bunker_kinds();
        self.register_bridged_conversation_kinds();
        #[cfg(feature = "zaps")]
        self.register_nostr_zap_signer_kinds();
        self.register_nostr_content_kinds();
        self.register_conversations_channel_kinds();
        self.register_conversations_keypackage_kinds();
        self.register_conversations_welcome_kinds();
        self.register_conversations_room_kinds();
        self.register_delegation_kinds();
        self.register_spam_kinds();
        self.register_moderation_kinds();
        self.register_family_kinds();
        self.register_auth_kinds();
        self.register_discovery_kinds();
        self.register_account_register_kind();
        self.register_account_age_nonce_kind();
        self.register_account_lockout_kind();
        self.register_claim_admin_kind();
        self.register_account_kinds();
        self.register_pending_actions_kinds();
        self.register_stats_kinds();
        self.register_linkpreview_kinds();
        self.register_files_versions_kinds();
        self.register_folders_kinds();
        self.register_share_kinds();
        self.register_sync_kinds();
        self.register_media_kinds();
        self.register_web_kinds();
        self.register_labels_kinds();
        self.register_admin_kinds();
        self.register_region_kinds();
        self.register_push_kinds();
        self.register_sessions_kinds();
        self.register_invite_kinds();
        self.register_nat_mode_kind();
        self.register_posts_kinds();
        self.register_profile_kinds();
        self.register_notifications_kinds();
        self.register_contacts_kinds();
        self.register_feed_kinds();
        self.register_personalization_kinds();
        self.register_search_kinds();
        self.register_content_index_kinds();
        self.register_segments_kinds();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_insert_and_lookup() {
        let mut r = KindRegistry::new();
        r.add(
            "fauna.bridges.link",
            RpcKindMeta::new(false, Duration::from_secs(30)),
        );
        let m = r.meta("fauna.bridges.link").unwrap();
        assert!(!m.forbid_replay);
        assert_eq!(m.default_deadline, Duration::from_secs(30));
    }

    #[test]
    fn registry_unknown_kind_returns_none() {
        let r = KindRegistry::new();
        assert!(r.meta("com.acme.foo").is_none());
        assert!(!r.contains("com.acme.foo"));
    }

    #[test]
    fn protocol_kinds_default_includes_echo() {
        let r = KindRegistry::default_with_protocol_kinds();
        let m = r.meta("fauna.protocol.echo").unwrap();
        assert!(!m.forbid_replay);
    }

    #[test]
    fn bridges_ui_kinds_register_with_correct_metadata() {
        use std::time::Duration;
        let mut r = KindRegistry::new();
        r.register_bridges_ui_kinds();
        let m = r
            .meta("fauna.bridges.list")
            .expect("fauna.bridges.list registered");
        assert!(!m.forbid_replay, "list is replay-safe");
        assert_eq!(m.default_deadline, Duration::from_secs(5));

        // T2 — set_settings is an idempotent PUT-overwrite; replay-safe.
        let m = r
            .meta("fauna.bridges.set_settings")
            .expect("fauna.bridges.set_settings registered");
        assert!(!m.forbid_replay, "set_settings is idempotent overwrite");
        assert_eq!(m.default_deadline, Duration::from_secs(5));

        // T2 — list_follows is a pure read.
        let m = r
            .meta("fauna.bridges.list_follows")
            .expect("fauna.bridges.list_follows registered");
        assert!(!m.forbid_replay, "list_follows is replay-safe");
        assert_eq!(m.default_deadline, Duration::from_secs(5));

        // T3 — link starts an OAuth flow; double-invocation on retry can
        // corrupt provider-side pending state, so forbid_replay=true.
        // The 30 s deadline absorbs the upstream round-trip envelope.
        let m = r
            .meta("fauna.bridges.link")
            .expect("fauna.bridges.link registered");
        assert!(m.forbid_replay, "link forbids replay (OAuth flow start)");
        assert_eq!(m.default_deadline, Duration::from_secs(30));

        // T3 — unlink is idempotent (already-unlinked is a no-op).
        let m = r
            .meta("fauna.bridges.unlink")
            .expect("fauna.bridges.unlink registered");
        assert!(!m.forbid_replay, "unlink is idempotent");
        assert_eq!(m.default_deadline, Duration::from_secs(5));

        // T4 — add_follow is forbid_replay=true: duplicate-follow is a
        // server-enforced constraint and a replay would surface a
        // spurious conflict, so the caller must explicitly re-decide.
        let m = r
            .meta("fauna.bridges.add_follow")
            .expect("fauna.bridges.add_follow registered");
        assert!(
            m.forbid_replay,
            "add_follow forbids replay (duplicate constraint server-enforced)"
        );
        assert_eq!(m.default_deadline, Duration::from_secs(5));

        // T4 — remove_follow is idempotent (already-removed is a no-op).
        let m = r
            .meta("fauna.bridges.remove_follow")
            .expect("fauna.bridges.remove_follow registered");
        assert!(!m.forbid_replay, "remove_follow is idempotent");
        assert_eq!(m.default_deadline, Duration::from_secs(5));

        // Follow requests — a pure read and an idempotent answer.
        for kind in [
            "fauna.bridges.list_follow_requests",
            "fauna.bridges.resolve_follow_request",
        ] {
            let m = r.meta(kind).unwrap_or_else(|| panic!("{kind} registered"));
            assert!(!m.forbid_replay, "{kind} is replay-safe");
            assert_eq!(m.default_deadline, Duration::from_secs(5));
        }

        // T5 — feeds.{list,create,delete} are all replay-safe at 5 s.
        // create is idempotent because the DB row has
        // UNIQUE(actor_id, bridge, feed_uri) and INSERT OR IGNORE
        // returns the same id on duplicate.
        let m = r
            .meta("fauna.bridges.feeds.list")
            .expect("fauna.bridges.feeds.list registered");
        assert!(!m.forbid_replay, "feeds.list is replay-safe");
        assert_eq!(m.default_deadline, Duration::from_secs(5));
        let m = r
            .meta("fauna.bridges.feeds.create")
            .expect("fauna.bridges.feeds.create registered");
        assert!(
            !m.forbid_replay,
            "feeds.create is replay-safe — INSERT OR IGNORE returns same id on duplicate"
        );
        assert_eq!(m.default_deadline, Duration::from_secs(5));
        let m = r
            .meta("fauna.bridges.feeds.delete")
            .expect("fauna.bridges.feeds.delete registered");
        assert!(!m.forbid_replay, "feeds.delete is idempotent");
        assert_eq!(m.default_deadline, Duration::from_secs(5));
    }

    #[test]
    fn email_kinds_register_with_correct_metadata() {
        use std::time::Duration;
        let mut r = KindRegistry::new();
        r.register_email_kinds();
        for kind in [
            "fauna.email.filters.list",
            "fauna.email.filters.create",
            "fauna.email.filters.get",
            "fauna.email.filters.update",
            "fauna.email.filters.delete",
        ] {
            let m = r.meta(kind).unwrap_or_else(|| panic!("{kind} registered"));
            assert!(!m.forbid_replay, "{kind} is replay-safe");
            assert_eq!(m.default_deadline, Duration::from_secs(5));
        }
        // `fauna.email.send` forbids replay (outbound delivery would
        // double-send to remote MX on a recovered connection) and uses
        // a 30 s deadline — same shape as `fauna.bridges.link`, the
        // OAuth-flow precedent for "rare dangerous ops".
        let send = r
            .meta("fauna.email.send")
            .expect("fauna.email.send registered");
        assert!(send.forbid_replay, "send forbids replay (double-send risk)");
        assert_eq!(send.default_deadline, Duration::from_secs(30));
        // `fauna.email.inbox.fetch` — caller-scoped INBOX read. Idempotent
        // (replay-safe) but a 60 s deadline (vs the 5 s filter reads):
        // mail bodies can be large, like `fetch_message_ciphertext`.
        let inbox = r
            .meta("fauna.email.inbox.fetch")
            .expect("fauna.email.inbox.fetch registered");
        assert!(!inbox.forbid_replay, "inbox.fetch is an idempotent read");
        assert_eq!(inbox.default_deadline, Duration::from_secs(60));
    }

    #[test]
    fn inbox_kinds_register_with_correct_metadata() {
        use std::time::Duration;
        let mut r = KindRegistry::new();
        r.register_inbox_kinds();
        for kind in ["fauna.inbox.fetch", "fauna.inbox.ack"] {
            let m = r.meta(kind).unwrap_or_else(|| panic!("{kind} registered"));
            assert!(!m.forbid_replay, "{kind} is replay-safe (idempotent)");
            assert_eq!(m.default_deadline, Duration::from_secs(5));
        }
        // `send` is the cross-nest outbound leg, at the longer 30 s
        // federation-dial deadline — and it is replay-FORBIDDEN. This
        // assertion read `!send.forbid_replay` until the 71st pass, i.e. the
        // unit test pinned the defect: the local leg allocates a
        // nonce-and-timestamp `content_id` that cannot dedup, so a replay
        // duplicates the delivery and double-charges `inbox_bytes_used`.
        // Corrected rather than deleted, so the pin now guards the fix.
        let send = r
            .meta("fauna.inbox.send")
            .expect("fauna.inbox.send registered");
        assert!(send.forbid_replay, "send is NOT replay-safe");
        assert_eq!(send.default_deadline, Duration::from_secs(30));
    }

    #[test]
    fn bluesky_kind_registers_with_correct_metadata() {
        use std::time::Duration;
        let mut r = KindRegistry::new();
        r.register_bluesky_kinds();
        let m = r
            .meta("bluesky.feed.thread")
            .expect("bluesky.feed.thread registered");
        // Idempotent read (re-fetch is side-effect-free) but a 30 s deadline:
        // the handler makes a live getPostThread XRPC call to Bluesky, so it
        // gets the external-round-trip envelope, not the 5 s local-read one.
        assert!(!m.forbid_replay, "thread fetch is replay-safe (idempotent)");
        assert_eq!(m.default_deadline, Duration::from_secs(30));
    }

    #[test]
    fn nostr_content_kinds_register_with_correct_metadata() {
        use std::time::Duration;
        let mut r = KindRegistry::new();
        r.register_nostr_content_kinds();
        // `nostr.zaps.total` is only in this family when `zaps` is on — the
        // carve is per-kind, so the assertion follows the same cfg.
        #[cfg(feature = "zaps")]
        let read_kinds: &[&str] = &["nostr.zaps.total", "nostr.badges.list"];
        #[cfg(not(feature = "zaps"))]
        let read_kinds: &[&str] = &["nostr.badges.list"];
        for read_kind in read_kinds.iter().copied() {
            let m = r.meta(read_kind).expect("nostr content read registered");
            assert!(!m.forbid_replay, "{read_kind} is a replay-safe local read");
            assert_eq!(m.default_deadline, Duration::from_secs(5));
        }
        let publish = r
            .meta("nostr.events.publish_signed")
            .expect("nostr.events.publish_signed registered");
        assert!(
            publish.forbid_replay,
            "publish_signed forbids replay (relay-enqueue of a one-shot event)"
        );
        assert_eq!(publish.default_deadline, Duration::from_secs(30));
    }

    #[test]
    #[cfg(feature = "zaps")]
    fn nostr_zap_signer_kinds_register_with_correct_metadata() {
        use std::time::Duration;
        let mut r = KindRegistry::new();
        r.register_nostr_zap_signer_kinds();
        // All three are light and replay-safe: the trust root is a small
        // caller-scoped roster whose writes are an upsert and a delete, so
        // a replayed envelope changes nothing. Nothing here mints key
        // material or makes an external round-trip.
        for kind in [
            "fauna.nostr.zap_signers.list",
            "fauna.nostr.zap_signers.add",
            "fauna.nostr.zap_signers.remove",
        ] {
            let m = r.meta(kind).expect("zap signer kind registered");
            assert!(!m.forbid_replay, "{kind} is replay-safe (upsert/delete)");
            assert_eq!(m.default_deadline, Duration::from_secs(5));
        }
    }

    #[test]
    fn conversations_channel_kinds_register_with_correct_metadata() {
        use std::time::Duration;
        let mut r = KindRegistry::new();
        r.register_conversations_channel_kinds();
        // send forbids replay (server-assigned seq + push fan-out); 30 s
        // deadline matches fauna.bridges.link.
        let send = r
            .meta("fauna.conversations.channel.send")
            .expect("fauna.conversations.channel.send registered");
        assert!(
            send.forbid_replay,
            "send forbids replay (MAX(seq)+1 + push duplication risk)"
        );
        assert_eq!(send.default_deadline, Duration::from_secs(30));
        // fetch + list_for_actor + actors are pure reads.
        for kind in [
            "fauna.conversations.channel.fetch",
            "fauna.conversations.channel.list_for_actor",
            "fauna.conversations.channel.actors",
        ] {
            let m = r.meta(kind).unwrap_or_else(|| panic!("{kind} registered"));
            assert!(!m.forbid_replay, "{kind} is replay-safe");
            assert_eq!(m.default_deadline, Duration::from_secs(5));
        }
        // The roster-read relay is a pure read too, at the federation-hop
        // deadline `send_remote` set the precedent for.
        let remote = r
            .meta("fauna.conversations.channel.actors_remote")
            .expect("fauna.conversations.channel.actors_remote registered");
        assert!(!remote.forbid_replay, "actors_remote is replay-safe");
        assert_eq!(remote.default_deadline, Duration::from_secs(30));
    }

    #[test]
    fn conversations_keypackage_kinds_register_with_correct_metadata() {
        use std::time::Duration;
        let mut r = KindRegistry::new();
        r.register_conversations_keypackage_kinds();
        // upload + count are pure-ish reads (replay-safe at 5 s).
        for kind in [
            "fauna.conversations.keypackage.upload",
            "fauna.conversations.keypackage.count",
        ] {
            let m = r.meta(kind).unwrap_or_else(|| panic!("{kind} registered"));
            assert!(!m.forbid_replay, "{kind} is replay-safe");
            assert_eq!(m.default_deadline, Duration::from_secs(5));
        }
        // fetch consumes the FIFO oldest KP; replay would consume a
        // second one on top of the one already returned.
        let fetch = r
            .meta("fauna.conversations.keypackage.fetch")
            .expect("fauna.conversations.keypackage.fetch registered");
        assert!(
            fetch.forbid_replay,
            "fetch forbids replay (FIFO consume drains the queue)"
        );
        assert_eq!(fetch.default_deadline, Duration::from_secs(5));
    }

    #[test]
    fn conversations_welcome_kind_registers_with_correct_metadata() {
        use std::time::Duration;
        let mut r = KindRegistry::new();
        r.register_conversations_welcome_kinds();
        let m = r
            .meta("fauna.conversations.welcome.deliver")
            .expect("fauna.conversations.welcome.deliver registered");
        assert!(
            m.forbid_replay,
            "deliver forbids replay (inbox row + push fan-out)"
        );
        assert_eq!(m.default_deadline, Duration::from_secs(30));
    }

    #[test]
    fn spam_kinds_register_with_correct_metadata() {
        use std::time::Duration;
        let mut r = KindRegistry::new();
        r.register_spam_kinds();
        for kind in ["fauna.spam.get_preferences", "fauna.spam.set_preferences"] {
            let m = r.meta(kind).unwrap_or_else(|| panic!("{kind} registered"));
            assert!(
                !m.forbid_replay,
                "{kind} is replay-safe (read / idempotent upsert)"
            );
            assert_eq!(m.default_deadline, Duration::from_secs(5));
        }
    }

    #[test]
    fn delegation_kinds_register_with_correct_metadata() {
        use std::time::Duration;
        let mut r = KindRegistry::new();
        r.register_delegation_kinds();
        for kind in [
            crate::delegation::KIND_HEARTBEAT,
            crate::delegation::KIND_OBSERVE,
        ] {
            let m = r.meta(kind).unwrap_or_else(|| panic!("{kind} registered"));
            assert!(
                !m.forbid_replay,
                "{kind} is replay-safe (LWW heartbeat / pure observe)"
            );
            assert_eq!(m.default_deadline, Duration::from_secs(5));
        }
    }

    #[test]
    fn moderation_kinds_register_with_correct_metadata() {
        use std::time::Duration;
        let mut r = KindRegistry::new();
        r.register_moderation_kinds();
        for kind in [
            "fauna.moderation.stats",
            "fauna.moderation.actions",
            "fauna.moderation.appeal",
            "fauna.moderation.train",
            "fauna.moderation.legal_takedown",
            "fauna.moderation.report_share.set",
            "fauna.moderation.report_share.status",
            "fauna.moderation.signal_share.set",
            "fauna.moderation.signal_share.status",
            "fauna.moderation.signal_contribute",
        ] {
            let m = r.meta(kind).unwrap_or_else(|| panic!("{kind} registered"));
            assert!(!m.forbid_replay, "{kind} is replay-safe");
        }
        // Reads + audit/append writes are quick; train (a post fetch) is heavy.
        for kind in [
            "fauna.moderation.stats",
            "fauna.moderation.actions",
            "fauna.moderation.appeal",
            "fauna.moderation.legal_takedown",
            "fauna.moderation.report_share.set",
            "fauna.moderation.report_share.status",
            "fauna.moderation.signal_share.set",
            "fauna.moderation.signal_share.status",
            "fauna.moderation.signal_contribute",
        ] {
            assert_eq!(
                r.meta(kind).unwrap().default_deadline,
                Duration::from_secs(5)
            );
        }
        assert_eq!(
            r.meta("fauna.moderation.train").unwrap().default_deadline,
            Duration::from_secs(10)
        );
    }

    #[test]
    fn family_kinds_register_with_correct_metadata() {
        use std::time::Duration;
        let mut r = KindRegistry::new();
        r.register_family_kinds();
        for kind in [
            "fauna.family.status",
            "fauna.family.policy.update",
            "fauna.family.graduate",
            "fauna.family.transfer",
            "fauna.family.transfer.accept",
            "fauna.family.transfer.decline",
            "fauna.family.transfer.cancel",
            "fauna.family.approvals.list",
            "fauna.family.approvals.decide",
            "fauna.family.contact.add",
            "fauna.family.contact.request",
            "fauna.family.feed_source.request",
            "fauna.family.notify_report",
            "fauna.family.usage_report",
            "fauna.family.device.mark",
        ] {
            let m = r.meta(kind).unwrap_or_else(|| panic!("{kind} registered"));
            assert!(!m.forbid_replay, "{kind} is replay-safe");
            assert_eq!(m.default_deadline, Duration::from_secs(5));
        }
    }

    #[test]
    fn labels_kinds_register_with_correct_metadata() {
        use std::time::Duration;
        let mut r = KindRegistry::new();
        r.register_labels_kinds();
        for kind in ["fauna.labels.attach", "fauna.labels.list"] {
            let m = r.meta(kind).unwrap_or_else(|| panic!("{kind} registered"));
            assert!(
                !m.forbid_replay,
                "{kind} is replay-safe (read / idempotent upsert)"
            );
            assert_eq!(m.default_deadline, Duration::from_secs(5));
        }
    }

    #[test]
    fn discovery_kinds_register_with_correct_metadata() {
        use std::time::Duration;
        let mut r = KindRegistry::new();
        r.register_discovery_kinds();
        for kind in [
            "fauna.nest.info",
            "fauna.handle.available",
            "fauna.nest.resolve",
            "fauna.actor.by_handle",
            "fauna.setup.status",
        ] {
            let m = r.meta(kind).unwrap_or_else(|| panic!("{kind} registered"));
            assert!(!m.forbid_replay, "{kind} is a replay-safe pure read");
            assert_eq!(m.default_deadline, Duration::from_secs(5));
        }
    }

    #[test]
    fn account_register_kind_registers_with_correct_metadata() {
        use std::time::Duration;
        let mut r = KindRegistry::new();
        r.register_account_register_kind();
        let m = r
            .meta("fauna.account.register")
            .expect("fauna.account.register registered");
        // Replay-safe (the registration conflict checks + idempotency cache),
        // 30 s like fauna.posts.create (write + DB txn + spawned DNS).
        assert!(!m.forbid_replay);
        assert_eq!(m.default_deadline, Duration::from_secs(30));
    }

    #[test]
    fn account_lockout_kind_registers_with_correct_metadata() {
        use std::time::Duration;
        let mut r = KindRegistry::new();
        r.register_account_lockout_kind();
        let m = r
            .meta("fauna.account.lockout")
            .expect("fauna.account.lockout registered");
        // Replay-safe (idempotent re-lock + idempotency cache), 5 s (quick write).
        assert!(!m.forbid_replay);
        assert_eq!(m.default_deadline, Duration::from_secs(5));
    }

    #[test]
    fn pending_actions_kinds_register_with_correct_metadata() {
        use std::time::Duration;
        let mut r = KindRegistry::new();
        r.register_pending_actions_kinds();
        for kind in [
            "fauna.pending_actions.list",
            "fauna.pending_actions.get",
            "fauna.pending_actions.cancel",
            "fauna.pending_actions.approve",
        ] {
            let m = r.meta(kind).unwrap_or_else(|| panic!("{kind} registered"));
            // All four are replay-safe @5s (reads + idempotent cancel/approve).
            assert!(!m.forbid_replay, "{kind} does not forbid replay");
            assert_eq!(m.default_deadline, Duration::from_secs(5));
        }
    }

    #[test]
    fn stats_kind_registers_with_correct_metadata() {
        use std::time::Duration;
        let mut r = KindRegistry::new();
        r.register_stats_kinds();
        let m = r
            .meta("fauna.stats.get")
            .expect("fauna.stats.get registered");
        // Pure read — replay-safe @5s.
        assert!(!m.forbid_replay);
        assert_eq!(m.default_deadline, Duration::from_secs(5));
    }

    #[test]
    fn linkpreview_kind_registers_with_correct_metadata() {
        use std::time::Duration;
        let mut r = KindRegistry::new();
        r.register_linkpreview_kinds();
        let m = r
            .meta(crate::linkpreview::KIND_LINKPREVIEW_RESOLVE)
            .expect("fauna.linkpreview.resolve registered");
        // Pure read performing a (cached) external fetch — replay-safe @30s,
        // matching the bluesky.feed.thread external-fetch precedent.
        assert!(!m.forbid_replay);
        assert_eq!(m.default_deadline, Duration::from_secs(30));
    }

    #[test]
    fn media_playback_ticket_kind_registers_with_correct_metadata() {
        use std::time::Duration;
        let mut r = KindRegistry::new();
        r.register_media_kinds();
        let m = r
            .meta(crate::media_ticket::KIND_MEDIA_PLAYBACK_TICKET)
            .expect("fauna.media.playback_ticket registered");
        // A mint that writes nothing — replay-safe @5s.
        assert!(!m.forbid_replay);
        assert_eq!(m.default_deadline, Duration::from_secs(5));
    }

    #[test]
    fn files_versions_kinds_register_with_correct_metadata() {
        use std::time::Duration;
        let mut r = KindRegistry::new();
        r.register_files_versions_kinds();
        for kind in ["fauna.files.versions.list", "fauna.files.versions.get"] {
            let m = r.meta(kind).unwrap_or_else(|| panic!("{kind} registered"));
            // Pure reads — replay-safe @5s.
            assert!(!m.forbid_replay, "{kind} does not forbid replay");
            assert_eq!(m.default_deadline, Duration::from_secs(5));
        }
    }

    #[test]
    fn folders_kinds_register_with_correct_metadata() {
        use std::time::Duration;
        let mut r = KindRegistry::new();
        r.register_folders_kinds();
        for kind in [
            "fauna.folders.create",
            "fauna.folders.list",
            "fauna.folders.update",
            "fauna.folders.delete",
            "fauna.folders.devices",
            "fauna.folders.members.list",
            "fauna.folders.members.list_actors",
            "fauna.folders.members.set_access",
            "fauna.folders.members.remove",
            "fauna.folders.members.evict",
            "fauna.folders.places.set",
            "fauna.folders.leave",
            "fauna.folders.share",
            "fauna.folders.content_key.put",
            "fauna.folders.content_key.get",
            "fauna.folders.lease.acquire",
            "fauna.folders.lease.release",
            "fauna.folders.public.fetch",
            "fauna.sync.conflicts.list",
            "fauna.sync.conflicts.report",
            "fauna.sync.conflicts.resolve",
        ] {
            let m = r.meta(kind).unwrap_or_else(|| panic!("{kind} registered"));
            // Reads + idempotent local mutations @5s.
            assert!(!m.forbid_replay, "{kind} does not forbid replay");
            assert_eq!(m.default_deadline, Duration::from_secs(5));
        }
        // The cross-nest roster relay: a pure read at the federation-hop deadline.
        let remote = r
            .meta("fauna.folders.members.list_actors_remote")
            .expect("fauna.folders.members.list_actors_remote registered");
        assert!(!remote.forbid_replay, "list_actors_remote is replay-safe");
        assert_eq!(remote.default_deadline, Duration::from_secs(30));
        // A deposit parks one item per accepted call: never replayed.
        let deposit = r
            .meta(crate::folders::KIND_FOLDERS_DEPOSIT)
            .expect("fauna.folders.deposit registered");
        assert!(deposit.forbid_replay, "a replayed deposit would park twice");
        assert_eq!(deposit.default_deadline, Duration::from_secs(30));
    }

    #[test]
    fn share_kinds_register_with_correct_metadata() {
        use std::time::Duration;
        let mut r = KindRegistry::new();
        r.register_share_kinds();
        for kind in [
            "fauna.share.create",
            "fauna.share.list",
            "fauna.share.revoke",
        ] {
            let m = r.meta(kind).unwrap_or_else(|| panic!("{kind} registered"));
            // Read + idempotent local mutations @5s.
            assert!(!m.forbid_replay, "{kind} does not forbid replay");
            assert_eq!(m.default_deadline, Duration::from_secs(5));
        }
    }

    #[test]
    fn sync_kinds_register_with_correct_metadata() {
        use std::time::Duration;
        let mut r = KindRegistry::new();
        r.register_sync_kinds();
        for kind in [
            "fauna.sync.register",
            "fauna.sync.device_grant.register",
            "fauna.sync.device_grant.revoke",
            "fauna.sync.changes.list",
            "fauna.sync.changes.record",
            "fauna.sync.changes.supersede",
            "fauna.sync.status",
            "fauna.sync.files",
            "fauna.sync.backup_status",
            "fauna.sync.devices.list",
            "fauna.sync.devices.delete",
            "fauna.sync.devices.p2p_participation.set",
            crate::sync::KIND_SYNC_SERVE_ANNOUNCE,
            crate::account_state::KIND_STATE_PUT,
            crate::account_state::KIND_STATE_RETIRE,
        ] {
            let m = r.meta(kind).unwrap_or_else(|| panic!("{kind} registered"));
            // Reads + idempotent local mutations @5s.
            assert!(!m.forbid_replay, "{kind} does not forbid replay");
            assert_eq!(m.default_deadline, Duration::from_secs(5));
        }
    }

    #[test]
    fn media_kinds_register_with_correct_metadata() {
        use std::time::Duration;
        let mut r = KindRegistry::new();
        r.register_media_kinds();
        let m = r
            .meta("fauna.media.list")
            .unwrap_or_else(|| panic!("fauna.media.list registered"));
        // A pure keyset-paginated read @5s.
        assert!(!m.forbid_replay, "fauna.media.list does not forbid replay");
        assert_eq!(m.default_deadline, Duration::from_secs(5));
    }

    #[test]
    fn web_kinds_register_with_correct_metadata() {
        use std::time::Duration;
        let mut r = KindRegistry::new();
        r.register_web_kinds();
        // Reads + idempotent publish/apex mutations @5s.
        for kind in [
            "fauna.web.publish.set",
            "fauna.web.publish.unset",
            "fauna.web.publish.list",
            "fauna.web.domain.get",
            "fauna.web.set_apex_actor",
            "fauna.web.get_apex_actor",
            "fauna.web.set_subdomain_enabled",
            "fauna.web.get_subdomain_enabled",
            "fauna.web.files.prune_sealed",
        ] {
            let m = r.meta(kind).unwrap_or_else(|| panic!("{kind} registered"));
            assert!(!m.forbid_replay, "{kind} does not forbid replay");
            assert_eq!(m.default_deadline, Duration::from_secs(5));
        }
        // domain.set / domain.delete are @30s writes.
        for kind in ["fauna.web.domain.set", "fauna.web.domain.delete"] {
            let m = r.meta(kind).unwrap_or_else(|| panic!("{kind} registered"));
            assert!(!m.forbid_replay);
            assert_eq!(m.default_deadline, Duration::from_secs(30));
        }
    }

    #[test]
    fn admin_kinds_register_with_correct_metadata() {
        use std::time::Duration;
        let mut r = KindRegistry::new();
        r.register_admin_kinds();
        for kind in [
            "fauna.admin.users.list",
            "fauna.admin.users.get",
            "fauna.admin.users.create",
            "fauna.admin.users.update",
            "fauna.admin.users.delete",
            "fauna.admin.users.clear_handle",
            "fauna.admin.users.evict",
            "fauna.admin.users.cancel_eviction",
            "fauna.admin.users.suspend",
            "fauna.admin.evictions.list",
            "fauna.admin.tiers.list",
            "fauna.admin.tiers.create",
            "fauna.admin.tiers.update",
            "fauna.admin.invite_codes.list",
            "fauna.admin.invite_codes.delete",
            "fauna.admin.invite_requests.list",
            "fauna.admin.invite_requests.approve",
            "fauna.admin.invite_requests.deny",
            "fauna.admin.admins.list",
            "fauna.admin.admins.add",
            "fauna.admin.admins.remove",
            "fauna.admin.stats",
            "fauna.admin.status",
            "fauna.admin.audit.list",
            "fauna.admin.audit.integrity",
            "fauna.admin.cluster.status",
            "fauna.admin.gc",
            "fauna.admin.worker.status",
            "fauna.admin.pending_actions.list",
            "fauna.admin.folders.create",
            "fauna.admin.folders.get",
            "fauna.admin.folders.add_member",
            "fauna.admin.services.list",
            "fauna.admin.services.update",
        ] {
            let m = r.meta(kind).unwrap_or_else(|| panic!("{kind} registered"));
            assert!(!m.forbid_replay, "{kind} does not forbid replay");
            assert_eq!(m.default_deadline, Duration::from_secs(5));
        }

        // `invite_codes.create` is the one admin kind that DOES forbid replay,
        // so it is asserted here rather than dropped from the list — it sat in
        // the loop above until the 71st pass, i.e. this unit pinned the defect.
        // An empty `code` mints a fresh random credential and inserts a row
        // keyed on it, so a replay leaves a second redeemable admission code.
        let create = r
            .meta("fauna.admin.invite_codes.create")
            .expect("fauna.admin.invite_codes.create registered");
        assert!(
            create.forbid_replay,
            "invite_codes.create mints a credential — it must forbid replay"
        );
        assert_eq!(create.default_deadline, Duration::from_secs(5));
    }

    #[test]
    fn push_kinds_register_with_correct_metadata() {
        use std::time::Duration;
        let mut r = KindRegistry::new();
        r.register_push_kinds();
        for kind in [
            "fauna.push.vapid_key",
            "fauna.push.subscribe",
            "fauna.push.unsubscribe",
            "fauna.push.presence",
        ] {
            let m = r.meta(kind).unwrap_or_else(|| panic!("{kind} registered"));
            // Read / idempotent writes @5s.
            assert!(!m.forbid_replay, "{kind} does not forbid replay");
            assert_eq!(m.default_deadline, Duration::from_secs(5));
        }
    }

    #[test]
    fn sessions_kinds_register_with_correct_metadata() {
        use std::time::Duration;
        let mut r = KindRegistry::new();
        r.register_sessions_kinds();
        for kind in [
            "fauna.sessions.list",
            "fauna.sessions.revoke",
            "fauna.sessions.revoke_all",
            "fauna.sessions.lockout",
        ] {
            let m = r.meta(kind).unwrap_or_else(|| panic!("{kind} registered"));
            // Read + idempotent mutations @5s.
            assert!(!m.forbid_replay, "{kind} does not forbid replay");
            assert_eq!(m.default_deadline, Duration::from_secs(5));
        }
    }

    #[test]
    fn claim_admin_kind_registers_with_correct_metadata() {
        use std::time::Duration;
        let mut r = KindRegistry::new();
        r.register_claim_admin_kind();
        let m = r
            .meta("fauna.auth.claim_admin")
            .expect("fauna.auth.claim_admin registered");
        // Replay-safe (claim-code deletion → already_claimed on replay +
        // idempotency cache), 5 s like the auth-bootstrap token mint.
        assert!(!m.forbid_replay);
        assert_eq!(m.default_deadline, Duration::from_secs(5));
    }

    #[test]
    fn account_kinds_register_with_correct_metadata() {
        use std::time::Duration;
        let mut r = KindRegistry::new();
        r.register_account_kinds();
        // Three reads + the two pending-action creators are @5 s; none forbids
        // replay (reads are pure; the pending-action creators are
        // idempotent-enough at the 5 s window).
        for kind in [
            "fauna.account.get",
            "fauna.quota.get",
            "fauna.account.am_i_admin",
            "fauna.profile.handle.change",
            "fauna.account.delete",
        ] {
            let m = r.meta(kind).unwrap_or_else(|| panic!("{kind} registered"));
            assert!(!m.forbid_replay, "{kind} does not forbid replay");
            assert_eq!(
                m.default_deadline,
                Duration::from_secs(5),
                "{kind} deadline is 5s"
            );
        }
        // upgrade is the invite-consuming write — 30 s, matching posts.create.
        let up = r
            .meta("fauna.account.upgrade")
            .expect("fauna.account.upgrade registered");
        assert!(
            !up.forbid_replay,
            "upgrade is replay-safe (idempotency cache)"
        );
        assert_eq!(up.default_deadline, Duration::from_secs(30));
    }

    #[test]
    fn invite_kinds_register_with_correct_metadata() {
        use std::time::Duration;
        let mut r = KindRegistry::new();
        r.register_invite_kinds();
        // submit is a write (30 s), like account.register.
        let submit = r
            .meta("fauna.account.invite_request.submit")
            .expect("invite_request.submit registered");
        assert!(!submit.forbid_replay);
        assert_eq!(submit.default_deadline, Duration::from_secs(30));
        // status / cancel / verify are reads / idempotent (5 s).
        for kind in [
            "fauna.account.invite_request.status",
            "fauna.account.invite_request.cancel",
            "fauna.account.invite_code.verify",
        ] {
            let m = r.meta(kind).unwrap_or_else(|| panic!("{kind} registered"));
            assert!(!m.forbid_replay, "{kind} is replay-safe");
            assert_eq!(m.default_deadline, Duration::from_secs(5));
        }
    }

    #[test]
    fn nat_mode_kind_registers_with_correct_metadata() {
        use std::time::Duration;
        let mut r = KindRegistry::new();
        r.register_nat_mode_kind();
        let m = r
            .meta("fauna.setup.nat_mode")
            .expect("fauna.setup.nat_mode registered");
        // Replay-safe (mutable set → idempotent, no conflict; + idempotency
        // cache replays the first reply), 5 s.
        assert!(!m.forbid_replay);
        assert_eq!(m.default_deadline, Duration::from_secs(5));
    }

    #[test]
    fn search_kind_registers_with_correct_metadata() {
        use std::time::Duration;
        let mut r = KindRegistry::new();
        r.register_search_kinds();
        let m = r
            .meta("fauna.search.query")
            .expect("fauna.search.query registered");
        assert!(!m.forbid_replay, "search is a replay-safe pure read");
        assert_eq!(m.default_deadline, Duration::from_secs(5));
    }

    #[test]
    fn content_index_kinds_register_with_correct_metadata() {
        use std::time::Duration;
        let mut r = KindRegistry::new();
        r.register_content_index_kinds();
        let record = r
            .meta(crate::content_index::KIND_RECORD)
            .expect("fauna.index.record registered");
        assert!(
            !record.forbid_replay,
            "record converges the same (path, blob hash) pair, replay-safe"
        );
        assert_eq!(record.default_deadline, Duration::from_secs(10));
        let list = r
            .meta(crate::content_index::KIND_LIST)
            .expect("fauna.index.list registered");
        assert!(!list.forbid_replay, "list is a pure read");
        assert_eq!(list.default_deadline, Duration::from_secs(5));
    }

    #[test]
    fn profile_kinds_register_with_correct_metadata() {
        use std::time::Duration;
        let mut r = KindRegistry::new();
        r.register_profile_kinds();
        // get is a pure read, like fauna.posts.get / fauna.account.get.
        let get = r
            .meta("fauna.profile.get")
            .expect("fauna.profile.get registered");
        assert!(!get.forbid_replay, "profile.get is a replay-safe pure read");
        assert_eq!(get.default_deadline, Duration::from_secs(5));
    }

    #[test]
    fn posts_kinds_register_with_correct_metadata() {
        use std::time::Duration;
        let mut r = KindRegistry::new();
        r.register_posts_kinds();
        // get is a pure read.
        let get = r
            .meta("fauna.posts.get")
            .expect("fauna.posts.get registered");
        assert!(!get.forbid_replay, "posts.get is a replay-safe pure read");
        assert_eq!(get.default_deadline, Duration::from_secs(5));
        // room_labels is the same class: a read of a derived view.
        let room_labels = r
            .meta("fauna.posts.room_labels")
            .expect("fauna.posts.room_labels registered");
        assert!(!room_labels.forbid_replay);
        assert_eq!(room_labels.default_deadline, Duration::from_secs(5));
        // create is idempotent on the content-addressed post_id; 30 s
        // deadline covers ingest verify + insert + fan-out.
        let create = r
            .meta("fauna.posts.create")
            .expect("fauna.posts.create registered");
        assert!(
            !create.forbid_replay,
            "posts.create is replay-safe (content-addressed post_id; put_post + replicate idempotent)"
        );
        assert_eq!(create.default_deadline, Duration::from_secs(30));
        // interact forbids replay — the like action double-counts the
        // score + re-notifies on replay.
        let interact = r
            .meta("fauna.posts.interact")
            .expect("fauna.posts.interact registered");
        assert!(
            interact.forbid_replay,
            "posts.interact forbids replay (like increments a non-idempotent score + notifies)"
        );
        assert_eq!(interact.default_deadline, Duration::from_secs(5));
    }

    #[test]
    fn notifications_kinds_register_with_correct_metadata() {
        use std::time::Duration;
        let mut r = KindRegistry::new();
        r.register_notifications_kinds();
        // Every notifications kind is replay-safe @5 s: list/count are pure
        // reads; mark_read is an idempotent upsert (no score-increment
        // hazard like posts.interact); dismiss/clear are idempotent deletes.
        for kind in [
            "fauna.notifications.list",
            "fauna.notifications.mark_read",
            "fauna.notifications.count",
            "fauna.notifications.dismiss",
            "fauna.notifications.clear",
        ] {
            let m = r.meta(kind).unwrap_or_else(|| panic!("{kind} registered"));
            assert!(
                !m.forbid_replay,
                "{kind} is replay-safe (pure read / idempotent upsert)"
            );
            assert_eq!(
                m.default_deadline,
                Duration::from_secs(5),
                "{kind} deadline is 5s"
            );
        }
    }

    #[test]
    fn contacts_kinds_register_with_correct_metadata() {
        use std::time::Duration;
        let mut r = KindRegistry::new();
        r.register_contacts_kinds();
        // Every connection-management kind is replay-safe @5 s: the three
        // reads are pure; the four writes are idempotent and converge on
        // re-issue (no score-increment hazard like posts.interact).
        for kind in [
            "fauna.knocks.list",
            "fauna.knocks.accept",
            "fauna.knocks.block",
            "fauna.knocks.unblock",
            "fauna.knocks.dismiss",
            "fauna.contacts.list",
            "fauna.contacts.confirm",
            "fauna.inbox.mode.get",
            "fauna.inbox.mode.set",
        ] {
            let m = r.meta(kind).unwrap_or_else(|| panic!("{kind} registered"));
            assert!(
                !m.forbid_replay,
                "{kind} is replay-safe (pure read / idempotent write)"
            );
            assert_eq!(
                m.default_deadline,
                Duration::from_secs(5),
                "{kind} deadline is 5s"
            );
        }
    }

    #[test]
    fn feed_kinds_register_with_correct_metadata() {
        use std::time::Duration;
        let mut r = KindRegistry::new();
        r.register_feed_kinds();
        // Every feed kind is replay-safe @5 s (owner-keyed; no
        // score-increment hazard like posts.interact).
        for kind in [
            "fauna.feed.list",
            "fauna.feed.create",
            "fauna.feed.get",
            "fauna.feed.update",
            "fauna.feed.delete",
            "fauna.feed.posts",
            "fauna.feed.local.posts",
            "fauna.feed.contributors.list",
            "fauna.feed.contributors.grant",
            "fauna.feed.contributors.revoke",
        ] {
            let m = r.meta(kind).unwrap_or_else(|| panic!("{kind} registered"));
            assert!(
                !m.forbid_replay,
                "{kind} is replay-safe (owner-keyed mutation / pure read)"
            );
            assert_eq!(
                m.default_deadline,
                Duration::from_secs(5),
                "{kind} deadline is 5s"
            );
        }
    }

    #[test]
    fn personalization_kinds_register_with_correct_metadata() {
        use std::time::Duration;
        let mut r = KindRegistry::new();
        r.register_personalization_kinds();
        // All three are replay-safe @5 s (pure read / idempotent
        // owner-keyed overwrite / idempotent delete).
        for kind in [
            "fauna.personalization.model.fetch",
            "fauna.personalization.model.put",
            "fauna.personalization.model.delete",
        ] {
            let m = r.meta(kind).unwrap_or_else(|| panic!("{kind} registered"));
            assert!(
                !m.forbid_replay,
                "{kind} is replay-safe (pure read / idempotent write)"
            );
            assert_eq!(
                m.default_deadline,
                Duration::from_secs(5),
                "{kind} deadline is 5s"
            );
        }
    }

    #[test]
    fn segments_kinds_register_with_correct_metadata() {
        use std::time::Duration;
        let mut r = KindRegistry::new();
        r.register_segments_kinds();
        let list = r.meta("fauna.segments.list").expect("list registered");
        assert!(!list.forbid_replay, "list is replay-safe");
        assert_eq!(list.default_deadline, Duration::from_secs(5));
        let changed = r
            .meta("fauna.segments.changed")
            .expect("changed registered");
        assert!(!changed.forbid_replay);
        assert_eq!(changed.default_deadline, Duration::from_secs(5));
    }

    #[test]
    fn recovery_kinds_register_with_correct_metadata() {
        use std::time::Duration;
        let mut r = KindRegistry::new();
        r.register_recovery_kinds();

        let submit = r.meta("fauna.recovery.registration.submit").unwrap();
        assert_eq!(submit.default_deadline, Duration::from_secs(10));
        // Replay-safe by construction: a re-delivered submit carries the same
        // `seq`, which the nest store refuses as non-advancing. If a future
        // change makes the submit seq-less or last-write-wins, this flag must
        // flip with it.
        assert!(!submit.forbid_replay);

        let chain = r.meta("fauna.recovery.registration.chain").unwrap();
        assert_eq!(chain.default_deadline, Duration::from_secs(5));
        assert!(!chain.forbid_replay);

        // Escrow: put is an upsert of the same bytes and challenge merely mints
        // another coexisting nonce, so both are replay-safe.
        let put = r.meta("fauna.recovery.escrow.put").unwrap();
        assert_eq!(put.default_deadline, Duration::from_secs(10));
        assert!(!put.forbid_replay);
        assert!(
            !r.meta("fauna.recovery.escrow.challenge")
                .unwrap()
                .forbid_replay
        );

        // `fetch` is the exception, and the pin that matters: its nonce is
        // consumed on the first attempt, so a blind auto-retry surfaces a
        // spurious `invalid_nonce` instead of the transport failure that
        // actually happened. If this flips to false, the client will retry a
        // call that cannot succeed.
        let fetch = r.meta("fauna.recovery.escrow.fetch").unwrap();
        assert_eq!(fetch.default_deadline, Duration::from_secs(10));
        assert!(fetch.forbid_replay);

        // The replacement window. `request` stays replay-safe only while the
        // store keeps the original `requested_at` for an unchanged record
        // digest (a retry must not extend the window); `veto` mirrors
        // `fetch`'s single-use-nonce reasoning.
        let request = r.meta("fauna.recovery.replacement.request").unwrap();
        assert_eq!(request.default_deadline, Duration::from_secs(10));
        assert!(!request.forbid_replay);
        assert!(
            !r.meta("fauna.recovery.replacement.challenge")
                .unwrap()
                .forbid_replay
        );
        let veto = r.meta("fauna.recovery.replacement.veto").unwrap();
        assert_eq!(veto.default_deadline, Duration::from_secs(10));
        assert!(veto.forbid_replay);
        assert!(
            !r.meta("fauna.recovery.replacement.status")
                .unwrap()
                .forbid_replay
        );

        // Succession. `submit` is replay-safe by outcome (the second delivery
        // hits the `actor_successions` PK and is refused, never applied twice)
        // and `lookup` is a pure read.
        let succession_submit = r.meta("fauna.recovery.succession.submit").unwrap();
        assert_eq!(succession_submit.default_deadline, Duration::from_secs(10));
        assert!(!succession_submit.forbid_replay);
        let lookup = r.meta("fauna.recovery.succession.lookup").unwrap();
        assert_eq!(lookup.default_deadline, Duration::from_secs(5));
        assert!(!lookup.forbid_replay);
        let status = r.meta(crate::recovery::SUCCESSION_STATUS_KIND).unwrap();
        assert_eq!(status.default_deadline, Duration::from_secs(5));
        assert!(!status.forbid_replay);
        let owed_settle = r
            .meta(crate::recovery::SUCCESSION_OWED_SETTLE_KIND)
            .unwrap();
        assert_eq!(owed_settle.default_deadline, Duration::from_secs(5));
        assert!(!owed_settle.forbid_replay);
    }

    #[test]
    fn capability_kinds_register_with_correct_deadlines() {
        use std::time::Duration;
        let mut r = KindRegistry::new();
        r.register_backup_kinds();
        r.register_capability_kinds();
        assert_eq!(
            r.meta("fauna.capabilities.mint").unwrap().default_deadline,
            Duration::from_secs(30)
        );
        assert_eq!(
            r.meta("fauna.capabilities.renew").unwrap().default_deadline,
            Duration::from_secs(30)
        );
        assert_eq!(
            r.meta("fauna.capabilities.fetch").unwrap().default_deadline,
            Duration::from_secs(5)
        );
        assert_eq!(
            r.meta("fauna.capabilities.revoke")
                .unwrap()
                .default_deadline,
            Duration::from_secs(5)
        );
        assert_eq!(
            r.meta("fauna.capabilities.reconcile")
                .unwrap()
                .default_deadline,
            Duration::from_secs(5)
        );
        for k in [
            "fauna.capabilities.mint",
            "fauna.capabilities.fetch",
            "fauna.capabilities.renew",
            "fauna.capabilities.revoke",
            "fauna.capabilities.reconcile",
        ] {
            assert!(!r.meta(k).unwrap().forbid_replay, "{k} is replay-safe");
        }
    }

    #[test]
    fn labeler_kinds_register_with_correct_deadlines() {
        use std::time::Duration;
        let mut r = KindRegistry::new();
        r.register_labeler_kinds();
        // publish/inspect carry WASM bytes → 30 s provision weight.
        for k in ["fauna.labelers.publish", "fauna.labelers.inspect"] {
            assert_eq!(
                r.meta(k).unwrap().default_deadline,
                Duration::from_secs(30),
                "{k} is provision-weight"
            );
        }
        // list/subscribe/unsubscribe are light reads / row writes → 5 s.
        for k in [
            "fauna.labelers.list",
            "fauna.labelers.subscribe",
            "fauna.labelers.unsubscribe",
        ] {
            assert_eq!(
                r.meta(k).unwrap().default_deadline,
                Duration::from_secs(5),
                "{k} is a light read/write"
            );
        }
        // All five are replay-safe (idempotent).
        for k in [
            "fauna.labelers.publish",
            "fauna.labelers.list",
            "fauna.labelers.inspect",
            "fauna.labelers.subscribe",
            "fauna.labelers.unsubscribe",
        ] {
            assert!(!r.meta(k).unwrap().forbid_replay, "{k} is replay-safe");
        }
    }

    #[test]
    fn bridge_kinds_register_with_correct_deadlines() {
        use std::time::Duration;
        let mut r = KindRegistry::new();
        r.register_bridge_kinds();
        let m = r.meta("fauna.bridges.fetch_wrapped_mls_blob").unwrap();
        assert_eq!(m.default_deadline, Duration::from_secs(5));
        assert!(!m.forbid_replay);
        let m2 = r.meta("fauna.bridges.provision_tls_cert_blob").unwrap();
        assert_eq!(m2.default_deadline, Duration::from_secs(30));
        assert!(r.contains("fauna.bridges.report_auth_event"));
        // C.1 IMAP metadata kinds.
        let list_m = r.meta("fauna.bridges.list_mailboxes").unwrap();
        assert_eq!(list_m.default_deadline, Duration::from_secs(5));
        assert!(!list_m.forbid_replay);
        let select_m = r.meta("fauna.bridges.select_mailbox").unwrap();
        assert_eq!(select_m.default_deadline, Duration::from_secs(5));
        assert!(!select_m.forbid_replay);
        // C.2 message-metadata kinds.
        let list_msg_m = r.meta("fauna.bridges.list_messages").unwrap();
        assert_eq!(list_msg_m.default_deadline, Duration::from_secs(5));
        assert!(!list_msg_m.forbid_replay);
        let fetch_meta_m = r.meta("fauna.bridges.fetch_message_metadata").unwrap();
        assert_eq!(fetch_meta_m.default_deadline, Duration::from_secs(5));
        assert!(!fetch_meta_m.forbid_replay);
        // C.3 body-fetch + index-segments.
        let fetch_ciph_m = r.meta("fauna.bridges.fetch_message_ciphertext").unwrap();
        assert_eq!(
            fetch_ciph_m.default_deadline,
            Duration::from_secs(60),
            "fetch_message_ciphertext must use 60 s routing deadline (bodies can be tens of MB)"
        );
        assert!(!fetch_ciph_m.forbid_replay);
        let fetch_seg_m = r.meta("fauna.bridges.fetch_index_segments_since").unwrap();
        assert_eq!(fetch_seg_m.default_deadline, Duration::from_secs(5));
        assert!(!fetch_seg_m.forbid_replay);
        // C.4 — store_flags + expunge.
        let store_flags_m = r.meta("fauna.bridges.store_flags").unwrap();
        assert_eq!(store_flags_m.default_deadline, Duration::from_secs(5));
        assert!(!store_flags_m.forbid_replay);
        let expunge_m = r.meta("fauna.bridges.expunge").unwrap();
        assert_eq!(expunge_m.default_deadline, Duration::from_secs(5));
        assert!(!expunge_m.forbid_replay);
        // C.5 — copy + move. Opposite values despite one shared mutation
        // helper: `copy`'s dest UID is server-allocated so a replay duplicates
        // the mail, while `move` consumes its own source rows (82nd-pass audit).
        let copy_m = r.meta("fauna.bridges.copy").unwrap();
        assert_eq!(copy_m.default_deadline, Duration::from_secs(5));
        assert!(
            copy_m.forbid_replay,
            "copy must be forbid-replay: a replay mints a second dest UID and duplicates the mail"
        );
        let move_m = r.meta("fauna.bridges.move").unwrap();
        assert_eq!(move_m.default_deadline, Duration::from_secs(5));
        assert!(!move_m.forbid_replay);
        // C.6 — append.
        let append_m = r.meta("fauna.bridges.append").unwrap();
        assert_eq!(
            append_m.default_deadline,
            Duration::from_secs(60),
            "append must use 60 s routing deadline (encrypted bodies can be tens of MB)"
        );
        assert!(!append_m.forbid_replay);
        // Mailbox-migration imports: the two body-carrying kinds keep the 60 s
        // routing deadline AND are forbid-replay (2026-08-01 audit — progress
        // accumulator + the dedup-less `skip_dedup: true` branch; rationale at
        // both declaration sites).
        for k in [
            "fauna.bridges.import_message",
            "fauna.bridges.import_message_batch",
        ] {
            let m = r.meta(k).unwrap();
            assert_eq!(m.default_deadline, Duration::from_secs(60), "{k}");
            assert!(m.forbid_replay, "{k} must be forbid-replay");
        }
        // D — provision_calendar (5 s fetch deadline).
        let prov_cal_m = r.meta("fauna.bridges.provision_calendar").unwrap();
        assert_eq!(prov_cal_m.default_deadline, Duration::from_secs(5));
        assert!(!prov_cal_m.forbid_replay);
        // D.2 — list_calendars (5 s fetch deadline).
        let list_cal_m = r.meta("fauna.bridges.list_calendars").unwrap();
        assert_eq!(list_cal_m.default_deadline, Duration::from_secs(5));
        assert!(!list_cal_m.forbid_replay);
        // D.3 — query_events (5 s fetch deadline).
        let query_ev_m = r.meta("fauna.bridges.query_events").unwrap();
        assert_eq!(query_ev_m.default_deadline, Duration::from_secs(5));
        assert!(!query_ev_m.forbid_replay);
        // D.4 — put_event_ciphertext (60 s routing deadline: encrypted bodies can be tens of MB).
        let put_ev_m = r.meta("fauna.bridges.put_event_ciphertext").unwrap();
        assert_eq!(
            put_ev_m.default_deadline,
            Duration::from_secs(60),
            "put_event_ciphertext must use 60 s routing deadline (encrypted bodies can be tens of MB)"
        );
        assert!(!put_ev_m.forbid_replay);
        // D.5 — delete_event (5 s fetch deadline: lookup + tombstone insert, no body upload).
        let del_ev_m = r.meta("fauna.bridges.delete_event").unwrap();
        assert_eq!(del_ev_m.default_deadline, Duration::from_secs(5));
        assert!(!del_ev_m.forbid_replay);
        // D.6 — sync_calendar_since (5 s fetch deadline: metadata-only query, no body upload).
        let sync_cal_m = r.meta("fauna.bridges.sync_calendar_since").unwrap();
        assert_eq!(sync_cal_m.default_deadline, Duration::from_secs(5));
        assert!(!sync_cal_m.forbid_replay);
        // E — provision_addressbook (5 s fetch deadline).
        let prov_ab_m = r.meta("fauna.bridges.provision_addressbook").unwrap();
        assert_eq!(prov_ab_m.default_deadline, Duration::from_secs(5));
        assert!(!prov_ab_m.forbid_replay);
        // E.2 — list_addressbooks (5 s fetch deadline).
        let list_ab_m = r.meta("fauna.bridges.list_addressbooks").unwrap();
        assert_eq!(list_ab_m.default_deadline, Duration::from_secs(5));
        assert!(!list_ab_m.forbid_replay);
        // E.3 — query_cards (5 s fetch deadline).
        let query_card_m = r.meta("fauna.bridges.query_cards").unwrap();
        assert_eq!(query_card_m.default_deadline, Duration::from_secs(5));
        assert!(!query_card_m.forbid_replay);
        // E.4 — put_card_ciphertext (60 s routing deadline: encrypted bodies can be large).
        let put_card_m = r.meta("fauna.bridges.put_card_ciphertext").unwrap();
        assert_eq!(
            put_card_m.default_deadline,
            Duration::from_secs(60),
            "put_card_ciphertext must use 60 s routing deadline (encrypted bodies can be large)"
        );
        assert!(!put_card_m.forbid_replay);
        // E.5 — delete_card (5 s fetch deadline: lookup + tombstone insert, no body upload).
        let del_card_m = r.meta("fauna.bridges.delete_card").unwrap();
        assert_eq!(del_card_m.default_deadline, Duration::from_secs(5));
        assert!(!del_card_m.forbid_replay);
        // E.6 — sync_addressbook_since (5 s fetch deadline: metadata-only query, no body upload).
        let sync_ab_m = r.meta("fauna.bridges.sync_addressbook_since").unwrap();
        assert_eq!(sync_ab_m.default_deadline, Duration::from_secs(5));
        assert!(!sync_ab_m.forbid_replay);
        // E.7 — delete_addressbook (5 s fetch deadline: existence check + cascade delete, no body upload).
        let del_ab_m = r.meta("fauna.bridges.delete_addressbook").unwrap();
        assert_eq!(del_ab_m.default_deadline, Duration::from_secs(5));
        assert!(!del_ab_m.forbid_replay);
    }

    /// The **exhaustive** set of replay-forbidden kinds, pinned so a silent
    /// downgrade is impossible.
    ///
    /// Why this exists alongside
    /// `rpc_router::tests::router_and_kind_registry_agree_on_every_kind`: that
    /// gate enforces *agreement* between the nest's table and this one, so a
    /// coordinated edit that flips a kind `true` → `false` in **both** passes
    /// it clean. Agreement is not the property that protects a user — the
    /// values are. `transport.md` § Idempotency and reconnect-with-resume makes
    /// `forbid_replay = false` an assertion that the handler is *naturally
    /// idempotent* (the nest's idempotency cache is per-`RpcConnection`, so it
    /// can never deduplicate an auto-retry, which always lands on a fresh
    /// connection), and this list is the record of which handlers have not
    /// made that claim.
    ///
    /// **Changing this list is the point, not a nuisance.** Adding a kind is
    /// free. *Removing* one asserts that the handler behind it is idempotent
    /// under a repeated same-key call — read the handler and say so in the
    /// commit body, then edit both tables (the parity gate will hold you to
    /// the second edit).
    #[test]
    fn the_replay_forbidden_set_is_exactly_these_kinds() {
        let r = KindRegistry::full();
        let actual: Vec<&str> = r
            .iter()
            .filter(|(_, m)| m.forbid_replay)
            .map(|(k, _)| k)
            .collect();

        // Sorted, because `iter()` walks a BTreeMap.
        let expected = [
            "fauna.admin.invite_codes.create",
            "fauna.bridges.add_follow",
            "fauna.bridges.check_submission_quota",
            // Each send mints a Sent row and an outbox item: a replay would
            // deliver the message twice.
            "fauna.bridges.conversation.send",
            "fauna.bridges.copy",
            "fauna.bridges.create_account_alias",
            "fauna.bridges.create_account_list",
            "fauna.bridges.create_forwarder",
            "fauna.bridges.deliver_sealed_scheduling",
            "fauna.bridges.generate_disposable_alias",
            "fauna.bridges.import_account_aliases",
            "fauna.bridges.import_message",
            "fauna.bridges.import_message_batch",
            "fauna.bridges.link",
            // Every application opens a new stream generation, so a replay
            // would supersede the stream the first one just opened.
            "fauna.bridges.restart_export_session",
            "fauna.bridges.rotate_list_unsubscribe_secret",
            "fauna.bridges.rotate_srs_secret",
            "fauna.bridges.send_list_message",
            // The id is minted server-side and, unlike its import twin, no
            // per-source lock stops a replay: it opens a SECOND session,
            // burning one of § Quota composition's three concurrency slots
            // and leaving a 30-day orphan row the caller cannot name.
            "fauna.bridges.start_export_session",
            "fauna.bridges.start_import_session",
            // Accumulator AND append: a replay would double-count the batch
            // and append the frame twice. The `chunk_idx` guard turns the
            // second attempt into a typed out-of-order refusal, which is a
            // recoverable answer but not one to arrive at blindly.
            "fauna.bridges.upload_export_chunk",
            "fauna.conversations.channel.send",
            "fauna.conversations.channel.send_remote",
            "fauna.conversations.keypackage.fetch",
            "fauna.conversations.room.accept_invite",
            // The sealing plane's two keying acts: both hand out key
            // material, so neither claims idempotence under a repeated
            // same-key call. (The mint's parent ratchet would refuse a
            // replay anyway; the backfill's would land the same rows again,
            // which is harmless but not a claim worth making.)
            "fauna.conversations.room.backfill_generations",
            "fauna.conversations.room.invite",
            // Inherits `invite`'s flag: the room home's federated issue door
            // delivers a knock per call (`register_conversations_room_kinds`).
            "fauna.conversations.room.invite_remote",
            "fauna.conversations.room.leave",
            "fauna.conversations.room.publish_generation",
            "fauna.conversations.room.remove",
            "fauna.conversations.room.revoke_invite",
            // The governance doors (`register_conversations_room_kinds`): a
            // replayed policy write, labeler-set write or ownership transfer
            // is a rollback.
            "fauna.conversations.room.set_labelers",
            "fauna.conversations.room.set_policy",
            // A seat's wrap target: a replayed older set would roll a
            // rotated seat back to a retired key.
            "fauna.conversations.room.set_reception_key",
            "fauna.conversations.room.transfer_ownership",
            "fauna.conversations.welcome.deliver",
            "fauna.email.send",
            // A deposit parks one item per accepted call: a replay would
            // park it twice.
            "fauna.folders.deposit",
            "fauna.inbox.send",
            // Each call mints a key, so a replayed frame must not silently
            // rotate twice — `rotate_srs_secret`'s reason
            // (`register_oauth_kinds`; a genuine duplicate is covered by the
            // router's idempotency cache). The forced arm doubly so: a replay
            // would drop the key the first call just minted.
            "fauna.oauth.force_rotate_issuer_key",
            "fauna.oauth.force_rotate_session_secret",
            "fauna.oauth.rotate_issuer_key",
            #[cfg(feature = "payments")]
            "fauna.payments.claims.mint",
            "fauna.posts.interact",
            "fauna.recovery.escrow.fetch",
            "fauna.recovery.replacement.veto",
            "nostr.events.publish_signed",
        ];

        assert_eq!(
            actual, expected,
            "the replay-forbidden set changed — see this test's doc comment"
        );
    }
}
