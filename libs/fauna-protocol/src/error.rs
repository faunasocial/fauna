//! Wire error type and LocalizedText. Per spec § 1.1 and § 2.5.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use fauna_cbor::Value;

/// Server-localizable text. Wire payloads contain only key + args; the
/// client crate's i18n layer renders the final string.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LocalizedText {
    pub key: String,
    #[serde(default)]
    pub args: BTreeMap<String, String>,
    /// Rule-4 catch-all. `args` is `tstr => tstr`, so a future *typed* arg
    /// (a count, a timestamp) cannot ride in it — it would arrive as a new
    /// top-level key, and this is what preserves it across the relay
    /// described on [`RpcError::extra`].
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

impl LocalizedText {
    pub fn new(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            args: BTreeMap::new(),
            extra: BTreeMap::new(),
        }
    }

    pub fn with_arg(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.args.insert(name.into(), value.into());
        self
    }
}

/// Wire RPC error. Carried in a `Reply.payload` when `ok=false`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: String,
    /// Boxed for the same reason [`details`](Self::details) is, and measured
    /// the same way: rule 4's `extra` catch-all costs 24 bytes on *both* this
    /// struct and [`LocalizedText`], which took `RpcError` to exactly 128 —
    /// and clippy's `result_large_err` fires at **at least** 128, not above it
    /// (measured 2026-09-11; the guard test below had said `<= 128` and would
    /// have passed the very size that trips the lint). Boxing the field takes
    /// the type to 64 bytes — below even the pre-catch-all 80 — so the lint
    /// has real headroom for the first time instead of sitting on its
    /// boundary. Wire-invisible: serde encodes through the box, byte-pinned by
    /// `rpc_error_wire_bytes_pin`. Reads (`err.message.key`) are unchanged via
    /// `Deref`; only construction sites spell the `Box`.
    pub message: Box<LocalizedText>,
    /// Opaque, kind-specific payload. Its *contents* are `Value`, so unknown
    /// structure inside it is preserved verbatim — but it is not a wildcard
    /// for new top-level keys; that is [`RpcError::extra`]'s job.
    ///
    /// Boxed so the *absent* case — every success path, and most errors — pays
    /// 8 bytes instead of `size_of::<Value>()` (96): `RpcError` is reachable
    /// from nearly every fallible function in the tree, and `Result` is as big
    /// as its largest variant. Boxing is invisible on the wire (serde encodes
    /// through the box; byte-pinned by `rpc_error_wire_bytes_pin` below) and
    /// keeps the type under clippy's default `result_large_err` threshold
    /// (decision record: `transport.md` § Wire format → In-memory
    /// representation).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub details: Option<Box<Value>>,
    /// Rule-4 catch-all (`transport.md` § Schema and forward-compat
    /// discipline, rule 4), added 2026-09-11.
    ///
    /// `RpcError` is not merely decoded at the edge — it is **relayed**: a
    /// federating nest decodes a *peer* nest's error off the federation
    /// channel (`federation_pool::decode_peer_reply`) and re-emits it to its
    /// own client untouched (`rpc_errors::map_peer_relay_error`). Without the
    /// catch-all that re-encode silently drops any top-level key a newer peer
    /// added — the "harmless for compat, lossy for relay" gap rule 4 exists to
    /// close — and the client, which may itself be newer than the relaying
    /// nest, never sees it.
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

impl RpcError {
    pub fn new(code: impl Into<String>, message_key: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: Box::new(LocalizedText::new(message_key)),
            details: None,
            extra: BTreeMap::new(),
        }
    }

    /// Attach a free-form text `details` payload (CBOR-encoded as text).
    /// Convenience for the common case where handlers want to surface
    /// an error description without inventing a per-error struct shape.
    pub fn with_details_text(mut self, details: impl Into<String>) -> Self {
        self.details = Some(Box::new(Value::String(details.into())));
        self
    }

    /// The decoder-side twin of [`Self::with_details_text`]: the free-form
    /// `details` text if present, else the wire `code`. Every caller that
    /// wants a human-readable string for a *decoded* `RpcError` used to
    /// hand-roll this exact fallback independently as a local `rpc_detail`
    /// helper (scouted 2026-08-19, byte-identical across 6 `*-machine`
    /// crates' `nest_api/ws_rpc.rs`); this is the one shared home.
    pub fn detail_or_code(&self) -> String {
        match self.details.as_deref() {
            Some(Value::String(s)) => s.clone(),
            _ => self.code.clone(),
        }
    }

    /// The last dotted segment of the wire [`code`](Self::code) — `not_found`
    /// out of `fauna.media.not_found` — which is what a client's per-feature
    /// error mapping actually keys on: the namespace names *which* handler
    /// refused, and the same refusal reaches a client under several namespaces
    /// (`transport.md` § `RpcError` shape: codes are `fauna.<area>.<error>`).
    ///
    /// The rule lives here, beside [`detail_or_code`](Self::detail_or_code) and
    /// [`is_guardian_approval_required`](Self::is_guardian_approval_required),
    /// for the reason that one says out loud: a client must not re-derive the
    /// code grammar for itself. It was re-derived anyway — six `*-machine`
    /// crates' `nest_api/ws_rpc.rs::map_err` each spelled
    /// `rpc.code.rsplit('.').next().unwrap_or("")` inline, the same six files
    /// `detail_or_code` was lifted out of, in the same function (found
    /// 2026-08-23). What each crate keeps is its own suffix→variant arm table,
    /// which is genuinely per-feature; the grammar is not.
    ///
    /// A code carrying no dot yields the whole string — the pre-lift behaviour,
    /// preserved deliberately so a malformed or namespace-less code still falls
    /// through each caller's `_ => Transient` arm rather than matching nothing
    /// in particular.
    pub fn code_suffix(&self) -> &str {
        self.code.rsplit('.').next().unwrap_or("")
    }

    /// The nest is running an **outdated version** relative to its own at-rest
    /// database — it cannot safely operate a DB written by a newer nest carrying
    /// a breaking schema change (version-compatibility.md § 2.2). A nest in this
    /// state boots into a degraded "needs-update" mode and answers every RPC
    /// with this error rather than crash-looping (`nest/common.md`
    /// § Client-state recoverability) or leaking a raw SQL string (Dim 4).
    ///
    /// The actionable, admin-facing meaning is "this nest must be updated"
    /// (not a transient/connectivity failure) so a client can route the user to
    /// an update prompt. Clients match on [`Self::CODE_NEST_OUTDATED`].
    pub fn nest_outdated() -> Self {
        Self::new(Self::CODE_NEST_OUTDATED, "error.nest.outdated")
    }

    /// Stable wire code for [`Self::nest_outdated`]. A shared symbol so the nest
    /// (emitter) and clients (Track 3 renderer) match on one constant rather
    /// than a stringly-typed literal in two places.
    pub const CODE_NEST_OUTDATED: &'static str = "fauna.nest.outdated";

    /// The nest hit a **query-time** schema/version mismatch — a handler ran a
    /// query against its own database and got back a `no such column …` /
    /// `no such table …`, i.e. the running binary's SQL shape does not match the
    /// DB shape (version-compatibility.md § 2.2 / Dimension 4). This is the case
    /// the *boot-time* `schema_meta` gate ([`Self::nest_outdated`]) structurally
    /// cannot catch — it surfaces mid-session, per RPC, not at boot.
    ///
    /// It carries the same actionable meaning as [`Self::nest_outdated`] ("this
    /// nest must be updated") and routes to the same [`RpcErrorAction::NeedsUpdate`]
    /// — but is a **distinct code** so the two are separable (boot-time downgrade
    /// verdict vs. escaped query-time mismatch) and so it never collides with the
    /// degraded-serve emitter. **No raw SQL rides in `details`** — the whole point
    /// is that a column/table name is never leaked to the client (Dim 4 target:
    /// "never a server SQL phrase").
    pub fn nest_schema_mismatch() -> Self {
        Self::new(
            Self::CODE_NEST_SCHEMA_MISMATCH,
            "error.nest.schema_mismatch",
        )
    }

    /// Stable wire code for [`Self::nest_schema_mismatch`]. Matched by
    /// [`Self::action`] (→ `NeedsUpdate`) and [`Self::localized`].
    pub const CODE_NEST_SCHEMA_MISMATCH: &'static str = "fauna.nest.schema_mismatch";

    /// `fauna.profile.get`'s "this account has never published a profile"
    /// refusal. Named here rather than spelled at each site because it is the
    /// one profile refusal a *caller* branches on rather than surfaces: a
    /// read-modify-write of the signed profile treats it as "no base document"
    /// and mints a minimal one (`fauna-client-profile::publish_recovery_head`),
    /// where every other failure must abort the write. Produced by
    /// `bins/fauna-nest/src/profile_handlers.rs`.
    pub const CODE_PROFILE_NOT_FOUND: &'static str = "fauna.profile.not_found";

    /// `fauna.posts.get`'s "no such post for this caller" refusal — a post that was
    /// deleted, never existed, or is quarantine-gated for the caller. Named for the
    /// same reason as [`Self::CODE_PROFILE_NOT_FOUND`]: a caller branches on it. The
    /// feed's quoted-post resolve folds the not-found embed on exactly this code and
    /// retries on anything else (`ui/feed.md` § Post deletion). Produced by
    /// `bins/fauna-nest/src/posts_handlers.rs` (`not_found_ns("posts", …)`).
    pub const CODE_POST_NOT_FOUND: &'static str = "fauna.posts.not_found";

    /// `fauna.sync.device_grant.register`'s "this device key is TOMBSTONED"
    /// refusal — the nest's own memory that the user ended this identity by
    /// deleting its device row (`revoked_device_grants`; the 2026-08-15
    /// revocation hardening). Distinct from the generic
    /// `fauna.sync.invalid_grant` (bad capability / bad signature) because it
    /// is the one grant refusal a *caller* acts on rather than retries — it
    /// raises the loud removed-from-account state (`sync-agent.md`
    /// § Credential model, row 47 decision 4), and a ceremony-capable sign-in
    /// seeing it mints a SUCCESSOR principal — the evidence gate of principal
    /// succession (`account-data-plane.md` § The store device principal →
    /// *Principal succession after a device delete*, decision 1).
    /// Deliberately emitted only on the **authenticated** register path: the
    /// anonymous `fauna.auth.device_handshake` stays opaque
    /// (`fauna.auth.not_registered`) so a thief probing a dead key learns
    /// nothing. Additive (I4): a client that does not match this code treats it like
    /// any other failure and stays in the retry loop. Produced by
    /// `bins/fauna-nest/src/sync_handlers.rs`; matched by
    /// `fauna_client_sync::is_device_grant_revoked`.
    pub const CODE_SYNC_DEVICE_GRANT_REVOKED: &'static str = "fauna.sync.device_grant_revoked";

    /// `fauna.sync.register`'s tier device-cap refusal: a **new** `device_id`
    /// past the caller's `max_devices`, with nothing written (`devices.md`
    /// § Step 4). A re-register of a device the actor already holds is never
    /// refused this way, and a nest with tier quotas off never emits it. It
    /// clears only when a slot frees (the user removes a device, or the admin
    /// moves the account to a bigger tier), so a caller names it with that
    /// remedy instead of reading it as an offline nest. Produced by
    /// `bins/fauna-nest/src/sync_handlers.rs` (`device_quota_err`); matched by
    /// `fauna_client_sync::is_device_limit_exceeded`.
    pub const CODE_SYNC_DEVICE_LIMIT_EXCEEDED: &'static str = "fauna.sync.device_limit_exceeded";

    /// A write past the account's one storage allowance — shared by mail,
    /// calendar and contacts (`caldav-server.md` § QUOTA → § Enforcement
    /// points; `imap-server.md` § Quota enforcement points). Nothing was
    /// stored; it clears only when the user frees space or the admin raises
    /// the ceiling. Minted by the nest's `bridge_routing_handlers::over_quota`;
    /// the Go MDA answers it `NO [OVERQUOTA]` on IMAP and `507` on CalDAV and
    /// CardDAV, the Go MTA `552 5.2.2`.
    pub const CODE_BRIDGES_OVER_QUOTA: &'static str = "fauna.bridges.over_quota";

    /// The storage-quota refusal of every custody-recording door, and the
    /// ratified honest outcome of a re-seed whose corpus outgrows the fresh
    /// nest's quota (`backup-destinations.md` § Re-seed, phase 2) — raised in
    /// the admin app like any other, never a re-seed special case. Produced by
    /// `bins/fauna-nest/src/sync_handlers.rs` (`quota_err`).
    pub const CODE_SYNC_STORAGE_QUOTA_EXCEEDED: &'static str = "fauna.sync.storage_quota_exceeded";

    /// `fauna.backup.custody.materialize`'s empty-target refusal: the scope or
    /// folder already holds live records the ceremony did not write. No force
    /// arm exists; the remedy is pointing at another nest. Also what a
    /// **repeat** of a materialize that already succeeded answers, which is
    /// why a ceremony driver reads it as "this nest already holds this corpus"
    /// rather than as a failure to retry. The emitter is
    /// `bins/fauna-nest/src/backup_handlers.rs` (`materialize_error`), pinned
    /// equal to these constants by that module's tests; the classifier is
    /// `fauna_client_backup::reseed`.
    pub const CODE_BACKUP_TARGET_NOT_EMPTY: &'static str = "fauna.backup.target_not_empty";

    /// The empty-target rule's other half: the named folder is empty but
    /// carries a publication-bearing property (an audience already exists), so
    /// re-homing into it would publish the corpus to a roster the owner never
    /// chose. Remedy: name another folder or clear the property.
    pub const CODE_BACKUP_TARGET_NOT_FRESH: &'static str = "fauna.backup.target_not_fresh";

    /// The covered-folder arm's target set does not exist on the nest: the
    /// ceremony's seed-holding process creates it through the shared create
    /// helper before materializing, and the nest never creates one
    /// (`writer-signed-change-records.md` ruling (7)(a)(i)). Remedy: the caller
    /// prepares the set, then retries.
    pub const CODE_BACKUP_TARGET_MISSING: &'static str = "fauna.backup.target_missing";

    /// The covered-folder arm's request carried no owner signatures — the arm
    /// never mints an unsigned row (ruling (7)(a)(ii)).
    pub const CODE_BACKUP_SIGNATURE_REQUIRED: &'static str = "fauna.backup.signature_required";

    /// `fauna.folders.update`'s refusal of a WebDAV flip OFF while an adoptable
    /// pseudo-device row of the set is unsigned (`writer-signed-change-records.md`
    /// ruling (7)(b)); `details` carries `{ "unadopted": <count> }`. Remedy: the
    /// serve-disable composition's sweep (`fauna.folders.served_rows.adopt`),
    /// then the flip again — the composition does both on its bounded retry.
    pub const CODE_FOLDERS_SERVED_ROWS_UNADOPTED: &'static str =
        "fauna.folders.served_rows_unadopted";

    /// `fauna.folders.served_rows.adopt`'s whole-page refusal of a row
    /// `sync_writer_sig::served_row_adoptable` rejects (ruling (7)(b)(i));
    /// `details` names the seq. An honest sweep never sends one.
    pub const CODE_FOLDERS_SERVED_ROWS_UNADOPTABLE: &'static str =
        "fauna.folders.served_rows_unadoptable";

    /// The DAV recorder's refusal of a non-delete record carrying no
    /// `content_key_version` (ruling (7)(b)(i)(3)) — a guard that fires only on
    /// a wiring bug, beside `fauna.bridges.path_seal_required`.
    pub const CODE_BRIDGES_CONTENT_KEY_VERSION_REQUIRED: &'static str =
        "fauna.bridges.content_key_version_required";

    /// The corpus on the target is not whole yet (a missing mirror, a segment
    /// without its sidecar, an empty folder set): let delivery finish, then
    /// retry. Needs nothing else from the owner.
    pub const CODE_BACKUP_CUSTODY_INCOMPLETE: &'static str = "fauna.backup.custody_incomplete";

    /// A covered-folder path the custodian holds without the sealed name the
    /// folder arm re-homes from. Remedy: a fresh pull pass on the custodian,
    /// then a re-delivery.
    pub const CODE_BACKUP_CUSTODY_UNSEALED: &'static str = "fauna.backup.custody_unsealed";

    /// Materialize with no `NestBackupKey` grant on the target — the ceremony's
    /// enrollment step did not run (or was revoked since).
    pub const CODE_BACKUP_NOT_ENROLLED: &'static str = "fauna.backup.not_enrolled";

    /// `fauna.backup.custodian.checkin`'s refusal when the owner's registry
    /// holds no client-device row by that id for the checking-in device:
    /// the destination was removed, or it is registered to another device. It
    /// is the authority's answer that this device is no longer that
    /// destination's custodian, so a host that gets it re-reads its assignment
    /// immediately instead of waiting for its next rediscovery. It is the
    /// signal and not the verdict; the re-read decides. A typed code rather
    /// than `fauna.protocol.malformed` with a details string, so the host can
    /// never mistake a transport fault or a reworded message for it.
    pub const CODE_BACKUP_CUSTODIAN_NOT_ASSIGNED: &'static str =
        "fauna.backup.custodian_not_assigned";

    /// `fauna.account.state.put`'s refusal of a `(scope, writer, writer_seq)`
    /// coordinate the nest will **never** accept this row at: the seq does not
    /// advance the writer's head for the item (a replay of a row it already
    /// holds, or a row behind a later life's), or the coordinate was already
    /// spent on any item (a burnt journal — `account-replica-posture.md`
    /// § The store device principal, refinement 11). One code for both by the
    /// nest's design (`StateEntryError` in `bins/fauna-nest/src/db/
    /// account_state.rs`): the client's remedy is the same either way — the
    /// local row stays, the walk's own verdict tells a replay (self-echo)
    /// from a burn (rotation), and the relay row the publish recorded before
    /// the send is retired, because the nest has given its final word on the
    /// coordinate (refinement 11 → *a refused row's relay residue*). Matched
    /// by [`is_account_state_stale_writer_seq`](Self::is_account_state_stale_writer_seq).
    pub const CODE_ACCOUNT_STATE_STALE_WRITER_SEQ: &'static str =
        "fauna.account.state.stale_writer_seq";

    /// True when the nest refused a class-2 publish at its coordinate for
    /// good ([`CODE_ACCOUNT_STATE_STALE_WRITER_SEQ`](Self::CODE_ACCOUNT_STATE_STALE_WRITER_SEQ)).
    /// A whole-code match: the refusal is emitted under exactly one
    /// namespace, unlike the guardian-approval family below.
    pub fn is_account_state_stale_writer_seq(&self) -> bool {
        self.code == Self::CODE_ACCOUNT_STATE_STALE_WRITER_SEQ
    }

    /// The authenticating identity has been **succeeded** — its account was
    /// re-pointed to `new_actor_id` by a RecoveryKey-authorized succession
    /// (`docs/goal/behavior/identity-succession.md` § Enforcement on the home
    /// nest, step 4). Every seed-signature ceremony and the emergency lockout
    /// answer with this instead of minting anything.
    ///
    /// **This is the refusal that makes a stolen seed inert.** Signature
    /// verification in Fauna is self-describing — `verify_envelope` reads the
    /// pubkey out of the payload — so a thief's signatures keep verifying
    /// forever on their own merits; only a table consult can stop honoring
    /// them, and this is that consult's wire form.
    ///
    /// It is deliberately **not** collapsed into `fauna.auth.not_registered`.
    /// That code is opaque on purpose (it must not distinguish unknown from
    /// suspended), but supersession is the opposite case: the *owner* needs to
    /// be told exactly what happened and where to go, because the honest client
    /// hitting this is the succeeded user's own device, whose next step is to
    /// import the successor identity (§ Propagation). So the error names the
    /// successor and the kind that serves the proof.
    ///
    /// The successor id is public by construction (it rides the statement, which
    /// is distributed to peers, MLS members and federation), so naming it leaks
    /// nothing a `lookup` call would not already answer.
    pub fn superseded(new_actor_id: &[u8; 32]) -> Self {
        let hex = hex::encode(new_actor_id);
        Self {
            code: Self::CODE_SUPERSEDED.to_string(),
            message: Box::new(
                LocalizedText::new("error.auth.superseded").with_arg("new_actor_id", hex.clone()),
            ),
            details: Some(Box::new(Value::Map(BTreeMap::from([
                (
                    "new_actor_id".to_string(),
                    Value::Bytes(new_actor_id.to_vec()),
                ),
                ("new_actor_id_hex".to_string(), Value::String(hex)),
                // Where to fetch the statement (`identity-succession.md:71`).
                // Named here rather than assumed, so a client that hits this
                // refusal needs no out-of-band knowledge to verify the claim —
                // and so it verifies it rather than trusting this reply, which
                // the nest is not authorized to make on its own.
                (
                    "statement_kind".to_string(),
                    Value::String(Self::SUCCESSION_LOOKUP_KIND.to_string()),
                ),
            ])))),
            extra: BTreeMap::new(),
        }
    }

    /// Stable wire code for [`Self::superseded`].
    pub const CODE_SUPERSEDED: &'static str = "fauna.auth.superseded";

    /// The sign-in refusal: this nest does not sign the identity in —
    /// unknown, suspended or removed, deliberately indistinguishable on the
    /// wire (no oracle). Answered by `fauna.auth.verify` and the anonymous
    /// handshakes. A client that held a bearer and meets it on a re-mint was
    /// cut off mid-session, and lands the launch surface's
    /// previously-signed-in row (`onboarding.md` § App-launch routing).
    pub const CODE_NOT_REGISTERED: &'static str = "fauna.auth.not_registered";

    /// The [`CODE_NOT_REGISTERED`](Self::CODE_NOT_REGISTERED) refusal, with
    /// the message key the nest sends it under.
    pub fn not_registered() -> Self {
        Self::new(Self::CODE_NOT_REGISTERED, "error.auth.not_registered")
    }

    /// True for a [`Self::not_registered`] refusal.
    pub fn is_not_registered(&self) -> bool {
        self.code == Self::CODE_NOT_REGISTERED
    }

    /// Stable wire code of the account-lockout refusal, answered by the
    /// handshakes and by `fauna.auth.verify` (`login.md` § Silent Challenge).
    pub const CODE_ACCOUNT_LOCKED: &'static str = "fauna.auth.account_locked";

    /// The `locked_until` (Unix seconds) carried by an
    /// [`CODE_ACCOUNT_LOCKED`](Self::CODE_ACCOUNT_LOCKED) refusal, if this is
    /// one and it is well-formed. `None` for any other code or a payload that
    /// is not an integer.
    pub fn locked_until_secs(&self) -> Option<u64> {
        if self.code != Self::CODE_ACCOUNT_LOCKED {
            return None;
        }
        match self.details.as_deref() {
            Some(Value::Integer(n)) => u64::try_from(*n).ok(),
            _ => None,
        }
    }

    /// The pre-identity kind that serves the succession statement proving a
    /// [`Self::superseded`] refusal. A shared symbol so the emitter and every
    /// app match one constant rather than a stringly literal in eight places.
    pub const SUCCESSION_LOOKUP_KIND: &'static str = "fauna.recovery.succession.lookup";

    /// The successor actor id carried by a [`Self::superseded`] refusal, if this
    /// is one and it is well-formed.
    ///
    /// Returns `None` for any other code — so a client cannot accidentally read
    /// a successor out of an unrelated error and act on it.
    pub fn superseded_by(&self) -> Option<[u8; 32]> {
        if self.code != Self::CODE_SUPERSEDED {
            return None;
        }
        let Some(Value::Map(map)) = self.details.as_deref() else {
            return None;
        };
        match map.get("new_actor_id") {
            Some(Value::Bytes(b)) => b.as_slice().try_into().ok(),
            _ => None,
        }
    }

    /// The stable **suffix** of the namespaced guardian-approval refusal
    /// (`fauna.{ns}.guardian_approval_required`). Not a whole code, because the
    /// namespace names *which* handler refused — `inbox` and `knocks` for a
    /// ward's outbound contact attempt, `bridges` for a blocked feed source,
    /// `conversations` and `account` for their own gated actions — and an app
    /// offering "ask your guardian" cares only that a guardian must approve.
    /// Match with [`is_guardian_approval_required`](Self::is_guardian_approval_required).
    pub const CODE_SUFFIX_GUARDIAN_APPROVAL_REQUIRED: &'static str = ".guardian_approval_required";

    /// True when the nest refused this action because a guardian must approve it
    /// (`family-safety.md` § Guardian policy pillar 1 — the ward's send to a
    /// non-contact, a blocked feed source, …).
    ///
    /// This is the hinge the ward-side ask affordances hang off: the refusal is
    /// *typed* precisely so an app can replace a dead error banner with
    /// "ask your guardian" (`contact-request-guardian-button` /
    /// `bridge-source-request-button`, §§ Child-initiated contact requests /
    /// Feed-source approvals). Shared here rather than per app so the namespace
    /// set can grow nest-side without seven apps each learning the new one — and
    /// so nobody re-derives it as a `contains()` that a future
    /// `…_required_notice` code would trip.
    pub fn is_guardian_approval_required(&self) -> bool {
        // Whole-suffix match on a non-empty namespace: `fauna.<ns>.<suffix>`.
        self.code
            .strip_suffix(Self::CODE_SUFFIX_GUARDIAN_APPROVAL_REQUIRED)
            .and_then(|head| head.strip_prefix("fauna."))
            .is_some_and(|ns| !ns.is_empty() && !ns.contains('.'))
    }

    /// How a client should route this error in the UI — the shared
    /// classification seam for version-compatibility.md **Dimension 4** ("a
    /// version/schema mismatch must be *distinguishable* from transient errors so
    /// the client routes the user to an update prompt vs. a retry"). Keyed on the
    /// stable wire [`code`](Self::code), so the *one* mapping here is consumed by
    /// every app surface (the launch machine, the conversations rail, …)
    /// instead of each re-deriving "is this retryable?" from a stringly match.
    ///
    /// - [`RpcErrorAction::NeedsUpdate`] — software is out of date (today only
    ///   [`CODE_NEST_OUTDATED`](Self::CODE_NEST_OUTDATED), the degraded-serve
    ///   boot mode); route to an actionable update prompt, never an auto-retry.
    /// - [`RpcErrorAction::Transient`] — a transport/connectivity/server-transient
    ///   protocol fault; safe to auto-retry.
    /// - [`RpcErrorAction::Rejected`] — a definite refusal that won't change on
    ///   retry (auth/permission/not-found/malformed/unknown-kind, and any code
    ///   this seam doesn't recognise); show the localized message, don't spin.
    pub fn action(&self) -> RpcErrorAction {
        match self.code.as_str() {
            Self::CODE_NEST_OUTDATED
            | Self::CODE_NEST_SCHEMA_MISMATCH
            // A cross-nest relay reached a peer nest that predates the required
            // federation kind (S5, `federation.md` § Cross-nest shared folders
            // + channel append): software out of date — the *peer's* — so the
            // update affordance, never an auto-retry or an auth framing.
            | "fauna.federation.peer_nest_outdated" => RpcErrorAction::NeedsUpdate,
            // Supersession is permanent and actionable, never retryable: the
            // identity this client authenticates as will *never* work again.
            // Retrying it silently is the one behavior that would strand the
            // user in a reconnect loop instead of routing them to the import.
            Self::CODE_SUPERSEDED => RpcErrorAction::Rejected,
            // The tier device-cap refusal clears only when a slot frees (the
            // user removes a device, or the admin raises the tier) — never on
            // its own, so never `Transient`. It falls to `Rejected` by default
            // anyway; the explicit arm exists because rendering and
            // classification move together (`localized` carries this code's
            // exact arm), so a later reader finds both halves side by side.
            Self::CODE_SYNC_DEVICE_LIMIT_EXCEEDED => RpcErrorAction::Rejected,
            "fauna.protocol.timeout"
            | "fauna.protocol.disconnected"
            | "fauna.protocol.resync_required"
            | "fauna.protocol.internal"
            | "fauna.protocol.encode_failed"
            // The post-commit mail/CalDAV/CardDAV placement-journal append
            // failed after its SQLite mutation already committed (nest
            // `rpc_errors::placement_journal_diverged`) — same retry-safety
            // as the generic internal error it distinguishes itself from.
            | "fauna.bridges.placement_journal_diverged" => RpcErrorAction::Transient,
            // The ONE `rate_limited` code that is not a plain rolling window,
            // so it must be matched BEFORE the family arm below.
            // `email_handlers` mints it for the hourly outbound send-rate
            // window *and* for the per-actor recipients-per-day submission
            // quota (`SubmissionQuotaOutcome::OverQuota`). A client backing off
            // on the latter would spin for up to a day, so this code keeps the
            // no-auto-retry framing until the nest splits the two causes.
            "fauna.email.rate_limited" => RpcErrorAction::Rejected,
            // The **rate-limit family**. Every `fauna.{ns}.rate_limited` comes
            // from one nest seam (`rpc_errors::rate_limited_ns`) and means one
            // thing: a rolling window refused this call, and the same call
            // succeeds once the window rolls. Keyed on the family (the code's
            // LAST segment, the same join `localized_family` uses) so a
            // namespace added later is classified on arrival.
            //
            // ⚠ This arm is a **transport-confidentiality** control, not just a
            // retry hint. `FaunaMlsBackend::resolve_foreign` consults this
            // classification to decide whether a foreign nest *answered* about
            // a handle; while `fauna.protocol.rate_limited` fell to the default
            // `Rejected` arm below, a peer nest throttling the anonymous
            // `fauna.actor.by_handle` discovery probe read as "not a Fauna
            // recipient here" and the chain downgraded a **known** Fauna peer
            // to plaintext SMTP (`federation.md` § Peer-auth model →
            // *Discovery-failure semantics*, case 2). Members: the anonymous
            // per-source throttle (`anonymous_rate_limit`, 60/60 s), the
            // non-claimant folder-channel commit cap (`bridge_rate_limit::
            // CHANNEL_COMMIT_LIMITER_CONFIG`, 3600 s, `federation.md`
            // § residual (a)), and the bridge/moderation rails (60 s).
            _ if self.code_suffix() == "rate_limited" => RpcErrorAction::Transient,
            // Default to Rejected (show the message, no auto-retry) rather than
            // Transient: an unrecognised future code spun in a retry loop would
            // wedge silently, whereas surfacing its localized message is always
            // safe and actionable. ⚠ Because this default is an OPEN set, it
            // must never be the sole input to a downgrade decision — see the
            // family arm's note and `remote_by_handle_outcome`'s allowlist.
            _ => RpcErrorAction::Rejected,
        }
    }

    /// Render this error's user-facing localized message, keyed on the stable
    /// wire [`code`](Self::code) (Dimension 4: clients render the localized
    /// `message`, *not* the raw `details` SQL/internal string). Matching on
    /// `code` rather than the dynamic `message.key` lets every app share this
    /// one renderer, and keeps the join on the half of the wire error that is
    /// contractual: `message.key` is wire-level diagnostic by design and 248
    /// non-test `error.*` keys have no string in `en.yaml` at all, so resolving
    /// the key through [`fauna_i18n::strings::lookup`] would render *nothing*
    /// for most codes. (That resolver does exist — an earlier revision of this
    /// comment claimed the generated layer had no runtime key→string resolver,
    /// which was already false; apps consume it, e.g. `fauna-linux`'s
    /// `i18n::mod`. It is simply the wrong join for this path.)
    ///
    /// Two tiers, in order:
    ///
    /// 1. **Exact code** — a curated, specific string for one code. Add an arm
    ///    here whenever a namespace's refusal deserves its own sentence.
    /// 2. **Namespaced family fallback** ([`Self::localized_family`]) — the
    ///    nest mints its refusals as `fauna.{ns}.{family}` from the shared
    ///    `rpc_errors::*_ns` seams, so the family lives in the code's LAST
    ///    segment and is the same across every namespace. Keying on that
    ///    segment covers a namespace added later *on arrival*, instead of
    ///    falling through to the generic string until someone notices — which
    ///    is exactly what had happened to the whole authorization family
    ///    (15 namespaces, 35 call sites rendering "Please try again" for a
    ///    permanent "you may not").
    ///
    /// Anything matching neither tier falls back to a generic localized string
    /// (their own typed handlers, e.g. the auth ceremonies, render their own
    /// messages; this is the "I just have an `RpcError`" path).
    pub fn localized(&self) -> &'static str {
        use fauna_i18n::strings::error;
        match self.code.as_str() {
            Self::CODE_NEST_OUTDATED => error::nest::OUTDATED,
            Self::CODE_NEST_SCHEMA_MISMATCH => error::nest::SCHEMA_MISMATCH,
            "fauna.protocol.unknown_kind" => error::protocol::UNKNOWN_KIND,
            "fauna.federation.peer_nest_outdated" => error::federation::PEER_NEST_OUTDATED,
            "fauna.protocol.malformed" | "fauna.protocol.malformed_error" => {
                error::protocol::MALFORMED
            }
            "fauna.protocol.timeout" => error::protocol::TIMEOUT,
            "fauna.protocol.cancelled" => error::protocol::CANCELLED,
            "fauna.protocol.internal" => error::protocol::INTERNAL,
            "fauna.protocol.encode_failed" => error::protocol::ENCODE,
            "fauna.protocol.replay_too_large" => error::protocol::REPLAY_TOO_LARGE,
            "fauna.protocol.disconnected" => error::protocol::DISCONNECTED,
            crate::email::MESSAGE_TOO_LARGE_CODE => error::email::TOO_LARGE,
            crate::email::SENDER_HANDLE_DENIED_CODE => error::email::PERMISSION_DENIED,
            // The rate-limit family's two exact-code arms, both outranking the
            // family fallback for the same reason the doc gives above — the
            // namespace says more than the family can. The conversations cap
            // names the wait; the email code covers a daily quota as well as an
            // hourly window, so it must NOT promise "a moment" (and is the one
            // family member `action()` keeps `Rejected` — rendering and
            // classification move together).
            "fauna.conversations.rate_limited" => error::conversations::RATE_LIMITED,
            "fauna.email.rate_limited" => error::email::RATE_LIMITED,
            // The reach-policy refusal says more than the generic authorization
            // family below (it names the contact-request remedy), so its exact
            // arm wins. The string had been in the catalog — carrying a comment
            // naming this very code — while no arm reached it, which is how the
            // family gap below stayed invisible: the one namespace anybody had
            // written a string for rendered the generic fallback too.
            "fauna.conversations.forbidden" => error::conversations::FORBIDDEN,
            // The tier device-cap refusal (`devices.md` § Step 4) — "a typed
            // error the app renders so the user can ask their admin for a
            // bigger tier". It carried no arm from 2026-09-02 (the day the nest
            // began emitting it) to 2026-09-15, so every app showed the generic
            // fallback for a permanent, remediable refusal. The same string is
            // the Devices page's standing enrollment notice
            // (`EnrollmentRefusal::notice`); `action()` keeps it `Rejected`.
            Self::CODE_SYNC_DEVICE_LIMIT_EXCEEDED => error::sync::DEVICE_LIMIT_EXCEEDED,
            // A handle someone else holds (`settings.md` § User actions: the
            // change is "refused with your nest's reason"). Every app rendered
            // the generic fallback for this permanent, remediable refusal
            // until 2026-09-22 — the reason only a nest can know, lost at the
            // last hop. The registration twin says the same thing.
            "fauna.profile.handle_taken" | "fauna.account.handle_taken" => {
                error::profile::HANDLE_TAKEN
            }
            "fauna.profile.handle_cooldown" => error::profile::HANDLE_COOLDOWN,
            // A list member on a hosted domain (`mail-mass-mailing.md` § Don't
            // do these): the refusal must point at aliases, the remedy.
            "fauna.bridges.recipient_on_local_domain" => error::bridges::RECIPIENT_ON_LOCAL_DOMAIN,
            // A write past the one storage allowance (mail, calendar and
            // contacts share it): the refusal names the remedy — free space or
            // a bigger allowance — instead of the generic fallback it rendered
            // until the DAV writes began enforcing the quota.
            Self::CODE_BRIDGES_OVER_QUOTA => error::bridges::OVER_QUOTA,
            // A forward-all target on a hosted domain (`mail-forwarding.md`
            // § Don't do these) — the same remedy, an alias.
            "fauna.bridges.forward_target_on_local_domain" => {
                error::bridges::FORWARD_TARGET_ON_LOCAL_DOMAIN
            }
            // Tier 2: the namespaced families. Never widen this into a
            // `_ => lookup(&self.message.key)`; see this method's doc comment.
            _ => self.localized_family(),
        }
    }

    /// Tier 2 of [`Self::localized`]: render a `fauna.{ns}.{family}` code from
    /// its **family** — [`Self::code_suffix`], the shared spelling of the code
    /// grammar, never a re-derived `rsplit` (that helper's own doc records the
    /// six crates that re-derived it) — so one arm covers every namespace that
    /// mints through the nest's shared `rpc_errors::*_ns` seams.
    ///
    /// Only families whose meaning is genuinely namespace-independent belong
    /// here. Authorization is the clearest such family: `permission_denied`,
    /// `forbidden` and `not_owner` all say the same thing to a user — *you may
    /// not* — whichever plane refused, and deliberately say no more than that,
    /// so a caller cannot probe which check fired (the same reasoning behind
    /// `conversations.forbidden` being one string for all of its causes).
    ///
    /// ⚠ A family whose *classification* would also have to change does NOT
    /// belong here on rendering grounds alone: rendering and classification
    /// move together or not at all. `rate_limited` used to be this rule's live
    /// counter-example — it was rendered nowhere because [`Self::action`]
    /// classified only `fauna.conversations.rate_limited` as `Transient`. That
    /// partial classification turned out to be a security defect once
    /// `resolve_foreign` began reading it (see [`Self::action`]'s family arm),
    /// so **both** halves moved together: the family is `Transient` there and
    /// carries a shared string here, with `fauna.email.rate_limited` carved out
    /// of both (it stays `Rejected` and keeps its own exact-code arm).
    fn localized_family(&self) -> &'static str {
        use fauna_i18n::strings::error;
        match self.code_suffix() {
            // `permission_denied` — the per-namespace seam and the central
            // `fauna.bridges.permission_denied` (an unknown or revoked actor).
            // `forbidden` — the structural gates (federation's 403 arm, the
            // `bare_forbidden_ns` admin refusals). `not_owner` — the door's
            // uniform owner-scoping refusal, deliberately identical whether the
            // row is absent or belongs to a stranger.
            "permission_denied" | "forbidden" | "not_owner" => error::AUTHORIZATION,
            // `rate_limited` — the rolling-window refusals the nest mints from
            // `rpc_errors::rate_limited_ns`. One sentence for every namespace,
            // matching the family's `Transient` classification in
            // [`Self::action`]. `fauna.conversations.rate_limited` and
            // `fauna.email.rate_limited` have exact-code arms that win.
            "rate_limited" => error::RATE_LIMITED,
            _ => error::UNEXPECTED,
        }
    }

    /// Log this error's admin-debug `details` — the log is the ONLY honest
    /// home for it. Every `Display` impl over a client error type carrying an
    /// `RpcError` must keep `details` out of the rendered string (the ratified
    /// contract: no diagnostic ever reaches `error-message`,
    /// `conversations.md` § Errors & edge cases), so without this call the
    /// nest's own explanation of what went wrong crosses the wire and is then
    /// discarded with no other channel anywhere. `kind` is the RPC kind that
    /// failed, so log entries group by call site — without it, three very
    /// different failure sources (a blob-store write, a SQLite CAS conflict, a
    /// missing reserved folder) all read as one indistinguishable
    /// `fauna.protocol.internal`.
    pub fn log_operator_details(&self, kind: &str) {
        match self.details.as_deref() {
            Some(details) => tracing::warn!(
                kind,
                code = %self.code,
                ?details,
                "nest rpc error (details are log-only, never user-facing)"
            ),
            None => tracing::warn!(kind, code = %self.code, "nest rpc error"),
        }
    }
}

/// The UI action a client should route an [`RpcError`] to — the shared
/// version-mismatch-vs-transient distinction of version-compatibility.md
/// Dimension 4. Produced by [`RpcError::action`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RpcErrorAction {
    /// Software (the nest today; a client in a future capability check) is out of
    /// date. Route to an actionable update prompt, never an auto-retry.
    NeedsUpdate,
    /// A definite server refusal that won't change on retry. Show the localized
    /// message; do not auto-retry.
    Rejected,
    /// A transport/connectivity/server-transient fault. Safe to auto-retry.
    Transient,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{decode_strict as decode, encode_canonical};

    #[test]
    fn localized_text_round_trip() {
        let lt = LocalizedText::new("error.bridges.not_linked").with_arg("bridge_id", "nostr");
        let bytes = encode_canonical(&lt).unwrap();
        let decoded: LocalizedText = decode(&bytes).unwrap();
        assert_eq!(decoded, lt);
    }

    /// The `fauna.<area>.<error>` grammar's suffix rule, which six `*-machine`
    /// crates each spelled inline before it had a home here. The no-dot case is
    /// pinned because it is the one a caller's arm table cannot see: it must
    /// keep yielding the whole code so the value falls through to `_ =>
    /// Transient` rather than matching a real suffix by accident.
    #[test]
    fn code_suffix_is_the_last_dotted_segment() {
        let suffix_of = |code: &str| {
            RpcError::new(code, "error.unexpected")
                .code_suffix()
                .to_string()
        };
        assert_eq!(suffix_of("fauna.media.not_found"), "not_found");
        assert_eq!(suffix_of("fauna.protocol.unknown_kind"), "unknown_kind");
        assert_eq!(
            suffix_of("fauna.inbox.guardian_approval_required"),
            "guardian_approval_required"
        );
        // No namespace at all: the whole code, not an empty match.
        assert_eq!(suffix_of("malformed"), "malformed");
        assert_eq!(suffix_of(""), "");
    }

    #[test]
    fn rpc_error_round_trip_no_details() {
        let err = RpcError::new("fauna.bridges.not_linked", "error.bridges.not_linked");
        let bytes = encode_canonical(&err).unwrap();
        let decoded: RpcError = decode(&bytes).unwrap();
        assert_eq!(decoded, err);
        assert_eq!(decoded.details, None);
    }

    /// `family-safety.md` § Guardian policy pillar 1 / § Child-initiated contact
    /// requests: the ward's blocked action is refused with a *typed* error whose
    /// whole purpose is that the app can offer "ask your guardian" in its place.
    /// Recognising it must not be a stringly match re-derived per app — the
    /// namespace varies by which handler refused, and only the suffix is stable.
    #[test]
    fn every_guardian_approval_namespace_is_recognised() {
        for ns in [
            "inbox",
            "knocks",
            "contacts",
            "conversations",
            "bridges",
            "account",
        ] {
            let err = RpcError::new(
                format!("fauna.{ns}.guardian_approval_required"),
                format!("error.{ns}.guardian_approval_required"),
            );
            assert!(
                err.is_guardian_approval_required(),
                "namespace {ns} must be recognised",
            );
            // It is a definite refusal — retrying it unchanged never succeeds.
            assert_eq!(err.action(), RpcErrorAction::Rejected);
        }
    }

    /// The predicate matches the whole code, never a substring: a future code
    /// that merely *mentions* the suffix must not be mistaken for the refusal
    /// the ask affordance hangs off.
    #[test]
    fn a_lookalike_code_is_not_a_guardian_approval_refusal() {
        for code in [
            "fauna.inbox.send_failed",
            "fauna.family.guardian_approval_required_notice",
            "guardian_approval_required",
            "fauna..guardian_approval_required",
            "fauna.nest.outdated",
            "",
        ] {
            assert!(
                !RpcError::new(code, "error.x").is_guardian_approval_required(),
                "{code:?} must not read as the refusal",
            );
        }
    }

    #[test]
    fn nest_outdated_is_actionable_not_transient() {
        // Dim 4 core: the degraded-serve code must route to an update prompt, not
        // a retry loop. A client that misclassifies this spins forever.
        let err = RpcError::nest_outdated();
        assert_eq!(err.code, RpcError::CODE_NEST_OUTDATED);
        assert_eq!(err.action(), RpcErrorAction::NeedsUpdate);
        assert_ne!(err.action(), RpcErrorAction::Transient);
        assert_eq!(err.localized(), fauna_i18n::strings::error::nest::OUTDATED);
        // Renders the localized message, never the bare wire key.
        assert_ne!(err.localized(), err.message.key);
    }

    #[test]
    fn nest_schema_mismatch_is_needs_update_not_transient() {
        // Dim 4 residual: a *query-time* schema/version mismatch (a post-boot
        // `no such column …` the boot-time schema_meta gate cannot catch) must
        // route to an update prompt, not the auto-retry loop `fauna.protocol.internal`
        // (Transient) would spin forever.
        let err = RpcError::nest_schema_mismatch();
        assert_eq!(err.code, RpcError::CODE_NEST_SCHEMA_MISMATCH);
        assert_eq!(err.action(), RpcErrorAction::NeedsUpdate);
        assert_ne!(err.action(), RpcErrorAction::Transient);
        assert_eq!(
            err.localized(),
            fauna_i18n::strings::error::nest::SCHEMA_MISMATCH
        );
        assert_ne!(err.localized(), err.message.key);
        // Never leaks a server SQL phrase in `details` (Dim 4 target).
        assert_eq!(err.details, None);
    }

    #[test]
    fn protocol_faults_classify_transient_vs_rejected() {
        for code in [
            "fauna.protocol.timeout",
            "fauna.protocol.disconnected",
            "fauna.protocol.resync_required",
            "fauna.protocol.internal",
            "fauna.protocol.encode_failed",
        ] {
            assert_eq!(
                RpcError::new(code, "error.x").action(),
                RpcErrorAction::Transient,
                "{code} should be Transient",
            );
        }
        for code in [
            "fauna.protocol.unknown_kind",
            "fauna.protocol.malformed",
            "fauna.auth.not_registered",
            "fauna.account.invite_request_not_found",
        ] {
            assert_eq!(
                RpcError::new(code, "error.x").action(),
                RpcErrorAction::Rejected,
                "{code} should be Rejected",
            );
        }
    }

    /// The non-claimant folder-channel commit rate cap's refusal (
    /// `federation.md` § residual (a)) must be `Transient` — a client that
    /// classified it `Rejected` would show a dead-end message instead of
    /// backing off and retrying, which is the whole point of a typed
    /// retryable code over `permission_denied`.
    #[test]
    fn channel_commit_rate_limited_is_transient_and_localized() {
        let err = RpcError::new("fauna.conversations.rate_limited", "error.x");
        assert_eq!(err.action(), RpcErrorAction::Transient);
        assert_eq!(
            err.localized(),
            fauna_i18n::strings::error::conversations::RATE_LIMITED
        );
        assert_ne!(
            err.localized(),
            fauna_i18n::strings::error::UNEXPECTED,
            "must not silently fall through to the generic fallback"
        );
    }

    /// The **rate-limit family**, and the one namespace deliberately outside it
    /// (`federation.md` § Peer-auth model → *Discovery-failure semantics*). The
    /// security finding that forced this: a peer nest throttling the anonymous
    /// `fauna.actor.by_handle` discovery probe answers
    /// `fauna.protocol.rate_limited`, which fell to this table's default
    /// `Rejected` arm and so read to the recipient picker as "not a Fauna
    /// recipient here" — downgrading a **known** Fauna peer to plaintext SMTP,
    /// the exact case the ruling exists to prevent.
    ///
    /// Every `fauna.{ns}.rate_limited` the nest mints comes from one seam
    /// (`rpc_errors::rate_limited_ns`) and means the same thing — a **rolling
    /// window** refused this call, so the same call succeeds once the window
    /// rolls. That is `Transient` by construction, and keying on the family
    /// covers a namespace added later *on arrival* rather than re-opening the
    /// same hole for whoever adds it.
    #[test]
    fn the_rate_limit_family_is_transient_except_the_email_quota() {
        for code in [
            // The generic per-source anonymous/connection throttle — the one
            // the discovery probe trips (`anonymous_rate_limit`, 60/60 s).
            "fauna.protocol.rate_limited",
            // The non-claimant folder-channel commit cap (3600 s window).
            "fauna.conversations.rate_limited",
            // The bridge / moderation rails' sliding windows (60 s).
            "fauna.bridges.rate_limited",
            "fauna.moderation.rate_limited",
            // A namespace nobody has minted yet: covered on arrival.
            "fauna.somefuturens.rate_limited",
        ] {
            assert_eq!(
                RpcError::new(code, "error.x").action(),
                RpcErrorAction::Transient,
                "{code} is a rolling-window refusal and must be Transient",
            );
        }

        // The ONE carve-out. `fauna.email.rate_limited` is two refusals under
        // one code: an hourly send-rate window (transient) AND the per-actor
        // **recipients-per-day submission quota** (`email_handlers`, the
        // `SubmissionQuotaOutcome::OverQuota` arm). "Wait a moment and retry"
        // is wrong for the second — a client backing off on it would spin for
        // up to a day — so it stays `Rejected` until the nest splits the two
        // causes into distinct codes.
        assert_eq!(
            RpcError::new("fauna.email.rate_limited", "error.x").action(),
            RpcErrorAction::Rejected,
            "the daily submission quota is not retryable-in-a-moment",
        );

        // The family key is the LAST segment, exactly as `localized_family`
        // reads it: a code merely containing the word is not in the family.
        assert_eq!(
            RpcError::new("fauna.rate_limited.something_else", "error.x").action(),
            RpcErrorAction::Rejected,
        );
    }

    /// Classification and rendering move together ([`RpcError::localized_family`]'s
    /// own standing rule). The family's classification moved above, so its
    /// rendering moves here — and the email carve-out, which did NOT move, gets
    /// its own exact-code string rather than the family's "wait a moment".
    #[test]
    fn the_rate_limit_family_renders_with_its_classification() {
        use fauna_i18n::strings::error;

        for code in [
            "fauna.protocol.rate_limited",
            "fauna.bridges.rate_limited",
            "fauna.moderation.rate_limited",
            "fauna.somefuturens.rate_limited",
        ] {
            assert_eq!(
                RpcError::new(code, "error.x").localized(),
                error::RATE_LIMITED,
                "{code} must render the family string, not the generic fallback",
            );
        }

        // The two exact-code arms that outrank the family fallback: the
        // conversations cap says more, and the email quota says something
        // genuinely different (it is not retryable in a moment).
        assert_eq!(
            RpcError::new("fauna.conversations.rate_limited", "error.x").localized(),
            error::conversations::RATE_LIMITED,
        );
        assert_eq!(
            RpcError::new("fauna.email.rate_limited", "error.x").localized(),
            error::email::RATE_LIMITED,
        );
        assert_ne!(
            RpcError::new("fauna.email.rate_limited", "error.x").localized(),
            error::RATE_LIMITED,
            "a Rejected code must never render the family's retry framing",
        );
    }

    /// The namespaced authorization families render *you may not*, never the
    /// generic "Please try again" — across every namespace the nest mints from,
    /// including ones that do not exist yet.
    ///
    /// Red-verifies for its own reason: restore `localized`'s bare
    /// `_ => error::UNEXPECTED` in place of the family fallback and every case
    /// here fails on the `assert_ne!`.
    #[test]
    fn every_namespaced_authorization_refusal_localizes_as_authorization() {
        use fauna_i18n::strings::error;
        // The real minted codes, one per live seam: `permission_denied_ns`,
        // `forbidden_ns`, `bare_forbidden_ns`, `central_permission_denied`, and
        // the segments door's `not_owner`.
        for code in [
            "fauna.sync.permission_denied",
            "fauna.bridges.permission_denied",
            "fauna.posts.permission_denied",
            "fauna.federation.forbidden",
            "fauna.setup.forbidden",
            "fauna.segments.not_owner",
            // A namespace nobody has written yet: the family fallback must
            // cover it on arrival — that is the whole point of keying on the
            // last segment rather than enumerating namespaces.
            "fauna.a_namespace_invented_tomorrow.permission_denied",
        ] {
            let err = RpcError::new(code, "error.ignored");
            assert_eq!(
                err.localized(),
                error::AUTHORIZATION,
                "{code} must render the authorization string"
            );
            assert_ne!(
                err.localized(),
                error::UNEXPECTED,
                "{code} must not fall through to the generic retry string: a
                 permanent refusal telling the user to try again is the defect
                 this fallback closes"
            );
            // Rendering says "you may not"; classification must agree that
            // there is nothing to retry.
            assert_eq!(
                err.action(),
                RpcErrorAction::Rejected,
                "{code} must not be auto-retried"
            );
        }
    }

    /// A curated exact-code arm outranks the family fallback, so a namespace
    /// that has something more useful to say keeps saying it.
    #[test]
    fn a_curated_forbidden_arm_wins_over_the_family_fallback() {
        use fauna_i18n::strings::error;
        let err = RpcError::new("fauna.conversations.forbidden", "error.ignored");
        assert_eq!(err.localized(), error::conversations::FORBIDDEN);
        assert_ne!(
            err.localized(),
            error::AUTHORIZATION,
            "the reach-policy string names the contact-request remedy; the
             generic family string does not"
        );
    }

    /// The family fallback is keyed on the LAST segment, and must not swallow
    /// codes that merely contain a family word elsewhere, nor invent a string
    /// for a family it was never given.
    #[test]
    fn the_family_fallback_does_not_over_reach() {
        use fauna_i18n::strings::error;
        for code in [
            // A family with no shared string: still the generic fallback.
            "fauna.index.bytes_not_held",
            "fauna.account.actor_exists",
            // The family word is present but is not the last segment.
            "fauna.forbidden.something_else",
            "fauna.rate_limited.something_else",
        ] {
            let err = RpcError::new(code, "error.ignored");
            assert_eq!(
                err.localized(),
                error::UNEXPECTED,
                "{code} has no family string and must fall back"
            );
        }
    }

    #[test]
    fn localized_covers_protocol_family_and_falls_back() {
        use fauna_i18n::strings::error;
        assert_eq!(
            RpcError::new("fauna.protocol.unknown_kind", "k").localized(),
            error::protocol::UNKNOWN_KIND,
        );
        assert_eq!(
            RpcError::new("fauna.protocol.disconnected", "k").localized(),
            error::protocol::DISCONNECTED,
        );
        // An unrecognised code falls back to the generic localized string, never
        // a raw code or the wire key.
        assert_eq!(
            RpcError::new("fauna.some.future.code", "k").localized(),
            error::UNEXPECTED,
        );
    }

    /// The tier device-cap refusal renders through its own exact arm — the
    /// cap and both remedies, never the generic fallback (which is what every
    /// app showed for it until 2026-09-15) — and stays `Rejected`: only a
    /// freed slot clears it, so an auto-retry would spin.
    #[test]
    fn device_limit_exceeded_localizes_with_its_remedy_and_is_rejected() {
        use fauna_i18n::strings::error;
        let err = RpcError::new(
            RpcError::CODE_SYNC_DEVICE_LIMIT_EXCEEDED,
            "error.sync.device_limit_exceeded",
        );
        assert_eq!(err.localized(), error::sync::DEVICE_LIMIT_EXCEEDED);
        assert_ne!(err.localized(), error::UNEXPECTED);
        assert!(
            err.localized().contains("Devices"),
            "the sentence must point at the page carrying the remedy"
        );
        assert_eq!(err.action(), RpcErrorAction::Rejected);
    }

    /// A handle someone else holds carries the nest's reason to the user
    /// (`settings.md` § User actions), for the change and for registration
    /// alike, and the cooldown has its own sentence — neither is the generic
    /// fallback, and neither is retried: only a different handle clears them.
    #[test]
    fn a_taken_or_cooling_handle_localizes_its_reason_and_is_rejected() {
        use fauna_i18n::strings::error;
        for code in ["fauna.profile.handle_taken", "fauna.account.handle_taken"] {
            let err = RpcError::new(code, "error.profile.handle_taken");
            assert_eq!(err.localized(), error::profile::HANDLE_TAKEN, "{code}");
            assert_eq!(err.action(), RpcErrorAction::Rejected, "{code}");
        }
        let cooling = RpcError::new("fauna.profile.handle_cooldown", "k");
        assert_eq!(cooling.localized(), error::profile::HANDLE_COOLDOWN);
        assert_eq!(cooling.action(), RpcErrorAction::Rejected);
    }

    /// A list member on a hosted domain is refused with a sentence pointing
    /// at aliases (`mail-mass-mailing.md` § Don't do these) — never the
    /// generic fallback — and is not retried.
    #[test]
    fn a_local_domain_list_member_points_at_aliases() {
        let err = RpcError::new(
            "fauna.bridges.recipient_on_local_domain",
            "error.bridges.recipient_on_local_domain",
        );
        assert_eq!(
            err.localized(),
            fauna_i18n::strings::error::bridges::RECIPIENT_ON_LOCAL_DOMAIN
        );
        assert!(err.localized().contains("alias"));
        assert_eq!(err.action(), RpcErrorAction::Rejected);
    }

    /// A forward-all target on a hosted domain is refused the same way — its
    /// own sentence naming aliases (`mail-forwarding.md` § Don't do these),
    /// never the generic fallback, never retried.
    #[test]
    fn a_local_domain_forward_target_points_at_aliases() {
        let err = RpcError::new(
            "fauna.bridges.forward_target_on_local_domain",
            "error.bridges.forward_target_on_local_domain",
        );
        assert_eq!(
            err.localized(),
            fauna_i18n::strings::error::bridges::FORWARD_TARGET_ON_LOCAL_DOMAIN
        );
        assert!(err.localized().contains("alias"));
        assert_eq!(err.action(), RpcErrorAction::Rejected);
    }

    /// A write past the account's one storage allowance — mail, calendar and
    /// contacts alike (`caldav-server.md` § QUOTA → § Enforcement points) —
    /// renders its own storage-full sentence, never the generic fallback, and
    /// is never retried: it clears only when the user frees space.
    #[test]
    fn an_over_quota_write_says_storage_is_full() {
        let err = RpcError::new("fauna.bridges.over_quota", "error.bridges.over_quota");
        assert_eq!(
            err.localized(),
            fauna_i18n::strings::error::bridges::OVER_QUOTA
        );
        assert!(err.localized().contains("storage is full"));
        assert_eq!(err.action(), RpcErrorAction::Rejected);
    }

    #[test]
    fn message_too_large_localizes_and_is_rejected_not_retried() {
        // fauna.email.send / import_message both report
        // crate::email::MESSAGE_TOO_LARGE_CODE (smtp-server.md § Message size
        // limits) — a client renders the same localized text a same-code
        // client-side pre-check would show, and never auto-retries it.
        use fauna_i18n::strings::error;
        let err = RpcError::new(
            crate::email::MESSAGE_TOO_LARGE_CODE,
            "error.email.too_large",
        );
        assert_eq!(err.localized(), error::email::TOO_LARGE);
        assert_eq!(err.action(), RpcErrorAction::Rejected);
    }

    /// Byte-exact wire pin for the boxed `details` representation. The fixture
    /// bytes were captured from `encode_canonical` on the PRE-box representation
    /// (`details: Option<Value>`, 2026-07-30) — this test failing means the
    /// in-memory boxing decision (transport.md § Wire format, "In-memory
    /// representation") stopped being wire-invisible, which must never happen.
    #[test]
    fn rpc_error_wire_bytes_pin() {
        let mut args = std::collections::BTreeMap::new();
        args.insert("limit".to_string(), "10".to_string());
        let mut map = std::collections::BTreeMap::new();
        map.insert("retry_after_ms".to_string(), Value::Integer(1500));
        map.insert("reason".to_string(), Value::String("throttled".into()));
        let err = RpcError {
            code: "fauna.test.pin".into(),
            message: Box::new(LocalizedText {
                key: "error.test.pin".into(),
                args,
                extra: Default::default(),
            }),
            details: Some(Box::new(Value::Map(map))),
            extra: Default::default(),
        };
        let expected = "a364636f64656e6661756e612e746573742e70696e6764657461696c73a266726561736f6e697468726f74746c65646e72657472795f61667465725f6d731905dc676d657373616765a2636b65796e6572726f722e746573742e70696e6461726773a1656c696d6974623130";
        assert_eq!(hex::encode(encode_canonical(&err).unwrap()), expected);
        let decoded: RpcError = decode(
            &expected
                .as_bytes()
                .chunks(2)
                .map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 16).unwrap())
                .collect::<Vec<u8>>(),
        )
        .unwrap();
        assert_eq!(decoded, err);
    }

    /// `RpcError` must stay under clippy's 128-byte `result_large_err` default —
    /// that lint (no bespoke threshold) is the guard that keeps `Result`s cheap
    /// across the RPC layer; `details` and `message` are boxed precisely for
    /// this. If this fires, shrink the type again rather than raising a
    /// threshold (decision record: transport.md § Wire format).
    ///
    /// **Strictly less than 128, not `<=`.** The bound was `<= 128` until
    /// 2026-09-11, when adding rule 4's `extra` to this struct and to
    /// `LocalizedText` landed the type on exactly 128 — the assert passed and
    /// clippy went red anyway, reporting "the `Err`-variant is at least 128
    /// bytes". The lint's threshold is inclusive; the guard's was not, so it
    /// had a one-byte blind spot at precisely the value it exists to catch.
    #[test]
    fn rpc_error_stays_under_default_result_large_err() {
        assert!(
            std::mem::size_of::<RpcError>() < 128,
            "RpcError is {} bytes; clippy::result_large_err fires at >= 128",
            std::mem::size_of::<RpcError>()
        );
    }

    #[test]
    fn rpc_error_round_trip_with_details() {
        let err = RpcError {
            code: "fauna.protocol.cancelled".into(),
            message: Box::new(LocalizedText::new("error.cancelled")),
            details: Some(Box::new(Value::String("user-initiated".into()))),
            extra: Default::default(),
        };
        let bytes = encode_canonical(&err).unwrap();
        let decoded: RpcError = decode(&bytes).unwrap();
        assert_eq!(decoded.code, "fauna.protocol.cancelled");
        assert_eq!(
            decoded.details.as_deref(),
            Some(&Value::String("user-initiated".into()))
        );
    }

    #[test]
    fn superseded_names_the_successor_and_survives_the_wire() {
        let successor = [0xb2u8; 32];
        let err = RpcError::superseded(&successor);
        assert_eq!(err.code, RpcError::CODE_SUPERSEDED);

        // The successor must survive encode/decode: a client's whole next step
        // (import that identity) is read out of this error.
        let bytes = encode_canonical(&err).unwrap();
        let decoded: RpcError = decode(&bytes).unwrap();
        assert_eq!(decoded.superseded_by(), Some(successor));

        // And it must say where to fetch the proof, so the client can verify
        // the claim rather than trust this reply.
        let Some(Value::Map(map)) = decoded.details.as_deref() else {
            panic!("superseded must carry a details map");
        };
        assert_eq!(
            map.get("statement_kind"),
            Some(&Value::String(RpcError::SUCCESSION_LOOKUP_KIND.to_string()))
        );
        assert_eq!(
            map.get("new_actor_id_hex"),
            Some(&Value::String("b2".repeat(32)))
        );
        assert_eq!(
            decoded.message.args.get("new_actor_id").map(String::as_str),
            Some("b2".repeat(32).as_str())
        );

        // Permanent refusal — never auto-retried, or the succeeded device
        // spins in a reconnect loop instead of prompting the import.
        assert_eq!(decoded.action(), RpcErrorAction::Rejected);
    }

    #[test]
    fn superseded_by_reads_nothing_out_of_an_unrelated_error() {
        // A successor id is an authorization-adjacent value: no call site may
        // pick one up from an error that is not a supersession refusal.
        let mut impostor = RpcError::superseded(&[0xb2; 32]);
        impostor.code = "fauna.auth.not_registered".into();
        assert_eq!(impostor.superseded_by(), None);

        assert_eq!(
            RpcError::new("fauna.protocol.internal", "error.protocol.internal").superseded_by(),
            None
        );

        // Right code, malformed payload — refuse rather than guess.
        let mut truncated = RpcError::superseded(&[0xb2; 32]);
        truncated.details = Some(Box::new(Value::Map(BTreeMap::from([(
            "new_actor_id".to_string(),
            Value::Bytes(vec![0xb2; 31]),
        )]))));
        assert_eq!(truncated.superseded_by(), None);
    }

    /// Observability half: `details` must reach a
    /// human triaging a failure — the log is its only home, since `Display`
    /// must never carry it (the sibling pin below).
    #[derive(Clone, Default)]
    struct CaptureWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for CaptureWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CaptureWriter {
        type Writer = Self;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    #[test]
    fn log_operator_details_reaches_the_log_when_present() {
        let capture = CaptureWriter::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(capture.clone())
            .with_max_level(tracing::Level::WARN)
            .finish();

        let err = RpcError::new("fauna.protocol.internal", "error.protocol.internal")
            .with_details_text("sqlite CAS conflict on config row");
        tracing::subscriber::with_default(subscriber, || {
            err.log_operator_details("fauna.drafts.put");
        });

        let logged = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
        assert!(
            logged.contains("sqlite CAS conflict on config row"),
            "details text must reach the log: {logged}"
        );
        assert!(logged.contains("fauna.drafts.put"));
        assert!(logged.contains("fauna.protocol.internal"));
    }

    #[test]
    fn log_operator_details_is_fine_with_no_details() {
        // No capture needed — this just proves the `None` arm doesn't require
        // a `details` payload to log something useful.
        let err = RpcError::new("fauna.protocol.timeout", "error.protocol.timeout");
        err.log_operator_details("fauna.drafts.put");
    }
}
