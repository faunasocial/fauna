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
from pathlib import PurePosixPath

import pytest

from actions import ActionLayer
from common.accounts import single_account_seed
from drivers import create_driver
from helpers.prev_build import expected_incompatible_reasons

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
#: `helpers/prev_build.py` can build it.
SKEW_CLIENTS = ("linux", "macos", "windows")

#: Clients whose real conversations rail is a launch-time env gate rather than a
#: post-launch test-agent command. See the module doc.
#:
#: windows joined 2026-07-15: `ConversationsManagerHost.RegisterRealManager` (the
#: manager the `data.conv_real_backend_active` poll observes) only runs when
#: `FAUNA_E2E_REAL_CONVERSATIONS` is set at launch (`App.xaml.cs::BuildE2eConvSessionAsync`)
#: — the windows `set_state` login always builds the real `ConversationsSession` (mirrors
#: linux), but registers it as the ACTIVE manager only under this gate, exactly like
#: apple's `applySessionPatch`. `set_state`'s session handling (`App.xaml.cs`, the
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
#: Implemented by linux (`apps/fauna-linux/src/main.rs`), web
#: (`apps/fauna-web/src/lib/conversations.ts`), windows (`FaunaApp/Testing/TestAgent.cs`)
#: and — since 2026-07-14 — **both apple agents** (`Fauna-macOS/App/FaunaMacApp.swift`,
#: `Fauna-iOS/App/FaunaApp.swift`), which drive the same shared-Rust manager methods the
#: GUI's on-Enter handler does. Adding `macos` here turns the skew DM leg on and, via
#: `AT_REST_CLIENTS` below, admits apple to the at-rest grid.
#:
#: ⚠ The apple agents also no longer ignore a command they do not implement: an unknown
#: command now surfaces on `error-message` instead of vanishing. The old silence is what
#: made a missing send look like at-rest data loss (an empty `list_threads()`), and it
#: cost two sessions.
SUPPORTS_REAL_SEND = ("linux", "web", "windows", "macos")


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
#: at-rest grids on a Mac after this flip verifies the two apple legs light up.
PREV_BUILD_SUPPORTS_REAL_SEND = ("linux", "web", "windows", "macos")


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
#: ⚠ Only `macos` was re-measured — `ios` is not in `SKEW_CLIENTS`, so the grid never drives
#: it here. If the pinned nest ever *regresses* this cell for apple, the symptom is a
#: `threads=[]` `pytest.fail` in `test_head_client_journey_against_previous_nest[macos]`.
DM_STABLE_AGAINST_PINNED_NEST = ("linux", "macos")


def dm_stable_against_pinned_nest(client: str) -> bool:
    """Is this client's DM leg STABLE against the PINNED nest?

    See `DM_STABLE_AGAINST_PINNED_NEST`. NOT the same question as `supports_real_send`:
    apple can send (green against a HEAD nest) but its DM against the *pinned* nest was
    measured flaky, so it drives the feed post there instead.
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
#: linux and macOS are BOTH members — foundational, not e2e-only additions — but by
#: different mechanisms, and macOS's was mis-stated until it was measured 2026-07-15.
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
#: **windows is excluded 2026-07-15, and will stay excluded until the pin advances past
#: this commit.** `BackupPaths.DataDir`'s `FAUNA_E2E_DATA_DIR` override — the seam
#: `state_home_config`'s windows branch relies on — is new THIS session; no pinned build
#: can possibly honor an env var that didn't exist when it was built. Measured: the
#: pinned build silently falls back to the REAL `%LocalAppData%\\Fauna`, so (a) the
#: upgrade cell's phase 1 (pinned build writes) leaves the isolated HOME's `mls.db`
#: entirely absent (`home.mls_db.exists()` false — vacuous-assertion guard catches it
#: cleanly, not a crash), and (b) the downgrade cell's phase 2 (pinned build reads a
#: HEAD-written HOME) opens with an empty `threads=[]` — the "unbound engine" signature
#: `_assert_thread_visible`'s own error message calls out, NOT data loss (HEAD's write
#: at the isolated path is untouched; only the OLD build is looking in the wrong place).
#: **Unblock:** once a pin lands that postdates this change, add `"windows"` here (the
#: standing instruction already governs pin-advance sessions generally — see
#: `pinned-previous-build.toml`'s update rule) and re-run
#: `test_client_at_rest_upgrade.py --client windows`.
PREV_BUILD_SUPPORTS_HOME_ISOLATION = ("linux", "macos")


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
#: ⚠ **windows measured 2026-07-15 on the pin: excluded by `PREV_BUILD_SUPPORTS_
#: HOME_ISOLATION`, NOT by send capability** (windows has been in `PREV_BUILD_SUPPORTS_
#: REAL_SEND` since the pin's commit — this is a genuinely different gate; see that
#: tuple's doc for the measured failure signatures). The windows real-binary SKEW-SMOKE
#: suite (`test_version_skew_real_binary.py`, not this file) is fully green, including
#: the real-MLS DM leg — only the at-rest HOME-isolation seam is pin-gated.
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


#: Where each app keeps the MLS state store — the user-irrecoverable file the at-rest
#: grid is really about (group secrets + signing key; the nest side is ciphertext, so if
#: this is lost the conversation is gone). Relative to the persistent HOME.
#: windows relocates its ENTIRE `%LocalAppData%\Fauna` data dir under `FAUNA_E2E_DATA_DIR`
#: (`state_home_config` below), not just a keyring namespace — so `mls.db` lands directly
#: under the pinned `home`, with no `Fauna`-subfolder segment the way the real profile has.
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
    "windows": "mls.db",
}

#: Clients whose HEAD build has completed its account-scoping leg: the MLS store moved
#: from the flat path above to `<state base>/<actor-id-hex>/mls.db` (`account-scoping.md`
#: § The scoping taxonomy). Grows one entry per client leg.
#:
#: The at-rest grid reads this in ONE place, the downgrade cell: an older build's reader
#: is still at the flat path, so what it renders is whatever the flat store holds. Where
#: HEAD created the HOME from scratch (the downgrade cell's phase 1), the flat store was
#: never written, so the older build authenticates and runs on an EMPTY conversation
#: history rather than the newer build's. That is the accepted, uniform consequence of
#: per-account placement — the contract it must not break is "destroy nothing", which
#: phases 1+3 assert.
HEAD_MLS_STORE_IS_ACCOUNT_SCOPED = ("macos", "linux")


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
#: MLS state".
PREV_BUILD_MLS_STORE_IS_ACCOUNT_SCOPED = ("macos", "linux")


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
#: `AccountStateDir.scopedMlsDbName`, linux keeps the historical `mls_state.db`.
SCOPED_MLS_DB_NAME = {
    "linux": "mls_state.db",
    "macos": "mls.db",
}


def scoped_mls_db_relpath(client: str, actor_id_hex: str) -> str:
    """Where `client`'s per-account-scoped MLS store lives, relative to HOME.

    `<base>/<actor-id-hex>/<SCOPED_MLS_DB_NAME[client]>`, where the flat dir is
    `MLS_DB_RELPATH[client]`'s parent — `AccountStateDir.swift` on apple, and
    `account_scope.rs` over the shared `actor_state_dir` on linux. Only meaningful for
    a client where `head_mls_store_is_account_scoped` (HEAD) or
    `prev_build_mls_store_is_account_scoped` (the pinned previous build) is true.
    """
    flat_dir = PurePosixPath(MLS_DB_RELPATH[client]).parent
    return str(flat_dir / actor_id_hex.lower() / SCOPED_MLS_DB_NAME[client])


def hex_device_id(label: str) -> str:
    """A VALID 64-char-hex device id, derived from a human-readable label.

    **A `set_state` device_id must be hex.** Real device ids are
    (`hex::encode(device_id())`), and the shared upload/key-package gestures
    hex-DECODE it — so a readable placeholder like ``"prev-client-alice"`` fails
    `invalid device_id hex` on any client that reads it (`helpers/e2e_session.py`
    documents the same trap for `E2E_LOGIN_DEVICE_ID`). linux tolerated the readable
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
    keychain living as a file in the credential dir, and windows relocates the whole
    `%LocalAppData%\\Fauna` data dir (MLS store, logs,
    trust-pin store — `BackupPaths.DataDir` in the app) via `data_dir` plus its existing
    file-backed credential store (`FAUNA_E2E_CREDENTIAL_DIR`, the same seam a normal
    windows e2e login already uses — see `drivers/windows.py`). Same concept, three
    spellings; the mapping is confined to this function so no test file ever branches
    on it.
    """
    if client == "macos":
        return {"home": str(home), "credential_dir": str(creds)}
    if client == "windows":
        return {"data_dir": str(home), "credential_dir": str(creds), "keyring_app": keyring_app}
    return {
        "xdg_base": str(home),
        "keyring_app": keyring_app,
        "credential_dir": str(creds),
    }


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
    """
    from conftest import _apply_r14_trust_env

    environment = client_env(client)
    _apply_r14_trust_env(environment, nest, request, build_commit=build_commit)
    config = {
        "url": nest["url"],
        "app_path": str(app_path),
        "environment": environment,
        **state_home_config(client, home=home, creds=creds, keyring_app=keyring_app),
    }
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
    silently no-ops).
    """
    from conftest import _apply_r14_trust_env

    environment = client_env(client, suppress_push=suppress_push)
    _apply_r14_trust_env(environment, nest, request, build_commit=build_commit)
    driver = create_driver(client)
    driver.launch({
        "url": nest["url"],
        "app_path": str(app_path),
        "environment": environment,
    })
    driver.set_state(_session_patch(nest, user, handle, device_id))
    return driver, ActionLayer(driver)
