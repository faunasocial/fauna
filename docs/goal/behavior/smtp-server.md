# SMTP server — target state

Owns: smtp-server, mta, smtp-submission, spf, arc-authentication, greylisting, role-address-routing, tlsrpt
Status: ratified
Authority: the SMTP entry point — inbound MX perimeter (policy-stack ordering, HELO/FCrDNS/DNSBL/greylist, error/tempfail strategy, connection-time limits, TLS posture per port), the 465/587 submission surface (AUTH set + lockout, RCPT-time recipient handling, per-actor quotas), outbound delivery (queue lifecycle, retry curve, MX resolution, outbound MTA-STS/DANE enforcement, NDR + backscatter suppression, TLSRPT both directions), the SmtpVerdict log + Prometheus metrics contracts, and end-to-end inbound pipeline ordering; defers scorer placement to architecture/content-scoring.md, per-scorer config to behavior/mail-content-scanning.md + behavior/mail-spam.md, the filter engine to behavior/email-filters.md (and its forward action to behavior/mail-forwarding.md), message-size ceilings to behavior/mail-message-size.md, the first-party client send/receive RPCs to behavior/mail-app-surface.md, alias resolution to behavior/mail-aliases.md, IMAP server semantics to behavior/imap-server.md, process lifecycle to behavior/mail-bridge-lifecycle.md, the knob catalog to behavior/mail-policy-config.md, the published per-domain MTA-STS policy to behavior/mail-multidomain.md, and the cert-honesty coupling rule to architecture/nest/tls-certificates.md § D. `tests/e2e-unified/ui.yaml` owns admin-UI element IDs; the internal SMTP security design plan (tracked internally) owns ordering and slice scope.

---

## Section map

The perimeter and delivery story, in reading order. Three concepts split out to their own
owner docs on 2026-08-03 (this doc was 226K); each keeps a stub at its original location.

| Section | What it answers |
|---|---|
| § Goal → § Inbound policy stack | What the SMTP entry point is for, and the order checks run in |
| § Inbound perimeter hardening | rDNS/FCrDNS, HELO validation, greylisting, TLSRPT listener, role addresses |
| § TLS posture per port → § Auth on each port | Per-port TLS and AUTH sets |
| § Recipient handling on submission | RCPT-time resolution and per-actor quotas |
| § Connection-time limits | Caps, timeouts, tarpit (**message-size ceilings → `mail-message-size.md`**) |
| § Outbound delivery | Queue lifecycle, retry curve, MX resolution, NDR, outbound TLSRPT |
| § Log shape → § Metrics surface | The SmtpVerdict log + Prometheus contracts |
| § Architectural rules → § Don't do these | The standing rules |
| § Two-process architecture → § Transport labels | MTA/MDA topology and the inbound scoring pipeline |
| § Outbound submission flow | The per-user view of a submission, and the in-domain partition both submit paths share |
| § First-party client send + receive | Stub — **owner is `mail-app-surface.md`** |
| § Email filter rules | Stub — **owner is `email-filters.md`** |
| § MUA-facing view | How Thunderbird/Apple Mail connect, and what the user sees |

---

## Goal

A single hardened SMTP entry point for the nest, **run by the mail-bridge in its MTA role** (`bins/fauna-bridges/`, role discovered from nest at startup by the enrolled service-user keypair — there is no `--mode={mta,mda}` CLI flag). It accepts mail for the local domain, runs a fixed-order policy stack against every transaction, ships the wire bytes plus envelope context to nest over WS-RPC + DAG-CBOR, and emits one structured-log line plus per-verdict metric per transaction. It owns no message storage; storage is nest's job.

The policy stack, log shape, metric set, and TLS posture described below all run in `fauna-mail-bridge`. **All admin-facing configuration — bind addresses, DKIM keys, TLS / ACME details, spam thresholds, DNSBL lists, rate limits, MTA-STS / DANE settings, submission auth policy — is set from a Fauna app, propagated via nest state, and consumed by the mail-bridge over WS-RPC.** CLI args / env vars on the bridge binary are reserved for OS-deployment topology (keypair file path, nest WS endpoint, log level). See `docs/goal/architecture/apps/bridges.md` for the bridge-as-client concept and `principles.md` § One configuration surface for the configuration rule.

**Bar: parity-or-better with a fully hardened postfix install.** Any feature a hardened postfix install offers (SPF/DKIM/DMARC/ARC verification, MTA-STS, DANE, TLS 1.3 floor with cipher pin, error tarpit, smuggling-resistant body parse, per-IP and per-/24 rate limits, structured logging, metrics, SCRAM auth on submission) must be present here, and beyond that the listener exposes ARC seal-on-relay, persistent /24+/64 rate limits across restart, and tighter defaults than postfix ships out of the box.

**When in doubt, choose the stricter option.** Avoiding security vulnerabilities outranks compatibility with old or buggy email clients/senders. If a sender violates RFC 5321/5322 in a way a strict reading would reject, we reject — even if postfix would accept by default. Permissive options are taken only when there is a strong, documented reason and an explicit user approval in the conversation introducing them; past approvals don't carry forward. Preferably we are both strict *and* compatible; when those conflict, strict wins.

**Default-off on first claim.** A fresh nest with a freshly-claimed admin has mail **off** by default. Identity, DMs, and federation are on; mail (port 25 / 465 / 587, DKIM keys, the external attack surface) waits for the admin to explicitly enable it from their Fauna app. The published Docker image always carries the mail-bridge binary — there is no `email` cargo/build flag gating it out of the image (`docs/goal/architecture/installers/docker.md` § Goal) — but the s6 service is `down` by default, and the supervisor does not start it until nest writes the lifecycle flag in response to the admin toggle (see `docs/goal/architecture/installers/docker.md` § s6-overlay Services and `docs/goal/architecture/apps/bridges.md` § Concept → "Lifecycle: supervisor-managed, admin-toggled"). "Works out-of-the-box" means the user lands on a working system without hand-editing config files — not that every external-facing surface is live by default.

---

## Implementation status today

The sections below describe target state for the SMTP entry point. (History: the legacy Go terminator `bins/fauna-bridge-imap/` and its Rust IPC backplane `bins/fauna-bridge-daemon/` were deleted at the I6 cutover on 2026-05-24; their file paths resolve via git history only.) Known gaps where current code lags target — read first when scoping a slice:

- **Message size limits — `max_message_bytes` (50 MB default) is the real end-to-end
  ceiling (ceiling retirement landed 2026-07-18).** A sealed body too large for the 2 MiB
  WS-RPC frame is staged on the bulk-byte plane, crosses as a `body_ref`, rests as
  frame-sized **continuation records**, and is read back byte-for-byte. The former at-rest
  ceiling (a single 16 MiB CARv2 record × the ~1.91× v1 envelope bloat, ~8.1 MB) is **gone**
  — continuation records rest a body of any size, so `MAX_SEALED_BODY_AT_REST_BYTES` /
  `MAX_RAW_MESSAGE_BYTES_AT_REST` were deleted and the product ceiling is `max_message_bytes`
  *alone* (the SMTP `Data` clamp, EHLO `SIZE`, nest import/APPEND admission, and
  `fauna.email.send` all read the one shared `effective_max_raw_message_bytes` rule). Owned
  by § Message size limits below (cap rationale: `../architecture/transport.md` § Max frame);
  **read its implementation-status bullets before scoping any size-related slice** — several
  plausible-sounding slices (compress the stored body) are already ruled out there.
- **Port 25 STARTTLS — landed (T1.1).** `runListenerWithBackend` (`bins/fauna-bridges/internal/mta/server.go`) sets `srv.TLSConfig` when a TLSProvider is wired: port 25 advertises STARTTLS in EHLO, supports the upgrade, and `inboundBackend.requireStartTLS` rejects pre-STARTTLS MAIL FROM with `530 5.7.10` (`InboundTLSMode=required`). TLSProvider == nil → plaintext-only, no STARTTLS advertised. Remaining follow-up: assert via `mta.Run` in a tier_3 e2e (deferred — production wiring is mechanical and exercised by the TLS submission e2e).
- **Deploy-image security perimeter — guarded (tier_4; tracked internally).** Three security properties now have a deploy-image (tier_4) regression guard, not just the Go unit coverage that bypasses s6/packaging: (1) **no open relay** — an unauthenticated port-25 RCPT TO a non-`local_domains` domain is rejected (`550 5.7.1`, the first check in `server.go::inboundSession.Rcpt`, ahead of recipient validation and independent of the spam perimeter); (2) **no unauthenticated submission relay** — an unauthenticated `MAIL FROM` on 587 (STARTTLS) and 465 (implicit TLS) is rejected (`530 5.7.0`; `AllowInsecureAuth = false`); (3) **no local-domain spoofing** — an unauthenticated inbound whose `From:` is one of the deployment's own local domains is rejected (`550 5.7.1 DMARC reject`) when that domain publishes DMARC `p=reject`. `tests/e2e-unified/tests/platform/docker/test_mail_security_relay.py` drives the real image with the bridges brought to serving; the anti-spoofing case (`test_local_domain_spoofing_rejected`) publishes the policy `verify_inbound` reads from the container resolver via a `fakes/fake_dns` sidecar wired in with `docker run --dns`. **Writing that guard surfaced + fixed a real DMARC-enforcement gap:** `verify_inbound`'s verdict mapping keyed on the DMARC *results* rather than the published policy, and mail-auth leaves both results `None` for a fully *unauthenticated* message, so the policy did not decide the verdict. `classify_dmarc` (`libs/fauna-mail/src/auth.rs`) now keys the verdict on the published policy per RFC 7489 §6.6.2 — the invariant is that a domain publishing `p=reject` has its spoofs refused on the DMARC gate, whatever the sending side's SPF record says. The guard's test sends one From field; a message with a second From field — which would pass DMARC as its first and render as its last — is refused earlier, by the DATA-time From-field count (§ Architectural rules → *Exactly one From field*), witnessed by Go unit tests rather than this tier_4 guard.
- **Inbound auth enforcement — landed (T1.5).** `verify_inbound` (shared Rust) + `mta/auth_enforce.go` enforce DMARC-reject (`550 5.7.1`), SPF-hardfail (`550 5.7.23`), DKIM-fail (`550 5.7.20`), in that order, behind `!auth.log_only`; SPF/DKIM defer to DMARC, and a DMARC `p=quarantine` failure is filed to the recipient's Junk by the spam gate. **Gap:** structured `SmtpVerdict` emission (the `verdict` field / `smtp_inbound_messages_total` per-verdict labels named throughout this doc) is not yet wired in the Go bridge — reject paths emit a generic log line + the gate's enhanced code, not the named `rejected_*` verdict string — **and the verdict-stream RPC itself, `fauna.bridges.report_smtp_verdict` (§ Log shape), is unbuilt: no allowlist entry, no bridge caller** (`mail-observability.md` § Data source tracks the nest-side half). Doc-wide, pre-existing; applies to every reject row in § Error / tempfail strategy and to § Log shape's WS-RPC shipping claim (both target-state). **Corrected 2026-07-22:** this also covers every *other* fine-grained `smtp_inbound_*_total`/`smtp_auth_*_total` counter name cited elsewhere in this doc (HELO/rDNS mismatch classes, `*_fail_open_total`, `*_dnsbl_score_signal_total`, `*_tls_version_total`, `*_mta_sts_violations_total`, `*_fcrdns_total`, etc.) — none of these per-mechanism names exist in `bins/fauna-bridges/internal/metrics/metrics.go` except the two `*_fail_open_total` counters (verified: the file's `init()` registers the collectors in § Metrics surface's table and no others). § Metrics surface's table is the actual registry; treat any `_total` name elsewhere in this doc that isn't in that table as target-state, not built.
- **Role-address routing (postmaster@/abuse@/noc@/security@) — deployment-wide default landed (T2.5 T1).** The nest `validate_recipient` handler (`bins/fauna-nest/src/bridge_routing_handlers.rs`) classifies a reserved role local-part on alias-miss (`fauna_mail::aliases::classify_role_address`) and resolves it to the primary admin actor (`db::admin::list_admin_actors().first()`, lowest `added_at` = the claimer) instead of the unknown-recipient `550` — the § abuse@ / postmaster@ never-reject invariant (`:234`). **The superset resolver `resolve_recipient` mirrors this** (`mail-aliases.md` § Resolution order step 6 — classify on alias-miss, route to the admin ahead of catch-all, carry the same `is_role_address` bit), and the **`validate_recipient`→`resolve_recipient` RCPT-TO cutover is now COMPLETE on both call sites** (inbound MX + submission), preserving role-address routing rather than regressing it. **`validate_recipient` is retained** — it is no longer the RCPT-TO resolver but remains the **exact-only AUTH-time resolver** that maps a login username (MTA-submission / IMAP / CalDAV `resolveActor`) to its canonical actor; login must resolve an exact identity, not an alias/wildcard/catch-all/forwarder, so the two RPCs are not interchangeable there and `validate_recipient` is not deleted. No-admin-claimed is unreachable in production (MTA enrollment requires an authenticated admin) but defended as an `internal()` → `451` tempfail, never a hard `550`. **Gaps:** the **role-address quota bypass** (`:235`) is **wired** (tracked internally, Slice C — per-mailbox *inbound* storage quota is now enforced at `ingest_inbound_mail` → `552 5.2.2`, and role-address deliveries carry an `is_role_address` bit — from `resolve_recipient` at RCPT-TO time, per the cutover above — onto the ingest request to skip the per-mailbox quota pre-check, so an over-quota admin mailbox still receives postmaster mail). The **greylist bypass** (`:236`) is **wired** (tracked internally): greylisting moved nest-side (`fauna.bridges.check_greylist` → `greylist_tuples`, the `:201` shape; the prior Go in-process-map drift is resolved), and `check_greylist_handler` short-circuits `Pass` for a reserved role local-part before the tuple/decide. **per-domain re-routing** (`:238`, `mail.inbound.role_addresses_per_domain`) is **wired** (tracked internally): the writer `fauna.bridges.set_role_address` stores a per-domain `role_address_overrides` map and `resolve_local_recipient` routes postmaster/abuse/noc/security to the per-domain override actor if set, else admin (`mail-multidomain.md` § Per-domain role-address routing § Wire shape + storage). The **`tlsrpt@`/`dmarc-report@` processor dispatch** (`:228`, `:229`) is still **not** wired — they fall back to the admin mailbox (never-reject holds; the per-domain override is ignored for them by design) pending its `ingest_tlsrpt_report` implementation (T4).
- **Inbound client-receive arrival push (`fauna.mail.received`) — landed (Slice 4a, deploy-verify; `mail-app-surface.md` § Inbound client receive → Arrival push).** The mail-ingest path (`bins/fauna-nest/src/bridge_routing_handlers.rs::persist_decoded_inbound_mail`) emits a `fauna.mail.received` push to the recipient actor on every genuinely-new placement, via the `crate::segments::notify_mail_received` helper (sibling to `notify_segments_changed`). The native apps that drive the shared `ConversationsSession::start_receive_loop` (**linux / windows / macos / ios / tui**) subscribe `fauna.mail.received` for free via the shared `fauna-client-conversations::NestConversationsPush` (the unified receive loop's `ConvPushEvent::MailReceived` arm — `libs/fauna-conversations/src/session.rs`), so a delivered message wakes a prompt mail poll instead of waiting one backstop-ticker cycle; the periodic ticker remains the reconnect / missed-push backstop. (Linux drove a bespoke `mail_sink.rs::start_inbound_poll` tokio loop until it was folded onto the shared loop (tracked internally) — so all five native apps now share one receive path; tui joined on the same shared loop at its own build, M4 slice F, 2026-07-13/14.) The segment-backup push pumps already subscribed to the kind, so backups become prompt for free (today's subscriber is the custodian pull; the nest's own coordinator observes its writes directly). Coverage: `bins/fauna-nest/tests/segments_changed_push.rs::ingest_handler_emits_mail_received_push` (tier_3 — drives the real `ingest_inbound_mail` handler, asserts the push) + the protocol round-trip in `push_events.rs` + `libs/fauna-conversations/tests/fauna_mls_backend_tests.rs::session_receive_loop_reacts_to_mail_push` (the shared receive-loop's mail-push arm). **All 7 apps now react to the push**, closing the web/android gap this bullet used to name: web hand-mirrors the shared loop's arms in its own JS push subscription (`apps/fauna-web/src/lib/conversations.ts`, wired 2026-07-12 — `../architecture/transport.md` § Push events), and android now drives `start_receive_loop` through the shared `conversations_session` factory (wired 2026-07-02), joining linux/windows/macos/ios/tui. **Gap:** a **GUI e2e proving push-not-poll** delivery is still blocked by the session-cached fixed-`FAUNA_CONV_POLL_SECS` driver (the nest-emit Rust test is the deterministic proof instead).
- **Outbound prompt-drain push (`fauna.bridges.outbound_ready`) — landed (Slice 4b, deploy-verify; § Outbound delivery → Prompt-drain nudge).** The symmetric counterpart of the inbound arrival push above. The `fauna.email.send` handler (`bins/fauna-nest/src/email_handlers.rs`), after a non-empty remote enqueue, calls `crate::bridge_routing_handlers::notify_bridges_outbound_ready` — a sibling of `notify_bridges_config_changed` **filtered to `BridgeRole::Mta`** — which emits a payloadless `fauna.bridges.outbound_ready` push to the approved MTA bridge. The Go MTA's `wsrpc.PushDispatcher` routes it (`internal/wsrpc/push_outbound_ready.go::OutboundReadyHandler`) to `OutboundWorker.Trigger()`, so a client-driven send relays promptly instead of within ≤30 s. Best-effort; the `fetch_outbound_due` poll is the backstop. Coverage: `bins/fauna-nest/tests/outbound_ready_push.rs` (tier_3 — drives the real `fauna.email.send` handler, asserts the push reaches an MTA bridge and **not** an MDA bridge, and is absent on a purely-local send) + the Go `internal/wsrpc/push_outbound_ready_test.go` (handler fires `Trigger`) + the protocol round-trip in `push_events.rs`. **Scope note:** emitted only from the interactive `fauna.email.send` path; background enqueues (bounces/forwards/TLSRPT/security mail) rely on the poll.
- **Sent-mailbox surfacing in the conversations view (`fauna.email.sent.fetch`) — landed (nest + shared + linux lead + web; `mail-app-surface.md` § Inbound client receive → Scope).** A `Sent`-mailbox sibling of `fauna.email.inbox.fetch` (shared `mailbox_fetch_handler` factored in `bins/fauna-nest/src/email_handlers.rs`, reusing the `InboxFetchRequest`/`InboxFetchReply` mail-page types; `User`-class, caller-scoped, own Sent UID cursor) lets the native app display mail the user sent through an **external SMTP-submission MUA** (Thunderbird / macOS Mail) — those write a server-side Sent copy via `submit_inbound_mail` (§ Recipient handling on submission) that the INBOX-only `inbox.fetch` deliberately never showed. The native receive loop (`ConversationsSession::start_receive_loop`, over the shared `fauna-client-conversations::NestMailInboundSource::inbox_and_sent`) drains the Sent feed alongside INBOX (each with its own cursor + `seen` set), ingesting each record as an outbound message — its `From:` is the caller, so it renders as a sent bubble and threads by participants with the recipient. The Sent copy is sealed to the **sender's own** MSEK-derived read key, so the client opens it with the same `recipient_secret` as INBOX (no extra key material). **A first-party `fauna.email.send` now writes the same durable server-side Sent copy too (durability fix; tracked internally):** `send_handler` (`email_handlers.rs`) calls `seal_and_store_sent_copy` after delivery (best-effort relative to delivery — a delivered/queued send never fails on a Sent-copy write error), so mail composed in a Fauna app survives a restart and reloads via `sent.fetch` — previously it was echoed only client-side and vanished on relaunch. The client's local echo on send and the server Sent copy share the RFC `Message-ID`, so `ConversationsManager::ingest_inbound` dedups by Message-ID (the per-feed segment-id `seen` set structurally can't catch the echo) → the in-session view shows one copy; after restart the empty store reloads it. Coverage: nest handler conformance test (`bins/fauna-nest/tests/conformance_email_sent.rs`; the first-party Sent-copy round-trip is asserted in `conformance_email_send_in_domain.rs::in_domain_send_lands_in_recipient_sealed_inbox` — alice's `sent.fetch` returns the copy openable byte-for-byte with her own key) + the dedup units (`libs/fauna-conversations/tests/smtp_backend_tests.rs::{ingest_dedups_same_rfc_message_id_across_server_records,ingest_into_empty_store_reloads_the_sent_copy}`) + a `fauna-client-email` unit + a **web tier_3 round-trip** (`tests/e2e-unified/tests/test_mail_sent_feed.py` — an external-MUA SMTP submission's server-side Sent copy surfaces in the web conversations view via the Sent poll; web's `conversations.ts` drains a parallel Sent cursor over `emailSentFetch`/`fauna.email.sent.fetch`, the twin of the native `NestMailInboundSource` Sent feed). windows/macos/ios/android poll Sent **for free**: they drive the shared `start_receive_loop`, and the shared FFI `conversations_session` factory (`libs/fauna-ffi/src/nest_client.rs`) registers the `INBOX`+`Sent` source pair unconditionally (`NestMailInboundSource::inbox_and_sent`, the canonical-source lift, 2026-06-08); android adopted the same shared `conversations_session` factory 2026-07-02 (joining the shared `start_receive_loop`, not a bespoke poll), after the canonical-source lift, so it inherited the Sent feed with no Sent-specific per-app work — no per-app Sent poll was ever needed (the original "5-client Sent lift" plan was obsoleted by that lift). No production gap remains here; only per-app live-acceptance test coverage does (below). **tui poll Sent for free too, by the same shared function, just called directly rather than through the FFI factory** — `apps/fauna-tui/src/conversations/conv_backend.rs` (built after the lift, M4 slice F, 2026-07-13/14) calls `NestMailInboundSource::inbox_and_sent` and drives `start_receive_loop` exactly like linux's direct (non-FFI) consumption. **The macOS + iOS live tier_3 round-trips landed 2026-07-15** (`tests/e2e-unified/tests/test_mail_sent_feed.py` `--client macos`/`--client ios`, both GREEN — no client-side change needed; `real_conversations`/`FAUNA_E2E_REAL_CONVERSATIONS` was already client-agnostic in the e2e harness). android's live tier_3 round-trip remains the owed per-app acceptance (`--client android` is not yet wired in `test_mail_sent_feed.py`); tui's landed (the test carries `pytest.mark.tui`, generalizing the same real-`ConversationsSession`-under-`FAUNA_E2E_REAL_CONVERSATIONS` pattern). **The GUI client-process-restart e2e for the first-party durability path is now LANDED on linux** (`tests/e2e-unified/tests/test_mail_sent_copy_restart.py`, tier_3): the linux app composes+sends via `fauna.email.send`, the process is restarted via the linux `PlatformDriver.recover()` (teardown + relaunch a fresh process with a fresh data dir → empty in-memory `ConversationsManager` store), re-logs-in as the same actor, **re-acquires the MSEK**, and the sent message reloads purely from the server-side Sent copy (an external recipient → no local INBOX copy, so the Sent feed is the only surfacing path) — proving the durability claim end-to-end through a real restart, including the client's MSEK re-acquisition that the deterministic nest/shared proofs cannot cover. (The earlier note that "driver relaunch is Windows-only today" was stale — the linux `recover()` is itself app-process relaunch; the in-session local echo still masks the server copy without the restart, which is why the restart is load-bearing.) **The web page-reload variant is now LANDED too (tracked internally):** `tests/e2e-unified/tests/test_mail_sent_copy_restart_web.py` (tier_3, web) is GREEN — a web-composed first-party send to an external recipient writes a durable server-side Sent copy that survives a full `location.reload()` (which tears down the wasm `ConversationsManager` + resets the poll cursors to uid 0) and reloads via `fauna.email.sent.fetch` on the `Smtp` rail. **The windows app-process-restart variant is now LANDED too (2026-06-28):** `test_mail_sent_copy_restart.py` is generalized `+windows +real_conversations` and GREEN — the windows app composes+sends, the process is restarted via `WindowsBridgeDriver.recover()`, re-logs-in, re-acquires the MSEK, and the Sent copy reloads from the server. *(Landing it root-caused + fixed a real **e2e-harness** race — not a product bug: across `recover()` the pre-restart FaunaApp instance briefly outlived the relaunch and, because E2E disables single-instancing so concurrent app processes are allowed, its `TestAgent` kept polling the FlaUI bridge's **process-global** command queue — stealing the post-restart `set_state`/nav commands and mounting UI in the now-dead window while FlaUI inspected the new one (flaky `count=0`). Fixed by **epoch-fencing**: the bridge mints a per-`POST /session` epoch injected into the app env (`FAUNA_E2E_SESSION_EPOCH`), the `TestAgent` echoes it on `/app/commands`+`/app/state`, and the bridge serves/accepts only the current epoch, so a stale agent is a no-op.)* **The macOS app-process-restart variant is now LANDED too (2026-07-15):** `test_mail_sent_copy_restart.py` is generalized `+macos +real_conversations` and GREEN — the macOS app composes+sends, the process is restarted via the shared `InProcessAgentDriver.recover()` (teardown + relaunch, reusing the durable e2e Keychain `FAUNA_E2E_CREDENTIAL_DIR` so the fresh process re-derives the same identity), re-logs-in, re-acquires the MSEK, and the Sent copy reloads from the server. The iOS/android process-restart variants remain the per-app follow-on — iOS lacks the durable e2e Keychain twin (tracked as "iOS crash-recovery journeys"). *(Building the web test briefly mis-surfaced this as a web compose→send product bug; the actual root cause was a **test-harness** artifact — the e2e `conversations_accept_recipient` bridge command called `ensureManagerForTest()` → `installMockBackendsForTest()`, which replaced the real `SmtpBackend` with a no-op `MockRailBackend` that returns a synthetic `Ok` with no wire I/O, swallowing the send before `dm-send-button`. Fixed by routing that command through the real `getConversationsManager()`. Production web compose→send was never affected: the real `ConversationsManager::send_new_thread` → `SmtpBackend::send` → `WasmSmtpSink::submit` → `fauna.email.send` path always worked, as the SPA's singleton `emailSend` independently proved.)*
- **Connection-time limits — landed (T2.2; global cap generalized to all listeners 2026-06-02).** The three port-25 defenses in § Connection-time limits are wired in `mta/server.go`: the global concurrency cap, the escalating error tarpit (`reject` → `tarpitDelay`, 250ms × n capped at 5s, policy rejections only), and the explicit header-section cap (`checkHeaderSection`, 256 lines / 1 MiB → `554 5.6.0`, before the UniFFI parser). The global cap was lifted into the shared `internal/connlimit` package and now wraps **every** bridge listener (port 25 default 100; submission/IMAP/CalDAV default 4096), counted on `smtp_connections_total{port,result}` (SMTP) + `mda_connections_total{listener,result}` (IMAP/CalDAV); the tarpit + header caps stay port-25-only. All three are compile-time constants — NOT env-var or nest-config tunable (`mail-policy-config.md` § Compile-time decisions). The **per-command read timeout** slice IS taken on port 25 (the unauthenticated-MX slowloris surface): `runListenerWithBackend` sets `srv.ReadTimeout = 30s` (the doc target; go-smtp resets it before every line read, so it's per-command — a healthy sender's lines arrive well under it, a > 30 s stall is dropped before it pins a global slot). `WriteTimeout` stays 5 min (responses are tiny; not a slowloris vector and a slow-reading client must not fail a delivered message), and submission (465/587, `runSubmissionListener`) keeps 5 min (authenticated, AUTH-gated, per-IP concurrent-capped). A per-IP **concurrent**-connection cap is now also wired on port 25 (`perIPInbound` + `maxInboundConnsPerIP` = 50, half the global 100 — postfix's `smtpd_client_connection_count_limit` default — loopback-exempt, a fixed constant since unauthenticated MX has no `AuthPolicy`): defense-in-depth so no single source IP can pin all 100 global slots even via a trickle-slowloris pacing under the per-IP rate cap. Still gaps in § Connection-time limits: the per-IP/per-subnet **rate** + per-IP **message-rate** knobs (catalog rows pending the nest-config rate-limiter buckets), making the inbound read timeout admin-tunable (`mail.inbound.read_timeout_s`, still compile-time for now), and `MaxLineLength = 1000` (not yet set).
- **`local_domains` plumbing — landed, both sides (nest-side + bridge-side).** Nest's `fauna.bridges.fetch_config` reply carries `local_domains: Vec<String>` (active-row projection of `mail_domains.domain_name` where `removed_at IS NULL`) and `primary_domain: String` (the `is_primary = true` row's name; empty when no primary exists). The bridge-side consumer cascade is wired: `mta.go`'s idle gate switches on `len(LocalDomains) == 0`, `inboundBackend.localDomains` filters RCPT TO against the active list, and outbound EHLO + the TLS provider anchor on `PrimaryDomain` (`internal/mta/mta.go`, `internal/mta/server.go`).
- **Per-option admin-UI knobs — the whole projected policy surface is admin-writable, not just a handful of rows.** Every admin-facing option — bind addresses, DKIM selectors, TLS / ACME details, spam thresholds, DNSBL lists, rate-limit caps, MTA-STS / DANE settings, submission auth policy, greylist windows — is nest-state, set from a Fauna app UI, and fetched by the mail-bridge over `fauna.bridges.fetch_config` per `principles.md` § One configuration surface. The current admin-write-path surface (which knobs, which sub-struct, which client rollout) is the catalog's own territory — see `mail-policy-config.md` § Implementation status today for the authoritative, currently-landed list (today: the five `put_{spam,auth,submission,imap,outbound}_policy` sub-structs, rendered on the flat `admin-mail` page across all 7 apps). There is **no** env-var configuration path.
- **TLS cert source is path-independent here.** HTTP-01 ACME (now implemented — multi-SAN, covering the apex + each active mail domain's `mail.<domain>`), admin-uploaded, and admin-synthesized self-signed are all live provisioning paths (see `mail-bridge-lifecycle.md` § TLS provisioning + § Implementation status). The cipher list and posture in § TLS posture per port apply regardless of which path provides the cert. The production `fauna-mail-bridge` consumes the cert over WS-RPC via the sealed `TlsCertBlob` path, not a shared file.
- **MTA-STS, DANE, ARC seal** — phased per the internal SMTP security design plan (tracked internally); verify current status against that plan before claiming any of these are wired in current code. (Inbound **DMARC enforce** + **SPF hardfail** + **DKIM enforce** are wired — see the auth-enforcement bullet above. **Outbound MTA-STS enforce AND DANE/TLSA are now landed Go-side — see the T2.1a / T2.1b bullets below. ARC seal-on-relay is not yet wired.**)
- **Outbound retry curve + 4 h delay-warning — landed nest-side (T1.3; tracked internally).** The retry schedule below is owned by nest, not the bridge: `mark_outbound_failed_handler` (`bins/fauna-nest/src/bridge_routing_handlers.rs`) consumes the shared `fauna_mail::outbound::retry::RetryPolicy` via `outbound_retry::schedule_after_failure` to compute `next_attempt_at` (honouring the wire `retry_after_seconds` as a floor), emits the once-per-message `Action: delayed` / `Status: 4.4.7` DSN (`outbound_retry::enqueue_delay_warning`, backscatter-suppressed), and on retry-budget / 5 d exhaustion runs the permanent-failure bounce path. The Go `OutboundWorker` no longer computes a backoff or decides give-up — it reports every temporary failure via `mark_outbound_failed`. **Curve defaults are now admin-tunable.** `retry_schedule_seconds`, `permanent_failure_timeout_hours`, and `delay_warning_at_hours` flow through `OutboundPolicyOverrides::effective()` (`bins/fauna-nest/src/db/mail_policy.rs`) into `outbound_retry::retry_policy_from_outbound`, called from `mark_outbound_failed_handler`'s retry scheduler — a stored `permanent_failure_timeout_hours=0` floors to the catalog default rather than permanently failing every message on its first attempt. (`ndr_rate_limit_days` is a separate knob on the same admin pane — see § NDR rate-limit per recipient below for its own wiring, landed 2026-08-25.) The table's "attempt 11" row is cosmetic — both `OutboundPolicy` (10-entry schedule) and `RetryPolicy` (9 after dropping the always-zero attempt-1 delay) yield 10 attempts plus the 5 d ceiling.
- **Outbound MTA-STS enforcement — landed Go-side (T2.1a; tracked internally).** Per § MX resolution item 3, the MTA worker fetches the recipient domain's MTA-STS policy from nest's new `fauna.bridges.fetch_mta_sts_policy(domain)` RPC — **nest** owns the `_mta-sts.<domain>` TXT + `.well-known/mta-sts.txt` fetch and the `max_age` cache (`AppState.mta_sts_fetcher` = `CachingMtaStsFetcher<LiveMtaStsFetcher>`, in the permanent `fauna_mail::outbound::mta_sts`) — and applies the per-host RFC 8461 §4.1 decision in `outbound.go` via the UniFFI `mta_sts_mx_matches` matcher: `enforce` refuses a non-matching MX (skip host → reschedule, never an immediate bounce) and requires WebPKI-verified STARTTLS on a matching one; `testing`/`none`/no-policy (incl. published-but-broken) proceed with opportunistic STARTTLS (RFC 7435, cert-unverified — a fix to the prior verify-under-opportunistic behavior). **Split:** this is **T2.1a** of roadmap row T2.1; **T2.1b — outbound DANE/TLSA — is now landed Go-side too** (tracked internally; see the next bullet). **Deferred:** a positive "delivery over MTA-STS-enforce-WebPKI-verified TLS" e2e (needs the stub cert injected into the bridge's outbound trust roots); the refusal paths are covered tier_3, and the TLS-capable stub MX that T2.1b added unblocks it.
- **Outbound TLSRPT reporter — landed (T2.4; tracked internally).** Per § TLSRPT outbound reporter, the production path is: the Go MTA worker classifies each per-host attempt's TLS outcome (`classifyTLSHandshakeError` + the pre-TLS STS signals in `outbound.go`) and reports it via `fauna.bridges.report_tls_attempt(recipient_domain, mx_host, result_type, mta_sts_outcome, mta_sts_policy, tlsa_records)` — **nest** reconstructs the RFC 8460 §4.4 policy bucket from those raw facts via the shared pure `fauna_mail::outbound::tlsrpt::policy_for_attempt` (Design 1 — no new FFI) and records it into the production `AppState.email.tlsrpt_aggregator`. The daily emitter (`outbound_tlsrpt::spawn_tlsrpt_daily_dispatch`, wired at startup) drains the aggregator at 00:00 UTC + per-domain jitter, fetches each recipient's `_smtp._tls.<domain>` rua (24 h cache), builds + gzips the report, and dispatches per URI (`mailto:` rides the outbound queue from `tlsrpt@<our-domain>`; `https:` POSTed), persisting one `tlsrpt_outbound_reports` row per transport (7-day retention sweep). The report-submitter domain is resolved at emit time via `resolve_reporting_mta` → the active primary `mail_domains` row (`state.email.domain` is `None` in the non-legacy build, so a startup snapshot would leave the emitter dead). **Deferred:** the per-domain `tlsrpt_send_reports` opt-out gate (default-on is correct per § TLSRPT outbound reporter; wires with the outbound mail-policy write-path). **Verification:** the Go classifier + report-flow is unit-tested (`internal/mta/outbound_tlsrpt_test.go`), the nest bucket reconstruction is unit-tested, and the nest-side emit pipeline is tier_3 e2e'd (`test_outbound_tlsrpt_emit_persists_aggregated_report`); a live Go-MTA → report → emit e2e is blocked on the pre-existing-red submission Sent-copy path (`451 4.7.0`), not on TLSRPT.
- **Outbound bridge-side policy knobs — landed (T2; tracked internally, Slice 2).** Of `OutboundPolicy`, exactly two fields are the outbound I/O owner's (the Go worker's) to apply; the rest are nest-side (retry curve, NDR, backscatter suppression, TLSRPT — see the bullets above). (1) **`ipv6_enabled`** (§ MX resolution, IPv4-only-sender behavior): `DefaultSMTPSender.Send` dials network `tcp` (dual-stack, Happy Eyeballs) by default, or `tcp4` (IPv4-only egress) when the admin disables it — `internal/mta/outbound.go::dialNetwork`. (2) **`treat_5xx_as_transient`**: `classify` demotes a 5xx whose RFC 3463 enhanced status is in the admin allowlist (default `5.0.0` when the response carries none) from permanent to transient, mirroring the shared `fauna_mail::outbound::classifier::DefaultBouncePolicy`. Both read **live** from the shared `mtaConfigHolder` per attempt (`cfg.outboundPolicy`), so a `config_changed` push hot-applies them with no restart. Coverage: `TestDialNetworkGatesIPv6`, `TestClassifyTreat5xxAsTransient`, `TestExtractEnhancedStatus`, `TestMTAConfigHolderHotApply` (tier_1; the IPv6/5xx effects aren't wire-observable without a dual-stack peer or a misconfigured MX, so unit tests are the correct tier).
- **Inbound `Received:` header prepend — IMPLEMENTED (tracked internally; 2026-05-25).** § Architectural rules requires the bridge to prepend a single sanitized canonical `Received:` trace header to inbound message bytes before `ingest_inbound_mail`. The production `fauna-mail-bridge` MTA path now does: `internal/mta/server.go::Data` builds the header from the session (HELO / client IP / TLS version+cipher / first local domain / single-recipient `for` / a fresh `NewQueueID`) and prepends it **topmost** — above the `X-Fauna-Scan-*` headers — via the existing `scan_gate.go::prependHeaders`, so every HPKE-sealed per-recipient copy carries exactly one Fauna receipt trace. The format + CR/LF/non-printable sanitization (`HeloDomain`/`ClientIP` → `unknown`, defeating header-injection forgery) live in shared Rust `libs/fauna-mail/src/received_header.rs::build_received_header` (uniffi-exported, the inbound sibling of the outbound `outbound::received_strip::strip_received_headers`); the Go side supplies only the I/O (clock + queue-id rand). Coverage: 7 Rust exact-format/sanitization unit tests + Go `received_header_test.go` (wiring + sanitizer-crosses-FFI, observed on the plaintext forward copy). The end-to-end inbound→sealed→IMAP-FETCH assertion rides Stage-5 deploy-verify's Slice 2 round-trip (`test_mail_bridge_mda`/MTA combined path) — the inbound seal is byte-preserving (independently green: `test_mda_imap_append_fetch_roundtrip`, `bins/fauna-nest/tests/mail_inbound_seal_unseal_round_trip.rs`), so the prepended header provably survives. The old retired-arch e2e (`tests/api/test_smtp_received_header.py`) was removed — it could only ever drive the deleted terminator.
- **`X-Fauna-*` strip at every filing door — IMPLEMENTED 2026-09-23.** § Architectural rules → *The `X-Fauna-*` namespace* names five doors; all five call the one shared strip, each pinned by a test that files a forged `X-Fauna-Spam-Threshold` / `X-Fauna-Address-*` pair beside a kept `X-Fauna-Forwarded-By` and reads the sealed or enqueued form back (`submission_test.go`, `append_test.go`, `conformance_email_send_in_domain.rs`, `conformance_mail_import_client.rs`; the inbound door's tests predate the ruling).
- **Email filter-execution hardening — IMPLEMENTED (tracked internally; 2026-06-26).** Two defense-in-depth fixes. **First:** `AddLabel` labels are validated as a single RFC 3501 keyword `atom` (`fauna_protocol::email::validate_label`) at create time (`email_handlers::validate_action`) and re-screened at ingest (`bridge_routing_handlers`), so a recipient rule can no longer file matching mail under a system flag (`\Deleted`/`\Seen`) or a whitespace-split keyword — see § Email filter rules. **Second:** the inbound MTA strips sender-forged reserved `X-Fauna-*` delivery-stamp headers (`X-Fauna-Scan-*`/`X-Fauna-Address-*`) before parse + seal (`received_header::strip_fauna_headers`, uniffi `strip_fauna_headers` → `mailfauna.StripFaunaHeaders` at `server.go::Data`), preserving only the inbound-consumed `X-Fauna-Forwarded-By` (forward-loop trace) — see § Architectural rules. Coverage: Rust unit tests (`fauna_protocol::email`, `fauna-mail received_header`), nest lib tests (`validate_action` create-reject + `ingest_inbound_mail_drops_unsafe_extra_flags`), Go `received_header_test.go::TestDataStripsForgedFaunaStampHeaders`; the existing `TestForwardSuppressedSelfSeen` guards `X-Fauna-Forwarded-By` preservation end-to-end.
- **Outbound DANE/TLSA enforcement — landed Go-side (T2.1b; tracked internally).** Per § MX resolution item 2 + § DANE, the MTA worker looks up each surviving MX host's DANE/TLSA records (RFC 7672) via nest's new `fauna.bridges.fetch_tlsa(mx_host)` RPC — **nest** owns the `_25._tcp.<mx_host>` **DNSSEC-validating** lookup (the Go stdlib can't do DNSSEC; `AppState.tlsa_resolver` = `LiveTlsaResolver` over the lifted `fauna_mail::outbound::dane::lookup_tlsa`, returning only `Proof::Secure`, SMTP-usable DANE-TA/EE records). When a host publishes such records the bridge sets STARTTLS mandatory and pins the handshake to them in its TLS `VerifyPeerCertificate` callback via the UniFFI `dane_chain_matches` decision (the pure cert-chain decision lifted into shared Rust — DANE-EE matches the leaf; DANE-TA requires the leaf to chain to the matched anchor and carry the MX name, see § Architectural rules); a pin mismatch or missing STARTTLS is a temporary failure (reschedule, **never** a plaintext downgrade or bounce). **DANE > MTA-STS** (§ MX resolution :498): secure TLSA records override the MTA-STS WebPKI-required posture; the MTA-STS mx: **refusal** gate still runs first (§ MX resolution :500), so a refused host costs no TLSA fetch. A `fetch_tlsa` RPC error never blocks delivery (falls back to the MTA-STS / opportunistic posture). **Both DANE legs are DNSSEC-gated as of 2026-09-01:** the MX RRset is resolved nest-side too, via `fauna.bridges.resolve_mx` (`AppState.mx_resolver` = `LiveMxRrsetResolver` over `fauna_mail::outbound::mx::lookup_mx_secure`), and the bridge skips the TLSA fetch outright when the reply's `secure` bool is false — RFC 7672 §2.2. Before that the MX leg came from Go's stdlib resolver unvalidated, so the TLSA leg's DNSSEC validation was authenticating whatever name a DNS-spoofing attacker supplied. See § Architectural rules below for the full DANE mechanism (there is no separate § DANE section — the content lives there).

---

## Configuration tiers

The mail-bridge participates in the three-tier configuration model canonicalized in `docs/goal/behavior/mail-policy-config.md` § Three configuration tiers (deployment / admin / user). The bridge process sees Tier 1 (its argv, plus the localhost-only topology hatch for the few deployment-topology values nest cannot provide because they're consumed pre-dial) and Tier 2/3 (everything nest-fetched, via `fauna.bridges.fetch_config` + the subscribe-and-hot-reload loop). Anything that wants to reach below Tier 1 from a non-nest source is a product-invariant violation.

The topology hatch (formerly "operator-hatch"; the artifact-written file keeps the legacy filename `operator-hatch.toml` — renamed 2026-07-09, no role called "operator" exists) is a **deliberate allow-list** of pre-dial deployment-topology fields (currently `data_dir` and `metrics_bind_addr`). The bridge's TOML reader rejects any other key at the parse boundary with a typed `ErrForbiddenField` pointing at the Fauna admin UI — there is no `--config=…` flag carrying DKIM keys, spam thresholds, or per-account fields; those live in nest state and arrive over WS-RPC.

---

## Process topology

| Port | Role | Process | TLS | AUTH |
|---|---|---|---|---|
| 25 | Inbound MX | mail-bridge in MTA role (`bins/fauna-bridges/`) | STARTTLS required, TLS 1.2 floor, 1.3 preferred | none |
| 465 | Submission, implicit TLS | mail-bridge MTA role | implicit, TLS 1.2 floor | SCRAM-SHA-256 preferred (future), OAUTHBEARER + PLAIN fallback over TLS |
| 587 | Submission, STARTTLS-required | mail-bridge MTA role | STARTTLS required | same set as 465 |
| `/api/v1/ws/{actor_id}` | bridge ↔ nest RPC | WS-RPC + DAG-CBOR | TLS to nest's HTTPS listener | challenge-response over the WS subprotocol, signed by the bridge's enrolled service-user Ed25519 keypair; per-role method allowlist enforced nest-side |

The mail-bridge holds no per-process state beyond its keypair file; all routing tables, rate-limiter buckets, DNSBL caches, and greylist state live nest-side (or are nest-pushed and held in-memory by the bridge under a TTL). There is exactly one MTA-role instance per nest deployment; horizontal scaling is by enrolling additional keypairs. The bridge ↔ nest transport is WS-RPC + DAG-CBOR — see `docs/goal/architecture/transport.md` § WS-RPC for the wire format, challenge-response auth, and per-role method-allowlist enforcement.

---

## Configuration is nest-side

Every admin-facing knob — bind addresses, DKIM selectors, TLS / ACME details, spam thresholds, DNSBL lists, rate-limit caps, MTA-STS / DANE settings, submission auth policy, greylist windows — lives in nest state, named in the nest configuration schema, set from a Fauna app UI, and fetched by the mail-bridge over WS-RPC at startup + watched for changes (per `principles.md` § One configuration surface — "nest configuration is set from Fauna apps, not from CLI args / env vars / hand-edited config files"). The bridge binary's only non-nest inputs are the deployment-topology flags: `--keypair-file`, `--nest-endpoint`, `--log-level`, optional `--data-dir`. There is no env-var configuration path; the few options whose admin-UI knob has not yet shipped are nest-config catalog entries pending their write-path, not env vars.

---

## Inbound policy stack

The fixed evaluation order on port 25 is:

```
connection
  → global concurrent-connection cap   [connection-time]
  → per-IP concurrent-connection cap    [connection-time]
  → per-IP rate limit                   [connection-time]
  → DNSBL                        [connection-time — reject-class hits short-circuit here, ahead of FCrDNS]
  → FCrDNS                       [connection-time capture only, mode={off|score_signal|enforce}; the enforce-reject itself fires later, at HELO]
  → STARTTLS-enforce gate        [envelope-time, first MAIL — fires before the HELO gates below, regardless of policy]
  → HELO validation              [envelope-time, first MAIL]
  → MAIL FROM                    [envelope-time]
    → sender-domain MX/A check   [envelope-time]
  → SPF                          [envelope-time identities; computed at DATA with the other verdicts]
  → RCPT TO                      [envelope-time]
    → recipient resolve          (RPC: resolve_recipient — aliases + role addresses, per-recipient)
    → greylist check             (RPC: check_greylist — per-recipient tuple; role addresses bypass)
  → DATA                         [data-time]
  → header-section caps          [data-time, bridge-side, before RPC]
  → From-field count             [data-time, bridge-side, before any parser reads From — exactly one, § Architectural rules]
  → DKIM                         [data-time]
    → enforce-on-fail gate       (when AuthPolicy.enforce_dkim, and only when DMARC didn't decide)
  → DMARC alignment              [data-time]
  → ARC validate                 [data-time]
  → Received: prepend            [data-time, bridge-side, immediately before store — runs AFTER auth verification, since verification reads the sender's original bytes and must not see a header we haven't stamped yet]
  → ARC seal                     [data-time, on relay only]
  → store                        (RPC: ingest_inbound_mail — per-mailbox quota enforced here, 552 5.2.2; role addresses bypass)
```

Each stage either accepts (fall through), rejects with a 5xx (permanent — sender bounces), or tempfails with a 4xx (transient — sender retries). On any tempfail the bridge emits a `SmtpVerdict` with `verdict=tempfail_*` and the connection terminates at that stage; later stages are not run.

**Connection-time** stages happen before the SMTP banner is sent (or are evaluated against the connection's first byte). **Envelope-time** stages run during MAIL FROM / RCPT TO. **Data-time** stages run after the DATA dot-stuffed body has been read.

Authentication-results (SPF, DKIM, DMARC, ARC) are **computed** in shared Rust (`libs/fauna-mail/src/auth.rs::verify_inbound`, `mail-auth`), called over UniFFI from the Go bridge once the DATA body is read; the four verdicts ride on `AuthVerdicts`. They are **enforced** in the bridge's auth-enforce stage (`bins/fauna-bridges/internal/mta/auth_enforce.go`) — see § Error / tempfail strategy for the per-gate codes. Enforcement order is DMARC-reject → SPF-hardfail → DKIM, all suppressed when `auth.log_only` is set (observe mode: verdicts computed + logged, nothing rejected). SPF and DKIM defer to DMARC: each gate fires only when DMARC did not decide (DMARC `Pass` or `Fail{Quarantine|Reject}` = decided). DMARC *reject* (`p=reject` + `enforce_dmarc`) rejects here; DMARC *quarantine* falls through to the C.7 spam gate's quarantine disposition (a folder route, not a 5xx). *(ARC verify is wired in `verify_inbound`; outbound ARC sealing lives in `libs/fauna-mail/src/outbound/arc.rs` where the DKIM private key loads, ARC selector identity still a follow-up.)*

---

## Inbound perimeter hardening

The connection-time and envelope-time stages in § Inbound policy stack are each spelled out below. Each stage with a human choice in it has its row in the policy catalog ([`mail-policy-config.md`](mail-policy-config.md) § Inbound perimeter) and is set from the app: the per-address connection rate, the blocklists, the reverse-lookup mode, the greeting identity check, greylisting. The rest have no switch, by design: the greeting's syntactic checks, required encryption, the sender-domain check and the concurrency caps are always on (`mail-policy-config.md` § Compile-time decisions). The defaults below are the shipping defaults; any deviation is the admin opting into edge cases. (This paragraph said every stage was individually toggleable until 2026-10-01; § Architectural rules and the catalog always said otherwise.)

### rDNS / FCrDNS

Forward-Confirmed Reverse DNS, applied at connection-time after the IP-rate-limit gate:

1. **PTR lookup** for the client IP. Missing PTR → `has_ptr = false`; do **not** alone-reject (overly aggressive — postfix's `reject_unknown_client_hostname` would block too much legitimate small-volume mail). Increment `smtp_inbound_no_rdns_total` and tag the session as `has_ptr=false` for the verdict log.
2. **Forward-confirm**: if PTR returned `<name>`, resolve `<name>`'s A + AAAA records. If the client IP is in the resolved set → `fcrdns_ok = true`. If not (PTR exists but doesn't forward-confirm) → `fcrdns_ok = false` (mismatch is a stronger negative signal than missing PTR — likely spoofed reverse zone).
3. Use as **signal, not gate**: `has_ptr=false` adds a +1 spam-score weight (admin-tunable); `fcrdns_ok=false` adds +2. Neither alone-rejects; both ride to nest's spam-score aggregator. Admin opt-in to alone-reject lives behind `inbound.reject_no_rdns = true` / `inbound.reject_fcrdns_fail = true` (both default `false`; admin toggles in the Fauna app UI when their abuse load makes the looser default untenable).
4. Resolver-error on any step fails open with `smtp_inbound_rdns_resolver_error_total{stage}`.

### HELO / EHLO syntactic + identity validation

Validation runs at the first `MAIL FROM` (one validation per session; not repeated on RSET).

**Syntactic** (existing rules in § Architectural rules; collected here for the named section):

- HELO/EHLO argument is required and non-empty.
- ASCII-only, printable, no CR / LF / null / non-printable bytes.
- Length ≤ 253 octets (RFC 5321 §4.5.3.1.1).
- Either a syntactically valid FQDN (RFC 1035 / RFC 5321 §2.3.5) **or** an IP address literal (`[192.0.2.1]` or `[IPv6:2001:db8::1]`, RFC 5321 §4.1.3).

**Identity** (the gap this section fills):

| Claim | Action | Why |
|---|---|---|
| HELO is `localhost` from a non-loopback peer | reject `554 5.7.0 Invalid HELO/EHLO` | obvious spoof; loopback peers (dev / test) exempted |
| HELO is a bare hostname (single label, no dots) from a non-loopback peer | reject `554 5.7.0 Invalid HELO/EHLO` | RFC 5321 §4.1.4 requires an FQDN; bare hostnames are 1990s-MUA artifacts that we shouldn't accept |
| HELO is **our own primary hostname** from a non-loopback peer | reject `554 5.7.0 Invalid HELO/EHLO` | a sender claiming to be us is impersonation; counter `smtp_inbound_helo_self_claim_total` |
| HELO is **a domain we host mail for** (in the deployment's `local_domains` list) from a non-loopback peer | reject `554 5.7.0 Invalid HELO/EHLO` | same logic — no external sender should claim to be one of our domains; counter same |
| HELO is an IP literal that doesn't match the client IP | reject `554 5.7.0 Invalid HELO/EHLO` (syntactic stage, always on) | a server that states an address other than the one it connects from is misconfigured or lying; § Architectural rules |
| HELO is an FQDN with no A/AAAA record | reject `554 5.7.0` while the identity check is on (`mail.inbound.helo_identity_required`, default **on**) | a name that does not exist identifies nobody. A resolver *error* is not this case: it fails open, never a reject for a DNS-availability problem |
| HELO is an FQDN whose A/AAAA resolve to a set not containing the client IP | reject `554 5.7.0` while the identity check is on | the analog of FCrDNS on the HELO side. Legitimate senders behind NAT or a load balancer can fail it, which is why this half is the admin's to switch off (`admin-mail-helo-identity-required-toggle`) and the syntactic half is not |

**Ruled 2026-10-01:** the last three rows said *accept with a counter* until this date, contradicting § Architectural rules, the policy catalog's default and the code, all three of which refuse. The table now follows them. The two self-claim rows (our own hostname, a domain we host) are the goal and are **unbuilt** — the catalog still lists `mail.inbound.reject_helo_self_claim` as not projected.

### Greylisting

Tuple-keyed deferral at **RCPT TO time, per recipient** (after `resolve_recipient` — the tuple key includes the recipient, so each RCPT on a transaction is greylisted independently; `server.go` calls `CheckGreylist` in `Rcpt`).

**Tuple key:**

1. Normalized sender domain (case-folded; address localpart discarded — many bulk senders rotate localpart per recipient, but the domain is stable).
2. Recipient address (full address, case-folded; greylisting per recipient — a domain that established trust on alice@ retries differently for bob@).
3. **Client /24 for IPv4, /64 for IPv6** (subnet, not full IP — many bulk senders rotate within a sub-net but keep the sub-net stable; per-IP greylisting forces full re-greylist of every IP, which is too aggressive and trains the bulk sender to spin more IPs).

**Defaults:**

- Minimum hold delay: **60 seconds** — first attempt within 60 s of the previous one is deferred. Real MTAs retry on the order of minutes; spammers either don't retry or do retry immediately and get re-deferred.
- Maximum hold delay window: **4 hours** — a retry after the 4 h window but with the same tuple gets re-greylisted (this defends against a spammer that retries once after 1 minute and then waits 4 days). The 4 h window is the typical real-MTA retry-attempt-2 timing per RFC 5321 §4.5.4.1.
- Whitelist on first success: **30 days** — a sender tuple that retried successfully gets bypassed for 30 days; spam fingerprints rotate faster than that, real senders are stable.

**Storage:** nest-side, in `greylist_tuples(sender_domain, recipient, subnet, first_seen, last_attempt, accepted_at)`. The MTA bridge queries via `fauna.bridges.check_greylist` at envelope-time; nest applies the policy. The MTA stores no greylist state locally — uniform behavior across bridge restart, important for parity-or-better-than-postfix.

**Wire behavior:** deferred = `451 4.7.1 Greylisted; try again later` (`bins/fauna-bridges/internal/mta/server.go`, the `CheckGreylist` defer branch). **Corrected — there is no separate `smtp_inbound_greylist_deferrals_total` counter**; a defer increments the shared `smtp_inbound_messages_total{verdict="rejected_greylist"}` counter (§ Metrics surface). Whitelisted = silent pass-through (no greylist verdict at all).

**Per-deployment toggle:** admin can disable globally (`inbound.greylist_enabled = false`, default `true`); admin can also per-(IP / domain) allowlist via mail-policy-config for senders that demonstrably don't retry properly (financial-institution one-time-passwords are the classic case — write down the exception in mail-policy-config, accept the abuse-vector cost).

### TLSRPT inbound report listener (RFC 8460)

We publish a `_smtp._tls.<our-domain>` TLSRPT policy by default (per mail-policy-config — admin can disable). Other senders that observe TLS failures while delivering to us submit aggregate reports via the URI we publish. The inbound listener accepts both transports:

- **`mailto:tlsrpt@<our-domain>`** — the role address (per § abuse@ / postmaster@ routing below) routes to a nest-side TLSRPT processor instead of the admin mailbox. The MTA recognizes the recipient at RCPT TO time (`tlsrpt@<local-domain>` is reserved), accepts the message, and dispatches the DATA body to nest's `fauna.bridges.ingest_tlsrpt_report(payload_bytes, source_envelope)` RPC.
- **`https://<our-domain>/.well-known/tlsrpt/v1/`** — POST endpoint on nest's HTTPS listener (not the bridge — TLSRPT submitters use the well-known URI without bridge involvement). Accepts `application/tlsrpt+json` (or `application/tlsrpt+gzip`), validates the JSON schema per RFC 8460 §4.4, stores in `tlsrpt_inbound_reports(submitter, report_id, received_at, payload)`.

The dispatch is **lossy intentionally**: malformed reports increment a counter and 200-ack rather than 4xx-reject, because TLSRPT submitters have no retry obligation for malformed reports. Storage retention: 90 days for ad-hoc admin inspection; aggregate metrics surfaced to the admin pane.

**Implementation status (corrected — first independent check of this section, 2026-07-23): both legs above are target state; neither is wired.** The `mailto:` leg's role-address routing exists (`tlsrpt@<local-domain>` is accepted and never rejected, per the never-reject invariant below), but it currently falls through to the **admin mailbox**, not a nest-side processor: `fauna.bridges.ingest_tlsrpt_report` has no allowlist entry and no handler anywhere in `bins/fauna-nest/src/` (grepped; only referenced in comments as future work) — the same gap § Implementation status today's role-address-routing bullet already tracks for this RPC. **The `https://<domain>/.well-known/tlsrpt/v1/` endpoint does not exist at all**: nest's HTTPS listener registers only `/.well-known/acme-challenge/{token}` (`bins/fauna-nest/src/acme_http01.rs:103`) and `/.well-known/mta-sts.txt` (`bins/fauna-nest/src/lib.rs:996`) — no TLSRPT route — and `tlsrpt_inbound_reports` is not a table in the schema (`bins/fauna-nest/src/db/migrations.rs` defines only `tlsrpt_outbound_reports`). Consistent with this, the published `_smtp._tls` record (`libs/fauna-mail/src/dns/per_domain.rs::build_tlsrpt_txt_record`) advertises only a `mailto:` `rua=` — no `https:` URI is ever published, so no peer is directed at the unbuilt endpoint. **Scope note 2026-09-25:** the admin-inspection surface for these inbound reports ("aggregate metrics surfaced to the admin pane" above) is **outside** the mail health readout the user ratified that day (`mail-deliverability.md` § Admin-pane Deliverability surface → *The mail health readout* — deliverability checks only, no TLS-report row in its id set); it stays target state with no build row until raised as its own ask, alongside the two legs above.

### abuse@ / postmaster@ role-address routing

RFC 2142 reserves a set of role addresses for any domain that runs network services. RFC 5321 §4.5.1 mandates `postmaster@` specifically. We honor:

| Role address | Source | Default destination |
|---|---|---|
| `postmaster@<our-domain>` | RFC 5321 §4.5.1 (mandatory) | the admin's mailbox (the user who claimed the box) |
| `abuse@<our-domain>` | RFC 2142 §4 (mandatory for domains that send mail externally — we do) | same |
| `noc@<our-domain>` | RFC 2142 §4 (recommended) | same |
| `security@<our-domain>` | RFC 2142 §4 + project policy | same |
| `tlsrpt@<our-domain>` | RFC 8460 + our published policy | the TLSRPT processor (see above) — NOT the admin mailbox |
| `dmarc-report@<our-domain>` | RFC 7489 + our published DMARC policy URI | the DMARC aggregate + forensic processor (see `dmarc-reporting.md` § Aggregate-report receive (RUA) / § Forensic-report receive (RUF)) — NOT the admin mailbox |
| `unsubscribe@<our-domain>` | RFC 8058 + our outbound list-mode (when a user runs a mailing list per `mail-mass-mailing.md`) | the list-unsubscribe mailto handler — the **nest's** `resolve_recipient` intercepts `unsubscribe+<token>@` ahead of role classification, decodes the HMAC token, flips `mail_list_members.unsubscribed_at` (fire-and-forget, `250` regardless), and returns `Discard`; the DATA body is discarded — NOT a user mailbox |

**Rules:**

- Role-address recipients **never reject at recipient-validate time**, even when the deployment hasn't explicitly created the mailbox. RFC 5321 §4.5.1 is explicit: postmaster mail is always accepted; rejecting it breaks the bounce-of-a-bounce protection that prevents mail loops. The same rule applies to `abuse@` (per RFC 2142): rejection would silence the network's ability to flag us as an abuse source.
- Role-address recipients **bypass per-mailbox quota** (see imap-server.md § Per-mailbox quota). An over-quota admin mailbox still receives postmaster mail; the alternative is silently losing security signals.
- Role-address recipients **bypass greylisting**: postmaster traffic is typically transactional and low-volume; greylisting it would delay critical postmaster-to-postmaster communication.
- **`unsubscribe@` is the exception — recognized ahead of role classification, not via `classify_role_address`.** It is deliberately **not** in `fauna_mail::aliases::DEFAULT_RESERVED_LOCAL_PARTS` (the set `classify_role_address` consults): adding it there would route inbound `unsubscribe+<token>@` to the **admin mailbox** instead of the unsubscribe handler. Instead, the nest's `resolve_recipient` intercepts the `unsubscribe+<token>@` local-part **before** role classification and returns the `Discard` outcome — the MTA accepts the RCPT with `250`, the nest performs the unsubscribe flip (`mail_list_members.unsubscribed_at`, fire-and-forget + idempotent), and the DATA body is dropped before the parse/auth/scan pipeline (`mail-mass-mailing.md` § The mailto handler). Two consequences distinguish it from the true role addresses above: (1) bare `unsubscribe@` with **no** token is the one reserved local-part that **does** reject (`550 5.1.1 unsubscribe@ requires a list-unsubscribe token`) — the never-reject rule is specific to the RFC 2142/5321 role addresses; (2) the `+<token>` suffix is preserved case-sensitively (it is a base64url value), unlike the case-folded base. `unsubscribe@` / `unsubscribe-*@` are correspondingly reserved at **alias-create** time via a separate uncircumventable creation predicate (`fauna_mail::aliases::is_creation_reserved_local_part`), again **not** the role-routing set.
- Routing is **per-deployment-domain**, not per-actor: an admin manages multiple local domains, all of their role addresses route to the admin's mailbox by default. **Per-domain re-routing shape spec'd in `mail-multidomain.md` § Per-domain role-address routing** (binding `mail.inbound.role_addresses_per_domain`; `tlsrpt@` and `dmarc-report@` always route to the deployment-wide processor inboxes regardless of per-domain override).
- The admin's mailbox shows role-address mail in INBOX by default; the admin can configure a Sieve rule (when ManageSieve lands — see imap-server.md § Upstream-blocked gaps) or the basic filter rule UI (§ Email filter rules) to route into a dedicated folder.

**Don't** auto-respond from any role address — auto-responders on `postmaster@` are an amplification vector and a backscatter source.

---

## TLS posture per port

| Port | Required min | Cipher policy | Cert source |
|---|---|---|---|
| 25 | TLS 1.2 floor (pinned), 1.3 preferred (Go's default max-version negotiation) | Go's default TLS 1.2 cipher-suite set (no explicit pin — see gap below); TLS 1.3 uses Go's hard-coded suite set | ACME-issued (nest HTTP-01) / admin-uploaded / self-signed floor — `mail-bridge-lifecycle.md` § TLS provisioning |
| 465 (submission) | implicit, TLS 1.2 floor, 1.3 preferred | same | same |
| 587 (submission) | STARTTLS required, TLS 1.2 floor, 1.3 preferred | same | same |

**Gap — the pinned AEAD-only cipher list and the shared "hardening factory" described in an earlier draft of this section were never built (corrected 2026-07-23, first independent check of this claim).** `internal/mta/mta.go` (port 25 + the 465/587 submission listeners) and `internal/mda/mda.go` (993/CalDAV) each independently construct their own bare `*tls.Config{GetCertificate: ..., MinVersion: tls.VersionTLS12}` literal — there is no `bridgetls.HardenedConfig()` factory and no `posture.PinnedCipherSuites()` anywhere in the Go bridge (verified: zero occurrences of `CipherSuites`, `TLS_ECDHE`, or `CurvePreferences` in `bins/fauna-bridges/`), so neither an explicit cipher-suite restriction nor a curve-preference pin is applied on any port including the 465/587 submission listeners — Go's built-in default TLS 1.2 suite negotiation and default curve list apply instead of the pinned list below. Cert *lookup* is genuinely shared, just under a different name: both packages plug the same `bridgetls.Provider.GetCertificate` callback (`internal/tls/tls.go`) into their own `tls.Config`, and that provider refreshes on a 12h timer or SIGHUP — there is no `CertReloader` type, `bridgetls.Provider` is the real name. The cipher/curve pin below remains the target (the `§ Goal` "cipher pin" bar) but is unbuilt on the submission ports today.

The **target** (not yet enforced — see gap above) pinned TLS 1.2 cipher list, in server-preference order:

1. `TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384` (RFC 5289)
2. `TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305` (RFC 7905)
3. `TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256` (RFC 5289)
4. `TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384` (RFC 5289)
5. `TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305` (RFC 7905)
6. `TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256` (RFC 5289)

Target curve preferences (not yet enforced): `X25519, P-256, P-384`. P-521 dropped (slow, no security upside on a public-internet listener).

`InboundTLSMode` ships as `required` only: port 25 demands STARTTLS in line with "Strict beats permissive" / RFC 8996. **No env-var override** (per `principles.md` § One configuration surface — nest options live in admin UI, not env vars); no admin-UI knob in scope today either. If a real deployment's senders need `opportunistic` or `off`, add an admin-UI toggle at that time — don't pre-ship the modes. Modern MTA coverage is high (>95%); the marginal-sender compatibility cost of `required` is what we accept in exchange for never-cleartext on 25.

ACME is implemented (nest HTTP-01 — § Implementation status today); outbound MTA-STS enforcement (T2.1a) and outbound DANE/TLSA (T2.1b) are landed on the production Go path (§ Implementation status today; § Architectural rules for the DANE mechanism). ARC seal-on-relay remains unwired.

**Cert-honesty coupling (the *published* MTA-STS mode follows cert reality).** The inbound MX cert is ACME-managed by default (`mail.<primary>` is part of the apex SAN set — `mail-multidomain.md` § Implementation status today), but falls back to the **always-live self-signed floor** whenever no trusted cert is currently obtainable — issuance hasn't yet succeeded, or a renewal lapses ([`../architecture/nest/tls-certificates.md`](../architecture/nest/tls-certificates.md) § A). A mail domain whose MX is currently on the floor must therefore **not** publish `_mta-sts.<domain>` mode `enforce` — a sender that fetched an `enforce` policy then refuses the non-WebPKI MX (the symmetric refusal we apply *as a sender* under outbound MTA-STS, above). So the published MTA-STS mode is **coupled to cert reality**: it drops `enforce → testing/none` while on the floor and restores `enforce` once a trusted cert is live for that MX. On a **DNSSEC** domain, an optional `_25._tcp.mail.<domain>` TLSA pinning the **stable-key** floor MX keeps a self-signed MX DANE-trusted without a CA. The coupling rule + the stable-key requirement are owned by `tls-certificates.md` § D; the per-domain mode values + record bodies stay owned by `mail-multidomain.md` § Per-domain MTA-STS.

Inbound STARTTLS enforcement is live via the `InboundTLSMode=required` gate (T1.1 — § Implementation status today). The old nest-side `EmailConfig.require_tls` TOML field is **dead** — zero consumers; deleting it is a one-line nest-track code follow-up (config-file-theatre residue), not wired to anything here.

---

## Error / tempfail strategy

Two-tier rule. **All RPC errors and DNS errors are tempfail (4xx).** Specifically:

| Condition | Code | Meaning | Why |
|---|---|---|---|
| `resolve_recipient` RPC error (nest down, transport error) | `451 4.7.1 try again later` | tempfail, sender retries | nest-side state unavailable; never silently accept undeliverable mail |
| `ingest_inbound_mail` RPC error at store time | `451 4.7.0 try again later` | tempfail, sender retries | nest-side state unavailable; never silently accept undeliverable mail |
| **Message over the inline ceiling** (raw bytes over `MAX_INLINE_RAW_MESSAGE_BYTES`, or an assembled ingest request over the 2 MiB WS frame) | `552 5.3.4 Message size exceeds fixed maximum` | permanent, sender bounces immediately | **ratified 2026-07-12** (§ Message size limits; cap rationale `transport.md` § Max frame): a size the transport can never carry is a *permanent* condition. Two enforcement layers — the pre-parse `io.LimitReader` clamp and the precise post-seal pre-RPC assembled-size guard (the index hint is input-dependent). Interim only: once the bulk-plane reference legs land, the ceiling reverts to `max_message_bytes` alone. The pre-fix behavior (over-cap fell into the generic `451` row above; the sender retried the doomed message for days before bouncing) was pinned by `test_mail_bridge_mta.py::test_inbound_message_over_the_ws_rpc_cap_is_never_accepted_then_lost`, which flips to `552` with the fix |
| DNS resolver error during DNSBL or PTR lookup | fall-open + counter + warn log | accept (fail-open) | poisoned resolver shouldn't reject all mail; counter surfaces it for ops |
| DNSBL listing | `550 5.7.1 blocked by DNSBL` | permanent | normal reject path |
| Recipient invalid (reject from `resolve_recipient`) | `550 5.1.1 user unknown` | permanent | normal reject path |
| Recipient resolved but has no MLS pubkey on file (`fetch_recipient_mls_pubkey` → none), and the actor is **not** a succession's successor | `550 5.1.1 recipient has no encryption key on file` | permanent | the recipient exists but has never enabled mail — no key is coming; checked at RCPT TO on both the inbound MX arm (`checkRecipientMLSPubkeyAtRcpt`, `mta/server.go::Rcpt`, exempting role addresses — see § abuse@ / postmaster@ routing) and submission RCPT TO (§ Recipient handling on submission, `resolveLocalRecipient`), with `Data`'s own pre-loop preflight (`preflightRecipientSealKeys`) as the inbound MX arm's backstop for a key revoked between RCPT and DATA |
| Recipient resolved, has no MLS pubkey on file, **and** the actor is a succession's successor (`fetch_recipient_mls_pubkey` reply's `succession_pending`) | `451 4.7.1 Recipient's mailbox is being restored; try again later` | tempfail, sender retries | **ratified 2026-08-11** (succession-aftermath.md § Re-key scope; found verifying the succession mail leg, `mail-credentials.md` § Rotation and recovery → *Succession*): between the succession ceremony and the successor's first sign-in, the handle resolves to an actor with a registered recipient pubkey **imminent but not yet present** — bouncing loses mail the sender never retries. What bounds the deferral is **not** a nest-side timer: the nest holds no expiry state and never converts the tempfail into a bounce on its own — the window closes only when the successor completes their first sign-in and re-provisions a key (nest-side), or independently when the sender's own outbound queue gives up retrying (its lifetime, not ours). The nest distinguishes the two situations by whether `new_actor_id` in `actor_successions` names this actor (`CacheDb::is_succession_new_actor`), never by string-matching the bridge's own error text. **The inbound MX arm's partial-failure guarantee****:** like submission's RCPT-time check (§ Recipient handling on submission), this recipient's tempfail is answered at RCPT TO alone — dropped from the envelope, the other recipients of the same multi-RCPT message unaffected — instead of failing the whole DATA transaction after earlier recipients had already been ingested (which used to cause every sender retry to re-deliver to them); `preflightRecipientSealKeys` re-checks every remaining recipient's keys once more in `Data`'s own pre-loop pass, immediately before the ingest loop starts. That narrows the partial-commit window from the sender-controlled RCPT→DATA gap down to the sub-second preflight→ingest-N gap inside this one handler — it does not close it to zero: `ingestForRecipient` re-resolves each recipient's own keys again on its own, and a key revoked inside that narrower window still tempfails/rejects the whole transaction there, after any earlier recipients in the same loop were already ingested. Closing that residual is the per-recipient ingest idempotency named as a follow-up at § Recipient handling on submission's *DATA-time residual* row, not yet built here either |
| Per-mailbox inbound quota exceeded (enforced at `ingest_inbound_mail`) | `552 5.2.2` | permanent | role-address deliveries bypass the per-mailbox quota pre-check (§ abuse@ / postmaster@ routing) |
| Relay denied (domain ≠ LocalDomain) | `550 5.7.1 relay denied` | permanent | normal reject path |
| Rate limit | `421 4.7.0` | tempfail (close conn) | sender retries from another IP / later |
| FCrDNS fail in `enforce` mode (`mail.inbound.fcrdns_mode`) | `550 5.7.25 No matching reverse DNS` | permanent | rejected at first MAIL FROM; verdict `rejected_fcrdns`; resolver errors fail-open |
| HELO invalid | `554 5.7.0 Invalid HELO/EHLO` | permanent | rejected at first MAIL FROM; verdict `rejected_helo` |
| STARTTLS missing in `required` mode | `530 5.7.10 STARTTLS required (RFC 3207)` | permanent | rejected at first MAIL FROM; verdict `rejected_no_starttls` (target — § Metrics surface: no `smtp_inbound_mta_sts_violations_total` counter exists today) |
| Sender domain has no MX/A | `550 5.7.1 Sender domain has no DNS records` | permanent | resolver fail-open: counter `smtp_inbound_sender_domain_fail_open_total{reason}` |
| Malformed sender (no `@`) | `554 5.1.7 Invalid sender address` | permanent | verdict `rejected_sender_domain` |
| Unknown sender to a supervised recipient whose guardian set `unknown_sender_mail=reject` | `550 5.7.1` | permanent | rejected **per-recipient at RCPT TO** by the resolver (§ Inbound policy stack), so co-recipients of the same message are unaffected. Policy semantics + the known-sender set are owned by [`family-safety.md`](family-safety.md) § The mail gate — **built** (landed 2026-07-09/10) |
| DMARC reject (`p=reject` + `enforce_dmarc`) | `550 5.7.1 DMARC reject` | permanent | rejected on DATA by `mta/auth_enforce.go` (runs first, ahead of SPF/DKIM); verdict `rejected_dmarc`; honors `log_only`. A DMARC `p=quarantine` failure is filed to the recipient's Junk by the spam gate, not rejected |
| SPF hard-fail with `enforce_spf_hardfail` (and no DMARC decision) | `550 5.7.23 SPF reject` | permanent | rejected on DATA by `mta/auth_enforce.go`; verdict `rejected_spf_hardfail`; defers to DMARC; honors `log_only`. `SoftFail`/`Neutral` are score-only, never reject |
| DKIM-fail with `enforce_dkim` enabled (and no DMARC `p=` decision) | `550 5.7.20 DKIM signature missing or invalid` | permanent | rejected on DATA by `mta/auth_enforce.go` (`dkim_gate.go`); verdict `rejected_dkim`; honors `log_only`; opt-in because legit unsigned senders exist |
| Header section exceeds caps (count or bytes; or no `\r\n\r\n` separator before cap) | `554 5.6.0 Header section ...` | permanent | parser-bomb defense, runs before the daemon's parser ever sees the bytes |
| Header section carries other than exactly one From field (none, or two or more) — inbound DATA and submission alike | `554 5.6.0 Message must carry exactly one From header field` | permanent | RFC 5322 §3.6 allows one, and RFC 7489 §6.6.1 names rejection for more; given two, DMARC aligns against the first while every app displays the last. Runs right after the header-section caps, before either parser reads a From; inbound pays the tarpit like the caps. Rule, count and per-door table: § Architectural rules → *Exactly one From field* |
| Submission: the one From field names other than exactly one mailbox (a list, a group of two, a display name with no address) | `554 5.6.0 From field must name exactly one mailbox` | permanent | after the From-field count, before the `Received:` strip; rule and reasons: `mail-multidomain.md` § From: header ownership |
| Submission: `From:` on a domain the deployment signs for names an address the authenticated actor does not own | `550 5.7.1 Sender not authorized for this address` | permanent | the envelope check's code family and predicate (`assertSenderOwned`); `451 4.7.1` when the resolver is unreachable (fail closed); independent of the DKIM set; owner `mail-multidomain.md` § From: header ownership |

The strategy line is: **fail-tempfail when the nest's view of state is unavailable; fail-open only when the *external* state (DNS, DNSBL) is unavailable, and always with a counter.** Silent fail-open on RPC errors () is forbidden — it accepts mail we cannot store.

---

## Auth on each port

- **Port 25:** no AUTH advertised. `Auth()` returns `503 5.5.1 Authentication not supported` if the client tries.
- **Port 465 (implicit TLS) and 587 (STARTTLS-required):** SCRAM-SHA-256 preferred (future workstream), `OAUTHBEARER` + `PLAIN` over TLS only as the current shipping set. `AllowInsecureAuth = false` on both. Each AUTH attempt unwraps a per-credential wrapped submission token (`fauna.bridges.fetch_wrapped_submission_token`) AEAD-sealed under the MUA-supplied credential and inner-Ed25519-signed by the user's primary client; AEAD success + signature verify is the authentication signal. Substitution-resistance is baked in at three layers: pre-AEAD `blob.index` cross-check against the caller-resolved `(actor_id, credential_id)`, AAD-bound AEAD-open, and inner signature verify against `actor_id`-as-VerifyingKey (codebase invariant: `actor_id == 32-byte Ed25519 verifying-key`). Mirrors the IMAP MDA's AEAD-unwrap-as-AUTH path but uses a *separate* blob shape (`WrappedSubmissionTokenBlob`, `kind="submission-token"`) carrying no decryption capability — a compromised MTA cannot read mail.

  Per-(principal, credential_id, source_IP) AUTH-failure lockout: a failure budget per 1-minute fixed window (catalog `mail.auth.max_auth_failures_per_minute`); the first attempt past it returns SMTP `421 4.7.0` *before* the AEAD-unwrap step, denying an attacker the AEAD-timing oracle. The `principal` is the canonical `base@domain` the presented username resolves to (`auth.PrincipalKey`), so presentation variants of one credential share a bucket (§ B7). The counter resets on every successful AUTH for that key and on window rollover. Two coarser defense-in-depth buckets ride alongside it — per-(principal, credential_id) across all IPs at a small multiple of this limit and per-source-IP across all usernames at a larger multiple — to close the distributed/rotating brute force a per-triple bucket misses (§ F11; full mechanism in `mail-policy-config.md` § Submission policy AUTH-failure-lockout row). State is kept in the bridge process (mirrors C.2's `RateLimiter` shape); independent of nest-side rate-limits on `fetch_wrapped_submission_token` (which guards nest against the same attacker but cannot, by construction, fire before the wire roundtrip).

Per-actor submission quotas are wired (§ Architectural rules — a nest-side per-actor recipients/day quota via `fauna.bridges.check_submission_quota`, plus a per-message recipient cap read from the wrapped submission token; no bridge-local `actorRateLimiter` — see § Architectural rules for the corrected mechanism, 2026-07-19); the outbound DKIM RSA key-length floor is enforced at the sign site (`SignError::RsaKeyTooSmall`, § Architectural rules).

---

## Recipient handling on submission (per-recipient, RCPT-time)

The submission listeners (465/587) validate **local-domain** recipients at **RCPT TO** time, matching the inbound resolver's timing (`docs/goal/behavior/mail-aliases.md` § Resolution order at RCPT TO time): for a local address, recipient existence is knowable up front, so it is resolved per-recipient rather than deferred to DATA. This is what makes the partial-failure guarantee below hold.

- **Local-domain RCPT** (domain ∈ `local_domains`): the bridge calls `fauna.bridges.resolve_recipient(local_part, domain, sender_domain)` — the same fixed-order alias resolver the inbound MX path uses (the superset of the AUTH-time exact-only `validate_recipient`; `mail-aliases.md` § Resolution order). Three outcomes:
  - **Resolved** → a local mailbox actor. If the recipient has not provisioned an MLS public key (`fetch_recipient_mls_pubkey` → none) → `550 5.1.1 recipient has no encryption key on file`, per-RCPT — **unless** the actor is a succession's successor still inside the re-provisioning window bounded by their first sign-in, not by a nest-side timer (`succession_pending` on the reply), which tempfails `451 4.7.1` instead (§ Error / tempfail strategy). (Resolving recipient existence *and* encryption-key availability at RCPT is what keeps a key-less recipient from failing the whole DATA.) Any matched-alias `X-Fauna-Address-*` headers (subaddress / wildcard / disposable / catch-all) are stamped onto the sealed local copy at DATA, mirroring the inbound path — so a local user emailing `bob+tag@<our-domain>` now reaches bob (exact-only `validate_recipient` previously 550'd it).
  - **Forward** → an admin external forwarder (`mail-aliases.md` § Kind 7): a mailbox-less local address that redirects to an external target. It is dispatched through the **same shared forward pipeline** as the inbound path (copy_mode=`redirect`, attributed to the managing admin, SRS-rewritten nest-side at queue-out, NDR to the admin) and writes **no local copy** — so a local user emailing `info@<our-domain>` reaches the external destination uniformly with inbound MX delivery (`mail-forwarding.md` § Admin external forwarders). The submission envelope sender is the authenticated user (never null — `Mail()` rejects `MAIL FROM:<>`), so the SRS encoding names the user as the original sender.
  - **Reject** → `550 5.1.1 user unknown` (or the nest-supplied code/reason — disabled / expired) on that one RCPT; the recipient is dropped from the envelope, the others unaffected.
  - `resolve_recipient` / `fetch_recipient_mls_pubkey` RPC or transport error → `451 4.7.1 try again later` on that RCPT (nest-side state unavailable; never silently accept undeliverable mail — same posture as the inbound Error/tempfail table).
- **External-domain RCPT**: accepted at RCPT (a remote mailbox's existence is not knowable at submission time) and delivered via `enqueue_outbound_mail`; permanent failures surface as an NDR per § Permanent-failure bounce generation. External is the **only** recipient class that can produce a delayed bounce from a submission.
- **Partial-failure guarantee**: one invalid recipient never fails delivery to the others. The server rejects only the bad RCPT and returns `250` at DATA, delivering to every accepted recipient (local + external). Whether the submitting MUA then proceeds or holds the whole send on a per-RCPT `550` is client-dependent — Thunderbird may prompt the user to correct the address before sending — but the server never silently drops mail and never defers the bad-address signal to a later bounce.
- **DATA-time residual**: with existence + key-availability resolved at RCPT, the only failures left at DATA are genuinely transient (nest unreachable mid-transaction, HPKE-seal error) — plus the one **permanent** size case: a message over the inline ceiling answers `552 5.3.4` (same two-layer enforcement as inbound — § Message size limits), so the submitting MUA surfaces "too large" to the user immediately instead of retrying. The transient class tempfails the whole DATA (`451 4.7.0`) and the MUA retries. Per-recipient idempotency on that retry is a follow-up (the inbound-write path carries no `original_msgid` dedup yet), so a transient failure after a partial dispatch can re-deliver to the already-served recipients — the same narrow window the pre-RCPT-validation code already had, not widened here.

The Sent copy for the sender's own actor is always emitted (`submit_inbound_mail`) and is independent of the per-recipient validation outcome.

---

## Greylisting (implementation)

> The greylist **behavior** is specified in § Greylisting above (`:187–205`) — that is the authoritative target. This section is the implementation pointer.

**Realized (tracked internally):** the tuple-key derivation + defer/pass decision are pure shared Rust (`libs/fauna-mail/src/greylist.rs` — `tuple_key` / `decide`); the state is nest-side in `greylist_tuples(sender_domain, recipient, subnet, first_seen, last_attempt, accepted_at)`; the Go MTA forwards `(from, to, client_ip)` over `fauna.bridges.check_greylist` at RCPT TO (after `resolve_recipient`) and holds **no** local state, so greylisting is uniform across bridge restart. The nest handler reads `greylist_enabled` + `greylist_delay_secs` (= min-hold) from the live spam-policy override on every call (so the admin toggle applies immediately, no bridge re-fetch); the 4 h retry window + 30 d whitelist are compile-time catalog defaults until their admin write-path lands (Bucket C, `mail-policy-config.md`). A `check_greylist` transport error **fails open** (accept) — greylisting is itself a deferral, so the safe direction on a backend blip is to accept, not 451 (`smtp_inbound_greylist_check_fail_open_total`). Rows are GC'd by `spawn_greylist_retention_sweeper` (30 d, the whitelist window). **Role-address recipients bypass greylisting** (`:236`): the handler classifies the RCPT local-part via `classify_role_address` and returns `Pass` for any reserved role address (postmaster@/abuse@/…) before the tuple/decide. **Gap:** the per-(IP / domain) allowlist described in § Greylisting above (`:205`) is target-state only — no `greylist_allowlist` field, wire type, or admin-UI control exists anywhere in the codebase today; it is pending its own write-path (catalog row `mail.inbound.greylist_allowlist`, `mail-policy-config.md`).

---

## Connection-time limits

The **global concurrent-connection cap** applies to **every** bridge listener — the shared `internal/connlimit` semaphore wraps the inbound (25), submission (465/587), IMAP (993/143) and CalDAV (443) listeners alike (an OOM/FD backstop no surface should be without). The cap *value* differs by surface: port 25 (unauthenticated MX) stays a deliberately tight botnet brake (100), while the authenticated submission/IMAP/CalDAV surfaces use a generous backstop (4096, matching the Rust nest listener loop's global connection cap, a constant) because they legitimately hold many long-lived sessions (IMAP especially — one persistent IDLE connection per mailbox per device). The per-IP/per-subnet connection/message **rate** knobs, the error tarpit, and the header-section caps remain **port-25-only** — they are inbound-MX-perimeter defenses tuned for unauthenticated MX traffic. Port 25 *also* carries a per-IP **concurrent**-connection cap (compile-time `maxInboundConnsPerIP` = 50, half the global 100 — postfix's `smtpd_client_connection_count_limit` default), wired *inside* its global cap via `perIPInbound`: defense-in-depth so no single source IP can hold all 100 global slots even via a patient trickle-slowloris (pacing under the per-IP rate cap and trickling a command every < 30 s to dodge the read timeout). Unlike the authenticated surfaces' cap below it is a **fixed constant** — unauthenticated MX has no `AuthPolicy.max_conn_per_ip` to feed it — set generously so a legitimate sending MX's shared-egress-pool fan-in is never refused. The authenticated submission/IMAP/CalDAV surfaces instead carry two per-IP defenses suited to long-lived authenticated sessions: (1) a per-IP **concurrent**-connection cap — the app-set catalog row `mail.auth.per_ip_max_concurrent_conn` (wire `AuthPolicy.max_conn_per_ip`, default 256, `0` = disabled), enforced by the Go `internal/connlimit` per-IP limiter (the analogue of the Rust `fauna-conn-limit::PerIpConnLimit` the nest TLS loop + SNI router use), keyed on the PROXY-v2-restored real client IP and hot-reloaded on `config_changed` — which bounds one source to a fraction of the 4096 global pool (the per-IP complement to the global cap); and (2) the AUTH-failure lockout (the shared `internal/authlock`, `imap-server.md` / `caldav-server.md` § Authentication). A per-IP *rate* cap is deliberately **not** applied to these surfaces: a legitimate IMAP client opens many long-lived IDLE connections, so connection *simultaneity* (the concurrent cap), not new-connection *rate*, is the right per-IP axis — and abuse beyond that is gated at AUTH.

| Limit | Default | Source | Role |
|---|---|---|---|
| Global concurrent connections | 100 (port 25); 4096 (465/587/993/143/CalDAV) | (compile-time) | back-pressure connection floods; `internal/connlimit` acquires a semaphore slot before Accept, so excess connections wait in the kernel backlog rather than spawning goroutines. For the implicit-TLS surfaces (465/993/CalDAV) the cap wraps the raw socket **below** `tls.NewListener` so the serving library still detects `*tls.Conn`. Counters `smtp_connections_total{port,result}` (25/465/587) and `mda_connections_total{listener,result}` (IMAP/CalDAV) increment `accepted` per accept, `capped` when an Accept() blocks > 1s (`connlimit.CapWarnDelay`) |
| Per-IP connection rate | 10/min | nest config `SpamPolicyThresholds.max_conn_per_min` (admin-UI binding `mail.inbound.per_ip_conn_per_min`) | bridge-local rate limiter — **port-25-only** |
| Per-IP concurrent connections (authenticated surfaces) | 256 (`0` = disabled) | nest config `AuthPolicy.max_conn_per_ip` (admin-UI binding `mail.auth.per_ip_max_concurrent_conn`) | bridge-local per-IP limiter (`internal/connlimit`, the Go analogue of Rust `fauna-conn-limit::PerIpConnLimit`) wrapping the **465/587/993/143/CalDAV** listeners — shed-on-cap, loopback-exempt, keyed on the PROXY-v2-restored real client IP, hot-reloaded on `config_changed`. Port 25 carries its **own** per-IP concurrent cap (next row) — a fixed constant, not this nest-config knob. The per-IP complement to the global cap. **Implementation status: fully enforced. The Rust wire + nest projection landed 2026-06-04 (`AuthPolicy.max_conn_per_ip` round-trips through `put_auth_policy`→`fetch_config`, conformance-tested); the Go `internal/connlimit` per-IP limiter (`PerIPLimiter`/`PerIPListener`, one per bridge role, shared across that role's authenticated listeners) wrap landed 2026-06-04, hot-reloaded on `config_changed` (tracked internally, § B(b.2)).** |
| Per-IP concurrent connections (port 25) | 50 (compile-time `maxInboundConnsPerIP`) | (compile-time) | bridge-local per-IP limiter (`internal/connlimit`) wrapping the **port-25** listener *inside* its global cap via `perIPInbound` — shed-on-cap (`smtp_connections_total{port="25",result="per_ip_shed"}`), loopback-exempt. Half the global 100 (matching postfix's `smtpd_client_connection_count_limit` default of 50), so no single source IP can pin all global slots — defense-in-depth beside port 25's per-IP *rate* cap (10/min) + 30 s read timeout against a patient trickle-slowloris (one IP pacing under the rate cap and trickling a command every < 30 s). A fixed constant, not a knob: unauthenticated MX has no `AuthPolicy` to feed it, and it is internal anti-abuse tuning generous enough to never refuse a legitimate sending MX's shared-egress-pool fan-in (`principles.md` § One configuration surface). **Implementation status: ENFORCED (`runListenerWithBackend`).** |
| Per-IP message rate | 50/hour (target) | (compile-time — not yet wired) | **Implementation status: NOT enforced** (corrected 2026-07-19). No per-IP message-rate counter exists anywhere in the Go bridge (§ Implementation status today's Connection-time-limits gap note); the value is a target/catalog default, not live code. |
| Per-conn recipients | 100 | (compile-time) | `srv.MaxRecipients = 100` (`internal/mta/server.go`), applied to every listener built through `runListenerWithBackend` |
| Per-command read timeout | 30s | nest config (`mail.inbound.read_timeout_s`, knob pending) | slowloris defense — go-smtp resets the deadline before every line read. **Implementation status: ENFORCED on port 25** (`runListenerWithBackend` `srv.ReadTimeout = 30s`); value is compile-time until the `mail.inbound.read_timeout_s` knob lands. Submission keeps 5 min (authenticated). |
| Per-command write timeout | 5 min (`srv.WriteTimeout`) | (compile-time) | on response writes — not a slowloris vector (responses are tiny) and a slow-reading client must not fail a delivered message, so it's set generous rather than tight, on both the inbound (`server.go:183`) and submission (`submission.go:1337`) listeners. **Corrected 2026-07-19** — this row previously claimed 30s behind a `mail.inbound.write_timeout_s` nest-config knob; neither the 30s value nor that catalog knob exist (no such key in `mail-policy-config.md`; verified against code). |
| Max line length | go-smtp default, 2000 octets | (compile-time — target 1000, not yet set) | RFC 5321 §4.5.3.1.6 says ≤998; the target is to clamp tighter than go-smtp's default 2000, matching § Implementation status today's Connection-time-limits gap note — `MaxLineLength` is never assigned anywhere in the bridge today (verified 2026-07-19), so go-smtp's built-in 2000-octet default is what actually applies. |
| Max header lines | 256 | (compile-time) | parser-bomb defense (`checkHeaderSection`, `mta/server.go`), runs before the UniFFI RFC-5322 parser; not tunable per `mail-policy-config.md` § Compile-time decisions |
| Max header bytes | 1 MiB | (compile-time) | same |
| Max message bytes | 50 MiB | nest config `SpamPolicyThresholds.max_message_bytes` (admin-UI binding `mail.inbound.max_message_bytes`) | pre-parser size cap on **both** the inbound `inboundSession.Data` and the outbound `submissionSession.Data` (the latter added § B6 — an authenticated submitter could otherwise force a multi-GiB transient via the 3–4× strip/sign/parse copy); the read is `io.LimitReader`-bounded and bodies above reject with `552 5.3.4` before `parse_rfc5322` runs. The effective enforcement value is `max_message_bytes` alone (ceiling retirement, 2026-07-18, deleted the at-rest clamp — continuation records rest a sealed body of any size). A body over the *inline* ceiling is no longer refused: it rides the bulk-byte plane by reference. Same value is advertised in EHLO `SIZE` — § Message size limits |
| Tarpit base / cap | 250ms × n_errors / 5s | (compile-time) | `time.Sleep` before each per-session error response; `n_errors` is the count of policy rejections seen on this session |

**Tarpit applies to policy rejections only** (`inboundSession.reject`, `mta/server.go`). Every `5xx` permanent reject plus the deliberate greylist `451` pays the escalating sleep; accepted commands don't, and the counter doesn't reset on a success (a session that mixes errors with successes keeps accumulating). The infra-class `451` tempfails (`resolve_recipient` transport error, `verify_inbound` transient, ingest tempfail — "nest's view of state is unavailable") are deliberately **not** tarpitted: we never slow a legitimate sender's retry because our own backend blipped. Connection-fatal `NewSession` rejects (rate-limit `421`, DNSBL `554`) are also not tarpitted — flood-shedding is the concurrency cap's job, and holding the goroutine longer under a flood is counterproductive.

**`reject_unauth_pipelining` is not implemented.** go-smtp v0.24 unconditionally advertises `PIPELINING` in EHLO (`conn.go:254`), so postfix's check ("client pipelined commands before the receiver advertised PIPELINING") doesn't apply in our deployment — we always advertise it. A real implementation requires either a fork of go-smtp to make PIPELINING optional, or hooking the bufio reader to detect pre-banner command stuffing. Documented gap; not in this session's scope. The tighter `MaxLineLength = 1000` partially mitigates by rejecting 100-command stuffed-into-one-line attempts at the parser layer.

### Message size limits

**Moved 2026-08-03 to [`mail-message-size.md`](mail-message-size.md)**, which now owns
the `mail-message-size` concept. That doc holds the product ceiling, the inline
ceiling and its bulk-plane reference legs, continuation records at rest, and the
`552 5.3.4` enforcement points. The `max_message_bytes` row in the table above is the
perimeter's view of it; the EHLO `SIZE` advertisement follows the same value.

---

## FCrDNS (forward-confirmed reverse DNS)

Three modes, controlled by the nest-config knob `mail.inbound.fcrdns_mode` (default `score_signal`):

- `off` — no PTR / forward lookup; bridge does not annotate the session.
- `score_signal` — perform PTR lookup; if a PTR exists, forward-resolve it and check whether `client_ip` appears in the resolved address set. Failure (no PTR, PTR with no forward record, or forward-set missing `client_ip`) does not reject — it increments `smtp_inbound_fcrdns_total{verdict="failed"}` and is forwarded to nest as `fcrdns_failed=true` for spam-score weighting (weight 1, additive to existing DNSBL signals).
- `enforce` — same lookup as above; failure rejects at first MAIL FROM with `550 5.7.25 No matching reverse DNS`, verdict `rejected_fcrdns`. Resolver errors fail open with `smtp_inbound_fail_open_total{reason="fcrdns"}` (consistent with DNSBL fail-open posture).

Implementation lives in `bins/fauna-bridges/internal/mta/`. The `SpamConfig::reject_no_rdns` knob (deleted at I6 cutover) was superseded by the bridge's `fcrdns_failed` signal.

---

## Outbound delivery (MTA → external MX)

The MTA-role process is the outbound delivery engine. Submission ports (465, 587) hand a message to the MTA after AUTH + accept; the MTA queues it and delivers to the recipients' MX hosts. The queue itself is **nest-side state** — `outbound_mail_queue` rows in nest's SQLite, one per (message, recipient) pair — and the MTA polls work units via `fauna.bridges.fetch_outbound_due` RPCs on a schedule. The MTA holds no on-disk queue beyond its keypair file; on restart it resumes from nest's view of `next_attempt_at`.

The full wire surface for the queue lifecycle:

- `fauna.bridges.enqueue_outbound_mail(original_msgid, original_sender, recipients[], raw_message)` — submission `Data` hands the body off unsigned; nest inserts one row per recipient and returns the assigned row ids. The nest signs each row as it leaves through `fetch_outbound_due` — the one DKIM sign site, owned by `mail-bridge-lifecycle.md` § DKIM provisioning (automatic) → *Custody moves to the nest*.
- `fauna.bridges.fetch_outbound_due(max, lease_seconds)` — worker polls; nest returns due rows (`next_attempt_at <= now`, `status='pending'`).
- `fauna.bridges.mark_outbound_delivered(id)` — 2xx end-of-DATA on the wire to the recipient MX.
- `fauna.bridges.mark_outbound_failed(id, retry_after_seconds, last_error)` — 4xx / connection / TLS failure; nest reschedules per the retry schedule below.
- `fauna.bridges.mark_outbound_bounced(id, reason)` — 5xx, no-MX, or retry-budget exhausted; nest transitions to `bounced` and (subject to the NDR rate-limit below) emits a DSN.

**Prompt-drain nudge (nest → MTA push).** The `fetch_outbound_due` poll cadence is the universal drain backstop, but it adds up to one poll cycle of latency on a freshly-enqueued row. So when nest enqueues a remote-recipient row on the **interactive** path — a client `fauna.email.send` to an off-domain address — it emits a `fauna.bridges.outbound_ready` push to the approved **MTA-role** bridge (the outbound twin of the inbound `fauna.mail.received` arrival push). The MTA's outbound worker reacts by triggering a drain immediately, so a user's sent mail relays promptly rather than within ≤30 s. The push is **best-effort and carries no payload** — a disconnected MTA simply drains on its next poll; the push is never a correctness dependency. It is MTA-only (the MDA runs no outbound worker), and is *not* emitted from the background enqueue paths (bounces/DSNs, forwards, TLSRPT reports, security mail), whose drain latency is unobserved — those rely on the poll.

**Operational deliverability concerns** (IP warm-up on a fresh deployment, periodic blocklist self-check, symptom diagnostics) are spec'd in `docs/goal/behavior/mail-deliverability.md`; this section owns the retry machinery + queue lifecycle, that doc owns the operational layer atop.

### Retry schedule

Per-recipient (not per-message — a single inbound message to ten recipients is ten independent retry curves). Exponential backoff with jitter:

| Attempt | Delay before this attempt | Cumulative wall-clock |
|---|---|---|
| 1 | 0 | 0 |
| 2 | 5 min | 5 min |
| 3 | 15 min | 20 min |
| 4 | 1 hour | 1h 20m |
| 5 | 4 hours | 5h 20m |
| 6 | 12 hours | 17h 20m |
| 7 | 1 day | 1d 17h 20m |
| 8 | 1 day | 2d 17h 20m |
| 9 | 1 day | 3d 17h 20m |
| 10 | 1 day | 4d 17h 20m |
| 11 | final attempt | **5d** → permanent failure → bounce |

Delays carry ±10% uniform jitter so a transient downstream outage doesn't synchronize the entire deployment's retries. The **permanent-failure timeout is 5 days** (RFC 5321 §4.5.4.1 ceiling: "Retries continue until the message is transmitted or the sender gives up; the give-up time generally needs to be at least 4–5 days").

A **delay warning** ("your message hasn't been delivered yet") is generated at the **4-hour mark** per recipient — sent to the original sender, RFC 3464 DSN with `Action: delayed`, `Status: 4.4.7`. The warning is per-recipient + once-per-message; subsequent delays in the same message don't re-warn.

The retry schedule is admin-tunable via mail-policy-config (`outbound.retry_schedule_seconds`, `outbound.permanent_failure_timeout_hours`, `outbound.delay_warning_at_hours`) but the **defaults above are the shipping defaults** — any admin who deviates is opting into edge cases for their own reasons.

### Permanent-failure bounce generation

A bounce (Non-Delivery Report — NDR) is generated for the original sender when:

1. The retry schedule above exhausts (final attempt fails on a 4xx or a connection failure).
2. **At any retry** if the recipient MX returns a 5xx that classifies as permanent (RFC 5321 §4.2.2 — `5XX [enhanced-status]` where the enhanced status is in the 5.x.x range and not in the admin allowlist of "treat-as-transient" codes).

**Format** is RFC 3464 multipart/report DSN:

```
Content-Type: multipart/report; report-type=delivery-status; boundary="..."

--<boundary>
Content-Type: text/plain; charset=utf-8

[Human-readable: "Message to <recipient> was not delivered after 5 days of retries. The remote server said: <last-error>"]

--<boundary>
Content-Type: message/delivery-status

Reporting-MTA: dns; <our-domain>
Arrival-Date: <original-submission-time>

Final-Recipient: rfc822; <recipient>
Action: failed
Status: <enhanced-status-code>
Diagnostic-Code: smtp; <last-wire-response-from-recipient-mx>
Last-Attempt-Date: <iso8601>

--<boundary>
Content-Type: message/rfc822-headers

[Original message headers — body is NOT included for size reasons; RFC 3464 allows headers-only when the body might be large]

--<boundary>--
```

The bounce envelope is the **null sender (`<>`)** per RFC 5321 §4.5.5 (bounces never themselves bounce — a bounce-of-a-bounce loop is the backscatter problem). RCPT TO is the original sender's address.

### NDR rate-limit per recipient

To avoid being a backscatter source: one bounce per `(original-sender-address, original-message-id)` in the **admin-configured `ndr_rate_limit_days` window (default 7 days)**. State is `bounce_history(original_sender, original_msgid, sent_at)` in nest (`bins/fauna-nest/src/db/outbound.rs`, real table + window query, tested). A second permanent-failure event in the same window emits the bounce on the **bridge log only**, not on the wire, and records the suppression as `verdict="suppressed_rate"` in that table — the `smtp_outbound_bounces_total{verdict="suppressed_rate"}` **metric counter itself does not exist** (§ Outbound metrics below). RFC 5321 §3.5 doesn't mandate this — it's a project-policy bound against amplification.

**The window is now the admin-facing `ndr_rate_limit_days` knob, not a compile-time constant (fixed 2026-08-25, row 434).** `generate_permfail_bounce` (`bins/fauna-nest/src/outbound_bounce.rs`) reads `state.db.get_outbound_policy().await?.effective().ndr_rate_limit_days` once per call and threads the resulting window into both `bounce_rate_limit_hit` call sites (the to-original-sender DSN and the forwarder NDR) — the same nest-side `Overrides::effective()` overlay pattern its sibling knobs in § Outbound retry curve above use (`retry_schedule_seconds` / `permanent_failure_timeout_hours` / `delay_warning_at_hours` → `outbound_retry::retry_policy_from_outbound`). A stored `0` rides through as a live `0` (no rate-limiting beyond a same-instant duplicate) with no floor — a legitimate admin allowance choice, unlike `permanent_failure_timeout_hours`'s floored `0`. An admin who changes the NDR rate-limit window in their app now sees it actually govern bounce suppression. Regression: `bounce_rate_limit_respects_admin_ndr_rate_limit_days_override` (`outbound_bounce.rs`).

### Backscatter suppression

A bounce is **not generated** when any of the following held for the original message at inbound time:

1. SPF on the inbound side returned hardfail (sender domain explicitly disowned the source IP).
2. DMARC alignment failed and the DMARC policy was `reject` or `quarantine`.
3. The MAIL FROM address was a null sender (`<>`) — we never bounce a bounce.
4. The original message was forwarded *to* us (not submitted by an authenticated user) AND the inbound delivery was rejected with a 5xx the original sender's MTA would have re-bounced themselves — RFC 7489 §3.1's "do not generate DSNs to potentially forged senders" rule.

In those cases the failure is logged as a `suppressed_backscatter` `bounce_history` row (not a `smtp_outbound_bounces_total` metric — see § Outbound metrics) and never reaches the wire. Admins can opt their deployment out of any specific suppressor via mail-policy-config (defaults are all-on); doing so accepts backscatter responsibility.

### Postmaster CC

**Never auto-CC.** Postmaster reports are an amplification vector and a legacy convention; modern mail-server admins have no expectation of receiving a copy of every bounce. Postmaster mail (delivery-status, abuse-reports, MTA-MTA negotiation logs) is exposed to the admin via the admin pane in their Fauna app (per `mail-policy-config.md`) and via the structured log stream, not as a CC on outbound bounces.

A per-account "BCC postmaster on my own bounces" opt-in is **not** in v1 scope; users who want their bounces visible read them from their own Inbox (bounces land there normally per the null-sender flow).

### MX resolution + IPv4/IPv6 mixed handling

Per recipient domain at delivery time:

1. **Resolve MX records** for the recipient's domain (RFC 5321 §5.1). Sort by `priority` ascending. Implicit MX (no MX RR, A/AAAA only) is handled per RFC 5321 §5 — use the A/AAAA records directly with priority 0. **A *failed* MX lookup is not a "no MX RR" answer.** Implicit MX fires only when the resolver authoritatively reports that the domain publishes no MX records; SERVFAIL, timeout, or no route says nothing about what it publishes, and must tempfail into the retry curve. Collapsing the two delivers mail to the recipient domain's own A record on any DNS hiccup — for any domain whose real MX is a third party, a host that was never meant to receive its mail. Both implementations now hold this: the Go MTA's `LiveMXResolver` takes the implicit-MX branch only on `dnsErr.IsNotFound`, and shared Rust's `fauna_mail::outbound::mx::resolve_mx` propagates the resolver's error (it had silently used `unwrap_or_default()` until 2026-08-22 — dark, so never live; `git log --grep "a failed MX lookup is not an implicit MX"`). A resolver written against the Rust `MxResolver` trait therefore owes the same split: "no records" → `Ok(vec![])`, everything else → `Err`.

   **The MX RRset is resolved WITH DNSSEC validation, and its provenance travels with the hosts.** RFC 7672 §2.2 requires *both* DANE legs to be secure: an SMTP client whose MX RRset was not DNSSEC-validated MUST NOT treat the destination as DANE-capable. Validating only the TLSA lookup authenticates the name the MX answer happened to carry — a DNS-spoofing attacker (exactly the attacker DANE exists to stop) forges `MX victim → mx.attacker`, publishes a genuine DNSSEC-signed TLSA for **their own** name, and the pin then succeeds *honestly* against the wrong host. So **nest owns this lookup too**, beside `fetch_tlsa` and for the same stated reason — the Go stdlib resolver cannot validate. The MTA bridge calls `fauna.bridges.resolve_mx(domain)`; nest runs `fauna_mail::outbound::mx::lookup_mx_secure` (`ResolverOpts.validate = true`, per-record `Proof::Secure`) and replies with the ranked hosts plus a per-RRset `secure` bool. `secure: false` **narrows the TLS posture, it never withholds delivery** — the hosts still travel and the attempt proceeds at the MTA-STS / opportunistic posture, the same fallback an Insecure *TLSA* answer already takes. Two answers are `secure: true` besides a validated RRset, both because no DNS answer chose the name: an **implicit-MX** result (the domain publishes no MX RRset, so the target is the recipient domain itself, straight off the envelope), and an **operator-hatch `mta_mx_override`** static route, which is local configuration and bypasses DNS entirely. `LiveMXResolver` — the Go stdlib path — is retained but reports `secure: false` unconditionally and is no longer the production resolver.
2. **DANE TLSA lookup** at `_25._tcp.<mx>.` per recipient host (per the existing architectural rule on DANE).
3. **MTA-STS policy fetch** per recipient domain (RFC 8461 — `_mta-sts.<domain>` TXT for policy id, `https://mta-sts.<domain>/.well-known/mta-sts.txt` for the policy itself). Cache per `max_age` value in the policy.
4. **For each MX host in priority order:** resolve A + AAAA. Per RFC 8305 (Happy Eyeballs) attempt one IPv6 connect and one IPv4 connect with a 250 ms preference delay favoring IPv6; whichever connects first wins. Failure on one MX host falls through to the next-priority MX, not to A-fallback on the same host. (Two MX hosts at the same priority round-robin per RFC 5321 §5.1.)
5. **TLS handshake** with the connected host, applying DANE pin (if TLSA records secured) or MTA-STS posture per the existing architectural rules.

**MTA-STS enforcement** (RFC 8461 §5, applied per host before the TLSA lookup and TCP race so a refused host wastes no network work):

- **`mode=enforce`** — if the connected MX hostname does not match any `mx:` pattern (RFC 8461 §4.1: case-insensitive, trailing-dot tolerant, `*.<base>` wildcards match exactly one prepended label), refuse this host. The outer per-host loop falls through to the next MX (refusal is per-host, not a permanent bounce; only after all MX hosts on this attempt are exhausted does the queue retry-or-bounce flow apply).
- **`mode=testing`** — record the mismatch but proceed with delivery to this host. Differs from `mode=enforce` only in the side-effect.
- **`mode=none`** — no enforcement; treat as if no STS policy were published.
- **Published-but-broken policy** (TXT advertises `v=STSv1` but body fetch / DNS / parse failed) — treat as no policy for delivery decisions per RFC 8461 §5; the failure is recorded for TLSRPT but never forces plaintext fallback or refusal.

A delivery attempt is a successful TLS + `MAIL FROM` + `RCPT TO` + `DATA` + `.` + 250-final-response across one MX host. Per-MX-host timeouts: 30 s connect, 60 s per command, 600 s total. Failure on connect / TLS / command falls to the next host (or the next attempt in the retry schedule, if all hosts are exhausted at this attempt).

**IPv4-only sender behavior:** if the deployment has only IPv4 outbound connectivity (admin-set `ipv6_enabled`), AAAA records are skipped silently; the next attempt re-resolves so transient IPv6-routing improvements take effect.

### TLSRPT outbound reporter (RFC 8460)

The MTA generates a daily aggregate TLSRPT report **per recipient domain that publishes a `_smtp._tls.<domain>` TLSRPT policy**:

1. Fetch the recipient's TLSRPT policy at the start of each delivery day (cached for 24 h).
2. Aggregate all outbound delivery attempts to that domain through the day, bucketed by (`policy_type`, `policy_string`, `failure_type`) per RFC 8460 §4.4.
3. At day's end (00:00 UTC, jittered per-domain to avoid stampede), serialize the JSON report per RFC 8460 §4.4 / IANA-registered schema, gzip it, and submit per the policy's reporting URI:
   - `mailto:` URI — send as an attached `application/tlsrpt+gzip` message from `tlsrpt@<our-domain>` (the postmaster role address). `Subject: Report Domain: <recipient> Submitter: <our-domain> Report-ID: <uuid>`.
   - `https:` URI — POST with `Content-Type: application/tlsrpt+gzip` and `Content-Encoding: gzip`.
4. Log the report's existence; retain the raw JSON for 7 days in `tlsrpt_outbound_reports` for ad-hoc admin inspection.

**`result-type` values per RFC 8460 §4.3.** TLS-handshake outcomes map to one of: `starttls-not-supported`, `certificate-host-mismatch`, `tlsa-invalid` (DANE-active validation failure), `validation-failure` (no DANE, no STS), or — when an enforce-mode MTA-STS policy is active — `sts-webpki-invalid` (WebPKI failure under STS). Pre-TLS STS-specific signals: `sts-policy-fetch-error` (TXT advertised STS but the policy body fetch failed), `sts-policy-invalid` (body fetched but unparseable), `sts-policy-mismatch` (connected MX not listed in the policy `mx:` patterns; emitted under `enforce` and `testing`). Pre-TLS signals are recorded as their own per-attempt buckets and do not block delivery in `testing` / fetch-error / invalid cases — only `enforce` mismatches refuse this host.

Report generation is **on by default** (RFC 8460 is non-coercive; sending reports is the cooperative behavior). Admins can disable per-domain via mail-policy-config; disabling globally is allowed but discouraged.

The reverse direction (receiving TLSRPT reports about our own MX) is the inbound listener; see § Inbound perimeter hardening.

### Outbound metrics

**Corrected 2026-07-23: this entire table is target-state, not built.** `libs/fauna-mail/src/outbound/metrics.rs` defines these five names as label-string *constants* only (a "contract... every emitter and every dashboard consumer agrees on" per its own module doc), but its own doc comment says the actual Prometheus registration + increment call sites are still the unstarted "Task 9b sub-task" — and a repo-wide grep confirms zero call sites reference any of these five constants anywhere outside that one file. None of the five names are registered in the Go bridge's metrics registry either (`bins/fauna-bridges/internal/metrics/metrics.go`, 14 real counters, none matching — same registry § Metrics surface above already reconciled). The underlying *mechanisms* these would-be counters describe are real and tested (bounce suppression → `bounce_history` rows, outbound retry/queue state, TLSRPT dispatch) — only the metrics-surface instrumentation on top of them is unbuilt.

| Counter | Labels | Notes |
|---|---|---|
| `smtp_outbound_attempts_total` | `verdict` (`delivered`, `tempfail_4xx`, `tempfail_connect`, `permfail_5xx`, `permfail_policy`) | target-state: one increment per per-recipient delivery attempt, once wired |
| `smtp_outbound_bounces_total` | `verdict` (`sent`, `suppressed_rate`, `suppressed_backscatter`) | target-state: NDR generation outcomes, once wired — the underlying `bounce_history` rows are real today |
| `smtp_outbound_queue_depth` | `state` (`pending`, `retrying`, `failed`) | target-state: gauge sampled from nest, once wired |
| `smtp_outbound_attempt_seconds` | `verdict` | target-state: histogram, once wired |
| `tlsrpt_reports_sent_total` | `transport` (`mailto`, `https`) | target-state: TLSRPT dispatch is nest-side Rust (`bins/fauna-nest/src/outbound_tlsrpt.rs::spawn_tlsrpt_daily_dispatch`), which emits no metrics registry at all today, so this one couldn't live in the Go bridge's registry even once wired |

New outbound counters land here; the inbound counters live in § Metrics surface above.

---

## Bridge ↔ nest transport

The MTA bridge talks to nest over **WS-RPC + DAG-CBOR** on `/api/v1/ws/{actor_id}`, authenticated by a challenge-response signed with the bridge's enrolled service-user Ed25519 keypair, with a per-role method allowlist enforced nest-side. The wire format, pre-identity handshake, reconnect semantics, and allowlist mechanics are owned by `docs/goal/architecture/transport.md` § WS-RPC; the bridge-as-client concept (enrollment, role discovery, capability boundaries) is owned by `docs/goal/architecture/apps/bridges.md`. The inbound SMTP path calls the `fauna.bridges.*` kinds — `resolve_recipient`, `check_greylist`, `ingest_inbound_mail` (which also enforces the per-mailbox inbound quota) — plus, target-state, the unbuilt verdict-stream `fauna.bridges.report_smtp_verdict` (§ Log shape); the submission path calls `resolve_recipient` (RCPT-TO) plus `enqueue_outbound_mail` / `fetch_outbound_due` / `mark_outbound_{delivered,failed,bounced}` (see § Outbound delivery for the queue lifecycle); `validate_recipient` is the AUTH-time exact resolver the submission/IMAP/CalDAV login paths use. RPC errors map to the tempfail codes in § Error / tempfail strategy: nest-state unavailable → `451`/`452`; external-state (DNS, DNSBL) unavailable → fail-open.

## Process hardening (systemd)

In bare-metal deployments the mail-bridge runs as a dedicated system user `fauna-bridge` (separate from `fauna`, the nest's user) under a hardened systemd unit. In the published Docker image it runs under s6-overlay as its own per-role non-root UIDs — `fauna-mta` (MTA) and `fauna-mda` (MDA), each distinct from `fauna`, the nest's own UID (see `docs/goal/architecture/installers/docker.md` § s6-overlay Services). The hardening directives for the systemd unit:

- `User=fauna-bridge`, `Group=fauna-bridge` — no membership in any other group; cannot read `/var/lib/fauna/` (the nest's identity store).
- `AmbientCapabilities=CAP_NET_BIND_SERVICE`, `CapabilityBoundingSet=CAP_NET_BIND_SERVICE` — only the capability needed to bind 25 / 465 / 587 (the bridge runs no ACME client and binds no port 80 — see § Architectural rules' ACME bullet: nest alone runs the HTTP-01 client).
- `ProtectSystem=strict`, `ProtectHome=true`, `ReadWritePaths=/var/lib/fauna-bridge` — read-only filesystem except the bridge's own data directory.
- `PrivateTmp=true`, `PrivateDevices=true`, `ProtectKernelTunables=true`, `ProtectKernelModules=true`, `ProtectKernelLogs=true`, `ProtectControlGroups=true`.
- `MemoryDenyWriteExecute=true`, `LockPersonality=true`, `RestrictRealtime=true`, `RestrictNamespaces=true`.
- `RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6` — no AF_NETLINK / AF_PACKET / AF_RAW.
- `SystemCallArchitectures=native`, `SystemCallFilter=@system-service`, `SystemCallFilter=~@debug @mount @cpu-emulation @keyring`.
- `NoNewPrivileges=true`.

The mail-bridge holds no decryption capability beyond what the MDA AUTH path unwraps per-session; compromising the bridge does not compromise nest's keys. The systemd-unit packaging for the bare-metal path lands with `installers/linux-nest.md`'s mail-bridge entries; the Docker image is the recommended deployment.

---

## Log shape

One JSON line per transaction at the natural rejection point (`Rcpt`, `Data`) and at the end of `Data` for accepted mail. Emitted via `slog` to stderr (box-owner-facing logs). **Target-state:** also shipped over WS-RPC to nest via `fauna.bridges.report_smtp_verdict` for the admin-pane dashboard rollup (per `docs/goal/behavior/mail-observability.md` § Data source — the verdict record is the raw source-of-truth for both surfaces); **that RPC is unbuilt today** (no allowlist entry, no bridge caller — § Implementation status today), so the verdict stream is stderr-only. Schema (`SmtpVerdict`, target contract for `bins/fauna-bridges/internal/mta/`):

| Field | Type | Notes |
|---|---|---|
| `client_ip` | string | extracted by `extractConnIP` |
| `helo` | string | from `gosmtp.Conn.Hostname()` |
| `mail_from` | string | envelope sender |
| `rcpt_to_hash` | string | SHA-256 of joined RCPT TOs (sorted, lowercased, comma-joined). Raw RCPT addresses are never logged. |
| `message_id` | string | from `Message-ID:` header if present, else `""` |
| `bytes` | int64 | DATA body size |
| `tls_version` | string | "TLS 1.3" / "TLS 1.2" / "" if cleartext |
| `tls_cipher` | string | RFC 8446 / 5246 cipher name; "" if cleartext |
| `spf` | string | `pass` / `fail` / `softfail` / `neutral` / `none` / `temperror` / `permerror` (from the `verify_inbound` UniFFI verdicts) |
| `dkim` | string | `pass` / `fail` / `none` / `temperror` / `permerror` (from the `verify_inbound` UniFFI verdicts) |
| `dmarc` | string | `pass` / `fail` / `none` (from the `verify_inbound` UniFFI verdicts) |
| `arc` | string | `pass` / `fail` / `neutral` / `none` / `temperror` / `permerror` (from the `verify_inbound` UniFFI verdicts, RFC 8617) |
| `dnsbl` | string | `listed` / `unlisted` / `fail-open` |
| `verdict` | string | `accepted` / `rejected_relay` / `rejected_rcpt` / `rejected_quota` / `rejected_rate` / `rejected_dnsbl` / `rejected_dmarc` / `rejected_spf_hardfail` / `rejected_spam_score` / `rejected_greylist` / `rejected_header_cap` / `rejected_helo` / `rejected_no_starttls` / `rejected_sender_domain` / `tempfail_validate` / `tempfail_quota` / `tempfail_dns` |
| `reason` | string | free text for the verdict |

**Ratified (2026-07-08):** the recipient-list hash is salted with a per-deployment value derived from the nest's private key — prevents rainbow-table reversal without breaking cross-correlation inside one nest's logs. Today's code is unsalted SHA-256; the salt lands with the verdict-stream emission work (§ Implementation status today — the `SmtpVerdict` wiring gap).

---

## Metrics surface

Exposed at `GET /metrics` on a separate listener bound to localhost only (decision in 01: separate listener, not the nest admin interface — box-owner-only, no auth required because it's loopback-only). Prometheus text format, emitted from a tiny in-process counter registry (`bins/fauna-bridges/internal/metrics/metrics.go`). The counters are the contract, the registry is an implementation detail.

**Loopback-only and box-owner-side**: the admin's Fauna app never scrapes this endpoint — the in-app dashboard (`docs/goal/behavior/mail-observability.md`) is sourced from the verdict-stream WS-RPC pipeline, not Prometheus. The two surfaces are independent and serve different audiences (a box owner with an existing Prometheus / Grafana stack scrapes `/metrics`; admins using the Fauna app see the in-app cards).

**Corrected 2026-07-22 (first independent check of this table since it was written — it never matched `metrics.go`, only word-substitution edits touched it before now):** the table below is the actual registry (`init()` in `metrics.go`), not the earlier draft's invented names.

| Counter | Labels | Cardinality |
|---|---|---|
| `smtp_connections_total` | `port`, `result` (`accepted`/`capped`) | bounded (3 ports × 2 results) |
| `mda_connections_total` | `listener`, `result` | bounded (IMAP/CalDAV; `imap-server.md` scope) |
| `smtp_inbound_messages_total` | `verdict` (the § Log shape `SmtpVerdict` string) | bounded by the verdict enum (~16) — registered with exactly one production call site (`internal/mta/server.go`'s greylist-defer branch, `WithLabelValues("rejected_greylist")`); no accept path and no other reject path increments it yet — see the note below the table. Renamed 2026-10-01 from `smtp_messages_total{direction,verdict}`, whose `direction` label no call site ever set to `outbound` |
| `smtp_inbound_sender_domain_fail_open_total` | `reason` | bounded — grows by resolver-error class (`timeout`, `resolver_error`) |
| `smtp_inbound_greylist_check_fail_open_total` | none | single counter |
| `smtp_inbound_srs_decode_total` | `outcome` | bounded (~6: `ok`/`not_srs`/`malformed`/`mac_fail`/`expired`/`orphan`; `mail-forwarding.md` § Bounce decode) |
| `smtp_inbound_filter_reject_total` | `mode` | bounded (3: `refuse_5xx`/`drop_multi`/`drop_null_sender`; § Email filter rules) |
| `smtp_inbound_autoreply_sent_total` | none | single counter |
| `smtp_inbound_autoreply_suppressed_total` | `reason` | bounded (~6: `rate` + the loop-guard reasons) |
| `wsrpc_calls_total` / `wsrpc_call_seconds` | `method`, `result` | bounded by RPC-kind count |
| `cgo_calls_total` | `symbol`, `result` | bounded by UniFFI symbol count |
| `bridge_role` (gauge) | `role` | bounded (2) |

**Not yet built:** the fine-grained per-verdict `SmtpVerdict` breakdown named in § Log shape (the ~16-value `rejected_*`/`tempfail_*` enum) is only partly counted — `smtp_inbound_messages_total`'s `verdict` label carries the named reason, but only `rejected_greylist` is ever set (§ Implementation status today's `SmtpVerdict` gap bullet covers the same wiring gap for the log line). `smtp_inbound_tls_version_total`, `smtp_auth_failures_total`, and `smtp_inbound_fcrdns_total` — all named in the earlier draft of this table — don't exist in the registry either. **`smtp_inbound_messages_total` itself is effectively unwired, despite being registered**: the single call site fires only on a greylist tempfail. Accepted mail and every other reject path leave it untouched, so it does not answer "is mail being delivered?" despite superficially looking wired. There is no outbound message counter.

The endpoint binds to `127.0.0.1:9090` by default (`main.go` `defaultMetricsBindAddr`), overridable via the localhost-only topology hatch `metrics_bind_addr` (OS-deployment topology, not nest config; no env-var path).

---

## Architectural rules

- **Strict beats permissive.** Default to the stricter RFC interpretation; reject before accepting. Compatibility quirks for non-conforming senders need explicit user approval and a counter so we can see how often the carve-out fires.
- **Security beats compatibility.** A reduction in the set of senders we accept is acceptable; a vulnerability is not. If a knob exists, default it to the most-secure setting and document the loosening cost on the alternative.
- The bridge does not store mail, attachments, or per-user state beyond ephemeral rate-limiter records. All persistent state crosses the RPC boundary.
- **Verification** of SPF/DKIM/DMARC/ARC runs in shared Rust (`libs/fauna-mail/src/auth.rs::verify_inbound`, using `mail-auth`), invoked over UniFFI from the Go bridge at DATA. **Enforcement** runs in the Go bridge's auth-enforce stage (`bins/fauna-bridges/internal/mta/auth_enforce.go`), which acts on the verdicts: DMARC-reject → 550 5.7.1, SPF-hardfail → 550 5.7.23, DKIM-fail → 550 5.7.20, in that order, all behind `!auth.log_only`. SPF and DKIM defer to DMARC (only fire when DMARC didn't decide); a DMARC *quarantine* failure is filed to the recipient's Junk by the spam gate (the `PolicyJunk` disposition), not a 5xx. There is exactly one verifier — the bridge does not re-verify.
- **Target state, not current behavior (corrected 2026-07-23 — this bullet named a nonexistent counter and overstated coverage).** The intent: every accepted-or-rejected transaction emits exactly one `SmtpVerdict` log line and exactly one per-transaction metrics increment, no transaction emitting two or zero. Today: `smtp_inbound_messages_total{verdict}` is registered (§ Metrics surface has the real 14) but has exactly one production call site (the greylist-defer path) and is not incremented on accept or on any other reject/tempfail path — see § Metrics surface's note below its table. The structured `SmtpVerdict` log line itself is also not wired as a distinct per-transaction emission (§ Implementation status today's `SmtpVerdict` gap bullet).
- DNS errors and DNSBL lookup errors fall open; nest-state errors fall tempfail.
- The mail-parser version is unified across the workspace at 0.11. Two RFC-5322 parsers seeing the same body is forbidden — this is the cryptographic-verification rule, where parser-disagreement = signature-disagreement = smuggling. The three RFC-5322 parsers in the codebase (Rust `mail-auth` for crypto, Rust `mail-parser` for data extraction, Go `emersion/go-message` for IMAP wire-response generation) each cover a distinct concern and are not interchangeable: the bridge's verification path uses `mail-auth` only (via `verify_inbound`), and nest's delivery/extraction path uses `mail-parser` only.
- The per-role method allowlist is enforced nest-side, keyed off the role nest assigned to the bridge's keypair (not anything the bridge claims). An MTA-role bridge cannot invoke MDA methods. See `docs/goal/architecture/apps/bridges.md` § Concept.
- The bridge MUST prepend a single canonical `Received:` header to the inbound message bytes before shipping to nest's `ingest_inbound_mail` RPC. Sender-controlled fields (`HeloDomain`, `ClientIP`) are sanitized — any CR / LF / non-printable byte forces the literal `unknown` placeholder, defeating header-injection forgery. *(Requirement met as of 2026-05-25 (tracked internally): the format + sanitizer live in shared Rust `libs/fauna-mail/src/received_header.rs::build_received_header` (uniffi-exported, inbound sibling of `outbound::received_strip`), prepended topmost in `internal/mta/server.go::Data` — see § Implementation status today. The retired `bins/fauna-bridge-imap` terminator originally implemented it as `BuildReceivedHeader` in `internal/smtp/received_header.go`.)*
- **The `X-Fauna-*` namespace — a sealed copy carries a delivery stamp only if the door that filed the copy wrote it (ruled 2026-09-23, widening the inbound-only rule below to every filing door).** Every door that seals bytes the nest did not compose runs the strip before it parses them and before it prepends its own genuine stamps: inbound MX DATA (`internal/mta/server.go::Data`), submission DATA on 465/587 (`internal/mta/submission.go`, beside the `Received:` strip, so the Fauna-recipient copies, the Sent copy and the relayed message are all clean, and the nest's hand-out signature covers the clean form), `fauna.email.send` (`bins/fauna-nest/src/email_handlers.rs`, once after the size ceiling and before the From count and every parse, so the in-domain copies and the Sent copy seal the same form), IMAP APPEND (`internal/mda/imap/append.go`, before `ParseRFC5322` — the dedup key is still taken over the literal as filed, so it agrees with the key a migration client derives over the same source bytes) and the migration import (`bins/fauna-nest/src/bridge_import_handlers.rs::import_one`, after the size and shape checks and before the parse and the seal). **Why the non-delivery doors strip too:** the stamps are trusted downstream without regard to which door filed the copy — the MDA's SELECT-time scorer reads `X-Fauna-Spam-Threshold` as *that message's* Junk tier (`0` switches auto-Junk routing off for it), and the alias-match stamps are what tells an MDA or app which alias a message arrived through — so a MUA's APPEND or a migration from a hostile source could file a copy whose forged stamp disables or forces Junk filing, or claims an alias match that never happened. A copy migrated from another Fauna nest loses that nest's genuine stamps: they were that nest's delivery policy over that nest's aliases, which this nest never vouched for, and the routing record proper survives in the `Received:` chain the copy also carries. `X-Fauna-Forwarded-By` keeps its carve-out at every door. The export's transit strip deliberately leaves the namespace alone (`mail-export.md` § UX shape step 2). The inbound rule, as first ratified: the bridge MUST strip every sender-supplied header in the reserved `X-Fauna-*` delivery-stamp namespace (`X-Fauna-Scan-*`, `X-Fauna-Address-*`, and any future stamp) from the inbound message bytes BEFORE parsing for the filter context and before prepending its own genuine stamps — the inbound sibling of the outbound `Received:` strip, generalizing "exactly one Fauna trace" from `Received:` to the whole namespace. A Fauna nest stamps these only at delivery (never on the wire), so any inbound copy is sender-forged; without the strip a forged `X-Fauna-Address-*` could match a recipient's alias-metadata filter rule or land in the sealed copy a client reads. **Exactly one carve-out: `X-Fauna-Forwarded-By` is NOT stripped** — it is the cooperative forward-loop trace that the inbound floor reads and `mail-forwarding.md` § Loop detection mandates is never stripped. One shared-Rust pure fn — `libs/fauna-mail/src/received_header.rs::strip_fauna_headers` (uniffi-exported `fauna_ffi::strip_fauna_headers`), called via `mailfauna.StripFaunaHeaders` at `internal/mta/server.go::Data` after the header-section cap and before `ParseRFC5322`, and at the four other doors named above. Case-insensitive `X-Fauna-` field-name prefix, RFC 5322 §2.2.3 continuation-aware, substring-safe (`X-Not-Fauna` survives); fail-safe toward stripping (any unrecognized `X-Fauna-*` is dropped). **The strip frames header lines exactly as the parser downstream does** — a line ends at LF with or without a preceding CR, a bare CR inside a line is content, the section ends at the first empty line — through the one shared walk every header strip uses (`libs/fauna-mail/src/header_walk.rs`), so a header section whose lines end in bare LF cannot carry a stamp past the strip that `ParseRFC5322` then reads. **The authenticated-sender stamp (ruled 2026-09-26) — the one stamp about *who sent it*:** `X-Fauna-Authenticated-Sender: <addr-spec>` (`libs/fauna-mail/src/sender_auth.rs`; the Go doors prepend `build_authenticated_sender_stamp` over UniFFI, the nest doors `authenticated_sender_stamp`) carries the address the filing door itself authenticated, lower-cased, and nothing else — never the `From:` header as such. Per door: inbound MX DATA stamps the `From:` addr-spec **only when the message's DMARC verdict is pass** (a DMARC none/fail or an ARC-only forward writes no stamp); submission DATA stamps the **envelope sender it validated as owned** by the authenticated actor (the submission `From:` header is not alignment-checked, so it is never what this stamp says); `fauna.email.send` stamps the `From:` address only when its in-domain handle gate fired and passed (the off-domain bypass writes nothing); the bridge-presented in-domain partition of `enqueue_outbound_mail` (`submit_outbound`) stamps the validated envelope sender the authenticated bridge presents — for an MDA auto-schedule fan-out the organizer the nest has bound `original_sender` to, for a list fan-out the list's own address, and nothing for the null-sender DSN / NDR / auto-reply callers; IMAP APPEND and the migration import write none — a copy they file names no authenticated sender. Absent means *unauthenticated*, never *unknown but fine*. Its one consumer, and the rule it enforces, are [`inbound-scheduling-authority.md`](inbound-scheduling-authority.md) § Who may mutate an existing event over the inbound rail → *The mail rail*.
- Header count and total header bytes are capped at the bridge BEFORE the UniFFI RFC-5322 parser ever runs (`checkHeaderSection`, `mta/server.go`: `maxHeaderLines = 256`, `maxHeaderBytes = 1 MiB`; compile-time constants, not tunable per `mail-policy-config.md` § Compile-time decisions). Over-cap messages — or no `\r\n\r\n` separator within the byte cap — get `554 5.6.0`; verdict `rejected_header_cap`.
- **Exactly one From field, counted before any parser reads one** (RFC 5322 §3.6; RFC 7489 §6.6.1). The rule above forbids two parsers disagreeing, and a second From field is the input on which ours do: `mail-auth` — whose parse `verify_inbound` hands to DMARC — takes the **first** From field, while `mail-parser` — every app's displayed sender, `sender_domain` / `from_norm`, submission's local-domain check (`submissionFromDomain`), and `fauna.email.send`'s handle gate — takes the **last**. Unrefused, `From: anyone@attacker.example` then `From: security@<own domain>` passes DMARC as the attacker and renders as the deployment, defeating § Implementation status today's perimeter property 3 for any `p=reject` domain. **The count** is one shared-Rust pure fn, `libs/fauna-mail/src/from_field.rs::from_field_count` (uniffi-exported; `mailfauna.FromFieldCount`), lexical and deliberately never below either parser's view: a line ends at LF with or without CR (both parsers take LF-only mail), a bare CR starts another candidate field, the name matches case-insensitively with whitespace allowed before the colon, an SP/HTAB line continues the previous field except on the first line, and the header section ends at the first empty line. Over-counting refuses only malformed mail; `libs/fauna-mail/tests/from_field_tests.rs` holds the count against both parsers over the review's twenty From shapes plus mixed line endings (a count of one ⇒ both choose the same address). **Per door** — the doors that make a trust decision about the sender refuse; the doors that file the account's own bytes do not: **inbound MX DATA** refuses `554 5.6.0` right after the header-section caps, tarpitted (`inboundSession.Data`; `TestInboundDataRefusesOtherThanOneFromField`); **submission 465/587** refuses `554 5.6.0` after the size guard, before the `Received:` strip (`submissionSession.Data`; `TestSubmissionDataRefusesOtherThanOneFromField`); **`fauna.email.send`** refuses `fauna.email.invalid_params` before the handle gate (`mail-app-surface.md` § First-party client send; `a_message_with_other_than_one_from_field_is_refused`); **`fauna.bridges.send_list_message`** refuses `fauna.bridges.invalid_params` before any quota is consumed — its DKIM key is picked by the From domain at the hand-out too (`mail-mass-mailing.md`); **IMAP APPEND** files the message as sent (`imap-server.md` § Write surface; `TestAppendFilesAMessageWithTwoFromFields`) — no authentication verdict is ever derived from an APPEND's From, all of that door's readers are `mail-parser` and agree (its `sender_domain` is the last From's, the one displayed), and refusing would strand mail a user files or imports. Mail already at rest is rendered as today: `conversations-at-rest.md` § Encryption at rest. The submission door additionally requires that one field to name exactly one mailbox, and that mailbox to be owned by the authenticated actor — owner `mail-multidomain.md` § From: header ownership.
- SMTP-smuggling resistance: only `\r\n.\r\n` ends DATA. Bare CR / bare LF terminators are NOT honored; they remain content. Pinned by regression fixtures in `internal/mta/` against go-smtp's normalization — `bare_lf_wire_test.go::TestWireBareLFHeadersCannotForgeFaunaStamps` writes raw DATA over a real listener carrying a bare-LF `.` line and bare-LF header lines, and asserts both reach `Data` as content (go-smtp hands them over unchanged, measured 2026-09-23). A future go-smtp bump that loosens this MUST trip those tests.
- HELO/EHLO is validated at first MAIL FROM (`validateHELOSyntax` in `internal/mta/policy.go`): empty / non-printable / non-ASCII / `localhost`-from-non-loopback / bare-hostname-from-non-loopback / IP-literal-mismatch all reject as `554 5.7.0`. Loopback peers are exempt from FQDN rules.
- HELO identity check augments the syntactic validation above: `ValidateHELOIdentity` performs DNS resolution of the HELO domain (skipping bracket-form IP literals and loopback peers) and rejects with `554 5.7.0 HELO domain does not resolve to peer IP` when the resolved A/AAAA set does not contain `clientIP`. Resolver errors fail open with `smtp_inbound_fail_open_total{reason="helo_identity"}`. Identity check shares the one-shot per-session gate with syntactic HELO validation — re-running on RSET → MAIL FROM is forbidden.
- Sender-domain MX/A is checked at MAIL FROM via `SenderDomainChecker` (`internal/mta/policy.go`), gated in `inboundSession.Mail` (`internal/mta/server.go`). MX is looked up first; absent MX falls back to A/AAAA (RFC 5321 §5.1 implicit MX). NXDOMAIN/NoData on every record type rejects as `550 5.7.1` (paying the per-session tarpit via `s.reject`); a malformed sender (no `@` / empty domain) rejects as `554 5.1.7`. Resolver errors fail OPEN (accept) with a `slog.Warn` + `smtp_inbound_sender_domain_fail_open_total{reason}` increment, the `reason` label bounded by error class (`timeout` / `resolver_error`). Two bypasses: the null sender `<>` (RFC 5321 §4.5.5 bounce traffic), and **loopback peers** — local relays / cron legitimately send from non-resolving sender domains, so the check is skipped for loopback clients (mirroring the HELO-identity loopback exemption above and postfix's `permit_mynetworks`-before-`reject_unknown_sender_domain` ordering). The check is unconditional otherwise (no mail-policy knob; compile-time always-on per `mail-policy-config.md` § Compile-time decisions). Runs on every MAIL FROM (the envelope sender can differ across an RSET), unlike the once-per-session HELO gates.
- Each per-session **policy** rejection is preceded by a `time.Sleep(min(n × 250ms, 5s))` tarpit (`inboundSession.reject`, `mta/server.go`), where `n` is the count of policy rejections seen on the session. Tarpit grows monotonically; success responses don't pay it, and infra-class `451` tempfails ("nest unavailable") are exempt so a backend blip never slows a legitimate sender's retry.
- Every bridge listener is wrapped in the shared `internal/connlimit` connection cap, bounding concurrent connections to a compile-time per-surface ceiling (port 25 `maxInboundConns` = 100; submission/IMAP/CalDAV `maxSubmissionConns`/`maxMDAConns` = 4096; not tunable). Above the cap, Accept() blocks; the kernel accept-backlog absorbs the burst. For the implicit-TLS surfaces (465/993/CalDAV) the cap wraps the raw socket below `tls.NewListener` so the serving library still sees a `*tls.Conn`. `smtp_connections_total{port,result}` (SMTP) and `mda_connections_total{listener,result}` (IMAP/CalDAV) count `accepted`/`capped`.
- Per-command read/write deadlines (`Server.ReadTimeout`/`WriteTimeout`) are set on the inbound server. go-smtp resets the read deadline before every `readLine()`, giving us per-command slowloris protection.
- The bridge↔nest transport is WS-RPC + DAG-CBOR with a challenge-response signed by the bridge's enrolled service-user keypair; nest enforces the per-role method allowlist. See `docs/goal/architecture/transport.md` § WS-RPC.
- In bare-metal deployments the bridge process runs as the dedicated `fauna-bridge` user with only `CAP_NET_BIND_SERVICE` (in the Docker image, as the `fauna` user with `setcap cap_net_bind_service`). It holds no decryption capability beyond what the MDA AUTH path unwraps per-session. Compromising the bridge does not compromise nest's keys.
- The bridge's bare-metal systemd unit is hardened with `MemoryDenyWriteExecute`, `SystemCallFilter=@system-service` minus `@debug @mount @cpu-emulation @keyring`, `RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6`, and the kernel-protection set (`ProtectKernel*`, `ProtectControlGroups`, `PrivateDevices`).
- TLS configurations across all bridge listeners (25, 465/587 submission, 993/CalDAV) share `MinVersion = TLS 1.2` and the same `bridgetls.Provider.GetCertificate` cert source, but — corrected 2026-07-23, see § TLS posture per port for the flow-traced gap — each of `internal/mta` and `internal/mda` builds its own `*tls.Config` literal rather than through one shared factory (no `bridgetls.HardenedConfig()` exists), and neither sets an explicit AEAD-only cipher list or curve preference. The uniformity that does exist today is accidental (both literals happen to be identical) rather than enforced by a shared factory; the cipher/curve pin is target-state.
- Cert reload is SIGHUP-driven through `bridgetls.Provider` (`internal/tls/tls.go` — corrected 2026-07-23; there is no `CertReloader` type): one instance per bridge process (`cmd/fauna-mail-bridge/main.go`), whose `GetCertificate` callback every listener the process runs (port 25 + 465/587 for an MTA-role process, 993/CalDAV for an MDA-role process — exactly one role runs per process) plugs into its own `tls.Config`. ACME and manual rotations both feed through this path; reload failure logs and keeps the previous cert (`Refresh` only stores on success, so a botched reload never zeros the active cert).
- Plaintext credentials that reach owned memory are zeroed on scope exit; the credential-handling zeroize audit lives in `docs/goal/architecture/security/zeroize-audit.md`.
- The dev-fallback self-signed cert generator (`bins/fauna-bridges/internal/tls/tls.go`) draws 128-bit random serials per RFC 5280 §4.1.2.2, derives the SAN's IP set from `net.InterfaceAddrs()` (cap 16) plus loopback, and uses 5-year CA validity / 1-year server validity. ACME replaces this in production; the dev cert is regenerated on demand and never persisted across machines.
- Supply-chain advisories are gated by `cargo audit --deny warnings` (RustSec) and `govulncheck` (Go vuln DB) jobs in `.github/workflows/supply-chain.yml`, alongside the existing `cargo-vet`. `--deny warnings` escalates unmaintained / informational advisories to fail too — a fail-loud posture; allowlisting is per-advisory and requires a stated reason.
- ACME — nest-issued cert delivered over WS-RPC. Nest runs a complete `instant-acme` HTTP-01 client (`bins/fauna-nest/src/acme_http01.rs`, multi-SAN over the apex + each active mail domain's `mail.<domain>`) and renews on the standard schedule. The bridge consumes that cert over WS-RPC via the sealed `TlsCertBlob` path — no second ACME client, no shared cert file, no env-var wiring. See `mail-bridge-lifecycle.md` § TLS provisioning.
- DKIM signing pipeline produces `c=relaxed/relaxed` per RFC 6376 §3.4.6. Pinned by `libs/fauna-mail/src/outbound/dkim.rs::tests::sign_uses_relaxed_relaxed_canonicalization`; a future mail-auth bump that changes the default OR a stray `.header_canonicalization(Simple)` call in our code makes the test fail.
- The submission send path's DKIM signature is **cross-implementation verified end-to-end** (the Stage-4 send proof). `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_submission_dkim_signature_verifies` (tier_3) delivers a submitted message through the real bridge to the in-process stub external MX, then runs an *independent* verifier (`dkimpy`, RFC 8463 ed25519 — never our `mail-auth` signer) over the delivered bytes against the domain's provisioned public key, asserting the signature **cryptographically verifies** AND is **aligned** (`d=` == the From domain). This is the in-CI proxy for Gmail's `dkim=pass` at Stage-5, not merely that a `DKIM-Signature` header is present (which is all `test_submission_round_trip` checks). The matching `v=DKIM1; k=ed25519; p=…` public DNS value is the one the nest publishes for the domain (`list_dkim_selectors`), read back by the `mail_bridge_mta` fixture and exposed on the handle (`dkim_public_dns_value` / `dkim_selector`), fed to a dkimpy `dnsfunc` so no real DNS query fires (Stage 2 publishes that same value verbatim). A companion `test_submission_multipart_dkim_verifies` proves the body-hash survives a realistic multipart/mixed message with a base64 attachment (the form a real Gmail send takes), guarding the strip → sign → deliver → capture path against body-mangling. (The signature is the nest's, made at the outbound hand-out — `mail-bridge-lifecycle.md` § DKIM provisioning (automatic); per-domain From-signing: `mail-multidomain.md` § Signing-key selection at outbound time.)
- **DKIM RSA keys must be ≥ 2048 bits per RFC 8301 (2018) — enforced at the *sign site*, not at key admission.** The floor lives where a plaintext RSA key is actually consumed — `fauna_mail::outbound::dkim::sign`'s `SigningAlg::RsaSha256` arm (`libs/fauna-mail/src/outbound/dkim.rs`) — which rejects a sub-2048-bit modulus (`SignError::RsaKeyTooSmall`) before emitting a signature. This is the durable home for the invariant *"this deployment never emits a DKIM signature under a sub-2048-bit RSA key,"* independent of how the key arrived. This is a defensive floor, not a live gap: no path supplies a weak RSA key — every nest mint is Ed25519 (`bins/fauna-nest/src/mail_dkim_key.rs`), the only RSA generator `generate_rsa_2048` (`libs/fauna-provisioning/src/dkim.rs`) is 2048-bit by construction, and there is no bring-your-own-DKIM-key import surface in any app or on the wire. The floor guards against any future import surface before it is built. Ed25519 keys are unaffected (fixed 256-bit curve, accepted by RFC 8463) — only the RSA path carries the floor. **The same floor covers ARC sealing:** ARC (RFC 8617 §4.1.2–4.1.3) reuses DKIM's signing algorithms and key infrastructure, so RFC 8301's 2048-bit RSA minimum applies equally to `fauna_mail::outbound::arc::seal`'s `RsaSha256` arm (`libs/fauna-mail/src/outbound/arc.rs`) — the second and only other place a plaintext RSA signing key is consumed in-tree. **Implementation status: the DKIM sign-site floor is wired** (`fauna_mail::outbound::dkim::sign`'s `RsaSha256` arm rejects a sub-2048-bit modulus with `SignError::RsaKeyTooSmall` before calling into the signer; pinned by `dkim.rs::tests::rsa_key_below_2048_bits_errors` / `::rsa_key_at_2048_bits_signs`). **The ARC seal-site floor is wired too** (`fauna_mail::outbound::arc::seal`'s `RsaSha256` arm rejects a sub-2048-bit modulus with `SealError::RsaKeyTooSmall` before calling into the sealer, sharing the same `rsa_modulus_bits` measurement helper as the DKIM site; pinned by `arc.rs::tests::rsa_key_below_2048_bits_errors` / `::rsa_key_at_2048_bits_seals`). Latent, not live: `arc::seal` still has no production caller until the Phase-E relay wiring (`submission.go` documents the deferral), so the floor lands at the primitive ahead of its first consumer.
- Outbound `Received:` headers are stripped before relay. The strip is one shared-Rust pure fn — `libs/fauna-mail/src/outbound/received_strip.rs::strip_received_headers` — exported over UniFFI (`fauna_ffi::strip_received_headers`). The production Go submission path (`bins/fauna-bridges/internal/mta/submission.go::Data`) calls it via `mailfauna.StripReceivedHeaders` before it enqueues the message, so internal hostnames and submitter IPs that the user's MUA or upstream relays attached do not reach the recipient's mailbox, and the DKIM signature the nest makes at the outbound hand-out covers the stripped form. Strip is RFC 5322 §2.2.3 continuation-aware and matches field name case-insensitively; substring matches (e.g. `X-Received-By`, `Received-SPF`) are NOT stripped.
- Outbound TLS to recipient MX hosts is pinned to DANE/TLSA records at `_25._tcp.<mx>.` per RFC 7672 when the host publishes them. **The Go MTA bridge owns the outbound TLS handshake but cannot do DNSSEC in the Go stdlib, so the work splits** (the mail-bridge-rearchitecture cutover rule — Go owns outbound TLS, nest does DNS work needing a Rust capability, FFI is pure computation):
  - **nest validates the *MX RRset* too — DANE binds only to a name that came out of one.** RFC 7672 §2.2: an SMTP client whose MX RRset is not DNSSEC-validated MUST NOT treat the destination as DANE-capable. The bridge resolves MX through `fauna.bridges.resolve_mx`, whose reply carries a per-RRset `secure` bool, and **skips the TLSA fetch entirely when it is false** — before the fetch, mirroring the MTA-STS refusal gate, so a host that can never be pinned costs no RPC. Skipping is the required direction: a non-secure MX answer falls back to the MTA-STS / opportunistic posture and still delivers; it never bounces mail. Corrected 2026-09-01: this doc previously stated the DNSSEC mandate for the TLSA leg alone, and the code matched it — the MX RRset came from Go's stdlib resolver with no validation at all, so an attacker who could spoof DNS redirected delivery to an MX they controlled, published a genuine signed TLSA for that name, and the pin succeeded against the wrong host while the bridge logged its strongest posture. Details of the lookup and the two non-RRset `secure: true` cases (implicit MX, operator transport override): § MX resolution item 1.
  - **nest does the DNSSEC-validating TLSA *fetch*.** The Go worker calls `fauna.bridges.fetch_tlsa(mx_host)` per surviving MX host; nest's handler runs `fauna_mail::outbound::dane::lookup_tlsa` (the permanent home in `fauna-mail`). The resolver runs with DNSSEC validation enabled (`ResolverOpts.validate = true` + hickory-resolver's `dnssec-ring`); only `Proof::Secure` records are retained, and only SMTP-usable DANE-TA (2) / DANE-EE (3) usages reach the wire (PKIX-TA/EE are dropped → an only-PKIX host yields no records, i.e. MTA-STS / opportunistic fallback, not an undeliverable hard-fail). Records with Insecure / Bogus / Indeterminate proof are discarded with a warn-log — RFC 7672 §2.2.1 mandates DNSSEC for DANE to be trustworthy, and an Insecure record could be spoofed by a DNS-MITM attacker.
  - **Go does the *pinning*.** When `fetch_tlsa` returns records the bridge sets STARTTLS mandatory and verifies the presented chain against them in its TLS `VerifyPeerCertificate` callback via the UniFFI `dane_chain_matches(records, cert_chain_der, mx_host)` decision — selector 0/1 full-cert/SPKI × matching 0/1/2 exact/SHA-256/SHA-512 per RFC 6698 — lifted whole into shared `fauna_mail::outbound::dane`. **The two usages are not the same test, and the difference is the security:**
    - **DANE-EE (3)** matches the leaf and nothing else is checked. RFC 7672 §3.1.1 waives name and validity deliberately: the record names *that exact key*, so holding its private key is the entire proof, and a CA compromise buys an attacker nothing at all.
    - **DANE-TA (2)** names a trust **anchor**, so a hash match is only a *candidate*: the presented leaf must additionally pass PKIX path validation **to that matched cert as the sole trust anchor** and carry `mx_host`'s name (RFC 7672 §3.1.1, RFC 6698 §2.1.1). ⚠ A bare "any chain cert hashes right" test authenticates **nobody** — association data is a hash of a *public* certificate, and an anchor is the most public certificate there is, so an on-path attacker needs no CA compromise and no private key but their own: they present `[their own self-signed leaf, a verbatim copy of the victim's published anchor]`. Corrected 2026-09-01; this doc previously described the match-any form as the contract.
    The whole decision stays in the Rust matcher rather than splitting the path validation Go-side, because that validation has to run against *the cert the record matched* — a split would force Go to re-derive the association match and keep a second copy of it in another language. WebPKI's public root store is bypassed (`InsecureSkipVerify`, the TLSA pin replaces it); for usage 2 the matched anchor is the only root offered, so a path to a public CA does not help an attacker. A pin mismatch or missing STARTTLS is a temporary failure — **no plaintext fallback**. A `fetch_tlsa` RPC error never blocks delivery (falls back to the MTA-STS / opportunistic posture).
  - **DANE > MTA-STS.** When both apply to a surviving host, the DANE pin overrides the MTA-STS WebPKI-required posture (§ MX resolution :498). The MTA-STS `mx:` refusal gate still runs *first* (§ MX resolution :500), so a refused host costs no TLSA fetch.
- Submission flows advertise `PLAIN` and `OAUTHBEARER` **today** (candidate-(b) set; `internal/mta/submission.go` — no LOGIN, no SCRAM). **Target (ratified 2026-07-08): SCRAM-SHA-256** (RFC 5802 + RFC 7677) preferred once built — RFC 5802-aware clients pick it first, the password never crosses the wire, the server stores `StoredKey + ServerKey` (not the password), and the verification can't be replayed against a different SCRAM server; `PLAIN` stays as the over-TLS-only fallback. go-smtp's `AllowInsecureAuth = false` is enforced on both submission listeners so plaintext mechanisms only run over TLS.
- **Corrected 2026-07-19 (first independent verification since bootstrap — the prior text described an `actorRateLimiter` + `ratelimit-state.json` design that was never built; see the flow-trace below).** Submission flows (ports 465 + 587) apply a per-actor **recipient** quota keyed on the authenticated `actor_id`, in two layers (`bins/fauna-bridges/internal/mta/submission.go::Rcpt`): (1) a **fast-path**, no-RPC per-message cap read straight from the wrapped submission token's `MaxRecipients` field (`SubmissionPolicyThresholds.max_recipients_per_message`, default 100) — over it returns `452 4.7.12 Per-message recipient limit exceeded`; (2) the **authoritative** per-actor **recipients/day** quota, decided nest-side via `fauna.bridges.check_submission_quota(actor_id, recipient_count)` → `try_consume_submission_quota` (day-bucket counter, default `SubmissionPolicyThresholds.max_per_day` = 1000) — over it returns `452 4.7.12 Per-actor submission quota exceeded`; an RPC transport/decode error tempfails `451 4.7.0` rather than smuggling past a gate the bridge can't evaluate. Both ports call the same nest-side counter, so a user can't dual-stream to escape it. The quota state lives nest-side (SQLite), not a bridge-local file — consistent with this doc's own "no per-user state beyond ephemeral rate-limiter records; all persistent state crosses the RPC boundary" rule above. **There is no per-actor messages/hour cap in the current implementation** — an earlier design draft described one (`200 msgs/hour`), but the two layers above are what shipped from the feature's first commit onward; `mail-policy-config.md`'s `mail.submission.per_actor_msg_per_hour` catalog row is correspondingly stale and should be read as "not implemented," not "admin-UI-pending." **This says nothing about the first-party app door**, which is a different surface: `fauna.email.send` does carry a per-actor messages/hour ceiling, as a hard-coded constant rather than that catalog row (`mail-app-surface.md` § Outbound metering, 2026-08-23). The daily recipients quota named above is shared by both doors on purpose — same actor, same counter, so neither door is an escape from the other. **One charging rule for that one counter (ruled 2026-09-25): it counts recipients that leave the deployment, one per accepted `RCPT TO`.** A recipient is *local* when RCPT-time resolution placed it in a mailbox on this deployment (the resolver's `Resolved` outcome); an outside-domain recipient, and an admin external forwarder (the resolver's `Forward` — the message leaves through the forward dispatch), are *remote* and charged. The app door draws the identical line (`mail-app-surface.md` § Outbound metering: an in-domain recipient the resolver places locally is uncharged; one it routes onto the outbound queue is charged). Mechanics: the bridge makes one `check_submission_quota` call per accepted RCPT carrying the running `recipient_count` — the per-message cap's input, and nothing else's — plus `recipient_is_local`; the nest debits **one** for a remote recipient and nothing for a local one, never `recipient_count`. Two defects the ruling closed: until 2026-09-25 the nest added `recipient_count` to the day's total on every call, so a k-recipient message cost k(k+1)/2 of the allowance (a 10-recipient send cost 55 of the default 1000), and submission charged local recipients where the app door did not. Compat: `recipient_is_local` is additive (`#[serde(default)]`; absent reads as remote) — an older bridge's recipients are all charged, and an older nest ignores the field and keeps its running-count debit until upgraded (`../architecture/version-compatibility.md`).
- **Implemented today:** a per-IP **connection-rate** limiter on the inbound (port 25) listener only (`bins/fauna-bridges/internal/mta/policy.go::RateLimiter`, wired at `mta.go` from live nest config `SpamPolicyThresholds.max_conn_per_min`, admin-UI binding `mail.inbound.per_ip_conn_per_min`, default 10/min — § Connection-time limits). It is a fixed-window in-memory counter, deliberately **ephemeral by design** (`config_holder.go` names it one of "the two 1-minute ephemeral counters" reset on every `config_changed` reload; consistent with this doc's own "ephemeral rate-limiter records" architectural rule) — it is **not** persisted to any state file, and a restart or config reload does give a connecting IP a fresh window. **Not yet built:** a per-**subnet** bucket (`/24` IPv4, `/64` IPv6) and a per-IP **message-rate** cap — both remain catalog rows pending their nest-config knobs (§ Implementation status today's Connection-time-limits gap note); no code path enforces either today, and no `ratelimit-state.json` or equivalent persistence file exists anywhere in the bridge (verified 2026-07-19 — the only on-disk state file in the whole bridge process is the keypair file). The port-25 per-IP **concurrent**-connection cap (§ Connection-time limits, compile-time 50) and the global cap are the defenses that actually cover the gap left by the missing rate buckets.
- **Retired (T3.1, 2026-05-24) — corrected 2026-07-22 (this doc had never caught up):** DNSBL score-class hits no longer feed a Fauna-side nest heuristic. `DNSBLChecker.Check` (`bins/fauna-bridges/internal/mta/policy.go`) still collects them as `DNSBLResult.ScoreReasons`, but there is no `dnsbl_score_reasons` RPC forward to nest and no `smtp_inbound_dnsbl_score_signal_total` counter anywhere in the bridge — rspamd is now the **sole** deployment-wide content scorer and its own RBL module covers score-class signals (`mail-spam.md` § Pipeline (recap from `smtp-server.md`), item 3). Reject-class hits are unaffected by this retirement — they remain a bridge-local perimeter hard-gate, independent of content scoring (next bullet).
- DNSBL hits are routed by per-list policy on the bridge (`bins/fauna-bridges/internal/mta/`). Reject-class hits (e.g. `zen.spamhaus.org` SBL/XBL/CSS — response codes 127.0.0.2-7, 9) reject the connection at 550 5.7.1. Lists not in `DefaultDNSBLPolicies` fall back to `DefaultUnknownPolicy` (any hit → Reject) — admin-added lists keep the safe-default behavior until a policy entry lands.
- **Target state, not yet built (corrected 2026-07-22 — this bullet and the next previously described it as current behavior; no code path or nest-config type backs either today, in Go or Rust).** A per-sender carve-out for the STARTTLS-required gate, via a nest-config cleartext allowlist (catalog row `mail.tls.cleartext_allowlist`, `mail-policy-config.md` § catalog, Bucket C — neither projected nor admin-writable): exact `user@domain`, `@domain` patterns, exact IPs, or CIDRs (IPv4 + IPv6), matched in-band at first MAIL FROM. No such allowlist, and no `accepted_cleartext_allowlisted` metric, exist in the bridge today.
- **`InboundTLSMode` ships as `required` only — deliberately, not as a gap (§ TLS posture per port has the authoritative framing).** There is no `opportunistic`/`off` mode, no `InboundTLSMode` type at all in Go or Rust — just the hardcoded `requireStartTLS bool` (`bins/fauna-bridges/internal/mta/server.go:349,542`), true whenever a TLS provider is wired, always rejecting cleartext with `530 5.7.10`. Pre-shipping the other modes ahead of a real deployment's need is out of scope by design; so is the `smtp_inbound_mta_sts_violations_total` metric named in an earlier draft of this bullet, which never existed.

## Don't do these

- Don't add silent fail-open paths. Every fail-open is a counter + a warn log.
- Don't log raw RCPT-TO addresses. Always hash. Privacy-by-default applies even to box-owner logs.
- Don't store per-account state in the bridge (rate-limit buckets are bridge-local; per-account quota and validation are nest-side).
- Don't expose the metrics endpoint on a non-loopback bind without auth.
- Don't introduce a per-RPC retry loop in the bridge: the SMTP RFC retry is the sender's job, not ours; one RPC error → one tempfail.
- Don't assume `ingest_inbound_mail` is idempotent without checking — the nest dedup story is owned by nest and is not part of this doc.
- Don't re-implement SPF/DKIM/DMARC/ARC verification in the Go bridge. Verification runs once in shared Rust (`verify_inbound`, `mail-auth`) over UniFFI. A second verifier at the bridge would be redundant and risks parser-disagreement bugs (CVE-2023-51764-class).
- Don't add a new `fauna.bridges.*` kind without adding it to nest's per-role method allowlist. A kind absent from the role's allowlist is denied nest-side — a security-review boundary, not a wire-format question.
- Don't reuse the `fauna` user (the nest's user) for the bridge on bare-metal. The bridge is the public-internet attack surface and must run as its own `fauna-bridge` system user with no read access to nest's data dirs. (In the Docker image the bridge runs as `fauna` but holds no decryption capability and has its own keypair.)
- Don't lower TLS `MinVersion` below TLS 1.2 — RFC 8996 deprecated TLS 1.0/1.1 in 2021 and any sender that requires the older versions is suspect.
- Don't add a CBC-mode cipher if/when the target cipher pin (§ TLS posture per port) is finally implemented — they're vulnerable to BEAST / Lucky13 / Bleichenbacher class attacks; the target list is AEAD-only by policy, not by accident.
- Don't let the two independently-built `internal/mta`/`internal/mda` `tls.Config` literals (§ TLS posture per port — there is no shared factory today, corrected 2026-07-23) drift apart on cipher/version settings even though nothing enforces their uniformity; consolidating them into one hardening factory (`bridgetls.HardenedConfig()`, target-state) is the follow-up that also lands the cipher/curve pin. Per-port cipher divergence, if introduced ad hoc in the meantime, is the kind of drift this project pre-empts.
- Don't bypass the DKIM enforce gate when a message has a valid ARC chain — ARC means "this intermediary verified the original signatures," not "this message's DKIM passes." If you find legitimate ARC-vouched mail being rejected by `enforce_dkim`, the right answer is `log_only` or DMARC-driven enforcement, not a quiet carve-out.

## Two-process architecture

```
Thunderbird ← IMAP/SMTP → fauna-mail-bridge (Go, MTA + MDA) ← WS-RPC + DAG-CBOR → fauna-nest
```

Two components handle email end-to-end:

- **fauna-mail-bridge** (`bins/fauna-bridges/`, Go, talks shared Rust
  via UniFFI): the MTA role serves SMTP on ports 25 (inbound from remote
  MTAs) and 465/587 (submission) plus outbound MX delivery; the MDA role
  serves IMAP (993/143) and CalDAV. It handles TLS termination, the SMTP
  and IMAP/CalDAV protocols, rate limiting, DNSBL checks, and
  SPF/DKIM/DMARC/ARC verification (over UniFFI to shared Rust). It talks
  to nest over WS-RPC + DAG-CBOR on `/api/v1/ws/{actor_id}`, authenticated
  by its enrolled service-user keypair, with a per-role method allowlist
  enforced nest-side.
- **fauna-nest** (`bins/fauna-nest/`, Rust): stores mailboxes and
  messages, serves the `fauna.bridges.*` WS-RPC surface the mail-bridge
  calls, stores the ClamAV/rspamd scan-verdict metadata the bridge
  reports (the scanners themselves run at the mail-bridge perimeter,
  never on the nest — § Inbound pipeline below), and enforces per-user
  obligation rules.

## Inbound pipeline

After the perimeter accepts a message (DKIM/SPF/DMARC verified, connection-
and envelope-time policy passed), it moves through the content-scoring
pipeline. **These scorers run at the mail-bridge (MTA) perimeter, on the
plaintext the bridge holds, before sealing — not on the nest** (per
[`../architecture/content-scoring.md`](../architecture/content-scoring.md),
since the bridge is the plaintext entry point for inbound mail). Only the
verdict metadata (and the sealed body) cross to the nest; the nest never
holds the plaintext and runs no content scorer. This section owns the
**ordering**; placement is `content-scoring.md`'s,
each scorer's config its feature doc's.

1. **ClamAV malware scan** — binary pass/reject signal; infected mail rejects
   at the SMTP perimeter and is not stored (or, per admin policy, is filed to
   the recipient's Junk or tagged). **Configuration + per-message `message_scan_results` storage + admin
   actions spec'd in `docs/goal/behavior/mail-content-scanning.md` § ClamAV.**
2. **rspamd content score** — statistical + rule-based content score.
   **Configuration + rule overrides + score-scaling spec'd in
   `mail-content-scanning.md` § rspamd; the scaled score feeds the
   combined-score formula in `mail-spam.md` § Combined-score formula.**
3. **Per-user Bayesian classifier** — per-user trained spam model
   (`mail-spam.md`). Needs the user's sealed model, so it is **not** an
   inbound-delivery-time perimeter step — it runs on the user's
   client (or an AUTH'd MDA session); see `content-scoring.md` § The placement
   matrix.
4. **Combined score → routing decision** — the combined score vs. the
   per-account threshold (`mail-spam.md` § Combined-score formula owns the formula). The
   default is **permissive auto-Junk**: below `spam_folder` → INBOX, at/above →
   Junk; the `reject` tier, the only other one, defaults to **disabled (`0`)**,
   so out of the box the bridge scores + tags + delivers and never 550-rejects
   on the content score — and nothing is ever held. Destructive actions are opt-in (an
   admin sets a non-zero reject tier, or a per-user filter rule acts on the score —
   step 5). rspamd is the sole deployment-wide content scorer (no separate
   auth/DNSBL heuristic); the per-user Bayesian is **not a perimeter step** — it
   is scored post-delivery (by the authenticated agent) and re-files to Junk
   there, so the *perimeter* combined score is the rspamd score (`mail-spam.md`
   § Scoring placement).
5. **Filter rules** — user-defined rules evaluated pre-seal at the perimeter
   (see [Email filter rules](#email-filter-rules) below; `mail-forwarding.md`
   § Storage-mode interaction / § Where the forward config lives at rest for
   the perimeter-eval rule).
6. **Delivery** — sealed to the recipient and ingested to INBOX / Junk / the
   filter-specified folder via `fauna.bridges.ingest_inbound_mail`. Alongside
   sealing, the perimeter computes the canonical **report-hash** once per
   message (pre-seal) and ships it on the ingest call as floor
   metadata — owner: `report-sharing.md` § Content identity (built; Slice 1,
   2026-07-07).

## Spam configuration

The live spam policy is the `SpamPolicyThresholds` projected from nest
config via `fauna.bridges.fetch_config`. The **canonical defaults + the
permissive auto-Junk semantics** (`spam_folder=5`,
`reject=0`; `0 = disabled`) live in `mail-policy-config.md` § Inbound
perimeter, not here. DNSBL servers, the score-before-reject threshold,
the rDNS signal, and the greylist windows are all entries in that
catalog; the FCrDNS signal (§ FCrDNS) supersedes the old standalone
reject-on-no-rDNS knob.

## Transport labels

After the verification pipeline completes, the mail-bridge attaches the
perimeter's results to the message as **score rows** — one per factor
(`spam`, `clamav`, `rspamd`, `auth_spf`, `auth_dkim`, `auth_dmarc`,
`auth_arc`), each an integer score, built by the shared
`fauna_core::scoring::perimeter_mail_score_rows` — and they travel with the
message to fauna-nest on `fauna.bridges.ingest_inbound_mail`, which persists
them. The row's shape and its carriers are owned by
[`../architecture/content-scoring.md`](../architecture/content-scoring.md)
§ The scoring-metadata bus. A blocklist hit is not among them: a listed
server is refused when it connects, and score-class blocklist forwarding is
retired. (Until 2026-10-01 this section described labels — `dnsbl-listed`,
`spam`, `auth-fail`, "with confidence scores" — that no code ever produced.)

## Outbound submission flow (per-user view)

1. Thunderbird connects to the mail-bridge on port 587 with TLS
   (`STARTTLS`) — or 465 with implicit TLS.
2. The mail-bridge authenticates the submission via the same
   AEAD-unwrap-as-AUTH mechanism as IMAP (a `WrappedSubmissionTokenBlob`
   carrying no decryption capability; see § Auth on each port). Both
   sender identities are then held to one ownership predicate — the
   envelope `MAIL FROM` at `MAIL`, the RFC 5322 `From:` header at `DATA`
   (exactly one mailbox, owned by the authenticated actor when the
   deployment signs for its domain) — owner `mail-multidomain.md`
   § Cross-domain submission policy and § From: header ownership.
3. The mail-bridge forwards the message to fauna-nest via the
   `fauna.bridges.enqueue_outbound_mail` WS-RPC kind (one row per
   **off-box** recipient on nest's `outbound_mail_queue`; see the wire
   surface above). **In-domain partition (nest-side):** the handler
   splits recipients by the deployment's primary mail domain and never
   enqueues an in-domain recipient onto the MX-relay queue (that would
   self-loop back through the box's own inbound listener and, on a
   containerized deploy, `554`-bounce at the inbound HELO-identity
   check). An in-domain recipient that resolves to a local mailbox (the
   full alias resolver — exact / `+suffix` / wildcard / catch-all /
   disposable / role-address, the same resolution `fauna.bridges.resolve_recipient`
   runs) is **sealed to that recipient's MSEK-derived pubkey and
   delivered locally** through the shared sealed-ingest path
   (`__mail/<actor>` segment + `bridge_imap_messages` INBOX) — exactly
   the in-domain delivery `fauna.email.send` does (`mail-app-surface.md`
   § First-party client send). **The partition lives at the queue-insert, not in this handler:**
   the nest-internal `submit_outbound` helper
   (`bins/fauna-nest/src/bridge_routing_handlers.rs`) is the single
   chokepoint — it partitions, seals in-domain recipients locally, and
   enqueues only external ones — so the invariant holds for **every**
   caller. The MTA submission path also pre-partitions on the Go side
   (`partitionRecipientsByLocalDomains`, passing only external recipients);
   the MDA auto-schedule gateway passes all attendees through this RPC and
   relies on the partition for its in-domain iMIP delivery
   (caldav-server.md § Server-side auto-schedule); and the nest-internal
   direct-enqueue callers route through `submit_outbound` too —
   **DSN/NDR generation** (`outbound_bounce.rs`, the to-original-sender
   bounce) and **vacation auto-reply** (`send_auto_reply`), so a bounce or
   auto-reply addressed back to an *in-domain* sender now delivers into that
   sender's sealed INBOX instead of MX-self-looping.
   **Security-notification mail** (`security_notify.rs`) always targets a
   known in-domain actor, so it seals straight to that actor via the shared
   sealed-ingest path (never the queue). The **forwarder NDR**
   (`generate_forwarder_ndr`) joins them: the forwarder is always a known
   in-domain actor (we hold its id on the queue row), so a *synchronous*
   permanent-failure NDR seals straight into the forwarder's INBOX too —
   exactly like security mail, no `SRS0=` MX loopback (mail-forwarding.md
   § NDR routing, N4b; the loopback previously hairpined and `554`-bounced on
   a containerized deploy at the inbound HELO-identity check, the same
   self-loop class fixed for the in-domain-*mailbox* path). Only the
   **retry / TLSRPT** paths deliberately stay on the direct queue: they
   re-queue rows already classified external or report to a remote domain's
   report URI. (The inbound `SRS0=` decode path still serves *asynchronous*
   downstream-MX bounces that legitimately arrive back over the wire —
   mail-forwarding.md § Bounce decode.)
   **Domain source (LOAD-BEARING, 2026-06-06):** `submit_outbound` resolves
   the primary-domain partition key at call time from the runtime
   `local_domains` table (`bridge_routing_handlers::primary_mail_domain` →
   `lookup_primary_mail_domain`), NOT the boot-time `state.email.domain`,
   which is always `None` on the Go-MTA build (:42). The original
   short-circuit keyed the split on that legacy field and so shipped
   **DEAD** — every recipient classified remote, in-domain recipients
   MX-self-loop-relayed in the real image — even though every tier_3
   conformance test passed (they set the field in-process). Only the
   real-image tier_4 acceptance caught it
   (`test_caldav_autoschedule_imip.py::…in_domain_attendee_delivered_to_inbox`,
   now the guard); `fauna.email.send` keeps its own exact-match in-domain
   path and shares the same `primary_mail_domain` resolver.
4. The MTA-role process polls `fauna.bridges.fetch_outbound_due` — the nest
   signs each message with DKIM as it hands it out — and delivers to recipients' MX hosts;
   `mark_outbound_delivered` / `mark_outbound_failed` / `_bounced`
   close out the row's retry curve.
5. A copy is saved to the user's Sent mailbox.
## First-party client send + receive

**Moved 2026-08-03 to [`mail-app-surface.md`](mail-app-surface.md)**, which now owns
the `mail-client-rpc` concept — `fauna.email.send` and its server-side Sent copy, the
`fauna.email.inbox.fetch` / `fauna.email.sent.fetch` read feeds, and the
`fauna.mail.received` arrival push. This doc keeps the **external-MUA** path it is the
entry point for: § Outbound submission flow (per-user view) above, § Auth on each port,
§ Recipient handling on submission, § Outbound delivery. The in-domain partition at the
queue-insert (`submit_outbound`) is described in § Outbound submission flow step 3 and is
shared by **both** paths — first-party sends route through the same chokepoint, which is
why an in-domain recipient never MX-self-loops regardless of which path submitted it.

## Email filter rules

**Moved 2026-08-03 to [`email-filters.md`](email-filters.md)**, which now owns the
`email-filters` concept — conditions, actions, multi-action composition, the
`fauna.email.filters.*` CRUD surface, and implementation status. The rules are
evaluated at this doc's perimeter, pre-seal: § Inbound pipeline step 5 is the
ordering claim, and that step is where a filter verdict enters placement.

## MUA-facing view (Thunderbird, Apple Mail)

> **Renamed 2026-08-03.** This section was called *IMAP serving (Thunderbird-facing)* and had
> accumulated children that are not IMAP at all — the inbound scoring pipeline, transport
> labels, the two-process topology, the filter engine, and the first-party client RPCs. Those
> are now top-level sections of this doc (or their own owner docs); what remains here is the
> genuine MUA-facing view: how a standards-compliant mail client connects and what its user
> sees. Old links to `smtp-server.md § IMAP serving` land here.

> **Sibling doc:** `docs/goal/behavior/imap-server.md` owns the IMAP **server** contract — write paths (APPEND, EXPUNGE, FLAGS STORE, MOVE/COPY, CREATE/DELETE/RENAME), modern extensions (IDLE, CONDSTORE/QRESYNC, NOTIFY, QUOTA), auth (OAUTHBEARER preferred / PLAIN/LOGIN over TLS fallback; AEAD-unwrap-as-AUTH against a per-credential wrapped-MLS-blob — unconditional, per the candidate (b) ratification (design ratified 2026-05-14; tracked internally) § Sub-question 1), and the MDA ↔ nest WS-RPC contract. This section keeps the **client-facing** view (connection setup, Thunderbird-style MUA config, the user-visible mailbox / spam / submission flow). On conflict in server semantics, imap-server.md wins.

The mail-bridge MDA-role process serves IMAP on port 993 (implicit TLS).
Thunderbird, Apple Mail, or any standards-compliant IMAP client
connects with the user's fauna handle as username and the MUA
credential as password — no custom client software is required. AUTH is
OAUTHBEARER (preferred) or PLAIN over TLS (fallback); both flow through
the same AEAD-unwrap-as-AUTH path against a per-credential wrapped-MLS
blob, where successful AEAD unwrap is the authentication signal. See
`docs/goal/behavior/imap-server.md` § Authentication for the full
contract; Thunderbird-side config is the standard IMAP/SMTP settings
below.

### Client configuration

The MUA connection details a user enters (IMAP/SMTP host and ports,
username form, AUTH mechanism, credential) are owned by
`docs/goal/behavior/mail-credentials.md` § MUA setup conventions and
rendered on every app by the shared `MuaInstructions`
(`libs/fauna-client-mail-settings/src/state.rs`) — not restated here.
This section keeps only the bridge-side username parsing below.
(Corrected 2026-09-23: a table here used to give a stale `localhost`
host and 587-only submission.)

A **bare handle** (`alice`) works because the bridge defaults a domain-less AUTH username to the box's `PrimaryDomain` via the shared `internal/auth.SplitEmailDefault`, applied uniformly across SMTP submission, IMAP, and CalDAV (`caldav-server.md` § Authentication — "Bare-username clients"; required for macOS Calendar.app, which sends only the local part). The full `alice@domain` form also works; on a multi-domain box, a bare handle resolves only on the primary domain.

The credential is the AEAD-unwrap key for the per-credential
wrapped-MLS blob; see `imap-server.md` § Authentication for issuance,
rotation, and revocation.

### Standard mailboxes and session flow

The standard mailbox set (INBOX / Archive / Drafts / Sent / Trash / Junk
with their SPECIAL-USE attributes), user-created mailboxes, and the
per-command IMAP semantics — including the authoritative command→kind
mapping onto the `fauna.bridges.*` MDA surface — are owned by
`docs/goal/behavior/imap-server.md` (§ Mailboxes; § MDA ↔ nest WS-RPC
contract).

### Spam handling (user-visible)

Email that passes connection-level checks but scores above the spam
threshold is delivered to the Junk mailbox instead of INBOX.
Thunderbird displays it in the Junk folder. Email that fails hard
checks (DNSBL-listed sender IPs, malware detected) is rejected at the
SMTP layer and never stored.

The per-user training feedback loop — the four signal sources
(mark-as-spam button, mark-as-not-spam button, IMAP `\Junk` flag-set,
MOVE-to-Junk), per-actor model storage, cross-actor isolation,
cold-start, reset, the combined-score formula combining rspamd +
per-user Bayesian — is spec'd in
`docs/goal/behavior/mail-spam.md`.

### Email aliases

Aliases let a user receive email at alternative addresses that route
to their main mailbox. The complete alias-kind taxonomy (exact /
+suffix sub-addressing / wildcard prefix / catch-all / disposable),
the fixed resolution order at RCPT TO time, per-alias controls
(spam-threshold override, disable, rate-cap, label), the
`mail-aliases` page UX, and the `account_aliases` storage shape
are spec'd in `docs/goal/behavior/mail-aliases.md`.

Wire surface: the bridge-class alias kinds
`fauna.bridges.{list,create,update,revoke,delete}_account_alias`,
`generate_disposable_alias`, `list_account_alias_hits`, `put_alias_policy`
(plus the Admin twins `get_alias_policy`, `set_catch_all_actor`)
— `mail-aliases.md` § Wire shapes is the authority.
(The former `/api/v1/email/aliases` HTTP surface was **deleted at the I6
cutover**, and `fauna.email.aliases.*` was never realized —
`api-layers.md` § Email.)

Example: `alias@domain` → `alice`'s INBOX. Aliases are resolved at
the SMTP recipient-resolution step (the `fauna.bridges.resolve_recipient`
RPC the mail-bridge calls at RCPT TO, on both the inbound MX and submission
paths) before the message is accepted.

### Known IMAP-server gaps (upstream-blocked)

Owned by [`imap-server.md`](imap-server.md) § Upstream-blocked gaps — the per-extension
status (SORT, THREAD, CONDSTORE, QRESYNC, QUOTA, NOTIFY, unsolicited-FETCH MODSEQ,
ManageSieve), which of them the vendored `go-imap` fork closed, and the standing rule that a
capability is advertised only when its wire response can be generated correctly. A
duplicate summary lived here until 2026-08-03 and had already drifted lossy against the
owner; it was removed rather than re-synced.

---

## Reading list

In priority order:
1. `principles.md` — engineering priorities.
2. (design ratified 2026-05-02; tracked internally) — slice ordering and dependency DAG.
3. (design ratified 2026-05-07; tracked internally) — the MTA/MDA two-process shape.
4. `docs/goal/architecture/transport.md` § WS-RPC — the bridge ↔ nest wire format and auth.
5. `bins/fauna-bridges/internal/mta/` — inbound policy, submission, and outbound delivery implementation.
6. `libs/fauna-mail/src/auth.rs::verify_inbound` — SPF/DKIM/DMARC/ARC verification (shared Rust, over UniFFI).
7. `libs/fauna-protocol/src/bridge_routing.rs` — `BridgePolicy` / `AuthPolicy` shape (the bridge-config wire types nest stores and projects to the MTA).
8. `libs/fauna-mail/src/outbound/{dkim,arc}.rs` — outbound DKIM signing and ARC sealing using `mail-auth`.
9. RFC 5321 (SMTP), RFC 7208 (SPF), RFC 6376 (DKIM), RFC 7489 (DMARC), RFC 8617 (ARC), RFC 8461 (MTA-STS), RFC 7672 (DANE for SMTP).
