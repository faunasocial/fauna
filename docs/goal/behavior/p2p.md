# P2P (Direct Contacts) — target state

Owns: p2p, p2p-participation, offline-share-initiation, delivery-seat
Status: ratified — the transport seam + substrate decision (iroh, adopted 2026-06-28/30); the **fauna-peer → Y.1 reframe was EXECUTED 2026-08-10** (refutable at build time), and the **WireGuard stack was DELETED OUTRIGHT 2026-08-23** (user-directed: "we are using iroh for now and there is no point in compiling wireguard"), so iroh-QUIC is the only substrate and the seam has one impl. The WG-era flow sections (wake signaling, NAT traversal, LAN file transfer) are re-derived over the seam from [`../architecture/account-sync-plane.md`](../architecture/account-sync-plane.md) § The peer leg; the data-plane build vehicle is the account-plane W2 (account-data-plane.md § Workstreams) workstream (peer leg + the § Cross-user shared-set transfer twin). § Cross-user shared-set transfer and § Wormability posture are ratified target state (2026-08-10, user-directed; unbuilt — refutable at build time)
Authority: the P2P optimization layer — the substrate-agnostic fauna-transport seam (dial/listen/PeerConn; the symmetric-key, auth-above-the-seam, and fallback-not-the-seam's-concern properties), the iroh-QUIC impl and the PeerNode lifecycle, LAN detection, tunnel lifecycle, the relay capability + iroh-relay sidecar advertisement, and the p2p page behavior; the cross-user plane's build contract, build ledger and phone-peer design → [`p2p-shared-set-build.md`](p2p-shared-set-build.md) (split 2026-09-27); ui.yaml (`p2p` page) owns element IDs + per-page element scope; defers the PQ posture to [`../architecture/security/post-quantum.md`](../architecture/security/post-quantum.md), nest↔nest channel auth to [`../architecture/federation.md`](../architecture/federation.md), SNI routing of `relay.<domain>` to [`caldav-server.md`](caldav-server.md) (registry: sni-routing), the chunk protocol to [`file-sync.md`](file-sync.md), and the relay-SAN/cert coupling to [`../architecture/nest/tls-certificates.md`](../architecture/nest/tls-certificates.md). **There is no WireGuard concept to own any more** — the stack, its `fauna.wireguard.peer.*` / `fauna.admin.wireguard.*` kinds, its nest config section and its NAT-signaling mechanism were all deleted 2026-08-23 (see § Implementation status today → *The WireGuard deletion*).

## Implementation status today

**The SAME-ACCOUNT peer data plane is dormant fleet-wide** — read this
paragraph's scope carefully, because the paragraph below it is the opposite.
**Dormant is the built state, not the target (user ruling 2026-09-26): the
plane is to be built, shared Rust first then tui, witnessing the three § Goal
promises the `device-to-device-file-transfer` catalog page holds as
outcomes.**
Same-account file-sync is 100% nest-mediated (`SyncClient` → nest; the peer
link carries no chunk/manifest transfer); the native consumers only start/stop
a peer node and never make calls through it. That substrate is foundational for
future P2P features, not a live traffic path — so changes to *it* are
pre-production internal work, not a compat-surface concern. **The CROSS-USER
share plane is a different story and is live**: on tui (2026-08-19) and linux
(2026-08-20/21) real user bytes move peer-to-peer, proven by an app-level
two-actor journey with the nest down (§ Cross-user shared-set transfer). A
session quoting "the peer data plane is dormant" at the share plane is quoting
the wrong paragraph.

**Offline AUTHORSHIP on the share plane was red from 2026-09-27 to 2026-10-02, and is restored.** Between those dates a write made on a shared set while the set's row could not be read was not sealed at all — the sync agent's pre-seal hold refused it — so no own-pending row was minted, and the two nest-down journeys of `tests/e2e-unified/tests/test_share_pump_two_actor.py` (`test_member_pulls_offline_authored_file_from_owner_peer`, `test_an_interrupted_peer_transfer_resumes_without_resending_what_arrived`) failed with the file never arriving. Decision 2′ of [`on-demand-files.md`](on-demand-files.md) § Shared sets on a capability host, built 2026-10-02, moved the hold from the seal to what is published to a nest: the write seals under the last floor read and its pending row serves on this plane, and nothing of it reaches a nest until a row read checks the floor; that section owns the rule, its built shape and its cost. The paragraphs below that cite those two journeys as green describe the state before 2026-09-27 and since 2026-10-02.

**The co-present ceremony's journey is green on tui, linux, windows (2026-09-15) and now macOS too (re-verified 2026-09-22).** On windows the dial never landed before 2026-09-15: every compare code carried a candidate pairing the `[::]` socket's port with an IPv4 address, which has no listener where that socket is v6-only (§ Offline share initiation, contract point 1). `test_a_co_present_ceremony_lists_the_shared_set_on_both_seats`, the § Offline share initiation *Proof*, last failed on macOS on 2026-08-27 — before that candidate fix, before `fauna_sync_engine::offline_share::flush` stopped erasing a deliver that lands while the recipient is still persisting its consent. Windows and macOS both re-read the group listing on the edge a ceremony lands a scope, through the FFI's `lands_a_scope` export, as tui and linux do.

**The ceremony's record rests on the device's own account store — the account plane's `fauna.state.group-share-ceremony` kind, born plane-only 2026-09-28 — so its PAGE surface answers with the nest unreachable, on every app.** § Offline share initiation's substantive offline claim is about the ceremony itself, and the cryptography keeps it; the *page that has to answer the offer* kept it only once its read stopped needing a nest. From 2026-09-22 the record was `UserConfig.group_shares`, read through `fauna_client_config::ConfigClient::load` — a `fauna.config.get` round-trip — so an unreachable nest blanked the recipient's consent card and both seats' scope rows unless the read fell back to the bound seat's in-memory copy. The E3 lead slice of the `__config` dissolution ([`../architecture/config-dissolution.md`](../architecture/config-dissolution.md) § Implementation status today owns the kind's build) moved the record to a fleet-only, tip-sealed plane kind, one byte-string row per ceremony side-record. The seat binds over an empty replica and each host lends it the record at its account-store-ready edge (`offline_share::lend_account_record` → `SessionSeat::lend_record`, § Offline share initiation → *The seat's record is lent late*); the lend and every later flush join both ways through `AccountStoreHandle::merge_group_shares`, so neither the store's gains nor the replica's in-flight ones are lost. `offline_share::load_group_shares(account, seat, own)` keeps the seat fallback with its reason changed: it reads the durable local rows (the lent record, else the runtime's handle) joined with the bound seat's replica, which answers alone for a frame ingested before the lend. The rows seal under the same generation tip as the ceremony's own held-root row, so the offline ceremony gains no new precondition; a device with no tip yet refuses the flush at the writer door (pinned by `fauna-sync-engine/tests/peer_leg_convergence.rs::the_group_share_ceremony_door_refuses_while_no_tip_resolves`), and two devices' records converge through the production door (`…::a_group_share_ceremony_record_converges_through_the_production_door`). It is one read on every app: tui and linux call it directly, and `fauna-ffi`'s `offline_share_load_group_shares` passes through (its `nest` parameter is unread now, kept so the windows, macOS, iOS and android calls are unchanged). Each app's wrapper opens its nest connection before the call, and only its own run of the journey below says whether that step fails first: macOS and windows reach the FFI with the nest down (windows witnessed 2026-09-29, once its folders page read the ceremony's half ahead of every nest round trip — sequenced behind the M2 peek or the machine refresh, a down nest had kept the card and the landed row from painting); iOS and android are still unwitnessed. Witnessed on tui, linux, macOS and windows by `tests/e2e-unified/tests/test_offline_share_two_seat.py::test_a_co_present_ceremony_completes_while_the_nest_is_unreachable`, which spends both nest-bound preconditions — the seat's bind and each device's first generation tip — before stopping the nest.

**The initiator's walk redials a dropped connection itself (2026-09-22).** § Offline share initiation's "a dropped co-present connection redials without re-scanning" had the admission half only: the recipient's record did admit a redial (`group_ceremony_peer::record_admits`), but the app's walk ended on any lost connection, and pressing Begin again minted a SECOND scope — a second card, not the same share picked up. `GroupShareInitiator::with_redial` now lets `drive` dial the same compare code again when the connection is lost after the offer crossed (a closed dispatcher or the synthesised disconnect — a refusal such as a decline stays terminal and is never redialed), keep asking about the same scope, and re-send the deliver it already built rather than minting a second generation, paced by `REDIAL_PAUSE` inside the step's existing budget. `offline_share::initiate` wires it, so every app that drives the ceremony through that door has it. Pinned by `fauna-client-capabilities/tests/group_ceremony_node.rs` and witnessed on tui by `tests/e2e-unified/tests/test_offline_share_two_seat.py::test_a_connection_dropped_part_way_picks_the_same_share_up_again`, which drops the recipient's connections through the compile-gated `offline_share_drop_connections` agent arm. linux has the arm too; the FFI apps reach the same call through the test-flavored `FfiCeremonySeat::drop_connections_for_test` (and the receive-window clock through `offline_share_advance_clock`) — macOS, iOS and windows carry their agent arms over them (windows since 2026-09-28), android still owes its own.

**An interrupted peer transfer resumes at FILE granularity, and the pull cursor now holds below a row that did not arrive (2026-09-23).** § Cross-user shared-set transfer's "an interrupted transfer resumes … with held chunks never re-sent" holds per file: the pump's fetch plan skips any row whose manifest the member already holds (`share_pump::plan_page_fetch`), and the ingest judges it current, so a file that arrived before the cut is never requested again. A file whose bodies were only partly fetched when the connection dropped is fetched whole on the next pass, because the spool keeps a file only once all its chunks are in; "held" means held in a landed file. Before 2026-09-23 the resume also had a hole for SEQUENCED rows: `always_resident::ingest_share_page` advanced the per-peer cursor to the page's highest seq even when one of its rows was skipped because its bytes never arrived, so the peer never served that row again and, with the nest down, it never arrived at all. The ingest now reports the lowest such seq (`PeerIngestReport::retry_floor`) and the cursor stops just below it; a permanent skip (an unsafe path, a row with no manifest) still moves it on. Pinned by `peer_share_ingest_test.rs::a_row_whose_bytes_did_not_arrive_holds_the_pull_cursor_below_it` and witnessed on tui by `tests/e2e-unified/tests/test_share_pump_two_actor.py::test_an_interrupted_peer_transfer_resumes_without_resending_what_arrived`. That journey cuts the transfer at a state, not on a race: the compile-gated `offline_share_hold_serves` arm parks the owner's next manifest answer, `offline_share_drop_connections` cuts the member's connection under it, and the owner's `share_serve_tally` state key (`fauna_sync_engine::share_serve_tally`) shows every file was answered exactly once. Pending (offline-authored) rows never touch the cursor and were already re-served every pass. linux has both arms and the state key (2026-09-24); the apps that do not bind the share plane yet cannot run it until they do (§ Implementation status today, the FFI share-plane host).

**A person the set was never shared with is refused at the serve door, and that is witnessed from a third seat (2026-09-23).** § Cross-user shared-set transfer's "admission is the front door, the M2 seal is the wall" had its front-door half pinned only one tier down (`share_pump_two_seats.rs::a_stranger_is_refused_at_admission_and_pulls_nothing`). No app gesture makes a stranger pull a set, because the pump dials only sets its own seat belongs to, so the app-level witness uses a compile-gated probe: `fauna_sync_engine::share_probe::probe_set`, driven by tui's `offline_share_probe_set` agent arm. It dials a peer by its compare code, claims the set, and then asks for the set's rows and for named manifests whatever the admission said — the request a hostile client would send, which the pump itself never makes after a refusal. Run from the member's seat it is its own control: admitted, the payload's row listed, its manifest fetched. Run from a third seat holding the manifest hashes the member saw, it gets no rows, no paths and no manifest bodies, the owner's `share_serve_tally` does not move for the payload, and the payload's bytes appear nowhere under the stranger's per-launch state root (`tests/e2e-unified/tests/test_share_pump_two_actor.py::test_a_person_the_folder_was_never_shared_with_gets_nothing_readable`, tui). linux has the probe arm too (2026-09-24): its argument parsing and the probe are one shared call, `share_probe::probe_set_from_args`, so no app re-derives how a code or group id is read. The apps that do not bind the share plane yet cannot run it until they do.

**The participation half of rule 5 is BUILT — designed and built 2026-09-25 (§ Per-device participation; user-directed): a user turns a device's peer listeners off from the devices page, and off leaves no socket.** Until that day no human could: the share seat bound at the account-store-ready edge on tui and linux consulting no user-set state, tui additionally handed the same-account peer-sync leg a transport at session assembly, and the only brakes were the `p2p-share` cargo feature and the nest's compile-flavor `p2p-share` advertisement (declared 2026-08-24, the design pass; linux's `p2p-tunnel-toggle` was never this control — it drives the dormant diagnostic node, § RATIFIED 2026-08-24 finding 2). What landed, in shared Rust first: the device-local row (`fauna_sync_engine::p2p_participation`, one meta-table row per (device, account), absent = on) behind `AccountStoreHandle::{p2p_participation, set_p2p_participation, p2p_participation_watch}`; the same-account leg's door — `peer_leg::ensure_bound` reads the row before the brake, folds a pending nest brake and sends the device's report on each full pass, and off drops the node, serve side and transport (`PeerLegPass::ParticipationOff`); the share seat's door — `offline_share::SessionSeat` carries the verdict the driver records, `get_or_bind` (both doors) refuses while off, `unbind` drops a bound seat, and `share_glue::run` selects on the runtime's watch, unbinds on off and reads `ServeStatus::ParticipationOff` on `share-serve-status`, inherited by every host (tui, linux, the `fauna-ffi` share-plane host); the wire — `SyncDevice.{p2p_participation, p2p_off_requested}` and `fauna.sync.devices.p2p_participation.set` with its owner arm (brake only) and self arm (a proof of possession by the row's principal, `sig_domain::DEVICE_P2P_PARTICIPATION_V1`), two additive `sync_devices` columns behind it; and the devices machine — `DeviceSummary.{p2p_participation, p2p_off_requested}`, `DevicesSnapshot::own_p2p_participation`, the per-row paint `DeviceSummary.p2p_participation_paint` (own, checked, label, actionable — `fauna_devices_machine::p2p_participation`, drawn at every snapshot by the same own-row rule the gesture takes its arm by), the injected `P2pParticipation` door (`fauna_client_account_runtime::p2p_participation`, wired on tui, linux and in `fauna-ffi`'s `build_devices_machine`) and the one gesture `DevicesMachine::set_p2p_participation(index, on)` (own row: the local switch; a sibling's row: request off, and a remote `on` refused without a call). **tui renders it** (lead app): `device-p2p-participation-toggle` on every `device-card`, drawn straight from the machine's published paint, tui handing its own device id to `set_this_device_row` for the window before its runtime names the enrolled row. **macOS renders it, fully built** (2026-09-26), through the shared FaunaKit `DeviceCard` both apple targets use — drawing the machine's published `DeviceSummary.p2pParticipationPaint` (2026-09-29; the FaunaKit copy of the own-row rule is deleted, `nil` reads the sibling arm), the own row naming itself to the machine via `DevicesMachine::set_this_device_row` (§ Per-device participation → *Which row is this device's*). **windows renders it** (2026-09-26): a CheckBox in the `device-card` template, tui's two arms (`FaunaApp.Core.ViewModels.DeviceParticipationPaint`, a local copy of the paint owed its swap to the published one), the own row named to the machine via `set_this_device_row` with the id `device-this-mark-badge` paints from; its account runtime backs the door, so the own-row switch writes the device-local row. **Bound:** windows wires no share plane (`start_share_plane`) and paints no `share-serve-status`, so the switch has no in-app listener of the share kind to drop and the plane-reading journey skips windows as unbuilt until the windows share-plane leg lands; the toggle-only journey runs there. **iOS renders the same card, and its own row is live (measured 2026-09-27):** the premise it was once recorded inert on — that iOS starts no account runtime — is refuted by the runtime's owner doc and the shared `FaunaClient.start()` funnel (iOS hosts it since 2026-08-25 — [`../architecture/account-runtime.md`](../architecture/account-runtime.md) § Implementation status today → *Built — W3 the apple host*), and a run on macOS settles it: the toggle-only journey passes on iOS, so the own-row switch writes the device-local row through the door and the state survives a fresh hydrate — windows' state. **Bound:** iOS wires no share plane (`start_share_plane`, its phone-peer leg — [`p2p-shared-set-build.md`](p2p-shared-set-build.md) § *Phone peers — design*), so it paints no `share-serve-status` and the plane-reading journey skips iOS as unbuilt; sibling rows' request-off arm works as everywhere. **linux and android render it** (2026-10-06): a check button (linux `views/devices_folders/roster.rs`) and a labelled switch (android `DevicesScreen.kt`'s `DeviceCard`) drawn straight from the machine's published paint, each app handing the row its `device-this-mark-badge` paints from to `set_this_device_row`; both doors are wired (linux `wire_fleet_removal`, android through `fauna-ffi`'s `build_devices_machine`). linux hosts the share plane, so both journeys run there. **Bound:** android starts no share plane and paints no `share-serve-status`, so the plane-reading journey skips android as unbuilt; and android has no e2e run venue yet, so its leg is compile-verified only. **web renders the request-off arm** (2026-10-06): a checkbox on every `device-card` drawn from the serialised paint (`setP2pParticipation` on the wasm devices machine), every row `own: false` because the tab wires no door (§ Per-device participation → *Web*) — so both own-row journeys declare web's absence. **Pinned:** `p2p_participation::tests` (the fold rules); `fauna-sync-engine/tests/p2p_participation_gate.rs` (the peer leg over the in-memory transport: bound → off → the accept side is gone and stays gone across a restart → on → bound again); `offline_share::tests::a_switched_off_device_refuses_both_doors_and_unbind_ends_the_listener`; `fauna-iroh/tests/participation_socket.rs` (a bound endpoint's UDP port is free once every handle drops — the fact both doors rest on); `bins/fauna-nest/tests/conformance_p2p_participation.rs` (owner arm brakes, owner-arm enable refused, self arm reports, an `on` report never clears a pending brake, a flipped or foreign signature is refused); `fauna_devices_machine::p2p_participation::tests` (the paint's two arms and the own-row rule, no door meaning no own row); `settings::devices::tests` on tui (it draws the published paint, and the app's own id alone never makes the own switch); `fauna-devices-machine/tests/devices_lifecycle.rs` (which row is this device's: a runtimeless seat's own row takes the own arm and surfaces the refusal with no nest call, a sibling stays on request-off, the door's enrolled row outranks the app's id — and the snapshot's paint names the same own row at refresh, none with no door); windows' `DeviceParticipationPaintTests` (its local copy's two arms, retired with the copy); and the journeys in `tests/e2e-unified/tests/test_p2p_participation.py` (the own-row toggle flips and survives a fresh hydrate, both ways; and, where the app hosts the share plane, `share-serve-status` reads off, then on again). **The cross-process arm is BUILT (2026-10-06):** when another process holds the engine — the co-located sync agent on a desktop — the door asks it for its pass after the row rests (`RequestMethod::ReconcileAccountRuntime`, served by the agent as `reconcile_now` on its mounted store; the shared `p2p_participation::EngineHolderNudge` over `SyncAgentProvisioner::reconcile_account_runtime`, published to the process's `EngineHolderNudgeSlot::seat` by each host's provisioner and read by the door on tui, linux and `fauna-ffi`), so the agent-held same-account listener drops within the gesture too; pinned by `pipe_server::reconcile_account_runtime_tests` (the verb runs a pass of the mounted store; nothing mounted is a quiet no-op) and `fauna-client-account-runtime/tests/p2p_participation_nudge.rs` (a switch beside the holder asks it for its pass; one in the holder asks no one). **One honest bound:** the seat slot's verdict is unread between sign-in and the account store's assembly (the seconds before the share driver's first pass), during which only the offline-share panel's own door could bind, and the driver's first pass unbinds it if the row says off. **Owed:** windows' swap from its local paint copy to the published paint, and iOS's own-row arm (owed a measurement, not a runtime — above).

**Two per-app claims corrected 2026-08-24; (a) resolved 2026-08-25.** *(a)* **android hosts no peer node, and the dead caller is now
deleted.** The long-standing note "hosts the peer node headlessly via its
foreground service" was false: `P2PTunnelService`
(`apps/fauna-android/app/src/main/java/com/fauna/app/core/P2PTunnelService.kt`)
called `peerTunnelStart`, and the manifest declared it, but **nothing ever
started the service** — the app's only `startForegroundService` call targets
`SyncService`. Deleted rather than wired: the same-account contact-plane node
it would have hosted stays dormant fleet-wide (§ Implementation status today,
opening paragraph), its outbound `PeerNode::dial` was already unwired with no
signaling transport, and — the item's own stated wire condition — starting an
uncontrollable headless listener today would front-run the rule-A-gated
per-device participation control (§ Implementation status today → *The
participation half of rule 5 is unbuilt*) that is the actual missing piece,
not android's account-runtime status (android became the account-runtime
seat's first consumer 2026-08-22, `../architecture/account-data-plane.md` §
Implementation status today → *Built — W3 the android host*, so that was never
the real gate). `P2PTunnelService.kt`, `P2PTunnelState.kt`, the manifest entry,
the `fauna_p2p` notification channel and the `tunnel` feature flag on every
android build recipe (`justfile`) are gone; the matrix row and ui.yaml's
android note record the deletion. *(b)* **linux did not render
this page's `error-message`; fixed 2026-08-25.**
ui.yaml `pages.p2p` declares it; linux used to write a failed start into an
untagged status row (`apps/fauna-linux/src/settings/p2p_tab.rs:148`), leaving
e2e convention 2 nothing to read on the only app that has the page. Now a
dedicated hidden-until-set label carries it, built + cleared per convention
2's 2026-08-04 rider. `p2p-lan-copy-btn` returning interface names rather than
addresses was fixed in the same commit (`p2p_tab.rs:413`, reusing
`fauna_peer_sync::lan::discover_lan_candidates`).

**The share leg (§ Cross-user shared-set transfer) is partly built and still
dark.** Landed 2026-08-17: the protocol + admission floor, the serve/pull core
(the byte half moves an ordinary multi-MB file end to end over a real channel),
the change-row provenance ruling, the **discovery carriage** — the
advertisement body, its binding to the MLS-authenticated sender, the
`fauna.state.share-endpoints` cache and its dial reader — and the **nest legs**
(slice D: the `p2p-share` capability token + the `p2p-share.member.admit`
chokepoint at the Welcome door — § *Wormability walk* rules 7 and 8). The
**bind door is also BUILT** (§ Offline share initiation below —
`group_ceremony_node` composes `ShareServer` onto the contact-plane node), and
**the group plane's own store landed 2026-08-17**
([`../architecture/account-data-plane.md`](../architecture/account-data-plane.md)
§ Implementation status today owns it — store table, machinery-root-sealed
plane, ceremony applier, member-to-member walk proven over a canned feed); the
node's `ShareServer` backing was a deliberate stub as of that landing —
**retired the next day** by the serve-side byte half (below).
The change-**row** half is BUILT (2026-08-17,
B2.1–B2.5 — [`p2p-shared-set-build.md`](p2p-shared-set-build.md) § *Build design — the row half*): funnel-write-through retention
plus the own-pending offline mint, the fail-closed cached writer roster, the
provisional overlay with conservative materialization, and the
retire-on-arrival reconcile — the store serves real retained rows, sequenced
and own-pending alike, never a synthesis. **The whole SHARED half of the
transfer plane is BUILT (2026-08-18, row 58 legs 1–6 — [`p2p-shared-set-build.md`](p2p-shared-set-build.md) § *Built — the
serve-side byte half* + § *Built — the pump, the boundary, and the discovery
halves*):** the byte half (manifest retention + range-derived chunks), the
real serve backing on the bind door (`bind_with_share_plane` — the ceremony
stub retired), the app↔agent ingest boundary, the pull pump with its
per-peer cursor, the publish decision + the durable sink door, and rule 7's
cached brake read — proven at tier_1 by a two-seat test moving a 20 MiB
file between two users over a real channel. **The tui APP LEG is BUILT
(2026-08-19, row 58 — [`p2p-shared-set-build.md`](p2p-shared-set-build.md) § *Built — the tui app leg*):** tui binds the seat at
the account-store-ready edge under the live-else-cached brake, runs the
pump + publish loop, registers the durable `ShareEndpointsSink`, and
renders the transfer surface (`share-serve-status` +
`share-transfer-*`) — the first app on which the plane is live end to end,
**proven the same day by the app-level two-actor journey**
(`test_share_pump_two_actor.py`: a file authored with the nest DOWN
arrives on the other user's seat peer-to-peer). **The plane's app-level
DRIVER was then LIFTED into shared Rust: `fauna_sync_engine::
share_glue` now owns the pump loop, the `FolderRef`-first spec join, the
last-known-spec hold, rule 7's brake composition, the advertisement decision,
the durable sink's body and the surface readings, over a per-app
`SharePlaneHost`; tui's glue became that host, and **linux is the second app
leg** (same day). Still open: the remaining apps' call sites + renders.
The **host they share is built** (2026-09-24, `fauna-ffi`'s `p2p-share`
feature — [`p2p-shared-set-build.md`](p2p-shared-set-build.md) § *Built — the shared driver, and the linux leg* owns the
detail); every native app hosts the account runtime it needs (iOS too, since
2026-08-25 —
[`../architecture/account-runtime.md`](../architecture/account-runtime.md)
§ Implementation status today → *Built — W3 the apple host*), so windows owes
only its call at the account-store-ready edge and the six-id paint, macOS
made its call 2026-09-25, and the two phones owe theirs behind the
phone-peer design ([`p2p-shared-set-build.md`](p2p-shared-set-build.md) § *Phone peers — design*; its shared-Rust arm is built, 2026-10-01).
Separately, the **offline-share affordance is BUILT on tui, both roles**
(2026-08-18 — § Offline share initiation → *Built — the affordance, both
roles*), **and on linux (2026-08-20, row 334 — the shared-logic half then
the panel/consent-card/group-scope-row UI wiring), on macOS+iOS
(2026-08-25/26), on windows (2026-08-27), and on android (2026-09-22)** — the two-seat journey's
`ceremony_seats` fixture is parametrized over `_SUPPORTED_APPS`
(`tui`, `linux`, `macos`, `windows`) rather than hardcoded to tui, so each
desktop leg is proven the same way tui's is. android is not among them: two
seats of one app on one machine means two installs, and an android run
drives one device serial, so its leg's witness is the single-seat file
alone. **web
is a declared absence for this ceremony** (§ *Wormability walk* rule 5: the
same `fauna-iroh`-has-no-wasm-target reason the transfer plane is absent
from web), not a trickle-down leg. The remaining three native apps
(windows, macos, ios) reach `fauna_client_capabilities::group_ceremony_node`
through the UniFFI boundary landed 2026-08-21
(`libs/fauna-ffi/src/offline_share.rs` — `FfiCeremonySeat` +
bind/view/parse-code/initiate/consent/decline/load-shares, mirroring tui's
own orchestration), which unblocks their own per-app UI wiring; android
inherits the same crates through the same boundary — its separate W3
account-runtime prerequisite was met 2026-08-22
(`../architecture/account-data-plane.md` § Implementation status today →
*Built — W3 the android host*), and android's per-app UI wiring landed
2026-09-22. **The ceremony's own orchestration was then
LIFTED into shared Rust (2026-08-22), the same move `share_glue` made for the
plane driver above: `fauna_sync_engine::offline_share` now owns the initiate /
consent / decline walks, the three load-bearing write-through orderings, the
config flush and the folders page's read, and tui and linux re-export it
rather than each holding a near-verbatim copy.** What stays per-app is only
what cannot follow it: each app's **bind door**, because the shared crate names
no concrete transport substrate (the iroh-cleanliness bargain
`fauna_sync_engine::peer_leg` documents), and the **i18n resolution** of the
two readings the folders page paints — the *resolution* only. Which key each
[`CeremonyStatus`] and each compare-code refusal carries is a fact, and it
lives once beside the types as
`fauna_client_capabilities::group_ceremony_view::status_label` /
`code_error_label`, returning a `LocalizedText` each app resolves through its
own pipeline (2026-08-23) — re-exported over UniFFI as
`offline_share_status_label` / `offline_share_code_error_label`, so the four
native legs inherit the mapping rather than each writing a third copy of it.
That is § Cross-user shared-set transfer's own rule
for this page, applied to the one reading that had not followed it: the
readings there are already listed under *what is shared, and therefore never
re-derived by a leg*, and are already implemented that way one section up the
same page (`fauna_sync_engine::share_glue::serve_status_label` →
`serve_status_text` in linux and tui). Until 2026-08-23 this sentence read
"the **wording**", and linux and tui each held a byte-identical copy of the
state → key `match`. `libs/fauna-ffi/src/offline_share.rs`'s duplication is
now GONE: its internals
(`flush`, `spawn_config_persist`, `now_fn`/`now_secs`, `write_through`,
`WriteThroughSide`) followed the orchestration onto
`fauna_sync_engine::offline_share` (2026-08-25), and
`offline_share_initiate`/`_consent`/`_decline` — the one layer that still
re-implemented the shared `initiate`/`consent`/`decline` walks instead of
calling them — were fixed the next day: `FfiCeremonySeat` now composes
`Arc<fauna_sync_engine::offline_share::CeremonySeat>` directly and every
ceremony act below is a thin pass-through (2026-08-26). What is
left in the FFI module is only the `uniffi::Record`/`#[uniffi::export]`
boundary itself, which genuinely cannot move into a crate with no UniFFI
surface. Its one blocker — the co-present dial having no addressing
information — is **RESOLVED 2026-08-19**: the compare code now carries this
device's bound LAN endpoints, so the code itself is the advertisement channel
(§ Offline share initiation → contract point 1, *The compare code carries the
addressing*). That is also why the discovery carriage above never closed
it: that publishes on an established set's conversation channel, which a
ceremony with no set and no shared nest does not have.

**The offline-share-initiation design (§ Offline share initiation, PQ-1
resolution 2026-08-17): every layer is BUILT on tui as of 2026-08-19, and on
linux as of 2026-08-20, including the one that makes two
co-present devices find each other.** The
carrier-agnostic half and the bind door landed 2026-08-17, the app surface's
initiator half the same day, its recipient half (consent, admission, and the
listing that makes a shared set visible) on 2026-08-18, and the **addressing
half on 2026-08-19** — the compare code carries the initiator's bound LAN
endpoints, resolving the one measured blocker (§ Offline share initiation →
contract point 1, *The compare code carries the addressing*).
In code:
`fauna_core::group_ceremony` — the signed offer / accept / deliver payloads
with the W8 sender-binding idiom (offer verification recomputes the scope id
from the carried birth record and requires the birth's authority to be the
signer; the accept carries the recipient's own actor-signed reception half;
the deliver carries the admission wrap + the machinery snapshot, adopted
through the ordinary `apply_class2` strictness) — and the admission-bundle
doors (`fauna_mls::wrapped_blob::group_generation_wraps` — machinery root +
retained generations sealed to the joiner's reception key, root commitment
verified in-door against the birth record). The group scope itself is
substantially built (2026-08-17: identity, roster lattice, generation
machinery with roster-coverage admissibility, registry home, reception
keypair kind — `../architecture/account-data-plane.md` § Implementation
status today owns the split). The ceremony **state machine** is also built
(same day): `fauna_client_capabilities::group_ceremony` — record-then-act
over `UserConfig::group_shares` (the account-state plane kind
`fauna.state.group-share-ceremony` since 2026-09-28), scope birth at begin (the root rests in
the ceremony record until its plane row lands), the mint + admission bundle
at deliver, and an admit step that runs the full verification chain
(re-derived scope id, in-door root commitment, authority-chain entry
verification, resolver with per-key commitment checks); the capstone test
runs the whole two-party ceremony in memory. The **peer-channel carriage
is BUILT too (2026-08-17)**: three `fauna.peer.share.ceremony.*` kinds on
the share serve set, initiator-originated, first-contact admitted by the
receive-act expectation (§ Offline share initiation → *Built — the
ceremony carriage*), with the capstone re-proven over a real channel.
The **bind door is BUILT (2026-08-17)** — the listener composition, with
rule 7's version brake made structural:
`fauna_client_capabilities::group_ceremony_node` composes `ShareServer` +
`GroupCeremonyPeer` onto an actor-keyed `PeerNode`, and `CeremonyNode::bind`
takes a *verdict* rather than a bare transport, so no caller can reach a
listener without having consulted the `p2p-share` advertisement (no
evidence at all refuses — the peer leg's posture verbatim). It also carries
the initiator walk and the affordance's shared paint decision
(`group_ceremony_view`), so seven apps share one implementation of the
pacing and of when a security-relevant gesture is clickable.
The **affordance is BUILT on tui, both roles (2026-08-17/18), and on linux
(2026-08-20, row 334)** — § Offline share initiation → *Built — the
affordance, both roles* — and its dial is **unblocked as of 2026-08-19**: the
compare code carries the addressing (contract point 1, *The compare code
carries the addressing*). **The affordance also landed on macOS+iOS
(2026-08-25/26)** — the shared FaunaKit `OfflineShareSectionView` +
`GroupInvitationRow` + `GroupScopeRow`, wired through the same FFI ceremony
boundary — **and on windows (2026-08-27)**: `INestRpcClient`'s five
nest-touching doors (bind/initiate/consent/decline/load-shares) over the
same FFI, the four pure/local reads called directly off `FaunaFfiMethods`
from the page (never routed through the RPC seam — the existing
`IdentityQrEncode` precedent for a locally-held-secret call), and the seat
itself held on `App` rather than the page instance (the page has no
`NavigationCacheMode`, so a page-scoped seat would have its armed
`ExpectFrom` listener destroyed by the very re-navigation the recipient's
own "did the invitation land" wait performs). **And on android
(2026-09-22)**, the last native leg — web is a declared absence, not a
trickle-down leg: the Folders page paints from the same FFI, and the seat,
the panel's state and every in-flight act live on an app-scoped
`OfflineShareHost` rather than the navigation-scoped `DevicesVM` — the
windows hazard again, plus a Compose-specific one: a `viewModelScope` Begin
would be *cancelled* by that re-navigation, and cancelling a UniFFI future
drops the Rust walk mid-ceremony. Android paints the gates from
`fauna_ffi::offline_share_gates` — `OfflineShareView`'s own predicates as a
Record, since the boundary carries a view's fields but never its methods;
the apple and windows legs still re-derive the act gate from the parse and
in-flight doors. android's e2e witness is the same file's marker; no
device-attached run has recorded it yet. ⚠ **A minified android build
(release, foss, the bundle) paints no ceremony buttons or code widgets
(measured 2026-09-30):** R8 folds the two gates to false and drops the
render after them, so only `offline-share-own-code` reaches the release
dex. Debug builds, which every e2e run uses, are unaffected. Still unbuilt here: group-scope
**content transfer** (the serve door still checks the folder family only —
the group content-kind sealing slice), and all seat-epoch machinery (the
messaging-seat half — gated behind A3's build, as before).

**The group plane's store LANDED 2026-08-17** (owner + build detail:
[`../architecture/account-data-plane.md`](../architecture/account-data-plane.md)
§ Implementation status today, the group-scope paragraph). Consequence for
this section: everything a ceremony produces now has a resting place — the
driver writes the plane rows through
`fauna_sync_engine::group_state_plane::GroupStatePlane` and the held-root +
reception-key rows through the fleet-scope account plane, and the restart
proof covers the joiner's admission bundle re-opening from the reloaded
reception secret. The affordance's *set listed* success now waits only on
the tui surface itself (its listing read is
`AccountStore::group_scope_states`) and, for live member-to-member
convergence, on the share serve set's group-scope feed wiring (the
`p2p-share` workstream's).

Substrate + seam (all landed; design ratified 2026-06-27, tracked internally):

| Piece | Status |
|---|---|
| `fauna-transport` seam trait (`dial`/`listen`/`PeerConn::{open_stream, accept_stream, peer_identity, path}`) | Landed — `libs/fauna-transport` |
| iroh-QUIC impl (`fauna_iroh::IrohTransport`, `libs/fauna-iroh`, `quic` feature) | Landed; **adopted as the production substrate** (2026-06-28; migration scheduled 2026-06-30). Intrinsic identity: the iroh `NodeId` *is* the Ed25519 actor key (PT-1b — no registry-trust dependency) |
| Y.1 peer channel (`libs/fauna-peer-channel`: `PeerStreamAdapter` L2, `PeerChannel` request/serve, `PeerNode` lifecycle) | Landed slices 1–7; proven over real iroh connections (and, until the stack was deleted 2026-08-23, real WireGuard ones — the seam carried two impls, which is what makes the surviving one substitutable) |
| `fauna.peer.*` kind set (`fauna_protocol::peer`) | Landed slice 6 — `fauna.peer.node_info`, `fauna.peer.exchange`. The vestigial `wg_public_key` field was **removed 2026-08-24**, ahead of the major bump that originally gated it; the removal is unobservable in both directions (the `extra` catch-all absorbs a WG-era peer's field; the field was `Option`+`default` on every build that declared it) and is pinned by two compat tests — owner + the three-part guard: [`../architecture/version-compatibility.md`](../architecture/version-compatibility.md) § Dimension 2 |
| Relay capability + sidecar | Landed: `capability::RELAY` advertised on `fauna.nest.info` (advertised while a relay sidecar is connected to the nest — `discovery_core::relay_sidecar_connected`; no flag) + additive `NestInfoReply.iroh_relay_url` derived from the claimed identity (`https://relay.<handle_domain()>`, orderable-apex-gated). `bins/fauna-iroh-relay` (self-hosted, `--features relay`); s6 sidecar + SNI route (no PROXY-v2) + `relay.<apex>` managed-cert SAN (deferred until the A record resolves) — tier_4-proven 2026-06-30. The relay fetches its cert as a sidecar via HPKE seal-on-read with zero `/data` access (owner: [`mail-bridge-lifecycle.md`](mail-bridge-lifecycle.md) § TLS provisioning + tls-certificates.md), over a channel it holds for its whole life. **The access rule is built (2026-10-03):** the relay asks the nest about every connecting endpoint (`fauna.relay.admit`) and refuses all but an enrolled device's principal key and a member's actor key (`relay_admission::key_is_known`, a lookup over `AdmissionSource::ALL`; witnesses `sidecar_channel::tests::relay_admit_serves_members_devices_and_nobody_else` on the nest and `relay_refuses_an_endpoint_that_is_not_a_member` in the relay binary). **A changed certificate reaches it at once** (`fauna.relay.cert_changed`; owner tls-certificates.md § Keeping the cert alive). **The image runs it unprompted (built 2026-10-03):** the relay s6 service is always up, the `services.json` `iroh_relay` flag and the `fauna-service-watcher` that polled it are deleted, and the nest hands the relay its cert only once it has a public name of its own (`sidecar_channel::relay_fetch_tls_cert` behind `discovery_core::relay_wanted`; until then the relay stands by) — nest witnesses `relay_is_handed_no_cert_until_the_nest_has_a_public_name` and `relay_sidecar_connected_follows_the_channel`; the image-level witness `test_iroh_relay.py::test_image_runs_its_relay_with_nothing_switched_on` exists. **Gaps against § The relay (measured 2026-10-01, re-read 2026-10-03):** no test above the transport crate drives two devices through it (`fauna-iroh`'s `iroh_dials_through_self_hosted_relay` and the relay binary's `binary_relay_carries_relay_only_peers` are the whole proof — every engine and app test runs with no relay address); a pair behind two NATs left the relayed path never while the relay served no address discovery, and with it served the cone pair turns direct in 0.3 s while symmetric and mixed pairs stay relayed (both measured 2026-10-05 by `just p2p-nat-probe`, § NAT hole punching); and the contact plane's seat binds with no relay, so no cross-user pair uses one yet — by ruling until the relay serves address discovery (built, below), when the seat takes it for the share plane's dials (§ The relay → *The cross-user seat and the relay*). **Across nests the relay serves nobody, by ruling, not by gap** (§ The relay → *Across nests*). **Address discovery is built (2026-10-05):** `run()` serves it with nothing switched on through `fauna_iroh_relay::production_options` — the substrate's server on `DISCOVERY_PORT` (7842/udp, the substrate default every endpoint's one-URL relay map already queries) on every interface, beside the loopback relay binds, its TLS from the relay's own hot-swap resolver — and `docker-compose.yml` publishes the port, the internet-setup guide opens it, and `nest/network-exposure.md` records the surface; witnesses `production_serves_discovery_on_every_interface_at_the_default_port` in the relay binary, `just p2p-nat-probe` (its default now grades the shipped shape: the cone pair direct, symmetric and mixed relayed) and the image-level `test_iroh_relay.py::test_published_discovery_port_answers_from_outside_the_container`, which exists. **Bytes ride direct paths only — ruled, not yet built** (§ The relay → *Address discovery, and what rides a relayed path*) |
| Security-review items | PT-1/PT-1b (identity rests on completed handshake proof), PT-4 (`is_safe_candidate` — loopback/link-local/IMDS rejected), PT-5 (no secrets in transport errors), `#[non_exhaustive]` hardening — all landed |
| Shipping image | `--features bluesky,nostr,activitypub` (`Dockerfile:175`; the relay binary is built in a cargo step of its own, copied into the image and always run) → a production nest with a public name of its own advertises `relay` and its relay URL once its relay is connected; a nest with no such name, or with no relay binary beside it, advertises none and clients use the nest-mediated fallback. **No shipped image ever compiled the `wireguard` feature** — which is what made deleting the stack a non-event for deployed nests |

Page surface × node hosting per app:

| App | Page surface | Peer node |
|---|---|---|
| linux | Settings → P2P tab (`apps/fauna-linux/src/settings/p2p_tab.rs`) — **the only P2P page on any app, and ratified 2026-08-24 as the only one there will be** (§ Element IDs → *RATIFIED 2026-08-24*): a Start/Stop toggle, this device's node id, its real LAN addresses (`fauna_peer_sync::lan::discover_lan_candidates`, fixed 2026-08-25, `p2p_tab.rs:412` — was interface *names*) and the contact list. Renders `error-message` on a failed start (fixed 2026-08-25 — was an untagged status-row subtitle) | **two, of opposite kinds.** The toggle's iroh `PeerNode` (slice 7, 2026-07-02) — manual Start, session-only, persisted nowhere, and **dormant**: it serves the base `fauna.peer.*` kinds and carries no live traffic. Separately the share plane's seat binds unconditionally at store-ready (`account_runtime.rs:206`), consulting the toggle not at all. linux passes `peer_transport: None`, so it runs no same-account peer-sync listener |
| android | none — **corrected 2026-08-24**: the long-standing "foreground `P2PTunnelService` (drives `peerTunnelStart`)" reading was false. The service existed and was manifest-declared, but **nothing ever started it** — the app's only `startForegroundService` call targets `SyncService`. **Deleted 2026-08-25** along with its manifest entry, `P2PTunnelState`, the `fauna_p2p` notification channel and the android-only `tunnel` cargo feature flag | **none.** The dead service and its FFI path (`fauna-ffi` `peer_tunnel_{start,stop,is_active}`, gated on the `tunnel` feature) are deleted; no android build enables `tunnel` any more. The uncompiled `#[cfg(feature = "tunnel")]` block it left in `libs/fauna-ffi/src/peer.rs` (with the feature and its `runtime` module) was deleted 2026-10-02: linux is the only peer-node host there will be (§ Element IDs), and it drives `PeerNode` directly |
| macos / ios | none | none |
| windows | none — the P2P page was WireGuard peer registration only and was deleted 2026-08-23 with the stack, along with the top-level-nav divergence it required | none |
| web | settings redirect stub (no dedicated surface) | none — relay-only by design |
| tui | none — the sub-page built 2026-08-10 was WireGuard peer registration only and was deleted 2026-08-23 with the stack. **A declared absence, not a tui parity gap, and as of 2026-08-24 the ratified target shape** (§ Element IDs → *RATIFIED 2026-08-24*): there is no P2P feature left for tui to lack, and linux's surviving page is a different (local-node) model tui never had | **two, both behind the device's own switch since 2026-09-25** — the share plane's seat and the same-account peer-sync leg's listener, each at its shared bind door; the switch is `device-p2p-participation-toggle` on the devices page, tui first (§ Per-device participation; § Implementation status today → *The participation half of rule 5 is BUILT*) |

Remaining: the per-app shells adopt the reshaped surface if/when they enable
P2P; the data plane lights up as the account-plane W2 peer leg + the
§ Cross-user shared-set transfer twin (that workstream owns sequencing). The
same-account peer leg is BUILT end-to-end (2026-08-15, owner
`account-data-plane.md` § Implementation status); the cross-user share twin's
**design pass ran 2026-08-17** (§ Cross-user shared-set transfer → *Build
contract* + *Wormability walk — the share leg*), and its **shared-Rust core is BUILT
2026-08-17** — the admission floor (slice A) plus the allowlisted serve set,
the puller, and the provenance policy (slice B; [`p2p-shared-set-build.md`](p2p-shared-set-build.md) § *Built — the serve/pull core*), and its **nest legs are BUILT 2026-08-17** (slice D: the `p2p-share`
capability token and the `p2p-share.member.admit` chokepoint at the Welcome
door — § *Wormability walk* rules 7 and 8 record both mechanisms), and its
**contact-plane node's own composition is BUILT 2026-08-17** (slice E's node
half: `group_ceremony_node::CeremonyNode::bind`, un-gated now that the brake
exists — § Offline share initiation above), and its **shared-Rust transfer
plane is BUILT 2026-08-18** ([`p2p-shared-set-build.md`](p2p-shared-set-build.md) § *Built — the serve-side byte
half* + § *Built — the pump, the boundary, and the discovery halves*: the
real `ShareServer`/`MultiSetShareStore` backing — the ceremony stub retired,
proven at tier_1 by `share_pump_two_seats.rs` moving a 20 MiB file between
two users — the pull pump, the app↔agent `ShareIngestDoor` boundary, the
discovery carriage, and the client-side `p2p-share.transfer` quota-gate
composition, § *Wormability walk* rule 8). **The tui affordance instantiated
the node's ceremony-only form as of 2026-08-18** (`CeremonyNode::bind`, built
2026-08-17/18 — § Offline share initiation → *Built — the affordance, both
roles*); at that point no flavor root yet called `bind_with_share_plane` or
`set_shared_sets`, so the real `ShareServer` backing was fed no claimed set in
production. **✅ Superseded 2026-08-19 — [`p2p-shared-set-build.md`](p2p-shared-set-build.md) § *Built — the tui app leg*:**
tui's `share_glue::run` (spawned at the `AccountStoreReady` edge, `app.rs`)
now calls `offline_share::bind_share_plane_seat` →
`CeremonyNode::bind_with_share_plane`, and `fauna_sync_engine::share_pump`
calls `set_shared_sets` — the app-side wiring (binding the transfer plane,
running the pump, registering the `ShareEndpointsSink`) and the
transfer-verdict surface (`share-serve-status` + `share-transfer-*`) are both
built and rendering on tui, the first app where the plane is live end to end.
**tui does link the
plane** (its `p2p-share` cargo feature is default-on since 2026-08-17), so
the store-safe witness's `fauna.peer.share.` absence-when-excluded claim is
proven, not vacuous (§ *Wormability walk* rule 5's discharge; the earlier
"vacuous until a root links it" finding in [`p2p-shared-set-build.md`](p2p-shared-set-build.md) § *Built — the serve/pull core*
was the state before that landing). The former "nest-side wake-signal emission
unbuilt" gap is **retired as an obligation** — the Y.1 reframe
(§ Tunnel lifecycle) ruled that under iroh nothing owes a nest-relayed wake.

**Peer-contact removal: BUILT on linux 2026-08-15, still absent
on the other six.** A 2026-08-02 review found the affordance written
three times and called zero — no app could remove a P2P contact, and no app
even rendered a contact LIST (a 2026-08-13 re-scope of the same finding: not
just no removal button, no list to hang one on). Closed on linux — at the
time the only app with an add-contact flow (the Accept-Invite dialog, since
**retired 2026-08-18** per § No pairing step, ever): a Contacts group in
`apps/fauna-linux/src/settings/p2p_tab.rs` renders `list_contacts()` as
`p2p-contact-row` items (`p2p-contact-name` + `p2p-contact-remove-button`,
user-approved ui.yaml ids, 2026-08-15), refreshed after a removal and on
every re-nav to the page. **No app has an add-contact flow anymore**; the
list trickles to the other apps together with the contacts-plane-derived
opt-in when § Inbound authorization's trigger fires, not before.
Costless to have deferred, since the contact set still gates nothing today
(§ Inbound authorization) — but now moot for linux, and the state of each
spelling below is unchanged. The state of each spelling:

| Spelling | Verdict |
|---|---|
| `PeerDb::delete_contact` (local-only) → FFI `FfiPeerDb::delete_contact` | **The survivor.** Local-only removal is the shape [`file-sync.md`](file-sync.md) § P2P peer state already ratified: peer state is device-local, and a future cross-device unification joins the private contact overlay (`../ui/contacts.md` § The private overlay), not a bespoke rail. Build the app affordance on this. |
| `fauna_peer::revocation::{revoke_contact, apply_tombstone}` + `SyncTombstone` → FFI `FfiPeerDb::revoke_contact` | **DELETED 2026-08-25**. Was a bespoke cross-device propagation rail the device-local ruling above supersedes; `apply_tombstone` had no consumer, so its tombstone reached nothing. Zero callers on any client confirmed before removal. If cross-device removal is ever wanted, it joins the private contact overlay ([`../ui/contacts.md`](../ui/contacts.md) § The private overlay — the never-built `__contacts` rail's successor), not a revived tombstone rail. `fauna_peer::contact_sync` (`SyncTombstone`/`SyncableContact`), left behind as dead code, was deleted 2026-10-02. |
| `P2pService::remove_contact` (`apps/fauna-linux/src/p2p.rs`) | **Called since 2026-08-15** — `settings/p2p_tab.rs::build_contact_row`'s remove button. Still one shell's; the other six join when the contact list trickles down at trigger time (§ No pairing step, ever). |

⚠️ The trap this recorded: three uncalled spellings was three chances to wire
the one that does not match the ratified design. Linux's own wrapper already
reached for `delete_contact` — correct under this ruling, by luck rather than
by a written rule. Now down to two spellings — the dark one is gone.

**The invite / pairing payload path is gone, and no invite QR is specified
anywhere.** `fauna_peer::qr::PeerInvite` encoded a
`fauna://peer?actor_id=…&wg_pub=…&ep=…&nonce=…` URI and was exposed over
UniFFI as `peer_generate_invite` / `peer_parse_invite`
(`libs/fauna-ffi/src/peer.rs`) — all deleted (below).
`p2p-invite-copy-btn` itself is now gone from every app that ever rendered
it — linux's whole "Invite Peer" preferences group, tui's inert copy button,
and windows' `InviteCopyBtn` — deleted 2026-08-18 along with the
Generate/nonce wiring and the Accept-Invite manual-fields dialog behind it
(§ No pairing step, ever; the element's own disposition below). The URI's
`wg_pub` was a WireGuard-era field and the WireGuard stack is DELETED (above),
while iroh's `NodeId` **is** the Ed25519 actor key — so a same-nest peer pair
needs no out-of-band key exchange at all.
Consistently, § Goal ratifies a target UX with **no pairing step**, and neither
a goal doc nor `ui.yaml` specifies an invite-QR element on any page. So the
invite surface was **WireGuard-era drift, not a half-built feature to finish**: the reframe (2026-08-10) disposed it — the `PeerInvite` URI type is
**deleted**, and `fauna_peer::qr` itself followed 2026-08-18 (§ No pairing
step, ever).
Do not build a peer-invite QR — the shared
`fauna_core::qr_matrix` encoder exists for
[identity export](../ui/settings.md) (its only consumer), and wiring it to a
peer invite would build UI for a deleted mechanism against a target UX that has
no pairing step. **Disposed (ratified 2026-08-18): § No pairing step, ever —
the Accept-Invite dialog, the nonce path, and the legacy exchange retire; the
row-drop and ui.yaml legs landed the same day, user-approved.**

Caller census for the two FFI invite faces (corrected 2026-07-13 — an earlier
revision claimed "no client calls either face", which was wrong): Android
shipped an unsanctioned drawer-level "P2P Contacts" cluster (contacts list +
QR-invite + detail screens, zero test IDs) calling both faces; it was
**removed 2026-07-13** per this section and the `ui.yaml` p2p notes (Android
hosts the peer node headlessly; the signal-receive scaffolding that survived
that removal — `P2PManager.handleInboxSignal` — was itself deleted 2026-08-23
with the signaling mechanism it parsed). The macOS
`PeerContactsView`/`PeerVM` invite section (equally unsanctioned — the matrix
above and `ui.yaml` record macOS as having no p2p surface), whose
`generateInvite` passed a throwaway keypair's **secret** bytes where the
public key belongs, was **removed 2026-07-13** in the same spirit: the view,
the `WireGuardView` it embedded, both view-models, and the `SettingsPage.p2p`
case are deleted, so neither Apple app has (or can route to) a p2p page.

**Both FFI invite faces were deleted 2026-07-15** (found consumerless by that
day's exhaustive export-surface audit): `peer_generate_invite` /
`peer_parse_invite` and the `FfiPeerInvite` record are gone from
`libs/fauna-ffi/src/peer.rs`, with the Go-binding regen committed alongside.
Crate-side, the reframe (2026-08-10) finished the thread: the consumerless
`PeerInvite` URI type was deleted, `fauna_peer::qr` kept only
`generate_nonce()` for linux's Generate button — and the pairing-surface
retirement (2026-08-18, § No pairing step, ever) deleted the module and the
button too.

**The remaining caller-less p2p FFI exports are KEPT, not dead (dispositions
written 2026-07-23).** A fleet-wide `#[uniffi::export]` caller audit (all three
name-manglings — snake_case, camelCase, PascalCase — across `apps/`, `bins/`,
`libs/`, excluding generated bindings *and* excluding `libs/fauna-ffi` itself,
then re-checked **inside** that crate for intra-crate callers) found 13 exports
in this doc's domain with zero callers anywhere. None is deletable, because this
doc's own ratified posture accounts for every one of them:

| Export(s) | Disposition |
|---|---|
| `signal_build_request` / `signal_build_accept` / `signal_build_hangup` / `signal_is_schema` (`peer.rs`) | **DELETED 2026-08-23.** These built WG-era signaling frames. Their KEEP disposition rested entirely on the parked WG fallback ("deletable only if the parked WG fallback itself is ever removed") — and it was, so the condition the disposition named is met. Under iroh, NAT traversal is intrinsic to the substrate (QUIC hole punch + relay rendezvous), so nothing owed nest relaying of these signals and no emitter was ever planned. |
| `wg_generate_keypair` / `wg_load_or_generate_keypair` / `wg_start_tunnel` / `wg_stop_tunnel` / `wg_tunnel_status` (`wireguard.rs`) | **DELETED 2026-08-23.** The KEEP rested on the impl being "parked as the dormant fallback (user, 2026-07-01)"; the same authority that parked it deleted it. `libs/fauna-ffi/src/wireguard.rs` and `wireguard_client.rs` are gone, along with the `wireguard` cargo feature. |
| `peer_contact_to_sync_json` / `peer_contact_from_sync_json` / `peer_contact_sync_namespace` (`peer.rs`) | **DELETED 2026-08-10.** The 2026-07-15 dark-rail audit's ruling stood and is now executed: peer-contact state is device-local (`file-sync.md` § P2P peer state; a future unification joins the private contact overlay, `../ui/contacts.md` § The private overlay), so this bespoke sync namespace never got wired. Removed the three exports + Go-binding regen; the `fauna_peer::contact_sync` module they rode (`SyncableContact`, and the `SyncTombstone` that `revoke_contact` — deleted 2026-08-25, see the spellings table above — consumed) was deleted 2026-10-02. |
| `nestless_validate` / `nestless_limitations` (`peer.rs`) | **DELETED 2026-08-10 (reframe, refutable ruling stood).** "nestless mode" is dissolved as a concept — [`../architecture/account-sync-plane.md`](../architecture/account-sync-plane.md) § The peer leg (user-directed 2026-08-10) rules that a nest or internet outage **is not a special mode**, so there was nothing for a user to switch. Removed linux's nestless switch (`settings/p2p_tab.rs` — the UI group, its hand-rolled `~/.config/fauna/p2p-nestless.json` persistence, and the four `nestless_*` i18n strings), `fauna_peer::nestless`, this FFI pair, and regenerated the Go binding. |

Method note for whoever re-runs this: `git grep -E` is POSIX ERE and does **not**
support `\b` — it silently matches nothing and makes every export look dead. Use
`git grep -lwE`. And an i18n **key name** (`settings.p2p_page.nestless_limitations`)
matches an export's word form without being a caller — the `nestless_limitations`
row above is exactly that false positive, which is why the earlier audit counted
16 caller-less exports where the true figure is 17.

---

## Goal

Direct device-to-device file transfer: when two devices are registered to the
same fauna nest and happen to be on the same local network (a laptop and a
desktop on home Wi-Fi), file transfers between them skip the nest entirely and
travel over an encrypted direct tunnel.

From the user's perspective nothing changes. There is no pairing step and no
network configuration, and a transfer waits on no manual toggle (linux's
`p2p-tunnel-toggle` starts only the dormant contact-plane node, a diagnostic —
§ RATIFIED 2026-08-24 finding 2; the one user control this area does owe, a
per-device participation switch, is § Per-device participation — on by
default, so nothing waits on it). The client detects that both
devices are on the same LAN, brings up the tunnel automatically, and routes
file chunks through it. Transfers are faster because they do not leave the
local network, and they stay private because the nest never sees the data. If
the tunnel goes down at any point, the client falls back to the normal
nest-mediated sync path transparently.

Cross-nest federation over a direct inter-nest tunnel is a **target-state
capability, not a shipped mechanism** — federation rides the WS-RPC channel
over TLS today (see § Cross-nest federation below; owner:
[`../architecture/federation.md`](../architecture/federation.md)).

The P2P / Direct Contacts page (`p2p` in ui.yaml) is **linux-only by ratified
target shape** (§ RATIFIED 2026-08-24 below — it does not trickle down): a
diagnostic Start/Stop over the dormant contact-plane node plus node-id and
LAN-address readouts. The WG-era copy buttons for tunnel IP, public key and
STUN endpoint, the P2P invite code and the peer-registration form are
**deleted** (§ Element IDs; § No pairing step, ever). The live cross-user p2p
surface is the share family on the `folders` page.

---

## Architecture

### Transport seam (substrate-agnostic)

P2P is structured as a **single transport seam** so the substrate is
pluggable: the shared-Rust **`fauna-transport`** trait abstracts L1/L2
only — `dial(peer, candidates) → PeerConn` / `listen() → Stream<PeerConn>`,
with `PeerConn::{open_stream, accept_stream, peer_identity, path}` — behind
which **iroh-QUIC is the only impl** since the bespoke userspace-WireGuard
stack was deleted 2026-08-23. The seam stays substrate-agnostic anyway: it
carried two impls for two months, and that is what makes the surviving one
substitutable rather than merely present. P2P is a pure optional optimization
layer over the always-available nest-mediated fallback.

Three properties are load-bearing (design ratified 2026-06-27, tracked
internally, §§2–3):

- **Symmetric.** The seam dials/listens by an Ed25519 key that may be a
  client device's **actor** key *or* a **nest** key — one uniform seam
  carries client↔client, client↔nest, *and* nest↔nest pairs (the
  private-nest↔private-nest hole-punch case included).
- **Auth is layered above the seam, never owned by it.** The transport
  only proves the remote controls the key it dialed/accepted; the caller
  applies the per-pair auth witness — actor-key for client↔client,
  **mutual nest-key** for nest↔nest (`../architecture/federation.md`),
  mixed for client↔nest.
- **The always-available fallback is not the seam's concern.** When no
  path establishes, the caller routes over the nest-mediated sync path;
  the seam yields a `PeerConn` or fails.

**Same-account device↔device carve-out (2026-08-10).** The "actor key as
node identity" rule above serves *contact*-plane pairs; same-account devices
all hold one actor key, so the account-data plane's device↔device sync leg
dials/listens by **per-device keys** (each replica's store device principal)
with same-account admission proven via `DeviceAuthorization` above the seam —
owner: [`../architecture/account-sync-plane.md`](../architecture/account-sync-plane.md)
§ The peer leg. A new seam consumer, not a change to this seam.

Every impl runs **in-process** (no daemon): iroh is quinn. The substrate is
**never a human config knob** — it is either
an artifact decision (the image ships the iroh relay sidecar or not) or a
capability the nest advertises + clients auto-negotiate (the `relay` token),
with transparent fallback. A user never picks the substrate.

**Corollary — substrate enablement is not an admin toggle and never becomes
one (ruled 2026-08-10, refutable; answers a recurring question).** The rule
above forbids a human choosing the substrate, so a nest-side "enable this
substrate" switch fails the one-surface test — *would a user or admin ever
want to choose this?* No: enablement is an **artifact/capability decision**
(bucket 1), expressed as capability advertisement + auto-negotiation.

The worked example was the nest's `[wireguard]` config section
(`WireguardSection.enabled`), read only behind a non-default cargo feature
that shipping images compiled out, whose only live setter was an e2e fixture.
**It was deleted outright 2026-08-23 with the rest of the stack** — the
ruling's conclusion held all the way to deletion: it was never converted into
app UI, because there was never a human choice inside it. The separate,
genuinely-human choice — per-device p2p participation (wormability rule 5's
"no listener when off") — is NOT the `p2p` page (that page's toggle drives
linux's dormant diagnostic node, § RATIFIED 2026-08-24 finding 2; the claim
that participation was "already app surface" there stood here until
2026-09-25 and was false): it is the devices page's
`device-p2p-participation-toggle`, § Per-device participation.

### The relay — who runs it, what it carries, who may use it (ruled 2026-10-01, refutable)

Three questions the corollary above left open, answered together because the
feature page that promises the relay cannot be true until all three are.

- **Who runs it: the released image.** By the
  corollary the relay runs because the artifact runs it — on every nest with
  a public name of its own (the condition `iroh_relay_url` is already gated
  on), with nothing for a person to switch. **Built 2026-10-03****:** there is no flag at all. The image's relay service is
  always up; what decides whether it serves is the nest, over the relay's own
  sidecar channel — a nest with no public name hands the relay no
  certificate, so the relay stands by (connected, listening on loopback,
  able to present nothing), and the claim that gives the nest its name
  reaches the relay as a push and it serves from that moment, no restart.
  The nest in turn knows its deployment runs a relay because one is
  connected to it, and advertises the capability, the relay URL, the
  `relay.<apex>` DNS row and the certificate name on that fact alone — so a
  nest with no relay binary beside it (a desktop-served nest) never
  advertises one. The `services.json` `iroh_relay` flag and the
  `fauna-service-watcher` that polled it are deleted: the flag was a
  switch with no legitimate hand to throw it. (This order — access rule
  first, then on — is the ruling's: a forwarder open to anyone is not
  shipped switched on.)
- **What it carries: the rendezvous, and the encrypted packets themselves
  when no direct path exists.** The relay helps two endpoints find a direct
  path and, when none can be made, forwards their QUIC packets — encrypted
  end to end between the two endpoints, so the relay cannot read them, holds
  no key for them and stores nothing. That is what "never a data middleman"
  means wherever a goal doc says it of the relay: never a party to the data,
  never a store, never load-bearing (the nest-mediated path is always there
  when the relay is not). It never meant that no traffic passes through it;
  the code has configured an ordinary relay from the first slice
  (`IrohTransportBuilder`, the relay attached to a dial only when
  `PathCandidates::relay_available`).
- **Who may use it: the devices of the nest's own members, and nobody
  else.** The relay admits an endpoint only when its key is one the nest
  knows — a device enrolled for one of its accounts (the peer leg's
  per-device keys) or a member's actor key (the contact plane's node
  identity). Any other key is refused: a relay that forwards for strangers
  is a free packet forwarder on every nest, a bandwidth and abuse liability
  with no benefit to its members, and there is nothing in it for an admin to
  choose. The relay's *address* is public by construction (`relay.<domain>`,
  a function of the nest's name), so its presence on the anonymous
  `fauna.nest.info` reply discloses nothing; wormability's "endpoints learned
  only through authenticated channels" speaks of peers' endpoints, which the
  relay's address is not. **Across nests: nobody.** A device whose account
  lives on *another* nest — a contact, a co-member of a shared set, a
  custodian — is refused like any other key the nest does not know, whoever
  on this nest it shares something with (*Across nests*, below).
  **Built 2026-10-03****:** the relay holds no
  list. It asks the nest about every endpoint that connects to it, over its
  sidecar channel, and the nest answers from a set of sources — today those
  two — so a later widening adds a source and rewrites nothing. It fails
  closed: with the channel down or on any error the endpoint is refused.
  The question is asked when an endpoint connects; a device removed while
  connected keeps its relay connection until it next reconnects, and what
  it can still reach is bounded by the peers' own admission
  (`DeviceAuthorization`, above the seam), never by the relay.

**Across nests — nobody (ruled 2026-10-01, refutable).** The members-only rule left one question open on purpose: is a device whose account lives on another nest ever admitted, and on what proof? **No.** Two people whose accounts live on different nests reach each other directly — on one network, or at an address learned over an authenticated channel that happens to be routable — or through their nests, and never through either relay. Four findings carry the ruling, each read from our own code:

1. **The relay can give such a pair nothing the nest does not already give it, because the relay is the nest's own box.** Where the set's home nest stores the set, every member's device, whichever nest its account lives on, already pulls the sealed bytes from that nest directly ([`../architecture/federation.md`](../architecture/federation.md) § Cross-nest shared folders + channel append; the byte plane's trust root is [`../architecture/security.md`](../architecture/security.md)'s). Where it does not — a metadata-only folder — the nest's own *relay serving* hands a holding seat's bytes to the reader, gated per folder by rows that nest wrote ([`file-sync.md`](file-sync.md) § Relay serving: a different mechanism from this section's relay, ratified and unbuilt, its cross-nest leg included). A transfer through the peer relay would move the same ciphertext through the same machine, in and out — twice the bandwidth of serving it from the store, and no less than relay serving costs — and when that box is down its relay is down with it. Admission would add no availability.
2. **What a relay could add is a direct path between two home networks, and our build does not make one.** No device advertises an address as the internet sees it (`fauna_sync_engine::peer_leg`'s `compose_facts` leaves `public_addrs` empty, and the share advertisement does the same), and the relay we run serves the relay protocol only, with the substrate's address-discovery server off (`fauna_iroh_relay::spawn_relay`). That is a fact about pairs on one nest too, and it is measured there: two devices behind two NATs stay on the relayed path for the life of the connection (§ NAT hole punching). *Narrowed 2026-10-05:* serving address discovery (*Address discovery, and what rides a relayed path*, below) is ruled for pairs on one nest and leaves this finding standing for a pair that shares no relay — the exchange that makes a direct path runs through a relay both endpoints share, and two members of different nests share none (the two-relays scheme, finding 3).
3. **Every way of admitting an outsider costs the nest the same things, and none can hide what carrying reveals.** An admitted key can hold a connection open, send to any connected key it can name at whatever rate the relay allows (the nest pays for both directions and the target's link pays too), learn whether a named key is connected right now, and, with a second admitted key, use the nest as a forwarder between two people who are not its members. The schemes weighed:
   - *The member vouches* — its device tells its own nest which outside keys to admit, for a bounded time. New nest state, a kind to write it, an expiry to enforce, and the nest holds a list of the outsiders each member deals with.
   - *The outsider carries a capability the member minted, which the relay checks without learning who minted it.* The unlinkability is void the moment a packet moves: the relay forwards by destination key, so it sees which member the outsider talks to. It would buy blind-credential machinery for no privacy, and leave abuse with no member to answer for it.
   - *Both relays, each side through its own.* As built, a dial names one relay — the dialer's own nest's, never one a peer advertised — and reaches only an endpoint connected to that same relay (`IrohTransport::dial` over a one-entry relay map), and nothing in the relay we run forwards to another relay. So each device staying on its own relay needs a nest-to-nest packet carrier that does not exist and would make every federated nest's members users of this relay; otherwise each device joins the other nest's relay, which is the vouching scheme twice.
   - *What the nest learns* is therefore not a property of the scheme. A relay learns the pair, the times and the volume by carrying them, whoever vouched and however blindly.
4. **Nothing that works is taken away.** The contact plane's seat binds with no relay at all today and advertises none ([`p2p-shared-set-build.md`](p2p-shared-set-build.md) § *Built — the tui app leg*), so no cross-user pair uses a relay yet, on one nest or on two. *Ruled 2026-10-05:* the seat takes its nest's relay once discovery is served, for pairs on one nest only (*The cross-user seat and the relay*, below); across nests nothing changes.

**The residual, stated.** Two people on different nests, on different networks, whose devices hold no routable address for each other, move a shared set through the set's home nest: at that nest's speed, on that nest's bandwidth, and not at all while it is unreachable. A custodian whose account lives on another nest is in the same position toward the owner's devices. Nothing fails — the nest path is the contract (§ 5 Routing and fallback) — and a direct path is all that is forgone.

**What would refute it,** and the shape a widening would take. (i) A plane between people on different nests whose traffic the nest path cannot carry — live media is the example — makes finding 1 false for that plane. (ii) A measurement that direct paths between home networks are makeable in our build *and* spare nests bandwidth that matters. The cheapest admission the weighing found, recorded so the next pass starts from it and not ruled in: a set's home nest admits exactly the keys it already lets read that set (its own `channel_foreign_members` rows), for as long as the row stands — no vouching message, nothing learned that the nest does not hold, and evicting the member severs it. It would also need a transport that can meet at a relay which is not the device's own (the seat itself takes its own nest's relay once discovery is served — *The cross-user seat and the relay*, below). The members-only build answers "is this key known?" from a set of sources, so one more source is not a rewrite.

**Address discovery, and what rides a relayed path (ruled 2026-10-05; ruling 1 is refutable by the measurement it orders, the rest at build time).** § NAT hole punching measured that two devices behind two NATs never leave our relay, and asked whether to serve address discovery or to rule the relay a forwarder. Four rulings, read from our own code and the measurement — never from the dependency's source:

1. **Serve address discovery — once it is measured to make direct paths.** The relay's whole worth is the direct path it helps make. A relay that only forwards carries every byte of a pair's traffic through the nest's own box, and so can never beat the nest path: that runs over the same box and the same links, and for a nest-resident folder carries the bytes anyway — a forwarding relay costs the box the same bytes twice for nothing, and the honest form of "rule it a forwarder" would be to take the relay out of the same-account leg altogether. The substrate makes a direct path from an address each endpoint learns as the internet sees it, and the one place a device of ours can learn that is its own nest's relay, which already runs on every nest with a public name: so the relay serves the substrate's address-discovery server beside the relay protocol (`fauna_iroh_relay::spawn_relay`'s `quic` half, which `run` turns on through `production_options` — built 2026-10-05). **Measured first, shipped second:** `just p2p-nat-probe` grows a variant whose relay serves address discovery, the cone, symmetric and mixed pairs are measured again under it, the numbers are written into § NAT hole punching, and the production relay serves discovery only when they show at least the cone pair — the common home router — going direct. A measurement in which no pair goes direct refutes this ruling in full: the relay is then a forwarder with no path to offer, and whether the same-account leg keeps it at all is a fresh design pass, not a build. *Measured 2026-10-05:* the cone pair goes direct 0.3 s after the dial, symmetric and mixed pairs stay relayed (§ NAT hole punching) — the ruling holds, and the production relay serves discovery.

2. **What serving it costs, and where each cost lands.** One UDP port on the relay's box, bound by the relay process itself on every interface — the SNI router fronting 443 passes TCP by server name and cannot front UDP — published by the deployment artifact (`docker-compose.yml`), opened by the guide's firewall step (`docs/guides/nest-internet-setup.md`), and nothing else. The port number is a Rust constant in the relay crate, the substrate's own default unless the measurement says otherwise, and no human chooses it ([`../principles.md`](../principles.md) § One configuration surface, bucket 1); the relay URL stays the only fact the nest advertises, the endpoint deriving the discovery port from it. A nest with no public name hands its relay no certificate and the relay stands by, so a home box behind a NAT exposes nothing new; the public nest of the home-with-public-relay bundle has a public name and runs the standard artifact, so it gets the port with every other public nest. The one anonymous surface it adds — a QUIC endpoint that tells any caller its own address as the relay sees it, and nothing about anyone else — is recorded in [`../architecture/nest/network-exposure.md`](../architecture/nest/network-exposure.md) by the build that opens it.

3. **No device advertises an observed address through the nest: `public_addrs` stays empty, for good.** The substrate exchanges each endpoint's observed address inside the connection it is making, over the relay both ends share, encrypted to the one admitted peer being dialed, and keeps it nowhere. A fauna-side advert through the device-endpoints entry would spell the same fact a second time with none of those properties: it rides the fleet-only scope to every sibling, and a merged entry is never forgotten ([`../architecture/account-sync-plane.md`](../architecture/account-sync-plane.md) § The peer leg → *Discovery*), so it would make a device's public-address history — where that device has been — durable state on every replica of the account. The wire field stays (additive-everywhere), read as it is read today (a safe candidate if ever present, `dial_target_from`) and written empty by every producer; `peer_leg.rs`'s note that the field waits for an observation leg is superseded by this ruling and says so.

4. **Bytes ride direct paths only; a relayed path carries what it carries today, and the nest path carries the bytes.** Whatever discovery measures, some pairs stay relayed — two symmetric gateways, carrier-grade NAT, a hostile network — and for them ruling 1's arithmetic holds: every byte through the relay is a byte the nest path would carry over the same box, once. So every pump on the seam — the same-account peer leg's block pull, the same-account chunk transfer, the share pump's page fetch — moves bytes over a connection only while `PeerConn::path()` is direct (`lan` or `wan_direct`), re-read before each pull because a relayed connection can turn direct while the substrate keeps punching; over a relayed path the leg admits and walks rows as it does today — small, and unchanged — and leaves the bytes to the nest path it already runs (the nest's store, or relay serving for a metadata-only folder — [`file-sync.md`](file-sync.md) § Relay serving), which is never a failed transfer (§ 5 Routing and fallback). The one exception is the nest path being unavailable to this device: then a relayed connection that already stands carries bytes too, since the alternative is no transfer at all, and "never a party to the data" holds exactly as before. It is only an already-standing connection because the relay asks the nest about every endpoint that connects and refuses while the nest is unreachable (*Who may use it*). None of this is a path choice — the substrate still selects the path inside `dial` (§ Don't do these) — it is what a pump does with the path it was given. *Built 2026-10-05:* `fauna_transport::bytes_may_ride` is the rule's one spelling, taking the connection's path and the pump's own nest fact (`NestPath`); `PeerChannel::path` reads the held connection live, so each pull re-reads it. The peer leg's block pull (`fauna_peer_sync::pull_missing_blocks`, before every want-list request; its nest fact is the pass's own fleet walk) and the share pump's page fetch (`fauna_sync_engine::share_pump::pull_set_from_peer`, before every page's bodies; its nest fact is the pass's transfer-policy read, an answer without the feature reading as unavailable) both call it, count what they leave (`DialPass::blocks_relay_deferred`, `SetPullOutcome::relay_deferred`) and walk their rows unchanged; a deferred share body leaves its row a placeholder and the pull cursor below it, so a later pass or the nest path lands it. The same-account chunk transfer is to call the same gate. The custody leg's content pull is none of the three pumps: its nest is the owner's, whose reachability it does not hold, so it passes `Unavailable` and pulls on any path as before.

**What would refute these.** Ruling 1: its own measurement (above). Ruling 2: a deployment the standard artifact does not reach that needs the port — it gets the port the way it gets 443, by the same artifact, never by a knob. Ruling 3: a direct path the substrate's exchange cannot make but an advertised address could — none is known; if one appears it is a design pass, since the history it would create is the objection, not the field. Ruling 4: a plane whose bytes the nest path cannot carry — live media between two devices of one account is the example, as it is for *Across nests* (i) — would ride a relayed path by its own ruling.

**The cross-user seat and the relay (ruled 2026-10-05; rulings 2–4 refutable at build time, the whole by ruling 1's measurement above).** The contact plane's one actor-keyed seat per session (§ Offline share initiation → *One seat per session*) serves two doors: the co-present ceremony, which must never gain a dial path the user did not choose, and the cross-user share plane, whose *Discovery* bullet (§ Cross-user shared-set transfer) has said since 2026-08-10 that two members on one nest and on different networks meet via the relay. The seat binds with no relay and advertises none, so *Who may use it* admits a member's actor key that nothing presents. Four rulings, read from our own code:

1. **The seat takes its nest's relay — once the relay serves address discovery, and not before.** A relay offers the share plane exactly what ruling 1 above says it offers the same-account leg: the direct path between two home networks it helps make, and nothing else of worth — over a relayed path the share pump moves no bytes (ruling 4; the nest's store, or relay serving, carries them) and the rows it would walk reach a member through the nest anyway, so a forwarding-only relay gives the seat nothing and costs it a standing connection. The seat therefore stays relay-less until the production relay serves discovery, and the build that binds it with the relay lands after that. The relay URL is the live-or-cached own-nest `iroh_relay_url` the same-account leg already hands its transport builder (`fauna_sync_engine::peer_leg`, `PeerLegFactoryInputs::relay_url`), read at the bind by whichever door binds; a seat bound without one stays without one for its life, and its advertisement says so.

2. **The ceremony keeps its rule by the gate the seam already has, not by a relay-less endpoint.** The relay is configured on the endpoint builder and *attached* to a dial only when that dial's `PathCandidates::relay_available` is set (`IrohTransport::dial`); every ceremony dial composes its candidates from the spoken code alone — its LAN candidates and nothing else (`fauna_client_capabilities::group_ceremony_node`'s `dial_with_candidates`, `fauna_sync_engine::share_probe::probe_set`) — so its `relay_available` is false by construction, and a relay on the endpoint hands the ceremony no dial path. The refusal of the relay as the ceremony's *addressing* (§ Offline share initiation, the resolved gap's option (c)) stands: the ceremony's dials never carry it and the ceremony still needs no nest. What the bind adds is a listener reachable through the relay by an admitted key — this nest's own members, who already reach the seat on a LAN by knowing the same key; who may *start* a ceremony is the receive act and who may *pull* a set is the roster consult, both above the seam and both unchanged. So the one seat stays one: a second, relay-bound endpoint for the share plane would be a second listener on one NodeId, exactly what the session slot exists to prevent, and is the fallback shape only if the measurement below fails.

3. **A share dial attaches a relay only toward a member advertising the dialer's own.** The advertisement carries the advertiser's seat's relay URL (the publish half tells the truth about its transport, as today), and the reader's `relay_available` follows the shared `dial_target_from`; the share plane adds one comparison — the advertised URL must equal the URL this seat was bound with — so a dial attaches a relay only toward a member of this nest. A different URL is a member of another nest, whose relay refuses this key and whose case *Across nests* finding 3 already rules out (the two-relays scheme); that dial goes with its direct candidates only, and the nest path carries the set. *Who may use it* changes nothing: member actor keys stay admitted, and this build is what first presents one.

4. **One actor key, several devices — accepted as it stands.** The share leg holds one live counterpart connection per remote actor ([`p2p-shared-set-build.md`](p2p-shared-set-build.md) § Build contract) and an advertisement supersedes per (set, member); at the relay a member's several devices present one key. What our relay does with two connections under one key is measured by the build with our own binary — never read from the dependency's source — and recorded in the build ledger; whichever device a relayed dial reaches serves a set only from its own serve map, every chunk self-verifies, and the nest stays a source, so it is a liveness matter in a best-effort plane, never a correctness one.

**What the build measures first, and what refutes these.** Before the seat binds with a relay, `fauna-iroh`'s own tests show that an unreachable relay URL on the builder leaves a direct-only dial by LAN candidate as fast and as sure as a relay-less bind's — the fear `offline_share.rs` recorded ("an unreachable relay is worse than a direct dial that simply fails"), made a number; a failure refutes ruling 2 and takes the fallback shape it names, with ruling 2 rewritten. Ruling 1's own measurement refutes the whole: no pair going direct makes the relay a forwarder, and the seat stays relay-less with whatever the same-account leg's fresh design pass decides. Ruling 3 is reopened only by a transport that can meet at a relay which is not the device's own, together with *Across nests* (ii), and by nothing else.

### Inbound authorization for `fauna.peer.*` (ratified 2026-08-02)

*"Auth is layered above the seam"* (above) says **where** the witness is applied;
this says **what** it is for client↔client kinds, and **when** it becomes
mandatory. Written before the first data-plane handler exists, so that handler's
author inherits a decision instead of re-deriving one.

- **`fauna.peer.node_info` answers any dialer, deliberately.** It is the
  protocol-version/capability probe that must answer *before* any witness can be
  evaluated, and reaching it already requires the node's Ed25519 `NodeId`. Its
  residual disclosure is a presence oracle to someone who already holds that key
  — accepted. It carries protocol version and a display name — empty everywhere
  today (linux passes `String::new()`; `libs/fauna-ffi/src/peer.rs` takes it as
  a caller parameter, so an app can populate it without any new field — see
  trigger 2 below before doing so).
- **Every other `fauna.peer.*` kind authorizes against the P2P contact set**
  — the peer-contact rows with `p2p_enabled` (`PeerDb::list_p2p_enabled`,
  `libs/fauna-peer/src/contact.rs`). The check belongs in the handler or in a
  dispatch wrapper introduced with the first such kind, **not** in
  `PeerHandlers` itself: per the seam's own rule the routing layer proves key
  possession and nothing more.
- **That contact set is NOT an authorization set today** — it gates nothing, and
  no code path consults it for admission. It becomes one at the trigger below,
  and not before.
- **Carve-out (2026-08-17): a kind with a stronger per-kind admission of its
  own satisfies this section through that admission.** The share leg's
  `fauna.peer.share.*` kinds admit by the set's M2 roster — membership only
  reachable through the owner's gated share + the recipient's contact-gated
  Welcome accept — which is a stronger, revocable, per-set authorization than
  the WG-era `PeerDb` rows; § Cross-user shared-set transfer → *Build
  contract* owns the reconciliation (that section wins on divergence, per its
  own header). The `PeerDb` contact set stays the stated admission set only
  for future `fauna.peer.*` kinds that arrive with no per-kind admission.

**The trigger that makes the check mandatory** — whichever comes first:

1. registering any `fauna.peer.*` kind beyond `node_info` that serves or accepts
   user content or per-user state; or
2. `node_info` carrying any **populated** user-influenced field — the existing
   display name included, whether an app populates it by user choice or by
   default (a populated field turns the presence oracle into a profile leak;
   counting only *added* fields would let the FFI-parameter display name slip
   through unmet).

At that moment the admission check and a **working removal affordance** must land
together — an authorization set the user cannot remove from is not revocable, and
[`../principles.md`](../principles.md) § User always controls their data
requires revocable. See § Implementation status today for what is dark right now.
Row **provenance** at that moment is ruled by § No pairing step, ever (below):
the add-affordance derives rows from the contacts plane, never from a pairing
gesture, so every admitted `actor_id` is real by construction.

### No pairing step, ever — the WG-era pairing surface retires (ratified 2026-08-18; the row-drop and ui.yaml legs landed the same day, user-approved)

**The question this resolves**: linux's Accept-Invite
dialog decodes the pairing **nonce** and stores it as `PeerContact.actor_id`
(`apps/fauna-linux/src/settings/p2p_tab.rs` — its own comment says "pseudo
actor_id for now"), and the actor-proving exchange exists in two spellings
with zero production callers. The two proposed fixes — (a) drive
`fauna.peer.exchange` over a live channel during accept, or (b) carry a
signed identity assertion in the invite payload — are **both rejected as a
false fork**: each would *finish* a surface this doc already rules "dormant
WireGuard-era drift, not a half-built feature to finish" (§ Implementation
status today), and each re-solves a problem the ratified design has already
solved twice, better:

- **Nest-reachable counterparts:** the contacts plane
  ([`../ui/contacts.md`](../ui/contacts.md) — knock lifecycle, federated
  discovery, both-endpoints-chosen `ContactStatus` edges) establishes real
  actor ids on all 7 apps today.
- **Co-present offline counterparts:** § Offline share initiation's ceremony
  delivers **mutual actor-key verification** over the PT-1b channel with the
  in-person compare affordance (rule-A-approved `offline-share-*` /
  `offline-receive-*` elements).

And under the adopted iroh substrate a pairing *proof* is **redundant by
construction**: the `NodeId` *is* the Ed25519 actor key, proven intrinsically
by the QUIC handshake (PT-1b — the Y.1 kind's own rustdoc concedes an iroh
peer omits the WG key for exactly this reason). Holding a merely *claimed*
actor id is impersonation-safe: dialing key X can only ever reach the holder
of X's secret. The one real question — how a user *learns* a counterpart's
actor id trustworthily — is answered by the two sources above; a
paste-a-nonce pairing gesture answers it strictly worse than either.

**Dispositions:**

| Surface | Disposition |
|---|---|
| linux Accept-Invite dialog (nonce / WG-pubkey / name / endpoint — four WG-era fields) + its pseudo-id `add_contact` write (`settings/p2p_tab.rs`) | **DELETE.** The dialog has no ui.yaml ids, and e2e seeds contacts via the `p2p_seed_contact_for_test` fixture command, never the dialog — zero coverage lost. The seed command stays (fixture setup, conventions point 8). |
| The invite-code Generate wiring + `P2pService::generate_invite_nonce` + `fauna_peer::qr` | **DELETE.** Landed together with the `p2p-invite-copy-btn` element removal below (2026-08-18) — the element and its retired-permanent-no-op posture are gone, not just the wiring behind it. |
| Legacy `fauna_peer::exchange` (length-prefixed JSON) | **DELETE.** Consumer census (re-verified 2026-08-18): its own `libs/fauna-peer/tests/integration.rs` only. The earlier "the Y.1 kind is the survivor; the legacy retires with the slice-7 consumer rewire" note resolves as: **neither spelling runs in production** — there is no consumer to rewire, so the legacy module simply deletes. |
| Y.1 `fauna.peer.exchange` kind (`libs/fauna-protocol/src/peer.rs`) | **KEPT as a wire type, unlike its FFI siblings (2026-08-23).** Its remaining job was carrying the WG x25519 registry binding, which an iroh peer never needs, so the WG deletion left it with no live job — but a *kind* is not an FFI export: removing one is subtractive on the wire, and the additive-everywhere rule forbids that within a major ([`../architecture/version-compatibility.md`](../architecture/version-compatibility.md)). The `signal_build_*` FFI exports could go because nothing on the wire depends on them; this stays, decoding into an unused reply. Its `wg_public_key` field is **gone since 2026-08-24** — removed ahead of the major bump under the user's population ruling, and unobservable to a deployed peer either way ([`../architecture/version-compatibility.md`](../architecture/version-compatibility.md) § Dimension 2). |
| `PeerContact` row provenance (target state) | Rows may hold only actor ids established by the contacts plane or the § Offline share initiation ceremony. At trigger time (§ Inbound authorization), the add-affordance is a **per-contact p2p opt-in derived from the contacts plane** — `PeerDb` stays the device-local admission cache so the check needs no registry lookup. Build detail lands then; no pairing gesture, ever. |
| Existing pseudo-id rows on alpha boxes | **DROPPED (user-approved 2026-08-18)** under the alpha deletion carve-out ([`../principles.md`](../principles.md) § No user-data loss). `PeerDb::open` (`libs/fauna-peer/src/contact.rs`) ran a one-shot `DELETE FROM peer_contacts` on first open past this change (`user_version`-gated), wiping the linux p2p contact-list rows added via the deleted dialog — random-nonce ids, undialable, gated nothing; the wipe was removed in the 2026-09-30 compat-remnant sweep ([`../architecture/compat-remnant-sweep.md`](../architecture/compat-remnant-sweep.md)), since no store written before it survives the baseline reset. The user re-adds real contacts when the opt-in affordance lands. |
| ui.yaml `p2p-invite-copy-btn` | **REMOVED (user-approved 2026-08-18, rule A).** With the invite concept dissolved the element had no future. Deleted from ui.yaml and every app that rendered it (linux's whole "Invite Peer" preferences group, tui's inert copy button + `Action::P2pCopyInvite` gesture, windows' `InviteCopyBtn`); `test_invite_copy_is_noop` retired with it. |

Consequence for the per-app trickle-down: with the dialog gone **no app has an
add-contact flow**, so the contact-list surface (`p2p-contact-row` family,
linux-only today) trickles to the other apps **when the trigger fires**,
together with the contacts-plane-derived opt-in — not before, and not as a
mirror of a pairing dialog.

### The deleted WireGuard impl (historical)

Until 2026-08-23 the seam had a second impl: a fully userspace WireGuard stack
(boringtun for the Noise engine, smoltcp for an in-process IP stack) with no
kernel module, no TUN device and no privileges — plus a **peer-registration
ceremony** in which a device uploaded its WireGuard public key over
`fauna.wireguard.peer.register` and the nest replied with a tunnel IP from its
own subnet, its endpoint and its public key, keeping a per-nest peer registry
that peers queried to find each other.

**All of it is deleted** — the crates (`fauna-wireguard`, `fauna-client-wireguard`),
the `fauna.wireguard.peer.*` and `fauna.admin.wireguard.*` kinds, the
`wireguard_peers` table, the nest's `[wireguard]` and `[stun]` config sections,
the STUN server, and every app surface that fronted them (user-directed: "we are
using iroh for now and there is no point in compiling wireguard").

Two things worth keeping from it, because they explain the shape of what is
left:

- **The registration ceremony has no iroh equivalent, by construction.** An
  iroh `NodeId` *is* the Ed25519 actor key, so there is nothing to upload and no
  registry to consult — which is why deleting registration removed a step rather
  than leaving a gap. This is the same reason § No pairing step, ever holds.
- **The LAN arithmetic outlived its host** and now lives in
  `fauna_peer_sync::lan` (§ LAN detection). It was never WireGuard-specific:
  it compares advertised addresses, and any substrate can use it.

The removal is recorded as an applied instance in
[`../architecture/version-compatibility.md`](../architecture/version-compatibility.md)
§ Dimension 2, under the user's population ruling that no deployed client
artifact still called those kinds.

---

## LAN detection

Implemented in `libs/fauna-peer-sync/src/lan.rs` (moved there 2026-08-23 from
the deleted `fauna-wireguard/src/endpoint.rs` — it is address arithmetic, never
WireGuard-specific). `discover_lan_candidates()`
enumerates every non-loopback private-range IPv4 address on local interfaces;
`is_private_ip()` classifies RFC 1918 (`10.0.0.0/8`, `172.16.0.0/12`,
`192.168.0.0/16`) plus link-local (`169.254.0.0/16`); `should_attempt_lan_probe()`
returns `true` when at least one local address shares a /24 subnet
(`is_same_subnet()`) with at least one peer-advertised LAN address.

No broadcast, mDNS, or network scanning is involved — the check is purely
arithmetic against the addresses a peer already advertised (§ Tunnel
lifecycle → 1. Discovery). Consumed by `fauna_peer_sync::discovery::sibling_dial_targets` (the discovery
feed below), and linux's P2P settings page calls `discover_lan_candidates()`
directly to render its LAN-addresses row. Two former consumers went with the
WireGuard stack 2026-08-23: `fauna_peer::path_cascade::attempt_cascade` (the
client-side cascade — iroh does its own path selection) and the nest's
WireGuard-fallback advertisement. The earlier `fauna-peer/src/lan.rs` duplicate
(`is_likely_lan_peer()`) was zero-caller dead code, removed.

---

## Tunnel lifecycle

> **Reframed over the seam (Y.1 reframe, 2026-08-10 — refutable at build
> time).** § 1 (discovery) and § 5 (routing/fallback) state current target
> semantics. §§ 2–4 and 6 document the **parked WG fallback's mechanism
> only** — under the adopted iroh substrate none of them is owed: connection
> establishment is a seam `dial` with **caller-supplied candidates**
> (same-account: the replica's device-endpoint entries,
> [`../architecture/account-sync-plane.md`](../architecture/account-sync-plane.md)
> § The peer leg / T5; cross-user: endpoints learned over authenticated
> channels, § Cross-user shared-set transfer), with hole punching and relay
> rendezvous intrinsic to the substrate. **No nest-relayed wake or signaling
> is owed.** If "prompt a peer to sync now" ever becomes a live need, it is
> the account plane's generic change nudge (`fauna.sync.changed` shape —
> charter § Nudges and backstops), never a bespoke p2p wake.

### 1. Discovery

Dial candidates come from the account plane, not a nest-queried peer list:
each device publishes its own device-endpoint entry (NodeId, LAN + public
addresses, relay URL) that every sibling replica reads directly from its own
copy of the store — full mechanism owned by
[`../architecture/account-sync-plane.md`](../architecture/account-sync-plane.md)
§ The peer leg → Discovery (T5); `fauna_peer_sync::discovery::sibling_dial_targets`
is the reader. LAN candidacy on those cached endpoints applies this doc's own
arithmetic (§ LAN detection) — no broadcast, mDNS, or network scanning.

The **cross-user** leg reaches the same shape from a different source: a
shared set's members advertise over the set's own authenticated channel rather
than through any account plane both sides could read, and the cached rows
compose through the *same* `dial_target_from` hygiene. Owner: § Cross-user
shared-set transfer → *Discovery* and [`p2p-shared-set-build.md`](p2p-shared-set-build.md) § *Built — the discovery carriage*.

### 2. Wake *(retired obligation — WG-era; wire frame stays registered, dormant)*

In the WG-era flow, the initiating device asked the nest to forward a wake
notification to the target device. The wire shape is the **typed push frame
`PushEvent::PeerWake`** (kind `fauna.peer.wake` —
`libs/fauna-protocol/src/push_events.rs`), carrying the requester's actor id,
endpoint, and a nonce; there is no legacy JSON framing. **The reframe
(2026-08-10) retired the emitter obligation**: no nest-side handler emits the
frame, and none is owed — under iroh a reachable peer is dialed directly with
candidates the caller already holds, and an unreachable peer converges later
(best-effort is the contract). The decode arm stays registered for wire
compatibility (additive-everywhere); it is dormant, not a gap.

### 3. Handshake *(dormant)*

The two devices complete the substrate's own handshake directly, using the LAN
IPs rather than the nest as an intermediary. Under iroh that is QUIC/TLS 1.3
with the Ed25519 `NodeId` as the authenticated identity, so the handshake
*is* the identity proof (PT-1b) — there is no separate registry lookup. (Until
2026-08-23 the alternative was a WireGuard Noise handshake driven by
`boringtun`, which needed the nest's peer registry to map a key to an
endpoint; § The deleted WireGuard impl.)

### 4. Tunnel established *(dormant)*

Once the handshake succeeds, there is an encrypted tunnel between the two
devices. No further traffic touches the nest for transfers routed through
this tunnel.

### 5. Routing and fallback

Client-side path selection was a caller-driven cascade,
`fauna_peer::path_cascade::attempt_cascade`, which tried paths in order
and yielded `CascadeResult::{LanDirect, WanDirect, Relay, Failed}`. **It was
deleted 2026-08-23 with the WireGuard stack**, and nothing replaces it: iroh
performs its own path selection inside `dial` (LAN, hole-punched WAN, relay),
surfacing the outcome as the seam's `PathKind` rather than as a caller-driven
cascade. On failure — or on later degradation — the caller still routes over
the always-available nest-mediated sync path, exactly as before. (The nest-side
`ConnectionRouter` in `wireguard/auto_switch.rs` went with it.)

Fallback is **automatic and silent — hard-coded** (ruling 2026-07-10, deleted
in code 2026-07-12). There is no fallback-policy knob, per the no-operator
invariant: a transfer never fails because a direct path is unavailable. The
vestigial `fauna_wireguard::config::FallbackPolicy` enum (Allow/Warn/Deny) and
its `Deserialize` config shape had zero production wiring (configuration-file
theatre) and were gone well before the stack itself. If a user-facing "prefer
direct paths" choice ever becomes a real need,
it is a app-UI choice persisted in nest state — never a config file.

### 6. Keepalive *(dormant)*

Liveness is the substrate's own concern. iroh/quinn runs QUIC keepalives and
surfaces path changes itself, so a dropped path is detected without a
fauna-side timer. (The deleted WireGuard impl needed one: a 250 ms tick drove
`boringtun`'s keepalive and handshake state machine.)

---

## NAT hole punching (non-LAN P2P)

**NAT traversal is intrinsic to the substrate.** iroh performs QUIC hole
punching plus relay rendezvous (the self-hosted relay sidecar, § Implementation
status today; what the relay carries when no direct path can be made, and for
whom: § The relay) inside `IrohTransport::dial`. There is no fauna-side signaling
mechanism, no nest coordination step, and nothing for an app to drive: a dial
either establishes a path or fails, and on failure the caller routes over the
nest-mediated fallback exactly as in the LAN case.

*Superseded 2026-10-05 by the build that serves address discovery: the next paragraph is the shipped result; this one records the relay-protocol-only shape, which `just p2p-nat-probe relay 60 off` still reproduces.* **Measured in our build (2026-10-05): two devices behind two different NATs never leave the relayed path.** The paragraph above states what the substrate can do; *as we configure it*, a pair behind two address-translating gateways connects through the relay in about three seconds and stays relayed for the life of the connection — a minute under a steady ~800 KiB/s stream, with the path read off `PeerConn::path()` on both ends — whether both gateways keep one outside port per inside socket (a port-restricted cone, the common home router), both pick a new port per destination (symmetric), or one of each. The same pair behind gateways that route without translating turns direct within a fraction of a second, so the testbed carries direct traffic and the probe sees the switch. Both configuration facts predict the result — no device advertises an address as the internet sees it (`compose_facts` leaves `public_addrs` empty — before 2026-10-05 for want of a leg to observe one, since then by ruling 3 of § The relay → *Address discovery, and what rides a relayed path*), and the relay runs without the substrate's address-discovery server (`fauna_iroh_relay::spawn_relay`), so neither endpoint learns an address the other's gateway lets through; serving address discovery changes it for the cone pair (next paragraph). **Witness:** `just p2p-nat-probe` — `scripts/p2p-nat-probe.py` drives `bins/fauna-iroh-relay/examples/nat_probe.rs` (the production peer-leg endpoint, bound on all interfaces, dialing with no direct candidate through `spawn_relay` with a fixed member set) on a docker testbed: each peer behind its own Linux `nft` gateway with its own outside address on a private WAN subnet (so a direct path there reads `lan`, not `wan_direct`), all on one host; each gateway drops what the WAN sends it unasked, as a home router does, and counts the UDP crossing straight between the two outside addresses. Graded against `relay` for every NAT mode (its `off` form), it fails when a NAT mode stops ending on the relay or the routed control stops going direct. **So every connection between a member's devices that are not on one network is carried in full by the nest's box**, in and out; read "hole punching" here as the substrate's capability, not a property of a deployment. Whether to serve address discovery or to rule the relay a forwarder is ruled (2026-10-05): serve it, measured first, and bytes ride direct paths only — § The relay → *Address discovery, and what rides a relayed path*.

**Measured with address discovery served (2026-10-05): the cone pair goes direct; symmetric and mixed pairs stay relayed.** The same probe with the relay serving the substrate's address-discovery server on its default UDP port beside the relay protocol (`RelayServerOptions::quic_bind`; since the build, the plain `just p2p-nat-probe`, whose defaults are exactly this run — so it fails if the shipped build stops making the cone pair's direct path, or starts making one this paragraph does not record), a minute's stream each: both cone gateways — the pair connects at once and the path turns direct 0.3 s after the dial, staying direct to the end (~42 MiB moved); both symmetric, and one of each — connected at once and relayed to the end, each side's ~20 punch packets arriving at the other gateway on a port it holds no mapping for; the routed control direct at 0.3 s as before. With discovery off the same testbed reproduces the paragraph above (connect ~3 s — each endpoint spends that asking a discovery port nobody serves — then relayed for good in every translated pair). The endpoint needs no change: the production builder's one-URL relay map already asks the relay's default discovery port, which every run's gateway counters show, so the relay URL stays the only advertised fact (ruling 2). One testbed fact the first measurement lacked: gateways that *accept* unasked WAN traffic defeat the cone pair's punch — the first punch to land confirms an inbound tuple the inside peer's own punch then has to masquerade around, so no hole ever lines up — which is why the gateways now drop it, as the home routers they stand for do. Ruling 1's clause therefore holds: at least the cone pair goes direct, so the production relay serves discovery (§ The relay → *Address discovery, and what rides a relayed path*).

**The bespoke alternative is deleted (2026-08-23).** The WireGuard fallback had
carried its own three-message ceremony relayed through the nest —
`EndpointReport` (a device's STUN-discovered public endpoint), `HolePunchStart`
(the nest telling both sides to begin, sharing a timestamp) and
`HolePunchResult` — plus a nest-side STUN server to discover those endpoints
with. All of it went with the stack, together with the `fauna.peer.signal` push
kind that would have carried it. It was never live: no nest handler ever
relayed those messages, so removing them closed a gap rather than a feature.
Do not reintroduce a signaling plane for iroh — it does not need one.

---

## File transfer flow (LAN) *(WG-era rendering — the target data plane is the W2 walk)*

> **Reframe (2026-08-10):** the target data plane is the account plane's
> store↔store walk and want-list chunk pull over the Y.1 `PeerChannel`
> ([`../architecture/account-sync-plane.md`](../architecture/account-sync-plane.md)
> § The peer leg for same-account; § Cross-user shared-set transfer below for
> the contact-plane twin) — the same chunk protocol this section describes,
> carried over seam streams rather than a routed WG tunnel. The numbered flow
> below is the WG-era rendering of that idea, kept as fallback documentation.

Once a tunnel is established, file transfers use the standard sync protocol
(chunking, BLAKE3 content-addressing, manifests — [`file-sync.md`](file-sync.md))
with the destination routed through the tunnel instead of the nest:

1. Source device has file chunks to send.
2. Path selection confirms a healthy direct path to the target device.
3. Chunks are sent directly to the peer; the tunnel encrypts each payload.
4. The receiving device decrypts, verifies BLAKE3 hashes, and writes chunks
   to disk — identical to the normal sync receive path.
5. No data transits the nest; round-trip latency drops to LAN speeds.
6. If the tunnel goes down mid-transfer, remaining chunks switch to the nest
   path. The chunk-addressed protocol means already-transferred chunks are
   not re-sent.

---

## Cross-user shared-set transfer — offline, "BitTorrent-like" (target state, ratified 2026-08-10)

**Scenario (user directive 2026-08-10):** two users — e.g. spouses, each with
their own account — move a large set of files (holiday videos, tens–hundreds
of GB) between their devices **offline over p2p**: chunked, resumable,
integrity-checked, multi-source. Analysis + the full internal BitTorrent
comparison: the 2026-08-10 p2p content-sharing considerations doc
(`2026-08-10-p2p-content-sharing-considerations.md`, internal plans tree —
tracked internally, not shipped; frozen). This section is the contract and
wins on divergence.

This is a **byte-path optimization under unchanged custody** — the
cross-user twin of the account plane's same-account peer leg
([`../architecture/account-sync-plane.md`](../architecture/account-sync-plane.md)
§ The peer leg). The sharing mechanism stays exactly M2 shared folders
([`../architecture/key-material-hierarchy.md`](../architecture/key-material-hierarchy.md)
§ Audience: an MLS group): membership, keys, rotation, and the chunk/manifest
pipeline ([`file-sync.md`](file-sync.md)) are unchanged; only where the
sealed bytes travel changes. Contract points, refutable at build time:

- **Precondition: the set is already shared** (Welcome delivered, envelope
  ingested) while connectivity existed. Offline share *initiation* is out of
  v1 scope for this M2-keyed path — **permanently, by design**: the
  offline-initiation path is a plane group scope, never a retrofit onto MLS
  folder keying (§ Offline share initiation below — PQ-1, resolved
  2026-08-17).
- **Identity + admission.** Contact-plane pairs dial by **actor key**
  (PT-1b, § Transport seam); admission above the seam is same-group
  membership. The content keys are themselves the confidentiality boundary:
  every transferable byte is M2-sealed ciphertext, so a wrongly-admitted
  peer can fetch nothing it can read (the membership check is the front
  door, the seal is the wall). The wall is as current as the serving host's
  last read of the set's content-key floor: a file authored while that host
  cannot reach a nest is sealed under the generation it holds, so a person
  removed since that read, and still admitted by a roster that has not
  heard either, can open it until the host reconnects — the rule, its
  nest-side guarantee and why no time limit is set are owned by
  [`on-demand-files.md`](on-demand-files.md) § Shared sets on a capability
  host, decision 2′ (ruled 2026-10-01).
- **Data plane.** The puller reads the set's change-log delta + manifests
  over the Y.1 `PeerChannel`, computes a want-list of chunk store-keys, and
  pulls chunks — the same latest-per-path fold and the same
  ciphertext-hash-addressed chunks the nest plane serves. Every chunk
  self-verifies (store key = hash of body; AEAD tag under the set's
  generation key) — the F9 anti-poisoning twin, peer-side.
- **Multi-source by construction.** The convergent M2 seal means owner
  devices, member devices, and the nest hold **byte-identical** chunks: any
  subset serves any part of the want-list concurrently; an interrupted
  transfer resumes from any other source (nest included, when connectivity
  returns) with held chunks never re-sent. No rarest-first/tit-for-tat
  machinery — those solve open-swarm stranger problems this closed,
  cryptographically-admitted set does not have (full table: the plans doc).
- **Discovery.** LAN candidacy by the shipped arithmetic (§ LAN detection);
  non-LAN via the relay (rendezvous, and the encrypted packets themselves
  when no direct path exists — never a party to the data) for two members
  whose accounts live on one nest — once the relay serves address
  discovery, which is when the seat takes it; until then the seat binds
  relay-less and such a pair pulls from the nest (§ The relay → *The
  cross-user seat and the relay*); two whose accounts live on different
  nests have no relay and pull from the set's home nest (§ The relay →
  *Across nests*); peer
  endpoints learned only through authenticated channels. **No global DHT,
  no scanning, no stranger-servable content** — deliberately: sharing
  reaches exactly the people the owner picked, which is both the privacy
  posture and the structural answer to open-swarm abuse (the policy layer
  on top: `p2p-share` is a charter member of the controversial-class
  feature registry — quota-shaped gating + excision, owner
  [`../architecture/dynamic-features.md`](../architecture/dynamic-features.md)).

### Offline share initiation (PQ-1 — resolved 2026-08-17; design, refutable until first build)

**The question:** a *first* share with no nest reachable needs identity
exchange, Welcome + envelope delivery, and roster/claim writes that are
nest-arbitrated today — plus reconcile-on-reconnect that cannot fork the
group. **The resolution splits along the R15 (account-data-plane.md § The ratified decisions) seam**
([`../architecture/account-data-plane.md`](../architecture/account-data-plane.md)
§ The ratified decisions R15 — storage keying vs messaging crypto): offline
*storage* shares stop being an MLS problem entirely, and the messaging half
gets a delivery-*seat* design instead of a delivery-service substitute.
Build state: § Implementation status today (the ceremony's
carrier-agnostic payloads, admission-bundle doors, state machine, and the
bind door are code as of 2026-08-17; the affordance's shared paint decision is
code too, but no app surface consumes it yet, and the whole messaging half is
not).

**1. Storage shares are born as group scopes, never as offline M2 folders.**
An offline-initiated shared file set is created as an account-plane **group
scope** under the T20 recipient-set scheme (random scope generation key
HPKE-wrapped to each member; no MLS epochs, no delivery service, membership
arbiter-free by T20 constraint (a)) — so initiation needs no nest **by
construction**. The shipped M2 path keeps its online-initiation precondition
permanently: offline initiation is never retrofitted onto MLS-keyed folders;
instead the D4 convergence direction (file-sync onto scope families, charter
§ Substrate settlements) gains its first named consumer. Contract points:

- **The initiation ceremony is two-party and co-present**, over the
  contact-plane peer channel (PT-1b actor-key dial), with an in-person
  key-verification affordance (QR/short-code compare — build detail, ships
  with the ceremony; **element IDs minted + rule-A user-approved 2026-08-17**:
  ui.yaml `folders` page `offline-share-*` / `offline-receive-*`, the
  consent card reusing the knock trio `folder-pending-share` +
  accept/decline; QR is a later per-app rendering of the same code
  elements, and the born set is unnamed in v1 — naming kinds arrive with
  group content-kind sealing). One exchange
  delivers: mutual actor-key verification, the scope offer + accept, the
  recipient's wrap (T20's scheme), and the initial manifest/frontier
  handoff. Precedent for the offer/accept shape: the W8 custody ceremony.
  - **The compare code carries the addressing, not just the identity
    (RESOLVED 2026-08-19 — user decision; supersedes this point's original
    "LAN candidacy by the shipped arithmetic, § LAN detection").** The first
    build measured the consequence of a key-only code: nothing could dial
    (*Measured — the co-present dial has no discovery*, below). The
    resolution is **(b) of the three that bullet named — widen the
    co-present payload** to carry this device's bound LAN endpoints, so
    **the code itself is the advertisement channel** a nest-free pair
    otherwise lacks. Consequences, each load-bearing:
    - **"Compare the code" now means a *transcribed* code, not a spoken
      one** — this is the ratified-contract change the widening costs. The
      code is the 64-hex actor key, then `-`-separated 12-hex candidates
      (4 octets + 2-byte port). **Not QR-first:** tui is the lead app and a
      terminal has no camera, so the typed/pasted form is the one that must
      work; QR stays exactly what this point already called it, a later
      per-app rendering of the same elements.
    - **Additive** — a bare 64-hex key still parses, carrying no addressing.
    - **At most 4 candidates, IPv4 only** (`MAX_CODE_CANDIDATES`): the code
      is transcribed by a human, so its length is a UX budget; IPv6 would
      cost 32 hex characters per candidate and § LAN detection's arithmetic
      is IPv4-only anyway. The dial needs only one candidate to be reachable.
    - **A garbled candidate refuses the whole code** rather than being
      dropped — the module's repair-vs-refuse posture, and it surfaces the
      typo while the other person is still standing there.
    - **No new dependency, and no new *admission* surface**: the candidates
      are peer-supplied and ride the *existing* PT-4 `is_safe_candidate`
      filter inside the transport's `dial`, so a poisoned candidate is at
      worst a no-op — and *who* may start a ceremony is still the receive-act
      expectation, untouched by this. Options (a) a local-discovery crate and
      (c) the nest's relay were both refused — (a) costs a third-party tree
      in a shipped artifact across 7 apps (rule 1), (c) contradicts the
      nest-free claim.
    - **What it does newly disclose, stated rather than glossed:** the code
      now reveals this device's private LAN addresses to whoever holds it.
      Judged acceptable because the code is exchanged in person and never
      sent (the affordance's own help string says so), the addresses are
      RFC-1918 and meaningless off that network, and holding them grants
      nothing — admission is the receive act, not reachability. It is
      nonetheless a reason the "never send it in a message" instruction is
      now *more* load-bearing than when the code was a bare key, and a
      reason not to render this code anywhere it could be screenshotted into
      a support thread.
    - **Where the addresses come from:** the *assembler*, never the seam.
      `fauna_iroh::peer_leg_transport` already returns
      `(Arc<dyn PeerTransport>, Vec<SocketAddr>)`, and the seat records it
      via `CeremonyNode::with_bound_addrs`; the `PeerTransport` trait keeps
      its deliberate silence about bound addresses (`peer_leg` module docs).
      The bound-sockets × interface-addresses cross is the shared
      `fauna_core::device_endpoints::lan_socket_addr_candidates`, the same
      one the nest-published peer-leg facts use. **Only an IPv4 socket's
      port pairs with an IPv4 address:** the endpoint binds a `[::]` socket
      beside the `0.0.0.0` one, on its own port, and where that socket is
      v6-only (the Windows default) an IPv4 address carrying its port has no
      listener — every send to it draws a port-unreachable back.
    - **Deliberately still open:** the code has **no checksum**, so a
      mistyped key fails as a failed dial rather than an immediate refusal.
      That is pre-existing (a 64-hex key was always transcribed), and one
      contract change per pass; revisit if transcription errors show up in
      practice.
    - In code: `fauna_client_capabilities::group_ceremony_view::{PeerCode,
      format_peer_code, parse_peer_code}` and
      `group_ceremony_node::{with_bound_addrs, local_endpoints, dial_code}`.
      Proof: `group_ceremony_node.rs`'s
      `a_ceremony_dials_on_the_addressing_its_compare_code_carried`, which
      dials through a transport that refuses any dial whose candidates do
      not name the listener — with the key-only dial asserted to fail, so
      the test cannot pass for the wrong reason.
- **Roster and claims are plane data in the scope, not nest writes.**
  Membership entries ride per-writer logs and frontier-merge like any
  class-2 kind (charter § Ordering model); a member's nest first learns of
  the scope on next contact and ingests as an ordinary custodian replica —
  claim-time arbitration does not exist on this path.
- **Fork-free reconcile is by construction, with the lattice left to T20:**
  there are no epochs to fork — per-writer logs converge under frontier
  merge; concurrent *adds* must converge as wrap-set union within a
  generation, and any *remove* mints a new generation (R14 composition).
  The precise wrap/roster lattice and the per-member severance rule are
  designed (T20 resolved 2026-08-17 — charter § The audience ladder → The
  recipient-set scheme; key material:
  [`../architecture/key-material-hierarchy.md`](../architecture/key-material-hierarchy.md)
  § Audience: a storage group).
- **Admission for the transfer plane is a fourth witness kind** —
  group-scope membership, evaluated against the channel-proven actor key
  and yielding exactly the scope, through the same verdict-shaped core
  (charter § The admission seam, which registers the witness kind). The
  witness *form* is designed (T20 resolved 2026-08-17): the member's
  `Enrolled` roster entry plus its authoring chain to the authority actor
  root — charter § The recipient-set scheme, membership-witness bullet.
- **Built — the ceremony carriage (2026-08-17; refutable
  by the security review).** Three `fauna.peer.share.ceremony.*` kinds —
  offer push, accept poll, deliver push — ALL initiator-originated over
  one dialed channel: the accept side of a peer node serves and never
  originates, so the recipient stays purely reactive and the consent gap
  is the poll's pending (`None`) answer, never a hang; a declined
  invitation answers refused, terminally. **First-contact admission is the
  receive-act expectation** (the build decision rule 1 demanded): the
  recipient's own "receive a share" act, after the in-person compare,
  mints a device-local, single-actor, 15-minute-TTL expectation
  (`fauna_client_capabilities::group_ceremony_peer`) that admits exactly
  the scanned initiator's ceremony frames **before any payload parsing**;
  from offer-recorded on, the durable ceremony record itself admits its
  initiator, so a dropped co-present connection redials without
  re-scanning — **scoped to that record's own ceremony (2026-09-27):** the record takes frames only on its own
  `scope_id` and only while the ceremony is live (not declined; not yet
  delivered, or delivered within
  `GROUP_CEREMONY_DELIVER_RESEND_GRACE_SECS`, so the initiator's
  byte-identical re-send of a deliver whose ack a drop lost is acked and
  records nothing new); an offer for a scope with no record needs a live
  expectation, and at most
  `GROUP_CEREMONY_MAX_PENDING_INVITATIONS_PER_INITIATOR` un-consented
  invitations per initiator are held. The pre-decode gate names only the
  actor; the per-scope check runs after decode in
  `GroupCeremonyPeer::ingest_frame`. Only an *invited* record admits:
  every leg is initiator-originated, so an actor this account shared TO
  pushes nothing to its listener, and an accept frame pushed there is
  refused. This is
  § Inbound authorization's stronger-per-kind-admission carve-out applied
  to the ceremony family — an edge both endpoints chose. The eight-rule
  walk for the new listener surface: (1) the expectation/record gate runs
  before decode, pinned by a test that a refused actor's bytes never reach
  the seam, and fail-closed when no ceremony state is wired; (2) the
  wrapper structs, the frame enum, and the inner signed payloads all sit
  in `KIND_PAYLOAD_COVERAGE_P2P_SHARE` with real-signed corpus entries,
  both ties pinned; (3) the kinds joined `allowlisted_kinds()`, asserted
  as the whole set — the deliver's admission wrap is key material only as
  sealed end-to-end ciphertext to the recipient's reception key (the T20
  rail, the group twin of the M2 rail), opaque to every parser on the
  path; (4) every frame is signature-verified against the channel-proven
  sender at ingest, and a forged snapshot row faces ordinary first-contact
  adoption strictness; (5) the kinds ride the same `p2p-share` excision
  and the `fauna.peer.share.` grep family; (6) expectations expire and
  cancel, and a record admits only its own live ceremony — a decline, or
  the re-send grace running out after the deliver, withdraws it; (7) they serve only
  through the same brake-gated listener as the rest of the share set — no
  new bind door; (8) ceremony requests are metered by the same per-peer
  ledger, refused attempts included. The transport half is proven end to
  end over a real in-memory channel (the capstone re-run over the wire,
  `group_ceremony_over_wire.rs`).

- **Built — the affordance, both roles (tui, 2026-08-17/18; tui leads per
  the ordering rule; linux 2026-08-20, row 334).** The initiator half and the
  eight approved element ids landed 2026-08-17; the recipient half landed
  2026-08-18, once the group plane's store existed for its machinery to land
  in. Linux's leg landed as a shared-logic commit followed by the panel /
  consent-card / group-scope-row UI wiring, reusing the same eight ids and
  the existing knock-trio/folder-row ids — no new element ids. What the surface is:
  - **One seat per session, bound by whichever door asks first — never at
    login.** An actor-keyed endpoint for a feature most people never touch
    would be waste at login. The seat comes up when a panel opens (the
    co-present user's explicit "I am doing this now") or when the share
    plane's driver — started at the account-store-ready edge, § Cross-user
    shared-set transfer — first has a set to serve under an admitting brake,
    whichever is first; the other door is handed that seat back, so a session
    never holds two listeners on one NodeId, in either order or in a race.
    The door that binds reads the brake at that moment, so the *ceremony
    itself* then runs with no nest involved at all, which is the substantive
    offline claim. Every seat binds share-plane-capable over the session's own
    roster, which admits nobody until the plane's driver lends it the M2
    consult and nobody again once that driver ends: a seat the panel bound
    serves no set before the plane would have bound its own, and a ceremony in
    flight on it is never displaced. Built as
    `fauna_sync_engine::offline_share::SessionSeat`, the slot both doors bind
    through on tui and linux (2026-09-15) — it replaced two opposite per-app
    folds (tui replaced the panel's seat, linux kept it) that both ran after
    the driver's own listener was already up. The FFI apps' panel door binds
    through the same slot since 2026-09-24 (`fauna-ffi`'s process-wide,
    actor-keyed `SessionSeat`, emptied at the account runtime's teardown);
    before that it bound its own node outside any slot. (A cold start with no nest ever
    reachable still cannot bind through the panel's door — it has no cached
    capability read.)
  - **The seat's record is lent late — the bind never waits for the account
    runtime (ruled 2026-09-28; the lead E3
    slice's e2e refuted the opposite).** The ceremony record rests on the
    account plane (`fauna.state.group-share-ceremony`, one row per ceremony —
    [`../architecture/config-dissolution.md`](../architecture/config-dissolution.md)
    § The `__config` dissolution schedule, the kinds table), whose store
    exists only once the account runtime is assembled, seconds after
    sign-in; the panel opens whenever the co-present user opens it, and the
    two-seat journeys open it before that edge. A seat that required the
    store's handle at bind refused the panel with "this device has no account
    runtime yet" and the compare code never appeared. So the seat binds over
    an **empty in-memory replica** and the record is **lent to the session
    seat at the account-store-ready edge**, exactly as the M2 roster consult
    is (*One seat per session* above): one shared call on the slot
    (`SessionSeat::lend_record`), the lent handle held `Weak` — the account
    driver's command channel closes when its last handle drops, so a strong
    hold in a panel's seat would keep the runtime from closing — and the
    lend runs one flush at once, both directions of the record's own join
    (the replica into the store, the store's rows back into the replica).
    Who lends is the runtime's host, never the share-plane driver: tui and
    linux at the edge that also starts the driver, `fauna-ffi` at its
    account runtime's start — windows and iOS host no share plane and still
    run the ceremony. Until the lend, the compare code, the listener, the receive
    act, frame ingest and a decline run on the replica, and the persist
    task's flushes are no-ops the lend replays; the **two acts that need
    account material keep taking the handle and refuse plainly without
    it** — `initiate` (the authority carriage) and `consent` (the reception
    key, then the held-root and machinery rows) — which is the shape both
    have had since 2026-09-24. The page read (`load_group_shares`) keeps its
    seat fallback with a new reason: lent, it answers the durable fold
    joined with the replica (the durable copy wins, so a seat's view never
    masks what another device recorded); unlent, the replica. **The honest
    bound:** a frame ingested before the lend rests in memory only until it;
    a crash in that window (the seconds between sign-in and the store's
    assembly) loses the ingested offer, and the initiator's redial then
    needs a fresh receive act. Nothing a user made is at stake before
    consent, and consent itself needs the runtime.
  - **The consent card mints NO ids**: it is the knock trio
    (`folder-pending-share` + accept/decline) with a second source, so the two
    lists are ONE indexed family and a user sees one list of things awaiting an
    answer. Both gestures address the **scope id**, never the row position — an
    offer can land while the user's finger is moving, since the listener
    ingests it without asking the page.
  - **Consent is one act**, because it is one decision: the reception keypair is
    minted and rested, the accept recorded, the deliver awaited under a named
    generous budget, the admission run, and the machinery written through. Three
    orderings inside are load-bearing: the reception key rests *before* the
    accept (its public half rides the accept and the bundle seals to it — a
    crash between the two must leave an unused key, never an unopenable
    delivery); the held-root row lands *before* the machinery rows (which seal
    under it); every monotone marker is set *after* its write returned.
  - **A landed scope lists as an ordinary set row** — no new ids, since a
    ceremony-born set is a set — badged with who shared it, and read from the
    **store's** group rows rather than the ceremony record: a recorded-but-never-
    adopted scope is silently absent, never painted as a set the device cannot
    read. A group row carries no local-seat config, no name (v1 sets are
    nameless — the row shows the short scope id) and no leave button (severance
    is the authority's mint —
    [`../architecture/recipient-set-scheme.md`](../architecture/recipient-set-scheme.md)
    § The recipient-set scheme).
  - **The driver seam is shared, not per-app**: `AccountStoreHandle`'s four
    group doors (`put_group_held_root`, `put_group_reception_key`,
    `adopt_group_rows`, `group_scope_states`), the joiner's walk
    (`group_ceremony_node::{consent_to_group_share, decline_group_share,
    await_delivery}`), the consent-card projection
    (`group_ceremony_view::pending_group_invitations`) and the listing
    projection (`fauna_sync_engine::group_scope_view`). The four-app trickle-down
    (windows, macos, ios, android — web is a declared absence, § *Wormability
    walk* rule 5) consumes these rather than re-deriving them: windows/macos/ios
    through the UniFFI boundary (`libs/fauna-ffi/src/offline_share.rs`, landed
    2026-08-21), android through the same boundary — its own account-runtime
    prerequisite was met 2026-08-22.
  - **Proof:** `tests/e2e-unified/tests/test_offline_share_two_seat.py` — two
    seats, two accounts, one machine, the whole ceremony through two UIs, ending
    on *the recipient's folders page lists the shared set*; plus the decline
    leg's two independent halves (the card stops knocking AND nothing is
    adopted) and the uninvited-initiator refusal. The `ceremony_seats` fixture
    is parametrized over tui/linux (2026-08-20, row 334) rather than
    hardcoded to tui, so linux's leg is proven the same way.
  - **v1 stops at ADMISSION, not at bytes.** The machinery is adopted and the
    scope lists; content transfer waits on the group serve door, which still
    checks the folder family only (§ Cross-user shared-set transfer; the group
    content-kind sealing slice).
  - ⛔ **And it does not run yet at all** — the dial has no addressing
    information. See the next bullet.

- **Measured — the co-present dial has no discovery (2026-08-18, RED).** The
  two-seat journey drove the whole ceremony through two real apps and the
  initiator's own panel reported
  `could not reach them: transport i/o error: iroh connect: No addressing
  information available`. **The affordance, the walk, the refusal path and the
  paint are all correct — nothing can reach the counterpart.** Three facts,
  each verified rather than assumed:
  - The compare code carries **only the actor key** (it is read aloud, so it
    is 64 hex and nothing more), so a dial starts with an `EndpointId` and an
    empty `PathCandidates`.
  - `iroh` 1.0 as pinned in this tree exposes **no local/mDNS discovery
    feature at all** (`default`, `tls-ring`, `portmapper`, `metrics`,
    `platform-verifier`, `qlog`, `test-utils`, `fast-apple-datapath`,
    `unstable-*` — that is the whole list), and the ceremony deliberately
    attaches **no relay URL**.
  - **§ LAN detection does not close it**, which is the part the PQ-1 design
    got wrong by implication: `should_attempt_lan_probe()` keys on a
    *peer-advertised* LAN address, and advertisement is exactly the
    nest-mediated path a nest-free ceremony does not have. "LAN candidacy by
    the shipped arithmetic" therefore describes the transfer plane, not this
    one.

  **This is a design gap in contract point 1, not an implementation bug**, and
  the three ways out are not equivalent: a **local-discovery dependency** (new
  third-party crates — a supply-chain decision, not a code decision); **widening
  the co-present payload to carry LAN candidates**, which the QR rendering
  could carry for free but a *spoken* code cannot, so it changes what "compare
  the code" means; or the **nest's relay**, which contradicts the nest-free
  claim outright and fails cross-nest anyway. Owner: the next design pass on
  this section. Captured internally.

  ✅ **RESOLVED 2026-08-19 by (b)** — contract point 1's *The compare code
  carries the addressing* bullet above owns the resolution and its
  consequences. One correction to the framing recorded here: (b) is **not**
  "QR-first". The reasoning that reached for QR assumed the widened code needs
  a camera; tui is the lead app and a terminal has no camera, so the
  **transcribed** form is the one that had to work, and it does. QR remains a
  later per-app rendering of the same elements, unchanged.

**2. Messaging groups: the delivery seat is a named, re-pointable role a
member device can hold** — A3's arbiter-epoch machinery (charter § The nest
decomposed, item 2) instantiated per MLS group. Each group has exactly one
**sequencing seat**; today it is implicitly the channel-home nest
([`direct-messages.md`](direct-messages.md) § Welcome Delivery). Target
state: a signed **seat-epoch entry** in the group's channel state names the
seat — a nest *or a member device* — and the epoch at which it assumes;
handover is sequenced by the outgoing seat, or ceremony-forced on its death
with A3's fencing. A nest-less initiation makes the initiating member's
device the group's first seat, and the Welcome travels directly over the
peer channel — the co-present ceremony *is* the delivery. When a nest
becomes reachable, the seat re-points to it by ordinary handover; the
group's history is continuous. **Rejected: provisional-group +
rebase-on-reconnect** — rebasing MLS epochs rewrites transcript history,
which is precisely the fork the obligations below forbid, and it opens a
reconcile window where two members hold irreconcilable transcripts.

**Fork-free reconcile proof obligations** (each must hold in the build, and
each gets a test):

1. **Epoch linearity.** At most one commit is accepted per epoch: the seat
   sequences commits, and a member refuses any commit not attested by the
   group's current seat.
2. **Seat-chain linearity.** Seat-epoch entries form a single chain, each
   sequenced by its predecessor; a ceremony-forced replacement fences the
   dead seat (retired-seat memory — the W5 rotation-fence shape) so a
   partitioned old seat cannot mint competing epochs.
3. **Retrospective verifiability.** Commits carry the seat's order
   attestation, so a catching-up member verifies epoch order from any
   relaying peer without trusting it.
4. **Partition is a liveness cost, never a fork.** Seat unreachable ⇒ no
   membership changes; application messages within the current epoch still
   flow member↔member. A stated availability harm, no inconsistency.

**Trust posture — stated per the § Ordering model revisit trigger, never
inherited silently:** a device-held seat holds exactly the powers the nest
delivery service holds today over the same group — ordering among honest
commits, and withholding. MLS content authenticity and transcript agreement
are unchanged; members detect withholding/equivocation by comparing epoch
authenticators over the peer leg (detection, not proof — the same bound the
account plane accepts for frontiers). The generalization changes *who may
hold the seat*, never what the seat can do.

**The multi-device residue, named honestly:** a nest-less *member's own
fleet* still lacks its class-5 MLS-state arbitration (charter § The peer
leg → Scope). Single-device members are fully served by this design;
multi-device nest-less members wait on the *account* arbiter seat itself
becoming device-holdable — the same A3 generalization applied to the
account plane, owner charter § The nest decomposed item 2. Deliberately not
designed here.

**Build order (the tui chain):** T20's scheme detail (**resolved
2026-08-17**, same pass — charter § The recipient-set scheme) → the first
group scope + the initiation ceremony (shared Rust → nest ingest → **tui
leads the affordance**, per the ordering rule) → linux followed
(2026-08-20, row 334) → the UniFFI ceremony boundary (2026-08-21) → the
remaining four-app trickle-down (windows, macos, ios, android; web is a
declared absence, § *Wormability walk* rule 5). The messaging-seat half
builds separately, behind A3's build and the conversations plane; the
storage half never waits for it.

**Open questions:**

- **PQ-1 — offline share initiation. RESOLVED 2026-08-17** (design pass,
  refutable until first build) — § Offline share initiation above: storage
  shares are born as T20 recipient-set group scopes (no MLS, no seat, no
  arbiter — initiation nest-free by construction); messaging groups get the
  re-pointable delivery seat (A3's arbiter-epoch machinery per group,
  holdable by a member device). The R15 narrowing (2026-08-11) is what made
  the split available; the v1 M2-keyed precondition above stands
  permanently.
- **PQ-2 — hardening track.** Fuzzing the peer-channel framing + p2p kinds
  before the data plane ships (§ Wormability posture, mitigation "memory-safe
  stack" is necessary, not sufficient). **Scoped 2026-08-10** into a concrete
  cargo-fuzz track (framing decode, DAG-CBOR frame decode, kind payload
  decode; gate: green before any p2p data-plane slice ships). **BUILT
  2026-08-12:** the three check bodies live in
  `fauna_peer_channel::hardening`, shared by the workspace-excluded
  cargo-fuzz targets (`libs/fauna-peer-channel/fuzz`) and the
  `peer-channel-hardening-check` merge gate — a bounded smoke replaying the
  checked-in real-exchange corpus + a fixed deterministic sweep on every
  relevant push ([`../architecture/merge-gate-catalog.md`](../architecture/merge-gate-catalog.md)
  § The heavy gate catalog, thirteenth gate). The gate ran green at introduction,
  discharging the "green before any p2p data-plane slice ships" condition
  for slices landing while it stays green. **Pinned to the serve surface
  2026-08-12:** the gate's coverage was a hand-written snapshot of the kinds
  `fauna_peer_sync::server::allowlisted_kinds()` serves, with nothing tying
  the two together — so a fifth allowlisted kind would have re-fired the gate
  and passed **green** over its own un-smoked pre-auth parser, the gate's
  green reading as "the peer parser surface is hardened". The surface is now
  *declared* (`fauna_peer_channel::hardening::KIND_PAYLOAD_COVERAGE`) and the
  smoke checks it against the allowlist in both directions plus against the
  corpus on disk, so growing the allowlist without growing the coverage is a
  RED. Residual: coverage-guided long
  runs need `cargo-fuzz` + `libfuzzer-sys` (outside the vetted workspace
  lockfile) — install-gated on explicit user approval, per the fuzz crate's
  README; the gate never builds the fuzz crate.

> **The share leg's build contract, its build ledger, the row-half build design and the phone-peer design moved to [`p2p-shared-set-build.md`](p2p-shared-set-build.md) on 2026-09-27**, verbatim, when this doc reached 240,363 B (91.7% of the whole-file read ceiling, two days from it). The contract above, § Offline share initiation, § Peer-served change-row provenance and § Wormability walk stay here. Each moved heading below is a routing stub, so every citation still resolves in one hop.

### Build contract — the share leg's first design pass (2026-08-17; refutable at build time)

**→ [`p2p-shared-set-build.md`](p2p-shared-set-build.md) § Build contract — the share leg's first design pass** (2026-09-27 split).

### Peer-served change-row provenance (ruled, lifted; refutable by the security review)

The walk's rule-4 sequenced obligation. The problem: nest-served change rows
carry a nest-stamped `author_actor_id` (the authenticated writer), while a
peer-served row's only provenance is "an admitted member relayed it" — so a
read-only member could fabricate rows misattributed to writers, and no
receiving replica could tell. The 2026-08-17 ruling split the plane by
**what the channel can verify** and refused relay outright; writer-signed
change records
([`../architecture/writer-signed-change-records.md`](../architecture/writer-signed-change-records.md)
§ Writer-signed change records, ruling (3), which owns
the signature, the reader and the switch) gave rows a proof of their own, so
the refusal lifts:

- **Bytes are multi-source; a row travels as far as its proof does.** Chunks
  (store-keyed by ciphertext hash, AEAD-tagged) and manifests (fetched by
  manifest hash, verified against it) are **self-verifying regardless of who
  serves them** — any admitted member serves any of them, and the
  multi-source contract stands untouched for the heavy half of the transfer.
  A change **row** carries one of two proofs. **A writer-signed row verifies
  self-contained** — the row, its `signature`/`signer_key` pair and its
  delegated signer's cert carried inline on the page (`PeerShareChange.
  signer_cert`) — through the one shared reader every client reader uses,
  under the receiver's own set nonce; so **the serve side sends every row it
  holds**, its own and every writer-signed row it relays (own-pending and
  sequenced alike, each marked pending-vs-sequenced), and an N-member set
  offline-relays every member's metadata, not only the relayer's. **Every
  writer signs, so an unsigned row is refused** outside the signature check's
  class exemptions — the same refusal the nest pull makes, never a second
  policy. An exempt-class row's only proof is the channel's (PT-1b): it is
  admitted solely as the serving peer's own; a relayed unsigned row is
  refused, and the serve side never sends one. A fabricated, altered or foreign-set row
  fails the signature; a read-only member's row signed under its own key
  fails the writer check below. What a replica relays is what its reader
  verified — off the nest pull or a peer's page — kept byte-exact beside the
  cert it chained through.
- **The writer check is a cached-roster consult on the SIGNED actor,
  fail-closed.** A verified row is admitted only if its signed actor is the
  set's owner or a `writer` — per the reader's own roster read (the engine's
  ungated roster leg) and, where the reader has not read one, per the share
  leg's cached roster (refreshed at each online roster read — a small additive
  at-rest cache, the last roster that stands offline, each writer's proven
  predecessors kept beside the writer so the one judge places a retired
  identity's row as on the nest pull —
  [`../architecture/writer-signed-change-records.md`](../architecture/writer-signed-change-records.md)
  rulings (8)(e) and (11)(i)); the serving peer's
  role vouches for no one else's row. An exempt-class own row needs the
  serving peer to be a cached writer. No cached role ⇒ refuse rows, serve/pull bytes
  only. A stale cache denies fresh writers until reconnect (heals) and honors
  recently-demoted ones briefly (the provisional overlay's reconcile cleans
  up — next bullet).
- **Peer-ingested rows are a provisional READ-SIDE overlay; the nest stays
  the log's arbiter.** Accepted rows never enter the nest-sequenced log
  store: they overlay the latest-per-path fold for reads (and drive
  materialization), and on reconnect the nest's sequenced rows confirm or
  supersede them — the overlay never changes what the converged log says,
  so reconcile cannot fork (the same posture as the account plane's
  "nest-arbitrated kinds travel as intents at most").
- **A provisional DELETE never destroys local bytes.** Provisional
  creates/modifies materialize; a provisional delete affects the overlay
  *view* only — local file deletion follows nest-confirmed rows alone
  (deletion is the irreversible direction; the no-data-loss bias picks the
  recoverable failure).
- **Residual, stated — a p2p-relayed row never touches the home nest,** so
  device revocation is not enforced on it at ingest: a row signed by a device
  revoked since is admitted on its (still chain-valid) cert. It is the first
  cross-boundary flow that does not route through the distinguished replica
  ([`devices.md`](devices.md) § Revocation
  authority names the re-visit); bounded by the provisional overlay, which
  the nest confirms or supersedes on reconnect, though bytes may have been
  applied (ruling (5)(v) of the owner section).

### Built — the serve/pull core (2026-08-17, row 59 slice B): what the build measured

**→ [`p2p-shared-set-build.md`](p2p-shared-set-build.md) § Built — the serve/pull core** (2026-09-27 split).

### Built — the discovery carriage (2026-08-17, row 59 slice F): what the build measured

**→ [`p2p-shared-set-build.md`](p2p-shared-set-build.md) § Built — the discovery carriage** (2026-09-27 split).

### Built — the serve-side byte half (2026-08-18, slice E leg 1): what the build measured

**→ [`p2p-shared-set-build.md`](p2p-shared-set-build.md) § Built — the serve-side byte half** (2026-09-27 split).

### Built — the pump, the boundary, and the discovery halves: what the build measured

**→ [`p2p-shared-set-build.md`](p2p-shared-set-build.md) § Built — the pump, the boundary, and the discovery halves** (2026-09-27 split).

### Built — the tui app leg (2026-08-19): what the build measured

**→ [`p2p-shared-set-build.md`](p2p-shared-set-build.md) § Built — the tui app leg** (2026-09-27 split).

### Built — the shared driver, and the linux leg: what the build measured

**→ [`p2p-shared-set-build.md`](p2p-shared-set-build.md) § Built — the shared driver, and the linux leg** (2026-09-27 split).

### Phone peers — design (2026-09-26; the direction is user-ruled, the decisions below are refutable at build time; the shared arm BUILT 2026-10-01, the app legs UNBUILT)

**→ [`p2p-shared-set-build.md`](p2p-shared-set-build.md) § Phone peers — design** (2026-09-27 split).

### Built — the member advertisement unblocked: what the build measured

**→ [`p2p-shared-set-build.md`](p2p-shared-set-build.md) § Built — the member advertisement unblocked** (2026-09-27 split).

### Build design — the row half (B2 scoping pass, 2026-08-17; refutable at build time)

**→ [`p2p-shared-set-build.md`](p2p-shared-set-build.md) § Build design — the row half** (2026-09-27 split).

### Wormability walk — the share leg against the eight rules (first design pass; refutable by the security review)

The rules are § Wormability posture above; this is the share leg's per-rule
record, the cross-user twin of the peer leg's walk
([`../architecture/account-sync-plane.md`](../architecture/account-sync-plane.md)
§ The peer leg → Wormability walk). **By construction** = the specified shape
satisfies the rule; **obligation** = the build must land the named piece for
the verdict to hold.

1. **Admission before parsing — by construction.** Nothing beyond
   `fauna.peer.node_info` and the admit exchange parses before the M2
   admission ([`p2p-shared-set-build.md`](p2p-shared-set-build.md) § Build contract); endpoints are learned only from
   authenticated channels' cached advertisements — no global address space,
   no broadcast, no scanning. Propagation is bounded to set co-membership
   edges, each created by an owner's gated share + a contact-gated accept.
2. **Memory-safe pre-auth stack — by construction plus one obligation.**
   Same seam and stack (Rust + quinn + rustls); the new pre-auth surface is
   the `fauna.peer.share.*` envelopes, strict dag-cbor. Obligation: every
   new kind, payload struct, and witness inner parser joins
   `fauna_peer_channel::hardening::KIND_PAYLOAD_COVERAGE` with corpus
   entries, and the smoke pins the share dispatcher's allowlist exactly as
   it pins `fauna_peer_sync`'s — growing either allowlist without coverage
   is a RED (the PQ-2 pinned-to-the-serve-surface rule, extended to the
   second allowlist). **DISCHARGED 2026-08-17 (slice B):** all eleven share
   wire structs sit in `KIND_PAYLOAD_COVERAGE_P2P_SHARE` with corpus entries,
   and both ties are pinned — table ↔ corpus, and table ↔ the real
   `allowlisted_kinds()`. **Extended same day (slice 3):** the
   ceremony carriage's wrappers, frame enum, and inner signed payloads
   joined the table + corpus in the same change that allowlisted the kinds
   — § Offline share initiation's carriage record owns their walk. **And
   again (slice 2's slot):** the admit exchange's
   `group_witnesses` carriage + the carried `GroupRosterRecord`
   certificate's own decode joined the admit row's coverage — the fourth
   witness kind's inner parser is smoked where the wire reaches it.
3. **Least-kind dispatcher — obligation. DISCHARGED 2026-08-17 (slice B):**
   `fauna_peer_share::server::allowlisted_kinds()` is exactly the probe, the
   admit exchange, the set change-log read, manifest fetch, and the want-list
   chunk pull — asserted as the whole set, never a sample. No config,
   capability-mint, admin, or key-material kinds — content keys ride only the
   M2 rail ([`p2p-shared-set-build.md`](p2p-shared-set-build.md) § Build contract). **Extended same day (slice 3):** the
   three offline-ceremony kinds joined the set behind their own pre-parse
   admission; the deliver's admission wrap is sealed end-to-end to the
   recipient's reception key (the T20 rail) — § Offline share initiation's
   carriage record owns that walk.
4. **Received content inert at the transfer layer — by construction, with
   one SEQUENCED ruling.** Chunks are convergent AEAD ciphertext,
   store-keyed by ciphertext hash, self-verifying (the F9 twin) — a
   poisoned chunk fails its hash/tag check and is refused; the worst a
   compromised member can feed the byte path is storage waste, bounded by
   rule 8. The recorded interaction is the **change plane**: nest-served
   change rows carry a nest-stamped `author_actor_id`, while a peer-served
   row's provenance is only "an admitted member relayed it" — a read-only
   member could fabricate rows misattributed to writers. **RULED 2026-08-17
   (sequenced exactly as the peer leg's rule-4 hostile-row posture was):**
   § *Peer-served change-row provenance* above owns the posture (lifted
   2026-09-29 by writer-signed change records: a row relays as far as its
   signature proves it). The chunk-pull leg never waited on this ruling; the
   ingest leg builds against it.
5. **No listener when off; compiled away when excised — the excision half
   holds; the participation half DOES NOT (corrected 2026-08-24).** This
   entry used to read "Share kinds exist on the listener only while p2p
   participation is on (the contact-plane node's lifecycle — no
   unconditional bind)". **Both halves of that sentence are false in code**,
   and the correction is recorded here rather than quietly rewritten because
   a walk entry is a compliance claim: the share seat does **not** ride the
   contact-plane node's lifecycle (it binds its own seat, at the
   account-store-ready edge), and its bind **is** unconditional with respect
   to any human — see § Element IDs → *RATIFIED 2026-08-24* finding 4 and
   § Implementation status today → *The participation half of rule 5 is
   BUILT* for what was true until then. **HOLDS since 2026-09-25 —
   § Per-device participation, enforced at both bind doors in shared code:
   off drops the seat and the leg's node, and "no socket" is asserted on
   the socket (§ Implementation status today names the tests).** What
   else holds
   unchanged: web is structurally absent (no QUIC seam in the wasm graph). The whole plane sits behind the `p2p-share` cargo feature
   ([`p2p-shared-set-build.md`](p2p-shared-set-build.md) § Build contract), so a store-safe flavor ships **no share data plane at
   all** — rule 5's own excision claim. The **store-safe witness third
   column** ("same-account `fauna.peer.sync.` PRESENT + `fauna.peer.share.`
   ABSENT") lands WITH the first artifact that compiles a real share
   surface — the column-meaningfulness sequencing both walk records own —
   refreshing the four two-column sibling check rows in the same landing.
   **DISCHARGED 2026-08-17/18:** the share halves landed with tui's first
   root-linking artifact, the sync-PRESENT half and the sibling-row refresh
   the next day (`just tui-store-safe-check`; recipe shape owned by
   `dynamic-features.md` § The feature-matrix test story).
6. **Revocable per-device authority — by construction, and the STRONG
   severance class, with one honest limit.** Content severance here is
   **cryptographic**: eviction/leave rotates the set's content key (M2
   rotate-on-removal — the (a) class of the peer leg's corrected rule 6,
   with none of its generation-0 residue). Admission severs at the next
   evaluation (the roster consult re-runs; an evicted actor's next
   handshake finds no membership). The honest limit: the leg admits
   **actors**, so a counterparty's lost/compromised *device* still holds
   that counterparty's actor key — per-device severance on the contact
   plane is the counterparty's own device-removal + succession story,
   identical to every other actor-key surface; stated, not hidden. A second
   limit, stated the same way (2026-10-01): both severances reach a device
   through the nest, so a host that cannot reach one keeps admitting, and
   keeps sealing under the generation it holds, until it reconnects — what
   it authors in that window is open to a person removed since its last
   read, over this plane only and never from a nest
   ([`on-demand-files.md`](on-demand-files.md) § Shared sets on a capability
   host, decision 2′ owns the rule).
7. **Version brake — obligation; DISCHARGED 2026-08-17, both halves
   (nest slice D leg 1; client slice E).** The share plane lights behind a
   `p2p-share` nest capability token (cfg-gated advertisement, the
   `subscriptions` shape); the client binds and serves share kinds only
   under the token, cached last-known for offline starts exactly as the
   peer leg's brake evidence is — no evidence refuses by default. An
   excised nest omits the token (excision criterion 4). Built:
   `capability::P2P_SHARE` in `fauna-protocol::discovery`, advertised by
   `discovery_core::capabilities_for` under `cfg!(feature = "p2p-share")`
   (a default-ON `bins/fauna-nest` flavor-root feature forwarding
   `fauna-protocol/p2p-share`). It follows `SUBSCRIPTIONS`, not
   `PEER_SYNC`, deliberately: `peer-sync` is always-on because the nest
   carries none of that leg's traffic, but this plane has a nest-side
   half (rule 8's chokepoint), so a nest that compiled the plane away
   must not invite a client to bind a listener whose counterparty bound
   this box is not running. **The client-side halves (bind-refusal without
   cached evidence, the listener door) are now BUILT too**
   (`group_ceremony_node::{ceremony_bind_verdict, CeremonyNode::bind}` — no
   evidence at all refuses, verbatim the peer leg's posture), **and so is
   the cached-last-known read (2026-08-18)**: the bind's offline evidence is
   the peer leg's own `META_NEST_FACTS` cache re-served through
   `AccountStoreHandle::cached_nest_capabilities` — one cache, one writer,
   and the limits screen hides on the same token
   (`fauna_client_features::capability_token`). **The tui lead's app-side
   wiring is BUILT too (2026-08-17/18):** `offline_share.rs` reads the live
   brake (`ceremony_bind_verdict`) and calls `CeremonyNode::bind` with the
   resulting verdict — § Offline share initiation → *Built — the affordance,
   both roles*. **The transfer half's own bind is BUILT too (2026-08-19):**
   `share_glue::run` calls `bind_share_plane_seat`, which composes
   `bind_with_share_plane`/`set_shared_sets` — [`p2p-shared-set-build.md`](p2p-shared-set-build.md) § *Built — the tui app leg*.
   Since 2026-09-15 both doors bind `bind_with_share_plane` through
   the session's one `SessionSeat` (§ Offline share initiation → *One seat
   per session*); the verdict typing is the same, so this rule's evidence
   posture is unchanged.
8. **Bounded fan-out — obligation, on BOTH planes.** Listener-level:
   per-peer + per-window connection/request quotas on the share serve set,
   admission-refused attempts included, size-bounded ledger — **DISCHARGED
   2026-08-17 (slice B)**, by *reusing* `fauna_peer_sync::quota`'s ledger rather
   than growing a second one (it is peer-keyed and clock-injected, so it is
   transport-level, not account-plane-specific); a witness-refused admit spends
   the same budget as an admitted one, pinned. Feature-plane:
   `p2p-share.member.admit` composed **nest-side at the member-admit
   door** — **DISCHARGED 2026-08-17 (slice D leg 2), with the seam
   refined by building it.** The first design pass said "at the share
   roster write"; the build ruled that the gate composes one level up, in
   `welcome_deliver_core`'s first-reach arm on the **claim-row
   discriminator** (`folder_channel_claims` — nest state, never the
   sender's `req.kind`, which real folder shares don't reliably carry and
   a patched client could relabel). Three measured facts made that the
   ruling: (a) the raw roster write (`register_actor_channel_gated`)
   serves the conversation plane too and swallows its failures by design,
   so a gate composed there would either charge group-chat membership to
   the share plane or report the surface wired while binding nothing —
   the born-gated bar: a gate must live at a door that can *refuse*;
   (b) `welcome_deliver_core` is the ONE production door through which a
   new member reaches a claimed set's roster — the same-nest register and
   the cross-nest federation forward both fork inside it — and it already
   refuses first-reach Welcomes today (the reach floor), so a refused
   admit is established, recoverable behavior (nest state untouched; the
   owner's committed-but-un-Welcomed MLS add is cleared by the ordinary
   evict/rotate-on-removal path — no roster strands behind the crypto);
   (c) established recipients — roster or foreign-member rows — are
   in-band re-Welcomes: no gate, no spend, per the newness-delta rule, so
   idempotent retries cannot drain the unrefundable counterparty quota.
   `fauna.folders.share` (`share_core`) deliberately carries **no** gate
   call: it is per-set rather than per-member, skippable by a patched
   client for members 2..N, and a second spend point would double-count.
   The gate call is **unconditional** (no `cfg`): excising the
   `p2p-share` cargo feature removes the P2P transfer plane, not
   nest-mediated folder sharing, and the admission fan-out is the same
   bound in every flavor. Pinned through the real handler in
   `bins/fauna-nest/tests/conformance_feature_gate_p2p_share.rs`.
   And `p2p-share.transfer` composed
   **client-side at the transfer surface** (tier-1 constants ship in the
   artifact; the app is the enforcement point for a plane the nest never
   sees — [`../architecture/dynamic-features.md`](../architecture/dynamic-features.md)
   § Evaluation points) — **the MECHANISM is DISCHARGED 2026-08-18**: the pump prices every page through the ONE shared
   `feature_verdict` before any byte moves
   (`share_pump::{transfer_verdict, TransferUsageLedger}` — nest-served
   effective policy when reachable, the artifact's tier-1 constants
   offline), over a device-local day-bucketed ledger (meta-table, one
   owner, newness-delta counterparties, pruned past the largest window;
   device-local because the nest cannot arbitrate what it cannot see — the
   fleet-summed refinement is named forward work). One operation = one
   file transfer, ruled here. **Both metered dimensions are counted by the
   PULLING side, never taken from the counterparty (ruled 2026-08-20):**
   ops are bodies that actually arrived and volume is the bytes that
   actually arrived — `share_pump::SpoolSpend`, counted inside
   `spool_planned` as each body lands, discarded bodies included.
   The change row's own `size_bytes` may *price* the pre-authorisation
   (the plan is put to the gate before any byte moves, which is the point
   of pricing it) and may do nothing else: it is the serving peer's
   assertion about its own transfer, and a counterparty's assertion is
   never what the meter spends. For the same reason the record is
   unconditional on **arrival**, not on
   materialization — whether a row materializes is equally the peer's
   choice, and a page of rows the ingest's path guard refuses moved every
   byte. Consequence worth stating: volume is WIRE spend, so a compressed
   or encrypted body meters below its plaintext size, and a body fetched
   for a row that never materializes still meters. Over-counting against
   the counterparty is the sanctioned direction; under-counting on the
   counterparty's say-so is not a direction at all. A refusal stops the
   page and rides `SetPullOutcome::refusal` for the surface to render
   (Dim-3). The
   serve side stays bounded by the listener-level quotas above;
   feature-plane accounting of served bytes is named forward work. Open
   remainder: the SURFACE that renders the verdict (the tui lead's UI
   half).

## Cross-nest federation

> **Implementation status (2026-06-28): the bespoke nest↔nest WireGuard auto-peer
> path has been REMOVED as dead code.** The former `auto_peer_with_nest()` /
> `load_federation_peers()` (nest) and `register_with_nest()` (fauna-sync) all
> POSTed to an HTTP route deleted in the WS-RPC-everywhere rip-out, so they only
> logged a 404 warning and never established an inter-nest tunnel. They were
> gutted in the dead-code cleanup (the in-memory `FederationRegistry` is
> retained — still read by discovery / pairing / nest-sync). The live nest↔nest
> carrier is the **WS-RPC federation channel over TLS** (mutual nest-key auth,
> `fauna.federation.*`; `federation.md`), which the private↔relay deployment
> uses **without** a WireGuard tunnel (`deployment-home-with-public-relay.md`).

Linking two nests with a direct P2P tunnel remains a **demand-driven
target-state capability** (no live consumer): the substrate is decided (iroh,
behind the `fauna-transport` seam). Any future revival must (a) carry
peer-registration, if it needs any at all, on a **federation-channel** kind
(mutual nest-key — a nest is not an actor on a peer nest, `federation.md`),
never a bearer client kind; and (b) ride the seam. Under iroh it likely needs
none: a `NodeId` is the key itself. (The bearer kind this warned against,
`fauna.wireguard.peer.register`, no longer exists — deleted 2026-08-23.)

---

## Element IDs

Page elements (from ui.yaml): `p2p-tab`, `p2p-tunnel-toggle`,
`p2p-node-id-copy-btn`, `p2p-lan-copy-btn`, `p2p-contact-row`,
`p2p-contact-name`, `p2p-contact-remove-button` (indexed, user-approved
2026-08-15 — apps row 128; linux only today, see the per-app list below),
`error-message`.

No components: `p2p-wireguard-form` was **removed 2026-08-23** with the
WireGuard stack, along with the five `wg-*` ids it carried (`wg-pubkey-input`,
`wg-device-name`, `wg-listen-port`, `wg-register-button`,
`wg-unregister-button`) and three page elements — `p2p-wg-key-copy-btn` (no WG
key exists), `p2p-stun-copy-btn` (the nest STUN server that fed its row went
with the stack, so nothing can populate it) and, on the devices page,
`peer-wg-key-copy-btn`. Two survivors were **renamed onto substrate-neutral
ids** in the same change: `wg-register-button` → `p2p-tunnel-toggle` and
`p2p-tunnel-ip-copy-btn` → `p2p-node-id-copy-btn` (an iroh `NodeId` is the
dialable address; there is no tunnel IP). `p2p-lan-copy-btn` deliberately
**stayed** — the LAN arithmetic outlived the stack.
(`p2p-invite-copy-btn` was **removed** 2026-08-18, rule A, user-approved —
§ No pairing step, ever.)

Per-app surfaces today (see the § Implementation status matrix):

- **linux** — Settings → P2P tab (`src/settings/p2p_tab.rs`). The historic
  "Peers page overlaps `devices.peers`" drift item is **resolved by the
  2026-06-28 unification**: the Devices page is roster-only
  ([`../ui/devices.md`](../ui/devices.md)) and P2P is its own settings
  surface. **Contacts group (2026-08-15, row 128):** renders every
  `list_p2p_enabled()` contact as a `p2p-contact-row`, refreshed after a
  removal and on every re-nav to the page — the only app with a contact
  list today. Its Accept-Invite add flow was **retired 2026-08-18**
  (§ No pairing step, ever); no app has an add-contact flow now.
- **web** — settings redirect stub; no dedicated P2P surface (relay-only by
  design).
- **windows / tui / macos / ios / android** — **no page surface.** windows and
  tui each had one until 2026-08-23; both were WireGuard peer-registration
  forms (tui's built 2026-08-10 on windows' pattern), and both went with the
  stack. android has **no peer node either, and no caller left to grow one** —
  its dead `P2PTunnelService` (manifest-declared, called `peerTunnelStart`,
  but nothing ever started it) was deleted 2026-08-25 (corrected 2026-08-24;
  deleted row 56; § Implementation status today). The earlier reading
  "android hosts the node headlessly via its foreground service" was never
  true of a running app.

  **This is a declared absence, not a parity gap** — including for tui, whose
  parity is otherwise a standing requirement. There is no P2P feature left for
  those apps to lack: registration does not exist under iroh (a `NodeId` is
  the key), so the only page anyone still renders is linux's local-node
  Start/Stop surface, which is a different thing from what tui and windows
  had. If the page trickles out later it does so with the contact-list opt-in
  when § Inbound authorization's trigger fires, as one shape on all 7 apps.

### RATIFIED 2026-08-24 — the `p2p` page does not trickle down; 1-of-7 is the target shape

The design pass row 67 asked for, answered against code rather than against the
page's WG-era silhouette. **There is no seven-app P2P page to build**, and the
`p2p` page's absence on six apps is target state, not a parity queue. Three
findings carried the decision; each is a fact about current code, cited.

**1. The live cross-user p2p surface is already shipping — on the `folders`
page, not here.** `share-serve-status` and the `share-transfer-*` family, and
the `offline-share-*` / `offline-receive-*` ceremony family, are `folders`-page
elements (ui.yaml `pages.folders`), built on tui (2026-08-19) and linux
(2026-08-20/21). That placement is right and stays: the user's object is the
shared folder, not the transport under it. So the parity work the fleet
actually owes is **the remaining five apps' share-plane hosts and renders on
`folders`** (§ Implementation status today owns that ledger and each app's
gate), never a P2P page. A session reading
"1-of-7" as drift would queue six renders of the wrong surface.

**2. `p2p-tunnel-toggle` drives the DORMANT node, and the live listener binds
without asking anyone.** linux's toggle calls `P2pService::start_tunnel`
(`apps/fauna-linux/src/p2p.rs:117`), which hosts a `PeerNode` serving the base
`fauna.peer.*` kinds only — `node_info` and the vestigial `exchange` — and
carries no live traffic (that module's own header says so). It is started on
demand and **persisted nowhere**: the choice does not survive app restart. The
listener that *does* move user bytes is the share plane's seat, and it binds
**unconditionally at the account-store-ready edge on both built apps** —
`apps/fauna-tui/src/app.rs:3011` (`spawn_share_glue`) and
`apps/fauna-linux/src/account_runtime.rs:206` (`share_glue::start`) — neither
of which consults the toggle, or any user-set state at all. tui in fact binds
**two** listeners nobody can decline: the share seat above, and the
same-account peer-sync leg it hands a `peer_transport` to at session assembly
(`apps/fauna-tui/src/session.rs:966`; the leg's own listener is
`libs/fauna-peer-sync/src/server.rs:706`) — and tui is the app with no P2P page
at all. linux is the mirror image: it passes `peer_transport: None`
(`apps/fauna-linux/src/account_runtime.rs:156`), so the one app that *has* the
toggle runs fewer unconditional listeners than the app that has no control
surface whatsoever. Trickling the
toggle to six apps would therefore ship six controls over the node that does
nothing, while the node that does something stays uncontrollable everywhere.
The control that is actually owed is a different control, and it is captured as
its own track (finding 4 below).

**3. Both copy buttons lost their user job under iroh.** `p2p-node-id-copy-btn`
copies this device's node identity — but a `NodeId` *is* the actor's Ed25519
key and there is nothing left to register it with (§ No pairing step, ever), so
no flow consumes a hand-copied one. `p2p-lan-copy-btn` is vestigial for the
same reason, not for the bug it happened to also carry: since 2026-08-19 the
compare code **carries** the initiator's bound LAN endpoints (§ Offline share
initiation → contract point 1, *The compare code carries the addressing*) —
precisely the flow that used to need them — so no flow consumes a hand-copied
address either, regardless of whether the button hands over one. (It used to
be worse than vestigial: until fixed 2026-08-25,
`get_lan_addresses` returned **interface names**, not addresses at all — its
`/proc/net/fib_trie` branch read nothing out; it now reuses
`fauna_peer_sync::lan::discover_lan_candidates`,
`apps/fauna-linux/src/settings/p2p_tab.rs:413`.) Both stay on linux as
**diagnostics**, which is a legitimate thing for one app to have and a poor
thing to render six more times; they do not trickle. (The status page already
points at the node-id readout — `apps/fauna-linux/src/views/status.rs:578`.)

**4. The one real gap this pass found is NOT a page — it is a missing user
control, and it is captured, not closed here.** Wormability § Wormability walk
rule 5 states the share listener's kinds "exist on the listener only while p2p
participation is on (the contact-plane node's lifecycle — no unconditional
bind)". **That sentence is false in code today**, in both halves: the share
seat does not ride the contact-plane node's lifecycle (it binds its own seat,
finding 2), and its bind *is* unconditional with respect to any human — the
only brakes on it are the `p2p-share` cargo feature and the nest's `p2p-share`
capability advertisement, and that advertisement is a **compile-flavor**
decision explicitly ruled "never a human config knob"
(`bins/fauna-nest/src/discovery_core.rs:148` — `advertised_capabilities` reads
`cfg!(feature = "p2p-share")`). No user or admin on any of the seven apps can
turn their device's share listener off. Against [`../principles.md`](../principles.md)
§ One configuration surface's own test — *would a user or admin ever want to
choose this?* — "does this device run a network listener that serves my files
to my contacts" is unambiguously yes, which puts it in bucket (2): app UI on
all 7 apps, persisted. **What that control is, where it lives, and what backs
it is a security-boundary design question this pass deliberately did not
settle** (device-local state versus a per-device row on the nest-authoritative
devices roster is a real fork with a real posture difference — nest-held state
that can *enable* a listener remotely is a worse posture than one that can only
brake it), and it needs a new ui.yaml element, so it carries a rule-A user
gate; the gap itself is
declared in § Implementation status today. **Answered 2026-09-25 (user
ruling): device-local authority, a nest row that can only brake, an indexed
toggle under `device-card` — § Per-device participation.**

**Two per-app claims this pass found FALSE; both resolved 2026-08-25 (rows
56 and 36).**
ui.yaml's android note said android "hosts the peer node headlessly via its
foreground service" — `P2PTunnelService`
(`apps/fauna-android/.../core/P2PTunnelService.kt`) called `peerTunnelStart`,
but **nothing ever started the service**: the app's only
`startForegroundService` call targeted `SyncService`, so android hosted no
peer node at all and the FFI `tunnel` feature it compiled reached no caller.
Rather than wire it, the dead service, its manifest entry and the `tunnel`
build flag were deleted (§ Implementation status today → *Two per-app claims
corrected*). And linux did not render the `error-message` this page declares
(ui.yaml `pages.p2p`) — a failed start used to write into an untagged status
row (`p2p_tab.rs:148`), leaving convention 2's read-the-error-first rule
nothing to read on the one app that has the page; now a hidden-until-set
`error-message` label carries it, built + cleared per convention 2's
2026-08-04 rider.

**Unchanged by this ratification:** the `p2p-contact-row` family stays
sequenced behind § Inbound authorization's trigger, exactly as that section and
§ No pairing step, ever already rule — this pass did not front-run it.

---

## Security properties

| Property              | Detail                                                                                       |
|-----------------------|----------------------------------------------------------------------------------------------|
| **Encryption**        | QUIC/TLS 1.3 with the Ed25519 `NodeId` as the authenticated identity. |
| **Authentication**    | `peer_identity()` is intrinsic to the QUIC handshake (PT-1b) — the handshake IS the proof, with no registry-trust dependency. The deleted WireGuard impl needed the opposite: a nest-side registry mapping a registered public key to an endpoint, which is why its removal deleted a trust dependency rather than adding one. |
| **No kernel dependency** | Userspace quinn — no root, no admin, no kernel module, portable across all supported platforms. |
| **Automatic fallback** | If no direct path establishes or the tunnel fails, traffic routes through the nest. The nest path is always available (§ 5 — hard-coded, silent). |
| **LAN privacy**       | LAN detection is a local arithmetic check against RFC 1918 ranges. No broadcast packets, no mDNS queries, no network scanning. |
| **Forward secrecy**   | Ephemeral Diffie-Hellman exchange (both substrates) — session keys are not derivable from the static keypairs alone. |

### Wormability posture (ratified 2026-08-10)

A p2p data plane gives every participating device a network **listener** — a
remotely-reachable parser on consumer devices, homogeneous fleet-wide (one
bug is every device's bug). A worm needs an RCE, a way to find targets, and
authority to reach them; the posture bounds all three. Full analysis +
exposure inventory: the 2026-08-10 p2p content-sharing considerations doc
(internal plans tree — tracked internally, not shipped) § 3. The binding
rules (any new p2p kind or listener change must satisfy them, refutable by
the security review):

1. **Admission before parsing.** Everything beyond `fauna.peer.node_info`
   authorizes against the P2P contact set / `DeviceAuthorization` *before*
   any data-plane parsing (§ Inbound authorization above). Propagation is
   thereby bounded to edges both endpoints chose (own devices, mutual
   contacts) — and there is **no global address space to scan**: endpoints
   are learned only via authenticated channels (no DHT, no broadcast).
2. **Memory-safe stack on the pre-auth path.** Rust + quinn + rustls only;
   no hand-rolled pre-auth parsers. Standing dependency-audit obligation.
3. **Least-kind dispatcher.** The p2p dispatcher allowlists
   content-transfer kinds only — never config, capability-mint, admin, or
   key-material kinds. A fully compromised peer can at most feed content.
4. **Received content is inert at the transfer layer.** Peers exchange
   hash-addressed AEAD ciphertext; nothing received is interpreted or
   executed there — a poisoned chunk fails its hash/tag check and is
   refused. (Post-decrypt media decoding is the app's ordinary rendering
   path, unchanged by p2p.)
5. **No listener when off; compiled away when excised.** p2p disabled ⇒ no
   socket. A build flavor with the sharing feature excised
   ([`../architecture/dynamic-features.md`](../architecture/dynamic-features.md)
   § Compile-time excision) ships no share data plane at all.
6. **Revocable per-device authority.** Device revocation severs peer
   admission at the next handshake; contact removal severs the edge;
   rotate-on-removal severs content. No ambient authority on any device.
7. **Version brake.** The nest's capability advertisement can stop
   advertising a p2p capability fleet-wide — the emergency stop for a
   shipped wire bug (accepted limit: client update latency).
8. **Bounded fan-out.** Per-peer and per-window connection/rate quotas at
   the listener. Beneath them, `PeerNode`'s one accept loop never waits on a
   single peer: an admitted connection gets a hard-coded window
   (`INBOUND_STREAM_ACCEPT_TIMEOUT`) to open its first stream on its own task,
   at most `MAX_PENDING_INBOUND_ACCEPTS` wait at once node-wide (a fresh NodeId
   is free, so a per-peer count cannot bound them), and an inbound connection
   is released the moment it ends.

The same-account peer leg's per-rule compliance record lives with that
leg's owner:
[`../architecture/account-sync-plane.md`](../architecture/account-sync-plane.md)
§ The peer leg → Wormability walk (ratified 2026-08-11).

### Per-device participation — rule 5's off switch (ratified 2026-09-25, user-directed; build status in § Implementation status today)

**The control.** Every device runs, or does not run, the two peer listeners this area owns — the cross-user share seat and the same-account peer-sync leg — and dials peers for either plane only while it runs them. Whether it does is a **user choice per device** ([`../principles.md`](../principles.md) § One configuration surface, bucket (2)): `device-p2p-participation-toggle`, indexed under `device-card` on the devices page, on all 7 apps (ui.yaml `pages.devices` owns the id; the page's own UX rules are [`../ui/devices.md`](../ui/devices.md)'s). It is a toggle and not a derived marker because participation cannot be derived from anything, unlike `device-keyless-posture-badge`. The default is **on** — the works-out-of-the-box posture every device has had since the planes shipped — and off means: no socket of either kind, no serving, no peer pulls, no endpoint advertisements, and a co-present ceremony panel that refuses to bind. Nest-mediated sync and sharing are untouched: the nest stays the always-on path, and participation only decides whether this device also takes the direct one.

**Authority — asymmetric (user ruling 2026-09-25).** *The device itself is the authority for its own participation*, consistent with [`file-sync.md`](file-sync.md) § P2P peer state's device-local peer state: the fact lives in the device's own account store, one meta-table row per (device, account) (`fauna_sync_engine::p2p_participation`), which is exactly the store its co-located sync agent — the process that usually holds the same-account listener — shares with it; an absent row reads *on*. *A nest-side row may only ever bring a listener DOWN, never up.* The nest's `sync_devices` row carries two additive columns: `p2p_participation`, the device's own last report (`on`/`off`, NULL = never reported — what a sibling's card paints for it), and `p2p_off_requested`, a pending brake. The one kind, `fauna.sync.devices.p2p_participation.set` `{device_id, participating, [timestamp_ms, nonce, signature]}`, has the two arms `fauna.sync.device_grant.revoke` has: the **owner arm** (the account session alone) may set `participating: false` on any of the account's rows, which only raises `p2p_off_requested`, and is refused `permission_denied` on `participating: true`; the **self arm** (a proof of possession by the row's own principal, `sig_domain::DEVICE_P2P_PARTICIPATION_V1` over `actor_id ‖ device_key ‖ device_id ‖ participating ‖ timestamp_be ‖ nonce`) records the device's report, and clears the pending brake only when that report is `off`. A device honours a brake by **folding** it: on each full pump pass the elected engine holder reads its own row, and a pending brake becomes local `off`, acknowledged by a self-arm report of `off` — so a brake set while the device was away lands at its first pass back, and enabling is a local act on the device alone (the own-row toggle writes local `on` and reports it). There is no nest state a sibling can flip that ends in a listener coming up.

**Enforcement is shared code at the two bind doors, never per app.** (a) The same-account leg: `fauna_sync_engine::peer_leg::ensure_bound` reads the row before the brake; off drops the node, its serve side and the transport (the listener and the dial endpoint are one iroh endpoint) and answers `PeerLegPass::ParticipationOff`, so the pump's dial pass runs nothing. (b) The share seat: `fauna_sync_engine::offline_share::SessionSeat` carries the verdict — `get_or_bind`, the one body under both the panel's door and the driver's, refuses while off, and `unbind` drops a bound seat; the shared driver `share_glue::run` selects on the runtime's participation watch beside its tick, unbinds on off and reads `ServeStatus::ParticipationOff` on `share-serve-status`. Every host of the driver (tui, linux, and the `fauna-ffi` share-plane host windows, macOS, iOS and android reach) inherits both. (c) Promptness: the own-row toggle writes the row and wakes this process's own runtime at once, so the share seat drops within the pass it triggers; the same-account listener drops within the gesture too, wherever the engine lives — the door runs the holder's pass itself when this process holds the engine, and otherwise asks the holder (the co-located sync agent on a desktop) for that pass over the agent's control plane ([`../architecture/apps/sync-agent.md`](../architecture/apps/sync-agent.md) § Control plane split, `ReconcileAccountRuntime`). The ask is best-effort and sent only after the row rests: the store row is the fact both read, so a nudge that never arrives costs only the latency of the holder's next full pass — a reconnect wake or the 300 s backstop ([`../architecture/account-sync-plane.md`](../architecture/account-sync-plane.md) § Nudges and backstops) — and nothing depends on a wake arriving.

**Web** hosts no listener, so it has no own row to render the local arm on; every device it lists is a sibling, and its toggle is the owner arm only (request off).

**Which row is this device's** — decided by the devices machine, never an app: the door's enrolled row, else the row whose principal is this device's own fleet id, else the app's own device id handed in through `DevicesMachine::set_this_device_row` (the id its `device-this-mark-badge` paints from, so the switch and the marker agree). The last fallback exists for a native seat with no running account runtime — neither of the first two answers there — whose own row must take the OWN arm and paint the door's refusal on `error-message`, never the owner arm's brake against itself. The machine publishes the answer as well as acting on it: every row's `DeviceSummary.p2p_participation_paint` — own, checked, label, actionable — is drawn by the same rule the gesture takes its arm by, so an app draws that paint and never re-derives own-ness, and the checkbox and its click cannot disagree; with no door (web) no row is this device's.

**Tests that pin it** (rule 5's "no socket" is asserted on the socket, never on a flag): the peer leg over the in-memory transport — bound, then off, then the listener's accept side is gone; the seat — bound, then off, then unbound and a fresh bind refused; the iroh endpoint — a bound UDP port is free again once every handle drops; the nest — owner arm brakes, owner-arm enable refused, self arm reports and clears; and the tui journey — the toggle on this device's own card flips, survives a relaunch, and the plane's own reading says off. The file names are in § Implementation status today.

---

## Architectural rules

1. The `p2p` page is linux-only by ratified target shape (§ RATIFIED
   2026-08-24 — this rule read "feature parity across all 7 apps is the
   page-surface target" before it); the surface owed on all 7 apps is the
   `folders`-page share family, and the data plane lights up fleet-wide behind
   the seam, never per-app.
2. Observer-driven rendering once a shared snapshot exists.
3. All substrate work goes through the `fauna-transport` seam; per-app
   code never touches quinn directly.
4. The node identity is the actor's own Ed25519 key, already in the platform's
   secure store — there is no separate transport keypair to persist since the
   WireGuard stack was deleted 2026-08-23.
5. Whether a device runs its peer listeners is that device's own choice,
   read at the two shared bind doors and nowhere else (§ Per-device
   participation); an app renders the toggle, never a gate of its own.

## Don't do these

- Don't implement tunnel configuration logic per-app.
- Don't keep the node identity (the actor's Ed25519 key) outside the
  platform secure store, or mint a separate transport keypair (rule 4).
- Don't hand-roll a per-app path choice or fallback decision: iroh's `dial`
  selects the path (the caller-driven cascade was deleted 2026-08-23, § 5),
  and the always-available nest-mediated path is what makes "no direct path →
  nest fallback" transparent.
- Don't add a fallback-policy knob (config file, env, or otherwise) — the
  fallback is hard-coded silent (§ 5).
- Don't let any nest-held state bring a listener UP: the nest row can brake
  a device's participation, and only the device itself can enable it
  (§ Per-device participation).

## Done definition

- [ ] The remaining five apps host the share plane and render the
      `folders`-page share family (§ RATIFIED 2026-08-24 finding 1 — this
      replaces the earlier "all 7 apps render the full ui.yaml `p2p` element
      set", superseded there: 1-of-7 is the `p2p` page's target shape).
- ~~Registration / unregistration goes through shared Rust on every app.~~
  Retired 2026-08-23: registration does not exist under iroh — a `NodeId` is
  the key (§ No pairing step, ever).
- ~~Tunnel private key persists in the secure store on every app.~~ Retired
  2026-08-23: there is no separate transport keypair (Architectural rule 4).
- [x] The WG-era wake/signaling obligation retired (Y.1 reframe, 2026-08-10)
      — no nest emitter is owed; the frame stays registered, dormant.
- [ ] The peer data plane carries live chunk transport over the seam (Y.1)
      via the account-plane W2 peer leg + the § Cross-user shared-set
      transfer twin (that workstream owns sequencing; outbound
      `PeerNode::dial` gains its production consumer there) — **the dormant
      posture was NOT re-ratified (user ruling 2026-09-26): build it**, tui
      first; the `device-to-device-file-transfer`
      catalog page holds the three § Goal promises as its outcomes.
- [x] `FallbackPolicy` enum + config shape deleted (2026-07-12).
- [ ] `tests/e2e-unified/ui-actual-<app>.yaml`'s `p2p` block refreshed;
      `ui-actual-lint` introduces no new errors.

## Reading list

1. `principles.md` — product invariants + engineering principles.
2. `tests/e2e-unified/ui.yaml` — the `p2p:` page block.
3. `libs/fauna-transport`, `libs/fauna-iroh`, `libs/fauna-peer-channel`, `libs/fauna-peer/src/`, `bins/fauna-iroh-relay` — current implementation.
4. [`file-sync.md`](file-sync.md) — underlying sync protocol P2P uses for chunk transport.
5. The seam design (ratified 2026-06-27, tracked internally); the Y.1 envelopes design (ratified 2026-05-04, tracked internally).
6. `tests/e2e-unified/ui-actual-<your-app>.yaml`.
