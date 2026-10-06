# Mail forwarding — target state

Owns: mail-forwarding
Status: ratified
Authority: forwarding mechanics — per-account forward-all + per-rule forward actions, admin external-forwarder dispatch, SRS envelope rewrite + secret rotation + bounce decode, loop detection, the per-account forward rate-cap + queue, NDR-to-forwarder routing, and the plaintext-floor placement of forward config; defers the forwarder alias kind/storage/resolution to behavior/mail-aliases.md § Kind 7, the filter-rule action set + UI shape to behavior/email-filters.md § Email filter rules, knob tiers/bindings/surfaces to behavior/mail-policy-config.md, and the outbound queue/retry machinery to behavior/smtp-server.md § Outbound delivery. On conflict: filter-rule UI shape → smtp-server.md wins; knob naming → mail-policy-config.md wins; MTA pipeline ordering → smtp-server.md § Inbound policy stack wins.

> **Purpose:** the destination for "this user's mail is forwarded somewhere else." Covers the two forwarding shapes (per-account forward-all + per-rule forward-matching), the admin external-forwarder shape, SRS envelope rewriting (so the downstream MX's SPF check passes), loop detection (so a misconfigured pair of accounts can't tornado each other), NDR routing on permanent forward failure, the per-account rate cap (so a forwarding account can't be hijacked into a free open relay), and the forward pipeline's interaction with the sealed-at-rest posture (§ Storage-mode interaction). Build progress: § Implementation status today.

---

## Goal

Two user-facing forwarding shapes, both compatible with SPF + DMARC + DKIM + the loop-detection floor, both rate-capped so a compromised credential can't be hijacked into a free open relay:

1. **Per-account "forward all"** — Tier-3 knob (`mail.account.forward_all_to`). When set, every inbound message after local-delivery is also re-injected outbound to the configured address. Local copy is kept (the user has both their Fauna mailbox AND the downstream account); the downstream account also receives. Default is **both** — the user has a record in their own app, and the downstream copy is a convenience.
2. **Per-rule "forward to"** — the `forward` action in the per-account filter-rule action set (per `email-filters.md` § Email filter rules). On rule match: forward to the rule-configured address. The rule itself carries the copy-mode (default `copy` — keep local + forward; alternative `redirect` — forward without local delivery). Multiple rules can fire on one message → multiple forwards, one per matched rule. Forwards from rules respect the same SRS / loop / rate-cap floor as forward-all.

Plus a third, **admin-owned** shape — **admin external forwarders** (§ Admin external forwarders) — for mailbox-less addresses like `info@` that route to an external destination. It reuses the same dispatch (SRS / loop / rate-cap / NDR) but is attributed to the admin actor, configured deployment-side, and lives as an alias kind (`mail-aliases.md` § Kind 7). The two user shapes are user-facing and user-private; the admin shape is admin-facing and plaintext routing config (§ Storage-mode interaction).

**Bar: cooperative-MTA conformance.** A forward from a Fauna account to a downstream MX must SPF-align under our envelope MAIL FROM, must DKIM-survive the forward (or re-sign if we strip headers), must not generate backscatter (the bounce-routing floor below), and must not become a tornado-loop attack vector under any pair of misconfigured rule sets.

**Forwarding is an attributed actor's outbound, not the MTA's open relay.** Every forward is attributed to a Fauna actor (the forwarding-config owner — the account-holder for forward-all, the rule-creator for forward-matching, or the **admin** for an admin external forwarder, § Admin external forwarders). The forward consumes the attributed actor's submission quota — the nest-side per-actor recipients/day quota enforced via `fauna.bridges.check_submission_quota` (per `smtp-server.md` § Architectural rules; **corrected 2026-07-19** — there is no `per_actor_msg_per_hour` cap in the current implementation, only this recipients/day quota plus a per-message recipient cap), composed with the dedicated forward rate-cap below. There is no anonymous-forward path; the MTA does not relay on behalf of an unauthenticated principal — an admin forwarder is attributed to the admin (an authenticated principal), not to no one.

---

## Two forwarding shapes

### Per-account "forward all"

The forwarding account's `mail-settings` page (the Tier-3 per-account UX home per `mail-policy-config.md` § Tier 3 — per-account + § Policy catalog banner; ids allocated with the consuming per-app track — there is no `account-detail` page) carries a "Forward all incoming mail to" text field. Setting it to a non-empty address enables forward-all; clearing it disables.

- **Trigger point** — after the inbound MTA's `VerifyAndDeliver` RPC has accepted the message for local delivery and after the local mailbox write has committed (so a forward-all crash mid-flight leaves the local copy intact). The MTA reads the recipient's `forward_all_to` via `fauna.bridges.fetch_recipient_forward_config` — a **per-recipient** fetch (mirroring `fetch_recipient_mls_pubkey`); `forward_all_to` is per-account, *not* a field of the deployment-wide `fetch_config` snapshot. If set, the MTA queues a forward via `fauna.bridges.forward_message`.
- **Copy mode** — always `copy` (both local + downstream). A "redirect mode" (forward without local copy) is **not** offered on the forward-all knob — users who don't want a local copy use the per-rule shape with explicit `redirect`.
- **Address validation** — at the knob-write time, the client validates the address (RFC 5321 syntactic check), and nest re-validates server-side. Wire-time validation only catches typos; the actual deliverability of the address is empirically discovered on first forward.
- **Account aliases** — the forward-all target is the user's primary address, but aliases (per `mail-policy-config.md` § Tier 3 — `mail.account.aliases`) compose by reusing the same `forward_all_to`. Inbound to `alice@<our-domain>` and to `alice+work@<our-domain>` (an alias) both forward to the same configured downstream address.
- **The forwarding actor** is the account-holder. The forward consumes their submission quota and forward quota.

### Per-rule "forward to"

The user's filter-rules UI (per `email-filters.md` § Email filter rules) carries `forward` as one of the rule-action choices. A rule with this action carries:

- **Destination** — text field, the forward-to address. RFC 5321 syntactic validation at write time.
- **Copy mode** — `copy` (default — keep local + forward) or `redirect` (forward, do not deliver locally). Visible in the rule-editor UI; default checkbox is "keep a local copy."
- **Stop-on-match** — same as other rule actions; per `email-filters.md` § Email filter rules, "the first matching rule wins unless the rule is marked `continue`." A forward rule that is *not* marked `continue` halts further rule evaluation but the forward fires.

**Multiple rules firing on the same message** produce multiple forwards (one per matched rule), each to its rule-configured destination, each subject to the same SRS + loop + rate-cap floor independently. A message that hits 3 forward-rules + has forward-all set generates 4 forwards.

**`redirect` is a per-recipient placement decision (ratified 2026-09-25).** One fired `redirect` Forward anywhere in the recipient's `continue` chain means that recipient keeps **no local copy**; every forward then dispatched for the message — the chain's `copy`-mode rules and forward-all included — is labelled `copy_mode=redirect`, because no local copy exists to be truthful about. A `Discard`/`Reject` in the chain still suppresses everything (no delivery, no forward), exactly as for a `copy` Forward. With no local commit to order after, **the forwards' durable enqueue in nest's outbound queue IS the delivery commit**: a redirect recipient's forwards dispatch *first*, before any placement, and only if **none** could be enqueued — null sender, a loop floor (§ Loop detection), the size guard, a nest error — does the message fall back to ordinary local delivery. Suppression costs the redirect, never the mail: § Loop suppression vs. delivery extends to redirect as "no forward could be enqueued ⇒ the local mailbox receives it". A vacation `AutoReply` fires after either commit; an inbound SRS bounce is never forwarded and so always takes the local path. **Wire + storage:** the copy mode is the additive `#[serde(default)] redirect: bool` on `EmailFilterAction::Forward` (`fauna-protocol`, mirrored on `fauna_mail::filter::FilterAction` and the FFI enum) — a peer predating it reads every rule as `copy`, and a nest predating it stores a newer app's `redirect` rule as `copy`, the safe degradation both ways (`architecture/version-compatibility.md` I4); the field is tolerated, not preserved, through such a peer, and the "never opens in a form that would lose part of it" guarantee rests on the shared rule-editor seam: a form that collects the destination and copy mode reopens a rule with both and writes both back, and a form that does not yet collect them never opens a `Forward` (`ui/settings.md` § Where logic lives, *Email filter create-dialog encoding*). At rest the nest keeps `email_filters.action = forward:<address>` unchanged and carries the mode in the additive `email_filters.forward_redirect` column — a column rather than a new action-string prefix so an older nest binary opening a newer database ignores it and reads a `copy` rule, instead of misparsing an unknown prefix as its defensive `Discard` fallback.

### Conceptual ownership

Both shapes are the **user's outbound**. Even though the forward fires "automatically" without an explicit Send action, the message-on-the-wire is attributed to the forwarding actor — they configured the rule, they bear responsibility for what gets forwarded, they consume the quota, and **they receive the bounce** when forwarding fails (per § NDR routing below).

---

## Admin external forwarders

A third shape, **admin-owned**, alongside the two user shapes above: the deployment admin maps an incoming address on a local domain to an **external** destination with **no local mailbox** — the cPanel "forwarders" / catch-all-style routing every mail host offers (`info@<our-domain>` → `yourname@example.com`, `sales@<our-domain>` → an offsite team address). This is **not** an anonymous relay: it is owned by and attributed to the **admin actor**, who is the accountable Fauna principal for it.

- **It is an alias kind, not a new subsystem.** An admin forwarder is, mechanically, an alias whose target is external rather than a local actor. It lives as an `account_aliases` `kind='forwarder'` row and resolves through the **single** RCPT-TO resolver (`mail-aliases.md` § Kind 7 + § Resolution order — that doc owns the kind, storage, and resolution; this doc owns the dispatch below). Managed via Admin-class RPCs (`create_forwarder` / `list_forwarders` / `delete_forwarder`), kept out of the user-facing alias list.
- **It reuses the *same* forward dispatch as the user shapes.** On a forwarder match the resolver returns `Forward {forward_target, forwarder_actor_id=<admin>}`; the MTA hands it to the identical pipeline — SRS envelope rewrite (with the admin as the SRS forwarder-actor), loop detection, the per-account forward rate-cap (charged to the admin actor), and NDR-on-permanent-failure routed to the admin (§ NDR routing). The dispatch is owner-agnostic; only the attributed actor differs.
- **It is `redirect`-shaped:** no local copy is kept (there is no local mailbox). This is the genuine "forward-only address." So a forward nest did not durably enqueue — refused at the queue ceiling (§ Queue ceiling) or nest unreachable — answers **`451`**, never `250`; a floor's deliberate suppression (null sender, a loop floor, the size guard) is still a drop. DATA carries one reply for the whole envelope, so the MTA dispatches an envelope's forwarders **before** any of its local recipients — or, on submission, the sender's Sent copy — commit: the `451` never makes a retry re-deliver to them. A second forwarder in the same envelope whose predecessor already enqueued is re-forwarded on the retry — a duplicate at the external destination, never a loss.
- **Its config is plaintext at rest** (deployment routing metadata, § Storage-mode interaction), unlike a user's `forward_all_to`.

The admin shape and the user shapes converge on one dispatch pipeline and one resolver, and diverge only where they must: ownership/attribution and the user-vs-admin config surface. (Both rest at the plaintext routing-metadata floor today — § Where the forward config lives at rest; sealing user forward config for separate-bridge deployments is the deferred N1b upgrade.)

## SRS (Sender Rewriting Scheme)

### Why we need it

A naive forward — keep the envelope MAIL FROM unchanged, just re-emit to the downstream MX — fails at the downstream MX in two ways:

1. **SPF.** Per RFC 7208, the downstream MX checks SPF against the **envelope** MAIL FROM domain. If we forward `alice@gmail.com`'s mail to Yahoo without rewriting the envelope, Yahoo sees: connection from our MTA's IP, `MAIL FROM: alice@gmail.com`, SPF lookup of `gmail.com`'s `_spf.gmail.com` record, our IP not in Gmail's authorized senders → SPF hardfail → Yahoo discards or marks spam. Our forward is dead-on-arrival.

2. **DMARC.** Per RFC 7489 alignment, DMARC `pass` requires the envelope-from domain to align with the From: header domain via SPF *or* the DKIM-signing domain to align with the From: header domain via DKIM. Forwarding usually breaks SPF alignment (envelope is rewritten away from the From: domain); DKIM alignment can survive *if* the message body and the DKIM-signed headers are unmodified. We choose to make **both** survive by:
   - Rewriting envelope MAIL FROM under SRS so SPF aligns to our domain (which authorizes our MTA).
   - Leaving the From: header and the original DKIM signature untouched on the wire so the original signature still validates at the downstream MX — and the relaxed-DMARC path through DKIM still passes for the original `alice@gmail.com`.

SRS is the canonical wire-format for that envelope rewrite, designed exactly to be undoable on bounce so we can decode and route bounces correctly.

### SRS scheme

Reference: Mengwong's original SRS spec (https://www.libsrs2.org/srs/srs.pdf, 2004 — there is no IETF RFC for SRS; the scheme is community-canonical and implemented by exim, postfix-with-srs-plugin, qmail-srs, and others).

**Forward (envelope-from rewrite at our outbound MTA):**

```
original MAIL FROM:        <alice@gmail.com>
rewritten MAIL FROM:       <SRS0=HHH=TT=gmail.com=alice@<our-primary-domain>>
```

Where:

- **`SRS0=`** — the prefix that marks a first-hop SRS-rewritten address (vs. `SRS1=` which marks an SRS-of-SRS rewrite when we forward a message that was already SRS-rewritten by a peer; we honor both).
- **`HHH`** — 4 base32 characters of `HMAC-SHA-256(srs_secret, TT || forwarder_actor_id || sender_domain || sender_localpart)[:20bits]` truncated to 4 base32 chars. The MAC covers the **full payload**, including the Fauna `forwarder_actor_id` extension field below — so an attacker who observed one `SRS0=` address cannot swap the actor-id (and misroute the bounce) without invalidating `HHH`. The MAC validates that a bounce-decode is round-tripping our own encoding (not an attacker forging an SRS0= sender to inject mail). (The actor-id is implicit in the original 2004-SRS triple because vanilla SRS has no actor-id slot; Fauna's overload of the payload, § Bounce decode step 5, requires the MAC to cover it.)
- **`TT`** — 2 base32 characters encoding the day-of-deployment-epoch modulo 1024 (TT runs through one full cycle per 1024-day window). Used at decode time to reject bounces older than ~28 days (configurable; default 28 d via `mail.forward.srs_max_bounce_age_days`).
- **`sender_domain`** — the original sender's domain, dot-replaced as needed (the address localpart can contain literal `=` characters; we URL-style escape `=` and `@` in the encoded form per the SRS spec).
- **`sender_localpart`** — the original sender's localpart.

**SRS secret** — a per-deployment random 32-byte value stored in nest, never visible to users; rotated on admin action only (knob `mail.forward.rotate_srs_secret` — action button, not a value: `admin-mail-forward-srs-secret-rotate-button` on the flat `admin-mail` page, a PROPOSED id since 2026-10-01 that waits on the user's rule-A sign-off together with its unsubscribe-secret twin, with the same two-click confirm — `mail-mass-mailing.md` § Secret rotation; the nest kind is built, no test rotates it, and no app has the button). On rotation, in-flight SRS-encoded bounces issued under the old secret are decodable until they expire (a 2-secret window is held during the rotation overlap, per the SRS-spec'd rotation pattern).

### SRS1= (chained SRS)

When a forwarded message arrives *already* SRS-rewritten by a peer (we received `SRS0=...@<peer-domain>` as MAIL FROM) and we then forward *that* downstream, we rewrite to `SRS1=...@<our-domain>` per the SRS spec § 6 chained-forwarder behavior — keeping the original peer's `SRS0=` payload as a hop-aware payload so the chain is preserved in the encoded form.

**Chained-bounce routing is stop-at-forwarder, not up-chain re-bounce.** A bounce that returns to one of our `SRS1=...@<our-domain>` addresses is routed exactly like an `SRS0=` bounce: we decode our own short-id and deliver to **our** forwarding-config owner (the actor the short-id names — see § Bounce decode below), and we do **not** decode the embedded peer `SRS0=` payload to re-forward the bounce up-chain to the peer. The preserved peer payload is for the codec and diagnostics; re-bouncing it up-chain would be exactly the original-sender backscatter the stop-at-forwarder invariant (§ NDR routing, § Architectural rules — "Bounces decode to the forwarder, not the original sender") forbids. The codec is prefix-symmetric (`SRS1=` decodes like `SRS0=`), so this is purely a routing decision: our `decode_srs_bounce` already returns *our* forwarder for an `SRS1=` recipient, and the inbound path delivers there.

### SRS on outbound — the rewrite happens nest-side at queue-out

The envelope MAIL FROM is rewritten at queue-out time, but **nest-side** — not in the Go bridge. When nest serves a due forwarded row to the MTA (`fauna.bridges.fetch_outbound_due`, per `smtp-server.md` § Outbound delivery), it applies `srs_forward` to that row's stored sender and hands the bridge the already-rewritten envelope; the bridge dispatches what nest gives it. This keeps the per-deployment SRS secret entirely inside nest (the bridge never holds it — aligning with § SRS secret "never visible to users" and the product invariant that deployment secrets live in nest state). The stored row keeps its *original* sender, so a retry re-fetch re-derives the SRS address cleanly rather than double-rewriting.

The rewrite is per-recipient: each forwarded row is one `(message, recipient)` pair and carries its own SRS short-id (the row id, § Bounce decode step 5), so each recipient of one forward gets a distinct SRS-encoded envelope whose bounce decodes back to that exact row.

### Bounce decode on inbound

Inbound to `SRS0=...@<our-primary-domain>`:

1. **Recognize** at RCPT TO time: the localpart starts with `SRS0=` or `SRS1=` and the domain is one of our `local_domains`.
2. **Decode** — split on `=` into the **five** payload fields: `HHH`, `TT`, `<forwarder-short-id>`, `sender_domain`, `sender_localpart` (the concrete encoded form is in step 5; a payload with the wrong field count fails decode outright).
3. **Verify HMAC** — recompute `HMAC-SHA-256(srs_secret, TT || <forwarder-short-id> || sender_domain || sender_localpart)[:20bits]` against the encoded `HHH` — the MAC covers the **full payload including the short-id** (§ The SRS0= scheme; `libs/fauna-mail/src/srs/`), so a swapped short-id fails here. Mismatch → reject `550 5.1.1 SRS verification failed`, counter `mail_forward_srs_decode_mac_fail_total`. **Hard-reject not tempfail** — attacker forging SRS0= addresses must not be retried.
4. **Check TT age** — current day-of-epoch minus `TT`, modulo 1024. If > `mail.forward.srs_max_bounce_age_days` (default 28 d) → reject `550 5.4.4 SRS bounce expired`, counter `mail_forward_srs_decode_expired_total`.
5. **Route** — at this point the bounce is verified as ours, originating from a legit forward. **Deliver to the forwarding-config owner's mailbox** — *not* the decoded original sender (see § NDR routing below for the rationale). The bounce's encoded SRS payload carries a **forwarder short-id** as one of the inner fields, decoded by our inbound and mapped back to the forwarding actor; peer MTAs treat the whole address as opaque and round-trip it unchanged. Concretely the encoded form is `SRS0=HHH=TT=<forwarder-short-id>=<sender-domain>=<sender-localpart>@<our-domain>`.

   **The short-id is the `outbound_mail_queue` row id** of the forwarded row, not the 32-byte actor itself. The actor as hex (64 chars) would blow the RFC 5321 64-octet local-part limit, and a bare actor-id couldn't carry the bounced *destination*; the row id is bounded and `decode_srs_bounce` looks it up to recover both `forwarder_actor_id` (the row's `forward_actor_id`) and `original_destination` (the row's recipient). The HMAC covers the short-id, so an attacker can't swap it to misroute a bounce. (This is a Fauna-specific extension within the SRS payload; the codec is otherwise standard.)

If the row the short-id names is gone (the account was deleted, or the row was pruned, between forward and bounce — an **orphan**), the bounce is logged + counter `mail_forward_srs_decode_orphan_total` and dropped (delivered to no mailbox); the alternative is delivering to the admin's mailbox, which leaks the original sender's PII to the admin.

#### One owner for the bounce-outcome vocabulary

The six `decode_srs_bounce` outcome tokens — the verified-and-routable case, the four decode failures above, and the orphan — are **one table**, `fauna_mail::srs::SrsBounceOutcome`. Nest's handler asks it for every token; the Go MTA names them through the `wsrpc.SrsBounceOutcome` const family and never spells one. The four *failure* tokens are derived from `SrsError` via `From`, so a new decode failure cannot reach the wire without its token being decided at the owner; the two success-side outcomes are unreachable from an error by construction, because only the holder of the outbound table knows whether the forwarding row survived.

The token set is the contract the bridge's RCPT-TO switch is written against, and its `default:` arm answers an unrecognised outcome with `451` — the safe direction, and also a quiet one: a token drift would tempfail **every** SRS bounce indefinitely rather than fail loudly. It is pinned across the binary boundary by `libs/fauna-mail/tests/go_wire_outcome_contract.rs` + `bins/fauna-bridges/internal/wsrpc/go_wire_outcome_contract_test.go`, the decode-direction contract described in `mail-multidomain.md` § Per-domain MTA-STS → *One owner for the MTA-STS wire vocabularies*. Priority #2/#4.

### Compile-time decisions (NOT configurable)

- SRS scheme itself (the format of `SRS0=` / `SRS1=`) — RFC-less but de facto interoperable; changing it loses interop with peer SRS forwarders.
- HMAC algorithm (HMAC-SHA-256) and truncation (20 bits → 4 base32 chars) — security floor.
- TT modulus (1024 days) — interop floor.
- The choice to overload the payload with `<forwarder-actor-short-id>` — Fauna extension; cannot be opted out of without breaking our own bounce-decode.

---

## Loop detection

Two independent floors; either one trips → forward suppressed.

### Received: chain count

Per `smtp-server.md` § Architectural rules, "The bridge MUST prepend a single canonical `Received:` header." Each hop through our forward pipeline adds one. If the message arrives with **more than 10** `Received:` headers (counting all hops, ours and others'), the forward is suppressed; the **local delivery is still completed** (loop detection kills the forward, not the local mail), counter `mail_forward_loop_received_chain_exceeded_total`, log line with `verdict=forward_suppressed_received_chain`. The 10-hop floor matches the broader email-loop convention (RFC 5321 §6.3 says SHOULD reject "after some maximum" but doesn't pin the number; 10 is what postfix defaults to under `hopcount_limit`).

### X-Fauna-Forwarded-By: header

On every forward, our outbound MTA stamps:

```
X-Fauna-Forwarded-By: actor=<forwarder-actor-id>; t=<unix-time>; rule=<rule-id|forward-all>
```

(one such header per forwarding hop through Fauna; multiple Fauna deployments forwarding the same message accumulate one header each).

On inbound, before considering the forward action: parse the inbound message's headers for `X-Fauna-Forwarded-By`. If any one of those headers's `actor=` value matches **our own current forwarding actor**, the forward is suppressed (the message has already been forwarded by us — looping it again would tornado). Counter `mail_forward_loop_self_seen_total`, log `verdict=forward_suppressed_self_seen`. Local delivery still completes.

A peer Fauna deployment's `X-Fauna-Forwarded-By` header (with a different `actor=`) does **not** suppress our forward — we forward through, and our stamp gets prepended. The chain accretes; the Received: floor (10 hops) eventually wins.

The header is **never stripped, in either direction.** Outbound: peer forwarders need to see it to participate in loop detection. Inbound: it is the **one `X-Fauna-*` header the MTA's forged-delivery-stamp strip preserves** — `strip_fauna_headers` drops every other sender-supplied `X-Fauna-*` before parse (`smtp-server.md` § Architectural rules, EF-2), but stripping this one would blind the self-seen check above. The header is informational, not policy-sensitive (no PII beyond the actor-id, which is opaque to peers).

### Loop suppression vs. delivery

Loop suppression is a per-forward decision: the local mailbox still receives the message, the forward to the configured downstream is the only thing dropped. A user with a misconfigured forward-all (forwarding to an address whose owner forwards back) sees:
- First inbound from the real sender: local-delivered + forwarded outbound to peer.
- Bounceback from peer: local-delivered + forward attempt detected as a loop → suppressed.
- Net result: 2 messages in the local INBOX (the original + the peer's forward of our forward); no tornado.

---

## Per-account forward rate-limit

Two-tier composition, owned here as semantics: a Tier-3 (per-account) visible cap — `mail.account.forward_per_hour`, default 100/hour — under a Tier-2 admin ceiling — `mail.outbound.forward_max_per_account_per_hour`, default 500/hour. The per-account cap can be set lower than the ceiling, never higher. Tier/binding/surface for both knobs are the catalog's (`mail-policy-config.md` § Tier 2 — Outbound / § Tier 3 — per-account); this doc owns the composition rule and the defaults' rationale.

On forward attempt:
- Increment a per-actor sliding-window counter (hour-window, same shape as the submission rate-limiter in `smtp-server.md` § Auth on each port).
- If current count + 1 > min(account-cap, admin-ceiling): **queue** the forward (per § Queue ceiling below) and return; the rate-limiter pops queued forwards at the next allowed interval.
- Otherwise: dispatch the forward via the outbound delivery path (`smtp-server.md` § Outbound delivery).

### Queue ceiling

A per-actor forward queue with a ceiling of `min(account-cap, admin-ceiling) * 24` messages (default: 100 × 24 = 2400 queued forwards per account). **Only a `copy` forward is ever evicted (ratified 2026-09-27).** A parked `copy` row is a second copy — the original rests in the mailbox — so above the ceiling the **earliest** queued `copy` forward is dropped — FIFO eviction among copies — and the dropped forward generates a notification to the forwarding actor (in-app, not an email): "Forward to X dropped because your forward queue is full. Configured rate: 100/hour. Reduce inbound or increase the cap." A `redirect` row — a per-rule redirect (§ Per-rule "forward to") or an admin external forwarder (§ Admin external forwarders) — is the **only** copy of mail already answered `250`, as is a row of unknown mode, so no eviction ever selects it (`principles.md` § No user-data loss). A `redirect` forward that would take the queue over the ceiling with too few `copy` rows left to make room is **refused**: `forward_message` parks nothing, evicts nothing, and returns the `fauna.bridges.forward_queue_full` error, and the MTA keeps the mail on its own — a per-rule redirect falls back to local delivery ("no forward could be enqueued", § Per-rule "forward to"), and an admin forwarder, having no local mailbox, answers `451` so the sending MTA retries. A `copy` forward arriving when no older copy is left to evict is itself the one dropped (and notified). Either way **`forward_message` never replies `queued: true` for a row it did not keep** — the same holds at a ceiling of 0, which parks nothing in either mode.

The queue lives in nest as `forward_queue(id, actor_id, queued_at, source_message_id, original_sender, destination_address, rule_id_or_forward_all, raw_message, copy_mode)`; the `id` gives FIFO order (newest-evicts-oldest) and the promotion cursor. `copy_mode` (`copy` | `redirect`) is the forward's copy mode as `forward_message` received it, persisted on the parked row and carried onto the outbound row at promotion (`outbound_mail_queue.forward_copy_mode`, set on the directly-dispatched arm too): it is the one fact that says whether a queued forward is a **second** copy or the **only** copy of accepted mail, and no class stands in for it — forward-all follows the message and is sent as `redirect` whenever a redirect rule fired (§ Per-rule "forward to"). A row queued before the column existed carries no mode and is read as *possibly the only copy*. A parked forward stores the **original** envelope (`original_sender`) plus the `raw_message`, *not* a pre-encoded SRS envelope: the SRS short-id is the `outbound_mail_queue` row id (§ Bounce decode step 5), which does not exist until the forward is promoted, so the SRS rewrite happens at queue-out **after** promotion, exactly like a directly-dispatched forward. Promotion ("pops") happens at the rate-cap cadence as a nest-internal step inside the MTA's `fetch_outbound_due` poll (see § Wire shapes — `fetch_outbound_forwards_due`), so there is no separate nest timer and forwards dispatch through the one unified outbound path. The queue survives MTA restart (nest is authoritative).

### Why queue-not-drop on cap

Forwards are usually steady-state. A spike above the cap is more often "the rule is firing on a bulk mailing list message all at once" than "the account is being hijacked." Dropping silently would lose mail; queuing absorbs the spike and lets the cap smooth it out. The queue ceiling exists for the hijack case — at 2400 queued the cap is wrong for the account's actual usage, and the admin should investigate. The FIFO eviction is the admin-tolerable failure mode (newer forwards still get delivered; oldest drop) vs. the alternative tail-drop (newest forwards never deliver, queue stalls). That trade holds only because an evicted row is a second copy: the ceiling may cost a forward, never the mail, which is why the only copy is refused instead of evicted (§ Queue ceiling).

---

## NDR routing on permanent forward failure

Per the SRS encoding above, a permanent-failure bounce arises two ways, routed differently:

- **Synchronous permfail** — the downstream MX rejecting with a 5xx at delivery, or our retry schedule exhausting after 5 d (`smtp-server.md` § Retry schedule) — is an NDR **nest generates itself**. Because we hold the forwarding actor's 32-byte id on the queue row, we **seal the DSN straight into the forwarder's INBOX** via the shared sealed-ingest path (exactly like a security notification — `smtp-server.md` § Outbound submission flow), with no MX relay. (The earlier design enqueued this NDR to the forward row's own `SRS0=...@<our-primary-domain>` envelope and looped it back through our own inbound to be SRS-decoded; that predated nest's ability to seal a self-generated message to a mailbox, and on a containerized deploy the loopback hairpins through the docker bridge and `554`-bounces at the inbound HELO-identity check.)
- **Asynchronous bounce** — the downstream MX accepting the message at SMTP time, then later emitting a DSN to our `SRS0=...@<our-primary-domain>` envelope-from — arrives at our inbound MTA addressed to an `SRS0=...` recipient, where the inbound SRS-bounce path (§ Bounce decode) decodes the short-id, verifies the HMAC, and routes to the forwarding-config owner. This is the path the SRS encoding exists for.

**The forwarder gets the bounce, NOT the original sender.** Rationale:

- **The forwarder configured the rule.** Routing bounces to the original sender would surprise them (they sent mail to `alice@gmail.com`; getting a bounce from `<forwarder>@<our-domain>` is confusing — they don't know that their mail was being forwarded).
- **The forwarder needs to know the rule is broken.** If alice's forward-all to bob@yahoo.com starts permanently failing, alice needs to know — bob isn't getting alice's mail and alice doesn't know.
- **Original-sender backscatter is the avoidable failure mode.** Re-bouncing to the original sender for a forward that the original sender didn't know about is the classic backscatter vector. Forwarders own the consequences of their own configuration.

The bounce body to the forwarder is RFC 3464 DSN-formatted (mirroring `smtp-server.md` § Permanent-failure bounce generation): `Reporting-MTA: dns; <our-domain>`, `Final-Recipient: rfc822; <downstream-address-that-bounced>`, `Action: failed`, `Status: <enhanced-status>`, `Diagnostic-Code: smtp; <downstream-wire-response>`, `Last-Attempt-Date: <iso8601>`. Plus a tail blurb: "This bounce was generated because your forward-rule '<rule-name-or-forward-all>' attempted to forward a message from <original-sender> to <downstream-address>, and the destination MX rejected it permanently. You may want to remove or fix the rule."

### Suppression matches the outbound shape

Per `smtp-server.md` § Backscatter suppression — the standard rules apply identically to forward-failure NDRs (the bounce is *to the forwarder*, but the *original message* that produced the forward may itself have been backscatter-suppress-eligible; we don't generate a bounce of a bounce). Concretely: if the original inbound was a bounce (`MAIL FROM: <>`), we don't generate an NDR on the forward's failure — the forward should not have happened (forwarding null-sender bounces is itself the backscatter problem); see § Don't do these.

### NDR rate-limit

Same 7-day window per `(forwarding-actor, source-message-id)` per `smtp-server.md` § NDR rate-limit per recipient. A user with a chronically-broken forward rule sees one bounce per source message in any 7-day window; later attempts log + counter only.

---

## Storage-mode interaction

Forwarding is an **MTA-perimeter decision**, happening on plaintext bytes the MTA already holds in-process during the inbound DATA stage — one posture, unconditional (the storage-mode axis is retired; `../architecture/nest/storage-modes.md` owns the retirement). The MTA's post-classification stage (per `smtp-server.md` § Spam handling — the "Obligation rules" stage that evaluates user filter rules) runs against header + envelope + spam-score data — the plaintext-floor metadata the MTA already holds before sealing. A fired rule may forward via `fauna.bridges.forward_message(actor_id, source_envelope, source_body, destination, copy_mode, rule_id_or_forward_all)`; nest persists into `forward_queue` and emits the outbound message via the standard outbound delivery path (SRS-rewriting at queue-out time). The outbound forwarded copy is sent **plaintext to the downstream MX** (the downstream's own encryption is their own problem; we cannot seal under the downstream's keys). The local copy of the source message rests sealed at ingest (`encryption-at-rest.md` § Readable classes — mail is sealed to the recipient's standing MSEK-derived keypair, uniformly).

### Where the forward config lives at rest

The forward *decision* runs at the MTA perimeter (above); this section answers where the **config that drives it** — `forward_all_to`, the per-rule `forward` destinations, and the admin forwarders — rests.

**All forward config rests at the plaintext routing-metadata floor** — `forward_all_to` and the per-rule `forward` destinations alongside admin forwarders (`account_aliases` `kind='forwarder'`), the same tier as aliases, the catch-all designation, and `local_domains` (`mail-aliases.md` § Kind 7; `encryption-at-rest.md` § Plaintext floor). The MTA reads them directly to make the perimeter forward decision; nothing seals them.

**Why user forward config is *not* sealed (and the planned upgrade).** "Alice forwards to alice@gmail.com" is a private fact, and an earlier design sealed user forward config **wrapped to the MTA bridge service-user pubkey** (the `TlsCertBlob` class). That seal protects **only** against theft of the *nest* disk in a deployment where the **bridge is a separate trust domain** — its decode key (the bridge's x25519 private key) not on the nest disk (`encryption-at-rest.md` § Trust property under compromise). In the common **single-box** deployment the bridge's key is co-resident with the nest's at-rest store, so a full-disk theft defeats the seal exactly as it would the deployment DKIM/TLS keys — co-resident-theater there. Because the protection is real only for separate-bridge deployments, and the sealed form is costly (the bridge must read the config while the user is offline, so it cannot ride the user's sealed account-state plane — which no nest or bridge can open; it would need client-side HPKE-wrapping to each approved bridge, a typed `ForwardConfigBlob`, and a re-wrap-on-new-bridge lifecycle), user forward config rests at the plaintext floor for now.

**The separate-bridge-sealed form is a planned, additive upgrade** (tracked internally, § N1b), not a discarded idea: it adds a `provision_forward_config_blob` path + a `bridge_forward_config_blobs` fan-out table *alongside* the plaintext column, reclassifies user forward config to "sealed" here and in `encryption-at-rest.md`, and one-time-migrates existing values. The plaintext column + the `get/set_forward_all_to` RPCs stay regardless, so a separate-bridge deployment can opt in to the sealed form without a wire break. The N2 delivery trigger must obtain `forward_all_to` through a single chokepoint so that swap stays one-spot.

**Body-content matching rules** (filter rules that scan the message body — now in the v1 rule-condition set per `email-filters.md` § Email filter rules: a case-insensitive substring over the decoded `text/plain` + `text/html`) run at the same MTA perimeter, before sealing, on the plaintext the MTA already holds in-process. No body-content rule requires post-sealing plaintext access. (The full Sieve `body` extension and the ManageSieve protocol stay deferred per imap-server.md § Upstream-blocked gaps; v1 is the simple-substring subset.)

---

## Wire shapes (named, not redefined here)

| RPC | Caller | Purpose | Notes |
|---|---|---|---|
| `fauna.bridges.fetch_recipient_forward_config` | MTA | per-recipient forward config at the perimeter | `(actor_id)` → `{forward_all_to}` (per-rule destinations + rate cap join it as N3/N5 land). The MTA's single chokepoint for reading forward config — the one spot the N1b sealed-fetch upgrade swaps. |
| `fauna.bridges.forward_message` | MTA | enqueue a forward of an inbound message | `(actor_id, source_envelope, source_body, destination, copy_mode, rule_id_or_forward_all)`; nest enqueues one `outbound_mail_queue` row (`is_forwarded=1` + forwarder actor/rule). The dedicated `forward_queue` rate-window/eviction state of § Queue ceiling is the N5 rate-cap. |
| `fauna.bridges.fetch_outbound_forwards_due` | MTA | poll for queued forwards ready to dispatch | **Realized as a nest-internal promotion step folded into `fetch_outbound_due`, not a separate wire kind** (N5): forwards dispatch through the unified `outbound_mail_queue` path (the N2 reuse decision), so each poll first promotes parked `forward_queue` rows up to each actor's remaining hourly allowance into `outbound_mail_queue`, then returns due rows. nest applies the per-actor rate-cap window during that promotion. |
| `fauna.bridges.decode_srs_bounce` | MTA | decode + verify a `SRS0=` / `SRS1=` inbound recipient | `(local_part)` → `(forwarder_actor_id, original_sender, original_destination)` or decode-fail-reason |
| `fauna.bridges.forward_completed` | MTA | confirm a forward was delivered (or permanently failed) | **Subsumed by the existing outbound completion RPCs, not a separate kind:** a forward is an `outbound_mail_queue` row (`is_forwarded = 1`), so the MTA's normal `mark_outbound_{delivered,failed,bounced}` already drive completion, and the NDR decision (§ NDR routing) fires in `mark_outbound_bounced` / give-up (N4b). |
| `fauna.bridges.rotate_srs_secret` | admin client | issue a new SRS secret (2-secret window) | (action button — no app UI today; its home on the flat `admin-mail` page and its PROPOSED id are in § SRS scheme) |

Wire-level shape lives in the nest implementation track (`docs/goal/architecture/apps/bridges.md` § Bridge-kind catalogue (caller classes)); this doc owns which RPCs exist and what each carries.

---

## Architectural rules

- **Every forward has an attributed actor.** Forward-all attribution = the account holder; per-rule attribution = the rule's owner (same actor as the account holder; the rule is in the account's filter set); admin-external-forwarder attribution = the admin actor (`account_aliases.actor_id` on the `kind='forwarder'` row). Anonymous forwards (no actor) are forbidden — there is no relay-on-behalf-of-no-one path. The admin forwarder is the apparent exception that proves the rule: it is attributed to the admin, an authenticated principal, not to the original sender or to no one.
- **SRS rewrites envelope MAIL FROM; never the From: header or the body.** The original DKIM signature must survive the forward intact, so DMARC alignment via DKIM can carry the message through at the downstream MX. Stripping or rewriting From: would break DMARC alignment for the original sender and is the wrong shape.
- **SRS HMAC is required, not optional.** A forward without SRS rewriting fails SPF at the downstream MX → undeliverable. An SRS rewrite without HMAC is forgeable by an attacker → backscatter injection. Both floors are non-negotiable.
- **Bounces decode to the forwarder, not the original sender.** The forwarder configured the rule; they receive the consequence. Re-bouncing to the original sender is the classic backscatter pattern we suppress per `smtp-server.md` § Backscatter suppression — forwarders are the exception that doesn't get rebounded because *they* are the principal.
- **Loop detection is a per-forward decision, not a per-message decision.** Local delivery completes even when the forward is suppressed. Failing to deliver the local copy because we couldn't forward would surprise users (their Fauna mailbox is the canonical store, not the forward destination). A `redirect` rule inverts the order, not the rule: its forwards are attempted first, and a suppressed one is what puts the message in the local mailbox after all (§ Per-rule "forward to").
- **Forwards consume submission quota AND forward quota.** The two are composed (the nest-side per-actor recipients/day quota, `smtp-server.md` § Architectural rules, AND `mail.account.forward_per_hour`); a forward that would exceed either gets queued. A user submitting normal outbound and also being forward-active hits whichever cap binds first.
- **Forward queue is FIFO with newest-evicts-oldest at ceiling — among `copy` rows only.** Tail-drop (newest stops getting through) is admin-confusing — "my mail stopped forwarding starting at noon"; FIFO eviction ("my mail stopped forwarding in chronological order, and I got a notification") is the admin-tolerable failure mode. A `redirect` (or mode-less) row is the only copy of accepted mail and is never evicted; an over-ceiling `redirect` is refused so the MTA keeps the mail (§ Queue ceiling).
- **Body-content matching runs at the perimeter, pre-seal.** Per `email-filters.md` § Email filter rules, v1 body matching is a case-insensitive substring over the decoded `text/plain` + `text/html` the MTA holds before sealing — so a forward rule may match on the body alongside envelope + headers + spam-score, with no post-seal ciphertext access. (The full Sieve `body` extension and the ManageSieve protocol stay deferred.)
- **Forward attempts don't run on null-sender messages.** A message with `MAIL FROM: <>` is a bounce; forwarding bounces is the backscatter problem. Filter rules that would fire on a null-sender message have their `forward` actions silently skipped (per-account `mail.account.forward_null_sender_attempts_skipped_total` counter); other actions on the same rule (move-to-folder, etc.) still fire.
- **The SRS secret rotates only on admin action.** A periodic auto-rotation would orphan all in-flight bounces older than the rotation window, causing the SRS decode-orphan counter to spike on every rotation. The 2-secret overlap on admin-triggered rotation absorbs in-flight bounces; that's the right shape.
- **X-Fauna-Forwarded-By is informational, never stripped.** Peer Fauna deployments and any peer MTA's loop detector see the header; stripping would break the cooperative loop-detection floor.
- **Forwarding sends plaintext to the downstream MX.** We cannot seal under the downstream's keys. The local stored copy rests sealed; the wire copy is plaintext (downstream's encryption is their problem).
- **Permanent-failure NDR rate-limit composes with the standard outbound NDR floor.** Per `smtp-server.md` § NDR rate-limit per recipient — one bounce per `(forwarder, source-msg-id)` per 7 days.

## Don't do these

- Don't forward without SRS rewriting. The downstream MX's SPF check fails → message discarded silently. SPF-aware MTAs are 99%+ of inbound; "save a few bytes of envelope" is not worth the loss.
- Don't strip or rewrite the From: header on forward. DMARC alignment via DKIM survives only if the original signed headers are intact; rewriting From: breaks it; the downstream then sees DMARC fail + has no relaxed-alignment fallback → reject. Rewrite the envelope only.
- Don't deliver bounces to the original sender. Backscatter — the original sender doesn't know about the forward; sending them a bounce of a message they didn't realize was being relayed is amplification.
- Don't deliver SRS-decode-orphan bounces to the admin mailbox. The decoded payload carries the original sender's address as PII; landing it in the admin's INBOX (because the forwarder-account was deleted) leaks that PII to the admin. Drop with a counter; admin sees the counter, not the PII.
- Don't auto-rotate the SRS secret on a timer. Periodic rotation orphans in-flight bounces every cycle; admin-action-only rotation with the 2-secret overlap is the right shape.
- Don't allow forward-all to point at a domain we host (`local_domains`). That's a same-deployment forward; the right shape is the user *adding an alias*, not configuring a forward. The UI rejects the address at write-time; nest re-validates.
- Don't run forwards before local delivery. Local delivery commits first; the forward is a side-effect that fires only after the local copy is durable. An MTA crash mid-flight leaves the local copy intact; the user's primary record is their Fauna mailbox. The one shape that has no local copy to order after is a `redirect` rule, where the forward's durable enqueue is itself the commit and local delivery is the fallback when no forward could be enqueued (§ Per-rule "forward to") — never a silent drop.
- Don't forward bounces (null-sender messages). The original bounce target is the original sender's MTA; forwarding the bounce somewhere else amplifies backscatter, doesn't help the user, and confuses the original sender's MTA's NDR-suppression heuristics.
- Don't send a forward-failure notification by email to the forwarding actor — they already get the RFC 3464 DSN in their Fauna INBOX. A separate in-app notification + log line is appropriate; an extra email is duplicative noise.
- Don't build a rule evaluator that runs on the stored ciphertext. The rule eval is at the MTA perimeter (pre-sealing); reading sealed content to evaluate a filter rule is the wrong shape and would break the sealed-at-rest invariant (`encryption-at-rest.md` § The read-position purpose test).
- Don't surface the SRS secret in the admin UI. The admin can rotate the secret (action button) but never reads its bytes. Knowing the secret would let the admin forge SRS-encoded bounce addresses for arbitrary inbound mail to their own users.

## Implementation status today

The forwarding pipeline is being built shared-Rust-first
(tracked internally). What exists vs. what is still target-state:

**Landed — the shared SRS codec (tracked internally, § R1 (account-data-plane.md § The ratified decisions)):**

- `fauna_mail::srs` (`libs/fauna-mail/src/srs/mod.rs`, `srs` cargo feature, in `default`) is the pure,
  WASM-safe SRS encode/decode: `srs_forward(secret, local_domain, now_day, forwarder_actor_id,
  original_mail_from)` and `srs_decode(secret, now_day, max_bounce_age_days, local_part) -> SrsDecoded
  { forwarder_actor_id, original_sender }`, with `SrsError::{NotSrs, Malformed, MacFail, Expired}`
  mapping to the `550 5.1.1` / `550 5.4.4` replies of § Bounce decode. SRS0 + the chained SRS1 prefix
  both round-trip (the codec is prefix-symmetric; chained-bounce *routing* is N4, not the codec).
- **Wire details this codec fixes** (the doc delegated wire-format to "the consuming-track's commit",
  § Status): the HMAC input is the **full payload** `TT || forwarder_actor_id || sender_domain ||
  sender_localpart` (§ SRS scheme `HHH` now reflects this); the `HHH`/`TT` base32 alphabet is RFC 4648
  upper-case no-pad (an internal choice — peers round-trip the address opaquely, so it is not an interop
  surface); payload fields percent-escape `=`/`@`/`%` (and non-ASCII) so the `=`-split decode survives a
  localpart containing a literal `=`.

**Landed — per-account forward-all storage + config RPC (tracked internally, § N1):**

- `mail_account_settings(actor_id, forward_all_to, forward_per_hour, updated_at)` — a per-actor table
  (sibling of `spam_preferences`) holding `forward_all_to` at the plaintext routing-metadata floor
  (§ Where the forward config lives at rest), and `forward_per_hour`, the user's own hourly cap the N5
  rate-cap reads (write path below).
- `fauna.bridges.{get,set}_forward_all_to` (User-class WS-RPC) read/write it; shared client wrapper
  `fauna_client_bridges::MailAccountClient::{get,set}_forward_all_to` (priority #2). `set`
  RFC-5321-validates the address (`fauna_mail::validate_forward_target`) and rejects one pointing at a
  hosted `local_domains` address with its own code, `fauna.bridges.forward_target_on_local_domain`, whose
  localized sentence says to add an alias instead (§ Don't do these) — an app renders a refusal's code,
  never its `details`, so the generic `malformed` could not carry that remedy; a blank value clears (disables).
  No storage-mode branch — the plaintext floor, uniformly.
- `fauna.bridges.{get,set}_forward_per_hour` (User-class WS-RPC, 2026-09-25) read/write the caller's
  hourly cap; `get` also returns the ceiling it may not pass, so an app bounds its field without knowing
  the admin tier. `set` refuses (`malformed`) a value outside `1..=ceiling` via the shared
  `fauna_mail::validate_forward_per_hour` — zero would park every forward until eviction; above the
  ceiling is what § Per-account forward rate-limit forbids. There is no clear: the setting always has a
  value (default 100). Shared wrapper `fauna_client_bridges::MailAccountClient::{get,set}_forward_per_hour`.

**Landed — forward delivery trigger, full N2 (nest half + Go MTA stage; tracked internally, § N2):**

- **The forward decision lives at the MTA perimeter, not a nest hook.** The nest stores only the
  recipient-sealed `encrypted_body` (it cannot decrypt it), so it cannot produce the plaintext copy a
  downstream MX needs — a hook in `persist_inbound_mail_request` is infeasible. Only the Go MTA holds the
  plaintext during DATA. This is the § Trigger point / § Storage-mode interaction shape.
- `fauna.bridges.fetch_recipient_forward_config` (BridgeMta) — the MTA's single per-recipient chokepoint
  for reading `forward_all_to` at the perimeter (one spot the N1b sealed-fetch upgrade swaps).
- `fauna.bridges.forward_message` (BridgeMta) — enqueues one `outbound_mail_queue` row (`is_forwarded = 1`,
  carrying the forwarder actor + rule in new `forward_actor_id` / `forward_rule_id` columns) for dispatch
  via the existing outbound path. The dedicated `forward_queue` rate-window/eviction table of § Queue
  ceiling is the N5 rate-cap — N2 enqueues straight to `outbound_mail_queue` with **no rate-cap yet**.
- **Go MTA forward stage (N2-go):** after each per-recipient ingest commits, the MTA fetches the
  recipient's forward config and, for a non-null-sender message that passes the two loop floors
  (> 10 `Received:` hops, or our own `X-Fauna-Forwarded-By` already present — both suppress the forward
  while local delivery still completes), stamps `X-Fauna-Forwarded-By` and calls `forward_message`
  (`copy_mode = copy`, rule `forward-all`). The R2 loop helpers are shared Rust (`fauna_mail::forward_loop`)
  crossed to Go via UniFFI, so nest, apps, and the bridge agree on one spelling. So forward-all now
  fires end-to-end: a real inbound to a recipient with `forward_all_to` set enqueues an `is_forwarded`
  outbound row (tier_3 e2e).
**Landed — SRS envelope rewrite + secret storage + bounce decode (tracked internally, § N3):**

- **The SRS secret lives in nest state.** A `mail_srs_secrets` table holds the per-deployment 32-byte
  HMAC key, auto-seeded with one row on first DB open (the admin only *rotates* it via N5, never sets or
  reads it — § Don't do these). The legacy `--email-srs-key` CLI arg was deleted with the
  `legacy_in_nest_smtp` cargo feature at the I6 cutover; nest-state is the sole source.
- **The envelope rewrite runs nest-side at `fetch_outbound_due`** (§ SRS on outbound). Forwarded rows
  (`is_forwarded` + `forward_actor_id`) get their MAIL FROM replaced by `srs_forward(secret,
  primary_domain, now_day, <row-id short-id>, original_sender)` in the `OutboundUnit` nest hands the
  bridge; normal rows are untouched, and the stored row keeps its original sender. So forward-all now
  leaves the box SPF-aligned to our domain (tier_3 e2e: a forwarded message reaches the stub MX under an
  `SRS0=…@<primary-domain>` envelope).
- `fauna.bridges.decode_srs_bounce(local_part)` (BridgeMta) decodes + verifies an inbound `SRS0=`/`SRS1=`
  recipient, trying every stored secret (2-secret rotation overlap, N5), and maps the short-id (the
  outbound row id) back to `forwarder_actor_id` + `original_destination` for N4's NDR routing. Outcomes:
  `ok` / `not_srs` / `malformed` / `mac_fail` (`550 5.1.1`) / `expired` (`550 5.4.4`) / `orphan` (verified
  but the row is gone → drop + counter, never the admin mailbox).

**Landed — inbound async bounce → forwarder (tracked internally, § N4a):**

- **The Go MTA recognizes an inbound `SRS0=`/`SRS1=` bounce at RCPT-TO** (`internal/mta/server.go`): an SRS
  local-part on a local domain is decoded via `decode_srs_bounce` instead of the normal `resolve_recipient`
  resolver. `ok` →
  accept and deliver the bounce to the **forwarder's** mailbox (the existing per-recipient ingest path
  seals it to the forwarder + places it — § Bounce decode step 5); `orphan` → accept then silently drop at
  DATA (no mailbox, never the admin's); `mac_fail` → `550 5.1.1`; `expired` → `550 5.4.4`; `malformed` →
  `550`; `not_srs` → fall through to the normal recipient path. Outcomes are metered
  (`smtp_inbound_srs_decode_total{outcome}`). A delivered bounce is never re-forwarded (the forward-all
  stage is skipped for an SRS-bounce recipient). This is the **asynchronous** bounce path (the downstream
  MX accepted the forward, then bounced to our SRS0 envelope); tier_3 e2e drives the full round-trip
  (forward dispatched under SRS → bounce to the captured `SRS0=…@<domain>` → lands in the forwarder's
  INBOX; a forged MAC → `550`).
- **Forward-enqueue worker nudge:** the inbound forward-all stage pokes the outbound worker after
  enqueueing (like the submission Data hook), so an inbound forward delivers on the next poll rather than
  after a full `PollInterval`.

**Landed — synchronous-permfail bounce → forwarder (tracked internally, § N4b):**

- **When *our* outbound worker permanently fails to deliver a forward** (a 5xx during delivery, or
  retry-budget exhaustion → `mark_outbound_bounced`), nest now routes the DSN to the **forwarder**, not the
  original sender. `outbound_bounce::generate_permfail_bounce` branches on `is_forwarded` +
  `forward_actor_id` *before* the to-sender path; the `ForwardedAndExternallyBounced` suppressor — which
  governs only the suppressed bounce *to the original sender* — no longer swallows it.
- **The DSN reaches the forwarder by sealing it straight into the forwarder's INBOX** (§ NDR routing,
  synchronous-permfail case), not via an MX loopback. The forwarder is always a known in-domain actor (we
  hold its 32-byte id on the queue row), so `generate_forwarder_ndr` seals the DSN to it through the shared
  sealed-ingest path (`seal_and_ingest_local`) — exactly like a security notification, no MX relay. The
  DSN's `To:` header is the forwarder's handle address (falling back to `postmaster@<domain>`); the seal
  target is the actor id regardless, so a handle-less forwarder still receives it. The DSN carries the Fauna
  tail blurb (rule + original sender + downstream) and is rate-limited one per `(forwarder, source-msgid)`
  per 7 days. A null source-sender forward suppresses the NDR (don't bounce a bounce, `:207`); a deployment
  missing the primary mail domain, or a forwarder with no encryption key on file (the seal fails), marks the
  row bounced *without* a DSN rather than leak the bounce to the original sender. (The older design
  re-derived the forward row's own `SRS0=…@<primary-domain>` envelope and enqueued a null-sender DSN that
  looped back through our own MX to be SRS-decoded by the inbound path; it predated `seal_and_ingest_local`,
  and on a containerized deploy the loopback hairpined + `554`-bounced at the inbound HELO-identity check.
  The inbound SRS-bounce path, N4a, is retained for the **asynchronous** case — the downstream MX accepting,
  then later DSN-ing back to our `SRS0=` envelope.)

**Landed — per-account rate-cap + queue + SRS rotation (tracked internally, § N5):**

- **The per-account forward rate-cap is enforced nest-side at `forward_message`** (§ Per-account forward
  rate-limit). The effective cap is `min(forward_per_hour, mail.outbound.forward_max_per_account_per_hour)`
  — the per-account knob (`mail_account_settings.forward_per_hour`, default 100) capped by the admin
  ceiling (default 500). Both defaults are consts in `fauna_mail::forward_config`
  (`FORWARD_PER_HOUR_DEFAULT`, `FORWARD_MAX_PER_ACCOUNT_PER_HOUR_CEILING`) until the policy write-path track
  wires the admin knobs, like the alias-knob defaults. A per-actor sliding window counts the actor's
  `is_forwarded` rows enqueued in the last hour; a forward over the cap is **parked**, not dropped.
- **The daily recipients quota composes with it at both dispatch points** (§ Architectural rules). A
  forward the hourly cap would dispatch draws one unit of the forwarding actor's `bridge_submission_quota`
  day counter (`try_consume_submission_quota` — the counter both submission doors debit; a forward always
  leaves through the outbound queue, so it is a remote recipient under `smtp-server.md`'s charging rule).
  A forward the day's allowance cannot cover is **parked** exactly like an over-cap one and charges
  nothing; promotion draws the same unit per promoted row and stops at the day's allowance, so parked
  forwards resume once a new day opens it (`bridge_routing_handlers.rs::draw_forward_daily_unit`, called
  from `forward_message` and `promote_due_forwards`). Until 2026-09-27 no forward drew on the counter.
- **`forward_queue` is the holding table** (`bins/fauna-nest/src/db/forward_queue.rs`): a parked forward
  stores the original envelope + `raw_message` (SRS at queue-out post-promotion, § Queue ceiling). The
  ceiling is `min(cap, ceiling) * 24` (default 2400); over it the **oldest** parked `copy` forwards are
  FIFO-evicted and the forwarder gets an **in-app** notification (`mail.forward_queue_evicted`, never an
  email — § Don't do these). A `redirect` or mode-less row is never evicted: `enqueue_forward_queue`
  refuses a `redirect` it has no copy to make room for (one transaction, nothing written), and
  `forward_message` answers every unparked forward with `fauna.bridges.forward_queue_full` instead of a
  `queued` reply. The MTA's per-rule redirect falls back to local delivery on it; the admin forwarder
  dispatches ahead of the envelope's local recipients and answers `451` (`internal/mta/server.go`,
  `internal/mta/submission.go`). Parked forwards are promoted into `outbound_mail_queue` up to each actor's
  remaining hourly allowance by a nest-internal step folded into `fetch_outbound_due` (no separate timer or
  RPC — § Wire shapes).
- **The copy mode is persisted on every queued forward (2026-09-27)** — `forward_queue.copy_mode` and
  `outbound_mail_queue.forward_copy_mode`, written by `forward_message` on both arms and carried through
  promotion (§ Queue ceiling). An account succession reads it: a queued `copy` forward is burned whatever
  rule armed it, a `redirect` or mode-less one is carried to the successor (the registry rulings on
  `forward_queue` / `outbound_mail_queue` in `bins/fauna-nest/src/db/actor_tables.rs`). The queue
  ceiling's eviction reads it too: only `copy` rows are evicted (above).
- **`fauna.bridges.rotate_srs_secret` (Admin)** mints a fresh random 32-byte SRS secret nest-side and prunes
  `mail_srs_secrets` to the newest two (the 2-secret rotation overlap — § SRS secret). The admin triggers
  the action but never supplies or reads the bytes (§ Don't do these); `decode_srs_bounce` already tries
  every retained secret so an in-flight bounce minted under the prior secret still verifies.

**Not yet built (target-state above; tracked internally):**

- **The per-account forward-all and hourly-limit app UI is built on tui only (2026-09-27).** tui's
  `mail-settings` page carries the Forwarding section — `mail-settings-forward-all-to-input` and
  `mail-settings-forward-per-hour-input` (ui.yaml, allocated with this track), each committed on Enter
  over `MailAccountClient::set_forward_all_to_and_reload` / `set_forward_per_hour_and_reload` and
  shown while mail is enabled. The drafts are checked first by the shared
  `fauna_client_mail_settings::{parse_forward_all_to_draft, parse_forward_per_hour_draft}` (the same
  `fauna_mail` validators the nest runs, rendered as `LocalizedText`), bounded by the ceiling the read
  returns. The other six apps have neither field yet — the batched trickle-down, which also lifts the
  parsers and the two `_and_reload` calls across FFI/WASM (the `spam_threshold_override` pair is the
  precedent).
- **The per-rule `forward` dispatch arm is WIRED (2026-09-24), in both copy modes (redirect 2026-09-25).** Each fired
  Forward is stashed during composition and, after the recipient's local delivery commits, dispatched
  through the shared `dispatchForward` (`bins/fauna-bridges/internal/mta/server.go` `dispatchFilterForwards`) attributed to
  the rule's owner and stamped `rule=<filter-id>` — so the SRS / loop / null-sender / rate-cap floors
  apply exactly as for forward-all, and one message matching several forward rules forwards once per
  rule. The rule's destination is syntax-checked at create/update with the shared
  `fauna_mail::validate_forward_target` (no hosted-domain bar — that is forward-all's). **The
  `redirect` copy mode is built end to end** per § Per-rule "forward to": the additive
  `EmailFilterAction::Forward { address, redirect }` wire field (mirrored on `fauna_mail::filter::FilterAction`
  and the FFI enum), the nest's `email_filters.forward_redirect` column beside the unchanged
  `forward:<address>` string (`email_handlers::action_to_storage` / `action_from_storage`), the Go decode
  (`internal/mailfauna/filter.go`), and the MTA's per-recipient placement skip with its enqueue-first /
  local-fallback order (`server.go`, the `redirectRecipient` branch of the DATA stage) — witnessed by
  `tests/e2e-unified/tests/test_mail_bridge_filter_delivery.py::test_forward_rule_redirect_forwards_without_local_copy`
  (tier_3, `mail-filter-rules` outcome 17) and the `TestFilterForwardRedirect*` tier_1 Go tests. **The rule
  editor offers the Forward action on tui (2026-09-28):** the destination field and the "keep a local copy"
  checkbox go through the shared `FilterActionInputs` seam, which validates the destination with
  `validate_forward_target(address, &[])` and round-trips the copy mode through an edit — witnessed by
  `tests/e2e-unified/tests/test_settings.py::test_email_filter_forward_keeps_its_copy_mode_through_an_edit`
  (`mail-filter-rules` outcomes 4, 7, 8). **linux and web carry the same form (2026-10-02)** — linux calls the
  seam directly, web through the `encodeEmailFilterActionInputs` / `describeEmailFilterActionInputs` /
  `filterIsEditableFor` wasm exports, both witnessed by the same test. **android, macos, ios and windows
  carry the same form (2026-10-02)** through the UniFFI exports of the same seam
  (`encode_email_filter_action_inputs` / `describe_email_filter_action_inputs` /
  `email_filter_is_editable_for`), so every app offers Forward and opens a stored one. The paragraph below
  is the pre-wiring record.
- *(Pre-wiring record.)* The per-rule `forward` dispatch arm was the one unwired piece of the (live) filter evaluator. The
  perimeter filter-rule evaluator runs end-to-end at the MTA (`internal/mta/server.go` — fired
  FileInto / AddLabel / Discard / Reject / AutoReply actions are applied at delivery; `email-filters.md`
  § Email filter rules records perimeter execution wired, S3+S4a). `EmailFilterAction::Forward { address }`
  exists on the wire (`libs/fauna-protocol/src/email.rs`) and the evaluator recognizes a fired Forward,
  but the Go MTA's action loop only **logs** it (`verdict=filter_unwired`) instead of dispatching. The
  remaining slice: route a fired Forward into the same shared `dispatchForward` /
  `fauna.bridges.forward_message` pipeline that forward-all and admin forwarders already use, honoring
  the rule's `redirect` copy-mode (suppress local placement) and stamping the rule id into
  `X-Fauna-Forwarded-By` (`rule=<rule-id>` — § Loop detection). Forward-all works end-to-end; the
  per-rule shape waits only on this dispatch arm.
- **Admin external forwarders** (§ Admin external forwarders; `mail-aliases.md` § Kind 7) — the **alias-half
  is built** (tracked internally, § AF): `kind='forwarder'` storage (`forward_target` column +
  partial index), the Admin-class `fauna.bridges.{create_forwarder,list_forwarders,delete_forwarder}` RPCs (+ `MailAdminClient`
  wrapper), and the `Forward {forward_target, forwarder_actor_id}` outcome on `resolve_recipient` (resolver
  step 2, exact-key tier, attributed to the managing admin). The **dispatch** is the shared SRS / loop /
  rate-cap / NDR pipeline this doc owns (N3–N5, landed); the resolver hands the forward to it. The **Go-MTA
  RCPT-TO cutover** (`validate_recipient`→`resolve_recipient`) is **COMPLETE on both call sites** (inbound MX,
  Slice 3; submission, Slice 4 — 2026-06-01): the prod MTA now consumes the `Forward` outcome and dispatches
  admin forwarders through the shared `dispatchForward` (copy_mode=redirect) on both the inbound and the
  submission paths (a local user emailing a forwarder address reaches the external target uniformly).
  `validate_recipient` is retained as the AUTH-time exact resolver (`mail-aliases.md` § A2.2). The
  **separate-bridge-sealed form of user forward config** (§ Where the forward
  config lives at rest — wrapped-to-bridge, the `TlsCertBlob` class) is the deferred **N1b** upgrade
  (user forward config rests at the plaintext floor today, by decision; the sealed form is additive). No
  `bridge_forward_config_blobs` table / `ForwardConfigBlob` / `provision_forward_config_blob` yet. Tracked
  internally (admin-forwarder + sealing slices). The shared dispatch the admin shape reuses is the same N3–N5 work.

## Reading list

1. `principles.md` § Product invariants — the "nest config from apps" rule the per-account forward-all + per-rule forward-to knobs implement; the "user always controls their data" rule that puts a *user's* forwarding configuration in the user's own app, not in admin-edited rules (admin external forwarders, § Admin external forwarders, are the separate admin-owned shape for mailbox-less addresses that no user owns).
2. `docs/goal/behavior/email-filters.md` § Email filter rules — the `forward` action this doc gives mechanics to; § Outbound delivery — the queue / retry / bounce shape forwards reuse; § Backscatter suppression — the rule forward-NDRs compose with; § Architectural rules — the canonical Received: + the strict-beats-permissive default.
3. `docs/goal/behavior/mail-policy-config.md` § Tier 2 — Outbound (forward rate-limit admin ceiling) and § Tier 3 — Account (per-account forward-all + per-account forward-rate visible cap); § Architectural rules ("new knob = catalog entry + ui.yaml entry, same commit").
4. `docs/goal/behavior/bridges.md` § Bridge settings — the metadata-driven detail-page shell the per-account forward-all knob renders inside.
5. `docs/goal/behavior/dmarc-reporting.md` — the symmetric report-pipeline shape; the DKIM-survival-through-forward floor matters because DMARC pass via DKIM is the only path that survives a forward.
6. (design ratified 2026-05-07; tracked internally) § MTA encrypts at perimeter — the pre-sealing rule-eval timing forwards rely on; § Bridge↔nest WS-RPC API — the wire-level shape the new RPCs align with.
7. Mengwong's SRS spec (https://www.libsrs2.org/srs/srs.pdf, 2004 — no IETF RFC; the canonical scheme); RFC 7208 (SPF — why the envelope rewrite is needed); RFC 7489 § alignment (DMARC — why the From: header preserve is required); RFC 5321 §6.3 (the loop-count floor); RFC 3464 (DSN — the NDR-to-forwarder format).
