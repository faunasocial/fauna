//! Routing / data-plane WS-RPC payload types for the I2b mail-bridge
//! surface. Wrapped-blob payloads stay in `wrapped_blob.rs`; this
//! module covers everything the MTA/MDA bridges call that isn't a
//! blob fetch/store/revoke.

use crate::Value;
use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReportSessionCloseRequest {
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    pub credential_id: String,
    /// Free-text reason: "logout", "idle_timeout", "transport_disconnect", etc.
    pub reason: String,
    /// Bridge-local epoch-millis timestamp for the session-close event.
    pub occurred_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReportSessionCloseReply {
    pub ok: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ValidateRecipientRequest {
    pub local_part: String,
    pub domain: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ValidateRecipientReply {
    Resolved {
        #[serde(with = "serde_bytes")]
        actor_id: Vec<u8>,
        /// `true` when the recipient resolved via a **role-address** route
        /// (`postmaster@`/`abuse@`/`security@`/`tlsrpt@`/`dmarc-report@` …,
        /// `fauna_mail::aliases::classify_role_address`) rather than a normal
        /// alias. Role-address recipients **bypass per-mailbox quota** on
        /// inbound delivery (`smtp-server.md` § Architectural rules — "an
        /// over-quota admin mailbox still receives postmaster mail"); the Go
        /// MTA carries this per-recipient and echoes it on
        /// [`IngestInboundMailRequest::is_role_address`] so the ingest handler
        /// can skip the quota pre-check. (The same bit is the future seam for
        /// the role-address greylist bypass, `smtp-server.md` :205.)
        /// `#[serde(default)]` so an absent key decodes as `false` (non-role mail).
        #[serde(default)]
        is_role_address: bool,
    },
    Reject {
        reason: String,
    },
}

/// `fauna.bridges.check_greylist` (MTA at RCPT TO, after `validate_recipient`) —
/// nest-side greylist (`docs/goal/behavior/smtp-server.md` § Greylisting). The
/// bridge forwards the envelope `from` / `to` / peer `client_ip` and holds **no**
/// local state; nest derives the `(sender_domain, recipient, subnet)` tuple,
/// reads/writes `greylist_tuples`, and applies the defer/pass policy. This is
/// what makes greylisting uniform across bridge restart (the in-process map it
/// replaces was wiped on every supervisor-restart).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct CheckGreylistRequest {
    /// Envelope MAIL FROM address (nest extracts the sender domain; empty for
    /// the null sender `<>`).
    pub from: String,
    /// Envelope RCPT TO address.
    pub to: String,
    /// Peer IP (nest groups it into its /24 (IPv4) or /64 (IPv6) prefix).
    pub client_ip: String,
}

/// Verdict for `check_greylist`. Float-free bool (dag-cbor strict decode rejects
/// floats): `pass = false` ⇒ tempfail `451 4.7.1 Greylisted`; `pass = true` ⇒
/// accept (continue the transaction, silent on whitelist).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct CheckGreylistReply {
    pub pass: bool,
}

/// `fauna.bridges.resolve_recipient` (MTA at RCPT TO) — the **fixed-order**
/// alias resolver (`mail-aliases.md` § Resolution order: exact → forwarder →
/// +suffix → disposable → wildcard → role-address → catch-all → 550). The richer
/// **superset** of `validate_recipient` (which is exact-only): it resolves all
/// alias kinds, honors `disabled`, returns the `X-Fauna-Address-*` headers to
/// stamp + the per-alias control overrides, and so supersedes `validate_recipient`
/// at the RCPT-TO call once
/// the Go MTA migrates its RCPT-TO call. Request reuses `validate_recipient`'s
/// `{local_part, domain}` shape (the bridge already splits the RCPT this way),
/// so the cutover is a drop-in.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ResolveRecipientRequest {
    pub local_part: String,
    pub domain: String,
    /// The MAIL FROM domain, for the `alias_hits` audit log (A2.4). The Go MTA
    /// has it at RCPT TO; `#[serde(default)]` decodes an absent key as an empty
    /// `sender_domain`. Does **not** affect
    /// resolution — only the handler's hit-log records it.
    #[serde(default)]
    pub sender_domain: String,
    /// The full MAIL FROM address. Unlike `sender_domain` this **does** affect
    /// resolution: it is the subject of the guardian mail gate's known-sender
    /// check, which can refuse this recipient with a permanent `550` when their
    /// guardian set `unknown_sender_mail=reject`
    /// (`docs/goal/behavior/family-safety.md` § The mail gate). Envelope FROM is
    /// plaintext floor in both storage modes (`encryption-at-rest.md`
    /// § Plaintext floor). `#[serde(default)]` reads an absent field as the empty
    /// string. An absent or empty address (the null reverse-path) never Rejects at RCPT — DSN-ness is undecidable before `DATA` —
    /// so the gate defers to ingest, where uncorrelated null-path mail to a
    /// gated ward is *held* (never lost, never a bounce to `<>`).
    #[serde(default)]
    pub sender_address: String,
}

/// One header the MDA stamps on the message before the user's filter rules
/// run (`mail-aliases.md` § Kind 2/3/4 — `X-Fauna-Address-Suffix` /
/// `X-Fauna-Address-Wildcard-Suffix` / `X-Fauna-Address-Catchall`). Exact
/// matches stamp none.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct StampedHeader {
    pub name: String,
    pub value: String,
}

/// Resolver verdict for one RCPT TO (`mail-aliases.md` § Wire `:302` —
/// `{actor_id, headers_to_stamp, control_overrides}` on success). `Reject`
/// carries the SMTP `smtp_code` (550 user-unknown / disabled / invalid
/// sub-address today; 451 rate-limit once the bridge enforces the cap) + a
/// reason string for the bridge's SMTP response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ResolveRecipientReply {
    Resolved {
        /// 32-byte routing actor.
        actor_id: ByteBuf,
        headers_to_stamp: Vec<StampedHeader>,
        /// Per-alias control overrides (spam threshold + rate caps) the bridge
        /// applies; reuses [`AliasControls`] (its `label` is unset here — the
        /// resolver carries routing controls, not the UI tag).
        control_overrides: AliasControls,
        /// `true` iff the recipient resolved via the RFC 2142 role-address
        /// route (`postmaster@`/`abuse@`/… → the deployment admin's mailbox,
        /// `mail-aliases.md` § Resolution order step 6). Mirrors
        /// `ValidateRecipientReply::Resolved::is_role_address`: the Go MTA echoes
        /// it on the ingest request so an over-quota admin mailbox still receives
        /// role-address mail (it bypasses per-mailbox quota + greylisting).
        /// `#[serde(default)]` (= `false`) keeps a normal alias/exact resolve
        /// (non-role mail).
        #[serde(default)]
        is_role_address: bool,
    },
    /// An admin external forwarder matched (`mail-aliases.md` § Kind 7 +
    /// § Resolution order step 2 `:124`; `mail-forwarding.md` § Admin external
    /// forwarders): no local delivery, no headers. The MTA hands
    /// `{forward_target, forwarder_actor_id}` to the forward dispatch — the
    /// admin is the SRS forwarder-actor + the rate-cap / NDR principal. Wired
    /// into the in-process resolver now; the Go-MTA consumes it once the
    /// `validate_recipient`→`resolve_recipient` RCPT-TO cutover lands.
    Forward {
        forward_target: String,
        /// 32-byte managing-admin actor.
        forwarder_actor_id: ByteBuf,
    },
    Reject {
        smtp_code: u16,
        reason: String,
    },
    /// The recipient is a deployment-internal envelope-command address whose
    /// side effect the nest already performed during resolution (today:
    /// `unsubscribe+<token>@` — the RFC 8058 mailto one-click unsubscribe,
    /// `mail-mass-mailing.md` § The mailto handler). The command is
    /// fire-and-forget + idempotent, so the MTA MUST accept the RCPT with
    /// `250` and **discard** the message body — never deliver it to a mailbox.
    /// Distinct from `Resolved` (no mailbox, no headers, no quota/greylist) and
    /// from `Reject` (no 4xx/5xx). Bare `unsubscribe@` with no token is a
    /// `Reject { smtp_code: 550 }`, not a `Discard`.
    Discard,
}

// ── Per-account alias user surface ──
//
// User-class WS-RPC surface over `account_aliases`, named in
// `docs/goal/behavior/mail-aliases.md` § Wire shapes. These derive the
// owning actor from the authenticated caller, never a wire param:
// the **authenticated caller** (a user manages their *own* aliases only,
// § Cross-actor isolation `:252`), never from a wire param. Slice 1
// ships the exact-kind CRUD; the
// wildcard/+suffix resolver, disposable mint, and alias-hit audit land
// in A2 slices 2-4.

/// The four per-alias controls (`mail-aliases.md` § Per-alias controls).
/// Shared by create + update. `label` empty = no label (DB default `''`).
/// Each `Option` is `None` = inherit / unlimited; `Some` = set. There is
/// no `Option<Option<T>>` tri-state on the wire (it doesn't round-trip on
/// DAG-CBOR — both `None` and `Some(None)` encode as `null`): `update`
/// uses full-overwrite semantics, so the client always submits the
/// complete control set from the row it is editing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AliasControls {
    /// UI tag, ≤64 UTF-8 chars; empty string = no label.
    pub label: String,
    /// Per-alias spam-score override in **whole spam-points** (overrides
    /// the per-account / admin `max_score_before_spam_folder`). `None` =
    /// inherit. Scaled integer, not float — DAG-CBOR forbids floats and
    /// the override compares against `SpamPolicyThresholds
    /// .max_score_before_spam_folder: u32`.
    #[serde(default)]
    pub spam_threshold_override: Option<u32>,
    /// `451` tempfail over quota; `None` = unlimited.
    #[serde(default)]
    pub rate_limit_per_hour: Option<i64>,
    #[serde(default)]
    pub rate_limit_per_day: Option<i64>,
}

/// Wire representation of one `account_aliases` row. Mirrors the table
/// (`migrations.rs` `MIGRATIONS_MAIL_ALIASES` / `mail-aliases.md` § Storage)
/// with wire-friendly byte buffers and the scaled-integer spam threshold
/// above. `uses_remaining` / `expires_at` are `None` for every kind except
/// `disposable`; `forward_target` is `Some` only for `forwarder` (§ Kind 7).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AliasRow {
    /// 16-byte UUID.
    pub alias_id: ByteBuf,
    /// 32-byte owning actor id.
    pub actor_id: ByteBuf,
    pub local_domain: String,
    /// `"exact"` / `"wildcard_prefix"` / `"disposable"` (user CRUD + mint) or
    /// `"forwarder"` (admin external forwarders, § Kind 7). `catchall` is never
    /// a row — it's the `mail.inbound.catch_all_actor` policy key
    /// (`mail-aliases.md:198`).
    pub kind: String,
    pub pattern: String,
    /// Forwarder-only (`kind = "forwarder"`): the external destination the
    /// inbound is forwarded to (`mail-aliases.md` § Kind 7 / § Storage `:181`).
    /// `None` for every other kind. `#[serde(default)]` keeps fixtures + the
    /// pre-forwarder wire a drop-in.
    #[serde(default)]
    pub forward_target: Option<String>,
    pub label: String,
    pub disabled: bool,
    pub spam_threshold_override: Option<u32>,
    pub rate_limit_per_hour: Option<i64>,
    pub rate_limit_per_day: Option<i64>,
    /// Disposable-only (`None` for other kinds).
    pub uses_remaining: Option<i64>,
    /// Disposable-only epoch-millis expiry (`None` = no expiry).
    pub expires_at: Option<i64>,
    pub created_at: i64,
    pub last_hit_at: Option<i64>,
    pub hit_count: i64,
    /// Whether this row is the actor's canonical `<handle>@<domain>` exact
    /// alias — its primary mailbox + AUTH-login identity. Computed by
    /// `list_account_aliases_handler` from the runtime primary mail domain
    /// (the same resolution the revoke/delete/update guards use), never
    /// stored. Clients render the canonical row **read-only** (no
    /// disable/delete; "primary address") so the protection is visible
    /// (`mail-aliases.md` § Aliases UX). `#[serde(default)]` keeps the
    /// pre-flag wire + fixtures a drop-in.
    #[serde(default)]
    pub is_canonical: bool,
}

/// `fauna.bridges.list_account_aliases` (User) — `()` request; returns
/// the calling actor's own alias rows across all local domains.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ListAccountAliasesRequest {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ListAccountAliasesReply {
    pub aliases: Vec<AliasRow>,
}

/// `fauna.bridges.create_account_alias` (User) — create one alias owned
/// by the calling actor. `kind` is `"exact"` only this slice (other
/// kinds reject `fauna.protocol.malformed` with a forward pointer to the
/// A2 wildcard/disposable slices). `pattern` is validated (strict ASCII
/// `[A-Za-z0-9._-]`, length ≤64, not a reserved local-part); the
/// `(local_domain, pattern, kind)` UNIQUE constraint surfaces collisions
/// as `fauna.bridges.conflicts_with_existing_alias`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct CreateAccountAliasRequest {
    pub kind: String,
    pub local_domain: String,
    pub pattern: String,
    #[serde(default)]
    pub controls: AliasControls,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct CreateAccountAliasReply {
    /// 16-byte UUID of the newly created row.
    pub alias_id: ByteBuf,
}

/// `fauna.bridges.import_account_aliases` (User) — bulk-create exact aliases
/// from full addresses, one per `lines` entry. Best-effort: each line yields
/// an `ImportAliasOutcome`; a malformed/duplicate/over-cap line is reported,
/// not fatal. Idempotent — re-importing an existing address is `SkippedDuplicate`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ImportAccountAliasesRequest {
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ImportAccountAliasesReply {
    pub results: Vec<ImportAliasOutcome>,
}

/// One line's outcome. `line_index` is the 0-based position in the request
/// `lines` (blank lines are skipped and produce no outcome). `reason` is a
/// short human string on `Invalid`/`SkippedDuplicate`, `None` on `Created`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImportAliasOutcome {
    pub line_index: u32,
    pub address: String,
    pub status: ImportAliasStatus,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ImportAliasStatus {
    Created,
    SkippedDuplicate,
    Invalid,
    /// A status a newer nest added that this peer does not know: counted as
    /// not created, with the reason shown. Never written back.
    #[serde(other, skip_serializing)]
    Unknown,
}

/// `fauna.bridges.update_account_alias` (User) — full-overwrite of the
/// editable fields of an alias the **caller owns** (else `not_found`).
/// `kind` is immutable and not carried (`mail-aliases.md:241`); `pattern`
/// is re-validated as in create.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct UpdateAccountAliasRequest {
    /// 16-byte UUID; must belong to the calling actor.
    pub alias_id: ByteBuf,
    pub pattern: String,
    #[serde(default)]
    pub controls: AliasControls,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct UpdateAccountAliasReply {
    pub ok: bool,
}

/// `fauna.bridges.revoke_account_alias` (User) — flip `disabled = true`
/// on an alias the caller owns, preserving the row + audit
/// (`mail-aliases.md:224`). Idempotent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct RevokeAccountAliasRequest {
    /// 16-byte UUID; must belong to the calling actor.
    pub alias_id: ByteBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct RevokeAccountAliasReply {
    pub ok: bool,
}

/// `fauna.bridges.enable_account_alias` (User) — flip `disabled = false`
/// on an alias the caller owns: the reverse of `revoke` (which the goal doc
/// calls the *reversible* "soft intermediate", `mail-aliases.md:156`/`:339`).
/// Without this, `revoke` is a one-way trap — a client-causable unrecoverable
/// state. Idempotent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct EnableAccountAliasRequest {
    /// 16-byte UUID; must belong to the calling actor.
    pub alias_id: ByteBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct EnableAccountAliasReply {
    pub ok: bool,
}

/// `fauna.bridges.delete_account_alias` (User) — destructive irreversible
/// remove of an alias the caller owns (cascades `alias_hits`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct DeleteAccountAliasRequest {
    /// 16-byte UUID; must belong to the calling actor.
    pub alias_id: ByteBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct DeleteAccountAliasReply {
    pub ok: bool,
}

// ── Admin external forwarders (mail-aliases.md § Kind 7 / Wire :318-320) ──
//
// Admin-class WS-RPC surface over `account_aliases` `kind='forwarder'` rows —
// the cPanel "external forwarder" shape (`info@<our-domain>` → an external
// address, no local mailbox). Distinct from the User-class `*_account_alias`
// CRUD above: forwarders are deployment routing config, owned by + attributed
// to the **managing admin** actor (the nest derives that from the
// authenticated admin caller, not a wire param), and are excluded from
// `list_account_aliases`. The forward *dispatch* (SRS / loop / rate-cap / NDR)
// is `mail-forwarding.md`'s; these RPCs own only create / list / delete +
// the `Forward` resolver outcome.

/// `fauna.bridges.create_forwarder` (Admin) — create one external forwarder.
/// `pattern` is the local-part on `local_domain` (validated: strict ASCII,
/// ≤64, not a reserved local-part per the admin-tunable `reserved_local_parts`,
/// no collision with an existing exact alias or forwarder on `(local_domain,
/// pattern)`). `forward_target` is the external destination, validated with the
/// shared `fauna_mail::validate_forward_target` (RFC-5321 syntactic + **must
/// not be a domain this deployment hosts**, `mail-forwarding.md:244`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct CreateForwarderRequest {
    pub local_domain: String,
    pub pattern: String,
    pub forward_target: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct CreateForwarderReply {
    /// 16-byte UUID of the newly created forwarder row.
    pub alias_id: ByteBuf,
}

/// `fauna.bridges.list_forwarders` (Admin) — `()` request; enumerate the
/// deployment's forwarders (all `kind='forwarder'` rows across local domains).
/// Distinct from the owner-scoped `list_account_aliases`, which excludes
/// forwarders. Returns [`AliasRow`]s with `forward_target` set.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ListForwardersRequest {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ListForwardersReply {
    pub forwarders: Vec<AliasRow>,
}

/// `fauna.bridges.delete_forwarder` (Admin) — destructively remove a forwarder
/// by `alias_id` (cascades `alias_hits`). Admin-scoped (not owner-scoped): any
/// admin may delete any forwarder — they are deployment config, not personal
/// aliases.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct DeleteForwarderRequest {
    /// 16-byte UUID of the forwarder row to delete.
    pub alias_id: ByteBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct DeleteForwarderReply {
    pub ok: bool,
}

/// `fauna.bridges.generate_disposable_alias` (User) — mint one disposable
/// alias for the calling actor (`mail-aliases.md` § Kind 5). The nest derives
/// the `<handle>` + `<domain>` from the actor's canonical (oldest) exact
/// alias, mints a 6-char base32 token (collision-checked), and inserts a
/// `kind='disposable'` row.
///
/// `ttl_days` / `uses` are `None` = use the per-user default
/// (`DISPOSABLE_DEFAULT_TTL_DAYS` = 30 / `DISPOSABLE_DEFAULT_USES` = 1).
/// `uses = Some(0)` means **unlimited** uses (stored as a `NULL`
/// `uses_remaining`, bounded only by the TTL). `label` empty = no label.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct GenerateDisposableAliasRequest {
    #[serde(default)]
    pub ttl_days: Option<u32>,
    #[serde(default)]
    pub uses: Option<u32>,
    #[serde(default)]
    pub label: String,
}

/// Reply to a disposable mint (`mail-aliases.md` § Wire `:300` —
/// `{alias_id, full_address, token}`). `full_address` is the
/// `<handle>-temp-<token>@<domain>` the client copies to the clipboard;
/// `token` is the bare 6-char token (also embedded in `full_address`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct GenerateDisposableAliasReply {
    /// 16-byte UUID of the newly minted row.
    pub alias_id: ByteBuf,
    pub full_address: String,
    pub token: String,
}

/// `fauna.bridges.list_account_alias_hits` (User) — the per-alias audit list
/// (`mail-aliases.md` § Per-alias-hit audit list `:245`, § Wire `:301` —
/// `(alias_id, limit, before_hit_id?) → paginated alias_hits rows`). The
/// caller must **own** `alias_id` (the handler joins `alias_hits.alias_id →
/// account_aliases.actor_id == caller`, else `not_found` — never leak another
/// user's hits). Newest-first.
///
/// `before_hit_id` is the keyset cursor: pass the **last** `hit_id` of the
/// previous page to fetch the rows strictly older than it. The handler resolves
/// the cursor to its `received_at` and pages on the composite `(received_at,
/// hit_id)` (`hit_id` is a random UUID, not time-sortable on its own); a cursor
/// whose row has aged out of retention yields an empty page (pagination ends).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ListAccountAliasHitsRequest {
    /// 16-byte UUID of the alias whose hits to list (must be caller-owned).
    pub alias_id: ByteBuf,
    pub limit: u32,
    /// 16-byte keyset cursor — the previous page's last `hit_id`. `None` =
    /// newest page.
    #[serde(default)]
    pub before_hit_id: Option<ByteBuf>,
}

/// One `alias_hits` audit row (`mail-aliases.md` § Storage `:180-186`). The
/// owning `alias_id` is implied by the request (every row in a reply belongs
/// to it), so it's omitted from the row to keep the wire lean.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AliasHitRow {
    /// 16-byte UUID — also the keyset cursor for the next page.
    pub hit_id: ByteBuf,
    /// The full RCPT TO that matched (e.g. `bob-amazon@example.com`) — useful
    /// for wildcard / catch-all audit.
    pub matched_address: String,
    /// The MAIL FROM domain that delivered to this alias (empty if the MTA
    /// did not supply it).
    pub sender_domain: String,
    /// Epoch-millis the hit was recorded.
    pub received_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ListAccountAliasHitsReply {
    /// Newest-first page of hits.
    pub hits: Vec<AliasHitRow>,
}

// ── Mailing lists (mail-mass-mailing.md § Wire shapes) ───────────
//
// User-class WS-RPC surface over `mail_lists` + `mail_list_members` (the sixth
// `account_aliases.kind='list'` alias kind). A list is **per-user**: the owner
// is the authenticated caller (the nest derives `owner_actor_id` from the
// connection, never a wire param) and every read/write is owner-scoped. These
// are client↔nest kinds — not bridge-class — so there is no Go `ConfigSnapshot`
// mirror (unlike `MassMailingPolicy`). The Admin `rotate_list_unsubscribe_secret`
// (#11) + the `send_list_message` / `list_list_send_history` orchestration
// (#6 + #10b) land with their consuming items.

/// Wire repr of one `mail_lists` row joined with its `account_aliases` row for
/// the posting address. Mirrors `MailListRecord` (`db/mail_lists.rs`).
/// Timestamps are epoch-millis (the mail subsystem convention). The cached
/// `one_click_unsubscribe_token` is intentionally absent — an implementation
/// detail of the unsubscribe handlers, never surfaced to the list owner.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct MailListRow {
    /// 16-byte list UUID.
    pub list_id: ByteBuf,
    /// 16-byte UUID of the backing `kind='list'` `account_aliases` row.
    pub alias_id: ByteBuf,
    /// 32-byte owning actor id.
    pub owner_actor_id: ByteBuf,
    /// The list's posting address, from the joined alias row.
    pub local_domain: String,
    pub pattern: String,
    /// `List-Id` phrase source; `None` ⇒ the List-Id falls back to the list_id.
    #[serde(default)]
    pub friendly_name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// `List-Help` override; `None` ⇒ the default per-list help page.
    #[serde(default)]
    pub list_help_url: Option<String>,
    /// `List-Archive` source; `None` ⇒ the header is omitted (never fabricated).
    #[serde(default)]
    pub list_archive_url: Option<String>,
    /// Per-list per-send recipient cap override (≤ the admin ceiling); `None` ⇒
    /// the deployment ceiling applies.
    #[serde(default)]
    pub recipients_per_send: Option<i64>,
    pub created_at: i64,
    #[serde(default)]
    pub last_send_at: Option<i64>,
    /// Cached subscribed-member count.
    pub member_count: i64,
    /// The list owner's own daily quota meter (reset 00:00 UTC).
    pub sends_today: i64,
    pub recipients_today: i64,
}

/// `fauna.bridges.list_account_lists` (User) — `()`; the caller's own lists.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ListAccountListsRequest {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ListAccountListsReply {
    pub lists: Vec<MailListRow>,
}

/// `fauna.bridges.create_account_list` (User) — create one list owned by the
/// caller. `local_part` is validated (strict ASCII, ≤64, not a reserved
/// local-part); the `(local_domain, local_part, kind='list')` UNIQUE constraint
/// surfaces collisions as `fauna.bridges.conflicts_with_existing_alias`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct CreateAccountListRequest {
    pub local_part: String,
    pub local_domain: String,
    #[serde(default)]
    pub friendly_name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub list_help_url: Option<String>,
    #[serde(default)]
    pub list_archive_url: Option<String>,
    /// Optional per-list per-send cap; rejected `malformed` if > the admin
    /// ceiling (`MassMailingPolicy.list_recipients_per_send`).
    #[serde(default)]
    pub recipients_per_send: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct CreateAccountListReply {
    /// 16-byte UUID of the newly created list.
    pub list_id: ByteBuf,
}

/// `fauna.bridges.update_account_list` (User) — full-overwrite of the editable
/// metadata of a list the caller owns (the posting address is immutable, like
/// an alias `kind`). Owner-scoped (`ok = false` if not owned).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct UpdateAccountListRequest {
    /// 16-byte UUID; must belong to the caller.
    pub list_id: ByteBuf,
    #[serde(default)]
    pub friendly_name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub list_help_url: Option<String>,
    #[serde(default)]
    pub list_archive_url: Option<String>,
    #[serde(default)]
    pub recipients_per_send: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct UpdateAccountListReply {
    pub ok: bool,
}

/// `fauna.bridges.delete_account_list` (User) — destructively remove a list the
/// caller owns; cascades the alias row → list row → all member rows.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct DeleteAccountListRequest {
    /// 16-byte UUID; must belong to the caller.
    pub list_id: ByteBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct DeleteAccountListReply {
    pub ok: bool,
}

/// One `mail_list_members` row, owner-facing. The cached one-click token is
/// omitted (it never leaves the nest).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct MailListMemberRow {
    /// 16-byte member UUID.
    pub member_id: ByteBuf,
    pub recipient_address: String,
    pub subscribed_at: i64,
    /// `None` = subscribed; `Some(ts)` = unsubscribed (epoch-millis).
    #[serde(default)]
    pub unsubscribed_at: Option<i64>,
}

/// `fauna.bridges.list_list_members` (User) — enumerate a caller-owned list's
/// members. `not_found` if the list is not owned by the caller.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ListListMembersRequest {
    /// 16-byte list UUID (must be caller-owned).
    pub list_id: ByteBuf,
    /// Include unsubscribed members too (default: subscribed only).
    #[serde(default)]
    pub include_unsubscribed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ListListMembersReply {
    pub members: Vec<MailListMemberRow>,
    pub subscribed_count: i64,
    pub unsubscribed_count: i64,
}

/// `fauna.bridges.add_list_member` (User) — subscribe one address to a
/// caller-owned list. The nest validates the address (RFC 5321 syntactic +
/// **not** a hosted `local_domains` address, `400 recipient_on_local_domain`)
/// and derives the one-click token. Idempotent: a duplicate returns the
/// existing member with `added = false`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AddListMemberRequest {
    pub list_id: ByteBuf,
    pub recipient_address: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AddListMemberReply {
    /// 16-byte member UUID (new, or the pre-existing one on a duplicate).
    pub member_id: ByteBuf,
    /// `true` if a fresh subscription was created; `false` if the address was
    /// already a member (left untouched — subscription is sticky).
    pub added: bool,
}

/// `fauna.bridges.batch_import_list_members` (User) — bulk-subscribe addresses
/// to a caller-owned list (max `MassMailingPolicy.list_max_import_per_batch`).
/// Syntactically invalid + local-domain addresses are skipped (counted, not
/// fatal); already-subscribed addresses are skipped as duplicates.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct BatchImportListMembersRequest {
    pub list_id: ByteBuf,
    pub addresses: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct BatchImportListMembersReply {
    pub added: u32,
    pub skipped_invalid: u32,
    pub skipped_duplicate: u32,
}

/// `fauna.bridges.unsubscribe_list_member` (User, manual) — flip a member's
/// `unsubscribed_at` on a caller-owned list, by address (the one-click token
/// form is internal to the HTTPS/mailto handlers, never this RPC). Idempotent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct UnsubscribeListMemberRequest {
    pub list_id: ByteBuf,
    pub recipient_address: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct UnsubscribeListMemberReply {
    pub ok: bool,
}

/// `fauna.bridges.resubscribe_list_member` (User) — explicit re-subscribe (the
/// only way back from a sticky unsubscribe). Owner-scoped.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ResubscribeListMemberRequest {
    pub list_id: ByteBuf,
    pub recipient_address: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ResubscribeListMemberReply {
    pub ok: bool,
}

/// `fauna.bridges.rotate_list_unsubscribe_secret` (**Admin**) — rotate the
/// deployment-wide 32-byte one-click-unsubscribe secret and re-tokenize every
/// member under it (§ Secret rotation). Invalidates all in-flight tokens (a
/// browser click from an already-sent message 404s afterward). The lone
/// admin-class list RPC; the value itself is never exposed (only the rotate).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct RotateListUnsubscribeSecretRequest {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct RotateListUnsubscribeSecretReply {
    /// How many member rows were re-tokenized under the fresh secret.
    pub members_retokenized: u64,
}

/// `fauna.bridges.send_list_message` (User) — the canonical list-send fan-out
/// (§ Composing a list message). The nest validates ownership + the per-list
/// rate caps (§ Per-list rate accounting), then enqueues **one outbound per
/// subscribed member** — each with its own `List-*` headers (the per-recipient
/// one-click token) stamped into the body; the nest DKIM-signs each copy after
/// the stamp, at the outbound hand-out (the RFC 8058 List-* are in the signed
/// `h=` set). `message` is the client-composed RFC 5322 bytes; any
/// `List-*` the client wrote are stripped (the nest's stamp is authoritative).
/// This is the **only** list-send path: an external SMTP submission with MAIL
/// FROM = a list address is rejected (per-recipient stamping is structurally
/// impossible for a single-body submission, § How the per-list cap separates).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SendListMessageRequest {
    /// 16-byte list UUID; must belong to the caller.
    pub list_id: ByteBuf,
    /// The composed RFC 5322 message (headers + body) to fan out.
    pub message: ByteBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SendListMessageReply {
    /// How many per-member outbound rows were enqueued.
    pub queued_count: u64,
    /// The owner's remaining per-account list-recipient quota for today after
    /// this send (the compose-surface "approaching daily limit" meter).
    pub estimated_quota_remaining: u64,
}

/// One `mail_list_sends` audit row (§ Composing → the per-list send history).
/// `delivered_count` is the count the nest successfully queued (per-recipient
/// MX-delivery tracking is a later track; queued == delivered for this surface).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ListSendHistoryRow {
    pub sent_at: i64,
    pub recipient_count: i64,
    pub delivered_count: i64,
    pub unsubscribed_during_send: i64,
}

/// `fauna.bridges.list_list_send_history` (User) — the per-list send audit
/// (§ Composing). Owner-scoped; `not_found` if the list is not the caller's.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ListListSendHistoryRequest {
    /// 16-byte list UUID (must be caller-owned).
    pub list_id: ByteBuf,
    /// Max rows to return, newest first.
    pub limit: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ListListSendHistoryReply {
    pub sends: Vec<ListSendHistoryRow>,
}

// ── Per-account forward-all (mail forwarding) ────────────────────
//
// User-class WS-RPC surface for the per-account "forward all incoming mail
// to" knob (`mail.account.forward_all_to`), named in
// `docs/goal/behavior/mail-forwarding.md` § Per-account "forward all".
// The address is stored per-actor in `mail_account_settings.forward_all_to`
// at the plaintext routing-metadata floor in BOTH storage modes (the same
// tier as `local_domains`/aliases/admin-forwarders — § Where the forward
// config lives at rest). The separate-bridge-sealed alternative is the
// documented future upgrade (N1b), deferred.

/// `fauna.bridges.get_forward_all_to` (User) — `()` request; returns the
/// calling actor's forward-all target, or `None` when forwarding is disabled.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct GetForwardAllToRequest {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct GetForwardAllToReply {
    pub forward_all_to: Option<String>,
}

/// `fauna.bridges.set_forward_all_to` (User) — set a non-empty address to
/// enable forward-all, or clear (`None` / empty) to disable. The handler
/// RFC-5321-validates the address and rejects one that points at a hosted
/// `local_domains` address (that should be an alias, `mail-forwarding.md:244`).
/// Stored at the plaintext floor in both modes (no storage-mode branch).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SetForwardAllToRequest {
    #[serde(default)]
    pub forward_all_to: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SetForwardAllToReply {}

// ── Per-account spam-threshold override (Tier 3) ────────────────
//
// `docs/goal/behavior/mail-aliases.md` § Spam-threshold override — the MIDDLE
// tier of `per-alias override > per-account override > admin-tier default`, and
// `mail-policy-config.md` § Tier 3's `mail.account.spam_threshold_override`
// (surface `account-detail-mail-spam-threshold`). Stored per-actor in
// `mail_account_settings.spam_threshold_override` at the same plaintext
// routing-metadata floor as the forwarding columns beside it; the three tiers
// collapse nest-side at delivery (`fauna_mail::aliases::
// resolve_delivery_spam_threshold`) and ride out on the message as the
// `X-Fauna-Spam-Threshold` stamp, so nothing downstream re-resolves the chain.

/// `fauna.bridges.get_spam_threshold_override` (User) — `()` request; returns
/// the calling actor's per-account spam-folder threshold in whole points, or
/// `None` when the actor inherits the admin default.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct GetSpamThresholdOverrideRequest {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct GetSpamThresholdOverrideReply {
    #[serde(default)]
    pub spam_threshold_override: Option<u32>,
}

/// `fauna.bridges.set_spam_threshold_override` (User) — set a per-account
/// threshold, or clear it (`None`) to follow the admin default again.
/// **`Some(0)` is a real setting, not a clear**: it disables auto-Junk filing
/// for this account, which must outrank a non-zero admin default. Unbounded
/// above, exactly like the per-alias tier it layers under (`AliasControls
/// ::spam_threshold_override`) — the two tiers are one user-facing concept and
/// must not accept different ranges.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SetSpamThresholdOverrideRequest {
    #[serde(default)]
    pub spam_threshold_override: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SetSpamThresholdOverrideReply {}

// ── Per-account forward rate cap (Tier 3) ────────────────────────
//
// `docs/goal/behavior/mail-forwarding.md` § Per-account forward rate-limit —
// the user-visible `mail.account.forward_per_hour` (default 100/hour), which
// may be set lower than the admin ceiling `mail.outbound.forward_max_per_
// account_per_hour` but never higher. Stored per-actor in
// `mail_account_settings.forward_per_hour` beside the other per-account mail
// settings; the forward rate cap at `forward_message` reads it as
// `min(forward_per_hour, ceiling)`.

/// `fauna.bridges.get_forward_per_hour` (User) — `()` request; returns the
/// calling actor's hourly forward cap and the ceiling it may not exceed, so an
/// app bounds its field without knowing the admin tier.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct GetForwardPerHourRequest {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct GetForwardPerHourReply {
    pub forward_per_hour: u32,
    pub forward_per_hour_ceiling: u32,
    /// Forward-compat catch-all (transport.md § Schema and forward-compat
    /// discipline, rule 4) — an app-callable kind, so client↔nest wire.
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.bridges.set_forward_per_hour` (User) — overwrite the calling actor's
/// hourly forward cap. The nest refuses a value outside `1..=ceiling`
/// (`fauna_mail::validate_forward_per_hour`): zero would park every forward
/// until the queue evicts it, and above the ceiling is exactly what the goal
/// forbids. There is no clear — the setting always has a value (default 100).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SetForwardPerHourRequest {
    pub forward_per_hour: u32,
    /// Forward-compat catch-all (transport.md § Schema and forward-compat
    /// discipline, rule 4) — an app-callable kind, so client↔nest wire.
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SetForwardPerHourReply {}

// ── Forward delivery trigger (MTA perimeter, N2) ────────────────
//
// The forward DECISION is an MTA-perimeter decision: only the Go MTA
// holds the inbound plaintext during DATA (the nest stores only the
// recipient-sealed `encrypted_body`, so it cannot produce the plaintext
// copy a downstream MX needs). After the local mailbox write commits
// (`ingest_inbound_mail`), the MTA fetches the recipient's forward config
// via `fetch_recipient_forward_config` and — if set, non-null-sender, and
// loop-checks pass — enqueues the forward via `forward_message`.
// See `docs/goal/behavior/mail-forwarding.md` § Per-account "forward all"
// + § Storage-mode interaction + § Wire shapes.

/// `fauna.bridges.fetch_recipient_forward_config` (BridgeMta) — the MTA's
/// single chokepoint for reading a recipient's forward config at the
/// perimeter (mirrors the per-recipient `fetch_recipient_mls_pubkey`).
/// Today it returns just `forward_all_to`; the per-rule `forward`
/// destinations + rate cap join it as N3/N5 land. This is the one spot the
/// N1b sealed-fetch upgrade swaps (plaintext read → sealed-blob fetch +
/// bridge-side unseal), so callers must not read forward config elsewhere.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct FetchRecipientForwardConfigRequest {
    /// The recipient actor whose forward config to read (32 bytes).
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct FetchRecipientForwardConfigReply {
    /// The recipient's forward-all target, or `None` when forward-all is
    /// disabled (no row / NULL / blank).
    pub forward_all_to: Option<String>,
}

/// `fauna.bridges.fetch_recipient_filters` (BridgeMta) — the MTA's chokepoint
/// for reading a recipient's stored email filter rules at the perimeter, so the
/// pure `fauna_mail::filter::evaluate` engine can run pre-seal on plaintext
/// (`mail-forwarding.md` § Where rule eval runs — rule eval is an MTA-perimeter
/// decision; the nest only *stores + serves* rules, never evaluates on sealed
/// ciphertext). Returns the same `EmailFilter` shape the user-facing
/// `fauna.email.filters.list` returns — single-sourced through
/// `email_handlers::email_filters_for_actor` so the two surfaces never diverge
/// (#3) — in DB order (`priority ASC, id ASC`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct FetchRecipientFiltersRequest {
    /// The recipient actor whose filter rules to read (32 bytes).
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FetchRecipientFiltersReply {
    /// The recipient's stored filters, in evaluation order (`priority ASC, id ASC`).
    pub filters: Vec<crate::email::EmailFilter>,
}

/// Copy mode for a forward (`mail-forwarding.md` § Per-rule "forward to").
/// `Copy` keeps the local mailbox copy; `Redirect` forwards without one (a
/// per-rule redirect, the admin forwarder, and every other forward of a
/// message a redirect rule fired on — forward-all included). The
/// local-delivery skip for `Redirect` is the MTA's decision, made before
/// `forward_message`; nest persists the mode on the queued row, because
/// whether that row is a second copy or the only one decides what a
/// succession may burn.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ForwardCopyMode {
    #[default]
    Copy,
    Redirect,
}

/// `fauna.bridges.forward_message` (BridgeMta) — the MTA enqueues a forward
/// of an inbound message it has already locally delivered. Nest inserts one
/// `outbound_mail_queue` row (`is_forwarded = true`, carrying the forwarder
/// actor + rule for the SRS rewrite at queue-out (N3) and NDR routing (N4))
/// and returns its id. The SRS envelope rewrite happens at dispatch
/// (`fetch_outbound_due`, N3), NOT here — this call stores the *original*
/// envelope. `mail-forwarding.md` § Wire shapes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct ForwardMessageRequest {
    /// The forwarding actor (32 bytes) — the account-holder for forward-all,
    /// the rule-owner for per-rule, the admin for an admin forwarder. The
    /// SRS forwarder-actor + the NDR target + the rate-cap subject (N3-N5).
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    /// The source message's id (for NDR dedup / `forward_history`, N4).
    pub original_msgid: String,
    /// The original envelope MAIL FROM — SRS rewrites this at queue-out (N3).
    pub original_sender: String,
    /// The forward target address (the downstream recipient).
    pub destination: String,
    /// The plaintext source body to forward (RFC822; the MTA has stamped
    /// `X-Fauna-Forwarded-By` before calling). Sent plaintext to the
    /// downstream MX in both storage modes (§ Storage-mode interaction).
    #[serde(with = "serde_bytes")]
    pub raw_message: Vec<u8>,
    /// `"forward-all"` or a filter rule id — recorded for the NDR blurb (N4).
    pub rule_id_or_forward_all: String,
    /// Copy vs redirect (see `ForwardCopyMode`).
    #[serde(default)]
    pub copy_mode: ForwardCopyMode,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct ForwardMessageReply {
    /// The row id this forward landed on: the `outbound_mail_queue` row id when
    /// dispatched immediately (`queued = false`), or the `forward_queue` row id
    /// when the per-account hourly rate cap was hit and the forward was parked
    /// (`queued = true`, N5 `mail-forwarding.md` § Per-account forward
    /// rate-limit). The MTA fire-and-forgets either way.
    pub id: i64,
    /// `true` when the forward exceeded `min(forward_per_hour, admin-ceiling)`
    /// and was parked in `forward_queue` to be promoted at the rate-cap cadence
    /// (`:178`), instead of dispatched now. Informational for the MTA.
    #[serde(default)]
    pub queued: bool,
}

/// `fauna.bridges.decode_srs_bounce` (BridgeMta) — decode + verify an inbound
/// `SRS0=`/`SRS1=` recipient at RCPT-TO (`mail-forwarding.md` § Bounce decode).
/// `local_part` is the part *before* `@<our-domain>` (the bridge strips the
/// domain). On a verified Fauna-issued bounce, nest recovers the forwarder
/// actor + the bounced destination from the `outbound_mail_queue` row the SRS
/// short-id (the row id) names, so N4 can route the NDR to the forwarder's
/// mailbox rather than the original sender.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct DecodeSrsBounceRequest {
    /// The recipient local-part, without the trailing `@<our-domain>`.
    pub local_part: String,
}

/// Outcome of a `decode_srs_bounce`. `outcome` is the discriminator; the
/// payload fields are populated only on `"ok"`:
/// - `"ok"` — verified our-issued bounce; `forwarder_actor_id` /
///   `original_sender` / `original_destination` are set. N4 delivers the DSN
///   to the forwarder.
/// - `"not_srs"` — not an `SRS0=`/`SRS1=` address (treat as a normal recipient,
///   not a bounce).
/// - `"malformed"` — structurally invalid SRS → `550`, no retry.
/// - `"mac_fail"` — HMAC mismatch (forged/corrupt) → `550 5.1.1`, no retry
///   (`mail-forwarding.md:100`).
/// - `"expired"` — `TT` age over the max → `550 5.4.4` (`:101`).
/// - `"orphan"` — verified, but the forwarding row is gone (account deleted /
///   row pruned) → drop + counter, never the admin mailbox (`:104,:117`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct DecodeSrsBounceReply {
    /// Discriminator: `ok` / `not_srs` / `malformed` / `mac_fail` / `expired`
    /// / `orphan`.
    pub outcome: String,
    /// The forwarding-config owner (32 bytes); set only on `outcome="ok"`,
    /// empty otherwise.
    #[serde(with = "serde_bytes")]
    pub forwarder_actor_id: Vec<u8>,
    /// The original sender the forward carried; set only on `outcome="ok"`.
    pub original_sender: String,
    /// The downstream address that bounced (the forward's recipient); set only
    /// on `outcome="ok"`.
    pub original_destination: String,
}

/// `fauna.bridges.rotate_srs_secret` (Admin) — the admin issues a fresh SRS
/// secret, held alongside the prior one as the 2-secret rotation overlap
/// (`mail-forwarding.md` § Wire shapes `:245`, § Architectural rules `:262`).
/// The request carries no parameters: nest generates the random 32-byte secret
/// itself and the admin **never** supplies or reads the bytes (`:279`). Action
/// button on the admin Bridges-detail page.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct RotateSrsSecretRequest {
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4). This kind is APP-callable,
    /// so it is client↔nest wire and rule 4's in-image `strict` opt-out
    /// does not reach it: within a major, an older peer must tolerate a
    /// newer one's added field in BOTH directions
    /// (version-compatibility.md § I2).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct RotateSrsSecretReply {
    /// When the new secret was minted (epoch seconds) — observability only;
    /// the secret bytes are never returned (`:279`).
    pub rotated_at: i64,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4). This kind is APP-callable,
    /// so it is client↔nest wire and rule 4's in-image `strict` opt-out
    /// does not reach it: within a major, an older peer must tolerate a
    /// newer one's added field in BOTH directions
    /// (version-compatibility.md § I2).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── Local-domain admin (multidomain) ────────────────────────────
//
// Admin-class WS-RPC surface for the `mail_domains` table, named in
// `docs/goal/behavior/mail-multidomain.md` § Wire shapes. The nest DB
// API + the `ConfigSnapshot.local_domains` projection already exist;
// these types wire the admin-client → nest control plane on top of them.

/// Per-domain role-address overrides — the typed shape of the
/// `mail_domains.role_address_overrides` column (`mail-multidomain.md`
/// § Wire shape + storage). Each field is an optional 64-char lowercase actor
/// hex; `None` means "no override — fall back to the deployment admin". Only
/// the four overridable roles are representable: `tlsrpt` / `dmarc-report`
/// always route to the deployment-wide processor, so storing them would be a
/// footgun.
///
/// One shape everywhere: the column's JSON object is its at-rest encoding
/// (`fauna_mail::aliases::role_overrides::{parse_stored, to_stored}`), and the
/// admin-mail wire carries it typed in [`MailDomainRow`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RoleAddressOverrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub postmaster: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abuse: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub noc: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub security: Option<String>,
    /// Forward-compat catch-all (transport.md § Schema and forward-compat
    /// discipline, rule 4): this shape rides the app-callable admin-mail
    /// replies, so it is client↔nest wire.
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

impl RoleAddressOverrides {
    /// No role carries an override.
    pub fn is_empty(&self) -> bool {
        self.postmaster.is_none()
            && self.abuse.is_none()
            && self.noc.is_none()
            && self.security.is_none()
    }

    /// Set (or clear, with `None`) one role's override to a 64-char lowercase
    /// actor hex. `key` is a [`RoleAddressKind::as_storage_key`]; an unknown key
    /// is a no-op (the wire enum guarantees a valid key — this is defensive).
    pub fn set(&mut self, key: &str, actor_hex: Option<String>) {
        match key {
            "postmaster" => self.postmaster = actor_hex,
            "abuse" => self.abuse = actor_hex,
            "noc" => self.noc = actor_hex,
            "security" => self.security = actor_hex,
            _ => {}
        }
    }

    /// The override **target actor** for a lowercased role local-part, or
    /// `None` to fall back to the deployment admin: a non-overridable
    /// local-part (incl. `tlsrpt` / `dmarc-report`), an unset key, or a value
    /// that is not 32-byte hex (degrade to admin, never reject).
    pub fn resolve(&self, local_part: &str) -> Option<[u8; 32]> {
        let hex_str = match local_part {
            "postmaster" => self.postmaster.as_deref(),
            "abuse" => self.abuse.as_deref(),
            "noc" => self.noc.as_deref(),
            "security" => self.security.as_deref(),
            _ => None,
        }?;
        if hex_str.len() != 64 {
            return None;
        }
        let mut out = [0u8; 32];
        hex::decode_to_slice(hex_str, &mut out).ok()?;
        Some(out)
    }
}

/// DMARC policy mode for the `p=` / `sp=` tags (RFC 7489 §6.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DmarcMode {
    None,
    Quarantine,
    Reject,
}

impl DmarcMode {
    /// The RFC 7489 tag value.
    pub fn as_tag(self) -> &'static str {
        match self {
            DmarcMode::None => "none",
            DmarcMode::Quarantine => "quarantine",
            DmarcMode::Reject => "reject",
        }
    }
}

/// Identifier-alignment mode for the `adkim=` / `aspf=` tags (RFC 7489 §6.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DmarcAlignment {
    #[serde(rename = "s")]
    Strict,
    #[serde(rename = "r")]
    Relaxed,
}

impl DmarcAlignment {
    /// The RFC 7489 tag value.
    pub fn as_tag(self) -> &'static str {
        match self {
            DmarcAlignment::Strict => "s",
            DmarcAlignment::Relaxed => "r",
        }
    }
}

/// Forensic-report options for the `fo=` tag (RFC 7489 §6.3). Only emitted
/// when `ruf_publish` is set — has no effect without a `ruf=` destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DmarcForensicOptions {
    /// `0` — generate a report only if all underlying checks failed.
    #[serde(rename = "0")]
    AllFail,
    /// `1` — generate a report if any check produced a non-aligned result (default).
    #[serde(rename = "1")]
    AnyFailure,
    /// `d` — DKIM-failure reports.
    #[serde(rename = "d")]
    Dkim,
    /// `s` — SPF-failure reports.
    #[serde(rename = "s")]
    Spf,
}

impl DmarcForensicOptions {
    /// The RFC 7489 tag value.
    pub fn as_tag(self) -> &'static str {
        match self {
            DmarcForensicOptions::AllFail => "0",
            DmarcForensicOptions::AnyFailure => "1",
            DmarcForensicOptions::Dkim => "d",
            DmarcForensicOptions::Spf => "s",
        }
    }
}

/// A per-domain DMARC override partial-record — the typed shape of the
/// `mail_domains.dmarc_overrides` column (`dmarc-reporting.md` § Multi-domain
/// deployments). Every field is optional; an absent field inherits the
/// deployment-wide base. Keys are the `mail-policy-config.md` § DMARC catalog
/// binding names. One shape everywhere: the column's JSON object is its at-rest
/// encoding, and the admin-mail wire carries it typed in [`MailDomainRow`];
/// `fauna_mail::dmarc_publish` overlays it onto the published record.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DmarcOverrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy_mode: Option<DmarcMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subdomain_policy_mode: Option<DmarcMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pct: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adkim_mode: Option<DmarcAlignment>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aspf_mode: Option<DmarcAlignment>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rua_destination: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ruf_publish: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ruf_destination: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fo_mode: Option<DmarcForensicOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ri_seconds: Option<u32>,
    /// Forward-compat catch-all (transport.md § Schema and forward-compat
    /// discipline, rule 4): client↔nest wire, like its sibling above.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// Wire representation of one `mail_domains` row. Mirrors
/// `bins/fauna-nest/src/db/mail_domains.rs::MailDomain` with
/// wire-friendly byte buffers (`ByteBuf` rather than `[u8; N]`) and a
/// parsed `dkim_algorithms` list (the DB stores it as a JSON-text
/// column; the nest handler parses it for the wire). Per
/// `mail-multidomain.md` § The `mail_domains` model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct MailDomainRow {
    /// 16-byte UUID.
    pub domain_id: ByteBuf,
    pub domain_name: String,
    pub is_primary: bool,
    pub added_at: i64,
    pub removed_at: Option<i64>,
    pub restored_at: Option<i64>,
    pub dkim_selector: Option<String>,
    pub dkim_rotation_days: Option<i64>,
    /// e.g. `["ed25519", "rsa-2048"]`.
    pub dkim_algorithms: Vec<String>,
    pub mta_sts_mode: String,
    pub mta_sts_max_age_seconds: i64,
    pub mta_sts_cert_mode: String,
    /// 32-byte actor id; `None` = no per-domain catch-all.
    pub catch_all_actor_id: Option<ByteBuf>,
    /// Per-domain role-address overrides, typed (`mail-multidomain.md` § Wire
    /// shape + storage). Empty = every role routes to the deployment admin. The
    /// nest projects the stored column through this shape, and a stored value
    /// that does not decode projects as empty — one corrupt column never fails
    /// the reply it rides in.
    pub role_address_overrides: RoleAddressOverrides,
    /// The per-domain DMARC override partial-record, typed
    /// (`dmarc-reporting.md` § Multi-domain deployments). Every field `None` =
    /// no override; projected and degraded exactly like
    /// [`Self::role_address_overrides`].
    pub dmarc_overrides: DmarcOverrides,
    pub spf_record: String,
    /// Epoch-ms when the active `dkim_selector` last became active (stamped on
    /// every rotation flip); `None` until the first flip (the due window then
    /// runs from `added_at`). For the admin DKIM surface's "last rotated"
    /// display. `mail-multidomain.md` § Rotation.
    #[serde(default)]
    pub dkim_selector_activated_at: Option<i64>,
    /// Nest-computed signal: the active DKIM selector is due for rotation
    /// (`now - activated_at ≥ effective rotation_days`). Computed nest-side
    /// because the deployment-wide `mail.dkim.rotation_days` default lives
    /// there; the admin client's automatic DKIM-rotation producer reads this to
    /// provision + publish a fresh selector (no UI — scheduled and admin-invisible).
    /// `mail-multidomain.md` § Rotation.
    #[serde(default)]
    pub dkim_rotation_due: bool,
    /// Epoch-ms when a succession ceremony or the boot reconcile last
    /// cleared `catch_all_actor_id` because it named a retired identity;
    /// `None` when the catch-all was never set, or was last set/cleared by
    /// an admin. For the admin-dns surface's "a succession cleared this"
    /// state. `succession-aftermath.md` § Re-key scope.
    #[serde(default)]
    pub catch_all_cleared_by_succession_at: Option<i64>,
}

/// `fauna.bridges.add_local_domain` (Admin). `is_primary` is **not** a
/// parameter — the nest sets it to `true` iff no active domain exists
/// yet (first domain claimed = the deployment's primary), per
/// `mail-multidomain.md` § The primary domain. Idempotent on
/// `domain_name`: an already-active domain returns the existing row +
/// `skipped = true` (§ add `:349`).
///
/// The MTA-STS policy mode is **not** a parameter either: no human sets it.
/// The nest stores every new domain `testing` and advances it to `enforce` by
/// itself (`mail-multidomain.md` § The advance).
///
/// `dkim_algorithms` is intentionally absent: the nest `add_mail_domain`
/// does not persist it (column defaults `["ed25519","rsa-2048"]`), and
/// per-domain DKIM is wholly deferred — it lands with the track that
/// consumes it. `catch_all_actor` + `dkim_selector_override` *are*
/// persisted (the selector feeds the domain's `FetchConfigReply.dkim_selectors` entry).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AddLocalDomainRequest {
    pub domain: String,
    pub mta_sts_cert_mode: String,
    /// 32-byte actor id for a per-domain catch-all; `None` = none.
    #[serde(default)]
    pub catch_all_actor: Option<ByteBuf>,
    #[serde(default)]
    pub dkim_selector_override: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AddLocalDomainReply {
    pub domain: MailDomainRow,
    /// `true` when the domain was already active and this call was a
    /// no-op idempotent re-add.
    pub skipped: bool,
}

/// `fauna.bridges.remove_local_domain` (Admin) — soft-delete. Refuses
/// the primary (`mail-multidomain.md` § Removing `:389`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct RemoveLocalDomainRequest {
    pub domain: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct RemoveLocalDomainReply {
    pub domain: MailDomainRow,
}

/// `fauna.bridges.restore_local_domain` (Admin) — un-soft-delete within
/// the 30-day recovery window.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct RestoreLocalDomainRequest {
    pub domain: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct RestoreLocalDomainReply {
    pub domain: MailDomainRow,
}

/// `fauna.bridges.list_local_domains` (Admin) — `()` request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ListLocalDomainsRequest {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ListLocalDomainsReply {
    pub active: Vec<MailDomainRow>,
    pub soft_deleted_within_30d: Vec<MailDomainRow>,
}

/// `fauna.bridges.update_local_domain_config` (Admin) — partial edit of
/// the per-domain knobs. Every field is a plain `Option<T>` (`None` = leave
/// alone, `Some` = set). A knob that needs a genuine clear — a tri-state —
/// rides its own dedicated setter instead (`set_catch_all_actor`,
/// `set_role_address`, `set_dkim_rotation_days`), because `Option<Option<T>>`
/// is not DAG-CBOR-round-trippable (both `None` and `Some(None)` encode as
/// `null`; `serialization.md` § Tri-state fields). The DMARC policy is not
/// one of those: its "clear" is choosing the default, so a single
/// `Option<DmarcMode>` carries all three states. The MTA-STS policy *mode*
/// is not here at all: the nest advances it by itself (`mail-multidomain.md`
/// § The advance).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct UpdateLocalDomainConfigRequest {
    pub domain: String,
    #[serde(default)]
    pub mta_sts_max_age_seconds: Option<i64>,
    #[serde(default)]
    pub mta_sts_cert_mode: Option<String>,
    #[serde(default)]
    pub spf_record: Option<String>,
    /// The domain's published DMARC policy — the per-domain policy select
    /// (`dmarc-reporting.md` § Multi-domain deployments). `None` = unchanged;
    /// `Some(Quarantine | None)` sets the stored override's `policy_mode` and
    /// `subdomain_policy_mode` together; `Some(Reject)` — the default — clears
    /// both keys, so the domain inherits the deployment policy again. The nest
    /// merges into the stored partial, leaving its other keys alone. Additive:
    /// an older nest ignores it and an older client never sends it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dmarc_policy_mode: Option<DmarcMode>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct UpdateLocalDomainConfigReply {
    pub domain: MailDomainRow,
}

/// `fauna.bridges.set_catch_all_actor` (Admin) — designate (or clear) the
/// per-domain catch-all actor (`mail_domains.catch_all_actor_id`;
/// `mail-aliases.md` § Kind 4, `mail-multidomain.md` § Per-domain catch-all).
/// A dedicated setter rather than a field on
/// [`UpdateLocalDomainConfigRequest`]: catch-all is a *clearable* knob that
/// needs `set` / `clear`, and `Option<Option<T>>` is not DAG-CBOR-round-trippable
/// (both `None` and `Some(None)` encode as `null`). Here the whole call sets the
/// catch-all, so a single `Option<ByteBuf>` is unambiguous — `Some(id)` = set,
/// `None` = clear (there is no "leave alone" state). Mirrors the DB
/// `MailDomainUpdate { catch_all_actor_id: Some(<this>) }` tri-state outer-`Some`.
/// The RCPT-time resolver *consumption* of `catch_all_actor_id` is a separate
/// track (`mail-multidomain.md` § Implementation status today).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SetCatchAllActorRequest {
    pub domain: String,
    /// 32-byte actor id to designate; `None` clears the domain's catch-all.
    #[serde(default)]
    pub actor_id: Option<ByteBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SetCatchAllActorReply {
    pub domain: MailDomainRow,
}

/// The RFC 2142 role local-parts whose per-domain delivery target the admin may
/// override (`mail-multidomain.md` § Per-domain override). `tlsrpt` /
/// `dmarc-report` are intentionally **absent**: they always route to the
/// deployment-wide report processor regardless of any override, so they are not
/// settable. Wire-tagged in `snake_case` to match the `role_address_overrides`
/// JSON storage keys (`fauna_mail::aliases::role_overrides`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RoleAddressKind {
    #[default]
    Postmaster,
    Abuse,
    Noc,
    Security,
}

impl RoleAddressKind {
    /// The `role_address_overrides` JSON storage key for this role — also the
    /// reserved local-part it routes (`postmaster` / `abuse` / `noc` / `security`).
    pub fn as_storage_key(self) -> &'static str {
        match self {
            RoleAddressKind::Postmaster => "postmaster",
            RoleAddressKind::Abuse => "abuse",
            RoleAddressKind::Noc => "noc",
            RoleAddressKind::Security => "security",
        }
    }
}

/// `fauna.bridges.set_role_address` (Admin) — designate (or clear) the per-domain
/// override actor for one overridable role address (`mail_domains.role_address_overrides`;
/// `mail-multidomain.md` § Per-domain role-address routing). A dedicated setter
/// mirroring [`SetCatchAllActorRequest`]: the per-key set/clear is a tri-state
/// (`Option<Option<T>>`) that is not DAG-CBOR-round-trippable, so it cannot ride
/// [`UpdateLocalDomainConfigRequest`]. One call sets/clears one (domain, role):
/// `actor_id = Some(32 bytes)` designates the override; `None` clears it (the
/// role then falls back to the deployment admin). The other roles on the domain
/// are preserved (the nest does an atomic read-merge-write of the override map).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SetRoleAddressRequest {
    pub domain: String,
    pub role: RoleAddressKind,
    /// 32-byte actor id to designate as this role's override; `None` clears it.
    #[serde(default)]
    pub actor_id: Option<ByteBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SetRoleAddressReply {
    pub domain: MailDomainRow,
}

/// `fauna.bridges.set_dkim_rotation_days` (Admin) — set or clear a domain's
/// per-domain DKIM rotation interval (`mail_domains.dkim_rotation_days`;
/// `mail-multidomain.md` § Selector — *"the only admin lever"* for accelerated
/// rotation, surfaced on `admin-dns`). A dedicated setter mirroring
/// `set_catch_all_actor` / `set_role_address`: the set/clear is a tri-state not
/// DAG-CBOR-round-trippable, so it can't ride `UpdateLocalDomainConfigRequest`.
/// `rotation_days = Some(n)` sets the per-domain override (a *shorter* value
/// accelerates rotation — the deployment-wide default is quarterly); `None`
/// clears it, so the domain inherits the deployment default. DKIM is otherwise
/// automatic + admin-invisible (no client UI consumes this — it is the
/// incident-response / future-automation control plane, like `force_rotate_dkim`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SetDkimRotationDaysRequest {
    pub domain: String,
    /// `Some(days)` sets the per-domain override; `None` clears it (inherit the
    /// deployment-wide default). A non-positive value means "always due" — the
    /// degenerate accelerator the scheduled rotation-mint treats as immediate.
    #[serde(default)]
    pub rotation_days: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SetDkimRotationDaysReply {
    pub domain: MailDomainRow,
}

/// One `mail_domain_renames` row (`docs/goal/behavior/mail-primary-domain-
/// rename.md` § Data). `state` is the `fauna_mail::RenameState` wire string
/// (`fauna-protocol` doesn't depend on `fauna-mail`, so the enum crosses the
/// wire as its string form). The post-flip fields (`flipped_at`, `grace_*`,
/// `new_cert_fingerprint`, `ready_to_complete_at`, `completed_at`, …) are defined
/// now for the full lifecycle but stay `None` in SLICE 1 (the no-side-effect
/// RPCs only ever produce a `requested` or `aborted` row).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct MailDomainRenameRow {
    /// 16-byte UUID.
    pub rename_id: ByteBuf,
    /// 16-byte `mail_domains.domain_id` of the current primary.
    pub old_primary_domain_id: ByteBuf,
    /// 16-byte `mail_domains.domain_id` of the promotion target.
    pub new_primary_domain_id: ByteBuf,
    /// `fauna_mail::RenameState` as a snake_case string.
    pub state: String,
    pub started_at: i64,
    pub grace_days: i64,
    #[serde(default)]
    pub cert_acquired_at: Option<i64>,
    #[serde(default)]
    pub new_cert_fingerprint: Option<String>,
    #[serde(default)]
    pub flipped_at: Option<i64>,
    #[serde(default)]
    pub grace_started_at: Option<i64>,
    #[serde(default)]
    pub grace_ends_at: Option<i64>,
    #[serde(default)]
    pub ready_to_complete_at: Option<i64>,
    #[serde(default)]
    pub completed_at: Option<i64>,
    #[serde(default)]
    pub aborted_at: Option<i64>,
    #[serde(default)]
    pub abort_reason: Option<String>,
    /// 32-byte actor id of the admin who initiated the rename.
    pub initiated_by_actor_id: ByteBuf,
}

/// `fauna.bridges.start_primary_domain_rename` (Admin) — begin a primary-domain
/// rename (`mail-primary-domain-rename.md` § Wire shapes). The new primary must
/// already exist as an additional (two-step: `add_local_domain` first). `grace_days`
/// defaults to `fauna_mail::DEFAULT_GRACE_DAYS` (7) when absent; range `[1, 30]`.
/// SLICE 1 inserts the row at `requested` and does **not** auto-advance to
/// `cert_issuance` or fire any side effect.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct StartPrimaryDomainRenameRequest {
    /// 16-byte `mail_domains.domain_id` of the promotion target.
    pub new_primary_domain_id: ByteBuf,
    #[serde(default)]
    pub grace_days: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct StartPrimaryDomainRenameReply {
    pub rename: MailDomainRenameRow,
}

/// `fauna.bridges.get_primary_domain_rename_status` (Admin) — `()` request;
/// the reply's `rename` is `None` when no rename is in flight.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct GetPrimaryDomainRenameStatusRequest {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct GetPrimaryDomainRenameStatusReply {
    #[serde(default)]
    pub rename: Option<MailDomainRenameRow>,
}

/// `fauna.bridges.list_primary_domain_renames` (Admin) — `()` request; enumerate
/// all renames (in-flight + terminal) newest-first for the audit surface.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ListPrimaryDomainRenamesRequest {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ListPrimaryDomainRenamesReply {
    pub renames: Vec<MailDomainRenameRow>,
}

/// `fauna.bridges.abort_primary_domain_rename` (Admin) — unwind a rename. Valid
/// from any non-terminal state (SLICE 1 only reaches it from `requested`, where
/// there is nothing to unwind).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AbortPrimaryDomainRenameRequest {
    /// 16-byte rename id.
    pub rename_id: ByteBuf,
    #[serde(default)]
    pub abort_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AbortPrimaryDomainRenameReply {
    pub rename: MailDomainRenameRow,
}

/// `fauna.bridges.complete_primary_domain_rename` (Admin) — finalize a rename in
/// `ready_to_complete` (or in `grace` with `force = true`, accepting the early
/// cache-flush risk); terminal (`mail-primary-domain-rename.md` § Wire shapes,
/// SLICE 4). Without `force`, a `grace` row refuses `grace_period_not_expired`.
/// Carries `rename_id` (like `abort`) so a stale rename can't be completed across
/// an abort-then-restart race — the admin client has it from `start`/`get_status`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct CompletePrimaryDomainRenameRequest {
    /// 16-byte rename id.
    pub rename_id: ByteBuf,
    /// Override the grace window and complete early from `grace` (default false).
    #[serde(default)]
    pub force: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct CompletePrimaryDomainRenameReply {
    pub rename: MailDomainRenameRow,
}

/// `fauna.bridges.extend_primary_domain_rename_grace` (Admin) — push
/// `grace_ends_at` further out by `additional_days × 1 day`
/// (`mail-primary-domain-rename.md` § Wire shapes, SLICE 4). Valid from `grace`
/// or `ready_to_complete` (a `ready_to_complete` row reverts to `grace`, since
/// the deadline is future again). `additional_days` range `[1, 30]` per call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ExtendPrimaryDomainRenameGraceRequest {
    /// 16-byte rename id.
    pub rename_id: ByteBuf,
    /// Days to add to `grace_ends_at`; range `[1, 30]` per call.
    pub additional_days: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ExtendPrimaryDomainRenameGraceReply {
    pub rename: MailDomainRenameRow,
}

/// `fauna.bridges.force_rotate_dkim` (Admin) — emergency DKIM rotation for one
/// local domain (`mail-multidomain.md` § Rotation, "Emergency rotation").
/// This call flips the domain's **active** selector (`mail_domains.dkim_selector`)
/// to the newest selector the rotation mint seated, immediately — skipping the
/// 24 h peer-cache wait of a scheduled rotation — and the nest signs with the
/// new selector at the next outbound hand-out. DKIM is
/// an automatic concern (no manual selector knob), so this is the only public
/// surface that mutates `dkim_selector`. DNS publish/unpublish of the selector's
/// TXT records is client-side (`dns-management.md`), not nest's job here.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ForceRotateDkimRequest {
    pub domain: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ForceRotateDkimReply {
    /// The updated row, carrying the now-active `dkim_selector`.
    pub domain: MailDomainRow,
}

/// `fauna.bridges.provision_self_signed_cert` (Admin) — synthesize a
/// self-signed TLS cert for an active local mail domain and seal+fan it
/// out to every approved bridge with an x25519 pubkey (the bridge fetches
/// it via `fauna.bridges.fetch_tls_cert_blob`). The WS-RPC twin of the
/// former HTTP route
/// `POST /api/admin/local_domains/{domain}/self_signed_cert` (no longer served), per
/// `mail-bridge-lifecycle.md` § TLS provisioning, "Admin-synthesized
/// (self-signed)". The cert has a 90-day window and is NOT auto-renewed —
/// the admin re-calls to refresh (or to lift the seal onto a bridge that
/// has since attested its x25519 pubkey).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ProvisionSelfSignedCertRequest {
    pub domain: String,
    /// Extra DNS SANs alongside `domain` (which is always the CN + first
    /// SAN). Empty = just `domain`.
    #[serde(default)]
    pub additional_dns_sans: Vec<String>,
}

/// `{role, bridge_id}` identity of a bridge a synthesized cert was (or was
/// not) sealed to. Wire mirror of the nest-internal
/// `self_signed_cert::SealedBridge`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SealedBridgeInfo {
    pub role: String,
    pub bridge_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ProvisionSelfSignedCertReply {
    /// Bridges the sealed `TlsCertBlob` was fanned out to (had an x25519
    /// pubkey on file).
    pub bridges_sealed_to: Vec<SealedBridgeInfo>,
    /// Approved bridges skipped because they had no x25519 pubkey yet —
    /// re-call after the bridge attests via `register_service_user`.
    pub bridges_skipped_no_x25519: Vec<SealedBridgeInfo>,
    /// Unix-seconds expiry of the synthesized cert (90-day window).
    pub expires_at_unix: i64,
}

/// `fauna.bridges.restore_real_tls_cert` (Admin) — the **instant** switch back to
/// a real (CA-issued) cert after a self-signed override. Non-destructive and
/// parameterless: if a real cert was preserved it is restored immediately;
/// otherwise it is a no-op, because the ACME lifecycle task already self-heals a
/// self-signed cert to a real one automatically. (So this RPC is purely an
/// optimization that skips the ~5-minute self-heal wait — it is never required.)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct RestoreRealTlsCertRequest {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct RestoreRealTlsCertReply {
    /// `true` only when a preserved real cert was restored and is serving now.
    pub restored_immediately: bool,
    /// Machine token for the path taken: `"backup"` (a preserved real cert was
    /// copied back, hot-reloaded by the cert watcher) or `"self_heal"` (no
    /// backup → nothing to do; the ACME lifecycle self-heals to a real cert).
    pub method: String,
    /// Human-readable summary for the admin UI.
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FetchRecipientMlsPubkeyRequest {
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    /// True ONLY for a genuine mail-new-ingest resolution (the MTA's
    /// per-delivery seal-key lookup, content-sealing-epochs design § 3/§ 6) —
    /// `false`/absent for every other caller. This reply also feeds the MDA
    /// session's cached pubkey for CalDAV/CardDAV PUT, several
    /// collection-metadata seals, and the IMAP spam-model re-seal, none of
    /// which have an epoch opener; before this field existed, the handler
    /// applied the epoch-sealing gate to ALL of them once the write flip
    /// landed, silently sealing non-mail bytes under a mail epoch key none
    /// of those readers could open.
    /// Additive + defaulted false, so a non-mail seal site omitting it gets
    /// the standing-key behavior.
    #[serde(default)]
    pub mail_new_ingest: bool,
}

/// Both public halves of a recipient's standing (or, for a mail-new-ingest
/// resolution, per-epoch) seal key — the value of
/// [`FetchRecipientMlsPubkeyReply::key`]. The provision request requires both
/// halves and the nest stores them in one row, so they travel as one value:
/// a key with one half missing cannot be spelled
/// (`docs/goal/architecture/security/post-quantum.md` § Capability
/// negotiation and default-selection policy).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecipientSealKeyHalves {
    /// 32-byte X25519 public key the MTA encrypts mail content to.
    pub mls_pubkey: ByteBuf,
    /// The recipient's 1184-byte ML-KEM-768 encapsulation key
    /// (`fauna_pq_kem::MLKEM768_ENCAPS_KEY_LEN`), MSEK-derived alongside
    /// `mls_pubkey`. The MTA assembles the X-Wing public key as
    /// `from_parts(mlkem_ek, mls_pubkey)` and seals with the hybrid suite.
    pub mlkem_ek: ByteBuf,
    /// Forward-compat catch-all (`transport.md` § Schema and forward-compat
    /// discipline, rule 4) — a third-party principal reads this reply too.
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FetchRecipientMlsPubkeyReply {
    /// The recipient's seal key. `None` means the recipient has not yet
    /// provisioned one on this nest — caller decides whether to bounce,
    /// spool, or fall back.
    pub key: Option<RecipientSealKeyHalves>,
    /// `true` when `key` is `None` because this actor is a succession's
    /// **successor** (`succession-aftermath.md` § Re-key scope) who has not
    /// yet re-provisioned an MLS pubkey — the bounded window between the
    /// succession ceremony and the successor's first sign-in, during which
    /// mail should tempfail rather than bounce (`smtp-server.md`
    /// § Error / tempfail strategy). Meaningless when `key` is `Some`.
    /// Additive + defaulted false, so a reply not in succession (missing the
    /// field) decodes as "never onboarded" — the conservative reading a
    /// caller already knows how to handle.
    #[serde(default)]
    pub succession_pending: bool,
}

/// Admin-class request to upsert a recipient's MLS public encryption key
/// into the `actor_mls_pubkeys` table. Reply is the shared
/// `wrapped_blob::ProvisionReply`.
///
/// This is the production writer for the table that
/// `fauna.bridges.fetch_recipient_mls_pubkey` reads on every inbound
/// DATA — without it, the MTA's encrypt-to-recipient step on inbound
/// mail has no key to seal to and the ingest path rejects with
/// "recipient has not provisioned an MLS pubkey". Per the mail-bridge
/// rearchitecture's inbound mail flow (tracked internally).
///
/// `actor_id` / `mls_pubkey` are 32-byte buffers; the handler validates
/// lengths and rejects malformed payloads. Nest stores the bytes opaquely
/// — semantic validation (curve point, MLS-credential binding) is the
/// responsibility of the client that produced the key material.
///
/// `deny_unknown_fields` is retained deliberately: this lives in the
/// `bridge_routing` in-image MTA/MDA data-plane, where cross-version skew
/// is handled by capability negotiation rather than struct tolerance
/// (`docs/goal/architecture/transport.md` § Schema and forward-compat
/// discipline, rule 4). The post-quantum `mlkem_ek` is required: every
/// client publishes it (no capability token — `post-quantum.md`
/// § Capability negotiation).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct ProvisionRecipientMlsPubkeyRequest {
    /// 32-byte recipient actor id (Ed25519 pubkey).
    pub actor_id: ByteBuf,
    /// 32-byte X25519 public key the MTA encrypts mail content to.
    pub mls_pubkey: ByteBuf,
    /// Post-quantum sibling (S3c): the recipient's 1184-byte ML-KEM-768
    /// encapsulation key (`fauna_pq_kem::MLKEM768_ENCAPS_KEY_LEN`),
    /// MSEK-derived alongside `mls_pubkey`. Required: the MTA seals the
    /// recipient's inbound mail under the hybrid suite from the first
    /// message.
    pub mlkem_ek: ByteBuf,
    /// Content-sealing-epoch schedule (design 2026-07-18 § 3): the horizon
    /// of per-epoch mail sealing **public** keys the owner's client
    /// pre-publishes so the MTA can epoch-seal inbound mail with no client
    /// online. Additive: clients
    /// publish it unconditionally (no capability token). `None`/absent
    /// leaves any previously published schedule untouched (the handler upserts, never clears —
    /// rows are floor-safe public keys).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epoch_keys: Option<Vec<EpochSealKey>>,
}

/// One epoch's published mail sealing public halves — an element of
/// [`ProvisionRecipientMlsPubkeyRequest::epoch_keys`], stored as a row of
/// `actor_epoch_seal_keys`. Public keys only; floor-safe.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EpochSealKey {
    /// The mail sealing-epoch index
    /// (`fauna_mls::wrapped_blob::mail_sealing_epoch_of`).
    pub epoch: u64,
    /// 32-byte X25519 public half of the per-epoch recipient keypair
    /// (`derive_recipient_epoch_hpke_keypair(msek, epoch).1`).
    pub mls_pubkey: ByteBuf,
    /// 1184-byte ML-KEM-768 encapsulation key of the per-epoch X-Wing
    /// keypair — required, like the standing `mlkem_ek`.
    pub mlkem_ek: ByteBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FetchRecipientIndexKeyRequest {
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
}

/// Reply for `fauna.bridges.fetch_recipient_index_key`. Mirrors
/// `FetchRecipientMlsPubkeyReply`'s shape exactly: the recipient's
/// 32-byte X25519 public key for HPKE-encrypted index hints, or `None`
/// when the recipient has not yet provisioned one.
///
/// Index pubkey is provisioned separately from the MLS pubkey because
/// the index hint and the message body are decrypted by different
/// readers in the long run — the body is opened by the recipient's MDA
/// (on demand, per message), while the index hint is opened by the
/// recipient's index-builder client (in bulk, when reindexing). Keeping
/// the keys separable lets a future deployment scope the index-builder's
/// access to just the index hint set without granting body-read
/// capability. Per `docs/goal/architecture/encryption-at-rest.md`
/// § Index shape.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FetchRecipientIndexKeyReply {
    /// 32-byte X25519 public key the MTA encrypts the index hint to.
    /// `None` = recipient has not yet provisioned one (Phase E will
    /// add the provisioning RPC alongside MLS pubkey provisioning).
    pub pubkey: Option<ByteBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FetchConfigRequest {
    /// Forward-compatible scope selector. Slice 1 only accepts `"all"`;
    /// future scopes (`"spam"`, `"imap"`, etc.) will surface partial
    /// reply variants once a Go bridge actually subscribes to a subset.
    pub scope: String,
}

/// Request for `fauna.bridges.get_mail_config` — the **admin read twin** of
/// `fauna.bridges.fetch_config`. The bridge processes call `fetch_config`
/// (`BridgeMta | BridgeMda` class) to pull their live config; the admin client
/// calls this (`Admin` class) to read the same **overlaid effective config**
/// (catalog defaults with the per-sub-struct `put_<substruct>_policy` overrides
/// applied) so the `admin-mail` policy form can hydrate before edit — without
/// it the form could only blind-write. No arguments: it always returns the full
/// `FetchConfigReply` (scope `"all"`). The nest-side **alias** policy
/// (`put_alias_policy`) is *not* in `FetchConfigReply` — it has its own admin
/// read twin (`get_alias_policy`), mirroring the write-path split. See
/// `docs/goal/behavior/mail-policy-config.md` § UX shell + § Implementation
/// status today.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct GetMailConfigRequest {
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4). This kind is APP-callable,
    /// so it is client↔nest wire and rule 4's in-image `strict` opt-out
    /// does not reach it: within a major, an older peer must tolerate a
    /// newer one's added field in BOTH directions
    /// (version-compatibility.md § I2).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SpamPolicyThresholds {
    /// Deliver to the recipient's spam folder when the **combined score**
    /// (`max(rspamd_scaled, weighted_bayesian)`, 0–15 points) rises to this
    /// value (`AcceptToSpamFolder` disposition). **`0` = disabled** (no
    /// auto-Junk). When both tiers are non-zero,
    /// `max_score_before_spam_folder < max_score_before_reject` must hold —
    /// the bridge constructs a `fauna_mail::SpamPolicy` directly from these
    /// two fields and the Rust scorer's debug_assert in
    /// `libs/fauna-mail/src/spam/mod.rs::decide_spam_disposition` panics on a
    /// violation. There is no hold tier between them. Catalog row in
    /// `docs/goal/behavior/mail-policy-config.md` § Inbound perimeter.
    /// Default `5` (auto-Junk on) per the permissive auto-Junk policy.
    pub max_score_before_spam_folder: u32,
    /// Reject at SMTP DATA with 554 5.7.1 when the combined score rises to
    /// this value (`Reject` disposition; never reaches `ingest_inbound_mail`).
    /// **`0` = disabled (the default)** — the permissive policy never
    /// 550-rejects on the content score; an admin opts in by setting a
    /// non-zero value. (Perimeter hard-gates — auth, DNSBL reject-class,
    /// ClamAV malware — reject independently of this tier.)
    pub max_score_before_reject: u32,
    /// DNS blocklists the MTA queries during RCPT/DATA.
    pub dnsbl_servers: Vec<String>,
    /// Reject when sender IP has no rDNS entry.
    pub reject_no_rdns: bool,
    /// Greylist first-seen senders.
    pub greylist_enabled: bool,
    /// Hold-down period before re-attempted delivery is accepted.
    pub greylist_delay_secs: u32,
    /// Per-peer-IP SMTP connection ceiling per 60-second window. The
    /// MTA bridge's token-bucket gate rejects new connections from any
    /// IP that has already opened this many sessions in the trailing
    /// minute with `421 4.7.0`. Catalog row in
    /// `docs/goal/behavior/mail-policy-config.md` § Inbound hardening.
    pub max_conn_per_min: u32,
    /// FCrDNS mode: `"off"` skips the check entirely; `"score_signal"`
    /// runs the check and forwards the verdict to the spam scorer
    /// without rejecting; `"enforce"` rejects connections that fail
    /// FCrDNS with `550 5.7.25` once `reject_fcrdns_fail` is also set.
    /// Resolver errors always fail open. String-typed (not enum) for
    /// the same forward-compat reason as `ImapPolicy::delete_nonempty`.
    pub fcrdns_mode: String,
    /// HELO/EHLO identity check: require the HELO argument to A-resolve
    /// to a set including the peer IP. `false` keeps only the syntactic
    /// HELO check (FQDN-shaped, no control chars). Production default
    /// is `true`; the loopback exemption fires inside the check itself
    /// so local test harnesses can still use `localhost`.
    pub helo_identity_required: bool,
    /// Alone-reject for FCrDNS failure. Off by default; admins enable
    /// when they want a FCrDNS failure to reject the connection
    /// outright instead of being merely scored. Only meaningful when
    /// `fcrdns_mode == "enforce"`.
    pub reject_fcrdns_fail: bool,
    /// Maximum on-the-wire size of an inbound message in bytes,
    /// enforced as a pre-parser guard in the MTA bridge's
    /// `Session.Data` hook: bodies exceeding the cap are rejected
    /// with `552 5.3.4` before `parse_rfc5322` is called, so a 1 GiB
    /// header bomb doesn't tie up a goroutine on the UniFFI parse.
    /// Default is 50 MiB (50_000_000 bytes), matching the spec's §
    /// Inbound mail flow size-cap floor. Catalog row in
    /// `docs/goal/behavior/mail-policy-config.md` § Inbound hardening.
    pub max_message_bytes: u32,
    /// The **per-user Bayesian** weight in the combined-score formula, carried
    /// as milli to avoid a float wire field (`700` = 0.7). A Bayesian score of
    /// 15 (the max) contributes at most `15 * 0.7 = 10.5` to the combined
    /// score — a damper so a single-user-trained model can't dominate the
    /// deployment-wide rspamd signal. Consumed by the **MDA** scorer
    /// (`mailfauna.SpamPolicyFromSnapshot` → `WeightedBayesianMilliForModel`)
    /// and the Fauna-app / WASM scorer at the search-equivalent position —
    /// **not** the MTA perimeter (which hard-codes the per-user term to 0).
    /// Catalog row `mail.spam.bayesian_weight` in
    /// `docs/goal/behavior/mail-policy-config.md` § Spam (per-user training);
    /// formula in `docs/goal/behavior/mail-spam.md` § Combined-score formula.
    /// `#[serde(default)]` carries the 700-default when the key is absent.
    #[serde(default = "default_bayesian_weight_milli")]
    pub bayesian_weight_milli: u32,
    /// Cold-start floor: the per-user term is 0 until the model has at least
    /// this many training samples (`50`). Below it the confidence factor
    /// clamps to 0, so an under-trained model contributes nothing (the
    /// over-fit guard). Catalog row `mail.spam.bayesian_min_samples`.
    #[serde(default = "default_bayesian_min_samples")]
    pub bayesian_min_samples: u32,
    /// Full-confidence sample count: the linear confidence ramp reaches 1.0
    /// at this many samples (`200`). Between `bayesian_min_samples` and this
    /// value the per-user term ramps in linearly (no step discontinuity).
    /// Also the **fade horizon** of the deployment-baseline fold
    /// (`SpamModel::fold_baseline_faded` — the baseline fades to nothing
    /// exactly as the own model reaches full confidence). Catalog row
    /// `mail.spam.bayesian_full_confidence_samples`.
    #[serde(default = "default_bayesian_full_confidence_samples")]
    pub bayesian_full_confidence_samples: u32,
    /// Per-message training-audit retention in days (`30`). nest GC's
    /// `spam_training_history` rows older than this in its daily sweep; the
    /// learned n-gram weights persist (only the per-event undo trail ages
    /// out). **nest-consumed only** (the daily GC) — the bridge ignores it.
    /// Catalog row `mail.spam.training_history_retention_days`;
    /// `docs/goal/behavior/mail-spam.md` § Training-sample retention.
    #[serde(default = "default_training_history_retention_days")]
    pub training_history_retention_days: u32,
    /// Recipient-whitelist penalty in **points** (`0` = off, the default).
    /// Added to the combined milli-score (`* 1000`) for any recipient whose
    /// delivery carries the `X-Fauna-Address-Catchall` stamp — i.e. mail to an
    /// address not on the user's exact-alias whitelist (never-registered or
    /// dropped → fell through to the catch-all). **Applied at the Go MTA
    /// per-recipient delivery loop** (unlike the `bayesian_*` knobs, which the
    /// MTA ignores) via `fauna_mail::spam::apply_unlisted_recipient_penalty_milli`.
    /// Catalog row `mail.spam.unlisted_recipient_penalty`;
    /// `docs/goal/behavior/mail-spam.md` § Unlisted-recipient penalty.
    /// `#[serde(default)]` carries the 0-default when the key is absent.
    #[serde(default = "default_unlisted_recipient_penalty")]
    pub unlisted_recipient_penalty: u32,
    /// Standing publish of the deployment spam baseline (**default `false`**).
    /// On: the nest republishes the baseline by itself every
    /// `BASELINE_REPUBLISH_INTERVAL` (24 h), each run bound by the contributor
    /// floor and the delta floor exactly like the admin's "publish now". Off:
    /// no runs, and turning it off withdraws the served baseline. nest-consumed
    /// only — the bridge never reads it. Catalog row
    /// `mail.spam.baseline_standing_publish`; `docs/goal/behavior/mail-spam.md`
    /// § Cold start Path 2 → *Standing publish*. `#[serde(default)]` carries
    /// the off-default when the key is absent.
    #[serde(default)]
    pub baseline_standing_publish: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4). This kind is APP-callable,
    /// so it is client↔nest wire and rule 4's in-image `strict` opt-out
    /// does not reach it: within a major, an older peer must tolerate a
    /// newer one's added field in BOTH directions
    /// (version-compatibility.md § I2).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

fn default_bayesian_weight_milli() -> u32 {
    700
}

fn default_bayesian_min_samples() -> u32 {
    50
}

fn default_bayesian_full_confidence_samples() -> u32 {
    200
}

fn default_training_history_retention_days() -> u32 {
    30
}

fn default_unlisted_recipient_penalty() -> u32 {
    0
}

/// Catalog defaults from `docs/goal/behavior/mail-policy-config.md`
/// § Inbound hardening — single source of truth for the static baseline.
/// Admin-tuned overrides layer atop in a later phase.
impl Default for SpamPolicyThresholds {
    fn default() -> Self {
        Self {
            max_score_before_spam_folder: 5,
            max_score_before_reject: 0,
            dnsbl_servers: vec!["zen.spamhaus.org".into()],
            reject_no_rdns: false,
            greylist_enabled: true,
            greylist_delay_secs: 60,
            max_conn_per_min: 10,
            fcrdns_mode: "score_signal".into(),
            helo_identity_required: true,
            reject_fcrdns_fail: false,
            max_message_bytes: 50_000_000,
            bayesian_weight_milli: default_bayesian_weight_milli(),
            bayesian_min_samples: default_bayesian_min_samples(),
            bayesian_full_confidence_samples: default_bayesian_full_confidence_samples(),
            training_history_retention_days: default_training_history_retention_days(),
            unlisted_recipient_penalty: default_unlisted_recipient_penalty(),
            baseline_standing_publish: false,
            extra: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AuthPolicy {
    pub enforce_dmarc: bool,
    pub enforce_dmarc_quarantine: bool,
    pub enforce_spf_hardfail: bool,
    /// DKIM-fail messages are rejected at DATA when no DMARC `p=` decision
    /// applies (DMARC `none` / `Fail{policy=None}` / error verdicts). Off by
    /// default: many legitimate hobbyist senders ship unsigned mail and a
    /// strict gate breaks them. Admins flip this on once their sender
    /// universe is known to sign. See `docs/goal/behavior/smtp-server.md`
    /// § DKIM verdict pipeline + § Error / tempfail strategy.
    pub enforce_dkim: bool,
    /// Compute verdicts but never reject; useful during onboarding.
    pub log_only: bool,
    /// Per-(credential_id, source_IP) submission AUTH-failure ceiling per
    /// 1-minute fixed window. The 31st (default) attempt to AUTH against
    /// the same credential from the same source IP within the window
    /// returns SMTP `421 4.7.0` *before* the AEAD-unwrap step — denying
    /// the attacker an AEAD-timing oracle. Independent of nest-side
    /// rate-limits on `fetch_wrapped_submission_token` (this gate fires
    /// in the bridge process before any nest call). See
    /// `docs/goal/behavior/smtp-server.md` § Connection-time limits +
    /// `docs/goal/behavior/mail-policy-config.md` § Submission policy.
    ///
    /// `#[serde(default)]` carries the 30-default when the key is absent; deny-unknown-fields
    /// still rejects new fields, but a *missing* field falls back to the
    /// Default impl below.
    #[serde(default = "default_max_auth_failures_per_minute")]
    pub max_auth_failures_per_minute: u32,
    /// Per-source-IP **concurrent**-connection ceiling on the authenticated
    /// submission (465/587), IMAP (993/143) and CalDAV (443) listeners. The
    /// Go bridge wraps each of those listeners in a per-IP limiter
    /// (`internal/connlimit`, the Go analogue of the Rust
    /// `fauna_conn_limit::PerIpConnLimit` the nest TLS loop + SNI router use):
    /// once a single source IP holds this many simultaneous connections on a
    /// surface, further connections from it are **shed** (closed at accept)
    /// until one frees. **`0` = disabled.** Loopback is exempt (the in-process
    /// router/bridge dial loopback). Default 256 — generous for a real client
    /// (several devices × persistent IMAP IDLE) or a household behind one NAT
    /// IP, while bounding one source to a fraction of the 4096 global
    /// per-listener cap.
    ///
    /// This is **orthogonal** to the per-IP *rate* cap
    /// (`SpamPolicyThresholds::max_conn_per_min`, port-25-only) and to the
    /// per-credential AUTH-failure lockout (`max_auth_failures_per_minute`): a
    /// rate cap throttles new connections per minute (wrong for long-lived
    /// authenticated sessions), the lockout caps failed AUTH attempts, and
    /// this caps simultaneity — the authenticated surfaces' per-IP fairness
    /// backstop against one source monopolizing the global pool. Keyed on the
    /// real client IP the PROXY-v2 fix restores (`caldav-server.md`
    /// § Network exposure). Catalog row
    /// `mail.auth.per_ip_max_concurrent_conn` in
    /// `docs/goal/behavior/mail-policy-config.md`; behavior in
    /// `smtp-server.md` / `imap-server.md` / `caldav-server.md`
    /// § Connection-time limits. `#[serde(default)]` carries the 256-default
    /// when the key is absent.
    #[serde(default = "default_max_conn_per_ip")]
    pub max_conn_per_ip: u32,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4). This kind is APP-callable,
    /// so it is client↔nest wire and rule 4's in-image `strict` opt-out
    /// does not reach it: within a major, an older peer must tolerate a
    /// newer one's added field in BOTH directions
    /// (version-compatibility.md § I2).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

fn default_max_auth_failures_per_minute() -> u32 {
    30
}

fn default_max_conn_per_ip() -> u32 {
    256
}

impl Default for AuthPolicy {
    fn default() -> Self {
        Self {
            enforce_dmarc: true,
            enforce_dmarc_quarantine: true,
            enforce_spf_hardfail: true,
            enforce_dkim: false,
            log_only: false,
            max_auth_failures_per_minute: default_max_auth_failures_per_minute(),
            max_conn_per_ip: default_max_conn_per_ip(),
            extra: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SubmissionPolicyThresholds {
    pub max_per_day: u32,
    pub max_recipients_per_message: u32,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4). This kind is APP-callable,
    /// so it is client↔nest wire and rule 4's in-image `strict` opt-out
    /// does not reach it: within a major, an older peer must tolerate a
    /// newer one's added field in BOTH directions
    /// (version-compatibility.md § I2).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

impl Default for SubmissionPolicyThresholds {
    fn default() -> Self {
        Self {
            max_per_day: 1000,
            max_recipients_per_message: 100,
            extra: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImapPolicy {
    /// IDLE inactivity timeout the MDA enforces against MUA sessions.
    /// Catalog: `mail.imap.idle_timeout_seconds` (`mail-policy-config.md`
    /// § IMAP server policy). Default 1740 s (29 min), upper-bounded by
    /// RFC 9051 §7.4.1's 30-minute recommendation.
    pub idle_timeout_secs: u32,
    /// QRESYNC tombstone retention window. EXPUNGE rows in
    /// `bridge_imap_expunged` live for this many days; clients
    /// reconnecting past the window fall back to a UIDVALIDITY-driven
    /// resync (per `imap-server.md` § Tombstone retention). Catalog
    /// `mail.imap.tombstone_retention_days`. Default 30, floor 7 —
    /// the floor is enforced by `bins/fauna-nest`, not the protocol.
    pub tombstone_retention_days: u32,
    /// DELETE policy for non-empty mailboxes: `"forbidden"` (default,
    /// matches RFC 9051 §6.3.5's safer-of-two-stances choice) or
    /// `"allowed"` (admin opt-in). Catalog
    /// `mail.imap.delete_nonempty`. String-typed (not enum) to keep
    /// forward-compat — a future `"empty-trash-first"` value can land
    /// without breaking the wire.
    pub delete_nonempty: String,
    /// MDA in-memory BodyStructure / Envelope derivation-cache size,
    /// in entries. The cache lives on the MDA process; key is
    /// `(actor_id, mailbox, uid_validity, uid)`. Catalog
    /// `mail.imap.bodystructure_cache_max`. Default 4096; admins
    /// raise it on busy deployments via `mail-policy-config.md` Tier 2.
    pub bodystructure_cache_max: u32,
    /// QUOTA storage-resource ceiling (RFC 9208 RES-STORAGE), in
    /// bytes, applied per actor. The bridge translates to KiB on the
    /// IMAP wire (RFC 9208 §3.2). Catalog
    /// `mail.imap.storage_bytes_default`. Default `1 << 30` (1 GiB)
    /// per `imap-server.md` § QUOTA. Per-actor tier override is
    /// future scope once the user-account-tier surface lands
    /// (Phase F+); until then every actor sees this default.
    pub storage_bytes_default: u64,
    /// QUOTA message-count-resource ceiling (RFC 9208 RES-MESSAGE),
    /// in messages, applied per actor. Catalog
    /// `mail.imap.message_count_default`. Default 50_000 per
    /// `imap-server.md` § QUOTA.
    pub message_count_default: u32,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4). This kind is APP-callable,
    /// so it is client↔nest wire and rule 4's in-image `strict` opt-out
    /// does not reach it: within a major, an older peer must tolerate a
    /// newer one's added field in BOTH directions
    /// (version-compatibility.md § I2).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

impl Default for ImapPolicy {
    fn default() -> Self {
        Self {
            idle_timeout_secs: 1740,
            tombstone_retention_days: 30,
            delete_nonempty: "forbidden".into(),
            bodystructure_cache_max: 4096,
            storage_bytes_default: 1 << 30,
            message_count_default: 50_000,
            extra: BTreeMap::new(),
        }
    }
}

/// Outbound delivery policy per
/// `docs/goal/behavior/smtp-server.md` § Outbound delivery +
/// `docs/goal/behavior/mail-policy-config.md` § Outbound delivery.
/// The bridge reads this once at `fauna.bridges.fetch_config`, applies
/// to its `OutboundBridgeConfig`, and refreshes on
/// `fauna.bridges.config_changed` push (no CLI / env-var paths).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OutboundPolicy {
    /// Delays before each successive attempt, in seconds. Spec
    /// default: [0, 300, 900, 3600, 14400, 43200, 86400, 86400, 86400,
    /// 86400] — 10 entries for attempts 1..=10.
    pub retry_schedule_seconds: Vec<u64>,
    /// Total retry budget; crossing it promotes to permfail.
    pub permanent_failure_timeout_hours: u32,
    /// 4 h delay-warning per spec.
    pub delay_warning_at_hours: u32,
    /// NDR rate-limit window in days. Spec default: 7.
    pub ndr_rate_limit_days: u32,
    pub suppress_ndr_spf_hardfail: bool,
    pub suppress_ndr_dmarc_reject: bool,
    /// Postmaster CC — project policy is `false` (never CC). Visible
    /// in the admin UI but disabled per `mail-policy-config.md`.
    pub postmaster_cc_bounces: bool,
    /// TLSRPT outbound reports enabled — defaults true (cooperative
    /// behaviour per RFC 8460).
    pub tlsrpt_send_reports: bool,
    /// IPv6 outbound — autodetected from interface, admin overridable.
    pub ipv6_enabled: bool,
    /// Enhanced-status codes that the admin wants the classifier to
    /// treat as transient even when wire is 5xx.
    pub treat_5xx_as_transient: Vec<String>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4). This kind is APP-callable,
    /// so it is client↔nest wire and rule 4's in-image `strict` opt-out
    /// does not reach it: within a major, an older peer must tolerate a
    /// newer one's added field in BOTH directions
    /// (version-compatibility.md § I2).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

impl Default for OutboundPolicy {
    fn default() -> Self {
        Self {
            retry_schedule_seconds: vec![
                0, 300, 900, 3600, 14400, 43200, 86400, 86400, 86400, 86400,
            ],
            permanent_failure_timeout_hours: 120,
            delay_warning_at_hours: 4,
            ndr_rate_limit_days: 7,
            suppress_ndr_spf_hardfail: true,
            suppress_ndr_dmarc_reject: true,
            postmaster_cc_bounces: false,
            tlsrpt_send_reports: true,
            ipv6_enabled: true,
            treat_5xx_as_transient: Vec::new(),
            extra: BTreeMap::new(),
        }
    }
}

/// Bridge-process lifecycle policy per
/// `docs/goal/behavior/mail-bridge-lifecycle.md` § Shutting down +
/// `docs/goal/behavior/mail-policy-config.md` § Bridge process lifecycle.
/// The `mail.bridge.*` catalog namespace; mirrors the spam/auth/submission/
/// imap/outbound → sub-struct mapping (one struct per `mail.<ns>.*` namespace).
/// The bridge reads this at `fauna.bridges.fetch_config` and refreshes on
/// `fauna.bridges.config_changed` push (no CLI / env-var paths).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BridgePolicy {
    /// Graceful-shutdown drain budget, in seconds. On SIGTERM the bridge
    /// stops accepting new connections, answers `421 4.3.2 Service shutting
    /// down` on any new SMTP MAIL FROM (and `BYE` on any new IMAP
    /// SELECT/EXAMINE), drains in-flight transactions for up to this many
    /// seconds, then force-closes and exits. Catalog
    /// `mail.bridge.shutdown_grace_seconds` (`mail-policy-config.md:57`).
    /// Default 30; admin range 0–300 (0 ⇒ immediate force-close). The
    /// range is *enforced* by the future `put_bridge_policy` admin
    /// write-path (deferred to nest-side mail-admin work); the wire type is
    /// just `u32`.
    pub shutdown_grace_seconds: u32,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4). This kind is APP-callable,
    /// so it is client↔nest wire and rule 4's in-image `strict` opt-out
    /// does not reach it: within a major, an older peer must tolerate a
    /// newer one's added field in BOTH directions
    /// (version-compatibility.md § I2).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

impl Default for BridgePolicy {
    fn default() -> Self {
        Self {
            shutdown_grace_seconds: 30,
            extra: BTreeMap::new(),
        }
    }
}

/// Mass-mailing (mailing-list) policy ceilings per
/// `docs/goal/behavior/mail-mass-mailing.md` § Per-list rate accounting +
/// § The per-send cap. The `mail.outbound.list_*` catalog namespace (Tier-2
/// admin ceilings); mirrors the spam/auth/submission/imap/outbound/bridge →
/// sub-struct mapping. The MTA bridge reads this at
/// `fauna.bridges.fetch_config` (refreshing on `config_changed`) so list-mode
/// submissions enforce the per-send / per-account-per-day / per-deployment-
/// per-day recipient caps and the batch-import ceiling — no CLI / env-var path.
///
/// These are the *admin ceilings*; a per-list `recipients_per_send` override
/// (`mail_lists.recipients_per_send`, user-tier) may only ever *lower* the
/// per-send cap, never raise it above the ceiling here. The per-account
/// per-day *user* value (`mail.account.list_recipients_per_day`, Tier 3,
/// default 20 000) is per-account state, not a deployment-wide projection, so
/// it lives with the account, bounded by
/// `list_recipients_per_account_per_day_ceiling` below.
///
/// There is deliberately **no** unsubscribe-secret pointer here: the 32-byte
/// secret is **nest-held** (the goal doc § Token format puts it "in nest
/// state ... server-managed" — the same nest-held-plaintext model as the SRS
/// secret), and the MTA never needs it (unsubscribe handlers resolve a token
/// by the cached `mail_list_members.one_click_unsubscribe_token` index, not by
/// re-deriving it MTA-side), so nothing about the secret is projected to the
/// bridge. (An earlier sketch's wrapped-blob /
/// `list_unsubscribe_secret_blob_id` framing was a DKIM-model drift, corrected
/// to the SRS model against the authoritative goal doc.)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MassMailingPolicy {
    /// Max recipients in a single list submission — `552 5.3.4` over-cap, no
    /// auto-chunking. Catalog `mail.outbound.list_recipients_per_send`
    /// (`mail-mass-mailing.md` § The per-send cap). Default 5000.
    pub list_recipients_per_send_ceiling: u64,
    /// Per-account-per-day list-recipient ceiling — the upper bound the
    /// per-account user knob (`mail.account.list_recipients_per_day`) may be
    /// set to. Catalog `mail.outbound.list_recipients_per_account_per_day`
    /// (`mail-mass-mailing.md` § The per-day per-account cap). Default 50000.
    pub list_recipients_per_account_per_day_ceiling: u64,
    /// Deployment-wide per-day list-recipient safety valve — over the cap,
    /// every list submission tempfails until the next UTC day. Catalog
    /// `mail.outbound.list_recipients_per_deployment_per_day`
    /// (`mail-mass-mailing.md` § The per-day per-deployment cap). Default 500000.
    pub list_recipients_per_deployment_per_day_ceiling: u64,
    /// Max addresses accepted in one batch-import. Catalog
    /// `mail.outbound.list_max_import_per_batch` (`mail-mass-mailing.md`
    /// § `mail-list-members` page → Batch import). Default 10000.
    pub list_max_import_per_batch: u32,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4). This kind is APP-callable,
    /// so it is client↔nest wire and rule 4's in-image `strict` opt-out
    /// does not reach it: within a major, an older peer must tolerate a
    /// newer one's added field in BOTH directions
    /// (version-compatibility.md § I2).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

impl Default for MassMailingPolicy {
    fn default() -> Self {
        Self {
            list_recipients_per_send_ceiling: 5_000,
            list_recipients_per_account_per_day_ceiling: 50_000,
            list_recipients_per_deployment_per_day_ceiling: 500_000,
            list_max_import_per_batch: 10_000,
            extra: BTreeMap::new(),
        }
    }
}

/// One active local domain's DKIM selector, projected to the MTA bridge in
/// [`FetchConfigReply::dkim_selectors`]. The `selector` is the `s=` tag / the
/// `<selector>._domainkey.<domain>` DNS label the nest signs under for that
/// domain (`mail-multidomain.md` § Signing-key selection at outbound time);
/// the bridge holds no key for it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DomainDkimSelector {
    pub domain: String,
    pub selector: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4). This kind is APP-callable,
    /// so it is client↔nest wire and rule 4's in-image `strict` opt-out
    /// does not reach it: within a major, an older peer must tolerate a
    /// newer one's added field in BOTH directions
    /// (version-compatibility.md § I2).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FetchConfigReply {
    /// Whether the admin has enabled mail on this nest. The bridge
    /// branches on this to decide whether to bind its external
    /// listeners (port 25 for MTA, 143/993 for MDA): `false` means
    /// idle, `true` means start listening. The supervisor (s6 in
    /// Docker, systemd on bare-metal) is what gates whether the
    /// process runs at all; the field additionally lets the bridge
    /// handle a mid-transition snapshot where the admin toggled mail
    /// off but the supervisor hasn't yet torn down the process, and
    /// lets test fixtures deterministically exercise the idle path.
    /// Nest projects the persisted `mail_enabled` toggle in its
    /// **effective** form (Stage-5 default-off): a toggle the admin
    /// has never written reads `false` — enablement is always an
    /// explicit `fauna.bridges.set_mail_enabled(true)`, normally
    /// fired by the launched client's claim-time glue on a public
    /// real-domain claim (`onboarding.md` § 3b). The
    /// "requesting bridge is approved ⇒ enabled" derivation is
    /// retired.
    pub mail_enabled: bool,
    /// Whether the admin has enabled CalDAV on this nest — the calendar
    /// twin of `mail_enabled`. The MDA binds its CalDAV (443→:8444) listener
    /// iff this is `true`, independently of `mail_enabled` (which gates the
    /// IMAP 993/143 listeners) per `caldav-server.md` § Independent
    /// enablement: one MDA bridge serves both protocols from one TLS
    /// termination, each listener gated by its own flag, so a deployment can
    /// run calendar without email or email without calendar. nest projects
    /// this from the `caldav_enabled` toggle, falling back to `mail_enabled`
    /// when the admin has never set it explicitly (the out-of-the-box
    /// default — "enabling email also enables CalDAV"). `#[serde(default)]`
    /// so an absent key decodes with `false`.
    #[serde(default)]
    pub caldav_enabled: bool,
    /// The CalDAV listener port the MDA binds when no operator-hatch pins its
    /// CalDAV listener — i.e. on a bare-IP / desktop / domainless box that
    /// serves CalDAV **directly** at `<host>:<port>` (no SNI router). An
    /// **admin choice** — the client UI is the one user-config
    /// surface — set via `fauna.bridges.set_caldav_port`,
    /// persisted in nest state, defaulting to [`DEFAULT_CALDAV_PORT`] (8443).
    /// A router-fronted *domain* box keeps its loopback-IPC hatch
    /// (`caldav_listen_https = 127.0.0.1:8444`), which wins over this value —
    /// there the public CalDAV port is the router's 443, so this admin port is
    /// moot. `#[serde(default = "default_caldav_port")]` (8443, **not** 0) so an
    /// absent key decodes with a bindable port. Per
    /// `caldav-server.md` § Network exposure.
    #[serde(default = "default_caldav_port")]
    pub caldav_port: u16,
    /// Whether the admin has enabled CardDAV on this nest — the contacts twin
    /// of `caldav_enabled`. The MDA serves its CardDAV path handler
    /// (`/carddav/{user}/…`, beside `/caldav/…` on the **same** DAV listener —
    /// CardDAV rides the existing `caldav_port`, no separate port) iff this is
    /// `true`, independently of `mail_enabled` and `caldav_enabled` per the
    /// CardDAV design proposal (tracked internally): one MDA bridge serves
    /// mail + calendar + contacts from one TLS
    /// termination, each listener/handler gated by its own flag, so a
    /// deployment can run contacts without email or calendar. nest projects
    /// this from the `carddav_enabled` toggle, falling back to `mail_enabled`
    /// when the admin has never set it explicitly (a fresh real-domain
    /// deployment gets a contacts surface out of the box, the same default
    /// posture as CalDAV — CardDAV needs no MX/DKIM, only the HTTPS surface).
    /// `#[serde(default)]` so an absent key decodes with
    /// `false`.
    #[serde(default)]
    pub carddav_enabled: bool,
    /// Whether the admin has enabled WebDAV on this nest — the files twin of
    /// `carddav_enabled`. The MDA serves its WebDAV path handler
    /// (`/webdav/{user}/…`, beside `/caldav/…` + `/carddav/…` on the **same** DAV
    /// listener — WebDAV rides the existing `caldav_port`, no separate port) iff
    /// this is `true`, independently of `mail_enabled` / `caldav_enabled` /
    /// `carddav_enabled` per `docs/goal/behavior/webdav-server.md` § Independent
    /// enablement: one MDA bridge serves mail + calendar + contacts + files from
    /// one TLS termination, each listener/handler gated by its own flag, so a
    /// deployment can run files without email/calendar/contacts. nest projects
    /// this from the `webdav_enabled` toggle, falling back to `mail_enabled` when
    /// the admin has never set it explicitly (a fresh real-domain deployment gets
    /// a files surface out of the box, the same default posture as CalDAV/CardDAV
    /// — WebDAV needs no MX/DKIM, only the HTTPS surface). **Harmless-on:** the
    /// deployment toggle exposes nothing until a set is individually flagged
    /// (`folders.webdav_enabled`). `#[serde(default)]` so an absent
    /// key decodes with `false`.
    #[serde(default)]
    pub webdav_enabled: bool,
    /// Active mail-domain names — the derived projection of the
    /// `mail_domains` table's `domain_name` column where `removed_at
    /// IS NULL`. The MTA bridge accepts RCPT TO for any address whose
    /// domain is in this list and rejects others with `550 5.7.1
    /// Relay denied`; the MDA bridge uses the same list to scope IMAP
    /// namespace lookups. Empty list ⇒ bridge idles (no mail domains
    /// configured). Ordered with `is_primary = true` first then by
    /// `added_at ASC`. See
    /// `docs/goal/behavior/mail-multidomain.md` § The `mail_domains` model
    /// and § Architectural rules → "The `local_domains` list is a derived projection".
    #[serde(default)]
    pub local_domains: Vec<String>,
    /// The `is_primary = true` row's `domain_name`; empty when no
    /// primary exists yet (fresh nest pre-first-domain-claim). Anchors
    /// the MX target, TLSRPT/DMARC processor inboxes, MTA-STS/DKIM/ACME
    /// cert chain per `mail-multidomain.md` § The primary domain. Per-
    /// domain TLS/DKIM is a separate track; until then the bridge's
    /// single-domain TLS provider + DKIM registry + EHLO host all
    /// consume this field.
    #[serde(default)]
    pub primary_domain: String,
    /// Per-domain DKIM selectors — one [`DomainDkimSelector`] per **active**
    /// `mail_domains` row (`domain_name` + the `s=` tag / the
    /// `<selector>._domainkey.<domain>` DNS label). Each row's `dkim_selector`
    /// column, defaulting to `"default"` when NULL (a freshly-claimed domain).
    /// Ordered like `local_domains` (primary first, then `added_at ASC`). The
    /// set of domains the deployment signs for: the MTA bridge reads it for
    /// the From-header ownership check at submission
    /// (`mail-multidomain.md` § From: header ownership) and holds no key —
    /// the nest signs at the outbound hand-out.
    #[serde(default)]
    pub dkim_selectors: Vec<DomainDkimSelector>,
    pub spam: SpamPolicyThresholds,
    pub auth: AuthPolicy,
    pub submission: SubmissionPolicyThresholds,
    pub imap: ImapPolicy,
    pub outbound: OutboundPolicy,
    /// Bridge-process lifecycle policy (`mail.bridge.*`). `#[serde(default)]`
    /// so an absent key decodes with the catalog default
    /// (drain 30 s).
    #[serde(default)]
    pub bridge: BridgePolicy,
    /// Mass-mailing (mailing-list) policy ceilings (`mail.outbound.list_*`)
    /// per `docs/goal/behavior/mail-mass-mailing.md`. `#[serde(default)]` so an
    /// absent key decodes with the catalog defaults
    /// (5000 / 50000 / 500000 recipients, 10000 import batch).
    #[serde(default)]
    pub mass_mailing: MassMailingPolicy,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4). This kind is APP-callable,
    /// so it is client↔nest wire and rule 4's in-image `strict` opt-out
    /// does not reach it: within a major, an older peer must tolerate a
    /// newer one's added field in BOTH directions
    /// (version-compatibility.md § I2).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Default CalDAV listener port when the admin has never set one via
/// `fauna.bridges.set_caldav_port`. The MDA binds this when no operator-hatch
/// pins its CalDAV listener — the bare-IP / desktop / domainless serving case
/// (`caldav-server.md` § Network exposure). A router-fronted domain box's
/// loopback-IPC hatch wins over it. The single source of truth for "8443" on the
/// nest/protocol side; the Go MDA mirrors it.
pub const DEFAULT_CALDAV_PORT: u16 = 8443;

/// serde default for [`FetchConfigReply::caldav_port`] — an absent key
/// must decode with a **bindable** port (8443), never the `u16` zero value.
fn default_caldav_port() -> u16 {
    DEFAULT_CALDAV_PORT
}

impl Default for FetchConfigReply {
    fn default() -> Self {
        Self {
            mail_enabled: true,
            caldav_enabled: true,
            caldav_port: DEFAULT_CALDAV_PORT,
            carddav_enabled: true,
            webdav_enabled: true,
            local_domains: Vec::new(),
            primary_domain: String::new(),
            dkim_selectors: Vec::new(),
            spam: SpamPolicyThresholds::default(),
            auth: AuthPolicy::default(),
            submission: SubmissionPolicyThresholds::default(),
            imap: ImapPolicy::default(),
            outbound: OutboundPolicy::default(),
            bridge: BridgePolicy::default(),
            mass_mailing: MassMailingPolicy::default(),
            extra: BTreeMap::new(),
        }
    }
}

/// Admin override for the full `SpamPolicyThresholds` sub-struct
/// (`docs/goal/behavior/mail-policy-config.md` § Inbound perimeter).
/// One `Option<T>` per `SpamPolicyThresholds` field: `Some(v)` sets the
/// value, `None` ⇒ keep the catalog default from
/// `SpamPolicyThresholds::default()`. The override replaces the full
/// `dnsbl_servers` list when present (not a merge — an admin wanting
/// to extend Spamhaus passes `Some(vec!["zen.spamhaus.org".into(),
/// "extra.example".into()])`). The bridge enforces
/// `max_score_before_spam_folder < max_score_before_reject`
/// (`SpamPolicyThresholds` doc + the
/// `decide_spam_disposition` debug-assert), so `put_spam_policy_handler`
/// rejects an override whose *effective* (override-or-default) thresholds
/// violate that order with `fauna.protocol.malformed`.
///
/// Was `PutMailPolicyRequest` (the 4-field DNS-perimeter slice, finding
/// #4 of an earlier slice); A3 Bucket B renamed it to mirror `FetchConfigReply.
/// spam` and grew it to the full sub-struct (one of the five uniform
/// `put_<substruct>_policy` kinds).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PutSpamPolicyRequest {
    pub max_score_before_spam_folder: Option<u32>,
    pub max_score_before_reject: Option<u32>,
    /// `Some(vec![])` clears DNSBL queries entirely (air-gapped deploy;
    /// CI fixtures binding IPv4 loopback where the bridge's `policy.go`
    /// DNSBL gate would otherwise fire).
    pub dnsbl_servers: Option<Vec<String>>,
    pub reject_no_rdns: Option<bool>,
    pub greylist_enabled: Option<bool>,
    /// `Some(0)` collapses the hold-down to 0 s without disabling the
    /// tracking (admin still gets the audit trail; CI avoids the wait).
    pub greylist_delay_secs: Option<u32>,
    pub max_conn_per_min: Option<u32>,
    /// FCrDNS mode override (`"off"` / `"score_signal"` / `"enforce"`).
    /// `Some("off")` skips the connection-time PTR + forward-A round-trip
    /// entirely — the air-gapped / no-DNS deploy override, and the knob
    /// the e2e MTA fixture sets so the bridge does no resolver lookup in
    /// `NewSession`. `None` ⇒ keep the catalog `"score_signal"`.
    pub fcrdns_mode: Option<String>,
    pub helo_identity_required: Option<bool>,
    pub reject_fcrdns_fail: Option<bool>,
    pub max_message_bytes: Option<u32>,
    /// Per-user Bayesian combined-score weight, milli (default 700 = 0.7).
    pub bayesian_weight_milli: Option<u32>,
    /// Cold-start sample floor below which the per-user term is 0 (default 50).
    pub bayesian_min_samples: Option<u32>,
    /// Sample count at which the per-user confidence ramp reaches 1.0 and the
    /// cold-start baseline fade reaches 0 (default 200). The handler rejects an
    /// effective value `<= bayesian_min_samples` (a degenerate/inverted ramp).
    pub bayesian_full_confidence_samples: Option<u32>,
    /// `spam_training_history` retention in days; nest GC's older rows daily
    /// (default 30). nest-consumed only — the bridge ignores it.
    pub training_history_retention_days: Option<u32>,
    /// Recipient-whitelist penalty in points added to a catch-all recipient's
    /// combined score at the Go MTA loop (default 0 = off; `mail-spam.md`
    /// § Unlisted-recipient penalty).
    pub unlisted_recipient_penalty: Option<u32>,
    /// Standing publish of the deployment spam baseline (default off;
    /// `mail-spam.md` § Cold start Path 2 → *Standing publish*). Required, so
    /// every full PUT states it: `false` over a stored `true` turns it off and
    /// withdraws.
    pub baseline_standing_publish: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4). This kind is APP-callable,
    /// so it is client↔nest wire and rule 4's in-image `strict` opt-out
    /// does not reach it: within a major, an older peer must tolerate a
    /// newer one's added field in BOTH directions
    /// (version-compatibility.md § I2).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Admin override for the `AuthPolicy` sub-struct
/// (`mail-policy-config.md` § Submission policy / DMARC enforcement gates).
/// One `Option<T>` per field; `None` ⇒ `AuthPolicy::default()`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PutAuthPolicyRequest {
    pub enforce_dmarc: Option<bool>,
    pub enforce_dmarc_quarantine: Option<bool>,
    pub enforce_spf_hardfail: Option<bool>,
    pub enforce_dkim: Option<bool>,
    pub log_only: Option<bool>,
    pub max_auth_failures_per_minute: Option<u32>,
    pub max_conn_per_ip: Option<u32>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4). This kind is APP-callable,
    /// so it is client↔nest wire and rule 4's in-image `strict` opt-out
    /// does not reach it: within a major, an older peer must tolerate a
    /// newer one's added field in BOTH directions
    /// (version-compatibility.md § I2).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Admin override for the `SubmissionPolicyThresholds` sub-struct
/// (`mail-policy-config.md` § Submission policy). `None` ⇒
/// `SubmissionPolicyThresholds::default()`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PutSubmissionPolicyRequest {
    pub max_per_day: Option<u32>,
    pub max_recipients_per_message: Option<u32>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4). This kind is APP-callable,
    /// so it is client↔nest wire and rule 4's in-image `strict` opt-out
    /// does not reach it: within a major, an older peer must tolerate a
    /// newer one's added field in BOTH directions
    /// (version-compatibility.md § I2).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Admin override for the `ImapPolicy` sub-struct
/// (`mail-policy-config.md` § IMAP server policy). `None` ⇒
/// `ImapPolicy::default()`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PutImapPolicyRequest {
    pub idle_timeout_secs: Option<u32>,
    pub tombstone_retention_days: Option<u32>,
    /// `"forbidden"` / `"allowed"` (string-typed for forward-compat,
    /// like the wire field).
    pub delete_nonempty: Option<String>,
    pub bodystructure_cache_max: Option<u32>,
    pub storage_bytes_default: Option<u64>,
    pub message_count_default: Option<u32>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4). This kind is APP-callable,
    /// so it is client↔nest wire and rule 4's in-image `strict` opt-out
    /// does not reach it: within a major, an older peer must tolerate a
    /// newer one's added field in BOTH directions
    /// (version-compatibility.md § I2).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Admin override for the `OutboundPolicy` sub-struct
/// (`mail-policy-config.md` § Outbound delivery). `None` ⇒
/// `OutboundPolicy::default()`; list fields full-replace when present.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PutOutboundPolicyRequest {
    pub retry_schedule_seconds: Option<Vec<u64>>,
    pub permanent_failure_timeout_hours: Option<u32>,
    pub delay_warning_at_hours: Option<u32>,
    pub ndr_rate_limit_days: Option<u32>,
    pub suppress_ndr_spf_hardfail: Option<bool>,
    pub suppress_ndr_dmarc_reject: Option<bool>,
    pub postmaster_cc_bounces: Option<bool>,
    pub tlsrpt_send_reports: Option<bool>,
    pub ipv6_enabled: Option<bool>,
    pub treat_5xx_as_transient: Option<Vec<String>>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4). This kind is APP-callable,
    /// so it is client↔nest wire and rule 4's in-image `strict` opt-out
    /// does not reach it: within a major, an older peer must tolerate a
    /// newer one's added field in BOTH directions
    /// (version-compatibility.md § I2).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Admin override for the four nest-side **alias-policy** knobs
/// (`mail-policy-config.md` § Inbound perimeter — `mail.account.
/// exact_aliases_max`, `mail.inbound.{reserved_local_parts,
/// subaddressing_enabled,wildcard_prefix_enabled}`). Unlike the five
/// `put_<substruct>_policy` kinds above, these are **not** projected to
/// the bridge via `FetchConfigReply` — they are consumed nest-side by the
/// alias resolver (`resolve_recipient`) + alias CRUD
/// (`create_account_alias`), so this kind is write-path-only (no
/// projection, no bridge change). `None` ⇒ the documented
/// `fauna_mail::aliases` const default. Shares `PutPolicyReply`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PutAliasPolicyRequest {
    /// Per-account cap on user-added exact aliases (default 20). `Some(0)`
    /// forbids any beyond the signup canonical.
    pub exact_aliases_max: Option<u32>,
    /// Full-replace the reserved local-part list (default the six role names
    /// in `fauna_mail::aliases::DEFAULT_RESERVED_LOCAL_PARTS`: `postmaster`,
    /// `abuse`, `noc`, `security`, `dmarc-report`, `tlsrpt` — `unsubscribe@`
    /// is intentionally not reserved, it routes via a separate mechanism).
    /// `Some(vec![])` clears the reservation (distinct from `None` = keep
    /// default). Also the source the future admin-forwarder validator reads
    /// (tracked separately).
    pub reserved_local_parts: Option<Vec<String>>,
    /// `+suffix` sub-addressing on/off (default `true`).
    pub subaddressing_enabled: Option<bool>,
    /// `bob-*` wildcard-prefix aliases on/off (default `true`).
    pub wildcard_prefix_enabled: Option<bool>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4). This kind is APP-callable,
    /// so it is client↔nest wire and rule 4's in-image `strict` opt-out
    /// does not reach it: within a major, an older peer must tolerate a
    /// newer one's added field in BOTH directions
    /// (version-compatibility.md § I2).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Empty request for `fauna.bridges.get_alias_policy` — the **admin read twin**
/// of `put_alias_policy`. Mirrors [`GetMailConfigRequest`]: the four nest-side
/// alias knobs are **not** projected into [`FetchConfigReply`], so the
/// `admin-mail` form reads them through this dedicated kind (it always returns
/// the full effective [`AliasPolicy`], scope `"all"`). See
/// `docs/goal/behavior/mail-policy-config.md` § Implementation status today.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct GetAliasPolicyRequest {
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4). This kind is APP-callable,
    /// so it is client↔nest wire and rule 4's in-image `strict` opt-out
    /// does not reach it: within a major, an older peer must tolerate a
    /// newer one's added field in BOTH directions
    /// (version-compatibility.md § I2).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The **effective** (override-or-default) nest-side alias policy the
/// `fauna.bridges.get_alias_policy` admin read twin returns — each `None`
/// override resolved to its `fauna_mail::aliases` const default, so every field
/// is a concrete value (the wire mirror of the nest-side `ResolvedAliasPolicy`).
/// The `admin-mail` form hydrates from this before edit, then writes the whole
/// sub-struct back via [`PutAliasPolicyRequest`] (full PUT). This is the read
/// half of the write-path split: the five projected `put_<substruct>_policy`
/// kinds round-trip through `get_mail_config`/`FetchConfigReply`, while the
/// nest-side `put_alias_policy` round-trips through this kind.
///
/// [`Default`] carries the catalog defaults, sourced from `fauna_core::mail_aliases`
/// (this crate has no `fauna-mail` dependency, so it reads the shared owner one
/// layer down rather than duplicating the literals by hand — see the impl).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AliasPolicy {
    /// Per-account cap on user-added exact aliases (catalog default 20).
    pub exact_aliases_max: u32,
    /// Reserved local-part list — role addresses users cannot claim (catalog
    /// default the six names in `fauna_mail::aliases::DEFAULT_RESERVED_LOCAL_PARTS`).
    pub reserved_local_parts: Vec<String>,
    /// `+suffix` sub-addressing enabled (catalog default `true`).
    pub subaddressing_enabled: bool,
    /// `bob-*` wildcard-prefix aliases enabled (catalog default `true`).
    pub wildcard_prefix_enabled: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4). This kind is APP-callable,
    /// so it is client↔nest wire and rule 4's in-image `strict` opt-out
    /// does not reach it: within a major, an older peer must tolerate a
    /// newer one's added field in BOTH directions
    /// (version-compatibility.md § I2).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

impl Default for AliasPolicy {
    fn default() -> Self {
        // ONE definition, in `fauna_core::mail_aliases` — which owns the
        // wire-shape rationale (this used to hand-copy all four literals,
        // since `fauna-protocol` must not depend on `fauna-mail`; see the
        // `AuthVerdicts`/`mail_scan` re-exports above for the same shape).
        use fauna_core::mail_aliases::{
            DEFAULT_RESERVED_LOCAL_PARTS, EXACT_ALIASES_MAX_DEFAULT, SUBADDRESSING_ENABLED_DEFAULT,
            WILDCARD_PREFIX_ENABLED_DEFAULT,
        };
        Self {
            exact_aliases_max: EXACT_ALIASES_MAX_DEFAULT,
            reserved_local_parts: DEFAULT_RESERVED_LOCAL_PARTS
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            subaddressing_enabled: SUBADDRESSING_ENABLED_DEFAULT,
            wildcard_prefix_enabled: WILDCARD_PREFIX_ENABLED_DEFAULT,
            extra: BTreeMap::new(),
        }
    }
}

/// Shared reply for every `fauna.bridges.put_<substruct>_policy` Admin
/// kind (and `put_alias_policy`) — `{ ok: true }` on a successful upsert.
/// Clients discard it (`Result<(), _>`); a validation failure surfaces as
/// the namespaced `RpcError` (`fauna.protocol.malformed`) instead. Was
/// `PutMailPolicyReply`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PutPolicyReply {
    pub ok: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4). This kind is APP-callable,
    /// so it is client↔nest wire and rule 4's in-image `strict` opt-out
    /// does not reach it: within a major, an older peer must tolerate a
    /// newer one's added field in BOTH directions
    /// (version-compatibility.md § I2).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CheckSubmissionQuotaRequest {
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    /// Running count of recipients this submission has accepted so far,
    /// **including** the one this call is for — the per-message recipient
    /// cap's input and nothing else's. It never feeds the daily debit: the
    /// nest charges one recipient per call, so the bridge calls exactly once
    /// per accepted RCPT (`smtp-server.md` § Architectural rules, the
    /// charging rule). Must be > 0; nest rejects 0 as malformed.
    pub recipient_count: u32,
    /// Whether the recipient this call is for was placed in a mailbox on
    /// this deployment at RCPT time (the resolver's `Resolved` outcome). A
    /// local recipient never leaves the deployment and consumes none of the
    /// daily allowance; an outside-domain recipient or an admin external
    /// forwarder is remote and costs one. Additive (2026-09-25): absent on
    /// the wire reads as remote, so such a recipient is charged.
    #[serde(default)]
    pub recipient_is_local: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum CheckSubmissionQuotaReply {
    Allowed,
    OverQuota {
        /// Headroom the caller may retry with. Two distinct rejections
        /// share this reply (`check_submission_quota_handler`,
        /// `bins/fauna-nest`): the **daily** ceiling — remaining in
        /// today's bucket *before* the rejected call was attempted, so a
        /// too-large recipient list can be split and retried with
        /// `recipient_count <= remaining` — and the **per-message** cap
        /// (`mail.submission.max_recipients_per_message`, checked first),
        /// where `remaining` is that cap itself: the largest single
        /// message the admin's policy allows.
        remaining: u32,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PublicMailMetadata {
    /// **The sender's own RFC 5322 `Date:` header**, as epoch seconds — the
    /// MTA bridge parses it out of the message before ingest
    /// (`libs/fauna-mail/src/parser.rs` `msg.date()`). Plaintext per the
    /// encryption-at-rest floor so nest can read it without decrypting.
    ///
    /// **Unauthenticated: never key ordering, filtering, or retention on it.**
    /// Nothing in SMTP or RFC 5322 obliges a sender to fill this in
    /// truthfully, so any surface it drives is a surface the sender steers.
    /// It is the *message's* claimed date — the thing an MUA already shows
    /// from the body — and it is kept for exactly that. The nest's own
    /// arrival clock is a separate, server-assigned fact
    /// (`MailFloorMetadata::received_at`, returned to the ingest handler as
    /// `MailInsertOutcome::received_at`), and that is what
    /// `bridge_imap_messages.internal_date` / IMAP `INTERNALDATE`, the
    /// forensic scan row, and the scoring bus are stamped from
    /// (`imap-server.md` § SEARCH → *INTERNALDATE is the nest's own receipt
    /// time*).
    pub timestamp: i64,
    /// Bridge-reported ciphertext byte count. Stored explicitly even
    /// though it equals `encrypted_body.len()` so future bucket-rounding
    /// for privacy can swap out the value without touching the body.
    pub ciphertext_size: u32,
    /// Envelope FROM domain (e.g. "example.com"). Plaintext floor for
    /// SPF audit.
    pub sender_domain: String,
}

// ── AuthVerdicts (nested-enum wire shape) ──────────────────────────────
//
// ONE definition, in `fauna_core::mail_auth` — which owns the wire-shape
// contract and the reason it lives there. These re-exports keep every
// `fauna_protocol::bridge_routing::DkimVerdict` path working; the CBOR
// round-trip tests below stay as cheap regression guards, though what
// they pinned (two hand-mirrored copies agreeing) is now a property of
// the type system.
//
// Until 2026-08-17 this file carried a second, hand-mirrored copy and a
// `TODO: unify these definitions` proposing that `fauna-protocol` become
// the canonical home by inverting `fauna-mail::kind_registry`. Both
// halves of that plan were refuted when it was executed: no inversion was needed, because the `fauna-mail →
// fauna-protocol` edge already pointed the way unification required and
// nothing circular ever blocked it; and protocol was the wrong home
// precisely because it carries no UniFFI surface by design, while these
// types must reach the Go MTA that produces them. The duplication's real
// causes were narrower — `auth` not pulling `fauna-protocol`, and the
// missing UniFFI face. That is the whole ruling; it is written out here
// rather than pointed at, so it cannot rot away from the code it explains.
pub use fauna_core::mail_auth::{
    ArcVerdict, AuthVerdicts, DkimVerdict, DmarcPolicy, DmarcVerdict, SpfVerdict,
};

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SpamDisposition {
    #[default]
    Accept,
    /// The combined score reached the Junk tier.
    AcceptToSpamFolder,
    /// Filed to Junk by a rule, whatever the score — the sender's DMARC
    /// `p=quarantine`, a ClamAV hit under the `junk` action, or an
    /// unlisted-recipient penalty that reached the reject tier
    /// (`fauna_mail::spam::SpamDisposition::PolicyJunk`).
    PolicyJunk,
}

// ── Perimeter scan results (ClamAV verdict, rspamd score) ─────────────
//
// ONE definition, in `fauna_core::mail_scan` — which owns the wire-shape
// contract and the reason it lives there. These re-exports keep every
// `fauna_protocol::bridge_routing::ClamavVerdict` path working.
//
// Until 2026-08-18 this file carried a second, hand-mirrored copy, under
// the same stated reason the auth verdicts carried — "`fauna-protocol` is
// the L3 wire crate and cannot depend on `fauna-mail` (circular)". Row 163
// refuted that premise by executing it one family over, and the same fix
// was applied here: nothing circular was ever in the way, and
// the answer is not a move between these two crates but a third home both
// already depend on — `fauna-core`, which unlike protocol carries a UniFFI
// surface, and these types must reach the Go MTA that produces them.
//
// ⚠ Unlike the auth family, the two copies here did NOT agree, so this
// unification had a wire question to settle rather than assume. Protocol's
// copy carried the adjacent tagging and `Default`; fauna-mail's carried no
// serde container attributes at all, plus the UniFFI derives protocol
// lacked. The divergence was **latent, never live**: fauna-mail's serde
// impl is dead on the wire — the Go MTA hand-builds the adjacently-tagged
// shape rather than serializing the UniFFI type, and nothing in the Rust
// tree serialized that copy either. So the unified type is a strict union
// (protocol's serde attributes + fauna-mail's UniFFI derives + `Default`)
// and no byte moves. The full measurement lives on the definition.
pub use fauna_core::mail_scan::{ClamavVerdict, RspamdRuleContribution, RspamdScore};

/// A sealed mail body that crossed on the **bulk-byte plane** instead of inline
/// in the RPC frame (`smtp-server.md` § Message size limits; the rule:
/// `architecture/transport.md` § Max frame).
///
/// The 2 MiB WS-RPC cap is permanent for every caller class, so a sealed body
/// over [`fauna_mail::transport_limits::INLINE_MAIL_REQUEST_BUDGET_BYTES`] cannot
/// ride the frame. The producer stages it as content-addressed chunks
/// (`POST /api/v1/chunks`, one request each) and sends this reference in place of
/// the bytes; the consumer resolves it back to the identical byte string. The
/// chunk split and the store key are shared Rust — `fauna_mail::body_ref` — so
/// the Go MTA that stages, the nest that rejoins, and the Go MDA that re-fetches
/// cannot disagree about where a boundary falls.
///
/// Deliberately **not** a `ChunkManifest`: a mail reference is just the ordered
/// hash list, which keeps this path off the manifest type and its fail-closed
/// decode (owned by the folders track).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MailBodyRef {
    /// The staged chunks' blake3 digests, **in body order**. Each is the key the
    /// byte plane stores that chunk under
    /// (`chunk_routes.rs::resolve_verified_chunk_hash`).
    pub chunk_hashes: Vec<ByteBuf>,
    /// Length of the rejoined sealed body. The chunk *contents* are
    /// self-verifying (content-addressed), but the chunk *list* is not — a
    /// producer that named the wrong chunks, reordered them, or dropped one would
    /// otherwise hand nest a body that stores cleanly while being the wrong
    /// message. The consumer pins the join against this and fails closed.
    pub total_bytes: u64,
}

/// A **plaintext-derived** mail body that crossed on the bulk-byte plane under a
/// one-shot AEAD envelope (`smtp-server.md` § Message size limits, *the
/// staged-envelope rule*; ratified 2026-07-18).
///
/// The two legs that stage plaintext-derived bytes — client import and the
/// outbound queue pair — cannot use [`MailBodyRef`]: the open chunk-download
/// route is safe *only because everything in the store is ciphertext*
/// (`webdav-server.md` § Bulk-byte plane), and staging raw plaintext would both
/// disclose it to anyone holding the hash and open a `blake3(plaintext)`
/// correlation channel. So the producer AEAD-seals the whole plaintext under a
/// **fresh one-shot key** (`fauna_mail::staged_envelope`), splits the
/// *ciphertext* with the same `fauna_mail::body_ref` chunk rule, uploads the
/// chunks, and sends this reference **with the key**. The key rides inside the
/// already-confidential authenticated WS-RPC — the very channel that otherwise
/// carries the plaintext inline — so no party learns anything it could not
/// already read, and the chunk store never sees plaintext or a stable content
/// hash (fresh key ⇒ unique ciphertext ⇒ no cross-staging correlation).
///
/// A deliberately **distinct type** from [`MailBodyRef`], so a sealed-bytes
/// reference and an ephemeral-key ciphertext reference can never be confused at
/// the type level. The consumer joins fail-closed on [`Self::total_bytes`], then
/// opens the AEAD (whose tag authenticates the entire join).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct StagedBodyRef {
    /// The staged **ciphertext** chunks' blake3 digests, in body order — each the
    /// key the byte plane stores that chunk under, exactly as [`MailBodyRef`].
    pub chunk_hashes: Vec<ByteBuf>,
    /// Length of the rejoined **sealed** (nonce-prefixed ciphertext) bytes, *not*
    /// the recovered plaintext length. The consumer pins the join against this
    /// and fails closed before it ever opens the AEAD.
    pub total_bytes: u64,
    /// The one-shot AEAD key. A [`fauna_core::secret::SecretByteBuf`]: zeroized
    /// on drop and redacted in `Debug`, and serialized as a **byte string**
    /// (major type 2, like the `ByteBuf` chunk hashes beside it) so the Go
    /// bridge's `[]byte key` field matches on the wire — *not* [`SecretBytes`],
    /// which would encode as a CBOR array of integers.
    pub key: fauna_core::secret::SecretByteBuf,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct IngestInboundMailRequest {
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    /// The sealed body, inline. Empty **iff** [`Self::body_ref`] is set — a body
    /// over the inline budget crosses on the byte plane instead (the nest rejects
    /// an ingest that is empty on both, so a version-skewed producer can never
    /// store an empty message).
    #[serde(with = "serde_bytes")]
    pub encrypted_body: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub encrypted_index_hint: Vec<u8>,
    pub public_metadata: PublicMailMetadata,
    pub verdicts: AuthVerdicts,
    pub spam_score: u32,
    pub spam_disposition: SpamDisposition,
    /// Perimeter ClamAV verdict. `Infected` only on junk/tag actions —
    /// `reject` never ingests. `NotScanned` on every door that never invokes
    /// the scan gate (the submission twin and the sender's own Sent copy,
    /// `fauna_recipient.go`), which is also what an omitted field decodes to:
    /// a request that says nothing about ClamAV did not scan, and the nest
    /// must never turn silence into a `'clean'` record.
    #[serde(default)]
    pub clamav_verdict: ClamavVerdict,
    /// Perimeter rspamd score; `None` when rspamd is disabled (or pre-T3).
    /// Stored + header-stamped; does NOT feed `spam_disposition` in T1.4 (the
    /// `max(rspamd, weighted_bayesian)` combined-score feed is T3.1).
    #[serde(default)]
    pub rspamd_score: Option<RspamdScore>,
    /// `true` when this delivery resolved via a **role-address** route
    /// (`postmaster@`/`abuse@`/`security@`/`tlsrpt@`/`dmarc-report@` …) — the
    /// Go MTA echoes the bit it learned from
    /// [`ValidateRecipientReply::Resolved::is_role_address`]. The inbound
    /// enforcement point skips the per-mailbox quota pre-check when set, so an
    /// over-quota admin mailbox still receives postmaster mail (`smtp-server.md`
    /// § Architectural rules; `imap-server.md` § Quota enforcement points).
    /// Always `false` for `submit_inbound_mail` (own-submission is never a role
    /// address — and is exempt from the quota pre-check anyway). `#[serde(default)]`
    /// so an absent key decodes as `false`.
    #[serde(default)]
    pub is_role_address: bool,
    /// Filter-rule placement override (T3.3, `smtp-server.md` § Email filter
    /// rules). When `Some`, the recipient's matched filter resolved to a target
    /// mailbox — `FilterAction::FileInto { mailbox }` (the named folder) or
    /// `FilterAction::Allow` (whitelist → `"INBOX"`) — and the message is placed
    /// there, **overriding the `spam_disposition`→folder map** below. The
    /// `spam_disposition` field is left untouched (truthful record of what the
    /// spam policy decided); only placement changes. `None` → place by
    /// `spam_disposition` as before. The mailbox is auto-created if it does not
    /// exist (a custom user folder), the same way the six standard mailboxes are
    /// seeded. The eval is the MTA perimeter's decision (`fauna_mail::filter`);
    /// nest never evaluates filters (it cannot — in encrypted mode it never sees
    /// the subject/headers). `#[serde(default)]` so an absent key (no filter
    /// match) decodes as `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_mailbox: Option<String>,
    /// Filter-rule label flags (T3.3) — IMAP keywords to set on the placed
    /// message, from `FilterAction::AddLabel { label }`. Merged onto the base
    /// flags (`\Seen` for own-submission; none for inbound). Empty for an
    /// unmatched / non-label action. `#[serde(default)]` so an absent key decodes
    /// as an empty vec.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_flags: Vec<String>,
    /// The uniform scoring-metadata bus — one row per scored factor
    /// (`content-scoring.md` § The scoring-metadata bus; frame Q1). The
    /// perimeter mints it: the Go MTA calls the shared
    /// [`fauna_core::scoring::perimeter_mail_score_rows`] over UniFFI, once
    /// per recipient, and sends the rows here beside the per-kind fields
    /// above — which stay, as the *detail record* the rows summarize
    /// (`verdicts` → `smtp_verdicts`, `clamav_verdict` + `rspamd_score` →
    /// `message_scan_results`, `spam_score`/`spam_disposition` → the floor and
    /// placement), never as derivation inputs. The nest persists the array
    /// as-is and derives nothing from the per-kind fields (the contract
    /// phase; the expand-phase nest-side derivation is gone). Empty = the
    /// perimeter scored nothing — the own-submission Sent copy — and the nest
    /// records no rows. `#[serde(default)]` so an absent key decodes as
    /// empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scores: Vec<fauna_core::scoring::ScoreEntry>,
    /// Canonical 32-byte report-hash — the cross-user, cross-nest
    /// content-equality key for distributed report sharing, computed once per
    /// message at the MTA perimeter pre-seal by the shared-Rust
    /// `fauna_mail::report_hash` (`report-sharing.md` § Content identity).
    /// Empty = absent: a non-ingest record carries no hash and those messages
    /// simply cannot aggregate (graceful). Non-empty must be exactly 32 bytes
    /// (the handler rejects anything else as malformed). `#[serde(default)]`
    /// so an absent key decodes as empty.
    #[serde(with = "serde_bytes", default, skip_serializing_if = "Vec::is_empty")]
    pub report_hash: Vec<u8>,
    /// The full MAIL FROM address, for the guardian mail gate's known-sender
    /// check when the recipient is a supervised account whose guardian set
    /// `unknown_sender_mail=hold` (`family-safety.md` § The mail gate). The nest
    /// **recomputes** the hold verdict from its own stored policy — the bridge
    /// carries the envelope, never the decision. Envelope FROM is plaintext
    /// floor in both storage modes (`encryption-at-rest.md` § Plaintext floor);
    /// `public_metadata.sender_domain` is the coarser twin kept for the
    /// stored-footer audit shape. Empty = the SMTP null reverse-path `<>`
    /// (bounces/DSNs): never gated, so mail is never lost.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sender_address: String,
    /// Canonical dedup key over the *plaintext* message
    /// (`mailbox-migration.md` § Key format), computed at the MTA perimeter
    /// pre-seal by the shared-Rust `fauna_mail::dedup_key::mail_dedup_keys` —
    /// the same sanctioned plaintext position as `report_hash` above, and for
    /// the same reason: the nest receives only ciphertext. (The nest's own
    /// in-domain delivery, which holds the plaintext it seals, mints it the
    /// same way.)
    ///
    /// Nest **records** it in `actor_message_dedup` and never acts on a hit.
    /// Suppressing a delivery whose key already exists would silently drop real
    /// mail (a legitimate resend, or a second copy the user is a `Cc:` on); only
    /// `import_message` skips on a hit.
    ///
    /// Required: every delivery is indexed, and the nest refuses an empty key.
    pub dedup_key: String,
    /// The canonical-envelope key of the same plaintext, computed beside
    /// [`Self::dedup_key`] by the same shared function (`mailbox-migration.md`
    /// § The envelope key confirms a Message-ID hit). Recorded beside the key
    /// so a later `import_message` hit on this row skips only when the imported
    /// message's content agrees — the Message-ID alone is sender-chosen, and a
    /// stranger reusing one must not make the real message skip. Required.
    pub envelope_key: String,
    // The report's bounced *address* (`dsn_recipient`) — a hint, never an
    // authorization, carried only for a nest predating the Message-ID
    // correlation — left the wire with the compat-remnant sweep
    // (`version-compatibility.md` § Dimension 2, the fourth write-off).
    /// Set (only) when `sender_address` is empty — the SMTP null reverse-path
    /// `<>` — AND the raw message is a genuine RFC 3464 delivery-status report
    /// (`multipart/report; report-type=delivery-status`), and it is what
    /// the guardian mail gate authorizes on: the **original
    /// Message-ID the report is about**, read at the MTA perimeter from the
    /// report's `message/rfc822` / `text/rfc822-headers` part (RFC 3464 § 2.1.2).
    /// The nest delivers null-path mail to a supervised recipient only when this
    /// id matches one the ward's own outbound mail recorded — a Message-ID a
    /// Fauna app minted is unguessable, so a party the ward never mailed
    /// cannot forge a correlated report (the address alone was forgeable).
    /// `#[serde(default)]` → `None` for a non-DSN message, which fails *closed*
    /// for supervised recipients (held, never lost) and changes nothing for
    /// everyone else.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dsn_original_msgid: Option<String>,
    /// Set under the same condition as [`Self::dsn_original_msgid`]: every
    /// address appearing in the report's own address headers (`From:`,
    /// `Sender:`, `Reply-To:`, `To:`, `Cc:`), extracted at the MTA perimeter
    /// pre-seal, deduplicated and lowercased. These are the only addresses a
    /// one-click reply (or reply-all) to the report can be sent to, so the nest
    /// records them on a correlated delivery and **declines to auto-seed the
    /// ward's allowlist** for them — a reply to a report the *attacker* chose
    /// to send must not bootstrap permanent
    /// access. The nest delivers a correlated
    /// report only when this set is non-empty and small (a genuine DSN carries
    /// `From: MAILER-DAEMON@…` and little else); empty (a non-DSN message)
    /// or implausibly large fails *closed* for supervised recipients (held,
    /// never lost), the same precedent as [`Self::dsn_original_msgid`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dsn_report_addresses: Vec<String>,
    /// Set when the sealed body was too large for the RPC frame and crossed on
    /// the bulk-byte plane instead ([`MailBodyRef`]); [`Self::encrypted_body`] is
    /// then empty and the nest resolves the reference from its own blob store
    /// before taking the *same* `append_record` path an inline body takes — the
    /// at-rest shape, the seal, and the quota charge are identical either way.
    ///
    /// `#[serde(default)]` → `None` for an inline body (the MTA never
    /// sends an over-budget body — its perimeter refuses one with `552 5.3.4`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_ref: Option<MailBodyRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IngestInboundMailReply {
    /// Server-assigned 32-byte routing pointer (`blake3("fauna.bridges.
    /// ingest_inbound_mail.v1" || actor_id || timestamp_le ||
    /// encrypted_body)`). Same body+timestamp+actor returns the same id
    /// on retry — caller can use it for at-least-once delivery without
    /// duplicating storage.
    #[serde(with = "serde_bytes")]
    pub message_id: Vec<u8>,
}

/// `submit_inbound_mail` carries the same wire shape as
/// `ingest_inbound_mail`. The kind name distinguishes external-MX
/// inbound (`ingest`) from authenticated-MTA submission (`submit`);
/// the recipient-side `is_own_submission` flag is set server-side by
/// the submit handler, NOT supplied on the wire — that prevents a
/// malicious MTA from claiming external traffic is "own-submission"
/// to game any trust signal a future recipient UI derives from the
/// flag. The type alias exists for grep/IDE clarity so callers can
/// name the kind they're invoking.
pub type SubmitInboundMailRequest = IngestInboundMailRequest;
pub type SubmitInboundMailReply = IngestInboundMailReply;

/// Request for `fauna.bridges.report_rejected_scan` (T1.4 content scanning,
/// MTA-class). A `reject`-action ClamAV hit 554s at the SMTP perimeter and is
/// never stored, so it makes no `ingest_inbound_mail` call — but the admin
/// still wants the forensic audit row ("we rejected this") per
/// `mail-content-scanning.md` § Actions (the row is inserted with
/// `delivered=false`). This metadata-only report writes that row.
///
/// **Never carries the rejected bytes** — only the verdict metadata, same
/// deployment-data-plaintext contract as the `ingest`-path scan row
/// (`content-scoring.md` § The scoring-metadata bus). The nest derives a
/// synthetic `message_id` (there is no ingest id for a never-stored message)
/// and inserts via `insert_scan_result` with `delivered_to_actor=NULL`,
/// `action_taken='rejected_malware'`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ReportRejectedScanRequest {
    /// The ClamAV signature that matched (always present — only an `Infected`
    /// verdict with a `reject` action reaches this path in T1.4).
    pub clamav_signature: String,
    /// rspamd's score for the rejected message, when rspamd ran before the
    /// ClamAV reject; `None` when rspamd is disabled. Stored for forensics,
    /// never feeds routing.
    #[serde(default)]
    pub rspamd_score: Option<RspamdScore>,
    /// Receipt time (Unix seconds) — both `received_at` and `scanned_at` on the
    /// forensic row, and an input to the synthetic `message_id`.
    pub received_at: i64,
    /// Envelope-sender domain (plaintext-floor metadata; `IngestInboundMail`
    /// carries the same field). Forensic context + an input to the synthetic
    /// `message_id` so two distinct rejects don't collide.
    pub sender_domain: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReportRejectedScanReply {
    /// The synthetic 32-byte id the nest derived for the forensic row
    /// (`blake3("fauna.bridges.report_rejected_scan.v1" || signature ||
    /// received_at_le || sender_domain)`). Deterministic, so a retried report
    /// is idempotent (`INSERT OR REPLACE` on the same id).
    #[serde(with = "serde_bytes")]
    pub message_id: Vec<u8>,
}

// ── I4 Phase D.5 outbound queue (MTA bridge polls nest) ──────────
//
// `outbound_mail_queue` rows live in nest's `bins/fauna-nest/src/db/
// outbound.rs`; one row per (message, recipient). The MTA bridge polls
// `fetch_outbound_due`, attempts MX delivery, and reports back via one
// of `mark_outbound_delivered` / `mark_outbound_failed` /
// `mark_outbound_bounced`. Restart resumes from the row's
// `next_attempt_at`; no local-disk spool on the bridge side. Per goal
// doc `docs/goal/behavior/smtp-server.md` § Outbound delivery.

/// Request for `fauna.bridges.fetch_outbound_due`. The bridge worker
/// loop polls with `max` to bound batch size and `lease_seconds` to
/// give nest a hint of how long the bridge expects to take before
/// reporting back; nest currently treats `lease_seconds` as advisory
/// (the wire shape reserves it for a future lease-locking mode that
/// hides leased rows from sibling MTA workers). Both fields must be
/// non-zero — nest rejects 0 as malformed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FetchOutboundDueRequest {
    pub max: u32,
    pub lease_seconds: u32,
}

/// One due outbound-queue row returned by `fetch_outbound_due`. Mirrors
/// the persistence-layer `OutboundRow` minus diagnostic fields the
/// bridge doesn't act on (delay_warned_at, inbound_verdicts, …); those
/// stay nest-side because re-emitting them on the wire would let a
/// compromised MTA see the original recipient's inbound auth context.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct OutboundUnit {
    /// Server-assigned row id; pass back verbatim to the mark_* RPCs.
    pub id: i64,
    /// `Message-ID:` header value the original submission carried (used
    /// for log correlation and NDR addressing). UTF-8 string already
    /// stripped of angle brackets nest-side.
    pub message_id: String,
    /// Envelope sender (`MAIL FROM`) the original submission used.
    pub original_sender: String,
    /// Single RCPT TO for this row. Nest exploded multi-recipient
    /// submissions into one row per recipient at enqueue time.
    pub recipient: String,
    /// Post-DATA dot-stuffed `\r\n`-line-terminated bytes, ready to ship
    /// over SMTP. The nest DKIM-signs it at this hand-out when it holds the
    /// key for the From domain's active selector
    /// (`mail-bridge-lifecycle.md` § DKIM provisioning (automatic) → *Custody
    /// moves to the nest*); otherwise it is the row as it rests, unsigned.
    /// The worker ships it verbatim either way.
    #[serde(with = "serde_bytes")]
    pub raw_message: Vec<u8>,
    /// Attempt counter for diagnostics + retry-curve decisions. Zero
    /// on first poll.
    pub attempt_count: u32,
    /// Set **iff** [`Self::raw_message`] is empty: the plaintext body was over
    /// the inline reply budget, so nest sealed it under a one-shot AEAD key and
    /// staged the ciphertext on the bulk-byte plane (the staged-envelope rule,
    /// `smtp-server.md` § Message size limits). The worker GETs the chunks over
    /// the open download route, joins them fail-closed on `total_bytes`, and
    /// opens the AEAD with `key` to recover the identical `raw_message` bytes.
    /// nest re-stages statelessly per serve, so a re-fetch simply produces a
    /// fresh reference (superseded chunks age out as GC orphans). Additive on
    /// the version-locked in-image bridge data-plane (`transport.md` § Schema
    /// and forward-compat discipline, rule 4), so `deny_unknown_fields` stays.
    /// `#[serde(default)]` so an absent key (no staged body) decodes as `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub staged_body: Option<StagedBodyRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FetchOutboundDueReply {
    pub units: Vec<OutboundUnit>,
}

/// `mark_outbound_delivered` — the recipient's MX accepted the message
/// (a 2xx final reply at end-of-DATA). Nest transitions the row to
/// `sent` and stops scheduling retries.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MarkOutboundDeliveredRequest {
    pub id: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MarkOutboundDeliveredReply {
    pub ok: bool,
}

/// `mark_outbound_failed` — a soft (4xx / connection / TLS) failure the
/// bridge wants nest to retry. nest owns the retry curve
/// (`smtp-server.md` § Outbound delivery: "nest reschedules per the retry
/// schedule below"), so `retry_after_seconds` is a server Retry-After
/// *hint* (0 = none, capped server-side at the catalog ceiling), honoured
/// by nest as a *floor* under its computed backoff — never a ceiling. nest
/// applies the curve, emits the once-per-message 4 h delay-warning DSN,
/// bumps `attempt_count`, records `last_error`, and on budget/timeout
/// exhaustion promotes the row to a permanent-failure bounce.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MarkOutboundFailedRequest {
    pub id: i64,
    pub retry_after_seconds: u32,
    pub last_error: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MarkOutboundFailedReply {
    pub ok: bool,
}

/// `mark_outbound_bounced` — terminal failure. The bridge calls this
/// when either (a) the recipient's MX rejected with 5xx, (b) no MX
/// resolved, or (c) the retry budget was exhausted. Nest transitions
/// the row to `bounced` and queues an NDR for the original sender
/// (subject to the per-sender bounce rate-limit).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MarkOutboundBouncedRequest {
    pub id: i64,
    pub reason: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MarkOutboundBouncedReply {
    pub ok: bool,
}

/// `enqueue_outbound_mail` — the submission listener's `Data` hook
/// hands off a freshly-signed body for external delivery. Nest
/// inserts one `outbound_mail_queue` row per recipient and returns
/// the assigned row ids. Per goal doc § Outbound delivery: every
/// external recipient gets its own retry curve.
///
/// Two caller classes reach this (`bridge_method_allowlist.rs`):
/// - **`BridgeMta`** (the SMTP submission gateway) — sender-unconstrained
///   by design: the MTA AUTH'd an arbitrary local submitter and carries
///   the verified `original_sender`. It leaves `on_behalf_of_actor` unset.
/// - **`BridgeMda`** (the server-side `calendar-auto-schedule` gateway,
///   caldav-server.md § Server-side auto-schedule) — **caller-scoped**:
///   it MUST set `on_behalf_of_actor` to the AUTH'd organizer it is
///   fanning an iMIP out for, and nest rejects the call unless
///   `original_sender` resolves (exact alias) to that same actor. This
///   pins the MDA to sending only as the one organizer whose encrypted
///   PUT session it is inside — a strictly narrower grant than the MTA's
///   (the same `target == actor` shape `require_caller_scope` enforces
///   for the calendar storage RPCs).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EnqueueOutboundMailRequest {
    pub original_msgid: String,
    pub original_sender: String,
    pub recipients: Vec<String>,
    #[serde(with = "serde_bytes")]
    pub raw_message: Vec<u8>,
    /// The organizer actor (32 bytes) a `BridgeMda` caller is fanning out
    /// for. Required for the MDA path, omitted (`None`) by the MTA. nest
    /// caller-scopes the MDA against it (see the struct doc). Omitted on
    /// the wire when `None`, so the MTA's bytes are unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none", with = "serde_bytes")]
    pub on_behalf_of_actor: Option<Vec<u8>>,
    /// Set **iff** [`Self::raw_message`] is empty: the DKIM-signed body was over
    /// the inline request budget, so the MTA sealed it under a one-shot AEAD key
    /// and staged the ciphertext on the bulk-byte plane (the staged-envelope
    /// rule, `smtp-server.md` § Message size limits). nest resolves the chunks
    /// from its own blob store, joins them fail-closed on `total_bytes`, opens
    /// the AEAD with `key`, and enqueues the recovered `raw_message` exactly as
    /// an inline submission. Additive on the version-locked in-image bridge
    /// data-plane (`transport.md` rule 4), so `deny_unknown_fields` stays.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub staged_body: Option<StagedBodyRef>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EnqueueOutboundMailReply {
    /// One row id per **off-box (MX-relayed)** recipient, in order. An
    /// in-domain recipient that resolves to a local mailbox is delivered
    /// locally (sealed-ingest) and gets **no** queue row, so `ids` may be
    /// shorter than `recipients` (or empty when every recipient was
    /// in-domain). The bridge uses these for log correlation when it later
    /// sees a `fetch_outbound_due` reply mention them; it does not assume a
    /// 1:1 mapping to `recipients`.
    pub ids: Vec<i64>,
}

/// `deliver_sealed_scheduling` — the MDA server-side `calendar-auto-schedule`
/// gateway's **mailbox-less** delivery rail (caldav-server.md § Server-side
/// auto-schedule). The email-reachable leg rides `enqueue_outbound_mail` above;
/// a mailbox-less Fauna attendee (CalDAV enabled, email disabled) instead gets
/// the iMIP over the WS-RPC sealed MLS scheduling rail.
///
/// The MDA seals the one-off MLS scheduling welcome + iMIP message **itself**
/// (`mailfauna.BuildSchedulingDelivery`, an **ephemeral** signer — so the nest
/// only ever sees ciphertext, the encryption invariant), then ships the OPAQUE
/// bytes here. The nest runs `welcome.deliver`(tagged `Scheduling`) +
/// `channel.send` **as the organizer** (membership/inbox bind to the organizer,
/// not the MDA), caller-scoped to the AUTH'd organizer exactly like
/// `enqueue_outbound_mail` (BridgeMda-only — it MUST set `on_behalf_of_actor`,
/// and `original_sender` must resolve, exact-alias, to it). This keeps the MDA's
/// new perimeter to ONE delivery RPC plus the `keypackage.fetch` widening,
/// rather than blanket-widening the User-class conversation RPCs.
///
/// `peer_domain` carries the rail across nests: `Some(domain)` ⇒ the recipient
/// lives on a foreign nest and the home nest relays the Welcome there; `None` /
/// empty ⇒ same-nest. The channel log always lives on THIS (the organizer's)
/// nest; a cross-nest recipient drains the iMIP application message via the
/// membership-gated `fauna.federation.channel.fetch` relay.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DeliverSealedSchedulingRequest {
    /// The AUTH'd organizer (32 bytes) this delivery is fanned out for. The
    /// Welcome + channel message bind to it; nest caller-scopes the MDA against
    /// it (`original_sender` must resolve here). Always present (BridgeMda-only).
    #[serde(with = "serde_bytes")]
    pub on_behalf_of_actor: Vec<u8>,
    /// The organizer's `local@domain` (the MDA's AUTH'd identity —
    /// `AuthedLocalPart()@AuthedDomain()`). nest verifies it resolves
    /// (exact-alias, the login resolver) to `on_behalf_of_actor`.
    pub original_sender: String,
    /// Hex-encoded recipient actor id (32 bytes) — the mailbox-less attendee.
    pub recipient_actor_id: String,
    /// `Some(domain)` when the recipient lives on a **foreign** nest (the home
    /// nest relays the Welcome there); absent / empty ⇒ same-nest. nest derives
    /// the relay base URL via `resolve_handle_domain` — the same mapping the
    /// client rail's `peer_nest_url` uses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer_domain: Option<String>,
    /// Hex-encoded one-off MLS channel id (32 bytes), from
    /// `SealedSchedulingDelivery.channel_id_hex`.
    pub channel_id: String,
    /// Opaque sealed MLS Welcome bytes (the MDA's ephemeral-signer output) — fed
    /// to the recipient's `process_welcome` so it joins the one-off channel.
    #[serde(with = "serde_bytes")]
    pub welcome_bytes: Vec<u8>,
    /// Opaque sealed iMIP channel message (the `Scheduling` application
    /// envelope) — the channel's first + only message.
    #[serde(with = "serde_bytes")]
    pub app_envelope: Vec<u8>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DeliverSealedSchedulingReply {
    /// Inbox row id from the same-nest Welcome push (`0` on the cross-nest relay
    /// path, mirroring `WelcomeDeliverReply.inbox_id`).
    pub inbox_id: i64,
    /// Per-channel sequence assigned to the iMIP application message.
    pub seq: i64,
}

/// `fauna.bridges.send_auto_reply` (BridgeMta) — the MTA perimeter, after a
/// delivered message matched an `AutoReply` filter rule and passed the loop
/// guard (`fauna_mail::filter::auto_reply_decision`), hands nest the
/// already-composed + DKIM-signed reply bytes. nest atomically (1) checks +
/// records the vacation rate limit so each `(recipient, envelope-sender)` pair
/// gets at most one reply per `interval_hours` (`auto_reply_log`), and (2) if
/// the slot is free, enqueues the reply into `outbound_mail_queue` with a **null
/// envelope-from** (`MAIL FROM:<>`, RFC 3834 — an auto-reply must never be
/// bounceable into a loop; the submission `enqueue_outbound_mail` path forbids a
/// null sender, so this dedicated path owns it). nest derives the rate-limit key
/// by hashing the lowercased envelope-from (BLAKE3) so the convention lives in
/// one place and the raw sender never sits in `auto_reply_log`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct SendAutoReplyRequest {
    /// The recipient actor the auto-reply is "from" (32 bytes).
    #[serde(with = "serde_bytes")]
    pub recipient_actor_id: Vec<u8>,
    /// The envelope sender we are replying to (the outbound recipient). nest
    /// hashes it to the rate-limit key and uses it as the queued row's recipient.
    pub envelope_from: String,
    /// The vacation interval from the matched action (RFC 5230 default 168 h);
    /// nest clamps it to a sane floor/ceiling.
    pub interval_hours: u32,
    /// The reply's `Message-ID` (queue correlation / `original_msgid`).
    pub original_msgid: String,
    /// The composed + DKIM-signed RFC 5322 reply bytes.
    #[serde(with = "serde_bytes")]
    pub raw_message: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct SendAutoReplyReply {
    /// `true` ⇒ the slot was free; the reply is now enqueued. `false` ⇒ a reply
    /// was already sent within the window — suppressed, nothing enqueued.
    pub sent: bool,
}

// ── T2.1 outbound MTA-STS policy (MTA bridge asks nest at delivery) ──

/// Request for `fauna.bridges.fetch_mta_sts_policy`. The MTA bridge, at
/// delivery time, asks nest for the recipient domain's MTA-STS policy
/// (RFC 8461). nest owns the `_mta-sts.<domain>` TXT + `.well-known/
/// mta-sts.txt` fetch and the per-`max_age` cache (smtp-server.md § MX
/// resolution, item 3); the bridge applies the per-host enforce/testing
/// decision locally before the TLS handshake.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FetchMtaStsPolicyRequest {
    pub domain: String,
}

/// The parsed MTA-STS policy body (RFC 8461 §3.2), present iff
/// `FetchMtaStsPolicyReply::outcome == "found"`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MtaStsPolicyWire {
    /// `id=` from the `_mta-sts.<domain>` TXT (lets the bridge log policy rotations).
    pub id: String,
    /// `"enforce"` | `"testing"` | `"none"`.
    pub mode: String,
    /// `mx:` patterns (input order preserved); RFC 8461 §4.1 matching.
    pub mx: Vec<String>,
    pub max_age_secs: u32,
}

/// Reply for `fauna.bridges.fetch_mta_sts_policy`. `outcome` distinguishes
/// the four RFC 8461 §5 / RFC 8460 §4.3 lookup outcomes the bridge must
/// tell apart for both enforcement and (T2.4) TLSRPT attribution:
/// `"not_published"` (no STS advertised → no enforcement),
/// `"fetch_error"` (TXT advertised STS but body fetch failed → treat as
/// no-policy, never force plaintext), `"invalid"` (body unparseable →
/// treat as no-policy), `"found"` (policy parsed; `policy` is Some).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FetchMtaStsPolicyReply {
    pub outcome: String,
    pub policy: Option<MtaStsPolicyWire>,
}

// ── T2.1b outbound DANE/TLSA (MTA bridge asks nest at delivery) ──

/// Request for `fauna.bridges.fetch_tlsa`. The MTA bridge, at delivery
/// time, asks nest for a recipient MX host's DANE/TLSA records (RFC 7672).
/// nest owns the `_25._tcp.<mx_host>` DNSSEC-validating lookup (the Go
/// stdlib can't do DNSSEC); the bridge pins the TLS handshake against the
/// returned records via the `dane_chain_matches` UniFFI matcher. `mx_host`
/// is the bare MX hostname (no port).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FetchTlsaRequest {
    pub mx_host: String,
}

/// One DANE/TLSA record (RFC 6698) in the `fetch_tlsa` reply. nest returns
/// only DNSSEC-secure, SMTP-usable (DANE-TA / DANE-EE — RFC 7672 §3.1)
/// records, so the bridge treats a non-empty `records` list as "DANE
/// applies to this host". `usage` / `selector` / `matching` are the RFC
/// 6698 octets; `data` is the certificate-association data.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TlsaRecordWire {
    pub usage: u8,
    pub selector: u8,
    pub matching: u8,
    #[serde(with = "serde_bytes")]
    pub data: Vec<u8>,
}

/// Reply for `fauna.bridges.fetch_tlsa`. An empty `records` list means the
/// host publishes no usable DNSSEC-secure TLSA records — the bridge falls
/// back to the MTA-STS / opportunistic posture (no DANE pinning). A
/// resolver/network failure surfaces as an RPC error, which the bridge
/// also treats as "no DANE" (a TLSA fetch failure must not block delivery).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FetchTlsaReply {
    pub records: Vec<TlsaRecordWire>,
}

// ── Outbound MX resolution with DNSSEC provenance (MTA bridge → nest) ──

/// Request for `fauna.bridges.resolve_mx`. The MTA bridge, at delivery
/// time, asks nest to resolve a recipient domain's SMTP targets. nest owns
/// the lookup for the same stated reason it owns `fetch_tlsa`: the Go
/// stdlib resolver cannot do DNSSEC, and outbound DANE needs to know
/// whether the MX RRset was validated (RFC 7672 §2.2).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ResolveMxRequest {
    /// Bare recipient domain (the part after `@`), no trailing dot.
    pub domain: String,
}

/// One ranked SMTP target in the `resolve_mx` reply. `priority` is the RFC
/// 5321 §5.1 MX preference (lowest first); `hostname` is the bare exchange
/// name with no trailing dot and no port.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MxHostWire {
    pub priority: u16,
    pub hostname: String,
}

/// Reply for `fauna.bridges.resolve_mx`.
///
/// `secure` reports whether the MX RRset these hosts came from carried a
/// DNSSEC `Secure` proof — **the bridge must gate DANE pinning on it**.
/// Without that gate the DNSSEC validation on the TLSA leg authenticates a
/// name the attacker chose: a DNS-spoofing attacker forges
/// `MX victim.test → mx.attacker.test`, publishes a genuine signed TLSA for
/// their own name, and the pin succeeds against the wrong host
/// (`docs/goal/behavior/smtp-server.md` § Architectural rules, outbound
/// DANE). `false` means deliver, but with the MTA-STS / opportunistic
/// posture and no DANE — a non-secure MX answer must *skip* DANE, never
/// fail delivery. An implicit-MX answer (the domain publishes no MX RRset,
/// so the target is the recipient domain itself) is `secure: true`: no
/// attacker-chosen name entered the decision.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ResolveMxReply {
    pub hosts: Vec<MxHostWire>,
    pub secure: bool,
}

// ── T2.4 outbound TLSRPT per-attempt report (MTA bridge → nest) ──

/// Request for `fauna.bridges.report_tls_attempt` (T2.4). After each
/// outbound delivery attempt to one MX host, the Go MTA bridge reports the
/// raw TLS-posture facts it already holds; nest reconstructs the RFC 8460
/// §4.4 policy bucket via the shared pure
/// `fauna_mail::outbound::tlsrpt::policy_for_attempt` and records it into
/// the daily TLSRPT aggregator (`docs/goal/behavior/smtp-server.md`
/// § TLSRPT outbound reporter). Resolving the bucket nest-side single-sources
/// the `tlsa`>`sts`>`no-policy-found` precedence + the policy-string
/// formatting; the only genuinely Go-side input is `result_type` (the
/// `crypto/tls`/`net/smtp` handshake outcome, which can't be a shared-Rust
/// classification because the error types are Go's). MTA-only.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ReportTlsAttemptRequest {
    /// Recipient (`RCPT TO`) domain — the `sts`/`no-policy-found`
    /// policy-domain.
    pub recipient_domain: String,
    /// The MX host this attempt connected to — the `tlsa` policy-domain.
    pub mx_host: String,
    /// `None` = the TLS session succeeded; `Some(token)` = an RFC 8460 §4.3
    /// `result-type` (`starttls-not-supported`, `certificate-host-mismatch`,
    /// `validation-failure`, `tlsa-invalid`, `sts-webpki-invalid`,
    /// `sts-policy-mismatch`, `sts-policy-fetch-error`, `sts-policy-invalid`).
    pub result_type: Option<String>,
    /// The MTA-STS lookup outcome nest returned for this domain, echoed back
    /// so nest re-derives the bucket without a second fetch:
    /// `"not_published"|"fetch_error"|"invalid"|"found"`.
    pub mta_sts_outcome: String,
    /// The MTA-STS policy the bridge enforced — `Some` iff
    /// `mta_sts_outcome == "found"`; reused to build the `sts` policy-string.
    pub mta_sts_policy: Option<MtaStsPolicyWire>,
    /// The DANE/TLSA records the handshake was pinned to — non-empty iff
    /// DANE applied; reused (via `tlsa_policy_strings`) for the `tlsa`
    /// policy-string and to give `tlsa` precedence over `sts`.
    pub tlsa_records: Vec<TlsaRecordWire>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ReportTlsAttemptReply {
    pub ok: bool,
}

// ── I2b Phase-C.3 body-fetch + index-segments types ──────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FetchMessageCiphertextRequest {
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    /// 32-byte server-assigned message id.
    #[serde(with = "serde_bytes")]
    pub message_id: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum FetchMessageCiphertextReply {
    Found {
        /// The sealed body, inline. Empty **iff** `body_ref` is set.
        #[serde(with = "serde_bytes")]
        encrypted_body: Vec<u8>,
        ciphertext_size: u32,
        /// Epoch seconds (`bridge_inbound_mail.timestamp`).
        internal_date: i64,
        /// Set when the stored body is too large for the RPC frame: the nest has
        /// staged it on the bulk-byte plane and the MDA GETs the chunks back
        /// (`GET /api/v1/chunks/{hash}` — an **open** route: ciphertext by hash,
        /// no key, so no token is needed on this leg) and rejoins them with
        /// `fauna_mail::body_ref`.
        ///
        /// `#[serde(default)]` → `None` for every message that fits inline, which
        /// is every message that never needed a reference, so an MDA that does
        /// not know the field reading a newer nest can only ever meet a
        /// reference for mail too large to have been stored inline.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        body_ref: Option<MailBodyRef>,
        /// Unix **seconds** this nest stored (= sealed) the record — the
        /// content-sealing-epochs classification basis (`MailFloorMetadata::
        /// stored_at` ms / 1000). Distinct from `internal_date`, which is the
        /// message's own (possibly historical, client-supplied) timestamp:
        /// for MTA-ingested mail the two coincide, but an *imported* message
        /// carries a historical `internal_date` while its seal keyed off
        /// ingest-time now — an epoch-aware reader classifying off
        /// `internal_date` would try only epochs older than the seal's and
        /// go permanently dark on imported mail. Always on
        /// the wire; `0` = unknown (the append-time clock read failed), a
        /// standing-sealed record every epoch chain's standing arm opens.
        stored_at: i64,
    },
    NotFound,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FetchIndexSegmentsSinceRequest {
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    /// `None` = across all of the actor's mailboxes.
    pub mailbox: Option<String>,
    pub since_modseq: i64,
    /// 0 = no limit; pagination on ascending modseq.
    pub limit: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct IndexSegment {
    #[serde(with = "serde_bytes")]
    pub message_id: Vec<u8>,
    pub mailbox: String,
    pub modseq: i64,
    #[serde(with = "serde_bytes")]
    pub encrypted_index_hint: Vec<u8>,
    /// Unix **seconds** this nest stored (= sealed) the record, from the
    /// `segment_records.stored_at` mirror (ms / 1000) — the content-sealing-
    /// epochs classification basis for the sealed hint, which seals to the
    /// same epoch-gated recipient key as the body in the same ingest
    /// transaction (`seal_and_persist_local`). Always on the wire; `0` =
    /// unknown (the append-time clock read failed) — such a hint is
    /// standing-sealed, which every epoch chain ends on.
    pub stored_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FetchIndexSegmentsSinceReply {
    /// Ascending modseq.
    pub segments: Vec<IndexSegment>,
    /// MAX over the queried mailbox(es) for this actor; 1 if none.
    pub highestmodseq: i64,
    pub more: bool,
}

// ── I2b Phase-C IMAP metadata types ──────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ListMailboxesRequest {
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    /// When `true`, restrict the reply to mailboxes the actor has
    /// SUBSCRIBEd to (rows present in `bridge_imap_subscriptions`).
    /// LSUB and `LIST (SUBSCRIBED)` set this; plain LIST leaves it
    /// `false`. `#[serde(default)]`: an omitted field (plain LIST) decodes as `false`.
    #[serde(default)]
    pub subscribed_only: bool,
}

/// Per-mailbox metadata returned by `list_mailboxes`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MailboxEntry {
    pub name: String,
    pub uid_validity: u32,
    pub uid_next: u32,
    pub highestmodseq: i64,
    /// Total number of messages in this mailbox.
    pub exists: u32,
    /// Number of messages whose flag set lacks `\Seen`.
    pub unseen: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ListMailboxesReply {
    pub mailboxes: Vec<MailboxEntry>,
}

/// Optional QRESYNC hint supplied by an IMAP client on `SELECT (QRESYNC ...)`.
/// `(uid_validity, last_modseq)` from RFC 7162 §3.2 parameters 1 and 2; the
/// `last_uid_set` / `last_known_uids` portions are not needed at the
/// divergence-detection seam.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct QResyncHint {
    pub last_uid_validity: u32,
    pub last_modseq: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SelectMailboxRequest {
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    pub mailbox: String,
    /// MUA-supplied QRESYNC hint for divergence detection at SELECT time.
    /// The Go bridge forwards it (`internal/mda/imap/select.go`, T3.2-b);
    /// `None` only when the MUA didn't supply QRESYNC on this SELECT.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_qresync: Option<QResyncHint>,
    /// MUA identifier from the prior IMAP `ID` command (RFC 2971);
    /// advisory, may be missing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mua_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum SelectMailboxReply {
    Selected {
        uid_validity: u32,
        uid_next: u32,
        highestmodseq: i64,
        exists: u32,
        /// Always 0 — nest does not track `\Recent`.
        recent: u32,
        unseen: u32,
        /// UID of the first (lowest-UID) unseen message, or `None` if all
        /// messages have been seen.
        first_unseen_uid: Option<u32>,
    },
    NoSuchMailbox,
}

// ── I2b Phase-C.2 message-metadata types ─────────────────────────

/// Per-message metadata surface for `list_messages` and
/// `fetch_message_metadata` replies.  The `message_id` is the
/// server-assigned 32-byte routing pointer stored in
/// `bridge_inbound_mail`.  `flags` follows the IMAP token convention
/// (`\Seen`, `\Answered`, …, keywords); empty Vec means no flags set.
/// `internal_date` is epoch seconds (IMAP INTERNALDATE).
/// `ciphertext_size` comes from `bridge_inbound_mail.ciphertext_size`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MessageMeta {
    pub uid: u32,
    #[serde(with = "serde_bytes")]
    pub message_id: Vec<u8>,
    pub modseq: i64,
    pub flags: Vec<String>,
    pub internal_date: i64,
    pub ciphertext_size: u32,
    /// 1-based IMAP sequence number: the message's rank in the mailbox's full
    /// ascending-UID order (RFC 9051 §6.4.5), computed nest-side. Carried on
    /// every row (including a UID-subset fetch) so the Go bridge emits correct
    /// seqNums without re-fetching the whole mailbox to number them (F1).
    /// Additive; `#[serde(default)]` for fixture/decode hygiene.
    #[serde(default)]
    pub seq_num: u32,
}

/// Request for `fauna.bridges.list_messages`.
///
/// `since_modseq`: CONDSTORE incremental sync — when `Some(n)`, only
/// rows with `modseq > n` are returned, plus `expunged_uids` is
/// populated.  `limit`: 0 = no limit; pagination on ascending uid.
/// `after_uid`: resume token — return only rows with `uid > after_uid`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ListMessagesRequest {
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    pub mailbox: String,
    pub since_modseq: Option<i64>,
    pub limit: u32,
    pub after_uid: Option<u32>,
}

/// Reply for `fauna.bridges.list_messages`.
///
/// `messages` is ordered by ascending uid.  `expunged_uids` is
/// populated only when `since_modseq` was `Some`.  `more = true`
/// means the caller should page with `after_uid = messages.last().uid`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ListMessagesReply {
    pub messages: Vec<MessageMeta>,
    pub expunged_uids: Vec<u32>,
    pub highestmodseq: i64,
    pub more: bool,
}

/// Request for `fauna.bridges.fetch_message_metadata`.
///
/// `uids`: empty Vec = all messages in the mailbox (equivalent to
/// passing no UID filter).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FetchMessageMetadataRequest {
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    pub mailbox: String,
    pub uids: Vec<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FetchMessageMetadataReply {
    pub messages: Vec<MessageMeta>,
    /// Count of live messages in the mailbox (IMAP EXISTS total), independent
    /// of the `uids` filter. Lets the IDLE present-UID events (Append/Move-dst)
    /// report EXISTS after a single-UID metadata fetch instead of pulling the
    /// whole mailbox (F1). Additive; `#[serde(default)]` for decode hygiene.
    #[serde(default)]
    pub mailbox_total: u32,
}

// ── I2b Phase-C.4 store_flags + expunge types ────────────────────

/// Which mutation to perform in a `store_flags` request.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum StoreFlagsOp {
    /// Replace the entire flag set with the supplied list.
    #[default]
    Set,
    /// Add the supplied flags to the existing set.
    Add,
    /// Remove the supplied flags from the existing set.
    Remove,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct StoreFlagsRequest {
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    pub mailbox: String,
    /// Must be non-empty; handler returns malformed if empty.
    pub uids: Vec<u32>,
    pub op: StoreFlagsOp,
    /// IMAP flag tokens (e.g. `\Seen`, `\Flagged`, keywords).
    /// `\Recent` is rejected as malformed by the handler.
    pub flags: Vec<String>,
    /// CONDSTORE `UNCHANGEDSINCE modseq` precondition (RFC 7162 §3.1.3).
    /// When `Some(n)`, the handler partitions UIDs in-transaction:
    /// rows whose modseq ≤ n are updated and reported in `reply.updated`;
    /// rows whose modseq > n are skipped and reported in `reply.modified`.
    /// When `None` (the default), STORE is unconditional and `modified`
    /// is always empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unchanged_since: Option<i64>,
}

/// Per-UID result entry returned by `store_flags`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct StoreFlagsResultEntry {
    pub uid: u32,
    /// Resulting flag set in canonical (lexicographic) ordering.
    pub flags: Vec<String>,
    /// New modseq for this message (all touched rows in one STORE share
    /// the same bump).
    pub modseq: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct StoreFlagsReply {
    /// One entry per UID that had a placement row (missing UIDs are
    /// silently skipped per RFC 3501).
    pub updated: Vec<StoreFlagsResultEntry>,
    /// New mailbox highestmodseq after the bump.
    pub highestmodseq: i64,
    /// UIDs whose modseq was strictly greater than the request's
    /// `unchanged_since` and were therefore skipped per RFC 7162 §3.1.3.
    /// Empty when `unchanged_since` was `None` or every UID passed the
    /// precondition. The MDA emits `OK [MODIFIED <uid_set>]` from this.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modified: Vec<u32>,
}

/// Request for `fauna.bridges.expunge`.
///
/// `uids` empty ⇒ plain EXPUNGE (all `\Deleted`-flagged messages).
/// `uids` non-empty ⇒ UID EXPUNGE (RFC 4315): the intersection of
/// the listed UIDs and `\Deleted`-flagged messages.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExpungeRequest {
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    pub mailbox: String,
    pub uids: Vec<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExpungeReply {
    /// Ascending UIDs of messages that were deleted.
    pub expunged_uids: Vec<u32>,
    pub highestmodseq: i64,
}

// ── I2b Phase-C.5 copy + move types ──────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CopyMessagesRequest {
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    pub source_mailbox: String,
    /// Must be non-empty; handler returns malformed if empty.
    pub uids: Vec<u32>,
    /// Destination mailbox — auto-created if no state row exists yet.
    pub dest_mailbox: String,
}

/// One (source_uid, dest_uid) pair from a COPY or MOVE operation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CopyPair {
    pub source_uid: u32,
    pub dest_uid: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CopyMessagesReply {
    pub dest_uid_validity: u32,
    /// One entry per source UID that had a placement row; skipped UIDs excluded.
    /// Ordered by the original `uids` request order.
    pub copied: Vec<CopyPair>,
    pub dest_highestmodseq: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MoveMessagesRequest {
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    pub source_mailbox: String,
    /// Must be non-empty; handler returns malformed if empty.
    pub uids: Vec<u32>,
    /// Destination mailbox — auto-created if no state row exists yet.
    pub dest_mailbox: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MoveMessagesReply {
    pub dest_uid_validity: u32,
    /// One entry per source UID that was copied (and then expunged from source).
    pub moved: Vec<CopyPair>,
    pub source_highestmodseq: i64,
    pub dest_highestmodseq: i64,
}

// ── I2b Phase-C.6 append types ───────────────────────────────────

/// Request for `fauna.bridges.append`.
///
/// The MDA uploads a user-generated message (encrypted body + index hint)
/// into a named mailbox with caller-supplied flags.  Unlike inbound mail
/// there is no SMTP envelope, verdicts, or spam scoring — it is
/// user-uploaded.
///
/// Validation (handled server-side):
/// - `encrypted_body` must be non-empty.
/// - `encrypted_index_hint` must be non-empty.
/// - `ciphertext_size` must equal `encrypted_body.len()`.
/// - `\\Recent` must not appear in `flags` (RFC 3501 §2.3.2).
// `Default` is for struct-update fixtures (`..Default::default()`), not for a
// meaningful empty request — the validated invariants above make a defaulted
// value malformed on purpose. Two branches independently growing this struct
// then merge cleanly instead of colliding on the grown axis (same reasoning as
// `IngestInboundMailRequest`, which has derived it since the scores field).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AppendMessageRequest {
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    /// Destination mailbox.  Auto-created (lazy state row) if it does not
    /// already exist.
    pub mailbox: String,
    /// Initial IMAP flags.  `\\Recent` is rejected as malformed.
    pub flags: Vec<String>,
    /// The sealed message body, inline.  Empty **iff** [`Self::body_ref`] is set
    /// — a sealed body over the inline budget crosses on the bulk-byte plane
    /// instead (the nest rejects an APPEND that is empty on both, so a
    /// version-skewed MDA can never store an empty message).  The APPEND leg
    /// stages *already-sealed* bytes, so it rides a plain [`MailBodyRef`], never
    /// the plaintext staged-envelope (`smtp-server.md` § Message size limits).
    #[serde(with = "serde_bytes")]
    pub encrypted_body: Vec<u8>,
    /// Encrypted search-index hint.  Must be non-empty.  Always rides inline
    /// (only the body can go by reference).
    #[serde(with = "serde_bytes")]
    pub encrypted_index_hint: Vec<u8>,
    /// IMAP INTERNALDATE as epoch seconds.
    pub timestamp: i64,
    /// Must equal the sealed body length — `encrypted_body.len()` inline, or
    /// [`MailBodyRef::total_bytes`] when the body rode by reference.
    pub ciphertext_size: u32,
    /// Floor metadata (envelope FROM domain).  Empty string is allowed.
    pub sender_domain: String,
    /// Canonical dedup key over the *plaintext* message
    /// (`mailbox-migration.md` § Key format), computed by the Go MDA before it
    /// seals — the nest cannot compute it, since `encrypted_body` is already
    /// ciphertext by the time it arrives here.
    ///
    /// Nest **records** it in `actor_message_dedup` and never acts on a hit:
    /// an APPEND is a user storing mail, not an import, so a duplicate key must
    /// still be stored. Only `import_message` skips on a hit.
    ///
    /// Required: every APPEND is indexed, and the nest refuses an empty key.
    pub dedup_key: String,
    /// The canonical-envelope key of the same plaintext, computed beside
    /// [`Self::dedup_key`] by the same shared function (`mailbox-migration.md`
    /// § The envelope key confirms a Message-ID hit). Recorded beside the key;
    /// a later `import_message` hit on this row skips only when the envelope
    /// keys agree. Required.
    pub envelope_key: String,
    /// The sealed body by reference, when it was over the inline budget and rode
    /// the bulk-byte plane instead of [`Self::encrypted_body`] (`smtp-server.md`
    /// § Message size limits — the MDA-APPEND upward leg).  Exactly one of
    /// `encrypted_body` / `body_ref` carries the body; the nest resolves the
    /// reference from its local blob store, verifies it against the same seal
    /// gate an inline body passes, and stores it bit-for-bit identically —
    /// only the transport differed.  `#[serde(default)]` → `None` for an
    /// inline body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_ref: Option<MailBodyRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AppendMessageReply {
    /// Deterministic 32-byte server-assigned message id:
    /// `blake3("fauna.bridges.append_message.v1" ‖ actor_id ‖
    /// timestamp_le_i64 ‖ encrypted_body)`.  Distinct from the
    /// `ingest_inbound_mail` domain tag so identical bytes on different
    /// paths do not collide.
    #[serde(with = "serde_bytes")]
    pub message_id: Vec<u8>,
    /// Allocated UID in `mailbox`.
    pub uid: u32,
    /// UID validity of `mailbox`.
    pub uid_validity: u32,
}

// ── Mailbox-migration import types (`mailbox-migration.md`) ──────────
//
// The import surface is the User-class twin of the BridgeMda-only APPEND:
// the user's own Fauna app pulls mail from a foreign IMAP server and
// files each message into its own mailbox over these kinds. Every kind is
// caller-scoped — there is no target-actor field; the handler derives the
// owning actor from the authenticated caller (same convention as the
// account-alias family). Session-lifecycle naming mirrors
// `mail-export.md` § RPC table (`start_*_session` / `pause` / `resume` /
// `cancel` / `finalize` / `fail`).

/// Request for `fauna.bridges.start_import_session` — opened at the wizard's
/// confirmation step (the durable commit point). Rejected with a typed error
/// while another `running`/`paused` session exists for the same
/// `source_descriptor` (the multi-device lock, `mailbox-migration.md`
/// § Architectural rules).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct StartImportSessionRequest {
    /// Provider + hostname + username — never a password. Labels the session
    /// in the UX and keys the per-source concurrency lock + dedup context.
    pub source_descriptor: String,
    /// Client-side estimate from source enumeration; revised later via
    /// `ImportMessageBatchRequest::revised_total_count`.
    pub total_count: u64,
    /// The source mailbox names the wizard's scope step selected
    /// (`mailbox-migration.md` § Resume protocol: without this, a session
    /// resumed after a client restart has no way to learn which mailboxes it
    /// was importing). Additive — an omitted scope (no selection) round-trips as
    /// empty, and a resume against such a session degrades to "scope
    /// unknown," not a wire error.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scope: Vec<String>,
    /// The scope step's "since" date, as the bare `YYYY-MM-DD` the user typed
    /// (`mailbox-migration.md` § Wizard steps step 3; the date grammar itself
    /// is `mail-export.md` § UX shape step 2's). Empty means unbounded.
    ///
    /// Recorded here because **the row is the only durable record of the range
    /// the user asked for**: the per-mailbox cursor says where the walk got to,
    /// never what it was allowed to take, so a session that did not record its
    /// date can only resume by importing everything the user excluded — a
    /// silent whole-mailbox import charged to their quota, where a forgotten
    /// `scope` merely leaves a resume with nothing to iterate.
    ///
    /// Additive exactly as `scope` is: `#[serde(default)]`, so an omitted date
    /// round-trips to unbounded rather than a wire error. A value
    /// that is not a real calendar day never arrives — it refuses `Start`
    /// client-side, before any session exists.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub date_from: String,
    /// The client-minted sealed label over `source_descriptor`
    /// (`fauna_core::label_custody::seal_import_source`), stored verbatim by
    /// the nest so the descriptor stops resting in plaintext
    /// (`encryption-at-rest.md` § Implementation status today, bullet 15 (b)).
    ///
    /// Client-minted because the root is the owner's — the nest holds no key
    /// on this plane and never mints or opens a label; it is the same
    /// store-and-serve posture as `name_sealed` / `label_sealed` /
    /// `tags_sealed`.
    ///
    /// Additive and optional: a keyless connection (bearer-only, or the web
    /// arm) sends `None`, which rests sealless — the ratified degrade, not an
    /// error (`file-sync.md` § Sealed names & paths). An omitted field
    /// round-trips the same way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_sealed: Option<ByteBuf>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct StartImportSessionReply {
    /// Nest-generated session UUID (text form).
    pub session_id: String,
}

/// One message in an import request. Field roles mirror
/// `AppendMessageRequest` (the shared write path) plus the
/// source-tracking / dedup annotations from `mailbox-migration.md`
/// § Per-message flow.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ImportMessageItem {
    /// Destination mailbox. Auto-created if missing (same as APPEND).
    pub mailbox: String,
    /// Initial IMAP flags carried over from the source. `\Recent` rejected.
    pub flags: Vec<String>,
    /// Message body: the raw RFC 5322 bytes as fetched from the source, in
    /// **both** storage modes. Must be non-empty. The nest seals body and
    /// index hint at ingest to the recipient's registered seal key
    /// (`encryption-at-rest.md` — S1 uniform seal at ingest; the nest derives
    /// the search-index hint from this plaintext, so no hint rides the wire).
    /// The retired `body_mode`/`index_hint` fields (pre-Phase-3
    /// client-pre-seal shape) are ignored if an old sender still includes
    /// them — no shipped client ever did.
    #[serde(with = "serde_bytes")]
    pub body: Vec<u8>,
    /// Source INTERNALDATE as epoch seconds.
    pub timestamp: i64,
    /// Must equal `body.len()`.
    pub body_size: u32,
    /// Envelope-FROM domain the client extracted (populates `from_norm` for
    /// SEARCH, same as APPEND's `sender_domain`). Empty string allowed.
    pub sender_domain: String,
    /// Source IMAP UID + UIDVALIDITY — the resume cursor.
    pub source_uid: u32,
    pub source_uid_validity: u32,
    /// Client-computed dedup key (`mailbox-migration.md` § Dedup): normalized
    /// Message-ID, or the canonical-envelope SHA-256 fallback.
    pub dedup_key: String,
    /// Client-computed canonical-envelope key, the second half of the pair
    /// `fauna_mail::dedup_key::mail_dedup_keys` returns. A [`Self::dedup_key`]
    /// hit is skipped only when this agrees with the stored row's envelope key
    /// (`mailbox-migration.md` § The envelope key confirms a Message-ID hit) —
    /// so a stranger's earlier delivery reusing this message's Message-ID
    /// cannot make it skip. Required.
    pub envelope_key: String,
    /// Set when the raw RFC 5322 body was too large for the RPC frame and crossed
    /// on the bulk-byte plane instead — a **staged envelope** ([`StagedBodyRef`]):
    /// the client AEAD-encrypts the plaintext under a one-shot key, stages the
    /// *ciphertext* chunks with its own session bearer, and names them here.
    /// [`Self::body`] is then empty (exactly-one-of), and the nest resolves the
    /// reference from its own blob store, opens the envelope, and takes the *same*
    /// seal-at-ingest path an inline body takes — [`Self::body_size`] stays the
    /// plaintext length, checked after the open (`smtp-server.md` § Message size
    /// limits, *the staged-envelope rule*; `mailbox-migration.md` § RPC surface).
    ///
    /// `#[serde(default)]` → `None` for an inline body (a body over the inline
    /// ceiling is staged). **Skew contract**:
    /// `import_message` is a real client↔nest skew surface, so a nest that
    /// drops this unknown key sees an empty `body` and refuses it loudly per-message
    /// — never an empty import (`mailbox-migration.md` transport-ceiling block).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub staged_body: Option<StagedBodyRef>,
}

/// Request for `fauna.bridges.import_message` (single-message convenience
/// form of the batch kind — identical semantics to a batch of one).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ImportMessageRequest {
    pub session_id: String,
    pub message: ImportMessageItem,
    /// § Opt-out per session: "Import duplicates anyway" — skips the
    /// dedup check entirely for this call.
    pub skip_dedup: bool,
}

/// Per-message outcome, `mailbox-migration.md` § Per-message flow step 3:
/// `(uid | skipped_reason | errored_reason)`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ImportMessageOutcome {
    Imported {
        /// Deterministic server-assigned content id (APPEND-domain tag —
        /// imports share APPEND's insert path).
        #[serde(with = "serde_bytes")]
        message_id: Vec<u8>,
        uid: u32,
        uid_validity: u32,
    },
    /// Not stored; `reason` ∈ {"dedup", "quota_exceeded", ...}.
    Skipped { reason: String },
    /// Rejected; `reason` is the per-message failure (accumulated client-side
    /// for the review log). Mostly a free-form parse/validation string, with one
    /// **typed** value: an over-`effective_max_raw_message_bytes` body carries
    /// exactly [`crate::email::MESSAGE_TOO_LARGE_CODE`] — the same identifier
    /// `fauna.email.send` returns as an `RpcError` code — so a client renders
    /// "message too large" uniformly across both first-party legs.
    Errored { reason: String },
    /// An outcome a newer nest added that this peer does not know: counted as
    /// errored (logged, against the error budget). Never written back.
    #[serde(other, skip_serializing)]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImportMessageReply {
    pub outcome: ImportMessageOutcome,
    /// Session counter snapshot after this call (same values the
    /// `BridgeImportProgress` push carries).
    pub imported_count: u64,
    pub skipped_count: u64,
    pub errored_count: u64,
}

/// § Batching: at most 32 messages per `fauna.bridges.import_message_batch`
/// call (`mailbox-migration.md` § Batching).
///
/// One owner for a ceiling both ends of this contract used to declare
/// privately — the nest handler that enforces it and the client `BatchPacker`
/// that must never exceed it. Nest rejects an over-long batch as a
/// **whole-call** rejection, not a per-message error, so a client whose copy
/// drifted upward would lose every message in the batch rather than just the
/// surplus. That asymmetry is why this is one constant rather than two that
/// happen to agree.
pub const MAX_BATCH_MESSAGES: usize = 32;

/// § Batching: at most 16 MiB of body bytes per
/// `fauna.bridges.import_message_batch` call. See [`MAX_BATCH_MESSAGES`] for
/// why the ceiling has one owner.
///
/// The client packs *below* this, to what the WS-RPC frame actually accepts
/// (`BatchPacker::with_limits`); nest's check on the batch's referenced byte
/// total is the defense-in-depth bound, never an inline-frame promise
/// (`mailbox-migration.md` § Batching).
pub const MAX_BATCH_BYTES: usize = 16 * 1024 * 1024;

/// Request for `fauna.bridges.import_message_batch` — bounded by
/// [`MAX_BATCH_MESSAGES`] and [`MAX_BATCH_BYTES`]. Each message commits in its
/// own transaction; one bad message doesn't roll back its siblings.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ImportMessageBatchRequest {
    pub session_id: String,
    pub messages: Vec<ImportMessageItem>,
    pub skip_dedup: bool,
    /// Revised source-enumeration total (None = unchanged).
    pub revised_total_count: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ImportMessageBatchReply {
    /// Index-aligned with `messages`.
    pub outcomes: Vec<ImportMessageOutcome>,
    pub imported_count: u64,
    pub skipped_count: u64,
    pub errored_count: u64,
}

/// Wire view of one `import_sessions` row (`mailbox-migration.md`
/// § Progress lives nest-side).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ImportSessionInfo {
    pub session_id: String,
    pub source_descriptor: String,
    /// `running` / `paused` / `errored` / `completed` / `cancelled`.
    pub state: String,
    pub started_at: i64,
    pub last_progress_at: i64,
    pub total_count: u64,
    pub imported_count: u64,
    pub skipped_count: u64,
    pub errored_count: u64,
    /// Per-mailbox resume cursor: mailbox → (last processed source UID,
    /// source UIDVALIDITY).
    pub cursors: Vec<ImportMailboxCursor>,
    /// Session-fatal reason when `state == "errored"`.
    pub error_reason: String,
    /// The source mailbox names selected at `start_import_session` (§ Resume
    /// protocol) — empty for a session started with no selection.
    /// `#[serde(default)]` so an absent key round-trips to empty rather than
    /// a decode error.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scope: Vec<String>,
    /// The session's "since" date, as recorded at `start_import_session`
    /// (§ Wizard steps step 3) — what a resumed walk re-applies so it imports
    /// the range the user asked for rather than the whole mailbox. Empty means
    /// unbounded, and is also what an absent key round-trips to.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub date_from: String,
    /// The sealed label over `source_descriptor`, for a reader whose plaintext
    /// sibling has been scrubbed (§ Resume protocol step 1 renders the source
    /// of every resumable session).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_sealed: Option<ByteBuf>,
    /// **The salt `source_sealed` opens under** — the row's `source_hash`.
    ///
    /// ⚠ Ships WITH the seal, never without it. This salt is
    /// `import_source_hash(source_descriptor)`, and `source_descriptor` is
    /// precisely what the boot scrub blanks once the seal rests: a reply
    /// carrying the seal alone renders correctly until the first reboot and
    /// then degrades every session to an unrenderable one. The set-name plane
    /// shipped that exact hole twice (`label_custody::render_set_name`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_hash: Option<ByteBuf>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ImportMailboxCursor {
    pub mailbox: String,
    pub last_processed_source_uid: u32,
    pub source_uid_validity: u32,
}

/// Request for `fauna.bridges.list_import_sessions` — resume protocol step 1.
/// Caller-scoped; returns the caller's non-expired sessions (all states, so
/// the client can render recent history; `running`/`paused` are the
/// resumable subset).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ListImportSessionsRequest {}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ListImportSessionsReply {
    pub sessions: Vec<ImportSessionInfo>,
}

/// Shared request shape for the four state-mutating session kinds
/// (`pause` / `resume` / `cancel` / `finalize`). State machine per
/// `mail-export.md:132` (mirrored): `running` ↔ `paused`;
/// `running`/`paused` → `cancelled`; `running` → `completed`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ImportSessionActionRequest {
    pub session_id: String,
}

/// Request for `fauna.bridges.fail_import_session` — the client reports a
/// session-fatal source-side condition (auth fail, error budget blown);
/// nest records `errored` + reason and emits `BridgeImportError`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FailImportSessionRequest {
    pub session_id: String,
    pub reason: String,
}

/// Reply for all five state-mutating session kinds: the post-transition row.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ImportSessionActionReply {
    pub session: ImportSessionInfo,
}

/// `fauna.bridges.push.import_progress` — emitted to the importer's own
/// connected clients after each `import_message` / `import_message_batch`
/// call (`mailbox-migration.md` § Progress lives nest-side).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BridgeImportProgressPush {
    pub session_id: String,
    pub imported_count: u64,
    pub skipped_count: u64,
    pub errored_count: u64,
}

/// `fauna.bridges.push.import_error` — session-fatal error recorded
/// (`fail_import_session`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BridgeImportErrorPush {
    pub session_id: String,
    pub reason: String,
}

/// `fauna.bridges.push.import_complete` — session finalized; counters are
/// the end-of-import summary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BridgeImportCompletePush {
    pub session_id: String,
    pub imported_count: u64,
    pub skipped_count: u64,
    pub errored_count: u64,
}

// ── Mailbox-export types (`mail-export.md` § Wire shapes) ────────────
//
// The export surface is the read-out twin of the import block above: ten
// User-class, caller-scoped kinds over which the user's own Fauna app pulls
// its mail down as ciphertext, converts it locally, and pushes sealed chunks
// back up for the nest to concatenate. Field conventions are the import
// block's, because § Architectural rules requires the two to stay symmetric.
//
// **The nest is a chunk relay and nothing more** (§ Export pipeline, ratified
// unconditional 2026-07-23). It never sees a plaintext mail byte during an
// export, never holds the per-session key, and never parses a frame of the
// blob it is appending to — so nothing in this block carries plaintext, a
// key, or an offset into the blob that the nest would have to interpret.

/// Request for `fauna.bridges.list_own_mailboxes` — the scope step's options
/// (§ UX shape step 2).
///
/// **Deliberately empty, and deliberately a distinct kind from the MDA-only
/// `fauna.bridges.list_mailboxes`** (ratified 2026-09-20, `mail-export.md`
/// § Wire shapes): that kind takes a target actor because an MDA acts *for* a
/// credential whose actor it was handed, while a User-class kind derives the
/// owner from the authenticated caller and must offer no way to name another.
/// Widening the one kind would put both actor-derivation paths behind a single
/// name — exactly what the import twin refused when it made `import_message`
/// distinct from the MDA-only `append`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ListOwnMailboxesRequest {}

/// The caller's own mailboxes, ascending by name's raw bytes — the same total
/// order § Container shape's determinism contract emits entries in, so the
/// wizard's checklist and the archive agree without the client re-sorting.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ListOwnMailboxesReply {
    pub mailboxes: Vec<ExportMailboxEntry>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4). This kind is APP-callable,
    /// so it is client↔nest wire and rule 4's in-image `strict` opt-out
    /// does not reach it: within a major, an older peer must tolerate a
    /// newer one's added field in BOTH directions
    /// (version-compatibility.md § I2).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One row of the scope step's checklist: the name plus the count the wizard
/// shows beside it. No UID state — the export's cursor is per
/// `fetch_export_chunk_ciphertext`, not per listing.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ExportMailboxEntry {
    pub name: String,
    /// Live message count — the wizard's per-mailbox "n messages".
    pub exists: u32,
    /// The mailbox's IMAP `UIDVALIDITY`. Additive (2026-09-21, with the client
    /// drive loop): the EML-zip manifest records `imap_uidvalidity` per message
    /// (`fauna-mail/src/export/eml.rs`), and the down-leg's per-message reply
    /// carries only the UID — so without this the archive would either omit the
    /// field or record a `0` that is not the mailbox's validity. It is per
    /// mailbox, not per message, which is why it rides the listing rather than
    /// `ExportCiphertextMessage`.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub uid_validity: u32,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4). This kind is APP-callable,
    /// so it is client↔nest wire and rule 4's in-image `strict` opt-out
    /// does not reach it: within a major, an older peer must tolerate a
    /// newer one's added field in BOTH directions
    /// (version-compatibility.md § I2).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Request for `fauna.bridges.start_export_session` — the wizard's durable
/// commit point (§ UX shape step 3). **No target actor**: § Cross-actor
/// isolation forbids exporting another user's mail, and the shape of the
/// request is the first place that is enforced.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct StartExportSessionRequest {
    /// `mbox` / `maildir` / `eml-zip` — § Session row model's `format`, and
    /// the `format` half of every frame's AAD (§ Blob shape on disk), which is
    /// what makes a format substitution fail to open rather than mislead.
    pub format: String,
    /// DAG-CBOR blob recording mailbox selection, date range and the
    /// header-strip flag (§ Session row model). **Opaque to the nest**, which
    /// stores it verbatim and hands it back — the client that wrote it is the
    /// only reader, so the selection's shape can evolve without a wire change.
    #[serde(with = "serde_bytes")]
    pub scope_descriptor: Vec<u8>,
    /// The client-minted per-session key, wrapped under the user's actor key
    /// (§ Key material). Stored verbatim so **any** of the user's clients can
    /// fetch and unwrap it — not just the one that started the session.
    ///
    /// Optional on the wire so the column can carry a session opened by a
    /// client that mints the key later; a session that never supplies one has
    /// produced a blob no client can open, which the handler refuses rather
    /// than storing (`bridge_export_handlers.rs`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrapped_session_key: Option<ByteBuf>,
    /// Client-side estimate from enumerating the selected mailboxes; revised
    /// later via [`UploadExportChunkRequest::revised_total_count`], exactly as
    /// the import twin revises its own.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub total_count: u64,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4). This kind is APP-callable,
    /// so it is client↔nest wire and rule 4's in-image `strict` opt-out
    /// does not reach it: within a major, an older peer must tolerate a
    /// newer one's added field in BOTH directions
    /// (version-compatibility.md § I2).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

fn is_zero_u64(v: &u64) -> bool {
    *v == 0
}

fn is_zero_i64(v: &i64) -> bool {
    *v == 0
}

fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct StartExportSessionReply {
    /// Nest-generated session UUID (text form).
    pub session_id: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4). This kind is APP-callable,
    /// so it is client↔nest wire and rule 4's in-image `strict` opt-out
    /// does not reach it: within a major, an older peer must tolerate a
    /// newer one's added field in BOTH directions
    /// (version-compatibility.md § I2).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Wire view of one `export_sessions` row (§ Session row model).
///
/// ⚠ **`blob_path` is deliberately absent.** § Don't do these forbids exposing
/// the blob's filesystem path in any user-facing surface; the client's handle
/// on the bytes is [`Self::download_url`], and the path stays nest-internal.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ExportSessionInfo {
    pub session_id: String,
    pub format: String,
    /// `running` / `paused` / `errored` / `completed` / `cancelled`.
    pub state: String,
    pub started_at: i64,
    pub last_progress_at: i64,
    pub total_count: u64,
    pub exported_count: u64,
    pub skipped_count: u64,
    pub errored_count: u64,
    /// Resume cursor — the client's own opaque marker for where it stopped.
    pub last_processed_message_id: String,
    /// Session-fatal reason when `state == "errored"`.
    pub error_reason: String,
    /// Sealed bytes accepted into the blob so far; the finalized size once the
    /// session completes (§ Session row model).
    pub blob_bytes: u64,
    /// 30 days after `last_progress_at` (§ Expiry).
    pub expires_at: i64,
    /// The scope the wizard committed, handed back verbatim — the export twin
    /// of § Resume protocol: a session resumed after a client restart has no
    /// other way to learn which mailboxes it was exporting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_descriptor: Option<ByteBuf>,
    /// The wrapped per-session key (§ Key material), so a *second* client of
    /// the same user can open a download the first one's session produced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrapped_session_key: Option<ByteBuf>,
    /// Actor-authenticated download path, present once the blob is openable.
    /// Empty until the session completes — the wire half of § Architectural
    /// rules' no-partial-blob-download rule.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub download_url: String,
    /// The next chunk index the nest will accept (§ Blob shape on disk — the
    /// blob is frames in `chunk_idx` order). A resuming client reads this
    /// instead of guessing where its predecessor stopped.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub next_chunk_idx: u64,
    /// The session's current stream generation (`mail-export.md` § Resume): 0
    /// at start, + 1 per `restart_export_session`. A driver sends the
    /// generation it drives on every call, and the nest refuses a stale one —
    /// so the client that restarts a stream reads its generation from here.
    /// Additive; absent means 0, which is every fresh session.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub stream_generation: u64,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4). This kind is APP-callable,
    /// so it is client↔nest wire and rule 4's in-image `strict` opt-out
    /// does not reach it: within a major, an older peer must tolerate a
    /// newer one's added field in BOTH directions
    /// (version-compatibility.md § I2).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Request for `fauna.bridges.list_export_sessions` — the resume list on
/// client restart. Caller-scoped; takes no actor.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ListExportSessionsRequest {}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ListExportSessionsReply {
    pub sessions: Vec<ExportSessionInfo>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4). This kind is APP-callable,
    /// so it is client↔nest wire and rule 4's in-image `strict` opt-out
    /// does not reach it: within a major, an older peer must tolerate a
    /// newer one's added field in BOTH directions
    /// (version-compatibility.md § I2).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Shared request shape for the state-mutating session kinds that carry
/// nothing but the session (`pause` / `resume` / `cancel` / `finalize` /
/// `discard`). `fail_export_session` has its own request type: it carries a
/// reason too ([`FailExportSessionRequest`]).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ExportSessionActionRequest {
    pub session_id: String,
    /// **Present = "only while I am still the driver"** (`mail-export.md`
    /// § Resume): the transition applies only if this is the session's current
    /// stream generation, and answers [`EXPORT_STREAM_SUPERSEDED`] otherwise.
    /// The drive loop's own pause / resume / finalize set it (its failure path
    /// sets the twin field on [`FailExportSessionRequest`]), so a driver another
    /// device has restarted over can never pause, finish, fail or cancel the
    /// stream that replaced its own. **Absent = unconditional**:
    /// the user's own Cancel disposes of an export from any device.
    ///
    /// ⚠ `Some(0)` is serialized, deliberately — absent and zero mean different
    /// things here, unlike every sibling field's absent-means-zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_generation: Option<u64>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4). This kind is APP-callable,
    /// so it is client↔nest wire and rule 4's in-image `strict` opt-out
    /// does not reach it: within a major, an older peer must tolerate a
    /// newer one's added field in BOTH directions
    /// (version-compatibility.md § I2).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The typed refusal a stale stream generation is answered with — on
/// `upload_export_chunk` always, and on a session transition that carried
/// [`ExportSessionActionRequest::stream_generation`]. A client that sees it
/// drops its run and touches nothing else: the session now belongs to whichever
/// of the user's devices restarted it (`mail-export.md` § Resume).
pub const EXPORT_STREAM_SUPERSEDED: &str = "fauna.bridges.export_stream_superseded";

/// Request for `fauna.bridges.restart_export_session` — the **cold** resume
/// (`mail-export.md` § Resume): open a new stream generation on a session whose
/// stream this client cannot continue, because the app that held the encoder
/// has exited or is another device.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RestartExportSessionRequest {
    pub session_id: String,
    /// A **fresh** per-session key, minted and wrapped by the restarting
    /// client exactly as `start_export_session`'s is; it replaces the row's.
    /// One key per generation is what makes a frame of the abandoned stream
    /// fail to open inside the new one rather than pass AEAD at a matching
    /// index. Optional on the wire for the same reason as
    /// [`StartExportSessionRequest::wrapped_session_key`], and refused when
    /// missing for the same reason too.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrapped_session_key: Option<ByteBuf>,
    /// The restarting client's own estimate from the current listing — the
    /// mailboxes may have grown since the first stream began. 0 keeps the
    /// row's.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub total_count: u64,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4). This kind is APP-callable,
    /// so it is client↔nest wire and rule 4's in-image `strict` opt-out
    /// does not reach it: within a major, an older peer must tolerate a
    /// newer one's added field in BOTH directions
    /// (version-compatibility.md § I2).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Request for `fauna.bridges.fail_export_session` — the client reports a
/// condition that is fatal to the whole export (a record its account's keys
/// cannot open, a serializer refusal, a seal failure). The nest records
/// `errored` with the reason, unlinks the partial blob and emits
/// `BridgeExportError`, so the user's *other* devices — and this one after a
/// restart — read what happened instead of a bare `cancelled`
/// (`mail-export.md` § Resume).
///
/// The export twin of [`FailImportSessionRequest`], with the one thing the
/// import side has no use for: § Resume's only-while-I-am-still-the-driver
/// condition. Only a driver ever fails a session, and a failure that is one
/// device's alone (a dropped fetch) may land after another device restarted
/// the export — so it must not dispose of the stream that replaced its own.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FailExportSessionRequest {
    pub session_id: String,
    /// Why the export died, in the client's own words — it owns the
    /// conversion, so it owns the classification. Empty is refused: an
    /// `errored` row whose reason says nothing is the `cancelled` row this
    /// kind exists to improve on.
    pub reason: String,
    /// Same meaning as [`ExportSessionActionRequest::stream_generation`]:
    /// present = apply only while that generation is current, and answer
    /// [`EXPORT_STREAM_SUPERSEDED`] otherwise; absent = unconditional. The
    /// drive loop always sets it.
    ///
    /// ⚠ `Some(0)` is serialized, deliberately — absent and zero mean different
    /// things here, unlike every sibling field's absent-means-zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_generation: Option<u64>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4). This kind is APP-callable,
    /// so it is client↔nest wire and rule 4's in-image `strict` opt-out
    /// does not reach it: within a major, an older peer must tolerate a
    /// newer one's added field in BOTH directions
    /// (version-compatibility.md § I2).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply for the five state-mutating session kinds: the post-transition row.
/// (`discard_export_blob` answers [`DiscardExportBlobReply`] — its row is gone,
/// so there is nothing left to render.)
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ExportSessionActionReply {
    pub session: ExportSessionInfo,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4). This kind is APP-callable,
    /// so it is client↔nest wire and rule 4's in-image `strict` opt-out
    /// does not reach it: within a major, an older peer must tolerate a
    /// newer one's added field in BOTH directions
    /// (version-compatibility.md § I2).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply for `fauna.bridges.discard_export_blob` — the row and its blob are
/// gone. `existed = false` is the second discard of the same session, which is
/// the same answer and deliberately not an error: the operation is idempotent
/// from the client's side.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct DiscardExportBlobReply {
    pub existed: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4). This kind is APP-callable,
    /// so it is client↔nest wire and rule 4's in-image `strict` opt-out
    /// does not reach it: within a major, an older peer must tolerate a
    /// newer one's added field in BOTH directions
    /// (version-compatibility.md § I2).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Request for `fauna.bridges.fetch_export_chunk_ciphertext` — the pipeline's
/// **down-leg** (§ Export pipeline). Pages one mailbox at a time by ascending
/// UID, because that is the order § Container shape's determinism contract
/// writes entries in: a client walking the cursor is emitting the archive in
/// its final order without buffering it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FetchExportChunkCiphertextRequest {
    pub session_id: String,
    /// The mailbox being walked. The client picks it from its own
    /// `scope_descriptor`; the nest only enforces that the caller owns it.
    pub mailbox: String,
    /// Exclusive UID cursor — 0 starts the mailbox.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub after_uid: u32,
    /// Per-call ceilings. Clamped by the nest to [`MAX_EXPORT_FETCH_MESSAGES`]
    /// / [`MAX_EXPORT_FETCH_BYTES`]; 0 means "the nest's ceiling".
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub max_messages: u32,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub max_bytes: u64,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4). This kind is APP-callable,
    /// so it is client↔nest wire and rule 4's in-image `strict` opt-out
    /// does not reach it: within a major, an older peer must tolerate a
    /// newer one's added field in BOTH directions
    /// (version-compatibility.md § I2).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One message on the down-leg: the **sealed** record exactly as it rests,
/// plus the per-message metadata the serializers need
/// (`libs/fauna-mail/src/export/`). The nest holds no key, so nothing here is
/// openable by it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ExportCiphertextMessage {
    pub mailbox: String,
    pub uid: u32,
    /// The 32-byte message id (the record cid's digest tail).
    #[serde(with = "serde_bytes")]
    pub message_id: Vec<u8>,
    /// Canonical IMAP flag tokens — what Maildir++ encodes into its filename
    /// suffix and mbox into its `Status:` line.
    pub flags: Vec<String>,
    /// INTERNALDATE, epoch seconds — the entry mtime § Container shape's
    /// determinism contract stores, and the mbox `From ` line's date.
    pub internal_date: i64,
    /// Unix **seconds** this nest stored (= sealed) the record — the
    /// `segment_records.stored_at` mirror, exactly as `inbox.fetch`'s
    /// `InboxMessage::stored_at` projects it. It is what the client's record
    /// opener picks epoch keys by: INTERNALDATE is the message's own date,
    /// which for imported mail (and any mail delivered in a later mail epoch
    /// than it is dated) names an epoch the record was never sealed in, and
    /// the epoch chain scans only backward from its target. Additive; `0` =
    /// unknown (a failed clock read), and the client then falls back to
    /// INTERNALDATE.
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub stored_at: i64,
    /// The sealed body, inline. Empty **iff** [`Self::body_ref`] is set.
    #[serde(with = "serde_bytes")]
    pub sealed_body: Vec<u8>,
    pub ciphertext_size: u32,
    /// Set when the sealed body is too large for the RPC frame: the nest has
    /// staged it on the bulk-byte plane and the client GETs the chunks back —
    /// the same escape `fetch_message_ciphertext` uses, and the only reason a
    /// mailbox holding a large attachment can be exported at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_ref: Option<MailBodyRef>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4). This kind is APP-callable,
    /// so it is client↔nest wire and rule 4's in-image `strict` opt-out
    /// does not reach it: within a major, an older peer must tolerate a
    /// newer one's added field in BOTH directions
    /// (version-compatibility.md § I2).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FetchExportChunkCiphertextReply {
    pub messages: Vec<ExportCiphertextMessage>,
    /// The UID to pass as `after_uid` next. Equal to the request's when the
    /// page came back empty.
    pub next_after_uid: u32,
    /// True when this page reached the end of `mailbox` — the client moves on
    /// to the next mailbox in its scope. A client that inferred the end from an
    /// empty page instead would stop early on a UID gap.
    pub mailbox_done: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4). This kind is APP-callable,
    /// so it is client↔nest wire and rule 4's in-image `strict` opt-out
    /// does not reach it: within a major, an older peer must tolerate a
    /// newer one's added field in BOTH directions
    /// (version-compatibility.md § I2).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Request for `fauna.bridges.upload_export_chunk` — the pipeline's **up-leg**.
///
/// One call does two things that must not drift apart: it appends the sealed
/// frame and it folds that batch's counters into the session. They are one RPC
/// because the progress push § Session row model promises is "after each
/// batch", and because a separate progress kind would be a write the nest
/// accepts against a session it is not appending to.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct UploadExportChunkRequest {
    pub session_id: String,
    /// The frame's index (§ Blob shape on disk). The nest refuses anything but
    /// the session's `next_chunk_idx`, so a reordered or duplicated upload is
    /// a typed refusal here rather than an archive that fails to open later.
    pub chunk_idx: u64,
    /// The sealed frame bytes — `len || nonce || ciphertext`, already framed
    /// and sealed by the client. The nest appends them verbatim and parses
    /// nothing (§ Blob shape on disk).
    #[serde(with = "serde_bytes")]
    pub sealed_chunk: Vec<u8>,
    /// Counters for the messages this frame carried.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub exported_delta: u64,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub skipped_delta: u64,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub errored_delta: u64,
    /// The client's resume marker after this frame.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub last_processed_message_id: String,
    /// Revised estimate as enumeration finishes (the import twin's field).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revised_total_count: Option<u64>,
    /// The stream generation this frame belongs to (`mail-export.md`
    /// § Resume). The nest refuses any but the session's current one with
    /// [`EXPORT_STREAM_SUPERSEDED`], **before** it looks at `chunk_idx` — a
    /// driver that has been restarted over must hear "you are not the driver",
    /// never "resume from chunk n". Additive; absent means 0.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub stream_generation: u64,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4). This kind is APP-callable,
    /// so it is client↔nest wire and rule 4's in-image `strict` opt-out
    /// does not reach it: within a major, an older peer must tolerate a
    /// newer one's added field in BOTH directions
    /// (version-compatibility.md § I2).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct UploadExportChunkReply {
    /// Sealed bytes now resting in the blob, after this append.
    pub blob_bytes: u64,
    /// The next index the nest will accept — `chunk_idx + 1` on success.
    pub next_chunk_idx: u64,
    pub exported_count: u64,
    pub skipped_count: u64,
    pub errored_count: u64,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4). This kind is APP-callable,
    /// so it is client↔nest wire and rule 4's in-image `strict` opt-out
    /// does not reach it: within a major, an older peer must tolerate a
    /// newer one's added field in BOTH directions
    /// (version-compatibility.md § I2).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Per-call ceilings for the down-leg, owned here so the handler and the
/// client's pager enforce one number rather than two that happen to agree —
/// the same reason `MAX_BATCH_MESSAGES` lives beside the import types.
/// § Export pipeline's "16 MiB chunks" is the up-leg figure; the down-leg is
/// sized to the same budget so one fetch feeds roughly one chunk.
pub const MAX_EXPORT_FETCH_MESSAGES: u32 = 256;
pub const MAX_EXPORT_FETCH_BYTES: u64 = 16 * 1024 * 1024;

/// `fauna.bridges.push.export_progress` — emitted to the exporter's own
/// connected clients after each accepted `upload_export_chunk`
/// (§ Session row model).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BridgeExportProgressPush {
    pub session_id: String,
    pub exported_count: u64,
    pub skipped_count: u64,
    pub errored_count: u64,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4). This kind is APP-callable,
    /// so it is client↔nest wire and rule 4's in-image `strict` opt-out
    /// does not reach it: within a major, an older peer must tolerate a
    /// newer one's added field in BOTH directions
    /// (version-compatibility.md § I2).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.bridges.push.export_error` — session-fatal condition recorded.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BridgeExportErrorPush {
    pub session_id: String,
    pub reason: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4). This kind is APP-callable,
    /// so it is client↔nest wire and rule 4's in-image `strict` opt-out
    /// does not reach it: within a major, an older peer must tolerate a
    /// newer one's added field in BOTH directions
    /// (version-compatibility.md § I2).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.bridges.push.export_complete` — the session finalized and its blob
/// carries a terminator frame, so the download is openable (§ Blob shape on
/// disk). `download_url` is the actor-authenticated route; `blob_bytes` lets
/// the client show a size before it starts the transfer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BridgeExportCompletePush {
    pub session_id: String,
    pub download_url: String,
    pub blob_bytes: u64,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4). This kind is APP-callable,
    /// so it is client↔nest wire and rule 4's in-image `strict` opt-out
    /// does not reach it: within a major, an older peer must tolerate a
    /// newer one's added field in BOTH directions
    /// (version-compatibility.md § I2).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── I2b Phase-C.7 search types ───────────────────────────────────

/// IMAP SEARCH header axis: which `*_norm` column to substring-match.
/// All four are populated case-folded ASCII (lowercased, NFKC-fold);
/// `HeaderContains.value` is lowercased the same way by the handler
/// so the substring match is case-insensitive on the wire.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HeaderField {
    From,
    To,
    Cc,
    Subject,
}

/// One conjunctive predicate in a SEARCH request. The handler ANDs
/// every term; OR / NOT is handled MDA-side (Phase C scope-reduce
/// rejects them at the IMAP layer per `imap-server.md` § SEARCH).
///
/// `HasFlag` / `LacksFlag` match against the space-separated `flags`
/// column (canonical IMAP token form: `\Seen`, `\Flagged`, keywords).
/// `HeaderContains` runs case-folded substring against the
/// corresponding `*_norm` column. Date predicates compare against
/// `internal_date` (epoch seconds). Size predicates compare against the
/// record block length looked up by `segment_records.record_cid` through
/// the CARv2 index (`imap-server.md` § SEARCH), not a SQL byte column.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SearchTerm {
    HasFlag {
        flag: String,
    },
    LacksFlag {
        flag: String,
    },
    HeaderContains {
        field: HeaderField,
        value: String,
    },
    /// Inclusive: `internal_date >= ts`.
    SinceInternalDate {
        ts: i64,
    },
    /// Exclusive: `internal_date < ts`.
    BeforeInternalDate {
        ts: i64,
    },
    /// Strict: `ciphertext_size > size`.
    Larger {
        size: u32,
    },
    /// Strict: `ciphertext_size < size`.
    Smaller {
        size: u32,
    },
}

/// Request for `fauna.bridges.search_messages`. Returns UIDs in
/// ascending order that match the conjunction of every term. An
/// empty `terms` Vec returns every UID in the mailbox (callers that
/// don't want that should not issue the RPC).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SearchMessagesRequest {
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    pub mailbox: String,
    pub terms: Vec<SearchTerm>,
}

/// Reply for `fauna.bridges.search_messages`. `uids` is ascending and
/// deduplicated (the SQL `ORDER BY uid ASC` guarantees both, since
/// `(actor_id, mailbox, uid)` is the placement PK).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SearchMessagesReply {
    pub uids: Vec<u32>,
}

// ── I2b Phase-C.8 quota types ────────────────────────────────────

/// Request for `fauna.bridges.get_quota`. The MDA issues one per
/// actor per `GETQUOTA "user/<handle>"` or implicit `GETQUOTAROOT`
/// (RFC 9208). Empty body apart from the actor — limits live on
/// `ImapPolicy` (caller-side cache) but the handler returns them
/// in the reply too so the bridge can serve QUOTA without a
/// concurrent `fetch_config` round-trip.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GetQuotaRequest {
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
}

/// Reply for `fauna.bridges.get_quota`. `storage_bytes_used` is the
/// sum of each placement's record block length (looked up by
/// `segment_records.record_cid` through the CARv2 index, not a SQL byte
/// column — `imap-server.md` § QUOTA) over the actor's mail placements
/// (each placement counts, mirroring Dovecot's QUOTA accounting for
/// COPY-duplicated messages). `message_count_used` is the COUNT of
/// placements. `storage_bytes_limit` /
/// `message_count_limit` come from `ImapPolicy.storage_bytes_default`
/// / `message_count_default` (per-actor tier override is Phase F+).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GetQuotaReply {
    pub storage_bytes_used: u64,
    pub message_count_used: u32,
    pub storage_bytes_limit: u64,
    pub message_count_limit: u32,
}

// ── Spam-training labels + the sealed history op ─────────────────

/// Spam-training label: which side of the Bayesian classifier the
/// caller is feeding.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SpamLabel {
    /// Train the message as spam (`\Junk` flag set, MOVE into Junk,
    /// or first-party "Mark as spam" button).
    Spam,
    /// Train the message as ham (`\Junk` flag cleared, MOVE out of
    /// Junk, or first-party "Mark as not spam" button).
    Ham,
    /// A label a newer nest added that this peer does not know: a neutral
    /// badge, and undo is refused (the label decides which class an undo
    /// decrements). Never written back.
    #[serde(other, skip_serializing)]
    Unknown,
}

/// Provenance of one training event. `mail-spam.md` § Training
/// signal sources uses the same string tags to render the user's
/// training-history list (which lets them filter by "marked in
/// Thunderbird" vs. "marked in the Fauna app").
///
/// String-tagged (`#[serde(rename_all = "snake_case")]` on a unit-variant
/// enum). A source a newer nest adds decodes as [`TrainingSource::Unknown`]
/// (a neutral badge), so the wire shape does not churn.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum TrainingSource {
    /// IMAP STORE +/-FLAGS (\Junk) — Thunderbird's "Mark as junk".
    ImapJunkFlag,
    /// IMAP MOVE / COPY into or out of a `\Junk`-marked mailbox —
    /// Apple Mail's "Move to Junk" gesture.
    ImapJunkMove,
    /// The Fauna app's own "Mark as spam" gesture — the ratified
    /// first-party-button source (`mail-spam.md` § Wire shapes, ruled
    /// 2026-07-08: no separate `explicit_button` variant). Apps display it
    /// as "Fauna app".
    ManualOther,
    /// A source a newer nest added that this peer does not know: a neutral
    /// badge. Never written back.
    #[serde(other, skip_serializing)]
    Unknown,
}

/// The optional training-history mutation a **client-path** `put_spam_model`
/// write carries (`PutSpamModelRequest::history_op`), so the model re-seal and
/// its audit-trail INSERT/DELETE land **atomically** on one kind — option (a) of
/// the co-design (tracked internally; `mail-spam.md` § Training-sample
/// retention). Every train runs at a capability holder (the user's client or the
/// AUTH'd MDA session) — the nest never trains — so the holder seals the delta +
/// subject **to the actor's own recipient key** and ships the row here. Absent
/// (`None`) ⇒ a model-only write (a re-seal with no per-event history). Externally-tagged like [`crate::email::EmailFilterAction`] (a
/// DAG-CBOR-proven data enum). The nest stores the sealed bytes **verbatim /
/// opaque** — it holds only the actor's *public* half, so it can neither read nor
/// seal them (symmetric with the sealed model itself).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SpamHistoryOp {
    /// A client-path train event: insert one audit row. `sealed_subject` and
    /// `sealed_delta` are **client-sealed opaque bytes** (the `wrapped_blob`
    /// shape, sealed to the actor's own recipient key); the nest stores them
    /// verbatim into `spam_training_history.sealed_subject` /
    /// `.model_delta_applied` and cannot decode them. Both must be non-empty,
    /// and a `sealed_delta` that decodes as a plaintext n-gram set is refused
    /// (`invalid_params`). `message_id`, `mailbox`, `label`, `source` are
    /// plaintext metadata (not message content).
    Insert {
        /// The trained message's stable 32-byte content id (the
        /// `bridge_imap_messages` placement key), stored opaque for the row's
        /// reference — not decoded.
        #[serde(with = "serde_bytes")]
        message_id: Vec<u8>,
        /// The mailbox the message was in at train time (`INBOX` / `Junk` / …),
        /// returned separately on the list reply so a client renders
        /// `{unwrapped subject} · {mailbox}` for a sealed row.
        mailbox: String,
        /// The message subject, **sealed to the actor's own recipient key**
        /// (opaque). Stored verbatim into `spam_training_history.sealed_subject`.
        #[serde(with = "serde_bytes")]
        sealed_subject: Vec<u8>,
        /// The event's n-gram delta (`SpamModel::delta_ngrams`), **sealed to the
        /// actor's own recipient key** (opaque). Stored verbatim into
        /// `spam_training_history.model_delta_applied`; the client unwraps it for
        /// a client-side undo (`ModelWriteOp::Undo`).
        #[serde(with = "serde_bytes")]
        sealed_delta: Vec<u8>,
        /// Which side the event trained.
        label: SpamLabel,
        /// Where the training signal came from.
        source: TrainingSource,
    },
    /// A client-path undo: delete one of the caller's own history rows by id.
    /// The client has already applied the inverse delta locally and re-sealed the
    /// model (the `sealed_model` on the same request), so this only removes the
    /// consumed audit row, atomically with the model write — the one undo path
    /// (`mail-spam.md` § Undo). Caller-scoped (`WHERE actor_id = caller`); an id
    /// that isn't the caller's own row matches nothing.
    Delete {
        /// The 16-byte history-event id from a prior `list_spam_training_history`.
        #[serde(with = "serde_bytes")]
        history_id: Vec<u8>,
    },
}

// ── publish_spam_baseline + set_baseline_contribution ──────────────────────────
//
// The admin-opt-in deployment spam baseline (`mail-spam.md` § Cold start,
// Path 2). The admin republishes the deployment-wide baseline by aggregating
// the per-user models of every user who opted in via the per-user
// `set_baseline_contribution` toggle. Baseline publish is off by default, the
// per-user contribution is opt-in (default off), and the merged n-gram weights
// don't identify which user contributed — the user-controls-their-data
// invariant.

/// Request for `fauna.bridges.publish_spam_baseline` — empty (the admin is
/// the WS-RPC caller; the action takes no parameters). Admin-only.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PublishSpamBaselineRequest {
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply for `fauna.bridges.publish_spam_baseline`. The counts are
/// **aggregates** — they summarize the published baseline (how many
/// contributing models were merged and the total training samples behind
/// it) without identifying any individual contributor (`mail-spam.md`
/// § Cold start, Path 2 — "the admin cannot view individual contributions").
/// `contributors` counts opt-in users who had a trained model to merge (an
/// opt-in user with no model yet contributes nothing).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PublishSpamBaselineReply {
    /// Number of contributing per-user models merged into the baseline.
    pub contributors: u32,
    /// Total training samples (`spam + ham` messages) behind the baseline.
    pub sample_count: u32,
    /// Whether a real baseline was published. `false` when the publish was
    /// **withheld** because fewer than the k-anonymity floor
    /// (`fauna_mail::spam::BASELINE_MIN_CONTRIBUTORS`) of contributors opted in
    /// — the served baseline is then empty/withdrawn (`mail-spam.md` § Cold
    /// start Path 2). The admin UI surfaces "not published — too few
    /// contributors" from this flag. The nest always sends it; a consumer
    /// reads it as sent and never infers it from `sample_count`.
    #[serde(default)]
    pub published: bool,
    /// Opted-in contributors with a model row that could NOT be merged this
    /// run (`mail-spam.md` § Encrypted-mode interaction, ratified 2026-07-13):
    /// client-sealed contributors the granted holder didn't merge — holder
    /// unavailable / timed out, no reaching grant+copy, or an undecodable
    /// copy. Surfaced beside the existing "not published — too few
    /// contributors" so the admin sees the erosion instead of a
    /// quietly-shrunken `contributors` count. Still an **aggregate** — never
    /// identifies any contributor. **Forward-compat:** `#[serde(default)]` ⇒
    /// an omitted key decodes 0.
    #[serde(default)]
    pub skipped_contributors: u32,
    /// The run was **deferred** by the delta floor (`mail-spam.md` § Cold start
    /// Path 2 → *The floor applies to every published DELTA*): fewer than
    /// `BASELINE_MIN_CONTRIBUTORS` contributors' contributions changed since
    /// the last served baseline, so nothing was written and the box keeps
    /// serving what it had. A deferred reply carries `published = false` and
    /// `sample_count = 0`. **Forward-compat:** an omitted key
    /// decodes `false`.
    #[serde(default)]
    pub deferred: bool,
    /// Forward-compat catch-all (transport.md rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Request for `fauna.bridges.get_spam_baseline_state` — empty. Admin-only.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct GetSpamBaselineStateRequest {
    /// Forward-compat catch-all (transport.md rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply for `fauna.bridges.get_spam_baseline_state` — the deployment
/// baseline's CURRENT state, never its history (`mail-spam.md` § Cold start
/// Path 2 → *Standing publish*). It deliberately carries **no withdrawal time
/// and no withdrawal reason**: beside an account deleted at the same moment,
/// "withdrawn at T because a contributor left" would tell a small deployment's
/// admin who had been contributing. A withdrawn baseline reads exactly as one
/// never published. Every count is an aggregate.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct GetSpamBaselineStateReply {
    /// A real baseline (at or above the contributor floor) is served now.
    #[serde(default)]
    pub published: bool,
    /// Contributors summed into the served baseline (`0` when none is served).
    #[serde(default)]
    pub contributors: u32,
    /// Training samples behind the served baseline (`0` when none is served).
    #[serde(default)]
    pub sample_count: u32,
    /// When the served baseline was built, epoch milliseconds; `None` when
    /// none is served.
    #[serde(default)]
    pub published_at: Option<i64>,
    /// The last run's opted-in contributors it could not merge.
    #[serde(default)]
    pub skipped_contributors: u32,
    /// The last run was deferred by the delta floor ("waiting for more
    /// contributor activity").
    #[serde(default)]
    pub deferred: bool,
    /// Standing publish is on (the effective `baseline_standing_publish`).
    #[serde(default)]
    pub standing: bool,
    /// Forward-compat catch-all (transport.md rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Request for `fauna.bridges.set_baseline_contribution` — the per-user
/// opt-in toggle (`mail-spam-contribute-baseline-toggle`). Caller-scoped:
/// the WS-RPC connection's actor is the subject (no `actor_id` field — a
/// user sets only their own contribution flag, like `reset_spam_model`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SetBaselineContributionRequest {
    /// Whether to opt this actor's training into the deployment baseline.
    pub contribute: bool,
    /// Forward-compat catch-all (transport.md rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply for `fauna.bridges.set_baseline_contribution` — echoes the
/// resulting persisted flag (mirrors `fauna.spam.set_preferences`, which
/// echoes the resulting state rather than a bare status).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SetBaselineContributionReply {
    /// The actor's contribution flag after the update.
    pub contribute: bool,
    /// Forward-compat catch-all (transport.md rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── reset_spam_model / list_spam_training_history ──────────────────────────────
//
// The user-tier training-management RPCs that light up the `mail-spam` page
// (`mail-spam.md` §§ Reset, Training-sample retention, Undo, Wire shapes). Both
// are **caller-scoped**: the WS-RPC connection's actor is the subject (no
// `actor_id` field — a user manages only their own model + history, like
// `set_baseline_contribution`; § Cross-actor isolation). The two
// `BridgeSpamModel{Updated,Reset}` push events (below) tell the user's *other*
// open clients to refresh the history list.

/// Request for `fauna.bridges.reset_spam_model` — delete the caller's per-user
/// model + all their training history. Caller-scoped; no parameters. The reset
/// is irreversible (`mail-spam.md` § Reset).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ResetSpamModelRequest {
    /// Forward-compat catch-all (transport.md rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply for `fauna.bridges.reset_spam_model`. Bare ack — reset is idempotent
/// (deleting an already-absent model/history is a no-op).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ResetSpamModelReply {
    /// Forward-compat catch-all (transport.md rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One training-history row as `fauna.bridges.list_spam_training_history`
/// returns it — the `mail-spam-training-history-list` row shape (`mail-spam.md`
/// § Training-sample retention). The localized label/source **badges** are
/// rendered client-side from `label`/`source` (the shared
/// `fauna_client_mail_settings::{training_label_badge,training_source_badge}`
/// maps); this wire row carries the raw enums, not localized text.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SpamTrainingHistoryRow {
    /// 16-byte history-event id (UUID); the undo cursor.
    #[serde(with = "serde_bytes")]
    pub history_id: Vec<u8>,
    /// The mailbox the message was in at train time
    /// (`mail-spam-training-history-list-item-message`). The subject rests sealed
    /// (`sealed_subject`), so the nest cannot format it: a client renders
    /// `{unwrapped `sealed_subject`} · {`mailbox`}` from the two fields below.
    pub message: String,
    /// The trained message's subject **sealed to the actor's own recipient key**
    /// (the `wrapped_blob` shape) — every row carries one, the nest refuses a
    /// history insert without it. A client unwraps it under its own key and
    /// renders `{unwrapped subject} · {mailbox}` (`mail-spam.md` § Training-sample
    /// retention). Caller-scoped like the whole reply, so returning the caller's
    /// own sealed subject is no new exposure.
    #[serde(with = "serde_bytes", default)]
    pub sealed_subject: Vec<u8>,
    /// The mailbox the message was in at train time (`INBOX` / `Junk` / …),
    /// returned separately so a client can compose the row display
    /// (`{unwrapped subject} · {mailbox}`). `#[serde(default)]` ⇒ empty when
    /// absent (the client falls back to `message`).
    #[serde(default)]
    pub mailbox: String,
    /// Which side the event trained (`…-item-label`).
    pub label: SpamLabel,
    /// Where the training signal came from (`…-item-source`).
    pub source: TrainingSource,
    /// Epoch-millis the event was applied (`…-item-created-at`).
    pub created_at_ms: i64,
    /// The event's stored `spam_training_history.model_delta_applied` — the
    /// distinct n-gram set the event added, the exact forward delta the client
    /// replays in inverse for a client-side undo (`ModelWriteOp::Undo`;
    /// `libs/fauna-mail/src/spam/model_write.rs`, co-design § 3). Returned as the
    /// **stored bytes verbatim** — always an **opaque sealed** `wrapped_blob`
    /// (the client unwraps it under its own recipient key; the nest refuses a
    /// plaintext delta at write). Caller-scoped like the whole reply (the
    /// caller's own history). `#[serde(default)]` ⇒ an omitted key decodes as
    /// empty. `mail-spam.md` §§ Training-sample retention, Undo.
    #[serde(with = "serde_bytes", default)]
    pub model_delta_applied: Vec<u8>,
    /// Forward-compat catch-all (transport.md rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Request for `fauna.bridges.list_spam_training_history`. Caller-scoped (the
/// connection's actor); paginated newest-first (`mail-spam.md` § Undo — "the
/// most recent N rows").
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ListSpamTrainingHistoryRequest {
    /// Max rows to return. `None` ⇒ the nest default (100); the nest also caps
    /// the value so a client can't request an unbounded page.
    pub limit: Option<u32>,
    /// Keyset cursor: return only rows strictly older than this `history_id`
    /// (for "load more"). `None` ⇒ the newest page. An unknown/GC'd cursor
    /// yields an empty page (the client refetches from the start).
    pub before_history_id: Option<ByteBuf>,
    /// Forward-compat catch-all (transport.md rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply for `fauna.bridges.list_spam_training_history` — the rows plus the
/// per-user baseline-contribution read-back (`mail-spam-contribute-baseline-toggle`
/// renders its state from this; the setter is `set_baseline_contribution`,
/// which had no read path until this RPC — `mail-spam.md` § Wire shapes).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ListSpamTrainingHistoryReply {
    /// Newest-first training-history rows for the caller.
    pub events: Vec<SpamTrainingHistoryRow>,
    /// Whether this actor opts its training into the deployment baseline.
    pub contribute_baseline: bool,
    /// Forward-compat catch-all (transport.md rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.bridges.push.spam_model_updated` — the caller's training history
/// changed (a `put_spam_model` carrying a history insert or delete). Routed to every
/// connected client of `actor_id` so the user's *other* open `mail-spam` pages
/// refresh their training-history list (`mail-spam.md` §§ Training signal
/// sources, Undo). Best-effort; the page also refreshes on its own load.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BridgeSpamModelUpdatedPush {
    /// The actor whose model changed (== the connection's actor; carried so a
    /// multiplexed client can route the refresh).
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
}

/// `fauna.bridges.push.spam_model_reset` — the caller reset their per-user spam
/// model (model + all training history deleted). Routed to every connected
/// client of `actor_id` so other open surfaces clear their history list
/// (`mail-spam.md` § Reset).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BridgeSpamModelResetPush {
    /// The actor whose model was reset (== the connection's actor).
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
}

// ── I5 Phase D.6 — IMAP CREATE / DELETE / RENAME ───────────────────────────────

/// Request for `fauna.bridges.create_mailbox`. CREATE the user-named
/// mailbox under the caller actor; nest allocates a fresh
/// `uid_validity = (now_unix_millis << 16) | rand16()` per RFC 9051
/// §2.3.1.1, seeds `uid_next = 1`, `highestmodseq = 1`. Standard
/// (`\Inbox` / `\Archive` / `\Drafts` / `\Sent` / `\Trash` / `\Junk`)
/// names are reserved — they auto-exist; CREATE on any of them
/// returns `Reserved`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CreateMailboxRequest {
    /// 32-byte actor identifier.
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    /// Mailbox name as the MUA sent it on the IMAP wire (post-
    /// modified-UTF-7 decode, which is the bridge's responsibility —
    /// nest receives UTF-8). Validated per RFC 9051 §5.1.
    pub name: String,
}

/// Outcome of one `create_mailbox` call. Outcome-tagged
/// (`#[serde(tag = "outcome")]`) so future variants land additively.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum CreateMailboxReply {
    /// Mailbox row inserted; carries the freshly-allocated
    /// `uid_validity` so the bridge can report it on the next SELECT
    /// without a round-trip.
    Created { uid_validity: u32 },
    /// A mailbox with this name already exists for the actor (could
    /// be user-created or one of the six standard mailboxes auto-
    /// seeded by `ensure_bridge_imap_mailboxes`).
    AlreadyExists,
    /// Attempted to create one of the six standard mailboxes
    /// (`INBOX` / `Archive` / `Drafts` / `Sent` / `Trash` / `Junk`).
    /// Distinct from `AlreadyExists` so the bridge can surface a
    /// reserved-name error message even if the row didn't pre-exist
    /// (defensive — `ensure_bridge_imap_mailboxes` is always called
    /// on first AUTH, but a future deferred-seed path would still
    /// land on this variant first).
    Reserved,
    /// Name violates RFC 9051 §5.1: 8-bit byte, NUL, length > 255,
    /// or the canonical path separator `/` at the head/tail.
    InvalidName {
        /// Short human-readable cause (`"empty"`, `"too long"`, …).
        /// Surfaces verbatim in the IMAP `BAD` response text.
        reason: String,
    },
}

/// Request for `fauna.bridges.delete_mailbox`. DELETE the named
/// mailbox; under the default `mail.imap.delete_nonempty = forbidden`
/// policy a non-empty mailbox is rejected with `NotEmpty`. Under
/// `allowed` the messages are tombstoned via the EXPUNGE-equivalent
/// path first. Standard mailboxes always reject with `Reserved`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct DeleteMailboxRequest {
    /// 32-byte actor identifier.
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    /// Mailbox name as the MUA sent it.
    pub name: String,
}

/// Outcome of one `delete_mailbox` call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum DeleteMailboxReply {
    /// Mailbox row deleted. Under `delete_nonempty = allowed`, all
    /// contained messages were tombstoned in the same transaction.
    Deleted,
    /// No mailbox with this name exists for the actor.
    NoSuchMailbox,
    /// Attempted to delete one of the six standard mailboxes.
    Reserved,
    /// `delete_nonempty = forbidden` policy + the mailbox contains
    /// at least one un-expunged message. The caller may either ask
    /// the user to EXPUNGE first or, with admin opt-in, switch
    /// the policy to `allowed`.
    NotEmpty,
}

/// Request for `fauna.bridges.rename_mailbox`. RENAME the named
/// mailbox (and all dependent message rows) in one transaction;
/// `uid_validity` is preserved (RFC 9051 §6.3.6) so MUAs don't need
/// to re-sync. The INBOX special-case (§6.3.6) moves INBOX contents
/// to `new_name` (a fresh mailbox) and re-seeds an empty INBOX with
/// a new `uid_validity` and `uid_next = 1`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RenameMailboxRequest {
    /// 32-byte actor identifier.
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    /// Existing mailbox name. `INBOX` triggers the §6.3.6 special-
    /// case (move contents → new mailbox, re-seed empty INBOX).
    pub old_name: String,
    /// Target mailbox name. Validated per RFC 9051 §5.1.
    pub new_name: String,
}

/// Outcome of one `rename_mailbox` call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum RenameMailboxReply {
    /// Mailbox renamed; `uid_validity` preserved for non-INBOX.
    /// INBOX-rename re-seeded an empty INBOX with a fresh
    /// `uid_validity` (returned only for bridge debug logs — the
    /// MDA does not surface it to the MUA; `OK` is enough per
    /// RFC 9051 §6.3.6).
    Renamed,
    /// `old_name` does not exist for the actor.
    NoSuchSource,
    /// Attempted to rename one of the six standard mailboxes other
    /// than INBOX. INBOX itself is renamable per §6.3.6's special-
    /// case.
    ReservedSource,
    /// `new_name` is one of the six standard mailboxes (other than
    /// the renamed INBOX target, which is always a fresh user
    /// mailbox, never a reserved name — guarded by the wire layer
    /// rejecting `new_name = INBOX` separately). Distinct from
    /// `TargetExists` so the bridge can surface a reserved-name
    /// error message per goal-doc `imap-server.md` Write surface
    /// (RENAME row: "Target name reserved or already exists →
    /// same as CREATE" which itself distinguishes Reserved vs.
    /// AlreadyExists).
    TargetReserved,
    /// `new_name` already exists for the actor (user-created).
    TargetExists,
    /// `new_name` violates RFC 9051 §5.1.
    InvalidName {
        /// Short human-readable cause.
        reason: String,
    },
}

// ── I5 Phase D.7 — IMAP SUBSCRIBE / UNSUBSCRIBE ────────────────────────────────

/// Request for `fauna.bridges.subscribe_mailbox`. Records that
/// `actor_id` is interested in `mailbox` for LSUB / `LIST
/// (SUBSCRIBED)` enumeration. Idempotent: SUBSCRIBE on an
/// already-subscribed mailbox succeeds (no-op on PK conflict).
/// Subscriptions are decoupled from mailbox existence per RFC 9051
/// §6.3.7 — a client may SUBSCRIBE a mailbox that does not (yet)
/// exist or that has been DELETEd, and the subscription persists.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SubscribeMailboxRequest {
    /// 32-byte actor identifier.
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    /// Mailbox name as the MUA sent it on the IMAP wire (post-
    /// modified-UTF-7 decode, which is the bridge's responsibility —
    /// nest receives UTF-8). No reserved-name guard: SUBSCRIBE on a
    /// standard mailbox is legitimate (MUAs frequently subscribe
    /// `INBOX`, `Sent`, `Drafts`, etc. on first connect).
    pub mailbox: String,
}

/// Outcome of one `subscribe_mailbox` call. Carries no fields today;
/// future variants (e.g. caller-supplied subscription metadata)
/// would land additively under the `outcome` tag.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum SubscribeMailboxReply {
    /// Row inserted, or already present (idempotent — RFC 9051
    /// §6.3.7 requires SUBSCRIBE-twice to succeed).
    Subscribed,
}

/// Request for `fauna.bridges.unsubscribe_mailbox`. Mirrors
/// `SubscribeMailboxRequest` — same shape, opposite effect.
/// Idempotent: UNSUBSCRIBE on a not-subscribed mailbox succeeds
/// (no-op on zero rows affected).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct UnsubscribeMailboxRequest {
    /// 32-byte actor identifier.
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    /// Mailbox name as the MUA sent it on the IMAP wire.
    pub mailbox: String,
}

/// Outcome of one `unsubscribe_mailbox` call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum UnsubscribeMailboxReply {
    /// Row removed, or was already absent (idempotent — RFC 9051
    /// §6.3.8 makes UNSUBSCRIBE-twice a success).
    Unsubscribed,
}

// ── I5 Phase F.1 — subscribe_mailbox_state / BridgeMailboxStatePush ──────────

/// Request for `fauna.bridges.subscribe_mailbox_state`. Registers an
/// in-memory interest in mailbox-state push events for the *served
/// user's* mailbox; the MDA forwards each push to its IDLE / NOTIFY
/// wire handlers. Per `docs/goal/behavior/imap-server.md` § IDLE
/// (claim 1: `actor_id` is the served user, not the MDA service-user;
/// registry is per-WS-connection-lifetime, in-memory only — claim 7).
///
/// Unlike `SubscribeMailboxRequest` (the persistent IMAP-LIST
/// subscription table per RFC 9051 §6.3.7), this is a *push*
/// registration — it does not modify any DB state and disappears
/// when the calling MDA's WS connection closes.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SubscribeMailboxStateRequest {
    /// 32-byte actor identifier of the *served user*. The MDA includes
    /// this in the wsrpc body so nest can route emissions to
    /// subscriptions independent of which WS connection carries the
    /// served user's traffic (the MDA's own service-user actor is
    /// inferred from the WS auth).
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    /// Mailbox name, post-modified-UTF-7 decode (UTF-8 on the wire).
    /// Empty string = wildcard subscription (NOTIFY with an open SET);
    /// claim 6 in `imap-server.md` lists `MailboxName` /
    /// `SubscriptionChange` as advertised-but-no-op for v1, so the
    /// wildcard path is reserved for `MessageNew`/`MessageExpunge`/
    /// `FlagChange` only.
    pub mailbox: String,
}

/// Outcome of one `subscribe_mailbox_state` call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum SubscribeMailboxStateReply {
    /// Registration recorded; the returned `subscription_id` is the
    /// demuxing key the MDA's notification router uses to dispatch
    /// each incoming push to the right IMAP session.
    Subscribed { subscription_id: u64 },
}

/// Push frame body emitted by nest when a mailbox state change matches
/// an active subscription. Wrapped by `Frame::Push` with
/// `kind = "fauna.bridges.push.mailbox_state"`. Per
/// `docs/goal/behavior/imap-server.md` § IDLE / NOTIFY (claim 2: the
/// event-translation table for IDLE wire responses).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BridgeMailboxStatePush {
    /// Echoed back from the subscribe reply; the MDA's notification
    /// router demuxes by this id.
    pub subscription_id: u64,
    /// Served user's 32-byte actor identifier.
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    /// Mailbox the event targets (UTF-8, post-modified-UTF-7 decode).
    pub mailbox: String,
    /// The state change itself.
    pub event: MailboxStateEvent,
}

/// One mailbox state change. Variants map onto the IMAP wire
/// translations the MDA emits inside IDLE / NOTIFY (claim 2):
///
/// - `Append { uid, flags, modseq }` → `* EXISTS <new-count>` +
///   `* <seq> FETCH (UID … FLAGS … MODSEQ …)`
/// - `Flags { uid, flags, modseq }` → `* <seq> FETCH (UID … FLAGS … MODSEQ …)`
/// - `Expunge { uid, modseq }` → `* VANISHED <uid>` under QRESYNC, else
///   the per-UID `* <seq> EXPUNGE`
/// - `Move { src_uid, dst_uid, modseq_src, modseq_dst, side }` → on the
///   source mailbox's subscription a disappearance (`* VANISHED <src_uid>`
///   under QRESYNC, else `* <seq> EXPUNGE`), on the destination's
///   `* EXISTS` + FETCH for `dst_uid` (`imap-server.md` § Push wiring).
///   `side` names which of the two the receiving subscription is: IMAP UIDs
///   are per-mailbox, so the MDA cannot recover it from the UIDs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MailboxStateEvent {
    Append {
        uid: u32,
        flags: Vec<String>,
        modseq: i64,
    },
    Flags {
        uid: u32,
        flags: Vec<String>,
        modseq: i64,
    },
    Expunge {
        uid: u32,
        modseq: i64,
    },
    Move {
        src_uid: u32,
        dst_uid: u32,
        modseq_src: i64,
        modseq_dst: i64,
        side: MoveSide,
    },
}

/// Which end of a [`MailboxStateEvent::Move`] the receiving subscription's
/// mailbox is. The nest emits one `Move` per subscription set, each naming
/// its own side.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MoveSide {
    /// The mailbox the message left.
    Source,
    /// The mailbox the message arrived in.
    Destination,
}

/// Push `kind` string for `BridgeMailboxStatePush` (envelope key 7).
/// Matches `PushEvent::BridgeMailboxState`'s `kind()` mapping.
pub const PUSH_KIND_BRIDGE_MAILBOX_STATE: &str = "fauna.bridges.push.mailbox_state";

// ── config_changed push (bridge hot-reload) ──────────────────────────────────

/// Push frame body emitted by nest when bridge-relevant configuration
/// changes while a bridge is running. Wrapped by `Frame::Push` with
/// `kind = "fauna.bridges.config_changed"`. It signals the bridge to
/// re-fetch `fauna.bridges.fetch_config` and re-apply the snapshot
/// in-process — no restart, no SIGHUP (subscribe-and-hot-reload, per
/// `docs/goal/behavior/mail-bridge-lifecycle.md` § Running + § Architectural
/// rules). The push carries no config itself; it is a low-rate "re-fetch
/// now" nudge so an admin edit takes effect at the next request boundary
/// instead of waiting for the bridge's next reconnect.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BridgeConfigChangedPush {
    /// Advisory hint naming what changed (`config_change_reason::*`). The
    /// bridge re-fetches the whole `fetch_config` snapshot regardless of
    /// this value; it exists for observability and to let the bridge
    /// coalesce/debounce a burst of edits. Kept a `String` (not an enum) so
    /// a future reason never breaks the bridge's decode — an unknown reason
    /// is safe to treat as "re-fetch everything".
    pub reason: String,
}

/// Stable `reason` tokens for [`BridgeConfigChangedPush`]. Each names the
/// `fetch_config` surface a mutation touched, **plus** the `"tls"`
/// prompt-refresh-on-provision nudge (see [`TLS`]). The `"tls"` token is the
/// realized half of the anticipated prompt-refresh-on-provision: the bridge
/// re-fetches its `fetch_tls_cert_blob` on it, so a cert that lands on a
/// *running* bridge (a domain added post-claim → ACME issues; an admin
/// self-signed provision) is served promptly instead of on the 12 h refresh
/// timer — no wire change (the push already carries a free-form `reason`).
pub mod config_change_reason {
    /// `set_mail_enabled` flipped the deployment-wide enable toggle.
    pub const MAIL_ENABLED: &str = "mail_enabled";
    /// `set_caldav_enabled` flipped the deployment-wide CalDAV toggle. The
    /// MDA re-fetches `fetch_config` and, if its listener-gating tuple
    /// changed, exits cleanly so s6 restarts it bound to the new protocol set.
    pub const CALDAV_ENABLED: &str = "caldav_enabled";
    /// `set_caldav_port` changed the admin-set CalDAV listener port. A running
    /// MDA re-fetches `fetch_config` and, if its bound CalDAV port changed,
    /// exits cleanly so s6 restarts it bound to the new port (the same
    /// exit-and-rebind shape `CALDAV_ENABLED` uses; the MDA cannot rebind a
    /// listener in-process).
    pub const CALDAV_PORT: &str = "caldav_port";
    /// `set_carddav_enabled` flipped the deployment-wide CardDAV toggle. The
    /// MDA re-fetches `fetch_config` and, if its listener-gating tuple changed
    /// (CardDAV rides the shared DAV listener, so this only adds/removes the
    /// `/carddav` path handler — but a transition into/out of "no DAV protocol
    /// enabled" still changes whether the listener binds at all), exits cleanly
    /// so s6 restarts it bound to the new protocol set.
    pub const CARDDAV_ENABLED: &str = "carddav_enabled";
    /// `set_webdav_enabled` flipped the deployment-wide WebDAV toggle. The MDA
    /// re-fetches `fetch_config` and, if its listener-gating tuple changed
    /// (WebDAV rides the shared DAV listener, so this only adds/removes the
    /// `/webdav` path handler — but a transition into/out of "no DAV protocol
    /// enabled" still changes whether the listener binds at all), exits cleanly
    /// so s6 restarts it bound to the new protocol set.
    pub const WEBDAV_ENABLED: &str = "webdav_enabled";
    /// A local domain was added / removed / restored / reconfigured.
    pub const LOCAL_DOMAINS: &str = "local_domains";
    /// A `put_spam_policy` edit (also the legacy `spam` overlay knobs).
    pub const SPAM_POLICY: &str = "spam_policy";
    /// A `put_auth_policy` edit.
    pub const AUTH_POLICY: &str = "auth_policy";
    /// A `put_submission_policy` edit.
    pub const SUBMISSION_POLICY: &str = "submission_policy";
    /// A `put_imap_policy` edit.
    pub const IMAP_POLICY: &str = "imap_policy";
    /// A `put_outbound_policy` edit.
    pub const OUTBOUND_POLICY: &str = "outbound_policy";
    /// The client-set NAT axis flipped (`fauna.setup.nat_mode` /
    /// `apply_node_mode_change`). A running MDA re-fetches `fetch_config` /
    /// `whoami` and, on a public→private flip, re-resolves its IMAP/CalDAV bind
    /// toward the LAN-only default.
    pub const NODE_MODE: &str = "node_mode";
    /// A TLS cert landed on the nest (ACME issued/renewed a cert covering a new
    /// `mail.<domain>`, or an admin ran `provision_self_signed_cert`) — a
    /// **prompt-refresh-on-provision** nudge. Unlike the other tokens this does
    /// **not** name a `fetch_config` surface (TLS cert blobs are out of
    /// `fetch_config`'s scope, `mail-bridge-lifecycle.md` § TLS provisioning);
    /// it tells the bridge to re-run `fetch_tls_cert_blob` (seal-on-read hands
    /// it the cert currently on disk) so a cert that changed under a *running*
    /// bridge is served without waiting out the 12 h TLS refresh timer. The
    /// bridge re-fetches TLS on *any* `config_changed`, so the cert is picked
    /// up whatever the token — the token only makes the intent observable.
    pub const TLS: &str = "tls";
}

/// Push `kind` string for [`BridgeConfigChangedPush`]. Matches
/// `PushEvent::BridgeConfigChanged`'s `kind()` mapping and the
/// `mail-bridge-lifecycle.md` § Wire shapes name (`fauna.bridges.config_changed`).
pub const PUSH_KIND_BRIDGE_CONFIG_CHANGED: &str = "fauna.bridges.config_changed";

// ── outbound_ready push (MTA outbound-drain nudge) ────────────────────────────

/// Push frame body emitted by nest to the **MTA-role** bridge when a
/// nest-side enqueue adds a `pending` row to `outbound_mail_queue` for a
/// remote recipient (today: a client `fauna.email.send` to an off-domain
/// address). Wrapped by `Frame::Push` with
/// `kind = "fauna.bridges.outbound_ready"`. It nudges the bridge's outbound
/// worker to drain promptly (`OutboundWorker.Trigger()`) instead of waiting
/// for its next `fauna.bridges.fetch_outbound_due` poll — the outbound twin
/// of the inbound `fauna.mail.received` arrival push.
///
/// **Best-effort latency optimization, not a correctness signal.** The
/// `fetch_outbound_due` poll is the universal drain backstop for *every*
/// enqueue path (bounces, forwards, TLSRPT reports, security mail); this
/// push only fires from the interactive `fauna.email.send` path where the
/// ≤30 s poll latency is user-observable. A disconnected bridge's emit is a
/// no-op — it drains on its next poll. The MDA role has no outbound worker,
/// so the emit is filtered to `BridgeRole::Mta` (unlike `config_changed`,
/// which fans to every approved bridge).
///
/// Carries no payload today — it is a pure "drain now" nudge. The `extra`
/// flatten map is the standard push-payload forward-compat seam (per § 2.3
/// rule 4): a future nest could add an advisory hint without breaking an
/// older bridge's typed decode.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct BridgeOutboundReadyPush {
    #[serde(flatten, default)]
    pub extra: std::collections::BTreeMap<String, fauna_cbor::Value>,
}

/// Push `kind` string for [`BridgeOutboundReadyPush`]. Matches
/// `PushEvent::BridgeOutboundReady`'s `kind()` mapping and the
/// `mail-bridge-lifecycle.md` § Wire shapes name (`fauna.bridges.outbound_ready`).
pub const PUSH_KIND_BRIDGE_OUTBOUND_READY: &str = "fauna.bridges.outbound_ready";

// ── rescore_ready push (MDA/content-processor re-score-drain nudge) ───────────

/// Push frame body emitted by nest to the **MDA-role / content-processor**
/// bridge when a freshly-delivered mail item seeds a new per-user re-score
/// obligation at ingest (a `labeler:<hex>` `content_scores` backlog row).
/// Wrapped by `Frame::Push` with `kind = "fauna.bridges.rescore_ready"`. It
/// nudges the bridge's re-score drain worker to run promptly (design § 2.5
/// step 4; `content-scoring.md` § Timing → *Delivery-time fast path*) instead
/// of waiting for its next startup / `config_changed` / 12 h backstop trigger —
/// the re-score twin of the inbound `fauna.mail.received` arrival push.
///
/// **Best-effort latency optimization, not a correctness signal.** The drain's
/// periodic triggers are the universal backstop; this push only fires when a
/// delivery actually seeds an obligation (the recipient subscribes ≥1 WASM
/// mail labeler), and only to grant-holding roles. A disconnected bridge's
/// emit is a no-op — it drains on its next trigger. Filtered to
/// `BridgeRole::{Mda, ContentProcessor}` (only those run the drain).
///
/// Carries no payload today — a pure "drain now" nudge. The `extra` flatten
/// map is the standard push-payload forward-compat seam (§ 2.3 rule 4).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct BridgeRescoreReadyPush {
    #[serde(flatten, default)]
    pub extra: std::collections::BTreeMap<String, fauna_cbor::Value>,
}

/// Push `kind` string for [`BridgeRescoreReadyPush`]. Matches
/// `PushEvent::BridgeRescoreReady`'s `kind()` mapping.
pub const PUSH_KIND_BRIDGE_RESCORE_READY: &str = "fauna.bridges.rescore_ready";

// ── spam_baseline_publish push (holder-pull baseline drain nudge) ─────────────

/// Push frame body emitted by nest to the **MDA-role / content-processor**
/// bridge when an admin `publish_spam_baseline` opens a pending run over
/// client-sealed contributor copies (`mail-spam.md` § Encrypted-mode
/// interaction, ratified 2026-07-13 — the third holder-pull drain instance,
/// beside the re-score plane's [`BridgeRescoreReadyPush`]). Wrapped by
/// `Frame::Push` with `kind = "fauna.bridges.spam_baseline_publish"`. The
/// holder answers by pulling `fauna.capabilities.spam_baseline_worklist` for
/// this `run_id`, unseal-merging the served copies off-box, and submitting
/// its merged half via `fauna.capabilities.submit_spam_baseline`.
///
/// Unlike the best-effort re-score nudge, the publish handler awaits the
/// holder's submit against this run with a **bounded timeout** (there is no
/// nest→bridge request/response channel — only push events + holder-initiated
/// RPCs), so a disconnected / silent holder simply means the publish proceeds
/// from the nest-readable half with the unmerged count reported honestly.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct BridgeSpamBaselinePublishPush {
    /// The 16-byte pending-run id the holder echoes on its worklist pull and
    /// submit — scopes both to exactly this publish window.
    pub run_id: ByteBuf,
    #[serde(flatten, default)]
    pub extra: std::collections::BTreeMap<String, fauna_cbor::Value>,
}

/// Push `kind` string for [`BridgeSpamBaselinePublishPush`]. Matches
/// `PushEvent::BridgeSpamBaselinePublish`'s `kind()` mapping.
pub const PUSH_KIND_BRIDGE_SPAM_BASELINE_PUBLISH: &str = "fauna.bridges.spam_baseline_publish";

// ── Phase D — CalDAV r/w + provisioning ──────────────────────────────────────

// ── Phase D.4 — put_event_ciphertext ─────────────────────────────────────────

/// `fauna.bridges.put_event_ciphertext` — create or update a CalDAV event for
/// an actor. The MDA seals the iCalendar body before sending; nest stores the
/// ciphertext opaquely. Routing is by `uid_hash` so both create and update
/// share one round-trip without the caller knowing whether a prior row exists.
///
/// `if_match`: `Some("<etag>")` for conditional update (CalDAV `If-Match`),
/// `None` for unconditional (absent / `If-Match: *` in the upstream HTTP header
/// — the bridge normalises both to `None` on the wire).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PutEventCiphertextRequest {
    /// 32-byte actor identifier.
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    /// 32-byte calendar identifier (client-assigned).
    #[serde(with = "serde_bytes")]
    pub calendar_id: Vec<u8>,
    /// 32-byte blake3 hash of the plaintext CalDAV UID. Used for deduplication
    /// without revealing the UID itself; the plaintext UID stays inside the
    /// ciphertext.
    #[serde(with = "serde_bytes")]
    pub uid_hash: Vec<u8>,
    /// Sealed event body. Must be non-empty.
    #[serde(with = "serde_bytes")]
    pub encrypted_body: Vec<u8>,
    /// Sealed search-index hint. Must be non-empty.
    #[serde(with = "serde_bytes")]
    pub encrypted_index_hint: Vec<u8>,
    /// Bridge-reported epoch seconds (CalDAV CREATED / LAST-MODIFIED surrogate).
    pub timestamp: i64,
    /// Must equal `encrypted_body.len()`. Stored explicitly so future
    /// bucket-rounding for privacy can swap the value without touching the body.
    pub ciphertext_size: u32,
    /// ETag from a prior `PutEventCiphertextReply`; `None` means unconditional.
    pub if_match: Option<String>,
    /// Optional sealed Fauna-extension sidecar — the Fauna-only refinement
    /// layer (the `interested` RSVP refinement, per-attendee nest-url
    /// resolution hints) that is **never** served to a CalDAV MUA
    /// (caldav-server.md § Event resources). `None` on the wire means "no
    /// sidecar in this write": on an UPDATE the handler **preserves** the prior
    /// row's sidecar (a MUA PUT carries none, so its edits keep the Fauna
    /// refinement attached); a Fauna-app write sends `Some(..)` to replace
    /// both halves. The MDA always sends `None`.
    #[serde(default, skip_serializing_if = "Option::is_none", with = "serde_bytes")]
    pub encrypted_fauna_ext: Option<Vec<u8>>,
}

/// Reply to `fauna.bridges.put_event_ciphertext`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum PutEventCiphertextReply {
    /// No prior row for this uid_hash; a fresh event row was inserted.
    Created {
        /// Server-assigned 32-byte event id (deterministic blake3 hash).
        #[serde(with = "serde_bytes")]
        event_id: Vec<u8>,
        /// ETag for subsequent conditional requests.
        etag: String,
        /// New highestmodseq of the calendar.
        modseq: i64,
    },
    /// A prior row for this uid_hash existed and was replaced; a tombstone was
    /// written for the old event_id. Also returned for idempotent transport
    /// retries (the row already matches — modseq unchanged).
    Updated {
        /// Server-assigned 32-byte event id for the new body.
        #[serde(with = "serde_bytes")]
        event_id: Vec<u8>,
        etag: String,
        modseq: i64,
    },
    /// `if_match` was supplied but did not match the prior row's etag.
    PreconditionFailed {
        /// The etag of the row as it currently stands.
        current_etag: String,
    },
    /// No calendar row exists for `(actor_id, calendar_id)`.
    CalendarNotFound,
    /// An outcome a newer nest added that this peer does not know. The write
    /// may have landed: a reader treats it as "written, state unknown" and
    /// re-reads. Never written back (`tools/check-additive-evolution/enum_ledger.txt`).
    #[serde(other, skip_serializing)]
    Unknown,
}

/// `fauna.bridges.place_inbound_invite` — the MTA places an invitation that
/// arrived by email on the recipient's calendar (`caldav-server.md` §
/// Server-side auto-schedule, "Inbound invite"). The MTA holds the message in
/// cleartext for the length of the delivery only, so it seals both halves to the
/// recipient exactly as a CalDAV PUT seals them (encrypt-only — it never holds a
/// key that opens the recipient's calendar) and nest stores them opaquely.
///
/// **Create-only.** An invitation never changes an event already on the
/// calendar: email carries no proof of who sent it, so an update or a UID
/// collision is left to the user (the message itself stays in their inbox).
/// The event lands in the lazy `Personal` calendar, which nest provisions (its
/// metadata sealed to the recipient, encrypt-only) when the actor has none yet.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PlaceInboundInviteRequest {
    /// 32-byte recipient actor identifier.
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    /// 32-byte blake3 hash of the invitation's plaintext `UID`.
    #[serde(with = "serde_bytes")]
    pub uid_hash: Vec<u8>,
    /// Sealed stored event body. Must be a sealed recipient envelope.
    #[serde(with = "serde_bytes")]
    pub encrypted_body: Vec<u8>,
    /// Sealed search-index hint. Must be a sealed recipient envelope.
    #[serde(with = "serde_bytes")]
    pub encrypted_index_hint: Vec<u8>,
    /// Delivery time, epoch seconds (the event's CREATED / LAST-MODIFIED
    /// surrogate).
    pub timestamp: i64,
    /// The message's envelope sender (`MAIL FROM`). nest re-derives the
    /// recipient's guardian mail verdict from it (`family-safety.md` § The mail
    /// gate): an invitation reaches the calendar only when its mail was
    /// delivered, never when it was held.
    pub sender_address: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply to `fauna.bridges.place_inbound_invite`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum PlaceInboundInviteReply {
    /// The invitation is now on the recipient's `Personal` calendar.
    Placed,
    /// An event with this `UID` is already on one of the recipient's calendars
    /// and was left untouched.
    AlreadyOnCalendar,
    /// The recipient's guardian mail gate holds mail from this sender, so the
    /// invitation waits with the mail instead of reaching the calendar.
    Withheld,
    /// The recipient's shared storage quota has no room for the event, so it
    /// was not placed (`caldav-server.md` § QUOTA → § Enforcement points). The
    /// mail copy is already delivered; this is an outcome to log, never a
    /// failed delivery.
    OverQuota,
    /// An outcome a newer nest added that this peer does not know.
    #[serde(other)]
    Unknown,
}

/// `fauna.bridges.provision_calendar` — create a calendar collection for
/// an actor. Idempotent on byte-identical metadata; a metadata mismatch on
/// the same `(actor_id, calendar_id)` returns `Conflict` (rename is a
/// follow-up). The MDA (or the client itself) calls this before issuing any
/// CalDAV PUT/DELETE operations.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ProvisionCalendarRequest {
    /// The actor for whom the calendar is provisioned (32 bytes).
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    /// Opaque 32-byte calendar identifier (client-assigned, must be unique
    /// per actor).
    #[serde(with = "serde_bytes")]
    pub calendar_id: Vec<u8>,
    /// Sealed collection metadata (name, colour, etc.). Nest treats this as
    /// opaque bytes; the client seals it before sending.
    #[serde(with = "serde_bytes")]
    pub encrypted_metadata: Vec<u8>,
    /// When `false` (default), this is a MKCOL-style provision: insert a new
    /// row, or report `AlreadyExists` / `Conflict` against an existing row.
    /// When `true`, this is a PROPPATCH-style metadata update: overwrite the
    /// existing row's `encrypted_metadata`, bump `highestmodseq`, and reply
    /// `Updated` (or `NotFound` if no row exists for `(actor_id, calendar_id)`).
    /// `#[serde(default)]` decodes an omitted field (the MKCOL case) to
    /// `false` — MKCOL behaviour unchanged.
    #[serde(default)]
    pub update_metadata: bool,
}

/// Reply to `fauna.bridges.provision_calendar`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ProvisionCalendarReply {
    /// A new calendar row was inserted (MKCOL path, `update_metadata=false`).
    Created,
    /// A row with the same `(actor_id, calendar_id)` already exists and its
    /// metadata bytes are byte-identical — idempotent MKCOL retry.
    AlreadyExists,
    /// A row with the same `(actor_id, calendar_id)` exists but the metadata
    /// bytes differ. The existing row is unchanged; the client should either
    /// retry with the same bytes or use a different `calendar_id`. MKCOL path.
    Conflict,
    /// The existing row's `encrypted_metadata` was overwritten (PROPPATCH path,
    /// `update_metadata=true`); `highestmodseq` bumped.
    Updated,
    /// No row exists for `(actor_id, calendar_id)`. PROPPATCH path only —
    /// MKCOL never returns this (a missing row triggers the insert branch).
    NotFound,
    /// An outcome a newer nest added that this peer does not know. The
    /// provision may have landed: a reader re-lists the calendars. Never
    /// written back (`tools/check-additive-evolution/enum_ledger.txt`).
    #[serde(other, skip_serializing)]
    Unknown,
}

/// `fauna.bridges.list_calendars` — list all provisioned calendars for an
/// actor. Called by the MDA on a CalDAV PROPFIND for `/caldav/{user}/`.
/// Returns an empty list for actors with no provisioned calendars.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ListCalendarsRequest {
    /// The actor whose calendars are listed (32 bytes).
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
}

/// One calendar in a `ListCalendarsReply`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CalendarEntry {
    /// Opaque 32-byte calendar identifier (client-assigned).
    #[serde(with = "serde_bytes")]
    pub calendar_id: Vec<u8>,
    /// Sealed collection metadata (name, colour, etc.). Nest stores this
    /// opaquely; the MDA decrypts it in-session.
    #[serde(with = "serde_bytes")]
    pub encrypted_metadata: Vec<u8>,
    /// Current ctag (incremented on any write to the calendar).
    pub ctag: i64,
    /// Highest modseq seen in this calendar; used for delta-sync.
    pub highestmodseq: i64,
    /// Total number of events in the calendar.
    pub event_count: u32,
    /// Unix epoch seconds when the calendar was provisioned.
    pub created_at: i64,
}

/// Reply to `fauna.bridges.list_calendars`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ListCalendarsReply {
    /// All provisioned calendars, ordered by `created_at` ASC. Empty when
    /// the actor has no calendars.
    pub calendars: Vec<CalendarEntry>,
}

// ── Phase D.3 — query_events ──────────────────────────────────────────────────

/// `fauna.bridges.query_events` request — paginated REPORT (calendar-query /
/// multiget) against a single CalDAV collection. Nest applies no server-side
/// filtering (Decision 10: encrypted bodies are opaque); the MDA decrypts and
/// filters locally.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QueryEventsRequest {
    /// 32-byte actor identifier.
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    /// 32-byte calendar identifier (client-assigned).
    #[serde(with = "serde_bytes")]
    pub calendar_id: Vec<u8>,
    /// CONDSTORE incremental sync — when `Some(n)`, only events with
    /// `modseq > n` are returned.
    pub since_modseq: Option<i64>,
    /// Pagination resume token (32 bytes when `Some`). Returns only events
    /// with `event_id > after_event_id` (lexicographic on BLOB).
    pub after_event_id: Option<ByteBuf>,
    /// Maximum number of events to return. `0` = unbounded.
    pub limit: u32,
}

/// One event entry in a `QueryEventsReply::Ok`. Kept module-public so D.6
/// (`sync_calendar_since`) can reuse it without re-defining the shape.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct EventEntry {
    /// Server-assigned 32-byte event id (deterministic blake3 hash).
    #[serde(with = "serde_bytes")]
    pub event_id: Vec<u8>,
    /// Blake3-derived hash of the CalDAV UID (plaintext index; used for
    /// deduplication without revealing the UID itself).
    #[serde(with = "serde_bytes")]
    pub uid_hash: Vec<u8>,
    /// Sealed event body. The MDA decrypts this.
    #[serde(with = "serde_bytes")]
    pub encrypted_body: Vec<u8>,
    /// Sealed search-index hint.
    #[serde(with = "serde_bytes")]
    pub encrypted_index_hint: Vec<u8>,
    /// ETag for conditional request matching (CalDAV `If-Match` / `If-None-Match`).
    pub etag: String,
    /// Modseq at which this event was last written.
    pub modseq: i64,
    /// Bridge-reported ciphertext byte count.
    pub ciphertext_size: u32,
    /// Epoch seconds (CalDAV CREATED / LAST-MODIFIED surrogate).
    pub internal_date: i64,
    /// Optional sealed Fauna-extension sidecar (see
    /// `PutEventCiphertextRequest::encrypted_fauna_ext`). `None` when the event
    /// was last written by a MUA (no sidecar).
    /// A CalDAV MUA never receives this; only Fauna apps read it, applying
    /// the asymmetric `interested↔TENTATIVE` projection (caldav-server.md
    /// § RSVP semantics — the sidecar is authoritative for the `interested`
    /// refinement).
    #[serde(default, skip_serializing_if = "Option::is_none", with = "serde_bytes")]
    pub encrypted_fauna_ext: Option<Vec<u8>>,
}

/// Reply to `fauna.bridges.query_events`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum QueryEventsReply {
    /// Events returned successfully.
    Ok {
        /// Matching events, ordered by `event_id ASC`.
        events: Vec<EventEntry>,
        /// Current `highestmodseq` of the calendar (captured before the
        /// query; stable reference for the caller's next incremental sync).
        highestmodseq: i64,
        /// `true` iff more pages remain — caller should resume with
        /// `after_event_id = events.last().event_id`.
        more: bool,
    },
    /// No calendar row exists for `(actor_id, calendar_id)`.
    CalendarNotFound,
    /// An outcome a newer nest added that this peer does not know. A reader
    /// treats it as an error — never as `CalendarNotFound`, which would empty
    /// the list and forget the sync token. Never written back
    /// (`tools/check-additive-evolution/enum_ledger.txt`).
    #[serde(other, skip_serializing)]
    Unknown,
}

// ── Phase D.6 — sync_calendar_since ──────────────────────────────────────────

/// `fauna.bridges.sync_calendar_since` request — RFC 6578 `REPORT
/// sync-collection`. Asks nest for all events changed (and all tombstones
/// written) since `sync_token`. A `sync_token` of `"0"` performs a full sync;
/// any other value is treated as a decimal `i64` modseq, the `new_sync_token`
/// returned by a prior call. Sync-tokens are intentionally opaque strings on
/// the wire (per RFC 6578 § 3.1) even though the concrete representation is a
/// decimal modseq.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SyncCalendarSinceRequest {
    /// 32-byte actor identifier.
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    /// 32-byte calendar identifier (client-assigned).
    #[serde(with = "serde_bytes")]
    pub calendar_id: Vec<u8>,
    /// RFC 6578 opaque sync-token. `"0"` = full sync from the beginning;
    /// any other value must be a decimal `i64` modseq (as returned in a
    /// prior `new_sync_token`). Negative values are rejected as malformed.
    pub sync_token: String,
    /// Maximum number of changed events to return. `0` = unbounded. Note
    /// that tombstones (expunged entries) are never paginated — the caller
    /// always receives all tombstones since `sync_token` in one reply.
    pub limit: u32,
    /// MUA identifier from the HTTP `User-Agent` request header; advisory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mua_id: Option<String>,
}

/// A single deletion tombstone in a `SyncCalendarSinceReply::Ok`. Corresponds
/// to RFC 6578 VANISHED responses in CalDAV sync-collection reports.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExpungedEntry {
    /// Server-assigned 32-byte event id of the deleted row.
    #[serde(with = "serde_bytes")]
    pub event_id: Vec<u8>,
    /// Blake3-derived hash of the CalDAV UID (plaintext index without revealing
    /// the UID itself).
    #[serde(with = "serde_bytes")]
    pub uid_hash: Vec<u8>,
    /// Modseq at which this event was deleted (the tombstone's modseq).
    pub modseq: i64,
}

/// Reply to `fauna.bridges.sync_calendar_since`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum SyncCalendarSinceReply {
    /// Sync succeeded.
    Ok {
        /// Events changed since `sync_token`, ordered by `modseq ASC`.
        changed: Vec<EventEntry>,
        /// Tombstones written since `sync_token`, ordered by `modseq ASC`.
        expunged: Vec<ExpungedEntry>,
        /// Opaque cursor for the next call, formatted as a decimal string.
        ///
        /// When `more == true`, this is the modseq of the last returned event
        /// in `changed`; pass it back as `sync_token` to fetch the rest of the
        /// page.  When `more == false`, this is the calendar-wide
        /// highestmodseq — incremental sync resumes from it on the next call.
        /// Always strictly greater than the input `sync_token` if `changed` is
        /// non-empty.
        new_sync_token: String,
        /// `true` iff more changed events remain — the expunged list is always
        /// complete regardless of `more`. Caller should retry with the returned
        /// `new_sync_token` as the next `sync_token` to page forward.
        more: bool,
        /// `true` iff the supplied `sync_token` is valid (`<= highestmodseq`)
        /// but predates the tombstone-retention window, so nest cannot
        /// honestly enumerate the deletions since then (a tombstone newer
        /// than the token was expunged before the retention cutoff). The MDA
        /// translates `stale = true` to the RFC 6578 §3.8
        /// `DAV:valid-sync-token` precondition failure — the same MDA-side
        /// effect as `Stale`, but a distinct nest-side condition: retention
        /// expiry is expected, so **no** `bridge_restore_divergence` row is
        /// written (unlike the `Stale` MUA-ahead case). Defaults to `false`
        /// (the steady-state "changes since last sync" reply); `#[serde(default)]`
        /// keeps the field optional on the wire for forward-compat.
        #[serde(default)]
        stale: bool,
    },
    /// No calendar row exists for `(actor_id, calendar_id)`.
    CalendarNotFound,
    /// Client's sync-token is ahead of the calendar's current
    /// highestmodseq — the post-DR-restore "MUA ahead" case (spec § D6
    /// (γ)). MDA translates to RFC 6578 §3.8 `DAV:valid-sync-token`
    /// precondition failure; client falls through to full PROPFIND. Distinct
    /// from `Ok { stale: true }` (token behind retention): this case is
    /// forensic and writes a `bridge_restore_divergence` row.
    Stale { server_modseq: i64 },
    /// An outcome a newer nest added that this peer does not know. A reader
    /// does a full re-read and applies no deletions. Never written back
    /// (`tools/check-additive-evolution/enum_ledger.txt`).
    #[serde(other, skip_serializing)]
    Unknown,
}

// ── Phase D.5 — delete_event ──────────────────────────────────────────────────

/// `fauna.bridges.delete_event` — delete a CalDAV event for an actor by
/// `uid_hash` within a calendar. The MDA calls this in response to a CalDAV
/// DELETE on an event resource URL. `If-Match` is forwarded from the upstream
/// HTTP header; `None` means unconditional delete.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeleteEventRequest {
    /// 32-byte actor identifier.
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    /// 32-byte calendar identifier (client-assigned).
    #[serde(with = "serde_bytes")]
    pub calendar_id: Vec<u8>,
    /// 32-byte blake3 hash of the plaintext CalDAV UID — used as the lookup key
    /// without revealing the UID itself.
    #[serde(with = "serde_bytes")]
    pub uid_hash: Vec<u8>,
    /// ETag from a prior reply; `None` means unconditional delete.
    pub if_match: Option<String>,
}

/// Reply to `fauna.bridges.delete_event`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum DeleteEventReply {
    /// The event row was deleted and a tombstone was written. `modseq` is the
    /// new `highestmodseq` of the calendar.
    Deleted {
        /// Server-assigned 32-byte event id of the deleted row.
        #[serde(with = "serde_bytes")]
        event_id: Vec<u8>,
        /// New `highestmodseq` of the calendar after the delete.
        modseq: i64,
    },
    /// No event exists for the given `uid_hash`, **or** the calendar itself does
    /// not exist. The two paths are intentionally collapsed: callers must not
    /// distinguish a missing calendar from a missing event (Decision 7 —
    /// leaking which would let a caller probe whether a calendar exists).
    NotFound,
    /// `if_match` was supplied but did not match the current etag.
    PreconditionFailed {
        /// The etag of the event row as it currently stands.
        current_etag: String,
    },
    /// An outcome a newer nest added that this peer does not know. The
    /// delete may have landed: a reader re-reads. Never written back
    /// (`tools/check-additive-evolution/enum_ledger.txt`).
    #[serde(other, skip_serializing)]
    Unknown,
}

// ── Phase E — CardDAV r/w + provisioning ──────────────────────────────────────
//
// A structural mirror of the Phase D CalDAV surface (above) for address books /
// vCards, per the CardDAV design proposal (tracked internally): the MDA
// serves CardDAV to generic clients (DAVx5 / Apple Contacts / Thunderbird) from
// a new `bridge_carddav_*` store sealed to the actor's MLS pubkey — nest never
// holds plaintext vCard bodies, exactly as CalDAV never holds plaintext
// iCalendar. Every type here is the CalDAV twin with `calendar` → `addressbook`
// and `event` → `card`; the wire discipline (32-byte ids via `serde_bytes`,
// `#[serde(tag = "outcome")]` reply enums, blake3 `uid_hash`, RFC 6578
// sync-collection tokens) is identical.

// ── Phase E.4 — put_card_ciphertext ───────────────────────────────────────────

/// `fauna.bridges.put_card_ciphertext` — create or update a CardDAV card
/// (vCard) for an actor. The MDA seals the vCard body before sending; nest
/// stores the ciphertext opaquely. Routing is by `uid_hash` so both create and
/// update share one round-trip without the caller knowing whether a prior row
/// exists.
///
/// `if_match`: `Some("<etag>")` for conditional update (CardDAV `If-Match`),
/// `None` for unconditional (absent / `If-Match: *` in the upstream HTTP header
/// — the bridge normalises both to `None` on the wire).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PutCardCiphertextRequest {
    /// 32-byte actor identifier.
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    /// 32-byte address-book identifier (client-assigned).
    #[serde(with = "serde_bytes")]
    pub addressbook_id: Vec<u8>,
    /// 32-byte blake3 hash of the plaintext vCard UID. Used for deduplication
    /// without revealing the UID itself; the plaintext UID stays inside the
    /// ciphertext.
    #[serde(with = "serde_bytes")]
    pub uid_hash: Vec<u8>,
    /// Sealed vCard body. Must be non-empty.
    #[serde(with = "serde_bytes")]
    pub encrypted_body: Vec<u8>,
    /// Sealed search-index hint (sealed over the vCard's FN / EMAIL / TEL). Must
    /// be non-empty.
    #[serde(with = "serde_bytes")]
    pub encrypted_index_hint: Vec<u8>,
    /// Bridge-reported epoch seconds (vCard REV surrogate).
    pub timestamp: i64,
    /// Must equal `encrypted_body.len()`. Stored explicitly so future
    /// bucket-rounding for privacy can swap the value without touching the body.
    pub ciphertext_size: u32,
    /// ETag from a prior `PutCardCiphertextReply`; `None` means unconditional.
    pub if_match: Option<String>,
    /// Optional sealed Fauna-extension sidecar — the Fauna-only refinement
    /// layer (e.g. the `X-FAUNA-ACTOR-ID` linkage that points a vCard at a
    /// Fauna social contact, per the design § 2) that is **never** served to a
    /// CardDAV MUA. `None` on the wire means "no sidecar in this write": on an
    /// UPDATE the handler **preserves** the prior row's sidecar (a MUA PUT
    /// carries none, so its edits keep the Fauna refinement attached); a
    /// Fauna-app write sends `Some(..)` to replace both halves. The MDA
    /// always sends `None`.
    #[serde(default, skip_serializing_if = "Option::is_none", with = "serde_bytes")]
    pub encrypted_fauna_ext: Option<Vec<u8>>,
}

/// Reply to `fauna.bridges.put_card_ciphertext`.
///
/// Ledger answer `locked` (`tools/check-additive-evolution/enum_ledger.txt`):
/// only the in-image bridge decodes this while no app calls the kind. The
/// first app caller must add an unknown arm in the same change.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum PutCardCiphertextReply {
    /// No prior row for this uid_hash; a fresh card row was inserted.
    Created {
        /// Server-assigned 32-byte card id (deterministic blake3 hash).
        #[serde(with = "serde_bytes")]
        card_id: Vec<u8>,
        /// ETag for subsequent conditional requests.
        etag: String,
        /// New highestmodseq of the address book.
        modseq: i64,
    },
    /// A prior row for this uid_hash existed and was replaced; a tombstone was
    /// written for the old card_id. Also returned for idempotent transport
    /// retries (the row already matches — modseq unchanged).
    Updated {
        /// Server-assigned 32-byte card id for the new body.
        #[serde(with = "serde_bytes")]
        card_id: Vec<u8>,
        etag: String,
        modseq: i64,
    },
    /// `if_match` was supplied but did not match the prior row's etag.
    PreconditionFailed {
        /// The etag of the row as it currently stands.
        current_etag: String,
    },
    /// No address-book row exists for `(actor_id, addressbook_id)`.
    AddressbookNotFound,
}

/// `fauna.bridges.provision_addressbook` — create an address-book collection
/// for an actor. Idempotent on byte-identical metadata; a metadata mismatch on
/// the same `(actor_id, addressbook_id)` returns `Conflict` (rename is a
/// follow-up). The MDA (or the client itself) calls this before issuing any
/// CardDAV PUT/DELETE operations.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ProvisionAddressbookRequest {
    /// The actor for whom the address book is provisioned (32 bytes).
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    /// Opaque 32-byte address-book identifier (client-assigned, must be unique
    /// per actor).
    #[serde(with = "serde_bytes")]
    pub addressbook_id: Vec<u8>,
    /// Sealed collection metadata (name, colour, etc.). Nest treats this as
    /// opaque bytes; the client seals it before sending.
    #[serde(with = "serde_bytes")]
    pub encrypted_metadata: Vec<u8>,
    /// When `false` (default), this is a MKCOL-style provision: insert a new
    /// row, or report `AlreadyExists` / `Conflict` against an existing row.
    /// When `true`, this is a PROPPATCH-style metadata update: overwrite the
    /// existing row's `encrypted_metadata`, bump `highestmodseq`, and reply
    /// `Updated` (or `NotFound` if no row exists for `(actor_id,
    /// addressbook_id)`). `#[serde(default)]` decodes an omitted field (the MKCOL case)
    /// to `false` — MKCOL behaviour unchanged.
    #[serde(default)]
    pub update_metadata: bool,
}

/// Reply to `fauna.bridges.provision_addressbook`.
///
/// Ledger answer `locked` (`tools/check-additive-evolution/enum_ledger.txt`):
/// only the in-image bridge decodes this while no app calls the kind. The
/// first app caller must add an unknown arm in the same change.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ProvisionAddressbookReply {
    /// A new address-book row was inserted (MKCOL path, `update_metadata=false`).
    Created,
    /// A row with the same `(actor_id, addressbook_id)` already exists and its
    /// metadata bytes are byte-identical — idempotent MKCOL retry.
    AlreadyExists,
    /// A row with the same `(actor_id, addressbook_id)` exists but the metadata
    /// bytes differ. The existing row is unchanged; the client should either
    /// retry with the same bytes or use a different `addressbook_id`. MKCOL path.
    Conflict,
    /// The existing row's `encrypted_metadata` was overwritten (PROPPATCH path,
    /// `update_metadata=true`); `highestmodseq` bumped.
    Updated,
    /// No row exists for `(actor_id, addressbook_id)`. PROPPATCH path only —
    /// MKCOL never returns this (a missing row triggers the insert branch).
    NotFound,
}

/// `fauna.bridges.list_addressbooks` — list all provisioned address books for
/// an actor. Called by the MDA on a CardDAV PROPFIND for `/carddav/{user}/`.
/// Returns an empty list for actors with no provisioned address books.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ListAddressbooksRequest {
    /// The actor whose address books are listed (32 bytes).
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
}

/// One address book in a `ListAddressbooksReply`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AddressbookEntry {
    /// Opaque 32-byte address-book identifier (client-assigned).
    #[serde(with = "serde_bytes")]
    pub addressbook_id: Vec<u8>,
    /// Sealed collection metadata (name, colour, etc.). Nest stores this
    /// opaquely; the MDA decrypts it in-session.
    #[serde(with = "serde_bytes")]
    pub encrypted_metadata: Vec<u8>,
    /// Current ctag (incremented on any write to the address book).
    pub ctag: i64,
    /// Highest modseq seen in this address book; used for delta-sync.
    pub highestmodseq: i64,
    /// Total number of cards in the address book.
    pub card_count: u32,
    /// Unix epoch seconds when the address book was provisioned.
    pub created_at: i64,
}

/// Reply to `fauna.bridges.list_addressbooks`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ListAddressbooksReply {
    /// All provisioned address books, ordered by `created_at` ASC. Empty when
    /// the actor has no address books.
    pub addressbooks: Vec<AddressbookEntry>,
}

// ── Phase E.3 — query_cards ───────────────────────────────────────────────────

/// `fauna.bridges.query_cards` request — paginated REPORT (addressbook-query /
/// multiget) against a single CardDAV collection. Nest applies no server-side
/// filtering (encrypted bodies are opaque); the MDA decrypts and filters
/// locally.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QueryCardsRequest {
    /// 32-byte actor identifier.
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    /// 32-byte address-book identifier (client-assigned).
    #[serde(with = "serde_bytes")]
    pub addressbook_id: Vec<u8>,
    /// CONDSTORE incremental sync — when `Some(n)`, only cards with
    /// `modseq > n` are returned.
    pub since_modseq: Option<i64>,
    /// Pagination resume token (32 bytes when `Some`). Returns only cards
    /// with `card_id > after_card_id` (lexicographic on BLOB).
    pub after_card_id: Option<ByteBuf>,
    /// Maximum number of cards to return. `0` = unbounded.
    pub limit: u32,
}

/// One card entry in a `QueryCardsReply::Ok`. Kept module-public so E.6
/// (`sync_addressbook_since`) can reuse it without re-defining the shape.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CardEntry {
    /// Server-assigned 32-byte card id (deterministic blake3 hash).
    #[serde(with = "serde_bytes")]
    pub card_id: Vec<u8>,
    /// Blake3-derived hash of the vCard UID (plaintext index; used for
    /// deduplication without revealing the UID itself).
    #[serde(with = "serde_bytes")]
    pub uid_hash: Vec<u8>,
    /// Sealed vCard body. The MDA decrypts this.
    #[serde(with = "serde_bytes")]
    pub encrypted_body: Vec<u8>,
    /// Sealed search-index hint.
    #[serde(with = "serde_bytes")]
    pub encrypted_index_hint: Vec<u8>,
    /// ETag for conditional request matching (CardDAV `If-Match` / `If-None-Match`).
    pub etag: String,
    /// Modseq at which this card was last written.
    pub modseq: i64,
    /// Bridge-reported ciphertext byte count.
    pub ciphertext_size: u32,
    /// Epoch seconds (vCard REV surrogate).
    pub internal_date: i64,
    /// Optional sealed Fauna-extension sidecar (see
    /// `PutCardCiphertextRequest::encrypted_fauna_ext`). `None` when the card
    /// was last written by a MUA (no sidecar).
    /// A CardDAV MUA never receives this; only Fauna apps read it.
    #[serde(default, skip_serializing_if = "Option::is_none", with = "serde_bytes")]
    pub encrypted_fauna_ext: Option<Vec<u8>>,
}

/// Reply to `fauna.bridges.query_cards`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum QueryCardsReply {
    /// Cards returned successfully.
    Ok {
        /// Matching cards, ordered by `card_id ASC`.
        cards: Vec<CardEntry>,
        /// Current `highestmodseq` of the address book (captured before the
        /// query; stable reference for the caller's next incremental sync).
        highestmodseq: i64,
        /// `true` iff more pages remain — caller should resume with
        /// `after_card_id = cards.last().card_id`.
        more: bool,
    },
    /// No address-book row exists for `(actor_id, addressbook_id)`.
    AddressbookNotFound,
    /// An outcome a newer nest added that this peer does not know. A reader
    /// treats it as an error — never as `AddressbookNotFound` or an empty
    /// book. Never written back (`tools/check-additive-evolution/enum_ledger.txt`).
    #[serde(other, skip_serializing)]
    Unknown,
}

// ── Phase E.6 — sync_addressbook_since ────────────────────────────────────────

/// `fauna.bridges.sync_addressbook_since` request — RFC 6578 `REPORT
/// sync-collection`. Asks nest for all cards changed (and all tombstones
/// written) since `sync_token`. A `sync_token` of `"0"` performs a full sync;
/// any other value is treated as a decimal `i64` modseq, the `new_sync_token`
/// returned by a prior call. Sync-tokens are intentionally opaque strings on
/// the wire (per RFC 6578 § 3.1) even though the concrete representation is a
/// decimal modseq.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SyncAddressbookSinceRequest {
    /// 32-byte actor identifier.
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    /// 32-byte address-book identifier (client-assigned).
    #[serde(with = "serde_bytes")]
    pub addressbook_id: Vec<u8>,
    /// RFC 6578 opaque sync-token. `"0"` = full sync from the beginning;
    /// any other value must be a decimal `i64` modseq (as returned in a
    /// prior `new_sync_token`). Negative values are rejected as malformed.
    pub sync_token: String,
    /// Maximum number of changed cards to return. `0` = unbounded. Note
    /// that tombstones (expunged entries) are never paginated — the caller
    /// always receives all tombstones since `sync_token` in one reply.
    pub limit: u32,
    /// MUA identifier from the HTTP `User-Agent` request header; advisory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mua_id: Option<String>,
}

/// A single deletion tombstone in a `SyncAddressbookSinceReply::Ok`. Corresponds
/// to RFC 6578 VANISHED responses in CardDAV sync-collection reports.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExpungedCardEntry {
    /// Server-assigned 32-byte card id of the deleted row.
    #[serde(with = "serde_bytes")]
    pub card_id: Vec<u8>,
    /// Blake3-derived hash of the vCard UID (plaintext index without revealing
    /// the UID itself).
    #[serde(with = "serde_bytes")]
    pub uid_hash: Vec<u8>,
    /// Modseq at which this card was deleted (the tombstone's modseq).
    pub modseq: i64,
}

/// Reply to `fauna.bridges.sync_addressbook_since`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum SyncAddressbookSinceReply {
    /// Sync succeeded.
    Ok {
        /// Cards changed since `sync_token`, ordered by `modseq ASC`.
        changed: Vec<CardEntry>,
        /// Tombstones written since `sync_token`, ordered by `modseq ASC`.
        expunged: Vec<ExpungedCardEntry>,
        /// Opaque cursor for the next call, formatted as a decimal string.
        ///
        /// When `more == true`, this is the modseq of the last returned card
        /// in `changed`; pass it back as `sync_token` to fetch the rest of the
        /// page.  When `more == false`, this is the address-book-wide
        /// highestmodseq — incremental sync resumes from it on the next call.
        /// Always strictly greater than the input `sync_token` if `changed` is
        /// non-empty.
        new_sync_token: String,
        /// `true` iff more changed cards remain — the expunged list is always
        /// complete regardless of `more`. Caller should retry with the returned
        /// `new_sync_token` as the next `sync_token` to page forward.
        more: bool,
        /// `true` iff the supplied `sync_token` is valid (`<= highestmodseq`)
        /// but predates the tombstone-retention window, so nest cannot
        /// honestly enumerate the deletions since then (a tombstone newer
        /// than the token was expunged before the retention cutoff). The MDA
        /// translates `stale = true` to the RFC 6578 §3.8
        /// `DAV:valid-sync-token` precondition failure — the same MDA-side
        /// effect as `Stale`, but a distinct nest-side condition: retention
        /// expiry is expected, so **no** `bridge_restore_divergence` row is
        /// written (unlike the `Stale` MUA-ahead case). Defaults to `false`
        /// (the steady-state "changes since last sync" reply); `#[serde(default)]`
        /// keeps the field optional on the wire for forward-compat.
        #[serde(default)]
        stale: bool,
    },
    /// No address-book row exists for `(actor_id, addressbook_id)`.
    AddressbookNotFound,
    /// Client's sync-token is ahead of the address book's current
    /// highestmodseq — the post-DR-restore "MUA ahead" case. MDA translates to
    /// RFC 6578 §3.8 `DAV:valid-sync-token` precondition failure; client falls
    /// through to full PROPFIND. Distinct from `Ok { stale: true }` (token
    /// behind retention): this case is forensic and writes a
    /// `bridge_restore_divergence` row.
    Stale { server_modseq: i64 },
    /// An outcome a newer nest added that this peer does not know. A reader
    /// does a full re-read and applies no deletions. Never written back
    /// (`tools/check-additive-evolution/enum_ledger.txt`).
    #[serde(other, skip_serializing)]
    Unknown,
}

// ── Phase E.5 — delete_card ───────────────────────────────────────────────────

/// `fauna.bridges.delete_card` — delete a CardDAV card for an actor by
/// `uid_hash` within an address book. The MDA calls this in response to a
/// CardDAV DELETE on a card resource URL. `If-Match` is forwarded from the
/// upstream HTTP header; `None` means unconditional delete.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeleteCardRequest {
    /// 32-byte actor identifier.
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    /// 32-byte address-book identifier (client-assigned).
    #[serde(with = "serde_bytes")]
    pub addressbook_id: Vec<u8>,
    /// 32-byte blake3 hash of the plaintext vCard UID — used as the lookup key
    /// without revealing the UID itself.
    #[serde(with = "serde_bytes")]
    pub uid_hash: Vec<u8>,
    /// ETag from a prior reply; `None` means unconditional delete.
    pub if_match: Option<String>,
}

/// Reply to `fauna.bridges.delete_card`.
///
/// Ledger answer `locked` (`tools/check-additive-evolution/enum_ledger.txt`):
/// only the in-image bridge decodes this while no app calls the kind. The
/// first app caller must add an unknown arm in the same change.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum DeleteCardReply {
    /// The card row was deleted and a tombstone was written. `modseq` is the
    /// new `highestmodseq` of the address book.
    Deleted {
        /// Server-assigned 32-byte card id of the deleted row.
        #[serde(with = "serde_bytes")]
        card_id: Vec<u8>,
        /// New `highestmodseq` of the address book after the delete.
        modseq: i64,
    },
    /// No card exists for the given `uid_hash`, **or** the address book itself
    /// does not exist. The two paths are intentionally collapsed: callers must
    /// not distinguish a missing address book from a missing card (leaking
    /// which would let a caller probe whether an address book exists).
    NotFound,
    /// `if_match` was supplied but did not match the current etag.
    PreconditionFailed {
        /// The etag of the card row as it currently stands.
        current_etag: String,
    },
}

// ── Phase E.7 — delete_addressbook ────────────────────────────────────────────

/// `fauna.bridges.delete_addressbook` — delete a whole CardDAV address book and
/// **cascade-delete all its cards**. The MDA calls this in response to a WebDAV
/// DELETE on an address-book *collection* URL (`/carddav/{user}/{book}/`) —
/// distinct from `delete_card`, which DELETEs a single card resource. This is a
/// genuinely user-initiated destructive action (the *No user-data loss*
/// invariant permits explicit user deletes); the nest store performs the whole
/// cascade under one lock so a crash leaves either the entire book or nothing,
/// never a half-deleted book a client can't recover from (`carddav-server.md`
/// § Address-book collection model).
///
/// No `uid_hash` (the whole book, not one card) and no `if_match` (a
/// collection DELETE is unconditional; RFC 6352 clients do not send a
/// collection-level `If-Match`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeleteAddressbookRequest {
    /// 32-byte actor identifier.
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    /// 32-byte address-book identifier (client-assigned).
    #[serde(with = "serde_bytes")]
    pub addressbook_id: Vec<u8>,
}

/// Reply to `fauna.bridges.delete_addressbook`.
///
/// Ledger answer `locked` (`tools/check-additive-evolution/enum_ledger.txt`):
/// only the in-image bridge decodes this while no app calls the kind. The
/// first app caller must add an unknown arm in the same change.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum DeleteAddressbookReply {
    /// The address-book row and every card it held were deleted. `cards_deleted`
    /// is the number of card rows cascade-removed — observability only (`0` for
    /// an empty book). A non-optional `u32` (default `0`) keeps the dag-cbor
    /// wire flat: no nested `Option` (memory `dagcbor-nested-option-not-
    /// roundtrippable`).
    Deleted { cards_deleted: u32 },
    /// No address-book row exists for `(actor_id, addressbook_id)`. Idempotent
    /// re-delete — the MDA maps this to 404 (mirror `DeleteCardReply::NotFound`).
    NotFound,
}

// ── fauna.bridges.whoami — role / identity discovery ─────────────
//
// Sent by a fresh bridge process immediately after the WS-RPC
// handshake completes, to learn its role (MTA vs. MDA) and its
// deployment-time bridge identity without inferring them from argv.
// Replaces the retired `--mode` flag — the supervisor (s6/systemd)
// runs the binary with the right keypair file, and the binary asks
// nest who it is. Permitted regardless of resolved role since it is
// the call that resolves the role; subsequent calls flow through the
// regular allowlist gate.

/// Empty request — the calling actor is identified by the
/// authenticated WS-RPC connection's actor_id, not by anything on the
/// wire.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WhoamiRequest {}

/// Identity + role reply. `role` is `"mta"` or `"mda"`; `status` is
/// `"approved"` / `"pending"` / `"revoked"` (string for forward
/// compatibility with future states). `ed25519_pubkey_hex` round-trips
/// the actor_id so the caller can sanity-check the connection is
/// authenticated as the expected service-user pubkey;
/// `x25519_pubkey_hex` is useful for HPKE-unwrap sanity checking on
/// wrapped TLS / DKIM blobs (B.6).
///
/// Per-bridge domain is **not** carried on this reply — see
/// `docs/goal/behavior/mail-bridge-lifecycle.md § Wire shapes`. The
/// bridge learns its accept-RCPT-for-these-domains list from
/// `fauna.bridges.fetch_config`'s `local_domains` projection (a
/// derived view of the `mail_domains` table); the single-domain
/// anchor for TLS / DKIM / EHLO lives in the same reply's
/// `primary_domain` field.
///
/// `node_mode` is the nest's NAT axis (`"public"` / `"private"`,
/// string for forward-compat like `role`/`status`), sourced from the
/// nest's `[nest] mode` deployment-topology config. The MDA bridge
/// reads it at cold boot to pick its IMAP/CalDAV listener bind default:
/// a `"private"` nest defaults the bind to loopback rather than
/// all-interfaces, so a private-paired plaintext deployment never
/// silently exposes IMAP/CalDAV on a public interface
/// (`docs/goal/architecture/nest/deployment-home-with-public-relay.md`
/// § Plaintext-mode behavior + § Don't do these). The actual LAN bind
/// address remains a sanctioned operator-hatch / compose override (bind
/// address is OS-deployment topology, not nest state). An empty value
/// takes the bridge's plain non-private default (`resolveMDAListenAddrs`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WhoamiReply {
    pub role: String,
    pub bridge_id: String,
    pub status: String,
    pub ed25519_pubkey_hex: String,
    pub x25519_pubkey_hex: String,
    #[serde(default)]
    pub node_mode: String,
}

// ── Deliverability diagnostics + blocklist self-check ──────────────
// Admin-class `fauna.bridges.{run_deliverability_diagnostics,
// blocklist_self_check_run}` (docs/goal/behavior/mail-deliverability.md
// § Symptom diagnostics + § Blocklist self-check; § Wire shapes names the
// admin-client caller — these are admin-pane-only, NOT MTA-class). The
// nest orchestrator runs the checks over its DNS / STARTTLS seams and the
// shared `fauna_mail::deliverability` verdicts, then returns these rows.

/// One row of the deliverability diagnostic checklist
/// (`mail-deliverability.md` § Symptom diagnostics → Output rendering). `status`
/// is `"pass" | "warn" | "fail"`; `detail` is the plain-language pass note or
/// failure reason the admin reads.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct DiagnosticCheckResult {
    pub name: String,
    pub status: String,
    pub detail: String,
}

/// `fauna.bridges.run_deliverability_diagnostics` request — no parameters
/// (the deployment's primary domain + expected records come from nest state).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct RunDeliverabilityDiagnosticsRequest {}

/// The diagnostic checklist + the Unix-seconds the run completed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct RunDeliverabilityDiagnosticsReply {
    pub checks: Vec<DiagnosticCheckResult>,
    pub ran_at: i64,
}

/// One DNSBL's outcome in the blocklist self-check
/// (`mail-deliverability.md` § Blocklist self-check → results_json). `listed` =
/// the IP is on this DNSBL; `reason` = the TXT-record reason if any; `error` =
/// a resolver error (timeout/SERVFAIL → the yellow "?" state) — empty on a clean
/// listed/not-listed answer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct BlocklistServerResult {
    pub server: String,
    pub listed: bool,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub error: String,
}

/// `fauna.bridges.blocklist_self_check_run` request. `server = Some(x)` force-
/// refreshes a single DNSBL; `None` checks the whole configured set.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct BlocklistSelfCheckRunRequest {
    #[serde(default)]
    pub server: Option<String>,
}

/// The per-DNSBL results + the checked outbound IP (string form, empty if it
/// couldn't be resolved) + the Unix-seconds the check ran.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct BlocklistSelfCheckRunReply {
    pub checked_at: i64,
    #[serde(default)]
    pub outbound_ip: String,
    pub results: Vec<BlocklistServerResult>,
}

/// `fauna.bridges.outbound_warmup_status` request — no parameters (the warm-up
/// state is deployment-wide). `mail-deliverability.md` § Wire shapes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct OutboundWarmupStatusRequest {}

/// The deployment-wide fresh-IP warm-up state (`mail-deliverability.md`
/// § Fresh-IP warm-up + § Wire shapes). Also the reply for
/// `fauna.bridges.outbound_warmup_reset` ("() → updated state"). Timestamps are
/// Unix seconds; `0` encodes "never" for `first_outbound_at` / `last_reset_at`.
/// `today_max = None` is the day-30+ unlimited state (no warm-up cap).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct OutboundWarmupStatusReply {
    /// 1-based day since the deployment's first outbound mail.
    pub current_day: i64,
    /// Recipients counted against today's cap so far.
    pub today_used: i64,
    /// Today's cap; `None` = unlimited (warm-up complete, day ≥ 30).
    #[serde(default)]
    pub today_max: Option<i64>,
    /// 00:00-UTC epoch-secs the ramp completes (day 30); `0` if no outbound yet.
    pub ramp_end_date: i64,
    /// Lifetime outbound counter (admin interest; preserved across reset).
    pub lifetime_total: i64,
    /// Epoch-secs of the first ever outbound mail; `0` if none yet.
    pub first_outbound_at: i64,
    /// Epoch-secs of the last admin manual reset; `0` if never.
    pub last_reset_at: i64,
}

/// `fauna.bridges.outbound_warmup_reset` request — no parameters. Restarts the
/// ramp at day 1 (after a deployment IP change); the reply is an
/// [`OutboundWarmupStatusReply`] of the post-reset state. § Manual reset.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct OutboundWarmupResetRequest {}

// ── The mail health readout (Admin) ───────────────────────────────────────────
// `mail-deliverability.md` § The mail health readout: one server-side fold
// (`fauna_mail::health`) over facts the nest holds, returned by the Admin-class
// `fauna.bridges.mail_health` `()` → [`MailHealthReply`]. Additive and
// `#[serde(default)]` throughout; `state` is an OPEN string enum rendered through
// `fauna_core::format::mail_health_state_label` (unknown → "needs attention").

/// `fauna.bridges.mail_health` request — no parameters (deployment-wide).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct MailHealthRequest {}

/// One row of the readout's `admin-mail-health-check` component. `label_key` is
/// the row's i18n key; `state` is `pass` / `warn` / `fail` / `info` (open —
/// `fauna_core::format::mail_health_check_state_label`); `detail` a short
/// plain-language fact.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct MailHealthCheck {
    #[serde(default)]
    pub label_key: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub detail: String,
    /// Forward-compat catch-all (transport.md § Schema and forward-compat
    /// discipline, rule 4) — an app-callable kind, so client↔nest wire.
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The mail health readout. `checks` carries the seven rows in the fixed order
/// (bridge connection · blocklist self-check · outbound queue · DNS/auth
/// records · warm-up ramp · last delivered · last received). The two heartbeat
/// stamps are Unix seconds (`None` = never) and are facts, never a state input.
/// `delist_url` is the de-listing page of the first DNSBL listing the outbound IP
/// — present only while one does.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct MailHealthReply {
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub checks: Vec<MailHealthCheck>,
    #[serde(default)]
    pub last_outbound_delivered_at: Option<i64>,
    #[serde(default)]
    pub last_inbound_accepted_at: Option<i64>,
    #[serde(default)]
    pub delist_url: Option<String>,
    /// Forward-compat catch-all (transport.md § Schema and forward-compat
    /// discipline, rule 4) — an app-callable kind, so client↔nest wire.
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── Deliverability history / observability reads (Admin) ──────────────────────
// `mail-deliverability.md` § Wire shapes (the `list_*` rows) + § Admin-visible
// audit. The diagnostic + blocklist-self-check *write* halves (above) persist
// `mail_outbound_self_blocklist_check` / `mail_outbound_diagnostic_runs` rows;
// these reads return that 90-day-retained history to the admin pane. Each history
// row reuses the same structured verdict types as the live-run replies
// ([`BlocklistServerResult`] / [`DiagnosticCheckResult`]) — the stored
// `results_json` is exactly the serialized `Vec`, so the client renders typed
// rows off the wire with no per-app JSON parse.

/// `fauna.bridges.list_blocklist_self_check_history` request. `window_days` ≤ 0
/// ⇒ the full 90-day retention window. `mail-deliverability.md` § Wire shapes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ListBlocklistSelfCheckHistoryRequest {
    #[serde(default)]
    pub window_days: i64,
}

/// One persisted blocklist self-check (the 24h timer or an admin force-refresh),
/// newest-first in the history reply. `results` is the same per-DNSBL verdict
/// list a live `blocklist_self_check_run` returns.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct BlocklistSelfCheckHistoryRow {
    pub checked_at: i64,
    pub results: Vec<BlocklistServerResult>,
}

/// The blocklist-self-check history within the requested window, newest first.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ListBlocklistSelfCheckHistoryReply {
    pub rows: Vec<BlocklistSelfCheckHistoryRow>,
}

/// `fauna.bridges.list_deliverability_diagnostic_runs` request. `limit` ≤ 0 ⇒ a
/// default of 100 (capped at 1000). `mail-deliverability.md` § Wire shapes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ListDeliverabilityDiagnosticRunsRequest {
    #[serde(default)]
    pub limit: i64,
}

/// One persisted deliverability-diagnostic run (admin-on-demand), newest-first in
/// the history reply. `checks` is the same checklist a live
/// `run_deliverability_diagnostics` returns; `ran_by_actor_id` is the 32-byte
/// admin actor id that ran it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct DiagnosticRunHistoryRow {
    pub ran_at: i64,
    pub checks: Vec<DiagnosticCheckResult>,
    pub ran_by_actor_id: ByteBuf,
}

/// The diagnostic-run audit history, newest first, capped at the requested limit.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ListDeliverabilityDiagnosticRunsReply {
    pub rows: Vec<DiagnosticRunHistoryRow>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Value, decode_strict as decode, encode_canonical};
    use serde_bytes::ByteBuf;
    use std::collections::BTreeMap;

    // ── The two pre-existing mail write paths (`append` + inbound MTA
    //    ingest): the required dedup pair, and the additive `body_ref` ──────

    /// A field-less replica of the append request, exactly as a nest that does
    /// not know an additive field declares it (skew within a major version).
    /// Decoding a new-shape payload into it is the new-bridge → old-nest
    /// direction.
    #[derive(Deserialize)]
    struct OldAppendMessageRequest {
        #[serde(with = "serde_bytes")]
        #[allow(dead_code)]
        actor_id: Vec<u8>,
        mailbox: String,
        sender_domain: String,
    }

    #[test]
    fn append_request_dedup_key_round_trips() {
        let req = AppendMessageRequest {
            actor_id: vec![7u8; 32],
            mailbox: "INBOX".into(),
            flags: vec!["\\Seen".into()],
            encrypted_body: vec![1, 2, 3],
            encrypted_index_hint: vec![9],
            timestamp: 1_700_000_000,
            ciphertext_size: 3,
            sender_domain: "example.com".into(),
            dedup_key: "msgid:v1:a@example.com".into(),
            envelope_key: "env:v1:aa11".into(),
            body_ref: None,
        };
        let bytes = encode_canonical(&req).unwrap();
        assert_eq!(decode::<AppendMessageRequest>(&bytes).unwrap(), req);
    }

    #[test]
    fn append_body_ref_round_trips_and_is_omitted_when_the_body_rides_inline() {
        // The MDA-APPEND upward leg: an over-inline-budget *sealed* body crosses on
        // the byte plane by reference instead of `encrypted_body` (a plain
        // `MailBodyRef`, not the plaintext staged-envelope — APPEND is already
        // ciphertext). `ciphertext_size` stays the sealed length regardless.
        let by_ref = AppendMessageRequest {
            actor_id: vec![7u8; 32],
            mailbox: "INBOX".into(),
            encrypted_body: Vec::new(), // empty: the bytes are on the byte plane
            ciphertext_size: 9_000_000,
            body_ref: Some(MailBodyRef {
                chunk_hashes: vec![
                    ByteBuf::from(vec![0xAAu8; 32]),
                    ByteBuf::from(vec![0xBBu8; 32]),
                ],
                total_bytes: 9_000_000,
            }),
            ..Default::default()
        };
        let bytes = encode_canonical(&by_ref).unwrap();
        assert_eq!(decode::<AppendMessageRequest>(&bytes).unwrap(), by_ref);

        // Every message small enough to ride inline must encode to the
        // byte-identical pre-field shape — no `body_ref` key on the wire at all.
        let inline = AppendMessageRequest {
            actor_id: vec![7u8; 32],
            mailbox: "INBOX".into(),
            encrypted_body: vec![1, 2, 3],
            ciphertext_size: 3,
            ..Default::default()
        };
        let bytes = encode_canonical(&inline).unwrap();
        let map: BTreeMap<String, Value> = decode(&bytes).unwrap();
        assert!(
            !map.contains_key("body_ref"),
            "absent body_ref must not ride the wire as null"
        );
        assert_eq!(decode::<AppendMessageRequest>(&bytes).unwrap(), inline);
    }

    #[test]
    fn an_old_nest_tolerates_a_new_mdas_append_body_ref() {
        // A new MDA staging an over-budget APPEND body against an older nest: the
        // old nest ignores the unknown key rather than rejecting the APPEND (it
        // sees an empty body, which is why the *new* nest rejects
        // empty-body-with-no-ref outright; the MDA is version-locked to the nest
        // in the same image — transport.md rule 4).
        let req = AppendMessageRequest {
            actor_id: vec![7u8; 32],
            mailbox: "Archive".into(),
            sender_domain: "example.com".into(),
            encrypted_body: Vec::new(),
            body_ref: Some(MailBodyRef {
                chunk_hashes: vec![ByteBuf::from(vec![0xCCu8; 32])],
                total_bytes: 4_000_000,
            }),
            ..Default::default()
        };
        let bytes = encode_canonical(&req).unwrap();
        let old = decode::<OldAppendMessageRequest>(&bytes)
            .expect("an old nest must ignore the unknown body_ref, not reject the append");
        assert_eq!(old.mailbox, "Archive");
        assert_eq!(old.sender_domain, "example.com");
    }

    // ── The dedup pair (`mailbox-migration.md` § The envelope key confirms a
    //    Message-ID hit): `dedup_key` and `envelope_key` are required on all
    //    three producers' requests — there is no absent key ──

    #[test]
    fn a_request_without_either_key_is_refused_on_all_three() {
        // Every producer sends the pair; a request missing either half is
        // malformed rather than an unindexed message or a Message-ID-only
        // match.
        let append = AppendMessageRequest {
            actor_id: vec![7u8; 32],
            dedup_key: "msgid:v1:a@example.com".into(),
            envelope_key: "env:v1:ae6d".into(),
            ..Default::default()
        };
        let ingest = IngestInboundMailRequest {
            actor_id: vec![7u8; 32],
            dedup_key: "msgid:v1:a@example.com".into(),
            envelope_key: "env:v1:ae6d".into(),
            ..Default::default()
        };
        let item = ImportMessageItem {
            body: vec![1],
            body_size: 1,
            dedup_key: "msgid:v1:a@example.com".into(),
            envelope_key: "env:v1:ae6d".into(),
            ..Default::default()
        };
        for key in ["dedup_key", "envelope_key"] {
            let strip = |bytes: &[u8]| {
                let mut map: BTreeMap<String, Value> = decode(bytes).unwrap();
                assert!(map.remove(key).is_some());
                encode_canonical(&map).unwrap()
            };
            assert!(
                decode::<AppendMessageRequest>(&strip(&encode_canonical(&append).unwrap()))
                    .is_err(),
                "append without {key}"
            );
            assert!(
                decode::<IngestInboundMailRequest>(&strip(&encode_canonical(&ingest).unwrap()))
                    .is_err(),
                "ingest without {key}"
            );
            assert!(
                decode::<ImportMessageItem>(&strip(&encode_canonical(&item).unwrap())).is_err(),
                "import item without {key}"
            );
        }
    }

    #[test]
    fn the_dedup_pair_round_trips_on_all_three_requests() {
        let env = "env:v1:ae6d".to_string();
        let append = AppendMessageRequest {
            actor_id: vec![7u8; 32],
            dedup_key: "msgid:v1:a@example.com".into(),
            envelope_key: env.clone(),
            ..Default::default()
        };
        let ingest = IngestInboundMailRequest {
            actor_id: vec![7u8; 32],
            dedup_key: "msgid:v1:a@example.com".into(),
            envelope_key: env.clone(),
            ..Default::default()
        };
        let item = ImportMessageItem {
            body: vec![1],
            body_size: 1,
            dedup_key: "msgid:v1:a@example.com".into(),
            envelope_key: env,
            ..Default::default()
        };
        assert_eq!(
            decode::<AppendMessageRequest>(&encode_canonical(&append).unwrap()).unwrap(),
            append
        );
        assert_eq!(
            decode::<IngestInboundMailRequest>(&encode_canonical(&ingest).unwrap()).unwrap(),
            ingest
        );
        assert_eq!(
            decode::<ImportMessageItem>(&encode_canonical(&item).unwrap()).unwrap(),
            item
        );
    }

    #[test]
    fn ingest_inbound_mail_request_carries_both_dsn_facts_and_a_pre_field_bridge_decodes() {
        // The guardian mail gate's null-reverse-path correlation
        // (`family-safety.md` § The mail gate). Both fields ride together, both
        // are omitted when absent, and — the property that matters — a non-DSN
        // message decodes to `None`/`None`, which the gate reads as *no
        // correlation* and therefore **holds**. Fail-closed: it over-holds a
        // supervised ward's bounces, it never over-delivers.
        let with = IngestInboundMailRequest {
            actor_id: vec![7u8; 32],
            encrypted_body: vec![1, 2, 3],
            sender_address: String::new(), // the null reverse-path
            dsn_original_msgid: Some("<7f3a9c2e@fauna.test>".into()),
            dsn_report_addresses: vec!["mailer-daemon@remote.test".into()],
            ..Default::default()
        };
        let bytes = encode_canonical(&with).unwrap();
        assert_eq!(decode::<IngestInboundMailRequest>(&bytes).unwrap(), with);

        let pre_field = IngestInboundMailRequest {
            actor_id: vec![7u8; 32],
            encrypted_body: vec![1, 2, 3],
            ..Default::default()
        };
        let bytes = encode_canonical(&pre_field).unwrap();
        let map: BTreeMap<String, Value> = decode(&bytes).unwrap();
        assert!(!map.contains_key("dsn_original_msgid"));
        assert!(!map.contains_key("dsn_report_addresses"));
        let decoded = decode::<IngestInboundMailRequest>(&bytes).unwrap();
        assert_eq!(decoded.dsn_original_msgid, None);
        assert!(decoded.dsn_report_addresses.is_empty());
        assert_eq!(decoded, pre_field);
    }

    // ── Bulk-plane body references (smtp-server.md § Message size limits) ──

    #[test]
    fn ingest_body_ref_round_trips_and_is_omitted_when_the_body_rides_inline() {
        let by_ref = IngestInboundMailRequest {
            actor_id: vec![7u8; 32],
            encrypted_body: Vec::new(), // empty: the bytes are on the byte plane
            body_ref: Some(MailBodyRef {
                chunk_hashes: vec![
                    ByteBuf::from(vec![0xAAu8; 32]),
                    ByteBuf::from(vec![0xBBu8; 32]),
                ],
                total_bytes: 9_000_000,
            }),
            ..Default::default()
        };
        let bytes = encode_canonical(&by_ref).unwrap();
        assert_eq!(decode::<IngestInboundMailRequest>(&bytes).unwrap(), by_ref);

        // The overwhelming majority of mail still rides inline, and must encode to
        // the byte-identical pre-field shape — no `body_ref` key on the wire at all.
        let inline = IngestInboundMailRequest {
            actor_id: vec![7u8; 32],
            encrypted_body: vec![1, 2, 3],
            ..Default::default()
        };
        let bytes = encode_canonical(&inline).unwrap();
        let map: BTreeMap<String, Value> = decode(&bytes).unwrap();
        assert!(!map.contains_key("body_ref"));
        assert_eq!(decode::<IngestInboundMailRequest>(&bytes).unwrap(), inline);
    }

    #[test]
    fn an_old_nest_tolerates_a_new_mtas_body_ref() {
        // Skew direction that matters: a new MTA staging an over-budget body against
        // an older nest. The old nest ignores the unknown key rather than rejecting
        // the ingest — it simply sees an empty body, which is why the *new* nest
        // rejects empty-body-with-no-ref outright (an old nest cannot, so the MTA is
        // version-locked to the nest in the same image; see transport.md rule 4).
        #[derive(serde::Deserialize)]
        struct OldIngestInboundMailRequest {
            #[serde(with = "serde_bytes")]
            actor_id: Vec<u8>,
            #[serde(with = "serde_bytes")]
            encrypted_body: Vec<u8>,
        }

        let req = IngestInboundMailRequest {
            actor_id: vec![7u8; 32],
            encrypted_body: Vec::new(),
            body_ref: Some(MailBodyRef {
                chunk_hashes: vec![ByteBuf::from(vec![0xCCu8; 32])],
                total_bytes: 4_000_000,
            }),
            ..Default::default()
        };
        let bytes = encode_canonical(&req).unwrap();
        let old = decode::<OldIngestInboundMailRequest>(&bytes)
            .expect("an old nest must ignore the unknown body_ref, not reject the ingest");
        assert_eq!(old.actor_id, vec![7u8; 32]);
        assert!(old.encrypted_body.is_empty());
    }

    #[test]
    fn fetch_message_ciphertext_reply_carries_a_body_ref_only_for_an_over_budget_body() {
        let by_ref = FetchMessageCiphertextReply::Found {
            encrypted_body: Vec::new(),
            ciphertext_size: 9_000_000,
            internal_date: 1_700_000_000,
            body_ref: Some(MailBodyRef {
                chunk_hashes: vec![ByteBuf::from(vec![0xDDu8; 32])],
                total_bytes: 9_000_000,
            }),
            stored_at: 1_700_000_100,
        };
        let bytes = encode_canonical(&by_ref).unwrap();
        assert_eq!(
            decode::<FetchMessageCiphertextReply>(&bytes).unwrap(),
            by_ref
        );

        // An inline body carries no `body_ref` key at all; `stored_at` is
        // always present, the unknown `0` included.
        let inline = FetchMessageCiphertextReply::Found {
            encrypted_body: vec![1, 2, 3],
            ciphertext_size: 3,
            internal_date: 1_700_000_000,
            body_ref: None,
            stored_at: 0,
        };
        let bytes = encode_canonical(&inline).unwrap();
        let map: BTreeMap<String, Value> = decode(&bytes).unwrap();
        assert!(!map.contains_key("body_ref"));
        assert!(map.contains_key("stored_at"));
        assert_eq!(
            decode::<FetchMessageCiphertextReply>(&bytes).unwrap(),
            inline
        );
    }

    // ── Mailbox-migration import wire types ─────────────────────────

    #[test]
    fn import_message_request_round_trips() {
        let r = ImportMessageRequest {
            session_id: "3f1c9a2e-0000-4000-8000-000000000001".into(),
            message: ImportMessageItem {
                mailbox: "INBOX".into(),
                flags: vec!["\\Seen".into()],
                body: vec![1, 2, 3],
                timestamp: 1_700_000_000,
                body_size: 3,
                sender_domain: "example.com".into(),
                source_uid: 44,
                source_uid_validity: 7,
                dedup_key: "msgid:v1:a@example.com".into(),
                envelope_key: "env:v1:aa11".into(),
                staged_body: None,
            },
            skip_dedup: false,
        };
        let bytes = encode_canonical(&r).unwrap();
        let decoded: ImportMessageRequest = decode(&bytes).unwrap();
        assert_eq!(r, decoded);
    }

    #[test]
    fn import_message_item_staged_body_round_trips_and_is_omitted_when_inline() {
        // An over-inline-ceiling import rides a staged envelope: empty inline `body`,
        // the plaintext length in `body_size`, the ciphertext chunks + one-shot key in
        // `staged_body` (smtp-server.md § Message size limits, the staged-envelope rule).
        let staged = ImportMessageItem {
            mailbox: "INBOX".into(),
            body: Vec::new(),     // empty: the bytes are on the byte plane
            body_size: 6_000_000, // the PLAINTEXT length, not the sealed total
            dedup_key: "msgid:v1:big@example.com".into(),
            staged_body: Some(StagedBodyRef {
                chunk_hashes: vec![
                    ByteBuf::from(vec![0xAAu8; 32]),
                    ByteBuf::from(vec![0xBBu8; 32]),
                ],
                total_bytes: 6_000_040, // sealed (nonce + AEAD tag) length
                key: fauna_core::secret::SecretByteBuf::from(vec![0x11u8; 32]),
            }),
            ..Default::default()
        };
        let bytes = encode_canonical(&staged).unwrap();
        assert_eq!(decode::<ImportMessageItem>(&bytes).unwrap(), staged);

        // The overwhelming majority of imports ride inline and must encode to the
        // byte-identical pre-field shape — no `staged_body` key on the wire, so an
        // older nest decodes exactly what it saw before (the skew contract's premise).
        let inline = ImportMessageItem {
            mailbox: "INBOX".into(),
            body: vec![1, 2, 3],
            body_size: 3,
            dedup_key: "k".into(),
            ..Default::default()
        };
        let bytes = encode_canonical(&inline).unwrap();
        let map: BTreeMap<String, Value> = decode(&bytes).unwrap();
        assert!(!map.contains_key("staged_body"));
        assert_eq!(decode::<ImportMessageItem>(&bytes).unwrap(), inline);
    }

    #[test]
    fn import_message_outcome_variants_round_trip() {
        for outcome in [
            ImportMessageOutcome::Imported {
                message_id: vec![5u8; 32],
                uid: 3,
                uid_validity: 9,
            },
            ImportMessageOutcome::Skipped {
                reason: "dedup".into(),
            },
            ImportMessageOutcome::Errored {
                reason: "body_size mismatch".into(),
            },
        ] {
            let reply = ImportMessageReply {
                outcome: outcome.clone(),
                imported_count: 1,
                skipped_count: 2,
                errored_count: 3,
            };
            let bytes = encode_canonical(&reply).unwrap();
            let decoded: ImportMessageReply = decode(&bytes).unwrap();
            assert_eq!(reply, decoded);
        }
    }

    #[test]
    fn import_batch_request_round_trips_with_and_without_revised_total() {
        for revised in [None, Some(1234u64)] {
            let r = ImportMessageBatchRequest {
                session_id: "s".into(),
                messages: vec![ImportMessageItem {
                    mailbox: "Archive".into(),
                    body: vec![7],
                    body_size: 1,
                    dedup_key: "k".into(),
                    ..Default::default()
                }],
                skip_dedup: true,
                revised_total_count: revised,
            };
            let bytes = encode_canonical(&r).unwrap();
            let decoded: ImportMessageBatchRequest = decode(&bytes).unwrap();
            assert_eq!(r, decoded);
        }
    }

    #[test]
    fn import_session_info_and_actions_round_trip() {
        let info = ImportSessionInfo {
            session_id: "s".into(),
            source_descriptor: "gmail:imap.gmail.com:alice".into(),
            state: "paused".into(),
            started_at: 1_700_000_000,
            last_progress_at: 1_700_000_100,
            total_count: 50_000,
            imported_count: 10,
            skipped_count: 2,
            errored_count: 1,
            cursors: vec![ImportMailboxCursor {
                mailbox: "INBOX".into(),
                last_processed_source_uid: 99,
                source_uid_validity: 4,
            }],
            error_reason: String::new(),
            scope: vec!["INBOX".into(), "Sent".into()],
            date_from: "2023-11-14".into(),
            source_sealed: Some(ByteBuf::from(vec![0xA1u8; 48])),
            source_hash: Some(ByteBuf::from(vec![0xB2u8; 32])),
        };
        let reply = ImportSessionActionReply {
            session: info.clone(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: ImportSessionActionReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);

        let list = ListImportSessionsReply {
            sessions: vec![info],
        };
        let bytes = encode_canonical(&list).unwrap();
        let decoded: ListImportSessionsReply = decode(&bytes).unwrap();
        assert_eq!(list, decoded);
    }

    #[test]
    fn start_import_session_request_omits_scope_entirely_when_absent() {
        // `skip_serializing_if` → a request with no scope emits no `scope`
        // key, and decodes back to an empty scope
        // (mailbox-migration.md § Resume protocol: a session with no
        // selection just has no recorded scope to resume against — a degraded resume,
        // never a wire error).
        let req = StartImportSessionRequest {
            source_descriptor: "gmail:imap.gmail.com:alice".into(),
            total_count: 100,
            ..Default::default()
        };
        let bytes = encode_canonical(&req).unwrap();
        let map: BTreeMap<String, Value> = decode(&bytes).unwrap();
        assert!(
            !map.contains_key("scope"),
            "an empty scope must not ride the wire at all"
        );
        assert!(
            !map.contains_key("date_from"),
            "an unbounded import must not ride the wire either"
        );
        assert_eq!(decode::<StartImportSessionRequest>(&bytes).unwrap(), req);
    }

    #[test]
    fn an_old_nest_tolerates_a_new_start_import_session_scope() {
        #[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
        struct OldStartImportSessionRequest {
            source_descriptor: String,
            total_count: u64,
        }
        let req = StartImportSessionRequest {
            source_descriptor: "gmail:imap.gmail.com:alice".into(),
            total_count: 100,
            scope: vec!["INBOX".into(), "Sent".into()],
            ..Default::default()
        };
        let bytes = encode_canonical(&req).unwrap();
        let old = decode::<OldStartImportSessionRequest>(&bytes)
            .expect("an old nest must ignore the unknown scope, not reject the start");
        assert_eq!(old.source_descriptor, "gmail:imap.gmail.com:alice");
        assert_eq!(old.total_count, 100);
    }

    #[test]
    fn an_old_client_tolerates_a_reply_missing_scope() {
        // The reverse skew: a nest predating this field replies without
        // `scope` at all — the *new* client's struct must decode it to empty,
        // not fail.
        #[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
        struct OldImportSessionInfo {
            session_id: String,
            source_descriptor: String,
            state: String,
            started_at: i64,
            last_progress_at: i64,
            total_count: u64,
            imported_count: u64,
            skipped_count: u64,
            errored_count: u64,
            cursors: Vec<ImportMailboxCursor>,
            error_reason: String,
        }
        let old = OldImportSessionInfo {
            session_id: "s".into(),
            source_descriptor: "gmail:imap.gmail.com:alice".into(),
            state: "running".into(),
            ..Default::default()
        };
        let bytes = encode_canonical(&old).unwrap();
        let decoded = decode::<ImportSessionInfo>(&bytes)
            .expect("a new client must accept a scope-less reply, not reject it");
        assert_eq!(decoded.session_id, "s");
        assert!(decoded.scope.is_empty());
    }

    /// Bidirectional compat for the since date (`version-compatibility.md`:
    /// a client may be older *or* newer than the nest, so both skews round
    /// trip). The degrade is asymmetric in CONSEQUENCE, not in mechanism: an
    /// old nest that drops the date records a session whose resume will import
    /// everything the user excluded, which is exactly why the field exists —
    /// but a wire error would deny them the import altogether, so tolerating
    /// the skew is still right.
    #[test]
    fn the_import_since_date_round_trips_both_skews() {
        // Old nest ← new client: a request carrying the date must still decode
        // on a nest that has never heard of it.
        #[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
        struct PreDateStartRequest {
            source_descriptor: String,
            total_count: u64,
            #[serde(default)]
            scope: Vec<String>,
        }
        let bytes = encode_canonical(&StartImportSessionRequest {
            source_descriptor: "gmail:imap.gmail.com:alice".into(),
            total_count: 7,
            scope: vec!["INBOX".into()],
            date_from: "2023-11-14".into(),
            ..Default::default()
        })
        .unwrap();
        let old: PreDateStartRequest =
            decode(&bytes).expect("an old nest must ignore the added date, not reject the start");
        assert_eq!(old.total_count, 7);
        assert_eq!(old.scope, vec!["INBOX".to_string()]);

        // Old nest → new client: a reply predating the field decodes to the
        // empty string, which this surface reads as unbounded.
        #[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
        struct PreDateSessionInfo {
            session_id: String,
            source_descriptor: String,
            state: String,
            started_at: i64,
            last_progress_at: i64,
            total_count: u64,
            imported_count: u64,
            skipped_count: u64,
            errored_count: u64,
            cursors: Vec<ImportMailboxCursor>,
            error_reason: String,
            #[serde(default)]
            scope: Vec<String>,
        }
        let bytes = encode_canonical(&PreDateSessionInfo {
            session_id: "s".into(),
            state: "running".into(),
            scope: vec!["INBOX".into()],
            ..Default::default()
        })
        .unwrap();
        let decoded = decode::<ImportSessionInfo>(&bytes)
            .expect("a new client must accept a date-less reply, not reject it");
        assert_eq!(decoded.scope, vec!["INBOX".to_string()]);
        assert!(decoded.date_from.is_empty());
    }

    /// Bidirectional compat for the sealed-source pair (`version-compatibility.md`:
    /// a client may be older *or* newer than the nest and talk to several nests
    /// at once, so both skews must round-trip, not just the one we happened to
    /// ship first).
    #[test]
    fn the_sealed_source_pair_round_trips_both_skews() {
        // Old nest → new client: a reply predating the pair decodes to `None`,
        // and the client falls back to the plaintext the old nest still sends.
        #[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
        struct PreSealImportSessionInfo {
            session_id: String,
            source_descriptor: String,
            state: String,
            started_at: i64,
            last_progress_at: i64,
            total_count: u64,
            imported_count: u64,
            skipped_count: u64,
            errored_count: u64,
            cursors: Vec<ImportMailboxCursor>,
            error_reason: String,
        }
        let bytes = encode_canonical(&PreSealImportSessionInfo {
            session_id: "s".into(),
            source_descriptor: "gmail:imap.gmail.com:alice".into(),
            state: "running".into(),
            ..Default::default()
        })
        .unwrap();
        let decoded = decode::<ImportSessionInfo>(&bytes)
            .expect("a new client must accept a seal-less reply, not reject it");
        assert!(decoded.source_sealed.is_none());
        assert!(decoded.source_hash.is_none());
        assert_eq!(decoded.source_descriptor, "gmail:imap.gmail.com:alice");

        // Old nest ← new client: a request carrying the seal must still decode
        // on a nest that has never heard of the field.
        #[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
        struct PreSealStartRequest {
            source_descriptor: String,
            total_count: u64,
            #[serde(default)]
            scope: Vec<String>,
        }
        let bytes = encode_canonical(&StartImportSessionRequest {
            source_descriptor: "gmail:imap.gmail.com:alice".into(),
            total_count: 7,
            scope: vec!["INBOX".into()],
            source_sealed: Some(ByteBuf::from(vec![0xC3u8; 40])),
            ..Default::default()
        })
        .unwrap();
        let old: PreSealStartRequest =
            decode(&bytes).expect("an old nest must ignore the added seal, not reject the request");
        assert_eq!(old.total_count, 7);
        assert_eq!(old.scope, vec!["INBOX".to_string()]);
    }

    /// A keyless client's `None` must be **absent from the wire**, not an empty
    /// blob: an empty `source_sealed` rests as a non-NULL column that opens to
    /// nothing, which would let the nest's graduated scrub arm destroy the
    /// plaintext on behalf of a seal nobody can use.
    #[test]
    fn a_sealless_start_request_omits_the_field_entirely() {
        let with_seal = encode_canonical(&StartImportSessionRequest {
            source_descriptor: "s".into(),
            total_count: 1,
            source_sealed: Some(ByteBuf::from(vec![0u8; 1])),
            ..Default::default()
        })
        .unwrap();
        let without = encode_canonical(&StartImportSessionRequest {
            source_descriptor: "s".into(),
            total_count: 1,
            ..Default::default()
        })
        .unwrap();
        assert!(
            without.len() < with_seal.len(),
            "None must be skipped, not serialized as an empty byte string"
        );
        let decoded: StartImportSessionRequest = decode(&without).unwrap();
        assert!(decoded.source_sealed.is_none());
    }

    #[test]
    fn import_push_payloads_round_trip() {
        let p = BridgeImportProgressPush {
            session_id: "s".into(),
            imported_count: 5,
            skipped_count: 1,
            errored_count: 0,
        };
        let bytes = encode_canonical(&p).unwrap();
        let decoded: BridgeImportProgressPush = decode(&bytes).unwrap();
        assert_eq!(p, decoded);

        let e = BridgeImportErrorPush {
            session_id: "s".into(),
            reason: "auth_failed".into(),
        };
        let bytes = encode_canonical(&e).unwrap();
        let decoded: BridgeImportErrorPush = decode(&bytes).unwrap();
        assert_eq!(e, decoded);

        let c = BridgeImportCompletePush {
            session_id: "s".into(),
            imported_count: 5,
            skipped_count: 1,
            errored_count: 0,
        };
        let bytes = encode_canonical(&c).unwrap();
        let decoded: BridgeImportCompletePush = decode(&bytes).unwrap();
        assert_eq!(c, decoded);
    }

    #[test]
    fn validate_recipient_request_round_trips() {
        let r = ValidateRecipientRequest {
            local_part: "alice".into(),
            domain: "example.com".into(),
        };
        let bytes = encode_canonical(&r).unwrap();
        let decoded: ValidateRecipientRequest = decode(&bytes).unwrap();
        assert_eq!(r, decoded);
    }

    #[test]
    fn mail_domain_row_round_trips_with_bytes_and_algorithms() {
        let r = MailDomainRow {
            domain_id: ByteBuf::from(vec![7u8; 16]),
            domain_name: "example.com".into(),
            is_primary: true,
            added_at: 1_700_000_000,
            removed_at: None,
            restored_at: None,
            dkim_selector: Some("default".into()),
            dkim_rotation_days: Some(90),
            dkim_algorithms: vec!["ed25519".into(), "rsa-2048".into()],
            mta_sts_mode: "enforce".into(),
            mta_sts_max_age_seconds: 86400,
            mta_sts_cert_mode: "expand_primary".into(),
            catch_all_actor_id: Some(ByteBuf::from(vec![9u8; 32])),
            role_address_overrides: RoleAddressOverrides {
                postmaster: Some("ab".repeat(32)),
                ..Default::default()
            },
            dmarc_overrides: DmarcOverrides {
                policy_mode: Some(DmarcMode::None),
                pct: Some(50),
                ..Default::default()
            },
            spf_record: "v=spf1 mx ~all".into(),
            dkim_selector_activated_at: Some(1_700_000_500),
            dkim_rotation_due: true,
            catch_all_cleared_by_succession_at: Some(1_700_000_600),
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<MailDomainRow>(&bytes).unwrap());
    }

    /// The override maps ride the admin-mail wire typed, not as JSON text: an
    /// app reads each role's actor and each DMARC knob as a field.
    #[test]
    fn mail_domain_row_carries_the_overrides_typed() {
        let r = MailDomainRow {
            role_address_overrides: RoleAddressOverrides {
                abuse: Some("cd".repeat(32)),
                ..Default::default()
            },
            dmarc_overrides: DmarcOverrides {
                adkim_mode: Some(DmarcAlignment::Relaxed),
                fo_mode: Some(DmarcForensicOptions::Dkim),
                ..Default::default()
            },
            ..Default::default()
        };
        let back: MailDomainRow = decode(&encode_canonical(&r).unwrap()).unwrap();
        assert_eq!(
            back.role_address_overrides.abuse.as_deref(),
            Some("cd".repeat(32).as_str())
        );
        assert_eq!(
            back.role_address_overrides.resolve("abuse"),
            Some([0xcd; 32])
        );
        assert_eq!(back.role_address_overrides.resolve("tlsrpt"), None);
        assert_eq!(
            back.dmarc_overrides.adkim_mode,
            Some(DmarcAlignment::Relaxed)
        );
        assert_eq!(
            back.dmarc_overrides.fo_mode,
            Some(DmarcForensicOptions::Dkim)
        );
        assert!(back.dmarc_overrides.policy_mode.is_none());
    }

    /// The DMARC knobs keep their RFC 7489 tag spellings on every encoding —
    /// the JSON object at rest (`dmarc-reporting.md` § Multi-domain
    /// deployments) and the wire alike.
    #[test]
    fn dmarc_override_values_keep_their_rfc_spellings() {
        for (mode, tag) in [
            (DmarcMode::None, "none"),
            (DmarcMode::Quarantine, "quarantine"),
            (DmarcMode::Reject, "reject"),
        ] {
            assert_eq!(mode.as_tag(), tag);
        }
        assert_eq!(DmarcAlignment::Strict.as_tag(), "s");
        assert_eq!(DmarcAlignment::Relaxed.as_tag(), "r");
        assert_eq!(DmarcForensicOptions::AllFail.as_tag(), "0");
        assert_eq!(DmarcForensicOptions::AnyFailure.as_tag(), "1");
        assert_eq!(DmarcForensicOptions::Dkim.as_tag(), "d");
        assert_eq!(DmarcForensicOptions::Spf.as_tag(), "s");
        let ov = DmarcOverrides {
            policy_mode: Some(DmarcMode::Quarantine),
            ..Default::default()
        };
        let back: DmarcOverrides = decode(&encode_canonical(&ov).unwrap()).unwrap();
        assert_eq!(back, ov);
    }

    #[test]
    fn mail_domain_rename_row_round_trips_with_post_flip_fields() {
        // A row with the post-flip fields populated (a grace-state row) — proves
        // the full lifecycle shape round-trips, not just the slice-1 subset.
        let r = MailDomainRenameRow {
            rename_id: ByteBuf::from(vec![1u8; 16]),
            old_primary_domain_id: ByteBuf::from(vec![2u8; 16]),
            new_primary_domain_id: ByteBuf::from(vec![3u8; 16]),
            state: "grace".into(),
            started_at: 1_700_000_000,
            grace_days: 7,
            cert_acquired_at: Some(1_700_000_100),
            new_cert_fingerprint: Some("sha256:deadbeef".into()),
            flipped_at: Some(1_700_000_200),
            grace_started_at: Some(1_700_000_200),
            grace_ends_at: Some(1_700_600_000),
            ready_to_complete_at: None,
            completed_at: None,
            aborted_at: None,
            abort_reason: None,
            initiated_by_actor_id: ByteBuf::from(vec![9u8; 32]),
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<MailDomainRenameRow>(&bytes).unwrap());
    }

    #[test]
    fn start_primary_domain_rename_request_round_trips_with_and_without_grace() {
        let with = StartPrimaryDomainRenameRequest {
            new_primary_domain_id: ByteBuf::from(vec![4u8; 16]),
            grace_days: Some(14),
        };
        let bytes = encode_canonical(&with).unwrap();
        assert_eq!(
            with,
            decode::<StartPrimaryDomainRenameRequest>(&bytes).unwrap()
        );

        let without = StartPrimaryDomainRenameRequest {
            new_primary_domain_id: ByteBuf::from(vec![4u8; 16]),
            grace_days: None,
        };
        let bytes = encode_canonical(&without).unwrap();
        assert_eq!(
            without,
            decode::<StartPrimaryDomainRenameRequest>(&bytes).unwrap()
        );
    }

    #[test]
    fn get_primary_domain_rename_status_reply_round_trips_none_and_some() {
        let none = GetPrimaryDomainRenameStatusReply { rename: None };
        let bytes = encode_canonical(&none).unwrap();
        assert_eq!(
            none,
            decode::<GetPrimaryDomainRenameStatusReply>(&bytes).unwrap()
        );

        let some = GetPrimaryDomainRenameStatusReply {
            rename: Some(MailDomainRenameRow {
                rename_id: ByteBuf::from(vec![1u8; 16]),
                old_primary_domain_id: ByteBuf::from(vec![2u8; 16]),
                new_primary_domain_id: ByteBuf::from(vec![3u8; 16]),
                state: "requested".into(),
                started_at: 42,
                grace_days: 7,
                initiated_by_actor_id: ByteBuf::from(vec![9u8; 32]),
                ..Default::default()
            }),
        };
        let bytes = encode_canonical(&some).unwrap();
        assert_eq!(
            some,
            decode::<GetPrimaryDomainRenameStatusReply>(&bytes).unwrap()
        );
    }

    #[test]
    fn list_primary_domain_renames_reply_round_trips() {
        let r = ListPrimaryDomainRenamesReply {
            renames: vec![MailDomainRenameRow {
                rename_id: ByteBuf::from(vec![1u8; 16]),
                old_primary_domain_id: ByteBuf::from(vec![2u8; 16]),
                new_primary_domain_id: ByteBuf::from(vec![3u8; 16]),
                state: "aborted".into(),
                started_at: 42,
                grace_days: 7,
                aborted_at: Some(99),
                abort_reason: Some("nope".into()),
                initiated_by_actor_id: ByteBuf::from(vec![9u8; 32]),
                ..Default::default()
            }],
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<ListPrimaryDomainRenamesReply>(&bytes).unwrap());
    }

    #[test]
    fn abort_primary_domain_rename_request_round_trips() {
        let r = AbortPrimaryDomainRenameRequest {
            rename_id: ByteBuf::from(vec![5u8; 16]),
            abort_reason: Some("changed my mind".into()),
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(
            r,
            decode::<AbortPrimaryDomainRenameRequest>(&bytes).unwrap()
        );
    }

    #[test]
    fn complete_primary_domain_rename_request_round_trips_with_and_without_force() {
        let forced = CompletePrimaryDomainRenameRequest {
            rename_id: ByteBuf::from(vec![6u8; 16]),
            force: Some(true),
        };
        let bytes = encode_canonical(&forced).unwrap();
        assert_eq!(
            forced,
            decode::<CompletePrimaryDomainRenameRequest>(&bytes).unwrap()
        );

        let unforced = CompletePrimaryDomainRenameRequest {
            rename_id: ByteBuf::from(vec![6u8; 16]),
            force: None,
        };
        let bytes = encode_canonical(&unforced).unwrap();
        assert_eq!(
            unforced,
            decode::<CompletePrimaryDomainRenameRequest>(&bytes).unwrap()
        );
    }

    #[test]
    fn extend_primary_domain_rename_grace_request_round_trips() {
        let r = ExtendPrimaryDomainRenameGraceRequest {
            rename_id: ByteBuf::from(vec![7u8; 16]),
            additional_days: 5,
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(
            r,
            decode::<ExtendPrimaryDomainRenameGraceRequest>(&bytes).unwrap()
        );
    }

    #[test]
    fn add_local_domain_request_round_trips_with_and_without_catch_all() {
        let with = AddLocalDomainRequest {
            domain: "example.com".into(),
            mta_sts_cert_mode: "expand_primary".into(),
            catch_all_actor: Some(ByteBuf::from(vec![1u8; 32])),
            dkim_selector_override: Some("s1".into()),
        };
        let bytes = encode_canonical(&with).unwrap();
        assert_eq!(with, decode::<AddLocalDomainRequest>(&bytes).unwrap());

        // `None` catch_all must stay `None` (single-level Option is
        // CBOR-safe; the absence is the only `null` we expect).
        let without = AddLocalDomainRequest {
            domain: "two.example".into(),
            mta_sts_cert_mode: "wildcard".into(),
            catch_all_actor: None,
            dkim_selector_override: None,
        };
        let bytes = encode_canonical(&without).unwrap();
        let decoded = decode::<AddLocalDomainRequest>(&bytes).unwrap();
        assert_eq!(without, decoded);
        assert!(decoded.catch_all_actor.is_none());
    }

    #[test]
    fn list_local_domains_reply_and_empty_request_round_trip() {
        let req = ListLocalDomainsRequest {};
        let bytes = encode_canonical(&req).unwrap();
        assert_eq!(req, decode::<ListLocalDomainsRequest>(&bytes).unwrap());

        let reply = ListLocalDomainsReply {
            active: vec![MailDomainRow {
                domain_name: "example.com".into(),
                is_primary: true,
                dkim_algorithms: vec!["ed25519".into()],
                ..Default::default()
            }],
            soft_deleted_within_30d: vec![],
        };
        let bytes = encode_canonical(&reply).unwrap();
        assert_eq!(reply, decode::<ListLocalDomainsReply>(&bytes).unwrap());
    }

    #[test]
    fn update_local_domain_config_request_partial_round_trips() {
        let req = UpdateLocalDomainConfigRequest {
            domain: "example.com".into(),
            spf_record: Some("v=spf1 mx -all".into()),
            ..Default::default()
        };
        let bytes = encode_canonical(&req).unwrap();
        assert_eq!(
            req,
            decode::<UpdateLocalDomainConfigRequest>(&bytes).unwrap()
        );
    }

    #[test]
    fn update_local_domain_config_dmarc_policy_mode_is_additive() {
        // The pre-field request shape, as an older client sends it / an older
        // nest decodes it (version-compatibility.md — additive wire).
        #[derive(Debug, Serialize, Deserialize, PartialEq)]
        struct OlderRequest {
            domain: String,
            #[serde(default)]
            mta_sts_max_age_seconds: Option<i64>,
            #[serde(default)]
            mta_sts_cert_mode: Option<String>,
            #[serde(default)]
            spf_record: Option<String>,
        }
        let req = UpdateLocalDomainConfigRequest {
            domain: "example.com".into(),
            dmarc_policy_mode: Some(DmarcMode::Quarantine),
            ..Default::default()
        };
        let bytes = encode_canonical(&req).unwrap();
        assert_eq!(
            req,
            decode::<UpdateLocalDomainConfigRequest>(&bytes).unwrap()
        );
        // An older nest ignores the field.
        assert_eq!(
            decode::<OlderRequest>(&bytes).unwrap().domain,
            "example.com"
        );
        // An older client's request decodes as "unchanged".
        let older = OlderRequest {
            domain: "example.com".into(),
            mta_sts_max_age_seconds: None,
            mta_sts_cert_mode: None,
            spf_record: None,
        };
        let decoded =
            decode::<UpdateLocalDomainConfigRequest>(&encode_canonical(&older).unwrap()).unwrap();
        assert_eq!(decoded.dmarc_policy_mode, None);
    }

    #[test]
    fn set_catch_all_actor_request_round_trips_set_and_clear() {
        // Designate.
        let set = SetCatchAllActorRequest {
            domain: "example.com".into(),
            actor_id: Some(ByteBuf::from(vec![7u8; 32])),
        };
        let bytes = encode_canonical(&set).unwrap();
        assert_eq!(set, decode::<SetCatchAllActorRequest>(&bytes).unwrap());

        // Clear (None) — distinct on the wire from a set, since the field is a
        // single Option (no Option<Option> null-collision).
        let clear = SetCatchAllActorRequest {
            domain: "example.com".into(),
            actor_id: None,
        };
        let bytes = encode_canonical(&clear).unwrap();
        let decoded = decode::<SetCatchAllActorRequest>(&bytes).unwrap();
        assert_eq!(clear, decoded);
        assert!(decoded.actor_id.is_none());
    }

    #[test]
    fn set_dkim_rotation_days_request_round_trips_set_and_clear() {
        // Set an override.
        let set = SetDkimRotationDaysRequest {
            domain: "example.com".into(),
            rotation_days: Some(30),
        };
        let bytes = encode_canonical(&set).unwrap();
        assert_eq!(set, decode::<SetDkimRotationDaysRequest>(&bytes).unwrap());

        // Clear (None) — distinct from a set; single Option, no null-collision.
        let clear = SetDkimRotationDaysRequest {
            domain: "example.com".into(),
            rotation_days: None,
        };
        let bytes = encode_canonical(&clear).unwrap();
        let decoded = decode::<SetDkimRotationDaysRequest>(&bytes).unwrap();
        assert_eq!(clear, decoded);
        assert!(decoded.rotation_days.is_none());
    }

    #[test]
    fn set_role_address_request_round_trips_set_and_clear_each_role() {
        for role in [
            RoleAddressKind::Postmaster,
            RoleAddressKind::Abuse,
            RoleAddressKind::Noc,
            RoleAddressKind::Security,
        ] {
            // Designate.
            let set = SetRoleAddressRequest {
                domain: "example.com".into(),
                role,
                actor_id: Some(ByteBuf::from(vec![7u8; 32])),
            };
            let bytes = encode_canonical(&set).unwrap();
            assert_eq!(set, decode::<SetRoleAddressRequest>(&bytes).unwrap());

            // Clear (None) — distinct on the wire from a set (single Option, no
            // Option<Option> null-collision), exactly as catch-all.
            let clear = SetRoleAddressRequest {
                domain: "example.com".into(),
                role,
                actor_id: None,
            };
            let bytes = encode_canonical(&clear).unwrap();
            let decoded = decode::<SetRoleAddressRequest>(&bytes).unwrap();
            assert_eq!(clear, decoded);
            assert!(decoded.actor_id.is_none());
        }
    }

    #[test]
    fn role_address_kind_storage_keys_match_overridable_roles() {
        assert_eq!(RoleAddressKind::Postmaster.as_storage_key(), "postmaster");
        assert_eq!(RoleAddressKind::Abuse.as_storage_key(), "abuse");
        assert_eq!(RoleAddressKind::Noc.as_storage_key(), "noc");
        assert_eq!(RoleAddressKind::Security.as_storage_key(), "security");
    }

    #[test]
    fn provision_self_signed_cert_round_trips_with_and_without_sans() {
        let with = ProvisionSelfSignedCertRequest {
            domain: "mail.example.com".into(),
            additional_dns_sans: vec!["example.com".into(), "smtp.example.com".into()],
        };
        let bytes = encode_canonical(&with).unwrap();
        assert_eq!(
            with,
            decode::<ProvisionSelfSignedCertRequest>(&bytes).unwrap()
        );

        // Empty SAN list (the common case) must round-trip as an empty Vec.
        let without = ProvisionSelfSignedCertRequest {
            domain: "example.com".into(),
            additional_dns_sans: vec![],
        };
        let bytes = encode_canonical(&without).unwrap();
        let decoded = decode::<ProvisionSelfSignedCertRequest>(&bytes).unwrap();
        assert_eq!(without, decoded);
        assert!(decoded.additional_dns_sans.is_empty());

        let reply = ProvisionSelfSignedCertReply {
            bridges_sealed_to: vec![SealedBridgeInfo {
                role: "mta".into(),
                bridge_id: "mta-1".into(),
            }],
            bridges_skipped_no_x25519: vec![SealedBridgeInfo {
                role: "mda".into(),
                bridge_id: "mda-1".into(),
            }],
            expires_at_unix: 1_900_000_000,
        };
        let bytes = encode_canonical(&reply).unwrap();
        assert_eq!(
            reply,
            decode::<ProvisionSelfSignedCertReply>(&bytes).unwrap()
        );
    }

    #[test]
    fn alias_row_round_trips_with_scaled_threshold_and_disposable_nones() {
        // A populated exact row: threshold present (scaled int, not float),
        // rate caps present, disposable-only fields absent.
        let row = AliasRow {
            alias_id: ByteBuf::from(vec![7u8; 16]),
            actor_id: ByteBuf::from(vec![3u8; 32]),
            local_domain: "example.com".into(),
            kind: "exact".into(),
            pattern: "bob.smith".into(),
            forward_target: None,
            label: "work".into(),
            disabled: false,
            spam_threshold_override: Some(8),
            rate_limit_per_hour: Some(100),
            rate_limit_per_day: Some(500),
            uses_remaining: None,
            expires_at: None,
            created_at: 1_700_000_000_000,
            last_hit_at: Some(1_700_000_500_000),
            hit_count: 42,
            is_canonical: true,
        };
        let bytes = encode_canonical(&row).unwrap();
        let decoded = decode::<AliasRow>(&bytes).unwrap();
        assert_eq!(row, decoded);
        // Single-level Options stay distinct: the absent disposable fields
        // are the only `null`s we expect (no Option<Option> tri-state).
        assert!(decoded.uses_remaining.is_none());
        assert_eq!(decoded.spam_threshold_override, Some(8));

        // A bare row (every Option None, threshold inherited) round-trips too.
        let bare = AliasRow {
            local_domain: "d".into(),
            kind: "exact".into(),
            pattern: "x".into(),
            ..Default::default()
        };
        let bytes = encode_canonical(&bare).unwrap();
        assert_eq!(bare, decode::<AliasRow>(&bytes).unwrap());
    }

    #[test]
    fn create_account_alias_request_round_trips_with_and_without_controls() {
        let with = CreateAccountAliasRequest {
            kind: "exact".into(),
            local_domain: "example.com".into(),
            pattern: "b.smith".into(),
            controls: AliasControls {
                label: "banking".into(),
                spam_threshold_override: Some(3),
                rate_limit_per_hour: Some(10),
                rate_limit_per_day: None,
            },
        };
        let bytes = encode_canonical(&with).unwrap();
        assert_eq!(with, decode::<CreateAccountAliasRequest>(&bytes).unwrap());

        // Default controls (empty label, all-None) round-trip.
        let plain = CreateAccountAliasRequest {
            kind: "exact".into(),
            local_domain: "example.com".into(),
            pattern: "bob".into(),
            controls: AliasControls::default(),
        };
        let bytes = encode_canonical(&plain).unwrap();
        let decoded = decode::<CreateAccountAliasRequest>(&bytes).unwrap();
        assert_eq!(plain, decoded);
        assert!(decoded.controls.spam_threshold_override.is_none());
    }

    #[test]
    fn create_reply_and_list_round_trip() {
        let reply = CreateAccountAliasReply {
            alias_id: ByteBuf::from(vec![9u8; 16]),
        };
        let bytes = encode_canonical(&reply).unwrap();
        assert_eq!(reply, decode::<CreateAccountAliasReply>(&bytes).unwrap());

        let req = ListAccountAliasesRequest {};
        let bytes = encode_canonical(&req).unwrap();
        assert_eq!(req, decode::<ListAccountAliasesRequest>(&bytes).unwrap());

        let list = ListAccountAliasesReply {
            aliases: vec![AliasRow {
                kind: "exact".into(),
                pattern: "bob".into(),
                ..Default::default()
            }],
        };
        let bytes = encode_canonical(&list).unwrap();
        assert_eq!(list, decode::<ListAccountAliasesReply>(&bytes).unwrap());
    }

    #[test]
    fn import_account_aliases_reply_roundtrips() {
        let reply = ImportAccountAliasesReply {
            results: vec![
                ImportAliasOutcome {
                    line_index: 0,
                    address: "me-netflix@example.com".into(),
                    status: ImportAliasStatus::Created,
                    reason: None,
                },
                ImportAliasOutcome {
                    line_index: 2,
                    address: "bad@notmine.test".into(),
                    status: ImportAliasStatus::Invalid,
                    reason: Some("domain not local".into()),
                },
            ],
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: ImportAccountAliasesReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);

        // Request round-trips too (blank + multi-domain lines).
        let req = ImportAccountAliasesRequest {
            lines: vec![
                "me-netflix@example.com".into(),
                String::new(),
                "me-amazon@other.example".into(),
            ],
        };
        let bytes = encode_canonical(&req).unwrap();
        assert_eq!(req, decode::<ImportAccountAliasesRequest>(&bytes).unwrap());
    }

    #[test]
    fn resolve_recipient_reply_forward_round_trips() {
        let fwd = ResolveRecipientReply::Forward {
            forward_target: "oldaccount@example.com".into(),
            forwarder_actor_id: ByteBuf::from(vec![4u8; 32]),
        };
        let bytes = encode_canonical(&fwd).unwrap();
        assert_eq!(fwd, decode::<ResolveRecipientReply>(&bytes).unwrap());
    }

    #[test]
    fn forwarder_admin_rpcs_round_trip() {
        let create = CreateForwarderRequest {
            local_domain: "fauna.example".into(),
            pattern: "info".into(),
            forward_target: "oldaccount@example.com".into(),
        };
        let bytes = encode_canonical(&create).unwrap();
        assert_eq!(create, decode::<CreateForwarderRequest>(&bytes).unwrap());

        let created = CreateForwarderReply {
            alias_id: ByteBuf::from(vec![9u8; 16]),
        };
        let bytes = encode_canonical(&created).unwrap();
        assert_eq!(created, decode::<CreateForwarderReply>(&bytes).unwrap());

        let lreq = ListForwardersRequest {};
        let bytes = encode_canonical(&lreq).unwrap();
        assert_eq!(lreq, decode::<ListForwardersRequest>(&bytes).unwrap());

        // A forwarder row carries `forward_target`; other Options stay absent.
        let lreply = ListForwardersReply {
            forwarders: vec![AliasRow {
                kind: "forwarder".into(),
                pattern: "info".into(),
                local_domain: "fauna.example".into(),
                forward_target: Some("oldaccount@example.com".into()),
                ..Default::default()
            }],
        };
        let bytes = encode_canonical(&lreply).unwrap();
        let decoded = decode::<ListForwardersReply>(&bytes).unwrap();
        assert_eq!(lreply, decoded);
        assert_eq!(
            decoded.forwarders[0].forward_target.as_deref(),
            Some("oldaccount@example.com")
        );

        let del = DeleteForwarderRequest {
            alias_id: ByteBuf::from(vec![9u8; 16]),
        };
        let bytes = encode_canonical(&del).unwrap();
        assert_eq!(del, decode::<DeleteForwarderRequest>(&bytes).unwrap());
    }

    #[test]
    fn generate_disposable_alias_round_trips_with_and_without_overrides() {
        // Explicit overrides (incl. `uses = Some(0)` = unlimited).
        let with = GenerateDisposableAliasRequest {
            ttl_days: Some(7),
            uses: Some(0),
            label: "amazon".into(),
        };
        let bytes = encode_canonical(&with).unwrap();
        assert_eq!(
            with,
            decode::<GenerateDisposableAliasRequest>(&bytes).unwrap()
        );

        // All defaults (None / empty) round-trip.
        let bare = GenerateDisposableAliasRequest::default();
        let bytes = encode_canonical(&bare).unwrap();
        let decoded = decode::<GenerateDisposableAliasRequest>(&bytes).unwrap();
        assert_eq!(bare, decoded);
        assert!(decoded.ttl_days.is_none() && decoded.uses.is_none());

        let reply = GenerateDisposableAliasReply {
            alias_id: ByteBuf::from(vec![7u8; 16]),
            full_address: "bob-temp-a2b3c4@example.com".into(),
            token: "a2b3c4".into(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        assert_eq!(
            reply,
            decode::<GenerateDisposableAliasReply>(&bytes).unwrap()
        );
    }

    #[test]
    fn update_revoke_delete_alias_requests_round_trip() {
        let upd = UpdateAccountAliasRequest {
            alias_id: ByteBuf::from(vec![1u8; 16]),
            pattern: "bob.s".into(),
            controls: AliasControls {
                label: "renamed".into(),
                ..Default::default()
            },
        };
        let bytes = encode_canonical(&upd).unwrap();
        assert_eq!(upd, decode::<UpdateAccountAliasRequest>(&bytes).unwrap());

        let rev = RevokeAccountAliasRequest {
            alias_id: ByteBuf::from(vec![2u8; 16]),
        };
        let bytes = encode_canonical(&rev).unwrap();
        assert_eq!(rev, decode::<RevokeAccountAliasRequest>(&bytes).unwrap());

        let del = DeleteAccountAliasRequest {
            alias_id: ByteBuf::from(vec![3u8; 16]),
        };
        let bytes = encode_canonical(&del).unwrap();
        assert_eq!(del, decode::<DeleteAccountAliasRequest>(&bytes).unwrap());
    }

    #[test]
    fn validate_recipient_reply_resolved_round_trips() {
        // `is_role_address: true` exercises the field on the wire (a role
        // address resolved to the admin mailbox); the bridge maps it onto
        // the ingest request to bypass the per-mailbox quota pre-check.
        let r = ValidateRecipientReply::Resolved {
            actor_id: vec![7u8; 32],
            is_role_address: true,
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<ValidateRecipientReply>(&bytes).unwrap());
    }

    #[test]
    fn validate_recipient_reply_reject_round_trips() {
        let r = ValidateRecipientReply::Reject {
            reason: "no such recipient".into(),
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<ValidateRecipientReply>(&bytes).unwrap());
    }

    #[test]
    fn resolve_recipient_request_round_trips() {
        let r = ResolveRecipientRequest {
            local_part: "bob+work".into(),
            domain: "example.com".into(),
            sender_domain: "amazon.com".into(),
            sender_address: "seller@amazon.com".into(),
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<ResolveRecipientRequest>(&bytes).unwrap());
    }

    /// An absent `sender_address` (the null reverse-path) must decode to the
    /// empty address, which the guardian mail gate treats as system-generated
    /// mail and never refuses — it fails **open**, never bouncing legitimate
    /// mail (`family-safety.md` § The mail gate).
    #[test]
    fn resolve_recipient_request_decodes_without_sender_address() {
        #[derive(serde::Serialize)]
        struct WithoutSenderAddress {
            local_part: String,
            domain: String,
            sender_domain: String,
        }
        let bytes = encode_canonical(&WithoutSenderAddress {
            local_part: "bob".into(),
            domain: "example.com".into(),
            sender_domain: "amazon.com".into(),
        })
        .unwrap();
        let decoded = decode::<ResolveRecipientRequest>(&bytes).unwrap();
        assert_eq!(decoded.sender_address, "");
    }

    /// Same additive-compat guarantee on the ingest leg.
    #[test]
    fn ingest_inbound_mail_request_sender_address_round_trips_and_defaults_empty() {
        let mut r = IngestInboundMailRequest {
            actor_id: vec![1u8; 32],
            encrypted_body: vec![2, 3],
            encrypted_index_hint: vec![4],
            ..Default::default()
        };
        assert_eq!(r.sender_address, "", "absent by default");
        r.sender_address = "Stranger@Example.COM".into();
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(
            decode::<IngestInboundMailRequest>(&bytes)
                .unwrap()
                .sender_address,
            "Stranger@Example.COM",
            "the nest normalizes at the gate, not on the wire"
        );
    }

    #[test]
    fn resolve_recipient_request_sender_domain_defaults_when_absent() {
        // A request omitting `sender_domain`; `#[serde(default)]` decodes it
        // as empty. Encode the 2-field shape explicitly.
        #[derive(Serialize)]
        struct NoSenderDomainReq {
            local_part: String,
            domain: String,
        }
        let no_sender_domain = NoSenderDomainReq {
            local_part: "bob".into(),
            domain: "example.com".into(),
        };
        let bytes = encode_canonical(&no_sender_domain).unwrap();
        let decoded: ResolveRecipientRequest = decode(&bytes).unwrap();
        assert_eq!(decoded.local_part, "bob");
        assert_eq!(decoded.domain, "example.com");
        assert_eq!(decoded.sender_domain, "");
    }

    #[test]
    fn list_account_alias_hits_request_round_trips_with_and_without_cursor() {
        let with_cursor = ListAccountAliasHitsRequest {
            alias_id: ByteBuf::from(vec![4u8; 16]),
            limit: 100,
            before_hit_id: Some(ByteBuf::from(vec![9u8; 16])),
        };
        let bytes = encode_canonical(&with_cursor).unwrap();
        assert_eq!(
            with_cursor,
            decode::<ListAccountAliasHitsRequest>(&bytes).unwrap()
        );

        let first_page = ListAccountAliasHitsRequest {
            alias_id: ByteBuf::from(vec![4u8; 16]),
            limit: 50,
            before_hit_id: None,
        };
        let bytes = encode_canonical(&first_page).unwrap();
        assert_eq!(
            first_page,
            decode::<ListAccountAliasHitsRequest>(&bytes).unwrap()
        );
    }

    #[test]
    fn list_account_alias_hits_reply_round_trips() {
        let r = ListAccountAliasHitsReply {
            hits: vec![
                AliasHitRow {
                    hit_id: ByteBuf::from(vec![1u8; 16]),
                    matched_address: "bob-amazon@example.com".into(),
                    sender_domain: "amazon.com".into(),
                    received_at: 1_700_000_500,
                },
                AliasHitRow {
                    hit_id: ByteBuf::from(vec![2u8; 16]),
                    matched_address: "bob@example.com".into(),
                    sender_domain: String::new(),
                    received_at: 1_700_000_000,
                },
            ],
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<ListAccountAliasHitsReply>(&bytes).unwrap());

        // Empty page round-trips.
        let empty = ListAccountAliasHitsReply { hits: vec![] };
        let bytes = encode_canonical(&empty).unwrap();
        assert_eq!(empty, decode::<ListAccountAliasHitsReply>(&bytes).unwrap());
    }

    #[test]
    fn resolve_recipient_reply_resolved_round_trips() {
        // Headers + control overrides (with a set spam threshold) survive.
        let r = ResolveRecipientReply::Resolved {
            actor_id: ByteBuf::from(vec![7u8; 32]),
            headers_to_stamp: vec![StampedHeader {
                name: "X-Fauna-Address-Wildcard-Suffix".into(),
                value: "amazon".into(),
            }],
            control_overrides: AliasControls {
                spam_threshold_override: Some(8),
                rate_limit_per_hour: Some(100),
                ..Default::default()
            },
            is_role_address: false,
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<ResolveRecipientReply>(&bytes).unwrap());

        // Empty header list (exact-match shape) round-trips too.
        let exact = ResolveRecipientReply::Resolved {
            actor_id: ByteBuf::from(vec![1u8; 32]),
            headers_to_stamp: vec![],
            control_overrides: AliasControls::default(),
            is_role_address: false,
        };
        let bytes = encode_canonical(&exact).unwrap();
        assert_eq!(exact, decode::<ResolveRecipientReply>(&bytes).unwrap());

        // Role-address shape: routes to the admin, no headers, flag set.
        let role = ResolveRecipientReply::Resolved {
            actor_id: ByteBuf::from(vec![9u8; 32]),
            headers_to_stamp: vec![],
            control_overrides: AliasControls::default(),
            is_role_address: true,
        };
        let bytes = encode_canonical(&role).unwrap();
        assert_eq!(role, decode::<ResolveRecipientReply>(&bytes).unwrap());
    }

    #[test]
    fn resolve_recipient_reply_reject_round_trips() {
        let r = ResolveRecipientReply::Reject {
            smtp_code: 550,
            reason: "User unknown".into(),
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<ResolveRecipientReply>(&bytes).unwrap());
    }

    #[test]
    fn fetch_recipient_mls_pubkey_request_round_trips() {
        let r = FetchRecipientMlsPubkeyRequest {
            actor_id: vec![3u8; 32],
            mail_new_ingest: true,
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<FetchRecipientMlsPubkeyRequest>(&bytes).unwrap());
    }

    /// A request omitting `mail_new_ingest` (a non-mail seal site); the
    /// additive `#[serde(default)]` must decode it as `false` —
    /// the conservative, standing-key-only reading.
    #[test]
    fn fetch_recipient_mls_pubkey_request_without_mail_new_ingest_defaults_false() {
        #[derive(Serialize)]
        struct OldShape {
            #[serde(with = "serde_bytes")]
            actor_id: Vec<u8>,
        }
        let old = OldShape {
            actor_id: vec![3u8; 32],
        };
        let bytes = encode_canonical(&old).unwrap();
        let decoded: FetchRecipientMlsPubkeyRequest = decode(&bytes).unwrap();
        assert_eq!(decoded.actor_id, vec![3u8; 32]);
        assert!(!decoded.mail_new_ingest);
    }

    #[test]
    fn fetch_recipient_mls_pubkey_reply_some_round_trips() {
        // A key on file carries both halves: the X25519 pubkey and the
        // ML-KEM-768 ek (1184 B).
        let r = FetchRecipientMlsPubkeyReply {
            key: Some(RecipientSealKeyHalves {
                mls_pubkey: ByteBuf::from(vec![9u8; 32]),
                mlkem_ek: ByteBuf::from(vec![7u8; 1184]),
                extra: Default::default(),
            }),
            succession_pending: false,
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<FetchRecipientMlsPubkeyReply>(&bytes).unwrap());
    }

    #[test]
    fn fetch_recipient_mls_pubkey_reply_none_round_trips() {
        let r = FetchRecipientMlsPubkeyReply {
            key: None,
            succession_pending: false,
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<FetchRecipientMlsPubkeyReply>(&bytes).unwrap());
    }

    #[test]
    fn fetch_recipient_mls_pubkey_reply_succession_pending_round_trips() {
        // No pubkey yet, but this actor is a succession's successor — the
        // caller must tempfail, not bounce (smtp-server.md § Error /
        // tempfail strategy).
        let r = FetchRecipientMlsPubkeyReply {
            key: None,
            succession_pending: true,
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<FetchRecipientMlsPubkeyReply>(&bytes).unwrap());
    }

    #[test]
    fn fetch_recipient_mls_pubkey_reply_without_succession_pending_defaults_false() {
        // A reply for an actor not in succession omits this field; the
        // additive `#[serde(default)]` must decode it as `false` — the
        // conservative "never onboarded" reading (never widen to a tempfail
        // that was not intended).
        #[derive(Serialize)]
        struct OldShape {
            key: Option<RecipientSealKeyHalves>,
        }
        let old = OldShape { key: None };
        let bytes = encode_canonical(&old).unwrap();
        let decoded: FetchRecipientMlsPubkeyReply = decode(&bytes).unwrap();
        assert!(decoded.key.is_none());
        assert!(!decoded.succession_pending);
    }

    #[test]
    fn provision_recipient_mls_pubkey_request_round_trips() {
        let r = ProvisionRecipientMlsPubkeyRequest {
            actor_id: ByteBuf::from(vec![0x42u8; 32]),
            mls_pubkey: ByteBuf::from(vec![0xDDu8; 32]),
            mlkem_ek: ByteBuf::from(vec![0x55u8; 1184]),
            ..Default::default()
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(
            r,
            decode::<ProvisionRecipientMlsPubkeyRequest>(&bytes).unwrap()
        );
    }

    #[test]
    fn provision_recipient_mls_pubkey_request_epoch_keys_round_trip() {
        // The mail-epoch-schedule shape: a horizon of per-epoch public keys.
        // Absent (None) keeps the pre-epoch wire bytes identical
        // (skip_serializing_if), which the default-round-trip test pins.
        let r = ProvisionRecipientMlsPubkeyRequest {
            actor_id: ByteBuf::from(vec![0x42u8; 32]),
            mls_pubkey: ByteBuf::from(vec![0xDDu8; 32]),
            mlkem_ek: ByteBuf::from(vec![0x55u8; 1184]),
            epoch_keys: Some(vec![
                EpochSealKey {
                    epoch: 2958,
                    mls_pubkey: ByteBuf::from(vec![0x01u8; 32]),
                    mlkem_ek: ByteBuf::from(vec![0x02u8; 1184]),
                },
                EpochSealKey {
                    epoch: 2959,
                    mls_pubkey: ByteBuf::from(vec![0x03u8; 32]),
                    mlkem_ek: ByteBuf::from(vec![0x04u8; 1184]),
                },
            ]),
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(
            r,
            decode::<ProvisionRecipientMlsPubkeyRequest>(&bytes).unwrap()
        );
        // The None shape serializes WITHOUT the epoch_keys key.
        let old_shape = ProvisionRecipientMlsPubkeyRequest {
            actor_id: ByteBuf::from(vec![0x42u8; 32]),
            mls_pubkey: ByteBuf::from(vec![0xDDu8; 32]),
            ..Default::default()
        };
        let bytes = encode_canonical(&old_shape).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            !text.contains("epoch_keys"),
            "absent schedule must not emit the epoch_keys map key"
        );
    }

    #[test]
    fn provision_recipient_mls_pubkey_request_default_round_trips() {
        // Default = empty ByteBufs (catalog "zero" shape — the struct-update
        // fixture convention that keeps parallel field-adds merge-friendly). Round-trips
        // through canonical CBOR to catch any future shape drift.
        let r = ProvisionRecipientMlsPubkeyRequest::default();
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(
            r,
            decode::<ProvisionRecipientMlsPubkeyRequest>(&bytes).unwrap()
        );
    }

    #[test]
    fn fetch_recipient_index_key_request_round_trips() {
        let r = FetchRecipientIndexKeyRequest {
            actor_id: vec![3u8; 32],
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<FetchRecipientIndexKeyRequest>(&bytes).unwrap());
    }

    #[test]
    fn fetch_recipient_index_key_reply_some_round_trips() {
        let r = FetchRecipientIndexKeyReply {
            pubkey: Some(ByteBuf::from(vec![11u8; 32])),
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<FetchRecipientIndexKeyReply>(&bytes).unwrap());
    }

    #[test]
    fn fetch_recipient_index_key_reply_none_round_trips() {
        let r = FetchRecipientIndexKeyReply { pubkey: None };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<FetchRecipientIndexKeyReply>(&bytes).unwrap());
    }

    #[test]
    fn fetch_config_request_round_trips() {
        let r = FetchConfigRequest {
            scope: "all".into(),
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<FetchConfigRequest>(&bytes).unwrap());
    }

    #[test]
    fn fetch_config_reply_round_trips() {
        let r = FetchConfigReply::default();
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<FetchConfigReply>(&bytes).unwrap());
    }

    #[test]
    fn fetch_config_reply_per_domain_dkim_selectors_round_trip() {
        // Per-domain DKIM: every active domain projects its own selector so
        // the MTA bridge can build a per-domain registry and route signing by
        // the From: header domain (mail-multidomain.md § Per-domain DKIM). Pins
        // the multi-entry map round-trip + the empty-default (whole-bridge
        // degraded) case.
        let r = FetchConfigReply {
            local_domains: vec!["primary.test".into(), "second.test".into()],
            primary_domain: "primary.test".into(),
            dkim_selectors: vec![
                DomainDkimSelector {
                    domain: "primary.test".into(),
                    selector: "default".into(),
                    extra: Default::default(),
                },
                DomainDkimSelector {
                    domain: "second.test".into(),
                    selector: "sel2026".into(),
                    extra: Default::default(),
                },
            ],
            ..Default::default()
        };
        let bytes = encode_canonical(&r).unwrap();
        let decoded: FetchConfigReply = decode(&bytes).unwrap();
        assert_eq!(decoded.dkim_selectors.len(), 2);
        assert_eq!(decoded.dkim_selectors[1].domain, "second.test");
        assert_eq!(decoded.dkim_selectors[1].selector, "sel2026");
        assert_eq!(r, decoded);
        // Empty list (no primary / fresh nest) keeps the bridge degraded.
        assert!(FetchConfigReply::default().dkim_selectors.is_empty());
    }

    #[test]
    fn get_mail_config_request_round_trips() {
        // The admin read twin carries no arguments — it shares
        // `FetchConfigReply` as its reply (the overlaid effective config).
        let r = GetMailConfigRequest::default();
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<GetMailConfigRequest>(&bytes).unwrap());
    }

    #[test]
    fn bridge_policy_shutdown_grace_round_trips() {
        // T2.6 graceful shutdown: the drain budget must arrive at the
        // bridge so it can size its grace timer. Default is 30 s; this
        // pins both a custom value and the default's round-trip. Catalog
        // binding `mail.bridge.shutdown_grace_seconds` (mail-policy-config.md:57).
        assert_eq!(BridgePolicy::default().shutdown_grace_seconds, 30);
        let r = FetchConfigReply {
            bridge: BridgePolicy {
                shutdown_grace_seconds: 90,
                extra: Default::default(),
            },
            ..Default::default()
        };
        let bytes = encode_canonical(&r).unwrap();
        let decoded: FetchConfigReply = decode(&bytes).unwrap();
        assert_eq!(decoded.bridge.shutdown_grace_seconds, 90);
        assert_eq!(r, decoded);
    }

    #[test]
    fn spam_policy_thresholds_two_tier_round_trips() {
        // C.7 score-disposition gate: the wire shape must expose both
        // thresholds (spam_folder < reject) so the
        // bridge can construct a `fauna_mail::SpamPolicy` directly from
        // the snapshot rather than deriving a missing tier. Catalog
        // bindings in `mail-policy-config.md` § Inbound perimeter mirror
        // these field names.
        let r = FetchConfigReply {
            spam: SpamPolicyThresholds {
                max_score_before_spam_folder: 6,
                max_score_before_reject: 20,
                ..SpamPolicyThresholds::default()
            },
            ..Default::default()
        };
        let bytes = encode_canonical(&r).unwrap();
        let decoded: FetchConfigReply = decode(&bytes).unwrap();
        assert_eq!(decoded.spam.max_score_before_spam_folder, 6);
        assert_eq!(decoded.spam.max_score_before_reject, 20);
        assert_eq!(r, decoded);
    }

    #[test]
    fn spam_policy_bayesian_knobs_round_trip() {
        // Tier-2 `mail.spam.bayesian_*` + `training_history_retention_days`
        // (`mail-policy-config.md` § Spam) must round-trip so an admin's
        // override of the combined-score-formula knobs reaches the MDA scorer
        // (the 3 bayesian fields) and nest's GC/fade (full_confidence +
        // retention). Default-trip is the 700/50/200/30 catalog values.
        let r = FetchConfigReply {
            spam: SpamPolicyThresholds {
                bayesian_weight_milli: 850,
                bayesian_min_samples: 75,
                bayesian_full_confidence_samples: 300,
                training_history_retention_days: 14,
                ..SpamPolicyThresholds::default()
            },
            ..Default::default()
        };
        let bytes = encode_canonical(&r).unwrap();
        let decoded: FetchConfigReply = decode(&bytes).unwrap();
        assert_eq!(decoded.spam.bayesian_weight_milli, 850);
        assert_eq!(decoded.spam.bayesian_min_samples, 75);
        assert_eq!(decoded.spam.bayesian_full_confidence_samples, 300);
        assert_eq!(decoded.spam.training_history_retention_days, 14);
        assert_eq!(r, decoded);
        // Catalog defaults arrive unchanged when not overridden.
        let d = SpamPolicyThresholds::default();
        assert_eq!(d.bayesian_weight_milli, 700);
        assert_eq!(d.bayesian_min_samples, 50);
        assert_eq!(d.bayesian_full_confidence_samples, 200);
        assert_eq!(d.training_history_retention_days, 30);
    }

    #[test]
    fn spam_policy_unlisted_recipient_penalty_round_trips() {
        // Tier-2 `mail.spam.unlisted_recipient_penalty` (`mail-policy-config.md`
        // § Spam; `mail-spam.md` § Unlisted-recipient penalty) must round-trip
        // so an admin's non-zero penalty reaches the Go MTA per-recipient loop.
        let r = FetchConfigReply {
            spam: SpamPolicyThresholds {
                unlisted_recipient_penalty: 1000,
                ..SpamPolicyThresholds::default()
            },
            ..Default::default()
        };
        let bytes = encode_canonical(&r).unwrap();
        let decoded: FetchConfigReply = decode(&bytes).unwrap();
        assert_eq!(decoded.spam.unlisted_recipient_penalty, 1000);
        assert_eq!(r, decoded);
        // Catalog default is 0 (opt-in / off).
        assert_eq!(
            SpamPolicyThresholds::default().unlisted_recipient_penalty,
            0
        );
    }

    #[test]
    fn spam_policy_bayesian_knobs_default_when_missing() {
        // A `SpamPolicyThresholds` whose wire bytes omit the
        // Tier-2 bayesian/retention knobs (so those four fields are absent)
        // must still decode, with the `#[serde(default = "...")]` annotations
        // filling the 700/50/200/30 catalog values — the same treatment the
        // D.7 `max_auth_failures_per_minute` field gets.
        let mut spam_map: std::collections::BTreeMap<&str, Value> =
            std::collections::BTreeMap::new();
        spam_map.insert("max_score_before_spam_folder", Value::Integer(5));
        spam_map.insert("max_score_before_reject", Value::Integer(0));
        spam_map.insert(
            "dnsbl_servers",
            Value::List(vec![Value::String("zen.spamhaus.org".into())]),
        );
        spam_map.insert("reject_no_rdns", Value::Bool(false));
        spam_map.insert("greylist_enabled", Value::Bool(true));
        spam_map.insert("greylist_delay_secs", Value::Integer(60));
        spam_map.insert("max_conn_per_min", Value::Integer(10));
        spam_map.insert("fcrdns_mode", Value::String("score_signal".into()));
        spam_map.insert("helo_identity_required", Value::Bool(true));
        spam_map.insert("reject_fcrdns_fail", Value::Bool(false));
        spam_map.insert("max_message_bytes", Value::Integer(50_000_000));
        let knobless_bytes = encode_canonical(&spam_map).unwrap();
        let decoded: SpamPolicyThresholds = decode(&knobless_bytes).unwrap();
        assert_eq!(decoded, SpamPolicyThresholds::default());
        assert_eq!(decoded.bayesian_weight_milli, 700);
        assert_eq!(decoded.training_history_retention_days, 30);
    }

    #[test]
    fn auth_policy_enforce_dkim_round_trips() {
        // C.6 DKIM enforce-on-fail gate: the policy bit must round-trip
        // independently of `enforce_dkim`'s default-false, so an admin
        // who flips it true sees that flip arrive at the bridge.
        let r = FetchConfigReply {
            auth: AuthPolicy {
                enforce_dkim: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let bytes = encode_canonical(&r).unwrap();
        let decoded: FetchConfigReply = decode(&bytes).unwrap();
        assert!(decoded.auth.enforce_dkim);
        assert_eq!(r, decoded);
    }

    #[test]
    fn auth_policy_max_auth_failures_round_trips() {
        // D.7 submission auth-lockout: the per-(credential_id, source_IP)
        // failure ceiling must round-trip end-to-end. Default is 30; this
        // test pins both a custom value and that the default arrives at
        // the bridge unchanged.
        let r = FetchConfigReply {
            auth: AuthPolicy {
                max_auth_failures_per_minute: 60,
                ..Default::default()
            },
            ..Default::default()
        };
        let bytes = encode_canonical(&r).unwrap();
        let decoded: FetchConfigReply = decode(&bytes).unwrap();
        assert_eq!(decoded.auth.max_auth_failures_per_minute, 60);
        assert_eq!(r, decoded);
    }

    #[test]
    fn auth_policy_max_auth_failures_defaults_when_missing() {
        // A `FetchConfigReply` whose wire bytes omit D.7
        // (so `max_auth_failures_per_minute` is absent) must still decode,
        // with the Default impl filling in the 30-default. Newer fields
        // we add later get the same `#[serde(default)]` treatment.
        let mut auth_map = std::collections::BTreeMap::new();
        auth_map.insert("enforce_dmarc", true);
        auth_map.insert("enforce_dmarc_quarantine", true);
        auth_map.insert("enforce_spf_hardfail", true);
        auth_map.insert("enforce_dkim", false);
        auth_map.insert("log_only", false);
        // Use a shape that mirrors AuthPolicy with no
        // max_auth_failures_per_minute field — serde decodes it via the
        // `#[serde(default = "...")]` annotation.
        let keyless_auth_bytes = encode_canonical(&auth_map).unwrap();
        let decoded: AuthPolicy = decode(&keyless_auth_bytes).unwrap();
        assert_eq!(decoded.max_auth_failures_per_minute, 30);
        assert_eq!(decoded, AuthPolicy::default());
    }

    #[test]
    fn fetch_config_reply_mail_disabled_round_trips() {
        // Mid-transition path: admin has toggled mail off, snapshot
        // still goes over the wire while the supervisor tears the
        // bridge process down. The Phase C MTA listener gates on this
        // field — must round-trip cleanly so the idle path is reachable.
        let r = FetchConfigReply {
            mail_enabled: false,
            ..Default::default()
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<FetchConfigReply>(&bytes).unwrap());
    }

    /// The `admin-mail` surface is **client↔nest wire** — an app hydrates the
    /// page from `get_mail_config` and saves through `put_<substruct>_policy` —
    /// so unlike the same-artifact bridge kinds these payloads must ACCEPT a
    /// newer peer's unknown field and re-emit it, via rule 4's `extra` flatten
    /// rather than `deny_unknown_fields` (`transport.md` § Schema and
    /// forward-compat discipline, rule 4; `version-compatibility.md` § I2
    /// requires BOTH skew directions). Pinned so a tidy-up cannot quietly
    /// restore the stricter — and wire-breaking — posture.
    ///
    /// This test is the deliberate INVERSE of the
    /// `fetch_config_reply_rejects_unknown_field` assertion it replaces, which
    /// was green for as long as the struct was strict. Restoring
    /// `#[serde(deny_unknown_fields)]` on `SpamPolicyThresholds` turns this
    /// red — that is the red-verification for the posture, carried over from
    /// the old test rather than re-derived.
    #[test]
    fn fetch_config_sub_struct_wire_preserves_unknown_fields() {
        // Build an "extended" SpamPolicyThresholds map via Value — the shape a
        // NEWER nest sends an OLDER app once it grows a knob.
        let extended_spam = Value::Map(BTreeMap::from([
            (
                "max_score_before_spam_folder".to_string(),
                Value::Integer(5),
            ),
            ("max_score_before_reject".to_string(), Value::Integer(15)),
            ("dnsbl_servers".to_string(), Value::List(vec![])),
            ("reject_no_rdns".to_string(), Value::Bool(false)),
            ("greylist_enabled".to_string(), Value::Bool(false)),
            ("greylist_delay_secs".to_string(), Value::Integer(60)),
            ("max_conn_per_min".to_string(), Value::Integer(60)),
            (
                "fcrdns_mode".to_string(),
                Value::String("score_signal".into()),
            ),
            ("helo_identity_required".to_string(), Value::Bool(true)),
            ("reject_fcrdns_fail".to_string(), Value::Bool(false)),
            ("max_message_bytes".to_string(), Value::Integer(50_000_000)),
            ("future_field".to_string(), Value::String("oops".into())),
        ]));
        let bytes = encode_canonical(&extended_spam).unwrap();
        let decoded: SpamPolicyThresholds =
            decode(&bytes).expect("a newer nest's added knob must decode, not refuse");
        assert_eq!(
            decoded.max_score_before_spam_folder, 5,
            "known fields still bind"
        );
        assert_eq!(
            decoded.extra.get("future_field"),
            Some(&Value::String("oops".into())),
            "the unknown knob is preserved in the catch-all, not dropped"
        );
        // ...and survives a re-encode, so an app that hydrates and saves back
        // does not silently strip a knob it never knew about.
        let round_tripped = encode_canonical(&decoded).unwrap();
        let as_map: BTreeMap<String, Value> = decode(&round_tripped).unwrap();
        assert_eq!(
            as_map.get("future_field"),
            Some(&Value::String("oops".into())),
            "the unknown knob is re-emitted on encode"
        );
    }

    /// The whole reply, not just a sub-struct: an older app must hydrate the
    /// `admin-mail` page against a newer nest. Same pin, same red-verification
    /// (restore `deny_unknown_fields` on `FetchConfigReply` and this fails).
    #[test]
    fn fetch_config_reply_wire_preserves_unknown_fields() {
        let bytes = encode_canonical(&FetchConfigReply::default()).unwrap();
        let mut map: BTreeMap<String, Value> = decode(&bytes).unwrap();
        map.insert("future_knob".to_string(), Value::Integer(7));
        let from_newer_nest = encode_canonical(&map).unwrap();

        let decoded: FetchConfigReply =
            decode(&from_newer_nest).expect("an older app must still hydrate");
        assert_eq!(
            decoded.extra.get("future_knob"),
            Some(&Value::Integer(7)),
            "unknown reply field preserved, not refused"
        );
    }

    /// The WRITE direction: a newer app's save must not be refused wholesale by
    /// an older nest. `PutSpamPolicyRequest` is the representative of the five
    /// `put_<substruct>_policy` requests, which are structurally identical.
    #[test]
    fn put_spam_policy_request_wire_preserves_unknown_fields() {
        let bytes = encode_canonical(&PutSpamPolicyRequest::default()).unwrap();
        let mut map: BTreeMap<String, Value> = decode(&bytes).unwrap();
        map.insert("future_knob".to_string(), Value::Bool(true));
        let from_newer_app = encode_canonical(&map).unwrap();

        let decoded: PutSpamPolicyRequest =
            decode(&from_newer_app).expect("an older nest must still accept the save");
        assert_eq!(
            decoded.extra.get("future_knob"),
            Some(&Value::Bool(true)),
            "unknown request field preserved, not refused"
        );
    }

    #[test]
    fn report_session_close_round_trips() {
        let r = ReportSessionCloseRequest {
            actor_id: vec![5u8; 32],
            credential_id: "cred-1".into(),
            reason: "logout".into(),
            occurred_at: 1_700_000_000,
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<ReportSessionCloseRequest>(&bytes).unwrap());
    }

    #[test]
    fn check_submission_quota_request_round_trips() {
        let r = CheckSubmissionQuotaRequest {
            actor_id: vec![5u8; 32],
            recipient_count: 7,
            recipient_is_local: true,
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<CheckSubmissionQuotaRequest>(&bytes).unwrap());
    }

    #[test]
    fn check_submission_quota_reply_allowed_round_trips() {
        let r = CheckSubmissionQuotaReply::Allowed;
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<CheckSubmissionQuotaReply>(&bytes).unwrap());
    }

    #[test]
    fn check_submission_quota_reply_over_quota_round_trips() {
        let r = CheckSubmissionQuotaReply::OverQuota { remaining: 12 };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<CheckSubmissionQuotaReply>(&bytes).unwrap());
    }

    fn sample_verdicts() -> AuthVerdicts {
        AuthVerdicts {
            dkim: DkimVerdict::Pass,
            spf: SpfVerdict::Pass,
            dmarc: DmarcVerdict::Pass,
            ..Default::default()
        }
    }

    fn sample_metadata() -> PublicMailMetadata {
        PublicMailMetadata {
            timestamp: 1_700_000_000,
            ciphertext_size: 14,
            sender_domain: "example.com".into(),
        }
    }

    #[test]
    fn public_mail_metadata_round_trips() {
        let m = sample_metadata();
        let bytes = encode_canonical(&m).unwrap();
        assert_eq!(m, decode::<PublicMailMetadata>(&bytes).unwrap());
    }

    #[test]
    fn auth_verdicts_round_trips() {
        let v = sample_verdicts();
        let bytes = encode_canonical(&v).unwrap();
        assert_eq!(v, decode::<AuthVerdicts>(&bytes).unwrap());
    }

    #[test]
    fn auth_verdicts_rejects_unknown_field() {
        // Each verdict slot rides as a {"kind":"..."} map; an unknown
        // top-level field on the AuthVerdicts struct (alongside dkim /
        // spf / dmarc / arc) must be rejected by `deny_unknown_fields`.
        let verdict_map = |kind: &str| {
            Value::Map(BTreeMap::from([(
                "kind".to_string(),
                Value::String(kind.into()),
            )]))
        };
        let extended = Value::Map(BTreeMap::from([
            ("dkim".to_string(), verdict_map("pass")),
            ("spf".to_string(), verdict_map("pass")),
            ("dmarc".to_string(), verdict_map("pass")),
            ("arc".to_string(), verdict_map("none")),
            ("future_field".to_string(), Value::String("oops".into())),
        ]));
        let bytes = encode_canonical(&extended).unwrap();
        let parsed: Result<AuthVerdicts, _> = decode(&bytes);
        assert!(
            parsed.is_err(),
            "expected unknown_field error, got {parsed:?}"
        );
    }

    // Pins the adjacently-tagged CBOR shape and confirms struct-variant
    // payloads (DkimVerdict::Fail, DmarcVerdict::Fail) round-trip. The
    // Go-side wsrpc mirror in bins/fauna-bridges/internal/wsrpc/
    // methods.go and the fauna-mail::auth definitions both have to
    // produce identical CBOR bytes for these variants — drift here is
    // a wire-protocol break.
    #[test]
    fn auth_verdicts_struct_variant_round_trip() {
        let v = AuthVerdicts {
            dkim: DkimVerdict::Fail {
                reason: "bad sig".into(),
            },
            spf: SpfVerdict::Pass,
            dmarc: DmarcVerdict::Fail {
                policy: DmarcPolicy::Quarantine,
            },
            arc: ArcVerdict::None,
        };
        let bytes = encode_canonical(&v).unwrap();
        assert_eq!(v, decode::<AuthVerdicts>(&bytes).unwrap());
    }

    #[test]
    fn spam_disposition_round_trips_each_variant() {
        for d in [
            SpamDisposition::Accept,
            SpamDisposition::AcceptToSpamFolder,
            SpamDisposition::PolicyJunk,
        ] {
            let bytes = encode_canonical(&d).unwrap();
            assert_eq!(d, decode::<SpamDisposition>(&bytes).unwrap());
        }
    }

    #[test]
    fn ingest_inbound_mail_request_round_trips() {
        let r = IngestInboundMailRequest {
            actor_id: vec![5u8; 32],
            encrypted_body: b"encrypted-body-bytes".to_vec(),
            encrypted_index_hint: b"encrypted-index-hint".to_vec(),
            public_metadata: PublicMailMetadata {
                ciphertext_size: 20,
                ..sample_metadata()
            },
            verdicts: sample_verdicts(),
            ..Default::default()
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<IngestInboundMailRequest>(&bytes).unwrap());
    }

    #[test]
    fn ingest_inbound_mail_request_is_role_address_round_trips() {
        // `is_role_address: true` (a role-address inbound delivery) must survive
        // the canonical-CBOR round-trip — the inbound enforcement point reads it
        // to skip the per-mailbox quota pre-check (smtp-server.md :204).
        let r = IngestInboundMailRequest {
            actor_id: vec![5u8; 32],
            encrypted_body: b"body".to_vec(),
            encrypted_index_hint: b"hint".to_vec(),
            public_metadata: PublicMailMetadata {
                ciphertext_size: 4,
                ..sample_metadata()
            },
            verdicts: sample_verdicts(),
            is_role_address: true,
            ..Default::default()
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<IngestInboundMailRequest>(&bytes).unwrap());
    }

    #[test]
    fn ingest_inbound_mail_request_scan_verdicts_round_trip() {
        // Each ClamavVerdict variant + an rspamd score (Some) and absence
        // (None) must survive the canonical-CBOR round-trip — scan metadata
        // rides this request (no separate scan RPC; content-scoring.md).
        let rspamd = RspamdScore {
            raw_milli: 2400,
            scaled_milli: 1200,
            flagged_rules: vec!["BAYES_HAM".into(), "URIBL_BLACK".into()],
            breakdown: vec![
                RspamdRuleContribution {
                    rule: "BAYES_HAM".into(),
                    score_milli: -2900,
                },
                RspamdRuleContribution {
                    rule: "URIBL_BLACK".into(),
                    score_milli: 5400,
                },
            ],
        };
        let cases = [
            (ClamavVerdict::Clean, Some(rspamd.clone())),
            (
                ClamavVerdict::Infected {
                    signature: "Eicar-Test-Signature".into(),
                },
                None,
            ),
            (
                ClamavVerdict::Error {
                    detail: "clamd timeout".into(),
                },
                None,
            ),
            (ClamavVerdict::BypassedOversize, Some(rspamd.clone())),
            (ClamavVerdict::NotScanned, None),
        ];
        for (clamav, rspamd_score) in cases {
            let r = IngestInboundMailRequest {
                actor_id: vec![5u8; 32],
                encrypted_body: b"body".to_vec(),
                verdicts: sample_verdicts(),
                clamav_verdict: clamav,
                rspamd_score,
                ..Default::default()
            };
            let bytes = encode_canonical(&r).unwrap();
            assert_eq!(r, decode::<IngestInboundMailRequest>(&bytes).unwrap());
        }
    }

    #[test]
    fn ingest_inbound_mail_request_decodes_pre_scan_wire() {
        // A request that omits the scan fields must still decode, and
        // `#[serde(default)]` must fill them with NotScanned / None — silence
        // about ClamAV is "not scanned", never an affirmative "clean" the nest
        // would record.
        #[derive(serde::Serialize)]
        struct PreScanIngest {
            #[serde(with = "serde_bytes")]
            actor_id: Vec<u8>,
            #[serde(with = "serde_bytes")]
            encrypted_body: Vec<u8>,
            #[serde(with = "serde_bytes")]
            encrypted_index_hint: Vec<u8>,
            public_metadata: PublicMailMetadata,
            verdicts: AuthVerdicts,
            spam_score: u32,
            spam_disposition: SpamDisposition,
            dedup_key: String,
            envelope_key: String,
        }
        let old = PreScanIngest {
            actor_id: vec![1u8; 32],
            encrypted_body: b"body".to_vec(),
            encrypted_index_hint: b"hint".to_vec(),
            public_metadata: sample_metadata(),
            verdicts: sample_verdicts(),
            spam_score: 3,
            spam_disposition: SpamDisposition::AcceptToSpamFolder,
            dedup_key: "env:v1:pre-scan".into(),
            envelope_key: "env:v1:pre-scan".into(),
        };
        let bytes = encode_canonical(&old).unwrap();
        let decoded = decode::<IngestInboundMailRequest>(&bytes).unwrap();
        assert_eq!(decoded.clamav_verdict, ClamavVerdict::NotScanned);
        assert_eq!(decoded.rspamd_score, None);
        assert_eq!(decoded.spam_score, 3);
        assert_eq!(
            decoded.spam_disposition,
            SpamDisposition::AcceptToSpamFolder
        );
    }

    #[test]
    fn ingest_inbound_mail_request_scores_round_trip() {
        // The uniform scoring-bus array (content-scoring.md § The
        // scoring-metadata bus) must survive the canonical-CBOR round-trip,
        // and its absence on the pre-field wire must decode as empty
        // (`#[serde(default)]` — the `decodes_pre_scan_wire` shape above also
        // covers the omit direction).
        use fauna_core::scoring::{ScoreEntry, TIER_ADMIN, TIER_USER, factor, scorer_version};
        let r = IngestInboundMailRequest {
            actor_id: vec![5u8; 32],
            encrypted_body: b"body".to_vec(),
            encrypted_index_hint: b"hint".to_vec(),
            public_metadata: sample_metadata(),
            verdicts: sample_verdicts(),
            scores: vec![
                ScoreEntry {
                    factor: factor::SPAM.to_string(),
                    score: 875,
                    tier: TIER_USER,
                    scorer_version: scorer_version::SPAM,
                },
                ScoreEntry {
                    factor: factor::RSPAMD.to_string(),
                    score: -1200,
                    tier: TIER_ADMIN,
                    scorer_version: scorer_version::RSPAMD,
                },
            ],
            ..Default::default()
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<IngestInboundMailRequest>(&bytes).unwrap());
        // Empty array is skipped on encode (skip_serializing_if), so the
        // grown struct still emits the exact pre-field wire bytes.
        let bare = IngestInboundMailRequest {
            actor_id: vec![5u8; 32],
            encrypted_body: b"body".to_vec(),
            ..Default::default()
        };
        let bare_bytes = encode_canonical(&bare).unwrap();
        let decoded = decode::<IngestInboundMailRequest>(&bare_bytes).unwrap();
        assert!(decoded.scores.is_empty());
    }

    #[test]
    fn ingest_inbound_mail_reply_round_trips() {
        let r = IngestInboundMailReply {
            message_id: vec![7u8; 32],
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<IngestInboundMailReply>(&bytes).unwrap());
    }

    #[test]
    fn list_mailboxes_request_round_trips() {
        let r = ListMailboxesRequest {
            actor_id: vec![5u8; 32],
            ..Default::default()
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<ListMailboxesRequest>(&bytes).unwrap());
    }

    #[test]
    fn list_mailboxes_reply_round_trips() {
        let r = ListMailboxesReply {
            mailboxes: vec![
                MailboxEntry {
                    name: "INBOX".into(),
                    uid_validity: 1,
                    uid_next: 3,
                    highestmodseq: 2,
                    exists: 2,
                    unseen: 1,
                },
                MailboxEntry {
                    name: "Sent".into(),
                    uid_validity: 1,
                    uid_next: 1,
                    highestmodseq: 1,
                    exists: 0,
                    unseen: 0,
                },
            ],
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<ListMailboxesReply>(&bytes).unwrap());
    }

    #[test]
    fn select_mailbox_request_round_trips() {
        let r = SelectMailboxRequest {
            actor_id: vec![7u8; 32],
            mailbox: "INBOX".into(),
            ..Default::default()
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<SelectMailboxRequest>(&bytes).unwrap());
    }

    #[test]
    fn select_mailbox_reply_selected_with_unseen_round_trips() {
        let r = SelectMailboxReply::Selected {
            uid_validity: 1,
            uid_next: 5,
            highestmodseq: 4,
            exists: 4,
            recent: 0,
            unseen: 2,
            first_unseen_uid: Some(2),
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<SelectMailboxReply>(&bytes).unwrap());
    }

    #[test]
    fn select_mailbox_reply_selected_no_unseen_round_trips() {
        let r = SelectMailboxReply::Selected {
            uid_validity: 1,
            uid_next: 3,
            highestmodseq: 2,
            exists: 2,
            recent: 0,
            unseen: 0,
            first_unseen_uid: None,
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<SelectMailboxReply>(&bytes).unwrap());
    }

    #[test]
    fn select_mailbox_reply_no_such_mailbox_round_trips() {
        let r = SelectMailboxReply::NoSuchMailbox;
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<SelectMailboxReply>(&bytes).unwrap());
    }

    // ── C.2 message-metadata round-trip tests ─────────────────────────

    fn sample_message_meta(uid: u32, flags: Vec<&str>) -> MessageMeta {
        MessageMeta {
            uid,
            message_id: vec![uid as u8; 32],
            modseq: uid as i64 + 100,
            flags: flags.into_iter().map(String::from).collect(),
            internal_date: 1_700_000_000 + uid as i64,
            ciphertext_size: 512 + uid,
            seq_num: uid,
        }
    }

    #[test]
    fn message_meta_empty_flags_round_trips() {
        let m = sample_message_meta(1, vec![]);
        let bytes = encode_canonical(&m).unwrap();
        assert_eq!(m, decode::<MessageMeta>(&bytes).unwrap());
    }

    #[test]
    fn message_meta_multi_flags_round_trips() {
        let m = sample_message_meta(2, vec!["\\Seen", "\\Answered", "$label1"]);
        let bytes = encode_canonical(&m).unwrap();
        assert_eq!(m, decode::<MessageMeta>(&bytes).unwrap());
    }

    #[test]
    fn list_messages_request_no_since_no_after_round_trips() {
        let r = ListMessagesRequest {
            actor_id: vec![5u8; 32],
            mailbox: "INBOX".into(),
            since_modseq: None,
            limit: 0,
            after_uid: None,
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<ListMessagesRequest>(&bytes).unwrap());
    }

    #[test]
    fn list_messages_request_with_since_and_after_round_trips() {
        let r = ListMessagesRequest {
            actor_id: vec![7u8; 32],
            mailbox: "Sent".into(),
            since_modseq: Some(42),
            limit: 100,
            after_uid: Some(99),
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<ListMessagesRequest>(&bytes).unwrap());
    }

    #[test]
    fn list_messages_reply_empty_round_trips() {
        let r = ListMessagesReply {
            messages: vec![],
            expunged_uids: vec![],
            highestmodseq: 1,
            more: false,
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<ListMessagesReply>(&bytes).unwrap());
    }

    #[test]
    fn list_messages_reply_with_messages_and_expunged_round_trips() {
        let r = ListMessagesReply {
            messages: vec![
                sample_message_meta(1, vec![]),
                sample_message_meta(2, vec!["\\Seen"]),
            ],
            expunged_uids: vec![5, 7],
            highestmodseq: 14,
            more: true,
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<ListMessagesReply>(&bytes).unwrap());
    }

    #[test]
    fn list_messages_reply_more_false_no_expunged_round_trips() {
        let r = ListMessagesReply {
            messages: vec![sample_message_meta(3, vec!["\\Seen", "\\Flagged"])],
            expunged_uids: vec![],
            highestmodseq: 5,
            more: false,
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<ListMessagesReply>(&bytes).unwrap());
    }

    #[test]
    fn fetch_message_metadata_request_empty_uids_round_trips() {
        let r = FetchMessageMetadataRequest {
            actor_id: vec![3u8; 32],
            mailbox: "INBOX".into(),
            uids: vec![],
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<FetchMessageMetadataRequest>(&bytes).unwrap());
    }

    #[test]
    fn fetch_message_metadata_request_with_uids_round_trips() {
        let r = FetchMessageMetadataRequest {
            actor_id: vec![4u8; 32],
            mailbox: "Junk".into(),
            uids: vec![1, 3, 7],
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<FetchMessageMetadataRequest>(&bytes).unwrap());
    }

    #[test]
    fn fetch_message_metadata_reply_round_trips() {
        let r = FetchMessageMetadataReply {
            messages: vec![
                sample_message_meta(1, vec![]),
                sample_message_meta(3, vec!["\\Seen"]),
            ],
            mailbox_total: 3,
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<FetchMessageMetadataReply>(&bytes).unwrap());
    }

    #[test]
    fn fetch_message_metadata_reply_empty_round_trips() {
        let r = FetchMessageMetadataReply {
            messages: vec![],
            mailbox_total: 0,
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<FetchMessageMetadataReply>(&bytes).unwrap());
    }

    // ── C.3 round-trip tests ──────────────────────────────────────────

    #[test]
    fn fetch_message_ciphertext_request_round_trips() {
        let r = FetchMessageCiphertextRequest {
            actor_id: vec![5u8; 32],
            message_id: vec![7u8; 32],
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<FetchMessageCiphertextRequest>(&bytes).unwrap());
    }

    #[test]
    fn fetch_message_ciphertext_reply_found_round_trips() {
        let r = FetchMessageCiphertextReply::Found {
            encrypted_body: b"encrypted-content-bytes".to_vec(),
            ciphertext_size: 23,
            internal_date: 1_700_000_042,
            body_ref: None,
            stored_at: 1_700_000_050,
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<FetchMessageCiphertextReply>(&bytes).unwrap());
    }

    #[test]
    fn fetch_message_ciphertext_reply_not_found_round_trips() {
        let r = FetchMessageCiphertextReply::NotFound;
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<FetchMessageCiphertextReply>(&bytes).unwrap());
    }

    #[test]
    fn fetch_index_segments_since_request_mailbox_none_round_trips() {
        let r = FetchIndexSegmentsSinceRequest {
            actor_id: vec![3u8; 32],
            mailbox: None,
            since_modseq: 0,
            limit: 0,
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<FetchIndexSegmentsSinceRequest>(&bytes).unwrap());
    }

    #[test]
    fn fetch_index_segments_since_request_mailbox_some_round_trips() {
        let r = FetchIndexSegmentsSinceRequest {
            actor_id: vec![4u8; 32],
            mailbox: Some("INBOX".into()),
            since_modseq: 42,
            limit: 100,
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<FetchIndexSegmentsSinceRequest>(&bytes).unwrap());
    }

    #[test]
    fn index_segment_round_trips() {
        let s = IndexSegment {
            message_id: vec![9u8; 32],
            mailbox: "INBOX".into(),
            modseq: 7,
            encrypted_index_hint: b"hint-bytes".to_vec(),
            stored_at: 1_700_000_050,
        };
        let bytes = encode_canonical(&s).unwrap();
        assert_eq!(s, decode::<IndexSegment>(&bytes).unwrap());

        // The unknown `0` still rides the wire, and a segment without the key
        // is refused rather than defaulted — no nest omits it.
        let unknown = IndexSegment {
            stored_at: 0,
            ..s.clone()
        };
        let bytes = encode_canonical(&unknown).unwrap();
        let mut map: BTreeMap<String, Value> = decode(&bytes).unwrap();
        assert!(map.contains_key("stored_at"));
        assert_eq!(unknown, decode::<IndexSegment>(&bytes).unwrap());
        map.remove("stored_at");
        let without = encode_canonical(&map).unwrap();
        assert!(decode::<IndexSegment>(&without).is_err());
    }

    #[test]
    fn fetch_index_segments_since_reply_empty_round_trips() {
        let r = FetchIndexSegmentsSinceReply {
            segments: vec![],
            highestmodseq: 1,
            more: false,
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<FetchIndexSegmentsSinceReply>(&bytes).unwrap());
    }

    #[test]
    fn fetch_index_segments_since_reply_nonempty_more_true_round_trips() {
        let r = FetchIndexSegmentsSinceReply {
            segments: vec![
                IndexSegment {
                    message_id: vec![1u8; 32],
                    mailbox: "INBOX".into(),
                    modseq: 2,
                    encrypted_index_hint: b"hint-a".to_vec(),
                    ..Default::default()
                },
                IndexSegment {
                    message_id: vec![2u8; 32],
                    mailbox: "Sent".into(),
                    modseq: 5,
                    encrypted_index_hint: b"hint-b".to_vec(),
                    ..Default::default()
                },
            ],
            highestmodseq: 10,
            more: true,
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<FetchIndexSegmentsSinceReply>(&bytes).unwrap());
    }

    #[test]
    fn fetch_index_segments_since_reply_more_false_round_trips() {
        let r = FetchIndexSegmentsSinceReply {
            segments: vec![IndexSegment {
                message_id: vec![3u8; 32],
                mailbox: "Junk".into(),
                modseq: 8,
                encrypted_index_hint: b"hint-c".to_vec(),
                ..Default::default()
            }],
            highestmodseq: 8,
            more: false,
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<FetchIndexSegmentsSinceReply>(&bytes).unwrap());
    }

    // ── C.4 store_flags + expunge round-trip tests ────────────────────

    #[test]
    fn store_flags_op_all_variants_round_trip() {
        for op in [StoreFlagsOp::Set, StoreFlagsOp::Add, StoreFlagsOp::Remove] {
            let bytes = encode_canonical(&op).unwrap();
            let decoded: StoreFlagsOp = decode(&bytes).unwrap();
            assert_eq!(op, decoded, "round-trip failed for {op:?}");
        }
    }

    #[test]
    fn store_flags_request_set_round_trips() {
        let r = StoreFlagsRequest {
            actor_id: vec![5u8; 32],
            mailbox: "INBOX".into(),
            uids: vec![1, 2, 3],
            op: StoreFlagsOp::Set,
            flags: vec!["\\Seen".into(), "\\Flagged".into()],
            ..Default::default()
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<StoreFlagsRequest>(&bytes).unwrap());
    }

    #[test]
    fn store_flags_request_add_round_trips() {
        let r = StoreFlagsRequest {
            actor_id: vec![6u8; 32],
            mailbox: "Sent".into(),
            uids: vec![10],
            op: StoreFlagsOp::Add,
            flags: vec!["\\Answered".into()],
            ..Default::default()
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<StoreFlagsRequest>(&bytes).unwrap());
    }

    #[test]
    fn store_flags_request_remove_round_trips() {
        let r = StoreFlagsRequest {
            actor_id: vec![7u8; 32],
            mailbox: "Drafts".into(),
            uids: vec![4, 5],
            op: StoreFlagsOp::Remove,
            flags: vec!["\\Draft".into()],
            ..Default::default()
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<StoreFlagsRequest>(&bytes).unwrap());
    }

    #[test]
    fn store_flags_result_entry_round_trips() {
        let e = StoreFlagsResultEntry {
            uid: 3,
            flags: vec!["\\Flagged".into(), "\\Seen".into()],
            modseq: 42,
        };
        let bytes = encode_canonical(&e).unwrap();
        assert_eq!(e, decode::<StoreFlagsResultEntry>(&bytes).unwrap());
    }

    #[test]
    fn store_flags_reply_empty_updated_round_trips() {
        let r = StoreFlagsReply {
            updated: vec![],
            highestmodseq: 7,
            ..Default::default()
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<StoreFlagsReply>(&bytes).unwrap());
    }

    #[test]
    fn store_flags_reply_nonempty_updated_round_trips() {
        let r = StoreFlagsReply {
            updated: vec![
                StoreFlagsResultEntry {
                    uid: 1,
                    flags: vec!["\\Seen".into()],
                    modseq: 10,
                },
                StoreFlagsResultEntry {
                    uid: 2,
                    flags: vec!["\\Flagged".into(), "\\Seen".into()],
                    modseq: 10,
                },
            ],
            highestmodseq: 10,
            ..Default::default()
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<StoreFlagsReply>(&bytes).unwrap());
    }

    // ── D.2 CONDSTORE UNCHANGEDSINCE extension round-trips ────────────────────

    /// `unchanged_since: None` and `modified: vec![]` must encode without
    /// emitting the field at all (serde-skip), so a request with no CONDSTORE
    /// precondition carries neither key — the struct-update fixture
    /// convention's carve-out.
    #[test]
    fn store_flags_request_omits_unchanged_since_when_none() {
        let r = StoreFlagsRequest {
            actor_id: vec![5u8; 32],
            mailbox: "INBOX".into(),
            uids: vec![1, 2],
            op: StoreFlagsOp::Set,
            flags: vec!["\\Seen".into()],
            unchanged_since: None,
        };
        let bytes = encode_canonical(&r).unwrap();
        let decoded: StoreFlagsRequest = decode(&bytes).unwrap();
        assert_eq!(r, decoded);
        // Wire must NOT contain the key "unchanged_since".
        let s = String::from_utf8_lossy(&bytes);
        assert!(
            !s.contains("unchanged_since"),
            "unchanged_since=None must be omitted on the wire"
        );
    }

    #[test]
    fn store_flags_request_with_unchanged_since_round_trips() {
        let r = StoreFlagsRequest {
            actor_id: vec![5u8; 32],
            mailbox: "INBOX".into(),
            uids: vec![1, 2, 3],
            op: StoreFlagsOp::Add,
            flags: vec!["\\Flagged".into()],
            unchanged_since: Some(42),
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<StoreFlagsRequest>(&bytes).unwrap());
    }

    #[test]
    fn store_flags_reply_with_modified_round_trips() {
        let r = StoreFlagsReply {
            updated: vec![StoreFlagsResultEntry {
                uid: 1,
                flags: vec!["\\Seen".into()],
                modseq: 11,
            }],
            highestmodseq: 11,
            modified: vec![2, 3],
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<StoreFlagsReply>(&bytes).unwrap());
    }

    /// `modified: vec![]` must not appear on the wire (skip_serializing_if),
    /// so a reply without CONDSTORE precondition carries no `modified` key.
    #[test]
    fn store_flags_reply_omits_modified_when_empty() {
        let r = StoreFlagsReply {
            updated: vec![],
            highestmodseq: 1,
            modified: vec![],
        };
        let bytes = encode_canonical(&r).unwrap();
        let decoded: StoreFlagsReply = decode(&bytes).unwrap();
        assert_eq!(r, decoded);
        let s = String::from_utf8_lossy(&bytes);
        assert!(
            !s.contains("modified"),
            "modified=empty must be omitted on the wire"
        );
    }

    #[test]
    fn expunge_request_empty_uids_round_trips() {
        let r = ExpungeRequest {
            actor_id: vec![8u8; 32],
            mailbox: "INBOX".into(),
            uids: vec![],
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<ExpungeRequest>(&bytes).unwrap());
    }

    #[test]
    fn expunge_request_nonempty_uids_round_trips() {
        let r = ExpungeRequest {
            actor_id: vec![9u8; 32],
            mailbox: "Trash".into(),
            uids: vec![3, 7, 11],
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<ExpungeRequest>(&bytes).unwrap());
    }

    #[test]
    fn expunge_reply_round_trips() {
        let r = ExpungeReply {
            expunged_uids: vec![1, 3, 5],
            highestmodseq: 20,
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<ExpungeReply>(&bytes).unwrap());
    }

    #[test]
    fn expunge_reply_empty_round_trips() {
        let r = ExpungeReply {
            expunged_uids: vec![],
            highestmodseq: 5,
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<ExpungeReply>(&bytes).unwrap());
    }

    #[test]
    fn submit_inbound_mail_request_round_trips_as_ingest() {
        let r = IngestInboundMailRequest {
            actor_id: vec![3u8; 32],
            encrypted_body: b"body".to_vec(),
            encrypted_index_hint: b"hint".to_vec(),
            public_metadata: PublicMailMetadata {
                ciphertext_size: 4,
                ..sample_metadata()
            },
            verdicts: sample_verdicts(),
            ..Default::default()
        };
        let bytes = encode_canonical(&r).unwrap();
        // Decode via the submit alias — byte-identical wire shape.
        let decoded: SubmitInboundMailRequest = decode(&bytes).unwrap();
        assert_eq!(r, decoded);
    }

    // ── C.5 copy + move round-trip tests ─────────────────────────────

    #[test]
    fn copy_messages_request_single_uid_round_trips() {
        let r = CopyMessagesRequest {
            actor_id: vec![5u8; 32],
            source_mailbox: "INBOX".into(),
            uids: vec![1],
            dest_mailbox: "Archive".into(),
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<CopyMessagesRequest>(&bytes).unwrap());
    }

    #[test]
    fn copy_messages_request_multiple_uids_round_trips() {
        let r = CopyMessagesRequest {
            actor_id: vec![6u8; 32],
            source_mailbox: "INBOX".into(),
            uids: vec![1, 2, 3],
            dest_mailbox: "Sent".into(),
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<CopyMessagesRequest>(&bytes).unwrap());
    }

    #[test]
    fn copy_pair_round_trips() {
        let p = CopyPair {
            source_uid: 3,
            dest_uid: 7,
        };
        let bytes = encode_canonical(&p).unwrap();
        assert_eq!(p, decode::<CopyPair>(&bytes).unwrap());
    }

    #[test]
    fn copy_messages_reply_empty_round_trips() {
        let r = CopyMessagesReply {
            dest_uid_validity: 1,
            copied: vec![],
            dest_highestmodseq: 1,
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<CopyMessagesReply>(&bytes).unwrap());
    }

    #[test]
    fn copy_messages_reply_nonempty_round_trips() {
        let r = CopyMessagesReply {
            dest_uid_validity: 1,
            copied: vec![
                CopyPair {
                    source_uid: 1,
                    dest_uid: 3,
                },
                CopyPair {
                    source_uid: 2,
                    dest_uid: 4,
                },
            ],
            dest_highestmodseq: 4,
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<CopyMessagesReply>(&bytes).unwrap());
    }

    #[test]
    fn move_messages_request_single_uid_round_trips() {
        let r = MoveMessagesRequest {
            actor_id: vec![7u8; 32],
            source_mailbox: "INBOX".into(),
            uids: vec![2],
            dest_mailbox: "Trash".into(),
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<MoveMessagesRequest>(&bytes).unwrap());
    }

    #[test]
    fn move_messages_request_multiple_uids_round_trips() {
        let r = MoveMessagesRequest {
            actor_id: vec![8u8; 32],
            source_mailbox: "Junk".into(),
            uids: vec![5, 6, 7],
            dest_mailbox: "Trash".into(),
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<MoveMessagesRequest>(&bytes).unwrap());
    }

    #[test]
    fn move_messages_reply_empty_round_trips() {
        let r = MoveMessagesReply {
            dest_uid_validity: 1,
            moved: vec![],
            source_highestmodseq: 5,
            dest_highestmodseq: 1,
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<MoveMessagesReply>(&bytes).unwrap());
    }

    #[test]
    fn move_messages_reply_nonempty_round_trips() {
        let r = MoveMessagesReply {
            dest_uid_validity: 1,
            moved: vec![
                CopyPair {
                    source_uid: 1,
                    dest_uid: 1,
                },
                CopyPair {
                    source_uid: 2,
                    dest_uid: 2,
                },
            ],
            source_highestmodseq: 4,
            dest_highestmodseq: 3,
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<MoveMessagesReply>(&bytes).unwrap());
    }

    // ── C.6 append round-trip tests ──────────────────────────────────

    #[test]
    fn append_message_request_with_flags_round_trips() {
        let r = AppendMessageRequest {
            actor_id: vec![5u8; 32],
            mailbox: "Drafts".into(),
            flags: vec!["\\Draft".into(), "\\Seen".into()],
            encrypted_body: b"encrypted-body-data".to_vec(),
            encrypted_index_hint: b"encrypted-hint".to_vec(),
            timestamp: 1_700_000_000,
            ciphertext_size: 19,
            sender_domain: "example.com".into(),
            ..Default::default()
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<AppendMessageRequest>(&bytes).unwrap());
    }

    #[test]
    fn append_message_request_empty_flags_round_trips() {
        let r = AppendMessageRequest {
            actor_id: vec![7u8; 32],
            mailbox: "INBOX".into(),
            flags: vec![],
            encrypted_body: b"body".to_vec(),
            encrypted_index_hint: b"hint".to_vec(),
            timestamp: 1_700_000_042,
            ciphertext_size: 4,
            sender_domain: String::new(),
            ..Default::default()
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<AppendMessageRequest>(&bytes).unwrap());
    }

    #[test]
    fn append_message_reply_round_trips() {
        let r = AppendMessageReply {
            message_id: vec![3u8; 32],
            uid: 7,
            uid_validity: 1,
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<AppendMessageReply>(&bytes).unwrap());
    }

    // ── D.1 provision_calendar round-trip tests ──────────────────────

    #[test]
    fn provision_calendar_request_round_trips() {
        let r = ProvisionCalendarRequest {
            actor_id: vec![1u8; 32],
            calendar_id: vec![2u8; 32],
            encrypted_metadata: b"sealed-metadata".to_vec(),
            ..Default::default()
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<ProvisionCalendarRequest>(&bytes).unwrap());
    }

    #[test]
    fn provision_calendar_reply_created_round_trips() {
        let r = ProvisionCalendarReply::Created;
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<ProvisionCalendarReply>(&bytes).unwrap());
    }

    #[test]
    fn provision_calendar_reply_already_exists_round_trips() {
        let r = ProvisionCalendarReply::AlreadyExists;
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<ProvisionCalendarReply>(&bytes).unwrap());
    }

    #[test]
    fn provision_calendar_reply_conflict_round_trips() {
        let r = ProvisionCalendarReply::Conflict;
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<ProvisionCalendarReply>(&bytes).unwrap());
    }

    #[test]
    fn provision_calendar_request_with_update_metadata_round_trips() {
        let r = ProvisionCalendarRequest {
            actor_id: vec![1u8; 32],
            calendar_id: vec![2u8; 32],
            encrypted_metadata: b"resealed-metadata".to_vec(),
            update_metadata: true,
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<ProvisionCalendarRequest>(&bytes).unwrap());
    }

    /// A wire shape without `update_metadata` (a plain MKCOL) must decode
    /// into the struct with `update_metadata = false`. This is the
    /// `#[serde(default)]` contract.
    #[test]
    fn provision_calendar_request_without_update_metadata_defaults_false() {
        #[derive(Serialize)]
        struct MetadataLessProvisionCalendarRequest {
            #[serde(with = "serde_bytes")]
            actor_id: Vec<u8>,
            #[serde(with = "serde_bytes")]
            calendar_id: Vec<u8>,
            #[serde(with = "serde_bytes")]
            encrypted_metadata: Vec<u8>,
        }
        let plain_mkcol = MetadataLessProvisionCalendarRequest {
            actor_id: vec![3u8; 32],
            calendar_id: vec![4u8; 32],
            encrypted_metadata: b"sealed-metadata".to_vec(),
        };
        let bytes = encode_canonical(&plain_mkcol).unwrap();
        let decoded = decode::<ProvisionCalendarRequest>(&bytes).unwrap();
        assert_eq!(decoded.actor_id, plain_mkcol.actor_id);
        assert_eq!(decoded.calendar_id, plain_mkcol.calendar_id);
        assert_eq!(decoded.encrypted_metadata, plain_mkcol.encrypted_metadata);
        assert!(
            !decoded.update_metadata,
            "an absent key must default update_metadata to false"
        );
    }

    #[test]
    fn provision_calendar_reply_updated_round_trips() {
        let r = ProvisionCalendarReply::Updated;
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<ProvisionCalendarReply>(&bytes).unwrap());
    }

    #[test]
    fn provision_calendar_reply_not_found_round_trips() {
        let r = ProvisionCalendarReply::NotFound;
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<ProvisionCalendarReply>(&bytes).unwrap());
    }

    // ── D.2 list_calendars round-trip tests ──────────────────────────

    #[test]
    fn list_calendars_request_round_trips() {
        let r = ListCalendarsRequest {
            actor_id: vec![1u8; 32],
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<ListCalendarsRequest>(&bytes).unwrap());
    }

    #[test]
    fn calendar_entry_round_trips() {
        let r = CalendarEntry {
            calendar_id: vec![2u8; 32],
            encrypted_metadata: b"sealed-metadata".to_vec(),
            ctag: 7,
            highestmodseq: 3,
            event_count: 42,
            created_at: 1_700_000_000,
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<CalendarEntry>(&bytes).unwrap());
    }

    #[test]
    fn list_calendars_reply_empty_round_trips() {
        let r = ListCalendarsReply { calendars: vec![] };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<ListCalendarsReply>(&bytes).unwrap());
    }

    #[test]
    fn list_calendars_reply_non_empty_round_trips() {
        let r = ListCalendarsReply {
            calendars: vec![
                CalendarEntry {
                    calendar_id: vec![3u8; 32],
                    encrypted_metadata: b"meta-a".to_vec(),
                    ctag: 0,
                    highestmodseq: 1,
                    event_count: 0,
                    created_at: 1_700_000_000,
                },
                CalendarEntry {
                    calendar_id: vec![4u8; 32],
                    encrypted_metadata: b"meta-b".to_vec(),
                    ctag: 0,
                    highestmodseq: 2,
                    event_count: 5,
                    created_at: 1_700_000_100,
                },
            ],
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<ListCalendarsReply>(&bytes).unwrap());
    }

    // ── D.3 query_events round-trip tests ────────────────────────────

    #[test]
    fn query_events_request_no_filters_round_trips() {
        let r = QueryEventsRequest {
            actor_id: vec![1u8; 32],
            calendar_id: vec![2u8; 32],
            since_modseq: None,
            after_event_id: None,
            limit: 0,
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<QueryEventsRequest>(&bytes).unwrap());
    }

    #[test]
    fn query_events_request_with_filters_round_trips() {
        let r = QueryEventsRequest {
            actor_id: vec![3u8; 32],
            calendar_id: vec![4u8; 32],
            since_modseq: Some(42),
            after_event_id: Some(ByteBuf::from(vec![7u8; 32])),
            limit: 50,
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<QueryEventsRequest>(&bytes).unwrap());
    }

    #[test]
    fn event_entry_round_trips() {
        let e = EventEntry {
            event_id: vec![5u8; 32],
            uid_hash: vec![6u8; 32],
            encrypted_body: b"encrypted-event-body-bytes".to_vec(),
            encrypted_index_hint: b"encrypted-index-hint-bytes".to_vec(),
            etag: "\"modseq-7\"".into(),
            modseq: 7,
            ciphertext_size: 26,
            internal_date: 1_700_000_042,
            // Prove the sidecar round-trips when present (Fauna-written event).
            encrypted_fauna_ext: Some(b"sealed-fauna-ext-sidecar".to_vec()),
        };
        let bytes = encode_canonical(&e).unwrap();
        assert_eq!(e, decode::<EventEntry>(&bytes).unwrap());

        // And the `None` case (MUA-written event / pre-sidecar row): the
        // `skip_serializing_if` omits the key, `serde(default)` decodes it back.
        let no_ext = EventEntry {
            encrypted_fauna_ext: None,
            ..e.clone()
        };
        let bytes = encode_canonical(&no_ext).unwrap();
        assert_eq!(no_ext, decode::<EventEntry>(&bytes).unwrap());
    }

    #[test]
    fn query_events_reply_ok_empty_round_trips() {
        let r = QueryEventsReply::Ok {
            events: vec![],
            highestmodseq: 1,
            more: false,
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<QueryEventsReply>(&bytes).unwrap());
    }

    #[test]
    fn query_events_reply_ok_nonempty_round_trips() {
        let r = QueryEventsReply::Ok {
            events: vec![
                EventEntry {
                    event_id: vec![10u8; 32],
                    uid_hash: vec![11u8; 32],
                    encrypted_body: b"body-a".to_vec(),
                    encrypted_index_hint: b"hint-a".to_vec(),
                    etag: "\"modseq-2\"".into(),
                    modseq: 2,
                    ciphertext_size: 6,
                    internal_date: 1_700_000_001,
                    ..Default::default()
                },
                EventEntry {
                    event_id: vec![12u8; 32],
                    uid_hash: vec![13u8; 32],
                    encrypted_body: b"body-b".to_vec(),
                    encrypted_index_hint: b"hint-b".to_vec(),
                    etag: "\"modseq-3\"".into(),
                    modseq: 3,
                    ciphertext_size: 6,
                    internal_date: 1_700_000_002,
                    ..Default::default()
                },
            ],
            highestmodseq: 5,
            more: true,
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<QueryEventsReply>(&bytes).unwrap());
    }

    #[test]
    fn query_events_reply_calendar_not_found_round_trips() {
        let r = QueryEventsReply::CalendarNotFound;
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<QueryEventsReply>(&bytes).unwrap());
    }

    // ── D.4 put_event_ciphertext round-trip tests ─────────────────────

    #[test]
    fn put_event_ciphertext_request_no_if_match_round_trips() {
        let r = PutEventCiphertextRequest {
            actor_id: vec![1u8; 32],
            calendar_id: vec![2u8; 32],
            uid_hash: vec![3u8; 32],
            encrypted_body: b"encrypted-icalendar-body".to_vec(),
            encrypted_index_hint: b"encrypted-index-hint".to_vec(),
            timestamp: 1_700_000_000,
            ciphertext_size: 24,
            if_match: None,
            ..Default::default()
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<PutEventCiphertextRequest>(&bytes).unwrap());
    }

    #[test]
    fn put_event_ciphertext_request_with_if_match_round_trips() {
        let r = PutEventCiphertextRequest {
            actor_id: vec![4u8; 32],
            calendar_id: vec![5u8; 32],
            uid_hash: vec![6u8; 32],
            encrypted_body: b"sealed-event-content".to_vec(),
            encrypted_index_hint: b"sealed-hint".to_vec(),
            timestamp: 1_700_000_042,
            ciphertext_size: 20,
            if_match: Some("00000000deadbeef".to_string()),
            // Prove the request-side sidecar round-trips when present.
            encrypted_fauna_ext: Some(b"sealed-fauna-ext".to_vec()),
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<PutEventCiphertextRequest>(&bytes).unwrap());
    }

    #[test]
    fn put_event_ciphertext_reply_created_round_trips() {
        let r = PutEventCiphertextReply::Created {
            event_id: vec![7u8; 32],
            etag: "0000000000000002".to_string(),
            modseq: 2,
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<PutEventCiphertextReply>(&bytes).unwrap());
    }

    #[test]
    fn put_event_ciphertext_reply_updated_round_trips() {
        let r = PutEventCiphertextReply::Updated {
            event_id: vec![8u8; 32],
            etag: "0000000000000003".to_string(),
            modseq: 3,
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<PutEventCiphertextReply>(&bytes).unwrap());
    }

    #[test]
    fn put_event_ciphertext_reply_precondition_failed_round_trips() {
        let r = PutEventCiphertextReply::PreconditionFailed {
            current_etag: "0000000000000002".to_string(),
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<PutEventCiphertextReply>(&bytes).unwrap());
    }

    #[test]
    fn put_event_ciphertext_reply_calendar_not_found_round_trips() {
        let r = PutEventCiphertextReply::CalendarNotFound;
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<PutEventCiphertextReply>(&bytes).unwrap());
    }

    // ── D.5 delete_event round-trips ──────────────────────────────────

    #[test]
    fn delete_event_request_no_if_match_round_trips() {
        let r = DeleteEventRequest {
            actor_id: vec![1u8; 32],
            calendar_id: vec![2u8; 32],
            uid_hash: vec![3u8; 32],
            if_match: None,
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<DeleteEventRequest>(&bytes).unwrap());
    }

    #[test]
    fn delete_event_request_with_if_match_round_trips() {
        let r = DeleteEventRequest {
            actor_id: vec![4u8; 32],
            calendar_id: vec![5u8; 32],
            uid_hash: vec![6u8; 32],
            if_match: Some("00000000deadbeef".to_string()),
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<DeleteEventRequest>(&bytes).unwrap());
    }

    #[test]
    fn delete_event_reply_deleted_round_trips() {
        let r = DeleteEventReply::Deleted {
            event_id: vec![7u8; 32],
            modseq: 3,
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<DeleteEventReply>(&bytes).unwrap());
    }

    #[test]
    fn delete_event_reply_not_found_round_trips() {
        let r = DeleteEventReply::NotFound;
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<DeleteEventReply>(&bytes).unwrap());
    }

    #[test]
    fn delete_event_reply_precondition_failed_round_trips() {
        let r = DeleteEventReply::PreconditionFailed {
            current_etag: "0000000000000002".to_string(),
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<DeleteEventReply>(&bytes).unwrap());
    }

    // ── D.6 sync_calendar_since round-trip tests ──────────────────────

    #[test]
    fn sync_calendar_since_request_full_sync_round_trips() {
        let r = SyncCalendarSinceRequest {
            actor_id: vec![1u8; 32],
            calendar_id: vec![2u8; 32],
            sync_token: "0".to_string(),
            limit: 0,
            ..Default::default()
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<SyncCalendarSinceRequest>(&bytes).unwrap());
    }

    #[test]
    fn sync_calendar_since_request_incremental_round_trips() {
        let r = SyncCalendarSinceRequest {
            actor_id: vec![3u8; 32],
            calendar_id: vec![4u8; 32],
            sync_token: "42".to_string(),
            limit: 100,
            ..Default::default()
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<SyncCalendarSinceRequest>(&bytes).unwrap());
    }

    #[test]
    fn expunged_entry_round_trips() {
        let e = ExpungedEntry {
            event_id: vec![5u8; 32],
            uid_hash: vec![6u8; 32],
            modseq: 7,
        };
        let bytes = encode_canonical(&e).unwrap();
        assert_eq!(e, decode::<ExpungedEntry>(&bytes).unwrap());
    }

    #[test]
    fn sync_calendar_since_reply_ok_empty_round_trips() {
        let r = SyncCalendarSinceReply::Ok {
            changed: vec![],
            expunged: vec![],
            new_sync_token: "1".to_string(),
            more: false,
            stale: false,
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<SyncCalendarSinceReply>(&bytes).unwrap());
    }

    #[test]
    fn sync_calendar_since_reply_ok_stale_round_trips() {
        // Past-retention signal: token is valid but predates the tombstone
        // window. Distinct from the Stale variant (MUA-ahead / restore).
        let r = SyncCalendarSinceReply::Ok {
            changed: vec![],
            expunged: vec![],
            new_sync_token: "42".to_string(),
            more: false,
            stale: true,
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<SyncCalendarSinceReply>(&bytes).unwrap());
    }

    #[test]
    fn sync_calendar_since_reply_ok_nonempty_round_trips() {
        let r = SyncCalendarSinceReply::Ok {
            changed: vec![
                EventEntry {
                    event_id: vec![10u8; 32],
                    uid_hash: vec![11u8; 32],
                    encrypted_body: b"body-a".to_vec(),
                    encrypted_index_hint: b"hint-a".to_vec(),
                    etag: "\"modseq-2\"".into(),
                    modseq: 2,
                    ciphertext_size: 6,
                    internal_date: 1_700_000_000,
                    ..Default::default()
                },
                EventEntry {
                    event_id: vec![12u8; 32],
                    uid_hash: vec![13u8; 32],
                    encrypted_body: b"body-b".to_vec(),
                    encrypted_index_hint: b"hint-b".to_vec(),
                    etag: "\"modseq-3\"".into(),
                    modseq: 3,
                    ciphertext_size: 6,
                    internal_date: 1_700_000_001,
                    ..Default::default()
                },
            ],
            expunged: vec![ExpungedEntry {
                event_id: vec![14u8; 32],
                uid_hash: vec![15u8; 32],
                modseq: 6,
            }],
            new_sync_token: "7".to_string(),
            more: true,
            stale: false,
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<SyncCalendarSinceReply>(&bytes).unwrap());
    }

    #[test]
    fn sync_calendar_since_reply_calendar_not_found_round_trips() {
        let r = SyncCalendarSinceReply::CalendarNotFound;
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<SyncCalendarSinceReply>(&bytes).unwrap());
    }

    #[test]
    fn whoami_request_round_trips() {
        let r = WhoamiRequest {};
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<WhoamiRequest>(&bytes).unwrap());
    }

    /// The `WhoamiReply` wire shape without `node_mode`, used only to
    /// synthesize an absent-key payload for the decode test.
    #[derive(Serialize)]
    struct WhoamiReplyWithoutNodeMode {
        role: String,
        bridge_id: String,
        status: String,
        ed25519_pubkey_hex: String,
        x25519_pubkey_hex: String,
    }

    #[test]
    fn whoami_reply_round_trips() {
        let r = WhoamiReply {
            role: "mta".into(),
            bridge_id: "mta-eu-1".into(),
            status: "approved".into(),
            ed25519_pubkey_hex: "ab".repeat(32),
            x25519_pubkey_hex: "cd".repeat(32),
            node_mode: "private".into(),
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<WhoamiReply>(&bytes).unwrap());
    }

    #[test]
    fn whoami_reply_tolerates_missing_node_mode() {
        // An absent `node_mode` key: `#[serde(default)]` must decode that into
        // an empty string (the bridge's plain non-private default).
        let without_mode = WhoamiReplyWithoutNodeMode {
            role: "mda".into(),
            bridge_id: "mda-1".into(),
            status: "approved".into(),
            ed25519_pubkey_hex: "ab".repeat(32),
            x25519_pubkey_hex: "cd".repeat(32),
        };
        let bytes = encode_canonical(&without_mode).unwrap();
        let decoded: WhoamiReply = decode(&bytes).unwrap();
        assert_eq!(decoded.node_mode, "");
    }

    // ── C.7 search round-trip tests ───────────────────────────────────

    #[test]
    fn header_field_all_variants_round_trip() {
        for field in [
            HeaderField::From,
            HeaderField::To,
            HeaderField::Cc,
            HeaderField::Subject,
        ] {
            let bytes = encode_canonical(&field).unwrap();
            assert_eq!(field, decode::<HeaderField>(&bytes).unwrap());
        }
    }

    #[test]
    fn search_term_has_flag_round_trips() {
        let t = SearchTerm::HasFlag {
            flag: "\\Seen".into(),
        };
        let bytes = encode_canonical(&t).unwrap();
        assert_eq!(t, decode::<SearchTerm>(&bytes).unwrap());
    }

    #[test]
    fn search_term_lacks_flag_round_trips() {
        let t = SearchTerm::LacksFlag {
            flag: "\\Deleted".into(),
        };
        let bytes = encode_canonical(&t).unwrap();
        assert_eq!(t, decode::<SearchTerm>(&bytes).unwrap());
    }

    #[test]
    fn search_term_header_contains_round_trips() {
        let t = SearchTerm::HeaderContains {
            field: HeaderField::From,
            value: "example.com".into(),
        };
        let bytes = encode_canonical(&t).unwrap();
        assert_eq!(t, decode::<SearchTerm>(&bytes).unwrap());
    }

    #[test]
    fn search_term_date_round_trips() {
        let since = SearchTerm::SinceInternalDate { ts: 1_700_000_000 };
        let before = SearchTerm::BeforeInternalDate { ts: 1_800_000_000 };
        for t in [since, before] {
            let bytes = encode_canonical(&t).unwrap();
            assert_eq!(t, decode::<SearchTerm>(&bytes).unwrap());
        }
    }

    #[test]
    fn search_term_size_round_trips() {
        for t in [
            SearchTerm::Larger { size: 100 },
            SearchTerm::Smaller { size: 200 },
        ] {
            let bytes = encode_canonical(&t).unwrap();
            assert_eq!(t, decode::<SearchTerm>(&bytes).unwrap());
        }
    }

    #[test]
    fn search_messages_request_round_trips() {
        let r = SearchMessagesRequest {
            actor_id: vec![5u8; 32],
            mailbox: "INBOX".into(),
            terms: vec![
                SearchTerm::HasFlag {
                    flag: "\\Seen".into(),
                },
                SearchTerm::HeaderContains {
                    field: HeaderField::Subject,
                    value: "invoice".into(),
                },
                SearchTerm::SinceInternalDate { ts: 1_700_000_000 },
                SearchTerm::Larger { size: 1024 },
            ],
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<SearchMessagesRequest>(&bytes).unwrap());
    }

    #[test]
    fn search_messages_request_empty_terms_round_trips() {
        let r = SearchMessagesRequest {
            actor_id: vec![6u8; 32],
            mailbox: "Sent".into(),
            terms: vec![],
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<SearchMessagesRequest>(&bytes).unwrap());
    }

    #[test]
    fn search_messages_reply_round_trips() {
        let r = SearchMessagesReply {
            uids: vec![1, 3, 7, 12],
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<SearchMessagesReply>(&bytes).unwrap());
    }

    #[test]
    fn search_messages_reply_empty_round_trips() {
        let r = SearchMessagesReply { uids: vec![] };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<SearchMessagesReply>(&bytes).unwrap());
    }

    // ── C.8 quota round-trip tests ────────────────────────────────────

    #[test]
    fn get_quota_request_round_trips() {
        let r = GetQuotaRequest {
            actor_id: vec![5u8; 32],
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<GetQuotaRequest>(&bytes).unwrap());
    }

    #[test]
    fn get_quota_reply_round_trips() {
        let r = GetQuotaReply {
            storage_bytes_used: 12_345_678,
            message_count_used: 42,
            storage_bytes_limit: 1 << 30,
            message_count_limit: 50_000,
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<GetQuotaReply>(&bytes).unwrap());
    }

    #[test]
    fn get_quota_reply_zero_used_round_trips() {
        let r = GetQuotaReply {
            storage_bytes_used: 0,
            message_count_used: 0,
            storage_bytes_limit: 1 << 30,
            message_count_limit: 50_000,
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<GetQuotaReply>(&bytes).unwrap());
    }

    #[test]
    fn imap_policy_default_includes_quota_defaults() {
        let p = ImapPolicy::default();
        assert_eq!(p.storage_bytes_default, 1 << 30);
        assert_eq!(p.message_count_default, 50_000);
    }

    // ── spam-training label round-trip tests ───────────────────────

    #[test]
    fn spam_label_variants_round_trip() {
        for label in [SpamLabel::Spam, SpamLabel::Ham] {
            let bytes = encode_canonical(&label).unwrap();
            assert_eq!(label, decode::<SpamLabel>(&bytes).unwrap());
        }
    }

    #[test]
    fn training_source_variants_round_trip() {
        for src in [
            TrainingSource::ImapJunkFlag,
            TrainingSource::ImapJunkMove,
            TrainingSource::ManualOther,
        ] {
            let bytes = encode_canonical(&src).unwrap();
            assert_eq!(src, decode::<TrainingSource>(&bytes).unwrap());
        }
    }

    #[test]
    fn publish_spam_baseline_round_trips() {
        let req = PublishSpamBaselineRequest::default();
        let bytes = encode_canonical(&req).unwrap();
        assert_eq!(req, decode::<PublishSpamBaselineRequest>(&bytes).unwrap());

        let reply = PublishSpamBaselineReply {
            contributors: 3,
            sample_count: 412,
            published: true,
            skipped_contributors: 1,
            deferred: true,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        assert_eq!(reply, decode::<PublishSpamBaselineReply>(&bytes).unwrap());
    }

    #[test]
    fn get_spam_baseline_state_round_trips() {
        let req = GetSpamBaselineStateRequest::default();
        let bytes = encode_canonical(&req).unwrap();
        assert_eq!(req, decode::<GetSpamBaselineStateRequest>(&bytes).unwrap());

        let reply = GetSpamBaselineStateReply {
            published: true,
            contributors: 5,
            sample_count: 900,
            published_at: Some(1_758_000_000_000),
            skipped_contributors: 2,
            deferred: true,
            standing: true,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        assert_eq!(reply, decode::<GetSpamBaselineStateReply>(&bytes).unwrap());
    }

    #[test]
    fn spam_baseline_additions_default_when_a_peer_omits_them() {
        // Additive-everywhere (version-compatibility.md): a reply omitting
        // `deferred`, and a policy omitting `baseline_standing_publish`,
        // both decode to off rather than failing. The PUT is the exception:
        // it must state the setting.
        #[derive(Serialize)]
        struct DeltaLessReply {
            contributors: u32,
            sample_count: u32,
            published: bool,
            skipped_contributors: u32,
        }
        let bytes = encode_canonical(&DeltaLessReply {
            contributors: 3,
            sample_count: 10,
            published: true,
            skipped_contributors: 0,
        })
        .unwrap();
        assert!(!decode::<PublishSpamBaselineReply>(&bytes).unwrap().deferred);

        let mut pre = encode_canonical(&SpamPolicyThresholds::default()).unwrap();
        let mut value: BTreeMap<String, Value> = decode(&pre).unwrap();
        assert!(value.remove("baseline_standing_publish").is_some());
        pre = encode_canonical(&value).unwrap();
        assert!(
            !decode::<SpamPolicyThresholds>(&pre)
                .unwrap()
                .baseline_standing_publish
        );
        assert!(
            decode::<PutSpamPolicyRequest>(
                &encode_canonical(&BTreeMap::<String, Value>::new()).unwrap()
            )
            .is_err(),
            "a PUT without the standing-publish setting is refused"
        );
    }

    #[test]
    fn publish_spam_baseline_reply_defaults_absent_skipped_contributors_to_zero() {
        // `skipped_contributors` is additive (`#[serde(default)]`): a pre-drain
        // nest omits the key and it decodes 0 rather than failing — the same
        // forward-compat guarantee `published` carries.
        #[derive(Serialize)]
        struct PreDrainPublishSpamBaselineReply {
            contributors: u32,
            sample_count: u32,
            published: bool,
        }
        let old = PreDrainPublishSpamBaselineReply {
            contributors: 3,
            sample_count: 412,
            published: true,
        };
        let bytes = encode_canonical(&old).unwrap();
        let decoded: PublishSpamBaselineReply = decode(&bytes).unwrap();
        assert_eq!(decoded.skipped_contributors, 0);
        assert_eq!(decoded.contributors, 3);
        assert!(decoded.published);
    }

    #[test]
    fn set_baseline_contribution_round_trips() {
        for contribute in [true, false] {
            let req = SetBaselineContributionRequest {
                contribute,
                extra: BTreeMap::new(),
            };
            let bytes = encode_canonical(&req).unwrap();
            assert_eq!(
                req,
                decode::<SetBaselineContributionRequest>(&bytes).unwrap()
            );

            let reply = SetBaselineContributionReply {
                contribute,
                extra: BTreeMap::new(),
            };
            let bytes = encode_canonical(&reply).unwrap();
            assert_eq!(
                reply,
                decode::<SetBaselineContributionReply>(&bytes).unwrap()
            );
        }
    }

    #[test]
    fn reset_spam_model_round_trips() {
        let req = ResetSpamModelRequest::default();
        let bytes = encode_canonical(&req).unwrap();
        assert_eq!(req, decode::<ResetSpamModelRequest>(&bytes).unwrap());
        let reply = ResetSpamModelReply::default();
        let bytes = encode_canonical(&reply).unwrap();
        assert_eq!(reply, decode::<ResetSpamModelReply>(&bytes).unwrap());
    }

    #[test]
    fn list_spam_training_history_round_trips() {
        // Request — both the newest-page (None) and the keyset-cursor form.
        let req = ListSpamTrainingHistoryRequest {
            limit: Some(50),
            before_history_id: Some(ByteBuf::from(vec![7u8; 16])),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        assert_eq!(
            req,
            decode::<ListSpamTrainingHistoryRequest>(&bytes).unwrap()
        );
        let req_first = ListSpamTrainingHistoryRequest::default();
        let bytes = encode_canonical(&req_first).unwrap();
        assert_eq!(
            req_first,
            decode::<ListSpamTrainingHistoryRequest>(&bytes).unwrap()
        );

        // Reply with rows covering both labels + a couple of sources + both the
        // plaintext (server-written) and sealed (client-written) row shapes.
        let reply = ListSpamTrainingHistoryReply {
            events: vec![
                SpamTrainingHistoryRow {
                    history_id: vec![1u8; 16],
                    message: "Win a prize · Junk".into(),
                    // Plaintext (server-written) row: no sealed subject; the
                    // subject already lives in `message`.
                    sealed_subject: Vec::new(),
                    mailbox: "Junk".into(),
                    label: SpamLabel::Spam,
                    source: TrainingSource::ImapJunkMove,
                    created_at_ms: 1_700_000_123,
                    // A plaintext serde_json delta (server-written row shape).
                    model_delta_applied: br#"["win","prize"]"#.to_vec(),
                    extra: BTreeMap::new(),
                },
                SpamTrainingHistoryRow {
                    history_id: vec![2u8; 16],
                    message: "Re: lunch · INBOX".into(),
                    sealed_subject: Vec::new(),
                    mailbox: "INBOX".into(),
                    label: SpamLabel::Ham,
                    source: TrainingSource::ManualOther,
                    created_at_ms: 1_700_000_456,
                    // Empty ⇒ a pre-build-item-3 nest omitting the field.
                    model_delta_applied: Vec::new(),
                    extra: BTreeMap::new(),
                },
                SpamTrainingHistoryRow {
                    history_id: vec![3u8; 16],
                    // Sealed (client-written) row: `message` degrades to the
                    // mailbox alone; a 1c client renders the unwrapped
                    // `sealed_subject` · `mailbox` instead.
                    message: "Junk".into(),
                    sealed_subject: vec![0x99; 40],
                    mailbox: "Junk".into(),
                    label: SpamLabel::Spam,
                    source: TrainingSource::ImapJunkFlag,
                    created_at_ms: 1_700_000_789,
                    // Opaque sealed delta (client-written row shape).
                    model_delta_applied: vec![0xAAu8; 64],
                    extra: BTreeMap::new(),
                },
            ],
            contribute_baseline: true,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        assert_eq!(
            reply,
            decode::<ListSpamTrainingHistoryReply>(&bytes).unwrap()
        );
    }

    #[test]
    fn spam_history_op_round_trips_both_variants() {
        // The externally-tagged data enum (like `EmailFilterAction`) round-trips
        // over the canonical DAG-CBOR codec for both variants.
        let insert = SpamHistoryOp::Insert {
            message_id: vec![0x22; 32],
            mailbox: "Junk".to_string(),
            sealed_subject: vec![0x33; 48],
            sealed_delta: vec![0x44; 80],
            label: SpamLabel::Spam,
            source: TrainingSource::ImapJunkMove,
        };
        let bytes = encode_canonical(&insert).unwrap();
        assert_eq!(insert, decode::<SpamHistoryOp>(&bytes).unwrap());

        let delete = SpamHistoryOp::Delete {
            history_id: vec![0x66; 16],
        };
        let bytes = encode_canonical(&delete).unwrap();
        assert_eq!(delete, decode::<SpamHistoryOp>(&bytes).unwrap());
    }

    #[test]
    fn bridge_spam_model_push_events_round_trip() {
        let updated = BridgeSpamModelUpdatedPush {
            actor_id: vec![3u8; 32],
        };
        let bytes = encode_canonical(&updated).unwrap();
        assert_eq!(
            updated,
            decode::<BridgeSpamModelUpdatedPush>(&bytes).unwrap()
        );
        let reset = BridgeSpamModelResetPush {
            actor_id: vec![4u8; 32],
        };
        let bytes = encode_canonical(&reset).unwrap();
        assert_eq!(reset, decode::<BridgeSpamModelResetPush>(&bytes).unwrap());
    }

    // ── I4 Phase D.5 outbound-queue round-trip tests ───────────────

    fn sample_outbound_unit() -> OutboundUnit {
        OutboundUnit {
            id: 4242,
            message_id: "abc.def@example.com".into(),
            original_sender: "alice@example.com".into(),
            recipient: "bob@dest.test".into(),
            raw_message:
                b"From: alice@example.com\r\nTo: bob@dest.test\r\nSubject: hi\r\n\r\nbody\r\n"
                    .to_vec(),
            attempt_count: 0,
            staged_body: None,
        }
    }

    #[test]
    fn fetch_outbound_due_request_round_trips() {
        let r = FetchOutboundDueRequest {
            max: 16,
            lease_seconds: 60,
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<FetchOutboundDueRequest>(&bytes).unwrap());
    }

    #[test]
    fn fetch_outbound_due_reply_empty_round_trips() {
        let r = FetchOutboundDueReply { units: vec![] };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<FetchOutboundDueReply>(&bytes).unwrap());
    }

    #[test]
    fn fetch_outbound_due_reply_one_unit_round_trips() {
        let r = FetchOutboundDueReply {
            units: vec![sample_outbound_unit()],
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<FetchOutboundDueReply>(&bytes).unwrap());
    }

    #[test]
    fn outbound_unit_rejects_unknown_field() {
        let extended = Value::Map(BTreeMap::from([
            ("id".to_string(), Value::Integer(7)),
            ("message_id".to_string(), Value::String("m".into())),
            ("original_sender".to_string(), Value::String("s".into())),
            ("recipient".to_string(), Value::String("r".into())),
            ("raw_message".to_string(), Value::Bytes(b"x".to_vec())),
            ("attempt_count".to_string(), Value::Integer(0)),
            ("zzz_unknown".to_string(), Value::Integer(1)),
        ]));
        let bytes = encode_canonical(&extended).unwrap();
        let parsed: Result<OutboundUnit, _> = decode(&bytes);
        assert!(
            parsed.is_err(),
            "expected unknown_field error, got {parsed:?}"
        );
    }

    #[test]
    fn mark_outbound_delivered_round_trips() {
        let r = MarkOutboundDeliveredRequest { id: 7 };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<MarkOutboundDeliveredRequest>(&bytes).unwrap());
        let reply = MarkOutboundDeliveredReply { ok: true };
        let bytes = encode_canonical(&reply).unwrap();
        assert_eq!(reply, decode::<MarkOutboundDeliveredReply>(&bytes).unwrap());
    }

    #[test]
    fn mark_outbound_failed_round_trips() {
        let r = MarkOutboundFailedRequest {
            id: 8,
            retry_after_seconds: 600,
            last_error: "451 4.7.1 temporary failure".into(),
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<MarkOutboundFailedRequest>(&bytes).unwrap());
        let reply = MarkOutboundFailedReply { ok: true };
        let bytes = encode_canonical(&reply).unwrap();
        assert_eq!(reply, decode::<MarkOutboundFailedReply>(&bytes).unwrap());
    }

    #[test]
    fn mark_outbound_bounced_round_trips() {
        let r = MarkOutboundBouncedRequest {
            id: 9,
            reason: "550 5.1.1 mailbox unknown".into(),
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<MarkOutboundBouncedRequest>(&bytes).unwrap());
        let reply = MarkOutboundBouncedReply { ok: true };
        let bytes = encode_canonical(&reply).unwrap();
        assert_eq!(reply, decode::<MarkOutboundBouncedReply>(&bytes).unwrap());
    }

    #[test]
    fn enqueue_outbound_mail_round_trips() {
        let r = EnqueueOutboundMailRequest {
            original_msgid: "abc.def@example.com".into(),
            original_sender: "alice@example.com".into(),
            recipients: vec!["bob@dest.test".into(), "carol@other.test".into()],
            raw_message: b"raw\r\n".to_vec(),
            ..Default::default()
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<EnqueueOutboundMailRequest>(&bytes).unwrap());
        let reply = EnqueueOutboundMailReply { ids: vec![1, 2] };
        let bytes = encode_canonical(&reply).unwrap();
        assert_eq!(reply, decode::<EnqueueOutboundMailReply>(&bytes).unwrap());
    }

    #[test]
    fn enqueue_outbound_mail_empty_recipients_round_trips() {
        let r = EnqueueOutboundMailRequest {
            original_msgid: "abc@ex.com".into(),
            original_sender: "alice@example.com".into(),
            recipients: vec![],
            raw_message: b"raw".to_vec(),
            ..Default::default()
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<EnqueueOutboundMailRequest>(&bytes).unwrap());
    }

    #[test]
    fn enqueue_outbound_mail_on_behalf_of_actor_round_trips() {
        // The MDA caller-scope field (caldav-server.md § Server-side
        // auto-schedule): present on the wire only when `Some`.
        let r = EnqueueOutboundMailRequest {
            original_msgid: "evt@example.com".into(),
            original_sender: "organizer@example.com".into(),
            recipients: vec!["bob@dest.test".into()],
            raw_message: b"BEGIN:VCALENDAR\r\n".to_vec(),
            on_behalf_of_actor: Some(vec![7u8; 32]),
            ..Default::default()
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<EnqueueOutboundMailRequest>(&bytes).unwrap());
        // `None` omits the key on the wire — the MTA's bytes stay byte-stable.
        let mta = EnqueueOutboundMailRequest {
            on_behalf_of_actor: None,
            ..r.clone()
        };
        let mta_bytes = encode_canonical(&mta).unwrap();
        assert!(mta_bytes.len() < bytes.len());
    }

    #[test]
    fn staged_body_ref_round_trips_and_the_key_is_a_byte_string() {
        // The staged-envelope reference (smtp-server.md § Message size limits):
        // a distinct type from MailBodyRef carrying the one-shot AEAD key beside
        // the ciphertext chunk hashes. The key MUST cross as a CBOR byte string
        // (major type 2) so the Go bridge's `[]byte key` decodes it — a
        // SecretBytes (array-of-ints) here would silently break Go interop.
        use fauna_core::secret::SecretByteBuf;
        let sref = StagedBodyRef {
            chunk_hashes: vec![
                ByteBuf::from(vec![0xAAu8; 32]),
                ByteBuf::from(vec![0xBBu8; 32]),
            ],
            total_bytes: 8_388_608,
            key: SecretByteBuf::from(vec![0x11u8; 32]),
        };
        let bytes = encode_canonical(&sref).unwrap();
        assert_eq!(sref, decode::<StagedBodyRef>(&bytes).unwrap());

        // Prove the `key` field is byte-string-encoded: an equivalent struct
        // whose key is a `ByteBuf` must encode byte-for-byte identically.
        #[derive(Serialize)]
        struct KeyAsByteBuf {
            chunk_hashes: Vec<ByteBuf>,
            total_bytes: u64,
            key: ByteBuf,
        }
        let twin = KeyAsByteBuf {
            chunk_hashes: vec![
                ByteBuf::from(vec![0xAAu8; 32]),
                ByteBuf::from(vec![0xBBu8; 32]),
            ],
            total_bytes: 8_388_608,
            key: ByteBuf::from(vec![0x11u8; 32]),
        };
        assert_eq!(
            bytes,
            encode_canonical(&twin).unwrap(),
            "StagedBodyRef.key must encode as a byte string, matching Go's []byte"
        );
    }

    #[test]
    fn outbound_staged_body_round_trips_and_is_omitted_inline() {
        // OutboundUnit / EnqueueOutboundMailRequest gain an additive staged_body;
        // omitted on the wire when None (so the pre-field bytes are byte-stable),
        // present as a byte-string-keyed reference when the body was staged.
        use fauna_core::secret::SecretByteBuf;
        let sref = StagedBodyRef {
            chunk_hashes: vec![ByteBuf::from(vec![0xCDu8; 32])],
            total_bytes: 4096,
            key: SecretByteBuf::from(vec![0x22u8; 32]),
        };
        let staged = OutboundUnit {
            id: 7,
            message_id: "m@x".into(),
            original_sender: "a@x".into(),
            recipient: "b@y".into(),
            raw_message: vec![],
            attempt_count: 0,
            staged_body: Some(sref.clone()),
        };
        let bytes = encode_canonical(&staged).unwrap();
        assert_eq!(staged, decode::<OutboundUnit>(&bytes).unwrap());
        let inline = OutboundUnit {
            raw_message: b"inline body".to_vec(),
            staged_body: None,
            ..staged.clone()
        };
        assert!(encode_canonical(&inline).unwrap().len() < bytes.len());

        let enq = EnqueueOutboundMailRequest {
            original_msgid: "m@x".into(),
            original_sender: "a@x".into(),
            recipients: vec!["b@y".into()],
            raw_message: vec![],
            staged_body: Some(sref),
            ..Default::default()
        };
        assert_eq!(
            enq,
            decode::<EnqueueOutboundMailRequest>(&encode_canonical(&enq).unwrap()).unwrap()
        );
    }

    #[test]
    fn deliver_sealed_scheduling_round_trips() {
        // The MDA mailbox-less rail (caldav-server.md § Server-side
        // auto-schedule, C3). Opaque sealed bytes + caller-scope fields.
        let r = DeliverSealedSchedulingRequest {
            on_behalf_of_actor: vec![7u8; 32],
            original_sender: "organizer@example.com".into(),
            recipient_actor_id: "aa".repeat(32),
            peer_domain: Some("peer.example.com".into()),
            channel_id: "bb".repeat(32),
            welcome_bytes: b"\x00sealed-welcome".to_vec(),
            app_envelope: b"\x01sealed-imip".to_vec(),
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<DeliverSealedSchedulingRequest>(&bytes).unwrap());
        // Same-nest: `peer_domain` omits the key on the wire.
        let same_nest = DeliverSealedSchedulingRequest {
            peer_domain: None,
            ..r.clone()
        };
        let same_bytes = encode_canonical(&same_nest).unwrap();
        assert!(same_bytes.len() < bytes.len());
        assert_eq!(
            same_nest,
            decode::<DeliverSealedSchedulingRequest>(&same_bytes).unwrap()
        );

        let reply = DeliverSealedSchedulingReply {
            inbox_id: 42,
            seq: 1,
        };
        let rbytes = encode_canonical(&reply).unwrap();
        assert_eq!(
            reply,
            decode::<DeliverSealedSchedulingReply>(&rbytes).unwrap()
        );
    }

    // ── N2 forward delivery trigger round-trip tests ──────────────

    #[test]
    fn fetch_recipient_forward_config_round_trips() {
        let r = FetchRecipientForwardConfigRequest {
            actor_id: vec![7u8; 32],
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(
            r,
            decode::<FetchRecipientForwardConfigRequest>(&bytes).unwrap()
        );
        let reply = FetchRecipientForwardConfigReply {
            forward_all_to: Some("bob@dest.test".into()),
        };
        let bytes = encode_canonical(&reply).unwrap();
        assert_eq!(
            reply,
            decode::<FetchRecipientForwardConfigReply>(&bytes).unwrap()
        );
        // Disabled (None) round-trips too.
        let none = FetchRecipientForwardConfigReply {
            forward_all_to: None,
        };
        let bytes = encode_canonical(&none).unwrap();
        assert_eq!(
            none,
            decode::<FetchRecipientForwardConfigReply>(&bytes).unwrap()
        );
    }

    #[test]
    fn fetch_recipient_filters_round_trips() {
        let r = FetchRecipientFiltersRequest {
            actor_id: vec![9u8; 32],
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<FetchRecipientFiltersRequest>(&bytes).unwrap());

        let reply = FetchRecipientFiltersReply {
            filters: vec![crate::email::EmailFilter {
                id: 1,
                name: "junk-by-score".into(),
                rules: vec![crate::email::EmailFilterRule::SpamScoreAtLeast { milli: 8000 }],
                combination: "all".into(),
                action: crate::email::EmailFilterAction::FileInto {
                    mailbox: "Junk".into(),
                },
                priority: 0,
                continue_on_match: false,
                created_at: 1_700_000_000_000,
                extra: Default::default(),
            }],
        };
        let bytes = encode_canonical(&reply).unwrap();
        assert_eq!(reply, decode::<FetchRecipientFiltersReply>(&bytes).unwrap());

        // Empty (no filters) round-trips too.
        let empty = FetchRecipientFiltersReply { filters: vec![] };
        let bytes = encode_canonical(&empty).unwrap();
        assert_eq!(empty, decode::<FetchRecipientFiltersReply>(&bytes).unwrap());
    }

    #[test]
    fn forward_message_round_trips() {
        let r = ForwardMessageRequest {
            actor_id: vec![3u8; 32],
            original_msgid: "abc.def@example.com".into(),
            original_sender: "alice@example.com".into(),
            destination: "bob@dest.test".into(),
            raw_message: b"From: alice@example.com\r\n\r\nhi\r\n".to_vec(),
            rule_id_or_forward_all: "forward-all".into(),
            copy_mode: ForwardCopyMode::Copy,
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<ForwardMessageRequest>(&bytes).unwrap());
        let reply = ForwardMessageReply {
            id: 42,
            queued: false,
        };
        let bytes = encode_canonical(&reply).unwrap();
        assert_eq!(reply, decode::<ForwardMessageReply>(&bytes).unwrap());
        let queued = ForwardMessageReply {
            id: 7,
            queued: true,
        };
        let bytes = encode_canonical(&queued).unwrap();
        assert_eq!(queued, decode::<ForwardMessageReply>(&bytes).unwrap());
    }

    #[test]
    fn forward_copy_mode_round_trips_both_variants() {
        for m in [ForwardCopyMode::Copy, ForwardCopyMode::Redirect] {
            let bytes = encode_canonical(&m).unwrap();
            assert_eq!(m, decode::<ForwardCopyMode>(&bytes).unwrap());
        }
    }

    #[test]
    fn decode_srs_bounce_round_trips() {
        let r = DecodeSrsBounceRequest {
            local_part: "SRS0=HHHH=AB=42=gmail.com=alice".into(),
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<DecodeSrsBounceRequest>(&bytes).unwrap());

        // Verified ("ok") reply carries the full triple.
        let ok = DecodeSrsBounceReply {
            outcome: "ok".into(),
            forwarder_actor_id: vec![7u8; 32],
            original_sender: "alice@example.com".into(),
            original_destination: "bob@dest.test".into(),
        };
        let bytes = encode_canonical(&ok).unwrap();
        assert_eq!(ok, decode::<DecodeSrsBounceReply>(&bytes).unwrap());

        // A failure reply leaves the payload empty (empty bytes round-trip).
        let fail = DecodeSrsBounceReply {
            outcome: "mac_fail".into(),
            ..Default::default()
        };
        let bytes = encode_canonical(&fail).unwrap();
        assert_eq!(fail, decode::<DecodeSrsBounceReply>(&bytes).unwrap());
    }

    #[test]
    fn rotate_srs_secret_round_trips() {
        let req = RotateSrsSecretRequest::default();
        let bytes = encode_canonical(&req).unwrap();
        assert_eq!(req, decode::<RotateSrsSecretRequest>(&bytes).unwrap());
        let reply = RotateSrsSecretReply {
            rotated_at: 1_700_000_000,
            extra: Default::default(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        assert_eq!(reply, decode::<RotateSrsSecretReply>(&bytes).unwrap());
    }

    // ── T2.1 fetch_mta_sts_policy round-trip tests ─────────────────

    #[test]
    fn fetch_mta_sts_policy_request_round_trips() {
        let r = FetchMtaStsPolicyRequest {
            domain: "dest.test".into(),
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<FetchMtaStsPolicyRequest>(&bytes).unwrap());
    }

    #[test]
    fn fetch_mta_sts_policy_reply_all_four_outcomes_round_trip() {
        // The three no-policy outcomes carry `policy: None`.
        for outcome in ["not_published", "fetch_error", "invalid"] {
            let r = FetchMtaStsPolicyReply {
                outcome: outcome.into(),
                policy: None,
            };
            let bytes = encode_canonical(&r).unwrap();
            assert_eq!(
                r,
                decode::<FetchMtaStsPolicyReply>(&bytes).unwrap(),
                "outcome={outcome} (policy: None) must round-trip"
            );
        }
        // `found` carries a populated policy (mode "enforce", 2 mx patterns).
        let r = FetchMtaStsPolicyReply {
            outcome: "found".into(),
            policy: Some(MtaStsPolicyWire {
                id: "20260523T000000".into(),
                mode: "enforce".into(),
                mx: vec!["mx1.dest.test".into(), "*.mx.dest.test".into()],
                max_age_secs: 604_800,
            }),
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<FetchMtaStsPolicyReply>(&bytes).unwrap());
    }

    // ── T2.1b fetch_tlsa round-trip tests ──────────────────────────

    #[test]
    fn fetch_tlsa_request_round_trips() {
        let r = FetchTlsaRequest {
            mx_host: "mail.dest.test".into(),
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<FetchTlsaRequest>(&bytes).unwrap());
    }

    #[test]
    fn fetch_tlsa_reply_empty_and_multi_record_round_trip() {
        // Empty (no DANE / fall-through) round-trips.
        let empty = FetchTlsaReply { records: vec![] };
        let bytes = encode_canonical(&empty).unwrap();
        assert_eq!(empty, decode::<FetchTlsaReply>(&bytes).unwrap());

        // DANE-EE SHA-256 (selector full cert) + DANE-TA SPKI SHA-512.
        let multi = FetchTlsaReply {
            records: vec![
                TlsaRecordWire {
                    usage: 3,
                    selector: 0,
                    matching: 1,
                    data: vec![0xab; 32],
                },
                TlsaRecordWire {
                    usage: 2,
                    selector: 1,
                    matching: 2,
                    data: vec![0xcd; 64],
                },
            ],
        };
        let bytes = encode_canonical(&multi).unwrap();
        assert_eq!(multi, decode::<FetchTlsaReply>(&bytes).unwrap());
    }

    // ── T2.4 report_tls_attempt round-trip tests ───────────────────

    #[test]
    fn report_tls_attempt_request_success_and_failure_round_trip() {
        // Successful opportunistic attempt, no STS / no DANE.
        let ok = ReportTlsAttemptRequest {
            recipient_domain: "dest.test".into(),
            mx_host: "mx.dest.test".into(),
            result_type: None,
            mta_sts_outcome: "not_published".into(),
            mta_sts_policy: None,
            tlsa_records: vec![],
        };
        let bytes = encode_canonical(&ok).unwrap();
        assert_eq!(ok, decode::<ReportTlsAttemptRequest>(&bytes).unwrap());

        // Failed DANE-pinned attempt under an enforce STS policy: both the
        // sts policy and the tlsa records present, a failure result-type set.
        let dane_fail = ReportTlsAttemptRequest {
            recipient_domain: "dest.test".into(),
            mx_host: "mx.dest.test".into(),
            result_type: Some("tlsa-invalid".into()),
            mta_sts_outcome: "found".into(),
            mta_sts_policy: Some(MtaStsPolicyWire {
                id: "20260523T000000".into(),
                mode: "enforce".into(),
                mx: vec!["mx.dest.test".into()],
                max_age_secs: 604_800,
            }),
            tlsa_records: vec![TlsaRecordWire {
                usage: 3,
                selector: 1,
                matching: 1,
                data: vec![0xde, 0xad, 0xbe, 0xef],
            }],
        };
        let bytes = encode_canonical(&dane_fail).unwrap();
        assert_eq!(
            dane_fail,
            decode::<ReportTlsAttemptRequest>(&bytes).unwrap()
        );
    }

    #[test]
    fn report_tls_attempt_reply_round_trips() {
        let r = ReportTlsAttemptReply { ok: true };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<ReportTlsAttemptReply>(&bytes).unwrap());
    }

    // ── I5 Phase D.6 — CREATE / DELETE / RENAME round-trip tests ──

    #[test]
    fn create_mailbox_request_round_trips() {
        let r = CreateMailboxRequest {
            actor_id: vec![7u8; 32],
            name: "Projects/2026".into(),
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<CreateMailboxRequest>(&bytes).unwrap());
    }

    #[test]
    fn create_mailbox_reply_created_round_trips() {
        let r = CreateMailboxReply::Created {
            uid_validity: 0xCAFEBEEF,
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<CreateMailboxReply>(&bytes).unwrap());
    }

    #[test]
    fn create_mailbox_reply_already_exists_round_trips() {
        let r = CreateMailboxReply::AlreadyExists;
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<CreateMailboxReply>(&bytes).unwrap());
    }

    #[test]
    fn create_mailbox_reply_reserved_round_trips() {
        let r = CreateMailboxReply::Reserved;
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<CreateMailboxReply>(&bytes).unwrap());
    }

    #[test]
    fn create_mailbox_reply_invalid_name_round_trips() {
        let r = CreateMailboxReply::InvalidName {
            reason: "too long".into(),
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<CreateMailboxReply>(&bytes).unwrap());
    }

    #[test]
    fn delete_mailbox_request_round_trips() {
        let r = DeleteMailboxRequest {
            actor_id: vec![1u8; 32],
            name: "Old Stuff".into(),
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<DeleteMailboxRequest>(&bytes).unwrap());
    }

    #[test]
    fn delete_mailbox_reply_deleted_round_trips() {
        let r = DeleteMailboxReply::Deleted;
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<DeleteMailboxReply>(&bytes).unwrap());
    }

    #[test]
    fn delete_mailbox_reply_no_such_mailbox_round_trips() {
        let r = DeleteMailboxReply::NoSuchMailbox;
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<DeleteMailboxReply>(&bytes).unwrap());
    }

    #[test]
    fn delete_mailbox_reply_reserved_round_trips() {
        let r = DeleteMailboxReply::Reserved;
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<DeleteMailboxReply>(&bytes).unwrap());
    }

    #[test]
    fn delete_mailbox_reply_not_empty_round_trips() {
        let r = DeleteMailboxReply::NotEmpty;
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<DeleteMailboxReply>(&bytes).unwrap());
    }

    #[test]
    fn rename_mailbox_request_round_trips() {
        let r = RenameMailboxRequest {
            actor_id: vec![2u8; 32],
            old_name: "old".into(),
            new_name: "new".into(),
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<RenameMailboxRequest>(&bytes).unwrap());
    }

    #[test]
    fn rename_mailbox_reply_renamed_round_trips() {
        let r = RenameMailboxReply::Renamed;
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<RenameMailboxReply>(&bytes).unwrap());
    }

    #[test]
    fn rename_mailbox_reply_no_such_source_round_trips() {
        let r = RenameMailboxReply::NoSuchSource;
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<RenameMailboxReply>(&bytes).unwrap());
    }

    #[test]
    fn rename_mailbox_reply_reserved_source_round_trips() {
        let r = RenameMailboxReply::ReservedSource;
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<RenameMailboxReply>(&bytes).unwrap());
    }

    #[test]
    fn rename_mailbox_reply_target_reserved_round_trips() {
        let r = RenameMailboxReply::TargetReserved;
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<RenameMailboxReply>(&bytes).unwrap());
    }

    #[test]
    fn rename_mailbox_reply_target_exists_round_trips() {
        let r = RenameMailboxReply::TargetExists;
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<RenameMailboxReply>(&bytes).unwrap());
    }

    #[test]
    fn rename_mailbox_reply_invalid_name_round_trips() {
        let r = RenameMailboxReply::InvalidName {
            reason: "contains NUL".into(),
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<RenameMailboxReply>(&bytes).unwrap());
    }

    // ── I5 Phase D.7 — SUBSCRIBE / UNSUBSCRIBE + ListMailboxes field-add ──

    #[test]
    fn subscribe_mailbox_request_round_trips() {
        let r = SubscribeMailboxRequest {
            actor_id: vec![1u8; 32],
            mailbox: "Saved Searches".into(),
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<SubscribeMailboxRequest>(&bytes).unwrap());
    }

    #[test]
    fn subscribe_mailbox_reply_subscribed_round_trips() {
        let r = SubscribeMailboxReply::Subscribed;
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<SubscribeMailboxReply>(&bytes).unwrap());
    }

    #[test]
    fn unsubscribe_mailbox_request_round_trips() {
        let r = UnsubscribeMailboxRequest {
            actor_id: vec![2u8; 32],
            mailbox: "INBOX".into(),
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<UnsubscribeMailboxRequest>(&bytes).unwrap());
    }

    #[test]
    fn unsubscribe_mailbox_reply_unsubscribed_round_trips() {
        let r = UnsubscribeMailboxReply::Unsubscribed;
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<UnsubscribeMailboxReply>(&bytes).unwrap());
    }

    /// The new `subscribed_only` field on `ListMailboxesRequest`
    /// must encode + decode losslessly when set explicitly.
    #[test]
    fn list_mailboxes_request_subscribed_only_true_round_trips() {
        let r = ListMailboxesRequest {
            actor_id: vec![5u8; 32],
            subscribed_only: true,
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<ListMailboxesRequest>(&bytes).unwrap());
    }

    /// A CBOR encoding of a `ListMailboxesRequest` with actor_id only (plain
    /// LIST, no `subscribed_only` field) must decode with
    /// `subscribed_only = false` (via `#[serde(default)]`). Construct the
    /// bytes by encoding a stand-in shape and then decoding into the
    /// current struct.
    #[test]
    fn list_mailboxes_request_without_subscribed_only_decodes_with_default_false() {
        // Stand-in for the plain-LIST wire shape (single serde_bytes actor_id
        // field, no other keys).
        #[derive(Serialize)]
        struct ActorOnlyListMailboxesRequest {
            #[serde(with = "serde_bytes")]
            actor_id: Vec<u8>,
        }
        let actor_only = ActorOnlyListMailboxesRequest {
            actor_id: vec![7u8; 32],
        };
        let bytes = encode_canonical(&actor_only).unwrap();
        let decoded: ListMailboxesRequest = decode(&bytes).unwrap();
        assert_eq!(decoded.actor_id, vec![7u8; 32]);
        assert!(!decoded.subscribed_only, "default = false");
    }

    // ── I5 Phase F.1 — subscribe_mailbox_state / BridgeMailboxStatePush ──

    #[test]
    fn subscribe_mailbox_state_request_round_trips() {
        let r = SubscribeMailboxStateRequest {
            actor_id: vec![3u8; 32],
            mailbox: "INBOX".into(),
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<SubscribeMailboxStateRequest>(&bytes).unwrap());
    }

    #[test]
    fn subscribe_mailbox_state_request_empty_mailbox_round_trips() {
        // Empty mailbox = NOTIFY wildcard subscription per goal-doc.
        let r = SubscribeMailboxStateRequest {
            actor_id: vec![4u8; 32],
            mailbox: String::new(),
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<SubscribeMailboxStateRequest>(&bytes).unwrap());
    }

    #[test]
    fn subscribe_mailbox_state_reply_subscribed_round_trips() {
        let r = SubscribeMailboxStateReply::Subscribed {
            subscription_id: 42,
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<SubscribeMailboxStateReply>(&bytes).unwrap());
    }

    #[test]
    fn mailbox_state_event_append_round_trips() {
        let e = MailboxStateEvent::Append {
            uid: 7,
            flags: vec!["\\Seen".into(), "\\Recent".into()],
            modseq: 12345,
        };
        let bytes = encode_canonical(&e).unwrap();
        assert_eq!(e, decode::<MailboxStateEvent>(&bytes).unwrap());
    }

    #[test]
    fn mailbox_state_event_flags_round_trips() {
        let e = MailboxStateEvent::Flags {
            uid: 11,
            flags: vec!["\\Deleted".into()],
            modseq: 99,
        };
        let bytes = encode_canonical(&e).unwrap();
        assert_eq!(e, decode::<MailboxStateEvent>(&bytes).unwrap());
    }

    #[test]
    fn mailbox_state_event_expunge_round_trips() {
        let e = MailboxStateEvent::Expunge {
            uid: 5,
            modseq: 200,
        };
        let bytes = encode_canonical(&e).unwrap();
        assert_eq!(e, decode::<MailboxStateEvent>(&bytes).unwrap());
    }

    #[test]
    fn mailbox_state_event_move_round_trips() {
        for side in [MoveSide::Source, MoveSide::Destination] {
            let e = MailboxStateEvent::Move {
                src_uid: 3,
                dst_uid: 8,
                modseq_src: 50,
                modseq_dst: 51,
                side,
            };
            let bytes = encode_canonical(&e).unwrap();
            assert_eq!(e, decode::<MailboxStateEvent>(&bytes).unwrap());
        }
    }

    /// `side` is required: a `Move` without it does not decode.
    #[test]
    fn mailbox_state_event_move_without_side_is_rejected() {
        #[derive(Serialize)]
        struct SidelessMove {
            kind: &'static str,
            src_uid: u32,
            dst_uid: u32,
            modseq_src: i64,
            modseq_dst: i64,
        }
        let bytes = encode_canonical(&SidelessMove {
            kind: "move",
            src_uid: 3,
            dst_uid: 8,
            modseq_src: 50,
            modseq_dst: 51,
        })
        .unwrap();
        assert!(decode::<MailboxStateEvent>(&bytes).is_err());
    }

    #[test]
    fn bridge_mailbox_state_push_round_trips_with_each_event() {
        let cases = [
            MailboxStateEvent::Append {
                uid: 1,
                flags: vec!["\\Recent".into()],
                modseq: 10,
            },
            MailboxStateEvent::Flags {
                uid: 2,
                flags: vec!["\\Seen".into()],
                modseq: 11,
            },
            MailboxStateEvent::Expunge { uid: 3, modseq: 12 },
            MailboxStateEvent::Move {
                src_uid: 4,
                dst_uid: 5,
                modseq_src: 13,
                modseq_dst: 14,
                side: MoveSide::Destination,
            },
        ];
        for (i, event) in cases.into_iter().enumerate() {
            let push = BridgeMailboxStatePush {
                subscription_id: (i + 100) as u64,
                actor_id: vec![9u8; 32],
                mailbox: "INBOX".into(),
                event,
            };
            let bytes = encode_canonical(&push).unwrap();
            assert_eq!(push, decode::<BridgeMailboxStatePush>(&bytes).unwrap());
        }
    }

    #[test]
    fn bridge_config_changed_push_round_trips() {
        for reason in [
            config_change_reason::MAIL_ENABLED,
            config_change_reason::LOCAL_DOMAINS,
            config_change_reason::SPAM_POLICY,
            config_change_reason::AUTH_POLICY,
            config_change_reason::SUBMISSION_POLICY,
            config_change_reason::IMAP_POLICY,
            config_change_reason::OUTBOUND_POLICY,
            config_change_reason::NODE_MODE,
            config_change_reason::TLS,
            // An unknown/future reason must still round-trip (String, not enum).
            "dkim",
        ] {
            let push = BridgeConfigChangedPush {
                reason: reason.to_string(),
            };
            let bytes = encode_canonical(&push).unwrap();
            assert_eq!(push, decode::<BridgeConfigChangedPush>(&bytes).unwrap());
        }
    }

    // ── Plan 2 T6 — QResyncHint, SelectMailboxRequest extensions,
    //    SyncCalendarSinceRequest extension, SyncCalendarSinceReply::Stale ─

    #[test]
    fn select_mailbox_request_omits_optional_fields_by_default() {
        let req = SelectMailboxRequest {
            actor_id: vec![1u8; 32],
            mailbox: "INBOX".into(),
            ..Default::default()
        };
        let bytes = encode_canonical(&req).expect("encode");
        let back: SelectMailboxRequest = decode(&bytes).expect("decode");
        assert_eq!(back, req);
        assert!(back.client_qresync.is_none());
        assert!(back.mua_id.is_none());
    }

    #[test]
    fn select_mailbox_request_round_trips_qresync_and_mua_id() {
        let req = SelectMailboxRequest {
            actor_id: vec![2u8; 32],
            mailbox: "INBOX".into(),
            client_qresync: Some(QResyncHint {
                last_uid_validity: 1_700_000_000,
                last_modseq: 99,
            }),
            mua_id: Some("Thunderbird/115.0".into()),
        };
        let bytes = encode_canonical(&req).expect("encode");
        let back: SelectMailboxRequest = decode(&bytes).expect("decode");
        assert_eq!(back, req);
    }

    #[test]
    fn sync_calendar_since_request_round_trips_with_optional_mua_id() {
        let req = SyncCalendarSinceRequest {
            actor_id: vec![3u8; 32],
            calendar_id: vec![4u8; 32],
            sync_token: "42".into(),
            limit: 0,
            mua_id: Some("Apple Calendar/14.0".into()),
        };
        let bytes = encode_canonical(&req).expect("encode");
        let back: SyncCalendarSinceRequest = decode(&bytes).expect("decode");
        assert_eq!(back, req);
    }

    #[test]
    fn sync_calendar_since_reply_round_trips_stale() {
        let reply = SyncCalendarSinceReply::Stale { server_modseq: 42 };
        let bytes = encode_canonical(&reply).expect("encode");
        let back: SyncCalendarSinceReply = decode(&bytes).expect("decode");
        assert_eq!(back, reply);
    }

    // ── A3 Bucket B — the five `put_<substruct>_policy` request types ──
    //
    // Each carries one `Option<T>` per `FetchConfigReply` sub-struct field
    // (leave/set). All fields are bool/int/string/Vec — dag-cbor-safe (no
    // floats), no `Option<Option<T>>`. The `..Default::default()` fixture
    // shape keeps two parallel sessions that grow the same sub-struct from
    // colliding on the grown axis (the struct-update fixture convention).

    #[test]
    fn put_spam_policy_request_full_round_trips() {
        let r = PutSpamPolicyRequest {
            max_score_before_spam_folder: Some(4),
            max_score_before_reject: Some(14),
            dnsbl_servers: Some(vec!["bl1.example".into(), "bl2.example".into()]),
            reject_no_rdns: Some(true),
            greylist_enabled: Some(false),
            greylist_delay_secs: Some(0),
            max_conn_per_min: Some(25),
            fcrdns_mode: Some("off".into()),
            helo_identity_required: Some(false),
            reject_fcrdns_fail: Some(true),
            max_message_bytes: Some(100_000_000),
            bayesian_weight_milli: Some(600),
            bayesian_min_samples: Some(80),
            bayesian_full_confidence_samples: Some(400),
            training_history_retention_days: Some(60),
            unlisted_recipient_penalty: Some(1000),
            baseline_standing_publish: true,
            extra: Default::default(),
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<PutSpamPolicyRequest>(&bytes).unwrap());
    }

    #[test]
    fn put_spam_policy_request_partial_round_trips() {
        // Empty DNSBL list (Some(vec![])) is a meaningful override
        // distinct from None; all-None default decodes too.
        let r = PutSpamPolicyRequest {
            dnsbl_servers: Some(Vec::new()),
            max_score_before_reject: Some(20),
            ..Default::default()
        };
        let bytes = encode_canonical(&r).unwrap();
        let back = decode::<PutSpamPolicyRequest>(&bytes).unwrap();
        assert_eq!(r, back);
        assert_eq!(back.dnsbl_servers, Some(Vec::<String>::new()));
        assert!(back.greylist_enabled.is_none());

        let empty = PutSpamPolicyRequest::default();
        let bytes = encode_canonical(&empty).unwrap();
        assert_eq!(empty, decode::<PutSpamPolicyRequest>(&bytes).unwrap());
    }

    #[test]
    fn put_spam_policy_request_bayesian_round_trips() {
        // The four Tier-2 bayesian/retention overrides round-trip as
        // `Option<u32>` (None ⇒ keep catalog default), alongside the perimeter
        // tiers — one `put_spam_policy` kind carries the whole sub-struct.
        let r = PutSpamPolicyRequest {
            max_score_before_spam_folder: Some(6),
            bayesian_weight_milli: Some(900),
            bayesian_min_samples: Some(40),
            bayesian_full_confidence_samples: Some(250),
            training_history_retention_days: Some(7),
            ..Default::default()
        };
        let bytes = encode_canonical(&r).unwrap();
        let back = decode::<PutSpamPolicyRequest>(&bytes).unwrap();
        assert_eq!(r, back);
        assert_eq!(back.bayesian_weight_milli, Some(900));
        assert_eq!(back.training_history_retention_days, Some(7));
        assert!(back.bayesian_min_samples.is_some());
        // All-None default decodes (no override).
        let empty = PutSpamPolicyRequest::default();
        let bytes = encode_canonical(&empty).unwrap();
        assert_eq!(empty, decode::<PutSpamPolicyRequest>(&bytes).unwrap());
        assert!(empty.bayesian_weight_milli.is_none());
    }

    #[test]
    fn put_alias_policy_request_full_round_trips() {
        let r = PutAliasPolicyRequest {
            exact_aliases_max: Some(5),
            reserved_local_parts: Some(vec!["postmaster".into(), "sales".into()]),
            subaddressing_enabled: Some(false),
            wildcard_prefix_enabled: Some(false),
            extra: Default::default(),
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<PutAliasPolicyRequest>(&bytes).unwrap());
    }

    #[test]
    fn put_alias_policy_request_partial_and_empty_round_trip() {
        // Empty reserved list (Some(vec![])) is a meaningful override
        // (clear the reservation) distinct from None; all-None default
        // decodes too.
        let r = PutAliasPolicyRequest {
            reserved_local_parts: Some(Vec::new()),
            exact_aliases_max: Some(50),
            ..Default::default()
        };
        let bytes = encode_canonical(&r).unwrap();
        let back = decode::<PutAliasPolicyRequest>(&bytes).unwrap();
        assert_eq!(r, back);
        assert_eq!(back.reserved_local_parts, Some(Vec::<String>::new()));
        assert!(back.subaddressing_enabled.is_none());

        let empty = PutAliasPolicyRequest::default();
        let bytes = encode_canonical(&empty).unwrap();
        assert_eq!(empty, decode::<PutAliasPolicyRequest>(&bytes).unwrap());
    }

    #[test]
    fn get_alias_policy_request_round_trips() {
        // The admin read twin carries no arguments — it returns the full
        // effective `AliasPolicy`.
        let r = GetAliasPolicyRequest::default();
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<GetAliasPolicyRequest>(&bytes).unwrap());
    }

    #[test]
    fn alias_policy_reply_round_trips_default_and_custom() {
        // Default carries the catalog values, from the one shared definition
        // in `fauna_core::mail_aliases` (no `fauna-mail` dep needed here).
        let d = AliasPolicy::default();
        assert_eq!(d.exact_aliases_max, 20);
        assert_eq!(d.reserved_local_parts.len(), 6);
        assert!(d.subaddressing_enabled);
        assert!(d.wildcard_prefix_enabled);
        let bytes = encode_canonical(&d).unwrap();
        assert_eq!(d, decode::<AliasPolicy>(&bytes).unwrap());

        let custom = AliasPolicy {
            exact_aliases_max: 5,
            reserved_local_parts: vec!["postmaster".into()],
            subaddressing_enabled: false,
            wildcard_prefix_enabled: false,
            extra: Default::default(),
        };
        let bytes = encode_canonical(&custom).unwrap();
        assert_eq!(custom, decode::<AliasPolicy>(&bytes).unwrap());
    }

    #[test]
    fn put_auth_policy_request_round_trips() {
        let r = PutAuthPolicyRequest {
            enforce_dmarc: Some(false),
            enforce_dkim: Some(true),
            log_only: Some(true),
            max_auth_failures_per_minute: Some(15),
            ..Default::default()
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<PutAuthPolicyRequest>(&bytes).unwrap());
        let empty = PutAuthPolicyRequest::default();
        let bytes = encode_canonical(&empty).unwrap();
        assert_eq!(empty, decode::<PutAuthPolicyRequest>(&bytes).unwrap());
    }

    #[test]
    fn put_submission_policy_request_round_trips() {
        let r = PutSubmissionPolicyRequest {
            max_per_day: Some(500),
            max_recipients_per_message: Some(50),
            extra: Default::default(),
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<PutSubmissionPolicyRequest>(&bytes).unwrap());
    }

    #[test]
    fn put_imap_policy_request_round_trips() {
        let r = PutImapPolicyRequest {
            idle_timeout_secs: Some(900),
            tombstone_retention_days: Some(14),
            delete_nonempty: Some("allowed".into()),
            bodystructure_cache_max: Some(8192),
            // u64 storage ceiling: 2 GiB
            storage_bytes_default: Some(2 << 30),
            message_count_default: Some(100_000),
            extra: Default::default(),
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<PutImapPolicyRequest>(&bytes).unwrap());
    }

    #[test]
    fn put_outbound_policy_request_round_trips() {
        let r = PutOutboundPolicyRequest {
            retry_schedule_seconds: Some(vec![0, 600, 3600]),
            permanent_failure_timeout_hours: Some(72),
            delay_warning_at_hours: Some(2),
            ndr_rate_limit_days: Some(3),
            suppress_ndr_spf_hardfail: Some(false),
            suppress_ndr_dmarc_reject: Some(false),
            postmaster_cc_bounces: Some(false),
            tlsrpt_send_reports: Some(false),
            ipv6_enabled: Some(false),
            treat_5xx_as_transient: Some(vec!["5.7.1".into()]),
            extra: Default::default(),
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<PutOutboundPolicyRequest>(&bytes).unwrap());
    }

    #[test]
    fn put_policy_reply_round_trips() {
        let r = PutPolicyReply {
            ok: true,
            extra: Default::default(),
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<PutPolicyReply>(&bytes).unwrap());
    }

    #[test]
    fn deliverability_diagnostics_reply_round_trips() {
        let r = RunDeliverabilityDiagnosticsReply {
            checks: vec![
                DiagnosticCheckResult {
                    name: "SPF record present".into(),
                    status: "pass".into(),
                    detail: "v=spf1 mx ~all".into(),
                },
                DiagnosticCheckResult {
                    name: "Reverse-DNS matches HELO".into(),
                    status: "fail".into(),
                    detail: "PTR vps-1.provider.test != mail.example.com".into(),
                },
            ],
            ran_at: 1_760_000_000,
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(
            r,
            decode::<RunDeliverabilityDiagnosticsReply>(&bytes).unwrap()
        );
        // Request is the empty-struct unit.
        let req = RunDeliverabilityDiagnosticsRequest {};
        assert_eq!(
            req,
            decode::<RunDeliverabilityDiagnosticsRequest>(&encode_canonical(&req).unwrap())
                .unwrap()
        );
    }

    #[test]
    fn blocklist_self_check_reply_round_trips() {
        let r = BlocklistSelfCheckRunReply {
            checked_at: 1_760_000_000,
            outbound_ip: "203.0.113.7".into(),
            results: vec![
                BlocklistServerResult {
                    server: "zen.spamhaus.org".into(),
                    listed: false,
                    reason: String::new(),
                    error: String::new(),
                },
                BlocklistServerResult {
                    server: "b.barracudacentral.org".into(),
                    listed: true,
                    reason: "Listed for spam pattern X".into(),
                    error: String::new(),
                },
            ],
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<BlocklistSelfCheckRunReply>(&bytes).unwrap());
        // `server = Some` force-refresh request round-trips.
        let req = BlocklistSelfCheckRunRequest {
            server: Some("bl.spamcop.net".into()),
        };
        assert_eq!(
            req,
            decode::<BlocklistSelfCheckRunRequest>(&encode_canonical(&req).unwrap()).unwrap()
        );
    }

    #[test]
    fn mail_health_round_trips_and_tolerates_an_empty_map() {
        let r = MailHealthReply {
            state: "blocklisted".into(),
            checks: vec![MailHealthCheck {
                label_key: "admin.mail_page.health_check_blocklist".into(),
                state: "fail".into(),
                detail: "listed on bl.spamcop.net".into(),
                ..Default::default()
            }],
            last_outbound_delivered_at: Some(1_760_000_000),
            last_inbound_accepted_at: None,
            delist_url: Some("https://www.spamcop.net/bl.shtml".into()),
            ..Default::default()
        };
        assert_eq!(
            r,
            decode::<MailHealthReply>(&encode_canonical(&r).unwrap()).unwrap()
        );
        // Every field is `#[serde(default)]` — an older/newer peer's sparse map
        // decodes to the default rather than failing.
        let empty = encode_canonical(&BTreeMap::<String, u8>::new()).unwrap();
        assert_eq!(
            decode::<MailHealthReply>(&empty).unwrap(),
            MailHealthReply::default()
        );
        let req = MailHealthRequest {};
        assert_eq!(
            req,
            decode::<MailHealthRequest>(&encode_canonical(&req).unwrap()).unwrap()
        );
    }

    #[test]
    fn outbound_warmup_status_round_trips() {
        // Mid-ramp state: day 3, 120 used of 200, ramp ends in ~26 days.
        let r = OutboundWarmupStatusReply {
            current_day: 3,
            today_used: 120,
            today_max: Some(200),
            ramp_end_date: 1_762_500_000,
            lifetime_total: 4321,
            first_outbound_at: 1_760_000_000,
            last_reset_at: 0,
        };
        let bytes = encode_canonical(&r).unwrap();
        assert_eq!(r, decode::<OutboundWarmupStatusReply>(&bytes).unwrap());
        // Day-30+ unlimited state: today_max = None.
        let unlimited = OutboundWarmupStatusReply {
            current_day: 30,
            today_used: 99_999,
            today_max: None,
            ..Default::default()
        };
        assert_eq!(
            unlimited,
            decode::<OutboundWarmupStatusReply>(&encode_canonical(&unlimited).unwrap()).unwrap()
        );
        // Empty request types round-trip.
        let sreq = OutboundWarmupStatusRequest {};
        assert_eq!(
            sreq,
            decode::<OutboundWarmupStatusRequest>(&encode_canonical(&sreq).unwrap()).unwrap()
        );
        let rreq = OutboundWarmupResetRequest {};
        assert_eq!(
            rreq,
            decode::<OutboundWarmupResetRequest>(&encode_canonical(&rreq).unwrap()).unwrap()
        );
    }

    #[test]
    fn deliverability_history_round_trips() {
        // Blocklist history: a window request + a reply with two dated rows, each
        // carrying the same structured per-DNSBL verdicts as a live run.
        let breq = ListBlocklistSelfCheckHistoryRequest { window_days: 30 };
        assert_eq!(
            breq,
            decode::<ListBlocklistSelfCheckHistoryRequest>(&encode_canonical(&breq).unwrap())
                .unwrap()
        );
        let breply = ListBlocklistSelfCheckHistoryReply {
            rows: vec![
                BlocklistSelfCheckHistoryRow {
                    checked_at: 1_762_000_000,
                    results: vec![BlocklistServerResult {
                        server: "zen.spamhaus.org".into(),
                        listed: true,
                        reason: "Listed".into(),
                        error: String::new(),
                    }],
                },
                BlocklistSelfCheckHistoryRow {
                    checked_at: 1_761_000_000,
                    results: vec![],
                },
            ],
        };
        assert_eq!(
            breply,
            decode::<ListBlocklistSelfCheckHistoryReply>(&encode_canonical(&breply).unwrap())
                .unwrap()
        );
        // window_days defaults to 0 (⇒ full retention) when omitted.
        let empty_breq: ListBlocklistSelfCheckHistoryRequest =
            decode(&encode_canonical(&Value::Map(BTreeMap::new())).unwrap()).unwrap();
        assert_eq!(empty_breq.window_days, 0);

        // Diagnostic-run history: a limit request + a reply row with the checklist
        // + the 32-byte actor id that ran it.
        let dreq = ListDeliverabilityDiagnosticRunsRequest { limit: 10 };
        assert_eq!(
            dreq,
            decode::<ListDeliverabilityDiagnosticRunsRequest>(&encode_canonical(&dreq).unwrap())
                .unwrap()
        );
        let dreply = ListDeliverabilityDiagnosticRunsReply {
            rows: vec![DiagnosticRunHistoryRow {
                ran_at: 1_762_500_000,
                checks: vec![DiagnosticCheckResult {
                    name: "SPF record present".into(),
                    status: "pass".into(),
                    detail: "v=spf1 …".into(),
                }],
                ran_by_actor_id: ByteBuf::from(vec![7u8; 32]),
            }],
        };
        assert_eq!(
            dreply,
            decode::<ListDeliverabilityDiagnosticRunsReply>(&encode_canonical(&dreply).unwrap())
                .unwrap()
        );
    }

    #[test]
    fn delete_addressbook_round_trips() {
        // Request: book-level (actor + addressbook id, no uid_hash / if_match).
        let req = DeleteAddressbookRequest {
            actor_id: vec![9u8; 32],
            addressbook_id: vec![4u8; 32],
        };
        assert_eq!(
            req,
            decode::<DeleteAddressbookRequest>(&encode_canonical(&req).unwrap()).unwrap()
        );

        // Deleted carries the cascade count (non-optional u32, so 0 round-trips too).
        for n in [0u32, 1, 42] {
            let reply = DeleteAddressbookReply::Deleted { cards_deleted: n };
            let back =
                decode::<DeleteAddressbookReply>(&encode_canonical(&reply).unwrap()).unwrap();
            assert_eq!(reply, back);
            assert!(
                matches!(back, DeleteAddressbookReply::Deleted { cards_deleted } if cards_deleted == n)
            );
        }

        // NotFound is a bare tag.
        let nf = DeleteAddressbookReply::NotFound;
        assert_eq!(
            nf,
            decode::<DeleteAddressbookReply>(&encode_canonical(&nf).unwrap()).unwrap()
        );
    }
}

/// The unknown arm of every app-decoded reply enum
/// (`tools/check-additive-evolution/enum_ledger.txt`, transport.md § Rule 3).
/// Each test models a newer nest as a test-only twin enum carrying one extra
/// variant, encodes with the twin, and decodes with the real type: the value
/// must land in `Unknown` (never an error, never a known variant) and the
/// containing struct must still decode. `Unknown` is never written back.
#[cfg(test)]
mod unknown_arm_tests {
    use super::*;
    use crate::{decode_strict as decode, encode_canonical};

    /// A newer nest's all-unit enum: one value the real enum does not know.
    #[derive(Serialize)]
    #[serde(rename_all = "snake_case")]
    enum NewerUnit {
        FromTheFuture,
    }

    /// A newer nest's internally tagged reply: one outcome the real enum does
    /// not know.
    #[derive(Serialize)]
    #[serde(tag = "outcome", rename_all = "snake_case")]
    enum NewerOutcome {
        FromTheFuture { detail: String },
    }

    fn decode_newer<T: serde::de::DeserializeOwned>(newer: &impl Serialize) -> T {
        decode(&encode_canonical(newer).expect("encode twin")).expect("a newer value must decode")
    }

    #[test]
    fn all_unit_enums_decode_a_newer_value_as_unknown() {
        assert_eq!(
            decode_newer::<ImportAliasStatus>(&NewerUnit::FromTheFuture),
            ImportAliasStatus::Unknown
        );
        assert_eq!(
            decode_newer::<SpamLabel>(&NewerUnit::FromTheFuture),
            SpamLabel::Unknown
        );
        assert_eq!(
            decode_newer::<TrainingSource>(&NewerUnit::FromTheFuture),
            TrainingSource::Unknown
        );
    }

    #[test]
    fn tagged_replies_decode_a_newer_outcome_as_unknown() {
        let newer = NewerOutcome::FromTheFuture { detail: "x".into() };
        assert_eq!(
            decode_newer::<ImportMessageOutcome>(&newer),
            ImportMessageOutcome::Unknown
        );
        assert_eq!(
            decode_newer::<PutEventCiphertextReply>(&newer),
            PutEventCiphertextReply::Unknown
        );
        assert_eq!(
            decode_newer::<ProvisionCalendarReply>(&newer),
            ProvisionCalendarReply::Unknown
        );
        assert_eq!(
            decode_newer::<QueryEventsReply>(&newer),
            QueryEventsReply::Unknown
        );
        assert_eq!(
            decode_newer::<SyncCalendarSinceReply>(&newer),
            SyncCalendarSinceReply::Unknown
        );
        assert_eq!(
            decode_newer::<DeleteEventReply>(&newer),
            DeleteEventReply::Unknown
        );
        assert_eq!(
            decode_newer::<QueryCardsReply>(&newer),
            QueryCardsReply::Unknown
        );
        assert_eq!(
            decode_newer::<SyncAddressbookSinceReply>(&newer),
            SyncAddressbookSinceReply::Unknown
        );
    }

    /// The known values still decode as themselves — the arm is a catch-all for
    /// the unknown only.
    #[test]
    fn known_values_still_decode_as_themselves() {
        let back: SpamLabel = decode(&encode_canonical(&SpamLabel::Ham).unwrap()).unwrap();
        assert_eq!(back, SpamLabel::Ham);
        let back: QueryEventsReply =
            decode(&encode_canonical(&QueryEventsReply::CalendarNotFound).unwrap()).unwrap();
        assert_eq!(back, QueryEventsReply::CalendarNotFound);
    }

    /// `Unknown` is never written back: a path that would re-emit it fails
    /// loudly instead of inventing a value on the wire.
    #[test]
    fn unknown_is_never_serialized() {
        assert!(encode_canonical(&ImportAliasStatus::Unknown).is_err());
        assert!(encode_canonical(&ImportMessageOutcome::Unknown).is_err());
        assert!(encode_canonical(&SpamLabel::Unknown).is_err());
        assert!(encode_canonical(&TrainingSource::Unknown).is_err());
        assert!(encode_canonical(&PutEventCiphertextReply::Unknown).is_err());
        assert!(encode_canonical(&ProvisionCalendarReply::Unknown).is_err());
        assert!(encode_canonical(&QueryEventsReply::Unknown).is_err());
        assert!(encode_canonical(&SyncCalendarSinceReply::Unknown).is_err());
        assert!(encode_canonical(&DeleteEventReply::Unknown).is_err());
        assert!(encode_canonical(&QueryCardsReply::Unknown).is_err());
        assert!(encode_canonical(&SyncAddressbookSinceReply::Unknown).is_err());
    }

    /// The containing struct decodes when one field carries a newer value —
    /// the rest of the reply is not lost.
    #[test]
    fn a_containing_struct_survives_a_newer_field_value() {
        #[derive(Serialize)]
        struct NewerAliasOutcome {
            line_index: u32,
            address: String,
            status: NewerUnit,
            reason: Option<String>,
        }
        let got: ImportAliasOutcome = decode_newer(&NewerAliasOutcome {
            line_index: 3,
            address: "a@example.com".into(),
            status: NewerUnit::FromTheFuture,
            reason: Some("because".into()),
        });
        assert_eq!(got.status, ImportAliasStatus::Unknown);
        assert_eq!(got.line_index, 3);
        assert_eq!(got.reason.as_deref(), Some("because"));

        #[derive(Serialize)]
        struct NewerImportReply {
            outcome: NewerOutcome,
            imported_count: u64,
            skipped_count: u64,
            errored_count: u64,
        }
        let got: ImportMessageReply = decode_newer(&NewerImportReply {
            outcome: NewerOutcome::FromTheFuture { detail: "x".into() },
            imported_count: 1,
            skipped_count: 2,
            errored_count: 3,
        });
        assert_eq!(got.outcome, ImportMessageOutcome::Unknown);
        assert_eq!(
            (got.imported_count, got.skipped_count, got.errored_count),
            (1, 2, 3)
        );

        #[derive(Serialize)]
        struct NewerHistoryRow {
            #[serde(with = "serde_bytes")]
            history_id: Vec<u8>,
            message: String,
            #[serde(with = "serde_bytes")]
            sealed_subject: Vec<u8>,
            mailbox: String,
            label: NewerUnit,
            source: NewerUnit,
            created_at_ms: i64,
            #[serde(with = "serde_bytes")]
            model_delta_applied: Vec<u8>,
        }
        let got: SpamTrainingHistoryRow = decode_newer(&NewerHistoryRow {
            history_id: vec![9; 16],
            message: "Junk".into(),
            sealed_subject: vec![1, 2],
            mailbox: "Junk".into(),
            label: NewerUnit::FromTheFuture,
            source: NewerUnit::FromTheFuture,
            created_at_ms: 77,
            model_delta_applied: vec![3, 4],
        });
        assert_eq!(got.label, SpamLabel::Unknown);
        assert_eq!(got.source, TrainingSource::Unknown);
        assert_eq!(got.created_at_ms, 77);
        assert_eq!(got.model_delta_applied, vec![3, 4]);
    }
}
