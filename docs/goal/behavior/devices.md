# Devices — target state

Owns: devices, delegated-authoring
Status: ratified
Authority: the cross-app device model — device add/remove/list flow, the DeviceAuthorization capability model + the device-signed authoring acceptance rule (§ Device-signed authoring), session management + emergency lockout, the device-sync-channel primitive (dormant), the cross-device MLS state-sync flow's device-facing rules (durability Rules 1/2), and the MLS offline-compose plaintext-intent contract (§ Offline compose); page UX → [`../ui/devices.md`](../ui/devices.md) + ui.yaml; the MLS replica at-rest/reserved-set shape → [`reserved-folders.md`](reserved-folders.md) § MLS state replica; the MLS channel protocol → [`direct-messages.md`](direct-messages.md) (which defers cross-device state sync here); key audience → [`../architecture/key-material-hierarchy.md`](../architecture/key-material-hierarchy.md); first-device setup → [`onboarding.md`](onboarding.md).

Adding, syncing, and removing devices.

## Status

Token issuance shape unchanged. The MLS channel ciphertext plane is
**WS-RPC**: `fauna.conversations.channel.send` / `.fetch`, with the typed
`fauna.conversations.channel.message` push frame for delivery (the only
framing — no legacy JSON push remains). The `POST|GET /api/v1/channel/{id}` HTTP twins were **deleted**
in the Spec-Y cutover (T8); key packages are now published over
`fauna.conversations.keypackage.upload` (the `/api/v1/keypackage/*` HTTP
routes were deleted in Spec-Y2 slice 5). No HTTP residue remains on either
plane.

**Channel-based device sync is deferred** — see [§ Implementation status
today](#implementation-status-today). The deterministic-routing
`DeviceSyncChannel` primitive (`libs/fauna-mls/src/channel.rs`) is built and
unit-tested, but the sync-engine path that would drive it is not wired;
multi-device file propagation today rides the `fauna.sync.changes.{record,list}`
change plane ([`file-sync.md`](file-sync.md)), not the channel. The *general*
device-sync substrate for account data is settled (2026-08-10): the
generalized account feed, owner
[`../architecture/account-sync-plane.md`](../architecture/account-sync-plane.md)
§ The sync plane — the channel primitive stays dormant there too.

## Implementation status today

The protocol shapes in this doc are the **target state**. What is wired in
code today:

- **Live:** auth bootstrap (`fauna.auth.*`), device register / list / remove
  and the file-change sync plane (`fauna.sync.*`), and key-package publish over
  `fauna.conversations.keypackage.upload`. Multi-device file propagation works
  through the `fauna.sync.changes` change plane — independent of the channel.
- **Built 2026-09-22 — `fauna.sync.devices.list`'s `online` binds WS-RPC sessions to devices (it read `false` for every app seat and every per-user agent until then).** The handler read the `/sync/ws` data-plane registry alone, which only the legacy headless `fauna-sync` daemon registers on; the apps' in-process engines and the per-user `fauna-sync-agent` take the WS-RPC nudge and hold no data-plane socket ([`sync-engine-deployments.md`](sync-engine-deployments.md) § Control Plane Principle), so the Devices page painted offline for every device a user actually owns. Now the upgrade keeps the minting device key on the connection and the roster joins it to the row's principal — § Listing Devices → *The binding* owns the rule, including what binds nothing and how `last_seen_at` is written. Pinned nest-side by `bins/fauna-nest/tests/conformance_device_online.rs` and end to end by `tests/e2e-unified/tests/test_device_online.py` (tui). The socket sweep at device removal is a separate duty, built 2026-09-23 on this binding (`transport-connection.md` § *Revocation teardown* → *The per-device twin*). The `/sync/ws` data plane the handler once read was removed 2026-10-02 (`file-sync.md` § Relay serving → *The `/sync/ws` data plane leaves with the daemon*), so `online` is the WS-RPC binding alone.
- **Nest-only — session management and both lockout kinds (corrected 2026-09-19; this bullet read "Live" until then).** `fauna.sessions.{list,revoke,revoke_all,lockout}` and the pre-identity `fauna.account.lockout` are built and conformance-tested nest-side (`bins/fauna-nest/tests/conformance_sessions.rs`, `conformance_account_lockout.rs`), and **the four bearer kinds have one app consumer since 2026-09-28** — `fauna_client_account::SessionsClient` wraps them and tui renders the Sessions page off the shared fold ([`../ui/sessions.md`](../ui/sessions.md) § Implementation status today owns the per-app frontier); no client calls the pre-identity `fauna.account.lockout` yet (leg 3, the signed-out door). The app half is designed and **ratified 2026-09-25** — the user ruled the page's home (a Sessions rail sub-page after Devices), all three element-ID legs and the confirm shapes as recommended, and that verify refuses a locked account: page spec [`../ui/sessions.md`](../ui/sessions.md) § The ruling, behavior § Session Management + § Emergency lockout below; the build is queued tui-first. **Four gaps measured the same day; (1) and (2) are built as of 2026-09-20, (3) is built — its mint-time gate 2026-10-01, its app half (the scheduled refresh, the bearer mints' locked latch and the typed `FfiError`) 2026-10-03 — and (4) is built on tui (2026-10-04), the other six apps following in its trickle-down:** (1) **built 2026-09-20 — every bearer holder now keeps the `token_id` it minted.** It read "every client mint path drops `token_id`" until then: `fauna_anon_client::MintedBearer` carried only `token` + `expires_at`, as did `TokenRefreshOutcome::Success`, `FfiBearerToken` and wasm `mintBearer`, so no app could name its own session. Now all five seats retain it and fold it into the shared `fauna_protocol::auth::OwnSessionIds` set — § The client's own session → *Built 2026-09-20* has the seat-by-seat shape and the pins. **The consumer landed on tui 2026-09-28** (§ The client's own session → its closing paragraph); the other six apps follow the page's trickle-down; (2) **built 2026-09-20 — a single-session revoke now closes that session's sockets**, the per-token twin of the per-actor teardown ([`../architecture/transport-connection.md`](../architecture/transport-connection.md) § Connection lifecycle → *Revocation teardown* → *The per-token twin*). It read "unbuilt" until then: `session_handlers`' `revoke` / `revoke_all` arms dropped tokens only, and since the nest validates a bearer once, at the upgrade, a revoked session kept dispatching as `User` on its open connection until it next reconnected. Now the connection remembers the `token_id` it was upgraded with and both arms go through `AppState::{revoke_session_authority, revoke_other_sessions_authority}`, which do the token half and the socket half together — closing exactly the revoked session's sockets (4401) and leaving the actor's other sessions dispatching, with the caller's own Reply delivered first when the session being ended is the one asking. Wire-witnessed by `bins/fauna-nest/tests/conformance_revocation_teardown.rs`; (3) **the mint-time lock gate is built 2026-10-01 — `auth_core::verify_core` reads `locked_until` after `refuse_if_superseded` and the suspension refusal, answers `fauna.auth.account_locked`, and exempts admins (user ruling 2026-10-01, mirroring the use-time exemption at [`../architecture/api-layers.md`](../architecture/api-layers.md) § Layer 1 until a cannot-lock-the-last-admin guard exists); `SilentChallengeOutcome::Locked`, `LaunchSnapshot::locked_until_secs` and the terminal `Offline { transient: false }` landing are built (`libs/fauna-launch-machine/tests/token_refresh.rs`), and web's `classifyChallengeError` types it. That was the MINT half; the use-time half is the 2026-09-23 landing below. **The app half is built 2026-10-03, all in shared Rust:** the launch machine arms exactly one refresh at `locked_until` itself (`libs/fauna-launch-machine/tests/locked_refresh.rs`); both bearer mints latch the refusal with its unlock time and the reconnect supervisor holds its dials until then (`ws_challenge_bearer::tests::a_standing_lock_is_answered_without_dialling`, `reconnect::tests::a_locked_account_holds_until_its_unlock_time_and_no_longer`, `fauna-ws-substrate`'s `a_held_refusal_waits_out_its_time_instead_of_backing_off`); and the UniFFI apps receive `FfiError::AccountLocked { locked_until_secs }` (`fauna-ffi`'s `locked_maps_to_its_own_variant_carrying_the_unlock_time`) — § The locked state has the mechanism. What remains of the locked state is its surface, gap (4). The paragraph that follows is the pre-build measurement, kept for the use-time reasoning:** `fauna.auth.verify` had no lockout gate, by [`login.md`](login.md)'s own statement** (§ Silent Challenge: "the challenge kinds have no security side effects — no IP notification, no lockout enforcement") — `auth_core::verify_core` never reads `locked_until` (the handshake and device-handshake arms do), so a locked-out actor still mints a bearer through challenge/verify — and every door then refuses it at use: the per-RPC authority gate refuses everything it dispatches, and since 2026-09-23 the WS upgrade and every HTTP bearer route ask the same standing question before serving it ([`../architecture/api-layers.md`](../architecture/api-layers.md) § Layer 1: Core Client API → *What `caller_class_for_actor` refuses*), so the bearer opens no socket, receives no Push and moves no bytes (until then only dispatch asked, and a re-minted bearer reconnected and received Push frames). A *suspended* actor no longer mints at verify at all (`login.md` § Silent Challenge). That bounds the harm, but it means the apps whose launch rides challenge/verify (web and the UniFFI apps) never *receive* `fauna.auth.account_locked`, which § The locked state needs on every launch path — **the tension between the two docs was put to the user with this design's ask and RULED 2026-09-25: verify enforces the lock too** ([`login.md`](login.md) § Silent Challenge now says so; the mint-time build — `verify_core` reading `locked_until` after `refuse_if_superseded`, the `SilentChallengeOutcome::Locked` arm, the launch machine landing it terminal — landed 2026-10-01) — and wider since 2026-09-21: [`login.md`](login.md) § When to use which now mints every app-held bearer, refresh included, through challenge/verify, so the handshake's lockout gate is passed by no app at all (every seat since 2026-09-23, when the fan-out to `fauna-client`'s `WsChallengeBearer` and web's `getAuthToken` landed) and only the use-time standing check bounds a locked-out actor; (4) **the locked surface is built on tui (2026-10-04); the other six apps do not paint it yet.** tui's launch router lands a locked snapshot on its own surface — the standing notice and the one action — ahead of the generic needs-update arm, a lock met mid-session escalates to the same surface, the notice follows the machine's snapshot back out when the lock lapses (the machine's observer, no app-side timer), and the stolen-identity ceremony runs from there over an anonymous connection through the one shared composition `fauna_client_recovery::ceremony::succeed_stolen_identity`, which tui's signed-in Settings section now calls too (§ The locked state has the mechanism; witnessed by `tests/e2e-unified/tests/test_locked_surface.py` and, for the composition against a real socket with no bearer, `bins/fauna-nest/tests/conformance_locked_succession_ceremony.rs`). On the other six the refusal still reaches every launch path and both mint seats carry its unlock time (gap 3), but nothing paints it: web types it in `classifyChallengeError` and the UniFFI apps do not yet branch on `FfiError::AccountLocked`, and linux's and the UniFFI face's ceremony drivers still hand `succeed_with_held_kit` their signed-in requester — adopting the shared composition is part of each app's leg-2 lift (the element IDs are leg 2 of [`../ui/sessions.md`](../ui/sessions.md) § Element IDs, approved 2026-09-25 and allocated in ui.yaml with the tui build).
- **Live — the tier device-cap refusal reaches the user (shared Rust + tui + linux; 2026-09-15).**
  `fauna.sync.device_limit_exceeded` (§ Step 4, emitted since 2026-09-02) rendered as the
  generic fallback on every app and was only logged by the shared register sites until then.
  Now: `RpcError::localized` has an exact arm (`error.sync.device_limit_exceeded`, the cap
  and both remedies; `action()` keeps it `Rejected`), the account runtime's enrollment pass
  reports `EnrollmentPass::DeviceLimitExceeded` and records `EnrollmentRefusal` in the
  per-actor credential slot (cleared by the next accepted register, swept at sign-out), and
  the Devices page paints `EnrollmentRefusal::notice` off `AccountStoreHandle::enrollment_refusal`
  — owner of the render and its per-app build state: [`../ui/devices.md`](../ui/devices.md)
  § Implementation status today. Since 2026-09-24 a pump-holding seat serves the remedy
  (§ Removing a Device) at once. A cap-refused pass used to hold the removal's target
  resolve for as long as its unauthenticatable network legs took. The owning rule is
  [`../architecture/account-client-lifecycle.md`](../architecture/account-client-lifecycle.md) § The
  client-side lifecycle → *Commands and passes*.
- **Live — the This-device marker reads the ENROLLED row (tui + linux; 2026-08-19).**
  The row this machine's grant actually registered on is surfaced by
  `AccountStoreHandle::enrolled_device_row` (`libs/fauna-sync-engine`, off the principal
  slot's registration latch — a fact about the past, not a re-derivation), and
  `fauna_devices_machine::this_device_row` owns the rule that pairs it with the app's own
  `device.db` id: **enrolled wins; the own id is the fallback**. Both halves are correct
  in their own case, which is why the fallback is a rule and not a formality — decision 2's
  agent-less-platform case (iOS, Android) genuinely enrolls under the app's own id, and
  decision 5 accepts that multiplicity outright. *Enrolled wins* was once the converse's
  answer too — a placeholder-row enrollment the badge had to follow — but since the
  one-credential shape (RULED 2026-09-28, built 2026-09-29) every app enrolls on its own
  named row and the two halves always agree, the rule surviving as a formality
  ([`../architecture/apps/sync-agent-credentials.md`](../architecture/apps/sync-agent-credentials.md)
  § Credential model, the RULED 2026-09-28 block; its § Implementation status today owns
  the build). tui refreshes it on the Devices
  nav-edge hydrate, linux on the Devices `connect_map`; both are local store-thread reads,
  never IPC on a paint path. Before this, every app compared its own id, which named **no
  roster row at all** wherever a *different* app on the same box had provisioned the sync
  agent — on a family-safety surface, a guardian who cannot find their own
  device.
- **Live on windows too (2026-09-24) — the UniFFI door exists.** `fauna-ffi` exports
  `devices_this_device_row(own_device_id)` (`libs/fauna-ffi/src/devices.rs`): it reads
  `enrolled_device_row` off this process's account runtime and applies
  `fauna_devices_machine::this_device_row`, answering the own id when no runtime is
  assembled. The windows Devices page reads it on every hydrate, beside the enrollment
  notice. **macOS and iOS switched to it 2026-09-26** — the shared FaunaKit
  `DevicesMachineVM.thisDeviceRow`, read on every Devices appear; the participation
  toggle's own arm reads the same value (`p2p.md` § Per-device participation → *Which
  row is this device's*).
- **Gap — one app still compares its own id** (§ This-device marker; android).
  Android hosts the `fauna-ffi` account runtime (2026-08-22) and has the door above; it
  has not switched to it yet. Web reads the enrolled row since 2026-09-29, the day it
  began hosting the runtime (`accountEnrolledDeviceRow` — `../ui/devices.md`
  § Implementation status today, the device-cap paragraph). Android's badge
  falls back to the own id unconditionally: correct on a single-app box, wrong on a
  shared one, exactly as tui and linux were. This gap is tracked internally.
- **NOT a gap — "linux's actor-scoped `device.db` violates decision 5" is REFUTED
  (2026-08-19).** Linux reads its own id from an *actor-scoped* `device.db`
  (`actor_state_dir(flat, active_actor)`, `apps/fauna-linux/src/sync.rs`) while tui's was then
  machine-flat (`<config>/sync/device.db`), and it was thought that a ruled machine-scoped
  device id made one of them wrong. It does not: the claim traced to a doc comment on
  `fauna_client_sync::agent::agent_sync_device_id` citing decision 5, and the ratified
  decision 5 says the reverse — *"Boundary stated: per-actor, per syncing identity"*. Both
  shapes are conformant; state scoping is owned by
  [`file-sync.md`](file-sync.md) § Multi-account × File Provider consequence 3, whose
  per-actor scoping deliberately leaves a second account nothing to inherit. The comment is
  corrected and the non-ruling recorded beside decision 5 itself
  ([`../architecture/apps/sync-agent.md`](../architecture/apps/sync-agent.md) § Credential
  model). Independent of the badge either way: whichever id an app mints, the marker follows
  the row it enrolled on.
- **Deferred — channel-based device sync** (§ Step 5, § Device Sync Channel).
  The `fauna-mls::DeviceSyncChannel` primitive (deterministic routing id, MLS
  group create / join) is built and unit-tested, but the
  `libs/fauna-sync-engine` driver was never wired: the group was never
  bootstrapped (`bootstrap_device_sync_channel` had no callers), no read loop
  pulled from it (`pull_and_process_sync_channel` had no callers), and the
  encrypted-event post was gated behind an always-false `has_group` check. That
  inert path — together with its HTTP transport on the routes deleted in T8 /
  Spec-Y2 slice 5 — was **removed** so no future session trusts the dead route
  references. The clean primitive and the design notes
  (2026-03-08, tracked internally) remain for a future,
  *properly-specced* E2E-device-sync feature. **The channel-vs-`fauna.sync.changes`
  question is now settled for MLS state replication** (2026-07-05, § Cross-device
  MLS group-state sync): the state replica rides the `__mls` reserved set over
  WS-RPC, **not** the `DeviceSyncChannel` — an MLS device-group has a bootstrap
  circularity for exactly this use case (a fresh device cannot join an MLS
  channel without the MLS state it is trying to fetch), whereas `BackupKey` is
  derivable from the imported seed alone. The channel primitive stays dormant;
  any future device-channel feature (live device-to-device events) is orthogonal
  and would need its own bootstrap story.
- **Live — § Cross-device MLS group-state sync → *A sibling-joined group is
  adopted mid-session by a targeted import* (2026-09-01).** The conversation
  walk's first step on every app (`fauna_conversations::session::adopt_sibling_groups_first`,
  called by the native `poll_conversations` / the receive loop's `poll_bound`
  and the wasm `pollConversations`) runs the injected `SiblingGroupAdopter`: a `hash_only`
  `fauna.mls.get` probe of the `provider` tip, and on a change
  `ProviderReplica::import_group_into` per group the engine lacks, then
  `bind_restored_slice`. Shared Rust end to end; no per-app glue beyond the
  one-line sweep hook. The door imports only a group that positively seats
  this identity — a blank seat is refused there since 2026-09-01 (the
  subsection's *two doors, two answers* paragraph).
- **Live — § Cross-device MLS group-state sync → *Who may consume a Welcome —
  any device that holds its key* (2026-09-01).** No code moved: the ruling
  ratifies what every app already does — both arms on every device consume,
  the merge folds the identical join — and pins it at the engine
  (`fauna-mls::state_replica`) and the assembly
  (`fauna-client-mls-sync::orchestration`). The one row it left — the
  non-holder's `error`-level push-arm line — is closed: the engine's
  `MlsError::NotAddressedToThisDevice` (asked of the provider's own key-package
  storage, on both join doors), carried across the seam as
  `BackendError::WelcomeNotAddressedHere`, reported at `info` by
  `fauna_conversations::session::report_welcome_ingest_failure`. Shared Rust; all
  seven apps inherit it.
- **Live — § Cross-device MLS group-state sync → *A provider CAS conflict is
  reported, not repaired*, the classification (2026-09-01).**
  `fauna_mls::state_replica::merge_provider_replicas` attributes each
  both-changed key to its group and reads the epoch on each side through the
  exporter; same-leaf progress is reconciled to the side further along (its
  cursor rides with it, `ProviderMergeOutcome::reconciled_keys`), and only the
  genuine case reaches the `warn` in `MlsReplicaClient::save_provider_cas`.
  Shared Rust; all seven apps inherit it.
- **Live (landed 2026-09-20) — § Cross-device MLS group-state sync, the
  `history/<ch>` contents bullet → *the derived state is carried twice*, and
  Rule 3 applied to it.** Before this, the reaction aggregate and the delete
  tombstone were `ConversationsManager` memory only (`reactions`, `deleted`,
  `delete_claims`), projected onto every read and never written into the slice,
  which the module header and this doc both already described as carrying them
  — so every tombstone and every pill a device produced was lost at relaunch,
  the sender's own delete included, and nothing could replay them back. The
  manager's `snapshot_channel_slice` now runs the *same* projection
  `thread_detail` serves the apps (`project_reactions_and_deletes` — one
  function, two callers, so the app's answer and the replica's cannot drift)
  over the slice's messages, and records `deleted_messages` +
  the reaction event log beside the fold (then the stamp-less
  `reaction_events`; the stamped `reaction_log` alone since the compat-remnant
  sweep retired the stamp-less field, 2026-09-24 —
  [`../architecture/compat-remnant-sweep.md`](../architecture/compat-remnant-sweep.md)
  § Program 4); `restore_channel_slice` re-seeds the
  manager from both — the tombstone set by union (monotone), the reaction log
  fill-never-overwrite (this device's own log outranks another's copy).
  `merge_history_slices` unions the tombstone instead of taking the
  higher-watermark side's message copy, because an own `Delete` is opaque to
  its author and the deleting device need not be the furthest-polled one. Both
  new fields are additive and omitted when empty, so a channel with neither
  still encodes byte-identically to a pre-field slice. Shared Rust; all seven
  apps inherit it. Pinned by
  `a_tombstone_and_its_reaction_pills_survive_a_history_slice_restore`
  (`manager_integration_tests.rs`, red-verified — own delete, an admin's
  cross-sender delete, the pills, and a reaction toggled *after* the restore,
  the leg the folded aggregate alone cannot pass) plus
  `merge_unions_a_tombstone_the_higher_watermark_side_has_not_seen` (the
  legacy-only side-pick pin went with the stamp-less field).
- **Live (landed 2026-09-22) — § Cross-device MLS group-state sync, the
  `history/<ch>` slice's merge → a device no longer loses its OWN reactions to
  a concurrent save.** `merge_history_slices` resolved two devices' reaction
  log for one message by a side-pick — the higher-watermark side's log won
  whole — and what that cost was the losing device's own reactions since the
  fork, for the `Delete` tombstone's reason: an own `Reaction` is MLS-opaque to
  its author, so the reacting device need not be the furthest-polled one. It is
  now a **deduped union**, per message (at landing, whenever both sides' logs
  for that message were fully stamped; unconditionally since the stamp became
  required, below). What made the union available is the signed stamp
  landed the day before: the fold reads the SET of distinct signed ops rather
  than the sequence ([`community-rooms.md`](community-rooms.md) § The three
  classes → *Community* → *Who wrote it*, which owns that rule), so two copies
  of one op collapse and the order the union writes cannot change the answer.
  At landing its limit was the stamp-less event, which ranked by its log
  POSITION and so kept the side-pick, and the union's canonical order kept the
  stamp-less `reaction_events` projection an older peer read agreeing with the
  stamped fold. **Both retired 2026-09-24 by the compat-remnant sweep**
  ([`../architecture/compat-remnant-sweep.md`](../architecture/compat-remnant-sweep.md)
  § Program 4): every event carries its author's stamp
  (`StampedReactionEvent.sent_at_ms` is required; a stamp-less event is refused
  at rest), the stamp-less field is neither written nor read, and the log-order
  fold is gone. The union is still written in a canonical order that places
  `Remove` after `Add` at an equal stamp. Commutative down to the canonical
  bytes, which the equal-watermark tie-break reads. Shared Rust; all seven apps
  inherit it. Pinned by `merge_unions_two_fully_stamped_reaction_logs` and
  `a_unioned_log_keeps_a_same_stamp_retraction` (`store/history.rs`), and the
  retirement by `a_stamp_less_event_is_refused_at_rest` (`reactions.rs`) and
  `a_slice_carrying_only_the_retired_unstamped_reaction_field_restores_none`
  (`manager_integration_tests.rs`).
- **Live — § Cross-device MLS group-state sync, Rule 2 (save-ordering) + the
  automatic heal.** The ingest cursor rides inside the `provider` blob
  (`fauna-mls::state_replica::ProviderReplica::cursors`, merged per channel to the
  side whose values won it and `min` where neither did);
  `fauna-client-mls-sync::orchestration::save_snapshot` uploads every
  `history/<ch>` slice before the `provider` and skips the `provider` entirely if
  any history upload failed; `MlsError::FutureEpochCommit` classifies a skipped
  commit apart from an already-merged one. A **legacy** `provider` blob sealed
  before the `cursors` field decodes with none and falls back to the
  `history/<ch>` watermark — which can be torn (alpha replicas sealed under the
  pre-Rule-2 save order exist in the field). The **automatic heal** covers that
  case: on a `FutureEpochCommit`, the chat-rail poll
  (`fauna-conversations::backends::fauna_mls::poll_inbound_conv`) rewinds its
  in-flight cursor to 0 and re-walks the log in the same call — already-applied
  commits quiet-skip, already-held messages skip a `has_message` pre-check (so
  the re-walk does not re-fetch attachment blobs), the skipped commit applies,
  and the device lands on the head epoch; the durable cursor only ever advances
  (the rewind is transient), and the next provider save writes a consistent
  `{provider, cursor}` pair, retiring the torn state. Guarded to one rewind per
  channel per session (an un-processable commit re-tears on every walk) and
  skipped when the walk began at 0. The
  folder rail needs no heal — its cursor is RAM-only, seeded to 0 each launch.
- **Live — § Cross-device MLS group-state sync, Rule 2 (cursor safety), *both*
  rails (landed 2026-07-12).** An unhealable strand now genuinely stays loud, and
  a cursor never runs past an epoch transition the local group did not
  incorporate. `poll_inbound_conv` returns a `ConvPollOutcome { ingested,
  stalled }` (the chat twin of `FolderPollOutcome`): on a
  `CommitApplyOutcome::Stalled` record it stops the walk **before** that record —
  for **both** stall kinds (an own-leaf commit whose resync failed or ran
  gate-less, and a future-epoch strand whose one-per-session rewind is spent or
  unavailable) — and `BackendCatchUp::catch_up_after` turns a stalled walk on
  **either** rail into an error, aborting the gated send (pending cleared,
  retryable). Until this landed the chat rail did the opposite: the poll advanced
  its cursor past the stalled record and returned a bare count, so a gated
  `remove_participant` took its `expect_no_commit_since` baseline from *past* an
  unincorporated epoch transition — the nest accepted the rebuilt commit, every
  other member quiet-skipped it as `PastEpochCommit`, and the sender merged and
  reported success while the removed member stayed in the live group and kept
  decrypting. Pinned by `conv_poll_stalls_before_unincorporated_own_leaf_commit`
  and `gated_remove_over_an_unresyncable_own_leaf_commit_errors_instead_of_forking`.
- **Live (landed 2026-07-13, hardened same day) — § Cross-device MLS
  group-state sync, Rule 2's failure taxonomy on
  the inbound commit rail — and why no out-of-order buffer exists.** A failed
  `process_commit` is classified structurally
  (`fauna-mls::MlsError::InvalidCommit` vs the local-failure errors), and
  `apply_inbound_commit` rounds each side per Rule 2: an **intrinsically
  invalid** commit (bytes no member can ever apply — malformed, a non-commit
  body, or a validation verdict computed on state every honest member shares
  at the group's current epoch: the bytes, the public tree, the epoch's
  context and membership/confirmation keys) is `Skipped` — stalling would let
  one garbage record from any in-group member wedge every other member's
  channel forever (a remote DoS) — while a **local** failure on a
  possibly-valid commit (absent **own private epoch decryption keypairs**,
  storage/library/merge failure, a group the engine doesn't hold yet) is
  `Stalled`, stopping the cursor before it: the group may have advanced
  without this device, and consuming the record would silently drop every
  later message sealed under the new epoch (user-irrecoverable) and hand a
  gated send a forked baseline. **The split is per-variant, never per-class**:
  "reached deeper validation ⇒ deterministic across members" is FALSE —
  staging a commit's update path needs the receiver's own private epoch
  keypairs, device-local state a torn snapshot or replica restore can lose
  with the bytes and shared tree intact, and openMLS launders exactly that
  absence into the same `StageCommitError` class as byte-level rejections. So
  each openMLS error variant is classified explicitly in
  `MlsEngine::process_commit`, and every ambiguous or future variant rounds
  to *local* (a classifier whose two answers have asymmetric blast radius
  must never reach the destructive answer through a catch-all). Until this
  landed the catch-all returned `Skipped` for *everything*, bypassing the
  Rule-2 stall guard — the one live route to silent message loss on an
  otherwise-ordered log. A retry cannot re-decrypt the same bytes
  (PrivateMessage decryption consumes the sender-ratchet generation *before*
  any staging or merge verdict exists), so the engine memoizes the commit's
  identity **across the whole consumption window** — armed before the
  decrypt, kept on any post-consumption local failure (stage or merge),
  dropped on success and on every skip verdict — and the memo lives **in the
  provider KV**, so the same snapshot/replica CAS that persists the consumed
  generation persists the memo beside it: a relaunch or cross-device restore
  onto a post-consumption snapshot keeps the retry reporting the same local
  failure (a torn `{consumed-provider, no-memo}` pair is unrepresentable),
  until a replica resync heals the channel. The **decrypt** layer carries the
  same split one nesting level earlier: `SecretReuseError` (and
  `RatchetTypeError`) — device-local ratchet failures on a possibly-valid
  commit — round to *local* (stall, memo kept), never `Skipped`: a consumed
  generation is device-local state, and Skipping a commit the group applied
  would silently drop it — permanent for a single-device victim.
  Pinned by
  `conv_poll_skips_intrinsically_invalid_commit_without_wedging`,
  `conv_poll_stalls_on_locally_unappliable_commit_and_heals_after_welcome`,
  `conv_poll_stalls_when_own_epoch_keys_are_absent_instead_of_eating_the_commit`,
  and `fauna-mls`'s `process_commit_classifies_intrinsic_invalid_vs_local` /
  `own_epoch_key_absence_classifies_local_not_intrinsically_invalid` /
  `secret_reuse_error_classifies_local_stall_not_intrinsically_invalid` /
  `merge_failure_memo_keeps_retry_local_until_healed` /
  `stage_failure_memo_survives_relaunch_onto_a_post_consumption_snapshot` /
  `stage_failure_memo_rides_the_provider_replica`. **Corollary — no
  buffering rail:** an application message can never arrive *before* the
  commit that advances its epoch on any inbound route — every route walks a
  single per-channel nest log in seq order, and a commit always precedes the
  messages sealed under the epoch it creates. The only real-world shape that
  *looked* like out-of-order delivery was a commit this device failed to
  incorporate, which is exactly what the `Stalled` classification now stops
  the cursor on. The dark unit-tested buffer rail that once existed for this
  (`MlsEngine::{process_envelope, flush_epoch_buffer}` + `epoch_buffer`,
  production callers: none, ever) was deleted with this change rather than
  wired: a tested-but-uncalled recovery rail beside a live path that dropped
  on failure was the worst of both.
- **Live (landed 2026-07-13) — § Cross-device MLS group-state sync, the
  folder sweep's channel classification.** The background folder commit
  poll sweeps every engine group that is neither a bound chat thread, nor a
  scheduling channel, **nor durably chat-marked** — the marker
  (`fauna:channel_kind_chat:<ch>` in the provider KV, stamped by
  `FaunaMlsBackend::bind_channel`) rides the same snapshot/replica blob as
  the group itself, so an un-*re*bound chat channel after a relaunch or
  cross-device restore is classified correctly while its RAM-only binding is
  still being rebuilt. Before the marker, such a channel was swept as
  folder, applying its commits out of order: the sweep applies membership
  commits while skipping application messages, so it could advance the
  shared engine's epoch past an unread chat message — permanently
  undecryptable under MLS forward secrecy (a *No user-data loss* surface),
  reachable through a narrow multi-device conjunction. Residual: a chat
  channel bound only by pre-marker binaries keeps the old derivation until
  its next bind stamps it — the pre-existing exposure, narrowing with every
  launch, never widening. Pinned by
  `folder_sweep_never_eats_a_commit_on_an_unbound_chat_channel`
  (red-checked on both legs: classification and the full loss chain). The
  marker is also what the launch-evidence predicate reads off the replica's
  own bytes (`ProviderReplica::is_channel_chat`, 2026-09-22): a listed
  channel carrying it owes a `history/<ch>` slice, one without it owes
  nothing — owner
  [`../architecture/foreign-handle-resolution.md`](../architecture/foreign-handle-resolution.md)
  § Peer-auth model → *Discovery-failure semantics*.
- **Live — § Cross-device MLS group-state sync, Rule 1 (merge-ordering), the
  gate-less conversations path.** `fauna-conversations::backends::fauna_mls::
  RailBackend::{add_participant, remove_participant}`'s **else-branch** (no
  `CommitGate` injected — single-device client, a nest without the replica
  plane, or a degraded launch where `sync.load()` failed) stages the commit
  under the per-channel lock, sends, and merges only on nest accept (clears the
  pending on a send failure, so the operation is cleanly retryable). Residual,
  accepted: a crash in the instants between send-accept and merge leaves only
  the *authoring device* stale at the old epoch — its next poll hits the landed
  commit as a loud `OwnLeafCommit` and, once a gated launch has the replica
  plane, the §5 resync heals it; the group itself converges on the landed
  commit. This narrows the forbidden failure (whole-group permanent strand) to
  a single-device staleness with a detector and — **when the replica plane is
  reachable** — a heal path. **The gate-less strand is closed on native legs
  (landed 2026-07-13) by the durable local pending:** the else-branch persists
  the staged pending to the engine's local store (`MlsEngine::save_state`)
  before the send — the ungated-fallback discipline the folder Remove
  pioneered — and the relaunch reconciles it against the log, identity-checked
  both ways: the poll's own-commit arm (`apply_inbound_commit`) merges the
  reloaded pending when its stamped blake3 equals the logged commit's (the
  accept→merge crash window — the device converges with no gate, no plane, no
  user action), while a *resumed* pending (snapshot-loaded, its authoring
  process dead — tracked apart from live stages) that a complete unstalled
  walk from 0 never matched is provably undistributed and is cleared, so the
  interrupted operation retries cleanly. Pinned by
  `gate_less_crash_after_accept_heals_by_merging_the_reloaded_pending` and
  `gate_less_pending_that_never_landed_clears_after_full_walk` (crash-shaped:
  stage → persist → reopen a second engine over the same SQLite). **Remaining
  residual — web only, declared:** a wasm engine has no local store by
  construction (its only persistence is the nest replica — whose absence is
  what "gate-less" means), so a *web* gate-less client keeps the pre-existing
  safe-and-loud posture: the poll stalls before the unincorporated commit
  (Rule 2 bullet above), no fork, recoverable once the nest gains the plane.
  Same per-platform posture as the folder adapter's `persist_group_state`
  wasm no-op.
- **Live — § Cross-device MLS group-state sync, fetch-paging completeness
  (landed 2026-07-13).** The shared poll walks
  (`poll_inbound_conv` / `poll_inbound_scheduling` / `poll_inbound_folder`)
  page until an **empty** page, never until a short one — a short page does
  not mean drained: the nest byte-budgets every conv serve page to the 2 MiB
  frame (close-early-never-skip, the conv twin of the mail relay budget;
  budget core `bins/fauna-nest/src/segments/mod.rs::take_page_within_budget`),
  and an already-deployed nest clamps `limit: 0` to a ONE-record page. That
  clamp mismatch (client "whole tail" vs nest `clamp(1,500)`) had silently
  starved every production drain to one record per round trip and falsified
  the arm-2 reconcile's "complete walk from 0" premise above — a from-0 walk
  could believe itself complete after the first record and clear a resumed
  pending whose commit HAD landed further down the log. The nest now serves
  `limit <= 0` as the full 500-record page (`segments::effective_fetch_limit`)
  and the client never sends a non-positive limit; completeness is carried by
  the empty-page termination alone, so it holds against old nests and
  budget-shortened pages alike. Pinned by
  `poll_drains_the_whole_tail_against_a_nest_that_clamps_limit_zero_to_one`,
  `full_walk_reconcile_never_clears_a_landed_pending_behind_a_short_first_page`
  (client), `fetch_limit_zero_serves_the_full_page_not_one_record`,
  `fetch_page_closes_early_on_the_frame_budget_and_skips_nothing`,
  `fed_channel_fetch_page_closes_early_on_the_frame_budget`,
  `fed_mls_pull_page_closes_early_on_the_frame_budget`,
  `fed_mls_pull_freezes_on_a_single_over_frame_record_instead_of_skipping`
  (nest).
- **Live — § Cross-device MLS group-state sync, launch resilience.** A
  *transient* launch-time `sync.load()` failure (nest unreachable) no longer
  degrades the session to single-device until relaunch:
  `fauna-client-mls-sync::orchestration::restore_and_wire_with_retry` (all
  legs — linux `wire_mls_state_sync`, web `restoreMlsState`, the native FFI
  launcher) re-attempts the whole restore with exponential backoff (1 s
  doubling to a 30 s cap, indefinitely) until the nest is reachable, so the
  session converges to the fully-wired state with no user action. Three
  boundaries: the retry runs **before the first poll** (§5 preserved — a late
  re-load is forbidden because `restore_into` swaps the whole provider KV and
  would clobber post-launch engine state: sender-ratchet rewind = nonce reuse,
  post-launch groups wiped; blocking is free since every poll would fail
  against an unreachable nest anyway); it goes **through `load()` itself**
  (the launch save-gate lifts only inside a successful load, so no save can
  ever clobber the real replica); and only a **transport fault** retries — a
  nest-answered rejection (classified at the `RpcErrorClass::is_rejection`
  seam) fails fast to today's single-device fallback, so a client against an
  **older nest without the `fauna.mls` plane** keeps working (within-major
  bidirectional compat). The retry holds only `Weak` session handles, so
  logout / session replacement mid-retry ends the loop.
- **Live — EVERY key-package mint routes through the durable
  `ensure_keypackages` surface (save-before-publish), never a raw engine mint.** A
  provider-storage restore **swaps** the engine's KV, so a key package minted
  before it loses its private init key — a peer who fetched that package from the
  nest pool then mints a group whose Welcome this device can never join (the web
  slice-6 bug class; pinned by `fauna-mls`
  `key_package_minted_before_provider_swap_loses_its_init_key`, and one layer up
  at the replenish entry point by `fauna-conversations`
  `keypackage_minted_via_ensure_keypackages_survives_a_provider_swap`). MLS init
  keys are fresh random HPKE keys (not derived from the identity secret), so an
  unsaved mint is **user-unreconstructable** the instant its engine state is
  swapped. The invariant is therefore not merely *ordering* but *durability*, and
  it binds **every mint path — login replenish, any low-count auto-replenish, and
  the settings manual "refresh keys" surface**, because the provider swap is
  neither launch-only nor a mere race: `MlsStateSync::resync_provider` is a
  mid-session swap (fires on an own-leaf commit this device did not author), and
  the **launch restore itself** deterministically swaps the KV over any key
  minted before it completes — the window in which a returning / second device
  (one with a stored replica) can hit the settings "refresh keys" surface while
  the launch save-gate is still down. So every path mints through
  `ConversationsManager::ensure_keypackages` / `ensure_last_resort_keypackage`,
  which mint on the session engine and then — **before the public key package is
  published to the pool** — await a durable, Rule-2-ordered replica save through
  the injected `ProviderPersist` seam (`fauna-conversations::backend`, wired by
  `restore_and_wire` beside the history flush). The publish proceeds only once
  that save confirms the `provider` blob (the init keys' durable home)
  **landed**; before the launch restore lifts the save gate the flush no-ops, so
  the mint **fails loudly** rather than ship a package whose init key the
  imminent restore would wipe. The window *before the seam exists* refuses too:
  every leg declares the plane at session build
  (`FaunaMlsBackend::expect_replica_restore` — the shared tokio launcher for the
  FFI legs + tui, linux's session build, the wasm constructor), and until
  `restore_and_wire` injects the seam a mint fails the same way; a restore that
  fails permanently lifts the refusal (`abandon_replica_restore` — single-device,
  no swap is coming), and a client with no plane at all publishes directly. This is **save-before-publish**, the key-package
  analogue of `manager::send`'s durable-before-done (Rule 3): the debounced
  replica autosave stays the steady-state *coalescer* but is **no longer the
  durability mechanism for a mint** — a KP is never fetchable before its init key
  is durable (which the older notify→autosave path left true only once the
  debounce fired). Pinned one layer higher by
  `keypackage_mint_refuses_to_publish_until_the_provider_replica_is_durable` +
  `keypackage_mint_refuses_to_publish_while_the_replica_restore_is_pending`
  (`fauna-conversations`),
  `the_plane_holds_key_package_mints_until_the_launch_restore_ends` and
  `provider_persist_reports_durable_only_after_the_launch_gate_lifts`
  (`fauna-client-mls-sync`). The observers still fire on a mint for
  steady-state coalescing, but a mint is never a raw
  `MlsEngine::generate_key_packages` + a non-notifying upload, and never a
  throwaway `MlsEngine::new_in_memory` whose keys exist nowhere durable.
  - **login replenish** (one-time pool + last-resort) runs inside
    `ConversationsSession::start_receive_loop`, sequenced after the
    `MlsSyncLauncher` restore — every native leg inherits the ordering with no VM
    glue (the windows `App.xaml.cs` pre-loop calls and the linux login-task
    replenish are deleted); linux wires its restore before calling the loop, and
    the web SPA (which never runs the native loop) sequences its own replenish
    after `restoreMlsState`.
  - **auto-replenish + settings refresh:** web (`mgr.ensureKeypackages`) and
    windows (count-only settings, no client mint — a parity debt, not a
    declared difference: its Encryption page owes the low flag and the Refresh
    every other app has, `direct-messages.md` § Key Package Management) were
    always durable; linux
    (`conv_backend::replenish_key_packages`, now the Encryption page's refresh
    button only — its low-count auto-replenish off the settings count read was
    deleted 2026-09-27: it minted before the restore, a window the backend now
    refuses — see the pre-seam sentence above), android
    (`host.manager.ensureKeypackages`), and apple (`EncryptionSettingsVM.swift`
    → the session `ConversationsManager.ensureKeypackages`, reached via the
    app-held `ConversationsVM`) route through the same surface. The
    throwaway-engine `mls_generate_key_packages` FFI (the footgun that minted
    init keys nowhere durable) is **deleted tree-wide** — no app can
    reintroduce the bug without re-adding the FFI (tombstone comment at its old
    site in `libs/fauna-ffi/src/mls.rs`).
- **Live (landed 2026-09-02) — BOTH doors now enforce "takes the epoch over before
  its first send"; until this landed the sentence above was true of the launch door
  only.** The launch half was always structural: the sync state starts with an empty
  `authored` map and `authored_current_epoch` reads `unwrap_or(false)`, so a fresh
  process holds no send right on any channel. The **resync** half was not.
  `MlsStateSync::resync_provider` restores through `ProviderReplica::restore_into`,
  which swaps the engine's *whole* provider KV — rewinding the sender ratchet of
  every group the engine holds — while its only caller revoked authorship for the
  **one** channel whose own-leaf commit triggered it. So every other channel on the
  leaf kept `authored = true` over a rewound ratchet, and its next application send
  encrypted at a generation the peer had already consumed: nonce reuse, two
  plaintexts under one AEAD key, both durable in the nest's channel log. openMLS
  reports it as a `SecretReuseError` at the **receiver**, which is a liveness
  symptom rather than a mitigation — by the time it fires both ciphertexts are
  written. `resync_provider` now clears the whole `authored` map as part of the same
  restore, so the scope of the revocation matches the scope of the swap. The map is
  cleared **wholesale** rather than for the restored replica's own `channel_ids()`:
  the swap is wholesale, and `restore_from_provider_storage` carries the engine's
  own local-only groups across it (dropping the rest — since 2026-09-22; before, it
  kept them listed with no state), so `channel_ids()` under-counts
  what it touched. Over-clearing costs one self-`Update` per channel on its next
  send — the same commit a takeover posts anyway, and the same cost every launch
  already pays. Pinned by
  `a_resync_clears_the_send_right_on_every_channel_not_just_the_trigger`
  (`fauna-client-mls-sync`), red-first against the pre-fix code. The tie-rule
  comment in `state_replica.rs` that leans on this bound now names where it is
  enforced instead of asserting it.
- **Live (landed 2026-09-22) — the swap carries the engine's own local-only groups,
  and every Welcome join persists the provider.** A Welcome join quit inside the
  autosave debounce was wiped by the next launch's whole-KV swap while staying
  listed, and lost for good one launch later (the native copy overwritten by the
  hand-over flush); the resync door had the same shape. Owned by
  [Cross-device MLS group-state sync](#cross-device-mls-group-state-sync) → *A group
  the engine holds but the snapshot does not list survives the swap*: the door
  re-adopts a local-only group that positively seats this identity through the
  per-group carve-out and drops (map + native row) one that does not; `MlsEngine::new`
  sweeps a stale `active_groups` row; the three Welcome joins persist the provider
  after the join (the web engine's only durability); and a merged flush no longer
  records the tip as this device's own, so the adoption detector fetches what the
  merge folded in. Pinned red-first at every layer (the subsection lists the six).
- **Live (landed 2026-09-26) — the swap no longer carries a group another device
  left.** The carry above could not tell a join since the snapshot from a sibling's
  folder leave, so a sibling's next launch or resync re-listed the left folder and
  the leaver's next launch held it again. Owned by the same subsection → *A sibling's
  deletion is not a join*: the engine keeps a durable per-device record of the last
  listing it adopted or authored, and the swap drops a local-only group that record
  names.
- **Live (landed 2026-07-11) — Rule 3 durable-before-done closes the send-path loss
  window** (an own message sent within `REPLICA_DEBOUNCE`
  (1.5 s) of an app quit was permanently lost to its author — the root cause of the
  user-reported "after a restart a conversation lists but shows zero message bubbles";
  a violation of `principles.md` § No user-data loss). The mechanism
  and the fix are owned by [Durability rules](#durability-rules-ratified-2026-07-09)
  **Rule 3**: `manager::send` awaits a `history/<ch>` CAS-save after appending the own
  message (through the `HistoryPersist` seam `restore_and_wire` injects — shared Rust,
  all six legs, no per-leg glue), and `bootstrap_group` durably persists the still-empty
  slice before the first send's takeover CAS-puts the `provider`. Two constraints the
  obvious fixes missed, both resolved by that shape: (i) skipping empty slices in
  `snapshot_replica` would be *worse* — with no `history/<ch>` blob the next launch
  creates no thread and `poll_inbound_conv` early-returns without a binding, so the
  channel becomes permanently invisible *and* unreachable (the history blob is also
  what distinguishes a chat channel from a deliberately thread-less scheduling /
  folder channel at restore); (ii) a per-send **history-only** save is sufficient —
  the durable `provider` already lists the channel, because the first application send
  in any epoch runs the Rule-1 takeover (`ensure_epoch_takeover`, two provider
  CAS-puts) before the wire send. Proof: shared-Rust
  `orchestration::own_message_survives_a_hard_quit_immediately_after_send` +
  `a_quit_mid_send_after_bootstrap_still_restores_a_reachable_thread` (production
  send path, hard quit inside the debounce), and the windows tier_3 no-settle-gate
  restart test `test_conversations_own_message_survives_an_immediate_restart.py`
  (its settle-gated sibling `test_conversations_history_survives_app_restart.py`
  pins restore + render). Historical note: design residual (a) of
  the MLS cross-device state-sync design (ratified 2026-07-05; tracked internally) asserted
  "the history union-merge heals it on the next launch"; that was **false** (the
  thread store is RAM-only and is *seeded from* the replica) — the in-session CAS
  union heal is real but only a later same-session save applies it.
- **Conformant (landed 2026-07-09) — the folder Remove path's `CommitGate`
  adoption** (the former Rule-1 non-conformant site, LEAD ②, tracked internally; with the gate-less conversations
  path's stage-before-send fix above, **no known Rule-1 non-conformant site
  remains**). `FoldersAuthor::drive_removal`
  routes the Remove through `CommitGate::gated_remove_member` via the
  `RemovalCommitGate` seam (`fauna-client-folders`, adapter on
  `Arc<FaunaMlsBackend>`; every leg — ffi factory, linux, wasm — wires the
  session backend in). The once-blocking **async build hook was not needed**:
  under the gate the provider replica (step-2 CAS-put, identity-stamped) is the
  durable carrier of the staged Remove, so the sentinel needs no per-round
  byte update — the real gaps were the catch-up (`BackendCatchUp` now
  dispatches thread-less folder channels to `poll_inbound_folder`) and
  cursor safety (the folder walk stops before a commit it could not
  incorporate — Rule 2's "round toward the safe side" — so a rebase baseline
  can never pass an unincorporated epoch transition). **That dispatch was
  reachable only under a test fixture until 2026-07-12**: it routed on the
  in-memory `mark_folder_channel` marker, whose only production writer is the
  *recipient's* welcome-join and which empties on relaunch — while the only actor
  that ever gates a folder commit is the **owner** removing a member, and the
  owner's backend never marks (it creates the group through `FolderGroupCrypto
  for Arc<MlsEngine>`, which cannot reach the backend). So every contested
  owner-side removal misrouted to the thread-less chat poll, never advanced its
  cursor, and span the rebase to `RetriesExhausted` — **a contested folder
  member removal could not converge**, failing loudly and retryably (no fork: the
  chat poll's thread-less guard sits before its cursor mutation). The dispatch now
  routes on `FaunaMlsBackend::is_folder_rail`, the same **durable,
  engine-derived** derivation the background `folder_poll_channels` sweep
  already used — which is why that sweep was never affected. The **ungated fallback**
  (no gate injected) also satisfies Rule 1: stage → persist the staged pending
  locally (`MlsEngine::save_state`) → sentinel bytes (CAS) → merge → persist →
  send, with resume preferring durable sentinel bytes over any rebuild.
  Mechanism detail: `mls-group-key-material.md` § M2 *Rotate-on-removal*.

## Cross-device MLS group-state sync

**The gap this section closes** (stated as it stood on 2026-07-03, when the plane was unbuilt — **not** a description of today; for what is wired now, read [Implementation status today](#implementation-status-today) first). A second device imports the same secret key (same ActorId) and joins the file-change sync plane, but of itself it **gains no access to the user's pre-existing conversations**: the MLS group *cryptographic* state — ratchet tree, secret tree, per-epoch key schedule, epoch secrets, leaf keys — persists only on the device that processed each Welcome/commit (native SQLite `mls_state.db`, web IndexedDB) and has **no cross-device transport of its own**. Absent the replica plane below, conversations would be effectively single-device. The mechanism that removes that limit is specified next; its build state, leg by leg, is owned by *Implementation status today*.

**Mechanism (design ratified 2026-07-05** — full rationale + verified constraints
tracked internally; at-rest/reserved-set
shape authority: [`reserved-folders.md`](reserved-folders.md) § MLS state replica; key audience:
[`../architecture/key-material-hierarchy.md`](../architecture/key-material-hierarchy.md) § MLS
group state): a **state replica synced under `BackupKey` through the `__mls` reserved folder,
mirroring Drafts Sync** (single-leaf state-sync, *not* multi-leaf re-invitation). Two
`BackupKey`-sealed path families ride new User-class WS-RPC kinds `fauna.mls.{get,put}` (opaque
per-`(actor, path)` storage like `fauna.drafts.*`, plus a CAS `base`/`fauna.mls.conflict`
with client-side merge-retry — the shape of the `fauna.config.*` kinds retired 2026-10-02):

- **`provider`** — the openMLS provider snapshot (`engine.rs::export_provider_storage` +
  `list_groups_with_raw_ids`, restored by `restore_from_provider_storage`). A snapshot is the
  state of one *leaf*: one whose groups are seated under another identity's leaf — a
  predecessor's, at a successor's path after a succession — is never restored (owner:
  [`succession-aftermath.md`](succession-aftermath.md) § Re-key scope → *What a successor's
  replica restore may take from a predecessor's*).
- **`history/<channel_hex>`** — the per-channel thread-store slice **including own message
  plaintext** + the **reaction/delete derived state** + the per-channel ingest `watermark` (seq) +
  the **fetch coordinates of the slice's attachments**, keyed by handle (where each sealed blob
  rests on the channel's home nest and which key opens it; additive, omitted when empty — the
  rule they serve is [`conversation-attachments.md`](conversation-attachments.md) § Attachments →
  *Retention*). Required because a sender cannot MLS-decrypt its own application messages — log
  replay alone can never reconstruct own history on a second device; the coordinates because the
  restored device's poll never re-walks the records that named those attachments.
  - **The derived state is carried TWICE, deliberately: folded onto each message *and* raw
    beside it.** The folded form — the `deleted` flag and the reaction aggregate on each message
    — is what a client renders straight off the slice, including one too old to know the raw
    fields. The raw form — the **set of tombstoned message ids** and the **per-message reaction
    event log** `(actor, emoji, add|remove)` — is what the restoring device re-seeds its own
    projection from, and it is the only form that survives the two cases the fold does not: a
    restore onto a store that **already holds the message** (the append dedups by id and keeps
    the local copy, dropping the flag with it), and a **reaction made after the restore** (the
    projection overwrites the aggregate with a fold of its event log, and `toggle_reaction`
    resolves add-vs-remove by folding the same log — so without the raw events the user's own
    restored 👍 would re-add instead of retract, and that one event's fold would wipe every
    restored pill). Both raw fields are additive and omitted when empty. Why any of it must be
    here rather than replayed: an own `Reaction`/`Delete` is MLS-opaque to its own author off the
    log, and a restored device resumes *past* every record it already folded — so no re-walk can
    re-derive either, for the same reason own plaintext cannot be re-derived. This is
    § Durability rules rule 3 applied to the two derived states
    ([`../ui/conversations.md`](../ui/conversations.md) § Reactions & message delete → *At rest*
    defers the mechanism here).
  - **A community room's parked floor delete records ride here too** (additive, omitted when
    empty; landed 2026-09-20): the signed, unsealed moderation records the inbound walk stepped
    past but could not judge yet — the policy version they name was not served, or a name a
    succession could still lift is unresolved. A claim awaiting its verdict, deliberately unlike
    the tombstone set, which is verdicts only: nothing paints until the room backend judges it
    under a chain this device anchored itself, on its first pass over the room after the restore.
    Here for the walk's reason: the durable cursor advances past a record before it is judged, so
    a record still parked at quit is never met again on any device resuming from that cursor —
    and the loss was silent, since the room's unverified-moderation notice is derived off the same
    set. Public bytes, so persisting the claim costs no secret; a union on merge and on restore
    (either device may hold the only copy, and a re-judge of one the other side already honoured
    only re-applies a tombstone it carries), bounded like the live set. Not rule 3's case (the
    nest log still holds the bytes) — the rule and the ruling, with the re-walk alternative
    weighed, are [`conversation-rooms.md`](conversation-rooms.md) § Implementation status today,
    residual (d).

A device loads the replica and processes the nest-ordered per-channel log from its **ingest cursor**
(receiver ratchets are deterministic given the ordered stream → convergence for **other members'**
traffic; own traffic comes from the history slice). Concurrent writes are governed by the
**device-owned-epoch invariant**: a device may send application traffic on a channel only while
it authored the channel's latest **own-leaf** commit — a device switch posts a self-`Update`
commit first (fresh sender chains, so a shared single leaf never forks a ratchet generation).
The scope is the device's own leaf's lineage, deliberately: another *member's* commit does not
revoke the right (every epoch gives every member a fresh sender chain, and at most one of this
account's devices holds the authorship flag, so the single-leaf fork stays impossible) — a
revoke-on-any-foreign-commit over-approximation made two actively-sending members of a shared
folder channel take the epoch over from each other on every send pass, an endless commit churn
(fixed 2026-08-24; `FaunaCommitGate::note_foreign_commit`). Commits are
serialized by an optional `expect_no_commit_since` precondition on the channel's **routed** send —
`fauna.conversations.channel.send` for a same-nest channel, `…channel.send_remote` for one homed on
a foreign nest, whose relay carries the precondition into that nest's `fauna.federation.channel.append`
([`../architecture/federation.md`](../architecture/federation.md) § Cross-nest shared folders +
channel append owns the relay). Which nest is not a detail here: the precondition is a seq in the
*home* log's space, so it is only meaningful evaluated by the nest the cursor was read from, and a
commit sent to any other nest is not merely mis-filed but ungated. The nest rejects with
`fauna.conversations.channel.stale` if a `Commit` envelope landed since — envelope variants are
nest-visible without opening ciphertext; a rejected commit **rebases** (clear pending → process
intervening records → retry). An own-leaf
commit a device did not author is the **resync signal**: refetch the `provider` replica (uploaded
by the taking-over device *before* it sent the commit, so a crash mid-takeover stays recoverable).
The `BackupKey` audience keeps the whole plane opaque to the nest (no nest holds `BackupKey`) on
every nest — there is no storage mode ([`storage-modes.md`](../architecture/nest/storage-modes.md)).

### Durability rules (ratified 2026-07-09)

Three rules govern every write on this plane. The first two exist because MLS has **no repair
primitive for a stranded member**: a member cannot skip an epoch, no member can re-issue a commit
for a transition that already happened, and there is no re-Welcome for a member who is still in the
group but stuck. A device that loses its place is therefore stuck *permanently* — fail-closed (it
decrypts nothing new) rather than a confidentiality break, but a durable liveness brick the client
cannot repair, which is exactly the shape `principles.md` § Client-recoverable nest state forbids as
*client-causable unrecoverable state*. Both rules are consequences of one idea: **never let durable
state claim progress that durable state cannot justify.** The third exists because the plane is
the **only** durability the thread store has (it is RAM-only and re-seeded from the replica at
launch): state only this device can produce must reach the replica before the user can believe
it saved.

**Rule 1 — merge-ordering.** *No code path may merge an epoch-advancing commit until that commit's
bytes — or the staged pending that produces them — are durable somewhere the restart path will
find them.*

The failure it forbids: a device merges a commit locally, the send then fails (or the process
dies), and on relaunch the commit is absent from the log while the device has already advanced past
the epoch that produced it. It cannot re-issue the commit (MLS forbids it) and it is no longer at
an epoch any other member is at. Every remaining member is stranded at the old epoch with no
detector and no repair. **A local merge is not "rolled back by a crash":** the debounced replica
autosave snapshots the *whole* provider KV — every group, no channel-type filter — so unrelated
activity on any channel durably persists a merged-but-undistributed epoch within its debounce
window. So `merge → send` is a permanent-stranding window, not a retry window.

Three landed disciplines satisfy Rule 1, and are the prior art to copy:

- **gated commits** (`fauna-client-mls-sync::commit_gate`): stage pending → CAS-put `provider` →
  gate-send → merge on accept / clear on `StaleCommit` + rebase. The folder Remove adopts this
  loop wholesale when the plane is wired (`fauna-client-folders::RemovalCommitGate`).
- **the folder Remove's ungated fallback** (`fauna-client-folders`): stage → persist the staged
  pending locally (`MlsEngine::save_state`) → CAS-put the sentinel bytes → merge → persist → send,
  with resume preferring the durable bytes over any rebuild (a rebuild is safe only while nothing
  can have been distributed).
- **the gate-less conversations membership path** (`fauna-conversations::backends::fauna_mls`,
  the `add_participant`/`remove_participant` else-branch): stage → **persist the staged pending
  locally** (`MlsEngine::save_state`, native; a wasm engine has no local store — see
  [Implementation status today](#implementation-status-today)) → send → merge on accept /
  clear on failure — each followed by a persist — under the per-channel lock. Here "durable
  where the restart path finds it" is the **nest log itself** for the commit bytes (the merge
  runs only after `channel_send` accepted, so a merged-but-absent-from-the-log state is
  unreachable) plus the **local snapshot** for the staged pending: a relaunch reconciles the
  reloaded pending against the log by its stamped blake3 identity — merge it when its own
  commit landed (the accept→merge crash window), clear it once a full walk proves it was never
  distributed.

The optimistic merging `MlsEngine::{add_member,remove_member}` primitives do **not** satisfy the
rule and must not gain new production callers on any epoch-advancing path.

**Rule 2 — save-ordering, and where the cursor lives.** *Persist what cannot be reconstructed
before the state that records having consumed it. The durable per-channel ingest cursor rides
**inside** the `provider` blob, so `{provider, cursor}` is one CAS and a torn pair is
unrepresentable.*

The cursor is the read position of the crypto state, so it belongs in the same blob under the same
CAS — not in `history/<ch>`, which is a different blob under a different CAS. When they were
separate, both partial-save outcomes were harmful:

- **provider fails, history lands** → durable `{provider @ epoch N, watermark past the N→N+1
  commit}`. The next launch resumed the poll *after* a commit it never applied. Every later foreign
  commit is then for a future epoch, which openMLS reports as the same `WrongEpoch` as an
  already-merged one — so it was quiet-skipped and the device sat at a dead epoch forever, silently.
- **provider lands, history fails** → durable `{provider @ newer, history @ older}`. The cursor says
  "consumed up to seq C" while the plaintext for part of `(..C]` never landed. That data is
  **user-irrecoverable**: a sender cannot MLS-decrypt its own application messages, and a foreign
  record in that range cannot be re-decrypted either (the restored provider already consumed those
  ratchet generations).

Hence the save order is **every `history/<ch>` slice first (all attempted — own-message history is
user-irrecoverable, so maximize what lands per tick), then `provider` only if every history upload
succeeded.** The durable history then always covers at least the durable cursor, and the two
recovery directions are asymmetric by design: a cursor **behind** the provider costs an idempotent
re-walk (already-applied commits skip as past-epoch; records whose ratchet generation was already
consumed fail to decrypt and are skipped; re-folded messages dedup by `message_id`), while a cursor
**ahead** of it strands the device permanently. **Round toward the safe side** — this is also why
the CAS three-way merge resolves cursors per channel rather than by the theirs-wins rule its other
fields use: a channel whose values one side alone won takes that side's cursor, because the merged
state for it *is* that side's state and its cursor indexes it exactly; a channel neither side won
outright keeps the **`min`**.

Corollary, for detection rather than prevention: a commit for an epoch **ahead** of the group's is
never benign — it means this device skipped a commit. It is classified apart from an already-merged
one (`MlsError::FutureEpochCommit` vs `MlsError::PastEpochCommit`) and logged loudly, so a strand
from a legacy torn replica, or any future regression, cannot stay silent.

**Rule 3 — durable-before-done (landed 2026-07-11).** *A user action that creates state only this
device can produce — an own application message above all — completes only after that state is
durably persisted.* A sender cannot MLS-decrypt its own application messages, so an own message
(or own reaction / delete / rename / membership edit, all folded into the same slice) that misses
durable storage before a quit is **user-irrecoverable** — and the thread store is RAM-only,
re-seeded *from* the replica at launch, so there is nothing local left to re-upload afterwards.
The debounced replica autosave is the steady-state *coalescer*, not the durability guarantee: it
leaves a quit-inside-the-debounce window that Rule 3 closes. Mechanics:

- **`manager::send` awaits a `history/<ch>` CAS-save of the thread's slice after appending the own
  message**, through the `HistoryPersist` seam (`fauna-conversations::backend`, injected by
  `fauna-client-mls-sync::orchestration::restore_and_wire` beside the gate + cursor — one shared
  chokepoint, all six legs, no per-leg glue). The same awaited flush runs after every own store
  mutation: reactions, deletes, renames, membership edits.
- **No extra `provider` puts ride the send path.** The durable provider already lists the channel:
  the first application send in any epoch runs the Rule-1 takeover (`ensure_epoch_takeover`), whose
  loop CAS-puts the provider before and after the commit — so a per-send **history-only** save is
  both sufficient and cheap (one blob, proportional to the one thread).
- **`bootstrap_group` durably persists the (still empty) slice before returning** — i.e. before the
  takeover's provider puts, preserving Rule 2's history-before-provider order. **The chat
  Welcome-join (`ingest_welcome`) persists the joined channel's still-empty slice before returning
  too (2026-09-22)**, for the same reason from the other side: the bind stamps the durable chat
  marker that makes the channel owe a slice, and inside the autosave debounce the commit gate's
  provider-only put — on *any* channel — would otherwise list it slice-less, a pairing a restoring
  device cannot heal (no slice, no thread, no bind) and reads as the whole account unloaded
  ([`../architecture/foreign-handle-resolution.md`](../architecture/foreign-handle-resolution.md) § Peer-auth model →
  *Discovery-failure semantics*). A durable provider
  therefore never lists a **chat** channel without an accompanying `history/<ch>` blob, and a crash
  at any point of the first send restores a reachable (possibly empty) thread instead of an
  invisible channel peers keep posting into. (A provider-listed channel *without* a history blob
  remains the legitimate shape for the deliberately thread-less scheduling and folder channels —
  the history blob is what marks "this is a chat thread" at restore, which is also why skipping
  empty slices in `snapshot_replica` is forbidden — and why the launch-evidence predicate
  `federation.md` owns is keyed on the chat marker rather than a count of the listing.)
  **And every Welcome join — chat, scheduling, folder — persists the `provider` too
  (2026-09-22)**, after the slice, through the `ProviderPersist` seam: the join spent an init
  key, so the joined crypto state is user-irrecoverable until the replica lists it, and a web
  engine has no native store for the launch swap's carry to fall back on
  (§ *A group the engine holds but the snapshot does not list survives the swap*).
- **A transient flush failure is warn-logged, never surfaced as a send failure** — the wire send
  already succeeded, so failing the action would prompt a duplicate resend; the append's own
  `notify()` has already armed the debounced autosave as the retry.

Accepted residuals: a hard **crash** in the instants between the wire send and the awaited save can
still lose the own copy (the peer has the message; only a local outbox could close that, a
different design), and a transient-PUT failure followed by an instant quit falls back to the same
window. Both are crash-shaped, not quit-shaped — the *systematic* loss (every graceful quit within
1.5 s of a send) is gone. Pinned by
`orchestration::own_message_survives_a_hard_quit_immediately_after_send`,
`orchestration::a_quit_mid_send_after_bootstrap_still_restores_a_reachable_thread`, and the
windows tier_3 `test_conversations_own_message_survives_an_immediate_restart.py`.

### A provider CAS conflict is reported, not repaired (ratified 2026-09-01)

The three-way merge resolves a key **both** sides changed to different values
**theirs-wins** and reports it (`ProviderMergeOutcome::conflicted_keys`) —
**once it has ruled out same-leaf progress.** Until 2026-09-01 the premise was
that disjoint writers touch disjoint keys and two writers that applied the same
transition write the same bytes, so any both-changed key meant two writers.
Measured, that premise is false in the two-device steady state: the account's
one leaf lives on every device, and its **receiver ratchet is one KV key per
group**, so two devices that folded the same nest-ordered stream to *different
points* — one has decrypted three of a peer's records, the other one — change
that key to different values with no commit, no epoch change and no leaf
advanced; so do a device that folded a commit and its sibling that has not met
it yet, and the epoch owner's own send beside a sibling's receive (the sender
ratchet shares the key). Those are two positions on one deterministic stream,
which every two-device account is in whenever its sweeps flush out of step,
and until this re-cut each one fired the report below.

**What counts as a change (2026-10-05, with openMLS 0.9).** Two values are the same state when their bytes are equal, with one exception: the message-secrets store (openMLS's `MessageSecrets` key per group) records a wall-clock `added_at` for the current and every kept past epoch, read only by openMLS's time-based deletion of past-epoch secrets and never by decryption, so two devices that fold one commit milliseconds apart write different bytes for one state. Every comparison the merge makes — changed against the ancestor, changed to the same value on both sides — reads those values with every `added_at` removed (`fauna_mls::state_replica::provider_values_equivalent`), and keeps whichever side its ordinary rule picks; either timestamp is a correct one. Without it every such pair would run the classification below and every two-device join would read as two writers.

So a both-changed key is first **classified**, in `merge_provider_replicas`,
with the engine as the only oracle: the key is attributed to its group by
openMLS's own delete on a scratch store (the targeted import's technique — no
key layout is read), and the group's epoch on each side is read through the
public exporter (`fauna.merge.epoch-identity.v1`) — two sides at one epoch
export the same bytes, two epochs that differ by number or lineage do not.
Then: **same epoch on both sides → same-leaf progress; the side whose ingest
cursor for the channel is higher wins, silently** (the receiver ratchets are a
function of the stream position; equal cursors with differing bytes is the
owner's send moving the sender ratchet, and either value is sound at the path
because no device ever encrypts with a sender ratchet it loaded — every launch
and resync takes the epoch over before its first send). **Exactly one side
still at the ancestor's epoch, the other AHEAD of it → that other folded a
commit; it wins, silently** (with no ancestor for the group, the strictly later
epoch number wins). **Both sides moved off the ancestor, a side that moved off it
BACKWARDS or sideways, equal numbers off no ancestor, a side with no cursors, or
a key no group claims → theirs-wins and the key is reported.** The direction is
load-bearing and was not always asked: epoch *identity* alone cannot tell a side
that advanced from one that regressed, so a stale replica the nest still holds
and replays satisfied "moved off the ancestor's epoch" exactly as a folded commit
does, took the silent verdict, and became the merged replica with nothing
recorded — the report below is the only record there is, so its loss was the
whole defect. A regression is not a fold, and an
equal epoch number under a different lineage is a fork, not progress: both are
shapes the merge cannot tell from a real two-writer collision, which is where
loud belongs. A
reconciled channel takes the winning side's ingest cursor (its state is one
side's, so that side's cursor indexes it). Pinned by
`two_devices_folding_one_stream_at_different_paces_reconcile_to_the_side_further_along`,
`an_own_send_moves_the_same_key_without_a_cursor_lead_and_still_reconciles`,
`a_sibling_at_the_ancestors_epoch_yields_silently_to_the_side_that_folded_the_commit`,
`a_side_behind_the_ancestors_epoch_is_reported_not_silently_adopted`
and `two_commits_off_one_ancestor_stay_the_genuine_conflict`
(`fauna-mls::state_replica`), and at the save chokepoint by
`provider_cas_conflict_is_reported_never_silent` (`fauna-client-mls-sync::store`,
unchanged).

What survives the classification is the genuine case. That resolution is lossy
for ratchet state, and the condition it resolves is not a normal merge outcome:
a genuine conflict means **two writers advanced this account's single MLS
device leaf concurrently** — the violation the device-owned-epoch invariant
above forbids, arriving from a writer the intra-boundary guards cannot see (the
engine role lock is per (OS login, account); web's election is per origin —
[`../architecture/apps/account-scoping.md`](../architecture/apps/account-scoping.md)
§ Concurrent instances). Two accepted residuals. A sibling two or more commits
behind whose flush lands out of step is *both sides moved off the ancestor*
and is reported as genuine — rare (two commits between two flushes, plus the
out-of-step flush), and loud is the safe direction for a shape the merge
cannot tell from the real thing. And the same-epoch ranking reads the stream
position through the **ingest cursor**, which is a lagging proxy for it: a
snapshot folds its cursor in before reading the crypto values, so the cursor can
only lag, never lead (`MlsStateSync::snapshot_replica`), and two independent
snapshots may lag by different amounts — so the ranking can pick the side that
folded fewer. That is bounded and self-healing, which is why the rule stands as
written: the discarded fold is a receiver ratchet the winner has not consumed, so
its message is re-ingested, and no side is left at an epoch it cannot reach. It
is recorded because the mechanism's own comment used to assert the stronger
claim — that the ratchets are a function of the *cursor* — which the save path
contradicts in writing.

**The ruling: report it loudly, and do nothing else.** The consumer is a `warn`
in `fauna_client_mls_sync::MlsReplicaClient::save_provider_cas` carrying the
conflicted-key **count** (never the keys — openMLS storage keys carry raw group
ids, and the log page is user-readable and copyable). It is shared-Rust, at the
**merging** plane's one write chokepoint, so all seven apps inherit it. There is
a second writer of the `provider` path, and it is deliberately not this one:
`publish_provider` **replaces** the occupant without merging — its only
production caller is identity succession, where the occupant may be sealed under
a predecessor's key this client cannot even open. A wholesale replacement has no
ancestor, so it has no conflict to detect and needs none; adding this warning
there would fire *"two writers advanced one leaf"* on every succession, the
crying-wolf failure rejection 1 below rejects the alert surface for. `warn` clears
`fauna-log`'s `info` ring filter, so the line reaches every app's Settings →
Logs page rather than a developer console. This is Rule 2's corollary applied to
the other detector on this plane: a merge-plane danger signal must not stay
silent.

**Why the log is the only record — the data carries no marker.** The merged
replica is never restored into the running engine, and the resolution leaves
nothing behind that says it was one: the path holds the winner's values as a
plausible replica, this device's next flush folds them forward rather than
taking the path back (the next subsection owns the ancestor rule that makes it
so, and what it changed here), and no second report fires unless both sides
change the same key again — a new collision. Nothing in the data says one ever
happened. Pinned by
`the_conflict_winner_persists_past_the_losers_next_flush_unreported`
(`fauna-client-mls-sync::store`), beside the report's own test.

**Three things it is deliberately NOT**, each rejected on an owner doc's rule:

1. **Not a critical alert.** That plane is explicitly detector-driven and
   flag-free — *"No persistence — the detector, not a stored flag, is the source
   of truth"*, with feeders re-run by the 6 h sweep
   ([`critical-alerts.md`](critical-alerts.md) § Mechanism → *Lifetime*,
   *Who runs the detector*). A merge conflict is a past event with **no
   re-derivable detector**: by the paragraph above, the next sweep finds a
   replica that agrees with itself. Posting it would require exactly the stored
   flag the plane forbids, and re-posting it on every launch is that plane's
   crying-wolf failure.
2. **Not a re-election trigger.** The role lock and the Web Locks election are
   mechanisms *inside* one OS login or one origin; a conflict that survives them
   came from outside both, and between devices there is no election to re-run —
   coordination between devices is the device-owned-epoch invariant, whose repair
   is the own-leaf-commit resync, driven from the inbound path.
3. **Not a partial reload.** Provider KV entries are not independently
   swappable: splicing the winning bytes for the conflicted keys into a running
   engine builds a state no engine authored. The only coherent reload is the
   whole snapshot — `resync_provider`, which has its own detector and its own
   seating refusal. `ProviderMergeOutcome`'s doc comment asked for the partial
   reload until 2026-09-01; no caller ever did it, and the comment was corrected
   rather than implemented.

### A conflict-free merge is kept by the durable plane, not the engine (ratified 2026-09-01)

The ordinary three-way merge — a concurrent device wrote disjoint keys, no
conflict — folds the sibling's groups into the stored replica. The merging
engine never adopts that result: the only doors that swap a running engine's
provider KV are the launch restore and the own-leaf-commit resync
(`MlsStateSync::resync_provider`), each behind its own detector and the seating
refusal. So after a merge the device's *next* export still lacks everything the
sibling contributed. Until 2026-09-01 the merge result was also recorded as the
next merge ancestor (`provider_base`), which made that next flush meet a path
equal to its ancestor, skip the merge, and replace the path with the engine's
own export: every key the sibling had durably saved was gone from the replica
until the sibling's own next flush.

**Why that is not harmless.** Whether the loss is transient depends on the
sibling living to flush again — and the group exposed is exactly one only the
sibling holds: a Welcome it processed after this device's last launch or resync
(a Welcome is consumed on processing, and there is no re-Welcome for a member
already in the group — the strand shape the durability rules above exist to
forbid). While the sibling lives, a third device launching in the window
restores a replica without that group and does not see the conversation until
its own next launch. If the sibling never flushes again — lost, broken, wiped —
the account's only copy of that group's state is gone: every remaining device
is a stranded member of a conversation the sibling's user was told had saved
(Rule 3). That is durable state regressing below what durable state had already
justified — Rule 1's sentence read in the other direction — reached through the
plane's ordinary save path, with no client-side repair. It cannot be ratified
as harmless.

**The ruling: the durable plane keeps the merge; the engine does not adopt it.**
`MlsStateSync`'s `provider_base` — the dedup baseline and the merge ancestor
`save_provider_cas` is handed — now carries one invariant across all three of
its writers: *the last `provider` state this device is entitled to supersede
wholesale* — what the engine **adopted** (the launch `load`, `resync_provider`,
and the foreign-seat launch arm, where replacement is the design) or what it
**authored** (its own last export). After a CAS save it is the export the engine
authored, never the stored merge result. The consequence needs no new
mechanism: on every later flush the path is seen as advanced past the ancestor
and merged again, so a sibling contribution the engine has not adopted is
classified as "theirs advanced" and folded forward — while a group this engine
*deliberately* forgot (`MlsEngine::forget_group`, the folder-leave flow) is
still a deletion relative to its own last export, so the leaver's own flush
takes it off the path. The sibling devices that did not leave still hold the
group, and their swap is what keeps the deletion from coming back: it drops a
local-only group the replica already listed rather than carrying it (§ *A group
the engine holds but the snapshot does not list survives the swap* → *A
sibling's deletion is not a join*), so nothing is resurrected on any device.
The remainder stops being re-folded exactly when it stops being a remainder:
the next launch or resync adopts it into the engine, whose export then carries
it. One assignment in shared Rust at the merging plane's one write chokepoint;
all seven apps inherit it. Pinned by
`a_siblings_group_survives_this_devices_next_flush`
(`fauna-client-mls-sync::sync`).

**Adopting the merge into the running engine is deliberately NOT done.**
Rejection 3 above already rules out a partial reload; a whole-snapshot reload
mid-session is `resync_provider`'s door, driven by a detector (an own-leaf
commit this device did not author) under the channel's serialization lock,
with the pending-commit reconciliation the crash window needs. Running it on
every merge would swap the whole KV under in-flight sends for no durability
gain — the durable plane already keeps the state. What remains is a
*visibility* property this ruling leaves as it found it: a device learns of a
group a sibling joined at its next launch or resync, not mid-session — the
question the next subsection answers with a door of its own, a targeted
import that swaps nothing.

**What this changes for the conflict case above.** The winner's values now
persist past the loser's next flush instead of being taken back: only one side
changed relative to the loser's own last export, so the merge keeps theirs and
reports nothing a second time — a second warn fires only for a *new*
both-sides change, which is a new collision. The retake that the previous
subsection used to describe went away with the ancestor choice that produced
it. The foreign-seat retake at launch is untouched, because it rides `load`'s
baseline (the predecessor's snapshot, which the successor is entitled to
replace), not the save door's. Note the asymmetry the ancestor choice has for
a same-leaf *reconciliation*: when the device further along is the one merging
against a path the lagging sibling landed, its own export is the ancestor, so
every later flush of the ahead side is both-changed against that path again —
before the re-cut that re-warned on each flush until the lagging side caught
up and flushed; now each is reconciled the same way, and the path holds the
ahead state from the first.

**The cursor cost this used to carry is gone (2026-09-01).** While a device
carries an unadopted remainder every flush is a merge, and the merge used to
take the `min` of the two sides' cursors for *every* channel it had not
reconciled. That pinned a shared channel's durable cursor at the lower of the
two devices' read positions for the era, and reset a channel only the sibling
holds to 0 — a cursor-carrying side that lacks the channel was read as having
read position 0 there. Both were cursor-*behind*, Rule 2's safe direction, so
never a strand; the cost was an idempotent re-walk of unbounded length, paid by
every device that restored the blob. The mid-session targeted import below
shortened the era — it usually ends at the next conversation-poll tick now, not
only at a launch or resync — but did not remove the cost, and made the second
half of it *immediate*: the adopter seeds its own ingest cursor from the
snapshot it imports, so a merge-zeroed cursor is re-walked the moment the group
is adopted.

The classification above already gave a *reconciled* channel the winning side's
cursor; the same soundness now covers the channels it never examines, which is
almost all of them — an unadopted era's flushes are ordinarily uncontested. A
channel whose keys **one** side alone changed takes that side's cursor, because
every key of it the merge kept is that side's and the merged state for the
channel is wholly that side's. `min` remains for a channel both sides changed
without a contest, and for one whose keys cannot be attributed. Attribution here
is a byte match on the group id openMLS embeds in each key it writes — fauna's
own `fauna:`-prefixed per-channel markers are not attributed by this rule and
leave a channel on `min` — not the group-load-per-side the classification
affords under a contested key; it is pinned against that sounder oracle by
`every_openmls_written_key_of_a_group_embeds_its_serialised_group_id`, over a
real engine export, so a dependency bump that changed the key encoding falls
back to `min` and reds the test rather than mis-attributing a cursor. The rule
itself is
`each_one_sided_channel_keeps_the_cursor_of_the_side_whose_values_won`.

**Accepted residual, in Rule 2's safe direction.** A genuinely conflicted
group's merged per-key state persists in the replica until adoption rather than
being replaced within a tick; that state was already the lossy outcome the
conflict ruling accepts, and its repair remains the resync.

### A sibling-joined group is adopted mid-session by a targeted import (ratified 2026-09-01)

**The gap.** The two doors that put replica state into a running engine were
the launch restore and the own-leaf-commit resync, and the resync's signal
arrives only on a channel this device already polls. So a group another of
the user's devices joined (a Welcome it processed) or created reached this
device at its next launch — days, on a desktop that stays open — and no
mechanism closed that: the inbound poll walks `bound_channels()` only; a
`fauna.mls.put` records a change row and nudges nobody; the nest's per-actor
roster (`fauna.conversations.channel.list_for_actor`) has no app caller, and
was weighed and rejected as the detector — it is a *delivery* record (an actor
a Welcome was delivered to is on it whether or not any device joined), and
the replica is the account's held-group list. The Welcome push does fan out
to every connection of the actor, but the per-actor inbox acks the row for
whichever device drains it first, and a device lacking that key package's
init key fails the join — an unreliable signal, and none at all for a group
the sibling *created* or a device offline at push time.

**The detector: probe the tip, once per sweep, for a hash.** The receive
sweep — every backstop tick, every push nudge, every reconnect — asks the
injected `SiblingGroupAdopter` before it walks `bound_channels()` (in a full
sweep that is *after* the durable inbox drain — an order the Welcome-consumption
ruling below fixes by cost, not correctness: a drain that can join, joins, and
the same bytes result either way) — the fifth
seam beside the commit gate,
cursor, history and provider persists; declared in `fauna-conversations`,
implemented in `fauna-client-mls-sync`, injected by `restore_and_wire`, so
every app inherits it) whether the `provider` tip changed. The question is
`fauna.mls.get` with the `hash_only` flag: the nest answers with the stored
blob's `blake3` and no bytes (one manifest row), and the client compares against the digest it last loaded, stored or restored.
Only a tip *another device* wrote is fetched — and a tip this device's own
**merged** flush wrote counts as another's, since it carries what the merge
folded in (*A group the engine holds but the snapshot does not list survives
the swap*, 2026-09-22). (The client's hash-the-blob-itself fallback for a
nest that ignored the flag left with the 2026-09-24 compat-remnant sweep.)
Latency: at most one sweep after
the sibling's flush lands — the 30 s backstop, sooner on any push.

**The adoption: import ONE group the engine has never held, swap nothing.**
For every listed group the engine lacks, `ProviderReplica::import_group_into`
loads the group out of a scratch store over the snapshot, asks rule (1)'s
seating question of it — **and imports only a group that positively seats
this identity**: a group seated under another identity's leaf is never
imported (`succession-aftermath.md` § Re-key scope), and neither is a group
whose own leaf seats *nobody* (the blank seat, below) — and establishes
its entry set by **openMLS's own delete**: the group is deleted from the
scratch store and the keys that vanish are its entries — attribution by the
one authority that knows its key layout, so a dependency bump cannot silently
mis-attribute an entry. The engine then refuses if it holds the group or a
single one of those keys, inserts them, loads the group, persists it as a
joined group is persisted, and rolls the entries out again if it does not
load. **This ratifies the carve-out rejection 3 above left open:** importing a
group the engine holds *no* entry of is not the partial reload that ruling
forbids — nothing is spliced, the entries are one snapshot's coherent view of
one group, loaded by openMLS as a unit, the same bytes the launch restore
would load minus the swap of everything else. The whole-KV swap is
deliberately not used here: run on every sibling flush it would rewind every
other group's in-flight state under live sends and polls, a cost the resync
door pays only under its own detector's stall.

**Baseline, cursor, thread.** Each import is folded into `provider_base` —
the invariant, extended: adopted state is state the engine holds, so
the next flush carries the group without a concurrent-writer report — and the
ingest cursor is seeded as at launch (the snapshot's cursor for the channel,
else the slice's watermark). The thread binds from the `history/<ch>` slice
through `bind_restored_slice`, the one binding path launch and this door
share. A provider can land ahead of its slice (a gate save is provider-only),
so a channel imported without a slice stays remembered and is asked for again
every sweep until the slice lands; a channel that never gets one (folder and
scheduling channels are thread-less by design) stays unbound exactly as it
would at launch. A channel bound in a sweep is polled in that same sweep.

**A blank seat is refused at this door and examined-clean at the launch
door — two doors, two answers, one reason (ratified 2026-09-01).** A group that loads but whose own leaf names no
member is what every group this identity was evicted from looks like from
then on (a remove-old another member committed; `forget_group` is
voluntary, so the entry stays). The launch door counts that as *examined*
and not foreign, and must: its verdict is over the whole snapshot, so a
blank seat there is protected by its sibling groups — a predecessor's
snapshot positively seats the predecessor somewhere and the aggregate is
refused — while counting it as unknown would refuse the restore of every
user ever removed from one group, permanently. The per-group door has no
sibling to answer for it: refusing one blank-seat group vetoes nothing, and
importing it lets a foreign identity — a successor, mid-session, on the
very snapshot its launch just refused — come to hold a group it never
joined, and lets a device adopt a group *it* was evicted from, bind a
thread for it, fold it into its merge baseline and re-publish the evicted
group into the account replica for every later restore to pick up. So the
identity question is answered positively or the import does not happen.
**What a device does with a group it is no longer seated in: nothing.** It
stays unadopted — the end state an eviction means — with no thread bound
for it mid-session; its history slice still restores and binds at the next
launch under rule (2), readable history being the identity's, and the
launch's whole-KV restore of the identity's own snapshot carries the
evicted group's inert entries exactly as it always has. The refusal is
`MlsError::NotSeated`, distinct from the foreign seat's policy
violation, because the caller's log must tell them apart: a foreign seat
mid-session is an anomaly worth a warning every time, a blank seat is a
state met again on every sibling flush and is logged below `warn`. **And
the launch door keeps all-or-nothing over the SNAPSHOT** now that a
per-group door exists: the whole KV is one snapshot's coherent state,
including the global entries — the key packages whose init keys a Welcome
addresses — that no per-group attribution owns, so importing a launch
snapshot group by group would be the splice carve-out rejection 3 forbids.
The one thing the swap keeps that the snapshot does not carry is the
engine's **own** local-only groups, re-adopted through this door's carve-out
after the swap — the next subsection (re-cut 2026-09-22; until then the
sentence read as if the swap kept nothing, and the code kept the *listing*
while wiping the *state*). One residual is named, not
traced: a snapshot whose *every* group seats nobody is clean at the launch
door (nothing in its bytes says whose it was) and restores wholesale —
correct for this identity's own, un-cleared for a predecessor's.

Pinned by `a_never_seen_group_is_adopted_from_a_siblings_snapshot_and_nothing_else_moves`
and its four refusals — already-holds, foreign-seat, blank-seat
(`an_evicted_group_stays_unadopted_on_this_identitys_other_device`,
`a_foreign_identity_cannot_import_a_blank_seat_group_out_of_anothers_snapshot`),
not-listed — and the two doors' agreement on one predecessor-shaped snapshot
(`the_launch_door_and_the_adoption_door_agree_on_a_predecessor_shaped_snapshot`;
all `fauna-mls::state_replica`), by
`a_siblings_group_is_adopted_mid_session_and_bound_once_its_history_lands`
(`fauna-client-mls-sync::sync` — two devices over one nest: the group is live
on the device that never saw the Welcome, the peer's next message decrypts
there, the slice binds when it lands, a steady-state sweep costs the probe and
never the blob), and by the nest's
`a_hash_only_probe_answers_with_the_digest_and_no_blob`.

**A subtlety this door names, settled by the next subsection.** A snapshot
carries the sibling's key packages, so two online devices can both hold the
init key a Welcome addresses and both process it. That predates this door and
is independent of it; who may consume a Welcome is the ruling below — whose
answer is that the double consumption is one transition applied twice, not a
collision.

### A group the engine holds but the snapshot does not list survives the swap (ratified 2026-09-22)

**The gap.** A Welcome join lands in the native store the instant it
happens (`persist_group`), but the replica learned of it only from the
1.5 s autosave. Quit inside that window and the next launch reloaded the
group from SQLite, then restored a snapshot sealed before the join — and
the whole-KV swap wiped the group's entries while the insert-only group
loop kept it *listed*. The next snapshot therefore named a group with no
state, the graceful hand-over (`retire()` → `save_state`) wrote that KV
back over the local copy, and one launch later `MlsGroup::load` answered
nothing: the join was gone from every store the account has, while the
peer kept posting into a channel this account could no longer decrypt.
There is no re-Welcome for a member already in the group (the init key is
spent), so the loss was permanent. The same shape reached the own-leaf resync, which restores
through the same door.

**The rule, at the one door both callers share.** `restore_from_provider_storage`
— reached only through `ProviderReplica::restore_into`, from the launch
and from `MlsStateSync::resync_provider` alike — takes the snapshot
**wholesale** (all-or-nothing over the snapshot stands, globals included)
and then **re-adopts every group the engine holds that the snapshot does
not list, iff it was joined since this device last saw the replica's listing
and it positively seats this identity** (the first condition is the next
paragraph's). Each is examined
*before* the swap over a scratch copy of the engine's own pre-swap bytes,
exactly as the adoption door examines a sibling's snapshot: the entry set
is what openMLS's own delete removes (plus fauna's own `fauna:<name>:<channel>`
markers, which the delete never sees and which are the group's state all
the same), and the seat is read off the loaded group's own leaf. A group
that seats this identity is carried across — its entries re-inserted over
the swapped-in snapshot, the group reloaded from them — and this is the
carve-out shape, not a splice: one coherent view of one group, loaded by
openMLS as a unit, into a KV that holds no entry of it. A local-only group
that is **refused** — foreign seat (rule (1), warned), blank seat (an
eviction that already landed, logged below `warn`), will not load, or
attributes no entry — is **dropped from the map and its native
`active_groups` row swept**, so the map never again names a group the KV
holds no byte of; and `MlsEngine::new` sweeps a row whose group has no
state the same way instead of warning over it at every open. The seating
question is asked positively, as the per-group door asks it, for the
per-group door's reason: a per-group decision has no sibling group to
answer for a blank seat. On the **seating** question, keeping a local-only
group is never a wider trust decision than the refusal arm already makes (a
refused snapshot keeps *every* local group); asking anyway keeps the two
carve-out doors on one rule. The seating question says nothing about whether
the account still wants the group. That is the next paragraph's question.

**A sibling's deletion is not a join.** A group can be local-only for two
opposite reasons. This device joined it after the replica last listed its
state, and the join must survive. Or another device of the account deleted it
from the listing (a folder leave: `forget_group` plus the nest roster
self-drop, which writes no MLS commit, so this device's copy still seats the
account), and carrying it would undo the leave. Carrying it resurrects the
group: this device's next flush reads it as its own addition and lists it
again, and the leaver's next launch holds the folder it left. The swap
therefore asks the durable merge's own three-way question, with **this
device's last-seen listing** as the ancestor. That listing is the set of
groups named by a replica listing this engine adopted (the swap itself, and
the targeted import of a sibling's group) or authored (a flush of its own
export that landed). A local-only group that listing names was deleted by
another device: dropped, with its native row swept like a refused one. A
local-only group it does not name was joined since, and goes on to the
seating question. The listing is **per device and durable**, because the
launch door runs in a fresh process where the sync plane's in-memory merge
ancestor does not exist yet. It is an engine-owned record (`mls_state`,
`store_type` `_replica_listed` on native; in-memory on web, whose engine
holds no group at launch before the swap). The swap replaces it with the
listed groups it loaded, the targeted import adds its group, a landed flush
adds the flushed listing's groups that the engine still holds, and
`forget_group` removes its group. So a leave and then a rejoin inside one
debounce is still a join. Both doors read the same record: the launch door,
and the own-leaf resync mid-session. A landed flush is recorded by the two
save doors that hold the engine: `MlsStateSync::save_engine_provider_if_changed`
(the commit gate's crash-safety saves) and `orchestration::save_snapshot`
(the autosave, the Rule-3 persist seam, and web's save). A device whose record
predates the rule (empty) carries as before, which errs toward keeping a group,
never toward losing a join.

**The producer persists too.** Every Welcome join — chat, scheduling,
folder — now drives the `ProviderPersist` seam after the join (Rule 3 on a
spent init key: the joined state exists nowhere but this engine until the
replica lists it), best-effort like the slice persist, Rule-2-ordered by
the seam itself. This is what closes the web engine's case, which has no
native store for the door to carry from; on native the door alone would
do. `bootstrap_group` needs neither: its first send's takeover CAS-puts
the provider. **A consequence for the adoption detector:** with the join's
put landing before a sibling's next autosave, that sibling's merged flush
folds the joined group into the tip it writes — and a tip a device's own
merged flush wrote is *not* a tip its engine state wrote, so the digest
the detector compares against is left unset after any merged write; the
next sweep fetches the one blob and adopts what the merge carried (before,
the sibling that joined and then went quiet reached this device only at
its next launch).

Pinned by `a_group_the_snapshot_does_not_list_survives_the_swap_and_the_next_relaunch`
(native engine over a temp db, three launches, the peer's next message
decrypting after each), `a_local_only_group_that_does_not_seat_this_identity_is_dropped_not_listed`,
`a_local_only_group_seated_under_another_identity_is_dropped_by_the_swap`
(all `fauna-mls::state_replica`), `a_stale_active_groups_row_is_swept_at_open`
(`fauna-mls::engine`), `a_welcome_join_persists_the_provider_before_returning`
(`fauna-client-mls-sync::orchestration` — a second device launched the
instant the join returned holds the group), and
`folder_and_scheduling_welcome_joins_persist_the_provider`
(`fauna-conversations`); the detector consequence by the non-holder's
flush in `a_device_launched_before_the_mint_is_not_addressed_and_the_push_arm_says_so_below_warn`.
The deletion half is pinned red-first by
`a_folder_left_on_one_device_stays_left_across_a_siblings_launch` and
`…_across_a_siblings_resync` (`fauna-client-mls-sync::sync`: two engines of
one identity over one nest, one leaves, the other's launch and resync drop the
group, its next flush does not list it, and a join since its last flush is
still carried), and the record's durability across a real relaunch by
`a_group_a_sibling_deleted_is_dropped_at_the_next_launch_but_a_join_is_carried`
(`fauna-mls::state_replica`, native engine over a temp db).

### Who may consume a Welcome — any device that holds its key (ratified 2026-09-01)

**The question.** The nest fans a Welcome push to every live connection of the
recipient and enqueues one durable inbox row for the drain
([`../architecture/api-layers.md`](../architecture/api-layers.md) § Inbox &
Messaging, layer 3); the `provider` replica carries the account's key-package
pool to every device, private init keys included, and a mint is durable there
*before* its package is published (*save-before-publish*, § Implementation
status today). So every device launched after a mint can process every Welcome
addressed to that package, and two open apps routinely both do: the push arm
never acks, and the drain peeks before it acks, so a sibling's drain can
re-join a group this device joined from the push. The row that named
this read the outcome as two engines each believing
they are the group's single leaf — the two-writer violation the
device-owned-epoch invariant forbids, reported on the loser's flush — and
weighed three ways to
make consumption exclusive: the minting device owns its packages' Welcomes;
the drain's single-consumer ack is the only consumer; a nest-side claim on the
row.

**The measurement, which refutes the premise.** A join is a deterministic
transition over its inputs — the Welcome, the package's init key, its leaf
keys — and every one of those is the same bytes on every device that holds the
package. Two engines of one identity, the second restored from the first's
flush, that both process one Welcome land on **byte-identical provider
exports**: the same keys, the same values, the same group list; the consumed
package is gone from both (neither can join the same Welcome twice); and the
three-way merge the second flush runs against the first's landed replica
reports **no** conflicted key — the conflict ruling's own *"two writers that
applied the same transition write the same bytes"* case, not its violation.
Both then decrypt the peer's next message. The last-resort package, which the
nest never deletes, is consumed by every Welcome addressed to it, on every
device, with the same result. Pinned by
`two_online_devices_both_consume_one_welcome_as_one_identical_leaf` and
`two_online_devices_both_consume_every_welcome_addressed_to_the_last_resort_package`
(`fauna-mls::state_replica`), and at the production assembly by
`two_online_devices_both_consume_one_welcome_and_neither_flush_reports_a_conflict`
(`fauna-client-mls-sync::orchestration` — two devices through the push arm's
`ingest_welcome`, a third through the drain's after missing the push, every
flush silent at the log ring's filter).

**The ruling: any device that holds the key may consume a Welcome, on either
arm, with no coordination.** A double consumption is one transition applied
twice, and the merge folds it as such. The two engines are not two leaves in
disagreement; they are one leaf in one state — exactly what a launch restore
or the targeted import above produces by another door — and what governs them
from then on is unchanged: the device-owned-epoch invariant, which is about
*advancing* the leaf, not holding it. All three exclusivity candidates are
rejected on one ground: each adds a coordination point — a wire change and a
nest arbitrating what devices arbitrate; a round trip on every join; or a
Welcome stranded until its minting device returns — to prevent a collision
that does not occur.

**What it settles for the device that did not process the push.** Offline at
push time, it drains the row later: holding the key, it joins to the same
bytes; if a sibling's drain acked the row first, the targeted import above
adopts the group at its next sweep. Launched *before* the mint (its in-memory
KV predates the key), it fails the join on both arms — expected, not a fault:
its drain leaves the row un-acked (a skip, never an ack), and the targeted
import delivers the group once the sibling's flush lands. A Welcome is
stranded only if *no* device holds its key, which no device launched after the
mint's flush can be — and that flush precedes the publish. The receive sweep's
order — durable drain, then the adopter, then the poll — is therefore fixed by
cost, not correctness: a device that can join, joins, and the adopter then
finds the group held; the reverse would make the delivery guarantee wait on a
probe of the sibling's flush for no different result.

**What the measurement found beside the question.** Folding the same stream
at *different paces* is not the same-bytes case: the receiver ratchet lives
under one provider key per group, so two devices that flush out of step change
that key to different values with no commit, no epoch change and no leaf
advanced — the steady state every two-device account lives in whatever door
brought the group, which the double consumption creates none of. It was a
finding against the conflict ruling's premise above rather than this one's,
and that ruling now classifies it as same-leaf progress.

**What the non-holder reports.** Its failure is classified apart from a genuine
ingest fault, and by the engine rather than by the reporting line's guesswork:
before staging a Welcome, the engine asks its *own* provider storage whether it
holds a key package under any ref the Welcome's secrets address, and a device
holding none gets a typed verdict — *not addressed to a package this device
holds* — never a parse of the dependency's error text. The seam carries that
verdict unchanged, and the receive loop's push arm reports it at `info`, off the
`error` line a genuine ingest fault produces and out of the ring the user reads;
the row stays un-acked and the targeted import above still delivers the group.
Without it, a two-device account whose second app stays open while the first
replenishes the pool logged an `error` for *every* Welcome minted since that app
launched. A device that already joined through a one-time package reports the
same way after that package is consumed — the same verdict for the same reason,
since re-delivery cannot change either device's answer.

At-rest shape + the CAS merge rules for these blobs: [`file-sync.md`](file-sync.md) § MLS state
replica.

**Status: design ratified 2026-07-05; the full plane is LANDED on shared Rust + linux + web + the native FFI factory + tui.** Slice ledger (mechanism prose above; one proof each):

| Slice | What | Status / proof |
|---|---|---|
| 1 | Replica core — `fauna-mls::state_replica` (`ProviderReplica` capture/restore + three-way merge), `fauna-conversations::store::history` (`ChannelHistorySlice` + commutative merge) | LANDED, tier_1 |
| 2a | The `fauna.mls.{get,put}` plane — `ReplicaBase` CAS, nest `__mls` raw-opaque storage, `fauna.mls.conflict` | LANDED, tier_3 (`tests/api/test_mls_replica_sync.py`) |
| 2b | The `channel.send` commit gate — `expect_no_commit_since` → `fauna.conversations.channel.stale`, backed by the monotonic per-channel commit high-water mark (`channel_commit_watermark`, tombstone/compaction-safe) | LANDED, tier_3 |
| 3 | Commit-rebase primitives (`self_update`, the staged non-merging `add/remove_member_staged` twins, `merge_pending_commit`/`clear_pending_commit`) + the gated `ConversationsRpc::channel_send` seam (`ConvRpcError::StaleCommit`) | LANDED, tier_1 |
| 4a | `fauna-client-mls-sync` — `MlsReplicaClient` CAS merge-retry + `MlsStateSync` (launch gate; owns the per-channel processed-seq cursor); `MAX_MLS_REPLICA_BYTES` + `Zeroizing` at the unseal boundary (security findings) | LANDED, tier_1 + tier_3 |
| 4b | The `send_commit_gated` rebase loop (stage → CAS-put → gate-send → merge-on-accept / clear+rebase on stale) + the `CommitGate` seam + the production `FaunaCommitGate` (dyn-erased `MlsReplicaTransport`; `BackendCatchUp` over `poll_inbound_conv`, `Weak`-broken cycle) + `ensure_epoch_takeover` (§3c) | LANDED, tier_1 over the full production assembly |
| 4c | Own-leaf-foreign-commit resync — typed `MlsError::{OwnLeafCommit,PastEpochCommit}` classification; `resync_channel` refetches the provider replica and merges the restored pending only on an epoch + commit-identity match (malicious-nest-rollback hardened; a no-stamp replica is never merged — its epoch-only equality was retired 2026-09-24 by the compat-remnant sweep, [`compat-remnant-sweep.md`](../architecture/compat-remnant-sweep.md) § Program 4) | LANDED, tier_1 |
| 5 | Backend-owned shared seams — the per-channel serialization lock + the `ChannelCursor` seam — plus the LINUX leg (login-time restore-before-first-poll + gate/cursor injection + debounced autosave) | LANDED; GUI two-instance tier_3 (`test_fauna_mls_cross_device_sync.py` — a fresh same-identity device reads its OWN posted message) |
| 5-web | The WEB leg — `WsMlsReplicaTransport` + `restoreMlsState`/`saveMlsState`; a page reload is the second device (in-memory engine; the nest replica IS its persistence) | LANDED; tier_3 (`test_fauna_mls_web_cross_device_sync.py`) |
| 6 | Web single-tab guard retired by deleting the plane it guarded (the legacy standalone `mls_init_engine` key-package path — a live unjoinable-group defect); the ONE conversations engine mints key packages on every app | DONE; two concurrent tabs converge as two devices (`test_fauna_mls_web_concurrent_tabs`) |
| native | The shared FFI wiring — a crypto-free `MlsSyncLauncher` seam set once by the `fauna-ffi` `conversations_session` factory; `start_receive_loop` runs it before the first poll. One orchestration (`fauna-client-mls-sync::orchestration`), six triggers — and since 2026-07-17 the tokio trigger itself is shared (`fauna-client-mls-sync::launcher::tokio_launcher`, lifted out of `fauna-ffi` with a required `PostRestoreHook` argument that breaks the `fauna-client-folders` dependency cycle; the launch-time removal resume is the one shared hook body `fauna_client_folders::FolderRemovalResume`, which linux drives from its own glib trigger too) | LANDED (no per-app glue, no binding regen) |
| 5-apple | The APPLE leg — same shared FFI wiring, proven through two real macOS GUI apps (device B: pinned fresh `home` → `CFFIXED_USER_HOME`-isolated empty `conv-mls.db`); iOS shares the identical FaunaKit + FFI-factory path | LANDED; GUI two-instance tier_3 (`test_fauna_mls_cross_device_sync.py --client macos`) |
| 5-windows | The WINDOWS leg — same shared FFI wiring, proven through two real WinUI GUI apps (device B: pinned fresh `data_dir` → `FAUNA_E2E_DATA_DIR`-isolated empty `mls.db`; `BackupPaths.DataDir`). No app-side glue was owed: the `set_state` login's `BuildE2eConvSessionAsync` → `StartReceiveLoop` already inherits the `MlsSyncLauncher` restore ordering from the `native` row | LANDED; GUI two-instance tier_3 (`test_fauna_mls_cross_device_sync.py --client windows`) |
| 5-tui | The TUI leg (tui reached its parity milestone 2026-07-19; this row predates that flip and already treated tui as a full peer of the other 6 apps): `conv_backend.rs` consumes the shared tokio launcher directly at login (one tokio runtime, no FFI factory) with the shared `FolderRemovalResume` hook | LANDED 2026-07-17; GUI two-instance tier_3 (`test_fauna_mls_cross_device_sync.py --client tui` — device B restores alice's own message from an empty `mls_state.db`; probe-verified red without the injection) |
| remaining | Per-app GUI two-instance proof (android) | OPEN — genuinely per-machine (host emulator). Android's real-conversations launch gate is now PARTIALLY built: `FAUNA_E2E_REAL_CONVERSATIONS` reaches the app and registers the real session, compile + Robolectric-verified — mirrors the apple shape (poll nest-side, no client query command). Still missing: runtime proof on real hardware/the host emulator (never run), `FAUNA_CONV_POLL_SECS` doesn't reach the app (no subprocess-env launch path), and the store-isolation crux is untouched |

The multi-leaf re-invitation *alternative* (`libs/fauna-mls/src/recovery.rs::prepare_reinvitations`) was **deleted 2026-07-12** — superseded by the single-leaf state-replica sync above. It was a caller-less `pub mod` whose two functions drove `MlsEngine::{add_member, remove_member}` — the optimistic primitives [Rule 1](#durability-rules) forbids on any epoch-advancing path — across *every* group the device belongs to, so it stood as a loaded gun that several docs had to keep warning sessions away from. Nothing may be re-derived from it: a future device-recovery feature rides the `CommitGate`, like every other epoch-advancing path. The gap holds uniformly on every nest — there is no storage mode ([`storage-modes.md`](../architecture/nest/storage-modes.md)).

The **legacy standalone `mls_init_engine` singleton plane** that slice 6 retired on web is **deleted outright, 2026-07-22** — the file `libs/fauna-ffi/src/mls.rs`, its nine `#[uniffi::export]`s, `libs/fauna-ffi/tests/mls_engine_lifecycle.rs`, and apple's `FFICompat.swift` pass-through wrappers plus their one test are all gone. The path there: apple's macOS `MlsManager` — the plane's last production caller anywhere — was deleted 2026-07-19 (iOS never wired it; windows/android/tui/linux never used it, each already on the ONE conversations engine — a per-session `MlsEngine` built by the `ConversationsSession` factory, `libs/fauna-ffi/src/nest_client.rs` `build_conversations_session`), leaving a caller-less plane referenced only by its own tests. It was never the web-class *active* defect on apple (the unjoinable-key-package mint path had already been removed from the FFI) — it opened an idle second engine on its own `mls.db` and drove no crypto. The "last caller anywhere" claim was re-verified against current code immediately before the delete: the only remaining references across `apps/**` in any language were apple's own wrappers, and the windows/web mentions were comments explicitly recording that neither calls it. Nothing may re-derive a standalone-engine path: every app's MLS is the conversations rail's per-session engine.

## Offline compose — the plaintext-intent contract (resolved 2026-08-10, refutable until W4 (account-data-plane.md § Workstreams) code)

Offline compose for MLS channels queues **plaintext intents, encrypted at
send**. The outbox itself, the offline classification, and the drain role
belong to
[`../architecture/account-offline-mutation.md`](../architecture/account-offline-mutation.md)
§ The offline-mutation contract; this section owns the MLS-specific detail
(it resolves that contract's T6). Never pre-built ciphertext: sealing at
compose time would consume a sender-ratchet generation and bind an epoch
the send may never happen in — a stale-epoch send is exactly what the
device-owned-epoch invariant forbids, and a merged-but-unsent state is the
stranding shape Rule 1 exists to prevent.

- **The intent is the application payload, not an MLS message.** An outbox
  row: a client-generated intent id (the idempotency key), the channel id,
  the composed body exactly as `create_message` would seal it, attachment
  references into locally staged bytes, a compose stamp, and a per-channel
  compose sequence. It rests plaintext in the reading replica (the
  account-data plane's R3 (account-data-plane.md § The ratified decisions) posture — the same OS-user boundary as the thread
  store's own-message plaintext) and it is **replica-local**: the outbox
  never syncs; an intent drains only from the device that composed it
  (whether custody grants ever carry outbox intents is the charter's T13,
  not this section).
- **Drain = the ordinary send path, from an MLS-hosting process.** On
  reconnect, intents drain per channel in compose order: catch up the
  channel log (a stalled walk blocks the drain exactly as it blocks a gated
  send); take epoch ownership if needed (the self-`Update` takeover above);
  seal + upload attachments under the send epoch; send the application
  message sealed at the **then-current** epoch. Recipients are the
  channel's membership at send time — the ordinary semantics of a message
  that arrives late, not a leak; there is no per-member pinning at compose
  time. Because draining needs the MLS read-side state, **MLS intents
  drain only from a process hosting the account's conversations engine —
  an app, never the bearer-only sync agent** (its credential model
  deliberately excludes MLS state); the plane's engine-singleton carve-out
  for this is stated at its owner (account-data-plane.md § Multi-instance
  concurrency).
- **Idempotency.** The intent id rides `fauna.conversations.channel.send`
  into the nest's durable idempotency table (account-data-plane.md
  § Nest-side requirements): a replayed drain — a crash between the nest's
  accept and the outbox deletion — gets the recorded ack, not a duplicate
  message. The sender-ratchet generation consumed by a deduplicated
  re-seal is discarded harmlessly; receivers already tolerate skipped
  generations.
- **FIFO per channel, loud on permanent failure.** A retryable failure
  backs off on the outbox's ladder (the `transfer_queue` discipline,
  generalized); a permanent one — removed from the group, channel deleted —
  parks the channel's remaining intents in a user-visible failed-sends
  state, resurfaced as drafts. Never silently dropped, never reordered
  around a failure.
- **Scope: application messages only.** Membership and epoch operations
  (add/remove, any gated commit) stay **online-only** in the offline
  classification: they are precondition-gated races against the live log
  (`expect_no_commit_since`), and a deferred membership change that acts
  hours later is a security surprise, not a convenience.

## User Experience

The multi-device flow has two sides: a **primary device** (already registered) and a **new device** joining an existing account.

1. User has an account on Device A (primary).
2. User installs fauna on Device B.
3. On Device B, user selects "Sign In" and pastes the same 64-character hex secret key they backed up during onboarding.
4. Device B derives the same `ActorId` (Ed25519 public key) from the secret, then authenticates with the nest.
5. Optionally, Device A signs a `DeviceAuthorization` restricting what Device B can do (capability restriction).
6. Device B registers for sync (`fauna.sync.register`), publishes fresh key packages, and restores the user's conversations from the `__mls` state replica (§ Cross-device MLS group-state sync). It can now sync files and read + send in existing conversations.
7. The user can list all devices and sessions, revoke individual sessions, or perform an emergency lockout.

A **guardian-enrolled device** on a supervised account rides this same flow to *add* — but the nest refuses the supervised account's own attempt to remove it while the guardian's mark is active (the guardian un-marks it first, or graduation auto-revokes the mark); MLS removal then proceeds exactly as in § Removing a Device. Owner: [`family-safety.md`](family-safety.md) (ratified; the marker and the removal refusal are nest-side and app-independent — see its § Implementation status today for the ward-facing badge's per-app build state).

## Identity Model

Every actor has exactly **one** root Ed25519 keypair — the `ActorKeypair`. The 32-byte secret key is the same on every device; it is imported manually (no server-assisted transfer). The server only ever stores the public key (`ActorId`).

For capability-restricted secondary devices, a separate **device-level keypair** can be used. The primary device signs a `DeviceAuthorization` with the root key to grant a specific device a limited set of capabilities.

### DeviceAuthorization

Defined in `libs/fauna-core/src/data.rs`:

```rust
pub struct DeviceAuthorization {
    pub actor_id: ActorId,
    pub device_key: [u8; 32],
    pub capabilities: Vec<Capability>,
    pub created_at: Timestamp,
    pub expires_at: Option<Timestamp>,
}

pub enum Capability {
    Post,
    Follow,
    React,
    UpdateProfile,
    ManageSubscribers,
    All,
    RenewBearer,
}
```

The authorization is signed via the sign-over-CID `SignedEnvelope` shape
(`docs/goal/architecture/serialization.md`); the envelope ships alongside
the canonical bytes in the embed-as-bytes wire shape and is verified by the
nest before storing. This is distinct from the sync-level `capabilities`
field (a comma-separated string of `read` / `write`) on the device
registration record.

**Consumers of this model:** the subscription secondary-device delegation
(§ Step 3 below); — ratified 2026-07-19 — the **per-user sync agent's
renewal grant**: a `[RenewBearer]`-scoped authorization over a fresh
device-level keypair, stored on the agent's `sync_devices` row and exchanged
for ordinary session bearers app-dead. `RenewBearer` grants exactly that
exchange and nothing else (a `RenewBearer`-only authorization conveys no
Post/Follow/content capability). Mechanism, kinds, and revocation path →
[`../architecture/apps/sync-agent-credentials.md`](../architecture/apps/sync-agent-credentials.md)
§ Credential model (the owner of that flow); and — ratified 2026-07-23, with
the `Post`/`Tombstone`/`Profile` signing paths and the client authorize/revoke
control since built (atproto-pds-full.md § Implementation status today is
current) — the **server-held authoring sub-key**: the user's home nest
may hold a per-account, capability-restricted authoring keypair whose
`DeviceAuthorization` (identity-signed by the user's client, `[Post,
UpdateProfile]`-scoped, revocable) makes it a valid delegated signer of
`Post`/`Tombstone`/`Profile` envelopes — the "secondary device" here is the
nest itself, authoring on the user's behalf for protocol surfaces where no
client identity key is present (external ATProto apps posting through the
full PDS). The chain verify is the same author/signer/cert/capability shape
the broadcast-KeyBlob mint ships (`verify_key_blob_signature`); the wire
carriage and verification recipe →
[`../architecture/serialization.md`](../architecture/serialization.md)
§ Sign-over-CID; instantiation (key custody, mint ceremony, revocation
cascades) → [`atproto-pds-full.md`](atproto-pds-full.md) D10 (the owner of
that flow).

### Device-signed authoring — the acceptance rule (resolved 2026-08-10, refutable until W4 code)

The server-held authoring sub-key above was never nest-special: it is one
instantiation of a general rule, ratified here as the extension the
account-data plane's seedless surfaces need (it resolves
[`../architecture/account-replica-posture.md`](../architecture/account-replica-posture.md)
§ The store device principal's T7 — the named prerequisite before any
authoring surface holds only a device principal).

- **Acceptance rule — every verifier, one chain.** A content envelope whose
  signer is a device key (not the ActorId) verifies iff: **(1)** a
  `DeviceAuthorization` covering that device key, root-signed by the
  ActorId, accompanies it — the same author → signer → cert → capability
  chain the broadcast-KeyBlob mint and the D10 sub-key already verify
  (`verify_key_blob_signature` shape, sign-over-CID carriage); **(2)** the
  authorization's `capabilities` cover the authored kind per the mapping
  below; **(3)** it is unexpired, and unrevoked as far as the verifying
  surface can know (revocation authority below). Deny by default: a kind
  with no mapped capability requires the root key.
- **Capability→kind mapping (deny-by-default; extend by editing this
  list, never by inference):** `Post` → Post + Tombstone of own content;
  `UpdateProfile` → Profile; `React` → reactions; `Follow` →
  follows/subscribes; `ManageSubscribers` → subscriber management;
  `All` → every device-eligible kind;
  `SyncWrite` → file-sync change records (`fauna.sync.changes.record` and its
  federation relay — ruled 2026-09-27, unbuilt; the design and the reader
  policy are
  [`../architecture/writer-signed-change-records.md`](../architecture/writer-signed-change-records.md)
  § Writer-signed change records; a
  `[RenewBearer]`-only grant still authors nothing, and every verifier
  decodes the capability list open-set so a later variant is never a decode
  failure). **Never device-eligible** (root key
  only, whatever the grant says): identity succession, recovery-kit
  operations, minting or revoking a `DeviceAuthorization` (no delegation
  chains — a device cannot authorize a device), account deletion.
  `RenewBearer` conveys authoring of nothing (unchanged). MLS channel
  traffic is out of scope — its sender authentication is the MLS leaf
  credential, not this chain; likewise the account-data plane's per-writer
  journal rows, whose writer authentication is the sync plane's admission
  witness (charter § The peer leg) and, for the sealed class-2 form, its
  T14.
- **Carriage.** Within the home nest, by reference: the registered device
  row is the authorization. Across a trust boundary — federation receive,
  cross-account peer serving — inline: the authorization envelope travels
  with the content envelope so the chain verifies self-contained.
- **Revocation authority is the distinguished replica.** Revocation stays
  device-row deletion; the home nest enforces it live at ingest, so a
  post-revocation authoring attempt fails there. Content accepted before
  revocation stays valid (authored-while-authorized — no retroactive
  invalidation pass). A cross-boundary verifier holding an inline cert
  enforces expiry itself but learns revocation only through the home nest —
  acceptable because every cross-boundary flow already routes through the
  distinguished replica (account-data-plane.md § The nest decomposed,
  role 3); a future direct peer-ingest surface must re-visit this bullet
  before shipping. **Re-visited 2026-09-19 for the storage-group plane**,
  the first surface that accepts authoring chains peer-wise with no home
  nest on any path: there revocation is learned from the plane itself, and
  — having no replica to order ingest — is total rather than
  authored-while-authorized. Owner:
  [`../architecture/recipient-set-scheme.md`](../architecture/recipient-set-scheme.md)
  § The recipient-set scheme → *Severance, per axis* (*An authority
  device's removal*). The re-visit stays owed to any OTHER such surface.

## Adding a Second Device

### Step 1 — Import secret key

The user pastes the 64-character hex secret key. The client decodes it to 32 bytes and derives the `ActorId` locally. No network call is needed for this step.

### Step 2 — Authenticate

**`fauna.auth.handshake`** (WS-RPC; its `POST /api/v1/auth/token` HTTP twin was
deleted at the rip-out endgame) — identical to first-device login.

```json
{
  "actor_id": "<hex>",
  "timestamp": 1711612800000,
  "signature": "<hex>"
}
```

Signature: `Ed25519_sign(secret_key, actor_id_bytes || timestamp_be_bytes)`. Returns a Bearer token with a 1-hour TTL, plus the session's short `token_id` (the client stores it to name its own session in `fauna.sessions.{revoke,revoke_all}`). Migrated to WS-RPC as `fauna.auth.handshake` (the challenge-response variant is `fauna.auth.verify`); both replies carry `token` + `token_id` + `expires_at`.

### Step 3 — Device authorization (optional)

If the primary device wants to restrict what the new device can do:

1. Primary constructs and signs a `DeviceAuthorization` with the root key.
2. Primary uploads it over WS-RPC: **`fauna.subscriptions.delegate.upload`** (the HTTP write twin is deleted).
3. Nest verifies the Ed25519 signature, stores the authorization.
4. The authorization can be retrieved later via **`GET /api/v1/subscriptions/delegate/{author_id}`** — deliberately-kept public HTTP residue (federation bootstrap, Bucket B), not a deprecated twin.

Skipping this step gives the new device full capability — it authenticates with the same root key and is treated identically to the primary.

### Step 4 — Register for sync

**`fauna.sync.register`** (WS-RPC; the `POST /api/v1/sync/register` HTTP twin is deleted — only the file byte download survives as HTTP on the sync plane, the `/api/v1/sync/ws` upgrade having been removed 2026-10-02)

```json
{
  "actor_id": "<hex>",
  "device_id": "<hex>",
  "label": "Device B",
  "capabilities": "read,write"
}
```

`device_id` is a random 32-byte value generated per-device (not tied to the keypair). `capabilities` defaults to `"read,write"`. A read-only device can receive sync changes but cannot record them.

**The tier's device cap is enforced here** (owner of the quota: [`admin.md`](admin.md) § 2 Users — the tier *is* the quota). On a multi-tenant nest a **new** `device_id` past the caller's `max_devices` is refused with `fauna.sync.device_limit_exceeded`, a typed error the app renders so the user can ask their admin for a bigger tier; nothing is written. *Where* the app renders it, and through which shared state, is owned by [`../ui/devices.md`](../ui/devices.md) § Errors & edge cases (the Settings → Devices `error-message`, off the account runtime's recorded enrollment refusal — since 2026-09-15); the wire code's own rendering is `RpcError::localized`'s exact arm, the same sentence. Two things this deliberately does **not** refuse: a **re-register of a `device_id` the actor already holds** — the row is an upsert, so re-labelling and every re-provision keep working at the cap — and anything at all on the embedded single-user desktop nest, where tier quotas are off. The nest's own WebDAV pseudo-device does not consume a slot — but this door refuses a client-supplied `device_id` equal to that pseudo-device's derived id outright, with `fauna.sync.invalid_request`, unconditionally (not gated on the tier cap being on): the id is nest-authored, so a client naming it is malformed input, not an ordinary register.

### Step 5 — Join DeviceSyncChannel *(dormant primitive — not the current flow; see § Implementation status today)*

The `DeviceSyncChannel` is an MLS group restricted to a single actor's devices. **The shipped second-device flow does not use it** — conversations access is restored via the `__mls` state replica (§ Cross-device MLS group-state sync); this section describes the dormant primitive only.

**Primary creates or updates the channel:**

```rust
DeviceSyncChannel::create(engine, actor_id, device_key_packages)
// returns (DeviceSyncChannel, Welcome)
```

The `Welcome` message is delivered to the new device out-of-band (via the nest inbox or direct transfer).

**New device joins:**

```rust
DeviceSyncChannel::join(engine, actor_id, welcome)
```

Both devices now share an encrypted MLS group. They agree on the same deterministic routing `channel_id` and the MLS-derived `mls_channel_id`.

### Step 6 — Publish key packages

**`fauna.conversations.keypackage.upload`** — upload fresh MLS key packages so other devices can add this device to future groups. (The `POST /api/v1/keypackage/{actor_id}` HTTP twin was deleted in Spec-Y2 slice 5.)

## Device Sync Channel *(dormant primitive — not the current flow; see § Implementation status today)*

Defined in `libs/fauna-mls/src/channel.rs`:

```rust
pub struct DeviceSyncChannel {
    /// Deterministic routing ID — any device can compute this independently.
    pub channel_id: ChannelId,
    /// MLS group-derived ID — needed for encrypt/decrypt operations.
    pub mls_channel_id: ChannelId,
}
```

The routing ID is deterministic:

```
channel_id = blake3(actor_id.0 || b"fauna.devices.v1")
```

This allows any of the actor's devices to independently locate the channel without prior coordination. The underlying MLS group has its own random `group_id`; the deterministic `channel_id` is only used for routing and lookup.

The channel carries `DeviceSyncMessage` payloads (e.g., `BlobAdded`) wrapped in `ChannelMessage` envelopes. All members can encrypt and decrypt using their copy of the MLS group state.

## Session Management

Sessions are Bearer tokens. Each token is associated with the IP address used at creation time.

Session management is **WS-RPC** (`fauna.sessions.{list,revoke,revoke_all,lockout}`, gated `User | Admin`); every HTTP twin is **deleted** (the `≡ former` column below is history, not a live surface). `revoke_all` names the session to keep by **`keep_token_id`** — the WS connection drops the raw bearer after the upgrade handshake, so the client passes the `token_id` it learned at mint (see Step 2). The authed `fauna.sessions.lockout` carries no signature (the connection authenticates the actor); the no-token recovery path is the **pre-identity WS-RPC kind `fauna.account.lockout`** on the anonymous connection (its HTTP form is also deleted). Ceremony owner: [`login.md`](login.md).

| Kind | Description |
|---|---|
| `fauna.sessions.list` (≡ former `GET /api/v1/account/sessions`) | List all active (unexpired) tokens with `token_id`, `created_at`, `expires_at`, `last_used_at`, `ip_address`, and `minted_by_device` — the renewal device key when the session was minted by a device grant over `fauna.auth.device_handshake` (or by a custodian over `fauna.auth.custody_handshake`), absent for a direct sign-in. It is the same key as the roster row's `principal` (`fauna.sync.devices.list`), which is what relates this list to § Listing Devices — join on `principal`, never on `device_id` |
| `fauna.sessions.revoke` (≡ former `DELETE …/sessions/{token_id}`) | Revoke a specific session (must belong to the authenticated actor) |
| `fauna.sessions.revoke_all` (≡ former `POST …/sessions/revoke-all`) | Revoke all sessions except the one named by `keep_token_id`; returns `{ "ok": true, "revoked": N }` |
| `fauna.account.lockout` (≡ former `POST /api/v1/account/lockout`) | Emergency lockout — no Bearer token required; Ed25519 signature on the **anonymous** WS connection. The authed equivalent is the bearer kind `fauna.sessions.lockout`. |

### What a session is, and what revoking one does (designed 2026-09-19; app half built on tui 2026-09-28, other apps pending — § Implementation status today)

A session is **one bearer**: every mint — handshake, challenge/verify, device handshake, custody handshake — creates a new row with a fresh `token_id` and a one-hour life, and no mint replaces an earlier row. An always-on app therefore shows one row, two for the sixty seconds in which it has minted its successor and the predecessor has not yet expired; a native app that hosts the account runtime holds a second, independent session minted by its device grant (`minted_by_device` = its own principal); a co-located sync agent holds a third.

**Revoking a session ends that bearer — and closes every connection that was upgraded with it** (the per-token twin of the per-actor revocation teardown; built 2026-09-20 — [`../architecture/transport-connection.md`](../architecture/transport-connection.md) § Connection lifecycle → *Revocation teardown* → *The per-token twin*). **It does not sign a device out.** A device that holds the identity seed, or a live device grant, mints a new bearer by itself on its next request, because the seed *is* the account and the grant is a standing authority. So the honest meaning of the two controls is: *revoke one* cuts a token that leaked or that the owner does not recognize and forces whoever held it to prove themselves again; *revoke all others* does that to every session but the caller's. The real remedies sit elsewhere and the surface must say so where it offers the controls: a session minted by a device grant is ended for good by **removing that device** (§ Removing a Device — it tombstones the grant and sweeps its sessions); somebody who holds the seed is ended only by the **succession ceremony** ([`identity-succession.md`](identity-succession.md)). The app surface that renders this is [`../ui/sessions.md`](../ui/sessions.md).

### The client's own session (designed 2026-09-19; the holders built 2026-09-20, first consumer tui 2026-09-28)

Marking "this session" and sending `keep_token_id` both need the client to know its own `token_id`, which no mint path kept until 2026-09-20. The shape, in shared Rust and nowhere per-app: **the bearer holder retains the ids it minted.** `fauna_anon_client::MintedBearer` gains `token_id` (so `TokenCache` and the three `Ws*Bearer`s carry it for free); `fauna-launch-machine`'s `TokenRefreshOutcome::Success` / `State::Online` gain it on both the refresh and the silent-challenge arm; `BearerSource` gains a defaulted `own_token_ids()` beside `bearer_actor_id()`; wasm `challengeVerify` (the SPA's one mint since 2026-09-23) emits it and the SPA's token cache keeps it. The holder keeps a **set** — the current id plus every earlier id of its own that has not yet expired — in memory only, pruned by `expires_at`, never persisted.

Why a set rather than the one current id: bearers renew hourly (and on any 401), and the predecessor row outlives the renewal. With only the current id, the app's own previous token would paint as an unknown second session, and *sign out everywhere else* would appear to find and kill a stranger. With the set, **every own row folds into the one "this app" row**, across any number of renewals. `keep_token_id` is the **current** id read from the holder **at call time**, never from a previously painted list — a renewal between paint and press must not name a dead token. The residual race (a renewal landing between that read and the nest applying the revoke) revokes the app's own newest bearer; that is harmless by the wire's own contract — the next request re-mints.

Two bounds, stated rather than hidden. A relaunched app has forgotten its previous run's ids, so that run's last token shows as an unmarked row for up to an hour. And a session minted by *this machine's* device grant from another process (the co-located sync agent) is recognized not by token id but by `minted_by_device` equalling this machine's enrolled principal — the same read door as § This-device marker — and is marked "This device", not "This app".

**Built 2026-09-20 — the holders, not the consumers.** Every seat named above now retains its ids: the fold rule is one shared type, `fauna_protocol::auth::OwnSessionIds` (`record`/`prune`/`ids_at`/`current_at`, `now` passed in), sitting beside `BEARER_REFRESH_BUFFER_SECS` for the same reason that constant does — three holders keep this set and one pruning on its own rule would fold a different set of rows into "this app" than its siblings. `MintedBearer` carries `token_id`, so `TokenCache` and all three `Ws*Bearer`s answer for free; `TokenRefreshOutcome::Success` and `State::Online` carry it on both the refresh and the silent-challenge arm, with the set on the machine's `Inner` so it survives the `Online → Refreshing → Online` round trip; `BearerSource` gained defaulted **async** `own_token_ids()` / `current_token_id()` (async, unlike the sync `bearer_actor_id()`, because the set lives behind the holder's own lock — a sync accessor would `try_read` it and silently under-report under contention); `FfiBearerToken` and `FfiSilentSignInResult` both carry it additively, the latter because challenge/verify is the UniFFI apps' launch mint; wasm `challengeVerify` (the SPA's one mint since 2026-09-23) emits it, the web SPA keeps the set per (node, identity) in `$lib/own-session-ids.ts`, and the wasm `LaunchMachine` exposes `currentTokenId()` so web's launch-primed token — the one bearer web does not mint through `getAuthToken` — is nameable too. A 401 clears the token and deliberately **keeps** the set: the row stays listed nest-side until it expires, so dropping it would paint the app's own session as a stranger for the rest of that hour. **The first consumer landed 2026-09-28 (tui):** `fauna_client_account::SessionsClient` reads the set through `fauna_protocol::auth::OwnSessionSource` (answered natively by `fauna_client::NestClient` from its one bearer source) — folding every own id into the one "This app" row and filling `keep_token_id` from `current_token_id()` at press time — and the page is [`../ui/sessions.md`](../ui/sessions.md), whose § Implementation status today tracks the other six apps.

### Emergency lockout

No Bearer token is needed. The request is authenticated by an Ed25519 signature over `actor_id_bytes || timestamp_be_bytes` (the same construction as `fauna.auth.handshake`'s no-nonce direct auth). The timestamp must be within 300 seconds of server time.

```json
{
  "actor_id": "<hex>",
  "timestamp": 1711612800000,
  "signature": "<hex>"
}
```

**The lockout window is a hard-coded 24 hours** (`account_core::EMERGENCY_LOCKOUT_SECS`, shared by both lockout kinds — ruled 2026-08-24). Neither request carries a duration. The former `duration_secs` was never covered by the signature (which signs `actor_id ‖ timestamp_be` only), so on this bearer-less ceremony an on-path capture of a short lockout could have been re-issued as a longer one inside the ±300 s window — and no app in all history ever exposed a duration choice, making the field configuration-theatre on the wire (`principles.md` § One configuration surface). It was ignored from 2026-08-24 and left the wire 2026-09-24 with the compat-remnant sweep (`version-compatibility.md` § Dimension 2, the fourth write-off); an older client's stray key lands in `extra` and moves nothing. 24 h — the old clamp's maximum — on purpose: the panic button's job is protective containment until the owner completes a real remedy (device revocation, seed-escrow restore, succession — all reachable while locked, since the recovery ceremonies are pre-identity), and a window that silently expires overnight defeats it, while 24 h stays bounded so a false alarm cannot permanently brick the account. The nest revokes all active tokens for the actor and sets `locked_until` in the database, blocking new auth requests until the lockout expires.

### The two panic buttons — lock, and "my identity was stolen" (designed 2026-09-19)

A member can be offered two emergency controls, and they mean different things. **Lock** is *containment*: it needs only the seed (or a live session), it freezes the account for everyone — the owner included — for 24 hours, it removes nobody, and it cannot be shortened or undone. **"My identity was stolen"** ([`identity-succession.md`](identity-succession.md)) is the *remedy*: it needs the recovery kit, it moves the account to a new key, and the old key — the thief's copy included — stops working for good. Whoever presses Lock must already have been told the following, so the surface that offers it carries all five as REQUIRED copy, visible before the confirm and not behind it: (1) every device is signed out, this one too; (2) nobody can sign in for 24 hours, the owner included, and there is no unlock; (3) it does **not** remove somebody who holds the secret key — they come back when the lock lapses; (4) **they can do the same to the owner**: both the session revokes and the lock are seed-authorized, so a thief can sign the owner out and lock the account, again every 24 hours, for as long as they hold the key; (5) the one control a seed thief cannot press and the lock does not block is the recovery-kit ceremony — it is pre-identity, it lands *through* an active lock, and the successor account does not inherit the lock — so *a lock the owner did not set is itself the sign to use the kit*.

Point 5 binds the app, not only the nest (whose half is proven end to end — [`identity-succession.md`](identity-succession.md) § Implementation status today, *the pre-identity remedies are reachable*): **the stolen-identity ceremony must be reachable from a locked-out app and must ride an anonymous connection.** **Built on tui 2026-10-04:** the ceremony is one shared composition, `fauna_client_recovery::ceremony::succeed_stolen_identity`, which dials its own anonymous connection and takes no requester from the app, and tui calls it from both the locked launch surface and the signed-in Settings section (§ The locked state). Until then — measured 2026-09-19 — tui's ceremony handed `succeed_with_held_kit` the *signed-in* requester, which a lock has just torn down, and was offered only inside the signed-in Settings shell, so the documented sequence "lock first, then run the ceremony" dead-ended in the app exactly when it was needed. linux's driver and the UniFFI face (`succession_succeed_with_held_kit`) still have that shape, and no other app offers the ceremony outside its Settings shell; each closes both with its leg-2 lift.

### The locked state (designed 2026-09-19; built on tui 2026-10-04 — § Implementation status today gap 4)

`fauna.auth.account_locked` (details = `locked_until`, Unix seconds) is a refusal no retry can clear before its time, so it is handled like the superseded refusal, not like a stale bearer: **terminal until `locked_until`**. `fauna-launch-machine` carries it as an additive `LaunchSnapshot::locked_until_secs` field — not a new `LaunchPhase` variant, for the reason `superseded_successor` is a field ([`identity-succession.md`](identity-succession.md) § Implementation status today) — with the phase staying `Offline { transient: false }`, and schedules exactly one refresh at `locked_until`; `WsChallengeBearer` latches it beside `SupersededLatch` and the reconnect supervisor treats it as terminal-until-then instead of backing off and re-signing a doomed handshake. **The refresh is the machine's own:** parking the lock arms it, with no loop for an app to spawn, so it also covers a lock met at launch (before any session exists) and the four apps that never run the machine's TTL loop. It fires a few seconds past `locked_until` on the device's clock (`LOCK_LAPSE_GRACE_SECS` — the unlock time is the nest's clock, and a refresh a moment early would spend the one attempt on a refusal), re-reads the wall clock every minute so a suspended device does not overrun it, and is cancelled by any state change. **One refresh per lock:** a refresh the nest answers with the same unlock time arms nothing further, while a different unlock time is a new lock and earns its own; a transient failure lands the ordinary retryable offline. **The latch stands only until `locked_until`** (`fauna_client::LockedLatch`, on the silent-challenge mint and the store principal's device-handshake mint alike): while it stands a mint answers the refusal without dialling, and once it lapses the next mint dials and the nest's answer replaces it — the supervisor's side is [`../architecture/transport-connection.md`](../architecture/transport-connection.md) § Connection lifecycle → *A locked account holds the reconnect loop*. The UniFFI apps' silent sign-in raises it typed, `FfiError::AccountLocked { locked_until_secs }`. The app paints a standing, localized notice — the unlock time through the shared `format_unix_local`, and the sentence that a lock the owner did not set means somebody holds the secret key — with one action: the stolen-identity ceremony, run over an anonymous connection with the seed this device still holds as `old_identity`, adopting the successor through the existing add-account switch. A device that locks the account itself lands on the same surface. This needs the refusal on **every** launch path — ruled 2026-09-25: `fauna.auth.verify` enforces the lock like the handshake ([`login.md`](login.md) § Silent Challenge); the build status is § Implementation status today gap 3. Page contract: [`../ui/sessions.md`](../ui/sessions.md) § Layout & flow.

### The signed-out door (designed 2026-09-19; unbuilt)

The pre-identity kind earns an app surface for one reason: **locking the account from a device the owner does not want to sign in on.** An owner whose only device was taken has their seed backup and a borrowed machine (or any browser at their nest's web app); importing the identity there would persist the seed on someone else's device and pull the account down onto it. The door — an entry on the onboarding identity choice — takes the secret and the account's handle, shows the same REQUIRED copy, signs `fauna.account.lockout` over the anonymous connection, and **persists nothing**. On a device that is signed in, the control uses the bearer kind; the two kinds share one window and one effect. Page contract: [`../ui/sessions.md`](../ui/sessions.md) § Layout & flow.

## Listing Devices

**`fauna.sync.devices.list`** (WS-RPC; the `GET /api/v1/sync/devices` HTTP twin is deleted)

Returns all registered devices for the authenticated actor:

```json
{
  "devices": [
    {
      "device_id": "<hex>",
      "label": "Device A",
      "capabilities": "read,write",
      "registered_at": 1711612800000,
      "last_seen_at": 1711699200000,
      "online": true,
      "folders": [
        { "name": "photos", "flags": { "originates": true, "accepts": false, "applies_deletes": false } }
      ]
    }
  ]
}
```

`online` reflects whether the device currently has an active WS-RPC connection to the nest bound to the device (the binding: next paragraph; built 2026-09-22) — the only kind of socket a device holds since the `/sync/ws` data plane was removed (2026-10-02). This is a *device's* liveness; the Media page's `media-source-status` dot is a *folder's* content reachability, a different verdict owned by [`file-sync.md`](file-sync.md) § Content reachability.

**The binding (ratified and built 2026-09-22).** A WS-RPC connection is bound to a device when its bearer was minted over `fauna.auth.device_handshake` by the key that device's roster row carries as its granted principal (`sync_devices.auth_device_key`, the wire's `principal`). The mint verified possession of that key, so the binding is nest-verified with no wire change and no client-asserted device id. The nest captures the key on the connection at the upgrade, beside `token_id` (`RpcConnection::bound_device_key`; [`../architecture/transport-connection.md`](../architecture/transport-connection.md) § Connection lifecycle), because the token row is never re-read and a socket outlives its bearer's hour. The roster answers `online = a live connection of this actor bound to the row's principal` (`WsState::has_connection_bound_to`, asked inside the actor's own entry, because a device id is client-chosen and unique only within an actor). What binds nothing: a seed-minted bearer (handshake, challenge/verify — an identity, not a device), the custody handshake's tag (the custodian's own actor id, excluded at the upgrade), and a connection of another actor (the join runs inside the actor's own entry). So a native app's *own* session makes no row online; its row is online through the account runtime's principal client, minted by the machine's store writer key on the row the enrollment latched on — the same for a co-located sync agent. (The legacy headless daemon, which stayed online through the `/sync/ws` data plane, was removed 2026-10-02, and that data plane with it.) Web hosts no account runtime and registers no row. A row with no principal (never enrolled, or its grant retired) reads offline whatever socket lingers: the verdict follows the row, and closing a retired key's socket is device removal's duty (`transport-connection.md` § *Revocation teardown*), not this read's.

**`last_seen_at`** is the last moment a WS-RPC connection bound to the device registered with the nest, or the row's registration when none has — a connection-time mark, never a heartbeat. The bound upgrade touches it (`touch_device_last_seen_by_principal`), actor-scoped because a device id is unique only within an actor.

Two additive fields not shown above: `guardian_marked` (bool; see § Removing a Device's
guardian-mark check — owner [`family-safety.md`](family-safety.md)) and an opaque
`label_sealed` byte blob carried alongside the plaintext `label` (owner: [`path-sealing.md`](path-sealing.md)).

### This-device marker (client-side, no wire change)

The roster above is **identical regardless of which client asks** — nothing in
`fauna.sync.devices.list`'s response says which row belongs to the asking app. So each app marks
the roster row it **enrolled on** as **"This device"** — a pure client-side string comparison
against a value the app already holds, no nest change, no new field.

⚠ **The marked value is the row the app ENROLLED on, which is not always its own locally-generated
`device_id`.** Each app does hold such an id (storage is per-platform — a SQLite `device_identity`
table for tui/linux, `localStorage` for web, `EncryptedSharedPreferences` for android, each
converging on the same wire shape; how the value is minted — derived per account from an
install-scoped secret — is owned by `sync-agent-credentials.md` § Credential model's 2026-09-20
ruling), and where that id *is* the enrolled row the comparison is
exactly it. But which row an enrollment targets is owned by
[`../architecture/apps/sync-agent-credentials.md`](../architecture/apps/sync-agent-credentials.md) § Credential model
(the RULED 2026-08-15, row 47 block, decision 2), and it makes a **co-located sync agent's**
advertised id win over the app's own whenever there is one — precisely so two apps on one box
converge onto one row instead of two. The badge must follow that convergence: marking the app's
own id there marks a row the app did not enroll on. This section states *what* is compared; it
does not restate how the target is chosen.

**How the app knows it.** Not by re-running the target gate at paint time — by reading the
enrollment's own record. `AccountStoreHandle::enrolled_device_row` reports the row half of the
principal slot's registration latch: the row this machine's grant actually registered on, a
fact about the past rather than a re-derivation. `fauna_devices_machine::this_device_row` owns
the pairing rule — **the enrolled row wins; the app's own id is the fallback** — and every app
that paints the badge resolves through it. The fallback is a rule, not a formality: decision
2's agent-less-platform case enrolls under the app's own id, and the latch answers `None`
before the ceremony's nest legs have first succeeded and
again after a re-mint, so an app that unconditionally preferred a live probe would blank the
badge exactly where it is correct today.

**Why this exists** (`family-safety.md` § Full visibility): a supervised account's
ward could register a decoy device whose displayed row was byte-identical to their guardian's
enrolled one, so the guardian marked the wrong row. The nest-side fix widened the guardian's own
row's displayed `device_display_identity` (code) so two rows can never render identically
([`value-formatting.md`](value-formatting.md) § Device display identity) — correct, but it still
asks a human to compare hex by eye, and the code widens exactly when two devices are similar. The
this-device marker removes the comparison entirely for the common case (a guardian checking from
the device they themselves enrolled): the check becomes "is the This-device tag on the row I
meant to keep," with nothing to compare.

**Rendering.** `device-this-mark-badge` (ui.yaml `devices` page, `optional_elements`, indexed —
user-approved 2026-08-11) on the `device-card` row whose `device_id` equals the enrolled id. Not
mutually exclusive with `device-guardian-mark-badge` — a guardian's own enrolled device can
legitimately carry both if the guardian marked their own device (unusual but not disallowed).

## Removing a Device

**`fauna.sync.devices.delete`** (WS-RPC; the `DELETE /api/v1/sync/devices/{device_id}` HTTP twin is deleted)

> **The account-data plane adds a second removal leg (R14 build design,
> 2026-08-13):** the nest device row governs connection auth and bearers
> (this section, unchanged); *generation admissibility and wrap targeting* —
> and, since 2026-09-20, *peer-plane admission at every sibling*
> ([`../architecture/account-sync-plane.md`](../architecture/account-sync-plane.md)
> § The admission seam → *Validity and severance*, which owns that bound) —
> are severed only by the plane's `fauna.state.device-set` removal record —
> the remove-device action writes both, built 2026-09-16 in
> `DevicesMachine::remove_device` (shared Rust, all 7 apps — web's fleet leg
> through the account port since 2026-09-30:
> `architecture/account-client-lifecycle.md` § The client-side lifecycle →
> *The account port*). **Which fleet member the second leg removes is resolved from
> client-held truth before either leg runs, never read off the roster row —
> and a removal that resolves to the device in hand, or to no device the
> app can verify, is refused with nothing deleted** (the page says so on
> `error-message`; removing the device you are on is sign-out). A device
> whose own record disagrees with its roster row is removed by its key
> instead, the user's call (ruled 2026-09-19; built on tui and macOS + iOS
> 2026-09-25 —
> [`../ui/devices.md`](../ui/devices.md) § Members without a matching entry,
> linux/windows/android owing the render). **The two
> legs are crash-safe:** the intent to remove is recorded durably before the
> nest deletion and the account runtime finishes the second leg on its own
> if the page could not. **The deletion below runs at one nest — the one the
> removing app is bound to; every other nest of the account, and a removal
> made by key, is reached through the removed device's grant, revoked by
> key** (ruled and built 2026-10-01: same owner, *The nest half follows
> merged state*). Owner of the target rule, the binding, the
> completion rule and their bounds:
> [`../architecture/account-data-taxonomy.md`](../architecture/account-data-taxonomy.md)
> § The generation machinery → *Fleet-scope reclamation*, clause (4).
>
> **What the removed device itself does afterwards is ruled elsewhere, not
> here:** it stays ended — its data path dies with its writer-key tombstone
> and the machine returns only as a successor
> ([`../architecture/account-replica-posture.md`](../architecture/account-replica-posture.md)
> § The store device principal → *Principal succession after a device
> delete*) — and on the group plane a device its authority line refuses
> writes its revocations and nothing else
> ([`../architecture/recipient-set-scheme.md`](../architecture/recipient-set-scheme.md)
> § The recipient-set scheme → *Severance, per axis* → *An authority
> device's removal*).

The nest performs these checks before deletion:

1. **Ownership check** — device must belong to the authenticated actor. Returns 404 if not found or not owned.
2. **Guardian-mark check** (supervised accounts only) — a device the guardian marked cannot be removed by the supervised account while the guardian link is active: refused as `guardian_marked`. Owner of this mechanism: [`family-safety.md`](family-safety.md) § the guardian-device marker (the guardian un-marks it first, or graduation auto-revokes the mark).
3. **Sole-source check — RETIRED (ruled 2026-09-28, the folders mode contraction design pass; built until the role-contraction slice landed it, 2026-09-29).** A folder has no type and no single source (`folders.md` § Target re-model — places with flags replaced the role enum): the nest holds the head, so a folder no device feeds is an ordinary nest-held folder (the archive-import folder is born that way) and removing a device's places loses nothing; the refusal, `get_sole_source_folders`, `folders.source_device_id` and its `fauna.sync.status` echo all go. What it did until then (history) — if the device is the only `source`-role seat of any folder, the request is rejected with 409 Conflict:
   ```json
   {
     "error": "device is the sole source for folders",
     "folders": ["photos", "documents"]
   }
   ```
   The caller must designate another source device for those folders first.
4. **Deletion** — removes the device from `sync_devices` and its `folder_members` entries in the remover's own folders only. A device id is unique only within an account, and another account can register the same id (it reads peers' raw ids off a shared folder's change feed), so every statement that reaches `folder_members` by device id — this delete, the grant revoke's and adopt's placeholder cleanups, and the per-device folder roles on a device listing — is scoped to folders the calling account owns; since a seat only ever admits the owner's own device into the owner's own folder, the scope loses nothing a legitimate removal meant to remove.
5. **WebSocket cleanup** — disconnects the device's active WebSocket if present.

Success response:

```json
{
  "deleted": true,
  "folders_removed_from": 2
}
```

### MLS removal (client responsibility)

After the server-side deletion, the removed device's cryptographic reach is bounded by the MLS plane: for shared folders the Remove path runs the gated rotate-on-removal commit (`mls-group-key-material.md` § M2 *Rotate-on-removal*), and conversations converge per § Cross-device MLS group-state sync. MLS ratcheting means a removed member cannot decrypt messages sent after its removal commit — forward secrecy at the protocol level, not just revoked server access. *(The `DeviceSyncChannel`-group removal described in older revisions rode the dormant primitive — see § Device Sync Channel.)*

## Security Properties

| Property | Mechanism |
|---|---|
| No passwords | Possession of the 32-byte secret key = account access on any device |
| No server-side key storage | Nest stores only the public key (`ActorId`); the secret never leaves the client |
| Forward secrecy | MLS ratcheting — removed devices cannot decrypt messages after their removal commit is processed |
| Capability restriction | `DeviceAuthorization` signed by the root key limits what a secondary device can do |
| Emergency lockout | Ed25519 signature (no Bearer token required) locks the account for a hard-coded 24 hours (§ Emergency lockout) |
| No account recovery without the key *(amended 2026-07-23; nest-side complete 2026-07-29; both ceremonies driven end to end on the lead app 2026-08-02, other apps' legs pending)* | With no recovery kit, unchanged: a lost secret key means an unrecoverable account, and there is no "forgot password" flow. With a registered RecoveryKey (offline kit): seed **loss** is recoverable via the seed-escrow restore, and seed **theft** is recoverable via the succession ceremony (old key refused, account re-pointed to a successor). Owner: [`identity-succession.md`](identity-succession.md) — the seed-signature paths in this doc (handshake, emergency lockout) return its `superseded` refusal once an identity has been succeeded |

## New Platform Implementation Checklist

Steps required to add multi-device support to a new fauna app:

1. **Secret key import** — accept 64-char hex input, decode to 32-byte Ed25519 secret.
2. **Derive ActorId** — compute Ed25519 public key from the secret.
3. **Authenticate** — `fauna.auth.handshake` (WS-RPC) with Ed25519 signature over `actor_id_bytes || timestamp_be_bytes`.
4. **Generate device_id** — random 32-byte value, stored locally per-device (not the keypair).
5. **Register for sync** — `fauna.sync.register` with `actor_id`, `device_id`, `label`, `capabilities`.
6. **Start the conversations session** — the shared `ConversationsSession` (direct Rust, UniFFI, or WASM) restores the `__mls` state replica before its first poll (§ Cross-device MLS group-state sync) — this is what gives the new device the user's existing conversations.
7. **Publish key packages** — session-owned replenish after the replica restore (`fauna.conversations.keypackage.upload`), so other actors can add this identity to future groups.

## FAQ

**Q: How many devices can I have?**
A: Depends on tier — Free: 2, Personal: 5, Community: 10 (the seeded defaults; tiers are admin-editable rows, so a deployment's actual caps may differ).

**Q: Can I use the same secret key on multiple devices simultaneously?**
A: Yes. Each device authenticates independently with the same keypair. Each gets its own bearer token and device_id for sync.

**Q: What happens if I revoke all sessions?**
A: All bearer tokens except the current one are invalidated. Other devices must re-authenticate (sign a new challenge). No data is lost.
