# The account runtime's roles — which co-located process runs what — target state

Owns: account-runtime-roles
Status: ratified — the election (W5.1, 2026-08-14), the conversations-engine role lock and the refusal retirement (W5.6, 2026-08-15), the in-process hand-over (2026-08-29), the seed-leg role (2026-10-01), the agent's presence lock and the yield (2026-10-08) and the awaited fleet walk (2026-10-12) are built; split verbatim out of `account-runtime.md` on 2026-10-14
Authority: **which co-located process holds which per-account role** — the engine-singleton election's mechanics (T9), the seed-leg role, the agent's presence lock and the yield, a non-holder's awaited fleet walk to the holder, the conversations-engine role lock (the third exclusive critical section), the in-process hand-over of a role, and the lock's successor (the narrowing of "exclusive-or-die" to the three critical sections). **NOT owned here** — the store contract, the serving lock, local change notification and the own-pump notice, the seedless host, hosting and renewal, the migration/adoption critical section, the top-up self-heal, the path unification and the per-app runtime seat → [`account-runtime.md`](account-runtime.md); the client scoping taxonomy and the per-app concurrent-instance law → [`apps/account-scoping.md`](apps/account-scoping.md); the sync agent's own lifecycle → [`apps/sync-agent.md`](apps/sync-agent.md); the charter and the cross-cutting status → [`account-data-plane.md`](account-data-plane.md). On conflict in those domains, raise it.

Last verified: 2026-10-14 (split verbatim; per-slice build dates are in the status entries below)

Split verbatim out of [`account-runtime.md`](account-runtime.md) on 2026-10-14 — that doc stood at 219,873 B growing about 4.4K B a day, nine days from the 262,144 B whole-file read ceiling, and about two thirds of its week's growth was this family. Its rule bullets and their status entries moved together; a routing stub naming the members remains at each original location; prior history: `git log --follow docs/goal/architecture/account-runtime.md` (and, before 2026-09-06, `account-data-plane.md`). The `W<n>` workstream labels and `R<n>` decision labels used throughout are defined in [`account-data-plane.md`](account-data-plane.md) § Workstreams and § The ratified decisions.

> **Reading this doc.** Its text was carried **verbatim** out of [`account-runtime.md`](account-runtime.md) on 2026-10-14, so an unqualified `§ <name>` citation inside it may name a section that is no longer a sibling on the page. Resolve any such name in [`account-runtime.md`](account-runtime.md) first (the store contract, the serving lock, change notification, the seedless host, hosting, the top-up self-heal, the W3 seat), then in the account data family that doc's own reading note lists. Positional words (“above”, “below”) inside a carried block point within this doc: the boundary-crossing ones were rewritten to name [`account-runtime.md`](account-runtime.md) at the split.

## Section map

- **[Multi-instance concurrency (W5)](#multi-instance-concurrency-w5)** — the election and the per-account roles. **The heading is unchanged from [`account-runtime.md`](account-runtime.md)** (and from `account-data-plane.md` before it), so a `§ Multi-instance concurrency (W5) → *<member>*` citation resolves by swapping the filename. The section's opening, the store contract, the serving lock and change notification stay there.
- **[Implementation status today](#implementation-status-today)** — the election and role entries, newest first, then the W5.1 and W5.6 build-out in their original order.

## Multi-instance concurrency (W5)

- **Election mechanics (T9 — resolved 2026-08-10, refutable until W5
  code): the election IS a kernel-arbitrated advisory lock**, held for the
  role's lifetime: `flock` on `<store dir>/engine.lock` (unix — the sync
  agent's `InstanceLock` precedent, `apps/sync-agent.md` § single-instance),
  a named mutex derived from the store path (windows), the Web Locks API
  keyed by store name (web, among tabs). **Web's lock names, and why the
  conversations-engine role is a SECOND name in the same tab (ruled
  2026-10-01; advisory, refutable
  downstream).** Every web lock is named `fauna.<owner>.<role>/<key>` — the
  owning shared crate, the role as its native lock file names it, then what
  that lock is keyed on: `fauna.account-store.engine/<store name>` and
  `fauna.account-store.seed-legs/<store name>` (this election and the
  seed-leg role, `fauna_account_store::locks_web`),
  `fauna.mls.conversations-engine/<actor-id-hex>` (the third section below,
  keyed on the actor because web has one MLS device leaf per (origin,
  account) and no per-app `mls_state.db` to key on), and the keyless
  origin-wide `fauna.accounts.migrate` (the registry mutation section,
  `fauna_client_accounts`). The conversations-engine role and this election
  are two Web Locks held side by side in the one tab that hosts an
  account's runtime, and that is the uniform shape, not a web deviation:
  natively they are two sections in two files (`engine.lock` in the store
  dir, `mls_state.db.lock` beside the app's MLS database), held in steady
  state by two processes on a desktop (the agent pumps, the app hosts the
  MLS engine). One name was refused twice over: a Web Lock does not
  re-enter, so a single name would refuse this election in the very tab
  that holds the MLS role; and keying the store's election on an app-side
  role would re-key a shared-Rust section on a per-app concept every other
  host keeps apart. Which of the two an erase asks, and why one probe is
  enough, is [`apps/account-scoping.md`](apps/account-scoping.md)
  § Concurrent instances → *Web owes the same refusal through the lock it
  already has*. Kernel arbitration is the whole
  design: no probe window, automatic crash release, no leases, heartbeats,
  or election protocol. Priority was ruled behavioral, not protocol — on
  desktops the apps *ensure the agent* (the shipped provisioning convergence
  loop already spawns and probes it) rather than take the role themselves,
  so the agent would hold the lock in steady state — **and that premise
  fell 2026-10-08**: every built desktop host starts its own runtime at the
  post-auth hook that provisions the agent, the agent mounts seconds later,
  so the app's runtime usually won and kept the role until it exited. The
  agent's priority is a mechanism since that ruling — *The agent holds the
  role when present*, below. Where no agent exists (mobile, web, an
  agent-less box) the first process to take the lock is the singleton,
  releasing on exit. **One per-kind carve-out:** the singleton
  drains outbox intents whose seal needs only store-held keys; MLS-sealed
  intents drain from a process hosting the conversations engine — an app,
  never the bearer-only agent (detail owner:
  [`../behavior/devices.md`](../behavior/devices.md) § Offline compose).
- **The seed-leg role: a leg only a signed-in app can run is elected among the seed holders, never by the engine role (ruled 2026-10-01; built 2026-10-01 — § Implementation status today).** The engine role elects who pumps the bound nest, and on a desktop the process that wins it in steady state holds no seed. Three steps of the pass need what only a signed-in app holds — the identity seed, or the owner session it opens: the enrollment registration ([`account-replica-posture.md`](account-replica-posture.md) § The store device principal — registering needs the owner session), escrow recovery ([`account-data-taxonomy.md`](account-data-taxonomy.md) § The generation machinery → *Escrow recovery*), and the secondary leg with the custody arm that rides it ([`account-sync-plane.md`](account-sync-plane.md) § The bind leg, ruling 4). Each was ruled "skipped on the seedless host, healed by a signed-in app's pass on the same machine", and read 2026-10-01 that pass does not exist on a desktop whose agent is up: the app beside the agent is a non-holder and runs no pass. So there a linked nest is neither completed nor custodied (measured 2026-10-01, the two-box recovery journey's first run), a key held only in escrow is not recovered, and a machine bound to a rebuilt box is not registered there again (the first read from the code, not measured; the second shown by its conformance case's red run). A phone, a browser tab and a desktop app that holds the engine role were never affected. A user cannot see the election — the own-pump bullet in [`account-runtime.md`](account-runtime.md) states that for repaints — so the rule is the same rule: **what a signed-in device does for its account does not depend on which co-located process pumps.** Six parts. **(1) A second role.** Exactly one co-located seed-holding runtime per store holds the **seed-leg role**, by a second kernel-arbitrated lock with the election's own T9 mechanics — `seed-legs.lock` beside `engine.lock` in the store dir, a second Web Locks name on web — the store's third exclusive section. Only seed-holding runtimes contend; a seedless runtime never takes it. It is tried at assembly right behind the engine election and tried again wherever that election is (the backstop tick, an explicit `reconcile_now`), with the same degrade posture: open at start, closed on a later try. It carries no priority: the first seed holder to take it keeps it until it exits. **(2) Who runs the seed-only steps.** The holder of both roles runs them inside its pass, exactly as built — every phone, every tab, a desktop with no agent. A seed-leg holder that is not the engine holder — the desktop app beside its agent — runs the **seed pass**: the seed-only steps alone, in pass order, at every wake on which an engine holder runs a full pass (the first after assembly, the backstop tick, a reconnect, `reconcile_now`), never per nudge — and once more when the fleet scope's first listing lands after a first seed pass that ran on a store that had never listed it (a sign-in beside the agent, on a fresh or erased store: that pass's escrow recovery had no receipt or mint row to ask on), the listed fact re-read on the grant latch's poll until then. An engine holder without the seed-leg role — the seedless agent, or a seed holder beside another that took the role first — skips them. A step added later that needs the seed or the owner session joins the seed pass in the change that adds it. **(3) What the seed pass may write: nothing the engine role makes exclusive.** It walks and reconciles no bound plane, sends and banks no watermark, drains no outbox, clears nothing and settles no replica. It writes the store as a non-holder already does — rows through the bound planes' writer door (a receipt, a custody row), published by its own publish step — and through linked planes, which by construction leave every memory about the bound nest alone ([`account-sync-plane.md`](account-sync-plane.md) § The bind leg, ruling 4). The credential slot's registration latch is written by whichever process registered, as today. **(4) Each step beside a seedless holder.** *Enrollment:* the seed pass asks the bound nest which replica it is over the owner session, compares the answer with the store's settled replica, and runs the registration — with the latch void when the two differ or the device handshake voided it. Void once per replica: the seed pass settles nothing, so the two keep differing until the engine holder's pass settles, and once a seed pass has registered at a replica the latch is that replica's. The watermark clear, the holdings re-arm and the settling stay the engine holder's, whose own register probe finds the grant on the nest at its next pass. A removed-from-account answer reassembles the seed-holding runtime exactly as it does in a pass. *Escrow recovery:* a recovered key lands in the shared slot's retained bundle, where the holder reads it; the seed pass reads for itself the ancestry recovery admits; the rows the key opens are re-presented by the holder's next full reconcile, which the seed-holding runtime's host asks for as soon as a seed pass has keyed one (the `EngineHolderNudge` seat the yield of *The agent holds the role when present* part 4 uses), so beside a seedless holder they are readable within that pass; with no seat, within one backstop interval of the recovery. *The restore gesture* ([`../ui/nests.md`](../ui/nests.md) § Trust facet — restore from a custodian nest → *After a restore*; ruled 2026-10-17) makes the same ask with the same seat on a *restored* outcome — the rows it brought back, keyed by it, reach the bound nest through the engine holder's next full pass alone — and when the runtime that ran it holds the engine itself it runs that pass at once instead (an immediate backstop tick), so a restore never waits a backstop interval to publish. *The secondary leg:* unchanged, over the app's own connector; the retires it mirrors are read from the store ([`account-sync-plane.md`](account-sync-plane.md) § The bind leg, ruling 5). **(5) Reports, counters, the notice.** `reconcile_now` on such a runtime still answers `skipped_non_holder`, with the seed pass's own slots filled. The seed pass moves no pass counter — "the pump has run" stays the engine holder's statement — and it is a run of the pump's funnel for the change generation of that bullet, because it can merge an entry a read answers. It is driven beside the command channel and cut by a sign-out like any pass. **(6) Refused.** *The app takes the engine role from the agent while it lives.* It moves the bound nest's pump, the outbox drain and the peer leg's listener between processes at every app launch and quit: each handover is a catch-up pass and a re-bound endpoint every sibling then walks. That spends the steady state R2 chose, one always-on engine with thin apps beside it, to fix legs that need none of it. (The presence signal and the yield ruled 2026-10-08 — *The agent holds the role when present*, below — run the other way only: an app yields to the agent, so the role moves once per agent start, never per app launch or quit.) *The agent is given the means to run them.* Every door the secondary leg uses admits the authenticated account, and a seedless process has a device principal registered at the bound nest alone: reaching a linked nest from the agent means an enrollment, a device row and a revocation path at every linked nest, or an owner session lent to a process that holds no seed, which ends the agent's device-principal-only posture ([`apps/sync-agent-credentials.md`](apps/sync-agent-credentials.md) § Credential model). Neither helps escrow recovery, which needs the seed itself. The cost, stated: these legs run only while a signed-in app runs on the machine — for a linked nest, ruling 4's own bound. *Every seed-holding non-holder runs them, with no election.* Two apps on one account would push the same rows verbatim to one linked nest, and the loser's refused push retires a copy that is live on both nests ([`account-sync-plane.md`](account-sync-plane.md) § The bind leg, ruling 1(c)); a tab beside a seed-holding holder tab would repeat that tab's leg for nothing.
- **The agent holds the role when present — the presence lock and the yield (ruled 2026-10-08; all five parts built the same day — § Implementation status today).** The store contract's "the agent when present (R2), else an elected app instance" had no mechanism: the election is first-come, and *present* was told by nothing. T9 made the agent's priority behavioral — the apps *ensure the agent* rather than take the role — and the built hosts refute the premise: every desktop app starts its own runtime at the same post-auth hook that provisions the agent (tui's `session.rs`, linux's `account_runtime.rs`, the `fauna-ffi` seat), the agent's stint mounts seconds later, so the app's runtime usually wins and keeps the role until it exits — measured 2026-08-18 as the holder varying between runs of one suite, and since 2026-10-08 it decides whether same-account file bodies cross the peer leg at all, because the file engines, and with them `PeerLegBinding::file_sync`, live only in the agent ([`../behavior/p2p.md`](../behavior/p2p.md) § Implementation status today). A user cannot see the election, so the rule is the rule the own-pump bullet in [`account-runtime.md`](account-runtime.md) and the seed-leg bullet above state for repaints and for the seed-only legs: **on a machine whose agent hosts the store, the agent holds the engine role whichever process started first, and an app holds it only while no agent does.** Five parts. **(1) Presence is a fourth store lock, told by the kernel.** The agent's mount takes `<store dir>/agent.lock` (`fauna_account_store::locks::AgentPresenceLock`) **exclusive and blocking**, before its engine election and for the mount's lifetime — the serving lock's shape with the sides mirrored. The wait is bounded by construction: the only other takers are the apps' probes, each a shared try held for the length of one `try_lock_shared` and dropped, and the agent is single-instance per user, so no second exclusive taker exists; a try here would read a probe's instant as absence until the next recheck, the race the lock exists to close. Who takes it and who probes it follows the principal, as the seed-leg role does: a seedless runtime — the agent, W5.5a — takes it and never probes; a seed-holding runtime — every app — probes it and never takes it. Web has no agent and its election answers *absent* without asking; a phone's probe finds the file free and the first-come election stands there unchanged. Degrade is the election's own: an agent whose acquire fails mounts anyway (the lock states priority; `engine.lock` keeps exclusivity, and the apps then see today's first-come behaviour), and an app whose probe fails reads *absent* — open, a lone app must pump. **(2) An app contends only while no agent is present.** A seed-holding runtime's election probes first, at assembly and wherever the election is re-tried (the backstop tick, `reconcile_now`): present → it stays, or becomes, a non-holder with no try at all; absent → the try as built. **(3) An app holder yields.** A seed-holding holder re-reads presence between passes — on a poll with the grant latch's own shape (`GRANT_LATCH_POLL`, 500 ms doubling) and the backstop interval as its ceiling, re-armed from the floor whenever the role is taken, so an agent that mounts seconds after the app's sign-in is noticed within seconds and a holder beside no agent soon re-reads only on the tick it already has — and at the tick and on `reconcile_now`. Present → the holder **stands down, in this order:** the host's legs come down first — the peer node, its serve side and transport, the sibling channels and file-sync hooks, the custody leg's holder state (`HostLegs` gains the stand-down; `NoLegs` answers it with nothing) — then `engine.lock` is released, then the role fact flips (`PumpCycles`; `skipped_non_holder` on the next `reconcile_now`, and a `yielded` slot on the report of a `reconcile_now` that did it — a yield on the poll or the tick shows in the role fact alone). The order is the W5.7 invariant: the machine's one NodeId is never an endpoint in two processes, so the yielding listener is gone before the taker can bind. Nothing mid-pass is cut — every re-read is a wake between passes. The seed-leg role is untouched: the app that yielded is a seed-leg holder beside a seedless engine holder and runs its seed pass, the built shape. A degraded re-read keeps the role (a degrade is never a yield), and the role is never *taken* from a live holder — `engine.lock` stays the one arbiter; the yield only hands it back. **(4) The agent takes it promptly, and from nobody.** The yielded role is free; the agent's runtime, a non-holder, takes it at its tick or on its `reconcile_now`, and the yielding side asks for that now: the handle exposes the role as a watch, and the shared host layer (`fauna_client_account_runtime`, in the start every desktop host already runs) asks the agent for its pass on each holder→non-holder edge through the `EngineHolderNudge` seat the participation switch already publishes (`ReconcileAccountRuntime`, whose arm runs the pass on a non-holder too — a non-holder's `reconcile_now` is the re-election, and the kernel arbitrates). No seat, or an agent not yet answering → its tick, as today. When the agent exits or crashes the kernel drops both its locks; the app's next re-read finds presence absent, its election contends and wins, and it pumps — the resilience W5.1 built, unchanged; when the agent returns, part (3) hands the role back. So the handover count is bounded by agent starts, never by app launches and quits — which is why this is not the alternative the seed-leg bullet's part (6) refuses: that one moves the pump on every app launch and quit; this one moves it once, onto the process R2 chose to keep it. **(5) What stays.** tui keeps its in-process peer-transport factory: it is invoked only by a holder, and under this rule an app holder exists only on a box with no agent, where the leg still carries the account plane's device↔device convergence (the journeys that read the holder's leg read it there) and `file_sync: None` is honest — no engine on such a box holds file bodies; whether the other desktop apps take the dep stays the W5.7 trickle-down question. **Refused:** *app runtimes never contend while an agent is provisioned* — provisioning is a hope, not a fact; a refused-slot agent mounts nothing, and the account would never pump. *The peer leg follows the file engines rather than the election* — a second holder of the NodeId, exactly what W5.7's factory refinement forbids, and "the engine-singleton speaks for its store" re-opened. *The yield as a reassembly* — the heal path rebuilds an identity that did not change and releases the seed-leg role for nothing. *An agent→app push* — the agent's event stream reaches no app's runtime today, the poll's first intervals are the latency a push would buy, and a push stays additive if one is ever wanted. **Proofs the build owes:** the lock's exclusion pins beside `EngineLock`'s; `conformance_account_runtime` in V8's shape — an app runtime that started first yields to an agent runtime that starts second, and takes the role back when that agent exits, roles read as report facts; the listener-before-lock order over the in-memory transport; the nudge on the yield's edge beside `p2p_participation_nudge.rs`.
- **A non-holder's awaited fleet walk reaches the holder (ruled 2026-10-12; built the same day — § Implementation status today).** A non-holder runtime asked for a walk it must await — today the mail refresh's `walk_fleet`, the stale-sibling rule's barrier ([`../behavior/mail-credentials.md`](../behavior/mail-credentials.md) § Hard revoke → *Published before the nest moves*, rule (4)) — does not answer "another process on this device holds the engine role" and stop: it asks the holder to walk the fleet scope now, on its behalf, awaits that walk's outcome, and then reads the walked rows from the store the two share. This is the evolution the conversations bullet below declares — non-holders *reaching the holder* over IPC — and leaves the single-writer contract untouched: the holder walks, the non-holder only reads. **Why (measured 2026-10-12 on the tier_3 stale-sibling journey):** on every desktop in steady state the agent holds the role, and a reconnect reconnects the agent too, so a rule that leaves the walk to "the next connect" takes the walk leg on no desktop at all — a laptop's next connect is its next wake, and until then a nest beside its walked custody stays unhealed. **The door is `AccountStoreHandle::walk_fleet` itself**, so every `MailStore::walk` consumer gets the reach with no app glue: a pass that reports `skipped_non_holder` hands the walk to a holder reach the host seats — the shared host layer (`fauna_client_account_runtime`) seats it over the `EngineHolderNudge` seat the yield (part 4 above) and the participation switch already drive, which gains an awaited walk beside its nudge; web (no agent) seats nothing and a phone publishes no agent into the seat it shares with the desktops, so neither reaches a holder — and there the app is the holder anyway. **The holder's arm is awaited too:** a new agent verb (`RequestMethod::WalkFleetNow`), invoke-and-reply like `ApplyHeldDeletes` and never a silent drop, runs the agent runtime's own `walk_fleet` — `Prompt::Awaited`, so the e2e fleet-walk hold spares it as it spares every awaited leg — and answers whether the walk landed and, if not, why, within a bound stated once beside the verb (the app's request waits past it, so the reply it reads is the agent's verdict); a verb that answers nothing in the bound, a seat with no agent answering, or a holder whose walk failed leave `walk_fleet` an `Err` naming it, and the caller's backstop — the next connect — stands as before. **Refused:** *wait for the holder's own pass to move the custody* — a sleep on the holder's timing (its reconnect pass, which under a hold never comes), the shape the runtime's own nudges exist to avoid where a poke is possible; *"next connect" as the rule* — the measurement above; *the holder re-seals too* — the refresh needs the app's nest session and the seed-leg posture, and the agent is seedless: it walks, the app decides. **Proofs the build owes:** `conformance_account_runtime` — a non-holder's `walk_fleet` beside a holder that serves it answers `Ok` and reads the walked rows, and beside no seat answers the role refusal as today; a pipe test of the verb's reply and bound; and the tier_3 journey tightened to require the re-seal on every run.
- **The conversations-engine role is the THIRD exclusive critical section
  (ruled 2026-08-15 — the ruling that closed the W5.6 inventory gate).**
  MLS exclusivity is *permanent*, not a transitional shim: two engines
  advancing one group's epochs fork the ratchet, so the state diverges even
  when every SQLite write is transactional — no WAL/transaction discipline
  ever removes it, which is why `mls_state.db` (class 5, un-re-syncable)
  gets a **role lock**, not a store upgrade. The process hosting a live MLS
  engine over a given `mls_state.db` holds a kernel-arbitrated advisory
  lock with exactly the election's T9 mechanics — flock (unix), the
  `LockFileEx` family (windows), Web Locks (web); no probe window,
  automatic crash release — acquired in the **shared engine-construction
  path** (one primitive, never seven app call sites:
  `fauna_mls::storage::SqliteStorage::open`, which every file-backed
  `MlsEngine::new` runs through) before the `mls_state.db` open, held for
  the engine's lifetime. **The guard lives beside the state it guards, and
  is keyed to the file it covers**: the lock file is the guarded database's
  own name + `.lock` (`mls_state.db` → `mls_state.db.lock`, a sibling in
  each app's account-scoped dir today), so instances of the *same* app on
  one account contend while different apps — separate `mls_state.db`
  files, each its own MLS device today — do not, and the keying survives
  any future move of the file. *(Build refinement 2026-08-15, W5.6 — the
  ruling's fixed sibling name `mls.lock` was refuted at build time: a fixed
  name keys the guard to the directory, so any layout with two MLS
  databases in one directory — a `NamedTempFile` test's shared OS temp dir
  above all — would contend on one machine-global lock; deriving from the
  guarded file's own name keeps the beside-the-state placement while fixing
  the keying. Pinned by
  `two_databases_in_one_directory_do_not_contend`.)* A non-holder
  instance's conversations surface refuses honestly ("served in another
  instance", the `error-message` conventions; the typed verdict is
  `fauna_mls::MlsError::ServedElsewhere` over the storage layer's
  `StateServedElsewhere`) — never a silent `SQLITE_BUSY` — and because the
  holder is by construction the (app, account)'s one conversations engine,
  the declared evolution is non-holders *reaching the holder* over IPC
  (conversations rendering in every instance) without the guard changing
  shape — the first such non-holder is the cli's device principal, reaching
  the holder as the conversations oracle's custodian
  ([`../behavior/conversations-oracle.md`](../behavior/conversations-oracle.md),
  ruled 2026-10-17). This also makes the drain carve-out well-defined under
  concurrency: "a process hosting the conversations engine" (the
  MLS-sealed-intent drain,
  [`../behavior/devices.md`](../behavior/devices.md) § Offline compose)
  is the role-lock holder. A lock-file I/O failure **fails closed** —
  deliberately the opposite of the instance lock's degrade-open posture,
  because this state is class 5: a directory that cannot host the
  zero-byte lock file cannot host the SQLite journal either, so degrading
  open would trade a near-impossible availability corner for a silent
  ratchet fork. **Deliberate tripwire, do not "fix":** `mls_state.db`
  stays non-WAL with `busy_timeout` 0 — both halves now explicit and
  test-pinned at `SqliteStorage::open` (the W5.6 build found the ruling's
  "opens it with neither pragma" premise half-wrong: the driver defaults
  `busy_timeout` to 5 s, so the ruled 0 is now *set*, not assumed) — so a
  guard regression's first symptom is a hard, immediate `SQLITE_BUSY`
  write error, not silent interleaving and not a five-second stall.
- **The role is HANDED OVER in-process, never waited out (2026-08-29).**
  "Held for the engine's lifetime" is exactly right between *instances* —
  RAII, crash release, no protocol — and was wrong within one. A single
  instance rebuilds its conversations session for the same account on every
  re-login, account switch and factory-reset re-onboard, and it does so
  **build-first**: the successor engine is constructed, and only then swapped
  in over the predecessor. So the successor asked for a lock its own
  predecessor still held and was refused `ServedElsewhere` — a message about
  *another instance* that was, here, a lie about itself. The refusal is not
  cosmetic: a refused build leaves the account with no conversations rail at
  all, degrading silently wherever the shell treats the build as best-effort.
  **The rule: a session factory RELEASES BEFORE IT BUILDS.** Waiting for the
  predecessor's last reference to drop is explicitly *not* the mechanism —
  those references live in three languages at once (an app-shell field, a
  host singleton, a stashed FFI handle), and a release that depends on
  counting them across an FFI boundary is a race dressed as a lifetime. So
  the hand-over is explicit and ordered: `MlsEngine::retire` flushes the
  provider snapshot, releases the role lock, and **poisons the retired
  handle** — every later statement on it fails typed
  (`storage::MlsStateRetired`), the same fail-loud posture as the
  `busy_timeout` 0 tripwire beside it. Poisoning is what keeps this a
  hand-over rather than a relaxation: after a retire there is still exactly
  one handle that may touch the database, and the predecessor is provably
  not it. ⚠ **The poison alone was not enough, and the gap is worth stating
  because it is the natural one to leave (closed 2026-08-29).** The hazard this
  exclusivity exists for is *epochs* — two engines advancing one group's ratchet
  — and epochs do not live in SQLite. They live in the in-memory openMLS
  provider, which `encrypt` and `decrypt` mutate without touching storage at
  all, so a retired engine went on sealing valid ciphertext under the account's
  own leaf and originating Commits, persisting none of it (measured: 281 bytes
  of valid application ciphertext from a retired engine). A live sealer whose
  writes are *guaranteed* to fail is the ratchet-fork condition itself. So
  `retire` now quiesces the **group** as well as the store:
  `MlsEngine::is_retired` gates every method that mutates group state or emits
  bytes another member acts on — seal, decrypt, commit origination and
  application, key-package minting — each refusing typed `MlsError::Retired`.
  Read-only accessors stay open on purpose: the hand-over is graceful precisely
  so teardown can still ask what the engine knew. ⚠ **And the decrypt refusal
  had to be paired with a walk-level refusal, or it would have been worse than
  the ghost it replaced.** The inbound walks advance their cursor past a record
  *before* decrypting it and skip decrypt failures with `continue` (correctly —
  a sender cannot decrypt its own posts), and the session writes that cursor to
  the durable cross-device watermark. A retired engine that merely failed each
  decrypt would therefore skip every record it walked, permanently, on every
  device that later resumes from that watermark. `poll_inbound_conv` /
  `_scheduling` / `_folder` refuse at entry instead, before the cursor moves,
  leaving the records on the nest log for the successor — which holds the
  retire-point snapshot and can still decrypt them. **Declared non-goal:**
  `export_provider_storage` stays open, a pure read whose only write is the
  CAS-protected nest replica save, which fails safe against the successor's
  base. Exclusivity **between processes is untouched** — another
  instance's engine is unreachable from this one, keeps its lock, and is
  still refused honestly, which is the case the refusal was written for. The
  seam is `ConversationsManager::retire_conversations_engine` (drop the
  `Rail::FaunaMls` registration, `RailBackend::retire` on the way out),
  called by the shared native factory
  (`fauna_ffi::FfiNestClient::conversations_session*`) before
  `MlsEngine::new`, so macOS / iOS / windows / android inherit it with no
  glue of their own. Measured on macOS as an order-dependent e2e failure —
  the second real-conversations test in a module could not activate, either
  test passed alone — whose user-visible twin is an A → B → A account switch
  finding A's store held by A's own predecessor. ⚠ **The two Rust apps do not
  reach that factory, and were left build-first because of it (closed
  2026-08-31).** linux and tui construct their own `ConversationsManager` and
  call `MlsEngine::new` directly, so "every native app hands the role over" was
  true of the four UniFFI apps and false of the other two — and on linux the gap
  is the worse one, because its manager is a process-lifetime singleton and
  neither sign-out nor an account switch drops the rail
  (`clear_for_identity_change` deliberately preserves backends). Every
  same-account re-login there — sign out and back in, a factory-reset
  re-onboard, a driver replaying a login the launch flow had already restored —
  asked for a lock its own predecessor held, took the `ServedElsewhere` arm, and
  left the conversations page blank over a live engine. Both apps now run the
  same release before they build: linux at
  `conversations::conv_backend::build_session_engine` (the one factory `app.rs`'s
  `AuthSuccess` arm calls), tui at `start_with_db`; pinned in each app by
  `a_same_account_relogin_gets_the_conversations_role_back`. The *door* is a
  second, weaker guard and both apps now spell it the same way: an authenticated
  `set_state` patch naming the live actor **and** nest converges instead of
  re-establishing (tui's `session::apply_session_patch`, linux's
  `session_patch_switches_session` — which had compared the secret alone, so a
  same-actor-different-nest patch neither switched nor rebuilt and the app went
  on serving the old nest). The door cannot replace the release: a sign-out
  leaves no live session to compare against, and two logins racing at launch are
  both invisible to it. ⚠ **And the hand-over has to
  end the predecessor's RECEIVE LOOP too, because the success path is the only
  path a shell installs a successor on (closed 2026-08-30).** Every shell that
  reaches this seam assigns its session field only after the factory returns a
  value — apple's `ConversationsVM.activate` as its first statement, windows'
  `_liveConvSession` inside `if (convSession is not null)` — both inside a
  best-effort `try` whose catch arm only logs, because a failed build is
  deliberately not fatal to the launch. So on the failure path *nothing replaces
  the predecessor*, and a receive loop that exits only when its session is
  dropped is a loop that never exits: one leaked task per failed build, each
  holding `backend` and `manager` strongly for the life of the process, and each
  one an account-scoped writer that outlived the state it serves
  ([`apps/account-scoping.md`](apps/account-scoping.md) § The scoping taxonomy,
  corollary 2). Waiting for the drop is the same reference-counting the rest of
  this ruling refuses, so the loop gets a **second exit** on the same footing as
  the session-closed one: `RailBackend::retire` fires a retirement event
  (`FaunaMlsBackend`'s `EngineRetired`, the awaitable twin of the engine's
  `is_retired` flag) and the loop `select!`s on it beside `SessionClosed` — an
  event, never a tick, the same law the 2026-08-27 move off the polled
  `Weak<()>` set. It fires from the retire, so it does not care whether the
  successor build then succeeds or throws; on the success path it simply beats
  the drop that used to do the work. **Declared non-goal:** the shell's stale
  *reference* is untouched — no Rust code can reach an app-shell field, which is
  why the loop, not the reference, is what the hand-over is made to guarantee.
  Pinned by `receive_cycle_poke_tests.rs`
  `retiring_the_engine_ends_the_loop_a_failed_build_left_installed`, which holds
  the session live across the whole assertion so the drop cannot be what passes
  it, and reds for the full budget against the session-closed arm alone.
  ⚠ **The quiesce is per-door BY DECISION, and the list is kept by a test rather
  than by audit (ruled 2026-08-30).** There is no single seam to put it behind:
  the mutating doors are distinct openMLS operations with different signatures,
  so "gates every method that mutates group state" is a universal enforced by
  fourteen-odd individual `ensure_live()?` calls — and a universal kept by hand
  is only ever as good as its last audit. This one was short **twice**. The
  costly miss was `join_from_welcome` and `join_from_welcome_bytes`: a Welcome
  join mutates only the *in-memory* provider, so the retire's storage poison
  never reached it and `persist_group` swallows both of its failures as `warn!`
  — the retired engine joined, held the group live, and persisted nothing. And
  because the durable inbox reads a successful apply as *"durably applied — ack
  it"* (`fauna-client-inbox`'s `apply_one`), the nest then **dropped the durable
  row**: the successor never received the Welcome, it cannot be replayed, and
  the user is left a member of a group no engine of theirs can open — a
  [`nest/common.md`](nest/common.md) § Client-state recoverability breach rather
  than a ghost. So the refusal must reach that layer as an `Err`; an
  `is_retired` check that returned `Ok(())` would ack-and-drop, strictly worse
  than the bug it replaced. `forget_group` was short on the same terms (it
  half-applied — the in-memory removal landed, then the poisoned store refused
  the durable one), and the whole-KV `restore_from_provider_storage` with it,
  which is why the restore chokepoint `ProviderReplica::restore_into` now
  carries **two** refusals: the seating verdict for
  [`../behavior/succession-aftermath.md`](../behavior/succession-aftermath.md)
  rule (1), and a typed `Err` for this contract. **What keeps the list complete
  is `every_group_state_mutating_door_is_quiesce_guarded`**, which re-derives
  the census from `engine.rs`'s own source on every run and fails when a new
  `&self` method reaches the group map, an openMLS group constructor or the raw
  KV swap without `ensure_live()` — so the next door cannot be missed silently,
  and an exemption has to be written down as one. The tell worth keeping: the
  three group-**creating** doors were never enumerated as a class, which is how
  two of them were guarded and the third was not. **Declared out of the class:**
  Fauna's own side-band memos in the provider KV (channel-kind labels,
  welcome-sender records, pending-commit hashes) — no other member reads them
  and they cannot fork an epoch.
  ⚠ **"Inherits it with no glue of its own" holds only for a shell that keeps ONE
  manager across the hand-over — and windows does not (closed 2026-09-02).** The
  factory can only retire the manager it is *handed*, and windows replaces its
  process-wide `ConversationsManagerHost.Instance` at every actor change (its
  sanctioned exception to the no-swap rule: the outgoing identity's rails,
  observers and threads must not survive a switch), **before** the successor
  build. So the factory's retire ran against a brand-new manager with no
  `Rail::FaunaMls` registered, took its documented no-op arm, and the
  predecessor engine was left to precisely the reference drop this ruling
  refuses — the shell's `Dispose()` releasing one `Arc` while the departing
  `FfiNestClient`'s `scheduling_session` stash, an in-flight build closed on a
  continuation, and a winding-down receive loop held the rest. Windows now calls
  `retire_conversations_engine` — exported over UniFFI for exactly this — on the
  OUTGOING manager before dropping it, which is the same explicit ordered
  hand-over moved to the one seam the factory cannot see, not a second
  mechanism. The generalization worth carrying: **a shell that drops or swaps
  its manager, or never hands the factory a manager at all, owes the retire
  itself**; only a shell that keeps ONE manager across the hand-over *and*
  hands it to the manager-taking factory arm is covered by the factory alone.
  ⚠ **Android is the third case, and a narrower one than windows' swap
  (closed 2026-09-12).** `ConversationsManagerHost.startConversationsSession`
  (`apps/fauna-android/app/src/main/java/com/fauna/app/core/conversations/ConversationsManagerHost.kt:225-231`)
  calls the FRESH-MANAGER `conversationsSession(...)` factory arm, never
  `conversations_session_over_manager` — so `build_conversations_session`'s
  `manager` parameter is `None` on every android build, and its retire-on-
  handoff guard (`if let Some(m) = &manager`) never runs at all, for ANY
  manager, regardless of what android's shell does with its own field. That is
  a stricter miss than windows': windows' swapped-in manager at least
  *receives* the factory's call and takes its documented no-op arm because it
  carries no `Rail::FaunaMls`; android's factory call never happens in the
  first place. Teardown
  (`ConversationsManagerHost.kt:258-268`) was pure reference-dropping —
  cancel the loop, `close()` the session, null `sessionManager` — exactly the
  hazard this ruling refuses. Android now calls `retireConversationsEngine()`
  on the outgoing `sessionManager` in `stopConversationsSession`, before
  nulling it, the android twin of windows' `ResetForActorChange` call; pinned
  offline by
  `receive_cycle_poke_tests.rs::a_fresh_manager_factory_build_hands_nothing_over_only_the_shells_own_retire_does`. ⚠ **And windows' erase-time release does not
  cover this seam, which is why the retire above is not redundant beside it.**
  `FfiNestClient::release_account_scoped_stores`
  ([`apps/account-scoping.md`](apps/account-scoping.md) § Erasure follows
  scope) reaches the same manager through the client's `scheduling_session`
  stash, but windows calls it from `ClearCredentialNamespace`, which reads
  `_rpcClient` — and the account-switch and factory-reset paths run
  `DisposeNestClients()` first, which nulls that field. So on exactly the
  paths that swap the manager, the erase-time release finds no client and the
  swap-seam retire is the only hand-over that runs; on sign-out, which does
  not pre-null it, both do and the retire is idempotent.
- **The lock's successor (inventory RULED 2026-08-15 — the gate below is
  lifted).** "Exclusive-or-die" narrows to the **three** critical sections
  (the *Store contract* bullet's two, in [`account-runtime.md`](account-runtime.md), and the conversations-engine role above); the per-account raise channel survives as UX (raise is still the
  right plain-launch behavior); the launch-collision refusal path dies —
  under one load-bearing ordering, the same law as "the election lands
  before the refusal dies": **every shipped conversations engine acquires
  `mls.lock` before any app's refusal retires.** Bearer-only processes
  hosting no MLS engine are sanctioned same-account co-residents already —
  the R2 agent builds its engines `mls: None` and never takes the account
  instance lock — while the app legs retire tui-first under W5.6.
  **Executed 2026-08-15: the re-ratification is recorded in
  account-scoping.md § Concurrent instances** (per-app `ServingMode`; tui
  retired first, the trickle-down rows), and the ordering
  held — the role lock landed in the shared engine-construction path before
  tui's refusal died. The per-store inventory that gated this narrowing
  (every non-multi-process-safe account-scoped store needs a named answer)
  is closed: dispositions in the ruled Gap entry dated 2026-08-15 in
  § Implementation status today.

## Implementation status today

*The entries below were carried verbatim out of [`account-runtime.md`](account-runtime.md) § Implementation status today on 2026-10-14; that doc keeps the rest of the W5/W6 build-out and the W3 seat.*

- **BUILT 2026-10-12 — § Multi-instance concurrency → *A non-holder's awaited fleet walk reaches the holder*.** The door is `AccountStoreHandle::walk_fleet` (`fauna-account-plane`): a pass reporting `skipped_non_holder` hands the walk to the `HolderWalk` its host seated on the handle (`seat_holder_walk`; a per-runtime `HolderWalkSeat` shared by every clone), logs `the engine holder walked the fleet scope for this non-holder` when it lands, and with no seat answers the role refusal as before. The shared host layer's `resolve_and_start` seats the process-global `EngineHolderNudgeSlot::seat()` itself — the slot is the `HolderWalk`, reading the agent provisioner the host publishes into it later and none once it is gone — so tui, linux and the `fauna-ffi` seat (macOS, windows) get the reach with no app glue, and a process that never publishes a provisioner (the phones) reaches nothing. `EngineHolderNudge` gained `walk_fleet_for_non_holder`, and the `SyncAgentProvisioner` impl sends `fauna_ipc::sync::RequestMethod::WalkFleetNow`, waiting `WALK_FLEET_NOW_REQUEST_TIMEOUT` (35 s); the agent (`pipe_server::walk_fleet_now_within`) runs its mounted handle's own `walk_fleet` within `WALK_FLEET_NOW_BOUND` (30 s) and replies `ResponsePayload::FleetWalked(FleetWalkOutcome { walked, reason })` — no store mounted, a failed walk, or a walk outliving the bound answer `walked: false` with the reason, the pass running on. Witnesses: `conformance_account_runtime.rs` `a_non_holders_walk_fleet_is_walked_by_the_seated_holder` (no seat → the role refusal; a failing reach → its reason; a walking reach → `Ok` and the non-holder reads the walked row), the agent's `walk_fleet_now_*` pipe tests, and tier_3 `test_mail_stale_sibling_after_key_rotation.py`, which now requires the re-seal on every run.
- **BUILT 2026-10-08 — § Multi-instance concurrency → *The agent holds the role when present — the presence lock and the yield*, parts 1–3 and 5; part 4's prompt handover built the same day.** The lock is `fauna_account_store::locks::AgentPresenceLock` on `<store dir>/agent.lock` (exclusive blocking `acquire`, shared-try `is_present`); the election seam asks it through `EngineElection::agent_present` (`FileElection` probes the file, `WebElection` answers absent); the driver probes before every seed-holding try (`elect_at_start`, the tick, `reconcile_now`), re-reads presence while a seed-holding runtime holds the role (`Wake::PresencePoll`, the grant latch's floor doubling to the backstop interval, and at the tick and on `reconcile_now`), and yields legs first — `HostLegs::stand_down`, natively `peer_leg::stand_down` awaiting `PeerNode::shutdown` before the leg's state drops, and the custody leg's held stores — then the lock, then `PumpCycles` (`PumpReport::yielded` on a `reconcile_now` that did it). The agent's `account_host::assemble` takes the presence lock before `AccountStoreRuntime::start` and its `Mounted` drops it after the runtime's shutdown. Pins: `locks::tests` (`an_agent_holder_is_seen_until_it_drops`, `the_agent_acquire_waits_out_a_probe_instead_of_refusing`, `the_agent_lock_is_the_reserved_file_independent_and_never_deleted`); `conformance_account_runtime::an_app_runtime_yields_the_role_to_an_agent_that_mounts_beside_it_and_takes_it_back_when_it_exits` (red with the native probe answering absent); `p2p_participation_gate::a_yield_to_the_agent_takes_the_listener_down_before_it_releases_the_role` (red with the lock released ahead of the stand-down); `account_runtime::tests::a_seed_holding_runtime_never_takes_the_agent_lock_and_a_seedless_one_leaves_it_to_its_host` and `…::a_seed_holding_holder_yields_to_an_arriving_agent_and_reports_it`. The listener-order pin lives in `p2p_participation_gate.rs`, beside the participation switch's own listener-down pins over the same runtime-plus-in-memory-transport harness, rather than in `peer_leg_convergence.rs`, which drives no runtime. **Part 4:** `PumpCycles` carries the role as a `watch` beside its atomic, moved by `set_holder` on a change only, and `AccountStoreHandle::subscribe_engine_role` hands it out; `resolve_and_start` spawns `fauna_client_account_runtime::holder_yield::spawn_holder_yield_nudge` over it with `EngineHolderNudgeSlot::seat`, which asks the published nudge for its pass on each holder→non-holder edge and ends when the watch closes; the agent's `handle_reconcile_account_runtime` runs `reconcile_now` on a non-holder too (its `is_engine_holder` short-circuit is gone). Pins: `p2p_participation_nudge.rs` (`a_yield_beside_a_seat_asks_the_agent_for_its_pass_once`, red with the watch asking nothing; `a_yield_with_no_seat_asks_no_one`; `taking_the_role_asks_no_one`) and `pipe_server::reconcile_account_runtime_tests::reconcile_account_runtime_on_a_non_holder_takes_a_free_role` (red with the short-circuit in place). The agent logs the take at INFO (`account pass nudge: took the engine role an app's runtime yielded`). Live, the same day, a tier_3 tui seat with its agent up: the app's presence poll yielded, the agent's `ReconcileAccountRuntime` arrived 1 ms later and its pass held the role 188 ms after the yield, with no `reconcile_now` typed; the committed tier_3 witness is `test_account_runtime_handover.py::test_the_agent_takes_the_engine_role_the_app_yields` (tui and linux, 2026-10-13: the app read `(True, True)` at sign-in on both, then the role settled and the agent logged its nudge take; red with the agent's nudge arm taking nothing). Before part 4, between the yield and the agent's next backstop tick or `reconcile_now` no process pumped the store. Live, a tier_3 tui seat signed in with its agent up read `account_pump_role` `(True, True)` at sign-in and `(True, False)` seven seconds later with no poke (2026-10-08); the other half — the app taking the role back once the agent is stopped — is pinned at the Rust tier and, since 2026-10-13, by the tier_3 `test_account_runtime_handover.py::test_the_app_takes_the_engine_role_back_when_the_agent_stops` (tui and linux): the agent is stopped through its own `Shutdown` verb over the unix socket (`helpers/sync_agent_ipc_unix.py::shutdown_agent`), and the app, poked through `reconcile_now`, wins the election (red with the app's presence probe always answering present). Windows and macOS have no handover witness yet: the windows agent logs to its own file, and tui on macOS exposes no per-launch agent socket. The tui witnesses of the same-account file plane were waiting on part 4 alone.
- **Before 2026-10-08 the election was first-come alone**: `elect_at_start` gave the role to whichever co-located runtime reached `engine.lock` first, and on a desktop the app's runtime and the agent's mount both start at the post-auth hook, so the holder was a race the app usually won (measured 2026-08-18 as the holder varying between runs of `test_account_runtime_pump.py`) — which since 2026-10-08 decides whether same-account file bodies cross the peer leg at all ([`../behavior/p2p.md`](../behavior/p2p.md) § Implementation status today).
- **RULED 2026-10-01 — § Multi-instance concurrency → *Election mechanics*, web's lock names.** Built the same day: the SPA's conversations-engine lock is named to the scheme (`fauna.mls.conversations-engine/<actor>`, derived today by `fauna_mls::engine_role::web_role_lock_name`; it was `fauna.engine.<actor>`), and `fauna_account_store::locks_web`'s module docs state the ruling in place of the design question they carried. **Built 2026-10-09 — the lift the ruling names:** the conversations-engine role is taken in the wasm engine-construction path — `fauna-wasm`'s `conversationsManager` takes it before any engine exists, as `fauna_mls::storage::SqliteStorage::open` takes it natively, and rejects with a typed error the SPA renders as `served_elsewhere` — its name derived once in `fauna_mls::engine_role` beside `role_lock_path`; the tab's holds live in a per-tab registry there rather than on the manager (a wasm object is freed only by an explicit `free()`, and the manager's readers outlive a teardown), released by the SPA's one teardown (`resetConversationsManager`) together with the runtime; the erase probe is `fauna-wasm`'s `account_scope` (`actorsServedByAnotherTab`, `removeAccountBlock`); and one Web Locks request primitive, `fauna_core::web_lock`, carries all three crates' legs. The SPA makes no `navigator.locks` call; the moved cases are `fauna-wasm`'s `tests/engine_role.rs`, under `just wasm-test-check`. The lift moved no name.

- **RULED + BUILT 2026-10-01 — § Multi-instance concurrency → *The seed-leg role*, parts 1–5.** **The lock:** `fauna_account_store::locks::SeedLegLock` on `<store dir>/seed-legs.lock` (`store::SEED_LEGS_LOCK_FILENAME`), its web twin in `locks_web` under a second Web Locks name (`seed_legs_lock_name`), each a try with the engine lock's own outcome arms; its seat is `EngineElection::try_acquire_seed_legs`, answered by both hosts' elections (`fauna_sync_engine::account_runtime::FileElection`, `fauna_account_plane::web_host::WebElection`). **The role:** `AccountDriver::serve` takes it for a seed-holding principal at assembly, right behind the host's engine election (degrade open), and re-tries it at the backstop tick and on `reconcile_now` (degrade closed); a seedless principal never asks; the lock is a local of the serve, released with the assembly. `FleetWriter`'s two seed-only slots (`escrow_recovery`, `linked_nests`) are filled only for the role's holder, read afresh at every pass, so an engine holder without the role skips both steps. **The seed pass:** `account_driver::pass::seed_pass`, run by a seed-leg holder that is not the engine holder at the first wake after assembly, the backstop tick, a reconnect and `reconcile_now`, never on a nudge — the pin re-read, the enrollment registration (`pass::seed_enrollment`: the bind check's read-only half, `pass::bound_and_settled`, shared with the full pass's `bind_check`, then the one enrollment step both passes run, with the latch void once per replica — `bind_leg::BindMemo::seed_registered_at` — and the grant gate opened on a registered verdict; a cap-refused machine ends its seed pass there, as it ends a full pass), escrow recovery with the pass's own ancestry read (`bind_leg::fetch_verified_ancestors`, once per assembly and again when the pin moved), the secondary leg with its custody arm over the store's retire record, then both bound planes' `publish_pending` for what those steps journaled. It runs through `contained_pump` with no cycle counter, so it is contained, driven beside the command channel and cut by a sign-out, and moves no pass counter; its report says `skipped_non_holder` with its own slots filled, and the serve reads that report as it reads a full pass's: a stale-writer refusal reassembles, and so does a removed-from-account answer, under the same one-rotation cap, which a healthy answer re-arms. No app wires anything: every host reaches the driver through those two elections. Proofs: `conformance_account_plane_bind` — `a_seed_holder_beside_a_seedless_engine_holder_completes_and_custodies_the_linked_nest` (two runtimes on one store dir, the app never holding the engine role, ending with a seed-only reader recovering from the linked nest), `a_retire_the_seedless_holder_sent_reaches_the_linked_nest_through_the_stores_record`, `two_seed_holders_on_one_store_dir_run_one_secondary_leg_between_them`, `a_seed_holder_beside_a_seedless_engine_holder_registers_the_machine_at_a_rebuilt_nest` (the agent's every connection is the machine's store principal's, refused `not_registered` by the rebuilt box until the app's seed pass registers there; the agent's next pass verifies, settles and re-deposits, and a seed-only reader recovers from the rebuilt box), each red-verified; the removed-from-account heal in `fauna_sync_engine::account_runtime::tests::a_machine_deleted_beside_a_seedless_holder_revives_from_the_seed_pass`; the lock's exclusion pins beside `EngineLock`'s (`locks::tests`, and `tests/web.rs` in a browser). **The change generation part 5 names — built 2026-10-02:** a seed pass is a run of the funnel the generation is measured around, so one that changed an entry moves it with no pass counter moving (the *own-pump notice source* entry in [`account-runtime.md`](account-runtime.md) § Implementation status today). End to end, the two-box recovery journey runs its tui and linux device with the device's own sync agent up ([`nest/box-recovery.md`](nest/box-recovery.md) § Implementation status today). **The store thread's stack is stated, 8 MiB** (`fauna_sync_engine::account_runtime::STORE_THREAD_STACK_BYTES`): that journey's first run with the agent up aborted the debug linux app with a stack overflow in the seed pass on std's 2 MiB spawn default, the pass's future already boxed (measured 2026-10-01: red on 2 MiB, green on 3 MiB). What that stack is spent on is the next entry.
- **BUILT 2026-10-13 — § Multi-instance concurrency → *The seed-leg role*, parts 2 and 4: the seed pass after a first listing, and the owed pass.** Measured on linux with the real agent: after a sign-out (store and slot erased) and a sign-in in one app process, the app started a non-holder, its seed prologue ran before the agent's first listing, and every tip-sealed read was held for the escrow holder (`device=false holder=true`) until the next backstop tick — the Backups page refused for the 40 s the e2e waited. The driver now arms `Wake::FirstListingPoll` when a seed prologue finds the fleet scope unlisted, re-reads `AccountStore::listed` on `presence_poll_backoff`'s shape and runs one seed pass (`seed first listing`) when it lands; a seed pass whose recovery reports `Recovered(n > 0)` moves `PumpCycles`'s owed-pass watch (`AccountStoreHandle::subscribe_holder_pass_owed`), over which `resolve_and_start` spawns `holder_yield::spawn_holder_pass_owed_nudge` on the yield's seat. Part 4's "where the holder reads it" was unbuilt for a holder in another process: `PrincipalBundle`'s view is loaded once and re-read only at its own writes, so the agent's pass never saw the app's recovered key; every full pass now starts with `PrincipalCustody::adopt_slot_generation_keys` (adds what the slot carries and the view lacks, drops nothing — a shred stays the slot's). The yield's watch read its starting role inside the spawned task, so a yield before the task's first poll asked nothing; it reads it at subscribe time now. Pins: `account_runtime::tests::a_machine_signed_out_and_in_beside_its_agent_keys_the_generation_without_a_command` (red with the hold's sources as measured) and `p2p_participation_nudge.rs`'s `each_owed_pass_beside_a_seat_asks_the_agent_once` and `an_owed_pass_with_no_seat_asks_no_one`. The first-listing gate's immediate refusal on a non-holder before its holder's first listing (`account-client-lifecycle.md` § The client-side lifecycle → *The first listing*, clause (2)) is unchanged; it lasts about a second.
- **Built — W5.1 the engine-singleton election, unix leg (2026-08-14): the
  first W5 slice — the T9 lock is code and the runtime elects in front of
  the pump.** `fauna_account_store::locks::EngineLock` is the
  kernel-arbitrated non-blocking try-lock on the reserved
  `<store dir>/engine.lock` (std `File::try_lock` — the
  `AccountInstanceLock` idioms: never-deleted `0600` lock file, crash
  release, degrade reported to the caller), and `AccountStoreRuntime`'s
  assembly takes the election inside the readiness barrier: the holder
  pumps exactly as before; a non-holder serves every command — reads,
  `put_preference` (since 2026-09-22 the local write plus the publish
  step, on every role — the charter's pump bullet, wake source (4)),
  intent enqueue — while
  dropping nudges and reconnect wakes, and re-tries the election on the
  backstop tick (and on an explicit `reconcile_now`), so the role
  transfers when the holder exits (and, since 2026-10-08, to the agent
  when it arrives — *The agent holds the role when present*); the takeover
  pass is the new holder's catch-up. `PumpReport::skipped_non_holder` is the in-band role answer.
  Two degrade rulings recorded at the code (`elect_at_start` + the ticker
  arm): at start, degrade-OPEN — a lone process must pump, the
  `account-scoping.md` posture preserved; on a re-try, stay a non-holder —
  a refusal already proved arbitration works there. The MLS drain
  carve-out stays declared at the drain seam (`IntentDrainer::Mls`, W4)
  whichever process holds the role. Proven: tier_1 exclusion pins in
  `locks`, and tier_3 `conformance_account_runtime` V8 — two
  runtimes on ONE store dir (the lock lives on the open file description,
  so same-process acquires contend exactly like processes): roles as
  report facts, the non-holder's dual write landing in store + rail, and,
  after the holder exits, a rail-only write imported through the
  survivor's backstop-tick takeover. ⚠ **`AccountStoreHandle::shutdown()`
  releases the election BEFORE it answers (fixed 2026-08-19).** It used to
  reply to `Cmd::Shutdown` and *then* break out of the assembly loop, so it
  returned while the worker still held `EngineLock`: a caller that shuts one
  runtime down and immediately expects the role to be free — a co-located
  agent taking over once the app is down, or the same process re-assembling
  for another account — got a `Refused` for a runtime that was already gone,
  with nothing to wait on but a sleep. The reply is now sent after the loop's
  per-assembly locals drop, which is what `shutdown()`'s own contract ("drops
  the store, and exits") always claimed. It surfaced as a real red in
  `agent_process_tier3::agent_process_hosts_the_shared_account_store_with_no_app_running`
  — the proof cited below — which no gate runs, because `tier3-nest` is
  opt-in. **Not built (the rest of W5):** the
  `data_version` notification floor (W5.2 — a non-holder serves reads but
  has no change-notice path of its own yet), the migration/adoption
  critical section under concurrent cold open (W5.3), the windows leg (T9
  ratifies a named mutex; std's `LockFileEx` on the same file compiles
  ungraded — whether the file lock is the simpler uniform shape is the
  windows leg's call, a candidate T9 refutation to grade on Windows), and
  the web Web-Locks leg. **The at-most-one-instance law is still in
  force** — W5.1 replaces its guarantee *for the pump* only; the
  app-level refusal retires at W5.6, after W5.2/W5.3 (the charter's
  ordering rule: the election lands before the refusal dies).

- **⚠ Gap found 2026-08-15 while scoping W5.6 — RULED 2026-08-15:
  the store-safety inventory is CLOSED with a named answer per
  store; the critical-section count re-ratifies to THREE; the build is
  still owed.** The finding, compressed: the at-most-one-instance law
  ([`apps/account-scoping.md`](apps/account-scoping.md) § Concurrent
  instances) refuses a second same-account *process* outright, so it had
  been the de facto cross-process guard for **every** account-scoped store
  — not only the account store § Multi-instance concurrency made
  multi-process-safe — and the one store that never named it is MLS:
  `mls_state.db` is class 5 (§ The audience ladder), every "one engine over
  one `mls_state.db`" statement in the tree is an *in-process* rule, and
  two engines advancing one group's epochs fork the ratchet
  (user-irrecoverable by construction — class 5 never rides the plane; the
  file stays consistent under WAL *or* the rollback journal while the MLS
  state diverges). **The ruling** (mechanism owner: § Multi-instance
  concurrency → the conversations-engine-role bullet): candidate (i) is
  adopted as the permanent structural shape — the conversations-engine role
  is the third exclusive critical section, guarded by `mls.lock` beside the
  `mls_state.db` it covers, acquired in the shared engine-construction path,
  with the non-holder's conversations surface refusing honestly and the
  holder-as-service IPC evolution declared. Candidate (iii) is affirmed as
  the already-live present (the bearer-only agent — engines `mls: None`,
  never takes the account instance lock). Candidate (ii) — a static
  "conversations only in the singleton instance" product restriction — is
  **REJECTED, not escalated**: it is coarser than the lock gives for free,
  mechanically confused when the engine singleton is the bearer-only agent
  (no app would have conversations at all), and forecloses the
  holder-as-service evolution; since the ruled shape is strictly additive
  over today's whole-instance refusal, no genuine product tradeoff remained
  to put to the user, so the "(ii) goes to the user" trigger never fired.
  **The inventory's dispositions** (each posture verified in code 2026-08-15): the W5 account store —
  multi-process-safe, W5.1–W5.3; **`mls_state.db` — the third exclusive
  section above** (posture ratified non-WAL/`busy_timeout` 0 as a loud
  tripwire); the WireGuard key — kernel-arbitrated single mint
  (`hard_link` publish, adopt-never-clobber; fixed 2026-08-15, and the key
  itself deleted with the stack 2026-08-23 — the `hard_link`-publish
  single-mint pattern is the reusable half); the
  backup-audit state — atomic replace, last-writer-wins by design, every
  field recreatable (fixed 2026-08-15; both fixes:
  `git log --grep 'the two multi-process write gaps'`); `config-replica` —
  already atomic, lost updates re-converge at next sign-in;
  `p2p_contacts.db`/`p2p_quality.db` — refuse-don't-corrupt
  (`busy_timeout` 0), class-4 loss-tolerant, acceptable as-is (a
  `busy_timeout` would smooth refusals but is not owed by W5.6);
  `spam_model.json` — last-writer-wins, re-trainable, declared
  wipe-tolerant; the folder↔folder location map — guarded by the sync
  agent's own `InstanceLock`, unaffected; install-scoped stores (tui's
  nest-identity pin + sealed credential store) — out of scope: already
  exposed to cross-*account* concurrency today, the same-account
  retirement adds no new exposure. **Build status (2026-08-15):** the
  role lock LANDED — the W5.6 pre-step, entry *Built — W5.6's
  conversations-engine role lock* below, with two ruling details refined
  at build time and recorded in the mechanism bullet (file-derived lock
  name instead of the fixed `mls.lock` sibling; `busy_timeout` 0 set
  explicitly, the driver's default having proved to be 5 s) — honoring
  the load-bearing ordering: the lock lands **before** any app's refusal
  dies (slice contract § W5.6). The
  at-most-one-instance law stays in force until W5.6's retirement leg
  lands, tui-first; the switcher's "open as new instance"
  non-active-rows-only restriction lapses per app with that app's
  refusal (`apps/fauna-linux/src/settings/account.rs`).

- **Built — W5.6 the refusal retirement, tui-first (2026-08-15): the
  at-most-one-instance law is superseded on tui.**
  `fauna_client_accounts::ServingMode` carried the per-app split while the
  retirement rolled out; every leg has landed and the `Exclusive` arm is
  removed, so servers acquire the per-account lock **shared**
  (`Concurrent`) and the exclusive lock survives only as the probe.
  `is_served`'s exclusive probe stays truthful over shared holders, and a
  shared acquire absorbs the probe's two-syscall window with a bounded
  retry. tui's leg: `become_session_instance_or_exit` serves `Concurrent`;
  the honest conversations refusal rides
  `ConversationsState::served_elsewhere` →
  `sync_page_error`'s top-precedence arm → `error-message`
  (`conversations.errors.served_elsewhere`), surfaced at establish so an
  engine-less session never hides it. Proven: five new tier_1 pins in
  `instance_lock.rs`; the flipped e2e
  `test_tui_bound_launch_onto_the_served_account_coexists` — both
  instances authenticated, a muted-words add in one visible in the other
  over the shared store, the non-holder's honest refusal, the holder
  clean. The trickle-down rows for windows/linux/macos are tracked
  internally; those legs have landed (linux, macOS and
  windows), and `ServingMode::Exclusive` is removed. `AlreadyServed` stays,
  because it still fires against the sign-out probe's exclusive holder.

- **Built — W5.6's conversations-engine role lock (2026-08-15): the third
  exclusive critical section is kernel-enforced at
  `fauna_mls::storage::SqliteStorage::open`.** Every file-backed engine
  construction (`MlsEngine::new` → `SqliteStorage::open` — tui, linux, the
  ffi apps alike; `bins/fauna-sync` too, until its removal 2026-10-02) now try-acquires the role lock at
  `role_lock_path` (the database's own name + `.lock`) before the SQLite
  open and holds it for the storage's — hence the engine's — lifetime;
  a held lock refuses with the typed `StateServedElsewhere`, mapped to
  `MlsError::ServedElsewhere` at the engine boundary so apps can refuse
  honestly rather than report a broken store; lock-file I/O failure fails
  closed (class-5 state). The lock-file mint is the new workspace-shared
  `fauna_core::fs_lock` (never-truncate / never-delete / owner-only),
  which `fauna_client_accounts` and `fauna_account_store::locks` now also
  consume — one idiom, three lock families. Pinned tier_1 in
  `fauna-mls/src/storage.rs`: the exclusion pin (second open refused
  typed while the first lives, succeeds after release —
  mutation-red-verified), the derived lock name + never-deleted file, the
  two-databases-one-directory independence pin, and the ratified tripwire
  (non-WAL + `busy_timeout` 0, both now explicit). **Not yet consumed:**
  no app can reach the refusal until its same-account launch refusal
  retires (the W5.6 retirement leg, tui-first) — until then the account
  instance lock still refuses the second process earlier.
