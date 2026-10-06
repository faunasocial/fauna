# Mail message size — target state

Owns: mail-message-size
Status: ratified
Authority: the mail message-size story — the product ceiling (`max_message_bytes`), the inline ceiling (`MAX_INLINE_RAW_MESSAGE_BYTES`), the bulk-plane reference legs, continuation records at rest, and the `552 5.3.4` perimeter enforcement points with their reply codes; defers the transport-side max-frame rationale to `../architecture/transport.md` § Max frame, the part/head record mechanism to `../architecture/message-segment-store.md` § Continuation records, the bulk-byte plane carve-out to `behavior/webdav-server.md` § Bulk-byte plane, and the SMTP perimeter it is enforced at to `behavior/smtp-server.md`. On conflict in element IDs or per-page element scope, `tests/e2e-unified/ui.yaml` wins.

> **Audience:** the nest + mail-bridge work wiring any leg that carries a mail body — MTA ingest, MDA APPEND, client import, submission enqueue, and every fetch that reads one back.
> **Split provenance:** carved out of `behavior/smtp-server.md` on 2026-08-03 (that doc was
> 226K, past comfortable cold-reading size). Content moved verbatim; the only edit was
> promoting the section heading one level. Pre-split history lives in `git log` on
> `behavior/smtp-server.md`.

---

## Message size limits (ratified 2026-07-12 — inline ceiling + bulk-plane references)

Owner of the mail message-size story. The transport-side rationale — why the 2 MiB
`MAX_RPC_WS_MESSAGE_SIZE` is permanent for every caller class and why oversized payloads
leave the RPC plane — is owned by `../architecture/transport.md` § Max frame; this section
owns mail's constants, enforcement points, and reply codes.

- **The product ceiling is `max_message_bytes`** (nest config, shipped default 50 MB — the
  table row above): the admin-tunable maximum a message may be, end to end.
  **Its upper bound is `MAX_MESSAGE_BYTES_CEILING` = 250,000,000 (250 MB, decimal like the
  knob; ruled 2026-08-26).** The nest refuses a
  write above it with `fauna.protocol.malformed` — surfaced by the shared admin machine in all 7
  apps, the precedent being the Bayesian-ramp refusal (`libs/fauna-client-mail-settings/src/admin_policy.rs`)
  — so no stored knob exceeds it and no read path clamps. The bound exists because the ClamAV
  sidecar's scan limits are set from it ([`mail-content-scanning.md`](mail-content-scanning.md)
  § Oversize messages: the gate scans everything the perimeter accepts, so a ceiling the scanner
  cannot cover is not a ceiling the product supports). 250 MB clears every mainstream provider's
  attachment limit with headroom while keeping one message's scan cost bounded; larger transfers
  belong to the file plane, not SMTP. *Built 2026-08-26:* the constant is
  `fauna_mail::transport_limits::MAX_MESSAGE_BYTES_CEILING`; the nest's `put_spam_policy`
  refuses a larger write (the read-side clamp for a value stored before that refusal existed
  was removed in the 2026-09-27 compat-remnant sweep — none such exists). The bound's *reason* — that the scan sidecar's limits
  are shipped for it — is only half-built: the gate now derives its cap from this knob, but
  the compose bundle still ships clamd with stock limits
  ([`mail-content-scanning.md`](mail-content-scanning.md) § Implementation status today).
- **The inline ceiling is `fauna_mail::transport_limits::MAX_INLINE_RAW_MESSAGE_BYTES`
  (1,500,000 raw RFC 5322 bytes)** — the largest raw message whose per-recipient sealed
  copy still fits an `ingest_inbound_mail` / `append` / `import_message` /
  `enqueue_outbound_mail` request inside one 2 MiB WS-RPC frame, with headroom for the
  seal overhead, the encrypted index hint, and the frame envelope. Single-sourced in
  shared Rust and uniffi-exported, so the Go bridge and every app read the same value.
- **Target state: a sealed body above the inline ceiling rides the bulk-byte plane as a
  reference, in both directions** (`../behavior/webdav-server.md` § Bulk-byte plane is the
  existing carve-out). Upward (MTA ingest, MDA APPEND, client import, submission enqueue):
  the producer uploads the sealed bytes to a staging endpoint on the byte plane (client
  bearer, or the bridge's short-TTL bulk token), then the RPC carries a `body_ref` instead
  of inline bytes; nest binds the staged bytes into the mail record atomically with the
  RPC, and unconsumed staging is TTL-GC'd. Downward (MDA FETCH, client inbox/sent fetch,
  MTA outbound-due): the reply carries a `body_ref` the consumer GETs over the byte plane.
  At-rest shape, seal derivation, and quota charging are unchanged — only the transport of
  the sealed bytes moves. Wire evolution is additive (new optional fields beside the
  existing inline ones); a body at or below the inline ceiling always rides inline, so
  pre-reference senders and readers are unaffected on every message they could previously
  carry. **Only the MTA-ingest and MDA-APPEND legs stage already-*sealed* bytes; client
  import and the outbound queue pair stage plaintext-derived bytes and therefore ride the
  staged-envelope rule below (ratified 2026-07-18) — plaintext must never enter the
  open-download chunk store.**
- **Nest bounds every upward reference before it reads a chunk (ratified 2026-09-28).** A reference's chunk list is request-supplied and not self-verifying — it can name one staged hash tens of thousands of times — so both resolvers (`mail_body_plane::resolve_body_ref` for the sealed legs, `resolve_staged_body` for the staged-envelope legs) refuse, as the caller's-fault `invalid_body_ref`, before the first store read: a declared `total_bytes` over the carrying leg's sealed ceiling, or a list naming more chunks than that total splits into (`fauna_mail::body_ref::check_mail_body_ref_shape`); and they refuse mid-read the chunk that carries the join past the declared total (`MailBodyJoin`). The leg ceilings are: client import, the admin knob's effective ceiling plus the staged-envelope overhead (refused first as the typed `message_too_large`); MDA APPEND, the same sealed ceiling its `ciphertext_size` check uses; MTA ingest, `MAX_MESSAGE_BYTES_CEILING` plus the seal allowance (knob-independent, so a knob change can never refuse mail the perimeter already accepted); the outbound enqueue, its own byte ceiling plus the envelope overhead. So a resolver holds at most the declared total plus one chunk.
- **Interim (until the reference legs land): the perimeter enforces
  min(`max_message_bytes`, inline ceiling), answering `552 5.3.4` — permanent — above
  it.** Two layers, both on inbound `Data` and submission `Data`: (1) the pre-parse
  `io.LimitReader` clamp (the existing `552 5.3.4` path, now clamped to the inline ceiling
  as well as the admin knob); (2) a precise post-seal, pre-RPC guard on the assembled
  request size — the encrypted index hint is input-dependent, so a sub-ceiling message can
  still assemble over-frame; the guard answers the same `552 5.3.4`, never the generic
  transient `451` (the pre-2026-07-12 bug: a permanently-oversized message was deferred as
  if the nest were down, and the sender retried for days). First-party legs
  (`fauna.email.send`, `import_message`) refuse an over-ceiling body client-side with a
  typed error rather than letting the transport sever the connection.
- **⚠ The binding ceiling is AT REST, not on the wire (established 2026-07-12;
  SUPERSEDED 2026-07-18 by continuation records + ceiling retirement — this bullet is
  the historical diagnosis; the § *Ceiling retirement — LANDED* bullet holds the
  terminal state where `max_message_bytes` alone binds).**
  Crossing the frame is now solved; *resting* is not, and that — not the transport — is
  what keeps `max_message_bytes` = 50 MB from being deliverable. A sealed body must fit
  **one CARv2 segment record, which is refused on READ above
  `fauna_carv2::v1::MAX_RECORD_LEN` (16 MiB)** — a garbage-parse guard whose own comment
  assumed "well above any expected fauna payload", a premise this feature invalidates.
  Compounding it, **`fauna_mail::segments::MailRecordEnvelope`'s payload fields are
  `Vec<u8>` without `serde_bytes`**, so canonical dag-cbor writes them as an *array of
  integers* rather than a byte string: bytes ≥ 24 cost two bytes each, so ciphertext
  expands **~1.91×** at rest (measured — a 10 MiB body produced a 19,987,416-byte
  record). *Every* stored mail record pays this today, not just large ones. Together
  they cap a **readable** sealed body at ~8.8 MB —
  `fauna_mail::transport_limits::MAX_SEALED_BODY_AT_REST_BYTES`.
  **The write path had no matching guard**, and the rule that must hold is that a record
  which cannot be read back is never stored in the first place. The gap was unreachable
  while the perimeter clamped at ~1.5 MB and becomes reachable with the reference legs,
  so the guard lands with them — `segments::mail::append_record` refuses such a record
  structurally, at the single choke point *every* ingest leg passes through (inbound,
  IMAP APPEND, import; the APPEND leg has no perimeter ceiling of its own). Pinned by
  `mail_body_ref_round_trip.rs::a_body_too_large_to_rest_is_refused_rather_than_stored_unreadable`.
  Raising the ceiling to `max_message_bytes` is an **at-rest** slice, not more transport:
  give the envelope `serde_bytes` (recovering ~48% of all mail storage) and/or keep an
  over-ceiling body's bytes on the byte plane by reference so the record itself stays
  small. Both are at-rest format changes on live alpha data ⇒ expand→migrate→contract,
  never a flip (`../architecture/version-compatibility.md`). **Compressing the stored
  body is not a lever:** it is HPKE-sealed ciphertext, sealed at the MTA before nest ever
  sees it, hence incompressible — real compression would have to happen *pre-seal* and is
  a sealed-format decision owned by `encryption-at-rest.md`.
- **Implementation status today (2026-07-12):** the **reference legs are BUILT nest-side
  and in shared Rust**; the **Go bridge legs are not**, so the live perimeter is still
  the interim `552 5.3.4` clamp.
  - **Built.** `fauna_mail::body_ref` — the one chunk-split/join + store-key rule all
    three legs share (uniffi-exported, so the Go MTA that stages, the nest that rejoins,
    and the Go MDA that re-fetches cannot disagree about a chunk boundary). Additive
    `MailBodyRef` on `IngestInboundMailRequest` and `FetchMessageCiphertextReply::Found`
    — a body at or below the inline budget still encodes **byte-identically** to the
    pre-field shape, and an old nest tolerates a new MTA's key. A body reference is the
    ordered chunk-hash list, deliberately **not** a `ChunkManifest`, keeping this path
    off that type's fail-closed decode. `fauna.bridges.mint_bulk_byte_token` gains an
    additive `purpose`: `Folder` → **BridgeMda only**, served-set gate, unchanged;
    `MailBody` → **BridgeMta or BridgeMda**, gated on the target being a mail recipient
    here (mail belongs to no folder, which is why widening the folder gate would have
    been meaningless). **An MTA still may not mint a folder token** — admitting it to
    the byte plane for mail did not open the WebDAV path to it. Nest's `mail_body_plane`
    resolves a reference upward and stages one downward.
    **Staging needs no lifecycle of its own:** a staged chunk is an orphan blob, and the
    existing blob GC already reaps orphans past its grace window while skipping fresh
    ones — which *is* "unconsumed staging is TTL-GC'd", for free, with no staging table
    and nothing to leak.
    Proven end to end against the real surfaces (real HTTP byte routes, real WS-RPC
    handlers, real segment store) by `bins/fauna-nest/tests/mail_body_ref_round_trip.rs`:
    a 6 MB message seals → mints a `MailBody` token → stages over `POST /api/v1/chunks`
    → ingests carrying only a `body_ref` → rests sealed → fetches back a `body_ref` →
    GETs the chunks over the **open** download route (so the downward leg needs no token
    at all) → rejoins → unseals byte-for-byte, with a wrong-key negative control.
  - **Not built.** The `append` / `import_message` / outbound-queue legs have no
    reference path — and the **outbound pair is a genuinely different shape**: its
    `raw_message` is *plaintext in a SQL BLOB* (nest must read it for SRS rewrite,
    DKIM signing, and bounce/DSN extraction) and both its structs set
    `deny_unknown_fields`, so an additive field there **hard-rejects** on an old peer.
    The IMAP `APPEND` leg has **no perimeter ceiling of its own** (only the at-rest
    write guard catches an oversized body there). First-party `fauna.email.send` /
    `import_message` over-ceiling typed client-side errors are not built.
- **Implementation status today (2026-07-12, later — the Go bridge legs LANDED):** the
  reference path is **live end to end**; `max_message_bytes` is no longer clamped to the
  inline ceiling. Both mail-ingesting legs stage, the MDA reads a reference back, and
  EHLO tells the truth.
  - **Built (Go MTA — the producer).** `stageSealedBody`
    (`bins/fauna-bridges/internal/mta/body_ref.go`) is the one staging seam **both**
    ingesting legs call: the inbound MX path (`inboundSession.ingestForRecipient`) and
    the authenticated submission / Sent-copy path
    (`submissionSession.deliverToFaunaActor`). **Since lifted out into the shared
    `internal/mailstage.StageSealedBody` (§ IMAP APPEND leg below) so the MDA's
    APPEND leg shares the identical switchover predicate and chunk boundaries —
    `body_ref.go` in `internal/mta` now keeps only the plaintext-derived outbound
    staging (§ *The plaintext legs* below).** Over the inline budget ⇒ mint a
    `MailBody` bulk-byte token → split via the shared-Rust `fauna_mail::body_ref` → upload
    each chunk to `POST /api/v1/chunks` → send a `body_ref`. At or under it, the body rides
    inline and the request encodes byte-identically to its pre-reference shape.
    **The submission leg is not optional**: the perimeter now admits messages far larger
    than the frame, so a submission path without a reference leg would seal a
    multi-megabyte message and have the transport sever the connection under it — the
    permanent-failure-as-transient bug this design exists to kill. `CiphertextSize`
    (the `RFC822.SIZE` floor) is captured pre-staging, so it stays the true sealed size
    when the body leaves the request.
  - **Built (Go MDA — the consumer).** `SealedBodyOf`
    (`internal/mda/imap/body_ref.go`) resolves a reply's body whichever way it arrived,
    and every consumer of a fetch reply goes through it: IMAP `FETCH`, both `\Junk`
    train-signal paths, background spam re-scoring, and the re-score drain. The chunk
    download route is **open** — confidentiality is cryptographic, not transport-scoped —
    so the read leg needs no token at all. It **fails closed**: the chunk contents are
    self-verifying (content-addressed) but the chunk *list* is not, so the rejoin is
    pinned against the reference's declared total, exactly as nest pins it on the way up.
  - **Built (the perimeter + EHLO).** `clampToInlineCeiling` is **deleted**. The `Data`
    paths and the EHLO `SIZE` advertisement now both read
    `fauna_mail::transport_limits::effective_max_raw_message_bytes` — one shared-Rust
    rule, so enforcement can never drift from the at-rest limit backing it. EHLO
    previously advertised go-smtp's 1 GiB backstop, ~120× the truth. It advertises the
    **boot** snapshot's value deliberately: go-smtp reads `srv.MaxMessageBytes` live
    per-EHLO off a non-atomic field it owns, so hot-applying a later admin change would
    be a data race; both staleness directions are safe (a stale-high `SIZE` costs one
    wasted transfer then the permanent `552` the live guard raises; a stale-low one makes
    the sender self-limit), so we advertise boot rather than race it.
  - **The size failures are one permanent class.** `mailstage.ErrMessageTooLarge` →
    `552 5.3.4` on **both** the inbound and submission ladders (the submission ladder previously had
    no size branch at all and would have deferred `451`). At the time it had two causes: a
    sealed body over `MAX_SEALED_BODY_AT_REST_BYTES`, and the pathological case where the
    *index hint alone* overflows the frame (only the body can leave it). **Since ceiling
    retirement (2026-07-18) only the index-hint cause remains** — the at-rest ceiling is
    gone, so an over-budget **body** is staged (and, for the SMTP legs, the raw product
    ceiling refuses over-`max_message_bytes` mail before sealing). An over-budget body is
    never refused for its size.
  - **The perimeter is honest, and (as of 2026-07-12) `max_message_bytes` = 50 MB is
    still NOT deliverable** — *superseded 2026-07-18 by ceiling retirement, which made it
    deliverable; see the § Ceiling retirement bullet.* At the time: the effective ceiling
    was `min(max_message_bytes, MAX_RAW_MESSAGE_BYTES_AT_REST)` — about 8.06 MB raw. That
    gap was the at-rest slice's to close, not the transport's, and it was stated here
    rather than papered over. A `0` knob did not mean uncapped: the at-rest ceiling was
    physics, not policy (retirement replaced that fallback with the product default).
  - **Proven.** tier_3, real SMTP + real IMAP, both binaries: a 6 MB inbound message
    stages → ingests by reference → rests sealed → is served back over `FETCH BODY[]`
    byte-for-byte
    (`tests/e2e-unified/tests/test_mail_inbound_to_imap.py::test_an_over_frame_inbound_message_delivers_by_reference_and_fetches_byte_for_byte`).
    The two former refuse-tests now assert *delivery*:
    `test_mail_bridge_mta.py::test_inbound_message_over_the_raw_inline_ceiling_now_delivers`
    (which pins the perimeter *clamp* — at ~2 MB the sealed body still rides inline) and
    `::test_inbound_message_over_the_ws_rpc_cap_is_never_accepted_then_lost` (3 MiB, whose
    sealed body genuinely exceeds the budget, so it is the in-file proof of the reference
    path — and whose "a 250 MUST mean stored" property is finally reachable). A new test
    pinned the at-rest ceiling that then bound; **ceiling retirement (below) renamed it
    `::test_inbound_message_formerly_over_the_at_rest_ceiling_now_delivers` and flipped its
    assertion from refusal to delivery** — it is now the direct change-detector for ceiling
    retirement. The seal-overhead allowance the raw perimeter is derived from is
    itself pinned by measuring a real seal
    (`mail_body_ref_round_trip.rs::the_seal_allowance_covers_what_the_seal_actually_adds`).
- **Implementation status (2026-07-12, later still — the envelope-v2 EXPAND step is
  LANDED; the write flip is NOT).** The at-rest slice's first lever (the `serde_bytes`
  envelope) is staged as expand→contract on a surface that is **wire as well as at-rest**:
  the outer `MailRecordEnvelope` bytes ship verbatim to every app
  (`fauna.email.inbox.fetch` / `sent.fetch` → the shared
  `fauna_mail::segments::receive::open_inbound_record`) and cross nest↔nest on the mail
  relay — so every reader everywhere must carry the new decoder before any writer flips.
  - **Landed (expand).** `MailRecordEnvelope` decode is **version-dispatched dual-shape**
    (`libs/fauna-mail/src/segments/envelope.rs`): v1 (bare-`Vec<u8>` array-of-ints
    payloads, the ~1.91× shape — **frozen**, decodes forever within this major) and v2
    (`serde_bytes` byte-string payloads — payload + small constant). The two shapes are
    mutually indecodable at the CBOR level (pinned by test), a future version fails
    loudly, and the v1 encoding is pinned by golden bytes. Because the decoder rides the
    shared crate, nest, peer nests, and all 7 apps gain it from one change.
  - **LANDED 2026-07-18: the write flip.** `MAIL_ENVELOPE_WRITE_FORMAT` now selects v2,
    flipped in one commit together with `MAIL_CONTINUATION_WRITE_DEFAULT` and
    `MAIL_CLIENT_FEED_REFERENCE_DEFAULT` — the three shared one deployed-reader gate.
    Gate evidence: example.com ran that build (both reader capabilities content-verified),
    and the user confirmed every installed client build and every other box in the
    rollback window is past 2026-07-17. New writes are v2; a v2 record at a v1-only
    reader would be an honest per-record decode error, which is what the gate protected.
    - **The at-rest ceiling constants were NOT re-derived, and the earlier plan to
      re-derive them to ~16 MiB is superseded.** Continuation records (below) landed in
      the same flip, and they dissolve the single-record ceiling entirely rather than
      raising it: `append_record` splits any sealed body over `MAIL_BODY_PART_CAP_BYTES`
      into frame-sized parts, so no body is "too large to rest" at any size. A re-derived
      16 MiB constant would have described a code path that no longer executes for large
      bodies. `MAX_SEALED_BODY_AT_REST_BYTES` / `MAX_RAW_MESSAGE_BYTES_AT_REST` therefore
      survived the write flip **unchanged, as perimeter policy rather than at-rest
      physics** — and were then **deleted** by ceiling retirement later the same day (the
      § *Ceiling retirement — LANDED* bullet below), so `max_message_bytes` alone now holds
      the SMTP ceiling.
    - **Side effect worth knowing — RETIRED.** `content_seal_backfill` used to decode a
      stored record and write a *fresh* envelope via `MailRecordEnvelope::new`, so any
      record it reseals was upgraded v1 → v2 opportunistically (safe, since every deployed
      reader handles v2). **That module is deleted** as of the 2026-08-17 record-identity
      cutover: the cutover's boot reconcile tombstones every pre-cutover record outright,
      so there is no residual raw corpus left for a reseal pass to walk
      (`../architecture/message-segment-store.md` § Record identity per kind). The stored
      population no longer drifts toward v2 through this path.
  - **v1 records were never re-encoded, and the v1 decoder is now gone.** The flip
    changed new writes only (user decision 2026-07-12: no effort on migrating
    already-stored content), and a frozen v1 decoder kept reading records stored before
    it — until the compat-remnant sweep's baseline reset (no pre-sweep data exists
    anywhere, `../architecture/version-compatibility.md` § Dimension 2) removed it,
    2026-09-30. A v1 record is now an honest per-record decode error, like any
    unsupported envelope version (`libs/fauna-mail/src/segments/envelope.rs`).
- **The at-rest design is RATIFIED (2026-07-12), BUILT (2026-07-13) and LIVE since the
  2026-07-18 write flip: an over-cap sealed body rests as continuation records** — N part
  records + one head record inside the same `__mail` segments; mechanism, invariants, the
  rejected byte-plane-resident alternative, **and the full build status** are owned by
  `../architecture/message-segment-store.md` § Continuation records → Implementation
  status (writer `append_continuation_record`, reaper `reap_headless_parts`, serve-join
  `read_sealed_body_with_floor`, relay part-skip — all landed; continuation *writes* now
  unconditional since the compat-remnant sweep removed the `MAIL_CONTINUATION_WRITE_DEFAULT`
  gate, 2026-09-30). What this section owns — mail's
  consequences and sequencing:
  - The mail head is **envelope format v3** (the § dispatcher above gains a third arm;
    v3 writes gate on deployed readers exactly as the v2 flip does — one reader gate can
    clear both, and the gate constant flips together with `MAIL_ENVELOPE_WRITE_FORMAT`).
    The part cap is the shared-Rust constant
    `fauna_mail::transport_limits::MAIL_BODY_PART_CAP_BYTES` (1 MiB), sized so a part's
    full relay wire tuple fits the 2 MiB federation frame with headroom.
  - **Ceiling retirement — LANDED 2026-07-18 (its own slice, after the write flip).**
    `MAX_SEALED_BODY_AT_REST_BYTES` / `MAX_RAW_MESSAGE_BYTES_AT_REST` are **deleted** — the
    effective ceiling is `max_message_bytes` *alone* (the SMTP `Data` clamp, EHLO `SIZE`,
    nest import/APPEND admission, and `fauna.email.send` all follow from the one shared-Rust
    `effective_max_raw_message_bytes` rule), the `552` at-rest refusal is gone (any admitted
    size now rests as continuation records), and the S8 size probes retarget accordingly.
    - **The per-record write guard** in `segments::mail::append_record` is now only an
      internal invariant (no single record over the CARv2 cap), never a message ceiling —
      anything over `MAIL_BODY_PART_CAP_BYTES` splits before reaching it.
    - **APPEND became a nest-enforced ceiling.** IMAP APPEND has no SMTP perimeter clamp of
      its own; before retirement its de-facto ceiling was the (shared, Go-side) at-rest
      refusal inside `StageSealedBody`, which was ~8 MB and never actually checked
      `max_message_bytes`. With that refusal removed, nest's `append_message_handler` now
      authoritatively refuses a declared `ciphertext_size` over `max_message_bytes` (+
      `SEAL_ENVELOPE_ALLOWANCE_BYTES`, since it sees only the sealed body) with the shared
      typed `message_too_large`, which the MDA maps to an IMAP `BAD`. Mirrors `import_one`'s
      nest-side admission; the bridge is untrusted. (`imap-server.md` § Write surface defers
      the APPEND size story here.)
    - **The `0`-knob (no snapshot) fallback** is the shipped product default
      (`DEFAULT_MAX_MESSAGE_BYTES`, 50 MB), not the deleted at-rest ceiling — never
      uncapped, because go-smtp treats a `0` `MaxMessageBytes` as unlimited.
    - **Wire/binding.** Both deleted constants were uniffi-exported and read by the Go
      bridge (`EffectiveMaxRawMessageBytes` sets go-smtp's `MaxMessageBytes` and the EHLO
      `SIZE` advertisement at `internal/mta/server.go` + `internal/mta/submission.go`), so
      the retirement regenerated the **tracked** `libs/fauna-mail-go` binding.
  - Ingest keeps the S1–S4 staging flow unchanged: staged chunks resolve to one verified
    `SealedRecordBytes`, and the continuation writer splits *after* verification, at the
    same `append_record` choke point every leg passes through. Downward legs
    (`FetchMessageCiphertextReply`, the client-feed reference leg) are unaffected in
    shape — the nest concatenates parts before the existing inline-or-`body_ref` split.
- **The client-feed reference leg (first-party inbox/sent) is BUILT nest-side, gated
  OFF (2026-07-17).** `mailbox_fetch_handler`
  (`bins/fauna-nest/src/email_handlers.rs`) packs each page's messages inside one 2 MiB
  WS-RPC frame; the perimeter admits raw messages to ~8 MB (a ~4 MB sealed body at v1's
  1.91× rest shape already overflows the frame), so a message whose stored **outer**
  envelope exceeds the frame cannot ride inline. Its downward leg is the § bullet above
  ("client inbox/sent fetch: the reply carries a `body_ref` the consumer GETs over the
  byte plane"): the feed stages the whole outer envelope on the byte plane and serves an
  additive `InboxMessage.body_ref` (empty inline envelope); the client GETs the chunks
  over the **open** download route, rejoins them to the exact stored envelope, and opens
  it as if it had arrived inline. At-rest shape and seal are untouched — only the feed's
  transport moves, exactly as on the MDA path.
  - **Built (nest serve side).** Additive `InboxMessage.body_ref: Option<MailBodyRef>`
    (`serde(default, skip_serializing_if)`, wire-invisible when absent, so an inline
    message encodes byte-identically to the pre-reference shape) + the stage-and-reference
    path in `mailbox_fetch_handler` (once gated by
    `segments::mail::client_feed_reference_enabled`; the gate left with the
    2026-09-24 compat-remnant sweep, so the reference serve is unconditional).
    Proven tier_3 (real HTTP byte routes, real feed handler) by `bins/fauna-nest/tests/
    mail_body_ref_round_trip.rs::the_mailbox_feed_serves_an_over_frame_envelope_by_reference`.
  - **ON since 2026-07-18, and why it was gated.** Serving an empty-envelope `body_ref`
    to a client that lacks the feed-side resolver **wedged its inbox drain** — until
    2026-09-15 the native open path (`NestMailInboundSource::fetch`) treated any record
    it could not open as *fatal to the page* (today an unopenable record is skipped and
    counted, and only a failed *resolve* of the reference stays fatal-and-retried —
    `mail-app-surface.md` § Inbound client receive → *Unopenable records*; a
    resolver-less reader would now skip such a record, a loss rather than a wedge, so
    the gate's reason stands). So the reference serve is a distinct **reader capability** the client
    must carry first, exactly like the v2/v3 envelope decoders: one deployed-reader
    confirmation cleared all three, and this gate flipped together with
    `MAIL_ENVELOPE_WRITE_FORMAT` + `MAIL_CONTINUATION_WRITE_DEFAULT`
    (`../architecture/message-segment-store.md` § Continuation records co-locates the
    flag).
  - **Former interim behavior, now retired.** While gated off an over-frame envelope was
    SKIPPED (still IMAP-readable; the feed never stalled on it). Post-flip it is served
    by reference instead. A page still closes early with `more = true` once its messages
    fill the frame — that budget rule is independent of the gate.
  - **Built — the client resolver (the reader half, which ships FIRST;
    2026-07-17).** The rule is one shared function,
    `fauna_mail::body_ref::resolve_referenced_mail_body` (feature
    `body-ref-resolve`): width-check each 32-byte hash → `ContentHash` → GET the
    chunks over the open route through the cross-target
    `fauna_core::file_download::BlobFetcher` seam → rejoin fail-closed on the
    declared total (`join_sealed_mail_body_checked`). Both receive paths call it
    before `fauna_mail::open_inbound_record*` and then open the result exactly as
    an inline `sealed_envelope`, so no target can drift on the derivation or the
    fail-closed rule. Only the per-target *fetch binding* differs:
    `fauna_client::NestPublicChunkFetcher` (native — reuses the session
    `AuthClient`'s pinned http; the route ignores the bearer it carries) behind
    `NestMailInboundSource::fetch`, shared across all six native apps; and a
    gloo-net binding behind `WasmConversationsManager::resolveMailBodyRef` (web),
    which the SPA's `drainFeed` calls when `body_ref` is present and feeds
    straight to `ingestSealedInbound` — kept beside that method rather than
    inside it so the ingest stays synchronous. It lands **unconditionally**
    (harmless: a client that can resolve a reference it never receives loses
    nothing) and reaching every app is what the serve gate waits on.
    Resolve failure follows each path's existing open-failure semantics — fatal
    to the page natively, per-record on web — and is transient by construction:
    the nest re-stages the chunks on every serve, so the next tick retries.
    Proven tier_3 against a real nest serve + the real open route by
    `mail_body_ref_round_trip.rs::the_mailbox_feed_serves_an_over_frame_envelope_by_reference`,
    which drives this same shared resolver; the fail-closed axes (hash width,
    lying total, missing chunk) are unit-pinned in `fauna_mail::body_ref`.
    **Proven through the app UI since 2026-08-31** — the premise behind the old
    "not yet proven" note (that a process-global const with no runtime flip
    keeps an e2e from serving a reference) stopped holding when the gate's
    default became `true` at the 2026-07-18 flip: a spawned nest now serves
    references without anything to turn on.
    `test_mail_client_receive_over_frame_reference.py` drives a real ~3 MiB
    inbound message all the way to a rendered bubble and reads the HEAD and TAIL
    markers back out of it, green on **web** (2026-08-31), **tui** (2026-09-01),
    **windows**, **macos** and **ios** (2026-09-11 — apple only once its
    whole-app hang on this body was fixed, § Implementation status today), and
    **linux** (2026-09-13, § Implementation status today).
    ⚠ One thing that proof also measured, and which is NOT a settled product
    answer: on windows the bubble takes **40.7 s** to appear after the thread is
    opened, where web, tui and (since the fix) macos and ios take a couple of
    seconds at most (linux's 2026-09-13 figure was masked and its real cost
    was worse than windows'). See § Implementation status today, which owns
    both fixes.
- **The plaintext legs — the staged-envelope rule (ratified 2026-07-18; DESIGNED, not
  built).** Two legs cannot stage sealed bytes: **client import**
  (`ImportMessageItem.body` is raw RFC 5322 — the nest seals at ingest, and that posture
  *stays*: client-pre-seal was retired because the serve path recognises only
  `seal_recipient_blob` output, `mailbox-migration.md` § Architectural rules) and the
  **outbound queue pair** (`outbound_mail_queue.raw_message` is plaintext the nest must
  read — SRS rewrite, list-send DKIM handling, bounce/DSN extraction — and the MTA must
  read to ship over SMTP to an external MX that holds no Fauna key). The open download
  route is safe *only because everything in the chunk store is ciphertext*
  (`webdav-server.md` § Bulk-byte plane — "confidentiality is cryptographic, not
  transport-scoped"); staging plaintext would break that blanket invariant: disclosure to
  anyone holding the hash, plus a blake3-of-plaintext correlation/confirmation channel
  (two actors staging identical bytes collide; anyone who can guess a message's exact
  bytes can confirm its presence). So above the inline ceiling these legs stage a
  **staged envelope**: the producer AEAD-encrypts the whole plaintext under a **one-shot
  random 32-byte key** (ChaCha20Poly1305 — the in-tree symmetric convention; random
  nonce prepended to the ciphertext; AAD = a fixed domain-separation string), splits the
  *ciphertext* with the same `fauna_mail::body_ref` chunk rule, uploads the chunks, and
  the RPC carries the reference **and the key** as a
  `StagedBodyRef { chunk_hashes, total_bytes, key }` — a deliberately distinct type from
  `MailBodyRef`, so a sealed-bytes reference and an ephemeral-key ciphertext reference
  can never be confused at the type level. The key rides inside the already-confidential
  authenticated WS-RPC — the very channel that today carries the plaintext itself
  inline, so no party learns anything it could not already read; the chunk store never
  sees plaintext or a stable content hash. The consumer joins fail-closed on the
  declared total, opens the AEAD (whose tag authenticates the entire join), and proceeds
  exactly as if the bytes had arrived inline. Staged chunks stay disposable GC orphans;
  the fresh key means unique ciphertext per staging, so there is no cross-staging dedup
  to lose. **Rejected alternatives (recorded — do not re-propose):** client-pre-seal for
  import (reverses the ratified S1 seal-at-ingest: the serve discriminator recognises
  only `seal_recipient_blob` output, and it re-opens the unsealed-User-class-ingest hole
  Phase 3 closed); a tokenized/authenticated download route for staged plaintext
  (`webdav-server.md` explicitly rules out treating the store as a chunk-hash ACL);
  raw-plaintext staging (breaks the ciphertext-only store invariant above).
  - **Leg: client import** (`import_message` / `import_message_batch`). Additive
    `staged_body: Option<StagedBodyRef>` on `ImportMessageItem`, exactly-one-of with a
    non-empty `body` (the guard shape `persist_inbound_mail_request` already uses).
    The client stages with its own **session bearer** — the chunk write routes already
    accept it (`webdav-server.md` § Bulk-byte plane), so no mint and no new auth
    surface. Nest resolves from its local blob store, opens the envelope, checks the
    recovered plaintext length against `body_size`, then follows the existing
    `import_one` path unchanged: validate, seal-at-ingest via the D2 resolver, quota on
    the sealed size, dedup. Admission ceiling is `effective_max_raw_message_bytes` —
    the same one shared rule the SMTP `Data` paths read. Wire application owned by
    `mailbox-migration.md` § RPC surface. No deployed-producer constraint exists: no
    shipped app calls the import RPCs yet.
  - **Leg: the outbound queue pair — both directions, same envelope.**
    `EnqueueOutboundMailRequest` gains `staged_body: Option<StagedBodyRef>`
    (exactly-one-of with a non-empty `raw_message`; the bridge mints its `MailBody`
    bulk token exactly as the ingest staging leg does), and `OutboundUnit` gains the
    same field downward — nest encrypts under a fresh key per serve, stages to its own
    blob store (the serve shape), and hands the reference + key to the bridge; a
    re-fetch simply re-stages (stateless; superseded chunks are GC orphans). The
    `fetch_outbound_due` reply is additionally **byte-budgeted** — close early on the
    frame budget; queue rows are independent (a due-set poll, not a cursor walk), so a
    shorter page has no freeze semantics and no ordering hazard. At rest the queue row
    is unchanged: plaintext SQL BLOB the nest must read, on nest-local disk, never the
    open store (per-recipient row explosion duplicates a large body N ways — a
    pre-existing at-rest cost, accepted; a body-dedup normalization would be its own
    additive slice if it ever matters). **`deny_unknown_fields` stays on all four
    structs**: this is the version-locked in-image bridge data-plane
    (`../architecture/transport.md` § Schema and forward-compat discipline, rule 4 —
    both ends ship in one artifact, no cross-version skew exists), so the additive
    fields need no deployed-reader gate, and the skew hazards a cross-version reading
    would suggest (a new-nest reference handing an old bridge an empty `raw_message`)
    are structurally unreachable.
  - **✅ Two defects this design closed — BOTH SHUT, verified against code 2026-08-23.**
    Kept as the historical record of *why* the staged envelope is shaped as it is; the
    dated wording below ("open today", "Interim:") describes the **pre-S9.3** world and is
    superseded by the *Implementation status (2026-07-18)* paragraph directly beneath this
    list — read that first. ⚠ **Do not re-file either as a live hole** — a 2026-08-21
    documentation sweep did exactly that off this passage, and the claim was refuted
    against code on 2026-08-23.
    Where each is shut, in code: **(1)** the submission `552` clamp is RETIRED, replaced by
    `stageOutboundEnvelope` staging the over-inline-budget external body
    (`internal/mta/body_ref.go`, pinned `submission_test.go::TestSubmissionDataExternalOverInlineBudgetStages`);
    **(2)** `fetch_outbound_due_handler` byte-budgets its reply and closes the page early
    (`used + cost > OUTBOUND_REPLY_BUDGET_BYTES ⇒ break`), stages any over-inline row down to
    a tiny reference, and permfail-DSNs a row that alone still cannot fit — all three in
    `bins/fauna-nest/src/bridge_routing_handlers.rs`, pinned by
    `tests/outbound_fetch_budget.rs` (`fetch_outbound_due_pages_on_the_reply_byte_budget`,
    `a_row_over_the_inline_budget_is_staged_and_delivered`,
    `enqueue_over_the_inline_budget_via_staged_body_is_accepted`). The one leg still
    designed-not-built is the **forward** leg, named as such in the status paragraph — its
    interim guards STAY and are not part of this closed pair.
    1. **Submission→external over-frame sever.** The perimeter admits ~8.06 MB raw
       (`effective_max_raw_message_bytes`) but the external-recipient dispatch ships
       the signed body inline in `enqueue_outbound_mail` with no frame guard
       (`internal/mta/submission.go` `Data` → the enqueue call), so an
       external-recipient submission over ~2 MiB severs the WS connection — the
       permanent-condition-as-transient-failure class again, live since the S5–S8
       perimeter raise. Interim: a post-sign assembled-size guard on the external leg
       answering `552 5.3.4`, mirroring the S5 interim clamp; honest asymmetry until
       the reference leg lands (a user can *receive* ~8 MB but sends externally only
       to the inline ceiling).
    2. **Outbound dispatch wedge.** `fetch_outbound_due_handler` returns every due
       row inline with no byte budget. Every queue ingress (`enqueue_outbound_mail`,
       `forward_message`, `fauna.email.send`) is itself frame-capped, so no single row
       can exceed the frame today — but the reply **sums** rows: two ~1.2 MiB
       external-recipient submissions produce a due batch whose reply overruns the
       frame, is refused by the transport, and — the rows staying due — every
       subsequent poll re-assembles the same over-frame reply: **all outbound mail on
       the nest wedges permanently**. Interim: byte-budget the reply (close early;
       rows independent, the rest ride the next poll); backstop the enqueue/forward
       admission points at the inline budget (typed error beside the existing 50 MB
       bound — narrows the external-send window by the 64 KiB headroom until the
       reference leg lands, an accepted cost); and, defensively, fail a row that
       *alone* cannot fit a reply with a permanent-failure DSN at fetch time instead
       of wedging the poll (unreachable via today's frame-capped ingresses;
       belt-and-braces for the reference-leg future). The upward forward leg
       (`dispatchForward` → `forward_message`, full body inline) over-frame today
       fails as a logged dropped forward — the interim adds an explicit size
       suppression verdict there; the staged envelope lifts it.
  - **Implementation status (2026-07-18):** **the envelope + the outbound-pair
    leg are FULLY BUILT end-to-end (nest + shared Rust + Go), S9.3.** The envelope:
    `fauna_mail::staged_envelope` (feature `staged-envelope`: seal/open + the
    seal-and-split / join-and-open compositions, uniffi-exported for the Go
    bridge, fail-closed unit-pinned; `StagedSeal` carries a redacted `Debug`). The
    wire: additive `StagedBodyRef { chunk_hashes, total_bytes, key }` — a type
    distinct from `MailBodyRef`, its `key` a zeroizing, redacted
    `fauna_core::secret::SecretByteBuf` that encodes as a byte string (Go `[]byte`)
    — on `EnqueueOutboundMailRequest` + `OutboundUnit`. Nest:
    `mail_body_plane::{stage,resolve}_staged_body` (seal under a fresh one-shot key
    + stage / join-and-open, fail-closed) drive `enqueue_outbound_mail` (resolve
    the reference up into the queued plaintext) and `fetch_outbound_due` (stage an
    over-inline-budget row down, statelessly re-staging per serve). Go bridge:
    submission (`internal/mta/body_ref.go::stageOutboundEnvelope`) stages an
    over-inline-budget external body and enqueues the reference; the outbound
    worker (`internal/mta/outbound.go::bodyFor` → `resolveStagedOutboundBody`) GETs
    the chunks, joins fail-closed on the total, and AEAD-opens before DKIM-signing.
    **The interim `enqueue_outbound_mail` inline-budget backstop and the submission
    552 clamp are RETIRED — replaced by staging** (the product ceiling, enforced at
    the DATA-read `max_message_bytes` clamp, is now the only outbound size bound);
    the fetch-side alone-over-budget permfail-DSN narrows to an unreachable
    belt-and-braces rail (every served unit fits: inline rows ≤ the inline budget,
    staged rows carry a tiny reference). A staging failure on the submission leg is
    transient (`451`); a resolve failure on the worker fails closed to a retry.
    Pins: `outbound_fetch_budget.rs` (staged deliver + staged enqueue),
    `staged_envelope_test.go` (round-trip + corrupt-key/lying-total/nil-plane
    fail-closed), `submission_test.go::TestSubmissionDataExternalOverInlineBudgetStages`.
    **The client-import leg — the NEST half — is BUILT (S9.4).** Additive
    `ImportMessageItem.staged_body: Option<StagedBodyRef>` (exactly-one-of with a
    non-empty `body`, the `persist_inbound_mail_request` guard shape); `import_one`
    resolves it via `mail_body_plane::resolve_staged_body`, checks the recovered
    plaintext length against `body_size`, admits at `effective_max_raw_message_bytes`
    (the same shared rule the SMTP `Data` paths read — authoritative nest-side, since
    the client is untrusted), then takes the unchanged seal-at-ingest path. The
    **skew contract** (`mailbox-migration.md`) is honored: the
    empty-body refusal stays *ahead* of staged resolution, so an older nest that
    drops the unknown field degrades to a visible per-message error, never an empty
    import; a bad reference is a per-message `Errored` (siblings still import), an
    infra failure aborts the call. Pins: `bridge_import_handlers.rs` (6 handler tests
    — round-trip-seal, exactly-one-of, skew empty-body, body_size, admission,
    bad-ref-isolates-siblings) + `mail_import_staged_body.rs` (tier_3: the client
    stages ciphertext over the real `POST /api/v1/chunks` route with its **session
    bearer** — no mint — then imports by reference and the record opens with the
    owner's secret byte-for-byte). **Not built (a separate track, not S9.4): the
    client SEND path** that would drive this — no shipping app calls the import
    RPCs at all yet (even inline), so nothing produces a `staged_body` in production;
    the producer-side staging + send belongs with the import wizard (Track D,
    `mailbox-migration.md` § RPC surface). **Remaining, still designed-not-built:**
    the **forward leg** — the `forward_message` inline-budget backstop + the
    `dispatchForward` size-suppression verdict (`server.go`, pinned by
    `TestForwardSuppressedTooLarge`, now measuring the stamped bytes it ships) STAY
    until the forward leg gets its own staged path.
    **The IMAP APPEND leg is BUILT (2026-07-18).** An over-inline-budget sealed
    APPEND body now stages on the bulk-byte plane and the `fauna.bridges.append`
    RPC carries a plain `MailBodyRef` (already ciphertext, so no staged-envelope):
    additive `body_ref: Option<MailBodyRef>` on `AppendMessageRequest`
    (exactly-one-of with a non-empty `encrypted_body`, the
    `persist_inbound_mail_request` guard shape, tolerant reader — an old nest
    ignores the key); the nest `append_message_handler` resolves it via
    `mail_body_plane::resolve_body_ref` into the identical at-rest bytes an inline
    APPEND stores; the Go MDA decides inline-vs-staged through the newly-shared
    `internal/mailstage.StageSealedBody` (lifted out of the MTA so the switchover
    predicate and chunk boundaries are identical across every producer of sealed
    bytes) — an over-inline-budget body now stages instead of severing the
    frame. **The BAD-for-size mapping this bullet originally pinned to an
    at-rest ceiling is superseded by ceiling retirement (§ *Ceiling retirement —
    LANDED* above, "APPEND became a nest-enforced ceiling"): `StageSealedBody`
    no longer has an at-rest ceiling to refuse against (only the still-live
    hint-alone-over-budget case maps to `BAD` there), and the authoritative size
    gate is nest-side — `append_message_handler` refuses a declared
    `ciphertext_size` over `max_message_bytes` with the shared typed
    `message_too_large`, which the MDA maps to an IMAP `BAD` — a client upload
    the MUA must not retry.** Pins:
    `bridge_routing.rs` (wire round-trip + old-nest tolerance),
    `bridge_imap_handlers.rs` (the two exactly-one-of guards),
    `mail_body_ref_round_trip.rs::an_over_frame_append_crosses_by_reference...`
    (tier_3 end-to-end), and Go `append_stage_test.go`
    (`TestAppendStagesOverBudgetBodyByReference` /
    `TestAppendInlineBodyCarriesNoBodyRef` / `TestAppendOverProductCeilingMapsToBad`
    / the ceiling-retirement change-detector
    `TestAppendFormerlyOverAtRestCeilingNowStagesAndDelivers`).
    **The first-party `message_too_large` typed error is BUILT (2026-07-18).**
    Shared code `fauna_protocol::email::MESSAGE_TOO_LARGE_CODE`
    (`"fauna.email.too_large"`, precedent `fauna.mls.too_large`): `fauna.email.send`
    pre-checks `raw_rfc5322` against `effective_max_raw_message_bytes` (sourced from
    `get_spam_policy().effective().max_message_bytes`, never a hardcoded const, so
    the ceiling-retirement slice re-derives it) and returns it as an `RpcError`
    code; `import_message` reports the same identifier as its over-ceiling
    `ImportMessageOutcome::Errored` reason, so a client matches one identifier on
    both legs. This is the nest's authoritative product-ceiling enforcement (the
    client is untrusted); the **inline-ceiling** refusal that keeps an over-frame
    send from severing the WS connection is a distinct **client-side** pre-check.
    Pins: `conformance_email_send.rs::send_over_the_ceiling...`
    and the `bridge_import_handlers.rs` admission test.
    **The client-side render + inline-ceiling pre-check is BUILT (2026-07-19,
    shared Rust, all 7 apps for free).** `fauna_protocol::RpcError::localized()`
    maps `MESSAGE_TOO_LARGE_CODE` to the shared `error.email.too_large` i18n
    string (the same generic seam every conversations RPC failure already routes
    through — `fauna-client-conversations`'s `conv_rpc_error`, no new per-app
    wiring needed), so a live `fauna.email.send` rejection renders it
    automatically. The **inline-ceiling** pre-check lives in the Conversations
    compose seam (`fauna_conversations::backends::smtp::SmtpBackend::send`,
    compared against `fauna_mail::transport_limits::MAX_INLINE_RAW_MESSAGE_BYTES`)
    and refuses locally — before the RPC — with the identical localized string,
    reusing the page's existing `error-message` surface (no new ui.yaml ID). The
    `import_message`-outcome render leg has no consumer yet (the client import
    wizard — Track D above, `mailbox-migration.md` § RPC surface — is unbuilt,
    and no shipping app calls the import RPCs) — deferred to whichever
    session builds that wizard.

## Implementation status today

- **The client-feed reference leg is built, gate-on, and witnessed through the
  app UI** (see § Message size limits → *the client-feed leg*): green on web
  (2026-08-31), tui (2026-09-01), windows, macos and ios (2026-09-11), and linux
  (2026-09-13) via
  `tests/e2e-unified/tests/test_mail_client_receive_over_frame_reference.py`,
  which delivers a real ~3 MiB inbound message over SMTP and reads the HEAD and
  TAIL markers back out of the single rendered bubble. **linux's 2026-09-13
  green, and the "bubble 0.5 s after the thread opens" measured with it, were
  MASKED — corrected 2026-09-21.** The test reads only widget flags, and at the
  time a saturating e2e state publish held the GTK main loop, so the frame clock
  that lays the bubble out never ran during the test: the bubble *existed* in
  0.5 s and was never measured. Once that publish stopped saturating the
  loop the honest cost showed: one `gtk::Label` over
  the ~40,332-line paragraph stalls the UI thread for **~76 s** inside Pango
  (`g_utf8_strlen` under `pango_layout_get_size` → `gtk_label_get_layout`, a
  per-line walk from the start of the text — super-linear in hard breaks,
  apple's class below, not WinUI's), and every automation op times out behind
  it. linux therefore ADOPTS the shared line-run projection, as lazily laid-out
  labels (render-model.md § Implementation status today owns the
  decision). Measured through the same test with the saturator gone
  (2026-09-21): the bubble is there when the thread-open step returns
  (`[over-frame] bubble rendered 0.0s`), HEAD and TAIL read back from the
  document, and no automation op times out anywhere in the run — GREEN on an
  honest main loop this time. The inline-sized control
  (`test_mail_client_receive.py::test_client_driven_receive_renders_decrypted`)
  is green on macos and ios too, so the client-feed pipeline itself (stage →
  body_ref → chunk-fetch → rejoin → open → ingest) was never apple's problem —
  rendering the large body once the thread opened was.
- **The apple large-body hang — root cause profiled and FIXED (2026-09-11).** Symptom, 3/3 runs (macos ×2, ios ×1): after
  the thread opened, every automation RPC timed out at 120 s and `/health` stayed
  silent a further 90 s while the listener kept accepting connects, each run
  failing after 755–1324 s with the app never recovering. A `/usr/bin/sample` of the live
  `FaunaMacOS` mid-stall put **100% of main-thread samples** in the thread
  detail's `LazyStack.measureEstimates` → `StyledTextLayoutEngine.sizeThatFits` →
  `ResolvedStyledText.StringDrawing.sizeThatFits` →
  `-[NSAttributedString boundingRectWithSize:options:context:]` →
  `__NSStringDrawingEngine` → `CTLineCreateWithAttributedString` (glyph
  encoding), with the process at ~100% CPU in state `R` for 13 minutes —
  spinning, not deadlocked. The in-process automation server thread sat in
  `DispatchQueue.main.sync` (`InProcessAutomationServer.swift` `onMainActor`)
  behind it, and that server's accept loop answers one connection at a time
  (`serve`) — which is why even `/health`, which needs no main-thread state,
  went silent. The body is `plaintext_to_document`'s ONE paragraph (the filler
  has no blank line to split on) of ~40,332 hard-broken lines, painted as ONE
  SwiftUI `Text`, and that layout is **quadratic in hard line breaks**: a
  standalone probe of the same view chain measured 500 / 1,000 / 2,000 / 4,000
  lines at 0.38 / 1.39 / 5.4 / 21.1 s — about 36 minutes per layout pass for this
  body — while the same bytes as a single line took 0.06 s, and
  `.textSelection(.enabled)` made no difference. **Fix:** the shared paint
  projection `fauna_core::render::inline_line_runs` (render-model.md § Where
  logic lives) splits a text block into runs of at most 16 lines, and FaunaKit's
  `DocumentBodyView` paints a multi-run block as a `Text` per run in a
  `LazyVStack`, so only the runs on screen are laid out. Measured through the app
  UI on the same body: the bubble is already there when the thread-open step
  returns on macos, and appears 1.8 s after it on ios. A block of 16 lines or
  fewer — nearly all real prose — paints exactly as before; in a longer one a
  drag-selection spans one run.
- **Two earlier leads, kept because they were real — neither was the hang.**
  `DmMessageBubble`'s `isMutedMatch` eagerly rendered the whole document across
  the UniFFI boundary on every body pass; it now short-circuits on an empty mute
  list and matches the raw `body` like web/windows/android, and
  `dm-message-text`'s document-derived read (which must stay document-derived —
  `message.rs`'s cross-app read-uniformity contract) is memoized per
  `messageId`. `RenderDocument::to_plaintext` over the body
  measured 61.5 ms.
- **Opening a ~3 MiB body on windows took over two minutes, against a couple of seconds at
  most on web, tui, macos and ios — DIAGNOSED 2026-09-13: product cost, in two places, one of
  them fixed.** The over-frame test first recorded it as 40.7 s from the thread being opened to
  the bubble existing (2026-09-11); timed from the click in the live app it was far longer. It
  is NOT apple's failure: windows paints the whole document into ONE `TextBlock`
  (`DocumentPainter.Apply`), the same shape, but its text layout is not quadratic in hard breaks
  — at a fixed ~1.54 MB, cutting the breaks from 20 000 to 20 made the open *slower*, and `Apply`
  itself costs 0–1 ms — so windows does not adopt the line-run projection for apple's reason
  (render-model.md § Where logic lives). That pass also memoized
  `ConversationsViewModel.SelectedDetail`, which had re-derived and re-marshalled the whole thread
  on every property access (~130 ms for a 1.5 MB body), and found the automation reads cheap
  (`count()` 0.05 s, `get_text()` 0.09 s for 1.5 M characters). The seconds were then located by
  capping one render input at a time, timing the click to `thread-header` on the test's own open
  path: as shipped **146.5 s**; the list-row snippet capped
  **59.0 s**; row and bubble body both capped **3.6 s**. So:
  - **The thread-list snippet was the whole body** — a 3.1 MB string that every `snapshot()`
    re-derived (~270 ms per call in the e2e debug build), every state push shipped, and windows
    laid out in its one-line `conversation-item` row on every observer tick. That was ~60 % of
    the open. **Fixed in shared Rust**: the snippet is a bounded preview
    ([conversations.md](../ui/conversations.md) § State & data shape owns the bound), which also
    shrinks every app's row, the e2e state row and linux's notification body. With the fix in
    and nothing capped, the row's share is gone — a 489-byte snippet, `SyncThreads` back to
    4 ms — but the open still took **79.9 s** on a box running six other builds, all of it the
    bubble below.
  - **The rest was the bubble — FIXED 2026-09-15.** A single fresh layout of a ~3 MB `TextBlock`
    in the visible tree costs tens of seconds (≥32 s measured, re-entering the page with the
    thread selected) — not the ~350 ms an earlier forced layout suggested — and
    `ConversationsPage` rebuilt every bubble on every observer tick, so that layout was paid
    repeatedly while ticks queued behind it. windows now paints a body with a block over the
    shared line-run budget as one `TextBlock` per 16-line run in a virtualizing
    `ItemsRepeater` (only the on-screen runs lay out) and keys its bubble list by message id,
    re-binding in place per tick ([render-model.md](../architecture/render-model.md)
    § Implementation status today owns the decision). Measured
    through the same test: the bubble is already there when the thread-open step returns
    (`[over-frame] bubble rendered 0.0s`, against 40.7 s on 2026-09-11), the tick that paints
    it costs 0.67 s — 0.33 s marshalling the 3 MB document across UniFFI, 0.33 s walking it
    into 2 521 segments and painting — with one ~1.2 s UI stall for the first layout;
    `test_mail_client_receive_over_frame_reference.py --app windows` is GREEN.
  - **What only the harness pays** on top: each `set_state` round trip waited for a state push
    carrying the 3.1 MB snippet (1–4 s), and every UI Automation call blocks for as long as the
    UI thread is stalled — `open_thread_by_id` checks its 15 s deadline only between such calls,
    so in-flight calls carried it far past 15 s. `_wait_thread_open` raises at 5 s
    (2026-09-11), which kept `test_mail_client_receive_over_frame_reference.py` red on windows
    until the bubble cost was fixed; the 5 s stands.
  - **Budgets.** Nothing here yet specifies a render budget for a large body. The e2e budget
    (`BUBBLE_RENDER_BUDGET_S`, 90 s) was sized to tolerate the pre-fix windows open, which is
    deliberately not the same as endorsing it; every app now renders the bubble well inside it,
    and the test prints each app's open-to-bubble time (`[over-frame] bubble rendered …`), so
    it is measured wherever the test runs.
