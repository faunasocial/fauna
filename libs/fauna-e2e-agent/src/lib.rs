//! In-process e2e automation agent — the shared HTTP front-end.
//!
//! Every direct-Rust client (linux `fauna-desktop`, cli `fauna-tui`) hosts the
//! unified e2e `/element/*` + `/app/{state,commands}` contract *inside* the app
//! process, on a per-instance port from `FAUNA_E2E_AGENT_PORT` (see
//! `docs/goal/architecture/apps/linux.md` § E2E automation and
//! `apps/tui.md` § E2E automation; the Swift analogue is
//! `clients/apple-e2e-automation.md`). This crate owns the client-agnostic
//! half: the blocking HTTP server, request parsing (query/body/scope), the
//! wire JSON shapes, and the op types marshalled to the client's UI thread.
//! Each app supplies [`AgentHooks`]: how to dispatch an [`ElementOp`] onto
//! its UI thread, and how to serve the state protocol.
//!
//! Wire conventions the drivers (`tests/e2e-unified/drivers/http_bridge.py`)
//! depend on: reads reply `{text}`/`{visible}`/`{count}`/`{enabled}`/`{value}`
//! with safe defaults and HTTP 200 even when the element is absent; actuation
//! replies `{ok:true}` or `{error:…}`, and an `error` key maps to HTTP 404 (the
//! driver's retry path) unless the reply names its own `status` — which a
//! refusal *about an element that was found* must do, so the driver does not
//! scroll-retry a widget already on screen (see [`element`]).
//! `POST /app/commands` returns immediately; the driver polls
//! `GET /app/state` for `{last_command_id, ready, state}`.
//!
//! # What ships, and what is compiled out
//!
//! Both hosts depend on this crate **unconditionally** — they have to, since a
//! plain debug build hosts the agent through its `debug_assertions` arm and
//! Cargo cannot condition a dependency on a profile. So the crate carries its
//! own gate, `#[cfg(any(debug_assertions, feature = "e2e-agent"))]`, and splits
//! into two halves (`e2e-automation-surface-gating.md` § The convention,
//! convention 15's shared-Rust bullet):
//!
//! * **The agent surface — gated.** The HTTP server ([`start`], its routing and
//!   parsing), [`AgentHooks`], the stall capture, the timeout verdict, and the
//!   actuation gate with its three `FAUNA_E2E_*` reads. Absent from a plain
//!   `--release` build; a release-profile e2e build opts back in through the
//!   host's own `e2e-agent` feature.
//! * **The observable vocabulary — ungated.** State keys, command action names,
//!   the pure derivations behind them ([`connection_json`],
//!   [`command_needs_barrier`], [`barrier_probe_value`], …), and the small types
//!   a host's unconditionally-compiled plumbing names in a signature
//!   ([`ElementReq`], [`ScopeStep`], [`ConnectionReports`], [`LivenessStamp`]).
//!   tui compiles its agent plumbing in every flavor and supplies release twins
//!   for the gated half, so these names must resolve in a release build — they
//!   are contract vocabulary, not scripted control.
//!
//! The boundary is enforced, not just documented:
//! `tests/e2e-unified/tests/test_shared_crate_seam_gating.py::test_no_e2e_env_seed_read_ships_ungated`
//! scans every `FAUNA_E2E_*` env read in `libs/*` and `bins/*` for its compile
//! gate, this crate included.

use serde_json::{Value, json};
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
use std::collections::HashMap;
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
use std::sync::atomic::Ordering;
use std::sync::atomic::{AtomicI32, AtomicU64};
// `mpsc` rides `ElementOp`'s reply channel, which is ungated vocabulary.
use std::sync::mpsc;
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

#[cfg(any(debug_assertions, feature = "e2e-agent"))]
mod stall;

/// One step of a scoped query: `(container-id, index)` — the wire form of
/// `drivers/scope.py`'s `post-card[2]/quoted-post` syntax.
pub type ScopeStep = (String, usize);

/// The `barrier` command's action name — the causal anchor negative asserts use
/// instead of a settle-sleep (`e2e-conventions.md` § convention 14).
///
/// **Contract, identical on every app:** the agent acks `barrier` only after all
/// UI-thread work *enqueued before the command* has run. A test that must prove
/// "X did not happen" reads a generation counter, triggers, positively awaits the
/// trigger handler's own completion observable, issues `barrier`, and only then
/// asserts the counter unchanged — the absence is anchored to causal order, so a
/// green run pays nothing and a late X can no longer false-pass.
///
/// The name lives here because it is a convention-11 **cross-app contract entry**
/// (`e2e-conventions.md` § convention 11): a command one app implements is one
/// every app must implement or explicitly refuse. The *mechanism* is necessarily
/// per-app and deliberately not abstracted here — each app's UI thread has a
/// different queue discipline, and the shapes are not interchangeable:
///
/// - **tui** drains its `UiMessage` channel, because `tokio::select!` polls its
///   branches in random order and would otherwise let the command's arm win over
///   work already queued on another.
/// - **linux** round-trips a glib **idle** callback, because its command drain is
///   a glib *timeout* source and timeouts outrank idles — acking from the drain
///   alone would jump ahead of idle work queued first.
/// - **web** yields a double macrotask turn, because awaiting a promise orders
///   only against microtasks.
/// - **apple** (macOS + iOS, one shared FaunaKit handler) round-trips
///   `DispatchQueue.main` — the serial queue its probe batch and its own command
///   dispatch both ride, so a block enqueued by the barrier runs strictly after
///   every block enqueued before the command (Dispatch runs a serial queue FIFO).
///   It additionally `Task.yield()`s to order against main-actor jobs, which are
///   a different queue discipline rather than more of the same one. Unlike linux
///   it can ack from inside its own handler, because the apple agent sets
///   `last_command_id`/`ready` only *after* the handler returns.
/// - **windows** round-trips its `DispatcherQueue` on the agent's *existing*
///   post-action rail rather than adding a mechanism: `ProcessCommand` returns a
///   non-null postAction, so `PollLoopAsync` clears `ready`, `TryEnqueue`s the
///   continuation behind whatever the probe queued (that queue is FIFO), freezes
///   the ack probe inside it, and flips `ready` back only in that continuation's
///   `finally`. Built fused from the start — its leg landed after the fused form
///   existed, so it never had the two-command grading gap.
/// - **android** posts to the main `Looper` through ONE explicit `Handler` that
///   the probe's batch rides too, and freezes the ack probe inside that message.
///   Its agent already runs each command inside `withContext(Dispatchers.Main)`
///   and acks only after the block returns (apple's in-process shape), so the
///   command suspends on the hop and needs no second rail. ⚠ Never a nested
///   `withContext(Dispatchers.Main)` instead: from the main dispatcher it runs
///   inline, which would apply the probe before its ack (the windows leg's
///   inline-fallback trap). Plain `Handler.post` messages are synchronous, so the
///   hop also orders after earlier `Dispatchers.Main` (asynchronous) work.
///
/// ⚠ apple and windows reached a gradeable barrier from opposite directions, and
/// that is the transferable part: each leg's own command rail already ordered the
/// ack (an in-process handler there, a post-action there), so neither inherited
/// linux's and web's grading gap. **Read a new leg's rail before assuming it
/// does.**
///
/// An app whose agent cannot honour it must refuse loudly rather than ack early:
/// a barrier that acks too soon is worse than no barrier at all, because every
/// negative assert built on it silently reverts to a race.
pub const BARRIER: &str = "barrier";

/// The `barrier` **self-test** probe (`e2e-conventions.md` § convention 14's
/// per-platform self-tests). Enqueues UI work onto the same queue real work rides
/// and acks *without* waiting for it, so the only thing that can make the work
/// observable is a correct [`BARRIER`].
///
/// Payload `{"token": "<string>", "count": <n>, "barrier": <bool>}` (count
/// defaults to [`BARRIER_PROBE_DEFAULT_COUNT`]; see [`BARRIER_PROBE_FUSE_FIELD`]
/// for the fused form, which is the one that actually *grades* the barrier). It
/// enqueues `count` work items, the `i`-th publishing `"<token>#<i>"` at
/// `state.barrier_probe`. A correct barrier therefore leaves `"<token>#<count-1>"`
/// there, and any *prefix* of the batch is a visible, diagnosable failure rather
/// than a coin flip.
///
/// **Why a batch and not one item — this is the whole reason the probe works.**
/// The naive one-item probe is nearly vacuous on an app whose UI queue drains on
/// its own: the item lands microseconds later regardless, so a `barrier` that did
/// nothing would still usually pass. Concretely on tui, whose command arm and UI
/// arm are two branches of one `tokio::select!` (which polls branches in
/// **random** order), a do-nothing barrier acking before a single queued message
/// is a ~50/50 coin flip — a flaky test, not a proof. With `count` items it must
/// win that race `count` times in a row, so the do-nothing barrier fails with
/// probability `1 - 2^-count`: at the default that is a certainty for any
/// practical purpose. On linux and web, whose queues are FIFO against the
/// barrier's own mechanism, one item would already be deterministic; the batch
/// costs them nothing and keeps ONE probe shape across all apps.
///
/// It is a probe and not a real product surface on purpose — the alternative is
/// asserting the barrier with whatever async product path happens to exist on
/// each app, which measures that path's timing rather than the barrier.
pub const BARRIER_PROBE: &str = "barrier_probe";

/// How many work items [`BARRIER_PROBE`] enqueues when the payload omits
/// `count`. Sized for the probability argument in [`BARRIER_PROBE`]'s docs
/// (`2^-64`), not for duration — the items are trivial, so the batch is cheap on
/// every app and the barrier's cost stays proportional to real queued work.
pub const BARRIER_PROBE_DEFAULT_COUNT: usize = 64;

/// The `i`-th probe item's published value — the ONE place the
/// `"<token>#<i>"` shape is spelled, so the apps and the test cannot drift.
pub fn barrier_probe_value(token: &str, i: usize) -> String {
    format!("{token}#{i}")
}

/// [`BARRIER_PROBE`]'s optional `{"barrier": true}` payload field: enqueue the
/// batch and then run **this app's [`BARRIER`] mechanism**, all before acking
/// this one command.
///
/// ⚠ **This field exists because the two-command shape cannot grade the barrier
/// on most apps, and that was measured rather than reasoned.** With a plain
/// `barrier_probe` followed by a separate `barrier`, the driver's round trip
/// *between* the two commands is itself long enough for the app's queue to drain
/// unaided — linux's ~50 ms idle main loop, web's CDP eval round trip — so a
/// `barrier` that does nothing at all is indistinguishable from a correct one.
/// Graded 2026-08-13: deleting linux's idle round-trip (M4) and web's two
/// macrotask turns (M5) both left `test_agent_barrier.py` **green**, while the
/// equivalent tui mutant (M3) reddened, purely because tui's `select!` gives its
/// queue no comparable free drain. Fusing removes the gap instead of measuring
/// it, which is why it is the fix `e2e-conventions.md` § convention 14 names —
/// the alternative, a probe whose items are individually slow, buys the same
/// discrimination back at the price of exactly the wall-clock dependence this
/// convention exists to remove.
///
/// **The ack-time freeze ([`BARRIER_ACK_PROBE_KEY`]) is what makes the fused
/// form assertable**, and it is the same freeze the standalone [`BARRIER`]
/// performs: the app records what it saw at its own ack, so the driver's later
/// read reports the ack-time truth however late it arrives. Fused, that value is
/// `"<token>#<count-1>"` only if the barrier really ordered the batch, and
/// `None` if the app acked without draining — there is no intervening round trip
/// to rescue it.
///
/// **An app that does not implement this field fails the self-test loudly rather
/// than passing it** (convention 11's "never silently drop", satisfied by
/// construction): ignoring an unknown payload key means acking early, which
/// leaves the frozen key holding `None` and reds the assertion. So the field
/// needs no separate refusal surface, and adding it does not grow the cross-app
/// command table — [`BARRIER`] and [`BARRIER_PROBE`] remain the only two names
/// an app must implement.
pub const BARRIER_PROBE_FUSE_FIELD: &str = "barrier";

/// Whether a [`BARRIER_PROBE`] payload asks for the fused form
/// ([`BARRIER_PROBE_FUSE_FIELD`]) — the ONE place that predicate is spelled, so
/// the Rust apps cannot drift from each other or from the test.
pub fn barrier_probe_is_fused(payload: &Value) -> bool {
    payload
        .get(BARRIER_PROBE_FUSE_FIELD)
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Whether `action` + `payload` require this app to run its [`BARRIER`]
/// mechanism before acking — true for a bare [`BARRIER`] and for a fused
/// [`BARRIER_PROBE`]. Both Rust apps gate their barrier seam on exactly this, so
/// the fused form cannot be implemented on one and silently missed on the other.
pub fn command_needs_barrier(action: &str, payload: &Value) -> bool {
    action == BARRIER || (action == BARRIER_PROBE && barrier_probe_is_fused(payload))
}

/// The state key carrying **what the barrier saw at its own ack**, frozen — and
/// the only key the self-test may assert on.
///
/// ⚠ **This exists because asserting the LIVE `state.barrier_probe` after
/// `barrier()` returns is vacuous, and measurably so.** The driver reads state
/// over a separate round trip *after* the ack, and every app keeps republishing
/// state in the meantime (tui on every redraw, linux on its 50 ms tick) — so the
/// queue has drained on its own by the time the read lands, and a `barrier` that
/// did nothing at all passes. That is not a hypothesis: the first version of this
/// slice asserted the live key, went green on tui, and then **survived** the
/// mutant that deletes the drain entirely (2026-08-13). The frozen key is what
/// kills that mutant.
///
/// Each app writes it exactly once per barrier, on its UI thread, at the moment
/// it acks — after its own ordering work has run — and never recomputes it on a
/// later publish. So the value the driver eventually reads is the value that was
/// true *at the ack*, however late the read arrives.
pub const BARRIER_ACK_PROBE_KEY: &str = "barrier_ack_probe";

/// The `focus_move` command's action name — *"advance the keyboard focus by N
/// positions"*, and one half of convention 17's **layer (c)** walk vocabulary
/// (`e2e-conventions.md` § convention 17).
///
/// **Contract, identical on every app:** move the focus ring `times` positions
/// in `direction`, through the app's **real key door** — the same handler a
/// human's Tab/Shift-Tab (or Down/Up) keystroke reaches, never a private seam
/// that sets a focus index directly. That qualifier is the whole value of the
/// command: convention 17 ratified *"walks drive the real input doors"* precisely
/// because one of the three bugs that motivated it — the stale sub-page on
/// sidebar re-entry — lived in the difference between the human path and the
/// agent path, so a walk driving a private setter structurally could not see it.
///
/// Like [`BARRIER`] this is a convention-11 **cross-app contract entry**: a
/// command one app implements is one every app must implement or explicitly
/// refuse. The name lives here so the six non-tui apps stop spelling it by hand,
/// and so [`focus_move_request`] is the single place its payload vocabulary is
/// parsed — an app that re-derives the parse is free to disagree about what
/// `{"times": "3"}` means, and the walk it feeds then measures that disagreement
/// instead of the app.
///
/// **Payload** `{"direction": "next"|"prev", "times": <n>}`; `times` is optional
/// and defaults to 1. Parse it with [`focus_move_request`], and put a refusal on
/// the app's own `error-message` — an unparseable payload is not a reason to do
/// nothing quietly.
///
/// ⚠ **This is an input door, not a state field, and the distinction is
/// load-bearing.** The neighbouring `focused_line_count` ruling (2026-08-15) —
/// that the six non-tui apps owe no twin, because GTK/DOM/WinUI/SwiftUI/Compose
/// each guarantee at most one focused element per root and the identity-collision
/// class cannot arise there — is about an app *painting its own focus
/// affordance*, and does **not** carry over to this command. Every one of those
/// toolkits has Tab traversal, so every app can honour `focus_move`; refusing it
/// on the strength of that ruling would be reading a state-shape conclusion as an
/// input-shape one.
///
/// **web** is the one app where this command cannot run as in-page JS at all: an
/// untrusted `KeyboardEvent('keydown', {key: 'Tab'})` dispatched from script moves
/// no focus (measured 2026-08-21, bundled Chromium), so the browser's own
/// sequential-focus navigation is reachable only from the Playwright side.
/// `drivers/web.py` therefore overrides `focus_move` directly: `times` real
/// `Tab`/`Shift-Tab` presses through the web bridge's bare `/keyboard/press`
/// route, which Chromium receives as a *trusted* key event via CDP. The in-page
/// registry (`window.__fauna_callCommand`) keeps `focus_move` as a named refusal
/// pointing at this driver door (`$lib/focus-walk-e2e.ts`) rather than the generic
/// "unknown command" — see `e2e-systematic-ui-walks.md` § Implementation status
/// today → the web-leg entry.
pub const FOCUS_MOVE: &str = "focus_move";

/// The `switch_pane` command's action name — *"hand the keyboard to the nav
/// region or to the content region"*, the other half of the layer-(c) walk
/// vocabulary and a convention-11 contract entry on the same terms as
/// [`FOCUS_MOVE`].
///
/// **Contract, identical on every app:** give keyboard focus to the named
/// region through the app's real key door, exactly as the keystroke a human uses
/// to cross that boundary would (tui: `KeyCode::Left`/`Right`, reaching the same
/// `App::enter_sidebar_zone`/`enter_page_zone`).
///
/// **Why a walk needs it at all, rather than just tabbing.** A focus ring alone
/// cannot reach every state: on tui a page pane's ring does not hold the keyboard
/// until the zone changes, and a `nav` patch never sets the zone (`App::apply` is
/// zone-agnostic by design), so a page reached only through the state protocol
/// stays in whatever zone the session was already in. A walk that cannot cross
/// that boundary silently explores one region and reports having explored the
/// app — the under-coverage convention 17 exists to close.
///
/// **Payload** `{"pane": "page"|"sidebar"}`, parsed by [`switch_pane_target`].
///
/// An app whose layout genuinely has no two-region split refuses **by name and
/// with a reason**, and that refusal belongs in a goal doc as a declared absence
/// — not in a `.debug` log.
///
/// **web** implements it (all three shells — the main app, `settings`, `admin` —
/// permanently mount `<nav class="sidebar">` beside `<main class="content">`), but
/// the DOM has no keystroke or API that jumps between landmarks directly, so
/// `drivers/web.py`'s `switch_pane` crosses the way a keyboard user actually
/// would: press `Tab` (toward `page`) or `Shift-Tab` (toward `sidebar`) through
/// the real door until `document.activeElement` lies inside the target region.
/// Regions resolve by **landmark type**, never a hand-written path (the profile
/// page mounts a `<nav class="tabs">` *inside* `<main>`). Four terminations:
/// already inside → no-op; a full ring cycle back to the start having passed
/// through `BODY` (Chromium's own wrap discriminator) without ever entering the
/// region → no-op (no tab stops there); a cycle back to the start *without*
/// visiting `BODY` → a focus trap, loud refusal; [`FOCUS_MOVE_MAX_TIMES`] presses
/// with no termination → loud refusal. Full ruling:
/// `e2e-systematic-ui-walks.md` § Implementation status today → the web-leg entry.
pub const SWITCH_PANE: &str = "switch_pane";

/// The inclusive upper bound on [`FOCUS_MOVE`]'s `times`.
///
/// Every app runs the step loop on the thread that serves its agent, so an
/// unbounded count is not a slow command — it is an app on which *every*
/// subsequent command times out, which is convention 11's second corollary
/// (nothing blocks the agent-serving thread) and wears the same disguise as a
/// dropped command. The bound is generous against the real frontier (the widest
/// page in the fleet is a few dozen focusables) and exists to keep a walk bug
/// attributable to the walk rather than presenting as a dead app.
pub const FOCUS_MOVE_MAX_TIMES: u64 = 256;

/// Which way [`FOCUS_MOVE`] advances the ring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusDirection {
    /// Forward — the Tab / Down key's direction.
    Next,
    /// Backward — the Shift-Tab / Up key's direction.
    Prev,
}

/// Which region [`SWITCH_PANE`] hands the keyboard to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    /// The content region — tui's `Zone::Page`, a GUI app's main content area.
    Page,
    /// The navigation region — tui's `Zone::Sidebar`, a GUI app's nav rail.
    Sidebar,
}

/// Parse a [`FOCUS_MOVE`] payload, or say exactly what was wrong with it.
///
/// The error string is written to be pasted straight onto the app's own
/// `error-message` element: it names the command and the offending field, so a
/// failing walk diagnoses itself (convention 6) instead of leaving the next
/// session to guess which of the two fields the app disliked.
///
/// ⚠ **A present-but-malformed field is a refusal; only an ABSENT `times`
/// defaults.** The asymmetry is deliberate and is the defect this function was
/// extracted to remove: a `.as_u64().unwrap_or(1)` reading turns `{"times":
/// "3"}` into a perfectly legal one-step move that acks green, so the caller is
/// told its command ran when a *different* command ran. Once the action name is
/// recognized, that is the only disguise convention 11's silent drop has left.
pub fn focus_move_request(payload: &Value) -> Result<(FocusDirection, u64), String> {
    let direction = match payload.get("direction").and_then(|v| v.as_str()) {
        Some("next") => FocusDirection::Next,
        Some("prev") => FocusDirection::Prev,
        _ => {
            return Err(format!(
                "{FOCUS_MOVE}: `direction` must be \"next\" or \"prev\", got {}",
                describe(payload.get("direction"))
            ));
        }
    };
    let times = match payload.get("times") {
        None => 1,
        Some(v) => v.as_u64().ok_or_else(|| {
            format!(
                "{FOCUS_MOVE}: `times` must be a non-negative integer, got {}",
                describe(Some(v))
            )
        })?,
    };
    if times > FOCUS_MOVE_MAX_TIMES {
        return Err(format!(
            "{FOCUS_MOVE}: `times` is {times}, above the {FOCUS_MOVE_MAX_TIMES} cap — \
             the step loop runs on the thread that serves this agent, so a count \
             that large stalls every later command rather than just this one"
        ));
    }
    Ok((direction, times))
}

/// Parse a [`SWITCH_PANE`] payload, or say exactly what was wrong with it.
pub fn switch_pane_target(payload: &Value) -> Result<Pane, String> {
    match payload.get("pane").and_then(|v| v.as_str()) {
        Some("page") => Ok(Pane::Page),
        Some("sidebar") => Ok(Pane::Sidebar),
        _ => Err(format!(
            "{SWITCH_PANE}: `pane` must be \"page\" or \"sidebar\", got {}",
            describe(payload.get("pane"))
        )),
    }
}

/// Render a payload field for a refusal message — `absent` rather than `null`
/// when the key is missing, since those are different mistakes and the app's
/// `error-message` is the only place the caller learns which one it made.
fn describe(v: Option<&Value>) -> String {
    match v {
        None => "absent".to_string(),
        Some(v) => v.to_string(),
    }
}

/// The state key carrying the **session generation** — a monotonic counter the
/// app bumps once per authenticated-session teardown/rebuild, and the observable
/// convention 14's negative asserts read (`e2e-conventions.md` § convention 14,
/// mechanism (b)).
///
/// **Contract, identical on every app.** The counter starts at 0 for a fresh
/// app process, only ever increases, and is incremented **synchronously at the
/// initiation point** of a teardown — the moment the app commits to dropping the
/// authenticated session, *before* any deferral, await, or navigation. A test
/// proving "this gesture did NOT relaunch me" reads it, performs the gesture,
/// positively awaits the gesture handler's own completion observable, issues
/// [`BARRIER`], and asserts the value unchanged.
///
/// **Why the initiation point and not the teardown itself.** Two of the three
/// built apps *defer* the actual teardown (linux by a 100 ms
/// `glib::timeout_add_local_once`, so the dialog that triggered it can release
/// its modal grab; web by a full document navigation). A counter bumped where
/// the teardown *lands* is therefore invisible to a barrier that ran before the
/// deferral elapsed, and the negative assert silently reverts to the race it
/// replaced. Bumping at initiation is exactly convention 14's corollary — "a
/// would-be effect must be initiated synchronously inside a handler whose
/// completion is observable" — applied to the app rather than the test.
///
/// **Why a monotonic counter and not a boolean or a live state read.** A
/// generation is the one observable shape that is *conservative under a late
/// read*: the value only ever grows, so a read arriving after the app has
/// settled can only reveal MORE teardowns, never fewer. That is what keeps this
/// key clear of the vacuity trap that [`BARRIER_ACK_PROBE_KEY`] exists to
/// document — there the asserted value became true on its own over time, so a
/// late read passed against a do-nothing barrier; here a late read is strictly
/// stronger than an early one. The property is *not* free on every app: a web
/// teardown is a document navigation that destroys the JS heap, so web persists
/// the counter across it (`$lib/generation-e2e`, `sessionStorage`). An in-memory
/// web counter would reset to 0 across exactly the relaunch it exists to detect,
/// turning the assert vacuous in the one direction that matters.
pub const SESSION_GENERATION_KEY: &str = "session_generation";

/// The state key carrying a monotonic count of **completed account-activation
/// gestures** — one tap on an `account-switcher-item` row, counted when the
/// handler that evaluates the activation gate has returned, whatever it decided
/// (switched, declined, refused, or no-op'd on the already-active row).
///
/// **What it is for, and why it is not redundant with
/// [`SESSION_GENERATION_KEY`].** That counter answers "did a teardown happen";
/// this one answers "is the gesture that might have caused one *finished yet*".
/// [`assert_no_relaunch`-shaped tests][SESSION_GENERATION_KEY] need both: the
/// generation is the assertion, and this is the `settled` argument the helper
/// requires — the trigger handler's own completion observable, without which the
/// [`BARRIER`] would anchor to the click's *dispatch* rather than to the
/// handler's completion, and a teardown initiated a few statements later would
/// slip past.
///
/// **Why the three native apps need a counter where tui/linux/web need
/// nothing.** On those three the re-auth prompt is an in-app element, so
/// "the prompt closed" is already a completion observable and `settled` is just
/// `not is_visible(REAUTH_PROMPT)`. On **windows, apple and android** the prompt
/// is a native OS dialog — Windows Hello, LocalAuthentication, BiometricPrompt —
/// which carries no test ID by construction (`ui.yaml` declares
/// `account-activate-reauth-prompt` "never render" on all three), and under the
/// e2e seam it is not even shown: the verdict is read from a file. So the
/// gesture resolves with *nothing on screen changing at all*, which is precisely
/// the decline arm's product contract ("a pure no-op") — there is no state
/// change to watch, and the completion has to be published deliberately.
///
/// **Why a monotonic counter rather than an "in flight" flag.** A flag is the
/// obvious shape and is unsound here for the reason [`SESSION_GENERATION_KEY`]
/// documents from the other side: `false` is also its value *before* the handler
/// ever started, so a `settled` reading it can pass on the pre-gesture world and
/// the barrier again anchors to the dispatch. A count read *before* the trigger
/// and waited past afterwards cannot false-pass that way — the value it must
/// reach did not exist yet.
///
/// **Contract, identical on every app that publishes it.** Starts at 0 for a
/// fresh app process, only ever increases, and is bumped when the activation
/// handler completes — in a `finally`, so a refusal or a thrown error counts too
/// (a gesture that failed still *finished*, and a `settled` that only fires on
/// the happy path would hang the negative assert instead of failing it).
/// Publishing it is optional for an app whose prompt is in-app; an app that does
/// not publish it must be refused loudly by the consumer rather than treated as
/// settled (convention 11).
pub const ACTIVATION_GESTURES_KEY: &str = "activation_gestures";

/// The state key carrying the **critical-alert sweep's pass counters** —
/// `{"started": N, "completed": M}`, read straight off the shared
/// `CriticalAlerts` registry (`fauna_client_alerts::CriticalAlerts::
/// sweep_passes_started`, which owns the contract and the reasoning).
///
/// **Contract, identical on every app.** Both counters start at 0 for a fresh
/// app process and only ever increase. A pass bumps `started` before its first
/// feeder reads anything and `completed` after its last feeder has posted or
/// cleared, so `completed` never runs ahead of `started`.
///
/// **What it is for.** The sweep is the *only* trigger for the feeders that have
/// no page of their own (`critical-alerts.md` § Mechanism → *Who runs the
/// detector*), and it is a fire-and-forget spawn — so a test asking the negative
/// question, "this planted condition raises NO banner", had nothing to anchor to
/// and slept instead. Read `started` when planting the condition, then wait for
/// `completed` to exceed that value: a pass that began after the plant has now
/// finished, and the banner may be read.
///
/// ⚠ **Both halves are load-bearing — do not simplify this to one counter.** An
/// app's post-auth hook spawns a sweep *loop* per session establishment, and
/// re-establishing without a teardown leaves the previous loop running, so
/// several passes can be in flight at once and a bare completion count cannot
/// tell "a pass that saw my plant finished" from "a pass that started before it
/// finished". Comparing against `started` settles that by pigeonhole without
/// counting the loops.
pub const ALERT_SWEEP_PASSES_KEY: &str = "alert_sweep_passes";

/// The critical-alert re-sweep loop's **poke** — convention 14's `run_now` for
/// the six-hour clock (`fauna_client_alert_sweep::RE_SWEEP_INTERVAL_SECS`), the
/// twin of [`CONV_RECEIVE_NOW`] on a far slower cadence.
///
/// **Contract.** The command ends the current identity's re-sweep WAIT now, so
/// the app's real `run_alert_sweep_loop` body sweeps again
/// (`fauna_client_alert_sweep::run_alert_sweep_loop_wakeable` — the production
/// loop with only its wait raceable), and returns without awaiting the pass. The
/// barrier is therefore [`ALERT_SWEEP_PASSES_KEY`], not this ack: read `started`,
/// plant the condition, poke, wait for `completed` to pass it. A poke that lands
/// mid-pass is kept for the next wait, never dropped.
///
/// **Why this and not a re-established session.** A same-actor session patch
/// runs a converge-arm ONE-SHOT pass, not the loop — so a "condition that arises
/// while the app is open is announced without a restart" journey driven that way
/// passes with the loop deleted. Only waking the loop witnesses the clock the
/// goal doc names (`critical-alerts.md` § Mechanism → *How often the detector
/// runs*).
///
/// Pre-auth there is no loop to wake; the app refuses the poke loudly
/// (convention 11) rather than acking a wake nothing will ever read.
pub const ALERT_SWEEP_WAKE: &str = "alert_sweep_wake";

/// The Guardian Notify flush **poke** — convention 14's `run_now` applied to a
/// client-side cadence instead of a nest-side one.
///
/// The ward's client batches its guardian-floor enforcement events and flushes
/// them to the nest via `fauna.family.notify_report`
/// (`family-safety.md` § Guardian Notify). The flush rides each app's
/// once-a-minute tick — tui/linux `LOCK_TICK` (60 s), web's `lockTick` — so a
/// test that needs the report *now* would otherwise sleep out a full minute,
/// and would be racing it rather than waiting for it: the window in which the
/// ward's client still exists is bounded by the test's own next step (switching
/// identity destroys it). That is precisely the wall-clock dependence
/// convention 14 forbids.
///
/// **Contract.** The command runs the app's real flush path — the same
/// `take_due` gate the tick would call, not a bypass — and acks only once that
/// pass has completed. The *cadence gate itself is untouched*: the accumulator's
/// ≤hourly `notify_report_min_interval_secs` still applies, and the only case a
/// test needs is the eager first flush (no prior flush), which the gate already
/// allows. Nothing is injected into the clock.
///
/// A no-op ack is a correct outcome (nothing pending, or no authenticated ward)
/// and must stay quiet — unlike [`BARRIER`], an early ack here cannot silently
/// weaken a negative assert, because the property under test is *positive* (the
/// guardian's readout reaches the count) and fails loudly if the flush never
/// happened.
///
/// Per-app depth differs, and the difference is worth knowing when reading a
/// failure: **tui and linux await the `fauna.family.notify_report` round trip**,
/// so the ack is a true barrier; **web** fires the flush and returns without
/// awaiting the RPC (`$lib/family-notify-e2e` → `notifyBuffer.checkNow()`), so
/// there the guardian-side poll is what closes the loop. Both satisfy the
/// contract — "the flush pass has been run" — but only the first is a barrier.
pub const FAMILY_NOTIFY_CHECK_NOW: &str = "family_notify_check_now";

/// The state key carrying **per-channel counts of inbound MLS commits this
/// device folded in**, `{"<channel_hex>": N, …}` — read straight off the shared
/// backend (`fauna_conversations::backends::fauna_mls::FaunaMlsBackend::
/// folded_commits`, which owns the contract and the full reasoning; the JSON
/// shape is derived once in `fauna_conversations::state_json::
/// mls_folded_commits_json`).
///
/// **The barrier the product used to withhold.** A cross-device test needs to
/// know that device A has incorporated device B's epoch takeover *before* A
/// sends again — otherwise A's send is a stale blind-append rather than the
/// takeover path under test, and the downstream "the channel grew" assertion
/// passes either way. The natural barrier — wait for A to render B's message —
/// is not merely weak here, it is **impossible**: twin devices of one actor share
/// a single MLS leaf, and a sender cannot MLS-decrypt its own application
/// messages (`docs/goal/behavior/devices.md` § Cross-device MLS group-state
/// sync), so A can never see B's text at all. It was built and measured RED at
/// the full 90 s handshake budget before this key existed. What A *does* process
/// is B's **commit**, and this count is that fold-in made observable.
///
/// **Contract, identical on every app.** `{}` for a fresh app process (and for
/// one with no conversations session yet); each channel's count only ever
/// increases; an absent channel reads as 0. A commit that did **not** get
/// incorporated (`CommitApplyOutcome::Stalled`) never bumps it — releasing a
/// barrier on a stall would hand the test exactly the stale-epoch state it exists
/// to rule out.
///
/// **Usage — read the baseline BEFORE the peer writes**, not after:
///
/// ```text
/// folded = mls_folded_commits(driver)[channel_hex]   # baseline, before B sends
/// device_b.send(...)                                  # B's takeover commit lands
/// await_folded_commit_after(driver, channel_hex, folded, budget_s=...)
/// device_a.send(...)                                  # now provably a takeover
/// ```
///
/// A baseline read *after* B's send can already include the fold-in, leaving the
/// consumer waiting for a second commit that nobody will ever author.
///
/// ⚠ Publishing `{}` and not publishing at all are **different** answers and must
/// stay so: an app without the leg publishes nothing, and the consumer refuses
/// loudly (convention 11) instead of reading a silent "no fold-ins yet" and
/// hanging.
pub const MLS_FOLDED_COMMITS_KEY: &str = "mls_folded_commits";

/// The state key carrying the **client receive loop's cycle counters** —
/// `{"started": N, "completed": M}`, read straight off the shared session
/// (`fauna_conversations::ReceiveCycles`, which owns the contract; the JSON shape
/// is derived once in `fauna_conversations::state_json::conv_receive_cycles_json`).
///
/// **Contract, identical on every app.** Both counters start at 0 for a fresh
/// app process (and for one with no conversations session yet) and only ever
/// increase. A *cycle* is one full sweep of every receive rail — the durable
/// inbox drain, the conversation poll, the mail read-feeds, the scheduling and
/// folder feeds — whichever arm triggered it (backstop ticker, reconnect, or
/// the [`CONV_RECEIVE_NOW`] poke). A push's single-rail wake is deliberately not
/// a cycle: a caller asking "has a sweep run" means the sweep that would have
/// delivered *anything*.
///
/// **What it is for.** The receive loop is the delivery path for MLS
/// conversations and the mail read-feeds that ride it, and its backstop is a
/// 30 s ticker — so a test needing "the inbound has been looked for" had nothing
/// to anchor on and the e2e *shortened the tick* instead
/// (`FAUNA_CONV_POLL_SECS=2`). That is still a wall-clock dependence: it lowers
/// the odds of a false pass without changing the shape (convention 14).
///
/// **Usage — read `started` BEFORE the trigger**, then poke and wait for
/// `completed` to exceed that value:
///
/// ```text
/// started, _ = conv_receive_cycles(driver)      # baseline, before the trigger
/// peer.send(...)                                 # the arrival to be delivered
/// driver.call_command(CONV_RECEIVE_NOW)          # ask for a cycle now
/// await_receive_cycle_after(driver, started, budget_s=...)
/// assert thread_is_there()                       # a cycle has provably looked
/// ```
///
/// ⚠ **Both halves are load-bearing — do not simplify this to one counter**, for
/// the same reason [`ALERT_SWEEP_PASSES_KEY`] states: a bare completion count
/// cannot tell "a cycle that began after my trigger finished" from "a cycle
/// already in flight when I triggered finished". Comparing against `started`
/// settles it by pigeonhole, with no count of how many loops are live — which is
/// what keeps it sound across the linux session re-injection case, where a
/// replaced session's loop runs until its next liveness check.
///
/// ⚠ Publishing `{"started": 0, "completed": 0}` and not publishing at all are
/// **different** answers and must stay so: an app without the leg publishes
/// nothing, and the consumer refuses loudly (convention 11) instead of reading a
/// silent zero and spending its whole budget.
///
/// **`exit` — why the loop left, beside the counters.** `null` while the loop
/// runs (and before any session); once it has left, `"closed"` (the session was
/// dropped) or `"retired"` (its engine was handed over) — both designed — or
/// `"panicked"` (a receive pass panicked: the rail is dead until the app
/// restarts). Native words: `fauna_conversations::session::ReceiveLoopExit`.
/// Web's pump has no designed exit and publishes `"stalled"` while a pass has
/// outrun its ceiling (wasm cannot tell a panic from a hang), `null` otherwise.
/// The post-frame checker's `receive-loop-alive` fires on `"panicked"` and
/// `"stalled"` only, and reads a mapping with no `exit` field as `n/a`.
pub const CONV_RECEIVE_CYCLES_KEY: &str = "conv_receive_cycles";

/// The client receive loop's **poke** — convention 14's `run_now` for the
/// conversations/MLS/mail-read-feed delivery path, the twin of
/// [`FAMILY_NOTIFY_CHECK_NOW`] on a different cadence.
///
/// **Contract.** The command asks the app's real receive loop to run one cycle
/// now — the identical `full_sweep!` its backstop ticker runs, never a bypass or
/// a per-rail shortcut — and returns without awaiting it. The barrier is
/// therefore [`CONV_RECEIVE_CYCLES_KEY`], not this ack: read `started`, poke,
/// wait for `completed` to pass it.
///
/// **Why the ack is deliberately not the barrier.** An awaited reply would hang
/// whenever no loop is running (a session built but not started, a poke issued
/// pre-auth) — the reply would sit in a queue nobody drains. Signal-plus-counters
/// degrades loudly instead: the counters do not move and the consumer's deadline
/// poll fails naming the app and the key.
///
/// A no-op ack is a correct outcome (no session yet) and stays quiet: the
/// property under test is always positive — something should have been
/// delivered — so a cycle that never ran fails at the consumer's own wait rather
/// than silently here.
pub const CONV_RECEIVE_NOW: &str = "conv_receive_now";

/// State key: the **account-plane pump's** full-pass cycle counters —
/// `{"started": N, "completed": M}`, the [`CONV_RECEIVE_CYCLES_KEY`] twin for
/// the account runtime (`fauna_sync_engine::account_runtime::PumpCycles`,
/// which owns the counting; the handle's `pump_cycles()` is the read).
///
/// **Contract, identical on every app.** Both counters start at 0 for a fresh
/// process (and for one with no account store yet) and only ever increase. A
/// *cycle* is one full pump pass — reconcile + publish + bridge + the peer and
/// custody legs — whichever edge ran it (prologue, nudge, backstop ticker, or
/// the [`ACCOUNT_PUMP_NOW`] poke); a panic-contained pass still completes.
/// Same two-counter pigeonhole as the receive cycles: read `started` BEFORE
/// the trigger, poke, wait for `completed` to pass the baseline. Publishing
/// zeros and not publishing at all stay **different** answers (convention 11):
/// an app with no account-store leg publishes nothing.
pub const ACCOUNT_PUMP_CYCLES_KEY: &str = "account_pump_cycles";

/// The account-plane pump's **poke** — convention 14's `run_now` for the
/// account runtime: ask the app's store runtime for one full pump pass NOW
/// (`AccountStoreHandle::reconcile_now`, the ticker's own work on demand —
/// never a bypass), then run the app's ceremony drive so any act the pass
/// left owed (an unposted custody receipt, an owed registry write) posts
/// without waiting for a production edge. Fire-and-forget: the barrier is
/// [`ACCOUNT_PUMP_CYCLES_KEY`], not this ack — an awaited reply would hang
/// when no runtime is up (pre-auth), and a poke that ran nothing fails at
/// the consumer's own deadline poll, naming the app and the key.
pub const ACCOUNT_PUMP_NOW: &str = "account_pump_now";

/// State key: the photo-backup **pass funnel**, apple-only today (PhotoKit is
/// the ingress, so web/linux/windows/tui have no leg and publish nothing —
/// convention 11: publishing zeros and not publishing at all stay different
/// answers). Shape: `authorization` (the Photos grant word) plus the integer
/// counters `assets_seen`, `already_synced`, `export_failed`, `ingest_failed`,
/// `uploaded`, `pending`, `passes_started`, `passes_completed`, `completed_total`,
/// and `last_backup_at`, plus `enabled` — the install's PERSISTED backup choice
/// (the `fauna.photoBackupEnabled` default), not the toggle's view state: an ON
/// is written only once PhotoKit's authorization callback lands, so this is the
/// barrier for "the choice took", which no pass counter can be (a pass may return
/// before it counts, e.g. with no network).
///
/// **Why a funnel and not a boolean.** A pass that uploads nothing is this
/// feature's whole failure surface and it has four causes that are identical from
/// outside the app — PhotoKit handed us nothing, everything was already synced,
/// the export failed, the ingest threw — three of them silent by construction.
/// The counters make "backup did nothing" name WHICH nothing. Every value is a
/// plain field read off `PhotoBackupEngine` (convention 11 — never a round trip).
///
/// `passes_started` / `passes_completed` are the ordinary two-counter pigeonhole
/// ([`ACCOUNT_PUMP_CYCLES_KEY`]): four different edges call `syncNewPhotos()`
/// (the enable toggle, the PhotoKit change observer, the `BGProcessingTask`, and
/// the Sync-now button), so a completion *timestamp* cannot attribute a pass to
/// the one under test. Read `passes_started` before the trigger, trigger, then
/// wait for `passes_completed` to pass the baseline. `pending` is the in-flight
/// countdown behind `photo-backup-sync-progress`; it is 0 whenever no pass is in
/// flight, so it is a progress readout and never a pass barrier.
pub const PHOTO_BACKUP_KEY: &str = "photo_backup";

/// Photo backup's **scheduled-pass poke** — convention 14's `run_now` for the one
/// photo-backup ingress trigger no test can wait for: on iOS the OS owns the
/// schedule (`ui/folders.md` § Photo backup — "iOS grants no period control, so
/// the OS schedules"), and `BGProcessingTask` has no public initializer, so the
/// handler cannot be called with a real task either.
///
/// **Contract.** The command drives `BackgroundScheduler.runScheduledUploadPass`
/// — the *production* body of the `social.fauna.sync.upload` handler, extracted
/// so the poke and the OS take the identical path, never a test-only shortcut
/// around it. Fire-and-forget: the barrier is [`PHOTO_BACKUP_KEY`]'s
/// `passes_completed`, not this ack, so a poke arriving before the session has an
/// engine is a quiet, honoured no-op rather than a dropped command
/// (convention 11) and the consumer's own deadline poll is what fails, naming the
/// app and the key. apple-only, like the key.
pub const PHOTO_BACKUP_SCHEDULED_PASS_NOW: &str = "photo_backup_scheduled_pass_now";

/// Photo backup's **library seed** — the macOS venue's stand-in for "a photo was
/// taken": add the image at the command's `path` to the REAL System Photo Library
/// through a real `PHAssetCreationRequest`, and report
/// `{"local_identifier", "filename", "library_count"}` once it has been fetched
/// back. The iOS twin seeds from outside the app (`simctl addmedia`); macOS has no
/// such tool, so the add goes through PhotoKit inside the app.
///
/// **Contract.** Awaited, with the report in the result slot. Refused LOUDLY —
/// never a prompt — when the Photos grant is not in place, because a change request
/// under `notDetermined` raises the system alert and blocks an unattended run.
/// Driven only by the real-session photo-library launch
/// (`e2e-conventions.md` convention 12's macOS arm), whose driver refuses it in every
/// other launch mode: the library is machine-global state convention 10 keeps every
/// ordinary launch away from. apple-only (macOS).
pub const PHOTO_BACKUP_SEED_LIBRARY: &str = "photo_backup_seed_library";

/// Photo backup's **grant request** — ask the OS for Photos read-write access and
/// report `{"authorization": "<word>"}` (the [`PHOTO_BACKUP_KEY`] funnel's words).
/// Already decided, it answers at once; undecided, it raises the real system alert
/// and answers once a human has clicked it — the one human step of the macOS venue,
/// given once per stably signed test bundle id. apple-only (macOS).
pub const PHOTO_BACKUP_REQUEST_ACCESS: &str = "photo_backup_request_access";

/// State key: the home-screen widget's **background-refresh pass counters** —
/// `{"passes_started": N, "passes_completed": M, "last_pass_count": C}`, iOS-only
/// (the one app whose widget currency rests on an OS-scheduled task; macOS keeps
/// the app resident instead and publishes nothing — convention 11).
///
/// **Why counters of its own.** The pass is one conversations receive pass whose
/// ingest ticks the same observer every foreground arrival ticks, so neither the
/// widget snapshot nor [`CONV_RECEIVE_CYCLES_KEY`] can attribute anything to the
/// scheduled entry point. These counters are bumped by
/// `BackgroundScheduler.runWidgetRefreshPass` and nothing else: read
/// `passes_completed` before [`WIDGET_REFRESH_SCHEDULED_PASS_NOW`], wait for it to
/// pass the baseline. `last_pass_count` is the unread total the last completed
/// pass left behind — the count the widget shows — or `null` when that pass had
/// no live conversations session to poll, so "the pass ran" and "the pass
/// reached the rails" stay different answers.
pub const WIDGET_REFRESH_KEY: &str = "widget_refresh";

/// The home-screen widget's **scheduled-pass poke** — convention 14's `run_now`
/// for the iOS `social.fauna.widget.refresh` `BGAppRefreshTask`, a schedule the
/// OS alone owns and a task type with no public initializer
/// ([`PHOTO_BACKUP_SCHEDULED_PASS_NOW`]'s situation exactly).
///
/// **Contract.** The command drives `BackgroundScheduler.runWidgetRefreshPass` —
/// the *production* body of the handler, extracted so the poke and the OS take
/// the identical path. Fire-and-forget: the barrier is [`WIDGET_REFRESH_KEY`]'s
/// `passes_completed`, not this ack; a poke before a session exists is a quiet
/// no-op the consumer's own deadline poll fails on. iOS-only; macOS refuses it.
pub const WIDGET_REFRESH_SCHEDULED_PASS_NOW: &str = "widget_refresh_scheduled_pass_now";

/// State key: the shared feed manager's reload triple — `{"started": N,
/// "completed": M, "committed_gen": G}`, the [`CONV_RECEIVE_CYCLES_KEY`] twin
/// for the feed's re-query funnel (`fauna_feed::FeedManager::reload_counts`,
/// which owns the counting; `fauna_feed::feed_reloads_json` is the one shared
/// JSON shape).
///
/// **Contract, identical on every app.** `started` counts reload *initiations*
/// (the same value the manager's supersede guard claims — bumped synchronously
/// at the reload's first statement, convention 14's "initiated synchronously"
/// corollary for free); `completed` counts reloads that **committed** their
/// result to the snapshot, Ok and Err arms alike; `committed_gen` is the
/// *generation* of the newest such commit.
///
/// **Read `started` BEFORE the trigger, then wait for `committed_gen` to pass
/// that baseline.** Generations are claimed by `fetch_add` at the reload's
/// first statement, so `committed_gen > baseline` says exactly "a re-query that
/// BEGAN after the baseline read has landed its verdict", however many
/// refreshes were in flight when you read it.
///
/// ⚠ **Do NOT wait on `completed` instead** — the two-counter pigeonhole this
/// key shipped with (2026-08-22) is UNSOUND and was retired 2026-08-23. A
/// superseded reload never commits, so each one widens `started - completed`
/// permanently; a baseline taken after even one leaves `completed >
/// started-at-baseline` unreachable however healthy the manager is. Web burned
/// a 300 s budget and two sessions on it: a reconnect
/// fires two overlapping reloads *by construction*, the older is superseded,
/// the newer commits and renders its posts — and the barrier still reported
/// "no re-query ever committed". `completed` stays in the shape because the
/// `(started, completed)` pair is what separates the failure classes at a
/// timeout, but it is a diagnostic, never a release condition.
///
/// The consumer this was built for is the benign-flip re-hydrate guard
/// (`test_nest_flip_resilience.py::test_nest_flip_feed_rehydrate`): it proves
/// "the reconnect-triggered feed re-fetch RAN" by direct observation instead
/// of asserting that no other delivery path exists — the settle-sleep absence
/// premise that made the test unable to give a trustworthy verdict whenever a
/// concurrent (legitimate) refresh delivered the probe first.
///
/// ⚠ Publishing `{"started": 0, "completed": 0, "committed_gen": 0}` (a fresh
/// or not-yet-built manager — the legitimate zero) and not publishing at all
/// are **different** answers and must stay so: an app without the leg publishes
/// nothing, and the consumer refuses loudly (convention 11) instead of reading
/// a silent zero and spending its whole budget. A leg that publishes the pair
/// but not `committed_gen` is the same refusal case — it is a hand-rolled shape
/// rather than a read of `fauna_feed::feed_reloads_json`, and the consumer says
/// so rather than falling back to the retired arithmetic.
pub const FEED_RELOADS_KEY: &str = "feed_reloads";

/// State key: the Devices/Folders page machine's refresh triple —
/// `{"started": N, "completed": M, "committed_gen": G}`, the
/// [`FEED_RELOADS_KEY`] twin for `fauna_devices_machine::DevicesMachine::refresh`
/// (`DevicesMachine::refresh_counts` owns the counting;
/// `fauna_devices_machine::devices_refreshes_json` is the one JSON shape, and
/// `DevicesMachine::refreshes_json` its string form for the wasm/FFI faces).
///
/// **Contract, identical on every app and to [`FEED_RELOADS_KEY`]'s.** `started`
/// counts refreshes begun (the generation each claims at its first statement),
/// `completed` counts refreshes that committed their verdict to the snapshot —
/// an errored read commits too — and `committed_gen` is the newest committed
/// generation. **Read `started` BEFORE the trigger, then wait for
/// `committed_gen` to pass it**; `completed` is a diagnostic, never a release
/// condition.
///
/// **Why it exists.** A page visit refreshes the machine *asynchronously* on
/// most apps (linux spawns it from the page's map hook; web from the section's
/// mount), so a read right after the visit's nav ack sees the PRE-visit
/// snapshot — and a witness asserting "the rows still stand after a refresh
/// that could read nothing" passes vacuously on it. tui acks the nav only after
/// its refresh ran, which is why the Folders witnesses were first written there.
///
/// **The count must outlive a page visit.** The machine is one per signed-in
/// session on every app (web included, `wasm-folders.ts::sessionDevicesMachine`),
/// so the triple is monotonic across visits; a machine rebuilt per mount would
/// reset it under a baseline taken on the previous one. Published
/// unconditionally once the app has the leg — the zero triple before a machine
/// exists is the legitimate "none yet", distinct from an absent leg.
pub const DEVICES_REFRESHES_KEY: &str = "devices_refreshes";

/// State key: the post-claim **serving-enablement** step's runs —
/// `{"started": N, "completed": M, "runs": [{"actor_id", "decided": {"email",
/// "caldav", "carddav", "webdav"}, "completed"}]}`
/// (`fauna_client_mail_settings::serving_enablement::serving_enablement_json`,
/// the one shared derivation; the recording is `apply_serving_enablement`, the
/// one shared firing of `onboarding.md` § 3b's four machine-derived intents).
///
/// **Why this key exists.** The glue's enables are asynchronous, and the
/// "stays OFF" cases (a local handle, a sign-in, an invite redemption) have no
/// positive event to poll for — so its witnesses used to read the deployment
/// toggles after a fixed settle window, which false-REDs a slow box and
/// false-GREENs a glue that wrongly enables late. A run's `completed` is the
/// causal anchor convention 14 asks for instead.
///
/// **Contract.** One record per `LoggedIn` handoff this process ran, appended
/// at the step's first statement with the handoff's actor and the intents it
/// decided; `completed` flips once every step its plan holds has been answered
/// — **the decide-nothing run included** — and by a drop guard, so every
/// started run lands. **Find YOUR run by the actor you onboarded and wait for
/// its `completed`, then read the toggles once**: nothing this glue does for
/// that actor can land after that. Keyed on the actor rather than a count
/// against a baseline because the onboarding flows relaunch the app before the
/// handoff, which resets every process-global count; the `started`/`completed`
/// totals stay as a diagnostic only.
///
/// ⚠ The empty shape (`"runs": []` — the glue has not run in this process) and
/// not publishing at all are **different** answers and must stay so: an app
/// without the leg publishes nothing and the consumer refuses loudly
/// (convention 11).
pub const SERVING_ENABLEMENT_KEY: &str = "serving_enablement";

/// State key: what this session's inbound poll did with peer **share-endpoint
/// advertisements** — `{"seen": N, "no_sink": N, "captured": N,
/// "uncaptured": N}` (`fauna_conversations::state_json::
/// share_endpoints_counts_json`, the one shared derivation; the counting is
/// `FaunaMlsBackend::share_endpoints_counts`).
///
/// **Why this key exists.** The peer-transfer journey's step-3 barrier waits
/// for a `share-transfer-item` row and, when none comes, can say only *"no
/// dial row appeared"* — which is the same sentence for an advertisement that
/// never crossed the wire, one that crossed and was dropped for want of a
/// registered sink, and one the sink refused or could not write. Those three
/// point at different halves of the system, and separating them cost seven
/// e2e runs and four sessions of instrumentation
/// because the tally was read by tier_1 tests only. Published, the same
/// failure reads *"40 seen, 0 captured"* (conventions point 6).
///
/// **Contract.** `seen` counts decrypted `ShareEndpoints` bodies the poll
/// handed to the ingest arm; `no_sink` those observed with no sink registered
/// (**an app-glue bug on any app that believes it wired the plane** — both
/// tui and linux do); `captured` the ones the sink durably landed as a dial
/// row; `uncaptured` the ones it refused or could not write. Read
/// `uncaptured` as *refused OR unwritable*: an advertisement that did not
/// bind to its channel-proven sender is counted there, and so is one whose
/// account-plane write was refused — a `GenerationTip` row whose generation
/// tip will not resolve lands here, which is exactly what the linux seats
/// measured.
///
/// ⚠ Publishing the four zeros (a session that has polled nothing yet — the
/// legitimate zero) and not publishing at all are **different** answers and
/// must stay so: an app without the leg publishes nothing and the consumer
/// refuses loudly (convention 11) rather than reading a silent zero and
/// spending its whole budget waiting for a plane that was never wired.
pub const SHARE_ENDPOINTS_COUNTS_KEY: &str = "share_endpoints_counts";

/// State key: what this process's share plane has **served** to peers, as
/// `{"manifests": {path: n}, "chunks": {path: n}, "parked": n, "held": bool}`
/// (`fauna_sync_engine::share_serve_tally`, which owns the recording).
///
/// **What it witnesses.** `offline-sharing` outcome 8: *an interrupted
/// device-to-device transfer picks up where it stopped without re-sending what
/// arrived* (`p2p.md` § Cross-user shared-set transfer, "held chunks never
/// re-sent"). "Never re-sent" is a fact about the wire, so the SENDER counts:
/// one manifest and one chunk per successful answer, keyed by path. A receiver
/// that re-fetched a file it already held would otherwise read exactly like one
/// that never asked.
///
/// **`parked` / `held`** belong to the `offline_share_hold_serves` command.
/// While the hold is on, the next manifest request parks unanswered, so a
/// journey can cut the connection at a state-defined mid-transfer moment rather
/// than on a race. A parked request is never counted as served.
///
/// An app without a share-plane leg publishes nothing, which is a different
/// answer from an empty tally (the `SHARE_ENDPOINTS_COUNTS_KEY` contract).
pub const SHARE_SERVE_TALLY_KEY: &str = "share_serve_tally";

/// State key: the **new-message OS banners this app process actually fired**,
/// as `{"started": N, "completed": M, "fired": [{"thread_id", "label"}, …]}`,
/// built by the one shared derivation
/// `fauna_conversations::notification::message_banners_json` (which owns the
/// contract and the recording, so no app keeps its own tally).
///
/// **What it witnesses.** `conversations` outcome 11 — *a new message raises a
/// system notification while the app is running, except in the conversation you
/// already have open* (`conversations.md` § Where logic lives). The decision is
/// the shared `MessageNotificationTracker`'s three rules; the *firing* is app
/// glue per platform. Nothing else in the product reports that a banner went
/// out: the OS notification centre is not readable from a test, and asserting on
/// it would be asserting on the last inch rather than the mechanism. Before this
/// key the outcome was witnessed on **no column at all**, on either of the two
/// apps that had built the firing.
///
/// **The `fired` list is append-only for the process lifetime** and ordered by
/// firing, never a "latest banner" slot — a slot cannot answer *how many* or
/// *which threads*, and a test that read one would silently pass on a tick that
/// fired a banner it never meant to.
///
/// **⚠ Both counters are load-bearing — do not simplify to one.** They exist for
/// exactly the reason [`ALERT_SWEEP_PASSES_KEY`] gives: the assertions this key
/// serves are mostly *negative* ("the thread I have open raises NO banner"), and
/// a tick that merely *finished* after the plant may have read its snapshot
/// before it. The app bumps `started` before reading the snapshot it will diff
/// and `completed` after the tick's last fire, so `completed > started_at_plant`
/// proves by pigeonhole that a tick which began after the plant has finished —
/// with no count of how many observers the app runs.
///
/// **Contract, identical on every app that publishes it.** Both counters start
/// at 0 for a fresh app process and only ever increase; `completed` never runs
/// ahead of `started`; an entry is appended at the **firing site**, after every
/// suppression the app applies, so the list means "the user was shown this",
/// not "the tracker returned this".
///
/// ⚠ Publishing `{"started": 0, "completed": 0, "fired": []}` (an app that has
/// the leg and has fired nothing yet) and not publishing at all are **different**
/// answers and must stay so, exactly as for [`FEED_RELOADS_KEY`]: an app that
/// has not built the firing publishes nothing, and the consumer refuses loudly
/// (convention 11) rather than reading the empty list as a proven negative —
/// which would turn every unbuilt column's *absence* of a banner into a pass.
pub const MESSAGE_BANNERS_KEY: &str = "message_banners";

/// State key: this app's **transport connection**, as
/// `{"state": "<lowercase wire word>", "online": <bool>}` — built by
/// [`connection_json`], the one shared derivation.
///
/// **Why the harness needs it at all.** Every app desensitizes an
/// `OnlineOnly` affordance while the transport word is offline
/// (`fauna_protocol::offline_class::affordance`, the shared rule
/// `account-data-plane.md` § The offline-mutation contract class 3 owns), and
/// `"connecting"` is one of the offline words. So an e2e test that drives an
/// online-only control on a freshly launched app is racing the WS handshake
/// and loses exactly when the box is loaded — a latency-dependent flake of
/// precisely the class conventions point 14 forbids, invisible because it
/// only fires under load. The harness had no way to wait the handshake out:
/// nothing anywhere read connection state. This key is that observable, and
/// the fixtures barrier on it once per login rather than per action.
///
/// **`online` is shipped, not derived by the reader.** The verdict is
/// `fauna_protocol::offline_class::is_online` — "online unless the word is a
/// *known* offline word", the deliberately asymmetric polarity that keeps a
/// future state word from greying a working control. A consumer comparing the
/// word to `"connected"` inverts that ruling and blocks forever on the one
/// case the gate was built to tolerate, so the boolean crosses the boundary
/// already decided and the offline word list keeps exactly one owner (the same
/// bargain web already struck for the affordance itself, which it takes from
/// wasm as `{available: bool}` rather than re-reading the table in TS).
///
/// **`state` rides along for diagnosis only** — it is what makes a barrier
/// timeout say *"still `connecting` after Ns"* instead of "timed out"
/// (conventions point 6).
///
/// ⚠ Publishing `{"state": "connecting", "online": false}` (a real app still
/// shaking hands) and not publishing at all are **different** answers and must
/// stay so, exactly as for [`FEED_RELOADS_KEY`]: an app without the leg
/// publishes nothing and the barrier says which app is missing it, rather than
/// reading a silent `false` and spending its whole budget.
pub const CONNECTION_KEY: &str = "connection";

/// The state key carrying the app-held session bearer's **schedule on the
/// app's own clock** — `{"expires_in_secs": <i64>|null, "own_session_ids": [..]}`,
/// built by [`launch_token_json`]. `expires_in_secs` is the held bearer's
/// deadline (anchored at receipt on the one client clock,
/// `fauna_protocol::client_clock`) minus that clock's `now`: how long until the
/// refresh fires, plus the pre-expiry buffer. `null` while no bearer is held.
///
/// **Whose bearer.** The one the app actually refreshes: the launch machine's
/// (`TokenStatus::Valid { expires_at_secs }`) on tui and linux, whose session
/// bearer it serves through `LaunchMachineBearer`; `FfiNestClient`'s
/// `WsChallengeBearer` cache on the four UniFFI apps
/// (`FfiNestClient::launch_token_json_for_test`); `getAuthToken`'s home-nest
/// cache on web. The companion command [`LAUNCH_REFRESH_TOKEN`] forces that
/// same bearer's refresh.
///
/// **What it is for.** The wrong-clock refresh witness
/// (`test_onboarding_launch_routing_smoke.py` case M) launches an app whose
/// clock is hours AHEAD of the nest. The bug it guards against is a schedule
/// computed against the nest's absolute `expires_at` on that clock — the sleep
/// saturates to zero and the loop re-mints in a hot loop after every successful
/// refresh — and its symptom is exactly a non-positive value here. A read of
/// this key is the latency-independent assertion (convention 14) that the
/// refresh is *scheduled*, not spinning; `own_session_ids` is the client-side
/// mint count, which the test cross-checks against the nest's own sessions
/// list. TOP-level for the same cross-app-one-depth reason as
/// [`SESSION_GENERATION_KEY`]; every app's leg is one call to
/// [`launch_token_json`] (wasm-safe, in `fauna-e2e-contract`, so web and the
/// FFI build the same shape) over its held bearer.
pub const LAUNCH_TOKEN_KEY: &str = "launch_token";

/// The agent command forcing the held session bearer's refresh NOW, through
/// the production refresh path, awaited so the ack lands after the outcome —
/// the wrong-clock refresh witness's ceremony leg (case M). The bearer is the
/// one [`LAUNCH_TOKEN_KEY`] describes: `LaunchMachine::refresh_token` on tui
/// and linux, `FfiNestClient::refresh_held_bearer_for_test` (clear + re-mint
/// over the silent challenge) on the UniFFI apps, `getAuthToken(…, true)` on
/// web. No payload.
pub const LAUNCH_REFRESH_TOKEN: &str = "launch_refresh_token";

/// Build [`LAUNCH_TOKEN_KEY`]'s value — re-exported from the wasm-safe
/// `fauna-e2e-contract`, which owns the arithmetic.
pub use fauna_e2e_contract::launch_token_json;

/// The state key carrying the **launch clock this process signs in on** —
/// `{"offset_secs": <i64>, "now_secs": <i64>}`, built by [`clock_json`] from
/// `fauna_launch_machine::launch_clock`'s two getters (`clock_offset_secs`,
/// `now_secs_or_zero`).
///
/// **What it is for.** The wrong-clock launch witness
/// (`test_onboarding_launch_routing_smoke.py` case L) seeds
/// `FAUNA_E2E_CLOCK_OFFSET_SECS` and must prove the seed reached the process
/// that signed in: launch takes the silent challenge unconditionally, so a
/// green run on the REAL clock would witness nothing. This key is that
/// in-app control. One shape on all seven apps: the Rust shells call
/// [`clock_json`]; the FFI and wasm shells call their flavor's two test
/// getters and publish the same two keys. TOP-level for the same
/// cross-app-one-depth reason as [`LAUNCH_TOKEN_KEY`].
pub const CLOCK_KEY: &str = "clock";

/// Build [`CLOCK_KEY`]'s value. Both numbers come from
/// `fauna_launch_machine::launch_clock`, which this crate deliberately does not
/// depend on, so the caller reads and passes them.
pub fn clock_json(offset_secs: i64, now_secs: i64) -> Value {
    json!({ "offset_secs": offset_secs, "now_secs": now_secs })
}

/// Build [`CONNECTION_KEY`]'s value from the app's lowercase transport word.
///
/// Every app's leg is this one call, so the polarity cannot drift per app:
/// `fauna_protocol::offline_class::is_online` decides, here, once.
pub fn connection_json(connection_state_word: &str) -> Value {
    json!({
        "state": connection_state_word,
        "online": fauna_protocol::offline_class::is_online(connection_state_word),
    })
}

/// State key: how many connection-state reports this app's indicator has
/// received, and how many of them CHANGED its word —
/// `{"reports": N, "transitions": M, "word": "<last word>"}`, built by
/// [`ConnectionReports::json`].
///
/// **What it is for.** `ConnectionState::Unreachable` is *sticky*: once a run
/// of failures has tripped it, further failed attempts must keep the indicator
/// on "Cannot connect" rather than flicker back to "Connecting…"
/// (`transport-connection.md` § `Unreachable`). A single read of
/// [`CONNECTION_KEY`] cannot prove that — a flicker between two reads is
/// invisible. The pair can: `reports` grows with every state the supervisor
/// publishes (each retry publishes at least one), so "reports moved past R
/// while transitions stayed at T" is exactly "further attempts happened and
/// the word never changed", with no clock anywhere (convention 14).
///
/// **Contract, identical on every app.** Counted where the app's indicator
/// takes its state (tui: the `ConnectionState` UI message), both start at 0 per
/// process and only ever increase; a report whose word equals the previous one
/// bumps `reports` only. A `watch` receiver coalesces bursts, so `reports` is a
/// lower bound on what the supervisor published — never an overcount, which is
/// the direction the stickiness proof needs.
pub const CONNECTION_REPORTS_KEY: &str = "connection_reports";

/// Command: pace the app's reconnect retries —
/// `{"initial_ms": N, "max_ms": M}` to set, `{}` to restore the production
/// 1 s → 60 s. Drives `fauna_client::NestClient::set_reconnect_backoff_for_test`
/// (web drives the wasm reconnect loop's own override,
/// `fauna_rpc_wasm::WsRpcClient::set_reconnect_backoff_for_test`, a separate
/// implementation of the same contract).
///
/// **Why a pace and never a shorter threshold.** The journey it serves proves
/// that a connection which KEEPS failing reads "Cannot connect" — which is only
/// honest if it took the production number of consecutive failures
/// (`fauna_core::format::CONNECTION_UNREACHABLE_AFTER_CONSECUTIVE_FAILURES`). At
/// production pace those cost a minute or two of wall clock, which convention
/// 14 forbids a test to spend; at a test pace they cost milliseconds and are
/// still every one a real refused dial. The ack means the bounds are set, and
/// they reach the running supervisor on its next backed-off failure.
///
/// Refused loudly with no session (no client to pace), and on a malformed
/// payload — a half-parsed pace would silently leave the production one in
/// force and the journey would spend its budget waiting (convention 11).
pub const RECONNECT_BACKOFF: &str = "reconnect_backoff";

/// State key: the error surfaces this app has PAINTED —
/// `{"count": N, "showing": [{"id": …, "text": …}, …]}`, built by
/// [`PaintedErrorTally::json`] (`fauna-e2e-contract`).
///
/// **What it is for.** "A passing connection gap raises no error anywhere — the
/// connection indicator is the only place it shows" (`transport-connection.md`
/// § Connection-status indicator) is a negative claim over a whole window. A
/// read of `error-message` after the gap proves only that no error is up NOW;
/// one raised during the gap and cleared since would pass. `count` is monotonic
/// over every frame the app painted, so "count unchanged across the gap" is the
/// claim itself, anchored to the app's own paint rather than a sample.
///
/// **Contract, identical on every app.** `count` starts at 0 per process and
/// bumps once for each error surface that appears in a painted frame and was
/// not in the one before it (an error left standing across frames counts once;
/// the same error cleared and raised again counts twice). An *error surface* is
/// the page's `error-message` or an element whose id ends in `-error` — the
/// per-action error lines — and never a `-error-log` (a log of past failures,
/// not a raised error). `showing` is what the newest frame painted, for the
/// failure message (convention 6).
pub const PAINTED_ERRORS_KEY: &str = "painted_errors";

/// The pure halves of [`CONNECTION_REPORTS_KEY`], [`RECONNECT_BACKOFF`] and
/// [`PAINTED_ERRORS_KEY`] — the counters, the payload parser and the
/// error-surface predicate. They live in the wasm-safe `fauna-e2e-contract` so
/// the web leg counts with the same code as the native ones; re-exported here so
/// a native host names them where it always has.
pub use fauna_e2e_contract::{
    ConnectionReports, PaintedErrorTally, is_error_surface, reconnect_backoff_bounds,
};

/// A request forwarded to the client's UI thread, with the one-shot reply
/// channel the (blocking) server thread waits on.
pub struct ElementOp {
    pub req: ElementReq,
    pub reply: mpsc::Sender<Value>,
}

/// One element operation. `scope` narrows the lookup to a subtree (empty =
/// global); `index` picks among matches; `arg` carries the text/value/attr the
/// op needs.
pub struct ElementReq {
    pub kind: ElementKind,
    pub id: String,
    pub index: usize,
    pub scope: Vec<ScopeStep>,
    pub arg: String,
}

pub enum ElementKind {
    Text,
    Visible,
    Enabled,
    Count,
    Attr,
    Click,
    /// The Outlook day-cell second press: the widget's normal activation
    /// followed by the `n_press = 2` arm of its own click gesture — the two
    /// effects a real pointer double-press delivers, in that order (`events.md`
    /// § Layout & flow). Clients with no double-press gesture reply `{error:…}`;
    /// never a silent no-op (`testing.md` point 11).
    DoubleClick,
    Type,
    Clear,
    Select,
    /// Simulate the titlebar close button (linux close-to-tray coverage).
    /// Clients without a window reply `{error:…}`. Ignores `id`/`scope`/`arg`.
    WindowClose,
    /// Targeted scroll: bring the (indexed, scoped) element into its scroll
    /// container's viewport — the `POST /element/scroll-into-view` contract
    /// the flaui and apple bridges already serve. Replies `{found: true}` or
    /// `{error: …}` (absent element / no scrollable ancestor / unsupported
    /// client) — never a silent no-op.
    ScrollIntoView,
    /// Press one named key in the (indexed, scoped) element — `arg` is the key,
    /// in the web `KeyboardEvent.key` spelling (`"ArrowLeft"`, `"Home"`, …).
    /// Replies `{ok: true}` once the key's effect has landed, or `{error: …}`
    /// for a key or element the client cannot drive — never a silent no-op
    /// (`testing.md` point 11; the route answered a bare `{ok: true}` and did
    /// nothing until 2026-09-21).
    Key,
    /// The whole current frame as records — `GET /registry`, replying
    /// `{"elements": [...]}` in `PlatformDriver.registry_snapshot`'s cross-app
    /// shape (`tests/e2e-unified/drivers/base.py` owns the field contract; the
    /// apple, windows and web bridges serve the same route). Ignores
    /// `id`/`index`/`scope`/`arg`. A client with no whole-frame registry leaves
    /// the route unserved (`{error: …}`), which the driver reads as `None` —
    /// "no such surface", never an empty frame.
    Registry,
    /// The OS clipboard's text — `GET /clipboard/text`, replying `{"text": …}`
    /// (`null` when the clipboard holds no text), the route the flaui and web
    /// bridges serve for `PlatformDriver.get_clipboard_text`. Ignores
    /// `id`/`index`/`scope`/`arg`. A client whose clipboard is not the host's
    /// to read replies `{error: …}`, never an empty clipboard.
    ClipboardText,
}

/// A liveness stamp the host's UI thread touches on its own cadence, so an
/// [`element`] timeout can say **where the thread actually sits** rather than
/// only that it was late.
///
/// This exists because "the UI thread did not reply within Ns" is one of the
/// most expensive diagnoses this harness produces, and on its own it says
/// almost nothing. `e2e-conventions.md` convention 11 records the cost: every
/// claim→app journey on linux died on that exact message for four days, read
/// first as a newly-landed page, then as three `block_on` call sites that were
/// measured, moved, and changed the failure set *not at all* — the real cause
/// was unrelated to thread blocking. The lesson recorded there is the **search
/// order**: "measure *where the thread actually sits* before believing a
/// located hypothesis". A heartbeat is that measurement, and it is the one an
/// agent-side counter cannot supply — the drivers issue ops one at a time, so
/// the agent alone can never separate a UI thread that is *stalled* from one
/// that is merely slow *on this op*.
///
/// **Reading a timeout's verdict.** The host beats on a short fixed cadence
/// from its main loop (linux: a `glib::timeout_add_local` tick), so at the
/// moment [`element`] gives up:
///
/// * **stale beat** — the main loop itself is not running. Which of convention
///   11's two suspects it is — synchronous work on the thread, or the box
///   starving the process of a core — is answered by the agent's own
///   [`process_liveness`] watchdog, not left to the reader: a stamp beaten by a
///   plain OS thread that shares the process but not the main loop. Fresh
///   there while stale here means *this thread* is blocked and the rest of the
///   process ran — a finding in our code. Stale in both means the process got
///   no CPU at all — the box, not the code.
/// * **fresh beat** — the main loop is fine and served other work while this op
///   waited, so the op itself is what is slow. Suspect the route's own work.
///
/// A host that supplies no heartbeat keeps the old, unclassified message: the
/// field is `Option` precisely so adoption is per-host and a missing beat reads
/// as "not measured", never as "stalled".
///
/// **Why the second stamp was needed, measured.** The first whole-suite
/// `--app linux` sweep to run with a heartbeat (2026-09-02, 6.5 h, 3552 tests)
/// produced **304** stale-beat timeouts and *not one* of them could be
/// attributed, because the verdict above named two suspects and stopped. The
/// disjunction is the whole diagnosis: "our code blocks the loop" is a bug to
/// fix and "the box is oversubscribed" is not, and a sweep that cannot tell
/// them apart is unenumerable either way — which is exactly what happened
/// twice in a row.
///
/// The TYPE stays ungated because tui names it in the signature of plumbing it
/// compiles in every flavor (`automation::beat_while_pending`, and the release
/// twin of `loop_heartbeat` that hands it `None`). Its fields are therefore
/// dead in a release build — nothing constructs one — which the `allow` below
/// states rather than leaves as a warning.
#[cfg_attr(not(any(debug_assertions, feature = "e2e-agent")), allow(dead_code))]
pub struct LivenessStamp {
    /// Milliseconds since `origin`, stamped by the host's main loop. `0` means
    /// "never beaten" — the host has not adopted this yet.
    beat_ms: AtomicU64,
    /// The kernel task id of the thread that beat last. `0` = never beaten, or
    /// an OS with no capture (see [`stall`]).
    beater_tid: AtomicI32,
    origin: Instant,
}

/// The no-op release twin of [`LivenessStamp::beat`].
///
/// tui's `beat_while_pending` is ungated plumbing and closes over `stamp.beat()`,
/// so the method must resolve in a release build; it is never *called* there,
/// because `loop_heartbeat`'s release twin returns `None` and no stamp exists to
/// beat. The same-signature-no-op shape convention 15 prescribes for exactly
/// this case (`e2e-automation-surface-gating.md` § The convention).
#[cfg(not(any(debug_assertions, feature = "e2e-agent")))]
impl LivenessStamp {
    pub fn beat(&self) {}
}

#[cfg(any(debug_assertions, feature = "e2e-agent"))]
impl LivenessStamp {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            beat_ms: AtomicU64::new(0),
            beater_tid: AtomicI32::new(0),
            origin: Instant::now(),
        })
    }

    /// Call from the host's main loop on a short fixed cadence. Cheap by
    /// construction — one monotonic read and two relaxed stores, no allocation
    /// and no lock — so it is safe on the very thread whose responsiveness it
    /// measures.
    pub fn beat(&self) {
        let ms = self.origin.elapsed().as_millis() as u64;
        // `max(1)`: elapsed can still be 0ms on the first beat, and 0 is the
        // sentinel for "never beaten". A beat must never read as absent.
        self.beat_ms.store(ms.max(1), Ordering::Relaxed);
        // Record WHICH thread beat, not just when. This is what lets a stall
        // be sampled rather than only detected: the thread that last advanced
        // the loop is the thread now holding it, on a glib timer tick (linux)
        // and on a `block_on` main loop (tui) alike. Reading it per beat rather
        // than once at construction is deliberate — a host may build its stamp
        // on one thread and run its loop on another, and the beat is the only
        // event that proves which thread the loop is really on. The tid comes
        // from a thread-local, so this stays two stores.
        self.beater_tid
            .store(stall::current_tid(), Ordering::Relaxed);
    }

    /// The kernel task id of the thread that last beat, if any — the thread
    /// [`stall::capture`] samples when this stamp goes stale.
    pub(crate) fn beater_tid(&self) -> i32 {
        self.beater_tid.load(Ordering::Relaxed)
    }

    /// Age of the last [`Self::beat`], or `None` if the host has never beaten.
    pub fn since_beat(&self) -> Option<Duration> {
        let beat = self.beat_ms.load(Ordering::Relaxed);
        if beat == 0 {
            return None;
        }
        let now = self.origin.elapsed().as_millis() as u64;
        Some(Duration::from_millis(now.saturating_sub(beat)))
    }
}

/// The host-facing name for the main-loop stamp — the one `e2e-conventions.md`
/// convention 11 and every host cite. It is a [`LivenessStamp`]; the alias
/// exists because the *same* mechanism now measures two different things (the
/// host's main loop, and the agent's own [`process_liveness`] watchdog), and a
/// stamp named for one of its two readings would misdescribe the other.
pub type UiThreadHeartbeat = LivenessStamp;

/// The agent's own liveness stamp, beaten by a plain OS thread that shares the
/// process with the UI thread but not its main loop.
///
/// This is the discriminator that turns a stale main-loop beat from a suspect
/// list into a verdict. Both stamps are read at the same instant, so:
///
/// * **stale loop, fresh process** — the process was scheduled and running; the
///   *main loop* specifically did not advance. Synchronous work on the UI
///   thread, which is convention 11's rule and a bug in our code.
/// * **stale loop, stale process** — nothing in this process ran, the agent's
///   own sleeping thread included. The box starved it of a core. Not a finding
///   about the app, and emphatically not a reason to raise the ack budget,
///   which convention 14 forbids.
///
/// It is agent-owned rather than a host-supplied hook precisely so no host has
/// to adopt it: the moment a host supplies a main-loop heartbeat, it gets the
/// split for free. One agent runs per process, so one stamp suffices.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
fn process_liveness() -> &'static Arc<LivenessStamp> {
    static PROCESS: OnceLock<Arc<LivenessStamp>> = OnceLock::new();
    PROCESS.get_or_init(|| {
        let stamp = LivenessStamp::new();
        // Beat once HERE, synchronously, before the thread exists: `get_or_init`
        // returns the instant it is called, and a stamp whose first beat waits
        // on a freshly spawned thread reads as "never beaten" until that thread
        // is scheduled. That window reports UNMEASURED for an op timing out
        // inside it — the honest answer, but an avoidable one, and it is widest
        // on exactly the loaded box this stamp exists to indict.
        stamp.beat();
        let beating = stamp.clone();
        // A detached daemon-style beater. It outlives nothing (the process owns
        // it) and costs two relaxed stores a second, on the same e2e-only path
        // as the agent server itself.
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(HEARTBEAT_CADENCE);
                beating.beat();
            }
        });
        stamp
    })
}

/// How stale a beat must be before it is unambiguously a stalled main loop.
///
/// Sized off the host's cadence, not off the reply budget: a host beating every
/// [`HEARTBEAT_CADENCE`] can miss several beats to ordinary scheduling jitter on
/// a loaded box without being stalled, but a loop that has not run for two whole
/// seconds is not jittering — at that point it is either blocked or starved, and
/// both are findings.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
const HEARTBEAT_STALL_FLOOR: Duration = Duration::from_secs(2);

/// The cadence a host should call [`UiThreadHeartbeat::beat`] at. Named here so
/// the floor above and the hosts agree on one number.
pub const HEARTBEAT_CADENCE: Duration = Duration::from_millis(250);

/// The verdict clause appended to a timeout, derived from the two stamps' ages.
///
/// A pure function of two ages, deliberately: it is the whole diagnosis a sweep
/// is triaged by, and it must be assertable without running a UI, a box under
/// load, or a clock (convention 14).
///
/// `loop_age` is the host's main-loop stamp; `process_age` is the agent's own
/// [`process_liveness`] watchdog. The second only ever *narrows* the first — a
/// running loop is a running loop whatever the box is doing — so it is read
/// only on the stale-loop branch, where it separates a blocked thread from a
/// starved process.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
fn thread_verdict(loop_age: Option<Duration>, process_age: Option<Duration>) -> String {
    match loop_age {
        None => {
            " — no UI-thread heartbeat on this host, so whether the thread is \
stalled or this op is slow is UNMEASURED (see UiThreadHeartbeat)"
        }
        .to_string(),
        Some(age) if age >= HEARTBEAT_STALL_FLOOR => {
            let cause = match process_age {
                // The agent's own thread kept beating, so the process was on a
                // core the whole time and the main loop still did not advance.
                // That is synchronous work on the UI thread and nothing else.
                Some(p) if p < HEARTBEAT_STALL_FLOOR => format!(
                    ": the process itself was RUNNING (agent thread beat {:.1}s ago), so this \
is synchronous work ON THE UI THREAD (e2e-conventions.md convention 11), not the box",
                    p.as_secs_f64()
                ),
                // Nothing in this process ran, the agent's sleeping thread
                // included. Nothing here indicts the app.
                Some(p) => format!(
                    ": the agent's own thread did not run either ({:.1}s ago), so the whole \
PROCESS was off-CPU — the box starved it, and this is not an app finding",
                    p.as_secs_f64()
                ),
                None => ": suspect synchronous work on it (e2e-conventions.md convention 11), \
or the machine starving it of a core"
                    .to_string(),
            };
            format!(
                " — the UI thread's main loop last ran {:.1}s ago, so it is NOT running{cause}",
                age.as_secs_f64()
            )
        }
        Some(age) => format!(
            " — the UI thread's main loop is alive (last ran {:.1}s ago), so it \
is not stalled: this op itself is what is slow",
            age.as_secs_f64()
        ),
    }
}

/// Does this pair of ages indict OUR code — a main loop that is not running
/// inside a process that is?
///
/// The same split [`thread_verdict`] words, as a predicate, because the verdict
/// is no longer only a sentence: it decides whether the agent goes on to sample
/// the thread's stack ([`stall::capture`]). Sampling the *other* stale case (a
/// box that starved the whole process) would attach a stack to a stall that is
/// not an app finding at all, and every reader of it would go hunting a
/// blocking call that does not exist.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
fn blocked_in_our_code(loop_age: Option<Duration>, process_age: Option<Duration>) -> bool {
    matches!(
        (loop_age, process_age),
        (Some(loop_age), Some(process_age))
            if loop_age >= HEARTBEAT_STALL_FLOOR && process_age < HEARTBEAT_STALL_FLOOR
    )
}

#[cfg(any(debug_assertions, feature = "e2e-agent"))]
thread_local! {
    /// The previous `/element/*` op this agent answered, and how long it took.
    static LAST_SERVED: std::cell::RefCell<Option<(String, Instant, Duration)>> =
        const { std::cell::RefCell::new(None) };
    /// The last command injected through `POST /app/commands`.
    static LAST_COMMAND: std::cell::RefCell<Option<(String, Instant)>> =
        const { std::cell::RefCell::new(None) };
}

/// The sentence naming what the agent served *before* the op that timed out.
///
/// Pure, so the wording is assertable without a stall. It exists because the
/// first reading of a stalled-loop sweep could not tell whether the stall began
/// *with* the timed-out op or was already running when it arrived — and those
/// accuse different code. A previous op answered in milliseconds a moment
/// earlier says the thread was healthy right up to this op; a command injected
/// seconds ago says to look at what that command started.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
fn served_clause(
    prev_op: Option<(&str, Duration, Duration)>,
    prev_command: Option<(&str, Duration)>,
) -> String {
    let mut out = String::new();
    match prev_op {
        Some((what, ago, took)) => out.push_str(&format!(
            " — the op before it ({what}) was answered {:.2}s earlier, in {:.2}s",
            ago.as_secs_f64(),
            took.as_secs_f64()
        )),
        None => out.push_str(" — this was the first op this agent served"),
    }
    if let Some((action, ago)) = prev_command {
        out.push_str(&format!(
            "; last command injected: {action}, {:.2}s ago",
            ago.as_secs_f64()
        ));
    }
    out
}

/// [`served_clause`] over what this server thread has actually served.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
fn served_clause_now() -> String {
    let now = Instant::now();
    let op = LAST_SERVED.with(|c| c.borrow().clone());
    let cmd = LAST_COMMAND.with(|c| c.borrow().clone());
    served_clause(
        op.as_ref()
            .map(|(what, at, took)| (what.as_str(), now.saturating_duration_since(*at), *took)),
        cmd.as_ref()
            .map(|(action, at)| (action.as_str(), now.saturating_duration_since(*at))),
    )
}

/// The app-specific half of the agent.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
pub struct AgentHooks {
    /// Forward an op to the client's UI thread (e.g. `send_blocking` on the
    /// GTK drain channel, `send` on a tokio mpsc). `Err(())` = channel closed.
    pub dispatch: Box<dyn Fn(ElementOp) -> Result<(), ()> + Send + Sync>,
    /// Serve `GET /app/state`: `{last_command_id, ready, state}`.
    pub app_state: Box<dyn Fn() -> Value + Send + Sync>,
    /// Handle `POST /app/commands`: inject the command toward the UI thread and
    /// return immediately (`{ok:true}` / `{error:…}`) — the driver polls
    /// `/app/state` for the ack.
    pub inject_command: Box<dyn Fn(&Value) -> Value + Send + Sync>,
    /// Optional liveness stamp from the host's main loop — see
    /// [`UiThreadHeartbeat`]. `None` leaves a timeout unclassified rather than
    /// guessing, so a host adopts this deliberately.
    pub heartbeat: Option<Arc<UiThreadHeartbeat>>,
}

/// Resolve a `/element/select` value against the options a frame actually
/// painted — the ONE matcher every app shares, so "which values does this
/// picker accept" cannot answer differently per app.
///
/// Returns the index of the matching option, or `None` when the frame never
/// offered it (convention 11's twin rule: the caller then refuses, naming
/// `options`, rather than actuating).
///
/// **Exact match first, normalized match second.** The cross-app suite drives
/// pickers with a *stable key* (`"this-device"`, `"BodyContains"`) while a
/// client paints a human, often localized, *label* (`"This device"`, `"Body
/// contains"`). Normalizing both to lowercase alphanumerics maps the key onto
/// its option without forcing every picker to paint machine strings at the
/// user. Exact match always wins, so genuinely distinct options can never be
/// conflated by normalization.
///
/// This is deliberately not "reachability, weakened": the user does not type
/// the value, they pick a row, and a normalized key resolves 1:1 onto a row the
/// frame really painted — so the state reached is one a user can reach, which is
/// the whole property the twin rule protects.
///
/// It lives here because it was linux-only and that asymmetry was invisible
/// until it bit: linux's `DropDown` lookup normalized, tui's registry did not
/// check at all, so the suite's key-driven selects passed on both — one by
/// matching, one by writing through. The moment tui started checking, it was
/// stricter than linux for no stated reason (caught by
/// `test_task_delegation.py`, whose `select(…, "this-device")` met tui's painted
/// `[Automatic, This device]`). One matcher, one answer, every app.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
pub fn select_match(options: &[String], value: &str) -> Option<usize> {
    if let Some(i) = options.iter().position(|o| o == value) {
        return Some(i);
    }
    let norm = |s: &str| -> String {
        s.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(|c| c.to_lowercase())
            .collect()
    };
    let target = norm(value);
    options.iter().position(|o| norm(o) == target)
}

/// Start the agent server on `127.0.0.1:port`, on its own thread. Each request
/// is forwarded through `hooks` and the reply awaited synchronously (the driver
/// is sequential, so a single-threaded blocking server is sufficient).
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
pub fn start(port: u16, hooks: AgentHooks) {
    // Arm the process watchdog HERE, not lazily at the first timeout: a stamp
    // first beaten at the moment it is read is always fresh, which would report
    // "the process was running" for every stall including the ones where it was
    // not. It must have been beating for the whole window it describes.
    let _ = process_liveness();
    std::thread::spawn(move || {
        let server = match tiny_http::Server::http(("127.0.0.1", port)) {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!("[agent] failed to bind 127.0.0.1:{port}: {e}");
                return;
            }
        };
        tracing::debug!("[agent] element server listening on 127.0.0.1:{port}");
        for mut request in server.incoming_requests() {
            let (status, body) = handle(&mut request, &hooks);
            let data = body.to_string();
            let header =
                tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
                    .expect("static header");
            let resp = tiny_http::Response::from_string(data)
                .with_status_code(status)
                .with_header(header);
            let _ = request.respond(resp);
        }
    });
}

#[cfg(any(debug_assertions, feature = "e2e-agent"))]
fn handle(request: &mut tiny_http::Request, hooks: &AgentHooks) -> (u16, Value) {
    let method = request.method().as_str().to_string();
    let url = request.url().to_string();
    let (path, query) = url.split_once('?').unwrap_or((url.as_str(), ""));
    let q = parse_query(query);

    let mut raw = String::new();
    let _ = request.as_reader().read_to_string(&mut raw);
    let body: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);

    let (id, index) = resolve_id_index(&q, &body);
    let scope = parse_scope(&q, &body);
    let str_field = |k: &str| -> String {
        body.get(k)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let attr_name = q.get("attr").cloned().unwrap_or_default();
    let req = |kind: ElementKind, arg: String| ElementReq {
        kind,
        id: id.clone(),
        index,
        scope: scope.clone(),
        arg,
    };

    let started = Instant::now();
    let served = match (method.as_str(), path) {
        ("GET", "/element/text") => element(hooks, req(ElementKind::Text, String::new())),
        ("GET", "/element/visible") => element(hooks, req(ElementKind::Visible, String::new())),
        ("GET", "/element/enabled") => element(hooks, req(ElementKind::Enabled, String::new())),
        ("GET", "/element/count") => element(hooks, req(ElementKind::Count, String::new())),
        ("GET", "/element/attr") => element(hooks, req(ElementKind::Attr, attr_name)),
        ("POST", "/element/click") => element(hooks, req(ElementKind::Click, String::new())),
        ("POST", "/element/double_click") => {
            element(hooks, req(ElementKind::DoubleClick, String::new()))
        }
        ("POST", "/element/type") => element(hooks, req(ElementKind::Type, str_field("text"))),
        ("POST", "/element/clear") => element(hooks, req(ElementKind::Clear, String::new())),
        ("POST", "/element/select") => element(hooks, req(ElementKind::Select, str_field("value"))),
        ("POST", "/element/scroll-into-view") => {
            element(hooks, req(ElementKind::ScrollIntoView, String::new()))
        }
        ("POST", "/element/key") => element(hooks, req(ElementKind::Key, str_field("key"))),
        ("POST", "/window/close") => element(hooks, req(ElementKind::WindowClose, String::new())),
        ("GET", "/registry") => element(hooks, req(ElementKind::Registry, String::new())),
        ("GET", "/clipboard/text") => {
            element(hooks, req(ElementKind::ClipboardText, String::new()))
        }
        // State protocol — the driver POSTs a command then polls /app/state.
        ("GET", "/app/state") => (200, (hooks.app_state)()),
        ("POST", "/app/commands") => {
            // Recorded at INJECTION, not at the ack: a command whose work wedges
            // the UI thread never acks, and it is exactly that command a later
            // timeout needs named (`served_clause`).
            let action = body
                .get("action")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_string();
            LAST_COMMAND.with(|c| *c.borrow_mut() = Some((action, Instant::now())));
            (200, (hooks.inject_command)(&body))
        }
        // Stubs so the driver's helpers don't 404: no pointer scrolling needed
        // (finds are global), no screenshots/tree in the in-process path.
        ("POST", "/scroll") => (200, json!({ "ok": true })),
        ("POST", "/dismiss-dialogs") => (200, json!({ "ok": true })),
        ("GET", "/tree") => (200, json!({ "tree": "" })),
        ("POST", "/screenshot") => (200, json!({ "path": "" })),
        ("GET", "/health") => (200, json!({ "ok": true })),
        _ => (
            404,
            json!({ "error": "unknown path", "method": method, "path": path }),
        ),
    };
    // Remember the element op just answered, so the NEXT one's timeout can say
    // whether the thread was healthy a moment ago.
    if path.starts_with("/element/") || path == "/window/close" {
        let what = if id.is_empty() {
            format!("{method} {path}")
        } else {
            format!("{method} {path} id={id}")
        };
        LAST_SERVED.with(|c| *c.borrow_mut() = Some((what, Instant::now(), started.elapsed())));
    }
    served
}

/// How long [`element`] waits for the UI thread's reply before giving up.
///
/// Sized to the band between two real bounds rather than picked by feel:
///
/// * **Floor — an awaited actuation can legitimately take many seconds.** A
///   click the agent awaits may drive several nest RPCs in sequence, and these
///   machines routinely run 20+ concurrent sessions at double-digit load
///   (testing.md § the brittle-test plea). The previous flat 10 s left no
///   headroom for that, so ordinary load could 504 a perfectly healthy click —
///   a false red that reads as a product fault.
/// * **Ceiling — the driver's own POST budget**
///   (`tests/e2e-unified/drivers/http_bridge.py::BRIDGE_RPC_TIMEOUT_S`, **120 s**
///   since 2026-08-02; it was 30 s when this constant was sized on 2026-07-30,
///   and the stale "30 s" stood here until 2026-09-02). Staying under it keeps
///   *this* timeout the one that fires, so a wedged UI thread reports the
///   self-diagnosing `504 agent timeout` below instead of an opaque socket
///   error on the Python side (testing.md § point 6 — failures diagnose
///   themselves).
///
/// ⚠ **The 95 s of headroom that correction opens up is NOT a lever — do not
/// spend it.** The arithmetic invites raising this constant, and a whole-suite
/// `--app linux` sweep on 2026-08-28 made the invitation concrete: 150 timeouts,
/// every one of them this budget expiring, spread evenly across all ten deciles
/// of a six-hour run. `e2e-conventions.md` convention 11 rules that case
/// directly — "prefer the app's own off-thread idiom … over a larger ack budget,
/// which convention 14 forbids anyway" — and prescribes the alternative:
/// *measure where the thread actually sits before believing a located
/// hypothesis*. [`UiThreadHeartbeat`] is that measurement, and the verdict it
/// appends to the message below is what a future session should read before it
/// touches this number.
///
/// A generous budget inside that band is free on green runs — `recv_timeout`
/// returns the instant the op folds, so the size only affects how long a genuine
/// wedge takes to report (testing.md § point 14, named generous budgets).
///
/// **This budget is deliberately NOT the lever for a click that can block on an
/// unreachable nest.** A single RPC to a dead nest burns the client-side spec
/// default of 30 s all by itself (`fauna-client::client::request_inner`; the
/// per-kind registry is not wired up in production — `fauna-protocol::kind`
/// § the `register_*_kinds` caveat) — longer than this whole budget, so such an
/// op 504s here however the number is set. Raising this constant cannot fix
/// that and would only trade a fast 504 for a slow one; such an op must not be
/// awaited behind a click at all — see `PageOp::outlives_click` in
/// `apps/fauna-tui/src/app.rs`.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
const AGENT_REPLY_BUDGET: Duration = Duration::from_secs(25);

/// Forward an element op to the UI thread and block for its reply. A reply
/// carrying an `error` key maps to HTTP 404 (so the driver's scroll-retry /
/// failure path fires); reads never carry `error` (they default), so they 200.
///
/// **A refusal that is not "no such element" says so with its own `status`.**
/// 404 is the right default — most refusals mean the id resolved to nothing, and
/// the driver's `_post_with_scroll` retry exists precisely for the lazily-rendered
/// case. But an op the agent refuses *about an element it found* must not wear
/// that code: the driver would scroll three times looking for a widget that is
/// already there, then raise `LookupError`, which reads as "not rendered yet"
/// rather than "you asked for something impossible". Such a reply adds
/// `"status": <code>` (e.g. tui's 409 for a select value the frame never painted)
/// and it is used verbatim. The key is stripped before the body goes out, so the
/// wire shape stays `{"error": ...}` for every consumer.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
fn element(hooks: &AgentHooks, req: ElementReq) -> (u16, Value) {
    let (reply_tx, reply_rx) = mpsc::channel();
    if (hooks.dispatch)(ElementOp {
        req,
        reply: reply_tx,
    })
    .is_err()
    {
        return (500, json!({ "error": "agent channel closed" }));
    }
    match reply_rx.recv_timeout(AGENT_REPLY_BUDGET) {
        Ok(mut v) => {
            let override_status = v
                .as_object_mut()
                .and_then(|o| o.remove("status"))
                .and_then(|s| s.as_u64())
                .and_then(|s| u16::try_from(s).ok());
            let status = match (v.get("error").is_some(), override_status) {
                // An `error` reply naming its own status uses it verbatim.
                (true, Some(code)) => code,
                (true, None) => 404,
                // A *read* never carries `error`; a stray `status` on one would
                // be a typo, and silently turning a successful read into a 4xx
                // is the kind of quiet miscarriage convention 11 forbids.
                (false, _) => 200,
            };
            (status, v)
        }
        // Name the budget in the message: the driver surfaces this `error`
        // string verbatim, and "agent timeout" alone gave no way to tell a
        // wedged UI thread from a budget set too low for the op (exactly the
        // ambiguity that mis-filed the factory-reset journeys as a product bug).
        // The verdict clause is the difference between a finding and a mystery:
        // 150 of these in one sweep said only that the thread was late, and the
        // sweep could not be triaged. `thread_verdict` names which half of
        // convention 11's search order to look in.
        Err(_) => {
            let loop_age = hooks.heartbeat.as_ref().and_then(|h| h.since_beat());
            let process_age = process_liveness().since_beat();
            let mut error = format!(
                "agent timeout — the UI thread did not reply within {}s{}{}",
                AGENT_REPLY_BUDGET.as_secs(),
                thread_verdict(loop_age, process_age),
                served_clause_now(),
            );
            // The stack is taken ONLY on the verdict that indicts our code, and
            // only while the thread is still stalled — which is now, not after
            // the reply. `stall` explains why this is safe to do to a live app.
            if blocked_in_our_code(loop_age, process_age) {
                let tid = hooks.heartbeat.as_ref().map_or(0, |h| h.beater_tid());
                error.push_str(&stall::capture(tid));
            }
            (504, json!({ "error": error }))
        }
    }
}

/// Resolve `id`/`index` from the query string first, falling back to the JSON
/// body — `handle()`'s query-overrides-body precedence (the query always wins
/// when present, even if the body also carries a value; an unparseable query
/// `index` falls back to the body rather than erroring).
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
fn resolve_id_index(q: &HashMap<String, String>, body: &Value) -> (String, usize) {
    let id = q
        .get("id")
        .cloned()
        .or_else(|| body.get("id").and_then(|v| v.as_str()).map(String::from))
        .unwrap_or_default();
    let index = q
        .get("index")
        .and_then(|s| s.parse().ok())
        .or_else(|| {
            body.get("index")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize)
        })
        .unwrap_or(0);
    (id, index)
}

/// Parse the scope path from the request: a JSON array of `{id, index}` — in
/// the body for POST, or the JSON-encoded `scope` query param for GET (matching
/// `http_bridge.py`). Empty / absent → no scope (global search).
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
fn parse_scope(q: &HashMap<String, String>, body: &Value) -> Vec<ScopeStep> {
    let arr = body
        .get("scope")
        .cloned()
        .or_else(|| q.get("scope").and_then(|s| serde_json::from_str(s).ok()));
    let Some(Value::Array(items)) = arr else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|it| {
            let id = it.get("id")?.as_str()?.to_string();
            let index = it.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            Some((id, index))
        })
        .collect()
}

/// Minimal `application/x-www-form-urlencoded` query parser with percent-decode
/// (handles `%5B`/`%5D` in scoped ids and `+`-as-space).
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
fn parse_query(query: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        map.insert(percent_decode(k), percent_decode(v));
    }
    map
}

#[cfg(any(debug_assertions, feature = "e2e-agent"))]
fn percent_decode(s: &str) -> String {
    fauna_core::web::percent_decode(s, true)
}

// ---------------------------------------------------------------------------
// The actuation gate — convention 11 one layer down
// ---------------------------------------------------------------------------
//
// `e2e-conventions.md` § convention 11: *an illegal command is the same failure
// one layer down*. The command table above owes honesty about commands it
// cannot honour; the `/element/*` ACTUATION routes owe the same about controls
// the UI has DISABLED. An in-process agent that resolves a widget and drives it
// without consulting its enabled state will happily activate something no user
// could reach — not a *dropped* command but an **illegal one silently
// honoured**, which presents identically to a product bug three steps later.
//
// apple completed this rollout 2026-08-05 and is the worked example
// (`apps/apple-e2e-automation.md` § The actuation gate). This module is the
// Rust half, shared by every direct-Rust host of this crate (linux, tui) so the
// refusal shape, the marker text and the staging flags are identical across
// them rather than reinvented per app (priority #2).

/// Environment opt-OUT of refusal, for a host whose gate is already the default
/// (tui, and linux since its 2026-09-10 sweep). Set by pytest's
/// `--permissive-actuation`.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
pub const PERMISSIVE_ACTUATION_ENV: &str = "FAUNA_E2E_PERMISSIVE_ACTUATION";

/// Environment opt-IN to refusal, for a host still STAGING its gate (none of
/// this crate's hosts today — linux flipped 2026-09-10). Kept for the next host
/// that stages, so it can be exercised strictly — by its own tier_1s and by its
/// probe e2e — without flipping the default for the fleet.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
pub const STRICT_ACTUATION_ENV: &str = "FAUNA_E2E_STRICT_ACTUATION";

/// Run-scoped file every launch appends its violation markers to. `app.err`
/// cannot serve this: it lives in the launch's temp dir and the `app` fixture
/// cold-relaunches at every module boundary (`helpers/module_relaunch.py`), so
/// stderr markers do not survive a sweep. Set by pytest's `--actuation-log`.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
pub const ACTUATION_LOG_ENV: &str = "FAUNA_E2E_ACTUATION_LOG";

/// The greppable marker a permissive-mode violation writes. Deliberately the
/// same text apple logs, so one grep spans every app's sweep.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
pub const DISABLED_ACTUATION_MARKER: &str = "DISABLED-ACTUATION";

/// Is refusal ON for this process? `default_strict` is the HOST's stance, so a
/// host that already refuses (tui) and one still staging (linux) share every
/// line below and differ in one bool.
///
/// Permissive wins when both are set: the enumerating run must never be the run
/// that turns red, or the measurement is lost along with the offender list.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
pub fn strict_actuation(default_strict: bool) -> bool {
    if std::env::var_os(PERMISSIVE_ACTUATION_ENV).is_some_and(|v| !v.is_empty()) {
        return false;
    }
    if std::env::var_os(STRICT_ACTUATION_ENV).is_some_and(|v| !v.is_empty()) {
        return true;
    }
    default_strict
}

/// The marker line one violation writes, in both the log file and the app's
/// stderr. Pure, so the format is pinned without touching a file or the env.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
pub fn disabled_actuation_marker(route: &str, id: &str, index: usize) -> String {
    format!("[InProcessAutomation] {DISABLED_ACTUATION_MARKER} {route} id={id} index={index}")
}

/// The refusal body for driving a control the UI has disabled.
///
/// **409, never 404** — the element WAS found; its state is the problem. 404
/// sends `drivers/http_bridge.py` into its scroll-retry loop and the test dies
/// on a `LookupError` reading "not rendered yet", which is the opposite
/// diagnosis. The message names the element, the index and the route so the
/// failure diagnoses itself (convention 6) and one grep finds every instance.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
pub fn disabled_actuation_refusal(route: &str, id: &str, index: usize) -> Value {
    json!({
        "error": format!(
            "element is disabled: {id}[{index}] — {route} refused (convention 11: an \
             actuation route must not drive a control the UI has disabled)"
        ),
        "status": 409,
        "id": id,
        "index": index,
    })
}

/// The gate's decision, with no I/O of any kind: `Some(refusal)` to refuse the
/// actuation, `None` to let it proceed.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
pub fn actuation_verdict(
    route: &str,
    id: &str,
    index: usize,
    enabled: bool,
    strict: bool,
) -> Option<Value> {
    match (enabled, strict) {
        (true, _) => None,
        (false, true) => Some(disabled_actuation_refusal(route, id, index)),
        // Permissive: the control is still driven — that is what makes ONE sweep
        // enumerate EVERY offender. A strict sweep reds its test, and a red test
        // stops, so it reports at most the first offender per test and hides the
        // rest (`e2e-conventions.md` § convention 11).
        (false, false) => None,
    }
}

/// Append one marker to `path`, best-effort.
///
/// Best-effort is deliberate and load-bearing: an unwritable sink must never
/// fail the app under test. The measurement is worth less than the run.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
pub fn append_actuation_marker(path: &std::path::Path, line: &str) {
    use std::io::Write as _;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{line}");
    }
}

/// The one entry point an in-process agent calls before actuating: record any
/// violation and answer whether the actuation may proceed. `Some(refusal)` —
/// reply it verbatim, it carries its own 409 — or `None`, meaning drive it.
///
/// Call it AFTER resolving the element and BEFORE actuating, on every route
/// that drives a control: click, double-click, type, clear, select. Gating
/// click alone is not compliance — typing into a disabled field is the same
/// illegal act, and a half-applied gate leaves the hole open for the next
/// session to rediscover. Read routes (`text`/`visible`/`count`/`enabled`/
/// `attr`) must NOT be gated: reading a disabled control is exactly how a test
/// asserts that it *is* disabled. `scroll-into-view` is viewport positioning,
/// not actuation, and stays ungated for the same reason.
///
/// `enabled` must be read LIVE, per request. A predicate captured once at
/// registration refuses a control that has since enabled — worse than not
/// gating at all, because it breaks working tests rather than fake ones.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
pub fn gate_actuation(
    route: &str,
    id: &str,
    index: usize,
    enabled: bool,
    default_strict: bool,
) -> Option<Value> {
    if enabled {
        return None;
    }
    let line = disabled_actuation_marker(route, id, index);
    // `error!` rather than `debug!`: this reaches the app's captured stderr,
    // which for linux is `app.err` (`drivers/linux.py::app_stderr_text`) at the
    // harness's `RUST_LOG=info`. A `debug!` here would be the very shape
    // convention 11 forbids — a violation recorded where nothing reads it.
    tracing::error!("{line}");
    if let Some(path) = std::env::var_os(ACTUATION_LOG_ENV) {
        append_actuation_marker(std::path::Path::new(&path), &line);
    }
    actuation_verdict(route, id, index, enabled, strict_actuation(default_strict))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The happy shapes the walk runner actually sends, and the `times` default.
    ///
    /// An ABSENT `times` means one step — the walk runner's own step size, and
    /// the shape `tests/test_tui_settings_focus.py` has always sent.
    #[test]
    fn focus_move_parses_both_directions_and_defaults_to_one_step() {
        assert_eq!(
            focus_move_request(&json!({ "direction": "next", "times": 3 })),
            Ok((FocusDirection::Next, 3))
        );
        assert_eq!(
            focus_move_request(&json!({ "direction": "prev", "times": 1 })),
            Ok((FocusDirection::Prev, 1))
        );
        assert_eq!(
            focus_move_request(&json!({ "direction": "next" })),
            Ok((FocusDirection::Next, 1)),
            "an absent `times` is one step, not a refusal"
        );
    }

    /// A malformed request is a **named refusal**, never a silent fallback.
    ///
    /// The `times: "3"` case is the one that matters and the one that changed:
    /// a `.as_u64().unwrap_or(1)` reading — which is what tui shipped before
    /// this contract existed — turns a caller's typo into a *different, legal*
    /// command that acks green. That is convention 11's silent drop wearing the
    /// only disguise it has left once the action name is recognized: the command
    /// ran, just not the one that was asked for.
    #[test]
    fn a_malformed_focus_move_is_refused_by_name_not_silently_defaulted() {
        for (payload, want) in [
            (json!({}), "direction"),
            (json!({ "direction": "sideways" }), "direction"),
            (json!({ "direction": 1 }), "direction"),
            (json!({ "direction": "next", "times": "3" }), "times"),
            (json!({ "direction": "next", "times": -1 }), "times"),
            (json!({ "direction": "next", "times": 1.5 }), "times"),
        ] {
            let err = focus_move_request(&payload)
                .expect_err(&format!("{payload} must be refused, not defaulted"));
            assert!(
                err.contains(FOCUS_MOVE) && err.contains(want),
                "a refusal must name the command and the offending field so the \
                 app's own `error-message` diagnoses itself (convention 6); got {err:?}"
            );
        }
    }

    /// An unbounded `times` is a UI-thread stall, and a stall is unackable.
    ///
    /// Convention 11's second corollary is that nothing may block the thread
    /// serving the agent: every app runs this loop on its UI thread, so a walk
    /// bug asking for a billion steps does not produce a slow `focus_move`, it
    /// produces an app on which *every* command times out — the disguise that
    /// cost three investigations on windows. Refusing above the cap keeps that
    /// failure attributable to the caller that caused it. ⚠ Do NOT "fix" this by
    /// clamping instead: a silent clamp runs a command nobody asked for, which
    /// is the very thing the test above pins.
    #[test]
    fn focus_move_refuses_a_step_count_that_would_stall_the_ui_thread() {
        assert_eq!(
            focus_move_request(&json!({ "direction": "next", "times": FOCUS_MOVE_MAX_TIMES })),
            Ok((FocusDirection::Next, FOCUS_MOVE_MAX_TIMES)),
            "the cap itself is legal — the bound is inclusive"
        );
        let err = focus_move_request(&json!({
            "direction": "next",
            "times": FOCUS_MOVE_MAX_TIMES + 1,
        }))
        .expect_err("above the cap is a refusal");
        assert!(
            err.contains("times") && err.contains(&FOCUS_MOVE_MAX_TIMES.to_string()),
            "the refusal must name the bound it enforced; got {err:?}"
        );
    }

    /// `times: 0` is a legal no-op, deliberately.
    ///
    /// A walk runner computes its step count, and a computed zero is a real
    /// answer ("nothing to advance past"). Refusing it would make every caller
    /// branch around the degenerate case, and a no-op that acks is honest: the
    /// command was honoured, it simply had nothing to do.
    #[test]
    fn zero_steps_is_a_legal_no_op_not_a_refusal() {
        assert_eq!(
            focus_move_request(&json!({ "direction": "next", "times": 0 })),
            Ok((FocusDirection::Next, 0))
        );
    }

    /// The pane vocabulary, and its refusal.
    #[test]
    fn switch_pane_parses_the_two_panes_and_refuses_anything_else() {
        assert_eq!(
            switch_pane_target(&json!({ "pane": "page" })),
            Ok(Pane::Page)
        );
        assert_eq!(
            switch_pane_target(&json!({ "pane": "sidebar" })),
            Ok(Pane::Sidebar)
        );
        for bad in [json!({}), json!({ "pane": "middle" }), json!({ "pane": 2 })] {
            let err = switch_pane_target(&bad).expect_err(&format!("{bad} must be refused"));
            assert!(
                err.contains(SWITCH_PANE) && err.contains("pane"),
                "the refusal must name the command and the field; got {err:?}"
            );
        }
    }

    /// The two names are the wire strings every app and driver already spell.
    ///
    /// Pinned because the whole point of hoisting them here is that the six apps
    /// stop spelling them by hand: if a rename ever lands, this is the test that
    /// makes it a deliberate, fleet-wide act rather than a per-app drift.
    #[test]
    fn the_walk_command_names_are_the_wire_strings() {
        assert_eq!(FOCUS_MOVE, "focus_move");
        assert_eq!(SWITCH_PANE, "switch_pane");
    }

    /// The gate both Rust apps hang their barrier seam on. A bare `barrier`
    /// always barriers; a `barrier_probe` barriers only when fused.
    #[test]
    fn command_needs_barrier_covers_the_bare_and_the_fused_form() {
        assert!(command_needs_barrier(BARRIER, &Value::Null));
        assert!(command_needs_barrier(BARRIER, &json!({})));
        assert!(command_needs_barrier(
            BARRIER_PROBE,
            &json!({ "token": "t", BARRIER_PROBE_FUSE_FIELD: true })
        ));
        // The un-fused probe must NOT barrier — its early ack is the asymmetry
        // the whole self-test rests on.
        assert!(!command_needs_barrier(
            BARRIER_PROBE,
            &json!({ "token": "t" })
        ));
        assert!(!command_needs_barrier(
            BARRIER_PROBE,
            &json!({ "token": "t", BARRIER_PROBE_FUSE_FIELD: false })
        ));
        assert!(!command_needs_barrier("some_other_command", &json!({})));
    }

    /// A non-boolean flag reads as absent rather than as truthy. The fused form
    /// is what *grades* the barrier, so a malformed payload must fall back to
    /// the shape that fails loudly (an un-barriered ack reds the assertion),
    /// never to a silent "close enough".
    #[test]
    fn a_non_boolean_fuse_flag_is_not_fused() {
        for bad in [json!("true"), json!(1), json!(null), json!({})] {
            assert!(
                !barrier_probe_is_fused(&json!({ BARRIER_PROBE_FUSE_FIELD: bad })),
                "only a real `true` fuses the probe"
            );
        }
        assert!(!barrier_probe_is_fused(&Value::Null));
    }

    #[test]
    fn percent_decode_handles_scoped_ids() {
        assert_eq!(percent_decode("post-card%5B2%5D"), "post-card[2]");
        assert_eq!(percent_decode("a+b"), "a b");
    }

    #[test]
    fn percent_decode_passes_through_incomplete_or_invalid_escapes() {
        // Not enough trailing bytes for a full `%XX` escape — the `%` is emitted
        // literally and scanning resumes at the next byte.
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("100%2"), "100%2");
        // Two full trailing bytes but non-hex digits — same literal passthrough.
        assert_eq!(percent_decode("100%zz"), "100%zz");
    }

    #[test]
    fn parse_scope_reads_body_and_query() {
        let body = json!({ "scope": [{ "id": "post-card", "index": 2 }] });
        let q = HashMap::new();
        assert_eq!(
            parse_scope(&q, &body),
            vec![("post-card".to_string(), 2usize)]
        );

        let mut q = HashMap::new();
        q.insert(
            "scope".to_string(),
            r#"[{"id":"row","index":1},{"id":"cell"}]"#.to_string(),
        );
        assert_eq!(
            parse_scope(&q, &Value::Null),
            vec![("row".to_string(), 1usize), ("cell".to_string(), 0usize)]
        );
    }

    #[test]
    fn parse_scope_is_empty_when_absent_from_both_body_and_query() {
        let q = HashMap::new();
        assert_eq!(parse_scope(&q, &Value::Null), Vec::<ScopeStep>::new());
        assert_eq!(parse_scope(&q, &json!({})), Vec::<ScopeStep>::new());
    }

    #[test]
    fn parse_scope_falls_back_to_empty_on_malformed_query_json() {
        let mut q = HashMap::new();
        q.insert("scope".to_string(), "not valid json".to_string());
        assert_eq!(parse_scope(&q, &Value::Null), Vec::<ScopeStep>::new());
    }

    #[test]
    fn parse_scope_drops_items_missing_an_id() {
        let body = json!({ "scope": [{ "index": 2 }, { "id": "row" }] });
        let q = HashMap::new();
        assert_eq!(parse_scope(&q, &body), vec![("row".to_string(), 0usize)]);
    }

    #[test]
    fn parse_query_handles_multiple_pairs_bare_keys_and_empty_values() {
        let q = parse_query("a=1&b=2&bare&c=");
        assert_eq!(q.get("a"), Some(&"1".to_string()));
        assert_eq!(q.get("b"), Some(&"2".to_string()));
        assert_eq!(q.get("bare"), Some(&"".to_string()));
        assert_eq!(q.get("c"), Some(&"".to_string()));
        assert!(parse_query("").is_empty());
    }

    #[test]
    fn parse_query_percent_decodes_both_key_and_value() {
        let q = parse_query("post-card%5B2%5D=a+b");
        assert_eq!(q.get("post-card[2]"), Some(&"a b".to_string()));
    }

    #[test]
    fn resolve_id_index_prefers_query_over_body() {
        let mut q = HashMap::new();
        q.insert("id".to_string(), "from-query".to_string());
        q.insert("index".to_string(), "3".to_string());
        let body = json!({ "id": "from-body", "index": 9 });
        assert_eq!(resolve_id_index(&q, &body), ("from-query".to_string(), 3));
    }

    #[test]
    fn resolve_id_index_falls_back_to_body_when_query_absent() {
        let q = HashMap::new();
        let body = json!({ "id": "from-body", "index": 9 });
        assert_eq!(resolve_id_index(&q, &body), ("from-body".to_string(), 9));
    }

    #[test]
    fn resolve_id_index_defaults_when_both_absent() {
        let q = HashMap::new();
        assert_eq!(resolve_id_index(&q, &Value::Null), (String::new(), 0));
    }

    #[test]
    fn resolve_id_index_falls_back_to_body_when_query_index_unparseable() {
        let mut q = HashMap::new();
        q.insert("index".to_string(), "not-a-number".to_string());
        let body = json!({ "index": 5 });
        assert_eq!(resolve_id_index(&q, &body), (String::new(), 5));
    }

    fn stub_hooks(
        dispatch: impl Fn(ElementOp) -> Result<(), ()> + Send + Sync + 'static,
    ) -> AgentHooks {
        AgentHooks {
            dispatch: Box::new(dispatch),
            app_state: Box::new(|| Value::Null),
            inject_command: Box::new(|_| Value::Null),
            heartbeat: None,
        }
    }

    fn stub_req() -> ElementReq {
        ElementReq {
            kind: ElementKind::Text,
            id: "some-id".to_string(),
            index: 0,
            scope: Vec::new(),
            arg: String::new(),
        }
    }

    #[test]
    fn element_maps_dispatch_err_to_500() {
        let hooks = stub_hooks(|_op| Err(()));
        let (status, body) = element(&hooks, stub_req());
        assert_eq!(status, 500);
        assert_eq!(body, json!({ "error": "agent channel closed" }));
    }

    // --- UI-thread heartbeat: the timeout's verdict ------------------------
    //
    // These pin the CLASSIFICATION, not the wording, and none of them sleeps or
    // spawns: `thread_verdict` is a pure function of two ages, exactly so every
    // branch is testable without waiting out a 25s budget or loading a box
    // (convention 14 — assert latency-independent state).

    /// A process stamp that is beating normally — the ordinary case, so a test
    /// naming only the loop's age is not silently also asserting a starved box.
    const PROCESS_ALIVE: Option<Duration> = Some(Duration::from_millis(250));
    /// A main loop that has not run for well over the floor. Kept above it by
    /// `the_test_ages_straddle_the_floor` rather than by arithmetic, since
    /// `Duration`'s `+` is not const.
    const STALE: Option<Duration> = Some(Duration::from_secs(32));
    /// A main loop that ran one ordinary cadence ago.
    const FRESH: Option<Duration> = Some(Duration::from_millis(300));

    #[test]
    fn a_host_with_no_heartbeat_reports_the_verdict_as_unmeasured() {
        // The dangerous failure would be silently calling an unmeasured thread
        // "stalled": every non-adopting host would then read as a finding.
        let verdict = thread_verdict(None, PROCESS_ALIVE);
        assert!(verdict.contains("UNMEASURED"), "{verdict}");
        assert!(!verdict.contains("NOT running"), "{verdict}");
    }

    #[test]
    fn a_stale_beat_says_the_main_loop_is_not_running() {
        let verdict = thread_verdict(STALE, PROCESS_ALIVE);
        assert!(verdict.contains("NOT running"), "{verdict}");
        // The age itself must survive into the message — "stalled" without a
        // number cannot be compared across a sweep's timeouts.
        assert!(verdict.contains("32.0s"), "{verdict}");
    }

    #[test]
    fn a_fresh_beat_says_the_op_is_slow_not_the_thread() {
        let verdict = thread_verdict(FRESH, PROCESS_ALIVE);
        assert!(verdict.contains("is alive"), "{verdict}");
        assert!(verdict.contains("this op itself"), "{verdict}");
        assert!(!verdict.contains("NOT running"), "{verdict}");
    }

    // --- the process watchdog: which of the two suspects it was -------------
    //
    // The split these four pin is the entire reason the second stamp exists. A
    // stale main loop had two suspects and named both, so 304 timeouts in one
    // sweep were untriageable; the
    // process stamp decides between them.

    #[test]
    fn a_stalled_loop_in_a_running_process_indicts_the_ui_thread() {
        // The finding case: the agent's own thread kept running, so the box was
        // giving this process CPU and the main loop still did not advance.
        let verdict = thread_verdict(STALE, PROCESS_ALIVE);
        assert!(verdict.contains("NOT running"), "{verdict}");
        assert!(verdict.contains("process itself was RUNNING"), "{verdict}");
        assert!(verdict.contains("ON THE UI THREAD"), "{verdict}");
        // It must not hedge back toward the box once it has ruled it out — the
        // hedge is what made the old message untriageable.
        assert!(!verdict.contains("starving"), "{verdict}");
    }

    #[test]
    fn a_stalled_loop_in_a_stalled_process_indicts_the_box_not_the_app() {
        // The NOT-a-finding case. Reporting this as app-side synchronous work
        // would send a session hunting a blocking call that does not exist —
        // and the wrong fix for it (a larger ack budget) is one convention 14
        // forbids outright.
        let verdict = thread_verdict(STALE, Some(HEARTBEAT_STALL_FLOOR + Duration::from_secs(8)));
        assert!(verdict.contains("NOT running"), "{verdict}");
        assert!(verdict.contains("whole"), "{verdict}");
        assert!(verdict.contains("off-CPU"), "{verdict}");
        assert!(verdict.contains("not an app finding"), "{verdict}");
        assert!(!verdict.contains("ON THE UI THREAD"), "{verdict}");
    }

    #[test]
    fn a_live_loop_is_never_reclassified_by_the_process_stamp() {
        // The process stamp only ever NARROWS a stale-loop verdict. A loop that
        // is demonstrably running is running whatever the box is doing, and a
        // starved-process clause here would contradict the beat we just read.
        let verdict = thread_verdict(FRESH, Some(HEARTBEAT_STALL_FLOOR + Duration::from_secs(8)));
        assert!(verdict.contains("is alive"), "{verdict}");
        assert!(!verdict.contains("off-CPU"), "{verdict}");
        assert!(!verdict.contains("NOT running"), "{verdict}");
    }

    #[test]
    fn an_unmeasured_process_keeps_the_old_two_suspect_wording() {
        // Absent the second stamp the honest answer is the ambiguous one. A
        // missing measurement must never be upgraded into a verdict — the same
        // rule the `None` loop-age branch has always followed.
        let verdict = thread_verdict(STALE, None);
        assert!(verdict.contains("NOT running"), "{verdict}");
        assert!(verdict.contains("convention 11"), "{verdict}");
        assert!(verdict.contains("starving"), "{verdict}");
        assert!(!verdict.contains("process itself was RUNNING"), "{verdict}");
    }

    #[test]
    fn the_process_watchdog_is_already_beating_before_any_op_can_time_out() {
        // Armed in `start`, not lazily at the first read: a stamp first beaten
        // at the moment it is read is always fresh, so it would report "the
        // process was running" for every stall — including the starved ones it
        // exists to identify. Reading it twice here is the closest a pure test
        // can get to "it was beating for the window it describes".
        let first = process_liveness().since_beat();
        assert!(
            first.is_some(),
            "the watchdog must beat without waiting to be asked"
        );
        assert!(
            Arc::ptr_eq(process_liveness(), process_liveness()),
            "one process, one stamp — a second would measure a different thread"
        );
    }

    // --- the stack capture: which verdict earns one ------------------------

    #[test]
    fn only_the_ui_thread_verdict_asks_for_a_stack() {
        // The predicate and the sentence must agree, always: a stack attached
        // to the starved-box verdict would send a reader hunting a blocking
        // call that does not exist, and no stack on the app-side verdict is the
        // 110-unattributable-stalls state this whole mechanism exists to end.
        for (loop_age, process_age) in [
            (STALE, PROCESS_ALIVE),
            (STALE, Some(HEARTBEAT_STALL_FLOOR + Duration::from_secs(8))),
            (STALE, None),
            (FRESH, PROCESS_ALIVE),
            (None, PROCESS_ALIVE),
        ] {
            let says_ui_thread = thread_verdict(loop_age, process_age).contains("ON THE UI THREAD");
            assert_eq!(
                blocked_in_our_code(loop_age, process_age),
                says_ui_thread,
                "verdict and capture-predicate disagree for {loop_age:?}/{process_age:?}"
            );
        }
    }

    #[test]
    fn a_beat_records_the_thread_that_beat_it() {
        // The tid is what turns "the loop is stalled" into "here is the stack":
        // the thread that last advanced the loop is the one now holding it.
        let hb = UiThreadHeartbeat::new();
        assert_eq!(hb.beater_tid(), 0, "an unbeaten stamp names no thread");
        hb.beat();
        if cfg!(target_os = "linux") {
            assert!(hb.beater_tid() > 0, "a beat must name its own thread");
        }
    }

    // --- what the agent served before the op that timed out ----------------

    #[test]
    fn the_timeout_names_the_previous_op_and_how_long_ago_it_was() {
        let clause = served_clause(
            Some((
                "GET /element/visible id=feed-list",
                Duration::from_millis(310),
                Duration::from_millis(12),
            )),
            None,
        );
        assert!(clause.contains("feed-list"), "{clause}");
        assert!(clause.contains("0.31s earlier"), "{clause}");
        assert!(clause.contains("0.01s"), "{clause}");
    }

    #[test]
    fn the_timeout_names_the_last_command_when_there_was_one() {
        let clause = served_clause(None, Some(("set_state", Duration::from_millis(4200))));
        // "First op" matters as much as the age: it says the stall cannot be
        // blamed on an op this agent served, because it served none.
        assert!(clause.contains("first op"), "{clause}");
        assert!(clause.contains("set_state"), "{clause}");
        assert!(clause.contains("4.20s ago"), "{clause}");
    }

    #[test]
    fn the_test_ages_straddle_the_floor() {
        // Every verdict test above is named for a branch; if these two drifted
        // onto the same side of the floor the tests would still pass while
        // asserting nothing about the split.
        assert!(
            STALE.unwrap() >= HEARTBEAT_STALL_FLOOR,
            "STALE must be stale"
        );
        assert!(
            FRESH.unwrap() < HEARTBEAT_STALL_FLOOR,
            "FRESH must be fresh"
        );
        assert!(
            PROCESS_ALIVE.unwrap() < HEARTBEAT_STALL_FLOOR,
            "PROCESS_ALIVE must read as a running process"
        );
    }

    #[test]
    fn the_stall_floor_is_several_beats_wide() {
        // The floor exists to absorb scheduling jitter on a loaded box. A floor
        // at or below one cadence would call an ordinarily-late tick a stall,
        // which is the false-finding this whole mechanism exists to avoid.
        assert!(
            HEARTBEAT_STALL_FLOOR >= HEARTBEAT_CADENCE * 4,
            "floor {HEARTBEAT_STALL_FLOOR:?} must be several {HEARTBEAT_CADENCE:?} beats wide"
        );
    }

    #[test]
    fn an_unbeaten_heartbeat_reads_as_absent_and_a_beaten_one_does_not() {
        let hb = UiThreadHeartbeat::new();
        assert!(
            hb.since_beat().is_none(),
            "a host that never beat must read as unmeasured"
        );
        hb.beat();
        // The `max(1)` sentinel guard: a beat landing at elapsed==0ms must not
        // read as "never beaten".
        assert!(
            hb.since_beat().is_some(),
            "a beat at elapsed 0ms still counts as a beat"
        );
    }

    #[test]
    fn element_maps_error_reply_to_404() {
        let hooks = stub_hooks(|op: ElementOp| {
            let _ = op.reply.send(json!({ "error": "not found" }));
            Ok(())
        });
        let (status, body) = element(&hooks, stub_req());
        assert_eq!(status, 404);
        assert_eq!(body, json!({ "error": "not found" }));
    }

    #[test]
    fn element_maps_non_error_reply_to_200() {
        let hooks = stub_hooks(|op: ElementOp| {
            let _ = op.reply.send(json!({ "text": "hello" }));
            Ok(())
        });
        let (status, body) = element(&hooks, stub_req());
        assert_eq!(status, 200);
        assert_eq!(body, json!({ "text": "hello" }));
    }

    /// A refusal *about an element that was found* names its own status, so the
    /// driver doesn't read it as "not rendered yet" and scroll-retry three times
    /// (tui's select-not-offered, 409).
    #[test]
    fn element_error_reply_may_name_its_own_status() {
        let hooks = stub_hooks(|op: ElementOp| {
            let _ = op
                .reply
                .send(json!({ "error": "not offered", "status": 409 }));
            Ok(())
        });
        let (status, body) = element(&hooks, stub_req());
        assert_eq!(status, 409);
        // `status` is transport, not payload: it never reaches the wire, so
        // every consumer still sees the plain `{"error": ...}` shape.
        assert_eq!(body, json!({ "error": "not offered" }));
    }

    fn opts(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn select_match_prefers_an_exact_option() {
        let options = opts(&["__all__", "photos", "docs"]);
        assert_eq!(select_match(&options, "photos"), Some(1));
        assert_eq!(select_match(&options, "__all__"), Some(0));
    }

    /// The suite drives stable keys; clients paint labels. `"this-device"` must
    /// find `"This device"` — the case that made tui stricter than linux until
    /// this matcher was shared (`test_task_delegation.py`).
    #[test]
    fn select_match_maps_a_stable_key_onto_its_painted_label() {
        let options = opts(&["Automatic", "This device"]);
        assert_eq!(select_match(&options, "this-device"), Some(1));
        assert_eq!(select_match(&options, "BodyContains"), None);
        assert_eq!(
            select_match(&opts(&["Body contains"]), "BodyContains"),
            Some(0)
        );
    }

    /// **The fallback's ceiling, pinned deliberately: normalization bridges a
    /// key to its label only when the label IS that key respaced.** A label
    /// carrying anything more — a descriptive suffix, a qualifying prefix —
    /// is out of reach, and no amount of loosening could recover it: nothing
    /// derives `"Source"` from `"Protocol Source"`. So a client that paints
    /// such labels must carry the **wire value in its option model**, which is
    /// the ratified cross-app shape (web's `<option value>`, tui's
    /// `SelectTarget::RuleType`, linux's value-model + display expression;
    /// `feed.md` § Where logic lives — "the select's value stays the
    /// `FilterRule` variant name … only the label is localized").
    ///
    /// This is a **contract boundary, not a gap to close.** Widening the
    /// matcher (substring, prefix, fuzzy) to swallow these would give up the
    /// property the test below pins — that genuinely distinct options can
    /// never be conflated — and would silently actuate the wrong row. The
    /// three cases here are real: they are what made
    /// `test_create_feed_label_below_rule[linux]` a standing red until linux's
    /// rule-type model was moved onto wire values (2026-08-13).
    #[test]
    fn select_match_cannot_reach_a_label_carrying_text_beyond_its_key() {
        assert_eq!(
            select_match(&opts(&["Label Below (exclude spam)"]), "LabelBelow"),
            None,
        );
        assert_eq!(
            select_match(&opts(&["Label Above (show only)"]), "LabelAbove"),
            None,
        );
        assert_eq!(select_match(&opts(&["Protocol Source"]), "Source"), None);
        // The same client painting the wire values resolves all three — the
        // fix this boundary points at.
        let values = opts(&["LabelBelow", "LabelAbove", "Source"]);
        assert_eq!(select_match(&values, "LabelBelow"), Some(0));
        assert_eq!(select_match(&values, "LabelAbove"), Some(1));
        assert_eq!(select_match(&values, "Source"), Some(2));
    }

    /// Exact ALWAYS wins, so normalization can never conflate two options that
    /// are genuinely distinct — the property that makes the fallback safe.
    #[test]
    fn select_match_never_lets_normalization_beat_an_exact_option() {
        // "ab-c" normalizes to the same key as "abc", and both are offered.
        let options = opts(&["abc", "ab-c"]);
        assert_eq!(select_match(&options, "ab-c"), Some(1), "exact match wins");
        assert_eq!(select_match(&options, "abc"), Some(0), "exact match wins");
    }

    #[test]
    fn select_match_refuses_a_value_no_option_resolves_to() {
        assert_eq!(
            select_match(&opts(&["Automatic", "This device"]), "nope"),
            None
        );
        assert_eq!(select_match(&[], "anything"), None);
    }

    /// A `status` on a *successful* read is a typo, never an instruction —
    /// silently 4xx-ing a good read is exactly the quiet miscarriage the
    /// test-agent contract forbids.
    #[test]
    fn element_ignores_status_on_a_successful_read() {
        let hooks = stub_hooks(|op: ElementOp| {
            let _ = op.reply.send(json!({ "text": "hello", "status": 409 }));
            Ok(())
        });
        let (status, body) = element(&hooks, stub_req());
        assert_eq!(status, 200);
        assert_eq!(body, json!({ "text": "hello" }));
    }

    // --- the actuation gate ------------------------------------------------
    //
    // The decision is pinned through the PURE entry points, never by mutating
    // the process env: these run under libtest's thread fan-out, and an env
    // write is process-global, so an env-mutating pin would make its own
    // neighbours flaky — the exact class convention 14 exists to forbid.

    #[test]
    fn an_enabled_control_is_never_refused_in_either_mode() {
        assert_eq!(actuation_verdict("click", "save-btn", 0, true, true), None);
        assert_eq!(actuation_verdict("click", "save-btn", 0, true, false), None);
    }

    /// The refusal must be readable ON ITS OWN, because the driver surfaces the
    /// string verbatim and nothing else survives to the test author.
    #[test]
    fn a_strict_refusal_is_a_409_naming_element_index_and_route() {
        let refusal = actuation_verdict("type", "restore-confirm-input", 2, false, true)
            .expect("a disabled control must be refused in strict mode");
        assert_eq!(
            refusal["status"], 409,
            "404 would send the driver into its scroll-retry loop and report 'not rendered yet'"
        );
        assert_eq!(refusal["id"], "restore-confirm-input");
        assert_eq!(refusal["index"], 2);
        let msg = refusal["error"].as_str().unwrap();
        assert!(msg.contains("restore-confirm-input[2]"), "{msg}");
        assert!(msg.contains("type"), "{msg}");
        assert!(msg.contains("element is disabled"), "{msg}");
    }

    /// Permissive still DRIVES. This is the half that is easy to get backwards,
    /// and getting it backwards costs the whole measurement: a strict sweep
    /// stops at the first offender per test and hides every later one.
    #[test]
    fn permissive_mode_lets_a_disabled_control_through() {
        assert_eq!(
            actuation_verdict("click", "restore-confirm-button", 0, false, false),
            None
        );
    }

    #[test]
    fn the_marker_names_route_id_and_index_greppably() {
        assert_eq!(
            disabled_actuation_marker("select", "kind-select", 1),
            "[InProcessAutomation] DISABLED-ACTUATION select id=kind-select index=1"
        );
    }

    /// One sweep appends many markers from many launches, so the sink must
    /// APPEND rather than truncate — a truncating sink would report only the
    /// last launch's violations while reading like a complete enumeration.
    #[test]
    fn the_log_sink_appends_across_launches() {
        let dir = std::env::temp_dir().join(format!(
            "fauna-actuation-log-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("actuation.log");
        let _ = std::fs::remove_file(&path);

        append_actuation_marker(&path, "first");
        append_actuation_marker(&path, "second");

        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text, "first\nsecond\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An unwritable sink must never fail the app under test — the measurement
    /// is worth less than the run it measures.
    #[test]
    fn an_unwritable_log_sink_is_survivable() {
        append_actuation_marker(
            std::path::Path::new("/nonexistent-dir-for-the-gate-test/actuation.log"),
            "dropped",
        );
    }
}
