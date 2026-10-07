# The client-side account lifecycle — target state

Owns: account-client-lifecycle
Status: ratified — ruled 2026-08-12 and built the same day (the runtime and the tui pilot); every native app hosts it, and web's hosting program is ruled with its first two rulings built (§ Implementation status today); split verbatim out of `account-data-plane.md` on 2026-09-28
Authority: **how an app assembles and runs the account store and the planes** — the runtime's home crate and its wasm-capable split (*The trigger fired*, web's hosting program), how a wasm chunk other than the core chunk reaches web's runtime (*The account port*), the store-thread-plus-handle shape, the pump (its prologue, its four wake sources including the runtime's own push arm, commands and passes, the breath, the cap-refused pass, the barrier, the sign-out cut), the first listing and its read gate (what a replica that has never listed a scope may answer) with the unkeyed hold (what a listed replica may answer for a tip-sealed kind while it holds rows under a generation it has not keyed), the writer key's first mint, per-app consumption, and the preference-cluster pilot — together with their status entries. **NOT owned here** — the store contract the runtime drives (logical schema, placement, hydration) and the cross-cutting status → [`account-data-plane.md`](account-data-plane.md); which process holds the plane, and each app's host (the W3 seat's build-out, W5, W6) → [`account-runtime.md`](account-runtime.md); the nudge contract → [`account-sync-plane.md`](account-sync-plane.md) § Nudges and backstops; the ordered own publish and a writer's lifetime → [`account-replica-posture.md`](account-replica-posture.md) § The store device principal; the `__config` rail's retirement → [`config-dissolution.md`](config-dissolution.md). On conflict in those domains, raise it.

Last verified: 2026-09-28 (split verbatim out of [`account-data-plane.md`](account-data-plane.md); the dated clauses that follow moved with their text)

> **Audience:** anyone assembling the account runtime in an app, changing what the pump does between
> passes, or moving plane code toward web.
> **Purpose:** the rules every app's host implements, so seven hosts stay one runtime.

Split verbatim out of [`account-data-plane.md`](account-data-plane.md) on 2026-09-28, when that doc stood six days from the 262,144 B
whole-file read ceiling and this section, with its status entries, was more than half of its week
of growth. The 2026-09-06 partition had kept these rules with the store while the per-app seat
moved to [`account-runtime.md`](account-runtime.md); they have a code home of their own
(`fauna_sync_engine::account_runtime`, and since 2026-09-27 `libs/fauna-account-plane`) and a test
module of their own (`account_runtime::tests`). A routing stub remains at each original location;
prior history: `git log --follow docs/goal/architecture/account-data-plane.md`. The `W<n>` workstream labels and `R<n>` decision labels used throughout are defined in [`account-data-plane.md`](account-data-plane.md) § Workstreams and § The ratified decisions.

> **Reading this doc.** Its text was carried **verbatim**, with its headings kept under their
> original names, so a `§ The client-side lifecycle` citation resolves by swapping the filename.
> Three pointers to text that stayed behind were rewritten as links. An unqualified `§ <name>`
> citation naming a section not found here resolves in [`account-data-plane.md`](account-data-plane.md), then in the plane docs its own
> reading note lists, then in [`config-dissolution.md`](config-dissolution.md). The heading's *Built — W3 slice 1* / *slice 2* entries are in
> [`account-runtime.md`](account-runtime.md) § Implementation status today, where the 2026-09-06
> partition put them.

## Section map

| Section | What it holds |
|---|---|
| § The account store (W1) → § The client-side lifecycle (W3) | The rulings: home (with *The trigger fired*, web's hosting program, and *The account port*, web's chunk crossing), shape, the pump, the first listing (the read gate on a replica that has never listed a scope, and the unkeyed hold on one that has), the writer key, consumption, and the pilot. |
| § Implementation status today | The lifecycle's own build ledger since 2026-09-22: the account port (built with the fleet seam; the plane-kind seams owed by their cuts), web's hosting program, the runtime's push arm, the pass driver with its barrier, and the audit of every pump-owned write. |

## The account store (W1)


### The client-side lifecycle (W3 — ratified 2026-08-12; BUILT the same day, conforming: the runtime + the tui pilot, then all four of tui's preference surfaces — § Implementation status today → *Built — W3 slice 1* / *slice 2*)

How an app assembles and runs the store + planes + bridge — the piece W2.5
named as its placement debt ("no app assembles an `AccountStore` + plane +
bridge yet"). Six rulings:

- **Home: `fauna_sync_engine::account_runtime`**, feature `account-runtime`
  (which enables `preference-store`) — the account plane's drive layer sits
  beside the planes it pumps, the same crate-role multiplicity the db-floor
  ruling blessed ([`app-guidelines.md`](app-guidelines.md) rule 9's
  crate-layering note: engine **and** floor is not a smell). The crate stays
  native-only; **web was a declared absence at W3** (its preference surfaces
  stayed nest-mediated over the since-retired `ConfigClient`) and hosts the
  runtime since 2026-09-29, through the wasm-capable home below. The extraction
  trigger is pre-ratified, db-floor pattern: when web's `StoreBackend`
  backend lands ([`account-data-plane.md`](account-data-plane.md) § Store logical schema → *Physical realization*) or a consumer wants the
  runtime without the engine graph, planes + runtime move to a wasm-capable
  home with re-exports keeping call sites whole. **The trigger fired
  2026-09-26 — web's hosting is owed, in this order.** The community room class is the first
  consumer the blob rail cannot serve: its group-reception key is a
  fleet-only, `GenerationTip`-sealed kind (`fauna.state.group-reception-key`;
  [`../behavior/community-rooms.md`](../behavior/community-rooms.md)
  § Implementation status today, the founding entry, owns the class's
  ruling), and no correct web-only path to it exists short of the plane
  itself — sealing needs the resolved tip, publishing needs the writer
  journal and the in-seal signature, so a "thin" web writer is the plane
  over a store backend by another name. Measured 2026-09-26: the plane half
  of `fauna-sync-engine` — the account-state and group planes, the
  generation mint / tip / top-up / escrow-recovery / reclaim / unkeyable
  passes, the page walk, the preference bridge, the custody and principal
  rows, about 25 k lines — names no native API; the pump
  (`account_runtime.rs`, about 13 k lines) and the peer, custody and
  segment-backup legs do. Ruled: **(1)** the plane half moves to a
  wasm-capable crate, `fauna-sync-engine` re-exporting at the same paths;
  **(2)** the pump splits — the pass driver and the command service
  (platform-generic) join the plane crate; the native host (the store
  thread and its current-thread runtime, the peer leg, the custody leg,
  segment backup) stays; web hosts the driver as a `spawn_local` task in
  the SPA's process, the one-process posture
  [`account-runtime.md`](account-runtime.md) § Multi-instance concurrency
  already gives its web leg; **(3)** web's physical backend is IndexedDB +
  OPFS behind the same `StoreBackend` (§ Physical realization) — an
  in-memory backend is a test double and a wasm-compilation proof, **never
  the shipped web replica**: a replica that dies with the page cannot hold
  a reception key durable before its public half goes out, and every page
  load would re-run the prologue. Its production uses are the throwaway
  replicas — the box-recovery cold read's ([`nest/box-recovery.md`](nest/box-recovery.md)
  § The plane-era recovery floor, *(b)*) and, ruled 2026-09-30, the capability
  host's fleet-scope read ([`../behavior/on-demand-files.md`](../behavior/on-demand-files.md)
  § Shared sets on a capability host, decision 1′): reads that walk, key and
  fold, then drop the replica or hold it in memory only, hold nothing durable
  and publish nothing, so none of the reasons above reaches them; **(4)** web enrolls a store device
  principal like any machine
  ([`account-replica-posture.md`](account-replica-posture.md) § The store
  device principal → *Web's principal*) and calls the same
  `conversation_seams::wire`, from a wasm-capable home of the seam glue;
  **(5)** the localStorage rails migrate into the store at W6, as ruled
  in [`account-data-plane.md`](account-data-plane.md). The extraction (1) and the backend (3) are independent; (2) follows
  (1); (4) follows (2) and (3). Until (4) lands, founding and accepting a
  community room stay refused by name on web, and the devices page's
  device-cap notice stays a web absence.
  **Ruling (2)'s build decisions (2026-09-28).**
  The driver is `fauna_account_plane::account_driver` — the command service
  (`AccountStoreHandle` and the store-side command server), the pass and its
  report, the enrollment legs, the pass driver and the serve loop — and it
  crosses to its host over four seams, each in the plane crate. **The
  credential slot** is `principal_custody::PrincipalCustody`, a synchronous
  trait (a slot read is a local secret-store read, served as a local
  command) the native `PrincipalSlot` implements and web's slot will; the
  slot's plain data — the loaded grant, the bundle status, a standing
  refusal — moved with it, so `fleet_removal`'s completion leg,
  `group_authority_revocation` and the succession tail re-author
  (`succession_tail`) moved too. **The host legs** are
  `account_driver::HostLegs`: one call between the fleet walk and the
  device-endpoints step — natively the peer leg and the custody leg in the
  pump's own order, on web `NoLegs`; the legs' *report* shapes live in the
  plane crate's `host_legs`, so `PumpReport` is one concrete type on every
  host and a host without a leg reports `None` in that slot. **The
  election** is `host_legs::EngineElection`, async because web's Web Locks
  try is; the degrade-open ruling at start and the stay-a-non-holder ruling
  on a re-try are the driver's, the lock the host's. **Time**:
  `fauna_sleep::sleep` for the backstop ticker (one pinned sleep, re-armed
  when its tick is observed — tokio's `MissedTickBehavior::Delay` by
  construction), the sign-out grace and the retirement budget; the wall
  clock (`Timestamp`) for the pass timings; `tokio::sync` is fine, and
  `tokio::time`, `std::thread`, `Instant` and `SystemTime` are banned from
  the plane crate by a textual test (`tests/no_native_time.rs`). Two
  consequences: `observation_intake` moved with the command service, its
  writer still `pub(crate)` — the door and the writer share a crate, which
  keeps the security review's ruling rather than widening it; and the share
  leg's three typed handle doors (native-only — the share leg is web's
  declared absence) became generic ones, a caller-owned meta-table row
  (`meta_get` / `meta_put`, one owner per key) and a `states_of_kind` read
  the pure `share_dial_targets_from_rows` runs over, with the typed calls an
  extension trait in `share_pump`. `AccountStoreRuntime::start` is the
  native host: it mints the driver and its handle before the thread, and
  runs `AccountDriver::serve` once per assembly.
  **Ruling (4)'s build decisions (2026-09-29; refutable by the user until web's host lands).** Seven, each
  with its reason. **(a) The seam glue's home is `fauna-account-seams`** — a
  thin wasm-capable crate above `fauna-account-plane` (`account-driver`) and
  `fauna-conversations`, holding `conversation_seams::wire` and the three
  seams it registers (`group_reception`, `read_positions`,
  `contact_overlays`); `fauna-client-account-runtime` re-exports each at its
  old path, so tui, linux and the `fauna-ffi` seat compile unchanged. Not
  the plane crate, which is a dependency floor that names no conversations;
  not the native assembly, which is native-only by construction; and
  `fauna-conversations` must never learn about the runtime. **(b) How the
  seams' tasks run is the host's**: `fauna_account_seams::TaskSpawner`, one
  method over a boxed task that is `Send` natively and not on wasm32 —
  `tokio::runtime::Handle` natively, `LocalSpawner` (`spawn_local`) on web;
  the watchers' one wait is `fauna_sleep::sleep`, and the crate's
  `tests/no_native_time.rs` pins the plane crate's ban (minus
  `tokio::runtime`, which the native spawner arm alone names behind cfg).
  **(c) Web's credential slot is the SAME bundle, never a twin.** The
  format and logic of `fauna_sync_engine::principal_bundle` — the attribute
  names, the hex-over-canonical-dag-cbor retained-keys record, the
  content-addressed registration latch, the standing refusal, the staged
  removals, the writer-key mint/load and the backup-key heal — move into
  the plane crate as `principal_bundle`, generic over a `SlotStore` seam
  (get / set / delete of strings — the shape `CredentialStore` and
  `fauna_client_accounts::SecretStore` both already have) plus a
  read-modify-write section seam; the native `PrincipalSlot` becomes that
  bundle over `CredentialStore` with the store dir's `migration.lock` as
  its section (behavior unchanged), web's over `LocalStorageSecretStore`
  under the per-origin, per-actor key prefix the account registry already
  rests in ([`account-replica-posture.md`](account-replica-posture.md)
  § The store device principal → *Web's principal*). Web's section is the
  tab's own: the medium has no synchronous cross-tab lock (Web Locks is
  async, the slot seam is synchronous by ruling (2)), the staged-removal
  list is written by a human gesture and completed by the one pump holder,
  and two tabs staging removals at once is a human-speed race whose loss
  re-surfaces as an unstaged removal the user redoes — recoverable, so
  accepted. **(d) Web's escrow trust is the origin's TOFU pin**:
  `fauna_client_core::nest_trust::LocalStoragePinStore`'s identity for the
  serving origin — one pin per origin, which on web is per nest, shared by
  every account on it, exactly what native's per-host pin is (the SPA reads
  it through the same "what this machine pinned, never the nest's own
  claim" door as `trusted_escrow_holders`). No pin (a plaintext dev nest)
  is fail-safe — no receipt verifies, no tip resolves, fleet-only sealing
  stays refused with the no-tip error — so the web e2e over a plaintext rig
  needs the `test-helpers`-only seeded trust native gets from
  `FAUNA_E2E_TRUST_NEST_IDENTITY`, read from browser storage the way
  `fauna-client-region`'s seed is (convention 15: the export keyed on the
  feature, never the profile). **(e) Web's data path is the app session**:
  `process_rpc: None` — the tab's one `WsRpcClient` carries the enrollment
  legs and the data path alike, by web's one-socket-per-actor transport
  goal ([`transport.md`](transport.md) § Goal); the driver's
  grant-registration gate is inert on web (nothing waits on it) until a
  principal-authenticated web connection is ever ruled. The enrollment
  target is web's own derived device id
  (`AccountRegistry::device_id_for_actor`, the row the SPA already
  registers under), per the one-credential ruling
  ([`apps/sync-agent-credentials.md`](apps/sync-agent-credentials.md)
  § Credential model → the RULED 2026-09-28 block, decision 3). **(f) The
  web host lives in the plane crate's wasm32 arm** —
  `fauna_account_plane::web_host` (cfg wasm32, feature `account-driver`),
  the twin of `fauna_sync_engine::account_runtime`'s native host: `start`
  mints the driver and its handle, opens `IndexedDbBackend` under
  `StoreRoot::platform().store_name(actor)`, resolves the slot, takes the
  election over Web Locks (`EngineLock::try_acquire(store_name)`), and runs
  `AccountDriver::serve` with `NoLegs` on a `spawn_local` task, looping on
  `ServeEnd` as the native worker does; `fauna-wasm` supplies only what the
  SPA alone knows — its `WsRpcClient`, the keypair, the memberships closure
  (`ConversationsSession::joined_conv_channels`), the attested
  predecessors, the pin, the device id — and calls `conversation_seams::wire`
  at the store-ready edge beside `set_room_seams`. The precedent is
  `fauna-account-store`, whose web legs (`indexeddb`, `locks_web`,
  `root_web`) live in-crate under cfg. **(g) The proof shape**: a
  `wasm-bindgen-test` in headless Firefox over the real IndexedDB backend —
  the driver hosted by `web_host` with `NoLegs` and a fake requester (the
  `NestInfoOnly` shape the native convergence tests use), the seams
  registered through `wire`, a reception keypair put and read back through
  `AccountGroupReceptionKeys` — is the mechanism proof without a nest;
  founding and accepting on web are proved by the e2e
  (`test_conversation_room_community.py`'s `web` marker), which needs web's
  paint of the room controls first. **(h)** All seven landed 2026-09-29
  (§ Implementation status today).
  **Ruling (4), the teardown rider — web's stop names its reason, like every host's (ruled 2026-10-01; advisory, refutable by the user; built the same day — § Implementation status today).** A web sign-out stops the runtime with `AccountStoreHandle::shutdown_for_sign_out`, and every other stop — an account switch, a tab re-pinned to another account, a start that a later one superseded — is the plain `shutdown`, which leaves the machine enrolled. What the sign-out stop does is not this doc's: the plane severance is [`account-data-taxonomy.md`](account-data-taxonomy.md) § The generation machinery, clause (4), and the nest-side grant retirement is [`apps/sync-agent-credentials.md`](apps/sync-agent-credentials.md) § Credential model. Web is no exception to either, for the reason every host retires: its sign-out wipes the writer key with the rest of the credential namespace, so without the retirement the nest keeps a grant for a key nobody holds, and the fleet keeps an `Enrolled` member that every later generation mint wraps to and whose walk mark the retention gate waits on. A browser makes it worse, because a user who signs out of a shared one may never sign in there again to heal it. Four decisions. **(a) The sign-out gesture runs the stop itself, before it clears the identity.** The stop that exists today hangs off the actor-scoped reset, which fires on every identity change and cannot know why; so the reason is stated by the caller, as `fauna_client_account_runtime::StopReason` has it natively, never inferred. The reset's own stop then finds nothing running. **(b) The stop is awaited, under the hosts' one stop budget** (`ACCOUNT_RUNTIME_STOP_BUDGET`), a start still in flight waited out inside it; the credential wipe and the store erase follow it ([`apps/account-scoping.md`](apps/account-scoping.md) § The scoping taxonomy → *Erasure follows scope*, the web paragraph, owns that order and the record that makes it crash-safe). **(c) A tab with no runtime retires nothing and signs out anyway** — a start that failed, or a tab that never held the account's engine role — the native `NothingToStop` outcome: the enrollment stays until the user removes the device on the Devices page. **(d) The retirement stays in the core chunk**: the account port's decision (h) below already lists it among the doors that never cross.
  **The account port — how a wasm chunk other than the core chunk reaches web's runtime (ruled 2026-09-30; built the same day with its first seam, the Devices page's fleet door — § Implementation status today).** The runtime's `AccountStoreHandle` is a channel into the store task, and both ends of it live in the core chunk's linear memory (`fauna-wasm`'s `account_runtime`, decision (f) above). The page machines that need the store live in other chunks — the Devices/Folders machine in `fauna-wasm-folders`, the media machine in `fauna-wasm-media`, the ATProto settings machine in `fauna-wasm-atproto-settings` — and every chunk is its own instantiated module with its own memory, so no Rust value crosses between them ([`apps/web.md`](apps/web.md) § WASM Integration owns that discipline). Without the port, a removal from web's Devices page ran the roster leg alone, the page could paint no member group ([`../ui/devices.md`](../ui/devices.md) § Members without a matching entry), and no plane-only kind whose web consumer sits in such a chunk could be cut over ([`config-dissolution.md`](config-dissolution.md) § The `__config` dissolution schedule → *The closure order*, step (3)). Nine decisions, each with its reason.
  **(a) What crosses is a call, as bytes, through one JS method.** The typed interface is `SharedAccountPort { call(door: string, payload: Uint8Array): Promise<Uint8Array> }`: a door name, the call's arguments as canonical DAG-CBOR (`fauna_protocol::encode_canonical`), and its return the same way (`decode_strict`). It is the shared rpc port's shape ([`transport.md`](transport.md) § Design decisions, the one-socket-across-chunks entry) applied to the store. There is one method so that the SPA's implementation never changes when a door is added. Three other shapes were weighed and refused. *Moving the machine into the core chunk* serves one page and not the program: every remaining `__config` field becomes a plane kind, so every chunk that reads config becomes a store consumer, and moving each one ends with a single chunk. *A second runtime in the consumer's chunk* would be a second replica inside one tab, with a slot section and an election of its own, where decision (c) above gives the tab one section. *The handle itself over the port* (the shape `WsRpcClient::over_port` has) would put every one of the handle's doors one branch away from crossing — the shutdown, the sign-out retirement, the ceremony signing key, the raw kind and meta puts among them — and would make each door that must not cross a refusal at run time where it should be absent at compile time.
  **(b) A door is one method of a consumer seam, never a bare handle method.** A consumer seam is a trait the consumer's own crate declares and the runtime answers — the pattern every consumer below the plane already follows (`fauna_devices_machine::FleetRemoval`, `fauna_client_config::SuccessionLedgerStore`, `fauna_client_capabilities::custody_ceremony::CustodyRegistryWriter`, `fauna_conversations::backend::PeerAnchorStore`). The seam's implementation over the handle is where what the runtime holds joins the call: the ledger seam's identity and attested signer set, the fleet seam's refusal when no runtime runs. Serving the seam keeps all of that in the core chunk, so a consumer chunk supplies the gesture's own arguments and nothing else; and the shared code that consumes the seam is written against the trait, so it runs unchanged in any chunk.
  **(c) Both halves of a seam's crossing live beside its trait.** The crate that declares the seam carries a `port` module (feature `account-port`) holding two things: the **forwarder**, which implements the trait over a port transport by encoding, calling and decoding, and does nothing else; and **`serve`**, which answers one door from any implementor of the trait. Both chunks compile that crate, so the two ends of every crossing are one definition — the property `fauna-rpc-wasm` gives the rpc port — and the pair is tested natively over a loopback transport, with no browser. What every seam shares — the transport trait, the fault type, the JS binding with its TypeScript declaration, the loopback — is `libs/fauna-account-port`, a thin crate that names neither the plane nor any seam, so a consumer chunk links no store code.
  **(d) The core chunk serves each seam through the object the native seats wire.** `fauna-wasm`'s `account_port` module exports `accountPortCall`, reads the tab's runtime fresh on every call (`account_runtime::handle`, as a native adapter reads its source) and hands a door to its seam's `serve`. The Devices page's fleet seam is served by `RuntimeFleetRemoval`, the one adapter tui, linux and the `fauna-ffi` seat wire; it moves to `fauna-account-seams` and is re-exported at its old path, which is the pattern of ruling (4)'s decision (a). So the rule that an absent runtime refuses the removal ([`account-data-taxonomy.md`](account-data-taxonomy.md) § The generation machinery → *Fleet-scope reclamation*, clause (4), *The completion rule*) is one body of code on all seven apps. A plane-kind seam is served by its one implementation over the handle. A door the dispatch does not know is refused by name.
  **(e) A port is minted for one account and answered only by that account's runtime.** The SPA mints it per identity (`sharedAccountPort(secretHex)`, as `sharedRpcPort` is minted), every call carries that account's actor id to `accountPortCall`, and the core side refuses a call whose account is not the one its running runtime serves, exactly as it refuses when no runtime runs. A page machine can outlive an account switch by a tick; without the binding its next write would land in whichever runtime happened to be up, and one account's follow or folder key would rest in another account's store.
  **(f) Every failure is the seam's own failure value, never a success.** No runtime, another account's runtime, an unknown door, bytes that do not decode, and glue that threw are each a port fault, and the forwarder answers the method's refusal: `FleetRemovalRefusal::Unavailable` on the fleet seam, the store seams' `StoreError`. A seam method whose return type has no failure arm does not cross until it has one. Two chunks from different builds therefore fail closed, by the strict decode. The port adds no deadline: a call waits as a native caller of the same door waits, a tip-sealed put parked behind a pass included (the pump bullet below → *Commands and passes*).
  **(g) The port is not a wire surface, not an at-rest one, and not a principal.** Both ends are one build served together, so its byte shapes carry no compatibility duty ([`version-compatibility.md`](version-compatibility.md) governs the nest wire and the stores; the port is neither) and change with the seam. It sits inside the tab's one trust domain: the SPA's script already holds the account seed and hands it to every chunk constructor, so the port exposes nothing that script could not already do. What it must never do is widen a door. Every honest-writer check stays behind the handle, in the store task; the port carries arguments, never authority.
  **(h) The crossing set is a positive list.** A seam crosses when shared Rust in a non-core chunk needs it, and adding one is decision (c)'s module plus one arm in the core dispatch. These never cross: the runtime's lifecycle (start, shutdown, the sign-out retirement), the pump controls (`reconcile_now`, the barrier, the nudge), the ceremony authority, and the generic doors as such (`put_preference`, `states_of_kind`, the meta row, the intent queue — a typed seam over one of them is a seam like any other). A fact the SPA itself paints — the enrollment notice, the This-device row — stays a core-chunk export the page calls directly, as today. The first set is every crossing a gated consumer cut needed on 2026-09-30, the door names being `AccountStoreHandle`'s, which each seam's implementation calls: the Devices page's fleet seam (`FleetRemoval` — `resolve_removal`, `stage_removal`, `settle_removal`, `fleet_members`, `remove_member`) in `fauna-wasm-folders`, built with the port itself; the followed-folders seam (`follows`, `put_follow`, `unfollow`) in `fauna-wasm-folders` and `fauna-wasm-media`; the custody-ceremony seam (`custody`, `merge_custody`) in `fauna-wasm-folders`; the folder-keys seam (`folder_keys`, `merge_folder_keys`, `settle_folder_removal`) in `fauna-wasm-folders` and `fauna-wasm-media`; the ATProto credential seam (`atproto`, `put_app_credential`, `revoke_app_credential`) and the ATProto identity seam (`atproto_identity`, `merge_atproto_identity`) in `fauna-wasm-atproto-settings`; the mail seam in `fauna-wasm-labeler-catalog`, whose per-labeler grant mint reads the MSEK — its one crossing door is the READ fold (`mail`), and the row writes (the state row, a credential's put, re-wrap mark and revoke) stay with the mail-settings machine in the core chunk, refused on the forwarder without crossing; and the succession-ledger seam in `fauna-wasm-labeler-catalog` and `fauna-wasm-folders` (the labeler catalog's grant writes, the custody facet's read and revoke), whose crossing doors are the read and the join (`load`, `merge`) — the chain re-point and the grant-mark raise are the core chunk's post-store-ready pass's and never cross, and `self_actor` answers from the account the port was minted for. Each of the seven plane-kind seams is declared by its kind's consumer cut ([`config-dissolution.md`](config-dissolution.md) § The `__config` dissolution schedule → *Phases and gates*, the E3 slice), in the wasm-clean crate its consumers share.
  **(i) Wiring and proof.** A chunk machine that needs the store takes the port as `port: SharedAccountPort` (typed, never `any`) beside its `SharedRpcPort`, and wires its seams from it before its first refresh — on the Devices page `DevicesMachine.setAccountPort`, beside the seams `devices-session.ts` already wires. The proof has five parts. Per seam, the native round trip over the loopback: every method, both arms of its result, and a faulting transport answered by the refusal on every method. `fauna-devices-machine`'s removal-order pins run through the forwarder and `serve` (the intent staged before the nest deletion, settled on its outcome) — which is what makes web's removal the native one: the same machine, the same trait, the same adapter. A `wasm-bindgen-test` in `fauna-wasm` (headless Firefox, `just wasm-test-check`) over the real `web_host` runtime: no runtime refuses, another account's port refuses, and a served member read answers this browser's own fleet id. The SPA contract test (`just web-unit-test`, beside `shared-rpc-port-contract.test.ts`): one implementation of the port, every chunk loader typing it. And `tests/e2e-unified/tests/test_device_member_removal.py` on web.
  **Bounds, stated.** A tab that hosts no runtime — a second tab of the same account, since only the tab that won the MLS-writing role hosts one (`$lib/account-runtime`) — refuses a removal and lists no members, by the absent-runtime rule; the gesture works in the hosting tab. The same tab's preference surfaces wait out the handle bound and fail ([`config-dissolution.md`](config-dissolution.md) § The `__config` dissolution schedule → *What replaces the bridge's two carriages*, the declared bound). Key material a door returns (a folder key) crosses the script heap as bytes, as the account seed already does.
- **Shape: one dedicated store thread per (process, account); a `Send`
  handle for everyone else.** `AccountStoreRuntime::start` — the native
  host of the driver — spawns a named OS
  thread running a current-thread async runtime that owns the store
  connection — the sqlite backend's AFIT futures are not `Send`, the same
  fact that shaped `EngineHost`'s worker thread and `fauna-peer-sync`'s
  `ServeStoreHandle`, and this is that shape promoted to the client — and
  serves `AccountStoreHandle` (`Send + Clone`, channel-backed): reads,
  `put_preference`, scope nudges, shutdown. Every store access in a process
  crosses the handle, which preserves R2's "no IPC read path" (in-process
  channel, not IPC); under W5's multi-process law each process runs its own
  thread + connection over WAL (§ Multi-instance concurrency — the store's
  sanctioned multi-connection posture).
- **The pump: a prologue, then four wake sources — every pass driven
  beside the command channel (ruled 2026-09-22).** Prologue, run to
  completion before the first wait (the shipped receive-loop discipline —
  `fauna-conversations`' session loop): `publish_pending` → walk the
  account-state scope + each registered content scope. Then
  a select over: **(1)** the coalescing nudge channel (bounded
  `mpsc::channel(1)` + `try_send`, the sync agent's wake-sender shape) — fed
  by **the runtime's own push arm (ruled + built 2026-09-25)**: the session's
  push stream is a runtime param (`AccountRuntimeParams::pushes`, attached
  beside the reconnect watch by `with_session_wakes`), and ONE mapping
  (`nudge_scope_for_push`) names the scope a `fauna.sync.changed` wakes — a
  W2.3 scope tag wakes that scope, and an untagged push wakes nothing (the
  untagged `__config` nudge it once also mapped retired with the blob rail
  on 2026-10-02 — [`config-dissolution.md`](config-dissolution.md) § The
  `__config` dissolution schedule → *The closure order*, step (6);
  [`account-sync-plane.md`](account-sync-plane.md) § Nudges and backstops
  owns the nudge contract). No app hand-wires the arm, and the nest stays
  kind-blind (§ The `__config` dissolution schedule, P6) → walk that scope;
  **(2)** the backstop ticker (rescan cadence, minutes —
  correctness never depends on the nudge) → `reconcile` + `publish_pending` (a full pass
  also carries the capability reconcile sweep behind its fleet walk — owner
  [`../ui/nests.md`](../ui/nests.md) § Trust facet — grants → *Reconcile*, ruled
  2026-10-06, built 2026-10-06);
  **(3)** the reconnect watch
  (`NestClient::subscribe_reconnects`) of the app session's client and,
  when the data path rides the store principal's own client, of that client
  too (`resolve_and_start` merges the two, so a data-client drop the session
  never saw still re-walks within seconds, not at the backstop) →
  `publish_pending` + walk, because push `seq` resets on reconnect; **(4)** the local-write wake: a write
  through the handle (`put_preference`, a read-marker raise, an
  observation, the devices page's removal legs, the custody, group and
  share door puts) is the **local write only** — the row is durable and
  stamped when it answers, and it answers inside a pass in flight — and it
  arms a **publish step** (`publish_pending` on the delegable plane, then
  on the fleet plane — the network legs the write used to run inline) that
  the loop runs as soon as no pass is in flight, on every role (a
  non-holder's own rows are its own to publish). The ordering law is
  unchanged — `publish_pending` sends every unsent own row before its own,
  in journal order, so the own slot is always the contiguous attempted
  prefix, and a leg that failed leaves an unpublished row that arms
  (2)/(3) recover the same way; what changed is that a network failure is
  never the write's answer: the app's error surface reports local refusals
  only. The law and why it exists (an accepted inline publish racing the
  reconnect pass used to strand every earlier offline row):
  [`account-replica-posture.md`](account-replica-posture.md) § The store
  device principal, refinement 11 → *the ordered own publish*.
  **Commands and passes (ruled 2026-09-22 — the prologue stall).** A pass
  has no time bound: a first launch on a fresh replica is a full catch-up,
  measured past five minutes on linux under a 33-device account, and for
  all of it every preference surface read empty and swallowed writes,
  because commands were served only between passes. So the loop never
  lets a command wait for a pass. Every pass — the prologue, a nudge walk,
  the publish step, the backstop, reconnect and `reconcile_now` passes —
  is **driven beside the command channel**, and a command is one of two
  kinds. A **local command** — a store read (`get_preference`,
  `states_of_kind`, `data_version`, a group scope's rows, the cached nest
  facts), a read of the assembly's principal slot, the local half of
  `put_preference`, an intent enqueue, a content-scope registration, the
  app-fed endpoint facts (each pass reads a snapshot taken at its start),
  the share transfer ledger's persist (one meta-table write — the
  load-then-persist cycle around it is the *share* pump's, and no account
  pass holds the ledger),
  and the local half of the pump-owned writes — a read-marker raise and an
  observation (a read-modify-write with no yield between its read and its
  write, so no walk page merges between the two), the devices page's
  removal quartet (the reconcile that shares the slot's staged intents has
  no yield point of its own, so it reads the slot wholly before or after a
  command), a ceremony's group adoption (a monotone `apply_class2` merge into a group scope — the pass's group leg, the authority-revocation severance, does read and write group scopes across its awaits, so an adoption landing between them is seen one pass later, never overwritten), and
  the custody, group and share door puts **while an admissible generation
  tip resolves** — is served at the pass's next yield point, on the same thread and
  connection. It qualifies because it touches the store only through
  transactions of its own, holds nothing the pass loads at its start and
  persists at its end, borrows nothing the pass borrows exclusively, and
  does no network — so its interleaving with a pass at an await point is
  exactly the multi-process posture this section already sanctions (a
  non-holder process writing the store the holder is pumping). A
  **pass-bound command** — `reconcile_now`, the explicit barrier, the
  sign-out's enrollment retirement and the shutdown (they end the
  principal's sessions, which no pass may be mid-flight for) — is parked
  and served after the pass, in arrival order. So is a tip-sealed door put that finds **no** admissible
  tip: its door then runs the first-need mint, an escrow deposit and a
  publish (network) under a lock a pass may be holding, so it waits for
  the pass rather than run inside it. Every command's verdict and its
  argument are stated at `Cmd::is_local` (every pump-owned write audited
  2026-09-24). So that a pass
  parks a command by at most one unit, **every unit of local work inside a
  pass ends in a breath** (`pass_breath`, a scheduler yield): every store
  call is synchronous under its `async fn`, so a loop over local units
  never yields on its own — one breath per walk page, one per generation
  the escrow-recovery, top-up, unkeyable and reclamation steps consider,
  one per removed device the reclamation retires. The breath is also
  where the sign-out cut lands during a stretch with no network. **A
  cap-refused pass ends at its enrollment step (2026-09-24).** When the
  nest refuses the machine's register with `fauna.sync.device_limit_exceeded`,
  the principal's grant is registered nowhere, so the data path it
  authenticates cannot come up. Every network leg after that point would
  wait out its kind's whole deadline, and the pass-bound fleet-removal
  resolve behind the Devices page's remove button would park behind them.
  That removal is the remedy the refusal names. So the pass returns right
  after the enrollment step, and the next pass retries the register first
  (`account_runtime::tests::a_device_cap_refusal_ends_the_pass_so_a_removal_is_served_at_once`). **The
  barrier.** `start()` returns at assembly and the prologue runs after it,
  and a local command's round trip proves nothing about that pass; a
  caller that must sequence behind the prologue awaits
  `AccountStoreHandle::settled()` — the pass-bound no-op, answered only
  between passes after every command sent before it (`reconcile_now` is a
  barrier too, and runs a pass of its own). **A local write under a writer
  the same pass retires.** The put is refused atomically when the store's
  stamped writer has moved (the caller retries after the reassembly), so a
  row lands only under the writer current at its commit; if that same pass
  then fences the writer — a burnt journal, a removed-from-account
  successor, a sibling's rotation — the row is part of the predecessor's
  un-pushed tail, and succession decision 3's tail re-author re-puts it
  under the successor on the next pass, value and stamp preserved
  ([`account-replica-posture.md`](account-replica-posture.md) § The store
  device principal → succession decision 3). Nothing about a carried key
  rests on command ordering: a key loaded over a store with no stamped
  writer is retired and re-minted at assembly, inside the readiness
  barrier, so no command ever authors under it (refinement 11, arm (a);
  [`apps/sync-agent-credentials.md`](apps/sync-agent-credentials.md)
  § Implementation status today carries the corrected argument).
  **Bounding the pass itself is not this rule's job.** What dominated the
  measured five-minute prologue was the generation machinery under a
  33-device account — a reclamation pass refusing some 240 retires as
  `not_yet_stable`, an escrow recovery re-deriving every generation ever
  minted — each a per-row RPC or derivation whose cost is
  [`account-data-taxonomy.md`](account-data-taxonomy.md) § The generation
  machinery's to rule on; the pass is made interruptible and non-blocking
  here, never truncated. What the pass does carry since 2026-09-22 is its
  own clock: `PumpReport::timings` names how long each walk and each
  generation-machinery step took, and `log_pump` names the slowest at info
  on every pass of a second or longer — the attribution the measured
  prologue lacked (the taxonomy's status entry of that date states the
  measured floor per step).
  **A sign-out cuts any pass, the prologue included** (one grace after it
  is requested): its enrollment retirement is pass-bound, and a retirement
  that waited out the host's stop budget stranded a device row —
  [`apps/sync-agent-credentials.md`](apps/sync-agent-credentials.md)
  § Implementation status today owns the cut; with the breath above, the
  cut waits at most one unit of local work past its grace.
- **The first listing: a replica that has never listed a scope answers no read of it (ruled 2026-10-01; BUILT the same day — § Implementation status today → *the first-listing gate*).** A freshly signed-in device, a successor's first launch and a re-created store all start as a replica that holds only what it wrote itself; the account's rows arrive with its first listing of the scope from the nest. Until then the store has no answer to "what does the account hold?", and an empty answer is worse than none: a page paints it as the account's value, and a read-edit-write gesture puts it back, edited, at a stamp of now, which under latest-wins outranks the account's real row on every device (measured: [`config-dissolution.md`](config-dissolution.md) § Implementation status today → *a latest-wins write on a never-walked replica*). So the store keeps one durable fact per account-state scope, **listed**, and a read is gated on it.
  **(1) The fact.** `listed(scope)` is replica-local store meta: never synced, and gone with a wiped store, so a re-created store starts unlisted. It is recorded when a listing of the scope from the replica's **bound** nest — a pass's reconcile or a nudge's walk — has run to its end and the keying that follows it has finished: at once when the listing left no row unopened, otherwise when the pass that ran it ends. The pass re-presents what its escrow recovery keyed, so a row this device can open is open by then; a row it still cannot open does not hold the fact back, or one unopenable row would refuse every read for ever. Nothing else records it: not a pass that ended without its listing, not the since-deleted CAS-blob bridge's import, not a peer's or a linked nest's listing (only the bound nest is held complete — [`account-sync-plane.md`](account-sync-plane.md) § The bind leg), and not the account's birth on the device that created it (a successor identity is born too, and inherits). A store that predates the fact learns it at its next listing.
  **(2) The gate** is the first-pass barrier re-keyed (`account_driver::handle_source::first_listing_gate`; the barrier it replaced counted this process's completed passes whatever their outcome). A read that crosses it on an unlisted scope waits for the fact, bounded by `FIRST_PASS_WAIT`, and returns the moment it is recorded — the delegable listing runs early in the prologue, so a preference read no longer waits out a prologue the generation machinery stretches to minutes. If the bound passes, or the pass in flight ends, with the scope still unlisted, the read is **refused as not ready** — the transient class `StoreError::is_not_ready` names for a store that has not assembled — and never answers from the store as it stands; with no pass in flight the refusal is immediate, and a retry after the next pass has listed succeeds. On a listed scope the gate keeps the barrier's other job: a launch's first read waits, with the same bound, for this process's first listing or for the pass in flight to end without one (an offline launch), and then reads the store as it stands, which is stale and real, never empty out of ignorance. That wait is one per process and scope, not one per read. A process that pumps no pass reads the fact its engine holder recorded and waits for nothing.
  **(3) What crosses it.** The reads that cross the barrier keep crossing the gate, each for its own scope — the four preference records (`preference_surfaces::load_record`, and through it every `update_record`), the mail custody's load, the backup state's reads — and the DNS management record's read joins them. The rule for any other read: one that a write of a latest-wins value is derived from — a row of a `LatestWins` kind edited in place, or the stamped latest-wins half of a composite row ([`config-dissolution.md`](config-dissolution.md) § The `__config` dissolution schedule, P3) — crosses the gate. A write needs no gate of its own: a read-edit-write is refused at its read, a per-item put (a follow, a credential, a device's own endpoints row) starts from no read and destroys nothing, and a join kind's door joins whatever the listing lands later.
  **(4) What the user sees.** The refused read is the surface's failed-load state with the reason every nest-requiring affordance already carries (`common.needs_nest`), and the refused gesture fails with the same reason and can be retried; no new element, no new string. It is the retired blob rail's answer for a device that could not reach its nest, which could neither read nor write a preference, kept for the one replica that has nothing else to offer. Every listed replica keeps the plane's local-first read and write, offline included: [`account-offline-mutation.md`](account-offline-mutation.md) classes these writes offline-safe, and that holds from the first listing on.
  **Rejected.** *Journaling the gesture as an intent and replaying it onto the first listed record:* a save is not always a delta (a whole-list save, a pin), and a replace computed from an empty page is the destroying write itself; it needs a third durable queue beside the journal and the outbox, for a mutation the outbox ruling classes as a store write ([`account-offline-mutation.md`](account-offline-mutation.md) § The offline-mutation contract, discriminator 1); and the page would still paint an empty list as loaded. *Re-cutting the cluster's lists as per-field joins:* that changes what two concurrent writers lose, not what an unlisted replica reads — its page still shows nothing as the account's value, and a scalar or a replace-style save still starts from nothing — and a kind's merge policy freezes with its string. *Refusing at the writer door by policy:* a device's own endpoints row and every per-item put are latest-wins writes that must land before the first listing and destroy nothing. **What the gate does not change:** a listed replica that has gone stale still writes from the value it last held, and its newer stamp outranks what the fleet wrote to that one record meanwhile. That is latest-wins' own rule for a concurrent write ([`account-sync-plane.md`](account-sync-plane.md) § Merge-policy seam), bounded by the replica's last listing and narrowed by the launch wait of (2). **Declared residuals:** (i) *ruled since, as clause (5) below:* a listed replica that holds rows of a tip-sealed kind it cannot open — a generation it has not keyed — answers a read without them, so a read-edit-write of that kind's latest-wins value (the DNS record, a backup list) starts from less than the account holds and, landing at a newer stamp, destroys it; reproduced at the handle 2026-10-01 and closed by clause (5) the same day (§ Implementation status today → *the unkeyed hold*). (ii) *Not ruled here:* the nest-less profile ([`account-data-plane.md`](account-data-plane.md) R11), unbuilt, has no bound nest to list; it names its own source of the fact before it ships, and until then nothing here exempts a replica.
  **(5) The unkeyed hold: a listed replica answers no gated read of a tip-sealed kind while it holds fleet rows under a generation it may still be keyed for (ruled 2026-10-01; BUILT the same day — § Implementation status today → *the unkeyed hold*).** `listed` says the replica has seen every row the bound nest holds; it does not say the replica could open them. For a generation-0 kind the two are one, since the account's own key schedule opens every such row. For a kind sealed under the generation tip ([`account-data-taxonomy.md`](account-data-taxonomy.md) § The generation machinery) they are not: a row the listing left unopened for want of a generation's key may be the account's value of any tip-sealed kind — the envelope names its generation in cleartext and nothing else, and the kind is inside the seal — so a fold of the rows the replica could open is the unlisted replica's empty answer over again, and a read-edit-write from it destroys the account's record the same way (measured: the gap entry named above). The replica knows which generations it is missing, so it holds the read until it knows more. **The fact.** `unkeyed(scope)` is replica-local store meta beside `listed`, kept for the fleet scope: the ids of the generations under which a listing from the bound nest left a row unopened because this device held no key for that generation. A full listing that runs to its end replaces the set with its own; a nudge's walk only adds to it. Each id carries one bit, **answered empty**, set when the escrow holder has answered this replica's request for that generation with no wrap that opens (the request and its per-runtime memo are the taxonomy's *Escrow recovery*; the bit is durable where the memo is not, so a relaunch does not hold again on a question the holder has already answered, while each runtime start still asks once more). **The hold.** A generation in the set holds while merged state carries its mint row live, canonical and id-bound, and at least one source of its key still stands for this device: (a) *the device itself* — it keys the generation now, by a top-up that merged since or a recovery another runtime of the machine made, and no full listing has re-presented the rows; (b) *the holder* — this runtime holds the identity seed, merged state carries the receipt the escrow recovery asks on, and the answered-empty bit is not set; (c) *a sibling* — a currently verified, non-removed device other than this one that minted the generation (its mint passing the authorship gate) or that lists it in a verifying reach row. Nothing else holds. **The gate.** While any generation holds, a read that crosses the gate for a kind sealed under the generation tip waits, bounded by `FIRST_PASS_WAIT` and returning the moment none holds — the pass in flight may key the generation and re-read — and is refused as not ready at the bound, or when the pass in flight ends with a hold standing; it never answers from the store as it stands, at a launch's first read either. The reads of (3) this covers are the mail custody's load, the backup state's reads and the DNS record's read, and every read (3)'s rule adds for a tip-sealed kind; the four preference records are generation-0 rows of the delegable scope and never wait on it. Writes stay as (3) leaves them: the read-edit-write is refused at its read, and a per-item put, a join, the device's own endpoints row and the pass's writers land as before. So does the first-need mint: a holding device with no tip it can key still mints, and a sibling that keys both generations then re-seals the account's rows under the new one, which is one more way the rows open. **How a hold ends.** By the rows opening: a full listing with the key in hand, which for a key recovered from escrow is the same pass (the taxonomy's *Escrow recovery*, the pass that keys a generation is the pass that reads it). Or by its sources running out, each bounded by something a client can do: (a) by the next full listing; (b) by the holder's answer — a holder that cannot be asked is an unreachable nest, and the hold then says what every nest-requiring affordance says; (c) by the sibling topping this device up or re-sealing, or by the user removing that device on the Devices page, the standing gesture for a device that will not come back, which works from the held device. When no source stands, no living device and no holder can open those rows, what the store holds is all the account can read, and the read answers it. **Why no generation holds for ever.** The fact of (1) is unchanged, and an unopenable row still does not hold back `listed`. A hold needs evidence a vandal cannot write: a holder-signed receipt, which exists only for a deposit made over an account session and is spent by the holder's first answer, or the signature of a device the fleet still verifies. An invented or orphaned mint has neither — the shape [`account-data-taxonomy.md`](account-data-taxonomy.md) § The generation machinery's resolver ruling (2) and mint trigger (a) were written against — so it holds nothing, however many rows rest under it. **What the user sees.** (4) unchanged while the device itself or the holder is a standing source: the nest is what is missing, and `common.needs_nest` says so. Where a sibling is the only source, the same failed-load state and the same refused gesture carry one new shared reason string, saying that another of the account's devices holds what this one needs and that opening the app there, or removing that device if it is gone, ends the wait; no new element. **Bounds, stated.** (i) The hold reads this replica's merged state: a sibling that keys the generation only by top-up, where the minter has left and the sibling's reach row has not arrived, holds nothing until its next full pass publishes the reach. (ii) A seedless runtime never asks the holder, so only (a) and (c) hold it; the machine's seed-holding app recovers into the bundle both read. (iii) Removal is a decision: once the user has removed every sibling that held a generation and the holder has answered empty, a key that turns up later finds whatever this device wrote meanwhile. (iv) A device the fleet still verifies can hold another by listing a generation it never hands over — vandalism of a member's grade, ended by removing it. (v) The hold is per generation, so one held generation refuses every gated tip-sealed read, not only the kinds whose rows rest under it.
  **Rejected for (5).** *Ending the hold at the holder's answer alone:* a seedless replica never asks, so nothing would hold on the device a sibling has just approved and is about to top up — a window at every approval under the narrower seed postures ([`account-replica-posture.md`](account-replica-posture.md) § *Seed residency*) — and a rebuilt holder beside a sleeping sibling would release the destroying write. *A time bound on the sibling's source:* it delays the destroying write and does not prevent it, any constant is shorter than a laptop left in a drawer, and no human would choose the number. *Refusing the whole-record replace at the writer door, reads untouched:* the page still paints a default record as the account's value, and every replace door would carry the rule the one gate carries once — the first listing's own reason. *Withholding the first-need mint while a generation holds:* the pass's own writers trip the mint before any gesture (measured), so the refusal would have to cover the device's endpoints row and every per-item put, which destroy nothing; it re-narrows mint trigger (a) toward "refuse while a mint row exists", the wedge that trigger was widened to remove; and the read would still answer the default record. *A hold per kind:* an unopened row does not say its kind. *The top-up pass's inline and per-healer coverage as the sibling's evidence:* that predicate deliberately checks no authorship, so an invented mint naming a live member beside a wrap that opens nothing would hold for ever.
- **Writer key: W3 begins the T10 slot with its one durable-key item.** The
  first assembly for an account on a machine mints the store's Ed25519
  device keypair and persists the secret in the T10 credential slot
  (`fauna-credential-store`, namespace `fauna-account-store`, account
  attribute = actor id hex — § The store device principal owns the slot
  mechanics); every later assembly loads it, so the machine stays one writer
  forever (the journal's equivocation refusal depends on writer-id
  stability) — and, the converse, **a writer lives exactly as long as its
  journal**: a key loaded over a store with no stamped writer is retired and
  re-minted, never put to work ([`account-replica-posture.md`](account-replica-posture.md)
  § The store device principal, refinement 11 owns the rule and its
  burnt-journal heal). The `DeviceAuthorization` + bearer halves of the bundle join
  at W5 (T10/T11); at W3 the nest leg authenticates via the app's own
  session, per R4's closing rule ("authoring surfaces hold the seed exactly
  as today; the plane's data sync does not wait").
- **Consumption: assembly is per-app glue of a few lines at the app's one
  post-auth hook; everything else is the shared runtime.** tui first (the
  lead app): `session::establish` assembles the runtime and attaches the
  session's wake streams (`with_session_wakes` — the session client's
  reconnect watch and the push stream the runtime's own nudge arm reads; no
  app-side push arm since 2026-09-25; the data client's reconnect watch is
  the runtime's own, merged in `resolve_and_start`); linux mirrors it in `account_runtime.rs`;
  apple/windows/android reach the same runtime through a `fauna-ffi` factory
  in the batched trickle-down (the conversations-session factory shape); web
  as above. Teardown at sign-out **and** at plain quit — at W3 the pump is
  in-app-only, and replica freshness while no app runs arrives at W5 when
  the agent mounts the store as the always-on host (R2). Singleton
  discipline at W3 is the shipped at-most-one-instance law (§ Multi-instance
  concurrency — still the law until W5 lands); the runtime's start takes the
  store dir, so W5's `engine.lock` election (the flock filename already
  reserved by the store) slots in front of the pump without reshaping it.
- **Pilot: the preference cluster — superseding [`account-data-plane.md`](account-data-plane.md)'s W0 "notifications
  or contacts" suggestion.** W2.5 registered the four preference kinds and
  built their bridge, so the cluster meets the original pilot criteria
  (small, non-MLS, clearly-per-actor) with zero new kind work. The read-path
  inversion pilots on tui's moderation (muted-words) surface: reads answered
  from the handle, saves routed through `put_preference` (the blob rail
  was written beside it until closure step (5) deleted the bridge,
  2026-10-01). Notifications/contacts follow as later surfaces, not the pilot
  (§ Workstreams carries the same correction).

## Implementation status today

**BUILT 2026-10-01 — § The client-side lifecycle → *The first listing*: the first-listing gate.** **The fact** is `AccountStore::listed` / `record_listed`, the store-meta key `listed/<scope>` on all three backends (the store conformance case `the_listed_fact_is_per_scope_durable_and_absent_on_a_fresh_store`, graded over every arm). The bound plane alone records it: `AccountStatePlane::reconcile` and `walk` record it at once when the listing left no row unopened, and otherwise `AccountStatePlane::pass_ended` records it at the end of `pump`, or right after a nudge's walk, whose unit keys nothing more; a peer leg's plane, a linked nest's and the bridge record nothing, and a pass that was cut records nothing. **The gate** is `handle_source::first_listing_gate(handle, scope)`. It waits on the runtime's in-process `FirstListings` (shared by the handle and its two bound planes, kept across a reassembly), on `AccountStoreHandle::settled` and on `FIRST_PASS_WAIT`, and reads the durable fact on the store thread (`AccountStoreHandle::scope_listed`, a local command served inside a pass), so a runtime that pumps no pass answers what its engine holder recorded. The pass-count key is gone. The refusal is the typed `ScopeNotReady`; on a `StoreError` seam it is a `StoreError::Load` that `is_not_ready` recognises. **What crosses it:** `preference_surfaces::load_record` on the delegable scope; on the fleet scope the handle's `MailStore` loads, its `BackupStateStore` reads, `AccountStoreHandle::dns`, and `AccountStoreHandle::write_contact_overlay` — the contact overlay's save, whose store-thread read-modify-write re-stamps every register it names; the `ContactsCache` projection's load (`AccountStoreHandle::contact_overlays`) stays ungated, since it only renders what the store holds and the edit form is seeded from that projection. The mail gate sits on the handle's own seam impl, so a host that hands the raw handle to the succession aftermath is covered exactly as the sourced `AccountMailStore` is. **What the user sees:** the plane-only preference surfaces (`preference_surfaces`) answer the refusal as `common.needs_nest` (`preference_surfaces::plane_failure`), on all seven apps' seats; tui shows it on `error-message` for a refused load of the muted words, the trained topics, the task-delegation rows and the Folders page's default conflict policy, and for every refused gesture; tui and linux show it for a refused contact-overlay save (`contact_overlays::not_ready_reason`). **Pinned** in `account_runtime.rs`, each seen red for its stated reason: `a_first_write_on_a_never_listed_replica_is_refused_until_its_first_listing`; `a_first_write_after_a_failed_listing_is_refused_whatever_the_bridge_imported` (retires with the bridge); `a_first_listing_that_outlasts_the_bound_refuses_the_read` (the bound's arm, under a paused clock); `a_listed_replica_restarted_offline_reads_and_writes_and_a_wiped_one_does_not`; `a_dns_replace_from_a_never_listed_replica_is_refused_at_its_read`; `a_contact_overlay_save_from_a_never_listed_replica_is_refused`. **Owed.** (i) The six apps after tui render a refused *load* as they render any failed background read: the reason reaches them through the shared twins, and showing it is each app's follow-up. The mail-settings machine's `hydrate` keeps its default snapshot on any failed read, the refusal included, so its page reads "disabled" until the first listing, and its enable gesture is refused at its read (a refusal measured live on 2026-10-05 was the unkeyed hold's, not this gate's — the hold's entry below). (ii) The build's audit of clause (3)'s rule found two reads that did not cross the gate; the contact overlay's save now does (above), and one remains. The CAS-blob bridge's import arm (`preference_bridge::decide`, a plane with no row beside a non-default blob) put the blob's value on the plane at the blob's `updated_at` whether or not the scope was listed; it retired with the bridge at the dissolution's step (5), 2026-10-01. Every other latest-wins write door either starts from no read, is a join, or writes nothing on an empty read. The gate was owed before the dissolution's step (6) and no longer holds it; neither does clause (5)'s unkeyed hold, built the same day (the entry below) ([`config-dissolution.md`](config-dissolution.md) § The `__config` dissolution schedule → *What replaces the bridge's two carriages*).

**BUILT 2026-10-01 — § The client-side lifecycle → *The first listing*, clause (5): the unkeyed hold.** **The measurement it closes**, with two devices of one account over one nest and one trusted escrow holder: the first device mints the account's one generation and publishes a DNS record of two managed domains and one box's backup-destination list of two destinations; the second device then lists the fleet scope while the holder's read door is unreachable, so its walk leaves the first device's rows unopened and its own passes mint a second generation before any gesture. Before the hold, `AccountStoreHandle::dns` and the backup read folded the entries the store held, answered the default record and an empty list, admitted the edit, and once the fault lifted and both devices walked, the newer stamp had destroyed the two domains and two destinations on both devices. **The fact** is `AccountStore::unkeyed` — the store-meta key `unkeyed/<scope>`, one 33-byte record per generation (its id, then the answered-empty bit), on all three backends — written through `replace_unkeyed`, `add_unkeyed` and `mark_unkeyed_answered_empty` (the store conformance case `the_unkeyed_set_replaces_adds_and_keeps_its_bits_durably`, graded over every arm). A walk names, in `WalkReport::unkeyed`, the generations under which `AccountStatePlane::trial_open` left a row unopened because `generation_tip::generation_key_for` answered none — that arm only. The bound plane alone records them: `reconcile`, a full listing run to its end, replaces the set and carries each surviving id's bit; `walk`, a nudge's, only adds. **The bit** is set by `generation_escrow_recover::ensure_recovered`'s no-wrap arm; the `EscrowRecoveryMemo` and its one request per generation per runtime start are unchanged. **The predicate** is `unkeyed_hold::unkeyed_hold`, one function over merged state. A recorded generation holds only while its mint row is live, canonical, `Minted` and id-bound, and then on the three sources it reports as `HoldSources`: (a) `generation_key_for` answers a key; (b) a seed-holding runtime with custody, a receipt `escrow_acked_generations` names under the trusted holders and their verified ancestors (the recovery's own call), the bit clear; (c) a mint whose authorship verifies, by a verified member other than this device, or a verified member's reach row (`generation_topup::wrap_coverage`'s `reach` map) that lists the generation. The top-up pass's `covered` set is never read. It runs on the store thread (`AccountStoreHandle::unkeyed_hold`, a local command). **The gate** is `handle_source::read_gate(handle, kind)`: the first-listing gate on the kind's home scope, then, for a `GenerationTip` kind, the hold. A held read waits — woken by `FirstListings::note_unkeyed_changed` whenever a plane writes the set or the recovery sets a bit, bounded by `AccountStoreHandle::settled` and `FIRST_PASS_WAIT` — and is refused as `ScopeNotReady` (`ScopeNotListed` until this build), whose `NotReadyReason` is `HeldForNest` while source (a) or (b) stands and `HeldForSibling` where only (c) does. On a `StoreError` seam the sibling reason crosses as `LEDGER_AWAITING_SIBLING`, which `StoreError::is_not_ready` recognises beside `LEDGER_NOT_READY`, and `StoreError::not_ready_reason` names the string a surface shows. **What crosses it:** `AccountStoreHandle::dns`, the handle's `MailStore` loads, its `BackupStateStore` reads and `AccountStoreHandle::write_contact_overlay` (the overlay's save, whose store-thread read re-stamps the registers it names) — every gated read of a tip-sealed kind; the first-listing gate's audit found no other. Each caller degrades on the refusal: the backup enrollment heal skips (`is_not_ready`), the aftermath's re-grant reports a failed leg and retries, the box-retire wizard's DNS read is best-effort, and the pair trust view renders no backup rows, as it does on any failed read. **What the user sees:** `common.needs_nest` for sources (a) and (b), and the new shared string `common.needs_other_device` where a sibling is the only source — `handle_source::not_ready_reason` (behind `preference_surfaces::plane_failure` and `contact_overlays::not_ready_reason`) and `StoreError::not_ready_reason` map a refusal to its string; tui's Backups page shows either as its failed-load reason, and tui's and linux's contact-overlay save as the refused gesture's. **Audited, not widened:** a seedless runtime (the sync agent) makes no read-derived write of a tip-sealed latest-wins value — its handle is read for the folder-key custody and the content-key walk, and its pass's writers write the device's own rows (endpoints, reach, wraps) or joins — so bound (ii) costs nothing today. **Pinned** in `account_runtime.rs`, each seen red for its stated reason or shown to pin its clause by taking the clause out: `a_dns_read_held_for_an_unkeyed_generation_is_refused_until_the_key_arrives` and `a_backup_read_held_for_an_unkeyed_generation_is_refused_until_the_key_arrives` (the two pins that measured the loss, rewritten: three domains and three destinations on both devices), `a_holder_that_answers_empty_leaves_the_hold_on_the_minter`, `removing_the_minter_after_the_holder_answers_empty_ends_the_hold`, `a_held_replica_relaunched_offline_stays_held`, `a_replica_relaunched_offline_after_the_holder_answered_empty_is_not_held`, `a_hold_refuses_only_the_gated_tip_sealed_reads`, `a_seedless_runtime_is_held_by_a_sibling_that_minted_the_generation` and `a_seedless_runtime_is_not_held_by_the_receipt_alone`; the control `a_dns_edit_on_a_listed_replica_that_opened_the_accounts_row_lands_on_it` keeps its outcome; in `unkeyed_hold.rs`, `an_invented_mint_naming_a_live_member_holds_nothing` beside the sibling and reach controls. **Owed.** The six apps after tui render the refused load as they render any failed background read; showing the reason text is each app's follow-up. The mail-settings machine's default snapshot on a failed read (the first-listing entry above, owed (i)) reads "disabled" under a hold as it does before the first listing. **Measured live 2026-10-05, and closed: a hold that stood because the device dropped keys it had recovered.** A whole-suite tui run against the dev box failed 48 tests on a mail-enable gesture refused as not ready, and the refusal was this hold, not the bound of (2): the prologue ended in under 12 s, well inside `FIRST_PASS_WAIT`; its escrow recovery keyed all 45 of the run account's generations (`Recovered(45)`); the re-presenting `reconcile` then failed (`no candidate generation tip resolves for this device`), so the unkeyed set its fleet walk recorded was never replaced, and source (a) held every gated read for want of the nest. The cause was the retained-key carriage: `principal_bundle::persist_retained` trims the set to what fits one credential item (fifteen generations), and `PrincipalBundle::record_generation_key` rebuilt the in-memory view from the trimmed slot on every record, so a recovery of more generations than the item holds kept only fifteen and the newest; with the tip among the lost keys, the next tip-sealed write minted one generation more, which is why the set grew by about one per launch once past the item (a fresh account measured the same day kept one generation and a 2.6–3.3 s prologue over twenty relaunches). Since then a key the trim drops stays in the process's view for the life of the process (`PrincipalBundle`'s overflow, emptied of a generation only by its shred), and the gate logs which reason refused and which sources stand. **Pinned:** `account_runtime.rs`'s `a_device_that_recovers_more_generations_than_its_carriage_holds_keys_them_all` (twenty minting devices, then a fresh device that opens every row, mints nothing and has its DNS read answered; red before on four unopened rows) and `principal_bundle.rs`'s `every_key_this_process_obtained_stays_usable_past_the_carriage_cap`. What survives a restart is still the item's fifteen: a relaunch re-asks the holder, as every runtime start does, and which keys the carriage should keep stays the retention question [`owner-key-material.md`](owner-key-material.md) assigns to the cadence trigger (c).

**RULED + BUILT 2026-10-01 — § The client-side lifecycle → *Ruling (4), the teardown rider*: web's sign-out stops its runtime with the sign-out stop.** `StopReason`, `ACCOUNT_RUNTIME_STOP_BUDGET` (with its compile-time check against the pass grace and the retirement budget) and the one stop per handle (`stop_one`) moved from `fauna-client-account-runtime` to `fauna_account_plane::account_driver`'s `stop` module, re-exported at their old paths, so web states the native type. `fauna-wasm`'s `account_runtime::stop(reason)` runs it over the tab's runtime; `accountRuntimeShutdownForSignOut` is the sign-out export beside the plain `accountRuntimeShutdown`, and `accountRuntimeStopBudgetMs` hands the SPA the shared budget. `apps/fauna-web/src/lib/account-runtime.ts::stopAccountRuntime(reason)` takes `'sign-out'` or `'account-switch'`: `identity.logout()` awaits the sign-out stop, raced against the budget with an in-flight start waited out inside it, before it clears the identity (decisions (a) and (b)); the actor-scoped reset (`resetConversationsManager`) keeps the plain stop and finds nothing running after a sign-out; and no runtime starts for the signed-out account between the stop and the identity change. A tab with no runtime logs that nothing was retired and signs out (decision (c)). Proofs: the `wasm-bindgen-test` `account_runtime::tests::a_sign_out_stop_asks_the_nest_to_retire_and_the_erase_leaves_no_store` (over the real `web_host` runtime, a plain stop sends no `fauna.sync.device_grant.revoke` and the sign-out stop sends it), `test_sign_out_device_roster.py::test_signing_out_and_back_in_keeps_one_roster` green on web for the first time, and `test_sign_out_web.py::test_web_sign_out_retires_the_enrollment_and_erases_the_account_store`. **What it replaced, measured in Chromium the same day,** one dedicated account, sign in → sign out through Settings → sign in again: after the sign-out the nest's roster still named the first sign-in's principal on the machine's named row, 30 s later as well, where a native app's row reads no grant. The second sign-in re-adopted that row under a fresh writer key, so the roster stayed one row. And the second sign-in's replica read **both** writers `enrolled` in `fauna.state.device-set` (read once, right after its enrollment latched): each sign-out and sign-in in a browser left one more enrolled fleet member behind. What the same sign-out leaves in the browser is [`apps/account-scoping.md`](apps/account-scoping.md) § Implementation status today's (the 2026-10-01 web paragraph).

**RULED + BUILT 2026-09-30 — § The client-side lifecycle → *The account port*: how a wasm chunk other than the core chunk reaches web's runtime; built with its first seam, the Devices page's fleet door.** `libs/fauna-account-port` holds the transport trait, the fault, the encode/decode halves, the JS binding over `SharedAccountPort` and the loopback. `fauna-devices-machine`'s `port` module (feature `account-port`) holds the fleet seam's forwarder `PortFleetRemoval` and its `serve`; its loopback tests cover every method in both arms and a faulting transport on every method, and `tests/devices_lifecycle.rs` reruns the removal-order pins through forwarder → loopback → `serve`. `RuntimeFleetRemoval` moved to `fauna-account-seams` (re-exported at its old path). `fauna-wasm`'s `account_runtime` keeps the actor id beside the handle (`handle_for`), and its `account_port` module exports `accountPortCall`, proved by three `wasm-bindgen-test`s over the real `web_host` runtime (no runtime refuses, another account refuses, the running account's member read answers this browser's fleet id). The folders chunk's `DevicesMachine.setAccountPort` wires the forwarder, and `devices-session.ts` hands it `$lib/account-runtime`'s `sharedAccountPort(secretHex)` before the first refresh (`shared-account-port-contract.test.ts` pins the one implementation). So a web removal resolves and stages its fleet leg BEFORE the nest deletion and settles it on the outcome, and is refused — deleting nothing — while no runtime serves the account. Web's paint of the member group landed the same day, its confirm through the key-addressed `DevicesMachine.removeMember(deviceIdHex)`, and `test_device_member_removal.py` is green on web (with a tui sibling seat) — decision (i)'s e2e part; web's dispatcher answers `device_set_state` over the same reader as every native app, lifted to `fauna_account_plane::account_driver::e2e_readers`. The seven plane-kind seams of decision (h) are built by their kinds' consumer cuts ([`config-dissolution.md`](config-dissolution.md) § Implementation status today), each adding a `port` module beside its trait and one arm to `accountPortCall`'s dispatch. **Built — the custody-ceremony seam (2026-09-30):** `fauna_client_config::CustodyCeremonyStore` (`custody`, `merge_custody`) with its `custody_port` module (feature `account-port`: the forwarder `PortCustodyStore` and `serve`, loopback-proved on both methods, both arms and every fault), served in `accountPortCall` by the handle's own implementation for the port's account only; `DevicesMachine.custodyFacetLoad` takes the port (`shared-account-port-contract.test.ts` pins that it is handed `sharedAccountPort`). **Built — the followed-folders seam (2026-09-30):** `fauna_client_config::FollowsStore` (`follows`, `put_follow`, `unfollow`) with its `follows_port` module (feature `account-port`: the forwarder `PortFollowsStore`, its `from_js_port` constructor, and `serve`, loopback-proved on every method, both arms and every fault), served in `accountPortCall` by the handle's own implementation for the port's account only; the folders chunk's follow / unfollow / list faces and `setFollowedFoldersSource` and the media chunk's `setFollowedMediaSource` take the port (the same contract test pins each is handed `sharedAccountPort`). **Built 2026-09-30: the ATProto identity seam and the ATProto credential seam**, both wired by the ATProto chunk's `AtprotoSettingsMachine.setAccountPort` from `sharedAccountPort(secretHex)` before the first refresh (`wasm-atproto-settings.ts`): `fauna_client_atproto::identity_store::AtprotoIdentityStore` (`atproto_identity`, `merge_atproto_identity`; forwarder `fauna_client_atproto::port::PortAtprotoIdentityStore`, served by `fauna_account_seams::atproto_identity::RuntimeAtprotoIdentity`) and `fauna_atproto_settings_machine::AtprotoCredentialStore` (`atproto`, `put_app_credential`, `revoke_app_credential`; forwarder `fauna_atproto_settings_machine::port::PortAtprotoCredentials` with its loopback tests over every method, both result arms and every fault, served by `fauna_account_seams::atproto_credentials::RuntimeAtprotoCredentials`) — each served by the adapter the native seats wire. **Built 2026-09-30: the mail seam** (with `fauna.state.mail`'s cut): `fauna_client_config::MailStore` with its `mail_port` module (feature `account-port`: the forwarder `PortMailStore` and `serve` for its one crossing door, `mail.load` — the row writes stay with the mail-settings machine in the core chunk and refuse on the forwarder without a call), its refusal crossing as the bare message so `StoreError::is_not_ready` still recognises the transient one; loopback-proved on the read's both arms, every fault and the refused row doors; served in `accountPortCall` by the handle's own implementation for the port's account only; the labeler-catalog chunk takes the port in its constructor, and `createLabelerCatalogMachine` hands it `sharedAccountPort(secretHex)`. The folder-keys seam is served the same way (`fauna_client_folders::port`, over `fauna_account_seams::folder_keys::PlaneFolderKeys`). **Built — the succession-ledger seam (2026-10-04, retiring the interim ledger-only crossing the ledger's cut landed with on 2026-09-30):** `fauna_client_config::SuccessionLedgerStore` with its `succession_ledger_port` module (feature `account-port`: the forwarder `PortLedgerStore`, its `from_js_port` constructor minted for the port's account, and `serve` for the two crossing doors `succession_ledger.load` and `succession_ledger.merge` — `self_actor` answers from the minted account without a call, `repoint` and `raise_grant_marks` refuse on the forwarder without one), its refusal crossing as the bare message; loopback-proved on both doors' both arms, every fault on every method and the refused succession writes; served in `accountPortCall` by the handle's own implementation for the port's account only. The labeler-catalog chunk builds it from the constructor's port, and `DevicesMachine.custodyFacetLoad` / `custodyRevoke` take the port (`shared-account-port-contract.test.ts` pins both are handed `sharedAccountPort`, and that no ledger-only crossing reappears). The arm resolves the handle per call through `ResolvingLedgerStore` over `handle_for(actor)`, so a runtime still starting for the port's account is waited out (`LEDGER_READY_WAIT`) as the core chunk's own grant machines wait for it, and one that never comes answers the seam's own `LEDGER_NOT_READY` refusal.

**RULED 2026-09-26 — § The client-side
lifecycle → *The trigger fired*: web's account-plane hosting is owed, and
the community room class's web key home is the plane itself.** Ruling (1)
is built (2026-09-27): `libs/fauna-account-plane` holds the account-state
and group planes, the six generation passes, the page walk, the preference
bridge and the plane-row modules (`content_scope_plane` with its
`ScopeBinding`, `custody_rows`, `contact_overlay_rows`, `departure`,
`device_endpoints_writer`, `outbox`, `p2p_participation`, `scope_set`,
`seen_set_producer`), `fauna-sync-engine` re-exporting each at its old
path, and `just wasm-chunk-check` checks it for wasm32. The 2026-09-26
measurement over-counted by four, which stayed native then: `principal_bundle`
(the credential slot — `fauna-credential-store` is not wasm-clean; its
bundle has since moved, see ruling (4) below),
`fleet_removal` (its completion leg drives that slot) and, through its
`removed_device_ids` read, `group_authority_revocation`; and
`observation_intake`, whose writer the security review kept `pub(crate)` so
the runtime's handle stays its only door. Where the slot seam and that door
land is (2)'s to settle — the driver needs all four. Ruling (3)'s backend is
built too (2026-09-27): `fauna-account-store`'s `indexeddb` arm (IndexedDB
object stores + OPFS segment files) implements `StoreBackend`, graded in
headless Firefox by the same backend-generic conformance suite the SQLite
arm and the `memory` test double run (`src/conformance.rs`; `just
wasm-test-check`) — segment adoption included, over the real writer's output
pinned as bytes — with the store's web root (a per-origin `fauna-account-store/<actor-id-hex>`
store name) and its engine lock over Web Locks (§ Multi-instance concurrency
in [`account-runtime.md`](account-runtime.md)) beside it. Nothing else was
built then: web hosted no runtime and no device principal, opened no store,
and registered no `GroupReceptionKeys` seam (ruling (4), below, since). The program is the ruling's
(2)–(5) in the order it states; the pump split comes first, and web's
hosting follows it. **Ruling (2) is built too (2026-09-28):** `fauna_account_plane::account_driver` (feature
`account-driver`, which `fauna-sync-engine/account-runtime` forwards) holds
the driver, with `principal_custody`, `host_legs`, `fleet_removal`,
`observation_intake`, `group_authority_revocation` and `succession_tail`
beside it, every name re-exported by the engine at its old path;
`fauna_sync_engine::account_runtime` is the native host
(`AccountStoreRuntime::start`, the assembly loop, the slot, `NativeLegs`,
`FileElection`); `just wasm-chunk-check` checks the plane crate with the
driver for wasm32, and `tests/no_native_time.rs` pins the time ban. The
build decisions are in § The client-side lifecycle → *The trigger fired*.
**Ruling (4) is part-built (2026-09-29):** the seam glue's wasm-capable home exists — `fauna-account-seams`
holds `conversation_seams::wire` and the three seams over the plane crate's
`AccountStoreHandle`, its tasks spawned through `TaskSpawner` and its one
wait `fauna_sleep::sleep`, `fauna-client-account-runtime` re-exporting every
module at its old path, `just wasm-chunk-check` checking it for wasm32 and
`tests/no_native_time.rs` pinning the ban — decisions (a) and (b) of § The
client-side lifecycle → *The trigger fired*, ruling (4)'s build decisions.
Decision (c)'s native half is built too (2026-09-29): the T10 bundle is
`fauna_account_plane::principal_bundle::PrincipalBundle` (feature
`account-driver`), generic over `fauna_client_accounts::SecretStore` (the
`SlotStore` seam — the trait `CredentialStore` and `LocalStorageSecretStore`
already implement) and a `SlotSection`; `fauna_sync_engine::principal_bundle::PrincipalSlot`
is that bundle over `CredentialStore` with the store dir's `migration.lock`
(`MigrationSection`), byte-identical at rest, every name at its old path.
**Ruling (4) is built (2026-09-29) —
web hosts the runtime:** `fauna_account_plane::web_host` (cfg wasm32,
feature `account-driver`) mints the driver and its handle, resolves the
writer key through the plane bundle's one mint-or-load
(`principal_bundle::mint_or_load_writer_key`, which the native host calls
too) and the bundle over the caller's `SecretStore` with the tab's own
`TabSection`, opens `IndexedDbBackend` under `StoreRoot::store_name(actor)`,
takes the election over Web Locks (`WebElection`) and serves with `NoLegs`
on a `spawn_local` task, looping on `ServeEnd`; `fauna-wasm`'s
`account_runtime` module supplies the session's `WsRpcClient` (whose
reconnect counter and push stream now have Rust faces,
`subscribe_reconnects`/`subscribe_pushes`), the keypair, the MLS engine's
joined channels, the attested predecessors, the origin's pin (plus, under
`test-helpers` only, the e2e seed read from localStorage in the one grammar
`fauna_client_core::nest_trust::seeded_nest_identity` shares with native)
and web's derived device id, and registers the seams at the store-ready edge
through `conversation_seams::wire_parts` — the body `wire` itself runs, over
the manager and FaunaMls backend web holds without a session. The SPA starts
it once its conversations manager exists and shuts it down with the
actor-scoped reset (`$lib/account-runtime`); the devices page paints the cap
notice and marks This-device off the same handle; `PumpCyclesView` is the
one `account_pump_cycles` shape native and web publish. Proof: `fauna-wasm`'s
`account_runtime::tests` (headless Firefox, `just wasm-test-check`) puts a
group-reception keypair through the seam, shuts the store down, reassembles
it and reads the key back. The host shares the browser's one wasm stack
where the native host has a store thread of its own, so `fauna-wasm` links
its chunk with a 16 MiB stack (`libs/fauna-wasm/build.rs`): measured
2026-09-29, the proof traps at the linker's 1 MiB default in a debug build
— the serve loop polling a first-need generation mint down to its X-Wing
seal — and passes at 16 MiB (the minimum between not bisected). **Built
2026-09-30 — web's host runs the succession probe and the lost-slot heal**,
in the native order and with the native restart loop and caps:
`fauna_account_plane::principal_succession` holds both, generic over the
bundle's `SecretStore`, its `SlotSection` and the `StoreBackend` (the
rotation's own backend comes from the caller's opener), and
`fauna_sync_engine::principal_succession` instantiates them over the
credential store, `migration.lock` and SQLite at the old names. On web the
section is the tab's own (decision (c)): two tabs healing at once are
serialized by the heal's slot re-read and the fence's stamp re-check, not
by a lock. Proof: `account_runtime::tests`'s lost-slot case drops the
writer key from the slot over a kept IndexedDB store and reopens under a
fresh writer, the lost one retired and the store's rows intact. Web's
preference faces go through the shared `preference_surfaces` (step (2),
built 2026-09-30; plane-only since step (5), 2026-10-01 —
`config-dissolution.md` § Implementation status today owns it); the `__config`
localStorage replica retired with the rail on 2026-10-02 (the same entry,
closure step (6)). **Built 2026-10-01 — on web, a store transaction no longer
depends on when its caller is polled.** The driver serves a local command at
a pass's yield point and does not poll the pass until the command has
answered (§ The client-side lifecycle → the pump bullet's *Commands and
passes*). A native store call is synchronous under its `async fn`, so a
suspended pass has nothing in flight; a web one is not — an IndexedDB
transaction commits the moment its last request has answered with no further
one placed. So a read-then-write the pass had open committed request-less
while a command was served and its write met `TransactionInactiveError`:
measured in a web journey's console as `publish_pending (fleet): … relay
plane: IndexedDB put: TransactionInactiveError` on every pass, and in the
prologue's `tail re-author` — the tab's own rows never reached its relay
plane, and a multi-store method could commit only the requests that had
already landed. `IndexedDbBackend` now runs every transaction as a
`spawn_local` task of its own (`transact`, the backend's one door to a
transaction): the browser's microtask queue runs that task's continuation
whatever the caller is doing, the body owns what it touches, and a caller
dropped mid-method (a pass cut by a sign-out) leaves the task running to its
commit. *Commands and passes* is unchanged by it — the pass is still not
polled while a command is served, so a command still interleaves only at a
pass's await point. Proof (headless Firefox, `just wasm-test-check`): the
store's browser suite polls a relay put once, runs another method to
completion and only then awaits the put (`fauna-account-store`'s
`tests/web.rs`); `fauna-wasm`'s `account_runtime::tests` runs a pass beside
a hundred local reads with an unpublished own row on each plane, and finds
no store failure in the report and both planes' rows in the relay plane.

**RULED + BUILT 2026-09-25 — § The
client-side lifecycle → the pump bullet's wake source (1), the runtime's own
push arm and the `__config` nudge.** Measured 2026-09-25 while lifting
`task-delegation` outcome 7 to web: a pin cleared on web (store-less) landed
on the `__config` blob alone, the nest's untagged `__config` nudge woke
nothing on the tui seat, and the seat read the stale pin for the whole 300 s
backstop (the journey had to poke the pump). Ruled: a blob-only write reaches
a store-backed seat at **nudge latency**; the client owns the mapping, the
nest stays kind-blind. Built: the push→nudge arm moved INTO the runtime
(`AccountRuntimeParams::pushes`, `nudge_scope_for_push` in
`fauna_sync_engine::account_runtime`; `with_session_wakes` in
`fauna-client-account-runtime` attaches it beside the reconnect watch), tui's
and linux's hand-wired arms are gone, and apple/windows/android — which had
**no** push arm at all (the scope tag never crossed the FFI) — and the sync
agent's always-on host gain it with no shell change. The `__config` set name
is one constant (`fauna_protocol::config::CONFIG_SET`) on both ends. Proven:
tier_1 (`nudge_scope_for_push_maps_the_tag_then_the_config_set_then_nothing`,
`the_push_arm_feeds_the_nudge_channel`); tier_3 V1 now converges through the
runtime's own arm and **V16** imports a marker-blind blob write through the
real `__config` push with the backstop disarmed and no pass driven
(`bins/fauna-nest/tests/conformance_account_runtime.rs`); the task-delegation
journey's web leg passes on the nudge alone, its pump poke dropped. The
arm's `__config` half, `CONFIG_SET` and the blob write it carried retired
with the rail on 2026-10-02 ([`config-dissolution.md`](config-dissolution.md)
§ The `__config` dissolution schedule → *The closure order*, step (6)).

**RULED + BUILT 2026-09-22 —
§ The client-side lifecycle → the pump bullet's *Commands and passes*, the
local-write wake and the explicit barrier.** Measured in the 2026-09-22
whole-suite `--app linux` sweep: every command waited for the pass in
flight, a fresh sign-in's prologue is a full catch-up that scaled with the
session account (0.75 s at a quarter of the run, past five minutes at the
worst), and every preference surface read empty and swallowed writes for
all of it; the same wait held linux's main thread for the whole 5 s stop
budget at a sign-out landing mid-prologue, and the addendum's own launch
showed the prologue holding the thread more than four seconds past the
sign-out's grace, uncut — a stretch with no network, which the cut could
not reach. **Built in `fauna_sync_engine::account_runtime`:** `drive_pass`
drives every pass — the prologue, nudge walks, the publish step, the
backstop, reconnect and `reconcile_now` passes — beside the command
channel, serving local commands (`Cmd::is_local`) at the pass's yield
points and parking pass-bound ones; `pass_breath` is the yield every unit
of local work ends in (the page walk, and the escrow-recovery, top-up,
unkeyable and reclamation loops); `put_preference` is
`preference_put::put_preference_local` + the publish step
(`publish_pending`, run at the serve loop's top on every role; it ended in
`bridge_tick` until closure step (5) deleted the bridge, 2026-10-01);
`AccountStoreHandle::settled` is the barrier, and the conformance
tests that leaned on a command round trip proving the prologue take it.
**Proofs (tier_1, red-verified):**
`account_runtime::tests::a_preference_read_answers_while_the_prologue_is_held_on_the_network`
and `…::a_preference_write_lands_while_a_pass_is_held_and_publishes_after_it`
(the prologue held at its first walk for ever; before the driver both
waited on it), `…::settled_answers_only_once_the_pass_in_flight_has_ended`,
`page_walk::tests::every_accepted_page_ends_in_a_breath` and
`generation_escrow_recover::tests::every_generation_considered_ends_in_a_breath`
(polled by hand over an instant feed: one `Pending` per unit, none
otherwise). **Owed:** the whole-suite `--app linux` grade — `test_trained_topics.py`
×5 green, the muted-words ×4 and sync-default reds green or re-attributed. **Not this entry's, stated:** the cost of
the generation machinery's per-row steps under a large account, which is
what the five minutes were made of.

**BUILT 2026-09-24 — every pump-owned
write audited and promoted.** Every `Cmd` variant now states its verdict at
`Cmd::is_local`, and only `reconcile_now`, the barrier, the retirement and
the shutdown remain pass-bound. Local: the read-marker raise, the
observation, the fleet-removal quartet, the group adoption, the endpoint
facts (a per-pass snapshot), the share transfer ledger's persist, and the
tip-sealed door puts while a tip resolves — inside a pass
`serve_local_cmd` asks `AccountStatePlane::origination_mints` first and
parks the put when its door would mint. The custody puts
(`account_state_plane::put_lww_row_local`), the observation and the
`Removed` row take the local write and arm the publish step, which now
publishes the fleet plane beside the delegable one — closing a gap the two
group write-throughs had carried since they took `put_local`: they armed a
step that never sent their plane, so their rows waited for the next full
pass. The pump's own removal reconcile publishes the fleet plane after it
finishes an intent. **Proofs (tier_1):**
`account_runtime::tests::a_read_marker_raise_lands_while_a_pass_is_held_and_publishes_after_it`,
`…::an_observation_lands_while_a_pass_is_held_and_publishes_after_it`,
`…::a_device_removal_runs_whole_while_a_pass_is_held_and_publishes_after_it`,
`…::endpoint_facts_and_a_group_adoption_answer_while_a_pass_is_held`,
`…::the_share_transfer_ledger_persists_while_a_pass_is_held` (`p2p-share`),
`…::a_tip_sealed_put_that_would_mint_waits_for_the_pass_instead`,
`…::a_tip_sealed_put_lands_while_a_pass_is_held_once_a_tip_resolves` (the
fixture's nest stands a trusted escrow holder, so the first put mints and a
tip resolves), and the predicate at the plane,
`group_plane_restart::a_tip_sealed_origination_mints_only_while_no_tip_resolves`.
