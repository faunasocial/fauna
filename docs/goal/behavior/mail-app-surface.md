# First-party app mail surface — target state

Owns: mail-client-rpc
Status: ratified
Authority: the WS-RPC surface a Fauna app uses to send and receive mail without an external MUA — `fauna.email.send` (sender-handle verification and the durable server-side Sent copy), the `fauna.email.inbox.fetch` / `fauna.email.sent.fetch` read feeds and their cursors, the `fauna.mail.received` arrival push, and the `\Seen` read-state kinds (`fauna.email.inbox.mark_seen`, `fauna.email.inbox.flag_changes`, the `fauna.mail.flags_changed` wake); defers the SMTP submission entry point, the in-domain partition at the queue-insert (`submit_outbound`, shared by both paths), and the outbound queue lifecycle to `behavior/smtp-server.md` § Outbound submission flow, the IMAP server contract to `behavior/imap-server.md`, the conversations rendering of these feeds to `ui/conversations.md`, why mail read state is the `\Seen` flag and what the conversations store does with it to `behavior/conversation-read-state.md`, mail sealing at rest to `../architecture/encryption-at-rest.md` § Mail body, list fan-out to `behavior/mail-mass-mailing.md`, the frame-budget bulk-plane reference leg (`body_ref`) to `behavior/mail-message-size.md`, and the `flags` field's spam-scoring role to `behavior/mail-spam.md`. On conflict in element IDs or per-page element scope, `tests/e2e-unified/ui.yaml` wins.

> **Audience:** the nest work serving these kinds and the app work consuming them. The external-MUA equivalents (Thunderbird over 587/993) are `behavior/smtp-server.md` and `behavior/imap-server.md`; this doc is the path a Fauna app itself takes.
> **Split provenance:** carved out of `behavior/smtp-server.md` on 2026-08-03 (that doc was
> 226K, past comfortable cold-reading size). Content moved verbatim; the only edit was
> promoting the section heading one level. Pre-split history lives in `git log` on
> `behavior/smtp-server.md`.

---

## Implementation status today

- **Sender ownership at the app door — built, both arms (2026-09-27).** § Sender-handle verification states the one rule both sender doors enforce (`mail-multidomain.md` § From: header ownership): an in-domain `From:` must be an address the actor owns — handle *or* owned alias — and the one From field must name exactly one mailbox. `email_handlers.rs::send_handler` reads the From through the shared-Rust `fauna_mail::envelope::from_mailboxes` (other than one mailbox → `fauna.email.invalid_params`, beside the From-field count; the old `unknown@unknown` sentinel and its off-domain fall-through are gone), takes the handle match without a lookup, and otherwise asks the in-process alias resolver (`bridge_routing_handlers::resolve_local_recipient_unstamped`, the resolver `fauna.bridges.resolve_recipient` serves the MTA): only a deliverable mailbox owned by the caller admits; the refusal names both things the address could have been. No app composes a `From:` other than `<handle>@<domain>` today (`SmtpBackend::send`), so the alias arm is reachable only by a third-party app until an app grows a send-from selector. Proven by `bins/fauna-nest/tests/conformance_email_send_in_domain.rs::{an_owned_alias_is_admitted_as_the_from_address,another_actors_alias_is_refused_as_the_from_address,a_from_field_naming_other_than_one_mailbox_is_refused}` beside the handle-gate cases listed under § Sender-handle verification.

---

## First-party client send — `fauna.email.send`

**First-party clients** (the Fauna desktop / mobile apps composing
mail directly, not through Thunderbird + SMTP) submit through the
parallel `fauna.email.send` WS-RPC kind. The user client calls
`fauna-client-email::EmailClient::send(recipients, raw_rfc5322)`; the
nest handler does sender-handle verification, then for each **in-domain**
(Fauna→Fauna) recipient resolves the local part through the
`account_aliases` exact-match resolver (`lookup_exact_alias`, the same
exact-match step the MTA's RCPT-time `resolve_recipient` resolver uses — see
`mail-aliases.md`), seals the sender-supplied plaintext to that
recipient's MSEK-derived pubkey, and delivers it through the **same
sealed-ingest path** the Go MTA uses for local recipients (the
`__mail/<actor>` segment store + `bridge_imap_messages` INBOX). So
in-domain mail is visible to `fauna.email.inbox.fetch` and IMAP exactly
like external inbound. nest seals here because `fauna.email.send` carries
plaintext (the same trust model as a Thunderbird→MTA submission), and
for the same reason the handler first strips every reserved `X-Fauna-*`
delivery stamp the client supplied, so the in-domain copies and the Sent copy
below carry only the stamps delivery writes (rule and door list:
`smtp-server.md` § Architectural rules → *The `X-Fauna-*` namespace*);
in-domain delivery does **not** use the deprecated
`deliver_local`/legacy-inbox path. Out-of-domain recipients go onto the
same outbound queue. **The handler then writes a durable server-side Sent
copy**: it seals the sender-supplied plaintext to the *sender's own*
MSEK-derived read key and stores it in the sender's `Sent` mailbox through the
same own-submission ingest path an external-MUA submission uses
(`bridge_routing_handlers::seal_and_store_sent_copy` →
`persist_decoded_inbound_mail(is_own_submission=true)`), so a message composed
in a Fauna app survives a client restart and appears on every device — it
reloads via `fauna.email.sent.fetch` (the client conversation store is
in-memory and re-fetches from `uid 0` each launch; server-side is the right
home because the user is multi-device — product invariant *user always
controls their data*, consistent across devices). This is **best-effort
relative to delivery**: the message has already been delivered/queued, so a
Sent-copy write failure (e.g. a sender with no MLS pubkey on file) is logged,
never a send failure — failing here would risk a misleading client retry and a
remote-MX double-send. The in-session dedup against the client's local echo is
in § Inbound client receive → Sibling Sent feed. The handler returns a uniform
`SendEmailReply`
counter triple (`local_delivered` / `remote_queued` / `remote_errors`); a
per-recipient local failure — a resolved recipient with no encryption key on
file, or a resolver *error* — surfaces in `remote_errors` as `local: …` and is
never a silent drop. An in-domain recipient the resolver **rejects** (unknown
address, disabled, expired, invalid sub-address, over its rate cap) or
**forwards** is not a failure at all: it leaves local delivery for the outbound
queue and is counted in `remote_queued`, where the deployment's own MX
perimeter re-decides with the same resolver and emits the bounce —
`mail-aliases.md` § Per-alias rate-cap → *Second consumer, deliberately left
uniform* owns that seam, and it moves at both doors or neither. `forbid_replay=true` at 30 s — connection-recovery
auto-retry won't replay this kind (double-send risk to remote MX); same
shape as `fauna.bridges.link` (the OAuth-flow precedent for "rare
dangerous ops" in the spec).

**Sender-handle verification, and the client's half of it (2026-07-31; widened to owned addresses 2026-09-27).** The gate is nest-side and authoritative: when the `From:` domain is one the deployment claims, its address must be one the calling actor **owns** — the actor's handle, or an alias the alias resolver attributes to the actor — else `fauna.email.permission_denied`. This is the app door's statement of the one rule both sender doors enforce; the rule, its reasons and the exactly-one-mailbox requirement are owned by [`mail-multidomain.md`](mail-multidomain.md) § From: header ownership (the submission door's arm is `assertSenderOwned`; this door resolves through the same in-process alias resolver the MTA reaches over `fauna.bridges.resolve_recipient`). Two refusal reasons ride that one code — *no handle at all* and *a handle that does not match* — and the at-rest shape makes the first easy to get wrong: a registered actor with no handle stores the **empty string**, so `db::get_handle` answers `Some("")` and only an actor with no `users` row at all answers `None` (owner of the account shape: [`../architecture/nest/public-mode.md`](../architecture/nest/public-mode.md) § *A handle-less account*). An **off-domain** `From:` bypasses the gate entirely — the deployment is not authoritative for it. **Apps carry the matching pre-check**: `SmtpBackend::send` (shared, so all 7 apps inherit it) refuses locally with `error.email.no_handle` when it cannot resolve the user's own `<handle>@<domain>`, because the two ways of papering over an unresolvable sender are both worse than refusing — claiming `<session-handle>@<domain>` asserts an address the account does not own, and an **empty** `From:` is malformed RFC 5322 on the wire that the nest now refuses outright as naming no mailbox (`fauna.email.invalid_params`, the exactly-one-mailbox rule) — until 2026-09-27 it *evaded* this gate, which split the address on `@` and found nothing to split, and that is the shape the pre-check was written against. Coverage: `bins/fauna-nest/tests/conformance_email_send_in_domain.rs::{a_handleless_sender_is_refused_with_the_no_handle_reason,a_handleless_sender_is_not_gated_on_an_off_domain_from,a_secondary_active_domain_is_gated_like_the_primary,a_secondary_active_domain_still_admits_your_own_handle,a_from_field_naming_other_than_one_mailbox_is_refused,an_owned_alias_is_admitted_as_the_from_address,another_actors_alias_is_refused_as_the_from_address}` (the gate, including its **domain set**: "one the deployment claims" is every active `local_domains` row, not the primary alone — a narrowing corrected 2026-08-22) + `libs/fauna-conversations/tests/smtp_backend_tests.rs::{send_without_a_self_address_is_refused_before_reaching_the_sink,send_with_a_domainless_self_address_is_refused_too}` (the client pre-check) + `tests/e2e-unified/tests/test_mail_client_send.py`'s relay tests, which assert the delivered bytes carry the sender's own `From:`.

**Exactly one `From:` field, before the gate reads it.** A message whose header section carries other than exactly one From field is refused `fauna.email.invalid_params` right after the size pre-check: the handle gate reads the **last** From field, while a receiver's DMARC may align against the **first**, so a second field would let a message pass the gate as the sender's own address and leave carrying another. The rule, the count and the other doors: `smtp-server.md` § Architectural rules → *Exactly one From field*.

**List sends never ride raw SMTP** (per `docs/goal/behavior/mail-mass-mailing.md` § How the per-list cap separates): a submission whose `MAIL FROM` matches a `kind='list'` address is **rejected** at `enqueue_outbound_mail` (`fauna.bridges.list_submission_requires_send_rpc`) — per-recipient RFC 8058 stamping is structurally impossible for a single-body SMTP submission. The only list path is the `fauna.bridges.send_list_message` RPC fan-out, where the **nest** stamps each recipient's RFC 8058 + RFC 2369 headers and reserves the per-list / per-account-list / per-deployment-list rate caps **instead of** the per-actor submission caps (§ *Outbound metering* below for the caps that actually exist — the two catalog keys this sentence used to name, `mail.submission.per_actor_msg_per_hour` and `per_actor_rcpt_per_day`, are respectively never-built and a duplicate alias, per `mail-policy-config.md` § catalog); the nest DKIM-signs each message at the outbound hand-out.

(The DKIM private key is the nest's own and never reaches the MTA —
`mail-bridge-lifecycle.md` § DKIM provisioning (automatic).)

### Outbound metering — the ceiling counts the caller, not the header

**The metering key is the authenticated actor, never a value the caller supplies (ratified 2026-08-23).** This is the direct consequence of the § *Sender-handle verification* rule above: an **off-domain** `From:` bypasses the handle gate *by design*, so the caller may put any string there. A cap that counts by that string is defeated by editing a header — a fresh string each message is a fresh counter each message — and the account the cap exists to bound is never named. The rule generalizes past mail: **when a field is deliberately left untrusted because it is not an authorization input, nothing downstream may treat it as an identity** — not a rate-limit key, not a dedup key, not a cache key.

All of it fires **only when there is remote delivery to do**: a purely in-domain send never leaves the deployment, so it consumes no outbound allowance. Three layers, all nest-side:

1. **Per-actor messages/hour ceiling — the one that binds.** 100 messages/hour, counted over the outbound queue rows carrying this actor's submission attribution. A **hard-coded Rust constant, deliberately not a knob** (`../principles.md` § One configuration surface): it is an abuse boundary, not a feature — no user or admin would ever want to choose it. ⚠ Not to be confused with the never-built catalog row `mail.submission.per_actor_msg_per_hour`, which this does **not** resurrect (`mail-policy-config.md` § catalog reads it as "not implemented", and it stays that way — that row is the *submission* path's, and the submission path still has no messages/hour cap: `smtp-server.md` § Architectural rules).
2. **Per-sender-address ceiling — subordinate, and not redundant.** The same 100/hour counted by the `From:` address. Layer 1 bounds one *account*; this bounds one *address across accounts*, and the two come apart exactly where the handle gate does not reach: N distinct actors may each spend a full hourly allowance forging the *same* off-domain `From:`, which layer 1 alone permits. Kept for that, and pinned by a test that drives a different actor every message.
3. **Per-actor recipients/day quota — the same counter raw-SMTP submission consumes.** Charged on the remote recipients of the send, through the identical nest-side counter and admin-override overlay as `fauna.bridges.check_submission_quota` (owner: `smtp-server.md` § Architectural rules, which since 2026-09-25 also owns the one *local*-vs-*remote* line both doors charge by — this door's `remote_addrs` is that line: an in-domain recipient the resolver routes onto the outbound queue counts as remote; catalog key and default: `mail-policy-config.md` § catalog, `mail.submission.max_per_day`). One counter for both doors is the point — a user cannot dual-stream between this RPC and ports 465/587 to escape it. This is what makes `mail-mass-mailing.md` § *How the per-list cap separates from per-actor* true of the RPC half of its sentence, which before 2026-08-23 it was not.

All three refuse with `fauna.email.rate_limited` (`error.email.rate_limited`), the `details` string naming which ceiling was hit.

**At rest.** The submission attribution is `outbound_mail_queue.submit_actor_id`, an **additive nullable** column (`../architecture/version-compatibility.md` — a NOT NULL add would be non-additive) reconciled onto existing databases with no backfill, and its index is created *after* the column reconcile, not in the table's own schema batch (the 2026-08-03 boot-crash class). It is stamped **only** by `fauna.email.send`; every other producer of an outbound row — raw-SMTP submission, forwards, bounces, delay warnings, TLS reports, list fan-out — leaves it NULL and carries its own metering. Rows predating the column are therefore uncounted rather than misattributed: the window is one hour, so they age out of it within an hour of the upgrade, and undercounting an old row costs a briefly generous ceiling where guessing an owner would refuse a user mail they never sent.

## Inbound client receive — `fauna.email.inbox.fetch`

First-party clients **read** their received mail through the
`fauna.email.inbox.fetch` WS-RPC kind — the inbound twin of
`fauna.email.send`. It is **`User`-class and caller-scoped**: the
reading actor is the authenticated caller (no `actor_id` parameter), so
a caller can only ever read their own mailbox. This is the surface the
conversations view uses to render received mail (see
`docs/goal/behavior/conversations-at-rest.md` § Receiving into the conversations view);
it is the per-actor mail realization of the same "read sealed records of
a message-kind since a cursor" pattern that `fauna.conversations.channel
.fetch` realizes for the fauna-native MLS rail.

- **What it returns.** The caller's `INBOX` messages, newest delivery
  last, as the **opaque outer segment record** — the exact
  `fauna_mail::segments::MailRecordEnvelope` canonical bytes the segment
  store holds (`__mail/<actor>`), i.e. what `read_envelopes_bulk`
  returns. The client decodes that outer envelope and opens its inner
  sealed record (`.encrypted_body`) with
  `MlsCapability::open_mail_record(encrypted_body, snapshot)` using its
  MSEK-derived recipient HPKE keypair — the **same decode-then-open**
  two-step the fauna-native rail does on `ConvRecordEnvelope` (this is
  why the receive mechanism is uniform across message-kinds, not a
  mail-special case). Note the inner `.encrypted_body` is the distinct
  same-named `fauna_mls::wrapped_blob::MailRecordEnvelope` (the bridge
  HPKE seal) — `open_mail_record` decodes *that*, not the outer; the
  BridgeMda `fauna.bridges.fetch_message_ciphertext` happens to ship the
  already-unwrapped inner bytes, so it opens directly. The nest holds no
  opening key and never decrypts; the feed reads the
  same sealed records, caller-scoped instead of bridge-passed.
- **Wire shape.** Request `{ after_uid: u32 (0 = from the start),
  limit: u32 (0 = server default; clamped) }`. Reply `{ messages:
  [{ uid, message_id, internal_date, sealed_envelope, flags, body_ref,
  stored_at }], more }`. `flags`, `body_ref`, and `stored_at` are additive
  (an older peer omits/ignores them, unaffected): `flags` is the message's
  IMAP flag/keyword set, read by the on-device spam scorer to skip an
  already-scored message (`behavior/mail-spam.md` § Wire shapes); `body_ref`
  is present only when the stored envelope exceeds the WS-RPC frame budget,
  in which case `sealed_envelope` is empty and the client resolves the
  reference over the bulk-byte plane instead (`behavior/mail-message-size.md`);
  `stored_at` is the record's seal instant, the content-sealing-epochs
  classification basis a client's opener trials off instead of
  `internal_date` (`../architecture/encryption-at-rest.md` § Capability
  tiering) — always present, `0` when the storing nest's append-time clock
  read failed (a standing-sealed record, which the opener's standing arm
  opens).
- **Cursor = `INBOX` UID** (monotonic per mailbox; assigned at
  placement, so UID order ≈ delivery order). The client pages with
  `after_uid = last UID` until `more = false`, then waits for new mail —
  woken promptly by the `fauna.mail.received` arrival push (below) and
  re-polling on a periodic backstop.
- **Which keys the client opens with — the complete standing set, never the
  current generation alone (ratified + built 2026-09-15).** The client's
  receive path holds the same standing recipient keypairs the MDA reads out
  of the snapshot — the current MSEK's plus one per prior grace generation
  in `MailConfig.prior_mseks` — from the one shared derivation both are built
  from (`fauna_mls::wrapped_blob::derive_standing_mail_keypairs`; the set,
  the grace window and its cap are owned by
  `../architecture/owner-key-material.md` § Path B-sibling-2), and trials
  them through the same shared chain (`open_mail_record_standing`, the
  standing arm of `open_mail_epoch_chain`). So after a rotate-mail-keys the
  user's pre-rotation standing mail stays readable in the conversations view
  for as long as the grace window holds it, on every app — a record the MDA
  can open, the app can open too, and vice versa. Before this the client
  opened with the current generation only (a drift from the snapshot set),
  so a rotation blinded it to every standing record sealed before it.
- **Unopenable records — skip, count, keep receiving (ratified + built
  2026-09-15).** A record the client cannot open under that complete key set
  — its seal is addressed to no generation the account still holds (a
  rotation past the grace window; a mailbox turned off email-only and back on,
  which mints a fresh MSEK and clears `prior_mseks` by design —
  `mail-credentials.md` § MSEK lifecycle), or the envelope is malformed or
  tampered — is **deterministic** for that key set: the UID and segment-record
  id the cursor and dedup run on are feed metadata the seal never touches, so
  nothing about the page is in doubt, and a retry with the same keys can only
  meet the same answer. The rule, built once in shared Rust and applied by
  every app: the receive path **skips** the record, advances the cursor past
  it exactly as for an opened one, keeps ingesting everything after it, and
  **records the skip** on the `ConversationsManager`
  (`note_unopenable_mail`; read as `unopenable_mail_count`) so the page tells
  the user that some received mail could not be opened on this device
  (`../ui/conversations.md` § Errors & edge cases). The ledger is per app
  run, like the cursors: a relaunch re-drains every feed from UID 0 with
  whatever keys the account holds by then, and a record that opens after all
  retires its entry (`retire_unopenable_mail`) — the one retry that can
  differ. Only a failure to **fetch** a record's bytes (the `body_ref`
  resolve over the bulk-byte plane, a reply decode fault) stays fatal to the
  page and is retried next tick, because it is transient by construction.
  What this rule preserves is the *contained* condition of
  `../architecture/nest/common.md` § Client-state recoverability → *Per-object
  remedies*: one bad record affects that record, never the mail after it.
  Before this the native page opener failed the whole page on one such record
  (and web dropped it silently with no notice), so one unopenable record
  stopped that mailbox receiving anything after it, on every app, with no
  client action that could clear it.
- **Scope = `INBOX` (inbound).** Received non-spam mail. `Junk` is excluded
  (spam doesn't belong in the conversations view). This feed carries inbound
  only; the actor's own outbound copies come through the sibling Sent feed
  below.
- **Sibling Sent feed — `fauna.email.sent.fetch`.** Identical wire shape
  (`InboxFetchRequest`/`InboxFetchReply`), class (`User`), and caller-scoping
  to `inbox.fetch`, but scoped to the caller's `Sent` mailbox with its own UID
  cursor; both share the `mailbox_fetch_handler` internals. It surfaces the
  caller's own outbound mail in the conversations view as sent bubbles (each
  copy's `From:` is the caller, so it threads by participants), instead of being
  invisible to the INBOX-only inbound feed. **Two paths write the server-side
  Sent copy this feed reads, uniformly:** (1) a message sent through an
  **external SMTP-submission MUA** (Thunderbird / macOS Mail) —
  `submit_inbound_mail`, § Recipient handling on submission — and (2) a
  **first-party `fauna.email.send`** (the Fauna app's own compose), which
  writes the same kind of copy via `seal_and_store_sent_copy` (§ Outbound
  submission flow → First-party clients). Either way the copy is sealed to the
  **sender's own** MSEK-derived read key, so the client opens it with the
  **same** `recipient_secret` it uses for INBOX.
  - **In-session dedup against the local echo.** A first-party `fauna.email.send`
    is *also* echoed locally by the conversations manager on send (an immediate
    sent bubble). That local echo and the server-side Sent copy share the **same
    RFC `Message-ID`** (the one the client minted into the submitted message), so
    `ConversationsManager::ingest_inbound` dedups the Sent-feed copy against the
    echo **by Message-ID** — the in-session view shows exactly one copy. The
    per-folder re-poll `seen` set keys on the server *segment-record id* and so
    can't catch this (the local echo has no segment id); the Message-ID dedup is
    what does. After a client restart the in-memory store is empty, so the Sent
    copy ingests normally and the sent message **reloads** — which is the
    durability the server-side copy exists for. The client runs a second poll
    loop over this feed with an independent cursor + `seen`.
- **Sealed-at-rest invariant.** Mail bodies are sealed under the
  recipient's MSEK-derived read key at the MTA-bridge perimeter, uniformly
  (the storage-mode axis is retired — `../architecture/nest/storage-modes.md`
  owns the retirement; `docs/goal/architecture/encryption-at-rest.md` § Mail
  body); this feed never changes that — it ships the sealed envelope and the
  client decrypts. The same sealed records are the backup-of-record,
  restorable on a new device via `fauna.filesync.snapshot
  .restore_message_kind('mail')`.

### Arrival push — `fauna.mail.received`

The client doesn't poll blindly: the nest emits a `fauna.mail.received`
push to the recipient actor on every newly-placed inbound message,
mirroring `fauna.conversations.channel.message` on the fauna-native rail.
The push is a *wake*, not a payload — the sealed record still comes over
`fauna.email.inbox.fetch`; a connected client reacts to the push by
issuing one fetch, so received mail renders promptly instead of within
one poll cycle.

- **Kind / routing.** `fauna.mail.received`, a push (not a request),
  delivered only to the recipient actor's connected WS sessions via the
  same per-actor `notify_push` path the conversations rail uses; a caller
  never receives another actor's arrivals.
- **Payload — minimal.** `{ actor_id }` (hex). The body is informational:
  every consumer re-reads / re-diffs rather than trusting the push body,
  so no uid / mailbox / message-id is carried in v1.
- **When it fires.** On every genuinely-new placement in the mail-ingest
  path — external bridge inbound, authenticated own-submission, and
  in-domain `fauna.email.send` local delivery all share that path — for
  any destination mailbox (INBOX / Junk / Sent / filter-target). It does
  **not** fire on an idempotent re-ingest of an already-placed message.
- **Best-effort, poll-backstopped.** Dropped if the recipient has no live
  WS connection at delivery; correctness never depends on it. The client
  keeps a periodic `inbox.fetch` poll as the reconnect / missed-push
  backstop, so a dropped push costs at most one poll cycle of latency.
- **Scope interaction.** `inbox.fetch` is INBOX-only and `sent.fetch` is
  Sent-only; a push fired for a `Junk`-routed arrival self-filters to a cheap
  no-op on the client, while a `Sent`-routed arrival (an own-submission — an
  external MUA *or* a first-party `fauna.email.send`) wakes the client's
  `sent.fetch` poll so the sent message surfaces.
- **Former second consumer.** The `bins/fauna-sync` backup coordinator
  subscribed to the same kind to wake its segment-backup debounce, so
  backups became prompt (not only on the 15-min periodic timer / segment
  rotation); it was removed with the daemon on 2026-10-02, and segment
  backup is nest-driven (`backup-restore.md` § Background Tasks). It backed
  up all mailboxes, which is why the push fires on every placement, not
  only INBOX.

## Read state — `\Seen` from a first-party app

A Fauna app's read state for mail **is** the message's IMAP `\Seen` flag — the decision, and what the conversations store does with it, are owned by [`conversation-read-state.md`](conversation-read-state.md) § Mail: `\Seen` is the marker. This section owns the wire an app reads and writes the flag over. Reading at launch needs nothing new: `inbox.fetch` already returns each message's `flags`. Three additions, all additive, all `User`-class and caller-scoped like the feeds (no `actor_id` parameter — a caller only ever touches their own `INBOX`):

- **`fauna.email.inbox.mark_seen` — the flag write.** Request `{ uids: [u32] }`; reply `{ updated: u32 }` (how many rows gained the flag). It adds `\Seen` to each named `INBOX` UID that lacks it and does nothing else: no other flag, no removal, no other mailbox, an unknown or expunged UID is skipped without error, and a repeat is a no-op. That narrowness is the point — `fauna.bridges.store_flags` stays `BridgeMda`-only, and this is the second least-privilege `User`-class flag write beside `fauna.email.apply_spam_disposition`, built the same way: a named effect, never a general flag door. It goes through the same flag-write internals as `store_flags(op = add)`, so each changed row bumps its modseq and the mailbox's, appends the `StoreFlags` placement-journal record, and fans out the mailbox-state event an `IDLE`-ing mail client hears ([`imap-server.md`](imap-server.md)). Removal is deliberately absent: no app gesture un-reads a mail today, and a mail client that does it writes the flag over IMAP.
- **`fauna.email.inbox.flag_changes` — the delta.** Request `{ since_modseq: u64, limit: u32 (0 = server default; clamped), after_uid: u32 (additive, default 0) }`; reply `{ changes: [{ uid, flags, modseq }], highest_modseq, more }` — the `INBOX` rows whose modseq is above the cursor, ordered by `(modseq, uid)` so the oldest change comes first, each with its **whole current flag set** (state, not an edit script, so redelivery is harmless). The cursor is the pair `(since_modseq, after_uid)`: one flag write stamps every row it touches with a single shared modseq, so a page can end inside one write — while `more` is true the client resumes from the last change's `(modseq, uid)`, and once it is false from the reply's `(highest_modseq, 0)`; `after_uid = 0` means every row above `since_modseq`. The reply's `highest_modseq` is read under the same lock as the rows, so advancing to it misses nothing. Expunged rows are not reported; a message that left `INBOX` simply stops appearing in the feed at the next drain. The client's baseline is the additive **`highest_modseq`** field `inbox.fetch`'s reply gains — the mailbox's value when the page was built; the client keeps the one from the **first** page of its launch drain, so a change racing the drain is delivered again rather than missed.
- **`fauna.mail.flags_changed` — the wake.** A push to the actor's connected sessions whenever a flag write changes at least one `INBOX` row of theirs, whoever made it — a mail client through the MDA, or another Fauna device through `mark_seen`. Payload `{ actor_id }`, a wake and not a payload, best-effort and poll-backstopped exactly like `fauna.mail.received`: the client answers it with one `flag_changes` call and also makes that call on its periodic backstop.

**Compatibility.** Every nest serves `mark_seen` and `flag_changes`, so a refusal of either is an ordinary, retried failure; the client's silent "unknown kind ⇒ sync nothing" arm for a nest predating them left with the 2026-09-24 compat-remnant sweep (`../architecture/version-compatibility.md` § Dimension 2). A source that does not serve the kinds (anything but `INBOX`) still holds reads in memory for the run and syncs nothing.

**Implementation status today — built, both halves (app half 2026-09-24).** The nest serves all three additions and the `highest_modseq` baseline on `inbox.fetch`: `mark_seen` and the MDA's `store_flags` share one post-write path (journal, IDLE fan-out, and the `fauna.mail.flags_changed` wake on an `INBOX` change — `bins/fauna-nest/src/bridge_imap_handlers.rs::record_flag_writes`); the typed calls are `EmailClient::{inbox_mark_seen, inbox_flag_changes}` in `libs/fauna-client-email`. Every app calls them through one mapping (`fauna_client_conversations::{inbox_mark_seen_call, inbox_flag_changes_call}` — every nest refusal is a retried failure since the 2026-09-24 sweep, § Compatibility below; only a source that does not serve the kinds, anything but `INBOX`, stops the run) and hears the wake; what the app does with them is [`conversation-read-state.md`](conversation-read-state.md) § Implementation status today.
