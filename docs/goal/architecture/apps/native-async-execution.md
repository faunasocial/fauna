# Native apps: async execution model & the stack-size rule — target state

Owns: native-async-execution
Status: ratified
Authority: which thread/stack a fauna-ffi async export's future is polled on (foreign executor, not a tokio worker) and the resulting Box::pin-large-leaf-futures rule + the every-async-export-through-`#[fauna_uniffi_async::export]` rule (the work runs on a tokio worker) + the future-size-assertion test convention; defers the WS-RPC client-binding seam itself to architecture/apps/common.md § WS-RPC client binding and wire framing to architecture/transport.md.

## The execution model (where an FFI async future is polled)

A `fauna-ffi` `#[uniffi::export] async fn` (or async method) is **not** run on a
tokio worker thread. UniFFI's foreign-async integration drives the Rust future
from the *foreign* executor:

- **Swift** — the future is polled on a **Swift concurrency cooperative-pool
  thread** (the threads backing `await`).
- **Kotlin** — on the coroutine **dispatcher** thread.
- **C# (Windows)** — the **first** poll runs synchronously on whichever thread
  called the generated async wrapper (a `Task` suspends only once the Rust
  future genuinely yields); every poll after it runs on a **.NET ThreadPool**
  thread, because the generated layer awaits its poll loop with
  `ConfigureAwait(false)` (§ The rule → *C#: the FFI layer never needs the
  caller's thread back*).

`#[uniffi::export(async_runtime = "tokio")]` does **not** change *which* thread
polls the future. It only wraps the future so a **tokio runtime *context*** is
entered while it is polled (via `async-compat`), so tokio resources (timers,
`tokio::net`, `tokio::time::timeout`, `tokio::spawn` onto the global runtime)
work. The *polling* still happens on the foreign executor's thread. So
`async_runtime = "tokio"` buys you tokio APIs, **not** a 2 MB tokio worker stack.
That is why no fauna export uses it bare: every async export is declared
through `#[fauna_uniffi_async::export]`, which puts the work itself on a tokio
worker (§ The rule → *Every async export runs on a tokio worker*).

## The hazard: a small foreign stack + a large inline future

A tokio worker thread has a ~2 MB stack; a process main thread ~8 MB. The
foreign cooperative-pool / dispatcher threads are **much smaller** (Swift's
cooperative pool threads in particular). A Rust `async fn`'s future is a state
machine whose size is the **sum of every sub-future it holds inline** plus its
locals. A deeply-nested `async` chain therefore builds one big future that is
**constructed and moved on the caller's stack** and whose poll frames carry its
large variants — and on a small foreign stack that overflows, typically as a
`SIGBUS` "Could not determine thread index for stack guard region" (the guard
page is hit before any Rust panic can fire, so it is an immediate crash, not a
catchable error). It is **load-correlated** (scheduling pressure makes the
cooperative pool spin up more, smaller-stacked threads), so it reads as a flaky
`BridgeDead` rather than a deterministic failure.

### The measured incident (2026-06-16)

`FaunaMacOS` `SIGBUS` on a Swift cooperative-pool thread, in the **anonymous-WS
bearer-mint** path: `fauna-ffi` `mint_bearer` (`async_runtime = "tokio"`) →
`fauna_client::ws_handshake_bearer::mint_bearer_over_handshake` (as named then) →
`fauna_anon_client::AnonymousNestClient::connect` →
`fauna_anon_client::ws::connect_anonymous` →
`tokio_tungstenite::connect_async_tls_with_config` (rustls + tungstenite TLS/WS
handshake). The handshake future is **~10.9 KB**, held inline all the way up to
the FFI poll point on the small cooperative-pool stack.

### The second measured incident (2026-08-23) — and the class boxing cannot reach

`FaunaMacOS` `SIGBUS`, twice in one journey run, on
`com.apple.root.user-initiated-qos.cooperative` threads: `fauna-ffi`
`run_succession_aftermath` (`async_runtime = "tokio"`) →
`fauna_client_recovery::aftermath::run_succession_aftermath` → **either** leg 4
(`fauna_client_capabilities::succession::remint_capability_grants` →
`mint_grant` → `fauna_mls::wrapped_blob::seal_capability_xwing` → `xwing_seal` →
`fauna_pq_kem::encapsulate`) **or** leg 6
(`fauna_client_mail_settings::succession::burn_mail_after_succession` →
`start_rotation` → `provision_snapshot_for` →
`fauna_mls::wrapped_blob::mls_snapshot_plaintext::build_mls_snapshot_plaintext` →
`derive_recipient_xwing_keypair` → `fauna_pq_kem::derive_keypair`), each ending
in **ML-KEM-768** inside `libcrux`. Each report's own `vmRegionInfo` puts the
cooperative-pool stack at **544 KB** and `sp` **3456 / 4064 bytes** below it, so
the chain wants ≈548 KB. Every frame appears exactly once — a deep single chain,
not recursion.

⚠ **This is a different hazard from the 2026-06-16 one, and the rule below does
not reach it.** There the stack was eaten by one large *future* held inline, and
`Box::pin` moved it to the heap. Here the bottom ~6 frames are **plain
synchronous calls** — post-quantum keygen/encapsulation with large stack frames —
and a future's location does not change how much stack a synchronous callee
needs. **There is no leaf future to box.** So for a synchronous heavy leaf the
remedy is the *other* mechanism this doc already relies on (see the bearer-
authenticated WS leg below): get the work off the FFI poll stack entirely with
`tokio::spawn`, onto a ~2 MB runtime worker.

⚠ **It was NOT load-correlated and NOT flaky** — the other tell this class
breaks. It reproduced deterministically, gated on *what the user held*: only a
succession over an actor with a live capability grant (leg 4) or a mailbox
(leg 6) reaches the PQ crypto at all, so the same run's `__config`-only
successions passed cleanly every time. A hazard that presents as "2 of 4
journeys, always the same 2" is this class, not the flaky-`BridgeDead` one.

⚠ **The audit lens that missed it.** Sweep (2026-08-21) had audited
`fauna-client-recovery`'s FFI exports and recorded them *"safe with no
additional fix"* — correctly, for the question it asked, which was **"does this
path open a TLS/WS handshake, and is that leaf boxed?"** Every enumeration in
§ Implementation status today is built around connect/handshake leaves. Nothing
in this doc had ever asked **"does this path do heavy synchronous work?"**, and
`run_succession_aftermath` did not exist yet on 2026-08-21 in any case. Both
halves are now part of the sweep question (§ The rule).

## The rule

**`Box::pin` large leaf futures in the shared connect/handshake/crypto paths the
native apps reach over `fauna-ffi`.** Boxing moves the big state machine to
the heap, so the enclosing future — and everything that holds it inline up the
chain — stays small (the anon connect future dropped **10.9 KB → 296 bytes**).
This is the conservative, structural root fix: applied at the **shared leaf**, it
protects *every* caller of that path (#2), needs **no per-app code**, and is
behaviourally identical (a future's location does not change what it does).

Prefer boxing the **leaf** (e.g. the TLS handshake) over boxing at the FFI export
— the leaf fix propagates the size reduction up every chain that holds it and
covers every entry point that opens an anonymous connection (mint,
silent-challenge, registration, claim, invite, storage-mode, and the
recipient/attendee discovery exports `resolve_nest` / `resolve_handle` /
`classify_attendee_transport` all share `connect_anonymous`) — **including any
future caller**, since the fix lives below every one of them, not at each call
site. Moving the work onto a dedicated large-stack `std::thread` + a
current-thread runtime is a heavier alternative reserved for a path that stays
large even after boxing; boxing has been sufficient so far.

**A synchronous heavy leaf takes the other mechanism, not this one (2026-08-23).**
When what eats the stack is a *synchronous* callee — post-quantum keygen /
encapsulation being the measured case — there is no future to box, and boxing
anything above it moves bytes that were not the problem. `tokio::spawn` the work
off the FFI poll stack onto a ~2 MB runtime worker instead; inside an
`async_runtime = "tokio"` export the runtime context is already entered, so the
spawn is available with no runtime of one's own (`fauna-ffi`'s
`account_runtime::install` is the pattern, and the bearer-authenticated WS leg
below has always been safe by this mechanism alone). Prefer it at the **FFI
export**, not per-leg: one spawn covers every leg the export drives, including
ones added later. The spawn-at-the-export is no longer a per-site choice: every
async export takes it, through the one attribute below.

### Every async export runs on a tokio worker — `#[fauna_uniffi_async::export]` (2026-09-29)

**No UniFFI async export in the workspace is polled on the foreign executor's
stack.** Each is declared with `#[fauna_uniffi_async::export]`
(`libs/fauna-uniffi-async`) in place of `#[uniffi::export(async_runtime =
"tokio")]`, on an inherent `impl` block or a free function. The attribute keeps
every `async fn` as a plain, unexported Rust fn under its own name and
signature — tui, linux, the wasm twins and tests await it inline, unchanged —
and generates beside it the exported twin, under the same foreign name (so the
Swift / Kotlin / C# API does not move), which runs the inline fn through
`fauna_uniffi_async::off_foreign_stack`: a `tokio::spawn` onto a ~2 MB runtime
worker, awaited. The twin's receiver is `self: Arc<Self>` and a borrowed
argument becomes the owned form UniFFI lifts it through anyway
(`<T as LiftRef>::LiftType`); synchronous items are exported unchanged.

- **The call contract is the inline one.** The output is returned, a panic in
  the work is re-raised on the polling thread (UniFFI reports it as before), and
  **dropping the exported future aborts the work** at its next await point —
  a cancelled Swift `Task` or torn-down Kotlin scope leaves nothing running
  behind it (no orphaned task consuming the next event from a shared receiver).
- **Why every export and not an audited subset.** Which exports reach a deep
  chain is a transitive call-graph question over ~450 async fns; an enumeration
  of it is stale the day a leg is added under an export that was "safe", and
  the crash it guards against is a guard-page `SIGBUS` with no panic and no
  log. One rule the compiler applies costs a task spawn per call, and removes
  the question.
- **Why an attribute and not a hand-written wrapper per export.** The
  wrapper was the first shape (`LinkedNestsMachine`, 2026-09-29) and it is
  correct, but ~225 copies of it are ~225 places to get the name, receiver,
  borrowed arguments or foreign rename wrong, and nothing makes a new export
  write one. The attribute is one token per block, and the scan below makes it
  mandatory.
- **Enforced by a scan, not by review:**
  `fauna_uniffi_async::tests::no_async_export_bypasses_the_attribute` fails on a
  bare `uniffi::export(async_runtime = …)` anywhere under `libs/`, `bins/` or
  `apps/`; it runs in the merge-gate check's `workspace-test-check`. The
  expansion itself is pinned by the same crate's thread-identity tests
  (`Runtime::block_on` polls the twin on the calling thread with the runtime
  context entered — what a foreign executor does — and the work must record a
  different thread), plus the abort-on-drop and panic-propagation tests.

### C#: the FFI layer never needs the caller's thread back (2026-09-06)

**A UniFFI async call begun on a thread must run to completion without needing
that thread again.** On .NET the poll loop is ordinary `async`/`await` code, so
by default every iteration after the first resumes on the caller's captured
`SynchronizationContext`. WinUI's `DispatcherQueueSynchronizationContext` is
single-threaded — so a call issued from a ViewModel gets poll 1 on the UI
thread, and then queues polls 2..N *behind whatever that thread does next*. The
Rust future has been entered and has set whatever in-flight flag it keeps, but
its request is never issued and its response is never consumed. Worse, poll 1 is
**synchronous**, so a second call whose first poll blocks on state the starved
call holds blocks the UI thread outright, and the two deadlock: the holder needs
a thread only the waiter can release.

**The rule is enforced in the generator, once, for every export** (#2): both
awaits in `libs/uniffi-bindgen-cs/templates/Async.cs` (`_UniFFIAsync.PollFuture`
and `UniffiRustCallAsync`) and the one in `templates/macros.cs`'s `async_call`
wrapper carry `ConfigureAwait(false)`. **This does not move app code off the UI
thread**: a ViewModel's own `await machine.DoThing()` still captures the UI
context and still resumes on the UI thread, which is what a ViewModel needs —
only the FFI round trip underneath it goes context-free. The distinction matters
because the app-side convention is the opposite one: **a WinUI ViewModel must
not use `ConfigureAwait(false)`** (it throws `COMException` when a continuation
touches XAML off-thread). *Generated interop code* is library code and takes the
library rule; *ViewModels* take the UI rule.

**Do not fix this per call site.** `await Task.Run(() => machine.DoThing())` at a
call site works, but a ViewModel makes dozens of machine calls and the app has
many ViewModels — one missed site keeps the defect, and nothing tells you which.

Pinned by `FaunaApp.Tests.UniffiAsyncOffUiThreadTests`, which starts a machine
call on a purpose-built single-threaded `SynchronizationContext`, occupies that
thread, and asserts the call completes anyway (red-verified against the
pre-2026-09-06 templates: the call never completed, 20 s budget). Swift and
Kotlin need no counterpart — neither foreign executor is a single-threaded
context bound to the caller.

**So the sweep question is now two questions, not one.** For each FFI-reachable
export — **not only the async ones** — *(a)* does it hold a large future inline
(→ box the leaf), and *(b)* does it reach heavy synchronous work — PQ crypto
above all — on the poll stack (→ spawn it off)? Every enumeration in
§ Implementation status today before 2026-08-23 answers only (a); an entry saying
"safe" predates the second question and has not been asked it.

⚠ **A *synchronous* export is the hard case, and neither remedy above reaches
it**: there is no future to `Box::pin`, and `tokio::spawn` needs both an
`async_runtime = "tokio"` export's already-entered runtime context and an await
point to return through. It runs directly on the foreign caller's stack, full
stop. `fauna-ffi` has **19** such exports reaching a PQ leaf. For that shape the
remedies are, in order: **keep the leaf cheap** (the 2026-08-25 measurement — a
dependency optimization level, one owner, covers every export at once and is what
actually closed this class); then a dedicated large-stack `std::thread` +
current-thread runtime; then a documented guarantee about which thread the app may
call it from — which is the weakest, because nothing enforces it.

⚠ **And ask question (b) about the DEPENDENCY'S BUILD PROFILE before auditing call
sites.** The same crypto cost 290 KB of stack unoptimized and 16 KB optimized
(§ Implementation status today). An audit that enumerates exports without knowing
that number is measuring the wrong thing: it will find dozens of "exposed" paths
that are fine, and it cannot tell a caller that is safe from one that is 3 KB from
a guard page.

**Guard it with a future-size assertion**, not a stack-overflow test (an overflow
aborts the process and can't be caught, and any chosen small-stack threshold is
environment-fragile). A `std::mem::size_of_val(&the_future)` upper-bound test is
deterministic, server-free, and fails loudly if a refactor un-boxes the future —
see `fauna-anon-client::ws::tests::connect_anonymous_future_stays_small_for_foreign_stacks`.

⚠ **A future-size assertion does not see a poll frame.** A future's size is the
largest of its states. On an unoptimized build an `async fn`'s poll frame is the
*sum* of its temporaries, because each gets a stack slot of its own. So a long
`async fn` can hold a small future and still need most of a megabyte of stack
per poll, and un-boxing one of its sub-futures can deepen the stack without moving
any future's size (the account store thread: a 53,504-byte future under an
865,456-byte poll frame — [`../account-runtime.md`](../account-runtime.md)
§ Implementation status today). The assertion guards the shape it was written
for, a large future held inline on a small stack. Where the path runs on a thread
fauna spawns and sizes itself, **guard the depth as well**: a test double at the
path's leaf records how far below the thread's entry the stack is, and the test
asserts that against a share of the thread's stated stack. Nothing overflows, so
nothing aborts and no threshold has to be found by bisection — see
`fauna_sync_engine::account_runtime::tests::a_pass_runs_inside_half_the_store_threads_stack`,
and, for a leaf only another crate's suite can reach (the linked-nest leg, over
real in-process nests), `bins/fauna-nest`'s `conformance_account_plane_bind`,
which reads the same depth through `store_thread_stack_depth`. A leaf inside
dependency code has no place for a double (a TLS handshake under a login dial):
there the same suite reads the thread's stack pages instead — it hands the unused
ones back to the kernel before the work and reads which are present after it.

## Implementation status today

- **A synchronous export that reaches `tokio::spawn` panics on every call —
  `custody_drive`, FIXED 2026-09-14.** § The rule
  names the missing runtime context as a limit on the *stack* remedies for a
  synchronous export. It is also a correctness failure on its own.
  `libs/fauna-ffi/src/custody.rs`'s `custody_drive` was the one plain
  `#[uniffi::export]` in the custody face. It called
  `fauna_client_custody::spawn_drive`, whose `tokio::spawn` needs an entered
  runtime, and a foreign caller never has one. So every call panicked with
  "there is no reactor running", on windows, macOS/iOS and android alike. Each
  app caught the panic as a best-effort failure, so the owner-side ceremony
  drive never ran on any UniFFI app and nothing reported it. On windows the
  panic also skipped the facet load behind it. Fixed by making the export
  `async_runtime = "tokio"` like its three siblings, which is § The execution
  model's mechanism, and not by a fallback runtime. Pinned at the real crossing
  by `FaunaApp.Tests.CustodyDriveFfiTests`. ⚠ **The search key for this class
  is a synchronous `#[uniffi::export]` that reaches `tokio::spawn`, or anything
  else that needs `Handle::current()`.** As of this fix, `fauna-ffi`'s other
  non-test spawn sites (`account_runtime::install` via `start_account_runtime`,
  `run_succession_aftermath`, `FfiCustodianHost::start_push_debounce`) all sit
  behind async exports. A spawn inside a shared crate reached from a
  synchronous export is invisible to that grep, which is exactly how this one
  hid.

- **C#'s poll loop no longer captures the caller's context — LANDED 2026-09-06**
**.** All three awaits in the generated
  UniFFI async layer carry `ConfigureAwait(false)` (`templates/Async.cs` ×2,
  `templates/macros.cs`'s `async_call` ×1), so every one of the 550 generated
  async wrappers now completes off the caller's thread. Before this, a machine
  call issued from a WinUI ViewModel needed the UI thread back for every poll
  after the first: `test_bundled_provider.py --app windows` stalled at a
  *different* step in seven consecutive runs, always with the machine's
  in-flight flag set and the provider fake showing no request for the stalled
  call. Regenerate with `just windows-ffi{,-test}` after any template change —
  `build-system.md` § Regenerating after a generator change keys the staleness
  gate on `libs/uniffi-bindgen-cs/templates`, so this propagates by itself.
  Swift/Kotlin unaffected (no single-threaded foreign executor).

- **Post-succession aftermath (`fauna-ffi` `run_succession_aftermath`) — the
  first measured incident of the SYNCHRONOUS class, found and FIXED 2026-08-23
.** The export awaited
  `fauna_client_recovery::aftermath::run_succession_aftermath` inline on the FFI
  poll stack; legs 4 and 6 reach ML-KEM-768 through `fauna_pq_kem`, ≈548 KB of
  synchronous frames against Apple's 544 KB cooperative-pool stack, so the app
  **died** on the guard page — no Rust panic, nothing on the progress sink. See
  § The second measured incident for the two chains and the measurements. Fixed
  by `tokio::spawn`ing the pass in `libs/fauna-ffi/src/succession_aftermath.rs`,
  the § The rule remedy for a leaf that cannot be boxed. ⚠ **The class was then
  swept, and what closed it was not a call-site fix — see the next bullet.**

- **The class is CLOSED at one owner: the crypto's optimization level, measured
  2026-08-25.** The sweep this incident opened asked which *other* exports reach
  ML-KEM on the caller's thread. The enumeration found **51** — and then the
  measurement made almost all of them moot, by answering a question nobody had
  asked: *how much stack does the leaf actually want?*

  | `fauna-pq-kem` entry point         | `dev`, unoptimized dep | `dev`, `opt-level = 2` | `release` |
  |------------------------------------|-----------------------:|-----------------------:|----------:|
  | `derive_mlkem768_keypair_from_ikm` |                 227 KB |                  32 KB |    ≤16 KB |
  | `derive_keypair`                   |                 243 KB |                  32 KB |    ≤16 KB |
  | `derive_keypair_from_ikm`          |                 243 KB |                  32 KB |    ≤16 KB |
  | `encapsulate`                      |                 259 KB |                  32 KB |    ≤16 KB |
  | `decapsulate`                      |                 290 KB |                  32 KB |    ≤16 KB |

  (aarch64-apple-darwin, libcrux-ml-kem 0.0.8, each leaf on a thread of exactly
  that stack size in a child process — a guard-page hit is the measurement, so
  nothing is inferred. ≤16 KB is macOS's minimum thread stack, i.e. the release
  figure sits at the floor the platform will allocate.)

  ⚠ **So the discriminant is the DEPENDENCY'S OPTIMIZATION LEVEL, not the export,
  not the actor, and not the call site.** A shipped app was never near the guard
  page — 16 KB against Swift's 544 KB. `just mac-debug` and `just swift-test`
  build `fauna-ffi` **unoptimized**, which put 290 KB of crypto under whatever
  chain the export already had; that is why the crash reproduced in journey runs
  and has never been seen in a shipped artifact. The two prior readings of this
  incident (§ The second measured incident, and the actor-isolation bullet below)
  were each true about *their* half and neither reached this one.

  **The fix is one line at the single owner** (#2): the root `Cargo.toml` pins
  `[profile.dev.package.libcrux-ml-kem] opt-level = 2`, which holds every entry
  point at 32 KB — 6% of the smallest known foreign stack. It covers all 51
  exports at once, **including the 19 synchronous ones no `tokio::spawn` remedy
  can reach** (a sync export has no runtime context and no await point), plus
  every export added later, on all 7 apps and the nest. `opt-level = 2` and not
  `3` is measured, not taste: at 3 the extra inlining grows `derive_keypair`'s
  frame back to 48 KB. **Guarded by `libs/fauna-pq-kem/tests/stack_budget.rs`**,
  which asserts a 128 KB ceiling per leaf and goes red if the override is
  removed (red-verified: SIGABRT at 128 KB without it).

  ⚠ **What this does NOT close.** (a) The *responsiveness* axis is untouched —
  ML-KEM keygen is still CPU-bound work, and the corollary below ("it runs on the
  main actor is NOT an acceptable remedy") still holds, so the `tokio::spawn` /
  `spawn_blocking` fixes already landed stay correct for that reason and must not
  be reverted as redundant. (b) **windows measured 2026-08-30 — safe, 48× margin.**
  Unlike Swift's cooperative pool or Kotlin's coroutine dispatcher, .NET has no
  dedicated small-stack async-executor pool: reading the generated
  `_UniFFIAsync.PollFuture` (`FaunaApp.Core/Generated/uniffi/fauna_ffi.cs`), its
  FIRST poll call happens SYNCHRONOUSLY on whatever thread calls the async
  wrapper — a `Task` only truly suspends once the Rust future genuinely yields
  across an await boundary, and a fully-synchronous chain (the PQ-crypto class
  here) never yields at all, so it runs entirely on the CALLING thread's own
  stack. There is no separate foreign-executor thread to measure — only the
  ordinary CLR thread that happened to call in (the WinUI UI thread for a
  VM-issued call's FIRST poll; a .NET ThreadPool worker for every poll after it,
  and for calls issued off the UI thread — § The rule → *C#: the FFI layer never
  needs the caller's thread back*), and both get the OS-linked default stack
  size, so the margin below is the same either way.
  Measured directly via `GetCurrentThreadStackLimits` (a first-party, Vista+
  documented OS API — "retrieves the boundaries of the stack that was
  allocated by the system for the current thread") on a freshly created
  thread with no `maxStackSize` override — the same default every real call
  site gets: **1,572,864 bytes (1536 KB / 1.5 MB)**, aarch64 Windows 11
  (win-arm64), .NET 10. Against the 32 KB `opt-level = 2` PQ leaf: **≈48×
  margin**. Even against the pre-fix unoptimized worst case (290 KB,
  `decapsulate`) the margin would still be ≈5.4× — windows was never exposed
  by this class, on either build profile. Pinned by
  `FaunaApp.Tests.ForeignExecutorStackSizeTests` (asserts the default stack
  clears 10× the 32 KB leaf, so a future .NET/OS default shrinking this
  re-reds the suite rather than silently eroding the margin). android's foreign-executor stack size remains **unmeasured** —
  record it, never assume a typical value; at 32 KB per leaf the margin is
  now large on any plausible executor, but "large" is not a
  measurement.
  (c) **Resolved**: `fauna_mls::wrapped_blob::dav_body` now
  exposes `DavRecipientKeys` — one X-Wing derivation, reused across every item in
  a batch — and every CalDAV/CardDAV call site (`fauna-client-caldav`,
  `fauna-client-carddav`, `fauna-client-conversations`, both `fauna-ffi` DAV
  clients, `fauna-wasm`, `fauna-linux`, `fauna-tui`) threads it down from the
  caller that opens the batch instead of deriving per item. `unseal_dav_body`
  stays as a thin per-item wrapper for the single-item case. Pinned by
  `n_opens_under_one_derivation_cost_one_keygen`
  (`libs/fauna-mls/src/wrapped_blob/dav_body.rs`), a `thread_local!` counter
  test per convention 14 (derivations, not milliseconds).

- **The actor does NOT keep a UniFFI call on the main thread — REFUTED by the
  third measured incident (2026-09-29).** `FaunaMacOS` `SIGBUS` (stack-guard
  region), debug build, on a `com.apple.root.user-initiated-qos.cooperative`
  thread whose `vmRegionInfo` puts the stack at **544 KB**: `TrustBackupItemView`
  → `LinkedNestsVM.dispatch` (a `@MainActor` view model) →
  `MachineBackedVM.dispatchToMachine` → the generated
  `LinkedNestsMachine.dispatch(action:)` → `uniffiRustCallAsync` →
  `ffi_fauna_client_pair_rust_future_poll_void` → `LinkedNestsMachine::dispatch`
  → `revoke_backup_writer` → `refresh` → `build_home_row` →
  `backup_trust_rows` → `writer_row_state` → `NativeDestinationConnector` →
  `NestClient::connect` → `AuthClient::authenticate` → `WsChallengeBearer` →
  `AnonymousNestClient::connect` → `connect_anonymous` → the (already boxed)
  tungstenite handshake — 67 frames, every one appearing once. The generated
  `uniffiRustCallAsync` is a *nonisolated* `async` function, so Swift runs it —
  and with it every Rust poll — on the cooperative pool **whatever actor the
  caller is isolated to**. The bullet below reasons from the opposite premise
  and its "safe for a reason no one chose" conclusion does not hold: an FFI
  async export reached from a `@MainActor` view model is exactly as exposed as
  one reached from a bare `Task {}`. Nor could boxing reach this one: the leaf
  future is boxed; what eats the stack is the depth of the synchronous poll
  frames of a debug build's deep async chain, which is § The rule's
  spawn-at-the-export case. Fixed first for `LinkedNestsMachine` by hand-written
  spawning wrappers, then for **every** async export (next bullet). The
  machine's own pin, `ffi_exports_run_the_machine_off_the_foreign_poll_thread`
  (`libs/fauna-client-pair/src/lib.rs`), now drives the generated twins.

- **Every UniFFI async export runs on a tokio worker — LANDED 2026-09-29.** The sweep the bullet above opened asked which
  async exports await a connect/handshake/bearer chain or a PQ leaf inline. The
  answer is recorded as a rule, not a list (§ The rule → *Every async export
  runs on a tokio worker*): all 225 `async_runtime = "tokio"` export attributes
  now read `#[fauna_uniffi_async::export]`, and none was judged unnecessary.
  By crate: `fauna-ffi` (208, across 69 files — every client handle, the
  account/recovery/succession/custody/folders free functions, the resolve and
  mail-import exports), `fauna-client-mail-settings` (12 — including the two
  `bool_toggle_policy_machine!` expansions), `fauna-onboarding-machine` (8,
  the `test-helpers`-gated async twin included), `fauna-conversations` (3),
  `fauna-mail` (2), and one block each in `fauna-atproto-settings-machine`,
  `fauna-backups-machine`, `fauna-client-connected-apps`, `fauna-client-dns`,
  `fauna-client-pair`, `fauna-devices-machine`, `fauna-folders-machine`,
  `fauna-labeler-catalog-machine`, `fauna-launch-machine` and
  `fauna-media-machine`. What the audit found, and why it ends in "all":
  (a) a request on an already-connected `NestClient` is shallow — it waits on
  the supervisor's own spawned task and never dials inline — but most exports
  reach `NestClient::connect` / `ensure_auth` / `AnonymousNestClient::connect`
  somewhere down a machine's refresh or a handle's first use, and a name-level
  reachability pass over the workspace could not separate them (every generic
  name reaches a connect somewhere); (b) the PQ leaves alone are already closed
  by `opt-level = 2` (the bullet below), so PQ no longer decides anything;
  (c) the exports that already hand their work to a dedicated worker thread
  (`sync_engine_host`, `file_provider_host`) gain a redundant hop and nothing
  else. Synchronous `#[uniffi::export]`s are out of this rule's reach by
  construction (no runtime context, no await point) and stay covered by the
  PQ optimization-level fix.

- **What decides safety on Apple is WHICH ACTOR drives the call, and today the
  safe cases are safe by accident — measured 2026-08-23, row 196.** ⚠ Its
  premise is REFUTED (bullet above, 2026-09-29): a `@MainActor` caller does not
  keep the Rust poll on the main thread. The datum
  that forced this: in the same journey run that crashed, a user minting a trust
  **through the app UI** drove the identical `mint_grant` →
  `seal_capability_xwing` → `encapsulate` chain and did *not* crash. ⚠ **The
  obvious reading — "some Rust seam already protects that path" — is REFUTED**:
  that path has no intervening `tokio::spawn` at all (`fauna-client-pair`'s
  `LinkedNestsMachine::dispatch` → `mint` → `mint_grant`), and `fauna-ffi` holds
  only six non-test spawn sites in total. The difference is **Swift-side**:
  `LinkedNestsVM` is `@MainActor`, so that call ran on the main thread's ~8 MB
  stack, while `SuccessionAftermath.run` uses a bare `Task {}` — no actor
  isolation, hence the 544 KB cooperative pool. All 50 FaunaKit view models are
  `@MainActor`, so **every UI-driven call is currently safe for a reason no one
  chose**, and a future refactor moving any VM off the main actor reintroduces
  the crash with no code change at the call site.
  ⚠ **This widens the audit rather than shrinking it**, and it makes the
  *fire-and-forget* passes the exposed surface, not the UI — but **the exposed set
  is narrower than first recorded, and the test below now pins why.**
  ⚠ **CORRECTED 2026-08-24: `FaunaClient`'s bare `Task {}` helpers do NOT run on
  the small pool.** The earlier text here said they did "by construction"; they do
  not, because `FaunaClient` is itself `@MainActor`
  (`apps/fauna-apple/FaunaKit/Sources/FaunaKit/Core/FaunaClient.swift:4`) and an
  unstructured `Task {}` inherits the actor isolation of its enclosing
  declaration — **including on a `static` member**, since a global-actor attribute
  isolates a type's statics too. `refreshMailEpochSchedule` (`:463`) and
  `runSealBackfill` (`:475`) are statics on that `@MainActor` class, so both
  inherit the main actor and its ~8 MB stack. What made
  `SuccessionAftermath.run` different is not that it was fire-and-forget but that
  `SuccessionAftermath` is a bare `enum` carrying **no** global actor
  (`SuccessionAftermath.swift:29`), so its `Task {}` inherited none either. The
  discriminant is the *type's* isolation, never the fire-and-forget shape.
  **Asserted, not reasoned:** `ForeignStackIsolationTests` pins the rule in both
  directions — a `@MainActor` type's static keeps the main thread, a non-isolated
  type's static does not — so dropping `@MainActor` from `FaunaClient`, or a Swift
  release changing static-member isolation inheritance, fails at build time
  instead of becoming a crash with no panic and no log.
  ⚠ **`refreshMailEpochSchedule` is therefore NOT a latent crash — it is a
  RESPONSIVENESS defect, and it still wants the same fix.** Its chain
  (`MailSettingsMachine::refresh_epoch_schedule`,
  `libs/fauna-client-mail-settings/src/machine.rs:2087` → `provision_snapshot_for`
  `:2108`) is the same one crash 2 died in and it does run on **every
  authenticated start** — but on the main thread, so the stack is ample and the
  cost lands on the UI instead. By the corollary below ("it runs on the main actor
  is NOT an acceptable remedy") that is still the wrong place for ML-KEM keygen,
  and the remedy is unchanged: a Rust-side `tokio::spawn` at the FFI export.
  Reclassifying it matters for *triage*, not for whether to fix it — it is not the
  emergency the earlier text implied, and it is not dismissed either.
  ⚠ **So the enumeration's search key is a Swift-side one: types that reach an FFI
  export from a context carrying no global actor.** `SuccessionAftermath`'s shape
  is the template to grep for, not `Task {}` on its own — and the 50 `@MainActor`
  view models remain safe for a reason nobody chose, exactly as above.
  ⚠ **Corollary: "it runs on the main actor" is NOT an acceptable remedy** even
  though it happens to work — heavy post-quantum keygen on the UI thread is the
  wrong place for it on the other axis. The Rust-side spawn is the only fix that
  is correct about both stack and responsiveness.
  ⚠ **The FFI-reachable-surface crate list is corrected** to include
  `fauna-client-pair` (`lib.rs:1436`, `uniffi::export(async_runtime = "tokio")` on
  `impl LinkedNestsMachine` → `mint` `:1011` → `mint_grant` `:1033`) and
  `fauna-client-mail-settings` (`machine.rs:2036`, `mint_grant` `:1132`) — both
  reach the named leaves inline and appeared in **no** enumeration in this doc.

- **Anon-WS connect path — DONE 2026-06-17, re-verified 2026-07-23 and
  2026-08-05.**
  `fauna_anon_client::ws::connect_anonymous` `Box::pin`s every one of its
  connect branches — the `wss://` (`connect_async_tls_with_config`), plain
  `ws://` (`connect_async_with_config`), and the resolve-override
  (`client_async_tls_with_config`, dial-by-IP before DNS propagates, added
  2026-07-06) branches alike; size-guard test pins the future ≤ 2048 B (296 B
  today; 10.9 KB un-boxed). Covers every anon-WS caller (mint /
  silent-challenge / registration / claim / invite / storage-mode, the
  recipient/attendee anonymous-discovery exports `resolve_nest` /
  `resolve_handle` (`libs/fauna-ffi/src/resolve.rs`) and
  `classify_attendee_transport`'s `AnonAttendeeDiscovery`
  (`libs/fauna-client-caldav/src/lib.rs`), plus the cross-nest handle
  resolution added by the cross-nest-access-discovery feature —
  `ConversationsManager::resolve_recipient` (`libs/fauna-conversations/src/manager.rs`,
  `#[uniffi::export(async_runtime = "tokio")]`) → `probe_address` →
  `FaunaMlsBackend::resolve_foreign` (`libs/fauna-conversations/src/backends/fauna_mls.rs`)
  → `NestConversationsRpc::actor_by_handle_remote`
  (`libs/fauna-client-conversations/src/lib.rs`)) and flows to all native
  apps behind the FFI, automatically, because the box sits at the shared
  leaf.
- **Authenticated `NestClient` connect path — AUDITED 2026-06-17, re-verified
  2026-07-19 and 2026-08-05 (leg 2's mechanism changed, still SAFE — see below),
  SAFE (no fix needed).** The authenticated connect performs
  **two** TLS/WS handshakes, both off the small FFI poll stack — by two
  *different* mechanisms:
  1. **Bearer-mint** (`fauna.auth.{challenge,verify}` since 2026-09-23; the
     handshake before — same connect): `FfiNestClient::connect`
     (`libs/fauna-ffi/src/nest_client.rs`) → `NestClient::connect`
     (`libs/fauna-client/src/client.rs:163` `self.auth.authenticate()`) →
     `WsChallengeBearer::bearer`/`fetch_mint` → `fauna_anon_client::mint_bearer_over_silent_challenge`
     (`libs/fauna-anon-client/src/bearer.rs`) → `AnonymousNestClient::connect`
     → `connect_anonymous` — **already `Box::pin`'d** (every boxed connect
     branch in `libs/fauna-anon-client/src/ws.rs` — cite the symbol, the lines
     drift). This leg is awaited *inline* on the FFI poll stack, but its
     10.9 KB handshake future is heap-boxed, so it's safe (same fix as the anon
     path).
  2. **Bearer-authenticated WS** (`connect_with_subprotocol_bearer`,
     `libs/fauna-client/src/ws_adapter.rs`): reached *only* via the
     `tokio::spawn`'d supervisor — `fauna_ws_substrate::run_supervisor`,
     spawned in `libs/fauna-client/src/client.rs` (`tokio::spawn(run_supervisor(cfg))`)
     → `cfg.channel.connect()` → `connect_with_subprotocol_bearer`. `run_supervisor`
     is **never awaited inline** (its only non-test caller is that `tokio::spawn`),
     so this handshake always runs on a tokio worker thread (~2 MB stack), never
     the small FFI poll stack — safe by that mechanism alone, same as before.
     **Since the 2026-08-02 shared-trusted-dial consolidation**, `connect_with_subprotocol_bearer`
     also routes through `fauna_anon_client::tls_dial::dial_ws_trusted_with_graduate_retry`
     → `dial_ws_trusted` — the same `Box::pin`'d leaf `dial_ws_trusted` (below)
     shares with `fauna-anon-client::ws::connect_authed` (and, until its removal
     2026-10-02, `bins/fauna-sync`'s `/sync/ws` dial) — so this leg is now **also** boxed: belt-and-suspenders, not
     a gap. **The invariant a future refactor must preserve — across any crate
     move — is that `run_supervisor` stays `tokio::spawn`'d** (if it were ever
     awaited inline within an FFI export, this leg would still be safe today only
     because the leaf is boxed; losing *either* mechanism independently is fine,
     losing both is the hazard).
- **One-shot authenticated read (`TokenNestClient`, box-recovery native
  config-read leg) — VERIFIED 2026-07-19, re-verified 2026-08-05 (boxing moved
  to the shared `tls_dial` leaf, still SAFE), no fix needed; landed
  2026-07-07, after the 2026-06-17 audit above, so it wasn't yet
  covered).** `libs/fauna-onboarding-machine`'s native `RecoveryConfigReader`
  (`recovery_config.rs`) mints a bearer over `fauna.auth.handshake` then
  cold-reads the account plane's deployment seeds (the `__config` read it made
  before retired with the rail, 2026-10-02 —
  [`../config-dissolution.md`](../config-dissolution.md) § The `__config`
  dissolution schedule → *The closure order*, step (6)) via
  `fauna_anon_client::token_client::TokenNestClient::connect` →
  `crate::ws::connect_authed` → the shared **`Box::pin`'d** dial
  `crate::tls_dial::dial_ws_trusted` (`libs/fauna-anon-client/src/tls_dial.rs` —
  boxes both its pinned-SPKI and strict-WebPKI branches; `connect_authed` itself
  moved onto this shared leaf 2026-08-02, same shared-leaf principle as § The
  rule above), same rule as `connect_anonymous`. It is also reached only via a
  **spawned** task, never
  awaited inline on the FFI poll stack: `OnboardingMachine::start_provisioning`
  (`libs/fauna-onboarding-machine/src/machine.rs`) is a **sync** fn that
  `tokio::spawn`s (native) / `wasm_bindgen_futures::spawn_local`s (wasm)
  `run_provisioning_inner`, which is the sole production caller of
  `resolve_deployment_seed_and_domain` → `RecoveryConfigReader::resolve` →
  `TokenNestClient::connect`. Safe by **both** mechanisms independently (boxed
  leaf *and* never inline-polled) — belt-and-suspenders, not a gap.
- **Other `fauna-ffi` async exports — AUDITED 2026-06-17, re-verified
  2026-07-29 and 2026-08-05, no unsafe offenders.** A full-workspace grep for the
  `tokio-tungstenite`/`AnonymousNestClient::connect` connect call sites today
  additionally turns up `libs/fauna-sidecar-client/src/lib.rs` (the shared
  sidecar dialer for `fauna-iroh-relay`; depended on only by
  `bins/fauna-nest`, `bins/fauna-iroh-relay`, `libs/fauna-peer-channel` — never
  `fauna-ffi`), `bins/fauna-sync/src/ws_client.rs` (removed 2026-10-02),
  `bins/fauna-nest/src/federation_channel.rs`,
  `bins/fauna-nest/src/federation_pool.rs` (the `FederationChannelPool`'s
  URL→`nest_id` resolve — a longer-standing site this enumeration had missed
  until this sweep), `bins/fauna-nest/src/nest_link/client.rs`,
  `bins/fauna-router/src/ws_proxy.rs` (incl. `proxy_handler.rs` in the same
  binary), (added 2026-07-23 by the cross-nest byte-plane pin graduation, item
  1c) `bins/fauna-sync-agent/src/bridge.rs`'s `graduate_home_nest_pin` (dials
  the set's home nest via `fauna_anon_client::AnonymousNestClient::connect` to
  graduate its SPKI pin before the byte plane trusts it), (added 2026-07-29 by
  the identity-succession federation-propagation slice)
  `bins/fauna-nest/src/succession_pull.rs`'s `verify_and_record_from_home`
  (dials the actor's home nest to prove the anchor identity before trusting
  its succession-chain reply), `apps/fauna-linux/src/client.rs`'s
  `resolve_nest` / `resolve_handle_on_remote` (the linux app's own find-user
  domain/handle discovery flow, live since 2026-06-04 and distinct from the
  shared `resolve_recipient` path below — another longer-standing miss), and
  (added 2026-08-02/03 by the identity-succession client-side slice, found this
  sweep) four more app-native successor-verification sites: the same
  `apps/fauna-linux/src/client.rs`'s `verify_succession_successor`, and three on
  `fauna-tui` — `apps/fauna-tui/src/launch.rs`'s `verify_superseded_successor`,
  `apps/fauna-tui/src/conversations/conv_backend.rs`'s
  `TuiSuccessionChainSource::walk_from_domain`, and
  `apps/fauna-tui/src/settings/mod.rs`'s `finish_unconfirmed_succession` — all
  four dialing the actor's home/peer nest to verify a succession chain, none of
  them FFI-reachable (`fauna-tui`, like `apps/fauna-linux`, has no UniFFI layer
  at all — it is a native Rust binary with its own `tokio::main`) — all thirteen
  are nest/router/sync-agent/linux-app/tui-app-side-only, same class as
  `fauna-bridge-nostr/src/relay_client.rs` (depended on **only** by
  `bins/fauna-nest`, not `fauna-ffi` — it runs server-side on the nest's
  runtime, never a foreign FFI stack; `fauna-sync-agent` is likewise its own
  binary with its own `tokio::runtime::Runtime` (`lib.rs`'s `run_main`),
  spawned as a per-user desktop sidecar process, never reached through UniFFI;
  the linux app has no UniFFI layer at all, so nothing in `apps/fauna-linux`
  is ever FFI-reachable by construction). None of these run on a foreign FFI
  poll stack. Since the 2026-07-19
  pass, the FFI-reachable surface (`fauna-ffi`
  itself, plus the crates it re-exports over UniFFI — `fauna-conversations`,
  `fauna-launch-machine`, `fauna-client-caldav`, and, corrected this sweep (see
  the **ninth** recurrence below), `fauna-onboarding-machine`) has grown four call sites that
  open a fresh anonymous connection rather than reusing an established
  dispatcher: `resolve_nest` / `resolve_handle` (`libs/fauna-ffi/src/resolve.rs`)
  and `classify_attendee_transport`'s `AnonAttendeeDiscovery`
  (`libs/fauna-client-caldav/src/lib.rs`) — audited 2026-07-22 — plus (found in
  the 2026-07-23 sweep, added by the cross-nest-access-discovery feature)
  `ConversationsManager::resolve_recipient`
  (`libs/fauna-conversations/src/manager.rs`) → `probe_address` →
  `NestConversationsRpc::actor_by_handle_remote`
  (`libs/fauna-client-conversations/src/lib.rs`) — reached by every native FFI
  app (windows/macos/ios/android; linux calls the same `resolve_recipient`
  in-process, never through UniFFI, so it never hits a foreign stack here). All
  four route through `fauna_anon_client::AnonymousNestClient::connect` →
  `connect_anonymous`, i.e. the same already-boxed leaf as `mint_bearer` /
  `silent_challenge`, so they're safe with **no additional fix** — this is the
  shared-leaf design in § The rule paying off across a growing caller set, not
  a gap. A **fifth** FFI-reachable group, found only on this sweep's re-audit
  despite predating the four above (`fauna-launch-machine` shipped
  2026-05-31, before `resolve_nest`/`resolve_handle` even existed): `LaunchMachine`'s
  five `#[uniffi::export(async_runtime = "tokio")]` methods
  (`libs/fauna-launch-machine/src/machine.rs` — `start`, `refresh_token`,
  `notify_401`, `retry_silent_challenge`, `trust_nest_identity`, re-exported
  into `fauna-ffi` at `libs/fauna-ffi/src/launch.rs`) all await **inline**, with
  no intervening `tokio::spawn`, down through the `AuthConnector` trait's
  production impl `WsAuthConnector` (`libs/fauna-launch-machine/src/connector.rs`)
  into `probe_setup_status` (`libs/fauna-launch-machine/src/probe.rs`) and
  `connect_token_refresh` / `connect_silent_challenge`
  (`libs/fauna-launch-machine/src/auth.rs`) — three more
  `AnonymousNestClient::connect` call sites, all resolving to the same
  already-boxed `connect_anonymous` leaf, so likewise safe with **no additional
  fix**. (`fauna-tui`/`fauna-linux` also construct and drive `LaunchMachine`
  directly, `tokio::spawn`'d on their own runtimes with the `uniffi` cargo
  feature off — same non-FFI class as their other sites above, not a second
  FFI path.) So the claim immediately below undercounted: **not every other**
  `fauna-ffi` async export reuses an established dispatcher — these five do
  open a fresh one per call, same as the other four groups; every remaining
  `fauna-ffi` async export **outside all five groups** operates on the
  already-established dispatcher (no new TLS/WS handshake). No further boxing
  required today. **This enumeration recurringly goes stale as new async
  anon-client call sites land faster than the doc tracks them** — sweeps 55
  and 63 each found a new **FFI-reachable** export (added to the four-site list
  above); sweep found a new **non-FFI** caller (`fauna-sync-agent`'s
  `graduate_home_nest_pin`); the 2026-07-29 re-check
  found a **sixth** consecutive recurrence — again non-FFI:
  `succession_pull.rs`'s `verify_and_record_from_home`, added the same day by
  the identity-succession federation-propagation slice — and, from
  re-auditing the whole enumeration rather than trusting it, two
  longer-standing non-FFI sites that had escaped every prior sweep
  (`federation_pool.rs`'s peer-resolve, and the linux app's own
  `resolve_nest`/`resolve_handle_on_remote`), all now folded into what was then
  a nine-site list; a 2026-08-05 re-check found a **seventh** consecutive
  recurrence — again non-FFI, and again the identity-succession client-side
  work, landing 2026-08-02/03, three days after the sixth recurrence's fix: the
  four sites folded into the thirteen-site list above (linux's
  `verify_succession_successor` plus tui's three). **Both non-FFI recurrences
  six and seven trace to the same feature
  (identity-succession) landing its nest-side and client-side legs days apart**
  — this doc's own enumeration cannot get ahead of a feature that adds a fresh
  anonymous dial on each leg. This sweep (2026-08-13)
  found an **eighth** consecutive recurrence, of the *other* kind: **FFI-reachable**
  and *not* freshly landed — the `LaunchMachine` group above went unlisted since
  the crate shipped (2026-05-31, predating this doc itself, which was written
  2026-06-17), surviving every prior audit of "the FFI-reachable surface"
  because that audit re-grepped
  `AnonymousNestClient::connect` call sites in `fauna-ffi`/`fauna-conversations`/
  `fauna-client-caldav` but never walked `fauna-launch-machine`'s own connect
  sites forward to the `#[uniffi::export]` methods that reach them inline — the
  gap was in *tracing an existing export's call graph*, not in noticing a new
  export.

  This sweep (2026-08-17) re-grepped both categories per
  the standing instruction above and found a **ninth** consecutive recurrence,
  the same shape as the eighth: **FFI-reachable, not freshly landed, missed by
  the crate-name grep rather than by an export going unnoticed.**
  `fauna-onboarding-machine` is a **non-optional** `uniffi`-featured dependency
  of `fauna-ffi` (`libs/fauna-ffi/Cargo.toml`: `fauna-onboarding-machine = {
  path = "../fauna-onboarding-machine", features = ["uniffi"] }`), yet it never
  appeared in "the FFI-reachable surface" list above. Walking its own
  `#[uniffi::export(async_runtime = "tokio")]` surface forward turns up a
  **sixth and seventh** FFI-reachable group, both built on the crate's shared
  `WsNestApi` (`libs/fauna-onboarding-machine/src/nest_api/ws_nest_api.rs`),
  whose own module doc is headed "## One fresh connection per call" (`:22`):
  every `NestApi` trait method — `probe_setup_status`, `silent_challenge`,
  `submit_invite_request`, `recheck_invite_request`, `verify_invite_code`,
  `register`, `claim_admin`, `submit_nat_mode` (`submit_storage_mode` left with
  the storage-mode kind, 2026-09-24) — calls
  `self.core(base_url)` (`ws_nest_api.rs:416`) →
  `fauna_anon_client::AnonymousNestClient::connect_resolving`
  (`ws_nest_api.rs:316`) → `connect_with_registry_resolving`
  (`libs/fauna-anon-client/src/client.rs:133`) → the same already-`Box::pin`'d
  `crate::ws::connect_anonymous` leaf (`client.rs:138`) — so both new groups
  are **safe with no additional fix**, same shared-leaf payoff as the other
  seven. ⚠ **Line citations below re-verified and refreshed sweep
  (2026-09-16)** — `machine.rs` and `ws_nest_api.rs` have each grown several
  hundred lines from unrelated feature work since this enumeration was
  written; every symbol and call chain still resolves exactly as described,
  only the line numbers moved.
  - **Group six — `OnboardingMachine`'s own wizard-flow methods**
    (`libs/fauna-onboarding-machine/src/machine.rs`), all awaiting
    `self.nest_api` **inline**, no intervening `tokio::spawn`, same risk
    profile as `LaunchMachine`'s group five: `probe_setup_status_at` (:1520 →
    `probe_setup_status` :1524), `start_handle_check` (:3349 →
    `silent_challenge` :3582, `probe_setup_status` :3647),
    `wizard_submit_invite_request` (:3950 → `submit_invite_request` :4022),
    `recheck_invite_status` (:4107 → `recheck_invite_request` :4137, and its
    private `resolve_not_found_recheck` helper :4225 → `silent_challenge`
    :4234), `verify_oob_invite_code` (:4276 → `verify_invite_code` :4281),
    `redeem_invite` (:4314 → `register` :4408), `wizard_submit_claim_code`
    (:4632 → `claim_admin` :4728; its post-claim storage-mode compat leg
    retired 2026-09-24), `recheck_manual_dns` (:4830 → the shared
    `claim_provisioned_box` helper :5325 → `probe_setup_status` :5334,
    `claim_admin` :5468), and `submit_nat_mode_choice` (:5538 →
    `submit_nat_mode` :5573). These are the
    production call sites behind the "mint / silent-challenge / registration /
    claim / invite / storage-mode" caller list the Anon-WS connect path bullet
    already named above, never previously traced to file:line.
  - **Group seven — `AdminNatModeMachine`** (a second, independent
    `uniffi::Object`, `libs/fauna-onboarding-machine/src/admin_nat_mode.rs`),
    the post-onboarding Admin → Nest NAT-mode control: `hydrate` (:127 →
    `probe_setup_status` :133) and `submit` (:162 → `submit_nat_mode` :177),
    constructed with its own production `WsNestApi::new` at `:96`.

  Both groups predate this doc (`fauna-onboarding-machine` shipped before
  2026-06-17, same as `fauna-launch-machine`) and were missed for the same
  reason as the eighth recurrence: every prior audit walked
  `fauna-ffi`/`fauna-conversations`/`fauna-launch-machine`/`fauna-client-caldav`'s
  own connect sites but never `fauna-onboarding-machine`'s, despite it being
  uniffi-featured in `fauna-ffi`'s `Cargo.toml` from the start. The "FFI-reachable
  surface" crate list above is corrected to include it.

  **A separate, unaudited path was also found this sweep, outside this doc's
  `connect_anonymous` scope**: `fauna-ffi`'s `FfiMailImportClient` (native
  source-IMAP mail import, `libs/fauna-ffi/src/mail_import.rs`, added
  2026-07-11) is `#[uniffi::export(async_runtime = "tokio")]`'d
  (`mail_import.rs:176`), and its `connect` method (`mail_import.rs:212`)
  awaits `self.connector.connect(...)` **inline** down into
  `libs/fauna-mail/src/imap_client/native.rs`'s `NativeImapConnector::connect`
  (`native.rs:130`) and `NativeTlsTransport::handshake` (`native.rs:184`) — a
  raw `TcpStream::connect` + `rustls` `TlsConnector::connect` (`native.rs:202`)
  that is **not** `Box::pin`'d and has no future-size-assertion test anywhere
  in `libs/fauna-mail` or `libs/fauna-ffi`. This is a structurally different
  connect path from every group above (no `fauna_anon_client` involvement at
  all, so the shared boxed leaf gives it no protection) and this doc has never
  audited it. Not confirmed unsafe — the future's actual size versus the
  10.9 KB unboxed anon-WS baseline is unmeasured — but it matches this doc's
  own hazard shape (small foreign FFI poll stack + an inline, unboxed
  TLS-handshake future) closely enough that it needs the same audit treatment
  the connect-anonymous paths already got. **Flagged for a session with code
  access to measure and, if warranted, box it — not fixed here (docs-only
  sweep).** Resolved 2026-08-17 — see the dedicated bullet below; the actual
  hazard was NOT the TLS handshake this paragraph names.

  A structural fix (e.g. a workspace-wide test asserting every
  `AnonymousNestClient::connect` / raw `tokio_tungstenite` call site is covered
  by this enumeration, so a stale list fails CI instead of a doc sweep) is
  overdue rather than a tenth manual patch — **flagged for a session with code
  access, not fixed here (docs-only sweep).** The next sweep should re-grep
  both categories, not trust either list at face value, and should also sweep
  for other raw (non-`fauna_anon_client`) TLS/WS handshake futures reached
  inline from a `fauna-ffi` async export — the mail-import path above may not
  be the only one.

- **Mail-import IMAP connect path (`FfiMailImportClient`) — DONE 2026-08-17.**
  Resolves the flagged paragraph in the bullet above. Measured the actual
  future the FFI polls inline end to end — `FfiMailImportClient::connect`
  (`mail_import.rs:212`) — rather than trusting the first hop alone:
  **17088 bytes**, well past this doc's ~10.9 KB unsafe baseline. Isolating
  `ImapSession::connect`'s own future (17040 B) showed the overwhelming
  majority was **not** the TLS handshake this doc's pattern usually targets:
  it was `Connection::next_event`
  (`libs/fauna-mail/src/imap_client/connection.rs`) — `let mut chunk = [0u8;
  READ_CHUNK]` (`READ_CHUNK` = 16 KiB) was a stack array **re-declared inside
  the read loop and held across its own `.await`**, so the full 16 KiB rode
  inline in every future that awaits `ImapSession::connect`/`connect_starttls`.
  Not a nested-future-needing-`Box::pin` case — the same root hazard (a large
  state held inline on the small foreign FFI poll stack) from a different
  cause: a raw buffer, not a future, needed to move off the stack. Fixed by
  hoisting `chunk` outside the loop as a heap-allocated `Vec<u8>`
  (`ImapSession::connect`'s future: 680 B after).
  `FfiMailImportClient::connect`'s future: **1544 bytes** after, under the
  established 2048 B guard threshold. Belt-and-suspenders, found and fixed in
  parallel: `NativeTlsTransport::handshake` (`native.rs:184`) is now also
  `Box::pin`'d (1352 → 248 B on `NativeImapConnector::connect` in isolation) —
  a real hardening, though it was never the dominant contributor measured
  here (at most ~1.1 KB of the 17088). Size-assertion regression tests at
  three points, mirroring `connect_anonymous`'s: the two fix sites
  (`fauna_mail::imap_client::native::tests::
  imap_connect_future_stays_small_for_foreign_stacks` for the handshake,
  `…::imap_session_connect_future_stays_small_for_foreign_stacks` for
  `next_event`) and the FFI boundary itself
  (`fauna_ffi::mail_import::tests::connect_future_stays_small_for_foreign_stacks`).

- **`fauna-client-recovery`'s succession-ceremony/recovery-kit exports —
  found this sweep (2026-08-21), a
  tenth consecutive recurrence, same shape as the eighth/ninth:
  FFI-reachable, freshly landed, safe with no additional fix.** Before these
  two commits (both dated 2026-08-21), `fauna-ffi` had **no** dependency on
  `fauna-client-recovery` at all — the stolen-identity succession ceremony
  this doc's eighth-recurrence enumeration above already tracked as
  non-FFI-reachable (`apps/fauna-linux/src/client.rs`'s
  `verify_succession_successor`, `apps/fauna-tui/src/settings/mod.rs`'s
  `finish_unconfirmed_succession`, etc.) is now **also** reachable from
  apple/windows/android through
  `libs/fauna-ffi/src/recovery.rs` (feature `recovery-ceremony`, default-on
  via `store-safe`, `fauna-ffi/Cargo.toml:257`). Its
  `succession_succeed_with_held_kit`
  (`recovery.rs:495`, `#[uniffi::export(async_runtime = "tokio")]`) awaits,
  **inline with no intervening `tokio::spawn`**, down through
  `fauna_client_recovery::ceremony::finish_unconfirmed_succession`
  (`ceremony.rs:886`) → `fauna_anon_client::AnonymousNestClient::connect`
  (`ceremony.rs:923`) — which itself resolves to the same already-`Box::pin`'d
  `crate::ws::connect_anonymous` leaf (`AnonymousNestClient::connect` →
  `connect_with_registry_resolving` → `ws::connect_anonymous`,
  `fauna-anon-client/src/client.rs:102-138`; re-verified this sweep, lines
  drifted from file growth — cite the symbol) — so this new inline-await site is
  safe with **no additional fix**, the same shared-leaf payoff as every prior
  group. (`sweep_after_succession`/`sweep_as_successor`'s own
  `fauna_client::NestClient::connect` is the already-audited "Authenticated
  `NestClient` connect path" bullet above, not a new mechanism.) The module's
  other five exports (`recovery_kit_status`, `recovery_create_kit`,
  `recovery_request_seed_alone_replacement`,
  `recovery_veto_pending_replacement`,
  `recovery_reseal_escrow_with_held_kit`) all build their
  `RecoveryClient` from the caller's already-established
  `nest.nest_arc()` (`libs/fauna-ffi/src/nest_client.rs:348` — a plain
  accessor, no fresh dial) — no new TLS/WS handshake, same
  "already-established dispatcher" class as most other `fauna-ffi` async
  exports. **The "FFI-reachable surface" crate list is corrected to also
  include `fauna-client-recovery`** — it joins `fauna-conversations`,
  `fauna-launch-machine`, `fauna-client-caldav`, and
  `fauna-onboarding-machine`. Same lesson as recurrences eight and nine: this
  one was not a missed audit of a long-standing dependency but a genuinely
  new one landing the same day as this sweep — exactly what the standing
  structural-fix flag two paragraphs above (a workspace-wide test asserting
  every `AnonymousNestClient::connect` call site is covered by this
  enumeration) exists to catch automatically. Still not fixed here
  (docs-only sweep).
