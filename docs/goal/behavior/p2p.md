# P2P (Direct Contacts) — target state

Owns: p2p, p2p-participation
Status: ratified — the transport seam + substrate decision (iroh, adopted 2026-06-28/30); the **fauna-peer → Y.1 reframe was EXECUTED 2026-08-10** (refutable at build time), and the **WireGuard stack was DELETED OUTRIGHT 2026-08-23** (user-directed: "we are using iroh for now and there is no point in compiling wireguard"), so iroh-QUIC is the only substrate and the seam has one impl. The WG-era flow sections (wake signaling, NAT traversal, LAN file transfer) are re-derived over the seam from [`../architecture/account-sync-plane.md`](../architecture/account-sync-plane.md) § The peer leg; the data-plane build vehicle is the account-plane W2 (account-data-plane.md § Workstreams) workstream (peer leg + the § Cross-user shared-set transfer twin). § Wormability posture is ratified target state (2026-08-10, user-directed; refutable at build time), and the cross-user shared-set transfer plane — ratified the same day and partly built, its status in § Implementation status today below — is owned by [`p2p-shared-set.md`](p2p-shared-set.md)
Authority: the P2P optimization layer — the substrate-agnostic fauna-transport seam (dial/listen/PeerConn; the symmetric-key, auth-above-the-seam, and fallback-not-the-seam's-concern properties), the iroh-QUIC impl and the PeerNode lifecycle, LAN detection, tunnel lifecycle, the relay capability + iroh-relay sidecar advertisement, the p2p page behavior, and the same-account plane's nest-down folder-transfer ruling (§ Same-account nest-down folder transfer: what rides without a nest, what stays nest-arbitrated); the cross-user shared-set transfer plane — its contract, offline share initiation and the delivery seat, peer-served change-row provenance and the wormability walk → [`p2p-shared-set.md`](p2p-shared-set.md) (split 2026-11-04); its build contract, build ledger and phone-peer design → [`p2p-shared-set-build.md`](p2p-shared-set-build.md) (split 2026-09-27); ui.yaml (`p2p` page) owns element IDs + per-page element scope; defers the PQ posture to [`../architecture/security/post-quantum.md`](../architecture/security/post-quantum.md), nest↔nest channel auth to [`../architecture/federation.md`](../architecture/federation.md), SNI routing of `relay.<domain>` to [`caldav-server.md`](caldav-server.md) (registry: sni-routing), the chunk protocol to [`file-sync.md`](file-sync.md), and the relay-SAN/cert coupling to [`../architecture/nest/tls-certificates.md`](../architecture/nest/tls-certificates.md). **There is no WireGuard concept to own any more** — the stack, its `fauna.wireguard.peer.*` / `fauna.admin.wireguard.*` kinds, its nest config section and its NAT-signaling mechanism were all deleted 2026-08-23 (see § Implementation status today → *The WireGuard deletion*).

## Implementation status today

**The SAME-ACCOUNT peer data plane carries file bodies since 2026-10-08 — built in shared Rust and the per-user sync agent, witnessed on tui, windows, linux and macOS** — read this paragraph's scope carefully, because the paragraph below it is about the other plane. The user ruled 2026-09-26 that it be built rather than left dormant, shared Rust first then tui, witnessing the three § Goal promises the `device-to-device-file-transfer` catalog page holds as outcomes. **Built:** a file-sync download in the agent asks this account's other devices for its chunk bodies before the nest, over the channels the same-account peer leg's dial pass admitted (`fauna.peer.sync.chunks.pull`, answered through the serving seat's one serve core — [`relay-serving.md`](relay-serving.md) § Relay serving), and fetches from the nest only what no sibling served; a sibling that is off, refuses, misbehaves or is reachable only over a relay while the nest answers moves no byte, and the bodies a sibling did serve are kept (`fauna_sync_engine::sibling_source::sibling_first`). Manifests and the change feed stay nest-mediated while a nest answers: the plane takes the nest out of the BYTE path, not the control path — and since 2026-10-14 a folder's change rows and manifests also ride sibling to sibling, provisionally, so a write still crosses with no nest answering (§ Same-account nest-down folder transfer; built in shared Rust, unwitnessed). **Witnessed on tui 2026-10-08** (`tests/e2e-unified/tests/test_device_to_device_file_transfer.py`, outcomes 1–3 and 19): two tui seats of one account, each with its own agent up beside the running app — the agent holds the engine role there and its peer leg carries the bytes ([`../architecture/account-runtime-roles.md`](../architecture/account-runtime-roles.md) § Multi-instance concurrency → *The agent holds the role when present*) — admit each other with no gesture, and a file written on one into a metadata-only folder lands on the other with every chunk served by the sibling and none left to the nest; a file written while the other seat is away, whose writer is then gone too, still lands through the nest when that seat comes back. The observable is the receiving agent's own reading of the plane, the `peer_files` field of its `GetServiceStatus` reply (siblings held, chunks and bytes a sibling served, chunks left to the nest, and pieces a sibling served that failed their hash check and were refused). **Witnessed on windows 2026-10-09** by the same two journeys on two windows app seats, each with its own agent on a per-seat pipe: the plane needed nothing platform-specific, and because a windows agent keeps running after its app quits (it keeps syncing app-dead — [`../architecture/apps/sync-agent.md`](../architecture/apps/sync-agent.md) § Packaging + lifecycle), a seat that goes away also stops any agent still serving its pipe, through the agent's own `Shutdown` request, so "away" means the whole device (under the harness the bridge's app kill takes the agent with it, the app's own child, so that stop is the guarantee rather than the mechanism). **Witnessed on linux 2026-10-13** by the same two journeys on two linux app seats, each app direct-spawning its own agent on a socket in its own runtime dir; nothing platform-specific was needed, and a stopped seat's agent goes with its app. **Witnessed on macOS 2026-10-28** by the same two journeys on two macos app seats, each app spawning its own child agent on a socket under its launch's own HOME, and on two tui seats on the same machine; nothing platform-specific was needed, and a stopped seat's agent goes with its app's process group. **Rule 4 (§ Wormability posture) witnessed on tui, linux and macOS 2026-10-29** (`test_a_tampered_piece_from_your_other_device_is_refused`, outcome 6): one seat's agent is made to serve a forged copy of every chunk body through a fault arm compiled into test builds only (`RequestMethod::ArmPeerServeFault`, the agent's `peer_files::ServeFault`, convention 15), and the other seat refuses each forgery on its completion check (`fauna_peer_sync::ranged::ForgedBody`, counted as `refused` in its reading), keeps no body from that sibling, and lands the file byte for byte from the nest in a folder whose bytes the nest holds; the windows leg is the ordinary trickle-down. **Two tui seats on windows: red from their first run there (2026-10-31); the cause found and its trigger removed 2026-11-02; re-run on windows 2026-11-04, the direct transfer (outcomes 1–3), the tampered piece (outcome 6) and the nest fallback pass, the fallback intermittently caught by the deafness below.** The nest-down journey (outcome 7) passes there since 2026-11-05, once the cfapi on-demand host — every folder of a windows agent — ran the sibling row pull (§ Same-account nest-down folder transfer). One journey stays red there for a cause of its own: the three-seat placeholder journey, which the deafness below breaks when one seat's agent shuts down. Each tui app holds the engine role for its first seconds, binds the device's NodeId and publishes that listener; a sibling's agent connects to it; then the app yields to its own agent ([`../architecture/account-runtime-roles.md`](../architecture/account-runtime-roles.md) § Multi-instance concurrency → *The agent holds the role when present*, part 3), which binds afresh on another port. The yield dropped the app's endpoint without closing it, so the connected sibling kept sending into the port the app left until its idle timer gave up — and on Windows that is fatal beyond the one connection: the ICMP port-unreachable that answers a send into a dead port makes the iroh socket's next receive fail with `WSAECONNRESET` (os error 10054), after which that socket delivers no datagram at all, so the sibling's healthy connection to the other seat's agent timed out with it (measured: the last datagram received one second before the first 10054, none after, while the other agent went on sending for a minute). Linux and macOS ignore an ICMP on an unconnected UDP socket, which is why only Windows showed it. The stand-down now closes the transport, so the sibling is told at once. **The deafness itself is iroh 1.0's, on Windows, and still follows any other send into a dead port** — a sibling whose agent crashed or restarted on a new port while its old candidate is still published, a stale candidate a dial pass reads before the republish arrives; the remaining drop sites and the dependency path are captured. A relay that refuses its port does it too: a windows endpoint whose relay sits at a refused port took 4–12 s to dial a LAN candidate, against 92 ms with no relay (`fauna_iroh`'s `an_unreachable_relay_leaves_a_direct_lan_dial_as_fast_and_sure`, red on Windows on that bound). **Not yet:** the catalog page's remaining per-app absences: web is absent from the whole page, and android and iOS are excused from outcomes 1–8, 10, 11 and 16–19 (owner-approved 2026-10-16) because neither phone runs the per-user sync agent that carries the same-account plane's file bodies, while the participation-switch outcomes 9 and 12–15 stay owed on android; on iOS the journey's `tui+phone` pair, a witness of its own mapped to 12–15 alone, passes (2026-10-29), and only outcome 9's run is owed; a cut part-way through one file is pinned at the Rust tier alone (`peer_dial_convergence.rs`). **The CROSS-USER
share plane is a different story and is live**: on tui (2026-08-19) and linux
(2026-08-20/21) real user bytes move peer-to-peer, proven by an app-level
two-actor journey with the nest down (§ Cross-user shared-set transfer). A
session quoting "the peer data plane is dormant" at the share plane is quoting
the wrong paragraph.

**Offline AUTHORSHIP on the share plane was red from 2026-09-27 to 2026-10-02, and is restored.** Between those dates a write made on a shared set while the set's row could not be read was not sealed at all — the sync agent's pre-seal hold refused it — so no own-pending row was minted, and the two nest-down journeys of `tests/e2e-unified/tests/test_share_pump_two_actor.py` (`test_member_pulls_offline_authored_file_from_owner_peer`, `test_an_interrupted_peer_transfer_resumes_without_resending_what_arrived`) failed with the file never arriving. Decision 2′ of [`on-demand-files-shared-sets.md`](on-demand-files-shared-sets.md) § Shared sets on a capability host, built 2026-10-02, moved the hold from the seal to what is published to a nest: the write seals under the last floor read and its pending row serves on this plane, and nothing of it reaches a nest until a row read checks the floor; that section owns the rule, its built shape and its cost. The paragraphs below that cite those two journeys as green describe the state before 2026-09-27 and since 2026-10-02.

**The co-present ceremony's journey is green on tui, linux, windows (2026-09-15) and now macOS too (re-verified 2026-09-22).** On windows the dial never landed before 2026-09-15: every compare code carried a candidate pairing the `[::]` socket's port with an IPv4 address, which has no listener where that socket is v6-only (§ Offline share initiation, contract point 1). `test_a_co_present_ceremony_lists_the_shared_set_on_both_seats`, the § Offline share initiation *Proof*, last failed on macOS on 2026-08-27 — before that candidate fix, before `fauna_sync_engine::offline_share::flush` stopped erasing a deliver that lands while the recipient is still persisting its consent. Windows and macOS both re-read the group listing on the edge a ceremony lands a scope, through the FFI's `lands_a_scope` export, as tui and linux do.

**The ceremony's record rests on the device's own account store — the account plane's `fauna.state.group-share-ceremony` kind, born plane-only 2026-09-28 — so its PAGE surface answers with the nest unreachable, on every app.** § Offline share initiation's substantive offline claim is about the ceremony itself, and the cryptography keeps it; the *page that has to answer the offer* kept it only once its read stopped needing a nest. From 2026-09-22 the record was `UserConfig.group_shares`, read through `fauna_client_config::ConfigClient::load` — a `fauna.config.get` round-trip — so an unreachable nest blanked the recipient's consent card and both seats' scope rows unless the read fell back to the bound seat's in-memory copy. The E3 lead slice of the `__config` dissolution ([`../architecture/config-dissolution.md`](../architecture/config-dissolution.md) § Implementation status today owns the kind's build) moved the record to a fleet-only, tip-sealed plane kind, one byte-string row per ceremony side-record. The seat binds over an empty replica and each host lends it the record at its account-store-ready edge (`offline_share::lend_account_record` → `SessionSeat::lend_record`, § Offline share initiation → *The seat's record is lent late*); the lend and every later flush join both ways through `AccountStoreHandle::merge_group_shares`, so neither the store's gains nor the replica's in-flight ones are lost. `offline_share::load_group_shares(account, seat, own)` keeps the seat fallback with its reason changed: it reads the durable local rows (the lent record, else the runtime's handle) joined with the bound seat's replica, which answers alone for a frame ingested before the lend. The rows seal under the same generation tip as the ceremony's own held-root row, so the offline ceremony gains no new precondition; a device with no tip yet refuses the flush at the writer door (pinned by `fauna-sync-engine/tests/peer_leg_convergence.rs::the_group_share_ceremony_door_refuses_while_no_tip_resolves`), and two devices' records converge through the production door (`…::a_group_share_ceremony_record_converges_through_the_production_door`). It is one read on every app: tui and linux call it directly, and `fauna-ffi`'s `offline_share_load_group_shares` passes through (its `nest` parameter is unread now, kept so the windows, macOS, iOS and android calls are unchanged). Each app's wrapper opens its nest connection before the call, and only its own run of the journey below says whether that step fails first: macOS and windows reach the FFI with the nest down (windows witnessed 2026-09-29, once its folders page read the ceremony's half ahead of every nest round trip — sequenced behind the M2 peek or the machine refresh, a down nest had kept the card and the landed row from painting); iOS and android are still unwitnessed. Witnessed on tui, linux, macOS and windows by `tests/e2e-unified/tests/test_offline_share_two_seat.py::test_a_co_present_ceremony_completes_while_the_nest_is_unreachable`, which spends both nest-bound preconditions — the seat's bind and each device's first generation tip — before stopping the nest.

**The initiator's walk redials a dropped connection itself (2026-09-22).** § Offline share initiation's "a dropped co-present connection redials without re-scanning" had the admission half only: the recipient's record did admit a redial (`group_ceremony_peer::record_admits`), but the app's walk ended on any lost connection, and pressing Begin again minted a SECOND scope — a second card, not the same share picked up. `GroupShareInitiator::with_redial` now lets `drive` dial the same compare code again when the connection is lost after the offer crossed (a closed dispatcher or the synthesised disconnect — a refusal such as a decline stays terminal and is never redialed), keep asking about the same scope, and re-send the deliver it already built rather than minting a second generation, paced by `REDIAL_PAUSE` inside the step's existing budget. `offline_share::initiate` wires it, so every app that drives the ceremony through that door has it. Pinned by `fauna-client-capabilities/tests/group_ceremony_node.rs` and witnessed on tui by `tests/e2e-unified/tests/test_offline_share_two_seat.py::test_a_connection_dropped_part_way_picks_the_same_share_up_again`, which drops the recipient's connections through the compile-gated `offline_share_drop_connections` agent arm. linux has the arm too; the FFI apps reach the same call through the test-flavored `FfiCeremonySeat::drop_connections_for_test` (and the receive-window clock through `offline_share_advance_clock`) — macOS, iOS and windows carry their agent arms over them (windows since 2026-09-28), android still owes its own.

**An interrupted peer transfer resumes at FILE granularity, and the pull cursor now holds below a row that did not arrive (2026-09-23).** § Cross-user shared-set transfer's "an interrupted transfer resumes … with held chunks never re-sent" holds per file: the pump's fetch plan skips any row whose manifest the member already holds (`share_pump::plan_page_fetch`), and the ingest judges it current, so a file that arrived before the cut is never requested again. A file whose bodies were only partly fetched when the connection dropped is fetched whole on the next pass, because the spool keeps a file only once all its chunks are in; "held" means held in a landed file. Before 2026-09-23 the resume also had a hole for SEQUENCED rows: `always_resident::ingest_share_page` advanced the per-peer cursor to the page's highest seq even when one of its rows was skipped because its bytes never arrived, so the peer never served that row again and, with the nest down, it never arrived at all. The ingest now reports the lowest such seq (`PeerIngestReport::retry_floor`) and the cursor stops just below it; a permanent skip (an unsafe path, a row with no manifest, since 2026-10-11 a fetch failure no retry can cure — `apply_failure::permanent_reason`, e.g. a manifest from a newer format — and since 2026-10-14 a file the per-operation cap kept out) still moves it on. A sequenced row whose peer-chosen seq is not positive or not above the `since` the page was asked for is a protocol violation, refused at the page door before any fetch or cursor arithmetic (`always_resident::ingest_peer_page_landing`, both the share and the sibling origin; since 2026-10-11), so a hostile seq can neither panic the ingest nor move the cursor — pinned by `peer_share_ingest_test.rs::a_peer_served_seq_at_the_extremes_is_refused_without_a_panic` and `…::a_permanent_fetch_failure_does_not_hold_the_pull_cursor_but_a_transient_one_does`. Pinned by `peer_share_ingest_test.rs::a_row_whose_bytes_did_not_arrive_holds_the_pull_cursor_below_it` and witnessed on tui by `tests/e2e-unified/tests/test_share_pump_two_actor.py::test_an_interrupted_peer_transfer_resumes_without_resending_what_arrived`. That journey cuts the transfer at a state, not on a race: the compile-gated `offline_share_hold_serves` arm parks the owner's next manifest answer, `offline_share_drop_connections` cuts the member's connection under it, and the owner's `share_serve_tally` state key (`fauna_sync_engine::share_serve_tally`) shows every file was answered exactly once. Pending (offline-authored) rows never touch the cursor and were already re-served every pass. linux has both arms and the state key (2026-09-24); the apps that do not bind the share plane yet cannot run it until they do (§ Implementation status today, the FFI share-plane host).

**A person the set was never shared with is refused at the serve door, and that is witnessed from a third seat (2026-09-23).** § Cross-user shared-set transfer's "admission is the front door, the M2 seal is the wall" had its front-door half pinned only one tier down (`share_pump_two_seats.rs::a_stranger_is_refused_at_admission_and_pulls_nothing`). No app gesture makes a stranger pull a set, because the pump dials only sets its own seat belongs to, so the app-level witness uses a compile-gated probe: `fauna_sync_engine::share_probe::probe_set`, driven by tui's `offline_share_probe_set` agent arm. It dials a peer by its compare code, claims the set, and then asks for the set's rows and for named manifests whatever the admission said — the request a hostile client would send, which the pump itself never makes after a refusal. Run from the member's seat it is its own control: admitted, the payload's row listed, its manifest fetched. Run from a third seat holding the manifest hashes the member saw, it gets no rows, no paths and no manifest bodies, the owner's `share_serve_tally` does not move for the payload, and the payload's bytes appear nowhere under the stranger's per-launch state root (`tests/e2e-unified/tests/test_share_pump_two_actor.py::test_a_person_the_folder_was_never_shared_with_gets_nothing_readable`, tui). linux has the probe arm too (2026-09-24): its argument parsing and the probe are one shared call, `share_probe::probe_set_from_args`, so no app re-derives how a code or group id is read. The apps that do not bind the share plane yet cannot run it until they do.

**The participation half of rule 5 is BUILT — designed and built 2026-09-25 (§ Per-device participation; user-directed): a user turns a device's peer listeners off from the devices page, and off leaves no socket.** Until that day no human could: the share seat bound at the account-store-ready edge on tui and linux consulting no user-set state, tui additionally handed the same-account peer-sync leg a transport at session assembly, and the only brakes were the `p2p-share` cargo feature and the nest's compile-flavor `p2p-share` advertisement (declared 2026-08-24, the design pass; linux's `p2p-tunnel-toggle` was never this control — it drives the dormant diagnostic node, § RATIFIED 2026-08-24 finding 2). What landed, in shared Rust first: the device-local row (`fauna_sync_engine::p2p_participation`, one meta-table row per (device, account), absent = on) behind `AccountStoreHandle::{p2p_participation, set_p2p_participation, p2p_participation_watch}`; the same-account leg's door — `peer_leg::ensure_bound` reads the row before the brake, folds a pending nest brake and sends the device's report on each full pass, and off drops the node, serve side and transport (`PeerLegPass::ParticipationOff`); the share seat's door — `offline_share::SessionSeat` carries the verdict the driver records, `get_or_bind` (both doors) refuses while off, `unbind` drops a bound seat, and `share_glue::run` selects on the runtime's watch, unbinds on off and reads `ServeStatus::ParticipationOff` on `share-serve-status`, inherited by every host (tui, linux, the `fauna-ffi` share-plane host); the wire — `SyncDevice.{p2p_participation, p2p_off_requested}` and `fauna.sync.devices.p2p_participation.set` with its owner arm (brake only) and self arm (a proof of possession by the row's principal, `sig_domain::DEVICE_P2P_PARTICIPATION_V1`), two additive `sync_devices` columns behind it; and the devices machine — `DeviceSummary.{p2p_participation, p2p_off_requested}`, `DevicesSnapshot::own_p2p_participation`, the per-row paint `DeviceSummary.p2p_participation_paint` (own, checked, label, actionable — `fauna_devices_machine::p2p_participation`, drawn at every snapshot by the same own-row rule the gesture takes its arm by), the injected `P2pParticipation` door (`fauna_client_account_runtime::p2p_participation`, wired on tui, linux and in `fauna-ffi`'s `build_devices_machine`) and the one gesture `DevicesMachine::set_p2p_participation(index, on)` (own row: the local switch; a sibling's row: request off, and a remote `on` refused without a call). **tui renders it** (lead app): `device-p2p-participation-toggle` on every `device-card`, drawn straight from the machine's published paint, tui handing its own device id to `set_this_device_row` for the window before its runtime names the enrolled row. **macOS renders it, fully built** (2026-09-26), through the shared FaunaKit `DeviceCard` both apple targets use — drawing the machine's published `DeviceSummary.p2pParticipationPaint` (2026-09-29; the FaunaKit copy of the own-row rule is deleted, `nil` reads the sibling arm), the own row naming itself to the machine via `DevicesMachine::set_this_device_row` (§ Per-device participation → *Which row is this device's*). **windows renders it** (2026-09-26): a CheckBox in the `device-card` template, drawn from the machine's published `DeviceSummary.p2pParticipationPaint` as is (a null paint draws nothing; windows keeps no paint logic of its own), the own row named to the machine via `set_this_device_row` with the id `device-this-mark-badge` paints from; its account runtime backs the door, so the own-row switch writes the device-local row. windows hosts the share plane since 2026-10-10 (it calls `start_share_plane` from its sync-agent session install and paints `share-serve-status`), so the switch drops its share seat like every other host's and the plane-reading journey runs there. **iOS renders the same card, and its own row is live (measured 2026-09-27):** the premise it was once recorded inert on — that iOS starts no account runtime — is refuted by the runtime's owner doc and the shared `FaunaClient.start()` funnel (iOS hosts it since 2026-08-25 — [`../architecture/account-runtime.md`](../architecture/account-runtime.md) § Implementation status today → *Built — W3 the apple host*), and a run on macOS settles it: the toggle-only journey passes on iOS, so the own-row switch writes the device-local row through the door and the state survives a fresh hydrate — windows' state. **Bound:** iOS wires no share plane (`start_share_plane`, its phone-peer leg — [`p2p-shared-set-build.md`](p2p-shared-set-build.md) § *Phone peers — design*), so it paints no `share-serve-status` and the plane-reading journey skips iOS as unbuilt; sibling rows' request-off arm works as everywhere. **linux and android render it** (2026-10-06): a check button (linux `views/devices_folders/roster.rs`) and a labelled switch (android `DevicesScreen.kt`'s `DeviceCard`) drawn straight from the machine's published paint, each app handing the row its `device-this-mark-badge` paints from to `set_this_device_row`; both doors are wired (linux `wire_fleet_removal`, android through `fauna-ffi`'s `build_devices_machine`). linux hosts the share plane, so both journeys run there. **Bound:** android starts no share plane and paints no `share-serve-status`, so the plane-reading journey skips android as unbuilt; and android has no e2e run venue yet, so its leg is compile-verified only. **web renders the request-off arm** (2026-10-06): a checkbox on every `device-card` drawn from the serialised paint (`setP2pParticipation` on the wasm devices machine), every row `own: false` because the tab wires no door (§ Per-device participation → *Web*) — so both own-row journeys declare web's absence. **Pinned:** `p2p_participation::tests` (the fold rules); `fauna-sync-engine/tests/p2p_participation_gate.rs` (the peer leg over the in-memory transport: bound → off → the accept side is gone and stays gone across a restart → on → bound again); `offline_share::tests::a_switched_off_device_refuses_both_doors_and_unbind_ends_the_listener`; `fauna-iroh/tests/participation_socket.rs` (a bound endpoint's UDP port is free once every handle drops — the fact both doors rest on); `bins/fauna-nest/tests/conformance_p2p_participation.rs` (owner arm brakes, owner-arm enable refused, self arm reports, an `on` report never clears a pending brake, a flipped or foreign signature is refused); `fauna_devices_machine::p2p_participation::tests` (the paint's two arms and the own-row rule, no door meaning no own row); `settings::devices::tests` on tui (it draws the published paint, and the app's own id alone never makes the own switch); `fauna-devices-machine/tests/devices_lifecycle.rs` (which row is this device's: a runtimeless seat's own row takes the own arm and surfaces the refusal with no nest call, a sibling stays on request-off, the door's enrolled row outranks the app's id — and the snapshot's paint names the same own row at refresh, none with no door); and the journeys in `tests/e2e-unified/tests/test_p2p_participation.py` (the own-row toggle flips and survives a fresh hydrate, both ways; and, where the app hosts the share plane, `share-serve-status` reads off, then on again). **The cross-process arm is BUILT (2026-10-06):** when another process holds the engine — the co-located sync agent on a desktop — the door asks it for its pass after the row rests (`RequestMethod::ReconcileAccountRuntime`, served by the agent as `reconcile_now` on its mounted store; the shared `p2p_participation::EngineHolderNudge` over `SyncAgentProvisioner::reconcile_account_runtime`, published to the process's `EngineHolderNudgeSlot::seat` by each host's provisioner and read by the door on tui, linux and `fauna-ffi`), so the agent-held same-account listener drops within the gesture too; pinned by `pipe_server::reconcile_account_runtime_tests` (the verb runs a pass of the mounted store; nothing mounted is a quiet no-op) and `fauna-client-account-runtime/tests/p2p_participation_nudge.rs` (a switch beside the holder asks it for its pass; one in the holder asks no one). **One honest bound:** the seat slot's verdict is unread between sign-in and the account store's assembly (the seconds before the share driver's first pass), during which only the offline-share panel's own door could bind, and the driver's first pass unbinds it if the row says off. **Owed:** iOS's own-row arm (owed a measurement, not a runtime — above); windows' swap from its local paint copy to the published paint is done (the local copy is deleted).

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
participation half of rule 5 is BUILT*, since 2026-09-25) that was the actual missing piece,
not android's account-runtime status (android became the account-runtime
seat's first consumer 2026-08-22, `../architecture/account-data-plane.md` §
Implementation status today → *Built — W3 the android host*, so that was never
the real gate). `P2PTunnelService.kt`, `P2PTunnelState.kt`, the manifest entry,
the `fauna_p2p` notification channel and the `tunnel` feature flag on every
android build recipe (`justfile`) are gone; the matrix row and ui.yaml's
android note record the deletion. *(b)* **linux did not render
this page's `error-message`; fixed 2026-08-25.**
ui.yaml `pages.p2p` declares it; linux used to write a failed start into an
untagged status row (`apps/fauna-linux/src/settings/p2p_tab.rs:49`), leaving
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
§ Implementation status today → *Built — W3 the apple host*), so windows made its call
2026-10-10 (from its sync-agent session install, detached — `SharePlaneModel`;
the six ids paint under the offline-share panel), macOS
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
device-attached run has recorded it yet. A minified android build (release,
foss, the bundle) carries the whole ceremony render: all eight
`offline-share-*` / `offline-receive-*` ids reach the release dex (measured
2026-10-08), and `just android-store-safe-check`'s default column watches
both prefixes. No minified build has yet been launched on a device. Group-scope
**content transfer** is no longer unbuilt here: its feed, pump leg and
endpoints bootstrap are built, and [`group-set.md`](group-set.md) §
Implementation status today is the ledger for what remains. Still unbuilt
here: all seat-epoch machinery (the messaging-seat half — gated behind A3's
build, as before).

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
| Relay capability + sidecar | Landed: `capability::RELAY` advertised on `fauna.nest.info` (advertised while a relay sidecar is connected to the nest — `discovery_core::relay_sidecar_connected`; no flag) + additive `NestInfoReply.iroh_relay_url` derived from the claimed identity (`https://relay.<handle_domain()>`, orderable-apex-gated). `bins/fauna-iroh-relay` (self-hosted, `--features relay`); s6 sidecar + SNI route (no PROXY-v2) + `relay.<apex>` managed-cert SAN (deferred until the A record resolves) — tier_4-proven 2026-06-30. The relay fetches its cert as a sidecar via HPKE seal-on-read with zero `/data` access (owner: [`mail-bridge-lifecycle.md`](mail-bridge-lifecycle.md) § TLS provisioning + tls-certificates.md), over a channel it holds for its whole life. **The access rule is built (2026-10-03):** the relay asks the nest about every connecting endpoint (`fauna.relay.admit`) and refuses all but an enrolled device's principal key and a member's actor key (`relay_admission::key_is_known`, a lookup over `AdmissionSource::ALL`; witnesses `sidecar_channel::tests::relay_admit_serves_members_devices_and_nobody_else` on the nest and `relay_refuses_an_endpoint_that_is_not_a_member` in the relay binary). **A changed certificate reaches it at once** (`fauna.relay.cert_changed`; owner tls-certificates.md § Keeping the cert alive). **The image runs it unprompted (built 2026-10-03):** the relay s6 service is always up, the `services.json` `iroh_relay` flag and the `fauna-service-watcher` that polled it are deleted, and the nest hands the relay its cert only once it has a public name of its own (`sidecar_channel::relay_fetch_tls_cert` behind `discovery_core::relay_wanted`; until then the relay stands by) — nest witnesses `relay_is_handed_no_cert_until_the_nest_has_a_public_name` and `relay_sidecar_connected_follows_the_channel`; the image-level witness `test_iroh_relay.py::test_image_runs_its_relay_with_nothing_switched_on` passes against the image (first run 2026-10-12). **Gaps against § The relay (measured 2026-10-01, re-read 2026-10-14):** no app test drives two devices through it — every app test runs with no relay address. An engine-level witness does (2026-10-14): two seats of one account, each the shared account runtime with the peer leg the native apps bind (`fauna_iroh::peer_leg_transport_trusting`, the production factory body trusting the test relay's root), run app-less through the e2e harness library from two networks with no path between them, read the relay URL from the nest, dial and admit each other over the relay with the account-state walks completed there (`DialPass::admitted_relayed`), and leave the account's post bytes to the nest path while relayed (`DialPass::blocks_relay_deferred`, ruling 4) — `test_iroh_relay.py::test_two_seats_of_one_account_walk_through_the_relay_with_no_direct_path`, against the image (first run 2026-10-14). The image itself is proven at the relay protocol (2026-10-12): two devices the nest enrolled, run as the production peer-leg endpoint (`bins/fauna-iroh-relay/examples/nat_probe.rs`) from two networks with no path between them, connect through `relay.<domain>` on :443 and stay relayed with every byte delivered, and a key the nest does not know is refused on the same path (`test_iroh_relay.py::test_two_member_devices_with_no_direct_path_connect_through_the_relay`, `…::test_the_relay_turns_away_a_key_the_nest_does_not_know`); a pair behind two NATs left the relayed path never while the relay served no address discovery, and with it served the cone pair turns direct in 0.3 s while symmetric and mixed pairs stay relayed (both measured 2026-10-05 by `just p2p-nat-probe`, § NAT hole punching). **The contact plane's seat takes its nest's relay (built 2026-10-10):** both bind doors hand the ceremony factory the live-or-cached own-nest `iroh_relay_url` (`offline_share::CeremonyTransportInputs`), the seat records the relay it was actually bound with (`CeremonyNode::relay_url`), the share advertisement carries it (`share_glue::share_advertisement`) and a share dial attaches it only toward a member advertising the same one (`fauna_peer_sync::discovery::share_relay_attaches`), while every ceremony dial stays direct-only — tier-1 witnesses in `fauna_sync_engine::offline_share` and `fauna_peer_sync::discovery`, over in-memory transports (no test drives this pair through a relay); the measurements the build landed first are in § The relay → *The cross-user seat and the relay*. **Across nests the relay serves nobody, by ruling, not by gap** (§ The relay → *Across nests*). **Address discovery is built (2026-10-05):** `run()` serves it with nothing switched on through `fauna_iroh_relay::production_options` — the substrate's server on `DISCOVERY_PORT` (7842/udp, the substrate default every endpoint's one-URL relay map already queries) on every interface, beside the loopback relay binds, its TLS from the relay's own hot-swap resolver — and `docker-compose.yml` publishes the port, the internet-setup guide opens it, and `nest/network-exposure.md` records the surface; witnesses `production_serves_discovery_on_every_interface_at_the_default_port` in the relay binary, `just p2p-nat-probe` (its default now grades the shipped shape: the cone pair direct, symmetric and mixed relayed) and the image-level `test_iroh_relay.py::test_published_discovery_port_answers_from_outside_the_container`, which passes against the image (first run 2026-10-12). **Bytes ride direct paths only — built 2026-10-05 for the peer leg's block pull and the share pump's page fetch** (§ The relay → *Address discovery, and what rides a relayed path*, ruling 4) |
| Security-review items | PT-1/PT-1b (identity rests on completed handshake proof), PT-4 (`is_safe_candidate` — loopback/link-local/IMDS rejected), PT-5 (no secrets in transport errors), `#[non_exhaustive]` hardening — all landed |
| Shipping image | `--features bluesky,nostr,activitypub` (`Dockerfile:196`; the relay binary is built in a cargo step of its own, copied into the image and always run) → a production nest with a public name of its own advertises `relay` and its relay URL once its relay is connected; a nest with no such name, or with no relay binary beside it, advertises none and clients use the nest-mediated fallback. **No shipped image ever compiled the `wireguard` feature** — which is what made deleting the stack a non-event for deployed nests |

Page surface × node hosting per app:

| App | Page surface | Peer node |
|---|---|---|
| linux | Settings → P2P tab (`apps/fauna-linux/src/settings/p2p_tab.rs`) — **the only P2P page on any app, and ratified 2026-08-24 as the only one there will be** (§ Element IDs → *RATIFIED 2026-08-24*): a Start/Stop toggle, this device's node id, its real LAN addresses (`fauna_peer_sync::lan::discover_lan_candidates`, fixed 2026-08-25, `p2p_tab.rs:412` — was interface *names*) and the contact list. Renders `error-message` on a failed start (fixed 2026-08-25 — was an untagged status-row subtitle) | **two, of opposite kinds.** The toggle's iroh `PeerNode` (slice 7, 2026-07-02) — manual Start, session-only, persisted nowhere, and **dormant**: it serves the base `fauna.peer.*` kinds and carries no live traffic. Separately the share plane's seat binds at store-ready (`account_runtime.rs:270`), consulting the toggle not at all (since 2026-09-25 it consults the per-device participation row instead — § Implementation status today). linux passes `peer_transport: None`, so it runs no same-account peer-sync listener |
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

**A pull that fails keeps its transfer row — RULED 2026-10-24, BUILT 2026-11-01.** § Cross-user shared-set transfer → *The transfer row of a pull that failed* owns the rule and its build: every dial target a pass attempts yields one `share-transfer-item` row, and a failed pull reads *unreachable* (before admission) or *interrupted* (after), never a vanished row.

**A departed member's cached share endpoint stops being dialed at the roster — BUILT 2026-11-01.** § Cross-user shared-set transfer → *A departed member's cached endpoint*: the set's roster, as this replica holds it, decides at the dial-target read (the same-account leg's `verified_member` shape), the transfer surface drops the row by construction, and a tombstone follows on a positive reading only. Open: a set this replica itself leaves keeps its cached rows (outside every pass's read) until the leave writes their tombstones — that subsection's *Implementation status today*.

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
  **Built 2026-10-17****:** the relay does not
  ask the nest on every connect. It remembers a verdict per endpoint — an
  admit for 60 s, a refusal for 5 s (constants of the relay; the nest's reply
  carries no lease) — so **a revoked device can still reconnect for up to
  60 s**, and a device enrolled just after a refusal waits at most 5 s. A
  key with no remembered verdict spends one token of a single global budget
  (burst 100, 20 per second); past it the connect is refused without a
  question, like every other failure. Remembered verdicts spend nothing, so a
  member already known stays in while a flood of fresh keys empties the
  budget. **A flood that never lets the budget refill still locks out no
  member the relay has admitted within the last 7 days** (built 2026-10-18):
  the relay remembers each key it admitted for 7 days past
  the admit, and once the 60 s admit lapses it re-asks the nest about that key
  from a second, reserved budget (burst 50, 10 per second) before the global
  one. Fresh keys cannot reach the reserved budget, because only the nest's
  admit puts a key there. This memory is of its own past verdicts, not a list from the nest, and grants nothing by
  itself: the nest is asked every time, a refusal forgets the key, and a
  revoked device is out within 60 s as before. A member device the relay has
  not admitted in 7 days (or since the relay restarted) is refused, during
  such a flood, until the budget refills. The global budget is per relay, not per source address: relay
  connects on tcp 443 reach the relay from the SNI router with no PROXY
  header, so the router's own per-IP cap is the per-source brake.

**Across nests — nobody (ruled 2026-10-01, refutable).** The members-only rule left one question open on purpose: is a device whose account lives on another nest ever admitted, and on what proof? **No.** Two people whose accounts live on different nests reach each other directly — on one network, or at an address learned over an authenticated channel that happens to be routable — or through their nests, and never through either relay. Four findings carry the ruling, each read from our own code:

1. **The relay can give such a pair nothing the nest does not already give it, because the relay is the nest's own box.** Where the set's home nest stores the set, every member's device, whichever nest its account lives on, already pulls the sealed bytes from that nest directly ([`../architecture/federation-shared-folders.md`](../architecture/federation-shared-folders.md) § Cross-nest shared folders + channel append; the byte plane's trust root is [`../architecture/security.md`](../architecture/security.md)'s). Where it does not — a metadata-only folder — the nest's own *relay serving* hands a holding seat's bytes to the reader, gated per folder by rows that nest wrote ([`relay-serving.md`](relay-serving.md) § Relay serving: a different mechanism from this section's relay, ratified and unbuilt, its cross-nest leg included). A transfer through the peer relay would move the same ciphertext through the same machine, in and out — twice the bandwidth of serving it from the store, and no less than relay serving costs — and when that box is down its relay is down with it. Admission would add no availability.
2. **What a relay could add is a direct path between two home networks, and our build does not make one.** No device advertises an address as the internet sees it (`fauna_sync_engine::peer_leg`'s `compose_facts` leaves `public_addrs` empty, and the share advertisement does the same), and the relay we run serves the relay protocol only, with the substrate's address-discovery server off (`fauna_iroh_relay::spawn_relay`). That is a fact about pairs on one nest too, and it is measured there: two devices behind two NATs stay on the relayed path for the life of the connection (§ NAT hole punching). *Narrowed 2026-10-05:* serving address discovery (*Address discovery, and what rides a relayed path*, below) is ruled for pairs on one nest and leaves this finding standing for a pair that shares no relay — the exchange that makes a direct path runs through a relay both endpoints share, and two members of different nests share none (the two-relays scheme, finding 3).
3. **Every way of admitting an outsider costs the nest the same things, and none can hide what carrying reveals.** An admitted key can hold a connection open, send to any connected key it can name at whatever rate the relay allows (the nest pays for both directions and the target's link pays too), learn whether a named key is connected right now, and, with a second admitted key, use the nest as a forwarder between two people who are not its members. The schemes weighed:
   - *The member vouches* — its device tells its own nest which outside keys to admit, for a bounded time. New nest state, a kind to write it, an expiry to enforce, and the nest holds a list of the outsiders each member deals with.
   - *The outsider carries a capability the member minted, which the relay checks without learning who minted it.* The unlinkability is void the moment a packet moves: the relay forwards by destination key, so it sees which member the outsider talks to. It would buy blind-credential machinery for no privacy, and leave abuse with no member to answer for it.
   - *Both relays, each side through its own.* As built, a dial names one relay — the dialer's own nest's, never one a peer advertised — and reaches only an endpoint connected to that same relay (`IrohTransport::dial` over a one-entry relay map), and nothing in the relay we run forwards to another relay. So each device staying on its own relay needs a nest-to-nest packet carrier that does not exist and would make every federated nest's members users of this relay; otherwise each device joins the other nest's relay, which is the vouching scheme twice.
   - *What the nest learns* is therefore not a property of the scheme. A relay learns the pair, the times and the volume by carrying them, whoever vouched and however blindly.
4. **Nothing that works is taken away.** When this was ruled the contact plane's seat bound with no relay at all and advertised none ([`p2p-shared-set-build.md`](p2p-shared-set-build.md) § *Built — the tui app leg*), so no cross-user pair used a relay, on one nest or on two. *Ruled 2026-10-05, built 2026-10-10:* the seat takes its nest's relay, for pairs on one nest only (*The cross-user seat and the relay*, below); across nests nothing changes — a share dial toward another nest's member attaches no relay.

**The residual, stated.** Two people on different nests, on different networks, whose devices hold no routable address for each other, move a shared set through the set's home nest: at that nest's speed, on that nest's bandwidth, and not at all while it is unreachable. A custodian whose account lives on another nest is in the same position toward the owner's devices. Nothing fails — the nest path is the contract (§ 5 Routing and fallback) — and a direct path is all that is forgone.

**What would refute it,** and the shape a widening would take. (i) A plane between people on different nests whose traffic the nest path cannot carry — live media is the example — makes finding 1 false for that plane. (ii) A measurement that direct paths between home networks are makeable in our build *and* spare nests bandwidth that matters. The cheapest admission the weighing found, recorded so the next pass starts from it and not ruled in: a set's home nest admits exactly the keys it already lets read that set (its own `channel_foreign_members` rows), for as long as the row stands — no vouching message, nothing learned that the nest does not hold, and evicting the member severs it. It would also need a transport that can meet at a relay which is not the device's own (the seat itself takes its own nest's relay once discovery is served — *The cross-user seat and the relay*, below). The members-only build answers "is this key known?" from a set of sources, so one more source is not a rewrite.

**Address discovery, and what rides a relayed path (ruled 2026-10-05; ruling 1 is refutable by the measurement it orders, the rest at build time).** § NAT hole punching measured that two devices behind two NATs never leave our relay, and asked whether to serve address discovery or to rule the relay a forwarder. Four rulings, read from our own code and the measurement — never from the dependency's source:

1. **Serve address discovery — once it is measured to make direct paths.** The relay's whole worth is the direct path it helps make. A relay that only forwards carries every byte of a pair's traffic through the nest's own box, and so can never beat the nest path: that runs over the same box and the same links, and for a nest-resident folder carries the bytes anyway — a forwarding relay costs the box the same bytes twice for nothing, and the honest form of "rule it a forwarder" would be to take the relay out of the same-account leg altogether. The substrate makes a direct path from an address each endpoint learns as the internet sees it, and the one place a device of ours can learn that is its own nest's relay, which already runs on every nest with a public name: so the relay serves the substrate's address-discovery server beside the relay protocol (`fauna_iroh_relay::spawn_relay`'s `quic` half, which `run` turns on through `production_options` — built 2026-10-05). **Measured first, shipped second:** `just p2p-nat-probe` grows a variant whose relay serves address discovery, the cone, symmetric and mixed pairs are measured again under it, the numbers are written into § NAT hole punching, and the production relay serves discovery only when they show at least the cone pair — the common home router — going direct. A measurement in which no pair goes direct refutes this ruling in full: the relay is then a forwarder with no path to offer, and whether the same-account leg keeps it at all is a fresh design pass, not a build. *Measured 2026-10-05:* the cone pair goes direct 0.3 s after the dial, symmetric and mixed pairs stay relayed (§ NAT hole punching) — the ruling holds, and the production relay serves discovery.

2. **What serving it costs, and where each cost lands.** One UDP port on the relay's box, bound by the relay process itself on every interface — the SNI router fronting 443 passes TCP by server name and cannot front UDP — published by the deployment artifact (`docker-compose.yml`), opened by the guide's firewall step (`docs/guides/nest-internet-setup.md`), and nothing else. The port number is a Rust constant in the relay crate, the substrate's own default unless the measurement says otherwise, and no human chooses it ([`../principles.md`](../principles.md) § One configuration surface, bucket 1); the relay URL stays the only fact the nest advertises, the endpoint deriving the discovery port from it. A nest with no public name hands its relay no certificate and the relay stands by, so a home box behind a NAT exposes nothing new; the public nest of the home-with-public-relay bundle has a public name and runs the standard artifact, so it gets the port with every other public nest. The one anonymous surface it adds — a QUIC endpoint that tells any caller its own address as the relay sees it, and nothing about anyone else — is recorded in [`../architecture/nest/network-exposure.md`](../architecture/nest/network-exposure.md) by the build that opens it.

3. **No device advertises an observed address through the nest: `public_addrs` stays empty, for good.** The substrate exchanges each endpoint's observed address inside the connection it is making, over the relay both ends share, encrypted to the one admitted peer being dialed, and keeps it nowhere. A fauna-side advert through the device-endpoints entry would spell the same fact a second time with none of those properties: it rides the fleet-only scope to every sibling, and a merged entry is never forgotten ([`../architecture/account-sync-plane.md`](../architecture/account-sync-plane.md) § The peer leg → *Discovery*), so it would make a device's public-address history — where that device has been — durable state on every replica of the account. The wire field stays (additive-everywhere), read as it is read today (a safe candidate if ever present, `dial_target_from`) and written empty by every producer; `peer_leg.rs`'s note that the field waits for an observation leg is superseded by this ruling and says so.

4. **Bytes ride direct paths only; a relayed path carries what it carries today, and the nest path carries the bytes.** Whatever discovery measures, some pairs stay relayed — two symmetric gateways, carrier-grade NAT, a hostile network — and for them ruling 1's arithmetic holds: every byte through the relay is a byte the nest path would carry over the same box, once. So every pump on the seam — the same-account peer leg's block pull, the same-account chunk transfer, the share pump's page fetch — moves bytes over a connection only while `PeerConn::path()` is direct (`lan` or `wan_direct`), re-read before each pull because a relayed connection can turn direct while the substrate keeps punching; over a relayed path the leg admits and walks rows as it does today — small, and unchanged — and leaves the bytes to the nest path it already runs (the nest's store, or relay serving for a metadata-only folder — [`relay-serving.md`](relay-serving.md) § Relay serving), which is never a failed transfer (§ 5 Routing and fallback). The one exception is the nest path being unavailable to this device: then a relayed connection that already stands carries bytes too, since the alternative is no transfer at all, and "never a party to the data" holds exactly as before. It is only an already-standing connection because the relay asks the nest about every endpoint that connects and refuses while the nest is unreachable (*Who may use it*). None of this is a path choice — the substrate still selects the path inside `dial` (§ Don't do these) — it is what a pump does with the path it was given. *Built 2026-10-05:* `fauna_transport::bytes_may_ride` is the rule's one spelling, taking the connection's path and the pump's own nest fact (`NestPath`); `PeerChannel::path` reads the held connection live, so each pull re-reads it. The peer leg's block pull (`fauna_peer_sync::pull_missing_blocks`, before every want-list request; its nest fact is the pass's own fleet walk) and the share pump's page fetch (`fauna_sync_engine::share_pump::pull_set_from_peer`, before every page's bodies and again before each planned body, so a direct path that dies mid-page leaves the page's remaining bodies deferred; its nest fact is the pass's transfer-policy read, an answer without the feature reading as unavailable) both call it, count what they leave (`DialPass::blocks_relay_deferred`, `SetPullOutcome::relay_deferred`) and walk their rows unchanged; a deferred share body leaves its row a placeholder and the pull cursor below it, so a later pass or the nest path lands it. The same-account chunk transfer is to call the same gate. The custody leg's content pull is none of the three pumps: its nest is the owner's, whose reachability it does not hold, so it passes `Unavailable` and pulls on any path as before.

**What would refute these.** Ruling 1: its own measurement (above). Ruling 2: a deployment the standard artifact does not reach that needs the port — it gets the port the way it gets 443, by the same artifact, never by a knob. Ruling 3: a direct path the substrate's exchange cannot make but an advertised address could — none is known; if one appears it is a design pass, since the history it would create is the objection, not the field. Ruling 4: a plane whose bytes the nest path cannot carry — live media between two devices of one account is the example, as it is for *Across nests* (i) — would ride a relayed path by its own ruling.

**The cross-user seat and the relay (ruled 2026-10-05; rulings 2–4 refutable at build time, the whole by ruling 1's measurement above).** The contact plane's one actor-keyed seat per session (§ Offline share initiation → *One seat per session*) serves two doors: the co-present ceremony, which must never gain a dial path the user did not choose, and the cross-user share plane, whose *Discovery* bullet (§ Cross-user shared-set transfer) has said since 2026-08-10 that two members on one nest and on different networks meet via the relay. When this was ruled the seat bound with no relay and advertised none, so *Who may use it* admitted a member's actor key that nothing presented. Four rulings, read from our own code:

1. **The seat takes its nest's relay — once the relay serves address discovery, and not before.** A relay offers the share plane exactly what ruling 1 above says it offers the same-account leg: the direct path between two home networks it helps make, and nothing else of worth — over a relayed path the share pump moves no bytes (ruling 4; the nest's store, or relay serving, carries them) and the rows it would walk reach a member through the nest anyway, so a forwarding-only relay gives the seat nothing and costs it a standing connection. The seat therefore stays relay-less until the production relay serves discovery, and the build that binds it with the relay lands after that. The relay URL is the live-or-cached own-nest `iroh_relay_url` the same-account leg already hands its transport builder (`fauna_sync_engine::peer_leg`, `PeerLegFactoryInputs::relay_url`), read at the bind by whichever door binds; a seat bound without one stays without one for its life, and its advertisement says so.

2. **The ceremony keeps its rule by the gate the seam already has, not by a relay-less endpoint.** The relay is configured on the endpoint builder and *attached* to a dial only when that dial's `PathCandidates::relay_available` is set (`IrohTransport::dial`); every ceremony dial composes its candidates from the spoken code alone — its LAN candidates and nothing else (`fauna_client_capabilities::group_ceremony_node`'s `dial_with_candidates`, `fauna_sync_engine::share_probe::probe_set`) — so its `relay_available` is false by construction, and a relay on the endpoint hands the ceremony no dial path. The refusal of the relay as the ceremony's *addressing* (§ Offline share initiation, the resolved gap's option (c)) stands: the ceremony's dials never carry it and the ceremony still needs no nest. What the bind adds is a listener reachable through the relay by an admitted key — this nest's own members, who already reach the seat on a LAN by knowing the same key; who may *start* a ceremony is the receive act and who may *pull* a set is the roster consult, both above the seam and both unchanged. So the one seat stays one: a second, relay-bound endpoint for the share plane would be a second listener on one NodeId, exactly what the session slot exists to prevent, and is the fallback shape only if the measurement below fails.

3. **A share dial attaches a relay only toward a member advertising the dialer's own.** The advertisement carries the advertiser's seat's relay URL (the publish half tells the truth about its transport, as today), and the reader's `relay_available` follows the shared `dial_target_from`; the share plane adds one comparison — the advertised URL must equal the URL this seat was bound with — so a dial attaches a relay only toward a member of this nest. A different URL is a member of another nest, whose relay refuses this key and whose case *Across nests* finding 3 already rules out (the two-relays scheme); that dial goes with its direct candidates only, and the nest path carries the set. *Who may use it* changes nothing: member actor keys stay admitted, and this build is what first presents one.

4. **One actor key, several devices — accepted as it stands.** The share leg holds one live counterpart connection per remote actor ([`p2p-shared-set-build.md`](p2p-shared-set-build.md) § Build contract) and an advertisement supersedes per (set, member); at the relay a member's several devices present one key. What our relay does with two connections under one key is measured by the build with our own binary — never read from the dependency's source — and recorded in the build ledger; whichever device a relayed dial reaches serves a set only from its own serve map, every chunk self-verifies, and the nest stays a source, so it is a liveness matter in a best-effort plane, never a correctness one.

**What the build measures first, and what refutes these.** Before the seat binds with a relay, `fauna-iroh`'s own tests show that an unreachable relay URL on the builder leaves a direct-only dial by LAN candidate as fast and as sure as a relay-less bind's — the fear `offline_share.rs` recorded ("an unreachable relay is worse than a direct dial that simply fails"), made a number; a failure refutes ruling 2 and takes the fallback shape it names, with ruling 2 rewritten. Ruling 1's own measurement refutes the whole: no pair going direct makes the relay a forwarder, and the seat stays relay-less with whatever the same-account leg's fresh design pass decides. Ruling 3 is reopened only by a transport that can meet at a relay which is not the device's own, together with *Across nests* (ii), and by nothing else.

*Measured and built 2026-10-10:* an unreachable relay on the builder leaves a direct-only LAN dial as fast and as sure as a relay-less bind's, whether the relay's port refuses or its address drops in silence (dial medians 7.9 ms and 9.2 ms against 7.2 ms, binds alike at about 26 ms — `fauna-iroh`'s `an_unreachable_relay_leaves_a_direct_lan_dial_as_fast_and_sure`), so ruling 2 stands and the one seat binds with the relay. Two devices under one actor key on our relay binary (ruling 4): the later connection answers every relayed dial once its registration with the relay lands, and the earlier is not displaced for good — it answers again the moment the later one leaves (`fauna-iroh-relay`'s `two_devices_under_one_key_the_later_answers_and_the_earlier_survives`). Both doors read the relay URL at the bind from the nest facts they already read the brake from (the panel door live, the share plane's live-or-cached), the seat records what it was bound with, the advertisement carries that, and the share pump's dial composition attaches a relay only on an equal URL; the numbers are the build ledger's ([`p2p-shared-set-build.md`](p2p-shared-set-build.md) § *Built — the tui app leg*).

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

**→ [`p2p-shared-set.md`](p2p-shared-set.md)** (read-ceiling split, 2026-11-04). Moved there verbatim, whole: the scenario and its contract points; § The transfer row of a pull that failed; § A departed member's cached endpoint; § Offline share initiation (the delivery seat); § Peer-served change-row provenance; § Wormability walk — the share leg against the eight rules; and the one-line stubs (§ Build contract, the *Built* records, § Phone peers, § Build design — the row half) that point on to [`p2p-shared-set-build.md`](p2p-shared-set-build.md). § Wormability posture stays in this doc.

## Same-account nest-down folder transfer (ruled 2026-10-14; refutable by the user's word, which re-scopes the outcome instead)

**The question.** The catalog page [`device-to-device-file-transfer`](../../features/device-to-device-file-transfer.md) promises in outcome 7 that two of a user's devices on one network keep passing files to each other while the nest, or the internet, is down, and cited [`account-sync-plane.md`](../architecture/account-sync-plane.md) § The peer leg (requirement 7) for it. Measured 2026-10-14 on two tui seats holding an admitted same-account channel: with the fixture nest stopped, a file written on one seat into a metadata-only folder never reached the other in 180 s, the receiver's `peer_files` reading `{bytes:0, chunks:0, siblings:1}`. As built the plane carries bodies only (§ Implementation status today, first paragraph): a folder's manifests and change feed are the nest's sequenced sync log ([`file-sync.md`](file-sync.md) § Technical Flow, step 5 — the anchor is the ordering mechanism), and requirement 7 is a statement about the ACCOUNT store: its *Scope* bullet names the classes that converge peer-wise, and a folder's change rows are no account-plane kind at all. So the citation was wrong, and this doc's § Goal promised only the direct path with a nest fallback. Whether the outcome's sentence is nevertheless right is the ruling below.

**Ruling: the promise stands — folder file transfer between same-account devices OWES nest-down operation, for the content plane only.** Two of one account's devices that can reach each other keep passing a folder's files — the bodies and the change rows that name them — while no nest answers, exactly as two different users' devices already do on the cross-user share plane (§ Cross-user shared-set transfer; witnessed nest-down on tui by `tests/e2e-unified/tests/test_share_pump_two_actor.py`). Three grounds. (1) The directive the outcome was read from holds here in spirit though its letter is the account store's: app↔app sync is as natural as app↔nest, and an outage is merely the situation in which the peer leg is the only reachable peer set ([`account-sync-plane.md`](../architecture/account-sync-plane.md) § The peer leg, the 2026-08-10 directive). (2) Plane uniformity: the cross-user plane's row half — own-change-log retention at the one recording funnel, the own-pending offline mint under decision 2′, the provisional overlay, the retire-on-arrival reconcile and the writer-signed provenance filter ([`p2p-shared-set-build.md`](p2p-shared-set-build.md) § Build design — the row half; § Peer-served change-row provenance) — is built and witnessed, so a user who can pass files to a friend with the nest down but not to their own second laptop would meet an inversion no goal doc argues for; the richer existing pattern is lifted, never a thinner same-account special case minted beside it. (3) The admission core is one engine with three witnesses, and the same-account `DeviceAuthorization` verdict already admits a sibling to the bodies (`fauna.peer.sync.chunks.pull`), so admitting it to the rows opens no new trust boundary.

**What rides and what does not.** Rides: a folder's file bodies (built) and its change rows (this ruling) — own rows and verified writer-signed relayed rows, pending rows included — between devices of one account, both directions, with a per-sibling cursor as the share pump keeps one per peer. Does not ride, by [`account-sync-plane.md`](../architecture/account-sync-plane.md) § The peer leg's *Scope* rule for nest-arbitrated kinds and by [`sync-engine-deployments.md`](sync-engine-deployments.md) § Control Plane Principle: the folder row and its per-set policy, membership and the roster, the exclusive-editing lease, content-key generations and the floor — a nest-down seat works the folder as it last read it, and a write whose floor cannot be checked seals under the last floor read exactly as decision 2′ rules for a shared set ([`on-demand-files-shared-sets.md`](on-demand-files-shared-sets.md) § Shared sets on a capability host). The two invariants the row half carries are kept verbatim: **a provisional row never enters the nest-sequenced log store**, and **a provisional delete never destroys local bytes**.

**The nest keeps its authority when it returns.** The nest's log stays the one sequencer. On reconnect each writer uploads its own queued changes through the ordinary offline catch-up, the nest sequences them, and a sibling's provisional copy of a row retires when the row's sequenced echo arrives (retire-on-arrival); two devices that edited one path apart are judged by the conflict machinery on the sequenced arrival, as today, never by whichever provisional row a peer served first. Nothing a peer served is lost to that reconcile: the bytes were checked against the writer's own sealed manifest when they landed (§ Wormability posture), and a provisional delete destroyed nothing.

**Citations and the page.** Outcome 7 cites this section; the *Scope* bullet of requirement 7 points here for folder rows. The outcome's sentence is untouched — it is right as written. **Refutation path:** the user may instead rule nest-down folder transfer a non-goal of the same-account plane; then this section becomes that non-goal's record, and the outcome's re-scope is the user's own edit — a shrinking contract move on the user's word ([`../architecture/feature-catalog.md`](../architecture/feature-catalog.md) § Closing a gap), never a session's.

**Design (2026-10-14; refutable at build time): [`p2p-shared-set-build.md`](p2p-shared-set-build.md) § Build design — the same-account row leg.** Two pull kinds join the leg's allowlist, `fauna.peer.sync.changes.list` and `fauna.peer.sync.manifests.get`, the share leg's pair mirrored with the folder in the set's place (the design row named one kind; a row names a manifest, and with no nest answering the sibling is its only source); the own-folder pending mint needs nothing — the row half's mint fires on every folder engine's upload path before any network call, owner-sealed sets included, and the `transfer_queue` was never its source, so the build pins it instead; the share pump's ingest becomes one ingest parameterised by the page's origin (a share peer or a sibling device — the cursor key, the proven actor and the writer answer are what differ), compiled under `account-runtime` without `p2p-share`; the dial pass keeps filling the sibling registry and the engine pulls rows through it in its own loop beside the nest pull, no app-side pump; the kinds' walk entry is [`../architecture/account-sync-plane.md`](../architecture/account-sync-plane.md) § The peer leg → *Wormability walk*, the addendum after rule 8.

**Implementation status today: DESIGNED 2026-10-14, BUILT in shared Rust, WITNESSED on tui 2026-10-14.** Built: the two kinds on the same-account leg's allowlist, served through the provenance ruling to own-account devices only (`fauna_peer_sync::server`), the agent answering them from each running engine's state DB (`fauna-sync-agent`'s `peer_files`); the own-folder pending mint, pinned; the one ingest by page origin with the widened relay retention, compiled without `p2p-share` (`fauna_sync_engine::always_resident::ingest_peer_page_landing`); the engine's sibling pull (`SyncEngine::pull_sibling_changes`) — each sibling the registry holds paged from the greater of its cursor and the engine's anchor, manifests and chunks sibling first and the nest for the rest (`fauna_peer_sync::{pull_folder_changes, fetch_folder_manifest}` through `SiblingChannels`), run by both of the agent's engine loops — `run_watch_loop` after every nest pull, and the on-demand hydration loop after its startup fold and every re-pull — and on each admission the dial pass makes. The on-demand loop's pass is built since 2026-11-05: until then a seat whose folders are on-demand — every windows seat — asked no sibling for rows, and the journey below stayed red on tui-on-windows; an agent-hosted on-demand root lands a sibling's row as its share leg lands a share peer's there ([`p2p-shared-set-build.md`](p2p-shared-set-build.md) § Build design — the same-account row leg, decision 4). Pinned at the Rust tier with two engines and no nest (`fauna-sync-engine`'s `sibling_pull_test`): a write on one lands on the other as a provisional row with its bodies from the writer and no request to any nest, the receiver records nothing of it, the nest's sequenced row retires the overlay with no fork — through the resident apply and, for an on-demand receiver, through its placeholder fold, which confirms the landed body and mints no placeholder over it — a provisional delete leaves the bytes, a forged manifest drops its sibling, and an admission alone triggers the pull in a running loop; the on-demand loop's triggers are pinned in `fauna-sync-agent`'s `bridge` tests. One build-time narrowing: the owned-tree landing's storage-floor measurement stays with `p2p-share`'s `statvfs` dependency, so a build without the feature answers "unknown" there (no same-account owned-tree landing exists yet — the phone hosts' leg). Witnessed on two tui seats by `tests/e2e-unified/tests/test_device_to_device_file_transfer.py::test_two_devices_keep_passing_a_file_while_the_nest_is_down`: the fixture nest stopped, a write on one seat into a metadata-only folder lands on the other with every chunk from the sibling and none left to the nest (its agent's `peer_files` reading), then the nest restarted in place sequences the row exactly once, attributed to the writer (`fauna.sync.changes.list`, read behind an upload barrier from the receiver), with the receiver's copy standing and no conflict copy. **Witnessed on linux 2026-10-14** by the same journey on two linux app seats, after one shared-Rust fix it exposed: the landing's own write reached the receiver's upload door as a watcher event, and a door that took the landed body for an edit minted it as the receiver's own row, which the returning nest then sequenced a second time, attributed to the receiver (red 2 of 2 on linux, timing-dependent on tui). The upload door (`SyncEngine::upload_file_inner`) now declines a peer's provisional body, still hashing to what landed, as the owned tree's ingest already did (`holds_provisional_peer_body`), so no caller — watcher, scan or re-seal — can record it; pinned in `sibling_pull_test`'s convergence pin (red without the guard: B minted an own-pending row for A's body). The other apps' witnesses are the ordinary trickle-down. **Witnessed on the macOS app's own seats 2026-11-03** (the `2seat-native+native` pair, 2 of 2), after one fix it exposed: the macOS app pushed its agent a bearer cached at sign-in, so once the nest restarted the app re-pushed a token the nest no longer accepted on every provisioning tick, overwriting each one the agent's renewal loop minted on its `401` (`sync-agent-credentials.md` § Credential model → *A rejected credential asks at once*), and seat a's queued change never uploaded; every agent-driving app now pushes the session client's own 401-reactive bearer cache (`fauna_client_sync::agent::AgentBearerSource`, the FFI provisioner's default), with windows' own hook still to retire. **Witnessed on tui-on-windows 2026-11-05**, once the on-demand loop ran the pass, after one shared-Rust fix it exposed: a pending row is re-served on every pass, and the overlay forgot the landed body's hash on the second, which disarmed the upload door's guard — the receiver recorded the writer's file as its own before the writer's catch-up, and the nest's one row named the receiver. The overlay now keeps a re-served version's landed body ([`p2p-shared-set-build.md`](p2p-shared-set-build.md) § Build design — the row half), pinned in `sibling_pull_test` with the row served twice. **Witnessed on the windows app's own seats 2026-11-05** (the `2seat-native+native` pair, run by the verify queue), after one fix its first run exposed: the nest-down half passed, but seat a's catch-up stayed on `401`, because the windows app's push of a bearer the restarted nest had refused wrote it back over the replacement the agent's renewal loop had minted; the agent now keeps a live slot bearer over a push of the one it replaced ([`../architecture/apps/sync-agent-credentials.md`](../architecture/apps/sync-agent-credentials.md) § Credential model → *A refused bearer stays replaced*).

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
without asking anyone.** *(The "binds unconditionally" half is the 2026-08-24 finding as ratified; the control it called for was built 2026-09-25 as the per-device participation switch — § Implementation status today → *The participation half of rule 5 is BUILT*; the listener sites below are cited as they stand.)* linux's toggle calls `P2pService::start_tunnel`
(`apps/fauna-linux/src/p2p.rs:118`), which hosts a `PeerNode` serving the base
`fauna.peer.*` kinds only — `node_info` and the vestigial `exchange` — and
carries no live traffic (that module's own header says so). It is started on
demand and **persisted nowhere**: the choice does not survive app restart. The
listener that *does* move user bytes is the share plane's seat, and it binds
**unconditionally at the account-store-ready edge on both built apps** —
`apps/fauna-tui/src/app.rs:2314` (`spawn_share_glue`) and
`apps/fauna-linux/src/account_runtime.rs:270` (`share_glue::start`) — neither
of which consults the toggle, or any user-set state at all. tui in fact binds
**two** listeners nobody can decline: the share seat above, and the
same-account peer-sync leg it hands a `peer_transport` to at session assembly
(`apps/fauna-tui/src/session.rs:1248`; the leg's own listener is
`libs/fauna-peer-sync/src/server.rs:879`, `start_peer_sync_node`) — and tui is the app with no P2P page
at all. linux is the mirror image: it passes `peer_transport: None`
(`apps/fauna-linux/src/account_runtime.rs:175`), so the one app that *has* the
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
points at the node-id readout — `apps/fauna-linux/src/views/status.rs:247`.)

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
(`bins/fauna-nest/src/discovery_core.rs:171` — `advertised_capabilities` reads
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
row (`p2p_tab.rs:49`), leaving convention 2's read-the-error-first rule
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

**Tests that pin it** (rule 5's "no socket" is asserted on the socket, never on a flag): the peer leg over the in-memory transport — bound, then off, then the listener's accept side is gone; the seat — bound, then off, then unbound and a fresh bind refused; the iroh endpoint — a bound UDP port is free again once every handle drops; the nest — owner arm brakes, owner-arm enable refused, self arm reports and clears; and the tui journey — the toggle on this device's own card flips, rests across leaving and re-opening the devices page (a fresh hydrate; not an app relaunch), and the plane's own reading says off. The file names are in § Implementation status today.

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
- [x] The peer data plane carries live chunk transport over the seam (Y.1)
      via the account-plane W2 peer leg + the § Cross-user shared-set
      transfer twin (that workstream owns sequencing; outbound
      `PeerNode::dial` gains its production consumer there) — **the dormant
      posture was NOT re-ratified (user ruling 2026-09-26): build it**, tui
      first; the `device-to-device-file-transfer`
      catalog page holds the three § Goal promises as its outcomes. The
      shared-Rust and agent halves are built (2026-10-08, § Implementation
      status today) and the tui journey witnesses them (2026-10-08).
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
