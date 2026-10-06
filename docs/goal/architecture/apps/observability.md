# App & nest observability — logging

Owns: observability
Status: ratified — target 2026-06-04, 3-category scope expansion 2026-06-05 (user); implemented on all seven apps + the nest (tui's `admin-logs` page closed 2026-07-29, the last unbuilt surface); the once-pending iOS e2e re-run is recorded green in the feature ledger (2026-08-27 → 2026-09-21) — see § Implementation status today
Authority: the app+nest logging pipeline — the fauna-log crate (ring, rolling file, emit API, shared render format), the 3-category what-must-be-logged scope, the log-at-the-producer rule, the level mapping, the redaction rule, the sidecar log plane (co-resident service events into the admin ring — schema, allowlist contract, admission/rate limits, the remote ring), and the Settings→Logs / admin nest-logs surfaces; defers mail metrics to behavior/mail-observability.md (a *different* surface), settings-rail placement to ui/settings.md § Navigation model, element IDs + page scope to ui.yaml, fauna.admin.logs transport to architecture/transport.md, and the sidecar log plane's per-kind caller-class gating to architecture/apps/bridges.md § Bridge-kind catalogue.

**The 3-category scope (2026-06-05, user):** the ring is not just for *Rust
error prints*. **Every message in any of three categories — (1) displayed to
the user (an alert/error/toast), (2) printed to stdout/stderr/console, (3) a
meaningful error silently caught and ignored — on every app *and* the nest
must land in the same `fauna-log` ring.** This pulls the non-Rust shells
(web TS, windows C#, apple Swift, android Kotlin) into the funnel via the
runtime **emit API** (`log_message` over UniFFI, `logMessage` over WASM). See
§ What must be logged and § The emit API.

## Problem

Errors must be **visible to the person who can act on them** and **recoverable
after the fact** — not written to a stream nobody reads.

Historically Fauna Rust code printed errors with `eprintln!`/`println!`:

- **GUI apps** (linux, windows, apple, android) had **no `tracing`
  subscriber at all**, so `eprintln!` *was* the only "logging" — invisible to a
  user running a windowed app, and gone the moment the process exits.
- The **nest** had a subscriber but defaulted to ERROR-only, so operational
  status was `eprintln!`'d to stay visible (and one line was logged twice).
- **Shared libs** (e.g. `fauna-conversations`) `eprintln!`'d swallowed wire-op
  failures — a print from a library that has no console.

CLI tools (`fauna-storage` — a reserved
placeholder that prints a notice and exits, not a running daemon —
`fauna-sign`, `fauna-emulator-ctl`) are exempt: their stdout/stderr **is** the user
interface. (Disambiguation: the Windows per-user *sync agent*
`bins/fauna-sync-agent` is **not** exempt — it is a GUI-subsystem logon
agent with no console, so its stderr is discarded and it installs `fauna_log::init`
exactly like `fauna-nest-service`. It shipped as `fauna-sync.exe` until the A5 rename
(2026-07-22), which is when the name stopped colliding with the then-exempt CLI daemon
`bins/fauna-sync`, removed 2026-10-02; it is `fauna-sync-agent.exe` now. See § Implementation status today.)

## What must be logged

Three categories of message, on **every app and the nest**, must reach the
shared ring. A message that fits any of these and *isn't* in the ring is a gap:

1. **Displayed to the user.** Anything shown in the UI — an error banner, an
   alert/dialog, a toast/snackbar, a status/success line ("Saved", "Copied"),
   inline validation. The visible widget covers the *active* task; the ring is
   the durable record (a user who dismissed a toast, or wants the history, finds
   it on the Logs page). The banner/toast **stays** — it *also* logs.
2. **Printed.** Any `eprintln!`/`println!` (Rust), `console.*` (web),
   `print`/`NSLog`/`os_log` (apple), `Log.*`/`println` (android),
   `Console`/`Debug`/`Trace.WriteLine` (windows). A print to a console the user
   never sees is the original problem this doc exists to fix.
3. **Meaningfully swallowed.** A real error caught and discarded — `try?` /
   empty `catch {}` (Swift, Kotlin, C#, TS `.catch(()=>{})`), `let _ = <Result>`
   / `.ok()` / `if let Err(_) = …` (Rust) — where the error carried information
   (a wire op, RPC, parse, crypto, IO failure). The swallow may be a correct
   *control-flow* choice (best-effort cleanup, optional fetch); logging it at
   `warn`/`debug` keeps the decision while making the failure visible. Genuinely
   uninteresting discards (a `oneshot` reply whose receiver legitimately dropped,
   an infallible `write!` into a `String`) are **not** category 3 — don't log
   noise.

**The uniform shape:** category 1 is best covered by making each surface's few
**display funnels** (the shared error-banner / toast / alert helper) log as they
display — O(funnels), not O(call-sites). Categories 2 and 3 are site-by-site
(convert the print; add a log beside the swallow). In Rust this is a `tracing`
call; in a non-Rust shell it is one `log_message`/`logMessage` call (§ The emit
API).

**Log on the *event*, not the *paint* — log at the producer for reactive
banners.** A display funnel falls into one of two shapes, and the logging site
differs:

- **Imperative / one-shot** — a toast or banner shown *once* in response to an
  action (`show_toast`, a `set_error_message` call, a `catch` that assigns the
  error). Log right there; it fires once.
- **Reactive render** — an `error-message` banner that a view re-paints from
  state on *every* observer tick / snapshot (`m.error_message()`, `snap.error`,
  a bound `error` prop, an `errorMessage` `StateFlow`). Logging *in the render*
  re-logs the same line on every unrelated repaint — noise. Log instead where
  the error **state is set**: the shared state machine's error transition (Rust
  — covers all seven apps at once, priority #2), the ViewModel `catch` that
  assigns it, or a *change-detecting* hook (`$effect`, `.onChange(of:)`,
  `LaunchedEffect(error)`) that fires once per change, never per paint.

Reference: linux's onboarding banner logs in `fauna-onboarding-machine`
(`State::set_error`), not in the per-page render; its `show_toast` /
`set_error_message` / `error_toast` (imperative) log at the call.

## Target state

**Everything logs through `tracing`. Nothing in app or library code prints
to stderr/stdout.** Three pieces, uniform across all seven apps (priority #1)
and shared in Rust (priority #2):

### 1. Shared capture — the `fauna-log` crate

`libs/fauna-log` is the single home for log capture, used by **both apps and
the nest**:

- `RingLayer` — a `tracing_subscriber::Layer` that copies each event into a
  bounded in-memory ring (`RING_CAPACITY = 2000`, FIFO eviction). Pure Rust →
  compiles for native and wasm.
- `snapshot()` / `snapshot_at_least(level)` / `clear()` — read/trim the ring;
  these feed the Settings → Logs view and the nest admin RPC.
- `LogEntry { timestamp_ms, level, target, message }` / `LogLevel` — owned
  types that cross the UniFFI / WASM boundary unchanged.
- `init(data_dir)` (native only) — installs the process-global subscriber:
  `RingLayer` + a **daily-named, size-capped file** under `<data_dir>/logs/`
  (§ 2 Persistence) + stderr, all filtered by `RUST_LOG` (default `info`,
  matching the server fleet); stderr carries ANSI colour only when it is a
  terminal; a panic hook records every panic as an `error` event (ring + file)
  before the default hook prints it. Returns a guard the caller keeps alive. **The file layer is fallible, not the ring
  (2026-08-24):** `try_file_appender` degrades away (memory ring + stderr only,
  `eprintln!`-reporting the cause) instead of panicking when the log dir can't
  be created or written — an unmounted disk, or a macOS TCC deny on the
  app-group container — because `tracing_appender::rolling::daily`'s own panic
  there crash-looped a KeepAlive'd launchd agent; the ring and stderr layers
  always install regardless. See `apps/sync-agent.md` § Implementation status
  today for the incident this fixed.
- `file_layer(log_dir)` (native only) — the same size-capped file sink as a
  standalone layer, for a process that keeps its own subscriber (format,
  filter) and only needs the bounded file added (its one standalone consumer, the
  `fauna-sync` daemon's launchd job, was removed 2026-10-02).
- `log_message(level, target, message)` — the **emit API** (§ below): a runtime
  entry point for callers that can't use the `tracing` macros (the non-Rust
  shells). Exposed as `log_message` over UniFFI and `logMessage` over WASM.
- **Ring-ingest sanitization (2026-07-30):** both rings strip control
  characters (`char::is_control`) from `message` and `target` at the storage
  boundary — `fauna_core::control_chars::strip_control_chars`, the **project's**
  one strip implementation, which the sidecar plane's nest-side admission also
  calls (ahead of its byte cap, so the cap counts real content). It lived in
  `fauna_log` while logs were its only consumer and **moved to `fauna-core`
  2026-07-30** when a second boundary needed it — tui's render funnel strips
  remote-authored *content* the same way (`tui.md` § Rendering) — because a
  content boundary must not depend on the log crate, while a second copy would
  break the property that makes either fix trustworthy: there is exactly one
  strip and every boundary calls it. `fauna_log` re-exports it, so log-plane
  call sites are unchanged. Ring text is
  rendered into **terminals** (the tui `admin-logs` / `settings-logs` pages),
  and log writes are user-influenceable — the proven case is a non-admin's raw
  URL logged on the nest's media-proxy rejection path, and a probe confirmed
  ratatui's cell grid preserves ESC/BEL verbatim (security review) — so an unstripped escape is a terminal injection into an admin's
  screen, and a newline forges log-view line structure. Enforced at ingest,
  never at read, so every consumer (all seven apps' Logs pages, the admin
  view, the copy path) inherits it structurally. Deliberate residual: the
  on-disk rolling file and the stderr layer are `fmt`-layer output outside the
  rings and still carry raw bytes — they are developer/journald surfaces, not
  app surfaces.

### The emit API — `log_message` / `logMessage`

Rust code (nest, shared libs, the linux app, the native FFI glue) emits with
the `tracing` macros, captured by `RingLayer` — that needs no new surface. The
**non-Rust app shells** (web TypeScript, windows C#, apple Swift, android
Kotlin) have no `tracing`, so before this they could not reach the ring at all.
`fauna_log::log_message(level, target, message)` closes that gap and is exported
to every shell:

- **UniFFI** (`fauna-ffi`): `log_message(level: LogLevel, target: String, message: String)`.
- **WASM** (`fauna-wasm`): `logMessage(level: string, target: string, message: string)`
  (`level` is `"error"|"warn"|"info"|"debug"|"trace"`, unknown ⇒ `info`).

It re-enters the **same** subscriber the shell's `installLogging` set up (ring +
file/console + `EnvFilter`) — shell messages persist and filter exactly like
Rust ones, preserving the single-pipeline invariant ("everything logs through
`tracing`"). Because `tracing`'s static metadata `target` can't take a runtime
string, the caller's `target` rides a structured `log_target` field
(`fauna_log::TARGET_OVERRIDE_FIELD`) that `RingLayer` lifts into the entry's
target column; the fixed metadata target is `fauna_client` (the `RUST_LOG` knob
for shell-bridged lines). A shell calls it from all three categories: its
display funnel, each converted print, and each meaningful swallow.

### 2. Persistence & privacy

- **On-device rolling file** under the app's own data root (linux:
  `~/.config/fauna/logs/`, sharing the e2e per-run `XDG_CONFIG_HOME`
  isolation). The user's machine, the user's data (product invariant: *user
  controls their data*). The in-memory ring drives the live UI; the file is the
  long tail / crash forensics.
- **The file is bounded by construction — no human ever truncates a log
  (2026-09-30).** A process left running for weeks (the per-user sync agent
  retrying a rejected credential once filled a never-rotated launchd redirect to
  6.6 GB) must not grow its log without limit, and a log budget is nobody's
  choice, so the bound is a Rust constant (`fauna_log::rolling`): the day's file
  is `fauna.log.<YYYY-MM-DD>` (UTC), continuing in `fauna.log.<date>.<NNN>` once
  it reaches `MAX_FILE_BYTES` (10 MiB), so a lexicographic sort of the directory
  is oldest→newest; each newly opened file prunes the oldest `fauna.log.*` files
  until the directory fits `MAX_TOTAL_BYTES` (50 MiB). **A deployment artifact
  never redirects a Fauna process's stdout/stderr into a file of its own** — the
  process's own file already holds every line, and an OS redirect is a second,
  unrotated copy; stderr is left to what bounds it (a terminal, journald, the
  SCM's discard). A process that supervises a non-Rust child (the MDA
  supervisor's Go `fauna-mail-bridge`) pipes the child's stdout/stderr into
  its own `tracing`, line by line, so the child's output lands in the
  supervisor's bounded file instead of an inherited stream nothing keeps.
- **Redaction rule (authoring, not enforced by the layer):** NEVER log message
  plaintext, secret material (keys, tokens, claim codes), or other sensitive
  user content — these land in an on-disk file and a user-visible page. Log
  levels, targets, operation names, and **error metadata** only. Reviewers
  reject call sites that interpolate decrypted bodies or secrets. (The
  *enforced* filters are narrower and mechanical: the ring-ingest
  control-character strip, § 1 — redaction proper remains an authoring rule.)
- **Where the authoring rule has failed once, make the unsafe value
  unrepresentable instead (2026-09-01).** A log site is easy to add without
  knowing what a value renders, which is how a provisioning `warn!` that was
  itself a correct diagnosability fix came to write the user's DNS-provider API
  key to the rolling file and the Settings → Logs page: Namecheap's API is
  query-parameter driven, so the credential rode the request URL, and
  `reqwest::Error` renders that URL from `Display` *and* `Debug`. The fix is
  typed rather than editorial — `fauna_provisioning::ProvisionError::Http` can
  hold only a `RedactedHttpError`, whose sole constructor strips the URL's query
  string and userinfo, and the blanket `#[from] reqwest::Error` is gone, so `?`
  routes through the redaction and no call site can opt out. Scheme, host, port
  and path survive: those are the *error metadata* this rule permits. Reach for
  the same shape whenever a secret can enter an error type at a boundary — the
  rule above still governs everything else, and stays an authoring rule.
- Web (no filesystem): `RingLayer` only, plus the browser console; no on-disk
  file. Mobile native sinks (logcat / os_log) are additive, decided per lift.

### 3. Surfaces

- **Settings → Logs sub-page** (flat rail entry; `settings.md` § Navigation
  model). Renders `snapshot()` newest-first with a level filter, a
  copy-to-clipboard / export affordance, and a clear button. Element IDs +
  page scope: ui.yaml (`settings-logs` page; the shared `log-entry` /
  `log-level-filter` / `log-copy-button` / `log-clear-button` component IDs).
- **Per-page error banners stay — and now log as they display.** A user-facing
  failure still sets its page's `error-message` element (unchanged) — that is
  "displayed in the app" for the *active* task; the Logs page is the durable
  record. They are complementary, and per § What must be logged (category 1) the
  act of display itself feeds the ring at a level matching the banner kind
  (error→`error`, warning→`warn`, info/success→`info`) — but **at the producer,
  not the paint** (§ What must be logged, "Log on the *event*"). Imperative
  funnels (linux's `show_toast`/`set_error_message`/`error_toast`) log at the
  call. Reactive banners (web's `MessageBanner`, apple's
  `ErrorBanner`/`AppMessages`, android's `AppMessages` `StateFlow`, linux's
  per-page `error-message` labels) log where the state is **set** — the shared
  machine's error transition (e.g. `fauna-onboarding-machine::State::set_error`,
  one edit covering all seven apps), the ViewModel `catch`, or a
  change-detecting hook — never in the per-tick render.
- **Admin view of nest logs.** An admin-scoped WS-RPC (`fauna.admin.logs`,
  transport.md) returns the nest's `fauna-log` ring snapshot; the app's admin
  surface renders it with the same widget as the app Logs page. The nest
  installs `RingLayer` alongside its existing fmt subscriber.

## The sidecar log plane (nest-side sources beyond the nest process)

*Ratified 2026-07-22 (target state — see § Implementation status today for what
is built). Design input: the user's ask "could all docker logs appear in the
client-visible logs, filtered for security issues?" — the plane is the yes to
the intent and the deliberate no to the mechanism.*

The production container runs **seven** s6-supervised services (`docker/s6/*/run`:
nest, mail-bridge MTA, mail-bridge MDA, sni-router, supervisor,
atproto-bridge, iroh-relay), but only `fauna-nest` installs
`RingLayer` — the other six are invisible on the `admin-logs` page, and what
an admin needs in an incident (a router TLS failure, a bridge that won't
enroll, a crash-looping sidecar) is precisely what never surfaces there. The
sidecar log plane closes that gap: a co-resident service reports a **small set
of deliberate, admin-meaningful events** to nest over the authenticated channel
it already holds, and nest lands them in the admin Logs surface beside its own
entries.

**Never the docker logs, never a tee.** Reading another service's container
log stream would need the docker socket (a container that can read it can
start a privileged container — full host takeover, destroying the UID/Landlock
isolation `security.md` § Co-resident process trust boundary establishes) or
`/var/lib/docker/containers/*` (outside the container; Landlock-denied). And
piping any service's *existing* free-form log output through a filter is
unsafe regardless of source: bridge/sidecar log text was never written under
this doc's redaction rule (it interpolates recipient addresses and actor IDs,
correctly, for journald), and parts of it are remote-controlled (SMTP HELO
strings, peer header values) — you cannot reliably strip an address out of
free-form text, and forwarding it would hand remote peers a log-injection
surface into an admin page. Right content, wrong source: the plane is an
**emit API with an allowlist contract**, never a subscriber, tee, or filter
over a process's general log stream.

### The allowlist contract (authoring rule, per source)

- **Each source declares a compile-time catalogue of events** — one catalogue
  module per binary, so the full set is auditable in one read. Emitting is an
  explicit call at a deliberately chosen site (exactly like the app shells'
  `log_message`), not instrumentation of existing logging.
- **An event's message is a compile-time-constant template.** Interpolated
  values are restricted to the bounded classes: counts, sizes, durations,
  ports, protocol/error codes and kinds, DNS domain names, and Fauna
  service/component names. **Never:** free-form remote-controlled text (HELO
  strings, header values, upstream error strings passed through verbatim),
  message plaintext or subjects, mail addresses or local parts, actor IDs,
  secrets (§ Persistence & privacy redaction rule — it binds every plane
  source), or per-user attribution of any kind (corroborating posture:
  `behavior/mail-observability.md` § Cross-actor isolation).
- **Level ∈ {error, warn, info} only.** Debug/trace detail stays in the
  source's own stderr/journald stream; the plane is for events an admin acts
  on. The general SMTP/IMAP per-transaction chatter is *not* plane material —
  verdict/traffic telemetry is `mail-observability.md`'s (unbuilt, draft)
  surface, not this one.

### Wire — two legs, both additive

Matching the two authenticated channels co-resident services already hold
(`version-compatibility.md` additive discipline: an old nest answers
unknown-kind and the source silently disables reporting for that session; a
new nest with an old source simply sees no events):

- **Enrolled bridges** (mail MTA, mail MDA, atproto-PDS, content-processor):
  `fauna.bridges.report_log_events` — batched; caller-class-gated like every
  bridge kind (catalogue row + gating: `architecture/apps/bridges.md`
  § Bridge-kind catalogue).
- **Token-handshake sidecars** (iroh-relay): the sidecar originates
  `fauna.sidecar.log_events` up its existing internal duplex (`transport.md`
  § internal sidecar channel; kind constant beside `fauna.sidecar.hello` in
  `fauna-protocol`).

Batch payload: `{ events: [{timestamp_ms, level, event, message}], dropped }`
— `dropped` is the source-side loss count since the last accepted batch: both
queue overflow (drop-oldest eviction) **and** the events a *failed flush* threw
away. A transport failure discards the batch's events (re-queueing them would
let a flapping channel grow the queue without bound) but folds their count —
plus any count that batch was already carrying — back into `dropped`, so the
loss is reported on the next accepted batch rather than vanishing with it.
Saturating, so a very long outage reports a large number rather than wrapping to
a small one that reads as healthy.
The **source identity is never in the payload**: nest derives it from the
authenticated identity (bridge role / sidecar channel class), so a source
cannot speak as another source or as the nest. Source-side behavior: a small
bounded queue (drop-oldest, counted into `dropped`), timer/size-batched flush,
strictly best-effort — the plane must never back-pressure or fail the source's
real work.

### Admission, attribution, and the remote ring (nest-side)

- **Attribution:** the ring entry's `target` is `<source>:<event>` (e.g.
  `mta:tls.handshake_failed`, `relay:cert_fetch_failed`) with `<source>` from
  the fixed nest-side id set (`mta`, `mda`, `atproto`, `relay`,
  …). It rides the **existing** target column of the existing `log-entry`
  shape — so the plane needs **no new ui.yaml ID and no app change**; all
  7 apps render plane entries via the shared `fauna_log::format` as-is. (A
  dedicated source filter/column is a separate future ui.yaml conversation on
  its own merit.)
- **Admission sanitization** (defense-in-depth behind the authoring contract):
  control characters stripped; `message` capped (512 bytes); `event`
  constrained to `[a-z0-9_.-]` and capped (64 bytes); levels outside
  {error, warn, info} dropped-and-counted; batch length capped.
- **Rate limit:** per-source token bucket, hard-coded constants (bucket 1 of
  `principles.md` § One configuration surface — no human chooses these; tune
  by editing the constant): sustained ~30 events/min, burst ~60. Overflow
  drops the event and synthesizes at most one nest-side `warn` per source per
  minute naming the drop count — so flooding is visible, bounded, and
  attributed.
- **The remote ring:** plane entries land in a **separate** bounded ring
  beside the nest's own (`REMOTE_RING_CAPACITY = 1000` vs `RING_CAPACITY =
  2000`), via a direct push API (`fauna_log::push_remote_entry` — a remote
  entry carries its own timestamp/level/target, so it is pushed as data, never
  re-emitted as a synthetic local tracing event). `fauna.admin.logs` merges
  both rings timestamp-ordered into the one existing reply shape. The split
  ring is a security property: a chatty or hostile sidecar can never evict the
  nest's own history. Plane entries go to the ring only — the source's stderr
  already lands in the container log stream, so nest does not re-emit them to
  its own stderr/file (no doubled lines in `docker logs`).
- **App Settings → Logs is untouched** — the plane feeds the *nest* admin
  ring; client-local rings and their page keep their existing shape.

### Coverage (which of the nine, and why the rest are out)

| Service | Leg | Status |
|---|---|---|
| fauna-nest | its own `RingLayer` | shipped |
| mail-bridge MTA / MDA (Go) | `fauna.bridges.report_log_events` | shipped (`internal/logplane`) |
| iroh-relay (Rust) | `fauna.sidecar.log_events` | shipped 2026-07-22 (`log_catalogue.rs`); a later Rust sidecar follows nearly-free (same `fauna-sidecar-client` seam) |
| atproto-bridge | `fauna.bridges.report_log_events` | follow-on (same kind, its own catalogue) |
| sni-router | none yet | deferred; the design when wanted: a boot-minted token (`sidecar_tokens.rs`) + `/internal/router/ws`, same handshake — never a docker-log read. ⚠ The router now emits rate-limited `warn` lines when either connection cap sheds (`transport-connection.md` § Abuse posture) — the first signal it produces that an admin would actually want, and today reachable only by reading container logs off-box. That is the concrete motivation whenever this row is picked up. |
| fauna-supervisor | none **by design** | the root control sidekick stays deliberately minimal (narrow socket, no outbound client); nest logs the up/down commands it sends it, which is the admin-meaningful signal |

### Trust posture

A plane event is a **diagnostic report from a co-resident service**, exactly
as trustworthy as that service. A compromised sidecar can emit misleading —
but attributed, rate-limited, sanitized, size-bounded — lines into
`admin-logs`; it cannot impersonate the nest or another source, evict the
nest's own ring history, or inject unbounded/free-form content. Same framing
as the bridge self-probe (`security.md` § Co-resident process trust boundary):
provisioning/diagnostic signal, never a security attestation.

## Level mapping (authoring guidance)

The same level mapping applies whether the source is a Rust `tracing` call or a
shell `log_message`/`logMessage` call — the level argument carries the choice.

| Was | Becomes | When |
|---|---|---|
| `eprintln!("[x] … failed: {e}")` / `console.error` / `print` of a failure | `error` | an operation the user cares about failed |
| `eprintln!("Warning: …")` / `console.warn` | `warn` | recoverable / degraded, non-fatal |
| `eprintln!("[x] started / status …")` / `console.info` | `info` | lifecycle / status |
| `eprintln!("[x] debug detail …")` / `console.debug` | `debug` | developer diagnostics |
| **Error banner / alert displayed** | `error` | category 1 — the funnel logs the message it shows |
| **Warning banner displayed** | `warn` | category 1 |
| **Info / success toast displayed** ("Saved", "Copied") | `info` | category 1 |
| **Meaningful swallowed error** (`try?`, empty `catch`, `let _ = <Result>`) | `warn` or `debug` | category 3 — `warn` if it degrades the user's task, `debug` if best-effort cleanup |
| e2e `[TestAgent]`/`[agent]` traces | `debug` | test-harness diagnostics (captured via the stderr layer) |
| CLI subcommand output (`println!`) | **unchanged** | the CLI's stdout is its UI |

## Implementation status today

**Built end-to-end on all seven apps + the nest.** The once-pending **iOS e2e
re-run** of `test_settings_logs.py` + `test_admin_logs.py` is done: the feature ledger
(`docs/features/ledger/ios.json`, 2026-08-27 → 2026-09-21) records every one passed on ios, bar
the web-only `test_displayed_banner_reaches_ring` (skipped on every native app). tui's own `settings-logs` page, subscriber install, print→tracing
sweep, and shared-render-format consumption are built — see the coverage table
below. Everything else below is landed and verified.

**The bounded file (§ 2) — built 2026-09-30; no launchd redirect remains
(2026-10-01).** The size-capped sink replaces `tracing_appender`'s unbounded
daily file, and no plist a Fauna artifact writes carries
`StandardOutPath`/`StandardErrorPath`: the per-user sync agent's LaunchAgent
(the app's self-install heals an older plist that still does); the `.pkg`'s
`social.fauna.nest` LaunchDaemon, whose Rust `fauna-nest-daemon` now installs
`fauna_log::init` on its data dir; the `social.fauna.bridge` LaunchDaemon, whose
Rust `fauna-bridge-supervisor` does the same on its `bridge/` dir and pipes the
Go MDA child's output into it (`fauna_mda_supervisor::spawn_at`); and,
until the daemon's removal on 2026-10-02, `fauna-sync install`'s plist, which wired
`FAUNA_LOG_DIR` for `file_layer` instead. A `.pkg` upgrade rewrites a changed daemon plist. Pinned headlessly by
each binary's `init_logging` test, the MDA capture test and the installer's
dry-run plist assertions. **Open:** the Windows `fauna-bridge-service` still
installs a stdout-only subscriber, which the SCM discards — it gains the
captured MDA output only once it also calls `fauna_log::init`.

**tui's `admin-logs` page closed 2026-07-29** (`apps/fauna-tui/src/admin/logs.rs`)
— it was the second pending leg, having sat outside `navigation.admin_pages` and
so outside tui's M8 admin-shell build. It renders the fetched nest ring with the
same widget and the same `log-entry` / `log-level-filter` / `log-copy-button` ids
as tui's Settings → Logs page (§ 3 Surfaces), reusing that page's label set and
row rendering rather than re-rolling them, and carries no clear affordance (there
is no admin RPC to wipe the nest ring). The same slice lifted the wire→render
conversion into shared Rust as `fauna_client_admin::log_entry_from_wire`,
replacing linux's private copy — so a seventh app never adds a seventh
`AdminLogEntry → LogEntry` match (priority #2).

**§ The sidecar log plane — built end-to-end for four of the nine services** (the nest itself, the mail bridge's MTA and MDA roles, and iroh-relay; two new sources)
(contract ratified 2026-07-22; nest half + the iroh-relay and Go mail-bridge
sources landed 2026-07-22). Built and pinned
by tests: the separate remote ring (`REMOTE_RING_CAPACITY = 1000`,
`fauna_log::push_remote_entry` / `snapshot_remote` / `snapshot_merged`, with the
eviction-isolation property pinned); the wire types + both kind constants
(`fauna_protocol::log_plane`); nest admission (`bins/fauna-nest/src/log_plane.rs`
— sanitization, the per-source token bucket, the ≤1/min synthesized drop warn,
`<source>:<event>` attribution derived from the authenticated identity); both
wire legs (`fauna.bridges.report_log_events` gated to the four bridge classes
and denied to User/Admin; `fauna.sidecar.log_events` served on
the relay channel's lean loop); and `fauna.admin.logs`
serving the merged, timestamp-ordered view.

**The first source is built too: `fauna-iroh-relay`** (2026-07-22). The
source-side queue/flush/disable machinery is shared
(`fauna_sidecar_client::log_plane` — bounded drop-oldest queue with a counted
`dropped`, batch drain at the wire cap, disable-on-error-reply), so a further
Rust sidecar needs only its own catalogue module. The relay declares five
events (`serving`, `cert_refreshed`, `cert_refresh_failed`,
`cert_unparsable`, `nest_unreachable`) in
`bins/fauna-iroh-relay/src/log_catalogue.rs`. It holds its channel for its
whole life and flushes each time the channel does any work (a handshake, a
push from nest, a refresh), never on a timer of its own.

**The Go mail bridge leg is built too** (2026-07-22) — `internal/logplane`, a
Go mirror of the Rust helper's semantics (bounded 256 drop-oldest queue with a
counted `dropped`, batch drain at the wire cap, disable-on-error-reply) plus
the timer/size flush a long-lived channel allows (2 s / 32 events) and a final
flush after shutdown, shipped by `wsrpc.ReportLogEvents`. One catalogue serves
both roles, since one process runs one role: `ready`, `tls_cert_fetch_failed`,
`listener_bind_failed`, `nest_reconnected`, `shutdown_forced`,
`confinement_degraded`.

`confinement_degraded` (2026-07-24) is the plane's first use as a *secondary*
surface for a fact whose primary home is the wire: the bridge's startup
confinement self-probe (`../security.md` § Co-resident process trust boundary →
*Confinement self-probe*) records to its service-user row, which is queryable
state an admin view and a live e2e assert on, and additionally raises this warn
so a misprovisioned box is visible to an admin who never opens that row. It
satisfies § Deliverability by construction — the probe fires at startup, when
the bridge holds a working authenticated link.

**Deliverability is what bounds that catalogue, and it generalizes.** The plane
reports *to nest over the source's authenticated channel*, so a source can only
report what it can report while that channel works. Two candidate bridge events
were deliberately left out for that reason — *enrollment rejected* (pre-approval,
no authenticated WS exists yet) and *service user revoked* (nest's capability
gate is already refusing every kind, and the reconnector has torn the connection
down). Neither is a gap: in both cases **nest made the decision and already has
the fact**. The relay's `nest_unreachable` is the same constraint in another
shape: queued while the channel is down, delivered when it is back. Before cataloguing an event, ask whether the source still has a working
nest link at the moment it fires.

**The tier_4 assert is built too (2026-07-29)** —
`tests/e2e-unified/tests/platform/docker/test_sidecar_log_plane_docker.py::test_bridge_ready_event_reaches_admin_logs`
brings both mail-bridge roles to serving inside the real Docker image and reads
their `ready` events back off `fauna.admin.logs`, confirming the `<source>:<event>`
target and message reach the admin RPC end-to-end through a real deployed image
(not just the tier_3 conformance test that drives admission directly). Correction
to the prior claim here: this assert did NOT need a nest image carrying new code —
it reads via the admin RPC, not the SPA, so the already-pulled production image
sufficed; the client side needs no change at any point, since entries arrive in
the existing reply shape and render through `fauna_log::format` as-is.

**Not yet built:** the **atproto-bridge** catalogue.

| Surface | nest | linux | web | windows | macos | ios | android | tui |
|---|---|---|---|---|---|---|---|---|
| Subscriber install (`init` / `installLogging`) | ✅ (RingLayer beside its fmt subscriber) | ✅ | ✅ (ring-only + console) | ✅ | ✅ | ✅ | ✅ | ✅ (`session::install_logging` → `fauna_log::init_with_stderr`) |
| Print→tracing / `logMessage` sweep (category 2) | ✅ | ✅ (~179 sites) | ✅ (30 `console.*`) | ✅ | ✅ (47 sites) | ✅ | ✅ (31 `Log.*`) | ✅ (pure Rust `tracing` calls; the one remaining `eprintln!` is a deliberate belt-and-suspenders pre-exit print that *also* calls `tracing::error!` beside it) |
| Display-funnel producer logging (category 1) | n/a | ✅ (`show_toast`/`set_error_message` + shared machines; copy confirmations through `clipboard::copy_and_confirm`) | ✅ (`MessageBanner` `$effect`; copy confirmations through `lib/copy-confirm.ts`) | ✅ (`ViewModelBase.SetError`, 16 VMs) | ✅ (`AppMessages` `didSet`) | ✅ (shared FaunaKit) | ✅ (`AppMessages` StateFlow funnel) | ✅ (direct-Rust consumer of the same shared machines linux uses — `set_error` transitions log once; success lines such as the Status copy's "Copied!" through `wizard::log_displayed_notice`) |
| Meaningful-swallow logging (category 3) | ✅ | ✅ | ✅ | ✅ | ✅ (`logTry` helpers) | ✅ | ✅ (`ShellLog.{w,d}`) | not yet site-audited — a spot check found only the doc's own "genuinely uninteresting" exemption class (channel sends with a legitimately-dropped receiver, best-effort temp-file cleanup); a full sweep like linux's/macos's site counts is still owed |
| Settings → Logs page (`settings-logs`) | n/a | ✅ (reference) | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ (`settings/logs.rs`, M5 2026-07-15) |
| Admin nest-logs page (`admin-logs`, `fauna.admin.logs`) | ✅ (RPC) | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ 2026-07-29 (`admin/logs.rs`) — the last app; it sits outside `navigation.admin_pages`, so tui's 2026-07-19 admin-shell completion hadn't covered it |
| Shared render format (`fauna_log::format`) consumed | n/a | ✅ (direct) | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ (direct — `settings/logs.rs` calls `fauna_log::format::{level_for_index,format_line,rendered_text}`) |

Load-bearing mechanics (each verified in code):

- **`libs/fauna-log`** — `RingLayer` (`RING_CAPACITY = 2000`, FIFO), `snapshot`/
  `snapshot_at_least`/`clear`, native `init(data_dir)` (ring + the size-capped
  `rolling::CappedRollingFile` + tty-gated-colour stderr + the panic hook,
  `RUST_LOG` default `info`; the file leg degrades away fallibly via
  `try_file_appender`, § 1 above, rather than panicking on an unwritable log
  dir; the cap is pinned headlessly by
  `a_runaway_writer_stays_under_the_total_cap_forever`, red-verified against a
  disabled prune), the emit API `log_message` with the
  `TARGET_OVERRIDE_FIELD` (`log_target`) lift, the ring-ingest
  control-character strip (2026-07-30 — `strip_control_chars`, since lifted to
  `fauna_core::control_chars` and re-exported here, applied in both
  rings' push path; pinned by `ring_ingest_strips_control_characters` /
  `remote_ring_ingest_strips_control_characters`, and end-to-end ring →
  `format_line` → element text by tui's
  `a_ring_written_escape_never_reaches_the_rendered_element_text`, written
  red-first against the unstripped recorder), and the shared render module
  `format.rs` (`level_for_index` / `filter_entries` / `format_time(ms, tz_offset_secs)` /
  `format_line` / `rendered_text` / `LogRow` / `rows` — the caller supplies the
  local UTC offset, same contract as `fauna_core::format::relative_time`).
- **FFI/WASM exposure** — `fauna-ffi/src/logs.rs` (`install_logging`,
  `log_snapshot`/`log_snapshot_at_least`/`log_clear`, `log_message`, the `log*`
  render exports; default-on `logs` feature, dropped from the Go bridge) and
  `fauna-wasm/src/logs.rs` (`logSnapshot`/`logSnapshotAtLeast`/`logClear`,
  `logMessage`, render twins). `FfiAdminClient::logs()` / wasm `adminLogs()`
  fetch the nest ring.
- **Wire** — `fauna.admin.logs` (admin-scoped WS-RPC); `AdminLogEntry`/
  `AdminLogLevel`/`AdminLogsRequest`/`AdminLogsReply` are **standalone serde
  mirrors** in `fauna_protocol::admin` so the L3 protocol crate stays free of the
  observability crate; nest + linux convert at the edge. Client adapter:
  `fauna-client-admin::AdminClient::logs()`.
- **Producer-side logging in shared machines** — every shared client machine
  that feeds an `error-message` banner logs once at its error transition
  (`fauna-onboarding-machine::State::set_error` is the exemplar; also
  mail-settings, pair, dns, devices, folders, launch machines).
  `LocalizedText`-typed machines log via the redaction-safe
  `fauna_core::LocalizedText::log_line()`.
- **Windows per-user sync agent** (`bins/fauna-sync-agent`,
  ships `fauna-sync-agent.exe`) installs `fauna_log::init` — it is a GUI-subsystem
  logon binary (stderr discarded), NOT covered by the CLI exemption; tier_3
  `test_per_user_sync_agent.py` asserts the on-disk log.
- **Web boot capture** — `ensureWasm()` is promise-memoized so a concurrent
  double-init can't reset the wasm `RING` static; `refreshLogs` awaits the
  memoized `ensureLogging()`.
- **Noise exemptions held** — CLI stdout (its UI), `#[cfg(test)]` prints and
  their TestAgent analogs (android `testing/TestAgent.kt`, windows
  `Testing/TestAgent.cs`), benign discards, and ring-read failures on the Logs
  screens themselves.

Dated history (one line each): foundation + linux lead 2026-06-04 · 3-category
scope + emit API 2026-06-05 · apple capture + surfaces 2026-06-13 · windows/web/
android capture + surfaces landed through 2026-06-14 · render logic lifted to
`fauna_log::format` with all six apps consuming it 2026-06-14/15 (the five
per-app render twins deleted). Slice-level narratives: git history.
