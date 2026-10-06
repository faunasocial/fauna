# Mailbox migration — target state

Owns: mailbox-migration
Status: ratified
Authority: the import-from-foreign-IMAP surface — source-picker + wizard UX, client-side credential handling (never nest-side), the client-driven streaming model + batching/throttle, the import_sessions row + state machine + resume protocol, failure handling + error budget, the per-actor dedup contract, and quota composition; defers the landing write path + quota semantics to behavior/imap-server.md (§ Write surface / § Composition with submission quota), the export twin to behavior/mail-export.md, knob tiers to behavior/mail-policy-config.md, element IDs to ui.yaml.

> **Audience:** every per-app implementation of the import-from-IMAP wizard; the nest work owning the import-progress table + the `import_message` RPC; the shared IMAP-client logic lands in `libs/fauna-mail/` (shared-Rust work done in place — no nest-side IMAP client is involved).
> **Purpose:** the destination for "let me bring my Gmail / Outlook / iCloud / generic-IMAP mailbox into Fauna." On conflict in server semantics, imap-server.md wins.

## Implementation status today

**Corrected 2026-10-01 — this doc contradicted itself in seven places; each is now one rule, and five of them leave work the client does not do yet.** (1) *A failed connection* — the surface is a plain sentence plus a provider hint, never the server's raw reply (§ Wizard steps step 2, § Don't do these): **unbuilt** — the connect step returns the raw IMAP or transport error text into `error-message`, and no failure hint exists (the only provider strings are the pre-connect app-password help). (2) *Which mailboxes start unticked* (step 3): the five names are built, matched by name; Important, Starred and the special-use-attribute match are **unbuilt**. (3) *The user's own size limit* (step 3, "user can lower"; § Per-message flow step 1's skip): **unbuilt** — the wizard records `max_size_bytes` and the loop never reads it, so nothing is skipped as too large and only the nest's ceiling applies, as a failure. (4) *Over quota* (§ Quota composition): the nest's per-message `quota_exceeded` skip is built; the client's pause and its three-way dialog are **unbuilt** — the loop counts the skip and walks on through the rest of the source. (5) *A network failure that outlasts its retries* (§ Failure handling → *Source-side errors*): ruled a pause; **unbuilt** — the loop fails the session, which nothing can resume, and only the fetch side retries (a failed send to the nest is not retried by the loop). (6) `skipped_count` counts what the import wanted and did not take — dedup, quota and the size-limit skip — and never what the scope left out (§ Progress lives nest-side); the first two are as built, the third rides item 3. (7) Three citations of a section this doc never had now name § Wizard steps step 3, and the `staged_body` row of § RPC surface says what § Batching already did.

**The nest-side import surface is BUILT (2026-07-08, tracked internally — Track A), and so is the client side: the shared IMAP client, the wizard machine and the `mail-import` page on all seven apps (2026-09-01 — items 2–3 of the *Not yet built* list below carry the per-leg record). The one open client leg is the web source transport, so web cannot start a fresh import.** Landed nest-side:

- **All nine RPCs** (§ RPC surface below): wire types in `libs/fauna-protocol/src/bridge_routing.rs` (`ImportMessageItem`/`ImportMessageOutcome`/…), kind metadata in `kind.rs`, User-class allowlist arms + conformance tests in `bins/fauna-nest/src/bridge_method_allowlist.rs`, handlers in `bins/fauna-nest/src/bridge_import_handlers.rs` reusing APPEND's exact write path (`insert_appended_mail` → `place_or_get_existing_placement` → placement journal + IDLE/NOTIFY push) with authoritative quota enforcement (`enforce_imap_storage_quota`; over-quota → per-message `skipped_reason=quota_exceeded`).
- **`import_sessions` + `actor_message_dedup` migrations** (`bins/fauna-nest/src/db/migrations.rs`; row logic `db/mail_import.rs`): state machine, per-mailbox resume cursors, per-source lock (partial UNIQUE index over running/paused), 30-day `expires_at` GC swept **lazily** on the start/list entry points (no background task).
- **Push events** `BridgeImportProgress` / `BridgeImportError` / `BridgeImportComplete` (`push_events.rs` + nest `ws.rs`), emitted to the importer's own connected clients.

**Dedup population at the two pre-existing write paths is also BUILT** (2026-07-09, tracked internally — Track B), so a fresh import now dedups against *all* of the actor's mail, not just previously-imported mail:

- **The key computation** — `libs/fauna-mail/src/dedup_key.rs::mail_dedup_keys`, returning the `(dedup_key, envelope_key)` pair as the uniffi record `MailDedupKeyPair` (Go: `mailfauna.MailDedupKeys`), with the agree rule `envelope_keys_agree` beside it (§ Key format, § The envelope key confirms a Message-ID hit). Single-argument by design, no single-key entry point; a golden vector derived from § Dedup is asserted on both the Rust and Go sides, for the envelope key of a message with and without a Message-ID.
- **The wire** — `dedup_key` and `envelope_key` are required strings on `AppendMessageRequest`, `IngestInboundMailRequest` and `ImportMessageItem` (built 2026-10-01): a request missing either fails to decode (pinned by `a_request_without_either_key_is_refused_on_all_three`), and the nest refuses an empty one at all three doors (`fauna_mail::dedup_key::require_dedup_pair`), so there is no key-less request to describe. The Go↔Rust request fixtures carry both keys.
- **The producers** — all four send the pair: the Go MDA computes it from the raw literal at IMAP `APPEND` (`internal/mda/imap/append.go`); the Go MTA computes it once per message at DATA pre-seal, beside `report_hash`, for both external-MX delivery (`internal/mta/server.go`, from the *unstamped* `raw` so every recipient records the same pair) and own-submission (`internal/mta/fauna_recipient.go`); the import client derives it in the shared packer (`imap_client/batch.rs`) and the FFI facade (`FfiImportMessage`); and the nest mints it itself wherever it seals plaintext it holds (built 2026-10-01) — `seal_and_ingest_local` (`bins/fauna-nest/src/bridge_routing_handlers.rs`: `fauna.email.send`'s in-domain delivery, the in-domain partition of an MTA submission, and the bounce, forwarder-NDR and security-notice deliveries), computed from the raw message *before* the per-recipient delivery stamps are prepended, and `seal_and_store_sent_copy` (the sender's Sent copy); pinned by `in_domain_delivery_mints_the_dedup_pair_from_the_unstamped_raw` and `sent_copy_mints_the_dedup_pair`. At the MTA and MDA doors the nest cannot compute it — there it holds only ciphertext. The required-pair ruling of 2026-09-30 is fully landed: required on all three requests, NOT NULL at rest, no absent arm in `envelope_keys_agree` (§ The envelope key confirms a Message-ID hit).
- **Record-only, never skip.** Nest writes both keys into `actor_message_dedup` (`dedup_key` + `envelope_key`, both NOT NULL, `INSERT OR IGNORE`, first writer wins) and **never** acts on a hit at the delivery and APPEND paths: suppressing a delivery whose key already exists would silently drop a legitimate resend or a `Cc:` copy. Only `import_message` skips on a hit, and only when the envelope keys agree (`db/mail_import.rs::dedup_hit`), so a stranger's delivery reusing a Message-ID no longer pre-empts the real message's import (built 2026-09-29; pinned by `an_ingest_written_message_id_does_not_pre_empt_an_import_with_a_different_body`).

**✅ The 2026-07-10 transport-ceiling contradiction is RESOLVED (2026-07-12).** The 2 MiB
WS-RPC cap (`fauna_core::transport::MAX_RPC_WS_MESSAGE_SIZE`) is **permanent for every
caller class**; a sealed body above the mail **inline ceiling**
(`fauna_mail::transport_limits::MAX_INLINE_RAW_MESSAGE_BYTES`, 1.5 MB raw) crosses on the
**bulk-byte plane as a reference**, in both directions. The resolution rationale is owned
by `../architecture/transport.md` § Max frame; mail's constants, enforcement points, and
reply codes by `mail-message-size.md` § Message size limits. What it means for **import**:

- **§ Wizard steps step 3's 50 MiB per-message maximum stands** — reachable via the
  single-message `import_message` carrying a **staged-envelope reference**: the client
  AEAD-encrypts the raw RFC 5322 bytes under a one-shot key, stages the *ciphertext*
  chunks with its own session bearer, and the RPC carries
  `staged_body: Option<StagedBodyRef>` — exactly-one-of with a non-empty `body`;
  `body_size` stays the plaintext length, checked after the nest opens the envelope
  and before the unchanged seal-at-ingest path. Mechanism + rationale owned by
  mail-message-size.md § Message size limits, the staged-envelope rule — raw plaintext must
  never enter the open-download chunk store. **The nest half is BUILT (S9.4,
  ratified 2026-07-18):** `import_one` resolves `staged_body` via
  `mail_body_plane::resolve_staged_body`, checks `body_size`, admits at
  `effective_max_raw_message_bytes`, then seals at ingest; a bad reference is a
  per-message `Errored`, an infra failure aborts the call. **The client SEND path's RPC
  layer is BUILT (2026-08-27):** `fauna_mail::imap_client::MailImportClient`
  (`libs/fauna-mail/src/imap_client/send.rs`, `imap-client` feature) is a typed WS-RPC
  client over the full nine-kind § RPC surface, generic over `RpcRequester` — wasm-clean,
  one implementation for every native app (`fauna-client`) and the web SPA (`fauna-wasm`)
  alike, mirroring `fauna_client_conversations::ConversationsClient`. `send_unit` turns a
  `BatchPacker`-produced `ImportUnit` into the matching `import_message`/
  `import_message_batch` call. **Still open:** no app wires it in yet (no wizard
  controller calls `start_session`/`send_unit`/pause-resume-cancel, so still no shipping
  producer in practice), and it only ever sends whatever `ImportUnit` it is given —
  today that is always inline-bodied (`BatchPacker::to_item` always sets
  `staged_body: None`), so **no producer stages an over-ceiling body yet**; the
  AEAD-seal + chunk-upload staging leg is a separate, HTTP-byte-plane-owning slice. **The ceiling-retirement slice landed
  the same day (2026-07-18):** `effective_max_raw_message_bytes` no longer
  applies an at-rest clamp, so import's admission ceiling is now `max_message_bytes`
  alone (default 50 MB, matching § Wizard steps step 3's 50 MiB cap) — automatically, since
  `run_import` reads the same shared rule (`mail-message-size.md` § Message size limits,
  *Ceiling retirement — LANDED*).
  **Skew contract for the new field (stated 2026-07-18):** unlike the
  version-locked bridge legs, `import_message` is a real client↔nest skew surface with
  tolerant-reader structs — so a *newer* client sending `staged_body` (with `body` empty
  per the exactly-one-of rule) to an *older* nest has the field silently ignored and
  lands on the empty-`body` path. That path **must refuse loudly, never import an empty
  message** — and already does: `validate_item` rejects an empty body as a per-message
  `Errored { "body must not be empty" }`, visible in the import failure counters, and
  the source mailbox retains the message (degrade-visible, no loss). The build slice
  keeps that guarantee: the empty-body refusal must stay ahead of any staged-body
  resolution in the handler, so old-nest behavior is always the typed per-message error.
- **§ Batching packs to the wire frame, not to 16 MiB**: `BatchPacker::with_limits`
  clamps a batch to what the 2 MiB frame actually accepts; over-ceiling messages ride the
  single-message kind as references. The nest-side `MAX_BATCH_BYTES` (16 MiB) check stays
  as a defense-in-depth bound on a batch's *referenced* byte total, never an inline-frame
  promise (`transport.md` § Max frame point 3). **Both ceilings have one code owner since
  2026-08-22** — `fauna_protocol::bridge_routing::{MAX_BATCH_MESSAGES, MAX_BATCH_BYTES}`,
  beside the `ImportMessageBatchRequest` they bound; the nest handler and the client's
  `BatchPacker` read that one pair instead of each declaring its own.

The empirical record behind the resolution (do not re-establish): the cap is proven to
bind by `tests/e2e-unified/tests/api/test_ws_message_size_cap.py` (tier_3 — a 3 MiB
WS-RPC message is refused by the *transport*, connection closed, dispatcher never sees
it; 64 KiB decodes and dispatches), and the pre-fix mail consequence — oversized inbound
mail **deferred** with `451 4.7.0`, never delivered, never silently lost, sender retrying
for days — by
`tests/e2e-unified/tests/test_mail_bridge_mta.py::test_inbound_message_over_the_ws_rpc_cap_is_never_accepted_then_lost`
(tier_3), which flips to the ratified permanent `552 5.3.4` with the interim perimeter
fix. This was never import-specific: the Go MDA/MTA reach nest over the same capped
`/api/v1/ws/{actor_id}` with the body inline (`AppendMessageRequest.encrypted_body`,
`IngestInboundMailRequest.encrypted_body`).

**Open gap — the session store lags its export twin (measured 2026-09-26).** `mail-export.md` § Architectural rules holds the two session tables to one state machine and one 30-day GC, and the export store's progress fold was hardened in September 2026. The import store was not. In `db/mail_import.rs`, `record_import_progress` has no state guard, and the only guard is the handler's `running` read in `bridge_import_handlers.rs`, which sits before the per-item loop and outside the store lock. So a cancel or complete landing mid-call is followed by a fold that re-arms the finished session's 30-day window and moves its counters. A client `revised_total_count` above `i64::MAX` is stored as a negative number. `transition_import_session` still takes a free-form state string. The import row holds no blob, so the cost is a finished row listed longer and a counter that cannot be read. The rule here is unchanged: a finished session's window counts from real progress only, as § Expiry of the export twin states it.

**Not yet built** (the remaining tracks, in dependency order):

1. **Pre-existing-message constraint (permanent).** Messages stored *before* the dedup index existed have no dedup rows, and **no nest-side backfill can create them.** Re-importing a mailbox that overlaps pre-index mail double-stores the overlap. Accepted alpha cost; documented here so no session "fixes" it by weakening sealing.

   *This supersedes the earlier "a plaintext-mode backfill is possible" note (Track B3, decided 2026-07-09).* That note predated Phase 3, and the storage-mode axis it assumed is retired (`nest/storage-modes.md`). Mail is sealed at rest unconditionally, and a nest holds only the owner's *registered public key* — it seals, it never opens (`encryption-at-rest.md` § Readable classes, and its Phase-3 entry in `## Implementation status today`; the boot-time backfill in `bins/fauna-nest/src/content_seal_backfill.rs` is public-key-only for exactly this reason, and the sole `unseal_mail_record` calls in the nest are `#[cfg(test)]`). A nest therefore cannot read a pre-existing body to compute its dedup key.

   The one shape that *could* work is the **capability-mediated at-rest re-processing** rail (`encryption-at-rest.md` § Capability tiering), where a key-holding capability holder — the co-resident MDA, not the nest — opens records and writes back derived metadata, exactly as rescoring already does. That is a separate track, not part of the import contract, and it must not be attempted nest-side.
2. **The shared-Rust source-side IMAP client** — **the protocol core is BUILT** (2026-07-10, tracked internally — Track C1+C2); its **exposure is not**. Landed in `libs/fauna-mail/src/imap_client/` behind the opt-in `imap-client` feature: the `ImapTransport` + `ImapClock` AFIT seams (§ Where the IMAP client runs), sans-io response framing over `imap-proto`, the session state machine (`connect`/`login`/`AUTHENTICATE XOAUTH2`/`LIST`/`EXAMINE`/`enumerate_uids`/pipelined `fetch_messages`/`logout`), the pure `FetchThrottle` (4 concurrent, 100/min, rolling window), `INTERNALDATE` → epoch parsing, and the `BatchPacker` (32 msg / 16 MiB, oversized singles). Verified to compile for `wasm32-unknown-unknown`.

   **The native transport + UniFFI exposure are BUILT** (2026-07-11, tracked internally — Track C3), so the four UniFFI apps (apple / android / windows, plus linux + tui calling the crate directly) can now drive an import:

   - **`imap_client::native`** (`libs/fauna-mail`, opt-in feature `imap-client-native`) — the `tokio::net::TcpStream` + `tokio-rustls` `ImapTransport`, **both** TLS modes (§ The two TLS modes), plus the tokio `ImapClock`. It is a sibling of `outbound-net`, not part of `imap-client`: the protocol core stays wasm-clean and tokio/rustls arrive only for consumers that opt in, so one native transport serves all five native apps rather than one shell each.
   - **`FfiMailImportClient`** (`libs/fauna-ffi/src/mail_import.rs`, default-on feature `mail-import`) — the concrete `ImapSession<NativeTlsTransport>` facade UniFFI needs, since it cannot export the generic the seam is built on. It also derives the `dedup_key` + `sender_domain` wire fields itself, so a Kotlin/Swift shell cannot reimplement them and silently break dedup. The Go mail-bridge's `--no-default-features` build drops the feature (the bridge imports from no foreign mailbox).
   - **Coverage:** the scripted-transport unit tests (protocol) plus `libs/fauna-mail/tests/imap_client_native.rs` — the native transport against a **real TCP IMAP server with real TLS**: implicit-TLS fetch end-to-end, the STARTTLS in-place upgrade, an unverifiable source certificate refused *before the password is written*, and the plaintext-injection guard (mutation-verified).

   **The client→nest wire path is now proven end-to-end** (2026-07-12, tracked internally — Track C4): `bins/fauna-nest/tests/conformance_mail_import_client.rs` (tier_3) composes the two halves that were previously only proven alone. One test drives the **real shared-Rust IMAP client** against a **real TLS IMAP source server**, derives the wire fields through the same `mail_dedup_keys_from_slice` + `envelope::sender_domain` the FFI facade calls, pushes each message into a **real in-process nest** over authenticated WS-RPC `import_message`, and asserts: the record rests **sealed** and — seeded with a real X25519 keypair — **opens with the owner's secret to byte-for-byte the RFC 5322 the source served**; the source `(uid, uid_validity)` resume cursor round-trips into the `import_sessions` row; and mail the user **already holds from normal delivery** (dedup row written by the MDA/MTA producer path) is **skipped on its first import**, so an import never duplicates the mailbox it merges into. The seal-fidelity and dedup-skip assertions are mutation-verified. Rust↔Go dedup-key agreement remains pinned by the golden vector (`dedup_key.rs`), not by this test.

   **Still open:** the **web** transport (sans-io `rustls` over the relay) is unbuilt and stays sequenced last — the relay host is undeployed.
3. **The app wizard UI** — the `mail-import` page + full `mail-import-*` ID set is now **ratified in ui.yaml** (user-approved 2026-08-27, § UX shape below), mirroring `mail-export`'s "IDs precede the implementation" precedent.

   **The shared-Rust wizard machine is BUILT (2026-08-27):**
   `fauna_client_mail_settings::import::MailImportMachine`
   (`libs/fauna-client-mail-settings/src/import.rs`) mirrors `MailExportMachine`'s
   Snapshot/Action/dispatch/hydrate shape for the five-screen wizard (Source → Scope →
   Confirm → Progress → Done), over two seams — `MailImportNest` (real, wraps
   `MailImportClient`) and `ImportSourceNest` (the foreign-server half, real impl wraps
   `imap_client::ImapSession`). **The fetch-drive loop (`run_import`) is also BUILT**:
   walks every selected mailbox in source order, one throttled window at a time, packs
   through `BatchPacker`, sends via `send_unit`; pausable/cancellable between windows
   (Pause drains the in-flight window before stopping, Cancel aborts it); § Failure
   handling's 3-retry/backoff table and § Per-message error budget (both thresholds) are
   implemented, breaching either calls `fail_session`. **One same-process-only resume**
   (module docs on `run_import`): the per-mailbox cursor lives in the machine's own
   memory, not on the wire — a real client restart (§ Resume protocol step 2) re-walks
   every selected mailbox from its first UID rather than reading back
   `ImportSessionInfo.cursors`; harmless (nest's dedup skips the re-fetched overlap) but
   not the doc's full restart-resume, left for a follow-on slice. TDD'd throughout against
   fakes for both seams (no real network, no real IMAP server).

   **The `rpc_glue` construction is BUILT (2026-08-28):**
   `fauna_client_mail_settings::rpc_glue::build_mail_import_machine`
   (`libs/fauna-client-mail-settings/src/rpc_glue.rs`) mirrors
   `build_mail_export_machine`'s precedent, wiring both seams per platform. **Native:**
   both seams are real — `MailImportNest` wraps `MailImportClient<Arc<NestClient>>`;
   `ImportSourceNest` wraps a live `ImapSession<NativeTlsTransport>` (the `imap-client-native`
   transport, `tokio::net::TcpStream` + `tokio-rustls`), session-holding shape mirroring
   `fauna-ffi`'s `FfiMailImportClient` (duplicated rather than shared — this crate cannot
   depend on `fauna-ffi`, the dependency runs the other way). **Web (wasm):** `MailImportNest`
   is real too (`MailImportClient<WsRpcClient>`, wasm-clean); `ImportSourceNest` stays a stub
   (`Rejected`) — the web IMAP transport (sans-io `rustls` over the relay) is still the one
   unbuilt leg named above, so `hydrate`/`list_sessions`/pause/resume/cancel on an
   already-started session work on web, but `Connect` (starting a fresh import) does not yet.

   **The lead app's page is BUILT** (2026-08-28): `apps/fauna-tui/src/settings/mail_import.rs`
   renders the five wizard screens (Source → Scope → Confirm → Progress → Done) as a dumb
   renderer over `MailImportSnapshot`, dispatching `MailImportAction` and spawning
   `run_import`, with every `mail-import-*` id from `ui.yaml` painted.

   **The first trickle-down leg is BUILT — linux** (2026-08-31): `apps/fauna-linux/src/settings/mail_import.rs`, a rail sub-page of the settings
   shell reached at `{"view":"settings","id":"mail-import"}`, painting the same id set over the
   same shared machine (`crate::mail_glue::build_mail_import_machine`) with the same
   per-provider Source-field table. **Proven by the same e2e walk tui is:**
   `test_mail_import.py --app linux` -> 3 passed (real GTK app, real shared-Rust IMAP client
   over real TLS, real nest). Two shape notes a following leg inherits: the page's own
   `gtk::Entry`s ARE the draft buffers and the whole Source (or Scope) form commits as ONE
   ordered multi-action dispatch at Connect/Next — a per-keystroke dispatch races the
   Connect click and can log in with a truncated password; and the Progress screen carries a
   400 ms snapshot re-read tick while `run_import` runs, since that loop mutates the machine
   in the background and nothing else would repaint it. **The second leg is BUILT — web** (2026-08-31, same session):
   `apps/fauna-web/src/lib/components/MailImportSection.svelte`, a settings sub-page after
   `mail-export`, over a new `WasmMailImportMachine` face plus `importSourceKindLabel` /
   `importTlsModeLabel` (the `exportFormatLabel` precedent — web hand-rolls neither picker
   vocabulary). **Web ships the reduced slice this section already predicted, and does it
   out loud:** `Connect` cannot succeed, so the Source step is painted and actuable but
   carries the new `mail_import.source_unavailable` line ABOVE its fields — not merely on
   the rejection — because the alternative is a person typing a real mailbox password into
   a form that cannot use it. `run_import` is likewise not spawned there (it reads the
   source). The key is named for the *condition*, not for web: it retires when the transport
   lands, and nothing else on that page has to change. **Proven live:** `test_mail_import.py --app web` → 1 passed, 2 deselected (real browser, real nest) — the two
   deselected walks are the Connect-dependent ones. **The third leg is BUILT — android** (2026-08-31, same session):
   `MailImportScreen.kt` (stateless `MailImportContent` + the VM-bound wrapper) and
   `MailImportVM.kt`, at `settings/mail-import` off the mail-settings hub. Two things there
   are NOT copies of the export twin and a following leg needs both: the VM **spawns
   `runImport` itself** after a `Start`/`Resume` that comes back `RUNNING`, republishing on
   a 400 ms ticker until the loop ends; and the counts are `u64` (`ULong`), not the export
   twin's `u32`. Proof is Robolectric (10 tests) — Android e2e is blocked on the emulator host setup.
   **The fourth leg is BUILT — windows** (2026-08-31): `Controls/MailImportPanel.xaml` over `MailImportViewModel`, hosted by the
   settings-rail sub-page `SettingsMailImportPage` placed directly after `mail-export` —
   the same slot the linux/web IA gives it. Two things this leg contributed back rather
   than copying. **The two ordered dispatch sequences are now UniFFI-exported**
   (`connect_actions` / `scope_next_actions` in `libs/fauna-client-mail-settings/src/import.rs`):
   they were shared Rust already, but reachable only from a Rust caller, so the two
   bindings-consuming apps could not use them — android hand-rolled the ordering in Kotlin
   and windows was about to make it an eighth copy. **Android converged 2026-09-01**:
   `MailImportVM.kt`'s `connect()`/`scopeNext()` now call the exported `connectActions`/
   `scopeNextActions` instead of building the action lists by hand (closing a genuine
   micro-drift where the hand-rolled copy treated a `"0"` max-size input as unparseable),
   and `dispatch()` stops at the first rejected action rather than pressing on with a
   half-applied form, mirroring windows' `DispatchOneAsync`. Exporting them is the priority #2/#4
   answer, and it serves the apple legs too. **And the flaui bridge's `/element/attr` was
   dropping `index`** (hardcoded `Find(id, 0, scope)`, the parameter never parsed off the
   query string) — the identical bug the linux agent carried and fixed the same day, and
   silent for the same reason: an indexed row's neighbour usually holds the same value, so
   row 0's answer is normally the right one, until a walk toggles row N and reads it back.
   **The last two are BUILT — macOS + iOS** (2026-09-01), and they are ONE leg: a single shared FaunaKit `MailImportView` +
   `MailImportVM` over the UniFFI `MailImportMachine`, hosted by both targets'
   settings shells at a `SettingsPage.mailImport` rail slot directly after
   `mail-export` — the same slot linux/web/windows give it — with
   `APIClient.mailImportMachine()` vending the machine from
   `build_mail_import_machine(nest)`. **The page completes the trickle-down: all seven
   apps now render the wizard.** Its FFI surface needed nothing new — everything except
   the builder (which alone needs `FfiNestClient`) already lands in
   `uniffi.fauna_client_mail_settings`, the two exported action sequences included, so
   this leg starts at the SwiftUI view and calls `connectActions`/`scopeNextActions`
   rather than re-deriving either ordering in Swift. Three shape notes specific to a
   declarative UI. **`run_import` is spawned by the VM itself** after a `Start`/`Resume`
   whose POST-dispatch snapshot really reports `Running` — never a rejected one — with a
   400 ms repaint tick beside it, since the shared machine deliberately does not
   self-spawn the loop and without that call the Progress screen sits at zero while the
   session is genuinely open. **The shared `wizard-back-button` / `wizard-next-button`
   need no disambiguation mechanism here**: the body `switch`es on `step`, so only the
   active screen's copy is ever in the view tree, and that holds from first render rather
   than after a state update. **And the two no-wait reads the walk makes are answered from
   local synchronous state** — the picked provider kind (which decides Source-field
   visibility) and a mailbox row's `on`/`off` echo are both written in the control's own
   handler before the async dispatch, because the automation server acks as soon as that
   handler returns; leaving either to the dispatch's re-read reports the PREVIOUS value,
   stale rather than merely early. Apple needed no `/element/attr` index fix: its
   in-process server already parses `index` off the query and passes it as the leaf index,
   so it never carried the linux/FlaUI bug. This leg also retires the four
   `fauna.bridges.*_import_session` entries from the offline-gate checker's apple
   absence ledger — the Start/Pause/Resume/Cancel controls now carry their gates. Connect deliberately carries none: it is a `LOGIN` + `LIST` against
   the FOREIGN source over the client's own socket, not a nest RPC, so no wire kind exists
   for the offline gate to grade.

   **The tier_3 e2e walk of that page is BUILT and GREEN** (2026-08-28):
   `tests/e2e-unified/tests/test_mail_import.py` (3 tests) drives the wizard against
   `tests/e2e-unified/fakes/fake_imap_source.py`, a real TLS IMAP source server. Its
   structural blocker is closed — § The two TLS modes leaves the source connection no
   plaintext variant, so a harness-minted certificate had no way to be trusted by the
   compiled app; the source-IMAP trust seed
   ([`e2e-automation-surface-gating.md`](../architecture/e2e-automation-surface-gating.md)
   § The source-IMAP trust seed) supplies that one fact and nothing else. The module's app
   markers widen one app at a time as each leg lands (tui + linux today; **web carries a per-test marker on the reachability arm only** — the other two walks need a working `Connect`, so on web they are deselected, not skipped, and they widen with the transport).

   ⚠ **A leg whose first live run is red is not necessarily a product bug.** Both reds the
   linux leg hit were harness-contract mismatches, and the second was not even this feature's:
   the walk acts and reads with **no wait** in two places (pick a provider then type into the
   field it reveals; toggle a mailbox then read that mailbox's `state`), so a UI that updates
   through an async dispatch must answer those two from local synchronous truth; and linux's
   `/element/attr` route had always dropped its `index`, answering every
   `get_attr(id, name, index=N)` from row 0 — invisible until a test read a row it had just
   made differ from its neighbour. Check both before concluding the wizard is broken.

**The scope step's since date is applied as of 2026-09-22 — it had been collected and read by nothing.** `MailImportSnapshot.date_from` reached the wizard's state and stopped there: `run_import` walked every message of every selected mailbox, so a user who asked for "only mail since January" imported the whole source mailbox against their own quota (§ Quota composition). Unlike the export twin — whose identical gap was closed 2026-09-21, and which no app can yet start — **the import wizard ships on all seven apps, so this one was reachable by a real user.** § Wizard steps step 3 now states the locus and the scope rule, adopting `mail-export.md` § UX shape step 2's date grammar rather than minting a second one, and the filter is client-side on the INTERNALDATE the source already returns for every fetched message. **Additive on the wire and at rest, following the 2026-08-27 `scope` precedent exactly:** `date_from` on `StartImportSessionRequest` + `ImportSessionInfo` (`#[serde(default)]`), and an `import_sessions.date_from` column (in `MIGRATIONS_IMPORT_SESSIONS`, `TEXT NOT NULL DEFAULT ''`). **A cold-resume degrade was considered and rejected**: the export ratifies that a cold resume keeps its range; an empty `scope` degrades *visibly* (a resume has nothing to iterate) where a forgotten date degrades *silently*, into the very whole-mailbox import the field exists to prevent; and the import's own restart-resume is still the follow-on pass `import.rs` names — which is exactly why the field has to rest on the row **before** that pass exists, as `scope` did. Proven by the machine's unit tests over the in-memory source and nest: a since date imports exactly the in-range messages and leaves `skipped_count` at zero, the day boundary is pinned to the second, a malformed date opens no session, the date reaches the row and comes back on the session view, and a pause → resume keeps the floor. **This closes the first of the two sibling gaps `mail-export.md` § Implementation status today captured on 2026-09-21**; the second — the archive import's "until" held at its day's *first* instant — closed the same day, 2026-09-22 (`mail-export.md` § Implementation status today).

Read this section first when scoping a slice; §§ below are the ratified target.

## RPC surface (implemented shape)

All kinds are **User-class and caller-scoped** — there is no `actor_id` request field; nest derives the owning actor from the authenticated caller (the codebase's User-class convention, same as the account-alias family). Naming mirrors `mail-export.md` § RPC table. `fail`/`finalize` exist because `errored`/`completed` are ratified states that need a mutation path (the client detects source-side failure and end-of-enumeration; nest records + pushes).

| RPC (`fauna.bridges.*`) | Purpose | Shape notes |
|---|---|---|
| `start_import_session` | open the row at the wizard's durable commit point | `(source_descriptor, total_count, scope, source_sealed) → session_id`; typed `import_source_locked` while a running/paused session holds the same source — **the lock is keyed on `source_hash`, not the plaintext**, which the boot scrub blanks once the row rests sealed (`architecture/encryption-at-rest.md` § Implementation status today, bullet 15 (b)). `source_sealed` is the **client-minted** sealed label over `source_descriptor` (`fauna_core::label_custody::seal_import_source`; convergent under `source_hash`) — client-minted because the root is the owner's and a nest holds no key on this plane, the same store-and-serve posture as `folders.name_sealed`. Additive and optional: a keyless caller (bearer-only connection; the web arm, whose import-start is stubbed) omits it and the row rests sealless, the ratified degrade (`file-sync.md` § Sealed names & paths), never a refused import. Replay-forbidden, like the two `import_message` kinds — the id is server-minted, so a blind retry cannot recover it and would report the caller's own session as a competing one; on a mid-call disconnect the wizard reconciles through step 1 below (owner: `architecture/transport.md` § Idempotency and reconnect-with-resume). `scope` (the selected source mailbox names, additive field landed 2026-08-27) is what makes § Resume protocol step 3 possible at all — without it a session resumed after a client restart has no record of which mailboxes it was importing; `#[serde(default)]` so an old client's request round-trips to an empty (degraded-resume) scope rather than a wire error. `date_from` (the scope step's since date, additive field landed 2026-09-22) is the same shape for a sharper reason: the row is the only durable record of the range the user asked for, so a session that never recorded it can only resume by importing everything they excluded (§ Wizard steps step 3). The bare `YYYY-MM-DD` the user typed, empty meaning unbounded; `#[serde(default)]` so an old client round-trips to unbounded rather than a wire error; a value that is not a real calendar day never arrives, because it refuses `Start` client-side before any session exists |
| `import_message` | single-message import | `(session_id, message, skip_dedup) → (outcome, counters)`; `message` carries `mailbox, flags, body (raw RFC 5322), timestamp, body_size, sender_domain, source_uid, source_uid_validity, dedup_key, envelope_key` — the nest seals body + nest-derived index hint at ingest (§ Nest-side sealing); the pre-Phase-3 `body_mode`/`index_hint` wire fields are retired (ignored if an old sender includes them); an over-inline-ceiling body rides as `staged_body: Option<StagedBodyRef>` — designed 2026-07-18; the nest half is built and no client produces one yet (see the transport-ceiling block above, and § Batching) |
| `import_message_batch` | ≤32 msgs per call, byte-packed to the 2 MiB WS frame (`BatchPacker::with_limits`; the nest-side 16 MiB check is a defense-in-depth bound — § Batching) | adds `revised_total_count`; per-message transactions; index-aligned outcomes |
| `list_import_sessions` | resume protocol step 1 | `() → sessions` (all non-expired states; running/paused are resumable). Each session carries `source_sealed` **and its `source_hash` salt**; the client renders the source sealed-first (`label_custody::render_import_source`). The pair ships together or not at all — the salt derives from `source_descriptor`, which is exactly the column the scrub blanks, so a reply carrying the seal alone labels every session correctly until the first reboot and none of them after |
| `pause_import_session` | running → paused | `(session_id) → session` |
| `resume_import_session` | paused → running | 〃 |
| `cancel_import_session` | running/paused → cancelled | 〃 (already-imported messages are kept) |
| `finalize_import_session` | running → completed | 〃 + emits `BridgeImportComplete` |
| `fail_import_session` | running/paused → errored | `(session_id, reason)` + emits `BridgeImportError` |

The five state-transition kinds (`pause`/`resume`/`cancel`/`finalize`/`fail_import_session`) all declare `forbid_replay: false`, so — per `architecture/transport.md` § Idempotency and reconnect-with-resume — the handler must be naturally idempotent: a call landing after the session already reached the target state replies **success with the current row**, never a wrong-state error, since `allowed_from` only matches the *source* state and cannot itself distinguish "impossible" from "already converged". `fail_import_session`'s replay does not overwrite the original `reason` with whatever the replay carries.

The per-message resume cursor advances for **every processed message** — imported, skipped, or errored — so resume never refetches a source UID the client already handled. The dedup opt-out (§ Opt-out per session) rides as the `skip_dedup` request flag; opted-out imports still *populate* the index (first writer wins).

---

## Goal

A user (newly onboarding to Fauna, or an existing user adding an additional mailbox) can pull their existing mail from a foreign IMAP server (Gmail, Outlook/Hotmail/Office365, iCloud, generic) into their Fauna mailbox **without leaving the Fauna app**, without storing credentials nest-side, and without losing any structure (per-mailbox layout, dates, flags) the source server preserves.

The migration is **client-driven**: the user's Fauna app opens the IMAP session to the source server directly using shared-Rust IMAP-client logic (`libs/fauna-mail/src/imap_client/`, feature `imap-client`) and submits each fetched RFC 5322 message to nest as the importer's own actor, on the importer's own WS-RPC connection. Nest does **not** hold the user's foreign-IMAP credentials, ever — not "during the import," not "until the import completes." The MTA bridge is bypassed entirely; this is not external mail entering through the SMTP perimeter, it's the user's own client writing to their own mailbox via a **new User-class import RPC** (`import_message`, § Per-message flow) that shares `fauna.bridges.append`'s wire shape and lands on the same nest-side write path (`imap-server.md` § Write surface). The BridgeMda-class `append` itself is **not** callable by a user client — the per-role allowlist denies it — which is exactly why the import RPC is a distinct User-class kind.

**Bar: parity with Gmail's "Mail Fetcher" / Apple Mail's "Import Mailbox" / Thunderbird's "ImportExportTools NG."** All three are well-understood UX shapes; Fauna's import surface is the same conceptual shape, with two additional constraints (credentials don't transit nest; the client owns retry / resume) the others can't deliver because their architectures don't permit it.

---

## UX shape

The import wizard is the **`mail-import` page — a sub-page of `mail-settings`** reached via the Mail sub-nav, exactly like `mail-aliases` / `mail-lists` and symmetric with the `mail-export` wizard (`mail-export.md` § UX shape names it as its "(future) `mail-import`" twin; the 2026-06-01 mail-UX design pass flattened this whole family onto `mail-settings` sub-pages — the earlier account-detail/Bridges-page placement is superseded). Element IDs use the `mail-import-*` prefix — **ratified in `ui.yaml` (user-approved 2026-08-27)**, mirroring `mail-export`'s "IDs precede the implementation" precedent — no app renders the page yet.

### Wizard steps

1. **Source picker** — radio choice of provider:
   - **Gmail** — uses Google's app-password flow (requires 2FA on the user's Google account). Rationale: Google's 2024 turndown removed password-only ("Less Secure Apps") IMAP; OAuth (XOAUTH2) over IMAP remains Google's first-class path but requires restricted-scope app verification that is impractical for a self-hosted client, so app-password is the maintained path for us. The UI explains the 2FA + app-password setup in a sub-screen with a deep link to Google's app-password page (`https://myaccount.google.com/apppasswords`). The user pastes the 16-character app password.
   - **Outlook / Hotmail / Office365** — OAuth via Microsoft Graph (preferred, when available) **or** IMAP user/password fallback for personal accounts that don't support OAuth. The OAuth flow uses the same client-side OAuth pattern as the existing Bluesky bridge (`docs/goal/behavior/bridges.md` § Linking a Bluesky account (OAuth)) — the client redirects the browser, the OAuth callback resumes in the client, and the resulting access token is held client-side for the import session only. (Nest does not store the OAuth token; the OAuth dance terminates on the client.)
   - **iCloud** — Apple's app-password flow (requires 2FA, generated at `https://appleid.apple.com`). Same as Gmail's app-password pattern.
   - **Generic IMAP** — user/password against any IMAP server. Required fields: server hostname, port (default 993), TLS mode (implicit / STARTTLS), username, password.

   **Picking a preset provider (Gmail/Outlook/iCloud) replaces any host/port/TLS mode the user had already entered with that provider's fixed connection triple.** The shared `fauna_client_mail_settings::connect_actions` helper relies on this for **Gmail/iCloud only**: it omits `SetHost`/`SetPort` from the dispatched action sequence for those two kinds, which is only safe because selecting the kind is itself what wrote `host`/`port`/`tls_mode` — a caller must dispatch `SelectSourceKind` (never derive the connect-time kind from local UI state alone) before calling `connect_actions`. **Outlook is the exception**: it sits beside a disabled-OAuth fallback field the user can still type into, so `connect_actions` always sends whatever host/port the caller passes, never the preset the snapshot holds — `outlook.office365.com` is a default the picker paints, not a value that route ever dispatches.

2. **Connection test** — the client opens a TLS IMAP session to the source, LOGINs, and `LIST "" "*"` to enumerate the source mailboxes. If the connection fails (wrong creds, TLS handshake fail, host unreachable), the wizard says what went wrong in a plain sentence — could not sign in, could not reach the server, could not secure the connection — plus a hint specific to the provider (Gmail / Outlook / iCloud); the source server's own words go to the app's log, never to the surface (§ Don't do these, which rules this — corrected 2026-10-01: this step had said "verbatim"). Everything the user typed is kept, so they correct only what was wrong and retry from this screen. **Today the surface is the raw error and no failure hint exists** (§ Implementation status today).

3. **Scope picker** — defaults:
   - Mailboxes: **every mailbox the source lists is offered, and all start ticked except the ones that hold discards or duplicates** — Trash, Junk/Spam, and Gmail's `[Gmail]/All Mail`, `[Gmail]/Bin`, Important and Starred, whose contents are discards or re-include mail already in another mailbox. Those start **unticked, never hidden**: the user ticks one to take it, or unticks any other to leave it out. They are recognized by the source's special-use attribute where it advertises one (`\Trash`, `\Junk`, `\All`, `\Flagged`, `\Important`) and by their well-known names otherwise. (Corrected 2026-10-01: this step named two of them, § Don't do these said two more were "skipped", and the code unticks five by name — one rule now, the export wizard's own "everything except Trash and Junk" convention. **Built today:** the five names `trash`, `junk`, `spam`, `[gmail]/all mail`, `[gmail]/bin`, matched by name only; Important, Starred and the attribute match are not — § Implementation status today.)
   - Date range: **all messages** (user can set a "since" date to import only recent mail; "until" is rarely useful). **The date's shape, its meaning and its refusal of a bad value are `mail-export.md` § UX shape step 2's, adopted rather than minted afresh** — one date grammar across both halves of the mail-portability surface, so a user who has set an export range meets no second convention here; only the "until" half is absent, because this step offers none. **What is the import's own is the locus: the filter runs client-side**, on the INTERNALDATE the source already returns for every fetched message (§ Per-message flow step 1), so it needs no new IMAP command and no trust in a source server's `SEARCH`; a `UID SEARCH SINCE` pre-pass over § Resume protocol step 3's enumeration is a **transfer optimization deferred** exactly as the export's nest-side filter is — worth having against a remote source, never the thing the user's range depends on. **A message the date excludes is outside the import's scope**: it is not sent, it never reaches dedup or the quota check, and it is **not counted in `skipped_count`** — that counter is for messages the import wanted and could not take (§ Progress lives nest-side) — while the resume cursor still advances past it, so a paused import does not re-walk what it already ruled out. The date is **recorded on the session row** (§ Progress lives nest-side's `date_from`, § RPC surface's `start_import_session`) and re-applied by every resume, warm or cold: the alternative — a resumed session that has forgotten its range — is a silent whole-mailbox import charged to the user's quota (§ Quota composition), which is precisely the control the field exists to give them. Its one visible cost is the estimate: the source's `EXISTS` counts a whole mailbox, so under a since date `total_count` starts overstated and the loop revises it down to the truth as each mailbox finishes (§ Progress lives nest-side's `total_count`).
   - Max message size: **50 MiB per message** (matches Gmail's send limit; user can lower).
   - Destination mailbox mapping: 1:1 by default (source `INBOX` → Fauna `INBOX`, source `Sent` → Fauna `Sent`, etc.; source mailboxes with names matching no Fauna standard mailbox land as user-created mailboxes named after the source).

4. **Confirmation** — total messages + estimated bytes summarized; the user clicks "Start import." This is the durable commit point — nest opens an `import_session` row at this step.

5. **Progress** — full-screen progress UI:
   - Overall: `X of Y messages imported / Z skipped / W errored`, progress bar, elapsed + ETA.
   - Per-mailbox: list with per-mailbox progress.
   - Per-message error log: scrollable list of any skipped / errored messages with the per-message reason (parse fail, oversize, dedup-skip, source-side fetch error).
   - Buttons: **Pause**, **Resume**, **Cancel**. Pause persists; resume picks up where it left off. Cancel asks for confirmation; on confirm the import is aborted but already-imported messages are kept (canceling does not delete).
   - The screen survives app restart — the user closes the laptop, opens it later, returns to the same Fauna app surface and sees the progress UI resumed (the client re-subscribes to the import session's push events).

6. **Done** — summary card (X imported, Y skipped, Z errored, time elapsed); buttons for "View imported messages" (deep-link into Inbox) and "Review skipped" (deep-link into the skip log).

### Credential handling

| Credential | Where it lives | Lifetime |
|---|---|---|
| Source server password (app-password / IMAP user-pass) | client memory only, inside the IMAP client session struct | until LOGOUT (end of import, error, or cancel) |
| OAuth access token + refresh token (Microsoft Graph) | client memory only | until the import session ends; refresh tokens are discarded at end-of-session (the client does not persist them for a future import) |
| Source server hostname / port / TLS mode | client persistent storage (per the client's own state machine) | until the user re-uses or deletes the import setting |

**Nest never receives the source credentials.** The MUA-AUTH dance against the source server happens entirely on the client. Nest sees the WS-RPC traffic from the user's authenticated Fauna actor and the message bytes — never the credentials that produced them.

The above is a hard rule, not a default. A future "queued import" feature (where the user starts the import on one device, closes the client, and expects it to continue from another device) would require the credentials to land nest-side; that feature is **out of scope for v1** for exactly this reason. v1 imports are bound to the originating client process; the user keeps the client open or pauses.

**Retain-on-failure exception — a seam that can never connect.** The retain-on-failure behavior implicit above (a failed `Connect` leaves the typed password in place so the user can retry without re-entering it) assumes failure is *transient*. On a platform whose `ImportSourceNest` implementation cannot ever succeed — today, web, until the relay transport (§ Where the IMAP client runs) lands — that assumption is false: retry-without-re-entering is a benefit that platform can never deliver, so retaining the password on a failed connect is pure exposure with no offsetting benefit. Such a seam answers `false` from `ImportSourceNest::can_ever_connect()` (default `true`); `MailImportMachine::connect` (`libs/fauna-client-mail-settings/src/import.rs`) checks it on a failed connect and clears the password instead of retaining it when the answer is `false`. The wasm `RpcImportSourceNest` stub (`rpc_glue.rs`) is the current instance; the exception is named for the condition, not for web, so it retires on its own the day the web relay transport lands and that seam starts answering `true`.

---

## Client-driven streaming model

The data path:

```
Source IMAP server  ←——  client (shared-Rust IMAP client)  ——→  nest (fauna.bridges.import_message)
                          \                                       /
                           \      WS-RPC progress events       /
                            ────  ←─── (push: progress, errored) ───  ←──
```

### Where the IMAP client runs — the transport seam

The browser has no raw TCP, so the shared client cannot own its socket. Everything carrying ratified *behaviour* — the IMAP protocol state machine, the § Throttling caps, § Batching, the § Dedup key call, and § Resume protocol cursor handling — lives once in `libs/fauna-mail/src/imap_client.rs` (feature `imap-client`), generic over an **`ImapTransport`** byte seam that each platform shell implements.

| Layer | Owner | Notes |
|---|---|---|
| Protocol state machine, throttle, batching, dedup key, cursor | shared Rust (`fauna-mail`) | one implementation, all apps |
| `ImapTransport` — a byte pipe carrying **post-TLS plaintext IMAP** | platform shell | native: `tokio::net::TcpStream` + `tokio-rustls`. web: sans-io `rustls::ClientConnection` over a WebSocket relay |
| `ImapClock` — `now_ms` + `sleep_ms` | platform shell | shared code may not call `Instant::now()`; it panics on `wasm32-unknown-unknown` |

Both seams are **AFIT** (`async fn` in trait, unboxed), not `#[async_trait]`, so `Send` is inferred per implementation — native futures are `Send`, wasm futures are `!Send`, and one trait serves both. This mirrors `fauna_protocol::RpcRequester`, whose module docs give the same rationale.

**The web relay is a blind byte tunnel, and TLS terminates inside the client.** The relay ferries opaque ciphertext and never observes the IMAP session. A relay that *terminated* TLS — nest logging in to Gmail on the user's behalf — would hand nest the user's foreign-mailbox credentials and their entire mailbox in the clear, contradicting § Credential handling ("Nest never receives the source credentials … a hard rule, not a default") and the *User always controls their data* product invariant. **A credential-terminating relay must never be built.**

Feasibility is verified, not assumed: `rustls` 0.23 and the `imap-proto` response parser both compile for `wasm32-unknown-unknown`, and `rustls::ClientConnection` is sans-io (bytes in, bytes out), which is exactly the tunnel shape. **Native ships first** — the web relay host is not yet deployed — but web import is technically reachable and is *not* a permitted priority-#1 deviation.

**Source access is strictly read-only.** The client `EXAMINE`s (never `SELECT`s) each source mailbox and fetches with `BODY.PEEK[]` (never `BODY[]`), so neither `\Recent` nor `\Seen` is disturbed: a user who imports their Gmail finds Gmail exactly as they left it.

#### The two TLS modes, and where each half lives

§ Wizard steps ratifies two source TLS modes for *Generic IMAP* — **implicit** (TLS from the first byte; the 993 default, and what all three provider presets use) and **STARTTLS** (negotiated over a plaintext 143 connection). There is deliberately **no plaintext mode**: the source password crosses that connection, so an unencrypted source session is never an option a client may offer.

The split follows the seam. The **command exchange** is protocol, so it is shared (`ImapSession::connect_starttls` sends `STARTTLS` and awaits the tagged `OK`); the **handshake** is platform, so it is the transport's (`ImapTransport::upgrade_tls`, an in-place upgrade). Neither half is reimplemented per client, and certificates still never reach shared code.

Three refusals are load-bearing, all of them plaintext-downgrade hazards:

- **A `PREAUTH` greeting on a pre-TLS connection** — whatever authenticated the user crossed the wire in the clear.
- **Bytes buffered across the handshake boundary** — data the server sent after the `STARTTLS` `OK` but before TLS came up belongs to the *plaintext* stream, and reading it back afterwards would credit an attacker's bytes to the authenticated server (the STARTTLS command-injection class, CVE-2011-0411). RFC 3501 §6.2.1 requires discarding it; we refuse the session, since a well-behaved server never produces it.
- **A rejected `STARTTLS`** — there is no plaintext fallback, because a MITM can always manufacture a `NO`, and falling back would hand it the password.

Pre-TLS capabilities are never consulted: stripping the `STARTTLS` advertisement is the classic downgrade, so trusting the pre-TLS `CAPABILITY` to decide whether to *attempt* TLS defeats the point. The client issues the command unconditionally and reads capabilities *inside* TLS afterwards, as RFC 3501 §6.2.1 requires.

### Per-message flow

For each (source mailbox, source UIDVALIDITY, source UID):

1. **Client `FETCH` on source** — `UID FETCH <uid> (UID FLAGS INTERNALDATE BODY.PEEK[])` against the source IMAP session. Skip if oversize (compare `RFC822.SIZE` first when supported by the source).
2. **Client computes dedup key** — see § Dedup below.
3. **Client `fauna.bridges.import_message`** — **User-class** RPC (the caller is the user's own Fauna app, caller-scoped; not MDA-class — the allowlist denies BridgeMda kinds to User callers): APPEND's field roles plus the source-tracking / dedup annotations, returning `(uid | skipped_reason | errored_reason)` per message. Exact wire shape: § RPC surface above (`source_descriptor` lives on the *session*, not each message; the caller is the actor, so no `actor_id` field rides the wire).
4. **Nest seals at ingest, then applies the normal storage path** — the client sends the raw RFC 5322 bytes; the nest handler (`bridge_import_handlers.rs::import_one`) derives the search-index hint from the plaintext (same tokenizer as the MTA and `seal_and_persist_local`), seals body + hint to the recipient's registered seal key through the D2 resolver, **fail-closed on a missing key**, then writes a new `bridge_imap_messages` row + bumps `uid_next` + `highestmodseq` (same as a regular APPEND). The MTA bridge is **not** in this path; the seal that APPEND gets from its trusted BridgeMda caller the import path performs nest-side, because its caller is an untrusted user client (at-rest sealing owner: `encryption-at-rest.md` — S1 uniform seal at ingest).
5. **Nest updates the import session row** — `import_sessions(session_id, actor_id, source_descriptor, started_at, total_count, imported_count, skipped_count, errored_count, last_processed_source_uid, last_processed_source_uid_validity, state)`. Emits a `BridgeImportProgress` WS push event after each batch.
6. **Client receives the push** — updates UI progress; if the message was skipped or errored, the per-message reason is included in the push and accumulated in client state for the end-of-import review log.

### Batching

The client batches per WS-RPC frame: up to **32 messages** per `import_message_batch` call (a sibling RPC to `import_message` that takes a list), byte-packed by `BatchPacker::with_limits` to what the 2 MiB WS-RPC frame actually accepts (§ Implementation status today — the 2026-07-12 cap resolution; the nest-side `MAX_BATCH_BYTES` = 16 MiB check is a defense-in-depth bound on the batch's *referenced* byte total, never an inline-frame promise; § Implementation status today records the single code owner both sides read). Batching reduces WS round-trips without blocking on a single 50 MiB message; the client decides per-batch based on the running byte total. Inside nest, each message in the batch is its own SQLite transaction (so a single bad message in the batch doesn't roll back its 31 siblings).

Nest enforces both ceilings as a **whole-call rejection**, not a per-message error, so an over-long batch loses all 32 messages. Two consequences the client must honour:

- A message whose body exceeds the inline ceiling (`mail-message-size.md` § Message size limits) — legal, since § Wizard steps step 3 allows 50 MiB per message — fits no batch at all, and rides the single-message `import_message` kind carrying a staged-envelope reference (`staged_body: Option<StagedBodyRef>`, § Implementation status today) instead. The nest-side resolution is built; no shipping app produces one yet — the wizard now ships on all seven apps, but the shared `run_import` loop never mints a `staged_body` (grep `staged_body` under `libs/fauna-client-mail-settings/` finds nothing), so an over-ceiling message is still not sent by any client. This is what "without blocking on a single 50 MiB message" means concretely.
- Emission order equals source order. The per-message resume cursor advances for every processed message, so flushing a pending batch *before* an oversized single is what stops the cursor from skipping messages that were never sent.

### Throttling

The client throttles its `FETCH` rate against the source server to **at most 4 concurrent FETCH operations** and **at most 100 messages per minute per source server**. These are conservative — major providers tolerate much higher — but the conservative defaults avoid trips to a provider's abuse desk and avoid surprises when a user imports a 50,000-message account in the background.

The throttle is per-source-server, not per-mailbox; a Gmail import opens one source IMAP session and pipelines its FETCH within the per-server cap.

---

## Progress lives nest-side

The `import_sessions` row is the source of truth. The client renders from it; pause / resume / cancel mutate it; client restart re-subscribes to its push events. Key fields:

| Column | Purpose |
|---|---|
| `session_id` | uuid; primary key |
| `actor_id` | the importer (the user's own actor) |
| `source_descriptor` | provider + hostname + username (no password) — used by dedup + by the UX to label the session. **Content, not routing:** it is the user's external mailbox identity, so it rests **sealed** (`source_sealed`) and its plaintext is scrubbed at boot; readers render it through `label_custody::render_import_source` |
| `source_sealed` | the client-minted sealed label over `source_descriptor`, stored verbatim by the nest and opened only by the owner. `NULL` for a row a keyless client opened |
| `source_hash` | `import_source_hash(source_descriptor)` — both the seal's salt and the key the multi-device per-source lock reads (the plaintext lock index was rebuilt onto it at the v32 flip) |
| `state` | `running` / `paused` / `errored` / `completed` / `cancelled` |
| `started_at` | wall-clock start |
| `last_progress_at` | wall-clock of the most recent message-imported event |
| `total_count` | estimated; revised as the client enumerates the source |
| `imported_count` | committed to a Fauna mailbox |
| `skipped_count` | a message the import wanted and did not take: a dedup hit, one the mailbox had no room for (`quota_exceeded`), or one over the user's own size limit (§ Per-message flow step 1 — the client leaves it unfetched and lists it as too large; **unbuilt**, § Implementation status today). Never a message the scope's date or its unticked mailboxes left out — those are never sent and never counted (§ Wizard steps step 3). A message over the *nest's* ceiling that a client sends anyway is refused `message_too_large` and is `errored_count`'s |
| `errored_count` | per-message parse fail / RPC error / source FETCH error |
| `last_processed_source_uid_per_mailbox` | per-mailbox dict; the resume cursor |
| `last_processed_source_uid_validity_per_mailbox` | per-mailbox; if the source UIDVALIDITY rotates mid-import (rare — usually means the source mailbox was rebuilt) the client surfaces a warning + restart-import option |
| `scope` | the source mailbox names selected at `start_import_session` (landed 2026-08-27) — recorded once, never mutated after; what a resumed client re-`EXAMINE`s in step 3 below. Empty for a session an old client started before this field existed |
| `date_from` | the scope step's "since" date, as the bare `YYYY-MM-DD` the user typed (landed 2026-09-22) — recorded once beside `scope`, never mutated after; empty means unbounded. What lets a resume re-apply the range rather than silently importing the whole mailbox (§ Wizard steps step 3). Empty for a session an old client started before this field existed |
| `expires_at` | 30 days after `last_progress_at`; nest GC-removes the row after that |

Push events: `BridgeImportProgress { session_id, imported_count, skipped_count, errored_count }` after each batch; `BridgeImportError { session_id, reason }` on a session-fatal error (auth fail, network down past timeout); `BridgeImportComplete { session_id, summary }` at end.

### Resume protocol

On client restart or reconnect, the client:

1. `fauna.bridges.list_import_sessions(actor_id)` → list of sessions in `running` / `paused` state.
2. For each `running` session, the client re-opens its source IMAP connection (re-uses stored hostname / port / TLS / username; **the user re-enters the password** — credentials live in client memory only, so they're gone after the client process exits or restarts).
3. The client `UID FETCH last_processed_source_uid+1:* (UID RFC822.SIZE)` per mailbox to discover the next UIDs to fetch, and resumes. **The returned set must then be filtered to `uid >= last_processed_source_uid+1`:** RFC 3501 §6.4.8 specifies that a `<n>:*` UID range "always includes the UID of the last message in the mailbox, even if `n` is higher than any assigned UID value", so a mailbox that is fully imported re-yields its final message forever. Dedup would mask this; a `skip_dedup` import would not. **The session's `date_from` is re-applied to the resumed walk** — from the wizard's own state while the app that started the import is still running, and off the row (§ Progress lives nest-side) when it is not. A resume that dropped the range would import every message the user excluded, against their quota, and dedup would not stop it: those messages were never imported, so there is nothing for it to match.
4. Before fetching, the client re-`EXAMINE`s the mailbox and compares `UIDVALIDITY` against the stored cursor. A mismatch means the source mailbox was rebuilt and every stored UID is meaningless: the client surfaces the warning + restart-import option rather than importing against the new namespace.
5. The progress UI re-subscribes to `BridgeImportProgress` pushes.

The "user re-enters password on resume" friction is intentional — it's the same product invariant as v1's no-queued-import rule. If the friction becomes intolerable, the future "queued import" feature would relax it; until then, the rule stands.

---

## Failure handling

### Source-side errors

| Failure | Action |
|---|---|
| `LOGIN` rejected (auth fail) | Session moves to `errored` with a typed reason; UI offers "Try again with new credentials." |
| `EXAMINE mailbox` failed (mailbox vanished on source — the client never `SELECT`s, § Where the IMAP client runs) | Per-mailbox failure; the wizard logs it in the skip review list and continues with the next mailbox. |
| `FETCH` returned a parse error (source served malformed) | Per-message; `errored_count++`; logged with `reason=source_fetch_parse_error`; continue. |
| TCP / TLS error mid-session | Up to 3 retries with exponential backoff (5 s, 30 s, 2 min) before the client **pauses** the session with the reason shown — never `errored`, which is terminal and has no way back (§ RPC surface: `resume_import_session` is `paused → running` only). The resume cursor is kept, and the user resumes after fixing the network. (Ruled 2026-10-01, refutable until built: this row had said `errored` *and* "resume manually", which the state machine cannot both honour. **Today the loop fails the session** — § Implementation status today.) |
| OAuth token expired (Outlook flow) | Refresh once using the refresh token; if refresh fails, session moves to `errored` and asks the user to re-authorize. |

### Nest-side errors

| Failure | Action |
|---|---|
| `import_message` answers a message `quota_exceeded` (a per-message outcome, never a failed call — § Quota composition) | On the first such answer: pause the session, surface a "your Fauna mailbox is full" dialog with options: increase quota (Tier-2 admin only — surfaced as "ask your admin"), skip remaining (the rest of the import is recorded as skipped with `reason=quota_exceeded`), or cancel. |
| `import_message` returns `parse_error` (the body wasn't valid RFC 5322 — the source mailbox held mail that even `mail-parser` won't accept) | Per-message; `errored_count++`; logged with `reason=parse_error`; continue. |
| `import_message` returns `mailbox_not_found` (destination mailbox vanished — e.g., the user deleted it from another client during the import) | Auto-create the mailbox + retry once; if it fails again, move the session to `errored`. |
| WS-RPC connection drop | Client retries WS reconnect per the transport's existing exponential backoff (`docs/goal/architecture/transport.md`); no data loss, the resume cursor is at the last committed nest write. |

### Per-message error budget

The session moves to `errored` if **either** of:

1. More than 10% of messages errored in the most recent 1000-message window (running fraction).
2. More than 50 consecutive messages errored (the source is broken or our side is misconfigured — fail fast).

Both thresholds are admin-tunable — the knob names `mail.import.error_window_fraction` and `mail.import.consecutive_error_cap` are **target catalog entries**, added to the `mail-policy-config.md` catalog when this feature builds (not yet catalog rows); defaults above are the shipping shape.

---

## Dedup vs. existing mail

The dedup key per imported message:

1. **Primary: `Message-ID` header value** (RFC 5322 §3.6.4), normalized (case-fold the angle-bracketed `local@domain` part, trim whitespace, strip the angle brackets).
2. **Fallback (no Message-ID present): SHA-256 of canonical envelope** — `From || To || Cc || Date || Subject || sha256(body-bytes-with-RFC-5322-normalized-line-endings)`. The components are concatenated with the literal `\x00` separator (not present in valid mail headers) so no value collisions.

Both forms are computed for every message. The primary form (or, without a Message-ID, the envelope form) is the **dedup key** the index is looked up by; the envelope form is *also* always recorded, as the **envelope key** that confirms a hit (§ The envelope key confirms a Message-ID hit, below).

The primary form inherits RFC 5322 §3.6.4's global-uniqueness rule, so **Fauna's own outbound mail must honour it**: both submission paths mint a 128-bit random Message-ID (`fauna_conversations::rfc5322::new_message_id` in the apps; the RFC 6409 §8.3 stamp in the Go submission server). A clock-plus-counter id — which the compose path used until 2026-07-12 — collides across a user's devices and would make one of two genuinely distinct messages invisible to a later import. The randomness is *also* load-bearing for the guardian mail gate's bounce correlation, which owns the security half of the requirement: [`family-safety.md`](family-safety.md) § The mail gate.

### Key format (the stored string)

One definition, shared Rust: `libs/fauna-mail/src/dedup_key.rs::mail_dedup_keys`, returning the pair
`(dedup_key, envelope_key)` as the record `MailDedupKeyPair`, uniffi-exported so the Go MDA/MTA call it rather than
reimplementing it. There is no single-key entry point: a producer that could send one string without the
other would re-open the hole the envelope key closes.

**Its only argument is the whole raw RFC 5322 message.** Four independent producers compute this key —
the user's client at `import_message`, the Go MDA at IMAP `APPEND`, the Go MTA at the perimeter
pre-seal, and the nest at its own in-domain delivery — and the key is worthless unless all four agree
byte for byte. Passing pre-parsed envelope
fields would push the join convention for a multi-address `To:`, and the rendering of `Date:`, onto each
caller; two callers disagreeing there produce two different keys for one message and dedup silently stops
working. The parse therefore lives inside the shared function, once. The nest calls it only where it
seals plaintext it holds (§ The envelope key confirms a Message-ID hit); at the MTA and MDA doors it
holds only the sealed body.

The two `actor_message_dedup` `TEXT` columns `dedup_key` and `envelope_key` hold exactly the pair it returns:

```text
primary  = "msgid:v1:" ‖ lowercase(strip_angle_brackets(trim(Message-ID)))
envelope = "env:v1:"   ‖ hex(sha256(From ‖ 0x00 ‖ To ‖ 0x00 ‖ Cc ‖ 0x00 ‖ Date ‖ 0x00
                                    ‖ Subject ‖ 0x00 ‖ sha256(crlf_normalize(body))))

dedup_key    = primary when the message carries a Message-ID, else envelope
envelope_key = envelope, always
```

Three properties the two forms depend on, each pinned by a unit test:

- **The `v1` fence.** A change to either normalization is a **new version prefix**, never a silent
  redefinition — old keys must keep matching old keys, and the index is not re-derivable from a sealed
  body (§ Implementation status today, item 1).
- **Disjoint prefixes.** `msgid:` vs `env:` keeps a Message-ID that happens to look like a hex digest
  from aliasing an envelope hash.
- **Asymmetric case handling.** The Message-ID form case-folds (real duplicates differ in case far more
  often than distinct messages collide by it); the envelope form is **byte-exact** on the five header
  values, because there the full tuple *is* the equality signal. A Message-ID that is absent, empty, or
  `<>` selects the fallback.

`crlf_normalize` rewrites every bare `\n` and bare `\r` to `\r\n`, so a body that round-tripped through
a Unix-line-ending store still hashes to the same key.

### The envelope key confirms a Message-ID hit

The primary key is chosen by the sender. Every inbound MTA delivery writes the index, first writer wins, and nothing removes a row when the copy is expunged — so with the identifier alone as the signal, any internet sender who knows a Message-ID in a mailbox the user has not yet imported can plant a row that makes the real message skip as `dedup` at every later import (ruled 2026-09-28). A skip is therefore earned by the *content*, not by the identifier:

- **The lookup is unchanged.** `import_message` finds the actor's row whose `dedup_key` equals the item's, in any mailbox (§ Dedup scope).
- **A hit skips only when the envelope keys agree.** `agree(stored, candidate)` is equality of the two keys (§ *There is no absent key*, below). Two envelope keys that disagree are two messages: the item is stored, the index keeps pointing at the first copy (first writer wins, as everywhere in this table), and the per-message log records an ordinary import — the message was simply not a duplicate.
- **Why the envelope form and not a new digest.** The canonical envelope hash is the content-bound identity this doc already ratifies for Message-ID-less mail: it binds the body bytes and the five user-visible header values, every producer already computes it, and it needs no new version fence. A copy that matches on both keys is a faithful duplicate of what the user holds (same author, recipients, date, subject and body), and skipping a faithful duplicate is exactly Track B's ratified behavior; what the rule removes is the identifier-only match a stranger can mint. Header rewriting in transit (a re-folded `Subject:`, a list-munged `From:`) or a body change (a list footer, a 7-bit re-encoding) makes the two copies disagree, and the import stores the second one — a double-store, never a lost message, which is the failure direction this rule chooses (§ Dedup scope, second example).
- **There is no absent key (user-ruled 2026-09-30, under the compat-remnant sweep's fourth exception — `../architecture/version-compatibility.md` § Dimension 2).** Every row carries both keys and every candidate sends both, so `agree` takes two strings and has no "absent agrees with anything" arm. The earlier absent-key rule — a NULL row or a key-less item keeping the Message-ID-only match "for pre-existing mail or older clients" — was a pre-sweep compatibility shape: no such row or client exists, and the identifier-only window is closed rather than merely bounded. An empty string is an absent key by another name, so the nest refuses one at every door: the whole request at delivery and APPEND, the one item at `import_message`.
- **Producers send the pair, always — all four of them.** The three external producers (the Go MTA, the Go MDA, the import client) call the one shared function and send both strings as **required** wire fields on `ImportMessageItem`, `AppendMessageRequest` and `IngestInboundMailRequest`; and the nest's own in-domain delivery (`fauna.email.send` to a recipient on the same nest, the one door where the nest holds the plaintext it seals) computes the pair from the unstamped raw message the same way the MTA does, so mail between two users on one nest is indexed like any other delivery — as is everything else the nest seals from plaintext it holds: the sender's Sent copy and the notices the nest writes itself (bounces, security notices). The nest records both columns at every write path and applies `agree` only at `import_message`; APPEND and MTA delivery remain record-only. The agreement rule is shared Rust beside the key computation (`libs/fauna-mail/src/dedup_key.rs`), so the nest's check cannot drift from the definition.

### Dedup scope

A message is a duplicate (and thus skipped) iff its dedup key matches an existing row in **any of the actor's mailboxes**, not just the destination mailbox, and the two envelope keys agree (§ The envelope key confirms a Message-ID hit). This is the right default — re-running an import shouldn't double-store identical mail just because the user picked a different destination this time. Concrete:

- Re-importing the same Gmail account into INBOX after a previous import: every message dedup-hits → all skipped → "0 imported, 50,000 skipped" (correct).
- Importing a Gmail account where the user's iCloud account previously forwarded some messages to: the iCloud-via-forward messages already have Message-IDs from the original sender, dedup-hit → skipped (correct — the user already has those messages in their Fauna mailbox), provided the forwarded copy still carries the original's body and `From`/`To`/`Cc`/`Date`/`Subject` verbatim. A copy a forwarder or list rewrote on the way (a footer appended, the subject tagged, the sender munged) disagrees on the envelope key and is stored as the distinct message it now is.

The dedup scope is **per-actor**, not cross-actor. Two users sharing a Fauna deployment importing the same shared mailbox don't dedup against each other — each actor has its own mailbox set.

### Opt-out per session

The wizard surfaces an advanced option **"Import duplicates anyway"** (default unchecked). When checked, the import skips the dedup check entirely. Use case: the user is importing into a separate "archive" mailbox and wants a snapshot at a point in time, even of messages they already have. The option carries a warning ("this will double-store messages you already have").

### Dedup key persistence

The dedup keys live in a nest-side index `actor_message_dedup(actor_id, dedup_key, envelope_key, message_uri)` populated by every `import_message`, every regular `append`, every inbound MTA delivery and the nest's own in-domain delivery; both key columns are NOT NULL — every producer sends the pair (§ The envelope key confirms a Message-ID hit). Index size is bounded by the user's total message count. The index is plaintext (neither key is user content: the primary form is the normalized Message-ID **itself**, a routing identifier every relay on the path already handled in the clear, and the envelope form is a SHA-256 — both on the plaintext floor per `docs/goal/architecture/encryption-at-rest.md` § Plaintext floor by analogy with the existing message-and-event ID indexes); a different policy here would mean re-decrypting every message at every import to compute keys.

---

## Nest-side sealing (formerly "Storage mode interaction")

**There is no storage-mode branch on the import wire — the nest seals unconditionally.** *(Reconciled 2026-07-10 to the at-rest owner doc, exactly as the `:29` Track-B3 note was: the pre-Phase-3 prose here — client-pre-seals in encrypted mode, `mode=encrypted_preencrypted` discriminator — predated Phase 3 and contradicted the reader, which recognises only `seal_recipient_blob` output; flagged in a 2026-07-09 review, tracked internally. The storage-mode axis itself was retired 2026-07-12 — `nest/storage-modes.md`.)*

The client sends the raw RFC 5322 bytes it fetched from the source; the nest seals body + nest-derived index hint at ingest to the recipient's registered seal key, fail-closed — per-message flow step 4. Before the parse and the seal the nest removes every reserved `X-Fauna-*` delivery stamp the bytes carry (the namespace is written only by the door that files a copy, and a migrated copy — from a hostile source or from another Fauna nest — must not claim a spam tier or alias match this nest never decided; rule, door list and the `X-Fauna-Forwarded-By` carve-out: `smtp-server.md` § Architectural rules → *The `X-Fauna-*` namespace*). **A body the strip empties is refused, not filed** — bytes made of nothing but reserved stamps pass the empty-body refusal as sent and strip to zero, so the nest re-checks after the strip and answers that message alone with a per-message `Errored { "body is empty once its X-Fauna-* stamps are stripped" }`, its siblings still importing; the store never holds an empty body, which is what lets the export treat one as a caller bug and fail closed on it (`mail-export.md` § UX shape step 2). The dedup key is the client's, over the source bytes as fetched. At-rest sealing (why the nest seals, the D2 key seam, the one at-rest byte shape, what the MDA does on FETCH) is owned by `docs/goal/architecture/encryption-at-rest.md`; the imported record lands in the **`__mail/<actor>` segment-store placement, with the `bridge_imap_messages` row pointing at it** (the same shape every mail write uses — `mail-app-surface.md` § First-party client send), indistinguishable at rest from a regular APPEND.

---

## Quota composition

Imports count toward the user's **per-mailbox quota** (per `imap-server.md` § Composition with submission quota — STORAGE + MESSAGE resources). The STORAGE charge is the **sealed record size** (the bytes that actually rest on disk — slightly larger than the wire `body_size` by the seal overhead), so the client's headroom pre-check is a floor, not exact. The import client pre-checks headroom before each batch via the User-class **`fauna.quota.get`** (the account-cluster kind — `fauna.bridges.get_quota` is BridgeMda-only and denied to user clients); nest-side enforcement on `import_message` stays authoritative regardless:

- A message the mailbox has no room for is answered **per message** — `skipped`, reason `quota_exceeded`, counted in `skipped_count` — never as a failed batch: the messages before it in the same call have landed and stay. On the first such answer the client pauses the session and asks the user what to do (§ Failure handling → *Nest-side errors*: ask the admin for room, skip the rest, or cancel). (Corrected 2026-10-01: this bullet had the batch fail, § Implementation status today had the per-message skip, and § Failure handling had the pause and its dialog. The nest half is the built one; **the client's pause and dialog are not built — today the loop counts the skip and walks on**, § Implementation status today.)
- Submission quota (the per-actor 200 msgs/hour / 1000 recipients/day rate cap) does **not** apply — imports are not submissions. The user isn't sending mail; they're filing mail into their own mailbox.

This is the cleanest answer: import quota = mailbox quota, no special rate-cap for imports. A million-message imap account at the rate cap (4 concurrent FETCH × 100 messages/minute, per the throttle above) is on the order of 7 days of wall-clock for a maximum-throughput import; that is the user's expectation when they kick it off.

---

## Architectural rules

- **Client-driven.** The user's Fauna app is the IMAP client against the source server, full stop. Nest holds no foreign-IMAP credentials, no foreign-IMAP connections, no foreign-IMAP queue. (Future "queued import" — separate feature, out of v1 scope.)
- **One actor's import lives in their own client.** Imports are bound to the originating client process. Pause persists nest-side; resume requires the user to be back in a Fauna app + re-enter the source password. Multi-device parallel imports of the same source are blocked by a per-source lock in `import_sessions`, keyed on `source_hash` (the second client gets "this source is already being imported on another device"). It is keyed on the hash rather than the descriptor because the descriptor's plaintext does not survive the boot scrub: a plaintext-keyed check would stop matching after the first reboot and silently degrade the typed refusal into an opaque insert error.
- **Imports use the same write path as IMAP APPEND.** The same `bridge_imap_messages` row shape, the same `uid_next` / `highestmodseq` increment, the same per-mailbox quota check. Imports are not a special storage path; they're just APPENDs with a source-tracking annotation.
- **No nest-side mail fetcher.** Nest doesn't run an IMAP client. Even for "queued imports" if/when that feature lands, the architecture is "shared-Rust IMAP client running in a nest-spawned worker holding wrapped-blob credentials it can unwrap" — not "nest has built-in IMAP-client logic." (This is the same product-invariant logic that puts the SMTP bridge in `mail-bridge`, not in `nest`.)
- **Dedup is per-actor, by Message-ID first, by canonical-envelope-hash second — and a Message-ID hit skips only when the envelope keys agree** (§ The envelope key confirms a Message-ID hit). Both forms are computed by the producer at message-write time and live in `actor_message_dedup`. A new dedup mode is a new index column populated from its ratification onward, with an explicit rule for the rows that predate it: **no re-dedup pass over existing rows can exist**, because the nest cannot read the sealed bodies the keys derive from (*Not yet built*, item 1), so a mode that needs one is not implementable.
- **The nest seals; the import doesn't make storage-mode decisions.** The client sends raw RFC 5322; the nest seals body + derived index hint at ingest, fail-closed (§ Nest-side sealing). No client-pre-seal path exists — a pre-sealed body would be unreadable to the serve path's discriminator (owner: `encryption-at-rest.md`).
- **The import session is GC'd 30 days after the last activity.** Long-paused sessions disappear. The user re-runs the wizard if they want to pick up an abandoned import — the dedup index ensures already-imported messages are skipped, so re-running is cheap.
- **Throttle conservatively.** 4 concurrent FETCH × 100 messages/minute per source server, regardless of what the source might tolerate. We err toward "doesn't trip provider abuse detection" even at the cost of slower imports; a future user-visible "import faster (may trigger source-side rate limits)" toggle is fine but not v1.

## Don't do these

- Don't store the source-server password / OAuth token / app-password nest-side, ever. Not "during the import." Not "until the import completes." Not "in encrypted form." The credential lives in the client's memory and exits with the client process.
- Don't accept a source-IMAP credential through a Fauna app and forward it to the MTA bridge or to any nest RPC. The user's password is not a Fauna asset and never crosses the WS-RPC boundary.
- Don't fall back to a server-side fetcher for "long-running imports." The product invariant rule applies (per `mail-policy-config.md` § Architectural rules) and the v1 "user keeps the client open or pauses" friction is the right tradeoff for the long term.
- Don't write directly to `bridge_imap_messages` from a client. Imports go through the `import_message` / `import_message_batch` RPCs, which nest-side handlers translate to the same atomic-write path as `append`. Adding a side-channel for "fast import" is the kind of optimization that's a bug magnet.
- Don't dedup across actors. Cross-actor dedup would leak the existence of mail between accounts on the same deployment.
- Don't suppress the warning when "Import duplicates anyway" is checked. It's an advanced option for a real use case (snapshot-at-time) but it has real costs (double storage, double quota); a user who flips it deserves to know.
- Don't auto-translate Gmail's labels into Fauna mailboxes. Labels-as-mailboxes is Gmail-specific; importing a Gmail account creates one Fauna mailbox per Gmail folder-mode IMAP mailbox the Gmail server exposes (`[Gmail]/Sent`, etc.). Multi-label messages (same UID in multiple Gmail mailboxes from the same source UIDVALIDITY) are dedup-hit on the second import and stored once with a single Fauna mailbox placement. This is intentional — Fauna's mailbox model is flat, not label-based; converting at the user's request is a separate post-import feature, not part of the import contract.
- Don't import system messages by default — Gmail's "Important" and "Starred" pseudo-mailboxes start unticked on the scope step (§ Wizard steps step 3 owns the rule and its build state). Their contents are usually duplicates of mail already in INBOX.
- Don't surface the source server's wire response verbatim in a user-facing error without a human-friendly framing. "IMAP server returned `NO [AUTHENTICATIONFAILED] Invalid credentials (Failure)`" is fine inside the developer-mode error log; the surface error is "Couldn't sign in — check your password or app-password."

## Reading list

1. `principles.md` § Product invariants — the load-bearing "user always controls their data" + "nest config from apps, not CLI" rules.
2. `docs/goal/behavior/imap-server.md` § Write surface — the `append` RPC the import path piggybacks on.
3. `docs/goal/behavior/mail-policy-config.md` § Tier 3 — the per-account tier the `mail-import` wizard belongs to (the wizard page itself is a `mail-settings` sub-page, § UX shape).
4. `docs/goal/behavior/bridges.md` § Linking a Bluesky account (OAuth) — the client-side OAuth pattern reused for Outlook/Office365.
5. `docs/goal/architecture/encryption-at-rest.md` § Per-content-kind conformance — the sealed shape for stored mail.
6. `docs/goal/architecture/encryption-at-rest.md` § Plaintext floor — the rationale for keeping the dedup index in plaintext.
7. (design ratified 2026-05-07; tracked internally) § MTA encrypts at perimeter — where the client-side encryption path mirrors for imported mail.
8. `docs/goal/behavior/mail-export.md` — symmetric companion on the read-out path; reuses the `import_sessions` row shape verbatim as `export_sessions` with the same state machine + 30-day GC + push-event naming.
9. RFC 9051 (IMAP4rev2 — UID FETCH, UIDVALIDITY semantics), RFC 5322 §3.6.4 (Message-ID identity rule).
