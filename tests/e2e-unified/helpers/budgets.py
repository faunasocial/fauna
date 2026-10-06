"""Named wait-budget constants for ``helpers.waiting.wait_until`` (testing.md
convention 14). Each is a generous ceiling for one operation class — sized far
above any non-pathological delay, never tuned to observed latency. Pick the
constant matching the operation under test; add a new one (with a one-line
docstring naming what it guards) rather than stretching an existing class to
cover something it doesn't describe.
"""

UI_SETTLE_S = 15.0
"""A client-local UI state (toggle, sheet open/close, search-box filter over
already-fetched data) settles with no nest round-trip."""

RPC_ROUNDTRIP_S = 20.0
"""A client action dispatches over WS-RPC, the nest persists it, and the
client re-renders the confirmed state."""

IMAGE_PAINT_S = 30.0
"""A client fetches a media blob by hash over HTTP, opens it when it is sealed,
decodes it, and repaints the image element that shows it."""

PROVIDER_VERIFY_S = 60.0
"""An admin action verifies or publishes against an external DNS-provider API
(real HTTP, a fake-provider test double, or the web app's CORS-proxied
path) — slower and less predictable than an in-house RPC round-trip."""

MLS_HANDSHAKE_S = 90.0
"""A real cross-app FaunaMls flow: key-package publish, Welcome delivery, or
message decrypt, carried by the client receive loop.

That loop is **push-driven with a backstop ticker** — it subscribes
``ConversationsPush`` (`welcome.received` / `channel.message`) and sweeps every
rail on a tick or a reconnect — so this budget covers a push that was dropped or
never sent plus a full sweep, not a fixed polling latency. (It said "poll-only …
not a push" until 2026-08-14; that was already stale, and it mattered: it made
the ticker look like the only delivery path and the cadence like the thing to
tune.) A test that needs a sweep should POKE one and anchor on
``helpers.waiting.await_receive_cycle_after`` (``RECEIVE_CYCLE_S``) rather than
sizing a budget against the tick."""

CROSS_NEST_S = 60.0
"""A federated/cross-nest network hop against a real peer server (ActivityPub
webfinger/discovery, relationship delivery, timeline ingestion)."""

FEDERATION_EXCHANGE_CYCLE_S = 120.0
"""One unprompted exchange-originator cycle reaches a peer and its import lands
(`trending.md` § Federation exchange; `federation.md`'s originator plane).

Bigger than ``CROSS_NEST_S`` because it is not one hop: the cycle is scheduled,
not requested, so the budget covers the local-aggregate transition debounce
plus one possible min-peer-interval backoff before the pull happens at all,
and only then the round trip. A test asserting what an exchange did NOT carry
anchors on a control entry arriving within this budget rather than sizing a
wait against the cadence."""

PUSH_REFRESH_S = 30.0
"""A nest push (PushEvent::*) reaches an already-mounted page's own live
handler and the page re-fetches — a same-machine round trip actually needs
~5s, but dev machines run many sessions' suites at once (test_push_live_
refresh.py's reasoning): 30s costs nothing on the happy path and avoids load
turning a real push arm into a flaky red."""

PUSH_DIAL_S = 60.0
"""The nest has already decided to push to an offline actor and its background
task POSTs to the subscribed endpoint (a loopback listener the test owns): one
DB read and one local HTTP request, off the sender's reply path. Sized above
the nest's own whole-dispatch deadline (30 s, ``push.rs``), so a POST the nest
would still make is never given up on."""

SERVICE_BOOT_S = 180.0
"""A real OS service (systemd unit, socket) installs and reaches steady state
under real supervision (tier_4 / real_session)."""

ORCHESTRATION_STEP_S = 90.0
"""A background nest-provisioning orchestrator step (fake or real cloud
backend) advances to its next milestone."""

MAIL_AGENT_WRITEBACK_S = 90.0
"""The mail bridge's agent-side sealed model/history write-back lands after a
STORE command's tagged OK (asynchronous relative to the STORE response)."""

MAIL_OUTBOUND_CYCLE_S = 90.0
"""One outbound drain cycle completes after a `run_now` poke: the MTA bridge
wakes on the `fauna.bridges.outbound_ready` nudge, fetches the due batch, runs
an MX attempt (its own `AttemptTimeout` alone is 60s) and reports the outcome
back to nest, which advances the row. Sized above that attempt ceiling — the
poll interval it replaces was itself 30s, so a cycle that has to fall back to
the poll still fits."""

ALERT_SWEEP_PASS_S = 90.0
"""One critical-alert sweep pass completes after the session establishment that
spawned it (`helpers.waiting.await_sweep_pass_after`). A pass runs every feeder
in turn — two nest round trips plus, for the two directory-backed feeders, a
fetch of the public PLC audit log — and it is deliberately best-effort, so it
also has to absorb an unreachable feeder's own transport timeout before the pass
counts itself finished. Generous by design: a green run returns the instant the
pass lands, and only a genuine "the sweep never looked" spends this."""

RESCORE_WORKLIST_SERVE_S = 90.0
"""The nest serves the re-score drain a worklist after a `config_changed` poke
(`helpers.waiting.await_rescore_worklist_serve_after`). The MDA's drain wakes on
the push, does a use-time grant `Refresh` round trip, and only then asks what it
owes — so this covers two nest round trips plus the coalescing delay of a poke
that lands while a previous run is still in flight. Generous by design: a green
run returns the instant the serve is counted; only "the drain never looked"
spends this, and the 12h backstop means a missed push cannot rescue it."""

RECEIVE_CYCLE_S = 90.0
"""One full client receive cycle completes after a `conv_receive_now` poke
(`helpers.waiting.await_receive_cycle_after`). A cycle drains the durable inbox,
polls every bound conversation channel, sweeps both mail read-feeds and the
scheduling + folder feeds — several nest round trips, each of which may itself
be paging. Sized alongside `MLS_HANDSHAKE_S` because the same handshake work
happens inside it; generous by design: a green run returns the instant the
counter passes its baseline, and only "the loop never ran" spends this."""

MLS_COMMIT_FOLD_S = 90.0
"""One device folds in another device's epoch-advancing commit and its count of
folded-in commits for that channel moves (`helpers.waiting.await_folded_commit_after`).
The fold rides the client receive loop, so this covers the loop's own backstop
cadence plus a `channel.fetch` round trip and — for the twin-device own-leaf case
the two cross-device tests drive — the resync's *replica* refetch on top, which
is a second nest round trip against a `BackupKey`-sealed blob. Sized alongside
`MLS_HANDSHAKE_S` because it is the same handshake class; generous by design,
since a green run returns the instant the count moves and only a genuine "this
device never incorporated the other's commit" spends the budget."""

APP_RELAUNCH_S = 120.0
"""An app is torn down and relaunched against a preserved client-local store,
and its launch flow reaches a decided state (into the app, or onto the launch
screen that explains why not). Covers process teardown, cold start, store
re-read and the launch machine's first round trip — several seconds on an idle
machine, far more on a loaded one."""

APP_EXIT_S = 60.0
"""A closed window's graceful-quit path (WM_CLOSE/close-request → decide
hide-vs-quit → persist drafts + a bounded engagement-cue flush → teardown →
process exit) completes and `wait_app_exit()` observes it. Was three separate
literals — windows' two call sites carried `15`, linux's three carried `10` —
a per-app-and-per-call-site split with no stated reason (the `BRIDGE_ACK_BUDGET_S`
trap, generalized). The quit path itself is fast on an idle machine (its
own internal waits are bounded at 1.5s each), but the OS has to schedule the
quitting process's thread to run it at all, and `_RELAUNCH_READY_BUDGET_S`
(`drivers/windows.py`) already documents that getting a CPU slice on this
shared box can cost low double-digit seconds under concurrent sibling builds
— the same scheduling pressure applies to a process exiting, not just one
starting. Generous by design: `wait_app_exit` deadline-polls and returns the
instant the process is gone, so a green run pays only the real exit latency."""

PRE_IDENTITY_CHAIN_WALK_S = 60.0
"""An anonymous, pre-identity walk of an identity's registration/succession
chain completes and the client re-renders on its result. Several round trips on
a freshly opened connection, each rate-limited as the public directory oracle it
is — so meaningfully slower than a single authenticated RPC."""

IDENTITY_REFRESH_S = 120.0
"""A post-auth background identity re-challenge (`FaunaClient::silent_sign_in`)
classifies and, on a genuine change, escalates through
`DataMessage::NestIdentityChanged` to the blocking `launch_identity_changed`
surface. The round trip itself needs ~3s on a calm box; the previous 60s literal
measured FAIL on a loaded box (four build/e2e slots held, load ~20) — generous
by design, since a green run returns the instant the surface paints."""

UI_SCROLL_SWEEP_S = 45.0
"""A UIA percent-sweep (`http_bridge.wait_for`'s `_scroll_into_view` fallback,
used when the target has no `ScrollItemPattern` support) needs to walk a
long, single-`ScrollViewer` page to find a below-the-fold element. On
`AdminDnsPage.xaml`'s `DomainsList` a full 6-step sweep measures 469-805ms
(`fauna-bridge-diag-*.log`, 2026-09-14) — the earlier "17-25s" figure here
was `test_bundled_provider`'s unrelated `Actions.cs:995-1000` measurement,
copied in by mistake; this
page's actual repeat-poll failures were a genuine layout bug (a squeezed-
to-zero-width column, fixed in `AdminDnsPage.xaml`), never sweep slowness.
The 45s budget is still generous by design and worth keeping regardless —
a green run returns the instant the element is found."""

ACCOUNT_RUNTIME_ASSEMBLY_S = 240.0
"""The account-store runtime's post-auth assembly (`helpers.waiting.
await_account_runtime_assembled`) — a task spawned OFF the login path, never
awaited by it (`apps/fauna-linux/src/account_runtime.rs`'s `install` module
docs: "Assembly does real I/O — a credential-slot read, a store open, an IPC
round trip to the co-located agent"). `wait_until_online`'s connection barrier
covers transport only, not this; a caller that reads the account-store's
namespace right after login without this barrier races a background task with
no completion signal of its own — the class found via
`test_sign_out_erases_the_credential_namespace[linux]` failing its precondition
only when run after another app's login in the same session. Same budget
`test_account_runtime_pump.py` measured before this was lifted out of it;
generous by design, since a green run returns the instant the role publishes
`runtime_up`."""

MESSAGE_BANNER_PASS_S = 60.0
"""One new-message banner diff tick completes after an inbound message lands
(`helpers.waiting.await_banner_pass_after`). The tick is driven by the shared
`ConversationsManager`'s snapshot observer — an in-process channel wake, a
snapshot read and a `MessageNotificationTracker::diff` — so the honest cost is
sub-second and everything above that is the app's own event loop under a loaded
box. Sized like the other causal barriers rather than tight: a green run returns
the instant the tick lands, and only a genuinely un-driven observer (the failure
the barrier exists to name) spends it."""

SERVING_ENABLEMENT_S = 120.0
"""The post-claim serving-enablement step finishes after onboarding reaches the
logged-in shell (`helpers.waiting.await_serving_enablement_for`). The step runs
its plan sequentially: an `am_i_admin` round trip, then — on a real-domain claim —
the generated-password mailbox mint (several provision round trips, each retried
on a transient) and the three DAV toggles. Sized like the other causal barriers:
a green run returns the instant the onboarded actor's run is marked completed,
and only a glue that never runs or never finishes spends it."""
