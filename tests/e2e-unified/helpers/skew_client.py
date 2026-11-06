"""Launch an ARBITRARY client build against an ARBITRARY nest.

The shared launch seam of the two real-binary version-skew suites
(`test_version_skew_real_binary.py`, `test_client_at_rest_upgrade.py`). Those suites
are the only place in the harness that runs a client binary the *fixtures* did not
build — the pinned previous release — so they cannot use the cached `app` /
`logged_in_app` fixtures at all; they construct drivers directly, and the per-app
plumbing that conftest normally owns has to live somewhere. It lives here.

**Why a helper and not a branch in the test files.** `architecture/testing.md`
§ Cross-app e2e conventions point 7: platform checks live in the action/driver layer,
never in test files. What differs per client here is *launch plumbing* — never an element
ID, and never an assertion:

* **The real-MLS rail is switched on differently.** linux/web bind their real
  `ConversationsSession` for every e2e login by construction, and a test opts in with the
  test-agent command `enable_real_faunamls`. The native apps default to a deterministic
  MOCK conversations backend and bind the real rail only under a **launch-time env gate**,
  `FAUNA_E2E_REAL_CONVERSATIONS` (conftest `_apply_real_conversations_env`; read by the
  macOS/iOS `applySessionPatch`). So on apple the switch must be thrown in the launch
  config — there is no post-launch command to call, and linux's would simply not ack.

Everything else — the `set_state` session seed, the fake-DNS seam, the push-suppression
gate — is already uniform across the bridge-backed drivers, and both env gates below
reach apple through shared Rust (`libs/fauna-ffi/src/nest_client.rs` reads
`FAUNA_E2E_SUPPRESS_CONV_PUSH`) or the app itself.
"""

from __future__ import annotations

import hashlib
import os
from pathlib import Path, PurePosixPath

import pytest

from actions import ActionLayer
from common.accounts import single_account_seed
from drivers import create_driver
from helpers import real_rail_control
from helpers.prev_build import expected_incompatible_reasons, prev_build_undrivable_reason


def xfail_if_prev_build_undrivable(request, client: str) -> None:
    """Mark the running cell a strict xfail when it drives a pinned build of `client`
    that the harness cannot drive yet (`prev_build.PREV_BUILD_DRIVABLE_FROM`).

    Applied per parameter, at run time, because the grids are parametrized by client and
    only one leg is affected. Strict for the reason `expected_incompatible` is: while the
    window is open the cell must fail, so a pass reads red and says the table is stale.
    """
    reason = prev_build_undrivable_reason(client)
    if reason:
        request.applymarker(pytest.mark.xfail(True, reason=reason, strict=True))

def expected_incompatible(cell: str) -> pytest.MarkDecorator:
    """The strict xfail a cross-version cell carries while the pin PREDATES a ratified
    in-place break that covers it — inert (no mark applied) once the pin has advanced
    past every such break.

    `helpers/prev_build.py::RATIFIED_INPLACE_BREAKS` owns the table and the ancestry
    check; this is only the pytest shape. `strict=True` + `run=True` on purpose: the cell
    still runs and must FAIL, so a re-added dual-serving path (the regression
    `key-material-hierarchy.md` rule #8 forbids) surfaces as an XPASS red, and the
    reason line tells a reader which § Dimension 2 ruling they are looking at instead
    of a raw signature error. `raises=` is deliberately unset: the first refused
    ceremony surfaces as an `RpcCallError` in one cell and an `_await_login` assertion
    in another, and pinning either would turn a build-time failure into a red the
    window is supposed to absorb.

    The mark covers fixture SETUP too (pytest reports a setup error on an xfail-marked
    test as XFAIL): the module-scoped `previous_nest` fixture claims admin on the pinned
    nest with the HEAD harness's signer, so under the sig_domain reset it is the fixture,
    not the test body, that refuses first — every consumer of that fixture needs the mark,
    which is why the build-identity guard carries `CELL_HARNESS_CLAIMS_PREV_NEST`.
    """
    reasons = expected_incompatible_reasons(cell)
    return pytest.mark.xfail(
        bool(reasons),
        reason="; ".join(reasons) or f"no ratified in-place break pending for {cell}",
        strict=True,
    )


#: The apps whose *previous release* binary this harness can rebuild and drive.
#: linux is the reference leg; macos is the apple leg; windows joined 2026-07-15
#: (`version-compatibility.md` § Dim 6 — "the apple and windows legs follow the same
#: pattern on their own machines"). A client joins this tuple once
#: `helpers/prev_build.py` can build it. tui joined 2026-10-31: the lead app, and the one
#: leg that collects on every machine (`prev_build.py` § One client per machine). ios
#: joined the same day, on macOS: two concurrent apps take two simulators (`launch_build`)
#: and a persistent HOME is the simulator's kept container (`state_home_config`). web joined
#: 2026-11-01, on Linux: its "binary" is a built SPA tree (`prev_build.py`'s `web-app/`, HEAD's
#: `static_dir`), served by the harness's SPA proxy over whichever nest a cell dials
#: (`_web_dial`), and a persistent HOME is one BrowserContext on one origin (`_WebHome`).
SKEW_CLIENTS = ("linux", "macos", "windows", "tui", "ios", "web")

#: Clients whose real conversations rail is a launch-time env gate rather than a
#: post-launch test-agent command. See the module doc.
#:
#: windows joined 2026-07-15: `ConversationsManagerHost.MarkRealRailsRegistered` (the
#: readiness flag the `data.conv_real_backend_active` poll observes) and
#: `StartReceiveLoop` (which runs the MLS replica restore) only run when
#: `FAUNA_E2E_REAL_CONVERSATIONS` is set at launch (`App.xaml.cs::BuildE2eConvSessionAsync`)
#: — the windows `set_state` login always builds the real `ConversationsSession` over
#: `ConversationsManagerHost.Instance` (mirrors linux), but starts its receive loop and
#: marks the real rails ready only under this gate, exactly like apple's
#: `applySessionPatch`. `set_state`'s session handling (`App.xaml.cs`, the
#: `"session"` command branch) also calls `store.SaveSecret`/`SaveCachedHandle`/
#: `SaveDeviceId` — the same `ISecretStore` `seed_credentials` pre-seeding writes into
#: under e2e (`FAUNA_E2E_CREDENTIAL_DIR`) — so windows' identity persists exactly the way
#: apple's `set_state`-based seeding does, making `launch_on_home`'s `seed_via_set_state`
#: branch (below) correct for windows too, not just a coincidental reuse of this tuple.
_ENV_GATED_REAL_CONVERSATIONS = ("macos", "ios", "windows")

#: Clients whose test agent implements `conversations_real_resolve_send_new` — the
#: command that drives a REAL MLS send (resolve the recipient, bootstrap the group,
#: deliver the Welcome, post the Application envelope).
#:
#: Implemented by linux (`apps/fauna-linux/src/lib.rs`), web
#: (`apps/fauna-web/src/lib/conversations.ts`), windows (`FaunaApp/Testing/TestAgent.cs`)
#: and — since 2026-07-14 — **both apple agents** (`Fauna-macOS/App/FaunaMacApp.swift`,
#: `Fauna-iOS/App/FaunaApp.swift`), which drive the same shared-Rust manager methods the
#: GUI's on-Enter handler does. Adding `macos` here turns the skew DM leg on and, via
#: `AT_REST_CLIENTS` below, admits apple to the at-rest grid. tui's agent carries it too
#: (`apps/fauna-tui/src/automation.rs`).
#:
#: ⚠ The apple agents also no longer ignore a command they do not implement: an unknown
#: command now surfaces on `error-message` instead of vanishing. The old silence is what
#: made a missing send look like at-rest data loss (an empty `list_threads()`), and it
#: cost two sessions.
SUPPORTS_REAL_SEND = ("linux", "web", "windows", "macos", "tui", "ios")


def supports_real_send(client: str) -> bool:
    """Can `client`'s test agent drive a real-MLS send? See `SUPPORTS_REAL_SEND`."""
    return client in SUPPORTS_REAL_SEND


#: Clients whose **pinned previous build** implements `conversations_real_resolve_send_new`.
#:
#: Distinct from `SUPPORTS_REAL_SEND`, which describes the build in the working tree, and
#: the distinction is *not* pedantry — the two suites that skew builds ask the **old**
#: binary to send:
#:
#:   * `test_previous_client_journey_against_head_nest` launches the pinned build as the
#:     sender;
#:   * `test_previous_build_state_opens_under_head_build` has the pinned build **write**
#:     the MLS state store that HEAD then reopens — writing it is the whole point.
#:
#: A capability that lands *today* therefore cannot be exercised by a build pinned
#: *yesterday*, and gating those cells on the working tree's capability asks an old binary
#: for a command it has never heard of. It answers exactly as the pre-fix apple agent did —
#: silently — so the cell fails with `threads=[]` and an empty error surface, which reads
#: like the wire broke rather than like a leg that cannot exist yet.
#:
#: `macos` joined 2026-07-15 when the pin advanced (a build that carries
#: both apple agents' `conversations_real_resolve_send_new`, landed 2026-07-14, and the
#: durable e2e credential dir from 2026-07-13) — per the standing instruction that the
#: session advancing the pin past those commits flips this. First re-run of the skew +
#: at-rest grids on a Mac after this flip verifies the two apple legs light up. `tui`
#: and `ios` joined with their legs 2026-10-31: the pin carries the command.
PREV_BUILD_SUPPORTS_REAL_SEND = ("linux", "web", "windows", "macos", "tui", "ios")


#: The apps whose HEAD→**previous-nest** cell drives the DM leg — i.e. those whose DM is
#: **stable against the PINNED nest**. Clients outside this tuple drive the feed post +
#: read-back there instead: still a real content write over the skewed wire, but a stable one.
#:
#: **apple (`macos`) was added 2026-07-15 after a re-measurement on top of the I2 transition.**
#: It had been held out because, measured 2026-07-14, this cell produced **three different
#: outcomes in three consecutive runs** against the pinned nest (body never landed / body
#: landed / launch timeout) — a leg with three outcomes asserts nothing and can only cost
#: fleet-wide sessions a red. But those runs *predate the I2 transition landing*
#: (the shared onboarding machine now commits `encrypted` after claiming a
#: mode-unresolved nest, so the strict xfail this cell used to carry is gone). Re-measured on
#: top of the transition against the pinned nest, the HEAD apple app's DM leg
#: ran **7 consecutive times, all green** — the recipient resolved, the MLS group bootstrapped,
#: and the fresh-body Welcome+Application delivered and **decrypted** on the receiver every
#: run. The flakiness *was* the gap; the transition closed it. (The DM leg is the deepest wire
#: chain a client drives, so this is the leg most worth having under assertion.)
#:
#: ⚠ If the pinned nest ever *regresses* this cell for apple, the symptom is a
#: `threads=[]` `pytest.fail` in `test_head_client_journey_against_previous_nest[macos]`.
#:
#: **windows was added 2026-10-28.** It was never excluded for a recorded reason — it had
#: simply never been measured (its cell passed on the feed post alone). Measured on Windows
#: against the pinned nest: the HEAD windows app's DM leg ran **7 consecutive times,
#: all green** (one full-file run plus six cell-1 runs).
#:
#: **tui was added 2026-10-31**, with its leg and on the same measurement. Measured on Linux
#: against the pinned nest: the HEAD tui app's DM leg ran **7 consecutive
#: times, all green** (seven cell-1 runs).
#:
#: **ios was added 2026-11-01**, with its leg. Measured on macOS against the pinned nest: the HEAD ios app's DM leg ran **7 consecutive times, all green** (one
#: full-file run plus six cell-1 runs), each pair on two simulators.
#:
#: **web was added 2026-11-01**, with its leg. Measured on Linux against the pinned nest: a first 7-run measurement went 4 of 7, every red at the journey's first
#: step (the sender's `feed-view` absent) rather than at DM delivery, and cell 2 showed the
#: same red without this tuple. It was the seeded web launch's landing, fixed in
#: `launch_build` (the launch goes to the feed); after it the HEAD web app's DM leg ran
#: **7 consecutive times, all green**.
DM_STABLE_AGAINST_PINNED_NEST = ("linux", "macos", "windows", "tui", "ios", "web")

def dm_stable_against_pinned_nest(client: str) -> bool:
    """Is this client's DM leg STABLE against the PINNED nest?

    See `DM_STABLE_AGAINST_PINNED_NEST`. NOT the same question as `supports_real_send`:
    a client whose agent can send may still have a DM against the *pinned* nest that was
    measured flaky (apple, before 2026-07-15), so it drives the feed post there instead.
    """
    return client in DM_STABLE_AGAINST_PINNED_NEST


def prev_build_supports_real_send(client: str) -> bool:
    """Can `client`'s PINNED PREVIOUS build drive a real-MLS send?

    See `PREV_BUILD_SUPPORTS_REAL_SEND` — this, not `supports_real_send`, is the right
    gate for any cell whose sender/writer is the previous binary.
    """
    return client in PREV_BUILD_SUPPORTS_REAL_SEND


#: Clients whose PINNED PREVIOUS build honors the `state_home_config` isolation seam
#: (redirects its persistent data dir away from the real user profile under a
#: `launch_on_home` launch) — orthogonal to `PREV_BUILD_SUPPORTS_REAL_SEND`, which is
#: about the send *command*, not the isolation *plumbing*.
#:
#: All three are members — linux and macOS foundationally, not as e2e-only additions,
#: and by different mechanisms (macOS's was mis-stated until it was measured 2026-07-15).
#: linux locates its state under `XDG_CONFIG_HOME` (the driver's `xdg_base`). macOS
#: locates the MLS store under `.applicationSupportDirectory`, which resolves from
#: CoreFoundation's notion of home and IGNORES `HOME` — measured: with only `env["HOME"]`
#: relocated, the store still lands in the REAL `~/Library/Application Support`. It honors
#: `CFFIXED_USER_HOME`, which `drivers/macos.py` now sets whenever a caller pins `home`
#: (the `state_home_config` reuse seam). Because that is a CoreFoundation-level override,
#: not app code, EVERY macOS build honors it regardless of age — so macOS is NOT pin-gated
#: here the way windows is (windows needs an app-side `FAUNA_E2E_DATA_DIR` the pin must
#: postdate). apple's earlier pin-age gap was entirely about *credentials*
#: (`FAUNA_E2E_CREDENTIAL_DIR`, which happened to clear on the same pin advance as its send
#: capability), never about store relocation.
#:
#: **windows IS pin-gated, unlike macOS:** its seam is app-side —
#: `BackupPaths.DataDir`'s `FAUNA_E2E_DATA_DIR` override (2026-07-15) — so
#: a pinned build honors it only if it was cut after that commit. A pin that predates
#: it silently falls back to the REAL `%LocalAppData%\\Fauna`: the upgrade cell's phase 1
#: then leaves the isolated HOME's `mls.db` absent (the vacuous-assertion guard catches
#: it), and the downgrade cell's phase 2 renders `threads=[]` — the "unbound engine"
#: signature, not data loss. windows was excluded on exactly that measurement on
#: 2026-07-15 and joined 2026-10-09, on the pin of 2026-10-05. The other half
#: of its isolation, the `%LOCALAPPDATA%` ancestor the unified account store hangs off,
#: is harness-side (`drivers/windows.py`), so every build honors it whatever its age.
#:
#: tui is a member by the harness alone: the driver derives its XDG world and, on darwin
#: and windows, the HOME and `%LOCALAPPDATA%` the account store hangs off, so every build
#: honors them whatever its age (`state_home_config`).
#:
#: ios is a member by the simulator: the app's whole world is its data container, which a
#: launch keeps by skipping the uninstall (`keep_container`), so no build has to honor
#: anything; its credentials ride `FAUNA_E2E_CREDENTIAL_DIR`, which macOS's note above
#: dates long before the pin.
#:
#: web is a member by the harness alone: its whole world is the BrowserContext, which
#: every launch creates fresh, so the "HOME" `launch_on_home` keeps is that context on one
#: origin (`_WebHome`) and no build has to honor anything.
PREV_BUILD_SUPPORTS_HOME_ISOLATION = ("linux", "macos", "windows", "tui", "ios", "web")


def prev_build_supports_home_isolation(client: str) -> bool:
    """Does `client`'s PINNED PREVIOUS build honor the `state_home_config` seam?

    See `PREV_BUILD_SUPPORTS_HOME_ISOLATION`. Distinct from `prev_build_supports_real_send`
    — a client can send today on a build that still can't find the isolated HOME to send
    *from*.
    """
    return client in PREV_BUILD_SUPPORTS_HOME_ISOLATION


#: The apps the **at-rest** grid can run on — derived, not hand-listed.
#:
#: That suite's entire subject is the MLS state store (the user-irrecoverable file whose
#: loss destroys a conversation), and the only way to WRITE that store is a real MLS
#: send. A client whose agent cannot send would therefore run a test that hydrates an
#: identity and asserts nothing whatsoever about at-rest state — a green that means
#: nothing, which is worse than an absent leg. So the at-rest grid runs exactly on the
#: clients that can write the thing it is about, and apple joins **automatically** the
#: moment its agent lands `conversations_real_resolve_send_new`.
#:
#: The apps the at-rest grid can run on. Both directions require the PINNED build to be
#: a working participant on a shared HOME, so this derives from the pinned build's
#: capability, not the working tree's.
#:
#: ✅ **apple's at-rest leg is GREEN on `--client macos` against the pin, measured
#: 2026-07-15.** Both cells pass (upgrade: pinned writes → HEAD opens; downgrade: HEAD
#: writes → pinned opens → HEAD reopens, no data loss). Two things had to be true, and
#: NEITHER was a pin advance for the store seam itself:
#:
#:   1. The pinned build must implement `conversations_real_resolve_send_new` to WRITE the
#:      store (the upgrade direction's phase 1). This WAS pin-age — the 2026-07-15 advance
#:      to a newer build (past the 2026-07-14 command) cleared it, and `macos` joined
#:      `PREV_BUILD_SUPPORTS_REAL_SEND` the same day.
#:   2. The driver must actually relocate the store to the seeded HOME — and it did not.
#:      macOS resolves `.applicationSupportDirectory` (where `conv-mls.db`/`mls.db` live)
#:      from CoreFoundation's home, which ignores `HOME`, so with only `env["HOME"]` set the
#:      store leaked to the REAL `~/Library/Application Support/Fauna` (measured: real
#:      profile written mid-run, seeded home empty → the "wrote no MLS state" vacuous-guard
#:      failure). The fix is HARNESS-side with no app change: `drivers/macos.py` now also
#:      sets `CFFIXED_USER_HOME` when `home` is pinned, which relocates the store for BOTH
#:      builds (a CoreFoundation env var, not app code — so the pinned build honors it too).
#:      This is why macOS is NOT pin-gated on HOME_ISOLATION the way windows is.
#:
#: The N+61 "pinned-HOME never authenticates" symptom was count-1 (pin-age credentials),
#: NOT the store seam — on the new pin the pinned build logs in fine on a seeded HOME.
#:
#: windows was held out 2026-07-15 to 2026-10-09 by `PREV_BUILD_SUPPORTS_HOME_ISOLATION`,
#: NOT by send capability — a genuinely different gate; see that tuple's doc.
#:
#: **web's leg asserts a different store, and says so** (2026-11-01). Web keeps no `mls.db`:
#: its MLS engine is in memory and its persistence is the sealed `__mls` replica on the nest
#: (`version-compatibility.md` § Dimension 1, the MLS-state row: "Web keeps MLS in-memory,
#: persisted as the nest-side replica blob instead"). So on web the cells are not about a
#: browser-local MLS file, and must not be read as one. What they do assert is web's own
#: at-rest pair, both user-irrecoverable: the identity and account store in the origin's
#: storage (a no-seed launch reaching `authenticated` can only have read the other build's
#: slots) and the replica, which only that identity's `BackupKey` opens — the head build
#: decrypting the pinned build's conversation reads a replica the pinned build sealed, under a
#: key it derived from the identity the pinned build left in the browser. The vacuity guard is
#: the replica, not a file (`at_rest_state_missing`).
AT_REST_CLIENTS = tuple(
    c for c in SKEW_CLIENTS
    if prev_build_supports_real_send(c) and prev_build_supports_home_isolation(c)
)


def client_env(client: str, *, suppress_push: bool = False) -> dict[str, str]:
    """The launch env for a real-MLS journey on `client`."""
    env = {
        "FAUNA_DNS_PROVIDER_FAKE": "1",
        # Shrink the receive loop's backstop ticker so the inbox drains in seconds
        # rather than on the 30 s default (shared Rust `start_receive_loop`).
        "FAUNA_CONV_POLL_SECS": "2",
    }
    if client in _ENV_GATED_REAL_CONVERSATIONS:
        env["FAUNA_E2E_REAL_CONVERSATIONS"] = "1"
    if suppress_push:
        # Receive via the durable inbox drain alone (the layer-5 backstop), so a
        # missing push arm can't make a round-trip look green.
        env["FAUNA_E2E_SUPPRESS_CONV_PUSH"] = "1"
    return env


def enable_real_conversations(app, client: str) -> None:
    """Bind this process's REAL FaunaMls backend.

    A no-op on the env-gated native apps: `client_env` already threw the switch at
    launch, and there is no post-launch equivalent to call. Calling linux's command on
    apple would not activate anything — the backend choice is made when the process
    starts.
    """
    if client in _ENV_GATED_REAL_CONVERSATIONS:
        return
    app.conversations.enable_real_faunamls()


def await_real_rail_before_send(app, client: str) -> None:
    """Hold a seeded launch's first send until its real session is up.

    On the env-gated apps that session builds after `authenticated` flips, and a send
    landing first goes to the mock rail, which paints an `Smtp` thread. Sends only: a
    reader polls its thread anyway, and a pinned build that signed itself in from its
    store predates the stamp the witness reads.
    """
    if client in _ENV_GATED_REAL_CONVERSATIONS:
        real_rail_control.witness_real_rail(
            app.driver, context="the real rail before a skew send"
        )


#: Where each app keeps the MLS state store — the user-irrecoverable file the at-rest
#: grid is really about (group secrets + signing key; the nest side is ciphertext, so if
#: this is lost the conversation is gone). Relative to the persistent HOME.
#: windows' HOME stands in for `%LOCALAPPDATA%` and its data dir is laid out under it as
#: `<home>\\Fauna` (`state_home_config` below), production's own layout — so the path
#: carries the `Fauna` segment the real profile has. ios's HOME is its app data container
#: (`driver.app_data_container()`), so its path reads as macOS's does.
#:
#: ⚠ These are the **PINNED (pre-scoping) build's** paths, which is exactly what the one
#: consumer needs: the upgrade cell's vacuity guard, asserting that the PINNED build wrote
#: a store for HEAD to then open. A client listed in `HEAD_MLS_STORE_IS_ACCOUNT_SCOPED`
#: no longer writes here at HEAD — it writes `…/<actor-id-hex>/mls.db` and never reads
#: this flat file (the first-adopter adoption was retired by the compat-remnant sweep,
#: `version-compatibility.md` § Dimension 2).
MLS_DB_RELPATH = {
    "linux": "config/fauna/mls_state.db",
    "macos": "Library/Application Support/Fauna/conv-mls.db",
    "ios": "Library/Application Support/Fauna/conv-mls.db",
    "windows": "Fauna/mls.db",
    "tui": "config/fauna-tui/mls_state.db",
}

#: Clients whose HEAD build has completed its account-scoping leg: the MLS store moved
#: from the flat path above to `<state base>/<actor-id-hex>/mls.db` (`account-scoping.md`
#: § The scoping taxonomy). Grows one entry per client leg; windows' landed 2026-07-23
#: (`AccountStateDir.cs`, `<base>\\<actor>\\mls.db`).
#:
#: The at-rest grid reads this in ONE place, the downgrade cell: an older build's reader
#: is still at the flat path, so what it renders is whatever the flat store holds. Where
#: HEAD created the HOME from scratch (the downgrade cell's phase 1), the flat store was
#: never written, so the older build authenticates and runs on an EMPTY conversation
#: history rather than the newer build's. That is the accepted, uniform consequence of
#: per-account placement — the contract it must not break is "destroy nothing", which
#: phases 1+3 assert.
HEAD_MLS_STORE_IS_ACCOUNT_SCOPED = ("macos", "linux", "windows", "tui", "ios")


def head_mls_store_is_account_scoped(client: str) -> bool:
    """Whether `client`'s HEAD build keeps its MLS store under a per-account path.

    See `HEAD_MLS_STORE_IS_ACCOUNT_SCOPED` for what the at-rest downgrade cell does
    with it, and why the weaker phase-2 expectation is the honest one rather than a
    softened contract.
    """
    return client in HEAD_MLS_STORE_IS_ACCOUNT_SCOPED


#: Clients whose PINNED PREVIOUS build has ALSO completed its account-scoping leg —
#: i.e. the release the pin points to was cut AFTER the commit that moved the MLS
#: store off the flat `MLS_DB_RELPATH` path (`AccountStateDir.swift`).
#:
#: **Measured 2026-07-22, first execution of the downgrade cell's phase-2 branch:**
#: the pin was released at 13:09, the scoping commit landed
#: at 11:39 the same day — the release simply included same-day work, so the "previous" build is
#: NOT the pre-scoping build `HEAD_MLS_STORE_IS_ACCOUNT_SCOPED`'s downgrade-cell
#: special case assumed. Both builds now write/read the identical
#: `<base>/<actor-id-hex>/mls.db` path, so the older build DOES render the
#: newer build's conversation in phase 2 — the correct, intended behavior once both
#: sides share the mechanism, not a scoping regression.
#:
#: **This can only ever grow, never shrink, and once true for a client it never
#: reverts to false** — pins advance strictly forward in time, so there will never
#: again be a "previous build" that predates a commit already included in a past
#: pin. A client's scoping work is protected by the downgrade-cell special case for
#: exactly the one release-to-release window before the pin catches up to it; after
#: that, list it here and the two builds are tested as the (now-uniform) mechanism
#: they actually are.
#:
#: **linux was scoped long before it was listed.** Its leg (`account_scope.rs`,
#: 2026-07-22) predates the pin, so that build writes only
#: the per-account store — and the upgrade cell's vacuity guard, still looking at the
#: flat path, failed in every whole-suite linux sweep as "the previous build wrote no
#: MLS state". windows was listed when its at-rest leg was first enabled (2026-10-09):
#: its scoping leg landed 2026-07-23, long before the pin of
#: 2026-10-05. tui's store was already per-account at the pin of
#: 2026-10-21 when its leg joined, and so was ios's, which shares
#: macOS's `AccountStateDir.swift`.
PREV_BUILD_MLS_STORE_IS_ACCOUNT_SCOPED = ("macos", "linux", "windows", "tui", "ios")


def prev_build_mls_store_is_account_scoped(client: str) -> bool:
    """Whether `client`'s PINNED PREVIOUS build ALSO writes the per-account path.

    See `PREV_BUILD_MLS_STORE_IS_ACCOUNT_SCOPED`. Distinct from
    `head_mls_store_is_account_scoped`, which is permanently true once a client's
    scoping leg lands — this one is a property of the CURRENT PIN, and answers
    whether the downgrade cell's "older build reads the isolated flat path" special
    case is still honest for it.
    """
    return client in PREV_BUILD_MLS_STORE_IS_ACCOUNT_SCOPED


#: The per-account MLS store's FILE name, which is per client: apple's scoped store is
#: `AccountStateDir.scopedMlsDbName`, windows' `AccountStateDir.MlsDbName`, linux keeps
#: the historical `mls_state.db`.
SCOPED_MLS_DB_NAME = {
    "linux": "mls_state.db",
    "tui": "mls_state.db",
    "android": "mls.db",
    "macos": "mls.db",
    "ios": "mls.db",
    "windows": "mls.db",
}


def scoped_mls_db_relpath(client: str, actor_id_hex: str) -> str:
    """Where `client`'s per-account-scoped MLS store lives, relative to HOME.

    `<base>/<actor-id-hex>/<SCOPED_MLS_DB_NAME[client]>`, where the flat dir is
    `MLS_DB_RELPATH[client]`'s parent — `AccountStateDir.swift` on apple,
    `AccountStateDir.cs` on windows, and `account_scope.rs` over the shared
    `actor_state_dir` on linux and tui. Only meaningful for
    a client where `head_mls_store_is_account_scoped` (HEAD) or
    `prev_build_mls_store_is_account_scoped` (the pinned previous build) is true.
    """
    flat_dir = PurePosixPath(MLS_DB_RELPATH[client]).parent
    return str(flat_dir / actor_id_hex.lower() / SCOPED_MLS_DB_NAME[client])


def at_rest_state_missing(client: str, root, actor: dict, nest: dict) -> str | None:
    """Why the PINNED build left no at-rest MLS state for HEAD to open, or ``None`` when
    it did — the upgrade cell's vacuity guard, without which its assertion proves nothing.

    A native client's state is its store file under the HOME (`root`, where the last
    launch put it): per-account once the pinned build is scoped, flat before. web's is the
    sealed `provider` replica on the nest (`AT_REST_CLIENTS`' web paragraph), read as the
    actor over the wire: the owner alone can `fauna.mls.get` it, and the nest never opens it.
    """
    if client == "web":
        from clients.ws_rpc_admin_client import WsRpcAdminClient

        with WsRpcAdminClient(
            nest["url"], actor_id=actor["actor_id_bytes"], signing_key=bytes(actor["signing_key"]),
        ) as owner:
            blob = owner.call("fauna.mls.get", {"path": "provider"}).get("blob")
        if blob:
            return None
        return (
            "the previous web build sealed no `provider` MLS replica on the nest "
            f"(fauna.mls.get answered {blob!r}) — the upgrade assertion would be vacuous"
        )
    if prev_build_mls_store_is_account_scoped(client):
        path = Path(root) / scoped_mls_db_relpath(client, actor["actor_id_hex"])
    else:
        path = Path(root) / MLS_DB_RELPATH[client]
    if path.exists() and path.stat().st_size > 0:
        return None
    return f"the previous build wrote no MLS state at {path} — the upgrade assertion would be vacuous"


def head_app_path(client: str, request) -> str:
    """HEAD's build of `client`: its launch path, or for web the built SPA tree the
    `static_dir` fixture names (web launches from a URL; the tree is what a cell's proxy
    serves, `_web_dial`)."""
    if client == "web":
        return request.getfixturevalue("static_dir")
    from helpers.app_launch import app_launch_config

    return app_launch_config(client, request)["app_path"]


def _web_dial(request, tree, nest: dict):
    """``(the nest as the page dials it, the page URL, the proxy server)`` for a web app
    built as `tree` against `nest`.

    A browser reaches a nest only through an SPA proxy (`conftest._serve_spa_proxy`: the raw
    nest sends no CORS header), and that proxy is also what serves the build — so one
    function stands in for both of a native launch's inputs, the binary and the nest URL.
    Every URL the app reads (the session's `node_url`, the trust seed's key) is the dialled
    one. A callable `tree` is the `_WebHome` seam (one origin, its build swapped). The server is
    shut down when `request` finishes.
    """
    from conftest import _serve_spa_proxy

    url, server = _serve_spa_proxy(tree, nest["url"])
    request.addfinalizer(server.shutdown)
    return {**nest, "url": url}, url + "/app/", server


def _apply_web_launch_twins(driver, *, suppress_push: bool) -> None:
    """web's post-launch twins of `client_env`: the SPA has no process env, so the poll
    backstop and the push suppression are thrown through its e2e hooks instead
    (`drivers/web.py::set_conv_poll_secs`, `set_conv_push_suppressed`; both present at the
    pin). The fake-DNS gate has no web twin and needs none: the SPA dials the proxy."""
    driver.set_conv_poll_secs(int(client_env("web")["FAUNA_CONV_POLL_SECS"]))
    if suppress_push:
        driver.set_conv_push_suppressed(True)


def assert_web_build_served(driver, tree) -> None:
    """The page running now was loaded from `tree` — the web twin of the nest build-stamp
    guard (`test_the_two_nests_are_genuinely_different_builds`).

    The SPA carries no commit stamp, and a proxy serving the wrong tree would quietly turn
    a skew cell into HEAD against HEAD. So the guard reads what the page actually fetched:
    every content-hashed `_app/immutable/` chunk it loaded must exist in `tree`, and there
    must be at least one. Unchanged chunks keep their hash across builds, but the entry
    and every chunk whose source moved do not, so a page from the other tree loads at least
    one name this tree lacks and fails here.
    """
    loaded = driver.eval_js(
        "performance.getEntriesByType('resource').map(e => new URL(e.name).pathname)"
        ".filter(p => p.includes('/_app/immutable/'))"
    ) or []
    root = Path(str(tree))
    chunks = sorted({p.split("/app/", 1)[-1] for p in loaded})
    foreign = [c for c in chunks if not (root / c).is_file()]
    assert chunks and not foreign, (
        f"the web page was not served from {root}: loaded {len(chunks)} immutable chunk(s), "
        f"{len(foreign)} absent from that tree (first: {foreign[:3]}) — the cell would be "
        "testing some other build"
    )


class _WebHome:
    """One web "HOME": a BrowserContext on one origin, opened by build after build.

    A web app upgrades in place when the origin starts serving a new tree, and what it keeps
    is the origin's storage. So the HOME is the context of ONE driver behind ONE proxy whose
    tree swaps (`_serve_spa_proxy`'s callable `static_dir`, served `no-store`); a phase ends
    by closing the page and keeping the context (`end_phase` → `quit_app`), and the next
    phase points the proxy at its build and relaunches a page in that context
    (`hard_reload`). The driver is torn down for real when the test finishes.
    """

    _by_home: dict[str, "_WebHome"] = {}

    def __init__(self, request, nest: dict, tree):
        self.tree = [str(tree)]
        self.dialled, self.page_url, _server = _web_dial(
            request, lambda: self.tree[0], nest)
        self.driver = create_driver("web")
        request.addfinalizer(self.driver.teardown)

    @classmethod
    def open(cls, request, home, nest: dict, tree, *, seed: bool):
        key = str(home)
        if seed:
            cls._by_home[key] = cls(request, nest, tree)
            request.addfinalizer(lambda: cls._by_home.pop(key, None))
            return cls._by_home[key], True
        web_home = cls._by_home.get(key)
        if web_home is None:
            pytest.fail(f"no web HOME was opened at {home} — the first launch passes seed=True")
        web_home.tree[0] = str(tree)
        return web_home, False


def end_phase(client: str, driver) -> None:
    """End one phase of a `launch_on_home` HOME: the process exits and its data stays.

    A native app's data is its directory, so its driver is torn down. web's data is the
    BrowserContext, which a teardown would close; the page closes instead and the context
    stays for the next build (`_WebHome`)."""
    if client == "web":
        driver.quit_app()
    else:
        driver.teardown()


def hex_device_id(label: str) -> str:
    """A VALID 64-char-hex device id, derived from a human-readable label.

    **A `set_state` device_id must be hex.** Real device ids are
    (`hex::encode(device_id())`), and the shared upload/key-package gestures
    hex-DECODE it — so a readable placeholder like ``"prev-client-alice"`` fails
    `invalid device_id hex` on any client that reads it (`helpers/e2e_session.py`
    documents the same trap for `e2e_login_device_id`). linux tolerated the readable
    string, which is exactly why it survived the reference implementation; on apple it
    silently cost the whole real-MLS rail — no key packages published, so `list_threads()`
    stayed EMPTY and every DM assertion failed as if the conversation had been lost.

    Hashing the label keeps the call sites readable (``"prev-client-alice"``) AND the ids
    valid *and distinct per app* — two GUI apps in one cell are two different devices,
    so they must not share one id.
    """
    return hashlib.sha256(label.encode()).hexdigest()


def _session_patch(nest: dict, user: dict, handle: str, device_id: str) -> dict:
    return {
        "session": {
            "authenticated": True,
            "node_url": nest["url"],
            "secret_hex": user["signing_key"].encode().hex(),
            "handle": handle,
            "actor_id": user["actor_id_hex"],
            "device_id": hex_device_id(device_id),
        },
        "nav": {"stack": [{"view": "feed"}]},
    }


def state_home_config(client: str, *, home, creds, keyring_app: str) -> dict:
    """The launch keys that pin ONE persistent client state root.

    The seam that hands the same HOME to two *different builds* in sequence — the whole
    question the at-rest upgrade-in-place grid asks. The drivers default every launch to a
    fresh throwaway root (deliberate per-app isolation), so this has to be asked for.

    The key names differ because the platforms genuinely do: linux keeps client state
    under XDG dirs plus a *named keyring namespace*, macOS keeps it under Application
    Support (relocated via `CFFIXED_USER_HOME` — which the driver sets from the pinned
    `home`; macOS's `.applicationSupportDirectory` ignores `HOME` itself) with the E2E
    keychain living as a file in the credential dir, and windows pins `%LOCALAPPDATA%`
    itself (`local_appdata`) with the app's data dir under it as `<home>\\Fauna` —
    production's layout. Both are needed because windows has two roots there: the app's
    own data dir (MLS store, logs, trust-pin store — `BackupPaths.DataDir`, reached via
    `FAUNA_E2E_DATA_DIR`), and the unified account store shared Rust roots at
    `%LOCALAPPDATA%\\Fauna\\sync`, which only the ancestor reaches; a `data_dir` alone
    left every build on a fresh account store. Credentials ride its existing file-backed
    store (`FAUNA_E2E_CREDENTIAL_DIR`, the same seam a normal windows e2e login already
    uses — see `drivers/windows.py`). tui is linux's spelling on every platform, plus
    `home` for darwin, where the unified account store hangs off the relocated HOME
    rather than the XDG world (`drivers/tui.py::_account_store_root`; the key is read
    only there, and windows derives its `%LOCALAPPDATA%` from `xdg_base`). ios has no root
    to relocate: the simulator's data container is the app's HOME, per (device, bundle id),
    so what pins it is NOT uninstalling it, which `launch_on_home` decides per launch
    (`keep_container`). `home` is unused there. Same concept, five spellings; the mapping
    is confined to this function so no test file ever branches on it.
    """
    if client == "macos":
        return {"home": str(home), "credential_dir": str(creds)}
    if client == "ios":
        return {"credential_dir": str(creds)}
    if client == "windows":
        return {
            "local_appdata": str(home),
            "data_dir": os.path.join(str(home), "Fauna"),
            "credential_dir": str(creds),
            "keyring_app": keyring_app,
        }
    if client == "tui":
        return {
            "xdg_base": str(home),
            "home": str(home),
            "keyring_app": keyring_app,
            "credential_dir": str(creds),
        }
    return {
        "xdg_base": str(home),
        "keyring_app": keyring_app,
        "credential_dir": str(creds),
    }


def driver_home(driver) -> Path | None:
    """The HOME this launch actually used, where the driver places it rather than the
    caller (the ios simulator's data container); ``None`` where the caller's own."""
    container = getattr(driver, "app_data_container", None)
    return Path(container()) if container and container() else None


def launch_on_home(
    client: str,
    app_path,
    *,
    home,
    creds,
    keyring_app: str,
    nest: dict,
    user: dict,
    device_id: str,
    seed: bool,
    request,
    build_commit: str | None,
):
    """A GUI app from `app_path` on a PERSISTENT `home` — the at-rest grid's seam.

    `request` and `build_commit` seed the app's escrow trust in `nest` as
    `launch_build` does.

    `seed=True` (first launch only) leaves the identity on disk the way a completed
    onboarding would have. Every later launch passes `seed=False`: the build under test
    must hydrate the identity from what the OTHER build persisted, which is the point.

    The two apps seed differently, and the difference is unavoidable rather than
    incidental. linux takes a pre-boot `seed_credentials` launch key (the harness writes
    the store before the app starts). The macOS driver has no such key — the native app
    owns its keychain — so apple instead performs a normal `set_state` login on the first
    launch and lets the app persist it into the pinned credential dir (the same durability
    the crash-recovery journeys ride on). Both end with the same thing on disk: an identity
    a later build has to hydrate with no re-onboarding.

    On ios the first launch is also what empties the HOME: its container outlives the
    pytest `home` directory, so `seed=True` uninstalls and every later launch keeps it.
    """
    from conftest import _apply_r14_trust_env

    environment = client_env(client)
    if client == "web":
        # The HOME is one context on one origin (`_WebHome`). The first phase launches it,
        # its identity seeded as linux's is (the same logical-key map, into the origin's
        # storage); a later phase swaps the build that origin serves and opens a page on
        # what the earlier build left. The trust seed is written once, at the first
        # launch, and stays in the origin: both builds read its keyed grammar
        # (`prev_build.reads_keyed_trust_seed` — the pin postdates it).
        web_home, first = _WebHome.open(request, home, nest, app_path, seed=seed)
        driver = web_home.driver
        if first:
            _apply_r14_trust_env(environment, web_home.dialled, request,
                                 build_commit=build_commit)
            driver.launch({
                "url": web_home.page_url,
                "environment": environment,
                "seed_credentials": single_account_seed(
                    user["signing_key"].encode().hex(),
                    nest_url=web_home.dialled["url"],
                    device_id=hex_device_id(device_id),
                ),
            })
        else:
            driver.hard_reload()
        assert_web_build_served(driver, app_path)
        _apply_web_launch_twins(driver, suppress_push=False)
        return driver, ActionLayer(driver)
    _apply_r14_trust_env(environment, nest, request, build_commit=build_commit)
    config = {
        "url": nest["url"],
        "app_path": str(app_path),
        "environment": environment,
        **_seat_keys(client, request),
        **state_home_config(client, home=home, creds=creds, keyring_app=keyring_app),
    }
    if client == "ios":
        config["keep_container"] = not seed
    seed_via_set_state = seed and client in _ENV_GATED_REAL_CONVERSATIONS
    if seed and not seed_via_set_state:
        # The account-registry shape (`fauna/index` + per-actor slots) — the only
        # one linux reads since the legacy single slot retired.
        config["seed_credentials"] = single_account_seed(
            user["signing_key"].encode().hex(),
            nest_url=nest["url"],
            device_id=hex_device_id(device_id),
        )

    driver = create_driver(client)
    driver.launch(config)
    if seed_via_set_state:
        driver.set_state(_session_patch(nest, user, "at-rest", device_id))
    return driver, ActionLayer(driver)


def _seat_keys(client: str, request, second_seat=None) -> dict:
    """The launch keys naming which simulator an ios app runs on; empty elsewhere, where
    every process is its own seat.

    `second_seat` is conftest's `ios_second_seat` for the SECOND concurrently-live app:
    one simulator holds one install of a bundle id, and a launch there would tear the
    first app down."""
    if client != "ios":
        return {}
    if second_seat is None:
        return {"udid": request.getfixturevalue("ios_setup")["udid"]}
    udid = second_seat()
    if not udid:
        pytest.fail("no second iOS simulator could be created for the second app")
    return {"udid": udid}


def launch_build(
    client: str,
    app_path,
    nest: dict,
    user: dict,
    handle: str,
    device_id: str,
    *,
    request,
    build_commit: str | None,
    suppress_push: bool = False,
    second_seat=None,
):
    """A GUI app of `client`, from the binary at `app_path`, against `nest`.

    `app_path` is the seam the whole grid turns on: pass HEAD's binary or the pinned
    previous one and everything else is identical — except the escrow-trust seed,
    which each binary reads in its own grammar. `build_commit` is the commit
    `app_path` was built from when it is not the working tree's build (the pin's
    commit), `None` for the working tree's; `conftest._apply_r14_trust_env` picks
    the grammar from it.

    Login is the harness's standard `set_state` session seed (what every tier_3 journey
    uses). The client still performs the real connect/auth handshake against the nest over
    the wire, which is the login surface these suites test. `device_id` is load-bearing on
    apple — its test agents gate client construction on it, and omitting it yields an
    authenticated *shell* with no `FaunaClient` behind it (every nest-backed gesture then
    silently no-ops). A second concurrent app passes `second_seat` (`_seat_keys`).
    """
    from conftest import _apply_r14_trust_env

    environment = client_env(client, suppress_push=suppress_push)
    url = nest["url"]
    if client == "web":
        # web's build is a tree its own proxy serves, and that proxy is the nest the page
        # dials (`_web_dial`).
        nest, url, _server = _web_dial(request, app_path, nest)
    _apply_r14_trust_env(environment, nest, request, build_commit=build_commit)
    driver = create_driver(client)
    driver.launch({
        "url": url,
        "app_path": str(app_path),
        "environment": environment,
        **_seat_keys(client, request, second_seat),
    })
    driver.set_state(_session_patch(nest, user, handle, device_id))
    if client == "web":
        assert_web_build_served(driver, app_path)
        _apply_web_launch_twins(driver, suppress_push=suppress_push)
        # A seeded web launch lands where the SPA's own boot routes it once the session
        # hydrates, which can overtake the patch's `nav` (measured 2026-11-01: the sender's
        # `feed-view` absent in 5 of 14 two-seat runs, at the journey's first step). So the
        # launch goes to the feed through the tab bar once signed in, as the mixed-nests
        # cell does for every leg — reaching the tab bar at all is the signed-in shell.
        driver.wait_for_state(
            lambda s: bool(s.get("session", {}).get("authenticated")), timeout=60)
        driver.navigate_to("feed")
    return driver, ActionLayer(driver)
