# Mass mailing — target state

Owns: mass-mailing
Status: ratified
Authority: the legitimate-list / newsletter surface — the list alias kind's lifecycle, the `mail_lists` + `mail_list_members` storage shapes, the RFC 8058 one-click-unsubscribe pipeline (header stamping + token format + HTTPS endpoint + mailto handler + secret lifecycle/rotation), the RFC 2369 list-header set, per-list rate accounting, and the `mail-lists` / `mail-list-members` page UX; defers the alias-kind taxonomy + the reserved-local-part creation predicate to behavior/mail-aliases.md, per-actor submission rate semantics to behavior/smtp-server.md, knob tiers/defaults to behavior/mail-policy-config.md, and the per-domain alias namespace to behavior/mail-multidomain.md. On conflict: kind taxonomy → mail-aliases.md; submission rate semantics → smtp-server.md; policy-catalog naming → mail-policy-config.md; list pipeline + UX → this doc.

> **Audience:** the nest work maintaining the list pipeline (tables, the RFC 8058 HTTPS endpoint at `/list/unsubscribe?t=<token>`, the `unsubscribe+<token>@` mailto handler, the list rate accounting) and the shared `libs/fauna-mail/src/lists/` helpers; every per-app implementation of the `mail-lists` + `mail-list-members` pages (sub-pages of `mail-settings`, ratified into `ui.yaml` 2026-06-01).
> **Purpose:** the canonical doc for the legitimate-list / newsletter / mass-mailing surface: a user opts an outbound stream into list mode and gets RFC 8058 one-click unsubscribe, RFC 2369 list headers, and per-list rate accounting separate from the per-actor submission caps. Backend + tier_3 e2e are landed (§ Implementation status today); the list RPCs are catalogued in `docs/goal/architecture/apps/bridges.md` § Bridge-kind catalogue (caller classes).

## Implementation status today

**Backend foundation landed 2026-06-13** (tracked internally); the rest of the backend is the open remainder of that track. Landed:

- **Storage** — the `mail_lists` + `mail_list_members` tables (§ Storage) + the `account_aliases.kind = 'list'` discriminator (`fauna_mail::aliases::ALIAS_KIND_LIST`). `mail-aliases.md` § Alias kinds already carried the `List` row, so no taxonomy edit was needed.
- **Shared helpers** — `libs/fauna-mail/src/lists/` (the `lists` feature): the RFC 8058 `UnsubscribeTokenGenerator` (§ Token format) + the RFC 2369/8058 list-header builder (§ RFC 2369 list headers + § RFC 8058 one-click unsubscribe), both pure/WASM-safe.
- **Policy projection** — `MassMailingPolicy` in `FetchConfigReply` (the `mail.outbound.list_*` Tier-2 ceilings, § Per-list rate accounting), mirrored in the Go bridge `ConfigSnapshot`.
- **Unsubscribe secret** — the deployment-wide 32-byte HMAC secret (§ Token format), stored **nest-held plaintext** in `mail_list_unsubscribe_secrets` and auto-seeded on first DB open — the same model + lifecycle as the SRS secret (`mail_srs_secrets`), since the goal doc puts it "in nest state ... server-managed" and the MTA never needs it (the nest derives + verifies tokens; the unsubscribe handlers resolve a token by the cached index). There is **no** wrapped blob and **no** secret pointer projected to the bridge — an earlier design sketch's (tracked internally) DKIM-style wrapped-blob framing was a drift, corrected here to the SRS model.

**List + member CRUD + the RFC 8058 HTTPS endpoint landed 2026-06-13** ( tracked internally, items #10a + #4):

- **List/member CRUD RPCs** (§ Wire shapes) — nine User-class kinds in `bins/fauna-nest/src/bridge_list_handlers.rs` over the DB layer in `db/mail_lists.rs`: `list_account_lists`, `create_account_list` (inserts the `kind='list'` alias + `mail_lists` row in one tx), `update_account_list`, `delete_account_list` (cascades alias → list → members), `list_list_members`, `add_list_member` (validates against `local_domains`, derives the one-click token), `batch_import_list_members`, `unsubscribe_list_member` (manual), `resubscribe_list_member`. All per-user / owner-scoped; the member sub-surface verifies ownership first. `kind='list'` rows are excluded from the personal-alias surface (`list_aliases_for_actor`).
- **RFC 8058 one-click HTTPS endpoint** (§ The HTTPS endpoint) — the unauthenticated `GET/POST /list/unsubscribe?t=<token>` route on the nest's public axum router (`build_router`): GET renders a read-only confirm page, POST (with the `List-Unsubscribe=One-Click` body) flips `unsubscribed_at` by the cached token index. Full-route tier_3 conformance in `tests/conformance_list_unsubscribe.rs`. No `ui.yaml` id (nest-rendered external page).
- **Secret rotation** (§ Secret rotation) — the Admin `fauna.bridges.rotate_list_unsubscribe_secret` RPC + `CacheDb::rotate_list_unsubscribe_secret`: one atomic transaction that mints a fresh 32-byte secret (pruned to a single row — no overlap window), then re-derives every member's cached token under it. In-flight tokens from already-sent mail are invalidated (404 afterward); the lone admin-class list RPC.

**Mailto one-click unsubscribe + reserved-local-part split landed 2026-06-14** ( tracked internally, item #5):

- **Mailto handler** (§ The mailto handler) — the nest's `resolve_recipient` (`bridge_routing_handlers.rs::match_unsubscribe_local_part`) intercepts `unsubscribe+<token>@` **ahead of** the alias resolver + role-address classification, flips the member by the cached token index (`db.unsubscribe_member_by_token`, fire-and-forget + idempotent), and returns the new `ResolveRecipientReply::Discard` outcome. The Go MTA (`internal/mta/server.go`) accepts the RCPT `250` on `Discard`, marks the envelope discard-only, and drops the body at DATA **before** the parse/auth/scan pipeline (so a sender's SPF/DMARC posture can't turn the spec-mandated `250` into a 5xx). The `+<token>` suffix is preserved case-sensitively end-to-end (`splitRcptAddress` does not fold the local-part). Bare `unsubscribe@` with no token → `550 5.1.1`.
- **Reserved-creation/role-routing split** (§ Reserved local-part) — `unsubscribe` is **not** added to `DEFAULT_RESERVED_LOCAL_PARTS` (that set feeds `classify_role_address`, which would mis-route `unsubscribe+<token>@` to the admin mailbox). Instead a separate uncircumventable creation predicate (`fauna_mail::aliases::is_creation_reserved_local_part` + `CREATION_RESERVED_LOCAL_PART_FAMILIES`) refuses `unsubscribe@` / `unsubscribe-*@` at alias **and** list create time (enforced inside `validate_exact_local_part` / `validate_wildcard_prefix`, independent of the admin-tunable reserved list). `smtp-server.md` § abuse@ / postmaster@ role-address routing carries the cross-link.

**The list-SEND pipeline landed 2026-06-14** ( tracked internally, items #7/#8 + #10b + #6, **Option C**):

- **Rate accounting + daily reset** (#7/#8, § Per-list rate accounting) — `CacheDb::try_consume_list_quota` reserves the three caps atomically (per-send hard `552`, per-account-per-day + per-deployment-per-day tempfail `452`) and increments the per-list meters (`sends_today`/`recipients_today`, lazily zeroed on the first send of a new UTC day via a new `mail_lists.counters_day` column — no cron) plus the new `mail_list_account_daily_counter` / `mail_list_deployment_daily_counter` tables (self-resetting by epoch-day key). The admin-tunable ceilings come from `MassMailingPolicyOverrides::effective()` (the `mail_mass_mailing_policy` overrides table; the admin write RPC + UI land with the flat `admin-mail` page — until then the effective policy is the catalog default). **Deferred:** the Tier-3 per-account *user* knob (`mail.account.list_recipients_per_day`, default 20000) has no per-account storage yet, so the effective per-account cap is the admin ceiling (`list_recipients_per_account_per_day_ceiling`, 50000) until the user-knob surface lands.
- **`send_list_message` + `list_list_send_history`** (#10b, § Composing) — the canonical User-class fan-out: validate ownership → reserve the rate caps → enqueue one outbound per subscribed member (`submit_outbound`, `recipients = [member]`) with that member's `List-*` headers stamped into the body (`fauna_mail::lists::stamp_list_headers_on_message` strips any client `List-*`/`Precedence` and prepends the nest's), left **unsigned** at rest (the Option C seam — the nest signs each copy at the outbound hand-out like every other row; `mail-bridge-lifecycle.md` § DKIM provisioning (automatic)). The send is recorded in the new `mail_list_sends` table; `list_list_send_history` reads it. `delivered_count` is the count the nest queued (per-recipient MX-delivery tracking is a later track — queued == delivered for this audit today).
- **The raw-SMTP list-submission reject** (#6) — `enqueue_outbound_mail` rejects a submission whose MAIL FROM is a `kind='list'` address (`fauna.bridges.list_submission_requires_send_rpc`); `send_list_message` is the only list path (§ How the per-list cap separates).

**The tier_3 e2e suite landed 2026-06-14** ( tracked internally, item #12 — **the track is now complete**):

- **`tests/e2e-unified/tests/api/test_mail_lists_send.py`** (9 tests) drives the User-class list RPCs + the Admin `rotate_list_unsubscribe_secret` over the real WS socket against a real `fauna-nest`, with a real `fauna-mail-bridge` MTA fanning the send out to an in-process stub external MX. It proves end-to-end: list creation + the `kind='list'` alias excluded from the personal-alias surface; the **Option C hand-out DKIM signing** (an independent dkimpy verify confirms the delivered list mail's signature is valid, covers the RFC 8058 `List-*` `h=` set, and is From-aligned — the only test exercising the full nest-stamps → nest-signs-at-the-hand-out chain); both one-click unsubscribe channels (HTTPS `POST /list/unsubscribe` and the SMTP `unsubscribe+<token>@` mailto Discard path), each with a token read off a *delivered* `List-Unsubscribe` header; unsubscribed-member exclusion + resubscribe; token re-derivation across a secret rotation; and the per-send (`552`) + per-account-per-day (`452`) rate caps. The per-day-cap test uses one test-only hook (`mass_mailing_test_hook` → `POST /api/v1/test/mass-mailing/policy`) to lower the deployment ceiling.
- **Deferred tier_3 coverage** (not blocking — the code is complete + the user-facing pipeline is tier_3-proven): `test_list_admin_sees_count_not_members` lands with the flat **`admin-mail`** track that owns the (not-yet-existent) cross-user admin list-view RPC. The "list send doesn't consume the per-actor *submission* rate" invariant (the list path goes through `try_consume_list_quota` + `submit_outbound`, touching neither `bridge_submission_quota` nor the submission-token counter) is witnessed by `test_list_per_actor_rate_cap_not_consumed_by_list_send` (2026-09-25), which reads the counter at rest around a list send made with the owner's allowance already spent.
- **Where the raw-SMTP list refusal fires today (2026-09-28).** A mail app's `MAIL FROM` a list address is refused `550 5.7.1 Sender not authorized` by the bridge's MAIL FROM ownership check (`internal/mta/submission.go` `assertMailFromOwned` → `resolve_recipient`, which refuses a list address — next bullet), before any enqueue. Until 2026-09-28 the resolver never matched a `kind='list'` row, so a submitter that was also the domain's catch-all actor passed the ownership check and reached the nest-side `list_submission_requires_send_rpc` at `enqueue_outbound_mail` (§ Architectural rules), which the bridge maps, like every enqueue error, to a generic `451 4.5.0` tempfail; the list step closes that catch-all trigger, while the DATA-stage mapping of permanent enqueue refusals to a `5xx` stays open.
- **Inbound to a list address + the `List-Help` page (2026-09-28).** The shared matcher `fauna_mail::aliases::resolve_recipient` carries a list step on the exact key (after exact + forwarder, ahead of +suffix, disposable, wildcard, role-address and catch-all; `mail-aliases.md` § Resolution order step 2): the nest handler looks the address up with `lookup_list_id_for_address` and the matcher answers `Reject { 550, LIST_SUBMISSIONS_REFUSED }`, which the bridge's `rejectFromResolver` renders as the § Pattern line `550 5.1.1 List submissions not accepted at this address`. Before it, list-address mail fell through to the generic `550 5.1.1 User unknown`, or was delivered to the domain's catch-all actor when one was set; it never reached the list owner (the exact lookups filter `kind = 'exact'`). The default `List-Help` target `GET /list/<list-id>/help` (§ RFC 2369 list headers) is a static, unauthenticated nest page (`lib.rs::list_help_get`, same self-contained HTML shell as the unsubscribe pages, no `ui.yaml` id) that neither looks up nor echoes the id. Witnessed by `test_mail_lists_send.py::test_mail_to_a_list_address_is_refused` + `::test_list_help_link_opens_a_subscribe_unsubscribe_page` (the latter fetches the path from a delivered issue's `List-Help` header), with unit/route coverage in `aliases::tests::resolve_list_address_refuses_ahead_of_wildcard_and_catch_all`, `bridge_routing_handlers::tests::resolve_recipient_list_address_refuses_ahead_of_catch_all` and `tests/conformance_list_unsubscribe.rs`.

The **whole mass-mailing backend + its tier_3 e2e are landed.** No remaining backend work.

The `mail-lists` + `mail-list-members` pages and their component IDs were ratified into `ui.yaml` (2026-06-01) ahead of the backend, exactly as the `mail-settings` family was — the page element scope is the contract. The deployment-wide unsubscribe-secret rotate control and the per-list `recipients_per_send` admin ceiling are admin-policy concerns deferred to the flat `admin-mail` page (IDs allocated with that consuming track, not pre-invented here).

**The client seam went live 2026-07-29 — and had been a stub for six weeks.** The shared `MailListsMachine` / `MailListMembersMachine` (`libs/fauna-client-mail-settings/src/lists.rs`) were written UI-precedes-backend, with `rpc_glue`'s two seams returning an honest `unimplemented` rejection until the RPCs existed. The RPCs landed 2026-06-13/14 (above) and **nothing rewired the seams**, so linux, web, windows, apple and android each rendered both pages while every action on them failed. Nothing about it looked broken: the pages painted, the machines' unit tests passed, and `tests/e2e-unified/tests/test_mail_lists.py` was skipped with the reason "backend unbuilt … no nest handler" — a claim that had been false since 2026-06-13. Both seams (native + wasm) now call the real RPCs through `MailAccountClient`'s nine list methods (`libs/fauna-client-bridges`), so all seven apps inherit a working page from one commit.

Two shape notes the apps share, both in `lists.rs` so no app re-derives them:
- **The wire→view projections are shared** — `project_list_row` (address composition, the friendly-name→local-part fallback, `Option<String>`→`String` flattening) and `project_member_row` (the subscription status is **derived** from `unsubscribed_at`; the wire carries no status field).
- **The add-sheet's domain picker is sourced user-tier** (`derive_list_domains`), from the caller's own list + alias rows — never `fauna.bridges.list_local_domains`, which is Admin-class and which a plain user therefore cannot call. Lists are user-tier per § Architectural rules.

**Per-app render status.** **linux** (lead) and **web** built both pages first, then **windows**, **apple** and **android**; **tui** landed 2026-07-29 (`apps/fauna-tui/src/settings/{mail_lists,mail_list_members}.rs`, direct Rust, no FFI hop) — all seven apps now render them. **tui + linux are e2e-proven against the live backend** (`test_mail_lists.py`, then 7 tests — 8 since the nothing-selected fallback test landed 2026-08-05 — `--app tui,linux`, both green as of 2026-07-30). **windows joined them 2026-08-16** — the full module, `8 passed in 63.19s --app windows`. That run also settles a reported windows-only failure of `test_mail_list_members_batch_import_lands_every_valid_address` (`nest unreachable: rpc disconnected`, docs-consistency sweep, 2026-08-15): it does **not** reproduce, neither alone nor in module order, so it is read as a load/connection-layer red rather than a product or harness defect — no skip or xfail was added, because a green test needs no marker and a marker would have hidden the real state. Anyone meeting it again should treat an `rpc disconnected` as infrastructure until proven otherwise (`e2e-latency-independent-assertions.md` § point 14) rather than re-opening it as a mail-lists bug. **android carries the suite's marker as of 2026-07-30 too, but as a code review, not a live run** — its Screen/VM pair was read against ui.yaml's element spec and each of linux's three found bugs (members-button wiring, domain-picker staleness, list-item-name shape) and matches the correct shape on all three already (its `NavHost` destinations rebuild the ViewModel — and re-`hydrate()` — on every visit, so the staleness bug is structurally absent), but `--client android` stays host-emulator-gated fleet-wide, so this is unverified at runtime like every other android e2e surface. **windows landed 2026-08-03: 6/7 tests green `--app windows`** (`test_mail_lists.py` carries `pytest.mark.windows`). Two of linux's original bug classes matched exactly: `mail-lists-list-item-members-button` was hard-`IsEnabled="False"` with no click handler (an all-zero placeholder scoped `MailListMembersPanel` regardless — the identical shape linux/apple hit), and `mail-lists-list-item-name` rendered friendly-name-only, not "friendly name — address". Fixed: `MailListsPanel.Members_Click` sets a new `MailListMembersPanel.PendingListIdHex` static handle (consumed once, cleared) then routes via `MainPage.NavigateToSettingsSubPage` — windows has no single-page stack-switcher the way linux/tui do, so the settings shell's per-sub-page `Frame.Navigate` has no per-visit parameter slot beyond the shared `ServiceClients`, making a small side-channel the minimal-diff shape (mirrors the `BackupsPage.Current`/`ConversationsPage.Current` static-handle idiom windows already uses for other TestAgent/cross-page bridges); `MailListRow.NameLine` combines friendly name + address. **windows adopted the "nothing selected → falls back to the user's first list" fallback 2026-08-05**, tui's shape (a direct `SubMailListMembers` rail click used to always resolve to the placeholder id). Unlike tui, windows has no already-hydrated `MailListsMachine` any settings panel can peek at — each panel builds its own machine independently — so `MailListMembersPanel.EnsureVmAsync` does its own "list my lists, take the first" read (a throwaway `MailListsViewModel` over `BuildMailListsMachineAsync`) before constructing the scoped members machine when `PendingListIdHex` is unset; the placeholder id is now reserved for the genuinely-zero-lists case, matching tui's honest empty state. **The 7th test (`test_mail_list_members_batch_import_lands_every_valid_address`) was NOT fixed then** (superseded 2026-08-16: the full module is green on windows and the failure does not reproduce, above) — it hits `HTTP error: nest unreachable: rpc disconnected (was_in_flight=false)` mid-call (seen on 2 of 3 runs; the 3rd instead silently returned zero rows with no error), while the structurally-identical single-member `AddMemberAsync` path passes reliably. Pre-existing (reproduced before any of this session's fixes touched this area) and not isolated to a specific code path this session touched — tracked as a separate connection-layer bug, not a routing/render gap. **apple landed 2026-08-02: e2e-proven on BOTH macOS and iOS, 7/7 green on each.** Five real bugs, all in shared FaunaKit (`MailListsView`/`MailListMembersView` — one fix serves both apple targets): two matched linux's original bug classes (the Members button was still genuinely inert, wired to the placeholder id everywhere; the row name rendered friendly-name-only, not "friendly name + address"); the third explained why the page looked entirely broken rather than just buggy — both views used only `.accessibilityIdentifier`, never the `automation*` modifiers apple's in-process e2e driver actually reads (`apple-e2e-automation.md` § Why a registry, not accessibility — the identical gap class the nest-provisioning page had, 2026-07-18), so no element on either page was ever visible to the driver despite rendering correctly for a real user; a fourth, smaller gap surfaced once the suite could finally run: the delete button had no two-click confirm step, unlike every sibling app, fixed by porting `MailAliasesView`'s arm-then-confirm pattern; a fifth, iOS-only, was the classic rule-6 delete-zombie (`apple-e2e-automation.md` rule 6) — `MailListsView`'s list lived in a `Form`, iOS's lazy-`List` pooling of removed rows, fixed by converting to an eager `ScrollView { VStack }` mirroring `MailAliasesView`'s own precedent for the same bug. `test_mail_lists.py` carries `pytest.mark.macos`/`pytest.mark.ios`. **linux's three gaps found + fixed by widening the suite (2026-07-30):** (1) `mail-lists-list-item-members-button` carried no click handler ("inert in the embedded settings seed") and the members page was wired to an all-zero `PLACEHOLDER_LIST_ID_HEX` — fixed via an `on_navigate_to_members` callback (mirrors the Personalization page's `on_navigate_to_muted_words` pattern) that calls the members page's own `select(list_id_hex, friendly_name)` closure (rebuilds its machine, re-hydrates) then switches the settings stack, mirroring tui's `Action::MailListsOpenMembers` shape described below; (2) the add-sheet's domain picker was never refreshed on becoming visible (only hydrated once at settings-shell build time), so a domain seeded after login stayed invisible and `create_account_list` was called with an empty `local_domain` → `fauna.protocol.malformed` — fixed by wiring `mail-lists` into the settings shell's on-visible refresh hook, mirroring `subscriptions`/`general`/`linked-nests`/`account`; (3) `mail-lists-list-item-name` rendered only the friendly name, diverging from ui.yaml's documented "friendly name + send-from address" shape — fixed to match tui's `format!("{} — {}", friendly_name, address)` exactly. tui *set* the members-page routing shape rather than linux porting a pre-existing one: the row's button scopes the page to that `list_id`, and because `settings.md` § Navigation model also gives `mail-list-members` its own rail slot, an entry with nothing selected falls back to the user's first list (and paints an honest empty state when there are none) rather than hydrating against a placeholder id. **apple adopted the "nothing selected → falls back to first owned list" fallback 2026-08-28**, windows' shape (apple likewise has no already-hydrated `MailListsMachine` any settings page can peek at — each FaunaKit view builds its own machine independently): `MailListMembersVM.configure` does its own "list my lists, take the first" read via a throwaway `MailListsMachine` when the incoming `listIdHex` is the placeholder, before vending the scoped members machine; the placeholder id is reserved for the genuinely-zero-lists case. One fix in the shared FaunaKit VM serves both apple targets — `test_mail_list_members_nothing_selected_falls_back_to_first_list` 8/8 green on both `--app ios` and `--app macos`. **web and linux adopted the same fallback 2026-08-28**, closing the last gap — all seven apps now implement it. **web** (`MailListsSection.svelte`) already has an already-hydrated `MailListsMachine`/snapshot for the Lists view — tui's shape, not windows'/apple's throwaway-machine one — so its `resolveMembersFallback` just peeks at that snapshot's first list; it runs on SvelteKit's `afterNavigate` (the SPA's established reliable per-navigation-event signal — `WebSettingsSection.svelte`'s precedent, since `$page.params`/`$derived`/`$effect` do not change on a same-route re-navigation) and again once the snapshot itself loads (a nav can land before `machine.hydrate()` resolves). **linux**, like windows/apple, has no already-hydrated lists machine any settings page can peek at (`settings_shell` builds each page independently), so `mail_list_members::resolve_fallback` builds a throwaway `MailListsMachine`, wired to the settings stack's visible-child-changed signal (`settings_shell.rs`) — a no-op once a row's Members click has already set `has_list`, so the two paths (direct rail visit vs. row click) never race. `test_mail_list_members_nothing_selected_falls_back_to_first_list` green on both `--app web,linux` (1307s, real nest).

**Shared `member_status_label` formatter (2026-06-23).** The `mail-list-members-list-item-status` label (Subscribed / Unsubscribed) is rendered through the canonical `fauna_client_mail_settings::member_status_label(MemberStatus) -> LocalizedText` — one source of truth for the `MemberStatus`→i18n-key map (`mail_lists.status_{subscribed,unsubscribed}`), mirroring `fauna_client_search::render::content_type_badge`. It lifts the identical two-arm map that linux/windows/apple/android/web each hard-coded (priority #1/#2). **linux** consumes it directly (`.resolve(crate::i18n::strings::lookup)`). For the native apps it is `#[uniffi::export]`ed (added 2026-06-23 — `LocalizedText` is already a shared `fauna_core` `uniffi::Record`; `fauna-client-mail-settings` is gated out of the Go mail-bridge `--no-default-features` build, so no `libs/fauna-mail-go` regen). **android** consumes it (2026-06-23): `MailListMembersScreen` resolves `uniffi.fauna_client_mail_settings.memberStatusLabel(status)` through the shared `resolveLocalized` helper, injected into the Robolectric-tested `MailListMembersContent` as a `statusLabel` lambda so the FFI call stays in the stateful Screen. windows (`MailListMembersViewModel.cs`), apple (`MailListMembersView.swift`) and web (`MailListsSection.svelte`) have since adopted it too (each resolves the returned key through its own i18n runtime). Verified by `fauna_client_mail_settings::lists::tests::member_status_label_maps_{subscribed,unsubscribed}` + (android) Robolectric `MailListMembersContentTest::statusLabelComesFromTheSharedResolver` (the shared-mapping unit tests are the canonical proof on the five apps `test_mail_lists.py` doesn't cover yet).

**The import tally renders counts, not per-line reasons (2026-10-05).** `mail-list-members-import-result` (§ `mail-list-members` page) shows the shared `mail_lists.import_result` string — added / already subscribed / invalid — on tui, the lead app, and on macOS and iOS (one FaunaKit `MailListMembersView`, 2026-10-08), witnessed by `test_mail_lists_controls.py::test_import_tally_says_how_many_were_skipped`; the other four render nothing yet (web keeps `last_import` and never paints it). **Gap:** the spec's "each skipped line's reason" is unbuilt everywhere, because `BatchImportListMembersReply` carries only the three counts — the aliases twin's per-line `outcomes` shape is the model, and it needs an additive wire field first. Blank lines are dropped client-side before the call and are never counted.

**Sending to a list from the compose form landed on tui first (2026-10-08).** The list is addressed by typing its address into the conversations recipient picker; the shared SMTP rail (`fauna_conversations::backends::smtp`) sends a compose whose one mail recipient is one of the account's own lists through `OutboundMailSink::submit_to_list` → `fauna.bridges.send_list_message`, never as plain mail to the list address. The three compose-form texts are derived once in `fauna_conversations::list_send` onto `ComposeState::list_send` (refreshed by `ConversationsManager::refresh_list_send` when a chip commits, a thread opens and after every send), from `list_account_lists`' additive per-account meter (`account_recipients_today` / `account_recipients_per_day`) and the newest `list_list_send_history` row. The nest files ONE Sent copy of the client-composed message per send (best-effort, as `fauna.email.send` does), so the send is one entry in sent mail on every device. The native sink is `fauna_client_conversations::NestOutboundMailSink`; tui paints the three ids. **Gaps:** the other six apps paint nothing yet (linux, windows, apple and android share the sink and need only the render; web registers no SMTP sink at all); the per-account cap is the admin ceiling until the Tier-3 user knob lands (§ Implementation status, *Deferred*).


**The off-server List-Archive confirm landed on tui first (2026-10-08).** § Don't do these's "warn them once" is the shared predicate `archive_url_needs_confirm` (`lists.rs`) plus tui's armed submit (`apps/fauna-tui/src/settings/mail_lists.rs`), witnessed by `test_mail_lists_controls.py::test_an_off_server_archive_link_asks_once_before_saving`. macOS and iOS arm the same way (`MailListsView`'s add/edit sheet, 2026-10-08). **Gap:** the other four apps save on the first press.
---

## Goal

A user runs a newsletter or mailing list from their Fauna nest — 100 to 5000 recipients per issue, RFC 8058 + RFC 2369 conformant, with one-click unsubscribe that respects the recipient's mailto and https preferences. The deployment makes the legitimate-list use case **structurally first-class**:

1. **List is an alias kind (`kind='list'`).** One of the kinds in `mail-aliases.md` § Alias kinds (the taxonomy owner — 7 kinds today), sibling to exact / +suffix / wildcard / disposable / catch-all / forwarder. A list lives as a row in `account_aliases` with `kind = list`; the pattern is the list's local-part (`newsletter@<our-domain>`); the actor that owns the list is the user who created it.
2. **One-click unsubscribe.** Every outbound message stamps `List-Unsubscribe: <mailto:unsubscribe+<token>@<our-domain>>, <https://<primary-domain>/list/unsubscribe?t=<token>>` + `List-Unsubscribe-Post: List-Unsubscribe=One-Click` headers per RFC 8058. The recipient's MUA renders the unsubscribe affordance; clicking it fires the HTTPS POST (or mailto SMTP) and the recipient is unsubscribed without any further interaction.
3. **List headers.** RFC 2369's `List-Id`, `List-Help`, `List-Archive` headers stamped on every outbound; gives modern MUAs the metadata to render the conversation as a list-thread rather than a normal one-to-one mail.
4. **Per-list rate accounting separate from per-actor.** A user with 5 lists doesn't have their per-actor submission rate (1000/day) consumed by sending one newsletter to 5000 recipients; list submission is its own bucket with its own cap (`mail.outbound.list_recipients_per_send`, default 5000).
5. **`mail-lists` page UX.** A sub-page of `mail-settings`, same shape as `mail-aliases` (a list is an alias kind) — list of lists, add / edit / delete buttons, and a per-list `mail-list-members` page with member management.

**Bar: parity with what Buttondown / Mailchimp / Sendgrid / Substack ship for "legitimate-list sender" but self-hosted within the deployment + RFC-8058 compliant + integrated with the per-actor alias surface.** The Fauna-specific shape: a list is per-user, not per-deployment; the user controls their list members (admin can see counts but not edit); the deployment-wide ceilings are admin-tier safety valves against abuse.

**Default-on.** RFC 8058 + RFC 2369 stamping is mandatory: the nest stamps the One-Click + List-Id headers per recipient in the `send_list_message` fan-out, and a raw-SMTP submission from a list address is rejected (the only list path is the fan-out, § How the per-list cap separates). The per-list rate cap is 5000 recipients/send (admin-tunable; user can lower per-list). The per-deployment ceiling is 50000 recipients/account/day (admin-tier; protects the deployment's IP reputation).

---

## List as an alias kind

### Pattern

A list address looks like any other alias: a local-part on one of the deployment's `local_domains`. Conventionally `newsletter@<our-domain>`, `weekly@<our-domain>`, `<list-name>@<our-domain>`. The local-part is **structurally identical** to other alias kinds — same character class restrictions, same length limits, same uniqueness per (local_domain, pattern, kind).

The kind discriminator is `kind = 'list'` in the `account_aliases` row. The resolver at RCPT TO time (per `mail-aliases.md` § Resolution order) treats lists as a separate kind:

- A **list address as a RECIPIENT** (peer sends mail to `newsletter@<our-domain>`) is **not** the typical case — lists are outbound. The default behavior is to reject inbound mail to a list address with `550 5.1.1 List submissions not accepted at this address`. (A future "mailing-list-receive" mode where peer subscribers can reply to the list and have their reply broadcast to all subscribers is out of scope; the present doc is one-way outbound.)
- A **list address as MAIL FROM over raw SMTP** is **rejected** (`fauna.bridges.list_submission_requires_send_rpc`); the only list-send path is the `fauna.bridges.send_list_message` RPC, where the nest fans out one-per-member, stamps each recipient's List-\* headers, applies the per-list rate caps, and enqueues each row unsigned for the nest's hand-out DKIM signing (§ How the per-list cap separates). List mail is never produced by raw submission.

### Tier

User-tier — same as other personal alias kinds. A user owns their lists. The admin sees counts on the admin-pane but doesn't edit them; admin-tier list-creation is the admin acting in their user capacity (the admin has a user account; they create lists on their account, not "as admin").

### Cross-link to mail-aliases.md

The sibling kinds (exact, +suffix, wildcard, catch-all, disposable, forwarder) per `mail-aliases.md` § Alias kinds are unchanged. The list row in the kind-taxonomy table:

| Kind | Pattern | Tier | Default state |
|---|---|---|---|
| **List** | `<list-local-part>@<our-domain>` — a user's chosen address for the list | user (Tier 3) | none created by default; user creates on demand |

The shape parallels disposable (user mints on demand) but differs in lifecycle (a list is long-lived; a disposable is throwaway).

---

## Storage

The shipped SQLite DDL is `MIGRATIONS_MAIL_LISTS` (`bins/fauna-nest/src/db/migrations.rs` — the authoritative shape; ids are BLOB UUIDs, timestamps INTEGER epoch-millis per codebase convention):

```
mail_lists(
    list_id            BLOB PRIMARY KEY,
    alias_id           BLOB NOT NULL REFERENCES account_aliases(alias_id) ON DELETE CASCADE,
    owner_actor_id     BLOB NOT NULL,
    list_friendly_name TEXT,             -- shown in List-Id header; e.g., "Bob's Weekly Newsletter"
    description        TEXT,             -- user-facing list description
    list_help_url      TEXT,             -- optional; populates List-Help header
    list_archive_url   TEXT,             -- optional; populates List-Archive header
    recipients_per_send INTEGER,         -- per-list user override, ≤ the admin ceiling (§ The per-send cap)
    created_at         INTEGER NOT NULL,
    last_send_at       INTEGER,
    member_count       INTEGER NOT NULL DEFAULT 0,  -- cached count of subscribed member rows
    sends_today        INTEGER NOT NULL DEFAULT 0,  -- running meter; lazily zeroed on the first send of a new UTC day
    recipients_today   INTEGER NOT NULL DEFAULT 0,  -- per-day recipient meter (same lazy rollover)
    counters_day       INTEGER NOT NULL DEFAULT 0   -- epoch-day bucket the meters were last reset under (no cron)
)

mail_list_members(
    member_id         BLOB PRIMARY KEY,
    list_id           BLOB NOT NULL REFERENCES mail_lists(list_id) ON DELETE CASCADE,
    recipient_address TEXT NOT NULL,     -- the external address (e.g., alice@example.com)
    subscribed_at     INTEGER NOT NULL,
    unsubscribed_at   INTEGER,           -- nullable; null = subscribed, non-null = unsubscribed
    one_click_unsubscribe_token TEXT,    -- HMAC-derived per (list_id, recipient_address) — see § One-click unsubscribe
    UNIQUE (list_id, recipient_address)
)
```

Two sibling self-resetting counter tables back the per-day caps (§ Per-list rate accounting): `mail_list_account_daily_counter(actor_id, day, recipients_sent)` and `mail_list_deployment_daily_counter(day, recipients_sent)` — keyed on the epoch-day bucket, so a new UTC day is a fresh key with an implicit zero (no sweep needed). Send history lives in `mail_list_sends` (one row per `send_list_message` fan-out; read by `list_list_send_history`).

Indexes:

- `mail_lists.alias_id` for the alias-to-list lookup (the resolver checks if a RCPT TO matches a `mail_lists` row via the `account_aliases` row).
- `mail_lists.owner_actor_id` for the per-actor enumeration.
- `mail_list_members(list_id, unsubscribed_at)` for the subscribed-vs-unsubscribed count + the per-list-member enumeration.
- `mail_list_members.one_click_unsubscribe_token` for the unsubscribe-handler lookup (non-unique by design — a UNIQUE would turn a 192-bit HMAC collision into an insert failure for no benefit).
- `mail_list_sends(list_id, sent_at)` for the send-history read.

### The list as an alias row

When a user creates a list, the create flow:

1. Inserts a row in `account_aliases` with `kind = 'list'`, `pattern = <list-local-part>`, `local_domain = <chosen-domain>`, `actor_id = <user-actor-id>` (the column is `account_aliases.actor_id` — `mail_lists.owner_actor_id`, below, is a separate copy on the sibling table).
2. Inserts a corresponding row in `mail_lists` referencing the new alias row + the list's friendly-name + URL fields.

When the user deletes a list, the cascade fires on `mail_lists.alias_id` (the alias deletion cascades to the list row, which cascades to all member rows). The user's UI shows the "Delete list" affordance with the confirm dialog spelling out the cascading deletion.

### Per-list rate accounting

The `sends_today` and `recipients_today` columns on `mail_lists` are running meters, **lazily zeroed on the first send of a new UTC day** via the `counters_day` epoch-day bucket (`try_consume_list_quota`; the same lazy-rollover model as the warmup counters — no cron task). The per-account and per-deployment day caps ride the two self-resetting epoch-day-keyed counter tables above.

Per-actor rate accounting (per `smtp-server.md` § Submission policy's `mail.submission.per_actor_rcpt_per_day`) is **not** consumed by list submissions — list mail counts against the per-list cap instead. The per-actor cap remains for non-list submission (regular one-to-one mail).

---

## RFC 2369 list headers

Every outbound message from a list (i.e., MAIL FROM matches a `mail_lists` row) is stamped with:

| Header | Value | RFC |
|---|---|---|
| `List-Id` | `<list-friendly-name-or-list-id-uuid> <list-pattern@local-domain>` | RFC 2919 (List-Id is technically RFC 2919; ratified by RFC 2369 for use alongside the other List-* headers) |
| `List-Help` | `<mail_lists.list_help_url>` if set; else `<https://<primary-domain>/list/<list-id>/help>` (a static help page on the deployment's HTTPS endpoint explaining how to subscribe / unsubscribe) | RFC 2369 |
| `List-Archive` | `<mail_lists.list_archive_url>` if set; else **header omitted** (don't fabricate an archive URL — a list with no archive is structurally legitimate) | RFC 2369 |
| `List-Unsubscribe` | (RFC 8058 — see below) | RFC 2369 + RFC 8058 |
| `List-Unsubscribe-Post` | `List-Unsubscribe=One-Click` (RFC 8058 — see below) | RFC 8058 |
| `Precedence` | `bulk` | RFC 3834 §3.1.7 (legacy header — modern MUAs use List-Id instead, but Precedence: bulk remains the de-facto signal for "this is automated mail; don't auto-reply") |

The headers are stamped **per recipient by the nest** in the `send_list_message` fan-out (§ Composing a list message) — one outbound message per subscribed member, each carrying that member's one-click token — into the **unsigned** body; the nest then **DKIM-signs each message at the outbound hand-out** (the RFC 2369/8058 List-\* are in the signed `h=` set, so RFC 8058 §3's "signature-covered" requirement holds — the signer over-signs the full List-\* set even on non-list mail). The list owner's composed message **must not** stamp these headers itself — if it does, the nest strips any client `List-*` / `Precedence` and prepends its own (the nest's stamp is authoritative). There is **no** submission-time MTA stamping: a `kind='list'` address cannot be submitted over raw SMTP at all (§ How the per-list cap separates).

---

## RFC 8058 one-click unsubscribe

### The headers

```
List-Unsubscribe: <mailto:unsubscribe+<token>@<our-domain>>, <https://<primary-domain>/list/unsubscribe?t=<token>>
List-Unsubscribe-Post: List-Unsubscribe=One-Click
```

`<token>` is an HMAC-derived opaque value (see § Token format below); `<our-domain>` is the domain the list was created under; `<primary-domain>` is the deployment's primary domain (one HTTPS endpoint regardless of how many local domains the deployment has). If the deployment later renames its primary domain per `mail-primary-domain-rename.md`, the `/list/unsubscribe` endpoint stays bound on the **former** primary hostname permanently as long as that domain remains in `local_domains` — so unsubscribe clicks from already-sent mass-mail (which carry the pre-rename URL in their headers) keep working indefinitely. Tokens are hostname-independent (the HMAC is over the (list_id, recipient_address) pair); the post-rename endpoint at the old hostname dispatches to the same handler.

Both URIs (mailto + https) are mandatory per RFC 8058 §3 — RFC-conformant MUAs prefer the https form (the click is fully automated; the user doesn't need to interact with their mail client to unsubscribe).

### Token format

The token is an opaque, single-use-friendly, HMAC-signed value:

```
token = base64url(
  HMAC_SHA256(
    deployment_secret,
    concat(list_id, recipient_address)
  )[:24-bytes]
)
```

- `deployment_secret` is a 32-byte secret in nest state (one secret deployment-wide, not per-list; rotated via the admin-pane action on the flat `admin-mail` page — `admin-mail-list-unsubscribe-secret-rotate-button`, a PROPOSED id since 2026-10-01 that waits on the user's rule-A sign-off; the nest kind is built and no app has the button — § Secret rotation).
- `list_id` + `recipient_address` are the unique identifier of the subscription (one token per subscription).
- Truncated to 24 bytes (192 bits — collision-resistant up to ~2^96 expected birthdays; sufficient for the threat model where an attacker would need to enumerate a deployment's subscriber list).
- base64url-encoded for URL-safe transport.

The token is **deterministic** (same list_id + recipient_address → same token; computed on-demand, not stored — though `mail_list_members.one_click_unsubscribe_token` caches the value for fast lookup). Same token across re-subscriptions; an attacker who learned the token from a leaked headers dump could re-unsubscribe the recipient indefinitely, which is **not a privilege escalation** (the worst the attacker can do is unsubscribe the recipient; they can't subscribe someone new). The trade-off: deterministic token is much simpler than per-send rotation, and unsubscription is a recipient-friendly action by design.

### Secret rotation

When the admin rotates the secret (action button + confirm dialog "This invalidates all existing unsubscribe tokens. Already-unsubscribed members stay unsubscribed; future unsubscribe clicks from in-flight messages fail with 404. Most MUAs cache the previous tokens for the next-send cycle; messages already in flight may have invalidated tokens. Proceed?"):

1. Generate a new 32-byte secret and replace the single `mail_list_unsubscribe_secrets` row (nest-held plaintext, server-managed — the SRS-secret model, § Implementation status today; **not** a policy-catalog knob and **not** a wrapped blob; the value is never exposed in the admin UI — only the rotate button is).
2. Re-compute every `mail_list_members.one_click_unsubscribe_token` under the new secret — **in the same atomic transaction** as the secret swap (`CacheDb::rotate_list_unsubscribe_secret`; no overlap window, no background job).
3. Future outbound mails stamp the new token.
4. In-flight messages (already in recipient inboxes) carry the old token; unsubscribe attempts on those tokens 404 (the recipient sees "Unsubscribe link expired — please use the most recent newsletter's link").

Rotation is rare (security incident only); the operational cost (in-flight token invalidation) is the trade-off.

**The button (designed 2026-10-01; unbuilt on every app).** One id, `admin-mail-list-unsubscribe-secret-rotate-button`, on the flat `admin-mail` page. The confirm is the same-button two-click that page already uses for `admin-mail-health-warmup-reset-button` (`mail-deliverability.md` § Admin-pane Deliverability surface), so no confirm id is scoped: the first press relabels the button with one sentence naming the cost — unsubscribe links in mail already sent stop working — and the second dispatches `fauna.bridges.rotate_list_unsubscribe_secret`; the result shows on the page's existing message line. Its twin, the forwarding secret's button, sits beside it (`mail-forwarding.md` § SRS scheme), and the two ids wait on one sign-off. Until this button exists the rotation is an act the admin is promised and can reach from no app (`../principles.md` § One configuration surface).

### The HTTPS endpoint

```
POST https://<primary-domain>/list/unsubscribe?t=<token>
```

Handler logic:

1. Decode the token (base64url → 24 bytes).
2. Look up `mail_list_members.one_click_unsubscribe_token = <token>`. If no match: return 404 with a plain-language explanation.
3. If `unsubscribed_at IS NOT NULL`: return 200 with "Already unsubscribed." (idempotent on second-click).
4. Set `mail_list_members.unsubscribed_at = NOW()`.
5. Decrement `mail_lists.member_count`.
6. Return 200 with a confirmation page. This page is **nest-served HTML rendered for an external recipient** (no Fauna app, no auth) — it is not an app UI surface and carries no `ui.yaml` id (the mail-UX design pass keeps only client-rendered pages in the unified registry). The nest renders it directly from the `/list/unsubscribe` handler.

**No auth required** — the token IS the auth. Per RFC 8058 §3.3, the One-Click POST happens without the recipient interacting with the deployment's HTTPS frontend; the click in their MUA is the consent.

**GET on the same URL** renders the confirmation page (recipients with MUAs that don't support One-Click POST fall back to opening the link in a browser). The GET handler is **read-only** — clicking the link doesn't unsubscribe; the recipient has to click a "Confirm unsubscribe" button on the rendered page (which fires the POST behind the scenes). That is why an ambiguous-MUA click cannot itself unsubscribe anyone.

### The mailto handler

```
RCPT TO: unsubscribe+<token>@<our-domain>
```

`unsubscribe@` is reserved via the **creation-only predicate** (`fauna_mail::aliases::is_creation_reserved_local_part`), deliberately **not** the role-routing set — see § Reserved local-part below for the two-set split.

When a RCPT TO matches `unsubscribe+<anything>@<our-domain>`, the **nest** handles it — `resolve_recipient` (`bridge_routing_handlers.rs::match_unsubscribe_local_part`) intercepts the local-part **ahead of** the alias resolver and role-address classification:

1. The nest strips the `+<token>` suffix (preserved case-sensitively end-to-end — it is a base64url value).
2. The nest decodes the token and flips `unsubscribed_at` by the cached token index (`db.unsubscribe_member_by_token`) — **fire-and-forget and idempotent**; same flip logic as the HTTPS endpoint.
3. The nest returns the `ResolveRecipientReply::Discard` outcome; the MTA accepts the RCPT with `250` **regardless of token validity** — the success state is recipient-facing (RFC 8058 mailto senders don't process bounces), so an unknown/expired token is still a `250` + no-op, never a 5xx. The only 550 on this local-part is bare tokenless `unsubscribe@` (§ Reserved local-part).
4. **The DATA body is discarded** before the parse/auth/scan pipeline — RFC 8058 §3.1 says the mailto form may carry a body but the server doesn't need to parse it (and dropping pre-pipeline means a sender's SPF/DMARC posture can't turn the spec-mandated `250` into a 5xx).

The mailto form is the legacy fallback for MUAs that don't support RFC 8058 One-Click POST (Apple Mail at one point, older Thunderbird). Modern major MUAs prefer the https form.

---

## Per-list rate accounting

### The per-send cap

Each list submission is capped at `mail.outbound.list_recipients_per_send` (Tier 2, default 5000 — the admin ceiling). A user-tier override (the `mail-lists-add-sheet-per-send-cap-input` field on the `mail-lists` add/edit sheet) can lower the cap per-list (`mail_lists.recipients_per_send`); never raise it above the admin ceiling.

A single send exceeding the cap rejects hard — the `send_list_message` RPC returns the 552-class error (`Too many recipients per list send`) surfaced in the composing client — and the user reduces their recipient list and retries. (List sends never ride raw SMTP, § How the per-list cap separates; the 552/452 codes are the error taxonomy, not wire SMTP replies.) No automatic chunking (auto-chunking would create receivers seeing multiple separate sends with different timestamps; intentional + explicit chunking by the user is the supported pattern).

### The per-day per-account cap

Across all lists owned by one user, the per-day recipient count is capped at `mail.account.list_recipients_per_day` (Tier 3, default 20000 — user-tier knob). The admin ceiling is `mail.outbound.list_recipients_per_account_per_day` (Tier 2, default 50000) — the user's cap can be set anywhere from 0 to the admin ceiling.

Over-cap rejects with the 452-class error (`Per-account list recipient daily limit exceeded (try again tomorrow)`) — a tempfail-class code surfaced in the composing client, which can retry the send the next UTC day. The user's UI shows "Approaching daily limit — N more recipients today" when they're within 10% of the cap.

### The per-day per-deployment cap

The admin ceiling is `mail.outbound.list_recipients_per_deployment_per_day` (Tier 2, default 500000) — a deployment-wide safety valve against a runaway list-mailing pattern (e.g., a compromised account auto-generating lists). Over the cap, **every** list send tempfails until the next day; the deployment-wide warning surfaces on the admin mail-telemetry surface (`mail-observability.md` — target-state).

### How the per-list cap separates from per-actor

The discriminator is the **explicit `fauna.bridges.send_list_message` RPC**, not a MAIL-FROM match. A list send goes through that one RPC, which (after validating ownership) reserves the list counters atomically — `mail_lists.sends_today` / `mail_lists.recipients_today`, the per-account-per-day list counter, and the per-deployment-per-day list counter — and does **not** consume the per-actor submission counter (`mail.submission.per_actor_rcpt_per_day`). Regular one-to-one mail (`fauna.email.send` / authenticated raw-SMTP submission) consumes the per-actor counters and never touches the list counters. (Which counters each of those two doors consumes, and why both consume the *same* daily one: `mail-app-surface.md` § Outbound metering for the RPC, `smtp-server.md` § Architectural rules for raw SMTP. The RPC half of this sentence became true on 2026-08-23 — until then it metered by the caller's own `From:` header.)

A `kind='list'` address submitted over **raw SMTP** (an external MUA that AUTHs and sets MAIL FROM to a list address) is **rejected** at `enqueue_outbound_mail` with `fauna.bridges.list_submission_requires_send_rpc`. Per-recipient RFC 8058 stamping is structurally impossible for a single-body SMTP submission — every recipient needs a distinct `List-Unsubscribe` token, hence a distinct body, hence a distinct DKIM signature — so the only list path is the nest fan-out. That rejection is what makes the per-recipient one-click-unsubscribe contract unbypassable.

---

## `mail-lists` page UX

The `mail-lists` page is a sub-page of `mail-settings` (reached the same way as `mail-aliases` — a list is an alias kind). Same shape as the aliases page per `mail-aliases.md` § Account-detail Aliases section UX. Ratified into `ui.yaml` 2026-06-01.

### Layout

| Element | ui.yaml id |
|---|---|
| Page heading | `page-heading` |
| Per-list list (indexed component) | `mail-lists-list` |
| Per-row item | `mail-lists-list-item` |
| "+ Add list" button | `mail-lists-add-button` |

Each per-row item renders:

- List name + local-part (`Bob's Weekly` — `bob-weekly@<our-domain>`) → `mail-lists-list-item-name`.
- Member count, subscribed-only → `mail-lists-list-item-member-count`.
- Last-send-at → `mail-lists-list-item-last-send`.
- Sends-today / Recipients-today (running counters; the user's own quota meter) → `mail-lists-list-item-quota`.
- Edit button → `mail-lists-list-item-edit-button`.
- View members button → `mail-lists-list-item-members-button` — opens the `mail-list-members` page.
- Delete button → `mail-lists-list-item-delete-button` — destructive; confirms before cascading.

### Add list sheet

Triggered by the "+ Add list" button. Fields (one element set shared on every app — conditional fields show by context, like `mail-add-credential`):

| Element | ui.yaml id |
|---|---|
| List name input (free text, max 64 chars) | `mail-lists-add-sheet-name-input` |
| Local-part input (the address the list will send from) | `mail-lists-add-sheet-local-part-input` |
| Domain picker (the user's owned domains per `mail-multidomain.md`) | `mail-lists-add-sheet-domain-picker` |
| Description (optional, free text) | `mail-lists-add-sheet-description-input` |
| List-Help URL (optional) | `mail-lists-add-sheet-list-help-url-input` |
| List-Archive URL (optional) | `mail-lists-add-sheet-list-archive-url-input` |
| Per-send recipient cap (optional, ≤ admin ceiling) | `mail-lists-add-sheet-per-send-cap-input` |
| Submit button | `mail-lists-add-sheet-submit-button` |
| Cancel button | `mail-lists-add-sheet-cancel-button` |

The address-validation runs the same logic as `mail-aliases.md` § Cross-user uniqueness — the local-part can't collide with another user's exact alias, can't be a reserved local-part, must be unique within (local_domain, kind=list).

### `mail-list-members` page

Opened via the per-list "View members" button. A per-list page scoped to one `list_id`.

| Element | ui.yaml id |
|---|---|
| Page heading (with list name) | `page-heading` |
| Subscribed-count + unsubscribed-count summary | `mail-list-members-summary` |
| Members list (paginated, indexed component) | `mail-list-members-list` |
| Per-row member | `mail-list-members-list-item` (`-address`, `-subscribed-at`, `-status`) |
| Add member button | `mail-list-members-add-button` |
| Add member sheet (single address input) | `mail-list-members-add-sheet-address-input` / `-submit-button` / `-cancel-button` |
| Batch import button | `mail-list-members-import-button` |
| Batch import sheet (CSV upload OR paste-list-of-addresses) | `mail-list-members-import-sheet-input` / `-submit-button` / `-cancel-button` |
| Batch import per-line outcome tally (added / already subscribed / skipped invalid, with each skipped line's reason) | `mail-list-members-import-result` |
| Per-member unsubscribe button | `mail-list-members-list-item-unsubscribe-button` (manual user-driven unsubscribe; flips `unsubscribed_at`) |
| Per-member re-subscribe button | `mail-list-members-list-item-resubscribe-button` (user explicitly re-adds; flips `unsubscribed_at` back to NULL) |

The batch-import CSV format is one email-address per line (or one per row in the CSV); no header row required; lines with invalid email syntax skipped + reported. Maximum 10000 addresses per import (admin-tunable `mail.outbound.list_max_import_per_batch`, Tier 2).

### Composing a list message

A user composes a message in their mail-write UI (the regular mail composition surface per `smtp-server.md` § Submission). To send to a list, they enter the list address (`bob-weekly@<our-domain>`) in the To: field, the existing recipient picker of the conversations compose (`dm-compose-form`, the mail-write surface per `html-mail.md`). Compose-time list controls live on that compose form, not the `mail-settings` family: the pre-Send warning is `dm-compose-list-send-warning` and the approaching-quota / over-limit notice is `dm-compose-list-quota-warning`.

The compose flow:

1. User types the list address into the To: field (the recipient picker).
2. Client warns: "This will send to <member_count> subscribed recipients on <list-name>. Today's quota: <recipients_today> / <recipients_per_day>. Proceed?"
3. User clicks Send.
4. Client calls `fauna.bridges.send_list_message(list_id, message)` — the **nest** validates ownership, reserves the per-list rate caps atomically, stamps each member's `List-*` headers, and enqueues one outbound message per subscribed member (`submit_outbound`); the nest DKIM-signs each at the outbound hand-out. A message carrying other than exactly one From field is refused `fauna.bridges.invalid_params` before any cap is reserved, since that DKIM key is picked by the From domain (`smtp-server.md` § Architectural rules → *Exactly one From field*).
5. The client's sent-folder view shows one entry "Sent to <list-name> (<member_count> recipients)" — not one per recipient: the nest files one Sent copy of the client-composed message per send, never the per-member stamped copies.

The per-recipient delivery state (delivered / pending / unsubscribed-during-send) is queryable via the per-list-detail audit (`fauna.bridges.list_list_send_history(list_id)`); the UX renders it as one progress for the whole send on the compose form, `dm-compose-list-send-progress`; the one Sent entry reuses the existing thread row.

---

## Reserved local-part: `unsubscribe@`

`unsubscribe@` is reserved via a mechanism **deliberately distinct from the role-address set**: it lives in the uncircumventable **creation-only predicate** `fauna_mail::aliases::is_creation_reserved_local_part` (+ `CREATION_RESERVED_LOCAL_PART_FAMILIES`), **not** in `DEFAULT_RESERVED_LOCAL_PARTS` / `mail.inbound.reserved_local_parts` — that set feeds `classify_role_address`, which would mis-route `unsubscribe+<token>@` to the admin mailbox instead of the unsubscribe handler (`smtp-server.md` § abuse@ / postmaster@ role-address routing and `mail-aliases.md` § Reserved local-parts own the two-set split). The reservation is **per-domain independent** per `mail-multidomain.md` § Per-domain alias namespace — every local_domain has its own `unsubscribe@<that-domain>` route.

Mail to `unsubscribe+<token>@<our-domain>` routes envelope-time to the list-unsubscribe mailto handler (per § The mailto handler above). Mail to bare `unsubscribe@<our-domain>` (no token) is rejected with `550 5.1.1 unsubscribe@ requires a list-unsubscribe token` (the user-friendly explanation in case a confused human types it manually).

User-tier creation of an alias whose pattern matches `unsubscribe@` or `unsubscribe-*@` is refused with `400 reserved_local_part` per `mail-aliases.md` § Reserved local-parts.

---

## Wire shapes (named, not redefined here)

| RPC | Caller | Purpose | Notes |
|---|---|---|---|
| `fauna.bridges.list_account_lists` | user client | enumerate own lists | `()` → list of `mail_lists` rows (own actor) + the caller's per-account meter for today (`account_recipients_today`, `account_recipients_per_day` — the compose form's quota; § Composing a list message) |
| `fauna.bridges.create_account_list` | user client | create one list | `(local_part, local_domain, friendly_name, description?, list_help_url?, list_archive_url?, recipients_per_send?)` → `list_id` |
| `fauna.bridges.update_account_list` | user client | edit metadata | `(list_id, partial_row)` → ok |
| `fauna.bridges.delete_account_list` | user client | destructive remove (cascades members) | `(list_id)` → ok |
| `fauna.bridges.list_list_members` | user client | enumerate subscribed/unsubscribed members | `(list_id, paginate?, include_unsubscribed?)` → list of `mail_list_members` rows |
| `fauna.bridges.add_list_member` | user client | subscribe one address | `(list_id, recipient_address)` → ok |
| `fauna.bridges.batch_import_list_members` | user client | bulk subscribe | `(list_id, addresses[])` → `{added, skipped_invalid, skipped_duplicate}` |
| `fauna.bridges.unsubscribe_list_member` | user client (manual only) | flip `unsubscribed_at` | `(list_id, recipient_address)` → ok. The one-click token form is internal to the HTTPS/mailto handlers (they flip by the cached token index directly), **never this RPC** |
| `fauna.bridges.resubscribe_list_member` | user client | flip `unsubscribed_at` back to NULL | `(list_id, recipient_address)` → ok |
| `fauna.bridges.send_list_message` | user client (compose) | queue one outbound per member | `(list_id, message)` → `{queued_count, estimated_quota_remaining}` |
| `fauna.bridges.list_list_send_history` | user client | per-list send history | `(list_id, limit)` → list of `{sent_at, recipient_count, delivered_count, unsubscribed_during_send}` |
| `fauna.bridges.rotate_list_unsubscribe_secret` | admin client | rotate the deployment-wide secret | `()` → ok; mints the fresh secret + re-derives every member token in one atomic transaction |

Wire-level shape lives in the nest implementation track (`docs/goal/architecture/apps/bridges.md` § Bridge-kind catalogue (caller classes)); this doc owns which RPCs exist + what each carries.

---

## Architectural rules

- **List is an alias kind (`kind='list'`).** One of the kinds in `mail-aliases.md` § Alias kinds. Resolution at RCPT TO time treats it separately: inbound mail to a list address rejects; a raw-SMTP submission whose MAIL FROM is a list address is **rejected** (`fauna.bridges.list_submission_requires_send_rpc`) — the only list-send path is the `send_list_message` fan-out.
- **RFC 8058 + RFC 2369 stamping is mandatory.** The nest stamps the One-Click + List-Id headers per recipient in the `send_list_message` fan-out (the client can't bypass — a raw-SMTP submission from a list address is rejected); the nest DKIM-signs the stamped body at the outbound hand-out, so the List-\* are inside a From-aligned signature.
- **Per-list rate accounting separates from per-actor.** A list submission doesn't consume the per-actor submission rate cap; it consumes the per-list + per-account-list + per-deployment-list caps.
- **The unsubscribe token is HMAC-derived, deterministic.** Same list_id + recipient_address → same token. Determinism is the trade-off for simplicity; the worst an attacker can do with a leaked token is unsubscribe the recipient (not a privilege escalation).
- **The one-click HTTPS endpoint is unauthenticated.** The token IS the auth (per RFC 8058 §3.3). No login required for the recipient to click + unsubscribe.
- **The mailto endpoint is the legacy fallback.** Modern MUAs prefer the https One-Click form; the mailto form covers MUAs that don't support RFC 8058 §3.3.
- **GET on the unsubscribe URL is read-only.** Renders a confirmation page; the actual unsubscribe happens on POST.
- **Unsubscribe is sticky.** Once flipped, manual or one-click, the user re-adds explicitly. No auto-resubscription.
- **Lists are user-tier.** A user manages their own lists; the admin sees counts on the admin pane but doesn't edit. Deletion cascades through the alias row → list row → member rows in one transactional step.
- **Reserved local-part `unsubscribe@` is per-domain.** Per `mail-multidomain.md` § Per-domain alias namespace; reserved on every local_domain independently. User-tier alias claims on `unsubscribe@` or `unsubscribe-*@` refused at create time.
- **The deployment-wide secret rotates rarely.** Rotation invalidates in-flight tokens; the in-flight cost is the acceptable trade-off for security-incident recovery.

---

## Don't do these

- **Don't allow non-RFC-8058 unsubscribe shapes.** The nest always stamps the One-Click headers in the `send_list_message` fan-out, so a list message without them can never be produced; a raw-SMTP submission from a list address is rejected (`fauna.bridges.list_submission_requires_send_rpc`). The long-term-defensible default: legitimate lists conform to RFC 8058 (~99% of modern senders do); the few that don't are pre-2018 vintage and not worth supporting.
- **Don't ship list-id with personally-identifying information.** The List-Id is `<friendly-name> <list-pattern@local-domain>` — public; recipient sees the friendly name + the list address. Don't include the user's actor_id or any non-public identifier in the List-Id.
- **Don't let a user enumerate another user's list members via the admin pane.** The admin sees counts on the admin pane (e.g., "Bob has 3 lists with a total of 5000 subscribed recipients"); admin-pane per-list member detail is **forbidden** (a future surface might add admin-side abuse-investigation visibility, but it's out of scope for this doc; user data is user-owned).
- **Don't allow a list to send to recipients on `mail.local_domains`.** Same logic as `mail-forwarding.md` § Per-account "forward all" — if the recipient is on the deployment, the user should add them as a regular recipient or alias, not as a list member. The validator at member-add time refuses with `400 recipient_on_local_domain` and points the user at the alias surface.
- **Don't let a list's per-recipient rate-cap be raised above the admin ceiling.** Per-list `recipients_per_send` is bounded by `mail.outbound.list_recipients_per_send` (Tier 2 admin ceiling).
- **Don't store the one-click-unsubscribe token in a guessable form.** HMAC + 32-byte secret + 24-byte truncated output — the token has 192 bits of entropy, infeasible to brute-force against the deployment's HTTPS endpoint.
- **Don't auto-resubscribe an unsubscribed member.** Unsubscription is sticky; the user must manually re-add. Auto-resub would defeat the one-click unsubscribe contract.
- **Don't auto-chunk a list submission that exceeds the per-send cap.** Auto-chunking would create receivers seeing multiple separate sends with different timestamps; intentional chunking by the user (sending three separate messages each to a third of the list) is the supported pattern. Auto-chunk is a footgun.
- **Don't expose the deployment-wide unsubscribe secret in the admin UI.** The secret value isn't visible; only the rotate button is. Defends against an admin's compromised credentials surfacing the secret.
- **Don't tie list submissions to the per-actor submission rate cap.** Separate buckets; list and non-list submissions are independent.
- **Don't allow `unsubscribe@` to be claimed by a user as an exact alias.** Enforced by the creation-only predicate `is_creation_reserved_local_part` per `mail-aliases.md` § Reserved local-parts (landed).
- **Don't generate List-Archive URLs by default.** A list with no archive is structurally legitimate; fabricating an archive URL where none exists is misleading.
- **Don't surface the list-recipient delivery state on the user's UI as per-recipient detail.** The progress bar shows aggregate (`<N> of <M> delivered`); per-recipient delivery audit lives in the per-list send history with paginated detail (the user can drill into it but doesn't see it at compose time).
- **Don't allow the user to disable RFC 8058 stamping on a per-list basis.** Stamping is mandatory; per-list disable would create non-compliant senders + degrade the deployment's reputation. The MTA stamps regardless.
- **Don't run the unsubscribe HTTPS endpoint behind auth.** RFC 8058 §3.3 explicitly requires no-auth-needed for the One-Click form. Auth-gating would break the contract.
- **Don't store List-Archive URLs that point off-deployment without user-confirm.** If the user sets `list_archive_url = https://archive.example.com/<list>` (off-deployment), warn them once + persist. Don't silently accept; the URL is published to recipients via the List-Archive header. **The warning is the add/edit sheet's own submit, armed** — `apps/common.md` § Two-click confirm's inline arm, reused here for a save that destroys nothing (the risk is publishing a link, not losing data), so it carries no new `ui.yaml` id: when the List-Archive URL's host is neither one of the user's own domains (the add-sheet picker's options) nor a subdomain of one, and differs from the URL the list already stores, the first press of `mail-lists-add-sheet-submit-button` saves nothing and relabels the button with this feature's descriptive confirm `mail_lists.archive_off_server_confirm` ("This archive link is not on your server and goes out with every message. Save anyway?"); a second press while armed saves. **Once** means per URL: re-saving the URL the list already stores never asks again, and changing the URL after arming disarms. The decision is shared Rust, written once — `fauna_client_mail_settings::archive_url_needs_confirm`.

---

## Reading list

1. `principles.md` — § Product invariants (user always controls their data — list members are user-owned; admin sees counts not detail).
2. `docs/goal/behavior/mail-aliases.md` § Alias kinds — the kind taxonomy (7 kinds) the list kind belongs to; § Reserved local-parts (the creation-only `unsubscribe@` family); § Cross-user uniqueness (the list-local-part validation reuses the alias-uniqueness path).
3. `docs/goal/behavior/smtp-server.md` § Submission policy — the per-actor rate cap rules this doc adds the list-relaxation to; § abuse@ / postmaster@ role-address routing — the reserved local-parts pattern `unsubscribe@` joins; § Outbound delivery (the queue + retry machinery the list-send uses).
4. `docs/goal/behavior/mail-policy-config.md` § Tier 2 (new mass-mailing knobs); § Tier 3 (per-account list knobs); § Inbound perimeter (the reserved local-parts list extension).
5. `docs/goal/behavior/mail-multidomain.md` § Per-domain alias namespace — the per-domain `unsubscribe@` reservation pattern; § Per-domain catch-all (the catch-all and list-mode interact: a list address takes precedence over the per-domain catch-all per the resolver order).
6. `docs/goal/behavior/mail-observability.md` — list-send metrics surface on the admin mail-telemetry surface alongside non-list outbound (kind-partitioned aggregates; that doc's pipeline is target-state).
7. `docs/goal/behavior/mail-deliverability.md` — the warmup + blocklist-self-check shapes apply to list outbound too; list-mode submissions count against the deployment's warmup quota.
8. `docs/goal/architecture/apps/bridges.md` § Bridge-kind catalogue (caller classes) — where the new `list_*` RPCs land in a later nest-track commit.
9. (design ratified 2026-05-07; tracked internally) § Provisioning surface — the *DKIM* wrapped-blob shape (retired 2026-10-04), cited for contrast only: the list-unsubscribe secret deliberately does **not** ride it (nest-held plaintext `mail_list_unsubscribe_secrets` row, the SRS-secret model — § Implementation status today).
10. RFC 8058 (One-Click List-Unsubscribe + List-Unsubscribe-Post); RFC 2369 (List-Help, List-Archive, List-Unsubscribe); RFC 2919 (List-Id syntax); RFC 3834 (Precedence: bulk for auto-mail signaling).
11. Buttondown / Mailchimp / Substack / Sendgrid feature surfaces (informative — this doc's bar is parity with their list-management UX, self-hosted).
