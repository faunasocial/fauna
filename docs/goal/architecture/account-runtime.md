# The account runtime — which process holds the plane — target state

Owns: account-runtime
Status: ratified — W5 landed 2026-08-14/15 (engine-singleton election, the seedless host, hosting and renewal, the top-up self-heal, the peer-leg assembly seam) and the W3 seat has all seven app hosts, the last on 2026-08-27; split verbatim out of `account-data-plane.md` on 2026-09-06
Authority: **which process holds the plane, and how each app hosts it** — multi-instance concurrency (W5: the engine-singleton election, the seed-leg role, the seedless host, hosting registration and renewal, the migration/adoption critical section, the top-up self-heal), the path unification (W6), and the per-app runtime seat's build-out (W3: the `fauna-ffi` seat and the linux/android/apple/windows/tui hosts). **NOT owned here** — the client-side lifecycle rules the seat implements → [`account-client-lifecycle.md`](account-client-lifecycle.md) § The account store (W1) → *The client-side lifecycle (W3)*; the client scoping taxonomy and multi-account stages → [`apps/account-scoping.md`](apps/account-scoping.md); the sync agent's own lifecycle and credential model → [`apps/sync-agent.md`](apps/sync-agent.md); what the engine syncs → [`account-sync-plane.md`](account-sync-plane.md); the charter and the cross-cutting status → [`account-data-plane.md`](account-data-plane.md). On conflict in those domains, raise it.

Last verified: 2026-09-06 (split verbatim; per-slice build dates are in the status entries below)

Split verbatim out of [`account-data-plane.md`](account-data-plane.md) on 2026-09-06 — that doc had reached **686,434 B**, 2.62× the 262,144 B whole-file read ceiling, and no single seam could clear it (moving its status ledger alone left both halves breached, re-verified at three successive sizes). Its own `Authority:` line already enumerated the five concepts it owned; this is that list made structural, each concept taking its rule sections **and** its status-ledger entries together. A routing stub remains at each original location; prior history: `git log --follow docs/goal/architecture/account-data-plane.md`. The `W<n>` workstream labels and `R<n>` decision labels used throughout are defined in [`account-data-plane.md`](account-data-plane.md) § Workstreams and § The ratified decisions.

> **Reading this doc.** Its text was carried **verbatim** out of [`account-data-plane.md`](account-data-plane.md) on 2026-09-06, so an unqualified `§ <name>` citation inside it may name a section that is no longer a sibling on the page. Resolve any such name against the rest of the family first: [`account-data-plane.md`](account-data-plane.md) (the ratified decisions, the account store, the nest-side requirements and the cross-cutting status), then [`account-data-taxonomy.md`](account-data-taxonomy.md), [`account-sync-plane.md`](account-sync-plane.md), [`account-offline-mutation.md`](account-offline-mutation.md), [`account-replica-posture.md`](account-replica-posture.md). Positional words (“above”, “below”) inside a carried block point within this doc: every section moved whole, so an intra-section deictic could not break, and the boundary-crossing ones were scanned before the split.

## Section map

- **[Multi-instance concurrency (W5)](#multi-instance-concurrency-w5)** — the engine-singleton election and everything that follows from one engine per account. **The heading is unchanged from `account-data-plane.md`**, so a `§ Multi-instance concurrency (W5)` citation resolves by swapping the filename.
- **[Implementation status today](#implementation-status-today)** — the W5/W6 build-out first, then the W3 seat host by host.

## Multi-instance concurrency (W5)

Target: **concurrent same-account instances are supported** — 2 tui + 3
native instances on one account, an update in one visible in the others —
superseding the at-most-one-instance-per-account law
([`apps/account-scoping.md`](apps/account-scoping.md) § Concurrent
instances). **The contract LANDED 2026-08-15 (W5.6) and the supersession is
per app**: tui — the lead app — retired first (two same-account tui
instances run concurrently against one store dir, e2e-proven), with linux
and macOS retiring the same week and **windows on 2026-08-24**, which
completes the desktop set. An app that has not retired keeps the exclusive
law until its own trickle-down leg flips its `ServingMode`. Current per-app
retirement status, the mode split, and the re-ratified law are owned by
account-scoping.md's § Concurrent instances — not restated here to avoid
drift.

- **Store contract.** The store is multi-process-safe: SQLite WAL,
  transactions as the write unit, store-level advisory locks for its two
  genuinely exclusive critical sections — schema migration/adoption, and the
  **engine-singleton role** (exactly one co-located process runs the sync
  engine + outbox drain at a time: the agent when present (R2), else an
  elected app instance; every other instance is a plain reader/writer of the
  store). A third exclusive section exists *outside* the store — the
  conversations-engine role, below. One more *inside* it is the seed-leg
  role, below. **A sibling's commit makes a local write wait, never fail**:
  every store transaction that writes takes SQLite's write lock at its
  `BEGIN` (`SqliteBackend::write_tx`), where the busy timeout applies — a
  transaction that read first and asked for the lock afterwards is refused
  at once, with no wait, whenever another instance holds the lock or has
  committed since the read. **And one version of an entry names one
  value**: a class-2 entry's version is read before the write's transaction
  opens, so the write verifies inside it that the entry is still one below —
  an instance that lost that race re-reads and writes the next version,
  never a second value at the same one (both built 2026-10-01 —
  § Implementation status today).
- **The serving lock — the store's one *presence* lock (ruled 2026-09-20).** Beside the two exclusive sections above the store root carries `serving-<actor-id-hex>.lock`, one per account: **shared** for a serving app instance's lifetime, a momentary **exclusive try** for an erase asking whether anyone is running out of the store (`fauna_account_store::locks::ServingLock`; who takes it, why the agent does not, and what the erase does with the answer are the ruling's — [`apps/account-scoping.md`](apps/account-scoping.md) § Concurrent instances → *An erase refuses while a sibling serves the account*). Three properties are the mechanism's own. **It sits at the root, never inside `<root>/<actor>/`:** the erase it guards removes that directory, and a guard the erase unlinks is one the next instance locks a different inode of — the race the never-delete rule exists to close; at the root it is a file no sweep names (the whole-root sweep removes only well-formed actor *directories*). That placement is also what makes it sound for `engine.lock` and `migration.lock` to live *inside* the actor's store dir: nothing erases that dir while a serving instance holds this lock — because every user-facing erase that removes actor scopes under the root asks this lock first, the e2e agent's factory reset is the one exempt erase (no user is in front of it), and no erase without a gesture behind it runs there at all (which erases are which, and the one gesture not yet routed: the ruling's routing bullet). **The shared acquire blocks; it does not try** — the only exclusive taker is the erase's probe, which holds for one `try_lock`, so the wait is bounded by construction, whereas a try would turn that instant into a serving instance no erase can see. **It degrades open on both sides** — an instance that cannot take it serves unseen, a probe that cannot reach it reports free — the instance lock's posture, for the instance lock's reason.
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
  or election protocol. Priority is behavioral, not protocol: on desktops
  the apps *ensure the agent* (the shipped provisioning convergence loop
  already spawns and probes it) rather than take the role themselves, so
  the agent holds the lock in steady state; where no agent exists (mobile,
  web, an agent-less box) the first process to take the lock is the
  singleton, releasing on exit. **One per-kind carve-out:** the singleton
  drains outbox intents whose seal needs only store-held keys; MLS-sealed
  intents drain from a process hosting the conversations engine — an app,
  never the bearer-only agent (detail owner:
  [`../behavior/devices.md`](../behavior/devices.md) § Offline compose).
- **Local change notification (T9, same resolution): poll-with-poke.**
  Same-machine instances sharing one store need *notification*, not sync.
  The floor on every platform whose store is SQLite is `data_version`
  polling (one cheap PRAGMA read on the UI's existing refresh cadence; web's
  IndexedDB store has no such counter — the next bullet's web paragraph) —
  correctness never
  depends on anything else; the poke is the fast path where present: the
  agent's existing IPC pushed-events stream gains a store-changed event,
  and web pokes tabs over BroadcastChannel. Platform file-watch is
  rejected as the primary bus: seven per-platform watchers for a latency
  win the poke already provides. A poked or polled reader refreshes
  projections; the W2 peer leg is for *different* replicas (different
  machines) and never runs between processes sharing a store.
- **A runtime's own pump is a source of the notice too: an open store-backed surface repaints whoever changed the store (ruled 2026-10-01).** The floor above moves only when *another* connection commits, so alone it makes a repaint depend on which process won the election. The same change — another device's muted word walking in, a successor's inherited E1 values carried onto its own rail by its walk — repaints an open page in an app that sits beside the agent (the agent's connection committed it) and leaves the page stale in an app that pumps for itself (mobile, web, a desktop with no agent), until the user leaves the page and comes back. A user cannot see the election, so the rule is one rule: **an open surface whose render source is read through the account store shows what a fresh visit would show, within the notice's latency, whichever process applied the change.** "Read once per visit" is not a contract. Five parts. **(1) The own-pump source is a change generation on the runtime's handle** — a counter beside the pass counters (`PumpCycles`), awaitable in-process, moved once when a run of the pump ends, **iff that run changed an entry a read can answer**: a state entry put, merged or forgotten, a record added or dropped, a scope dropped — by a walk (its carry of a predecessor's delegable rows included), a re-walk behind a recovered key, or any step added later. "A run of the pump" is every run of the pump's one funnel, whether or not it counts as a pass cycle: a full pass, a nudge's walk, a publish step, a seed pass. A run that changed only bookkeeping moves nothing — a journaled row whose value lost, this replica's own rows coming back off the feed, frontiers, watermarks, relay rows — because runs are frequent (every scope nudge walks, the backstop tick runs a full pass every five minutes) and one consumer's re-drive is a whole feed reload. Pass *completion* (`pass_completed_after`) stays what it is, the "the pump has run" barrier; it is not the notice. The change is counted at the store's entry-write doors and read around the run rather than summed from the pass report, so a pump step added later cannot forget to count (build-refutable: the pinned property is "a changed run moves it, a quiet run does not", not where the count is taken). **(2) A gesture's own write is not a source.** A command the runtime serves for its own app (`put_preference`, a raise door) changes the store outside the pump; the surface that made the gesture repaints from the gesture's own answer, as today, and a second surface in the same process is the app's own state to keep. **(3) The generation sits beside the floor; neither replaces the other.** A non-holder runs no pump, so the floor (and the poke) is its only source; a holder's own run never moves its own floor reading, so the generation is its only source for that run; and a holder still needs the floor for what a co-located non-holder writes. Every runtime watches both. **(4) One watch in shared Rust joins the sources, and every app consumes that one watch**: generation, floor poll, and the poke where one is built, answered as a payload-free "may have changed" that ends when the runtime is gone. Its prior art is the loop the two conversation seams each carry (read positions and contact overlays — [`../behavior/conversation-read-state.md`](../behavior/conversation-read-state.md) § The read-marker record → *How the manager reaches the plane*); the watch is lifted from that loop and the seams become its consumers. Each app's handler is the one tui already has for the floor: re-drive the OPEN surface's own load (reload semantics, no new render path), for every surface that reads through the store; a new store-backed surface joins the handler in the change that adds it. The notice is a level, not an event: sources coalesce, a re-read that answers what the surface already shows paints nothing, and a re-read never discards an edit in progress. **(5) Web.** IndexedDB has no cross-connection counter (`data_version` answers `None`), so a tab has no floor at all. The holder tab's generation is web's first notice of any kind, and the BroadcastChannel poke is the floor's stand-in there rather than a fast path: it carries to the other tabs what `data_version` carries natively — a holder's changed run, a non-holder's gesture write. Latency: the own-pump source wakes at the end of the run that made the change; the floor at its poll cadence.
- **The seed-leg role: a leg only a signed-in app can run is elected among the seed holders, never by the engine role (ruled 2026-10-01; built 2026-10-01 — § Implementation status today).** The engine role elects who pumps the bound nest, and on a desktop the process that wins it in steady state holds no seed. Three steps of the pass need what only a signed-in app holds — the identity seed, or the owner session it opens: the enrollment registration ([`account-replica-posture.md`](account-replica-posture.md) § The store device principal — registering needs the owner session), escrow recovery ([`account-data-taxonomy.md`](account-data-taxonomy.md) § The generation machinery → *Escrow recovery*), and the secondary leg with the custody arm that rides it ([`account-sync-plane.md`](account-sync-plane.md) § The bind leg, ruling 4). Each was ruled "skipped on the seedless host, healed by a signed-in app's pass on the same machine", and read 2026-10-01 that pass does not exist on a desktop whose agent is up: the app beside the agent is a non-holder and runs no pass. So there a linked nest is neither completed nor custodied (measured 2026-10-01, the two-box recovery journey's first run), a key held only in escrow is not recovered, and a machine bound to a rebuilt box is not registered there again (the first read from the code, not measured; the second shown by its conformance case's red run). A phone, a browser tab and a desktop app that holds the engine role were never affected. A user cannot see the election — the bullet above states that for repaints — so the rule is the same rule: **what a signed-in device does for its account does not depend on which co-located process pumps.** Six parts. **(1) A second role.** Exactly one co-located seed-holding runtime per store holds the **seed-leg role**, by a second kernel-arbitrated lock with the election's own T9 mechanics — `seed-legs.lock` beside `engine.lock` in the store dir, a second Web Locks name on web — the store's third exclusive section. Only seed-holding runtimes contend; a seedless runtime never takes it. It is tried at assembly right behind the engine election and tried again wherever that election is (the backstop tick, an explicit `reconcile_now`), with the same degrade posture: open at start, closed on a later try. It carries no priority: the first seed holder to take it keeps it until it exits. **(2) Who runs the seed-only steps.** The holder of both roles runs them inside its pass, exactly as built — every phone, every tab, a desktop with no agent. A seed-leg holder that is not the engine holder — the desktop app beside its agent — runs the **seed pass**: the seed-only steps alone, in pass order, at every wake on which an engine holder runs a full pass (the first after assembly, the backstop tick, a reconnect, `reconcile_now`), never per nudge. An engine holder without the seed-leg role — the seedless agent, or a seed holder beside another that took the role first — skips them. A step added later that needs the seed or the owner session joins the seed pass in the change that adds it. **(3) What the seed pass may write: nothing the engine role makes exclusive.** It walks and reconciles no bound plane, sends and banks no watermark, drains no outbox, clears nothing and settles no replica. It writes the store as a non-holder already does — rows through the bound planes' writer door (a receipt, a custody row), published by its own publish step — and through linked planes, which by construction leave every memory about the bound nest alone ([`account-sync-plane.md`](account-sync-plane.md) § The bind leg, ruling 4). The credential slot's registration latch is written by whichever process registered, as today. **(4) Each step beside a seedless holder.** *Enrollment:* the seed pass asks the bound nest which replica it is over the owner session, compares the answer with the store's settled replica, and runs the registration — with the latch void when the two differ or the device handshake voided it. Void once per replica: the seed pass settles nothing, so the two keep differing until the engine holder's pass settles, and once a seed pass has registered at a replica the latch is that replica's. The watermark clear, the holdings re-arm and the settling stay the engine holder's, whose own register probe finds the grant on the nest at its next pass. A removed-from-account answer reassembles the seed-holding runtime exactly as it does in a pass. *Escrow recovery:* a recovered key lands in the shared slot's retained bundle, where the holder reads it; the seed pass reads for itself the ancestry recovery admits; the rows the key opens are re-presented by the holder's next full reconcile, so beside a seedless holder they are readable within one backstop interval of the recovery. *The secondary leg:* unchanged, over the app's own connector; the retires it mirrors are read from the store ([`account-sync-plane.md`](account-sync-plane.md) § The bind leg, ruling 5). **(5) Reports, counters, the notice.** `reconcile_now` on such a runtime still answers `skipped_non_holder`, with the seed pass's own slots filled. The seed pass moves no pass counter — "the pump has run" stays the engine holder's statement — and it is a run of the pump's funnel for the change generation of the bullet above, because it can merge an entry a read answers. It is driven beside the command channel and cut by a sign-out like any pass. **(6) Refused.** *The app takes the engine role from the agent while it lives.* It needs the priority T9 declined — a presence signal, a yield and a fast re-election where the election is one lock — and it moves the bound nest's pump, the outbox drain and the peer leg's listener between processes at every app launch and quit: each handover is a catch-up pass and a re-bound endpoint every sibling then walks. That spends the steady state R2 chose, one always-on engine with thin apps beside it, to fix legs that need none of it. *The agent is given the means to run them.* Every door the secondary leg uses admits the authenticated account, and a seedless process has a device principal registered at the bound nest alone: reaching a linked nest from the agent means an enrollment, a device row and a revocation path at every linked nest, or an owner session lent to a process that holds no seed, which ends the agent's device-principal-only posture ([`apps/sync-agent-credentials.md`](apps/sync-agent-credentials.md) § Credential model). Neither helps escrow recovery, which needs the seed itself. The cost, stated: these legs run only while a signed-in app runs on the machine — for a linked nest, ruling 4's own bound. *Every seed-holding non-holder runs them, with no election.* Two apps on one account would push the same rows verbatim to one linked nest, and the loser's refused push retires a copy that is live on both nests ([`account-sync-plane.md`](account-sync-plane.md) § The bind leg, ruling 1(c)); a tab beside a seed-holding holder tab would repeat that tab's leg for nothing.
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
  shape. This also makes the drain carve-out well-defined under
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
  above; the per-account raise channel survives as UX (raise is still the
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

*The entries below were carried verbatim out of [`account-data-plane.md`](account-data-plane.md) § Implementation status today, which stays the home of the cross-cutting entries no single plane owns.*

- **2026-10-02 — the runtime's hosts carry no `__config` client any more**. With the rail retired, no host builds a `ConfigClient` or a device-local `__config` replica beside its runtime, and the runtime's pump answers only scope-tagged pushes ([`config-dissolution.md`](config-dissolution.md) § Implementation status today, the closure-step-(6) entry).
- **RULED 2026-10-01 — § Multi-instance concurrency → *Election mechanics*, web's lock names.** Built the same day: the SPA's conversations-engine lock is named to the scheme (`$lib/webLocks::engineLockName` → `fauna.mls.conversations-engine/<actor>`, pinned by `tabPin.test.ts`; it was `fauna.engine.<actor>`), and `fauna_account_store::locks_web`'s module docs state the ruling in place of the design question they carried. **Not built — the lift the ruling names:** the conversations-engine lock is still taken in the SPA (`$lib/webLocks::tryHoldEngineRole`, the probe `actorsServedByAnotherTab` beside it) while its three siblings are taken in shared Rust; the uniform shape is the role taken in the wasm engine-construction path as `fauna_mls::storage::SqliteStorage::open` takes it natively, its name derived in `fauna-mls` beside `role_lock_path`, the erase probe in `fauna-wasm`'s `account_scope`, and one Web Locks request primitive under the three crates' legs — captured; the lift moves no name.

- **RULED + BUILT 2026-10-01 — § Multi-instance concurrency → *The seed-leg role*, parts 1–5.** **The lock:** `fauna_account_store::locks::SeedLegLock` on `<store dir>/seed-legs.lock` (`store::SEED_LEGS_LOCK_FILENAME`), its web twin in `locks_web` under a second Web Locks name (`seed_legs_lock_name`), each a try with the engine lock's own outcome arms; its seat is `EngineElection::try_acquire_seed_legs`, answered by both hosts' elections (`fauna_sync_engine::account_runtime::FileElection`, `fauna_account_plane::web_host::WebElection`). **The role:** `AccountDriver::serve` takes it for a seed-holding principal at assembly, right behind the host's engine election (degrade open), and re-tries it at the backstop tick and on `reconcile_now` (degrade closed); a seedless principal never asks; the lock is a local of the serve, released with the assembly. `FleetWriter`'s two seed-only slots (`escrow_recovery`, `linked_nests`) are filled only for the role's holder, read afresh at every pass, so an engine holder without the role skips both steps. **The seed pass:** `account_driver::pass::seed_pass`, run by a seed-leg holder that is not the engine holder at the first wake after assembly, the backstop tick, a reconnect and `reconcile_now`, never on a nudge — the pin re-read, the enrollment registration (`pass::seed_enrollment`: the bind check's read-only half, `pass::bound_and_settled`, shared with the full pass's `bind_check`, then the one enrollment step both passes run, with the latch void once per replica — `bind_leg::BindMemo::seed_registered_at` — and the grant gate opened on a registered verdict; a cap-refused machine ends its seed pass there, as it ends a full pass), escrow recovery with the pass's own ancestry read (`bind_leg::fetch_verified_ancestors`, once per assembly and again when the pin moved), the secondary leg with its custody arm over the store's retire record, then both bound planes' `publish_pending` for what those steps journaled. It runs through `contained_pump` with no cycle counter, so it is contained, driven beside the command channel and cut by a sign-out, and moves no pass counter; its report says `skipped_non_holder` with its own slots filled, and the serve reads that report as it reads a full pass's: a stale-writer refusal reassembles, and so does a removed-from-account answer, under the same one-rotation cap, which a healthy answer re-arms. No app wires anything: every host reaches the driver through those two elections. Proofs: `conformance_account_plane_bind` — `a_seed_holder_beside_a_seedless_engine_holder_completes_and_custodies_the_linked_nest` (two runtimes on one store dir, the app never holding the engine role, ending with a seed-only reader recovering from the linked nest), `a_retire_the_seedless_holder_sent_reaches_the_linked_nest_through_the_stores_record`, `two_seed_holders_on_one_store_dir_run_one_secondary_leg_between_them`, `a_seed_holder_beside_a_seedless_engine_holder_registers_the_machine_at_a_rebuilt_nest` (the agent's every connection is the machine's store principal's, refused `not_registered` by the rebuilt box until the app's seed pass registers there; the agent's next pass verifies, settles and re-deposits, and a seed-only reader recovers from the rebuilt box), each red-verified; the removed-from-account heal in `fauna_sync_engine::account_runtime::tests::a_machine_deleted_beside_a_seedless_holder_revives_from_the_seed_pass`; the lock's exclusion pins beside `EngineLock`'s (`locks::tests`, and `tests/web.rs` in a browser). **The change generation part 5 names — built 2026-10-02:** a seed pass is a run of the funnel the generation is measured around, so one that changed an entry moves it with no pass counter moving (the *own-pump notice source* entry below). End to end, the two-box recovery journey runs its tui and linux device with the device's own sync agent up ([`nest/box-recovery.md`](nest/box-recovery.md) § Implementation status today). **The store thread's stack is stated, 8 MiB** (`fauna_sync_engine::account_runtime::STORE_THREAD_STACK_BYTES`): that journey's first run with the agent up aborted the debug linux app with a stack overflow in the seed pass on std's 2 MiB spawn default, the pass's future already boxed (measured 2026-10-01: red on 2 MiB, green on 3 MiB). What that stack is spent on is the next entry.
- **MEASURED 2026-10-01 — the store thread's stack budget.** The store thread (`fauna-account-plane`, spawned by `fauna_sync_engine::account_runtime::AccountStoreRuntime::start`) polls every pass from one `block_on`, so its stack carries the whole chain. **The futures are not what fills it.** `worker`'s future is 53,504 bytes and the `AccountDriver::serve` future inside it 48,544 (aarch64 linux, a debug build). **The poll frames are, on an unoptimized build only.** Read off each function's prologue in the debug `fauna-tui` and `fauna-desktop` the two-box recovery journey runs (the two binaries carry byte-identical frames for every function on the path): `AccountDriver::serve` 865,456 bytes, `serve_local_cmd` 584,736, `pump` 365,712, `worker` 110,640, and 262,592 for `block_on` moving the 53 KB future by value through three frames — 1,238,688 bytes (1.18 MiB) in use whenever the serve is polled at all, before any pass. An unoptimized build gives every temporary of an `async fn` a stack slot of its own, so a long function's frame is the sum of its await sites. A release build (`fauna-sync-agent`, same instantiation) has `serve` at 35,104 bytes, `pump` at 16,384 and `serve_local_cmd` at 2,320: about 140,000 bytes before a pass. **High-water, measured** (the present bit of each of the thread's stack pages, read after every pass by a probe compiled in for the run and not kept; the journey passed on tui and on linux with the device's own sync agent up, and in that run the app held the engine role on both): a first sign-in's prologue reached 2,004 KiB on tui and 1,968 KiB on linux; the relaunched app's prologue, the pass that custodies the linked box in that journey, 2,136 KiB on tui and the same depth below the pass driver on linux (900 KiB against tui's 904). Std's default is 2,048 KiB: a debug app's plain prologue sits 44–80 KiB under it and the pass that reaches the linked box is over it. **So the constant stays at 8 MiB**, and what needs it is an unoptimized build — every dev, test and e2e binary; a shipped one would fit the default many times over, and one constant serves both because the stack is reserved, never committed. No future dominates, so nothing was boxed. **Why linux and tui differed in the run that found it: not established, and not the code.** Their frames are identical, and at 2 MiB both sit within 100 KiB of the guard page on the same journey; which process holds the engine role is a race the journey does not pin (the app runs a full pass or a seed pass accordingly), and the leaf a pass reaches depends on what it finds to do. In the run that found it both apps were on their seed pass and linux aborted about 50 ms in; no run of this measurement had an app on its seed pass. At the Rust tier a seed pass reaches 1,332 KiB against the fake nest and about 1,712 KiB with a linked nest under it (`conformance_account_plane_bind`'s `a_seed_holder_beside_a_seedless_engine_holder_completes_and_custodies_the_linked_nest`, in-process nests: 544 KiB below a pass driver that sits 1,164 KiB down) — 336 KiB inside the default, so that suite would not have reproduced the abort. **What a real `NestClient` puts under the linked leg, by the frame table** (the same debug `fauna-desktop`, first-party functions only): little of its own. The connector's future is 5,856 bytes and `NestClient::connect` 3,120; `connect` awaits the login mint inline, so that is polled under the pass — the bearer source and token cache (frames of 6,848–8,608 each), `fetch_mint` 7,232, and the anonymous login connection it dials, whose largest frames are `mint_bearer_over_handshake` 20,272, `connect_authed` 19,296 and `dial_ws_trusted_with_graduate_retry` 19,136 — about 100 KiB if every one of those is on the stack at once; reading the identity the connection is bound to is 4,528 + 6,160, and each request afterwards at most 8,672 + 4,224. The connection's own socket loop is not under the pass at all: `connect` spawns `run_supervisor`, a task the runtime's scheduler polls from `block_on`, beside the serve. What the table cannot size is the dependency code under that login dial (the TLS and WebSocket handshakes, unoptimized like every other crate in a dev build), **so it is measured**: `conformance_account_plane_bind`'s `a_seed_pass_connects_to_the_linked_nest_inside_half_the_store_threads_stack` serves the linked nest over TLS on a loopback port with a self-signed certificate and gives the app the production connector (`fauna_client_account_runtime::native_linked_nest_connector`), so a seed pass's store thread dials it for real. The stack pages under the connector are handed back to the kernel before it is polled (`madvise`) and the lowest one present again is read when it returns (`/proc/self/pagemap`; linux only), so the reading is the connect's own, dependency frames included. On a debug build (aarch64 linux) the connector is polled 1,321,376 bytes below the thread's entry, and the connect — the login mint over an anonymous connection (its TLS and WebSocket handshakes, the channel binding against the self-signed certificate, the silent challenge), then the bound-identity read — touches down to 1,723,167 (to the page): 401,791 bytes under the connector, the 100 KiB above included. The leg's requests over that connection are then made at 1,453,200. So the dial is the deepest thing the linked leg does, and at this tier it ends 365 KiB inside std's 2,048 KiB default: deep, and not by itself what took the linux app over. **Guarded at the Rust tier**, in `fauna_sync_engine::account_runtime::tests`: `a_pass_runs_inside_half_the_store_threads_stack` drives the real thread through a full pass and a seed pass against the suite's fake nest, which records how far below the thread's entry each request is polled (1,834,320 and 1,268,320 bytes on a debug build, no linked nest), and asserts half of `STORE_THREAD_STACK_BYTES` — red-verified with the constant at 3 MiB, so returning it to the default fails here; `the_store_threads_futures_stay_small` bounds the two futures at 64 KiB (red-verified at 32 KiB). The second is the form [`apps/native-async-execution.md`](apps/native-async-execution.md) § The rule prescribes and it does not see this failure: with the seed pass's `Box::pin` removed both futures measure the same and a full pass runs 334,624 bytes deeper (2,168,944), which only the depth test reads. **The linked-nest leg is guarded beside them**, in `bins/fauna-nest`'s `conformance_account_plane_bind`: `a_seed_holder_beside_a_seedless_engine_holder_completes_and_custodies_the_linked_nest` runs a seedless engine holder and a signed-in app on one store dir over real in-process nests, each of which records how deep in a store thread's stack its requests are polled (`fauna_sync_engine::account_runtime::store_thread_stack_depth`, which the `test-helpers` feature lends other crates' tests — no ordinary build marks the thread's entry). Only the app's connector reaches the linked nest, so that nest's reading is the seed pass's linked leg alone: 1,459,344 bytes on a debug build, and 1,880,624 at the bound nest; both are asserted against the same half of `STORE_THREAD_STACK_BYTES` (red-verified with the constant at 2 MiB). The real connection's three readings above are asserted against it too (red-verified with the constant at 3 MiB: the connect's reading alone goes over 1,572,864, and nothing overflows). The reading is taken where the request is made, so the in-process handler under it, which no app runs, is not in it (the page probe's 1,712 KiB above included it). **Not covered by any of the four:** a linked box with a public-CA certificate, whose dial verifies a WebPKI chain where the measured one verifies a channel binding, and a local command served inside a pass, which runs `serve_local_cmd`'s 584,736-byte frame beside the pass under the pass driver, where no nest request is made — measured once with the same page probe, a preference write landed inside a held prologue against the fake nest: 1,928 KiB, against 1,892 KiB for that prologue without it. Half the stack is the allowance for both. **The lever if the depth ever matters:** split the three long `async fn`s' arms into functions of their own (a frame then lives only while its arm runs), and box `worker`'s future into `block_on`.
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
  transfers when the holder exits; the takeover pass is the new holder's
  catch-up. `PumpReport::skipped_non_holder` is the in-band role answer.
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

- **Built — W5.2 the `data_version` notification floor (2026-08-14), and the
  bridge lost-update fix it surfaced.** The floor: `StoreBackend::data_version`
  (default `None` = "this medium has no counter", never "no change") →
  SQLite `PRAGMA data_version` in the native backend →
  `AccountStoreHandle::data_version()`, moving iff **another** connection
  committed — a runtime's own writes, its pump included, never move its own
  reading, so a single-instance deployment polls forever in silence. tui (the
  lead app) consumes it per the poll-with-poke posture, through the one shared
  store-change watch since 2026-10-02 (the *own-pump notice source* entry
  below): `session::account_store_watch` posts a payload-free
  `AccountStoreChanged` only on movement; the handler
  re-drives the OPEN surface's own load (`settings::store_resync_op` for the
  store-backed sub-pages; the feed's reload for its sealed scorers) — reload
  semantics, no new render path. Proven: the tier_1 iff-pin (own write never
  moves it, a sibling connection's commit does), the watch's change-detect
  pin (first-reading seeds silently; `None` neither fires nor clears —
  `fauna_account_seams::store_change`'s `the_floor_fires_only_on_a_real_move`), and
  tier_3 `conformance_account_runtime` V7 (a sibling's write moves the
  counter and is served without restart; own passes move nothing;
  symmetric). The poke fast path (agent IPC store-changed; web
  BroadcastChannel) stays W5.5+/W6.
  **The fix (same day, found by this slice's verification): an ABSENT blob
  has no freshness.** `bridge_tick` fed `decide` the absent rail's
  `default_user_config().updated_at` — a fabricated **now()** — so a replica
  bridging inside a sibling's `put_preference` two-leg window imported the
  DEFAULT preference at a manufactured newer stamp and published it,
  silently reverting the account's first save fleet-wide (caught as the V1
  conformance lost-update flake, diagnosed via the harness's new
  `eventually_or` post-mortem). The rule now lives at the load site: absent
  → epoch 0, so a present plane entry always mirrors out — which is exactly
  the documented crash-between-the-legs healing. Red-verified:
  `an_absent_blob_is_healed_by_mirror_never_imported_over_the_plane`
  (imports the empty default with the defect restored; mirrors with the
  fix).

- **Built — a store transaction that writes takes the write lock at its start (2026-10-01).** § Multi-instance concurrency (W5) → *Store contract*. **The defect:** every multi-statement write in `fauna_account_store::sqlite` opened a *deferred* transaction, and the guarded ones — a local state put, a local journal append, the group-state put, the record add — read the writer identity before their first write (`writer_guard_sync`). That read makes the transaction a reader, and SQLite does not run the busy handler for a reader asking for the write lock: with another same-account process holding the lock, or having committed since the read, the insert answered `database is locked` at once, the 5-second `busy_timeout` never consulted. So an app's local write — a preference save — could fail outright because a second instance on the machine was mid-commit. Found as tier_3 `conformance_account_runtime` V7 failing at the sibling's first put while the holder's first pass was still committing (pinned to one core: 7 of 20 whole-suite runs, 1 of 200 runs of the test alone). **The fix:** `SqliteBackend::write_tx` opens `BEGIN IMMEDIATE`, and every writing transaction of the backend opens through it; the one read-only transaction (`meta_get_all`) stays deferred, so a reader still never queues behind a writer. The writer guard and the compare-and-delete no longer lean on a snapshot conflict to refuse a concurrent re-stamp — nothing can commit between their read and their write. The sync database (`fauna_account_store::db`) needed nothing: each of its transactions writes first. Proof: tier_1 `sqlite::tests::a_guarded_local_write_waits_behind_a_siblings_write_lock` (a second connection holds the write lock until the put's busy handler has run; red-verified — refused with `database is locked` on the deferred form). **The defect the refusal had been hiding — two values at one entry version.** `AccountStore::put_state` (and `ingest_state`, and both group twins) read the entry's version, added one, and only then opened the write; nothing re-checked it. Two instances writing one `(kind, key)` could both land version *n*+1 under two journal rows, the entry keeping the later value. The walk then met its own writer's earlier row, found the entry at that row's version holding other content, and read it as the burnt-journal signature (`account-replica-posture.md` § The store device principal, refinement 11): the writer was marked burnt and the engine holder rotated onto a fresh key, leaving the sibling holding the retired grant. While the overlapping write was refused outright this needed the narrow window between the read and the `BEGIN`; once the write waited instead, tier_3 V9 (two cold assemblies on one store dir) showed it in about 6 % of whole-suite runs pinned to one core. **The fix:** the entry-and-row writes verify the version inside their own transaction, on every backend — `StoreBackend::state_put_with_row` and `group_state_put_with_row` answer `InsertOutcome::EntryMoved` and write nothing unless the entry is at exactly one below (`physical::entry_moved`; SQLite, the memory double, IndexedDB) — and the four store-level callers re-read and retry, as they already did for a lost seq. Proof: the backend-generic conformance case `an_entry_moved_since_the_read_refuses_the_pair_on_either_plane` (two handles on one medium; red-verified on the native arms with the check removed).

- **BUILT 2026-10-02 in shared Rust and on tui, 2026-10-03 the UniFFI face, linux and android, 2026-10-04 web's holder tab — the own-pump notice source; a web tab that hosts no runtime, windows, macOS and iOS not yet.** § Multi-instance concurrency (W5) → *A runtime's own pump is a source of the notice too*. **The count:** `AccountStore::entry_changes` (`fauna-account-store`), bumped by the entry-write doors when a read now answers differently — `put_state`/`ingest_state`/`put_group_state`/`ingest_group_state` when the entry is new or its value or tombstone changed (a re-put of the held value moves only its version), `forget_state` of a held entry, `stage_local_record`, `note_record` of a new or different index row, `apply_tombstone` of a known record, a segment adoption that indexed blocks, a `drop_scope` that dropped anything; journal rows alone, frontiers, watermarks, relay rows and the outbox never count. It is this store value's own: another connection's commit moves `data_version`, never it. **The generation:** `PumpCycles`' change generation, read as `AccountStoreHandle::change_generation` and awaited with `changed_after`, measured in `account_driver::drive::drive_pass` — the one driver every run goes through (`contained_pump`'s prologue, reconcile-now, reconnect, ticker, publish step and seed pass, and the nudge's walk, which calls `drive_pass` directly): the count is read before and after the run, what each local command served INSIDE the run wrote is measured on its own and taken out (part 2: a gesture is never the run's), and a run that changed anything moves the generation once at its end, however it ended. **The watch:** `fauna_account_seams::store_change::StoreChangeWatch` (re-exported as `fauna_client_account_runtime::store_change`) joins the generation, the floor polled every `STORE_CHANGE_POLL_INTERVAL` (10 s; the first reading seeds silently, a `None` neither fires nor clears) and an optional poke (`with_poke`, unwired — web's row feeds it), answering `changed()` until the floor's read errs; the two conversation seams' watchers (`read_positions`, `contact_overlays`) consume it and their private loops are gone, so they now wake on a changed run rather than on every pass. The overlay seam's one reliance on quiet passes — retrying a fold the store refused — became the fold writer's own retry on the floor's cadence. **tui:** `session::account_store_watch` consumes the watch and posts `DataMessage::AccountStoreChanged`, whose handler re-drives the open Muted words, Task delegation, Folders or Nests sub-page (and the feed); checked against the level rule, the Muted words load no longer clears the draft or the busy flag — a mutation's own outcome does (`MutedWordsMutated`). **Proofs** (`bins/fauna-nest/tests/conformance_account_runtime.rs`, each red-verified): `own_pump_a_siblings_row_applied_by_this_holders_walk_moves_the_generation` (the nudge's walk), `own_pump_a_successors_carry_of_its_predecessors_rows_moves_the_generation`, `own_pump_a_quiet_run_moves_nothing` (a pass meeting a `kept` row and its own `self_echo`, and an empty pass), `own_pump_a_gestures_own_write_and_its_publish_move_nothing` (beside and raced into a pass), `own_pump_a_non_holders_generation_never_moves` (a non-holder with no seed-leg role); the seed pass in `conformance_account_plane_bind.rs`'s `a_seed_holder_beside_a_seedless_engine_holder_completes_and_custodies_the_linked_nest` (moves the generation, no pass counter); end to end, `tests/e2e-unified/tests/test_store_change_notice.py` (two tui seats: a word added on one appears on the other's OPEN Muted words page with no re-visit). **The UniFFI face (2026-10-03):** `fauna-ffi`'s `store_change` module — one payload-free foreign call, `FfiStoreChangeListener::store_changed`, registered once per process with `set_store_change_listener` (the registration outlives sign-out and account switch; a notice with no listener is dropped); the relay is spawned at the runtime's store edge (`account_runtime::on_store_installed`) and ends with the watch, so every UniFFI app hears both sources with no per-app start or stop call. Proof: `account_runtime`'s `a_store_change_reaches_the_registered_listener` (a real store's change reaches a registered listener through the same edge `install` runs; which source woke it is the watch's own proofs' to pin). **linux:** `store_surfaces::watch`, spawned where the runtime is installed, posts a payload-free `DataMessage::AccountStoreChanged`; the handler asks `store_surfaces::open_surface` which store-backed surface is showing and re-drives its own load — the feed's current query, or the open Muted words, Task delegation, Folders, Devices or Nests sub-page through the settings shell's `store_resync`, an exhaustive match over `StoreSurface` running exactly what re-selecting the showing page runs. Checked against the level rule: the Muted words load no longer clears the draft or re-enables the add button (a mutation's own outcome does), and Muted words and Task delegation paint nothing when the answer is what they show. Proofs: `store_surfaces`' `only_the_open_store_backed_surface_is_named`; end to end, `test_store_change_notice.py` with linux as the open seat. **android:** `ApiClient.storeChangedTick`, fed by the listener registered before the runtime starts, is collected by each view model whose load reads through the store — Muted words, Task delegation, Nests (the pairing list, the custody fold, the escrow holders), trained topics, Devices and Folders (the machine's refresh and the enrollment notice) and the feed (`refreshCurrent`, the sealed scorers) — so only a live screen re-reads; drafts are the screens' own state and an unchanged record conflates. Proofs: `MutedWordsVMTest`, `DevicesVMTest`'s `aStoreChangeTickReReadsTheStoreBackedHalvesAndNothingElse`; no e2e journey — the android emulator runs only on the emulator host, so the mechanism is pinned by the relay proof above and the view-model tests. **web (2026-10-04, the holder tab):** `fauna-wasm`'s `accountStoreChangedAfter(seen)` — a Promise the caller re-arms with the count it was handed, the shape of that file's other exports — answers from `StoreChangeLevel` (`fauna-account-seams`' `store_change`), the watch as a counted level: one relay drives the one watch and counts its notices, so a notice landing between two waits is never lost and a burst reads as one; it is seeded where the tab's runtime starts and ends with it, a pending wait resolving `undefined` at the stop. The SPA's shared core runs one relay per started runtime (`$lib/store-change`, started by `$lib/account-runtime`) over a listener set the open pages join with `onStoreChange`: Muted words (a re-read that replaces the list only — never the draft — and replaces nothing when it fails or a gesture started since), Task delegation (skipped while a pick is in flight), Folders and Devices (each the periodic re-read's own body, run at once; Devices also re-folds its custody facet, which on web is a fold only and never drives the ceremony), the Nests page's custody facet, and the feed (`refreshCurrentFeed`, the reconnect arm's own re-drive). Only the tab that hosts the runtime hears it. Proofs: `conformance_account_runtime.rs`'s `own_pump_the_counted_level_moves_on_a_changed_run_and_ends_with_its_owner` (red-verified); `apps/fauna-web/src/lib/store-change.test.ts`; end to end, `test_store_change_notice.py` with web as the open seat, and `test_identity_succession_aftermath.py`'s `test_the_successors_open_muted_words_page_paints_what_it_inherited` (the successor enters Muted words once, right after the actor id switches, and the inherited word paints with no re-visit). **Not joined yet on those three apps** (each still reads fresh on its next visit): on web Personalization's trained topics (its load's failure path writes the page error past the publish sheet's precedence), the Folders default-conflict-policy select (a failed re-read resets it to automatic), the Nests page's linked rows (their hydrate is the machine's own nest round trip), the member-review page and the backup destinations; on linux the filter list's inherited-rule marks, the member-review page, Personalization's trained topics and the backup destinations — their loads have not been checked against the level rule; on android the Devices custody facet (its load drives the ceremony before it folds, and a notice must not become a drive), the Folders sync-defaults select (a failed re-read would reset it) and an open conversation's muted-keyword list. None of the three has a reporter-side hide list to re-read. **Not built:** any notice in a web tab that hosts no runtime — a second tab of the same account (no floor — IndexedDB has no counter — and no BroadcastChannel poke, so `with_poke` stays unwired); any notice on windows, macOS or iOS, which consume neither source though macOS and windows already run concurrent instances. **What a user sees there until it is:** a store-backed page that is open while the app's own pump changes the store keeps its old paint until the next visit — another device's muted word does not appear, and a successor who opens Muted words in the first seconds after signing in sees an empty list (measured on web before its holder tab consumed the notice: the list still empty 90 s after the inherited words had landed on the store). The succession journey (`test_the_successor_can_still_read_the_config_it_inherited`) waits by re-visiting the page for that reason, and stays the journey for the apps not built.

- **Built — W5.3 the migration/adoption critical section (2026-08-14): the
  second of the two exclusive sections is code, and concurrent cold opens
  found three real defects on the way.** `fauna_account_store::locks`
  (renamed from `engine_lock`, since it now owns both of the charter's
  locks) gains `MigrationLock`: the same kernel-arbitrated file lock on a
  *separate* reserved name `<store dir>/migration.lock`, taken **blocking**
  rather than try — the role is won, but migration is passed through, so
  waiting is the point. Two files, not one, because a single lock would make
  every cold open wait on the engine role, which is held for the role's whole
  lifetime. `SqliteBackend::open` holds it across its **entire** body and
  releases before returning; a live store handle never holds it.
  What concurrent cold opens actually broke, each fixed here:
  1. **The WAL conversion failed outright** — `PRAGMA journal_mode` does not
     consult the busy handler, so a racing opener got `database is locked`
     immediately *with* `busy_timeout` set. This is why the section starts
     before the connection, not merely around `migrate()`.
  2. **The at-rest format pair was written as two transactions**, and
     `AccountStore::open` refuses to guess at half a pair — so the loser read
     a perfectly good store as corrupt and bailed. The new one-transaction
     pair write (required of every backend; since the fix below it is
     `StoreBackend::meta_put_pair_max`, also rising-only) closes the window;
     adoption runs after the backend's section has been released, so the
     lock could not have covered it.
  3. **Two cold assemblies each minted their own writer key** — the credential
     slot's mint-or-load is a probe-then-act on state every co-located process
     shares, and the second assembly's store then refused it forever
     ("belongs to a different writer"): a store that app could never open
     again, the client-state-recoverability failure shape. `account_runtime`
     now resolves the writer key inside the same store-dir section.
  Proven: tier_1 in `locks` (blocking-not-refusing, the two files not
  contending, the reserved names) and in `sqlite` (four concurrent cold opens
  rebuild **exactly once** — counted, because the rebuild's *result* is
  idempotent and cannot answer "how many ran"; a dead migrator's store
  completed by the next opener; a second connection never observing half a
  pair, red-verified deterministically), plus tier_3
  `conformance_account_runtime` V9 — two runtimes assembling at the same
  moment on one cold store dir, both coming up on a store a later assembly
  re-opens. **Not built (the rest of W5):** the windows leg (T9's named
  mutex vs. `LockFileEx` on these same files — one call for both locks now),
  the web Web-Locks leg, and W5.4 onward. **The at-most-one-instance law is
  still in force**; with W5.1–W5.3 proven, W5.6 is what retires the
  app-level refusal.

- **Fixed 2026-10-01 — two more concurrent-cold-open defects, both a two-step READ that a sibling's write landed inside.** The store's adoption runs after the backend's migration section is released, so a co-located sibling writes during it; the entry above made those writes whole, and these two reads were still assembled from two instants. A real app reaches both: two processes assembling at once on one cold store dir is the app beside its agent at first sign-in, and the loser's assembly failed (recoverable by relaunch, never a stranded store). **(1) The format pair was read half.** `store::verify_and_stamp_format` read `format_version` and `min_reader_format_version` as two reads; a sibling's whole stamp between them read as *(absent, present)* and the open refused a good store as "half a version pair". It now reads the pair through `StoreBackend::meta_get_all`, one transaction. Pin: `sqlite::tests::the_open_reads_the_format_pair_as_of_one_instant` (the sibling's stamp lands inside the read's window). **(2) The lost-slot heal missed the stamp it lost the race to.** The assembly that LOADED the key its sibling had just minted enters the heal on the loaded-over-unstamped shape ([`account-replica-posture.md`](account-replica-posture.md) § The store device principal, refinement 11, arm (a)), which tells a mint in flight from a lost journal by two facts: the store's stamp and the slot's unstamped-mint marker. It read the stamp first and the marker second, while the sibling's open stamps the store and then spends the marker — so both writes fit between the two reads, the picture was *no stamp, no marker*, the heal minted a second key into the slot and `retire_unjournaled_writer` refused at the stamp it had just missed. `principal_succession::lost_slot_heal` now reads the marker first: a marker read as spent means the stamp had already landed, and the stamp read after it sees it. Every host shares the function and the stamp-then-spend order (`fauna_sync_engine::account_runtime`, `fauna_account_plane::web_host`). Pin: `fauna_sync_engine::principal_succession::tests::a_sibling_stamping_a_cold_store_under_the_heal_is_adopted_not_reminted` (the sibling's two writes land inside the heal's marker read). Both pins were shown red against the old read order. **Measured by contention** (Linux, `conformance_account_runtime` V9 alone, 16 copies of the test process at once pinned to one core): before, 5 red in 800 runs — 4 the heal, 1 the half pair; after, 0 red in 8,000 (2,000 on that same core, 2,000 on each of three more).

- **Built — W5.4a the principal bundle's carriage (2026-08-14): the T10
  slot's three remaining items join the writer key, resolved inside the same
  critical section.** `fauna_sync_engine::principal_bundle::PrincipalSlot`
  owns the sibling account attributes under `CRED_NAMESPACE` —
  `<actor_id_hex>/device-auth` (the root-signed `DeviceAuthorization` as its
  hex-over-canonical-dag-cbor `EmbedAsBytes` wire, verified on load: envelope
  signature, this account, this store's writer key; unusable → warn +
  not-enrolled, healed by the next ceremony), `<hex>/backup-key` (persisted
  from the seed-derived value at every assembly — **write-mostly carriage on
  a seed-holding surface; its consumer is W5.5's seedless app-dead agent**;
  a mismatched slot value is overwritten loudly, the deliberate asymmetry
  with the writer key's refuse-don't-remint, because the derivation is
  definitionally correct where a re-minted writer would fork the store), and
  `<hex>/generation-keys` (the R14 retained keys — see below). All three
  resolve inside the W5.3 migration section at assembly, per its
  probe-then-act lesson. **The retained-keys custody honors the crypto-shred
  ruling**: `generation_tip::RetainedKeyCustody` (the trait face the
  ungated machinery consults) records a key at every obtain (the mint door,
  the seal-time `key_for_tip`, the walk's unwrap), answers where the plane
  cannot for a *live* mint (a re-synced store, a sealed row walked ahead of
  its writer's mint row, a top-up that raced enrollment — deliberately
  looked up only **after** the mint row, never blind-cache-first), and
  **drops the key at any observation of a `Shredded` mint** — at the walk's
  apply/merge and at the read — so a slot that carried the key can never
  quietly defeat "deleting a generation = devices drop it". The shred
  *calling surface* (not yet built) owes the same drop on the originating
  device; the peer leg's custody is ATTACHED (2026-08-15, row 46 — the
  *Built — the peer-leg dial pass* entry): both dial-side pull planes carry
  it, and the shred-drop-over-a-dialed-channel pin is
  custody-mutation-red-verified — closing the security review's
  watch (the interim sequencing note lives in this entry's history: W5.7's
  landing was the serve side, which relays sealed bytes verbatim under R6
  and never unwraps). Observable:
  `AccountStoreHandle::principal_bundle_status`. Proven: tier_1
  `principal_bundle` (round-trips, foreign-writer/tampered grant refusals,
  backup-key heal, sibling-merge without lost updates, drop-without-
  resurrection), tier_3 `conformance_account_state_walk`'s
  `the_retained_bundle_bridges_the_unkeyed_window_and_drops_on_shred`
  (record at seal + unwrap, the bundle opening an unkeyed replica's rows
  with no top-up, walk-side and read-side shred drops — each seam
  mutation-red-verified), and the V9 extension (bundle resolution under
  concurrent cold assembly). **Built — W5.4b's ceremony half (2026-08-14):
  the enrollment ceremony is code.** The mint half runs at assembly, inside
  the same migration section (the seed is still in hand there;
  `fauna_client_sync::build_principal_grant` signs the `RenewBearer` grant
  over the **writer public key** — the principal IS the writer key, and the
  slot's loader refuses a grant over any other); the nest legs are the
  pump's retryable `ensure_enrollment_registered` step — `fauna.sync.
  register` under the writer-pub-hex device id (one `sync_devices` row per
  (machine, account), registered under the `SELF_REGISTER_LABEL` placeholder:
  no human has named the machine, and a labeled registration supersedes it)
  + `fauna.sync.device_grant.register`, latched content-addressed in the
  slot (`grant_registration_recorded` — a re-minted grant re-registers, an
  unchanged one costs no RPC; sign-out's `delete_namespace` clears latch and
  grant together). Proven tier_1 (grant-over-writer-key pin; the
  register-once-then-latch pin) and tier_3 (`conformance_account_runtime`
  V9: the racing cold assemblies hold ONE grant, the machine appears once in
  the devices list, the registered grant is findable by
  `fauna.auth.device_handshake`'s own lookup, and a later assembly loads —
  never re-mints — and answers `Current` off the latch). Pre-W5 agent rows
  converge (and the old agent grant row is deleted) in W5.5, not here —
  superseded 2026-09-28: under the one-credential ruling there is no second
  credential to converge (`apps/sync-agent-credentials.md` § Credential
  model, the RULED 2026-09-28 block; built 2026-09-29).

- **Built — W5.4b's bearer half (2026-08-14): the runtime's nest leg
  authenticates as the store principal — W5.4 is COMPLETE.** The W3-era
  "app's own session" auth is retired: `AccountRuntimeParams` gains
  `process_rpc`, the principal-authenticated requester every data leg
  (planes, walks, outbox, config) rides, while `rpc` — the app session —
  carries only the ceremony's two registration legs (they must ride an
  already-authenticated session; the grant they install is what the
  principal's connection authenticates with). The client:
  `fauna_client::ws_device_handshake_bearer` — a `BearerSource` minting
  over `fauna.auth.device_handshake` with the **writer key** (no identity
  keypair anywhere in it, `AuthClient::bearer_only`), composed by
  `device_principal_nest_client`; the writer key comes from the sanctioned
  pre-assembly resolver `resolve_writer_key_serialized` (mint-or-load under
  the migration section — never read the slot unserialized, the W5.3 rule).
  On a fresh machine the device client's first connects lose to the
  ceremony by design — `spawn_connect_retry` brings the connection up, and
  the pump's per-pass retry absorbs the window. tui (the lead app) wires
  it; `process_rpc: None` keeps every unmigrated caller byte-identical.
  Proven tier_1 (two independent fakes pin the split exactly — no data leg
  on the app session, no ceremony leg on the process requester) and tier_3
  (`conformance_account_runtime` **V10**, a real *listening* nest + two
  real `NestClient`s: ceremony over the app session → device-handshake
  mint against the registered grant → a fully clean pass over the
  principal-authenticated connection; the sessions list then shows both
  per-process sessions and the devices list one machine). Per-process
  reconnect gaps ride the backstop; the app session's reconnect wake stays
  the push-reset signal.

- **Built — the retained-keys carriage is bounded (2026-08-14): the set is trimmed to fit one credential item before it is
  written, and a failed write no longer claims it will heal.** R14 asks the
  bundle to carry "current tip + every generation still held for reading"
  (`owner-key-material.md` § Path A-sibling-2 → *bundle carriage*) — an
  unbounded set — while the value it serializes into is ONE credential item,
  and the tightest backend caps that hard: Windows Credential Manager stores
  a `CRED_TYPE_GENERIC` blob of at most `CRED_MAX_CREDENTIAL_BLOB_SIZE`
  (2560 bytes), with a best-effort-and-log write that reports nothing to the
  caller. **The stated bound is therefore
  `fauna_credential_store::MAX_ITEM_VALUE_BYTES` = 2560 bytes on every
  platform** — one number everywhere rather than a per-OS surprise found
  only on Windows (priority #1) — which **measures at 296 hex bytes per
  retained generation, so 8 generations** (measured by
  `the_retained_carriage_is_bounded_by_the_credential_item_cap`, not
  computed: canonical dag-cbor writes each `[u8; 32]` as a CBOR *array*
  whose every byte ≥ 24 costs two, so a byte-string reading overestimates
  capacity by nearly half). That test fixes its ids and keys rather than
  minting them, deliberately: with random keys the encoded size — and so the
  capacity — is a distribution rather than a number, and the `keep` pin was
  observed missing its own mutation a third of the time before the sizing was
  made worst-case. `persist_retained` trims in generation-id order
  until the encoded value fits, never dropping the entry whose obtain
  motivated the write, and warns per dropped generation. **Ordering is
  deterministic but carries no meaning** — generation ids are content-derived
  hashes and every retained generation is by definition still readable — and
  that is deliberate: a real retention policy needs a rotation cadence to
  size it, and only trigger (a) first-need mints today, so **the cap is
  unreachable in shipped behavior and whoever builds trigger (c) owns the
  retention question**. The trim exists so the day it becomes reachable it
  degrades loudly and recoverably rather than silently. Recovery is real: a
  trimmed key is re-obtained plane-natively (mint wraps / top-up); what is
  lost is the offline bridge for a store re-syncing from scratch and for
  W5.5's seedless agent, which has no other source. The read-back-failure
  warning no longer says "until the next obtain re-persists it" — the next
  obtain re-runs the same write with a set no smaller, so the condition is
  permanent until the set shrinks or the slot is re-enrolled, and it now says
  so. Custody, same commit: every read of a **secret** slot value
  (`backup-key`, `generation-keys`) goes through one
  `read_secret_slot_value` returning `Zeroizing<String>`, pinned at compile
  time by `_SECRET_SLOT_READ_IS_ZEROIZING` — `load_retained` had been
  dropping the hex of every retained key as a bare `String` while its sibling
  `resolve_backup_key` wrapped the identically-classed read
  (`key-material-hierarchy.md` § Carrier shape; the same class as two earlier
  misses, and routing all four reads through one function is what stops
  the family splitting again). `device-auth` deliberately stays unwrapped: a
  root-signed authorization is public, signature-verified capability, not
  secret material.

- **Built — the store's `min_reader` floor law (2026-08-14):
  the version pair restamps monotonically on every compatible open, and the
  breaking verdict runs before migrations mutate.** The floor was stamped
  only at store creation, so the first non-additive bump would have refused
  a store this binary *created* while admitting every store already in the
  field — and the fresh stamp was an unconditional upsert outside every
  lock, so a version-skewed cold race could pull the pair down. Now
  `AccountStore::open`'s compatible arm restamps **both** numbers up on
  every open (`version-compatibility.md` § 2.2's restamp-at-every-run law;
  the retired `UserConfig` blob's `max` idiom), the write itself is rising-only per key
  (`StoreBackend::meta_put_pair_max` — the nest `record_schema_meta`
  guarded-upsert idiom, one transaction, numeric compare; racing skewed
  binaries converge to the per-key max in any order, and a shipped pre-fix
  binary's low stamp is healed by the next current-binary open), a
  compile-time `assert!(MIN_READER_FORMAT_VERSION <= FORMAT_VERSION)`
  guards the constants, and `SqliteBackend::open` refuses a newer-breaking
  pair **before** the WAL conversion or `migrate()` touch anything (§ 2.2's
  check-before-mutate placement — today's migrations happen to no-op on a
  future-format store; the next one need not). `NewerAdditive` stays
  leave-alone in both directions (the nest posture). Proven tier_1, each
  pin mutation-red-verified exact: the floor rising on an upgrade-in-place,
  a racing lower stamp refused (including the "10" vs "2" numeric-compare
  leg), and the pre-migrate refusal with the untouched-store canary.

- **Built — W5.5a the seedless host (2026-08-14): a process holding no
  identity seed can mount and run an already-enrolled account store,
  which is what lets the sync agent be the always-on engine singleton
  (R2).** `AccountRuntimeParams::actor_keypair` becomes `principal:
  RuntimePrincipal` — `SeedHolding(keypair)` for the 7 apps, `Seedless`
  for the agent — an enum rather than an `Option` so the fork names
  itself at every call site. The seed reached four assembly legs and
  **only two of them genuinely needed it**: the `BackupKey` is now read
  from the T10 slot's `<hex>/backup-key` (the carriage W5.4a persists
  for exactly this consumer) instead of derived; the enrollment grant's
  mint and the fleet bootstrap rows are **skipped, not faked** (both are
  root-signed or seed-derived, so they belong to a signed-in app and heal
  at its next assembly — the contract those legs already had); and the
  `__config` client needs no secret at all, because the at-rest blob
  carries no signature envelope (§ *The store device principal* — the
  seal is `BackupKey`-only, `fauna_client_config::seal_user_config_with_key`;
  the keypair the older API takes is a Slice-B forward-compat parameter
  it ignores); that `__config` leg retired with the rail on 2026-10-02
  (`config-dissolution.md` § The `__config` dissolution schedule → *The closure order*, step (6)).
  **The refusal is the load-bearing half.** A seedless assembly against a
  slot carrying no `BackupKey` **fails outright** rather than proceeding,
  and a seedless resolve writes nothing back to the slot. Both follow from
  one property: sealing under a wrong-but-well-formed key *succeeds*, so a
  host that guessed would write ciphertext the account's own owner could
  never open — every row, silently, with no error anywhere — and a host
  that healed from a guessed key would lock every app out of the real one.
  That is the client-state-recoverability law (`nest/common.md`
  § Client-state recoverability), not an ergonomic choice; the recovery is
  "sign in on this machine once", which the refusal names.
  Proven tier_3 (`conformance_account_runtime` **V11**: an app enrolls and
  writes, shuts down, a seedless runtime mounts the same store dir and
  slot, reads the app's row, writes its own, completes a clean pass, and a
  seed-holding client opens the result — both directions, since a
  read-only host is useless and a write-only one is corruption; **V11b**:
  the unenrolled-machine refusal) and tier_1 (the carriage read, the
  absent/corrupt decline, the seedless no-write rule, and the `__config`
  seal's own both-directions round trip). **The hosting half of W5.5b
  landed 2026-08-15 (next entry); the T11 grant convergence is still
  owed.**

- **Built — W5.5b's hosting half (2026-08-15): the sync-agent PROCESS
  mounts the shared account store and is the always-on engine singleton
  (R2).** `bins/fauna-sync-agent` now takes `fauna-sync-engine`'s
  `account-runtime` feature, and its new `account_host` module runs
  beside the renewal and custodian loops — the same "must keep running
  app-dead" tasks — assembling `AccountStoreRuntime` with
  `RuntimePrincipal::Seedless` whenever a capability names an account,
  and tearing the mount down when that stops being true. Two properties
  are the substance:
  **(1) The root is the SHARED one.** The host resolves
  `StoreRoot::platform()` — the per-user root every co-located app
  resolves — never a dir derived from the agent's own `--data-dir`. A
  private root would put two journals under one machine-shared writer
  key, and that divergence surfaces only as `AccountStore::ingest`'s
  equivocation refusal *after* both have published. This is precisely the
  gap W6's path unification closed, so the agent inherits it rather than
  re-opening it.
  **(2) Nothing here decides whether it pumps.** The W5.1 election inside
  `start` does: a running app keeps the role it holds and the agent comes
  up beside it as a plain reader/writer; when that app exits the kernel
  drops its `flock` and the agent takes the role on its next backstop
  tick. Priority stays **behavioral** (desktops *ensure the agent*), never
  protocol — the agent asserts no precedence, which is what T9 requires.
  The data path authenticates as the **store principal** (the per-process
  bearer minted over the writer key), not as the agent's capability
  bearer, which is T11's "one principal per machine" holding for this
  process too.
  Proven tier_3 by `agent_process_tier3::agent_process_hosts_the_shared_account_store_with_no_app_running`:
  a real seed-holding app enrolls the machine and shuts down, the engine
  lock is asserted **free** so no stale holder can satisfy the test, the
  real agent binary is spawned and provisioned, and the lock then becomes
  **Refused** — a kernel-arbitrated fact, observable from outside every
  process involved and impossible to fake with timing (convention 14: a
  named budget with a deadline poll, no settle-sleep). The same case
  asserts no store appeared under the agent's `--data-dir`. tier_1 pins
  the MLS-free seedless shape of the params the host builds.
- **Built — W5.5b's renewal convergence (2026-08-15): the agent renews as
  the STORE PRINCIPAL, which is T11's substance.** The agent's bearer
  self-renewal (`renewal.rs::signing_keys_to_try`) offers the machine's
  writer key from the shared T10 slot **first**, and its own pushed
  `SyncCapability.renewal_signing_key` second, trying each in turn per
  renewal. On every machine an app has enrolled under W5 the nest
  therefore sees **one principal per machine, not one per process** — the
  T11 claim — while `renewal_signing_key` drops to a **compat field** (and,
  RULED 2026-09-28, is retired outright together with the fall-through this
  entry describes: the one-credential ruling, `apps/sync-agent-credentials.md`
  § Credential model; built 2026-09-29, so this entry is history — the agent
  renews under the principal alone).
  **The order is a preference, never a switch, and that is load-bearing:**
  the principal's grant is registered by the *app's* enrollment ceremony,
  so there are real states where the slot holds a key this nest does not
  know (enrolled against another nest; the ceremony's nest legs not landed
  yet; the grant revoked from the devices UI). Falling through to the
  pre-W5 credential keeps app-dead sync alive through all of them, at the
  cost of one failed handshake per hour in the rare case — and a hard
  switch would have re-entered the silent-stop this whole mechanism
  exists to close. The slot read is **load-only**
  (`principal_bundle::load_writer_key`, added here): a consumer
  authenticates as a principal that already exists or not at all, since a
  minted writer key would be an identity no nest has a grant for *and* a
  second writer for a store no app has enrolled — the divergence W5.3
  measured. Pinned tier_1 four ways (preference order; the unenrolled
  machine still renewing on the pushed seed; an enrolled machine renewing
  with no pushed seed at all — what makes the field demotable; and neither
  credential yielding **no** candidate rather than a minted one), three
  mutations each redding only its own pins.
  **Still owed by W5.5b:** the legacy grant row is never retired, so a
  machine can still show two `sync_devices` rows. That step is **refuted
  as originally written** — `apps/sync-agent-credentials.md` § Credential model's ⚠
  Refutation owns why an app-side deletion is unsafe (it revokes and
  tombstones the key a possibly-not-yet-upgraded agent is renewing with,
  and on linux the post-upgrade pairing is exactly that) and hands the
  safe ordering to a later slice as a design question.

- **Built — W6's path unification (2026-08-15): one per-user account-store
  root, resolved once in shared Rust.** `fauna_account_store::root` owns
  `StoreRoot` + `platform_state_base()` (the per-OS table on the Placement
  bullet, § The account store), the agent's `SyncPaths::production_base_dir`
  delegates to it, and `AccountRuntimeParams` split `state_base` into
  `store_root` (the unified placement) + `config_replica_base` (the
  app-local `__config` replica read, deliberately unmoved then; since retired
  with the succession ledger's cut) +
  `legacy_store_bases` (pre-W6 dirs to adopt). tui — the only assembly
  site — passes `StoreRoot::platform()` and names `config_dir()` as
  legacy; its existing store is adopted **move-don't-recreate** inside the
  W5.3 migration section (`root::adopt_legacy_store_dirs`: liveness proved
  by the legacy locks + a checkpoint whose busy column is read — ⚠
  `wal_checkpoint` reports blockage through its result row, it does not
  error; a busy legacy store fails assembly typed rather than starting
  fresh beside a live journal), and the vacated location keeps the
  **refusing husk** (format pair `u16::MAX` → the store's own typed
  `NewerBreaking` refusal in every shipped binary — downgrade is
  refuse-don't-corrupt, never a silent fresh journal under the
  machine-shared writer key). Guards mutation-red-verified exact (husk
  stamp, busy refusal, move-not-copy, checkpoint-busy — each redding only
  its own pins); the racing-cold-adopters proof rides the W5.3 lock; the
  assembly-level proof (`a_pre_unification_store_is_adopted_moved_not_re_synced`)
  adopts against an EMPTY nest so a re-sync cannot masquerade as a move.
  **Retired 2026-09-25** by the compat-remnant sweep
  ([`compat-remnant-sweep.md`](compat-remnant-sweep.md) § Program 4, tranche
  B1): the adoption, the husk, `legacy_store_bases` and that
  proof are gone — no pre-W6 store exists to adopt.
  **This closes the ⚠ Gap below and un-gates W5.5b** (and the per-app W3
  `MembershipSource` wiring the cross-app queue holds). Still W6-owed,
  separate slices: the web IndexedDB/OPFS backend
  arm, and the per-app trickle-down as each app wires the runtime (each
  names its own legacy base in `legacy_store_bases` — the mechanism is
  built and generic).

- **⚠ Gap found 2026-08-14 while scoping W5.5b — RESOLVED 2026-08-15 by
  the W6 path-unification build (the *Built — W6's path unification* entry
  above); kept for the record. The store dir was
  APP-namespaced while the writer key is MACHINE-shared, so a second
  co-located runtime on one account was a journal-equivocation trap. It
  gated W5.5b, and was a trap for the next APP to wire the runtime, not
  only for the agent.** The two halves disagree today: the T10 credential
  slot is `fauna-account-store` — *not* app-namespaced, so every process on
  the machine resolves the **same** writer key (`account_runtime` module
  docs: "the store's writer identity must never change once minted — the
  journal's equivocation refusal is keyed on it") — while the store dir is
  whatever `state_base` the app passes, and tui's is deliberately
  app-namespaced (`~/.config/fauna-tui`, "so it never contends with the
  linux app's `~/.config/fauna`"). Two runtimes on one machine would
  therefore keep **two journals under one `WriterId`**, each with its own
  `seq`. Neither refuses at open — both stores record the same writer — so
  the divergence surfaces only once both have published, as
  `AccountStore::ingest`'s "journal equivocation: scope … writer … seq …
  already held with different content" on whichever replica walks the
  other's rows: a break that is remote-visible and reached *after* the bad
  rows exist, not a local guard that stops it.
  **Not live today, and that is the whole point of recording it:** tui
  (`session.rs`) is the only assembly site in the tree, so exactly one
  runtime exists per machine. The trap arms itself the moment a **second**
  surface assembles one — the batched per-app trickle-down (linux/apple/
  windows/android), or W5.5b's agent — and none of those sessions has any
  local reason to look at the writer-key namespace.
  **The fix is W6's own deliverable — "per-platform adoption + path
  unification" (the roadmap table above)** — so W5.5b sequences behind that
  piece of W6 rather than inventing a second answer: one per-user
  account-store root every app and the agent resolve identically (a
  hard-coded constant + artifact wiring, product-invariant bucket (1) — no
  human chooses it), plus a move-don't-recreate adoption of the existing
  per-app store dirs (alpha data: the no-user-data-loss invariant makes this
  a *move*, never a re-sync-from-empty). Until it lands, the safe rule for
  any app wiring the runtime is **one assembly per machine per account**.

- **Built — W5.8 the top-up self-heal pass, writer half (2026-08-15):
  `fauna.state.generation-wrap` finally has a producer.** The kind was
  read-only in practice since the R14 build — `generation_tip` opens top-up
  rows, nothing wrote any — which left the partition § The generation
  machinery's accepted-residuals bullet names ("can partition *reads* of its
  own subset-sealed rows until the top-up machinery (W5-era self-heal pass)
  … covers them") permanently open: a device that enrolled after a mint, or
  that a subset mint left out, could never open **any** row sealed under that
  generation, however long it synced. `fauna_sync_engine::generation_topup`
  is the pass, one pump step after the fleet walk beside the device-endpoints
  writer and for the same reason (a sibling's enrollment merged this pass is
  exactly the device that needs wrapping). For every live mint this device can
  key and every verified non-removed member lacking both an inline wrap and a
  top-up row, it publishes one wrap. Four quiet skips, each a contract rather
  than an optimization: a generation this device cannot key (someone else's to
  heal — the ordinary state of a fresh device, so never an error), a
  `Shredded` mint (the crypto-shred clause), a **removed** device (the kind's
  own "never targets a removed id", via `FleetView::wrap_targets` — the
  severance would otherwise be undone by the healer — asked again right before each heal's write rather than once at the pass's start, because the removal is a local command served at the pass's own yields), and ourselves. It needs
  no election even though the engine-singleton runs it: two healers produce
  different ciphertexts for the same key, LWW picks one, and either opens
  because the receiver's check is the mint's key commitment.
  Pinned tier_1 by seven tests, four mutation-red-verified exact (each redding
  only its own pin): the wrap opening under the *target's* KEM secret, the
  inline-wrap and existing-row idempotence filters, and the removal.
  **One honest note recorded because the mutation round produced it:** the
  `Shredded` arm in this module is a redundant short-circuit — deleting it
  does not red the pin, because `generation_tip::generation_key_for` refuses a
  shredded row itself. It stays as a local statement of intent, and the module
  says so rather than carrying an untested guard as if it were the enforcement.
  **The end-to-end is proven too, with no nest anywhere**
  (`fauna-sync-engine`'s `peer_leg_convergence` §
  `the_top_up_pass_un_partitions_a_later_enrolled_replica`): A enrolls, mints
  generation 1 over itself alone, and seals a real `GenerationTip` row under
  it; B enrols *afterwards*; the two converge over the peer leg; **the
  partition is asserted before the fix** (B holds the mint row and cannot key
  it, and A's row has not merged); A runs the pass; B walks, reconciles, keys
  the pre-enrollment generation and reads exactly the bytes A sealed. That
  last step is what makes "not just device-endpoints" true — nothing in the
  read path is kind-specific, the generation key was the only gate.

- **Hardened 2026-08-15 — *the healer that believes the vandal*
  (a security-review finding, filed the same day W5.8 shipped): the pass's
  two skip grounds were both
  unauthenticated, attacker-writable, and absorbing** — one forged row (an
  appended `MemberWrap` winning the mint join with the honest signature
  carried through, or a well-formed garbage-ciphertext top-up row at
  `LatestWins` with a fabricated stamp) permanently and silently partitioned
  a chosen device from a chosen generation, defeating the very bound the
  accepted-residuals bullet stated. The hardening makes suppression evidence
  **authenticated or ignored** (mechanism owned by the wrap kind's bullet, §
  The generation machinery): arm A believes inline coverage only for the
  signed `member_ids` set ∧ wrap-present (healing, as a bonus, the honest
  member a minter listed but forgot to wrap); arm B re-shapes the kind into
  per-healer cells with healer-signed records and a verifying-preferred
  `CrdtPerField` join, so a forged row can neither displace nor pre-empt an
  honest heal, and coverage counts only verifying rows by currently-verified
  non-removed members. The read path (`generation_tip`) now falls through
  from a refused inline wrap to **every** available top-up (opens are
  commitment-checked — trying all is pure gain). Compatibility is additive:
  legacy cells stay readable forever, healers dual-write the legacy shape
  while pre-hardening binaries remain in the alpha fleet, and both builds
  pick the same winner on honest data. Under active forgery during version
  skew, honest republication re-converges the **per-healer** cells only
  (corrected 2026-08-15 — the original sentence claimed the
  legacy cell too, falsely): the legacy cell ranks by the unbounded outer
  stamp, so a forged `i64::MAX` legacy row is permanent for the skew
  window's whole duration — which is precisely why legacy rows never count
  as coverage, and why the skew window adds no exposure beyond what
  pre-hardening binaries already carry (they are fully exposed to that
  class regardless). **Retired 2026-09-24:** the legacy cells, the
  healers' dual-write and the per-target courtesy row, by the
  compat-remnant sweep ([`compat-remnant-sweep.md`](compat-remnant-sweep.md)
  § Program 4) — every wrap cell is a per-healer cell now.
  Landed as both probes from the verdict plus three new coverage pins,
  **four mutations red-verified exact** (arm A's `member_ids` half →
  PROBE-366-A; the coverage signature check → the bad-signature pin; the
  coverage membership check → the removed-healer pin; the join's
  verifying-preference → the join pins at both `fauna-core` and the merge
  seam). Residual honestly restated in the accepted-residuals bullet:
  in-place corruption of a member's own inline wrap remains durable until
  the target-authored "cannot key" signal lands
. Verify-back: tracked for
  confirmation.

- **Built — row 41, the target-authored "cannot key" signal (2026-08-15):
  the residual's one durable case is retired.** The sixth
  machinery kind, `fauna.state.generation-unkeyable` (its bullet in § The
  generation machinery owns the mechanism + anti-churn bound):
  `fauna_core::generation::GenerationUnkeyableRecord` (target-signed
  Asserted/Satisfied, `verifies_at`, the evidence-list signing domain, the
  verifying-preferred decode-or-fail join) + the registry row and strict adoption
  arm in `fauna_protocol::merge_policy`;
  `fauna_sync_engine::generation_unkeyable` is the target pass (one pump
  step after the top-up pass — asserts on covered-but-unkeyable, retracts
  on re-keyability, squat-gated, byte-quiet on identical evidence), and
  `generation_topup` consumes signals (a verifying assertion clears both
  suppression grounds; the own-cell-hash gate bounds the answer to one
  fresh wrap per healer per assertion; `put_heal` and the signal writer
  both stamp `max(now, cell stamp + 1)` — clock-regression-proof, pinned
  timing-independently on both sides). Proven by fauna-core law tests, the
  registry/adoption pin, 6 target-pass + 5 healer-side tier_1 pins, and
  the no-nest end-to-end
  (`peer_leg_convergence::the_cannot_key_signal_un_partitions_an_in_member_corrupted_wrap`
  — the partition AND the healer suppression asserted before the fix, the
  once-per-assertion bound asserted before the retraction merges,
  byte-quiet on both replicas at the end); **nine mutations red-verified,
  each exact**. Find it via `git log --grep 'cannot key generation G'`.

- **Hardened 2026-08-15 — *the forgery that moved one field
  over* (a security-review finding from grading the fix above): arm A's
  inline-coverage read believed a `member_ids` claim nothing
  authenticated.** The earlier remedy said "the signed `member_ids`
  set", but the site verified no signature and no binding — "inside the
  signature" was where the field lived, not a check the code performed.
  A `BackupKey` holder (a removed device included) could write a whole
  forged `Minted` value at an honest key, naming a victim the honest
  mint never listed with a garbage wrap; the forged variant wins the
  mint join by byte order, and every healer then read the victim as
  covered — permanently, silently, even healers still **holding the
  key** in their retained bundles (the read path's custody fallback is
  reached only after the coverage read, which returned first). The fix
  is the same instrument the read path already applies: inline coverage
  is believed only when the **core binds to its own key** (the
  content-derived generation id recomputes — which authenticates
  `member_ids` fully on its own; `minter_sig` would add authorship this
  decision does not need). Deliberately not a whole-row skip: a
  custody-holding healer must still heal the generation the forged row
  denies. The honesty rider carried forward: a healer *without* the key
  in custody is not helped — the forged row denies the generation to
  every such device, which is the pre-existing accepted
  `Minted`-suppression class. PROBE-368-B landed as the pin
  (`probe_368_b_a_forged_core_does_not_suppress_a_custody_holding_healer`
  — both prerequisites asserted, the heal proven to open at the victim),
  the binding-check mutation red-verified exact. Same landing: the
  accepted-residuals bullet's attacker-set phrasing sharpened to the
  `BackupKey` gate, and the module's legacy dual-write comment aligned
  to the spec (no retirement is scheduled; the code no longer promises
  one).

- **⚠ Fixed 2026-08-15, found by the proof above — the peer leg could not
  serve the fleet scope at all, so the generation machinery could not
  propagate without a nest.** `fauna_peer_sync::admission`'s
  `AdmissionVerdict::admits_scope` shape-checked an `AllOfAccount` verdict
  against `state` **or** a well-formed content scope — a list written before
  the A5 partition existed, and never updated when `state-fleet` was added
  with the R14 build design. Consequence: every device-set row, mint, escrow
  receipt and top-up wrap was refused at the serve door on the peer leg, so a
  nest-less fleet could never learn its own device set or distribute a
  generation key — precisely what § The generation machinery forbids ("plane
  rows over any leg (nest-mediated or peer) … a distribution door would
  re-couple generation propagation to nest liveness, which R11 and the peer
  leg exist to avoid"), and what the fleet scope's own definition had already
  ruled ("custody grants and admission verdicts enumerate both strings as
  ordinary scopes"). The check now defers to `is_served_scope` — the one owner
  of *which account-state scopes exist* — so the next sibling scope cannot
  drift the same way. Pinned tier_1 in `admission.rs` and mutation-red-verified
  both there and at the end-to-end.
  **Rider, also fixed:** `peer_leg_convergence` was failing **5 of 5**
  whole-file runs on `main`, with a different victim test each time, which had
  read as load flakiness. It is a startup race:
  `PeerNode::start_with` calls `transport.listen()` *inside* the accept task it
  spawns, so `start_peer_sync_node(..).await` returns before the listener has
  registered and the next `dial` answers `NoPath`. The harness now waits on
  that registration as a causal barrier (convention 14), and the suite is
  5-of-5 green. Worth knowing beyond the test: **a started node is not yet a
  listening node** — any production caller that dials immediately after
  starting one inherits the same race.

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

- **Built — W5.7 the peer-leg assembly seam (2026-08-15):
  `fauna_sync_engine::peer_leg` + tui's first `fauna-iroh` dep.** The runtime
  brings the same-account listener up itself, as a pump step (holder-only by
  placement — "the engine-singleton speaks for its store"), when four gates
  open in order: a transport factory in
  `AccountRuntimeParams::peer_transport`, the T10 slot's enrollment witness
  (`PrincipalSlot::device_authorization` — its wire IS
  `PeerSyncServerConfig::own_witness`), and the `peer-sync` brake, whose
  evidence is `fauna.nest.info` over the pump's own requester **cached in the
  store's meta table** (`peer_leg/nest_facts`) so an offline start binds from
  the last-known advertisement — no evidence at all refuses by default. The
  bind's observed facts (bound LAN candidates × interface addresses; the
  nest-advertised relay URL) fill the same slot `set_endpoint_facts` feeds
  (the app-fed Cmd deliberately wins as the explicit override), and the
  device-endpoints step publishes them on the same pass — dial candidates
  join the `node_id`-only floor row. `public_addrs` stays empty: reflexive
  observation needs the relay leg, and an honest absence beats a guess.
  **Two ruled details were refined at build time (the refutable-until-W5-code
  discipline; `peer_leg`'s module docs carry the full record):** the seam
  parameter is a **factory** invoked with the assembly's own resolved writer
  key — never a pre-built `Arc<dyn PeerTransport>` — because the machine's
  one NodeId (R5: the shared writer key since W5.4) must never be
  constructed as an endpoint in a non-holder process, and because the writer
  key resolves *inside* the W5.3 migration section, after params; and the
  ruled `Option<DeviceAuthorization>` witness parameter is refined away —
  the W5.4a slot is the witness source, and a second hand-fed witness could
  only agree with it or silently diverge. tui passes a one-closure factory
  (`IrohTransport::builder(writer_key)`, loopback-free default bind); the
  agent deliberately passes `None` until the dial-pass track wires it.
  Proven: tier_1 facts-composition + brake-cache pins (`peer_leg`); tier_3
  `conformance_account_runtime` § V12 (the elected runtime binds over the
  seam double, its candidates join the floor row, a root-signed sibling
  witness admits and walks a preference row out, a stranger witness is
  refused) + § V12b (brake-on keeps the factory un-invoked and no listener
  registered; the cached advertisement covers an offline restart; an
  evidence-less store refuses); and the real-substrate twin
  `fauna-iroh/tests/peer_leg_assembly_over_quic.rs` (the factory-built
  listener admits + refuses over a real QUIC handshake that proves the
  slot-witnessed NodeId). The dial pass, facts refresh, relay wiring, and
  agent factory this entry used to state as not-built are BUILT — the
  *Built — the peer-leg dial pass* entry below owns them.

- **Built — the peer-leg dial pass (2026-08-15, row 46):
  `fauna_sync_engine::peer_leg::dial_pass` — the pull half; the peer leg is
  now end-to-end in production code.** A pump step directly after the
  ensure step (holder-only by the same placement), running **only while the
  leg is bound** — bound implies elected + enrolled + brake-off, so rule 7's
  client gate covers dialing for free and a factory-less caller never dials.
  Per pass, per sibling from `sibling_dial_targets` (store-fed discovery,
  PT-4-filtered): dial over the SAME transport the listener runs on (one
  endpoint, one NodeId — the machine's device principal), mutual admission
  with the slot witness, then the ordinary pull-only walks — the delegable
  and fleet class-2 planes **with `RetainedKeyCustody` attached to both**
  (the production shape; the crypto-shred ruling's peer-leg obligation is
  thereby discharged — a `Shredded` mint merged over a dialed channel drops
  the retained key exactly as the nest leg does), plus every content scope's
  class-1 feed walk and want-list block pull. Per-sibling failures are
  isolated and absorbed under a generous whole-interaction budget — an
  unreachable sibling is ordinary weather, the next pass retries.
  **Facts refresh rides the ensure step while bound**: the interface list is
  re-read once per pass (one snapshot for ensure + dial), the nest facts
  re-fetched best-effort (an unreachable nest keeps the last-known relay
  half; a nest that stops advertising stops the leg at the next start —
  the ruled rule-7 semantic), and a changed composition republishes through
  the device-endpoints step's write-if-changed. Reflexive/public addresses
  stay an honest absence (they need relay-side observation — a stated
  remainder, not a gap). **The relay URL now reaches the endpoint builder**:
  `PeerLegFactoryInputs` carries it (own-nest provenance by construction —
  the runtime's own node-info read, never a peer advert) into the shared
  factory body `fauna_iroh::peer_leg_transport`, which both tui and the
  sync agent wrap — the agent's factory landed with this entry, so app-dead
  machines serve AND dial (the relay-URL-provenance rule is
  discharged for the
  peer-leg consumer; TLS toward a production `relay.<domain>` rides default
  trust roots, `custom_roots` staying the test-relay injection point —
  the posture, recorded here). Proven: tier_1
  `peer_leg` pins (+ the LAN-change refresh pin); tier_3
  `peer_dial_convergence.rs` — two real runtimes, two store dirs, one
  account, **no reachable nest** (every data leg fails), a preference
  written on A readable on B over the dialed leg driven entirely through
  the pump, and the shred-drop-over-the-dialed-leg pin whose
  custody-attach mutation reds it alone; conformance V12/V12b and the
  W2.6/W5.7 suites all green beside it. **Stated remainders:** reflexive
  address observation (relay-side), and per-sibling backoff tuning if
  backstop-cadence redials ever measure as noisy — both demand-driven,
  neither captured as a row (the dial cadence is bounded by the pass
  cadence by construction).


- **Built — W3 the second app host (2026-08-18): linux hosts the runtime, off
  a shared assembly.** The W3 trickle-down's real unit is not a parameter but a
  *host*: until this landing, `AccountRuntimeParams` had exactly two production
  construction sites — tui's `session::establish` and `fauna-sync-agent`'s
  `account_host` — and **no app but tui hosted the runtime at all**. That is
  what the trickle-down owes each remaining native app, and what the two
  outstanding W3 rows were both really
  blocked on. **The agent's host is not a substitute for an app's**: it is the
  app-dead backstop (W5.5b), and the two are designed to coexist under the W5.1
  `engine.lock` election. Two capabilities are structurally unreachable from
  it — the member half of the content-scope set (joined `__conv` channels come
  off a live MLS engine, which a bearer-only process does not link) and MLS-sealed
  outbox intents (the T9 carve-out holds those for a conversations-engine host) —
  so an account whose only host is the agent walks its own-actor scopes and
  nothing else, no matter what the agent is taught.
  **The assembly is now shared** (`fauna-client-account-runtime`, a native-only
  leaf above `fauna-sync-engine` + `fauna-client` + `fauna-client-sync`, the
  `fauna-client-custody` crate shape): of the fifteen params, only four are the
  app's — its nest client, its own dir, its `MembershipSource`, its device id —
  and the crate performs the other eleven, several of which encode a safety
  decision invisible at a call site (escrow trust from this machine's TOFU pin
  rather than the nest's own claim; the writer-key resolve and the agent's
  enrollment-target probe **off** the login path; `process_rpc` as the store
  principal's own bearer; degrade-to-blob-rail on every failure). Its two phases
  are the split that makes the last two true: `build_params` is synchronous and
  login-path-safe, `resolve_and_start` carries all the I/O — the writer-key
  resolve, this device's own id (a `device.db` open on tui, a state-dir read on
  linux, which is why it arrives as a closure rather than a value) and the
  agent's enrollment-target probe. **tui moved onto it in the same landing**, so
  the lift ships with two consumers and no second copy to drift.
  **linux is the new consumer** (`apps/fauna-linux/src/account_runtime.rs`,
  installed at the AuthSuccess hook after the conversations session so the first
  pump pass can already answer the membership question). Its `MembershipSource`
  reads `conv_backend::active_session()` on **every** call rather than
  snapshotting one — the seam's two rules, satisfied by construction: no session
  yet reads as `None` (*cannot tell*, never *left every channel* — which scope
  departure would turn into deleting this device's copy of every channel's
  content), and not caching is what lets a join or a leave take effect on the
  next pass with no notification path. Teardown is one line in
  `actor_scope::reset_actor_scoped_state` — the app's canonical one-list seam,
  so all six teardown paths get it rather than a seventh hand-list — and a
  generation counter makes a sign-out landing *mid-assembly* shut the fresh
  runtime down instead of installing it (the "signed-out account still being
  served" shape `apps/sync-agent.md` § Control plane split forbids). The peer
  leg stays structurally off here: it needs `fauna-iroh`, a separate tranche
  tui alone carries.
  **The landing is observable**: linux now answers the account pump's two
  convention-11 legs — the `account_pump_cycles` completion barrier and the
  `account_pump_now` poke (`fauna_e2e_agent::ACCOUNT_PUMP_CYCLES_KEY` /
  `ACCOUNT_PUMP_NOW`, `helpers/waiting.py::await_pump_cycle_after`) — where
  before it published neither, which the consumer correctly read as *this app
  has no account-store leg*. The state value's JSON is shared too
  (`account_pump_cycles_json`, the `fauna_conversations::state_json` pattern) so
  two hosting apps cannot spell one cross-app contract two ways.
  **The observable carries the ROLE, not just the counters, and that is what
  makes it testable at all.** W5.1's election decides who pumps, so a healthy
  hosting app that lost it to the co-located agent runs no pass and its counters
  sit at `(0, 0)` **forever** — correct, and on the counters alone
  indistinguishable from *no runtime at all* and from *runtime present, no pass
  yet*. Measured 2026-08-18 across one run: linux reached `(4, 3)` on one app
  instance and both linux **and tui** sat at `(0, 0)` on a later relaunched one,
  purely because of the lock. So `PumpCycles` publishes the role beside the
  counters (`AccountStoreHandle::is_engine_holder`, set at each of the worker's
  three role transitions and **before the readiness barrier**, so a caller
  reading straight after `start` never sees the default), and the state value is
  four-way discriminating: key absent = no leg; `runtime: false` = no runtime;
  `runtime: true, holder: false` = assembled, not pumping; both true = pumping.
  A cross-app test can then assert per role instead of assuming an election
  outcome — the first covering e2e, written against the counters alone, was
  deleted rather than shipped flaky. Proven: 8 tier_1 over the shared assembly
  (`fauna-client-account-runtime`) pinning each of the eleven — the two
  I/O-bound fields left unset, the shared per-user root rather than the app's
  own dir, no legacy-store adopt for a first-time host, `SeedHolding`, the
  membership passthrough and its absence, the peer leg off, the reconnect wake;
  2 tier_1 in the linux app; and 2 tier_1 on the role — a lone runtime publishes
  `holder` before `start` returns, and a *second* runtime over one store dir is
  a non-holder whose counters do not move even when explicitly poked (two
  runtimes in one process reproduce the real app-plus-agent arbitration exactly,
  since `flock` is per open file description). The cross-app e2e
  (`tests/test_account_runtime_pump.py`) asserts the trickle-down's actual claim
  unconditionally — *this app assembles a runtime at login* — and splits only
  the pump assertion by role.
  **Not built, stated (as of 2026-08-18; superseded):** windows, macOS, iOS and
  android hosted no account runtime yet — the `fauna-ffi` seat was the next
  consumer of this crate. **All four now do** (android 2026-08-22, macOS + iOS
  2026-08-25, windows 2026-08-27 — see § Implementation status today → *Built —
  W3 the `fauna-ffi` seat* and its three per-app entries below); web hosts the
  runtime as of 2026-09-29 (`account-client-lifecycle.md` § Implementation status today).

- **Built — W3 the `fauna-ffi` seat (2026-08-19): the shared half of the
  windows/macOS/iOS host, plus the erase duty it inherits.** The third consumer
  of `fauna-client-account-runtime` is `libs/fauna-ffi/src/account_runtime.rs`,
  behind the default-on `account-runtime` feature (its own, not a rider on
  `sync-engine-host`: the file-sync engine is a different plane an app can host
  without this one). It repeats linux's shape rather than inventing a second —
  process-global cell, `INSTALL_GEN` supersession guard, best-effort assembly
  that never fails a sign-in — and exposes three UniFFI methods on
  `FfiNestClient`: `start_account_runtime`, `stop_account_runtime`, and the
  convention-11 state read `account_pump_cycles_json` (which delegates to the
  **shared** JSON producer, so a third host cannot spell one cross-app contract
  a third way).
  **It is a separate call, not a parameter on `conversations_session`, and the
  reason is a rule rather than taste.** That factory already carries three of
  the four app-owned inputs, so folding the assembly into it is tempting; it is
  wrong because `memberships: None` is a **supported** wiring, so an app with no
  conversations rail must still host a runtime over its own-actor scopes. The
  membership source therefore reads the client's already-existing stashed
  session (`scheduling_session`) on every pump pass, which also means the two
  calls need **no ordering contract**: a runtime started first answers *cannot
  tell* until the session lands, then starts answering.
  **One input the shared assembly could not express, now added:
  `AppRuntimeInputs.store_container`.** `StoreRoot::platform()` is the per-OS
  desktop constant, and iOS is not a desktop: there the sandbox makes the
  per-app container the per-user root, and the unix derivation resolves
  `$HOME/.config/fauna/sync` *inside* the sandbox — writable, openable, and
  unreachable by the app extensions that share the container, so the failure is
  a silently unshared store rather than an error. `None` (every desktop host)
  keeps `platform()`; `Some(dir)` takes `StoreRoot::at`, mirroring that type's
  own two documented constructors.
  **The erase duty of `apps/account-scoping.md` § Erasure follows scope is
  discharged at the seat, for every app that CALLS the seat:**
  `account_state_erase_all_scopes` and `account_state_erase_scope` now sweep the
  W6 store root beside the app's own base. ⚠ **This entry originally claimed
  "for all three apps at once, with no call-site change", and that was wrong for
  apple:** its `AccountStateDir.erase`/`.eraseAll` were a hand-rolled
  `FileManager` sweep that never called this pair, so the fix reached neither
  target until apple moved onto it 2026-08-25 (§ *Built — W3 the apple host*
  below). Inheriting a seat is never automatic — a shell inherits what it calls.
  That root is a *sibling* of the app
  base, so the pre-fix erase missed it entirely — and because the credential
  namespace it accompanies holds the store's writer key, the stranded store then
  refuses every later sign-in and the app runs with no account runtime, for
  good. Both roots are now parameters of a pure inner half, so the covering
  tests never resolve the developer's real per-user root.
  **The install/teardown LIFECYCLE is shared too, from the same landing** —
  `AccountRuntimeHost` in the same crate. Writing the FFI seat produced a
  near-verbatim copy of linux's slot-plus-generation-guard, comments included,
  which is this crate's own founding argument (the second hand-written copy is
  where drift starts) applied to the lifecycle rather than the eleven params —
  and windows, macOS and iOS were about to make it five. What it guards is
  silent when wrong: a sign-out landing *while an assembly runs* must shut the
  fresh runtime down rather than install it, or the signed-out account keeps
  being served with a logged-out UI over a live pump. `begin` claims an install
  before the spawn **and registers the assembly as in flight**, `finish` installs
  or discards and owns **both** implied shutdowns (the superseded runtime, and
  the previous one it replaces), `take` hands teardown everything there is to
  stop. `take` advances the generation **even on an empty slot** — the case that
  matters, a sign-out during the very first assembly, which has nothing to take
  and everything to prevent. It carries a per-host payload (`()` for the FFI
  seat, the tokio `Handle` for linux, whose teardown runs on the GTK thread and
  must not build a runtime there) and deliberately does *not* perform the
  teardown shutdown, because whether to await or spawn it is the host's own
  constraint.
  ⚠ **An install and a teardown are each ONE transition over one lock**
  (2026-09-20). The host kept a lock
  per field, so an install was three decisions — is my claim current, remove the
  registration, write the slot — and a teardown landing between the last two
  found both halves empty: the registration just removed, the slot not yet
  written. It reported *nothing to stop*, the erase ran, and the install
  completed behind it, leaving the signed-out account's runtime installed and
  pumping against a store being erased (`os error 32` on windows, deleted-inode
  writes on linux). No `.await` sat in that gap, which is what made it read as
  safe — but `take` runs on another thread than `finish` (the GTK main thread on
  linux, the FFI caller's in the seat), so it never needed one. Both sequences
  are now single transitions over the host's whole state, which makes the
  in-between state unrepresentable rather than merely unlikely: while a claim is
  current, the host always holds either the registration or the slot for a
  teardown to find. The shutdowns still run after the lock is released, since
  nothing may `.await` under a `std::sync::Mutex`.
  ⚠ **Invalidating the claim is not stopping the store, and the host owes both**
  (2026-09-06). A superseded assembly shuts its runtime down *whenever it
  eventually finishes*; until then it holds `account-store.db` open on its own OS
  thread while the slot honestly reads `None` — and a sign-out's erase is
  synchronous, so it runs straight past. So `begin` registers a `PendingAssembly`
  (its `InstallClaim` carries the publishing half, and **dropping** a claim — the
  failed-assembly path, where `finish` is never called — resolves a waiting
  teardown at once rather than spending its budget), and `take` returns a
  `RuntimeTeardown { settled, pending, payload }` for
  `stop_account_runtime` to stop under one bounded budget. **The payload moved
  from install time to claim time in the same change**, and that is the load-
  bearing half: the teardown that most needs it is precisely the one that finds
  an empty slot, so under install-time capture linux had no runtime handle to
  drive its wait on exactly when it mattered. The failure is silent on POSIX
  (`unlink` removes an open file) and an `os error 32` erase failure on windows —
  `apps/account-scoping.md` § Erasure follows scope owns the obligation.
  Proven: 1 new tier_1 over the shared assembly (a sandboxed container becomes
  the root verbatim, and is still never the app's own dir), 3 over the shared
  lifecycle (a newer install invalidates the older claim; a teardown invalidates
  an in-flight claim **with nothing installed** — **red-verified** by bumping
  only when something was taken, which reds that pin alone; an empty host serves
  nothing and tears down cleanly), 5 more over the wait (a teardown mid-assembly
  comes away with the in-flight half *and* the payload; the hand-over happens
  exactly once; a failed assembly is not waited on; an assembly still in flight
  **is** awaited — **red-verified** by dropping the pending half instead of
  awaiting it, which reds that pin alone; the budget elapses rather than holding
  the sign-out open), 2 in `fauna-ffi`'s host and 3 over the erase (the
  all-accounts sweep reaches the store root — **red-verified** against the
  single-root form — removing one account leaves a sibling's store, absent roots
  are a no-op). linux moved onto the shared lifecycle in the same landing, so it
  ships with two consumers and no second copy.
  ⚠ **The superseded shutdown is a stop like any other, and it carries the
  superseding teardown's reason** (2026-09-20). The pump's prologue enrolls the machine *before* the assembly settles,
  so a sign-out landing mid-assembly supersedes a runtime that already owns a
  registered enrollment — and `finish` queues its own shutdown the instant it
  publishes the handle, ahead of anything the waiting teardown sends after
  waking. While that shutdown was a plain one, the teardown's sign-out
  retirement (`apps/sync-agent-credentials.md` § Implementation status today
  owns what a retirement is) always arrived at a runtime already stopped and
  read `account runtime is shut down`: one stranded device row per
  mid-assembly sign-out, silent, and in the e2e harness one per test whose
  reset follows its sign-in within the assembly's second or so. So `take`
  takes the caller's `StopReason` and records it in the same transition — a
  sign-out marks every claim so far as signed out, *before* the generation
  moves on — and `finish`'s superseded arm stops its runtime through the same
  shared per-handle stop the teardown uses. Both then retire first; two
  retirements of one key are harmless (key-addressed, the second clears
  nothing). A claim superseded by a newer `begin`, or by a switch-shaped
  teardown, still stops plainly: its slot survives, and retiring it would turn
  the next sign-in as that account into a removed-from-account state. The mark
  covers an old assembly that settles only after the *next* sign-in has
  claimed — its slot is just as erased — and never that next sign-in itself.
  tui reaches the same guarantee by queueing rather than marking: its sign-out still TAKES the `PendingAssembly` synchronously, so the shared stop owns the in-flight runtime under the teardown's reason, and the late `AccountStoreReady` that finds no live session queues its plain re-shut behind every stop in flight
  (`App::after_stops`, the shared `StopQueue`) — so it lands on an already-stopped handle, never ahead of the retirement. Proven: 1 tier_1 over
  the transitions, **red-verified** twice (the mark removed; the mark made
  sticky across the following sign-in), and `--app linux` measured — see the
  credentials doc's entry.
  ⚠ **tui is a deliberate NON-consumer of the lifecycle — do not "finish" the
  lift by moving it over.** It hosts the same runtime but reaches it
  differently: the started handle lands on the UI loop as
  `DataMessage::AccountStoreReady` and lives in `App` state
  (`app.settings.account_store`), so its supersession guard is an **actor-id
  match against the live session** (`app.rs`'s handler shuts the handle down
  when the ids differ) — exact where it runs, and needing no generation counter
  at all. Putting tui on `AccountRuntimeHost` would mean moving its handle into
  a process static, against that app's own no-globals, `App`-owned-state shape,
  for no correctness gain. Same lifecycle *contract*, two correct mechanisms;
  the params assembly is what all three genuinely share.
  **The WAIT, though, is genuinely shared, and tui takes it without the slot**
  (2026-09-06). `assembly_channel` / `PendingAssembly` / `stop_account_runtime`
  live in this crate and tui holds a `PendingAssembly` in `SettingsState` beside
  its handle, publishing through the same `AssemblySettle` the host registers —
  so all three hosts stop a mid-assembly sign-out one way and log it one way,
  with tui's handle still `App`-owned. **This is the shape a future lift should
  reach for when the two look duplicated: share the mechanism, not the
  ownership.** The distinction is not pedantry — the earlier reading of this
  ruling ("tui does not use the host, so its wait is tui's problem") is exactly
  what left the wait hand-rolled in one app and absent in the other for two days.
  **Not built, stated:** every app call site named here is now built. **android
  left this list 2026-08-22** (the seat's first consumer), **macOS + iOS left
  it 2026-08-25**, and **windows left it 2026-08-27** — see *Built — W3 the
  android host*, *Built — W3 the apple host* and *Built — W3 the windows host*
  below. web hosts the runtime as of 2026-09-29 (`account-client-lifecycle.md` § Implementation status today).

- **Built — W3 the android host (2026-08-22): the `fauna-ffi` seat's first
  consumer, and the sandboxed shape iOS inherits.** android hosts the runtime
  in-process, which on this target is not merely better but the only option:
  `apps/sync-agent.md` § Scope per platform lists android among the apps with
  no resident agent process, so there is no app-dead backstop and no W5.1 peer
  to lose the election to — a healthy signed-in android reports
  `runtime: true, holder: true`, the iOS branch of the acceptance test's
  contract. Wiring: `ApiClient.startAccountRuntime` at the account-ready edge
  inside `ensureNestConnected`, and `stopAccountRuntime` in `clearAuth` — the
  ONE funnel every android identity teardown (sign-out, account switch,
  factory reset) routes through, and exactly the set a plain process end is
  not in.
  **Placed beside the conversations session, never inside it.** Android's
  `startConversationsSession` returns early under
  `TestAgent.isE2EActive && !isRealConversationsActive`, so a call site there
  would deny the account plane to precisely the e2e runs that assert it —
  while `memberships: None` is a *supported* wiring. The two calls need no
  ordering contract, for the reason the seat entry above gives.
  **The container is MANDATORY here, and it is NOT named `sync`.** The generic
  unix branch of `StoreRoot::platform()` resolves `$HOME/.config/fauna/sync`,
  which inside the android sandbox is unwritable — and because the assembly is
  best-effort, the result would be a silent degrade to "no runtime", never an
  error. So android passes `store_container` like iOS. But android has exactly
  **one** `filesDir`, and `sync` is already the sync-engine store's name there
  (`AccountStores.SYNC_DIR`, inside each actor scope beside the container);
  when the name was chosen, `<filesDir>/sync` was also the pre-scoping flat
  store the since-retired first-adopter copied into an account's scope, which
  is how a W6 root there would have been silently adopted. Android's container is
  therefore `<filesDir>/account-store`
  (`AccountStores.ACCOUNT_STORE_DIR`) — one accessor reached by the start call
  and both erases, which is what makes the two roots unable to disagree. The
  erase half landed first, deliberately: while nothing writes a store there the
  argument is inert, so there is never a window where the runtime opens one
  root and the erase sweeps another. That widening is shared —
  `account_state_erase_scope` / `account_state_erase_all_scopes` grew a
  trailing `store_container_dir`, resolved by one private `store_root_for()`
  whose mapping is deliberately identical to the runtime's, so **iOS inherits
  it with no further Rust** (priority #2); every desktop caller passes `None`
  and is byte-identical in behaviour.
  **`own_device_id` is passed, not `None`.** Android's `indexLeaseDevice` is
  ratified `null` because a phone hosts no content index; that reason does not
  transfer to the account plane, so the host passes this install's stable sync
  device id — the same 32 bytes `syncEngineHost` seats. A wrong length FAILS
  the call by design rather than silently enrolling on the placeholder row.
  **The e2e leg, both halves:** `account_pump_cycles` is published top-level by
  `TestAgent.serializeState` (re-parsed from the shared JSON producer, never
  re-derived), and `account_pump_now` is honoured as a fire-and-forget poke —
  matching tui and linux, because awaiting a pump pass on the agent's dispatch
  loop would stall every later command behind one wedged pass. The poke needed
  a new shared export, `FfiNestClient::account_pump_now`, which windows and
  macOS/iOS inherit ready-made.
  ⚠ **Success item 3 is NOT claimed.** The android leg of
  `test_account_runtime_pump.py` is emulator-gated on the emulator host like every android
  e2e, and has not been run; what is proven here is the code path plus its
  headless pins. The test's `skip_unbuilt` arm stops firing for android by
  itself, because the role read now returns non-`None`.

- **Built — W3 the apple host (2026-08-25): macOS + iOS, and the erase leg the
  seat's own status entry wrongly reported as already inherited.** Both targets
  host the runtime through one shared FaunaKit seam,
  `FaunaClient.startAccountRuntime()`, called from `FaunaClient.start()` — the
  post-auth funnel both shells already share — so the per-target work is
  genuinely only the e2e leg. **iOS is the target that most needs it**:
  `apps/sync-agent.md` § Scope per platform keeps iOS app-side "entirely (no
  daemons)", so the in-process host is the only host its account plane gets and
  a healthy signed-in iOS reads `runtime: true, holder: true` — the android
  branch of the acceptance test's contract. macOS shares its store with the
  co-located `fauna-sync-agent`, so it legitimately reads `holder: false` when
  the agent wins the W5.1 election.
  **The one genuinely per-target start call is macOS's e2e path.** Its
  session-patch login deliberately skips `faunaClient.start()` (too heavy for
  the MainActor), so a hook only `start()` fired would deny the account plane to
  exactly the runs that assert it — android's own conversations-session trap,
  one shell over. iOS's e2e path calls `start()` and needs nothing extra.
  **⚠ iOS nevertheless did not assemble until 2026-08-26, and the cause was in
  neither app nor plane.** The Swift call site was right from the start; what
  was missing was a Cargo feature forward. `fauna_credential_store::cred_file_dir()`
  — the `FAUNA_E2E_CREDENTIAL_DIR` redirect — is gated
  `debug_assertions OR e2e-agent`, the apple FFI slices build `--release`, and
  `fauna-ffi`'s `test-helpers` did not forward `fauna-credential-store/e2e-agent`
  (it cannot: a Cargo feature may only name a DIRECT dep, and the crate is
  reached through `fauna-client-sync`). So the store fell through to the platform
  keyring arm, which on iOS is the inert `no_keyring`: the writer-key mint's
  read-back found nothing and the assembly refused with *"the credential store
  did not retain the writer key"*, leaving `--app ios` at `role: (False, False)`
  for 240 s. macOS never showed it — its e2e xcframework is the **debug** host
  flavor, where the `debug_assertions` arm keeps the redirect. The forward now
  lives on `fauna-client-sync`'s `test-helpers`, pinned by
  `test_client_sync_test_helpers_forwards_the_credential_redirect`; mechanism and
  the wider finding (macOS/windows were writing e2e writer keys into the box's
  real keyring the whole time) belong to
  [`e2e-automation-surface-gating.md`](e2e-automation-surface-gating.md)
  convention 15. **The reading to keep:** an app-side assembly that fails only on
  one target is not automatically that target's bug — the two apple shells run
  the same FaunaKit code and differed only in build profile.
  **The production half closed 2026-08-26 — iOS and android now assemble off
  the e2e redirect.** The forward above fixed only the harness leg: on a real
  device there is no redirect, `production_credential_store()` fell through to
  the inert `no_keyring` arm, and the same read-back refusal meant **no phone
  had ever assembled an account runtime in production**. The ruling and its
  shape are the contract owner's (`apps/common.md` § Credential storage → *The
  shared Rust credential slots on the phones*): the app lends its own platform
  store to `fauna-credential-store`'s new *foreign* arm over the very
  `FfiSecretStore` the registry rides (`install_platform_credential_store`,
  once at launch), selected only where no native arm exists and there
  outranking the e2e redirect, so the phone e2e now exercises the production
  seam. The T10 mechanics bullet (§ The store device principal) names the
  per-platform slot honestly as of the same date. Pinned by
  `fauna-credential-store`'s resolution matrix + foreign round-trip tests and
  `account_runtime`'s `the_writer_key_persists_and_reloads_over_the_phones_lent_store`;
  the e2e witness is the existing iOS `test_account_runtime_pump.py` leg,
  which now rides the Swift keychain's e2e file backing rather than the Rust
  redirect — measured 2026-08-26 on the run's credential dir: the whole T10
  bundle sits in `keychain.json` as `fauna-account-store/<actor>…` rows
  (writer key, backup key, device auth, grant marker) and no
  `fauna-account-store.json` exists, the exact inverse of the pre-change runs
  beside it. android's twin is code-complete and owed its emulator run.
  **And the store dir follows the key out of the platform backup (same day):**
  `AccountRuntimeParams::store_backup_exclusion` is stated by every assembly and
  applied before the dir is opened — a restored phone can no longer hold a
  store whose `ThisDeviceOnly` writer key did not travel (contract owner:
  `apps/common.md` § Credential storage → *the store dir follows the row*;
  mechanism owner: `behavior/backup-destinations.md` § Third destination kind →
  *Durability + labeling*). The residual — a slot lost while the dir survives —
  wants the built succession rotation as its self-heal; the inverse — a slot kept while the dir is gone (a reinstall with the
  keychain intact) — retires the key and re-mints under the journal-bound
  writer ruling (`account-replica-posture.md` § The store device principal,
  refinement 11, 2026-09-15).
  **Also closed in the same pass: iOS e2e failures now say what happened.**
  `drivers/ios.py` captured no app log at all, so the acceptance test's own
  routing advice ("grep the app log for `account runtime:`") had nothing to grep
  and two blind ~12-minute runs settled nothing; it now reads the rolling
  `<container>/Library/Application Support/logs/fauna.log.<date>` the app already
  writes, and the very first run carrying it named the cause outright.
  **`own_device_id` comes from `FaunaClient.deviceId`, NOT
  `APIClient.indexLeaseDevice`** — that helper is ratified `nil` on iOS because a
  phone builds no content index (`content-index.md` § Where the index is built),
  and that reason does not transfer to the account plane: iOS enrolls a device
  row like any other host, so reusing the helper would have left the sandboxed
  target — the one with no agent to fall back on — permanently on the
  placeholder row.
  **Teardown is the shared `FaunaClient.shutdown()`, and it became `async` for
  this.** That function is already the one funnel every identity teardown calls
  (sign-out, account switch, factory reset, add-account promotion) and a plain
  quit never does, which is exactly `stop_account_runtime`'s contract. It is
  **awaited, not spawned**: the teardown advances the install generation
  (`HOST.take()` bumps even on an empty slot), so a spawned one could land after
  the next login's `begin()` and supersede the *fresh* runtime — which nothing
  retries, leaving that account with no runtime for the rest of the process. The
  same reasoning `FileProviderCoordinator.signOut()` is awaited before
  `runLaunch()` under.
  **The erase leg, which review found missing.** apple's
  `AccountStateDir.erase`/`.eraseAll` were a hand-rolled `FileManager` sweep of
  the app's own base that never called `account_state_erase_*` at all — so both
  targets carried the full stranding bug (`apps/account-scoping.md` § Erasure
  follows scope) while the seat's status entry above read "inherited with no
  call-site change". Both now call the shared pair, and both — plus the start —
  read the container from ONE accessor, `AccountStateDir.storeContainerDir`:
  `nil` on macOS, where `StoreRoot::platform()` already resolves the user-domain
  base the agent shares (`~/Library/Application Support/Fauna/sync` — out of the
  app-group container since 2026-08-25, § Placement), and the app-group `sync`
  dir on iOS, e2e-gated to the in-container legacy base exactly as
  `FaunaClient.syncStateDir` is there (convention 10 — the app-group container
  is machine-global; a macOS e2e launch runs the production derivation inside
  its relocated `HOME`). The
  single-account erase needs no flat drop on any app: the flat pre-scoping
  layout and its `state-owner`-marker adoption were retired by the
  compat-remnant sweep (`version-compatibility.md` § Dimension 2, the fourth
  ratified exception), so nothing account-scoped rests outside an actor's
  scoped dirs.
  **The e2e leg:** `account_pump_cycles` published top-level by each shell's
  `serializeState()` (re-parsed from the shared JSON producer, never
  re-derived), and `account_pump_now` honoured as a spawned poke — matching
  android, tui and linux, because awaiting a pump pass on the agent's dispatch
  path would surface one wedged pass as an unrelated command timeout.

- **Built — W3 the windows host (2026-08-27), closing the seat's last app
  call site.** `NestRpcClient.StartAccountRuntimeAsync`/
  `StopAccountRuntimeAsync`/`AccountPumpCyclesJson`/`AccountPumpNowAsync` wrap
  the three `fauna-ffi` seat methods. windows shares its store with the
  co-located `fauna-sync-agent` exactly as macOS does (`apps/sync-agent.md` §
  Scope per platform), so a healthy signed-in app may legitimately read
  `runtime: true, holder: false` once that agent wins the W5.1 election.
  **Placed beside the conversations session in BOTH login seams, never inside
  either — the android/macOS trap this app hit too.** Production's
  `StartMainAppAsync` calls it unconditionally (not gated on `E2eEnv.Bridge`
  the way the conversations-session block is); the `set_state` e2e login path
  — which never reaches `StartMainAppAsync` — calls it a second time,
  independently, right beside `BuildE2eConvSessionAsync`. `memberships: None`
  is a supported wiring, so the two calls need no ordering contract.
  **Inputs:** `appDataDir` is `AccountStateDir.FlatBase` (the same base
  `account_state_*` already takes); `own_device_id` reuses
  `NestRpcClient.IndexLeaseDevice` — the SAME screened 32 bytes
  `conversations_session`'s `index_lease_device` seats, so a malformed id
  costs coordination, never this call; `store_container` is always `None` —
  desktop `NotApplicable` cloud-backup posture, matching macOS.
  **Stop is folded into the existing `DisposeNestClients` teardown funnel**
  (`App.StopAccountRuntimeThenDisposeAsync`, wrapping its already
  fire-and-forget dispose) rather than hand-listed at each of windows'
  several teardown sites — every one of them is a departure this client was
  hosting the runtime for, and the stop is a documented no-op when none was
  ever started, so covering a plain nest re-point too costs nothing.
  **The e2e leg:** `account_pump_cycles` published top-level by
  `App.SerializeState` (re-parsed from the shared JSON producer, never
  re-derived), and `account_pump_now` honoured as a fire-and-forget poke via
  `TestAgent`'s `account_pump_now` case — matching android/tui/linux.
  **Success claimed in full:** `pytest tests/e2e-unified/tests/test_admin_logs.py
  tests/e2e-unified/tests/test_account_runtime_pump.py --app windows -v`
  run live on Windows with no test edit — 5 passed, both
  `test_account_runtime_pump.py` legs included (unlike the android leg above,
  which remains emulator-gated and unrun).

- **Built — W3 slice 1 (2026-08-12): the account runtime + the tui pilot.**
  The lifecycle is code exactly as ruled (§ The account store → *The
  client-side lifecycle*): `fauna_sync_engine::account_runtime` (feature
  `account-runtime`) — a named store thread owning the
  `AccountStore<SqliteBackend>` under the per-actor dir, the `Send + Clone`
  `AccountStoreHandle`, the prologue + nudge/ticker/reconnect pump with
  per-pass panic containment, and the writer key beginning the T10 slot
  (mint-or-load via `fauna-credential-store`, read-back verified,
  corrupt-slot refusal — never a re-mint). **tui assembles it** at
  `session::establish` (spawned, best-effort — a failed assembly leaves
  every store-backed surface failing its gesture), tears it down at sign-out
  (`session::sign_out`, deterministic; a stale handle racing an account
  switch is shut down at delivery instead of installed) and at quit (the
  drop path), and its push pump feeds the nudge channel from the
  scope-tagged `fauna.sync.changed` arm (`app.rs`; since 2026-09-25 the arm
  is the runtime's own, fed by `with_session_wakes` —
  `account-data-plane.md` § Implementation status today). **The pilot surface
  reads the store:** tui muted-words load/save route through the shared
  store twins (`fauna_sync_engine::preference_surfaces` — the same
  `MutedWordsSnapshot` and the same one normalizer as the blob seam) when
  the handle is up, the blob rail otherwise; saves are the plane-first dual
  write, so an old client keeps reading every save (as built; plane-only,
  with a missing handle waited for, since closure step (5), 2026-10-01 —
  `config-dissolution.md` § Implementation status today). Riding the rewiring,
  sync-prefs' four hand-written save copies collapsed into the shared
  `fauna_client_config::{load,save}_sync_prefs` chokepoint (tui, linux and
  the ffi seat now reach it only through the dual-rail twins, the closure
  order's step (1); wasm's swap rides step (2)), so its own plane swap lands on one function (today
`fauna_account_plane::preference_surfaces::{load,save}_sync_prefs`, plane-only: the blob rail
retired 2026-10-02 — `config-dissolution.md` § The `__config` dissolution schedule → *The closure order*, step (6)). Proven: 12
  tier_1 (`account_runtime::tests` — pump, containment, writer-key
  stability, the surface twins' dual-write/normalization parity) and the
  tier_3 conformance pair over real nest handlers + real push frames
  (`bins/fauna-nest/tests/conformance_account_runtime.rs`: V1 — two
  runtimes converge through the scope-tagged nudge *alone*, backstop
  disarmed, red-verified by unplugging the pump; V3 — a marker-blind
  old-client blob write is imported by the production pump's next pass).

- **Built — W3 slice 2 (2026-08-12): all four preference surfaces read the
  store on tui.** The remaining three followed muted-words, on a shape the
  slice generalized rather than repeated. **The mutation logic moved to the
  sub-record**: a cluster's gestures are pure functions of *its own*
  `UserConfig` sub-record (`fauna_client_config::preference_records`), and
  the `&mut UserConfig` mutators were thin blob-rail wrappers over them that
  additionally bumped `updated_at` (delegation needed no lift — `set_pin` and
  `resolve_pin` were already sub-record-level; the wrappers retired with the
  rail on 2026-10-02 — `config-dissolution.md` § The `__config` dissolution schedule → *The closure order*, step (6)). Since a plane entry's value
  *is* the sub-record in canonical dag-cbor, the two rails then differed only in
  where the record is read and written, never in what a mutation means — the
  property that kept their stored bytes identical, which is what the
  bridge's echo-stop needed to see a converged fleet.
  `preference_surfaces` grew a generic `load_record`/`update_record` core
  (all four clusters on it) whose write **skips on unchanged encoded bytes** —
  stricter than the since-retired `ConfigClient::update_when`'s caller-reported flag, because a
  no-op write's fresh `LwwStamp` would outrank and lose a concurrent sibling's
  real edit. Where a surface composes its record with nest-side facts, the
  composer stayed in its own crate and grew a rail-agnostic entry point
  (`TaskDelegationView::{rows_from, resolve}`,
  `TrainedTopics::{rows_from, delete_model}`), so lease observation, the
  model-plane leg and row composition are one implementation on both rails;
  the plane-rail *sequences* for the four trained-topic gestures live in
  `preference_surfaces` too, so no app rewrites them. Proven: 4 more tier_1
  over the real runtime (sync-prefs' blob-seam parity including the
  out-of-catalog degrade, the registry + pins reaching the blob rail, the
  byte-level write skip, no-entry reads) plus the sub-record round-trip and
  cap on the bare records; tui's 1398 stay green.
  **Built, stated (2026-09-28):** every app but web hosts the runtime (the
  per-app host entries below), and on all six every E1 surface — muted
  words (load, save, add, remove), the reporter-side hide (load, hide,
  unhide), sync-prefs, the trained-topic list and its four gestures, and
  task delegation — dispatches through the `preference_surfaces` `_dual_rail`
  twins: the plane when the process's handle is up, the blob seam otherwise
  (as built; the twins collapsed onto the plane at closure step (5),
  2026-10-01 — `config-dissolution.md` § Implementation status today).
  tui and linux pass their own handle; the `fauna-ffi` seat (windows, macOS,
  iOS, android) passes `crate::account_runtime::handle()` from each façade,
  `TrainedTopics` and `TaskDelegationView` riding in as the twins' composer
  argument. No per-app rail branch remains. That is step (1) of
  `config-dissolution.md` § The `__config` dissolution schedule → *The closure
  order*, scheduled to retire the rail before the 2026-10 baseline and done at its step (6) on 2026-10-02; web's surfaces took the same twins in
  step (2), built 2026-09-30 (`config-dissolution.md` § Implementation status
  today); tui's first `fauna-iroh`
  dependency stays the W3 row's deferred tranche. (The scope-set gap this entry
  named is half-closed — see *Built — W3 the own-actor scope-set derivation*
  below; the seen-set trigger it named split into the built auto-in-set
  producer and the gated browse trigger — § The replica boundary → T1.)

- **Built — W3 the own-actor scope-set derivation (2026-08-12): a started
  replica walks its own content.** § Feeds and cursors → *Scope partition*
  says a replica's scope set is derived from what the account participates
  in; `fauna_sync_engine::scope_set` is that derivation, and it splits on
  where a scope id comes from. **The own-actor half is derived, not
  registered**: every own-actor kind's scope id *is* this actor, so
  `derive_own_actor_scopes` is a pure function of the actor id and
  `AccountStoreRuntime::start` seeds the pump's walk set from the id it
  already holds — before the prologue, so a fresh replica's first pass
  already walks its own mail, calendar, card and post scopes, and all 7 apps
  inherit the same four with no per-app code and no app able to forget one.
  Derivation failure is logged, not fatal (an empty content set is exactly
  the pre-derivation behavior, and the class-2 legs must keep pumping); the
  pump was already per-scope error-isolated, so a scope a nest refuses costs
  one report line, not the pass. **The member half is read, not pushed**:
  `AccountRuntimeParams` grew a `MembershipSource` the pump calls once per
  pass, so a join or a leave needs no notification path — the next pass
  simply derives a different set, and a left channel stops being walked
  (ending the subscription; dropping that scope's *items* is T2 transition
  3, untouched here). Its `None` means *cannot tell right now*, not *no
  channels* — a still-loading engine would otherwise answer empty, which
  reads as "left everything", so the runtime holds the last set it trusted.
  Scopes an app registers explicitly are kept apart from the derived set and
  unioned back in, so a re-derivation can never drop them. The source is the
  app's own **MLS engine**, local state — a replica with no nest in sight
  still knows what it joined: `ConversationsSession::joined_conv_channels` →
  `FaunaMlsBackend::conv_channels`, the narrow side of the same
  durable-marker split the folder sweep classifies on (a channel counts
  when bound as a chat thread in this process **or** carrying `MlsEngine`'s
  durable chat marker, never merely by being of unknown kind — registering a
  scope costs one empty feed read and touches no MLS state, so the sweep's
  epoch hazard has no analogue here, while a `conv` scope minted for every
  folder channel would be permanent per-pass noise). Its one residual is
  bounded and self-healing: a chat channel created by a pre-marker binary
  and not yet bound in this process is absent until the app next opens that
  thread, which is also when its content first matters. **tui wires it** (the
  lead app), and **linux followed 2026-08-18**. ⚠ The "the other six inherit the
  seam with a one-line param" this entry used to end on was **wrong, and cost
  two sessions**: the param is one line only for a
  process that already *hosts* the runtime, and no app but tui did. Wiring
  `MembershipSource` into an app is therefore the trailing line of hosting the
  runtime there — see *Built — W3 the second app host* above, which owns the
  prerequisite and the shared assembly that discharges it.
  The derived set is sorted by canonical scope string and
  duplicate-free, so two devices of one account derive an identical set in
  an identical order — what lets a caller diff a re-derivation against what
  it last registered. `ContentWalkReport` grew a `scope` field so a caller
  holding a pass's several walks reads each one's subject off the report
  instead of re-deriving registration order. Proven: 5 tier_1 on the
  derivation (own-actor shape, `conv` never derived from the account,
  per-channel member scopes, sorted/deduped, disjoint across actors) plus
  one over the real runtime asserting a started runtime walks exactly the
  four canonical own-actor scopes on a clean pass (red-verified by
  unplugging the seed: `got []`, 0 vs 4).
  Proven on the member half: 3 more tier_1 over the real runtime (joined
  channels walked, a join *and* a leave between passes moving the set with no
  register call and no nudge, an unanswerable source holding the last set, a
  registered scope surviving re-derivation) plus one in
  `fauna-conversations` pinning `conv_channels` against the sweep's own
  four-class fixture, relaunch included. The join/leave test is red-verified
  by unplugging the per-pass re-derivation.
  **Not built, stated:** windows, macOS, iOS and android pass no
  `MembershipSource` yet, so their `conv` scopes stay empty until each hosts
  the runtime (the `fauna-ffi` seat next); own-actor content
  scopes are walked for every account whether or not that kind has records,
  which is one empty feed read per kind per pass. (The departure gap this
  entry named — "the walk stops, the data stays" — is CLOSED; see the next
  entry.)

- **Built — W3 scope departure (2026-08-12): a left channel's items leave the
  replica.** T2 transition 3, the deletion half of what the scope-set
  derivation began. The **store half** is `AccountStore::drop_scope` →
  `StoreBackend::drop_scope`: one transaction removing that scope's journal
  rows, state entries, frontier vector, record-index rows and their loose
  blocks, relay rows and adopted-segment rows, followed by the segment
  **files** — the one reclamation `apply_tombstone` cannot do per record,
  since a CARv2 is immutable and eviction is whole-file. Crash-safety is
  atomic-or-resumable rather than atomic, because no transaction spans a
  filesystem: the row transaction also writes a pending-drop mark, the files
  are swept after it commits, and `SqliteBackend::migrate` replays any owed
  sweep at open — so a crash leaves "not dropped" or "dropped, files pending",
  never a half-departed scope. Rows-then-files is the deliberate order (the
  inverse of adoption's files-then-rows, for the same reason: an unrouted file
  is invisible, an unbacked routing row is the failure `block_get` cannot
  recover from).

  The **judgment half** is `fauna_sync_engine::departure`, a separate module
  because deriving wrongly costs a pass while deleting wrongly costs data. It
  keeps a **local, durable subscription marker** — the scope set this replica
  last affirmatively subscribed to — and a departure is `marker − fresh`, only
  ever that. Three refusals, each a shape rather than a check: no affirmative
  membership answer *this pass* → drop nothing (the `MembershipSource` `None`
  contract, whose stakes this raises from a skipped walk to deleted content —
  so the runtime now passes the answer's affirmativeness down beside the set,
  bundled in one struct so the two cannot be passed mismatched); no membership
  source at all → drop nothing, which is what makes the six-app pre-trickle-down
  state structurally incapable of deleting a channel; a registered scope is
  never a departure (it is unioned into every derivation). Own-actor scopes are
  likewise immune by construction — they are a pure function of the actor id.
  An empty derived set is refused outright, since nothing legitimate produces
  one. The marker is deliberately **local, not synced**: each device derives
  its own memberships from its own MLS state, and a synced marker would let one
  device's stale view delete another's data. On first run the marker seeds from
  `fresh ∪ scopes the store holds`, so a leave that happened before the marker
  existed is still detectable later; seeding judges nothing. **The two class-2
  rails are never memberships and never in the marker (`state`, and
  `state-fleet` since 2026-09-15)** — the seed had excluded only `state`, so a
  replica whose fleet scope was walked before its first affirmative pass
  carried `state-fleet` in its marker and dropped its device set and escrow
  target on the next pass; a fresh store per e2e relaunch masked it until the
  relaunch carry started restoring the replica, when the walk read the nest's
  own fleet rows above an emptied journal as a burnt writer
  (`account-replica-posture.md` § The store device principal, refinement 11).
  The rail filter runs on the departed set as well as the seed, so a marker an
  earlier build seeded with the rail is retired, never obeyed
  (`departure::tests::a_class2_rail_is_never_seeded_into_the_marker_nor_dropped_as_a_departure`).

  Departures run as the pump's **first** step of a full pass (never on a nudge
  — a single-scope wake says nothing about membership), so no later step in the
  pass touches a scope just left. A **re-join needs no code**: the frontier row
  left with everything else, so the next walk starts at cursor zero — the
  re-bootstrap path a fresh replica already takes. The seen-set entry for the
  departed scope survives structurally, not by care: it lives in the
  account-state scope and merely carries the departed scope's string as its
  *key*, so a `WHERE scope = ?` delete cannot reach it — which is what T2
  transition 4's grow-only law requires (the account observed those items;
  leaving does not unobserve them).
  Proven: 6 store-level tier_1 (every plane emptied with counts, a sibling
  scope untouched, the seen-set entry surviving a same-key drop, an unheld
  scope reporting zeros, segment files leaving the file area, and an
  interrupted drop resuming its sweep at open) + 5 module tier_1 on the
  marker semantics (first-run seeding judges nothing but widens, an unanswered
  pass neither drops nor advances, an empty set refused, own-actor scopes never
  departing, convergence after a drop) + 5 over the **real runtime** (a leave
  between passes dropping exactly that scope, an unanswerable source dropping
  nothing, a source-less replica dropping nothing, the seen-set outliving the
  content, a re-join re-walking from zero). Red-verified by unplugging the diff:
  the three drop-asserting runtime tests fail, the refusal tests correctly do
  not.
  **Not built, stated:** the first *production* items to drop arrive with conv
  scope-feed serving — until then the tests' staged
  rows are what exercise it; and a departed scope whose bytes sit in a segment
  shared with a still-held scope is not a case that can arise today (adopted
  segments are per-`(scope, kind)`), so no cross-scope segment refcount exists.

- **Built — W3 the peer content-coordinate relay (2026-08-12): a brand-new
  record's coordinates travel peer-wise.** The W2.6 gap ("a brand-new record
  on one device reaches a sibling only nest-mediated") is closed on both
  halves. **The content walk's ingest feeds the relay plane**
  (`fauna_sync_engine::content_scope_plane`): every coordinate-valid feed
  row lands verbatim as a `record-cid` relay row (writer =
  `NEST_SEQUENCER`, `item_key` = the CID digest, no inline entry — the
  coordinates ARE the payload), unconditionally, unknown ops included — the
  same non-editorializing rule as the class-2 walk, and the store's
  per-`(scope, writer, item)` collapse mirrors the nest's own (a tombstone
  supersedes its record's add). A one-writer content scope has no local
  publish, so the walk's ingest is the plane's only feeding point; the bulk
  bootstrap's mandated zero-frontier walk is what backfills it. **The peer
  serve answers the `record-cid` arm** (`fauna_peer_sync::server`): same
  wire, two rules of its own — a record-cid request names its content scope
  explicitly (no implied plane; an empty answer would read as "converged"
  to a walk — the same loud-refusal rule as the arm check), and an omitted
  frontier is `{nest: since}` (§ Feeds and cursors' scalar-cursor form; the
  class-2 client always sends an explicit frontier, so shipped behavior is
  unchanged). The walk needs no peer flavor: `ContentScopePlane` over
  `PeerRequester` is the identical nest-leg walk over the peer channel, and
  a peer-walking replica's ingest re-feeds its own relay plane, so a third
  sibling converges off it in turn (relay chaining). The allowlist is
  untouched (rule 3 — `record-cid` is an arm of the already-served feed
  kind, not a new kind). Proven store↔store with no nest anywhere
  (`peer_leg_convergence.rs::a_brand_new_records_coordinates_travel_peer_wise`,
  red-verified by unplugging the ingest's relay recording): coordinates
  converge B's index + nest-sequencer frontier off A's relay plane alone,
  B re-serves, and the bytes ride the shipped want-list pull beside them.
  **Not built, stated:** the peer-leg assembly seam + tui's `fauna-iroh`
  transport are W3 placement work; the leg's live operation is W5-coupled
  (the `DeviceAuthorization` admission witness) and R14-coupled (live
  discovery); a meaningful store-safe witness third column is W8-coupled
  (the `p2p-share` plane it proves absent is the share twin). PQ-2 fuzz is
  DONE (`peer-channel-hardening-check`; see the *Built — W2.6* entry).

- **Built — W3 the auto-in-set seen-set producer (2026-08-12): the plane's
  first production seen-set writer.** The R1 creation/delivery-class half of
  the producer decomposition (§ The replica boundary → T1):
  `fauna_sync_engine::seen_set_producer`, run by the pump after its content
  walks, raises each **own-actor** scope's `NEST_SEQUENCER` watermark to the
  store's accounted frontier and publishes the `fauna.state.seen-set` entry
  (key = the canonical scope string) only when membership grew — all 7 apps
  inherit it through the runtime with zero per-app code, and reading the
  *frontier* rather than a walk report means the first pass after an upgrade
  heals a producer-less binary's whole accounted history. Three rules a
  reader should not re-derive: **(a)** own-actor scopes only — a member
  scope's items are browse content, where a watermark would assert
  observations nobody made (the pass takes `derive_own_actor_scopes`'s
  output, never the walk set); **(b)** the pass runs *after* the class-2
  reconcile, so a sibling's raise is merged in first and a covered raise is
  the publish-free echo-stop (V4 pins the converged sibling publishing
  nothing); **(c)** a stored entry this binary cannot decode is a loud
  per-scope skip, never clobbered — the union join means a fresh overwrite
  would lose nothing fleet-wide, which is exactly why the local red flag
  must not be papered over. **Found and fixed under it, store format 2:**
  the journal's writer-global key refused two content scopes' coordinates
  as equivocation (§ Store logical schema component 2's `(scope, writer)`
  log-identity precision; the v1-store rebuild that shipped with it was
  retired 2026-09-25 by the compat-remnant sweep, min-reader stays 1). Proven: the runtime-level tier_1
  (delivery → watermark = frontier, published once, converged pass silent,
  new delivery advances), two direct tier_1 (pre-existing-frontier healing +
  zero-frontier skip; undecodable-entry isolation + never-clobber), the
  store-level regressions (two scopes' seq-1 logs coexist; the v1→v2
  rebuild), and tier_3 V4 (`conformance_account_runtime.rs`) over real nest
  handlers — red-verified by unplugging the pump step. **Not built,
  stated:** the T1 browse trigger (gated per the T1 producer decomposition;
  its enabling nest slice — serving `conv` scope feeds behind a
  membership-admission check — is queued nest-side), and nothing
  yet *consumes* the seen-set (materialized read-state is R9's).
